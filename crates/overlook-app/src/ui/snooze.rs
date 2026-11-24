//! Device-local email snooze UI: presets, custom date & time modal, reader banner,
//! list chip, context menu submenu, and periodic wakeup checks.

use super::*;
use crate::assets::icon;
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use overlook_core::snooze::{
    default_custom_snooze_inputs, format_snooze_until, parse_custom_snooze, snooze_presets,
};

pub(crate) struct SnoozeModal {
    pub(super) targets: Vec<ConversationRef>,
    pub(super) heading: String,
    pub(super) existing_wake_at: Option<Timestamp>,
    pub(super) date_input: Entity<InputState>,
    pub(super) time_input: Entity<InputState>,
    pub(super) error: Option<String>,
    _subs: Vec<Subscription>,
}

impl MailWindow {
    /// Active snooze wake timestamp for `target`, read purely from memory.
    pub(crate) fn snoozed_until(&self, target: &ConversationRef) -> Option<Timestamp> {
        self.index_for(&target.account_id)
            .and_then(|idx| idx.snoozed_until(&target.id))
    }

    /// Whether a single conversation can be snoozed (server-backed mail account, not offline).
    pub(crate) fn can_snooze_account(&self, account: &AccountId) -> bool {
        !self.is_offline(account) && self.known_account(account).is_some()
    }

    fn snooze_modal_targets(&self) -> Vec<ConversationRef> {
        let targets = self.target_conversations();
        if !targets.is_empty() {
            return targets;
        }
        self.active_summary().map(|s| vec![ConversationRef::of(&s)]).unwrap_or_default()
    }

    /// Opens the Snooze picker for the currently selected conversation(s).
    pub(crate) fn on_snooze_selected(
        &mut self,
        _: &SnoozeSelected,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.module != Module::Mail {
            return;
        }
        let targets = self.snooze_modal_targets();
        self.open_snooze_modal(targets, window, cx);
    }

    /// Cancels snooze for any snoozed conversation(s) in the current selection.
    pub(crate) fn on_unsnooze_selected(
        &mut self,
        _: &UnsnoozeSelected,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.module != Module::Mail {
            return;
        }
        let targets: Vec<ConversationRef> = self
            .snooze_modal_targets()
            .into_iter()
            .filter(|t| self.snoozed_until(t).is_some())
            .collect();
        if !targets.is_empty() {
            self.cancel_snooze_conversations(targets, window, cx);
        }
    }

    /// Snoozes (or reschedules) `targets` until `wake_at`, mirroring optimistically on
    /// in-memory `MailIndex`es and persisting via the serialized `local_writes` queue.
    pub(crate) fn snooze_conversations(
        &mut self,
        targets: Vec<ConversationRef>,
        wake_at: Timestamp,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let now_ms = now();
        if wake_at <= now_ms {
            crate::ui::toast(
                window,
                cx,
                crate::notify::Level::Warning,
                "Choose a date and time in the future.",
            );
            return;
        }
        let eligible: Vec<ConversationRef> = targets
            .into_iter()
            .filter(|t| self.can_snooze_account(&t.account_id))
            .collect();
        if eligible.is_empty() {
            return;
        }
        let affects_selected = self
            .selected
            .as_ref()
            .is_some_and(|sel| eligible.contains(sel));
        let next = affects_selected.then(|| self.removal_neighbour()).flatten();
        let count = eligible.len();
        for target in eligible {
            if let Some(idx) = self.index_for_mut(&target.account_id) {
                idx.set_snooze(target.id.clone(), wake_at);
            }
            self.queue_local_write(
                target.account_id,
                LocalWrite::Snooze {
                    conversation_id: target.id,
                    wake_at,
                },
            );
        }
        self.mail_index_generation += 1;
        self.refresh_mail();
        if affects_selected {
            self.select_after_removal(next, window, cx);
        }
        self.write_local_changes(window, cx);
        let until = format_snooze_until(wake_at, now_ms, &self.time_zone);
        let msg = if count == 1 {
            format!("Snoozed until {until}.")
        } else {
            format!("Snoozed {count} conversations until {until}.")
        };
        crate::ui::toast(window, cx, crate::notify::Level::Success, msg);
        cx.notify();
    }

    /// Cancels active snoozes for `targets`, immediately returning them to Inbox.
    pub(crate) fn cancel_snooze_conversations(
        &mut self,
        targets: Vec<ConversationRef>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let snoozed: Vec<ConversationRef> = targets
            .into_iter()
            .filter(|t| self.snoozed_until(t).is_some())
            .collect();
        if snoozed.is_empty() {
            return;
        }
        let affects_selected = self
            .selected
            .as_ref()
            .is_some_and(|sel| snoozed.contains(sel));
        let next = affects_selected.then(|| self.removal_neighbour()).flatten();
        let count = snoozed.len();
        for target in snoozed {
            if let Some(idx) = self.index_for_mut(&target.account_id) {
                idx.clear_snooze(&target.id);
            }
            self.queue_local_write(
                target.account_id,
                LocalWrite::CancelSnooze {
                    conversation_id: target.id,
                },
            );
        }
        self.mail_index_generation += 1;
        self.refresh_mail();
        if affects_selected {
            self.select_after_removal(next, window, cx);
        }
        self.write_local_changes(window, cx);
        let msg = if count == 1 {
            "Returned to Inbox.".to_owned()
        } else {
            format!("Returned {count} conversations to Inbox.")
        };
        crate::ui::toast(window, cx, crate::notify::Level::Success, msg);
        cx.notify();
    }

    /// Checks SQLite for due snoozes on a background thread and updates views when any wake up.
    pub(crate) fn check_due_snoozes(&mut self, cx: &mut Context<Self>) {
        let store = self.store.clone();
        let now_ms = now();
        cx.spawn(async move |this, cx| {
            let woken = cx
                .background_spawn(async move { store.wake_due_snoozes(now_ms).unwrap_or_default() })
                .await;
            if woken.is_empty() {
                return;
            }
            let _ = this.update(cx, |this, cx| {
                let mut affected_accounts: BTreeSet<AccountId> = BTreeSet::new();
                for s in &woken {
                    affected_accounts.insert(s.account_id.clone());
                    if let Some(idx) = this.index_for_mut(&s.account_id) {
                        idx.clear_snooze(&s.conversation_id);
                    }
                }
                this.mail_index_generation += 1;
                this.mark_counts_stale(affected_accounts.iter().cloned());
                if affected_accounts.contains(&this.account.id) {
                    this.rebuild_mail_index(cx);
                }
                let active = this.account.id.clone();
                for acct in affected_accounts.iter().filter(|a| **a != active) {
                    if this.all_inboxes_active() || this.other_indexes.contains_key(acct) {
                        this.rebuild_other_account_index(acct.clone(), cx);
                    }
                }
                this.refresh_mail();
                if !this.is_message_window() {
                    this.refresh_account_counts(cx);
                }
                this.reload_folder_windows(affected_accounts, cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Opens the Snooze picker modal (presets + custom date & time inputs).
    pub(crate) fn open_snooze_modal(
        &mut self,
        targets: Vec<ConversationRef>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let eligible: Vec<ConversationRef> = targets
            .into_iter()
            .filter(|t| self.can_snooze_account(&t.account_id))
            .collect();
        if eligible.is_empty() {
            return;
        }
        let existing_wake_at = if eligible.len() == 1 {
            self.snoozed_until(&eligible[0])
        } else {
            None
        };
        let heading = if eligible.len() == 1 {
            self.conversations
                .iter()
                .find(|c| ConversationRef::of(c) == eligible[0])
                .map(|c| c.subject.clone())
                .or_else(|| self.active_summary().map(|s| s.subject))
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| "(No subject)".to_owned())
        } else {
            format!("{} conversations", eligible.len())
        };
        let (default_date, default_time) = default_custom_snooze_inputs(
            now(),
            &self.time_zone,
            self.prefs.work_start_hour,
            existing_wake_at,
        );
        let date_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("YYYY-MM-DD")
                .default_value(default_date)
        });
        let time_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("9:00 AM")
                .default_value(default_time)
        });
        let sub_date = cx.subscribe_in(
            &date_input,
            window,
            |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::PressEnter { .. } => this.submit_snooze_modal(window, cx),
                InputEvent::Change => {
                    if let Some(m) = this.snooze_modal.as_mut() {
                        m.error = None;
                    }
                    cx.notify();
                }
                _ => {}
            },
        );
        let sub_time = cx.subscribe_in(
            &time_input,
            window,
            |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::PressEnter { .. } => this.submit_snooze_modal(window, cx),
                InputEvent::Change => {
                    if let Some(m) = this.snooze_modal.as_mut() {
                        m.error = None;
                    }
                    cx.notify();
                }
                _ => {}
            },
        );
        date_input.update(cx, |s, cx| s.focus(window, cx));
        self.snooze_modal = Some(SnoozeModal {
            targets: eligible,
            heading,
            existing_wake_at,
            date_input,
            time_input,
            error: None,
            _subs: vec![sub_date, sub_time],
        });
        cx.notify();
    }

    pub(crate) fn close_snooze_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.snooze_modal.take().is_some() {
            self.focus_current_module(window, cx);
            cx.notify();
        }
    }

    fn submit_snooze_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(modal) = self.snooze_modal.as_ref() else {
            return;
        };
        let date_str = modal.date_input.read(cx).value().to_string();
        let time_str = modal.time_input.read(cx).value().to_string();
        let targets = modal.targets.clone();
        match parse_custom_snooze(&date_str, &time_str, now(), &self.time_zone) {
            Ok(wake_at) => {
                self.close_snooze_modal(window, cx);
                self.snooze_conversations(targets, wake_at, window, cx);
            }
            Err(err) => {
                if let Some(modal) = self.snooze_modal.as_mut() {
                    modal.error = Some(err.to_string());
                }
                cx.notify();
            }
        }
    }

    /// Subtle chip shown on snoozed conversation rows in the message list.
    pub(crate) fn render_list_snooze_chip(&self, wake_at: Timestamp, p: &Palette) -> AnyElement {
        let label = format_snooze_until(wake_at, now(), &self.time_zone);
        h_flex()
            .flex_none()
            .items_center()
            .gap(px(4.))
            .h(px(20.))
            .px(px(6.))
            .rounded(px(5.))
            .bg(p.chip)
            .text_size(ui_px(11.))
            .font_weight(FontWeight::MEDIUM)
            .text_color(p.accent)
            .child(icon("clock").size(px(11.)).text_color(p.accent))
            .child(label)
            .into_any_element()
    }

    /// Banner shown in the reading pane header whenever the open conversation is snoozed.
    pub(crate) fn render_reader_snooze_banner(
        &self,
        summary: &ConversationSummary,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let wake_at = summary.snoozed_until?;
        let p = *cx.palette();
        let until = format_snooze_until(wake_at, now(), &self.time_zone);
        let conv_ref = ConversationRef::of(summary);
        let change_ref = conv_ref.clone();
        let cancel_ref = conv_ref;
        Some(
            h_flex()
                .items_center()
                .gap(px(8.))
                .px(px(12.))
                .py(px(7.))
                .rounded(px(8.))
                .bg(p.chip)
                .border_1()
                .border_color(p.divider)
                .text_size(ui_px(12.5))
                .child(icon("clock").flex_none().size(px(14.)).text_color(p.accent))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(p.foreground)
                        .child(format!("Snoozed until {until}")),
                )
                .child(
                    Button::new("reader-snooze-change")
                        .ghost()
                        .xsmall()
                        .label("Change time")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open_snooze_modal(vec![change_ref.clone()], window, cx);
                        })),
                )
                .child(
                    Button::new("reader-snooze-cancel")
                        .ghost()
                        .xsmall()
                        .label("Cancel")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.cancel_snooze_conversations(vec![cancel_ref.clone()], window, cx);
                        })),
                )
                .into_any_element(),
        )
    }

    /// Appends Snooze / Reschedule submenu and Cancel Snooze items to a conversation row context menu.
    pub(crate) fn append_snooze_menu_items(
        mut menu: PopupMenu,
        me: WeakEntity<Self>,
        state: &super::list_menu::SelectionState,
        offline: bool,
        presets: Vec<SnoozePreset>,
        window: &mut Window,
        cx: &mut Context<PopupMenu>,
    ) -> PopupMenu {
        if offline || state.all_in_trash || state.all_in_junk {
            return menu;
        }
        let submenu_label = if state.all_snoozed {
            "Change Snooze Time"
        } else {
            "Snooze"
        };
        let sub_me = me.clone();
        menu = menu.submenu_with_icon(
            Some(icon("clock")),
            submenu_label,
            window,
            cx,
            move |mut sub, _window, _cx| {
                for preset in &presets {
                    let wake_at = preset.wake_at;
                    let me = sub_me.clone();
                    sub = sub.item(
                        PopupMenuItem::new(format!("{} · {}", preset.label, preset.detail))
                            .on_click(move |_, window, cx| {
                                me.update(cx, |this, cx| {
                                    let targets = this.target_conversations();
                                    this.snooze_conversations(targets, wake_at, window, cx);
                                })
                                .ok();
                            }),
                    );
                }
                if !presets.is_empty() {
                    sub = sub.separator();
                }
                let me = sub_me.clone();
                sub.item(
                    PopupMenuItem::new("Pick date & time…")
                        .icon(icon("calendar"))
                        .on_click(move |_, window, cx| {
                            me.update(cx, |this, cx| {
                                let targets = this.target_conversations();
                                this.open_snooze_modal(targets, window, cx);
                            })
                            .ok();
                        }),
                )
            },
        );
        if state.any_snoozed {
            let cancel_me = me.clone();
            menu = menu.item(
                PopupMenuItem::new("Cancel Snooze")
                    .icon(icon("rotate-ccw"))
                    .on_click(move |_, window, cx| {
                        cancel_me
                            .update(cx, |this, cx| {
                                let targets = this.target_conversations();
                                this.cancel_snooze_conversations(targets, window, cx);
                            })
                            .ok();
                    }),
            );
        }
        menu
    }

    pub(crate) fn render_snooze_modal(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = *cx.palette();
        let Some(modal) = self.snooze_modal.as_ref() else {
            return div().into_any_element();
        };
        let now_ms = now();
        let presets = snooze_presets(now_ms, &self.time_zone, self.prefs.work_start_hour);
        let rescheduling = modal.existing_wake_at.is_some();
        let title = if rescheduling {
            "Change snooze time"
        } else {
            "Snooze until…"
        };
        let confirm_label = if rescheduling { "Reschedule" } else { "Snooze" };
        let targets = modal.targets.clone();

        div()
            .id("snooze-modal-overlay")
            .absolute()
            .inset_0()
            .occlude()
            .bg(gpui_kit::black().opacity(if p.is_dark() { 0.55 } else { 0.3 }))
            .flex()
            .items_center()
            .justify_center()
            .on_click(cx.listener(|this, _, window, cx| this.close_snooze_modal(window, cx)))
            .child(
                v_flex()
                    .id("snooze-modal")
                    .w(px(420.))
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
                            .child(icon("clock").flex_none().size(px(16.)).text_color(p.accent))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(ui_px(16.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(title),
                            )
                            .child(kbd("Esc", &p)),
                    )
                    .child(
                        div()
                            .text_size(ui_px(12.))
                            .text_color(p.muted)
                            .truncate()
                            .child(modal.heading.clone()),
                    )
                    .child(
                        v_flex().gap(px(2.)).children(presets.into_iter().enumerate().map(|(i, preset)| {
                            let targets = targets.clone();
                            let wake_at = preset.wake_at;
                            h_flex()
                                .id(SharedString::from(format!("snooze-preset-{i}")))
                                .items_center()
                                .justify_between()
                                .px(px(10.))
                                .h(px(32.))
                                .rounded(px(7.))
                                .cursor_pointer()
                                .hover(|d| d.bg(p.hover))
                                .child(
                                    div()
                                        .text_size(ui_px(13.))
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(p.foreground)
                                        .child(preset.label),
                                )
                                .child(
                                    div()
                                        .text_size(ui_px(12.))
                                        .text_color(p.muted)
                                        .child(preset.detail),
                                )
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.close_snooze_modal(window, cx);
                                    this.snooze_conversations(targets.clone(), wake_at, window, cx);
                                }))
                        })),
                    )
                    .child(divider(&p))
                    .child(
                        v_flex()
                            .gap(px(6.))
                            .child(
                                div()
                                    .text_size(ui_px(12.))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(p.muted)
                                    .child("Custom date and time"),
                            )
                            .child(
                                h_flex()
                                    .gap(px(8.))
                                    .child(
                                        v_flex()
                                            .flex_1()
                                            .gap(px(4.))
                                            .child(
                                                div()
                                                    .text_size(ui_px(11.))
                                                    .text_color(p.muted)
                                                    .child("Date (YYYY-MM-DD)"),
                                            )
                                            .child(Input::new(&modal.date_input)),
                                    )
                                    .child(
                                        v_flex()
                                            .flex_1()
                                            .gap(px(4.))
                                            .child(
                                                div()
                                                    .text_size(ui_px(11.))
                                                    .text_color(p.muted)
                                                    .child("Time"),
                                            )
                                            .child(Input::new(&modal.time_input)),
                                    ),
                            ),
                    )
                    .children(modal.error.clone().map(|e| {
                        div()
                            .text_size(ui_px(12.))
                            .text_color(p.error)
                            .child(e)
                    }))
                    .child(
                        h_flex()
                            .items_center()
                            .gap(px(8.))
                            .when(rescheduling, |row| {
                                let cancel_targets = targets.clone();
                                row.child(
                                    Button::new("snooze-modal-unsnooze")
                                        .ghost()
                                        .small()
                                        .label("Cancel snooze")
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.close_snooze_modal(window, cx);
                                            this.cancel_snooze_conversations(
                                                cancel_targets.clone(),
                                                window,
                                                cx,
                                            );
                                        })),
                                )
                            })
                            .child(div().flex_1())
                            .child(
                                Button::new("snooze-modal-cancel")
                                    .ghost()
                                    .small()
                                    .label("Close")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.close_snooze_modal(window, cx);
                                    })),
                            )
                            .child(
                                Button::new("snooze-modal-ok")
                                    .primary()
                                    .small()
                                    .label(confirm_label)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.submit_snooze_modal(window, cx);
                                    })),
                            ),
                    ),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use crate::ui::{apply_local_writes, combine_inbox_conversations, sidebar_counts, LocalWrite};
    use overlook_core::*;
    use overlook_store::{ListFilter, MailboxView, Store};
    use std::prelude::v1::test;

    fn account(store: &Store, id: &str, email: &str) -> Account {
        let a = Account {
            id: AccountId::from(id),
            provider: ProviderKind::Gmail,
            display_name: id.into(),
            email: email.into(),
            created_at: 1,
        };
        store.upsert_account(&a).unwrap();
        store
            .replace_labels(
                &a.id,
                &[
                    Label {
                        id: LabelId::from(format!("{id}:inbox")),
                        account_id: a.id.clone(),
                        kind: LabelKind::Inbox,
                        name: "Inbox".into(),
                        provider_id: Some(ProviderId("INBOX".into())),
                        color: None,
                        sort_order: 0,
                    },
                    Label {
                        id: LabelId::from(format!("{id}:trash")),
                        account_id: a.id.clone(),
                        kind: LabelKind::Trash,
                        name: "Trash".into(),
                        provider_id: Some(ProviderId("TRASH".into())),
                        color: None,
                        sort_order: 1,
                    },
                ],
            )
            .unwrap();
        a
    }

    fn inbox_msg(acct: &Account, id: &str, conv: &str, date: i64) -> Message {
        Message {
            id: MessageId::from(id),
            account_id: acct.id.clone(),
            conversation_id: ConversationId::from(conv),
            provider_id: Some(ProviderId(format!("p-{id}"))),
            from: Address::new(Some("Alex".into()), "alex@example.com"),
            to: vec![Address::new(None, acct.email.clone())],
            cc: vec![],
            bcc: vec![],
            reply_to: None,
            subject: format!("Thread {conv}"),
            snippet: "Hello".into(),
            body: Body { text: Some("Body".into()), html: None },
            date,
            is_read: false,
            is_starred: false,
            is_outgoing: false,
            label_ids: [LabelId::from(format!("{}:inbox", acct.id.0))]
                .into_iter()
                .collect(),
            attachments: vec![],
            rfc_message_id: Some(format!("<{id}@example.com>")),
            in_reply_to: None,
            references: vec![],
            category: None,
            is_important: false,
            focus_override: None,
            invitation: None,
            read_receipt: None,
        }
    }

    #[test]
    fn snooze_local_writes_isolate_accounts_update_sidebar_counts_and_all_inboxes() {
        let store = Store::open_in_memory().unwrap();
        let a = account(&store, "acct-a", "a@example.com");
        let b = account(&store, "acct-b", "b@example.com");
        let shared = ConversationId::from("shared-conv");
        store
            .upsert_messages(&[
                inbox_msg(&a, "a1", "shared-conv", 1_000),
                inbox_msg(&b, "b1", "shared-conv", 2_000),
            ])
            .unwrap();

        let future = overlook_core::time::now() + 3_600_000;
        let errors = apply_local_writes(
            &store,
            vec![(
                a.id.clone(),
                LocalWrite::Snooze {
                    conversation_id: shared.clone(),
                    wake_at: future,
                },
            )],
        );
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");

        let idx_a = store.mail_index(&a.id).unwrap();
        let idx_b = store.mail_index(&b.id).unwrap();

        // Sidebar counts: A has 0 Inbox unread and 1 Snoozed; B still has 1 Inbox unread and 0 Snoozed.
        let counts_a = sidebar_counts(&idx_a, &a.email, false);
        let counts_b = sidebar_counts(&idx_b, &b.email, false);
        assert_eq!(counts_a.get(&MailboxView::Kind(LabelKind::Inbox)).copied(), Some(0));
        assert_eq!(counts_a.get(&MailboxView::Kind(LabelKind::Snoozed)).copied(), Some(1));
        assert_eq!(counts_b.get(&MailboxView::Kind(LabelKind::Inbox)).copied(), Some(1));
        assert_eq!(counts_b.get(&MailboxView::Kind(LabelKind::Snoozed)).copied(), Some(0));

        // All Inboxes shows only B's conversation while A's is snoozed.
        let combined = combine_inbox_conversations([
            idx_a.conversations(&a.email, &MailboxView::Kind(LabelKind::Inbox), ListFilter::All, None),
            idx_b.conversations(&b.email, &MailboxView::Kind(LabelKind::Inbox), ListFilter::All, None),
        ]);
        assert_eq!(combined.len(), 1);
        assert_eq!(combined[0].account_id, b.id);

        // Cancelling snooze immediately restores A's conversation to All Inboxes.
        let errors = apply_local_writes(
            &store,
            vec![(
                a.id.clone(),
                LocalWrite::CancelSnooze {
                    conversation_id: shared,
                },
            )],
        );
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        let idx_a = store.mail_index(&a.id).unwrap();
        let combined = combine_inbox_conversations([
            idx_a.conversations(&a.email, &MailboxView::Kind(LabelKind::Inbox), ListFilter::All, None),
            idx_b.conversations(&b.email, &MailboxView::Kind(LabelKind::Inbox), ListFilter::All, None),
        ]);
        assert_eq!(combined.len(), 2);
    }
}
