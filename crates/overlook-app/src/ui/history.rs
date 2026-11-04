//! Undo, Ctrl+Z and the change history ("Time Machine").
//!
//! Every undoable action is recorded in the store as a [`HistoryEntry`] with the data that
//! takes it back. The toast's Undo button, Ctrl+Z and the History view all undo through the
//! same entries, and an entry is claimed in the store before it is undone, so it is undone
//! once however it is asked for.

use super::*;
use overlook_core::time::{civil_in_zone, day_group, relative_label, DayGroup};
use overlook_store::{reversal_for, HistoryEntry, Reversal, HISTORY_MAX_ENTRIES};

/// The time stamped on the next history entry: now, but always after the last one, so the
/// order of the entries is the order of the actions even when their writes land otherwise.
fn action_time() -> overlook_core::time::Timestamp {
    use std::sync::atomic::{AtomicI64, Ordering};
    static LAST: AtomicI64 = AtomicI64::new(0);
    let now = now();
    let prev = LAST.try_update(Ordering::SeqCst, Ordering::SeqCst, |last| Some(now.max(last + 1))).unwrap_or(0);
    now.max(prev + 1)
}

/// What undoing one entry did: finished at once, or still running against the server.
pub(super) enum UndoStep {
    Done(bool),
    Pending(Task<bool>),
}

/// Tags undo toasts (keyed by entry id), so an undo from anywhere can remove its toast.
pub(crate) struct UndoToast;

pub(crate) struct HistoryState {
    account: AccountId,
    entries: Vec<HistoryEntry>,
    loading: bool,
}

impl MailWindow {
    /// "Subject" or "Subject and 2 more", for the conversations `ids` belong to.
    fn describe_messages(&self, ids: &[MessageId]) -> String {
        let mut seen: Vec<&ConversationId> = Vec::new();
        let mut first: Option<&str> = None;
        for m in ids.iter().filter_map(|id| self.mail_index.message(id)) {
            if !seen.contains(&&m.conversation_id) {
                seen.push(&m.conversation_id);
                first.get_or_insert(m.subject.as_str());
            }
        }
        let subject = match first.map(str::trim) {
            Some("") => "(no subject)".to_owned(),
            Some(s) => format!("“{s}”"),
            None => format!("{} messages", ids.len()),
        };
        match seen.len() {
            0 | 1 => subject,
            n => format!("{subject} and {} more", n - 1),
        }
    }

    /// Makes `changes` as one undoable action of the open account, and records how to take
    /// it back. With `toast`, an Undo toast says what happened.
    pub(crate) fn perform(&mut self, title: impl Into<String>, changes: Vec<LocalChange>, toast: bool, window: &mut Window, cx: &mut Context<Self>) {
        let changes: Vec<LocalChange> = changes.into_iter().filter(|c| !c.message_ids().is_empty()).collect();
        if changes.is_empty() {
            return;
        }
        let mut ids: Vec<MessageId> = changes.iter().flat_map(|c| c.message_ids().iter().cloned()).collect();
        ids.dedup();
        let detail = self.describe_messages(&ids);
        // From the index as it is before the change, so only what changes is reversed.
        let reversal = reversal_for(&changes, &self.mail_index);
        for change in changes {
            self.apply_local(change, window, cx);
        }
        let title = title.into();
        let toast = toast.then(|| title.clone());
        self.record(self.account.id.clone(), title, detail, Reversal::Changes { changes: reversal }, toast, window, cx);
    }

    /// Records an action in the background. The Undo toast (`toast` is its text) appears
    /// once the entry has its id.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record(&mut self, account: AccountId, title: String, detail: String, reversal: Reversal, toast: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let store = self.store.clone();
        let at = action_time();
        cx.spawn_in(window, async move |this, cx| {
            let id = cx.background_spawn(async move { store.record_history(&account, at, &title, &detail, &reversal) }).await;
            let _ = this.update_in(cx, |this, window, cx| {
                match id {
                    Ok(id) => {
                        if let Some(text) = toast {
                            this.toast_with_undo(text, id, window, cx);
                        }
                    }
                    Err(e) => tracing::warn!("could not record the change for undo: {e}"),
                }
                this.reload_history(window, cx);
            });
        })
        .detach();
    }

    /// Shows a toast with an Undo button for history entry `id`.
    pub(crate) fn toast_with_undo(&self, text: impl Into<SharedString>, id: i64, window: &mut Window, cx: &mut Context<Self>) {
        let this = cx.entity().downgrade();
        window.push_notification(
            Notification::info(text)
                .id1::<UndoToast>(id as usize)
                .action(move |_note, _window, _cx| {
                    let this = this.clone();
                    Button::new("undo").xsmall().label("Undo").on_click(move |_, window, cx| {
                        let _ = this.update(cx, |t, cx| t.undo_entry(id, window, cx));
                    })
                })
                .autohide(true),
            cx,
        );
    }

    /// A mailbox change for `account`, which need not be the open one.
    pub(super) fn apply_local_for(&mut self, account: &AccountId, change: LocalChange, window: &mut Window, cx: &mut Context<Self>) {
        if account == &self.account.id {
            self.apply_local(change, window, cx);
        } else {
            self.local_writes.push_back((account.clone(), LocalWrite::Change(change)));
            self.write_local_changes(window, cx);
        }
    }

    /// Undoes one entry (its toast's Undo button, or the History view).
    pub(crate) fn undo_entry(&mut self, id: i64, window: &mut Window, cx: &mut Context<Self>) {
        let store = self.store.clone();
        self.claim_and_undo(move || store.begin_undo(id).map(|e| e.into_iter().collect()), window, cx);
    }

    /// Ctrl+Z: undoes the newest change of the open account that can still be undone.
    pub(crate) fn undo_last(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (store, account) = (self.store.clone(), self.account.id.clone());
        self.claim_and_undo(move || store.begin_undo_last(&account).map(|e| e.into_iter().collect()), window, cx);
    }

    /// "Restore to here": undoes entry `id` and every change after it.
    pub(crate) fn restore_to(&mut self, id: i64, window: &mut Window, cx: &mut Context<Self>) {
        let (store, account) = (self.store.clone(), self.account.id.clone());
        self.claim_and_undo(move || store.begin_undo_since(&account, id), window, cx);
    }

    fn claim_and_undo(&mut self, claim: impl FnOnce() -> overlook_store::Result<Vec<HistoryEntry>> + Send + 'static, window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |this, cx| {
            let claimed = cx.background_spawn(async move { claim() }).await;
            let _ = this.update_in(cx, |this, window, cx| match claimed {
                Ok(entries) if entries.is_empty() => crate::ui::toast(window, cx, crate::notify::Level::Info, "Nothing to undo."),
                Ok(entries) => this.run_undo(entries, window, cx),
                Err(e) => crate::ui::toast(window, cx, crate::notify::Level::Error, format!("Couldn’t undo: {e}")),
            });
        })
        .detach();
    }

    /// Takes back claimed entries, newest first, one at a time: a folder restore finishes
    /// before the next starts, so a folder's parent is back before the folder needs it.
    fn run_undo(&mut self, entries: Vec<HistoryEntry>, window: &mut Window, cx: &mut Context<Self>) {
        let store = self.store.clone();
        cx.spawn_in(window, async move |this, cx| {
            let (total, first_title) = (entries.len(), entries.first().map(|e| e.title.clone()));
            let mut undone = 0;
            for claimed in entries {
                // Re-read: an earlier step can have moved a folder this one refers to.
                let fresh = cx.background_spawn({
                    let (store, id) = (store.clone(), claimed.id);
                    async move { store.history_entry(id) }
                })
                .await;
                let entry = fresh.ok().flatten().unwrap_or(claimed);
                let Ok(step) = this.update_in(cx, |this, window, cx| this.undo_one(entry, window, cx)) else { return };
                let ok = match step {
                    UndoStep::Done(ok) => ok,
                    UndoStep::Pending(task) => task.await,
                };
                if ok {
                    undone += 1;
                }
            }
            let _ = this.update_in(cx, |this, window, cx| {
                match (undone, first_title) {
                    (0, _) => {}
                    (1, Some(title)) if total == 1 => crate::ui::toast(window, cx, crate::notify::Level::Info, format!("Undid: {title}")),
                    (n, _) => crate::ui::toast(window, cx, crate::notify::Level::Info, format!("Restored {n} changes.")),
                }
                this.reload_history(window, cx);
            });
        })
        .detach();
    }

    fn undo_one(&mut self, entry: HistoryEntry, window: &mut Window, cx: &mut Context<Self>) -> UndoStep {
        window.remove_notification1::<UndoToast>(entry.id as usize, cx);
        match &entry.reversal {
            Reversal::Changes { changes } => {
                for change in changes.iter().cloned() {
                    self.apply_local_for(&entry.account_id, change, window, cx);
                }
                UndoStep::Done(true)
            }
            Reversal::CancelSend { draft_id } => match self.store.cancel_send(&entry.account_id, draft_id) {
                Ok(draft) => {
                    if self.account.id != draft.account_id {
                        self.switch_account(draft.account_id, window, cx);
                    }
                    self.reload(cx);
                    self.open_draft(draft_id.clone(), window, cx);
                    UndoStep::Done(true)
                }
                Err(e) => {
                    // It has left: it stays in the timeline, no longer offered.
                    if let Err(e) = self.store.forget_reversal(entry.id) {
                        tracing::warn!("could not update the change history: {e}");
                    }
                    crate::ui::toast(window, cx, crate::notify::Level::Warning, format!("Too late to undo: {e}"));
                    UndoStep::Done(false)
                }
            },
            Reversal::RestoreFolder { .. } | Reversal::RecreateLabel { .. } => UndoStep::Pending(self.undo_folder_change(entry, window, cx)),
            Reversal::None => UndoStep::Done(false),
        }
    }

    // ------------------------------------------------------------ history view

    pub(crate) fn open_history(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.history = Some(HistoryState { account: self.account.id.clone(), entries: Vec::new(), loading: true });
        self.reload_history(window, cx);
        cx.notify();
    }

    pub(crate) fn close_history(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.history = None;
        self.list_focus.focus(window, cx);
        cx.notify();
    }

    /// Re-reads the open History view's entries; nothing when it is closed.
    pub(crate) fn reload_history(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(account) = self.history.as_ref().map(|h| h.account.clone()) else { return };
        let store = self.store.clone();
        cx.spawn_in(window, async move |this, cx| {
            let entries = cx.background_spawn({
                let account = account.clone();
                async move { store.history(&account, HISTORY_MAX_ENTRIES) }
            })
            .await;
            let _ = this.update(cx, |this, cx| {
                if let Some(h) = this.history.as_mut().filter(|h| h.account == account) {
                    h.loading = false;
                    match entries {
                        Ok(entries) => h.entries = entries,
                        Err(e) => tracing::warn!("could not read the change history: {e}"),
                    }
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(crate) fn render_history(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = *cx.palette();
        let Some(state) = self.history.as_ref() else { return div().into_any_element() };
        let now = now();
        let mut list = v_flex().gap(px(2.));
        let mut group: Option<String> = None;
        for entry in &state.entries {
            let label = match day_group(entry.at, now, &self.time_zone) {
                DayGroup::Earlier => relative_label(entry.at, now, &self.time_zone),
                g => g.label().to_owned(),
            };
            if group.as_deref() != Some(label.as_str()) {
                list = list.child(div().pt(px(10.)).pb(px(4.)).text_size(px(11.)).font_weight(FontWeight::SEMIBOLD).text_color(p.muted).child(label.clone()));
                group = Some(label);
            }
            list = list.child(self.render_history_row(entry, &p, cx));
        }
        let body = if state.loading {
            div().py(px(24.)).text_color(p.muted).child("Loading…").into_any_element()
        } else if state.entries.is_empty() {
            div().py(px(24.)).text_color(p.muted).child("Nothing changed yet. Archiving, moving, deleting, starring and sending appear here.").into_any_element()
        } else {
            list.into_any_element()
        };
        div()
            .id("history-overlay")
            .absolute()
            .inset_0()
            .occlude()
            .bg(gpui_kit::black().opacity(if p.is_dark() { 0.55 } else { 0.3 }))
            .flex()
            .items_center()
            .justify_center()
            .on_click(cx.listener(|this, _, window, cx| this.close_history(window, cx)))
            .child(
                v_flex()
                    .id("history")
                    .w(px(600.))
                    .max_w(relative(0.94))
                    .h(relative(0.8))
                    .rounded(px(12.))
                    .bg(p.surface)
                    .border_1()
                    .border_color(p.divider)
                    .shadow_lg()
                    .p(px(20.))
                    .gap(px(8.))
                    .on_click(|_, _, cx| cx.stop_propagation())
                    .child(
                        h_flex()
                            .items_center()
                            .child(div().flex_1().min_w_0().truncate().text_size(px(16.)).font_weight(FontWeight::SEMIBOLD).child("Change history"))
                            .child(kbd("Esc", &p)),
                    )
                    .child(div().text_size(px(12.)).text_color(p.muted).child(format!(
                        "{} · Undo one change, or restore to a point to take back everything after it. Ctrl+Z undoes the latest.",
                        self.account_name(&state.account)
                    )))
                    .child(div().id("history-list").flex_1().min_h_0().overflow_y_scroll().child(body)),
            )
            .into_any_element()
    }

    fn render_history_row(&self, entry: &HistoryEntry, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let (_, time, _) = civil_in_zone(entry.at, &self.time_zone);
        let undone = entry.undone_at.is_some();
        let id = entry.id;
        let status: AnyElement = if entry.can_undo() {
            h_flex()
                .gap(px(4.))
                .child(Button::new(SharedString::from(format!("history-undo-{id}"))).ghost().xsmall().label("Undo").on_click(cx.listener(move |this, _, window, cx| this.undo_entry(id, window, cx))))
                .child(Button::new(SharedString::from(format!("history-restore-{id}"))).ghost().xsmall().label("Restore to here").on_click(cx.listener(move |this, _, window, cx| this.restore_to(id, window, cx))))
                .into_any_element()
        } else {
            let text = if undone { "Undone" } else { "Can’t be undone" };
            div().text_size(px(11.)).text_color(p.muted).child(text).into_any_element()
        };
        h_flex()
            .id(SharedString::from(format!("history-row-{id}")))
            .items_center()
            .gap(px(12.))
            .px(px(8.))
            .py(px(6.))
            .rounded(px(6.))
            .hover(|s| s.bg(p.hover))
            .child(div().w(px(64.)).flex_none().text_size(px(11.)).text_color(p.muted).child(format_clock(time)))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .when(undone, |d| d.opacity(0.55))
                    .child(div().truncate().font_weight(FontWeight::MEDIUM).when(undone, |d| d.line_through()).child(entry.title.clone()))
                    .child(div().truncate().text_size(px(12.)).text_color(p.muted).child(entry.detail.clone())),
            )
            .child(div().flex_none().child(status))
            .into_any_element()
    }
}
