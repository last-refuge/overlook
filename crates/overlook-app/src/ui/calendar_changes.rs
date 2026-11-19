use super::MailWindow;
use crate::{assets::icon, theme::ActivePalette};
use gpui_kit::{
    base::{h_flex, v_flex},
    component::{
        button::{Button, ButtonVariants},
        scroll::ScrollableElement,
        Sizable,
    },
    prelude::*,
    *,
};
use overlook_core::*;

impl MailWindow {
    pub(crate) fn save_caldav_edit(
        &mut self,
        before: Option<Event>,
        after: Option<Event>,
        scope: RecurrenceScope,
        cx: &mut Context<Self>,
    ) -> Result<EventId, String> {
        let id = crate::caldav_sync::queue_change(
            &self.store,
            &self.account,
            CalDavEdit {
                before,
                after,
                scope,
            },
        )?;
        self.reload_calendar();
        self.sync_caldav_account(self.account.id.clone(), true, cx);
        Ok(id)
    }

    fn resolve_caldav(
        &mut self,
        change: CalDavChange,
        keep_local: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let result =
            crate::caldav_sync::resolve_change(&self.store, &self.account, &change, keep_local);
        match result {
            Ok(()) => {
                self.reload_calendar();
                // Resolution removes the focused action button. Restore a live
                // focus target so keyboard navigation and window shortcuts continue.
                self.cal_focus.focus(window, cx);
                self.sync_caldav_account(change.account_id, true, cx);
            }
            Err(e) => super::toast(window, cx, crate::notify::Level::Error, e),
        }
        cx.notify();
    }

    pub(crate) fn render_caldav_changes(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = *cx.palette();
        v_flex().id("caldav-changes").max_h(px(250.)).p(px(14.)).gap(px(12.)).bg(p.surface).border_b_1().border_color(p.divider)
            .child(h_flex().items_center().child(div().flex_1().font_weight(FontWeight::SEMIBOLD).child("Calendar changes"))
                .child(Button::new("close-caldav-changes").small().ghost().label("Close").on_click(cx.listener(|this,_,w,cx|{this.show_caldav_changes=false;this.cal_focus.focus(w,cx);cx.notify();}))))
            .children(self.caldav_changes.iter().map(|change| {
                let mine=change.clone(); let server=change.clone(); let retry=change.clone(); let discard=change.clone();
                let local=change.edits.last().and_then(|e|e.after.as_ref());
                let details=local.map(|e|format!("Your version: {} · {} – {}{}",tz::format_day(e.start),tz::format_clock(e.start),tz::format_clock(e.end),e.location.as_ref().map(|l|format!(" · {l}")).unwrap_or_default())).unwrap_or_else(||"Your change: delete this event".into());
                v_flex().gap(px(6.)).p(px(10.)).border_1().border_color(p.divider).rounded(px(8.))
                    .child(div().font_weight(FontWeight::MEDIUM).child(change.title()))
                    .child(div().text_size(crate::ui::ui_px(12.)).child(details))
                    .children(local.and_then(|e|e.description.as_ref()).map(|s|div().text_size(crate::ui::ui_px(12.)).child(s.chars().take(500).collect::<String>())))
                    .child(div().text_size(crate::ui::ui_px(12.)).text_color(p.muted).child(change.note.clone().unwrap_or_else(||match change.state {CalDavChangeState::Pending=>"Saved on this device. Waiting to upload.".into(),_=>"Checking the provider before retrying this upload. Your changes are saved.".into()})))
                    .when(change.state==CalDavChangeState::Conflict,|d| d
                        .child(div().text_size(crate::ui::ui_px(12.)).child(self.caldav_previews.get(&change.id).cloned().unwrap_or_else(||"Provider version: this event was deleted.".into())))
                        .when(change.remote.is_none(), |d| d.child(div().text_size(crate::ui::ui_px(12.)).text_color(p.muted).child("Recreating restores the saved event resource, including the full series for a repeating event.")))
                        .child(h_flex().gap(px(8.)).flex_wrap()
                            .child(Button::new(SharedString::from(format!("caldav-mine-{}",change.id))).small().primary().label(if change.remote.is_none(){"Recreate my version"}else{"Keep my changes"}).on_click(cx.listener(move |this,_,w,cx|this.resolve_caldav(mine.clone(),true,w,cx))))
                            .child(Button::new(SharedString::from(format!("caldav-server-{}",change.id))).small().label("Use provider version").on_click(cx.listener(move |this,_,w,cx|this.resolve_caldav(server.clone(),false,w,cx))))))
                    .when(change.state==CalDavChangeState::Failed,|d|d.child(Button::new(SharedString::from(format!("caldav-retry-{}",change.id))).small().icon(icon("refresh-cw").size(px(14.))).label("Retry").on_click(cx.listener(move |this,_,w,cx|{
                        if let Err(e)=this.store.mark_caldav_change(&retry,CalDavChangeState::Checking,None,"Checking this saved change again…".into()){super::toast(w,cx,crate::notify::Level::Error,e.to_string());}
                        this.reload_calendar(); this.cal_focus.focus(w,cx); this.sync_caldav_account(retry.account_id.clone(),true,cx);
                    })))
                    .child(div().text_size(crate::ui::ui_px(12.)).text_color(p.muted).child("Discarding removes the saved local change. Sync again to restore any provider copy."))
                    .child(Button::new(SharedString::from(format!("caldav-discard-{}",change.id))).small().label("Discard local changes").on_click(cx.listener(move |this,_,w,cx|this.resolve_caldav(discard.clone(),false,w,cx)))))
            })).overflow_y_scrollbar().into_any_element()
    }
}
