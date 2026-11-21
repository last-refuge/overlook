//! Account-bound organization writes inside the MCP idempotency transaction.

use super::McpWriteTxn;
use crate::{
    apply_change_tx, apply_queued_change_tx, history, labels_tx, load_messages_with, now, params,
    reversal_for, set_focus_override_and_rule_tx, Label, LabelId, LabelKind, LocalChange,
    MailIndex, Message, OperationId, OperationState, OptionalExtension, Result, Reversal,
    StoreError,
};
use std::collections::{BTreeMap, BTreeSet};

impl McpWriteTxn<'_, '_> {
    /// Resolve the bounded selection with one scan of account keys. Only load
    /// headers for matched IDs, and keep foreign or unknown IDs indistinguishable.
    pub fn organization_messages(
        &self,
        requested: &BTreeSet<&str>,
        mut opaque_id: impl FnMut(&str) -> String,
    ) -> Result<BTreeMap<String, Message>> {
        let mut stmt = self
            .tx
            .prepare_cached("SELECT id FROM messages WHERE account_id=?1")?;
        let mut rows = stmt.query([self.account.as_str()])?;
        let mut found = BTreeMap::new();
        while found.len() < requested.len() {
            let Some(row) = rows.next()? else { break };
            let raw: String = row.get(0)?;
            let opaque = opaque_id(&raw);
            if requested.contains(opaque.as_str()) {
                if let Some(message) = load_messages_with(
                    self.tx,
                    "WHERE m.account_id=?1 AND m.id=?2",
                    &[&self.account.as_str(), &raw],
                    false,
                )?
                .pop()
                {
                    found.insert(opaque, message);
                }
            }
        }
        Ok(found)
    }

    pub fn organization_labels(&self) -> Result<Vec<Label>> {
        labels_tx(self.tx, self.account)
    }

    /// Compute the inverse against the pre-change headers. The forward and
    /// reverse sequences use the same actions as the desktop history.
    pub fn organization_reversal(
        &self,
        changes: &[LocalChange],
        labels: Vec<Label>,
        messages: Vec<Message>,
    ) -> Reversal {
        let index = MailIndex::new(labels, messages, Default::default(), Default::default());
        Reversal::Changes {
            changes: reversal_for(changes, &index),
        }
    }

    pub fn organization_apply(
        &self,
        changes: Vec<LocalChange>,
        imap: bool,
    ) -> Result<Vec<OperationId>> {
        let at = now();
        let mut queued = Vec::new();
        let mut changes = changes.into_iter().peekable();
        while let Some(change) = changes.next() {
            if change.message_ids().is_empty() {
                continue;
            }
            // Moving an IMAP copy applies both local membership edits, but
            // only the destination is sent to the server. If the move fails,
            // a second queued removal could otherwise move it to the Inbox.
            let imap_move = imap
                && matches!(
                    (&change, changes.peek()),
                    (LocalChange::AddLabel { message_ids, label_id }, Some(LocalChange::RemoveLabel {
                        message_ids: source_ids, label_id: source_label,
                    })) if message_ids == source_ids && label_id != source_label
                );
            let op = apply_queued_change_tx(self.tx, self.account, change, at)?;
            if imap_move {
                let remove_source = changes.next().expect("matched the source removal");
                apply_change_tx(self.tx, self.account, &remove_source)?;
            }
            if op.state == OperationState::Pending {
                queued.push(op.id);
            }
        }
        Ok(queued)
    }

    pub fn organization_record(
        &self,
        title: &str,
        detail: &str,
        reversal: &Reversal,
    ) -> Result<i64> {
        history::record_history_tx(self.tx, self.account, now(), title, detail, reversal)
    }

    /// Returns whether a message override or the optional sender rule changed.
    pub fn organization_focus(
        &self,
        message: &Message,
        focused: Option<bool>,
        sender: Option<&str>,
    ) -> Result<(bool, bool)> {
        if &message.account_id != self.account {
            return Err(StoreError::NotOwned("message"));
        }
        let sender = sender.map(str::to_ascii_lowercase);
        let rule_changed = if let Some(email) = &sender {
            let previous: Option<bool> = self
                .tx
                .query_row(
                    "SELECT focused FROM focus_rules WHERE account_id=?1 AND email=?2",
                    params![self.account.as_str(), email],
                    |row| row.get(0),
                )
                .optional()?;
            previous != focused
        } else {
            false
        };
        let override_changed = message.focus_override != focused;
        if override_changed || rule_changed {
            set_focus_override_and_rule_tx(
                self.tx,
                self.account,
                if override_changed {
                    std::slice::from_ref(&message.id)
                } else {
                    &[]
                },
                focused,
                sender.as_deref().filter(|_| rule_changed),
            )?;
        }
        Ok((override_changed, rule_changed))
    }

    pub fn organization_find_history(
        &self,
        matches: impl FnMut(i64) -> bool,
    ) -> Result<Option<crate::HistoryEntry>> {
        history::find_history_tx(self.tx, self.account, matches)
    }

    /// Check the entire reversal before applying it. An unavailable message or
    /// folder prevents a partially restored history entry.
    pub fn organization_can_apply(&self, changes: &[LocalChange]) -> Result<bool> {
        let labels = self.organization_labels()?;
        let has_kind = |kind| labels.iter().any(|label| label.kind == kind);
        let mut stmt = self
            .tx
            .prepare_cached("SELECT 1 FROM messages WHERE account_id=?1 AND id=?2")?;
        for change in changes {
            let destination_exists = match change {
                LocalChange::AddLabel { label_id, .. }
                | LocalChange::RemoveLabel { label_id, .. } => {
                    labels.iter().any(|label| &label.id == label_id)
                }
                LocalChange::Archive { .. } => has_kind(LabelKind::Inbox),
                LocalChange::Trash { .. } | LocalChange::Untrash { .. } => {
                    has_kind(LabelKind::Inbox) && has_kind(LabelKind::Trash)
                }
                LocalChange::DeleteForever { .. } => false,
                _ => true,
            };
            if !destination_exists {
                return Ok(false);
            }
            for id in change.message_ids() {
                if !stmt.exists(params![self.account.as_str(), id.as_str()])? {
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }

    pub fn organization_mark_undone(&self, entry_id: i64) -> Result<bool> {
        Ok(self.tx.execute(
            "UPDATE change_history SET undone_at=?3 WHERE account_id=?1 AND id=?2 AND undone_at IS NULL",
            params![self.account.as_str(), entry_id, now()],
        )? == 1)
    }

    /// A POP3 folder exists in SQLite only. The caller restricts these tools to
    /// POP3, whose snapshot merge preserves user-created labels.
    pub fn organization_create_folder(&self, name: &str) -> Result<LabelId> {
        let id = LabelId::generate();
        self.tx.execute(
            "INSERT INTO labels(id, account_id, kind, name, sort_order)
             SELECT ?1, ?2, ?3, ?4, MIN(COALESCE(MAX(sort_order), -1) + 1, ?5)
             FROM labels WHERE account_id=?2",
            params![
                id.as_str(),
                self.account.as_str(),
                LabelKind::User.as_key(),
                name,
                i32::MAX
            ],
        )?;
        Ok(id)
    }

    pub fn organization_rename_folder(&self, id: &LabelId, name: &str) -> Result<bool> {
        Ok(self.tx.execute(
            "UPDATE labels SET name=?3 WHERE account_id=?1 AND id=?2 AND kind=?4 AND provider_id IS NULL",
            params![self.account.as_str(), id.as_str(), name, LabelKind::User.as_key()],
        )? == 1)
    }
}
