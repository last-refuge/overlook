//! Subscribed iCalendar (`.ics` / `webcal://`) modal, sidebar actions, and background sync.

use super::calendar::{hex_to_hsla, ACCOUNT_CALENDAR_COLORS};
use super::widgets::kbd;
use super::{MailWindow, Module};
use crate::assets::icon;
use crate::subscription_sync::{
    reexpand_subscription_offline, refresh_subscription, REFRESH_INTERVAL_MS,
};
use crate::theme::ActivePalette;
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Disableable, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::*;
use overlook_core::time::now;
use overlook_core::{
    normalize_subscription_url, tz, CalendarSubscription, CalendarSubscriptionId,
};

pub(crate) struct SubscriptionModal {
    pub(super) editing: Option<CalendarSubscriptionId>,
    pub(super) original_url: Option<String>,
    pub(super) url: Entity<InputState>,
    pub(super) name: Entity<InputState>,
    pub(super) color: Option<String>,
    pub(super) busy: bool,
    pub(super) error: Option<String>,
    pub(super) request_id: u64,
    _subs: Vec<Subscription>,
}

pub(crate) fn subscription_detail_label(sub: &CalendarSubscription) -> String {
    let mut parts = Vec::new();
    if let Some(host) = sub.host_label() {
        parts.push(host);
    }
    if let Some(err) = &sub.last_error {
        parts.push(err.clone());
    } else if let Some(at) = sub.last_synced_at {
        parts.push(format!(
            "Updated {} at {}",
            tz::format_day(at),
            tz::format_clock(at)
        ));
    } else {
        parts.push("Not synced yet".to_owned());
    }
    parts.join(" · ")
}

impl MailWindow {
    pub(crate) fn open_add_subscription_modal(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.cal_color_picker_open = None;
        let url = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("webcal://p...-caldav.icloud.com/... or https://...")
        });
        let name = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Optional (uses feed name if blank)")
        });
        let _subs = vec![
            cx.subscribe_in(&url, window, |this, _, ev: &InputEvent, window, cx| {
                if matches!(ev, InputEvent::PressEnter { .. }) {
                    this.submit_subscription_modal(window, cx);
                }
            }),
            cx.subscribe_in(&name, window, |this, _, ev: &InputEvent, window, cx| {
                if matches!(ev, InputEvent::PressEnter { .. }) {
                    this.submit_subscription_modal(window, cx);
                }
            }),
        ];
        url.update(cx, |s, cx| s.focus(window, cx));
        self.cal_sub_modal_seq = self.cal_sub_modal_seq.wrapping_add(1);
        self.cal_subscription_modal = Some(SubscriptionModal {
            editing: None,
            original_url: None,
            url,
            name,
            color: None,
            busy: false,
            error: None,
            request_id: self.cal_sub_modal_seq,
            _subs,
        });
        cx.notify();
    }

    pub(crate) fn open_edit_subscription_modal(
        &mut self,
        id: CalendarSubscriptionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(sub) = self
            .store
            .calendar_subscription(&id)
            .ok()
            .flatten()
        else {
            return;
        };
        self.cal_color_picker_open = None;
        let url = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("webcal://p...-caldav.icloud.com/... or https://...")
                .default_value(sub.url.clone())
        });
        let name = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Calendar name")
                .default_value(sub.name.clone())
        });
        let _subs = vec![
            cx.subscribe_in(&url, window, |this, _, ev: &InputEvent, window, cx| {
                if matches!(ev, InputEvent::PressEnter { .. }) {
                    this.submit_subscription_modal(window, cx);
                }
            }),
            cx.subscribe_in(&name, window, |this, _, ev: &InputEvent, window, cx| {
                if matches!(ev, InputEvent::PressEnter { .. }) {
                    this.submit_subscription_modal(window, cx);
                }
            }),
        ];
        name.update(cx, |s, cx| s.focus(window, cx));
        self.cal_sub_modal_seq = self.cal_sub_modal_seq.wrapping_add(1);
        self.cal_subscription_modal = Some(SubscriptionModal {
            editing: Some(sub.id),
            original_url: Some(sub.url),
            url,
            name,
            color: sub.color,
            busy: false,
            error: None,
            request_id: self.cal_sub_modal_seq,
            _subs,
        });
        cx.notify();
    }

    pub(crate) fn close_subscription_modal(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.cal_sub_modal_seq = self.cal_sub_modal_seq.wrapping_add(1);
        self.cal_subscription_modal = None;
        if self.settings.is_none() {
            self.focus_current_module(window, cx);
        }
        cx.notify();
    }

    pub(crate) fn submit_subscription_modal(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(modal) = self.cal_subscription_modal.as_ref() else {
            return;
        };
        if modal.busy {
            return;
        }
        let raw_url = modal.url.read(cx).value().to_string();
        let raw_name = modal.name.read(cx).value().to_string();
        let editing_id = modal.editing.clone();
        let original_url = modal.original_url.clone();
        let chosen_color = modal.color.clone();

        let normalized_url = match normalize_subscription_url(&raw_url) {
            Ok(u) => u,
            Err(err) => {
                if let Some(m) = self.cal_subscription_modal.as_mut() {
                    m.error = Some(err.to_owned());
                }
                cx.notify();
                return;
            }
        };

        if let (Some(id), Some(orig)) = (editing_id.clone(), original_url) {
            if normalized_url == orig {
                let trimmed = raw_name.trim();
                if trimmed.is_empty() {
                    if let Some(m) = self.cal_subscription_modal.as_mut() {
                        m.error = Some("Enter a calendar name.".to_owned());
                    }
                    cx.notify();
                    return;
                }
                match self.store.update_calendar_subscription_meta(
                    &id,
                    trimmed,
                    chosen_color.as_deref(),
                ) {
                    Ok(()) => {
                        let saved_name = trimmed.to_owned();
                        self.cal_subscription_modal = None;
                        self.reload_calendar();
                        if self.settings.is_none() {
                            self.focus_current_module(window, cx);
                        }
                        crate::ui::toast(
                            window,
                            cx,
                            crate::notify::Level::Success,
                            format!("Saved “{saved_name}”."),
                        );
                        cx.notify();
                    }
                    Err(e) => {
                        if let Some(m) = self.cal_subscription_modal.as_mut() {
                            m.error = Some(e.to_string());
                        }
                        cx.notify();
                    }
                }
                return;
            }
        }

        self.cal_sub_modal_seq = self.cal_sub_modal_seq.wrapping_add(1);
        let request_id = self.cal_sub_modal_seq;
        if let Some(m) = self.cal_subscription_modal.as_mut() {
            m.busy = true;
            m.error = None;
            m.request_id = request_id;
        }
        let (view_from, view_to) = self.cal_range();
        let store = self.store.clone();
        cx.notify();

        cx.spawn_in(window, async move |this, cx| {
            let result = super::spawn_blocking_io(move || {
                let sub_id = editing_id
                    .clone()
                    .unwrap_or_else(CalendarSubscriptionId::generate);
                let fetched = crate::subscription_sync::fetch_for_modal(
                    &normalized_url,
                    Some(&raw_name),
                    chosen_color.as_deref(),
                    &sub_id,
                    view_from,
                    view_to,
                )?;
                let is_edit = editing_id.is_some();
                if is_edit {
                    store
                        .replace_calendar_subscription_url(
                            &sub_id,
                            &fetched.normalized_url,
                            &fetched.name,
                            fetched.color.as_deref(),
                            fetched.etag.as_deref(),
                            fetched.last_modified.as_deref(),
                            &fetched.raw_ics,
                            fetched.expanded_from,
                            fetched.expanded_to,
                            &fetched.expanded_tz,
                            fetched.synced_at,
                            &fetched.events,
                        )
                        .map_err(|e| e.to_string())?;
                } else {
                    let sub = CalendarSubscription {
                        id: sub_id,
                        url: fetched.normalized_url,
                        name: fetched.name.clone(),
                        color: fetched.color,
                        visible: true,
                        sort_order: 0,
                        etag: fetched.etag,
                        last_modified: fetched.last_modified,
                        expanded_from: Some(fetched.expanded_from),
                        expanded_to: Some(fetched.expanded_to),
                        expanded_tz: Some(fetched.expanded_tz),
                        last_synced_at: Some(fetched.synced_at),
                        last_error: None,
                        created_at: fetched.synced_at,
                    };
                    store
                        .create_calendar_subscription(&sub, &fetched.raw_ics, &fetched.events)
                        .map_err(|e| e.to_string())?;
                }
                Ok::<_, String>((fetched.name, is_edit))
            })
            .await;

            let _ = this.update_in(cx, |this, window, cx| {
                let is_current = this
                    .cal_subscription_modal
                    .as_ref()
                    .is_some_and(|m| m.request_id == request_id);
                match result {
                    Some(Ok((name, is_edit))) => {
                        if is_current {
                            this.cal_subscription_modal = None;
                            if this.settings.is_none() {
                                this.focus_current_module(window, cx);
                            }
                            let msg = if is_edit {
                                format!("Updated “{name}”.")
                            } else {
                                format!("Added “{name}”.")
                            };
                            crate::ui::toast(window, cx, crate::notify::Level::Success, msg);
                        }
                        this.reload_calendar();
                    }
                    Some(Err(err)) => {
                        if let Some(m) = this
                            .cal_subscription_modal
                            .as_mut()
                            .filter(|m| m.request_id == request_id)
                        {
                            m.busy = false;
                            m.error = Some(err);
                        }
                    }
                    None => {
                        if let Some(m) = this
                            .cal_subscription_modal
                            .as_mut()
                            .filter(|m| m.request_id == request_id)
                        {
                            m.busy = false;
                            m.error = Some("Could not download the calendar URL.".to_owned());
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn toggle_subscription_visible(
        &mut self,
        id: CalendarSubscriptionId,
        cx: &mut Context<Self>,
    ) {
        let Some(current) = self
            .cal_subscriptions
            .iter()
            .find(|s| s.id == id)
            .map(|s| s.visible)
        else {
            return;
        };
        if self
            .store
            .set_calendar_subscription_visible(&id, !current)
            .is_ok()
        {
            self.reload_calendar();
            if self.module == Module::Calendar {
                self.ensure_subscription_expansion(cx);
            }
            cx.notify();
        }
    }

    pub(crate) fn set_subscription_color(
        &mut self,
        id: CalendarSubscriptionId,
        color: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if self
            .store
            .set_calendar_subscription_color(&id, color.as_deref())
            .is_ok()
        {
            self.cal_color_picker_open = None;
            self.reload_calendar();
            cx.notify();
        }
    }

    pub(crate) fn remove_subscription(
        &mut self,
        id: CalendarSubscriptionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = self
            .cal_subscriptions
            .iter()
            .find(|s| s.id == id)
            .map(|s| s.name.clone())
            .unwrap_or_else(|| "calendar".to_owned());
        if self.store.delete_calendar_subscription(&id).is_ok() {
            if self.cal_color_picker_open.as_ref()
                == Some(&super::calendar::CalendarColorTarget::Subscription(id.clone()))
            {
                self.cal_color_picker_open = None;
            }
            if self
                .cal_subscription_modal
                .as_ref()
                .is_some_and(|m| m.editing.as_ref() == Some(&id))
            {
                self.cal_subscription_modal = None;
            }
            self.cal_sub_syncing.remove(&id);
            self.cal_sub_next_retry.remove(&id);
            self.cal_sub_expanding.remove(&id);
            self.reload_calendar();
            crate::ui::toast(
                window,
                cx,
                crate::notify::Level::Info,
                format!("Removed “{name}”."),
            );
            cx.notify();
        }
    }

    pub(crate) fn sync_subscription(
        &mut self,
        id: CalendarSubscriptionId,
        force: bool,
        cx: &mut Context<Self>,
    ) {
        if self.cal_sub_syncing.contains(&id) {
            return;
        }
        let Ok(Some(sub)) = self.store.calendar_subscription(&id) else {
            return;
        };
        let now_ms = now();
        if !force {
            if self
                .cal_sub_next_retry
                .get(&id)
                .is_some_and(|retry| *retry > now_ms)
            {
                return;
            }
            if sub
                .last_synced_at
                .is_some_and(|at| now_ms.saturating_sub(at) < REFRESH_INTERVAL_MS)
            {
                return;
            }
        }
        self.cal_sub_syncing.insert(id.clone());
        let (view_from, view_to) = self.cal_range();
        let store = self.store.clone();
        cx.notify();

        cx.spawn(async move |this, cx| {
            let result = super::spawn_blocking_io(move || {
                refresh_subscription(&store, &sub, view_from, view_to, force)
            })
            .await;

            let _ = this.update(cx, |this, cx| {
                this.cal_sub_syncing.remove(&id);
                match result {
                    Some(Ok(_)) => {
                        this.cal_sub_next_retry.remove(&id);
                    }
                    Some(Err(delay_secs)) => {
                        this.cal_sub_next_retry
                            .insert(id, now() + (delay_secs as i64) * 1000);
                    }
                    None => {
                        this.cal_sub_next_retry.insert(id, now() + 60_000);
                    }
                }
                this.reload_calendar();
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn sync_all_subscriptions(&mut self, force: bool, cx: &mut Context<Self>) {
        let ids: Vec<CalendarSubscriptionId> = self
            .store
            .calendar_subscriptions()
            .unwrap_or_default()
            .into_iter()
            .map(|s| s.id)
            .collect();
        for id in ids {
            self.sync_subscription(id, force, cx);
        }
    }

    pub(crate) fn ensure_subscription_expansion(&mut self, cx: &mut Context<Self>) {
        for id in self.cal_sub_needs_expansion.clone() {
            if self.cal_sub_expanding.contains(&id) {
                continue;
            }
            let Ok(Some(sub)) = self.store.calendar_subscription(&id) else {
                continue;
            };
            let has_raw = self
                .store
                .calendar_subscription_raw_ics(&id)
                .ok()
                .flatten()
                .is_some();
            if !has_raw {
                self.sync_subscription(id, false, cx);
                continue;
            }
            self.cal_sub_expanding.insert(id.clone());
            let (view_from, view_to) = self.cal_range();
            let store = self.store.clone();
            cx.spawn(async move |this, cx| {
                let changed = super::spawn_blocking_io(move || {
                    reexpand_subscription_offline(&store, &sub, view_from, view_to)
                })
                .await
                .unwrap_or(false);
                let _ = this.update(cx, |this, cx| {
                    this.cal_sub_expanding.remove(&id);
                    if changed {
                        this.reload_calendar();
                        cx.notify();
                    }
                });
            })
            .detach();
        }
    }

    pub(crate) fn render_subscription_modal(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = *cx.palette();
        let Some(modal) = self.cal_subscription_modal.as_ref() else {
            return div().into_any_element();
        };
        let editing_id = modal.editing.clone();
        let is_edit = editing_id.is_some();
        let title = if is_edit {
            "Edit subscribed calendar"
        } else {
            "Add calendar from URL"
        };
        let confirm_label = if modal.busy {
            if is_edit {
                "Saving…"
            } else {
                "Adding…"
            }
        } else if is_edit {
            "Save"
        } else {
            "Add calendar"
        };
        let auto_selected = modal.color.is_none();

        let swatches = ACCOUNT_CALENDAR_COLORS.iter().enumerate().map(|(i, &(name, hex))| {
            let color = hex_to_hsla(hex).unwrap_or(p.accent);
            let selected = modal
                .color
                .as_deref()
                .is_some_and(|c| c.eq_ignore_ascii_case(hex));
            let hex_owned = hex.to_owned();
            let disabled = modal.busy;
            div()
                .id(SharedString::from(format!("sub-modal-swatch-{i}")))
                .size(px(22.))
                .rounded_full()
                .bg(color)
                .border_2()
                .border_color(if selected {
                    p.foreground
                } else {
                    gpui_kit::transparent_black()
                })
                .flex()
                .items_center()
                .justify_center()
                .when(!disabled, |d| d.cursor_pointer().hover(|d| d.opacity(0.85)))
                .when(disabled, |d| d.opacity(0.5))
                .tooltip(move |window, cx| Tooltip::new(name).build(window, cx))
                .when(selected, |d| {
                    d.child(icon("check").size(px(11.)).text_color(gpui_kit::white()))
                })
                .when(!disabled, |d| {
                    d.on_click(cx.listener(move |this, _, _w, cx| {
                        if let Some(m) = this.cal_subscription_modal.as_mut() {
                            m.color = Some(hex_owned.clone());
                        }
                        cx.notify();
                    }))
                })
        });

        div()
            .id("subscription-modal-overlay")
            .absolute()
            .inset_0()
            .occlude()
            .bg(gpui_kit::black().opacity(if p.is_dark() { 0.55 } else { 0.3 }))
            .flex()
            .items_center()
            .justify_center()
            .on_click(cx.listener(|this, _, window, cx| {
                if !this
                    .cal_subscription_modal
                    .as_ref()
                    .is_some_and(|m| m.busy)
                {
                    this.close_subscription_modal(window, cx);
                }
            }))
            .child(
                v_flex()
                    .id("subscription-modal")
                    .w(px(460.))
                    .max_w(relative(0.94))
                    .rounded(px(12.))
                    .bg(p.surface)
                    .border_1()
                    .border_color(p.divider)
                    .shadow_lg()
                    .p(px(20.))
                    .gap(px(12.))
                    .on_click(|_, _, cx| cx.stop_propagation())
                    .child(
                        h_flex()
                            .items_center()
                            .gap(px(8.))
                            .child(icon("calendar").flex_none().size(px(16.)).text_color(p.accent))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(super::ui_px(16.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(title),
                            )
                            .child(kbd("Esc", &p)),
                    )
                    .child(
                        div()
                            .text_size(super::ui_px(12.))
                            .text_color(p.muted)
                            .child("Subscribe to a read-only Apple iCloud public calendar (webcal://) or any .ics link."),
                    )
                    .child(
                        v_flex()
                            .gap(px(5.))
                            .child(
                                div()
                                    .text_size(super::ui_px(12.))
                                    .font_weight(FontWeight::MEDIUM)
                                    .child("Calendar URL"),
                            )
                            .child(
                                Input::new(&modal.url)
                                    .aria_label("Calendar URL")
                                    .disabled(modal.busy),
                            ),
                    )
                    .child(
                        v_flex()
                            .gap(px(5.))
                            .child(
                                div()
                                    .text_size(super::ui_px(12.))
                                    .font_weight(FontWeight::MEDIUM)
                                    .child("Name"),
                            )
                            .child(
                                Input::new(&modal.name)
                                    .aria_label("Calendar name")
                                    .disabled(modal.busy),
                            ),
                    )
                    .child(
                        v_flex()
                            .gap(px(6.))
                            .child(
                                div()
                                    .text_size(super::ui_px(12.))
                                    .font_weight(FontWeight::MEDIUM)
                                    .child("Color"),
                            )
                            .child(
                                h_flex()
                                    .items_center()
                                    .gap(px(6.))
                                    .flex_wrap()
                                    .child(
                                        Button::new("sub-modal-color-auto")
                                            .small()
                                            .toggled(auto_selected)
                                            .when(auto_selected, |b| b.primary())
                                            .disabled(modal.busy)
                                            .label("Auto")
                                            .on_click(cx.listener(|this, _, _w, cx| {
                                                if let Some(m) =
                                                    this.cal_subscription_modal.as_mut()
                                                {
                                                    m.color = None;
                                                }
                                                cx.notify();
                                            })),
                                    )
                                    .children(swatches),
                            ),
                    )
                    .children(modal.error.clone().map(|e| {
                        div()
                            .text_size(super::ui_px(12.))
                            .text_color(p.error)
                            .child(e)
                    }))
                    .child(
                        h_flex()
                            .items_center()
                            .gap(px(8.))
                            .pt(px(2.))
                            .when_some(editing_id, |row, id| {
                                row.child(
                                    Button::new("sub-modal-remove")
                                        .ghost()
                                        .small()
                                        .disabled(modal.busy)
                                        .label("Remove")
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.remove_subscription(id.clone(), window, cx);
                                        })),
                                )
                            })
                            .child(div().flex_1())
                            .child(
                                Button::new("sub-modal-cancel")
                                    .ghost()
                                    .small()
                                    .disabled(modal.busy)
                                    .label("Cancel")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.close_subscription_modal(window, cx);
                                    })),
                            )
                            .child(
                                Button::new("sub-modal-save")
                                    .primary()
                                    .small()
                                    .loading(modal.busy)
                                    .disabled(modal.busy)
                                    .label(confirm_label)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.submit_subscription_modal(window, cx);
                                    })),
                            ),
                    ),
            )
            .into_any_element()
    }
}
