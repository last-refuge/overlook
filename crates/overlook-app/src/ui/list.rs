//! Message list: title, filter tabs, day groups, and conversation rows.

use super::widgets::*;
use super::{ListRow, MailWindow, RowLayout, RowsKey};
use crate::assets::{color_icon, icon, ColorIcon as C};
use crate::theme::ActivePalette;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::menu::{ContextMenuExt, DropdownMenu, PopupMenu, PopupMenuItem};
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::{Sizable, StyledExt};
use gpui_kit::prelude::*;
use gpui_kit::*;
use gpui_kit::base::{h_flex, v_flex};
use overlook_core::status::draft_state_label;
use overlook_core::time::{day_group, relative_label, DayGroup};
use overlook_core::*;
use overlook_store::{ListFilter, LocalChange, MailboxView};

/// Explains IMAP folders that hold folders but no mail (`\Noselect`).
pub(super) const FOLDER_ONLY_HELP_URL: &str = "https://github.com/last-refuge/overlook/blob/main/docs/folder-only-mailboxes.md";

/// Wraps every row the list draws. The list lays each item out as a root with a definite
/// width and a height that fits its content, and a root that does not ask for the width is
/// sized to its contents, so a row would be only as wide as its text. A column that fills the
/// width stretches whatever it holds, which keeps that true for every kind of row, including
/// ones added later.
fn full_width(row: impl IntoElement) -> AnyElement {
    v_flex().w_full().child(row).into_any_element()
}

/// Whether the virtual list should reset its scroll position instead of diff-splicing.
/// Switching account/view/filter/search always starts at the top, and filling a previously
/// empty list must also reset because GPUI's `ListState::splice(0..0, count)` shifts
/// `logical_scroll_top` from `0` to `count` (jumping straight to the bottom).
fn should_reset_list_state(previous_len: usize, next_len: usize, context_changed: bool) -> bool {
    context_changed || (previous_len == 0 && next_len > 0)
}

/// The rows that differ between two lists: the range of `old` that was replaced and how many
/// rows replaced it. Rows that are the same at the start and at the end are left out, so the
/// list keeps their measured heights and the scroll position. `None` when nothing differs.
fn changed_span<T: PartialEq>(old: &[T], new: &[T]) -> Option<(std::ops::Range<usize>, usize)> {
    if old == new {
        return None;
    }
    let prefix = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    let suffix = old[prefix..].iter().rev().zip(new[prefix..].iter().rev()).take_while(|(a, b)| a == b).count();
    Some((prefix..old.len() - suffix, new.len() - prefix - suffix))
}

/// A command on a row of the list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RowAction {
    Archive,
    Snooze,
    CancelSnooze,
    Delete,
    ToggleRead,
    ToggleStar,
}

/// The calendar event an invitation in the list belongs to: its time, and whether you can
/// still answer it.
#[derive(Clone, Debug)]
pub(crate) struct InviteRow {
    event: EventId,
    start: i64,
    end: i64,
    all_day: bool,
    can_respond: bool,
    response: Option<ResponseStatus>,
}

/// `2–3 PM` or `10:00–10:30 AM`: the suffix once when both ends share it.
fn clock_range(start: i64, end: i64) -> String {
    let (a, b) = (overlook_core::tz::format_clock(start), overlook_core::tz::format_clock(end));
    match (a.rsplit_once(' '), b.rsplit_once(' ')) {
        (Some((from, first)), Some((_, last))) if first == last => format!("{from}–{b}"),
        _ => format!("{a}–{b}"),
    }
}

/// A file named on a row: its type badge and its name, which gives way first when space
/// runs short.
fn file_chip(name: &str, p: &crate::theme::Palette) -> Div {
    let (fill, ink) = p.file_badge(name);
    h_flex()
        .flex_shrink(1.)
        .min_w(px(0.))
        .max_w(px(190.))
        .items_center()
        .gap(px(5.))
        .h(px(22.))
        .pl(px(3.))
        .pr(px(8.))
        .rounded(px(6.))
        .bg(p.message_surface())
        .border_1()
        .border_color(p.divider)
        .text_size(crate::ui::ui_px(11.5))
        .font_weight(FontWeight::MEDIUM)
        .text_color(p.foreground)
        .child(
            div()
                .flex_none()
                .px(px(3.))
                .rounded(px(3.))
                .bg(fill)
                .text_color(ink)
                .text_size(crate::ui::ui_px(8.5))
                .line_height(crate::ui::ui_px(15.))
                .font_weight(FontWeight::BOLD)
                .child(overlook_core::file_badge(name)),
        )
        .child(div().min_w_0().truncate().child(name.to_owned()))
}

impl MailWindow {
    pub(super) fn render_list(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = *cx.palette();
        let compact = self.density == Density::Compact;
        let title: SharedString = if !self.search_query.is_empty() {
            "Search results".into()
        } else {
            self.view_title().into()
        };
        self.refresh_list_rows(compact);
        // An open offline folder and where it is on disk.
        let offline_place: Option<(LabelId, String)> = match &self.view {
            MailboxView::Label(id) if self.offline_active() && self.search_query.is_empty() => self.offline_dir(id).map(|d| (id.clone(), crate::offline_disk::shown_path(&d))),
            _ => None,
        };
        let no_rows = self.shown_rows.borrow().is_empty();
        let total = self.rows_total.get();
        let unread = self.rows_unread.get();
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
                .text_size(crate::ui::ui_px(13.))
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
                            .text_size(crate::ui::ui_px(22.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .line_height(crate::ui::ui_px(30.))
                            .child(title),
                    )
                    // How much is here, and how much of it is unread.
                    .child(
                        div()
                            .pt(px(3.))
                            .text_size(crate::ui::ui_px(12.))
                            .text_color(p.muted)
                            .child(if unread > 0 { format!("{unread} unread · {total}") } else { total.to_string() }),
                    )
                    // An offline folder says where its mail is, and opens it in the file manager.
                    .when_some(offline_place, |row, (label_id, place)| {
                        row.child(
                            h_flex()
                                .id("offline-place")
                                .flex_none()
                                .max_w(px(260.))
                                .cursor_pointer()
                                .hover(|d| d.text_color(p.foreground))
                                .tooltip(|window, cx| gpui_kit::component::tooltip::Tooltip::new(super::sidebar::REVEAL_LABEL).build(window, cx))
                                .on_click(cx.listener(move |this, _, window, cx| this.reveal_offline_folder(&label_id, window, cx)))
                                .mt(px(3.))
                                .gap(px(4.))
                                .items_center()
                                .px(px(7.))
                                .py(px(1.))
                                .rounded(px(5.))
                                .bg(p.chip)
                                .border_1()
                                .border_color(p.divider)
                                .text_size(crate::ui::ui_px(11.))
                                .text_color(p.muted)
                                .child(icon("hard-drive").flex_none().size(px(12.)).text_color(p.muted))
                                .child(div().min_w_0().truncate().child(place)),
                        )
                    })
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
                        // The tab not on show says how much unread mail waits in it.
                        let focused_unread = self.focused_inbox_unread_count();
                        let other_unread = self.other_unread;
                        let focus_tab = |label: &'static str, tab: FocusTab, cx: &mut Context<Self>| {
                            let active = inbox_tab == tab;
                            let waiting = match tab {
                                FocusTab::Focused => focused_unread,
                                FocusTab::Other => other_unread,
                            };
                            div()
                                .id(SharedString::from(format!("focus-{label}")))
                                .flex()
                                .items_baseline()
                                .gap(px(4.))
                                .pb(px(10.))
                                .border_b_2()
                                .border_color(if active { p.accent } else { gpui_kit::transparent_black() })
                                .text_size(crate::ui::ui_px(13.))
                                .text_color(if active { p.accent } else { p.muted })
                                .font_weight(if active { FontWeight::MEDIUM } else { FontWeight::NORMAL })
                                .cursor_pointer()
                                .child(label)
                                .when(!active && waiting > 0, |d| d.child(div().text_size(crate::ui::ui_px(12.)).child(format!("· {waiting}"))))
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
                    .text_size(crate::ui::ui_px(11.))
                    .text_color(p.muted)
                    .child(self.search_coverage.clone()),
            )
        };

        let body: AnyElement = if no_rows {
            let nothing_local = if self.all_inboxes_active() {
                false
            } else {
                !self.is_sample(&self.account.id) && self.account_message_count == 0
            };
            let paused_hint: String = match &self.status.state {
                SyncState::SyncFailed(reason) | SyncState::Paused(reason) if nothing_local && !self.is_syncing_active() => reason.clone(),
                _ => "Your inbox will appear here in a moment.".to_owned(),
            };
            let container = match &self.view {
                MailboxView::Label(id) => self.labels.iter().any(|l| &l.id == id && l.kind == LabelKind::Container),
                _ => false,
            };
            let (title, hint): (String, String) = match (&self.view, self.search_query.is_empty()) {
                _ if container => ("This folder only holds other folders.".into(), "Mail cannot be stored here. Create a folder inside it to file messages.".into()),
                _ if nothing_local => ("Downloading recent mail…".into(), paused_hint),
                (_, false) => ("No results".into(), "Try another name or keyword.".into()),
                (MailboxView::Kind(LabelKind::Inbox), _) if focus_split && inbox_tab == FocusTab::Other => ("Nothing in Other.".into(), "Newsletters and automated mail land here.".into()),
                (MailboxView::Kind(LabelKind::Inbox), _) => ("Your inbox is clear.".into(), "New mail will appear here.".into()),
                (MailboxView::Kind(LabelKind::Snoozed), _) => ("No snoozed conversations".into(), "Snooze an email to return it to your inbox later.".into()),
                (MailboxView::Kind(LabelKind::Drafts), _) => ("No drafts".into(), "Messages you start are saved on this device.".into()),
                (MailboxView::Kind(LabelKind::Outbox), _) => ("Outbox is empty".into(), "Messages waiting to be sent appear here.".into()),
                _ => ("Nothing here".into(), "There are no messages in this view.".into()),
            };
            let empty_icon = if container {
                "folder"
            } else if matches!(self.view, MailboxView::Kind(LabelKind::Snoozed)) && self.search_query.is_empty() {
                "clock"
            } else {
                "inbox"
            };
            v_flex()
                .items_center()
                .pt(px(72.))
                .gap(px(6.))
                .text_color(p.muted)
                .child(icon(empty_icon).size(px(28.)).text_color(p.divider))
                .child(div().text_size(crate::ui::ui_px(14.)).font_weight(FontWeight::MEDIUM).text_color(p.foreground).child(title))
                .child(div().text_size(crate::ui::ui_px(12.)).child(hint))
                .when(container, |d| {
                    d.child(
                        Button::new("folder-only-help")
                            .ghost()
                            .small()
                            .icon(icon("external-link").size(px(14.)))
                            .label("About folder-only mailboxes")
                            .on_click(|_, _, cx| cx.open_url(FOLDER_ONLY_HELP_URL)),
                    )
                })
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
                        let row = shown.borrow().get(ix).cloned();
                        match row {
                            Some(row) => full_width(view.update(cx, |this, cx| this.render_list_row(row, compact, cx))),
                            None => full_width(div()),
                        }
                    })
                    .size_full()
                    // Room under the last row, which the list counts into how far it scrolls.
                    .pb(px(12.)),
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
            .child(header)
            .children(coverage)
            .child(body)
    }

    /// Brings the virtual list in line with the data. Building the rows copies every
    /// conversation, so that waits for something they are built from to change rather than
    /// repeating on every repaint; a long Archive then costs a repaint no more than an Inbox.
    fn refresh_list_rows(&self, compact: bool) {
        let key = RowsKey {
            data: self.list_data_generation,
            account: self.account.id.clone(),
            all_inboxes: self.all_inboxes_active(),
            view: self.view.clone(),
            filter: self.filter,
            inbox_tab: self.inbox_focus(),
            searching: !self.search_query.is_empty(),
            today: overlook_core::time::local_day(overlook_core::time::now(), &self.time_zone),
            time_zone_revision: self.time_zone_revision,
        };
        if self.rows_key.borrow().as_ref() != Some(&key) {
            let context_changed = self.rows_key.borrow().as_ref().is_some_and(|prev| {
                prev.account != key.account
                    || prev.all_inboxes != key.all_inboxes
                    || prev.view != key.view
                    || prev.filter != key.filter
                    || prev.inbox_tab != key.inbox_tab
                    || prev.searching != key.searching
            });
            let rows = self.list_rows();
            self.rows_total.set(rows.iter().filter(|r| !matches!(r, ListRow::Group(..))).count());
            self.rows_unread.set(rows.iter().filter(|r| matches!(r, ListRow::Conversation(c) if c.is_unread())).count());
            self.sync_list_state(&rows, context_changed);
            *self.rows_key.borrow_mut() = Some(key);
        }
        // Density, sender-picture, preview, and typography settings change every row's height,
        // and the list keeps the heights it measured for rows that are out of view.
        let layout = RowLayout {
            compact,
            avatars: self.prefs.show_avatars,
            previews: self.prefs.show_previews,
            all_inboxes: self.all_inboxes_active(),
            ui_size: self.prefs.fonts.clamped_ui_size(),
            list_family: self.prefs.fonts.effective_list_family().to_owned(),
            list_size: self.prefs.fonts.list_size,
        };
        if self.row_layout.borrow_mut().replace(layout.clone()).is_some_and(|before| before != layout) {
            self.list_state.remeasure();
        }
    }

    /// Tells the virtual list which rows changed since the last frame. Rows that are the
    /// same at the start and the end keep their measured heights and the scroll position,
    /// except when changing views or refilling an empty list, which resets cleanly to the top.
    fn sync_list_state(&self, rows: &[ListRow], context_changed: bool) {
        let mut old = self.shown_rows.borrow_mut();
        if should_reset_list_state(old.len(), rows.len(), context_changed) {
            self.list_state.reset(rows.len());
            *old = rows.to_vec();
        } else if let Some((replaced, count)) = changed_span(&old, rows) {
            self.list_state.splice(replaced, count);
            *old = rows.to_vec();
        }
    }

    fn list_text_px(&self, base: f32) -> Pixels {
        px((base * self.prefs.fonts.list_scale() * 2.).round() / 2.)
    }

    fn list_font_family(&self) -> SharedString {
        SharedString::from(self.prefs.fonts.effective_list_family().to_owned())
    }

    fn render_list_row(&mut self, row: ListRow, compact: bool, cx: &mut Context<Self>) -> AnyElement {
        let p = *cx.palette();
        let list_font = self.list_font_family();
        match row {
            ListRow::Group(label, n) => h_flex()
                .items_center()
                .justify_between()
                .px(px(11.))
                .pt(px(13.))
                .pb(px(5.))
                .font_family(list_font)
                .text_size(self.list_text_px(11.))
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
            MailboxView::Kind(LabelKind::Snoozed) if self.search_query.is_empty() && !self.all_inboxes_active() => {
                let now = overlook_core::time::now();
                let groups: Vec<overlook_core::SnoozeGroup> = self
                    .conversations
                    .iter()
                    .map(|c| overlook_core::snooze::snooze_group(c.snoozed_until.unwrap_or(now), now, &self.time_zone))
                    .collect();
                let mut current: Option<&overlook_core::SnoozeGroup> = None;
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
            _ => {
                let now = overlook_core::time::now();
                let groups: Vec<DayGroup> = self.conversations.iter().map(|c| day_group(c.latest, now, &self.time_zone)).collect();
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
        let row_ref = super::ConversationRef::of(c);
        let selected = self.selected.as_ref() == Some(&row_ref);
        let marked = self.marked.contains(&row_ref);
        // A marked row is tinted like the open one; only the open one gets the edge bar.
        let highlighted = selected || marked;
        let unread = c.is_unread();
        let now = overlook_core::time::now();
        let time = relative_label(c.latest, now, &self.time_zone);
        let who = c.participants.first().cloned().unwrap_or_else(|| self.owner_for(&c.account_id));
        let row_labels = self.labels_for_account(&c.account_id);
        let label_name = c
            .label_ids
            .iter()
            .filter_map(|id| row_labels.iter().find(|l| &l.id == id))
            .find(|l| l.kind == LabelKind::User)
            .map(|l| overlook_core::sidebar::folder_title(&l.name).to_owned());
        let account_badge: Option<AnyElement> = self.all_inboxes_active().then(|| {
            let name = self.account_name(&c.account_id);
            let tip = self
                .known_account(&c.account_id)
                .map(|a| self.account_menu_label(&a))
                .unwrap_or_else(|| name.clone());
            let mark = self.known_account(&c.account_id).map(|a| self.account_mark(&a).render(12., &p));
            h_flex()
                .id(SharedString::from(format!("acct-badge-{}-{}", c.account_id.0, c.id)))
                .flex_shrink(1.)
                .min_w(px(0.))
                .max_w(px(150.))
                .items_center()
                .gap(px(4.))
                .px(px(6.))
                .py(px(1.))
                .rounded(px(5.))
                .bg(if highlighted { p.message_surface().opacity(0.6) } else { p.chip })
                .text_size(self.list_text_px(10.5))
                .line_height(self.list_text_px(15.))
                .text_color(if highlighted { p.selection_text } else { p.muted })
                .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(window, cx))
                .children(mark)
                .child(div().min_w_0().truncate().child(name))
                .into_any_element()
        });
        let conv_ref = row_ref.clone();
        let star_conversation = row_ref.clone();
        let starred = c.starred;
        let pending = c.pending;
        // Dragging one row of a marked set drags the whole set.
        let drag = if marked && self.multi_selected() {
            let n = self.marked.len();
            super::ConversationDrag { mail: super::DraggedMail::Selection, title: format!("{n} conversations"), count: 0 }
        } else {
            super::ConversationDrag { mail: super::DraggedMail::Conversation(row_ref.clone()), title: c.subject.clone(), count: c.message_count }
        };
        let me = cx.entity().downgrade();
        let menu_ref = row_ref.clone();
        let has_junk = row_labels.iter().any(|l| l.kind == LabelKind::Spam);
        let can_delete_forever = self.can_delete_forever_for(&c.account_id);
        // What the row carries under its preview: an invitation's time, its files by name,
        // and its folder.
        let invitation: Vec<AnyElement> = c.invitation.as_ref().filter(|_| !compact).map(|inv| self.invitation_chip(c, inv, cx)).unwrap_or_default();
        let snooze_chip: Option<AnyElement> = c.snoozed_until.map(|ts| self.render_list_snooze_chip(ts, &p));
        let compact_snooze_chip = if compact { snooze_chip } else { None };
        let extra_snooze_chip = if !compact { c.snoozed_until.map(|ts| self.render_list_snooze_chip(ts, &p)) } else { None };
        let extras = !compact && (c.has_attachments || label_name.is_some() || !invitation.is_empty() || extra_snooze_chip.is_some());

        div()
            .id(SharedString::from(format!("conv-{}-{}", c.account_id.0, c.id)))
            .group("conversation-row")
            .relative()
            .h_flex()
            .items_start()
            .gap(px(11.))
            .px(px(11.))
            .py(px(if compact { 8. } else { 12. }))
            .rounded(px(7.))
            .border_b_1()
            .border_color(if highlighted { gpui_kit::transparent_black() } else { p.divider })
            .cursor_pointer()
            .when(highlighted, |d| d.bg(p.selection))
            .when(!highlighted && unread, |d| d.bg(p.list_unread()))
            .when(!highlighted, |d| d.hover(|d| d.bg(p.hover)))
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
            .when(!compact && self.prefs.show_avatars, |d| d.child(avatar(&who, 32., &p)))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(2.))
                    .font_family(self.list_font_family())
                    .child(
                        h_flex()
                            .items_center()
                            .gap(px(8.))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(self.list_text_px(13.))
                                    .font_weight(if unread { FontWeight::BOLD } else { FontWeight::NORMAL })
                                    .text_color(p.foreground)
                                    .child(c.participant_line()),
                            )
                            // How many messages the conversation holds.
                            .when(c.message_count > 1, |d| {
                                d.child(
                                    div()
                                        .flex_none()
                                        .px(px(6.))
                                        .rounded(px(8.))
                                        .bg(if highlighted { p.message_surface().opacity(0.55) } else { p.chip })
                                        .text_size(self.list_text_px(11.))
                                        .line_height(self.list_text_px(16.))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(if highlighted { p.selection_text } else { p.muted })
                                        .child(c.message_count.to_string()),
                                )
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
                                    .text_size(self.list_text_px(11.))
                                    .font_weight(if unread { FontWeight::SEMIBOLD } else { FontWeight::NORMAL })
                                    .text_color(if unread { p.accent } else { p.muted })
                                    .child(time),
                            ),
                    )
                    .child(
                        h_flex()
                            .items_center()
                            .gap(px(6.))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(self.list_text_px(13.))
                                    .font_weight(if unread { FontWeight::BOLD } else { FontWeight::NORMAL })
                                    .text_color(p.foreground)
                                    .child(c.subject.clone()),
                            )
                            .children(compact_snooze_chip)
                            .children(account_badge),
                    )
                    .when_some(
                        (!compact && self.prefs.show_previews)
                            .then(|| overlook_core::clean_snippet(&c.snippet, 160))
                            .filter(|s| !s.is_empty()),
                        |d, snippet| {
                            d.child(
                                div()
                                    .truncate()
                                    .text_size(self.list_text_px(12.))
                                    .text_color(p.muted)
                                    .child(snippet),
                            )
                        },
                    )
                    .when(extras, |d| {
                        let files = &c.attachment_names;
                        // Wraps rather than cutting off what doesn't fit; the list measures
                        // rows again whenever its width changes.
                        d.child(
                            h_flex()
                                .flex_wrap()
                                .items_center()
                                .gap_x(px(5.))
                                .gap_y(px(4.))
                                .mt(px(4.))
                                .min_w_0()
                                .text_size(self.list_text_px(11.))
                                .text_color(p.muted)
                                .children(extra_snooze_chip)
                                .children(invitation)
                                // The files by name and type; a message whose only parts are
                                // pictures in its text keeps the plain "Attachments" note.
                                .when(c.has_attachments && files.is_empty(), |d| {
                                    d.child(icon("paperclip").size(px(12.)).text_color(p.muted)).child("Attachments")
                                })
                                // The first file by name, and how many more: two names would
                                // each be cut to a few letters in a list of usual width.
                                .children(files.first().map(|name| file_chip(name, &p)))
                                .when(files.len() > 1, |d| {
                                    let more = files.len() - 1;
                                    let names = files[1..].join(", ");
                                    d.child(
                                        div()
                                            .id(SharedString::from(format!("more-files-{}-{}", c.account_id.0, c.id)))
                                            .child(chip(format!("+{more}"), &p))
                                            .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(names.clone()).build(window, cx)),
                                    )
                                })
                                .children(label_name.map(|l| chip(l, &p))),
                        )
                    }),
            )
            .child(
                Button::new(SharedString::from(format!("star-{}-{}", c.account_id.0, c.id)))
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
                        this.star_conversations(vec![star_conversation.clone()], !starred, window, cx);
                    })),
            )
            .child(self.row_quick_actions(c, cx))
            .on_drag(drag, |d: &super::ConversationDrag, _pos, _window, cx| {
                let label = if d.count > 1 { format!("{} · {} messages", d.title, d.count) } else { d.title.clone() };
                cx.new(|_| super::DragPreview { label })
            })
            .on_click(cx.listener(move |this, e: &ClickEvent, window, cx| {
                let modifiers = e.modifiers();
                if modifiers.control {
                    this.toggle_marked(conv_ref.clone(), window, cx);
                } else if modifiers.shift {
                    this.extend_marked_to(conv_ref.clone(), window, cx);
                } else if e.click_count() == 2 {
                    this.open_conversation_window(conv_ref.clone(), window, cx);
                } else {
                    this.select_conversation(Some(conv_ref.clone()), window, cx);
                }
            }))
            .context_menu(move |menu, window, cx| Self::row_menu(menu, me.clone(), &menu_ref, has_junk, can_delete_forever, window, cx))
    }

    /// The commands a row offers while the pointer is over it, floating over its time and
    /// star: archive (or restore), delete, read or unread, and star. They act on that row
    /// alone, so whatever is open stays open.
    fn row_quick_actions(&self, c: &ConversationSummary, cx: &mut Context<Self>) -> impl IntoElement {
        let p = *cx.palette();
        let state = super::list_menu::selection_state([c], self.labels_for_account(&c.account_id));
        let (archive_label, archive_icon) = if state.all_in_trash {
            ("Restore", C::Move)
        } else if !state.any_in_inbox {
            ("To Inbox", C::Move)
        } else {
            ("Archive", C::Archive)
        };
        // Offline mail has no Archive, and deleting it is for good: that asks first, from the
        // menu or the Delete key, not from a button under the pointer.
        enum QuickGlyph {
            Color(C),
            Symbol(&'static str),
        }
        let offline = self.offline_active();
        let mut actions: Vec<(RowAction, &'static str, QuickGlyph)> = Vec::new();
        if c.snoozed_until.is_some() && !offline {
            actions.push((RowAction::Snooze, "Change snooze time", QuickGlyph::Symbol("clock")));
            actions.push((RowAction::CancelSnooze, "Cancel snooze", QuickGlyph::Symbol("rotate-ccw")));
        } else {
            if !offline {
                actions.push((RowAction::Archive, archive_label, QuickGlyph::Color(archive_icon)));
            }
            if !offline && !state.all_in_trash && !state.all_in_junk {
                actions.push((RowAction::Snooze, "Snooze", QuickGlyph::Symbol("clock")));
            }
        }
        if !state.all_in_trash && !offline {
            actions.push((RowAction::Delete, "Delete", QuickGlyph::Color(C::Delete)));
        }
        actions.push(if state.any_unread {
            (RowAction::ToggleRead, "Mark read", QuickGlyph::Color(C::Read))
        } else {
            (RowAction::ToggleRead, "Mark unread", QuickGlyph::Color(C::Mail))
        });
        actions.push((RowAction::ToggleStar, if state.all_starred { "Unstar" } else { "Star" }, QuickGlyph::Color(C::Star)));
        let row_ref = super::ConversationRef::of(c);
        h_flex()
            .absolute()
            .top(px(6.))
            .right(px(6.))
            .gap(px(2.))
            .p(px(2.))
            .rounded(px(7.))
            .bg(p.message_surface())
            .border_1()
            .border_color(p.divider)
            .shadow_sm()
            // Hidden elements take no clicks, so the row reads normally until hovered.
            .invisible()
            .group_hover("conversation-row", |s| s.visible())
            .children(actions.into_iter().map(|(action, label, glyph)| {
                let target = row_ref.clone();
                let icon_el = match glyph {
                    QuickGlyph::Color(c) => color_icon(c, 15., &p).into_any_element(),
                    QuickGlyph::Symbol(name) => icon(name).size(px(14.)).text_color(p.action).into_any_element(),
                };
                Button::new(SharedString::from(format!("quick-{action:?}-{}-{}", c.account_id.0, c.id)))
                    .ghost()
                    .xsmall()
                    .w(px(26.))
                    .h(px(24.))
                    .child(icon_el)
                    .accessibility_label(label)
                    .tooltip(label)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.row_action(&target, action, window, cx);
                    }))
            }))
    }

    /// A row's own command. The open conversation acts as the toolbar would, moving on to
    /// the next one after archiving; any other row changes alone and the selection stays.
    pub(super) fn row_action(&mut self, target: &super::ConversationRef, action: RowAction, window: &mut Window, cx: &mut Context<Self>) {
        if self.offline_active() && matches!(action, RowAction::Archive | RowAction::Delete | RowAction::Snooze | RowAction::CancelSnooze) {
            return;
        }
        if matches!(action, RowAction::Snooze) {
            self.open_snooze_modal(vec![target.clone()], window, cx);
            return;
        }
        if matches!(action, RowAction::CancelSnooze) {
            self.cancel_snooze_conversations(vec![target.clone()], window, cx);
            return;
        }
        if self.marked.is_empty() && self.selected.as_ref() == Some(target) {
            match action {
                RowAction::Archive => self.on_archive(&super::ArchiveSelected, window, cx),
                RowAction::Delete => self.on_trash(&super::TrashSelected, window, cx),
                RowAction::ToggleRead => self.on_toggle_read(&super::ToggleRead, window, cx),
                RowAction::ToggleStar => self.on_toggle_star(&super::ToggleStar, window, cx),
                RowAction::Snooze | RowAction::CancelSnooze => unreachable!(),
            }
            return;
        }
        let Some(row) = self.conversations.iter().find(|c| super::ConversationRef::of(c) == *target).cloned() else { return };
        let state = super::list_menu::selection_state([&row], self.labels_for_account(&row.account_id));
        let ids = self.message_ids_in(target);
        match action {
            RowAction::Archive if state.all_in_trash || !state.any_in_inbox => {
                let changes = self.changes_to_inbox_for(&row.account_id, &ids);
                self.perform_for(row.account_id.clone(), "Moved to Inbox", changes, true, window, cx);
            }
            RowAction::Archive => self.perform_for(row.account_id.clone(), "Archived", vec![LocalChange::Archive { message_ids: ids }], true, window, cx),
            RowAction::Delete if !state.all_in_trash => self.perform_for(row.account_id.clone(), "Moved to Trash", vec![LocalChange::Trash { message_ids: ids }], true, window, cx),
            RowAction::Delete => {}
            RowAction::ToggleRead => self.set_read_for(&row.account_id, ids, state.any_unread, window, cx),
            RowAction::ToggleStar => self.star_conversations(vec![target.clone()], !state.all_starred, window, cx),
            RowAction::Snooze | RowAction::CancelSnooze => unreachable!(),
        }
    }

    /// The date of an invitation, and Accept and Decline when the event is on your
    /// calendar and still waits for your answer; once answered, the answer. Separate
    /// pieces, so a narrow row wraps between them rather than cutting one off.
    fn invitation_chip(&self, c: &ConversationSummary, inv: &overlook_core::ics::Invitation, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let p = *cx.palette();
        let event = self.invite_row(&c.account_id, &inv.uid);
        let day = |ts: i64| overlook_core::tz::zoned(ts).strftime("%a %b %-d").to_string();
        let clock = overlook_core::tz::format_clock;
        let when = match &event {
            Some(e) if e.all_day => format!("{} · All day", day(e.start)),
            Some(e) => format!("{} · {}", day(e.start), clock_range(e.start, e.end)),
            None => match inv.start_in_zone(&self.time_zone) {
                Some(s) if inv.all_day => format!("{} · All day", day(s)),
                Some(s) => format!("{} · {}", day(s), clock(s)),
                None => "Invitation".to_owned(),
            },
        };
        let cancelled = inv.is_cancellation();
        let date = h_flex()
            .flex_none()
            .items_center()
            .gap(px(4.))
            .h(px(22.))
            .px(px(6.))
            .rounded(px(6.))
            .bg(p.chip)
            .text_size(crate::ui::ui_px(11.5))
            .font_weight(FontWeight::MEDIUM)
            .text_color(if cancelled { p.error } else { p.foreground })
            .child(icon(if cancelled { "calendar-x" } else { "calendar" }).size(px(12.)).text_color(if cancelled { p.error } else { p.accent }))
            .child(if cancelled { format!("Cancelled · {when}") } else { when });
        let mut row: Vec<AnyElement> = vec![date.into_any_element()];
        if let Some(e) = event.filter(|_| !cancelled) {
            match e.response {
                Some(ResponseStatus::NeedsAction) | None if e.can_respond => {
                    for (label, response) in [("Accept", ResponseStatus::Accepted), ("Decline", ResponseStatus::Declined)] {
                        let event_id = e.event.clone();
                        row.push(
                            // Accept is the answer most invitations get, so it leads: semibold in
                            // the action colour. Decline stays quiet.
                            Button::new(SharedString::from(format!("rsvp-{label}-{}-{}", c.account_id.0, c.id)))
                                .ghost()
                                .xsmall()
                                .flex_none()
                                .h(px(22.))
                                .px(px(6.))
                                .border_1()
                                .border_color(p.control_border.opacity(0.45))
                                .child(div().text_size(crate::ui::ui_px(11.5)).map(|d| if response == ResponseStatus::Accepted {
                                    d.font_weight(FontWeight::SEMIBOLD).text_color(p.accent)
                                } else {
                                    d.font_weight(FontWeight::MEDIUM).text_color(p.muted)
                                }).child(label))
                                .accessibility_label(label)
                                .tooltip(format!("{label} this invitation"))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.respond_to_event(event_id.clone(), response, window, cx);
                                }))
                                .into_any_element(),
                        );
                    }
                }
                Some(response) if response != ResponseStatus::NeedsAction => {
                    let accepted = response == ResponseStatus::Accepted;
                    row.push(
                        h_flex()
                            .items_center()
                            .gap(px(4.))
                            .text_size(crate::ui::ui_px(11.5))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(if accepted { p.success } else { p.muted })
                            .when(accepted, |d| d.child(icon("check").size(px(12.)).text_color(p.success)))
                            .child(response.label())
                            .into_any_element(),
                    );
                }
                _ => {}
            }
        }
        row
    }

    /// The calendar event behind an invitation in the list, read purely from memory
    /// (`self.invite_events` is populated on mail/thread/calendar refresh, never in render).
    fn invite_row(&self, account: &AccountId, uid: &str) -> Option<InviteRow> {
        self.invite_events.get(&(account.clone(), uid.to_owned())).and_then(|e| e.as_ref()).map(|e| InviteRow {
            event: e.id.clone(),
            start: e.start,
            end: e.end,
            all_day: e.all_day,
            can_respond: e.is_invitation(),
            response: e.my_response(),
        })
    }

    fn can_move_to_focused_inbox(&self, target: &super::ConversationRef) -> bool {
        self.inbox_focus() == Some(FocusTab::Other)
            && self.search_query.trim().is_empty()
            && self.position_of(target).is_some()
    }

    /// The right-click menu of a conversation row. The row joins the selection first, so
    /// every item acts on the same rows the toolbar would.
    fn row_menu(
        mut menu: PopupMenu,
        me: WeakEntity<Self>,
        target: &super::ConversationRef,
        has_junk: bool,
        can_delete_forever: bool,
        window: &mut Window,
        cx: &mut Context<PopupMenu>,
    ) -> PopupMenu {
        let state = me.update(cx, |this, cx| {
            this.mark_for_menu(target, window, cx);
            // Offline folders to move server mail into, by name.
            let mut offline_folders: Vec<(LabelId, String)> = if this.offers_offline() && !this.offline_active() {
                this.labels_of(&this.offline.id).iter().filter(|l| l.kind == LabelKind::User).map(|l| (l.id.clone(), Self::offline_folder_name(&l.name))).collect()
            } else {
                Vec::new()
            };
            offline_folders.sort_by_cached_key(|(_, name)| name.to_lowercase());
            let focus_target = this.can_move_to_focused_inbox(target).then(|| target.clone());
            let can_delete = if this.multi_selected() { this.can_delete_forever_selected() } else { can_delete_forever };
            let state = this.selection_state();
            let has_junk = if this.multi_selected() { state.has_junk_label } else { has_junk };
            let snooze_presets = overlook_core::snooze::snooze_presets(
                overlook_core::time::now(),
                &this.time_zone,
                this.prefs.work_start_hour,
            );
            (state, !this.all_inboxes_active() && this.view == MailboxView::Kind(LabelKind::Sent), this.offline_active(), this.offers_offline(), offline_folders, focus_target, has_junk, can_delete, snooze_presets)
        });
        let Ok((state, in_sent, offline, offers_offline, offline_folders, focus_target, has_junk, can_delete_forever, snooze_presets)) = state else { return menu };
        let multi = state.count > 1;
        let act = |me: &WeakEntity<Self>, f: fn(&mut Self, &mut Window, &mut Context<Self>)| {
            let me = me.clone();
            move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                me.update(cx, |this, cx| f(this, window, cx)).ok();
            }
        };
        if !multi {
            let open_ref = target.clone();
            let open_me = me.clone();
            menu = menu
                .item(PopupMenuItem::new("Open").icon(icon("mail-open")).on_click(move |_, window, cx| {
                    open_me
                        .update(cx, |this, cx| this.open_conversation_window(open_ref.clone(), window, cx))
                        .ok();
                }))
                .separator()
                .item(PopupMenuItem::new("Reply").icon(icon("reply")).action(Box::new(super::Reply)).on_click(act(&me, |this, window, cx| {
                    this.select_conversation(this.selected.clone(), window, cx);
                    this.reply_default(window, cx);
                })))
                .item(PopupMenuItem::new("Reply All").icon(icon("reply-all")).action(Box::new(super::ReplyAll)).on_click(act(&me, |this, window, cx| {
                    this.select_conversation(this.selected.clone(), window, cx);
                    this.reply(DraftKind::ReplyAll, window, cx);
                })))
                .item(PopupMenuItem::new("Forward").icon(icon("forward")).action(Box::new(super::Forward)).on_click(act(&me, |this, window, cx| {
                    this.select_conversation(this.selected.clone(), window, cx);
                    this.reply(DraftKind::Forward, window, cx);
                })));
            if in_sent {
                // A Sent conversation can also hold the replies that came back: resend the
                // newest message of our own.
                menu = menu.item(PopupMenuItem::new("Resend This Message…").icon(icon("send")).on_click(act(&me, |this, window, cx| {
                    this.select_conversation(this.selected.clone(), window, cx);
                    let source = this.thread.iter().filter(|m| m.is_outgoing).max_by_key(|m| m.date)
                        .or_else(|| this.thread.iter().max_by_key(|m| m.date))
                        .cloned();
                    if let Some(m) = source { this.resend_message(m, window, cx); }
                })));
            }
            menu = menu.separator();
        }
        let (read_label, read_icon) = if state.any_unread { ("Mark as Read", "mail-open") } else { ("Mark as Unread", "mail") };
        menu = menu.item(PopupMenuItem::new(read_label).icon(icon(read_icon)).action(Box::new(super::ToggleRead)).on_click(act(&me, |this, window, cx| {
            this.on_toggle_read(&super::ToggleRead, window, cx);
        })));
        let (star_label, star_icon) = if state.all_starred { ("Unstar", "star-off") } else { ("Star", "star") };
        menu = menu.item(PopupMenuItem::new(star_label).icon(icon(star_icon)).action(Box::new(super::ToggleStar)).on_click(act(&me, |this, window, cx| {
            this.on_toggle_star(&super::ToggleStar, window, cx);
        })));
        menu = Self::append_snooze_menu_items(menu, me.clone(), &state, offline, snooze_presets, window, cx);
        menu = menu.separator();
        if let Some(conversation) = focus_target.filter(|_| !multi) {
            let me = me.clone();
            menu = menu.item(PopupMenuItem::new("Move to Focused Inbox").icon(icon("inbox")).on_click(move |_, window, cx| {
                me.update(cx, |this, cx| {
                    if this.can_move_to_focused_inbox(&conversation) {
                        this.move_conversation_focus(&conversation, true, true, window, cx);
                    }
                }).ok();
            }));
        }
        menu = menu.item(PopupMenuItem::new("Move to…").icon(icon("folder-input")).action(Box::new(super::MoveSelected)).on_click(act(&me, |this, window, cx| {
            this.open_move_palette(window, cx);
        })));
        if offers_offline && !offline {
            let folders = offline_folders.clone();
            let me = me.clone();
            menu = menu.submenu_with_icon(Some(icon(super::sidebar::OFFLINE_FOLDER_ICON)), "Move to Offline Folder", window, cx, move |mut sub, _window, _cx| {
                for (label_id, name) in &folders {
                    let (me, label_id) = (me.clone(), label_id.clone());
                    sub = sub.item(PopupMenuItem::new(name.clone()).on_click(move |_, window, cx| {
                        let drag = super::ConversationDrag { mail: super::DraggedMail::Selection, title: String::new(), count: 0 };
                        me.update(cx, |this, cx| this.drop_on_offline(drag, label_id.clone(), window, cx)).ok();
                    }));
                }
                if !folders.is_empty() {
                    sub = sub.separator();
                }
                let me = me.clone();
                sub.item(PopupMenuItem::new("New Offline Folder…").on_click(move |_, window, cx| {
                    me.update(cx, |this, cx| {
                        let account = this.offline.id.clone();
                        this.open_folder_prompt(account, super::folder_manage::FolderPromptMode::New { parent: None }, window, cx);
                    })
                    .ok();
                }))
            });
        }
        let (archive_label, archive_icon) = if state.all_in_trash {
            ("Restore", "archive-restore")
        } else if !state.any_in_inbox {
            ("To Inbox", "inbox")
        } else {
            ("Archive", "archive")
        };
        if !offline {
            menu = menu.item(PopupMenuItem::new(archive_label).icon(icon(archive_icon)).action(Box::new(super::ArchiveSelected)).on_click(act(&me, |this, window, cx| {
                this.on_archive(&super::ArchiveSelected, window, cx);
            })));
        }
        if has_junk {
            if state.all_in_junk {
                menu = menu.item(PopupMenuItem::new("Not Junk").icon(icon("shield-check")).on_click(act(&me, |this, window, cx| {
                    this.move_target_to_inbox(window, cx);
                })));
            } else {
                menu = menu.item(PopupMenuItem::new("Move to Junk").icon(icon("shield-alert")).on_click(act(&me, |this, window, cx| {
                    let drag = super::ConversationDrag { mail: super::DraggedMail::Selection, title: String::new(), count: 0 };
                    this.drop_on_view(drag, MailboxView::Kind(LabelKind::Spam), window, cx);
                })));
            }
        }
        // Offline mail has no Trash: Delete Permanently is the one way to delete it.
        if !state.all_in_trash && !offline {
            menu = menu.item(PopupMenuItem::new("Delete").icon(icon("trash")).action(Box::new(super::TrashSelected)).on_click(act(&me, |this, window, cx| {
                this.on_trash(&super::TrashSelected, window, cx);
            })));
        }
        if can_delete_forever {
            menu = menu.item(PopupMenuItem::new("Delete Permanently…").icon(icon("shredder")).action(Box::new(super::DeleteForeverSelected)).on_click(act(&me, |this, window, cx| {
                this.on_delete_forever(&super::DeleteForeverSelected, window, cx);
            })));
        }
        if !multi {
            menu = menu.separator().item(PopupMenuItem::new("Print…").icon(icon("printer")).on_click(act(&me, |this, window, cx| {
                this.select_conversation(this.selected.clone(), window, cx);
                if let Some(m) = this.thread.iter().max_by_key(|m| m.date).cloned() {
                    this.print_message(m, window, cx);
                }
            })));
        }
        menu
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
                    .font_family(self.list_font_family())
                    .child(
                        h_flex()
                            .items_center()
                            .gap(px(8.))
                            .child(div().flex_1().min_w_0().truncate().text_size(self.list_text_px(13.)).font_weight(FontWeight::MEDIUM).child(recipients))
                            .child(div().text_size(self.list_text_px(11.)).text_color(p.muted).child(relative_label(d.updated_at, now, &self.time_zone))),
                    )
                    .child(div().truncate().text_size(self.list_text_px(13.)).child(d.display_subject()))
                    .child(div().truncate().text_size(self.list_text_px(12.)).text_color(p.muted).child(if preview.is_empty() { "Draft in progress".to_owned() } else { preview }))
                    .child(
                        h_flex()
                            .items_center()
                            .gap(px(6.))
                            .mt(px(4.))
                            .text_size(self.list_text_px(11.))
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
            .on_click(cx.listener(move |this, e: &ClickEvent, window, cx| {
                if outbox {
                    if this.outbox_selected.as_ref() != Some(&draft_id) {
                        this.select_outbox_item(draft_id.clone(), window, cx);
                    }
                    if e.click_count() == 2 {
                        this.open_outbox_window(draft_id.clone(), window, cx);
                    }
                } else {
                    if !this.composer.as_ref().is_some_and(|c| c.draft.id == draft_id) {
                        this.open_draft(draft_id.clone(), window, cx);
                    }
                    if e.click_count() == 2 {
                        this.pop_out_composer(window, cx);
                    }
                }
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::changed_span;
    // The parent's GPUI glob also exports a `test` proc macro.
    use std::prelude::v1::test;

    #[test]
    fn an_unchanged_list_replaces_nothing() {
        assert_eq!(changed_span(&[1, 2, 3], &[1, 2, 3]), None);
        assert_eq!(changed_span::<u8>(&[], &[]), None);
    }

    #[test]
    fn only_the_rows_that_differ_are_replaced() {
        // New mail on top, mail removed from the end, one row changed in the middle.
        assert_eq!(changed_span(&[2, 3, 4], &[1, 2, 3, 4]), Some((0..0, 1)));
        assert_eq!(changed_span(&[1, 2, 3, 4], &[1, 2, 3]), Some((3..4, 0)));
        assert_eq!(changed_span(&[1, 2, 3], &[1, 9, 3]), Some((1..2, 1)));
        // Rows removed from the middle.
        assert_eq!(changed_span(&[1, 2, 3, 4], &[1, 4]), Some((1..3, 0)));
    }

    #[test]
    fn repeated_rows_do_not_make_the_start_and_end_overlap() {
        // The shared prefix and suffix must not claim the same row twice.
        assert_eq!(changed_span(&[1, 2, 2, 3], &[1, 2, 3]), Some((2..3, 0)));
        assert_eq!(changed_span(&[1, 2, 3], &[1, 2, 2, 3]), Some((2..2, 1)));
        assert_eq!(changed_span(&[7, 7], &[7, 7, 7]), Some((2..2, 1)));
    }

    #[test]
    fn a_list_that_empties_or_fills_is_replaced_whole() {
        assert_eq!(changed_span(&[1, 2], &[]), Some((0..2, 0)));
        assert_eq!(changed_span::<u8>(&[], &[1, 2]), Some((0..0, 2)));
        assert_eq!(changed_span(&[1, 2], &[3, 4, 5]), Some((0..2, 3)));
    }

    #[test]
    fn switching_views_or_leaving_an_empty_list_resets_scroll_to_top() {
        assert!(super::should_reset_list_state(0, 5, false));
        assert!(super::should_reset_list_state(10, 5, true));
        assert!(super::should_reset_list_state(10, 0, true));
        assert!(!super::should_reset_list_state(10, 11, false));
        assert!(!super::should_reset_list_state(0, 0, false));
    }
}
