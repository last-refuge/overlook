//! Message list: title, filter tabs, day groups, and conversation rows.

use super::widgets::*;
use super::{ListRow, MailWindow};
use crate::assets::icon;
use crate::theme::ActivePalette;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::{Sizable, StyledExt};
use gpui_kit::prelude::*;
use gpui_kit::*;
use gpui_kit::base::{h_flex, v_flex};
use overlook_core::status::draft_state_label;
use overlook_core::time::{day_group, relative_label, DayGroup};
use overlook_core::*;
use overlook_store::{ListFilter, MailboxView};

impl MailWindow {
    pub(super) fn render_list(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = *cx.palette();
        let compact = self.density == Density::Compact;
        let title: SharedString = if !self.search_query.is_empty() {
            "Search results".into()
        } else {
            self.view_title().into()
        };
        let rows = self.list_rows();
        let total = rows.iter().filter(|r| !matches!(r, ListRow::Group(..))).count();
        self.sync_list_state(&rows);
        let focus_split = self.inbox_focus().is_some();
        let inbox_tab = self.inbox_tab;
        let filter = self.filter;
        let this = cx.entity().downgrade();

        let tab = |this: &Self, label: &'static str, filter: ListFilter, cx: &mut Context<Self>| {
            let active = this.filter == filter;
            div()
                .id(SharedString::from(format!("filter-{label}")))
                .pb(px(10.))
                .border_b_2()
                .border_color(if active { p.accent } else { gpui_kit::transparent_black() })
                .text_size(px(13.))
                .text_color(if active { p.accent } else { p.muted })
                .font_weight(if active { FontWeight::MEDIUM } else { FontWeight::NORMAL })
                .cursor_pointer()
                .child(label)
                .on_click(cx.listener(move |this, _, window, cx| this.set_filter(filter, window, cx)))
        };

        let header = v_flex()
            .px(px(18.))
            .pt(px(16.))
            .child(
                h_flex()
                    .items_center()
                    .gap(px(9.))
                    .child(
                        div()
                            .text_size(px(22.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .line_height(px(30.))
                            .child(title),
                    )
                    .child(chip(total.to_string(), &p))
                    .child(div().flex_1())
                    .when(focus_split, |row| {
                        let t = this.clone();
                        row.child(
                            Button::new("list-filter")
                                .ghost()
                                .small()
                                .icon(icon(if filter == ListFilter::All { "list-filter" } else { "funnel" }).size(px(16.)))
                                .tooltip("Filter")
                                .dropdown_menu(move |menu, _window, _cx| {
                                    let mut menu = menu.label("Show");
                                    for (label, f) in [("All", ListFilter::All), ("Unread", ListFilter::Unread), ("With attachments", ListFilter::Attachments)] {
                                        let t = t.clone();
                                        menu = menu.item(PopupMenuItem::new(label).checked(filter == f).on_click(move |_, window, cx| {
                                            let _ = t.update(cx, |this, cx| this.set_filter(f, window, cx));
                                        }));
                                    }
                                    menu
                                }),
                        )
                    })
                    .child(
                        Button::new("density")
                            .ghost()
                            .small()
                            .icon(icon(if compact { "rows-4" } else { "rows-3" }).size(px(16.)))
                            .tooltip(if compact { "Comfortable density" } else { "Compact density" })
                            .on_click(cx.listener(|this, _, window, cx| this.toggle_density(window, cx))),
                    ),
            )
            .child(
                h_flex()
                    .gap(px(18.))
                    .mt(px(14.))
                    .border_b_1()
                    .border_color(p.divider)
                    .when(focus_split, |row| {
                        let focus_tab = |label: &'static str, tab: FocusTab, cx: &mut Context<Self>| {
                            let active = inbox_tab == tab;
                            div()
                                .id(SharedString::from(format!("focus-{label}")))
                                .pb(px(10.))
                                .border_b_2()
                                .border_color(if active { p.accent } else { gpui_kit::transparent_black() })
                                .text_size(px(13.))
                                .text_color(if active { p.accent } else { p.muted })
                                .font_weight(if active { FontWeight::MEDIUM } else { FontWeight::NORMAL })
                                .cursor_pointer()
                                .child(label)
                                .on_click(cx.listener(move |this, _, window, cx| this.set_inbox_tab(tab, window, cx)))
                        };
                        row.child(focus_tab("Focused", FocusTab::Focused, cx)).child(focus_tab("Other", FocusTab::Other, cx))
                    })
                    .when(!focus_split, |row| {
                        row.child(tab(self, "All", ListFilter::All, cx))
                            .child(tab(self, "Unread", ListFilter::Unread, cx))
                            .child(tab(self, "Attachments", ListFilter::Attachments, cx))
                    }),
            );

        let coverage = if self.search_query.is_empty() {
            None
        } else {
            Some(
                div()
                    .px(px(18.))
                    .pt(px(10.))
                    .text_size(px(11.))
                    .text_color(p.muted)
                    .child(self.search_coverage.clone()),
            )
        };

        let body: AnyElement = if rows.is_empty() {
            let nothing_local = !self.is_sample(&self.account.id) && self.store.message_count(&self.account.id).unwrap_or(0) == 0;
            let paused_hint: String = match &self.status.state {
                SyncState::SyncFailed(reason) | SyncState::Paused(reason) if nothing_local && !self.is_syncing_active() => reason.clone(),
                _ => "Your inbox will appear here in a moment.".to_owned(),
            };
            let (title, hint): (String, String) = match (&self.view, self.search_query.is_empty()) {
                _ if nothing_local => ("Downloading recent mail…".into(), paused_hint),
                (_, false) => ("No results".into(), "Try another name or keyword.".into()),
                (MailboxView::Kind(LabelKind::Inbox), _) if focus_split && inbox_tab == FocusTab::Other => ("Nothing in Other.".into(), "Newsletters and automated mail land here.".into()),
                (MailboxView::Kind(LabelKind::Inbox), _) => ("Your inbox is clear.".into(), "New mail will appear here.".into()),
                (MailboxView::Kind(LabelKind::Drafts), _) => ("No drafts".into(), "Messages you start are saved on this device.".into()),
                (MailboxView::Kind(LabelKind::Outbox), _) => ("Outbox is empty".into(), "Messages waiting to be sent appear here.".into()),
                _ => ("Nothing here".into(), "There are no messages in this view.".into()),
            };
            v_flex()
                .items_center()
                .pt(px(72.))
                .gap(px(6.))
                .text_color(p.muted)
                .child(icon("inbox").size(px(28.)).text_color(p.divider))
                .child(div().text_size(px(14.)).font_weight(FontWeight::MEDIUM).text_color(p.foreground).child(title))
                .child(div().text_size(px(12.)).child(hint))
                .into_any_element()
        } else {
            let view = cx.entity();
            let shown = self.shown_rows.clone();
            div()
                .flex_1()
                .min_h_0()
                .px(px(8.))
                .child(
                    list(self.list_state.clone(), move |ix, _window, cx| {
                        let Some(row) = shown.borrow().get(ix).cloned() else { return div().into_any_element() };
                        view.update(cx, |this, cx| this.render_list_row(row, compact, cx))
                    })
                    .size_full(),
                )
                .vertical_scrollbar(&self.list_state)
                .into_any_element()
        };

        v_flex()
            .id("message-list")
            .key_context("MessageList")
            .track_focus(&self.list_focus)
            .size_full()
            .bg(p.list_surface())
            .border_r_1()
            .border_color(p.divider)
            .child(header)
            .children(coverage)
            .child(body)
    }

    /// Tells the virtual list which rows changed since the last frame. Rows that are the
    /// same at the start and the end keep their measured heights and the scroll position.
    fn sync_list_state(&self, rows: &[ListRow]) {
        let mut old = self.shown_rows.borrow_mut();
        if old.as_slice() == rows {
            return;
        }
        let prefix = old.iter().zip(rows).take_while(|(a, b)| a == b).count();
        let suffix = old[prefix..].iter().rev().zip(rows[prefix..].iter().rev()).take_while(|(a, b)| a == b).count();
        self.list_state.splice(prefix..old.len() - suffix, rows.len() - prefix - suffix);
        *old = rows.to_vec();
    }

    fn render_list_row(&mut self, row: ListRow, compact: bool, cx: &mut Context<Self>) -> AnyElement {
        let p = *cx.palette();
        match row {
            ListRow::Group(label, n) => h_flex()
                .items_center()
                .justify_between()
                .px(px(11.))
                .pt(px(13.))
                .pb(px(5.))
                .text_size(px(11.))
                .text_color(p.muted)
                .child(label)
                .child(n.to_string())
                .into_any_element(),
            ListRow::Conversation(summary) => self.render_conversation_row(&summary, compact, cx).into_any_element(),
            ListRow::Draft(draft) => self.render_draft_row(&draft, cx).into_any_element(),
        }
    }

    pub(super) fn list_rows(&self) -> Vec<ListRow> {
        let mut rows = Vec::new();
        match &self.view {
            MailboxView::Kind(LabelKind::Drafts) if self.search_query.is_empty() => {
                rows.extend(self.drafts.iter().cloned().map(ListRow::Draft));
            }
            MailboxView::Kind(LabelKind::Outbox) if self.search_query.is_empty() => {
                rows.extend(self.outbox.iter().cloned().map(ListRow::Draft));
            }
            _ => {
                let now = overlook_core::time::now();
                let groups: Vec<DayGroup> = self.conversations.iter().map(|c| day_group(c.latest, now, self.utc_offset)).collect();
                let mut current: Option<&DayGroup> = None;
                for (i, c) in self.conversations.iter().enumerate() {
                    let group = &groups[i];
                    if current != Some(group) {
                        let n = groups[i..].iter().take_while(|g| *g == group).count();
                        rows.push(ListRow::Group(group.label().to_owned().into(), n));
                        current = Some(group);
                    }
                    rows.push(ListRow::Conversation(c.clone()));
                }
            }
        }
        rows
    }

    fn render_conversation_row(&self, c: &ConversationSummary, compact: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let p = *cx.palette();
        let selected = self.selected.as_ref() == Some(&c.id);
        let unread = c.is_unread();
        let now = overlook_core::time::now();
        let time = relative_label(c.latest, now, self.utc_offset);
        let who = c.participants.first().cloned().unwrap_or_else(|| self.owner());
        let label_name = c
            .label_ids
            .iter()
            .filter_map(|id| self.labels.iter().find(|l| &l.id == id))
            .find(|l| l.kind == LabelKind::User)
            .map(|l| l.name.clone());
        let conv_id = c.id.clone();
        let star_id = c.latest_message_id.clone();
        let starred = c.starred;
        let pending = c.pending;
        let drag = super::ConversationDrag {
            message_ids: self.mail_index.message_ids_in(&c.id),
            title: c.subject.clone(),
            count: c.message_count,
        };

        div()
            .id(SharedString::from(format!("conv-{}", c.id)))
            .relative()
            .h_flex()
            .items_start()
            .gap(px(11.))
            .px(px(11.))
            .py(px(if compact { 8. } else { 12. }))
            .rounded(px(7.))
            .border_b_1()
            .border_color(if selected { gpui_kit::transparent_black() } else { p.divider })
            .cursor_pointer()
            .when(selected, |d| d.bg(p.selection))
            .when(!selected && unread, |d| d.bg(p.list_unread()))
            .when(!selected, |d| d.hover(|d| d.bg(p.hover)))
            // Unread and selected both mark the left edge, like Outlook: unread with the
            // accent bar, selected with the same bar on the tinted background. Alexander's
            // light blue selection carries itself, so its amber bar marks unread mail only.
            .when(unread || (selected && p.resolved != overlook_core::ResolvedTheme::Alexander), |d| {
                d.child(
                    div()
                        .absolute()
                        .left_0()
                        .top(px(if selected { 12. } else { 10. }))
                        .bottom(px(if selected { 12. } else { 10. }))
                        .w(px(3.))
                        .rounded(px(2.))
                        .bg(p.message_accent()),
                )
            })
            .when(!compact && self.prefs.show_avatars, |d| d.child(avatar(&who, 30., &p).mt(px(1.))))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(2.))
                    .child(
                        h_flex()
                            .items_center()
                            .gap(px(8.))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(px(13.))
                                    .font_weight(if unread { FontWeight::BOLD } else { FontWeight::NORMAL })
                                    .text_color(p.foreground)
                                    .child(c.participant_line()),
                            )
                            .when(c.message_count > 1, |d| {
                                d.child(div().text_size(px(11.)).text_color(p.muted).child(c.message_count.to_string()))
                            })
                            .when(pending, |d| {
                                d.child(
                                    icon("cloud-off")
                                        .size(px(13.))
                                        .text_color(p.warning),
                                )
                            })
                            .child(
                                div()
                                    .flex_none()
                                    .text_size(px(11.))
                                    .font_weight(if unread { FontWeight::SEMIBOLD } else { FontWeight::NORMAL })
                                    .text_color(if unread { p.accent } else { p.muted })
                                    .child(time),
                            ),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_size(px(13.))
                            .font_weight(if unread { FontWeight::BOLD } else { FontWeight::NORMAL })
                            .text_color(p.foreground)
                            .child(c.subject.clone()),
                    )
                    .when(!compact && self.prefs.show_previews, |d| {
                        d.child(
                            div()
                                .truncate()
                                .text_size(px(12.))
                                .text_color(p.muted)
                                .child(c.snippet.clone()),
                        )
                    })
                    .when(!compact && (c.has_attachments || label_name.is_some()), |d| {
                        d.child(
                            h_flex()
                                .items_center()
                                .gap(px(6.))
                                .mt(px(4.))
                                .text_size(px(11.))
                                .text_color(p.muted)
                                .when(c.has_attachments, |d| {
                                    d.child(icon("paperclip").size(px(12.)).text_color(p.muted)).child("Attachments")
                                })
                                .children(label_name.map(|l| chip(l, &p))),
                        )
                    }),
            )
            .child(
                Button::new(SharedString::from(format!("star-{}", c.id)))
                    .ghost()
                    .xsmall()
                    .icon(
                        icon("star")
                            .size(px(15.))
                            .text_color(if starred { p.brand_accent } else { p.muted }),
                    )
                    .tooltip(if starred { "Unstar" } else { "Star" })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.set_starred(vec![star_id.clone()], !starred, window, cx);
                    })),
            )
            .on_drag(drag, |d: &super::ConversationDrag, _pos, _window, cx| {
                let label = if d.count > 1 { format!("{} · {} messages", d.title, d.count) } else { d.title.clone() };
                cx.new(|_| super::DragPreview { label })
            })
            .on_click(cx.listener(move |this, _, window, cx| this.select_conversation(Some(conv_id.clone()), window, cx)))
    }

    fn render_draft_row(&self, d: &Draft, cx: &mut Context<Self>) -> impl IntoElement {
        let p = *cx.palette();
        let selected = self.composer.as_ref().is_some_and(|c| c.draft.id == d.id);
        let recipients = if d.to.is_empty() {
            "No recipients".to_owned()
        } else {
            d.to.iter().map(Address::display_name).collect::<Vec<_>>().join(", ")
        };
        let now = overlook_core::time::now();
        let state = draft_state_label(&d.state);
        let state_color = match d.state {
            DraftState::Failed(_) => p.error,
            DraftState::Uncertain => p.warning,
            DraftState::Queued | DraftState::Sending => p.accent,
            _ => p.muted,
        };
        let draft_id = d.id.clone();
        let outbox = d.state.is_outbox();
        let cancellable = matches!(d.state, DraftState::Queued | DraftState::Failed(_));
        let undecided = matches!(d.state, DraftState::Uncertain);
        let preview: String = d.body_text.lines().find(|l| !l.trim().is_empty() && !l.starts_with('>')).unwrap_or("").chars().take(90).collect();

        div()
            .id(SharedString::from(format!("draft-{}", d.id)))
            .h_flex()
            .items_start()
            .gap(px(11.))
            .px(px(11.))
            .py(px(12.))
            .rounded(px(7.))
            .border_b_1()
            .border_color(if selected { gpui_kit::transparent_black() } else { p.divider })
            .cursor_pointer()
            .when(selected, |d| d.bg(p.selection))
            .when(!selected, |d| d.hover(|d| d.bg(p.hover)))
            .child(owner_avatar(&self.owner(), 30., &p))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(2.))
                    .child(
                        h_flex()
                            .items_center()
                            .gap(px(8.))
                            .child(div().flex_1().min_w_0().truncate().text_size(px(13.)).font_weight(FontWeight::MEDIUM).child(recipients))
                            .child(div().text_size(px(11.)).text_color(p.muted).child(relative_label(d.updated_at, now, self.utc_offset))),
                    )
                    .child(div().truncate().text_size(px(13.)).child(d.display_subject()))
                    .child(div().truncate().text_size(px(12.)).text_color(p.muted).child(if preview.is_empty() { "Draft in progress".to_owned() } else { preview }))
                    .child(
                        h_flex()
                            .items_center()
                            .gap(px(6.))
                            .mt(px(4.))
                            .text_size(px(11.))
                            .text_color(state_color)
                            .child(icon(if outbox { "clock" } else { "check" }).size(px(12.)).text_color(state_color))
                            // The state wording can be long ("Delivery uncertain: …"); it
                            // gives way so the buttons stay inside the row.
                            .child(div().flex_1().min_w_0().truncate().child(state))
                            .when(undecided, |row| {
                                let resend = draft_id.clone();
                                let discard = draft_id.clone();
                                row.child(
                                    Button::new(SharedString::from(format!("resend-{}", draft_id)))
                                        .ghost()
                                        .xsmall()
                                        .label("Send again")
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            cx.stop_propagation();
                                            this.resend_draft(resend.clone(), window, cx);
                                        })),
                                )
                                .child(
                                    Button::new(SharedString::from(format!("discard-{}", draft_id)))
                                        .ghost()
                                        .xsmall()
                                        .label("Discard")
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            cx.stop_propagation();
                                            this.discard_send(discard.clone(), window, cx);
                                        })),
                                )
                            })
                            .when(cancellable, |row| {
                                let id = draft_id.clone();
                                row.child(
                                    Button::new(SharedString::from(format!("cancel-{}", draft_id)))
                                        .ghost()
                                        .xsmall()
                                        .label("Edit")
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            cx.stop_propagation();
                                            this.cancel_send(id.clone(), window, cx);
                                        })),
                                )
                            }),
                    ),
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                if outbox {
                    this.select_outbox_item(draft_id.clone(), window, cx);
                } else {
                    this.open_draft(draft_id.clone(), window, cx);
                }
            }))
    }
}
