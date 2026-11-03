//! Change history: what the user did to their mail, and how to take it back.
//!
//! Every undoable action (archive, move, trash, star, send, folder delete …) is recorded as
//! one entry holding a [`Reversal`]: the data needed to put things back as they were. The
//! entries back the toast's Undo button, Ctrl+Z and the History view, and they survive a
//! restart. Undoing an entry marks it undone instead of recording a new one.

use super::*;
use serde::{Deserialize, Serialize};

/// Entries older than this are pruned.
pub const HISTORY_MAX_AGE_MS: i64 = 30 * 24 * 60 * 60 * 1000;
/// At most this many entries are kept per account.
pub const HISTORY_MAX_ENTRIES: usize = 500;

/// How to take an action back.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Reversal {
    /// Local mailbox changes, applied in order.
    Changes { changes: Vec<LocalChange> },
    /// Take a queued send back out of the Outbox.
    CancelSend { draft_id: DraftId },
    /// Move a folder that went to Trash back under `parent` (`None`: the top level).
    RestoreFolder { label_id: LabelId, parent: Option<LabelId> },
    /// Recreate a deleted label (its full name) and put it back on its messages.
    RecreateLabel { name: String, message_ids: Vec<MessageId> },
    /// Recorded for the timeline, but it can't be taken back.
    None,
}

impl Reversal {
    /// Follows a folder whose id changed (an IMAP rename or move). Returns whether
    /// anything referred to it.
    pub fn rename_label(&mut self, old: &LabelId, new: &LabelId) -> bool {
        let mut changed = false;
        let mut follow = |id: &mut LabelId| {
            if id == old {
                *id = new.clone();
                changed = true;
            }
        };
        match self {
            Reversal::Changes { changes } => {
                for c in changes.iter_mut() {
                    if let LocalChange::AddLabel { label_id, .. } | LocalChange::RemoveLabel { label_id, .. } = c {
                        follow(label_id);
                    }
                }
            }
            Reversal::RestoreFolder { label_id, parent } => {
                follow(label_id);
                if let Some(parent) = parent {
                    follow(parent);
                }
            }
            Reversal::CancelSend { .. } | Reversal::RecreateLabel { .. } | Reversal::None => {}
        }
        changed
    }

    pub fn is_undoable(&self) -> bool {
        match self {
            Reversal::Changes { changes } => !changes.is_empty(),
            Reversal::None => false,
            _ => true,
        }
    }
}

/// One recorded action.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryEntry {
    pub id: i64,
    pub account_id: AccountId,
    pub at: Timestamp,
    /// What happened, e.g. "Archived", "Moved to Work".
    pub title: String,
    /// What it happened to, e.g. a subject and "and 2 more".
    pub detail: String,
    pub reversal: Reversal,
    pub undone_at: Option<Timestamp>,
}

impl HistoryEntry {
    pub fn can_undo(&self) -> bool {
        self.undone_at.is_none() && self.reversal.is_undoable()
    }
}

/// Rewrites the account's history after folders changed id, inside the rename's transaction.
pub(crate) fn rename_labels_tx(tx: &Connection, account: &AccountId, id_changes: &[(&LabelId, &LabelId)]) -> Result<()> {
    let rows: Vec<(i64, String)> = {
        let mut stmt = tx.prepare("SELECT id, reversal_json FROM change_history WHERE account_id=?1 AND undone_at IS NULL")?;
        let rows = stmt.query_map([&account.0], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    for (id, json) in rows {
        let Ok(mut reversal) = serde_json::from_str::<Reversal>(&json) else { continue };
        let mut changed = false;
        for (old, new) in id_changes {
            changed |= reversal.rename_label(old, new);
        }
        if changed {
            tx.execute("UPDATE change_history SET reversal_json=?2 WHERE id=?1", params![id, serde_json::to_string(&reversal)?])?;
        }
    }
    Ok(())
}

/// The changes that undo `changes`, computed from `index` as it was before they were made.
/// Only messages the changes actually affect are reversed, so a mixed selection (some
/// already starred, some already in the Inbox) goes back exactly as it was.
pub fn reversal_for(changes: &[LocalChange], index: &MailIndex) -> Vec<LocalChange> {
    let inbox = index.label_of_kind(LabelKind::Inbox);
    let trash = index.label_of_kind(LabelKind::Trash);
    let mut scratch = index.clone();
    let mut out: Vec<Vec<LocalChange>> = Vec::new();
    for change in changes {
        out.push(inverse_of(change, &scratch, inbox.as_ref(), trash.as_ref()));
        scratch.apply(change);
    }
    out.into_iter().rev().flatten().collect()
}

fn inverse_of(change: &LocalChange, index: &MailIndex, inbox: Option<&LabelId>, trash: Option<&LabelId>) -> Vec<LocalChange> {
    let known = |ids: &[MessageId]| -> Vec<&Message> { ids.iter().filter_map(|id| index.message(id)).collect() };
    let ids_where = |ids: &[MessageId], keep: &dyn Fn(&Message) -> bool| -> Vec<MessageId> { known(ids).into_iter().filter(|m| keep(m)).map(|m| m.id.clone()).collect() };
    let has = |m: &Message, l: Option<&LabelId>| l.is_some_and(|l| m.has_label(l));
    let mut out = Vec::new();
    let mut push = |c: LocalChange| {
        if !c.message_ids().is_empty() {
            out.push(c);
        }
    };
    match change {
        LocalChange::SetRead { message_ids, read } => push(LocalChange::SetRead { message_ids: ids_where(message_ids, &|m| m.is_read != *read), read: !read }),
        LocalChange::SetStarred { message_ids, starred } => {
            push(LocalChange::SetStarred { message_ids: ids_where(message_ids, &|m| m.is_starred != *starred), starred: !starred })
        }
        LocalChange::AddLabel { message_ids, label_id } => {
            push(LocalChange::RemoveLabel { message_ids: ids_where(message_ids, &|m| !m.has_label(label_id)), label_id: label_id.clone() })
        }
        LocalChange::RemoveLabel { message_ids, label_id } => {
            push(LocalChange::AddLabel { message_ids: ids_where(message_ids, &|m| m.has_label(label_id)), label_id: label_id.clone() })
        }
        LocalChange::Archive { message_ids } => {
            if let Some(inbox) = inbox {
                push(LocalChange::AddLabel { message_ids: ids_where(message_ids, &|m| m.has_label(inbox)), label_id: inbox.clone() });
            }
        }
        LocalChange::Trash { message_ids } => {
            // Untrash returns messages to the Inbox; the ones that weren't there are archived again.
            let trashed = ids_where(message_ids, &|m| !has(m, trash));
            let outside_inbox: Vec<MessageId> = known(&trashed).into_iter().filter(|m| !has(m, inbox)).map(|m| m.id.clone()).collect();
            push(LocalChange::Untrash { message_ids: trashed });
            push(LocalChange::Archive { message_ids: outside_inbox });
        }
        LocalChange::Untrash { message_ids } => {
            // Trash takes them out of the Inbox again, wherever they were before.
            push(LocalChange::Trash { message_ids: ids_where(message_ids, &|m| has(m, trash)) });
        }
        LocalChange::DeleteForever { .. } => {}
    }
    out
}

const ENTRY_COLUMNS: &str = "id, account_id, at, title, detail, reversal_json, undone_at";

fn row_to_entry(r: &Row<'_>) -> rusqlite::Result<(HistoryEntry, String)> {
    Ok((
        HistoryEntry {
            id: r.get(0)?,
            account_id: AccountId(r.get(1)?),
            at: r.get(2)?,
            title: r.get(3)?,
            detail: r.get(4)?,
            reversal: Reversal::None,
            undone_at: r.get(6)?,
        },
        r.get(5)?,
    ))
}

fn with_reversal((mut entry, json): (HistoryEntry, String)) -> HistoryEntry {
    // A row written by a newer version may hold a reversal this one doesn't know.
    entry.reversal = serde_json::from_str(&json).unwrap_or(Reversal::None);
    entry
}

impl Store {
    /// Records an action taken at `at` and returns its id. Entries are ordered by `at`, so
    /// the caller stamps it when the action happens: writes may land in another order.
    pub fn record_history(&self, account: &AccountId, at: Timestamp, title: &str, detail: &str, reversal: &Reversal) -> Result<i64> {
        let json = serde_json::to_string(reversal)?;
        self.with_tx(|tx| {
            tx.execute(
                "INSERT INTO change_history(account_id, at, title, detail, reversal_json) VALUES(?1,?2,?3,?4,?5)",
                params![account.0, at, title, detail, json],
            )?;
            Ok(tx.last_insert_rowid())
        })
    }

    /// The account's recorded actions, newest first.
    pub fn history(&self, account: &AccountId, limit: usize) -> Result<Vec<HistoryEntry>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(&format!("SELECT {ENTRY_COLUMNS} FROM change_history WHERE account_id=?1 ORDER BY at DESC, id DESC LIMIT ?2"))?;
            let rows = stmt.query_map(params![account.0, limit as i64], row_to_entry)?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?.into_iter().map(with_reversal).collect())
        })
    }

    pub fn history_entry(&self, id: i64) -> Result<Option<HistoryEntry>> {
        self.with_conn(|conn| {
            Ok(conn.query_row(&format!("SELECT {ENTRY_COLUMNS} FROM change_history WHERE id=?1"), [id], row_to_entry).optional()?.map(with_reversal))
        })
    }

    /// The newest entry of the account that can still be undone.
    pub fn last_undoable(&self, account: &AccountId) -> Result<Option<HistoryEntry>> {
        Ok(self.history(account, HISTORY_MAX_ENTRIES)?.into_iter().find(HistoryEntry::can_undo))
    }

    /// Entries of the account from `id` onwards that can still be undone, newest first: what
    /// "Restore to here" takes back.
    pub fn undoable_since(&self, account: &AccountId, id: i64) -> Result<Vec<HistoryEntry>> {
        let all = self.history(account, HISTORY_MAX_ENTRIES)?;
        let Some(pos) = all.iter().position(|e| e.id == id) else { return Ok(Vec::new()) };
        Ok(all.into_iter().take(pos + 1).filter(HistoryEntry::can_undo).collect())
    }

    /// Marks an entry undone. Returns `false` if it already was, so an undo runs once.
    pub fn mark_undone(&self, id: i64) -> Result<bool> {
        self.with_tx(|tx| Ok(tx.execute("UPDATE change_history SET undone_at=?2 WHERE id=?1 AND undone_at IS NULL", params![id, now()])? == 1))
    }

    /// Claims an entry for undoing: marks it undone and returns it, or `None` when it is
    /// already undone or can't be undone. Claiming first means an undo runs once even when
    /// the button and Ctrl+Z race.
    pub fn begin_undo(&self, id: i64) -> Result<Option<HistoryEntry>> {
        let Some(entry) = self.history_entry(id)? else { return Ok(None) };
        Ok((entry.can_undo() && self.mark_undone(id)?).then_some(entry))
    }

    /// Claims the newest entry of the account that can still be undone (Ctrl+Z).
    pub fn begin_undo_last(&self, account: &AccountId) -> Result<Option<HistoryEntry>> {
        let _claim = self.history_claim.lock().expect("history mutex poisoned");
        let Some(entry) = self.last_undoable(account)? else { return Ok(None) };
        Ok(self.mark_undone(entry.id)?.then_some(entry))
    }

    /// Claims every entry from `id` onwards that can still be undone, newest first
    /// ("Restore to here").
    pub fn begin_undo_since(&self, account: &AccountId, id: i64) -> Result<Vec<HistoryEntry>> {
        let _claim = self.history_claim.lock().expect("history mutex poisoned");
        let mut out = Vec::new();
        for entry in self.undoable_since(account, id)? {
            if self.mark_undone(entry.id)? {
                out.push(entry);
            }
        }
        Ok(out)
    }

    /// Keeps the entry in the timeline but stops offering to undo it (an undo that came too
    /// late, like a message that has already left).
    pub fn forget_reversal(&self, id: i64) -> Result<()> {
        let json = serde_json::to_string(&Reversal::None)?;
        self.with_tx(|tx| {
            tx.execute("UPDATE change_history SET reversal_json=?2, undone_at=NULL WHERE id=?1", params![id, json])?;
            Ok(())
        })
    }

    /// The account's messages that carry `label`, for putting a deleted label back.
    pub fn message_ids_with_label(&self, account: &AccountId, label: &LabelId) -> Result<Vec<MessageId>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare("SELECT ml.message_id FROM message_labels ml JOIN messages m ON m.id=ml.message_id WHERE m.account_id=?1 AND ml.label_id=?2 ORDER BY ml.message_id")?;
            let rows = stmt.query_map(params![account.0, label.0], |r| Ok(MessageId(r.get(0)?)))?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
    }

    /// Puts an entry back to not undone, after an undo that failed.
    pub fn clear_undone(&self, id: i64) -> Result<()> {
        self.with_tx(|tx| {
            tx.execute("UPDATE change_history SET undone_at=NULL WHERE id=?1", [id])?;
            Ok(())
        })
    }

    /// Drops entries older than `HISTORY_MAX_AGE_MS` and beyond `HISTORY_MAX_ENTRIES` per account.
    pub fn prune_history(&self) -> Result<()> {
        let cutoff = now() - HISTORY_MAX_AGE_MS;
        self.with_tx(|tx| {
            tx.execute("DELETE FROM change_history WHERE at < ?1", [cutoff])?;
            tx.execute(
                "DELETE FROM change_history WHERE id IN (
                    SELECT id FROM (SELECT id, ROW_NUMBER() OVER (PARTITION BY account_id ORDER BY at DESC, id DESC) AS n FROM change_history) WHERE n > ?1)",
                [HISTORY_MAX_ENTRIES as i64],
            )?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn label(id: &str, kind: LabelKind) -> Label {
        Label { id: LabelId::from(id), account_id: AccountId::from("a"), kind, name: id.into(), provider_id: None, color: None, sort_order: 0 }
    }

    fn message(id: &str, labels: &[&str], read: bool, starred: bool) -> Message {
        Message {
            id: MessageId::from(id),
            account_id: AccountId::from("a"),
            conversation_id: ConversationId::from(format!("c-{id}").as_str()),
            provider_id: None,
            from: Address::named("Maya", "maya@example.com"),
            to: vec![],
            cc: vec![],
            bcc: vec![],
            reply_to: None,
            subject: format!("Subject {id}"),
            snippet: String::new(),
            body: Body { text: None, html: None },
            date: 1,
            is_read: read,
            is_starred: starred,
            is_outgoing: false,
            label_ids: labels.iter().map(|l| LabelId::from(*l)).collect(),
            attachments: vec![],
            rfc_message_id: None,
            in_reply_to: None,
            references: Vec::new(),
            category: None,
            is_important: false,
            focus_override: None,
            invitation: None,
        }
    }

    fn index() -> MailIndex {
        MailIndex::new(
            vec![label("inbox", LabelKind::Inbox), label("trash", LabelKind::Trash), label("work", LabelKind::User)],
            vec![
                message("in", &["inbox"], false, false),
                message("in-work", &["inbox", "work"], true, true),
                message("archived", &[], true, false),
                message("trashed", &["trash"], true, false),
            ],
            Default::default(),
            Default::default(),
        )
    }

    fn ids(list: &[&str]) -> Vec<MessageId> {
        list.iter().map(|s| MessageId::from(*s)).collect()
    }

    fn state(index: &MailIndex) -> Vec<(MessageId, BTreeSet<LabelId>, bool, bool)> {
        let mut out: Vec<_> = ["in", "in-work", "archived", "trashed"]
            .iter()
            .filter_map(|id| index.message(&MessageId::from(*id)))
            .map(|m| (m.id.clone(), m.label_ids.clone(), m.is_read, m.is_starred))
            .collect();
        out.sort();
        out
    }

    /// Applying an action and then its reversal gives back the index it started from.
    fn assert_round_trip(changes: Vec<LocalChange>) {
        let before = index();
        let reversal = reversal_for(&changes, &before);
        let mut after = before.clone();
        for c in &changes {
            after.apply(c);
        }
        for c in &reversal {
            after.apply(c);
        }
        assert_eq!(state(&after), state(&before), "{changes:?} reversed by {reversal:?}");
    }

    #[test]
    fn every_change_round_trips_on_a_mixed_selection() {
        let all = ids(&["in", "in-work", "archived", "trashed"]);
        let work = LabelId::from("work");
        assert_round_trip(vec![LocalChange::SetRead { message_ids: all.clone(), read: true }]);
        assert_round_trip(vec![LocalChange::SetRead { message_ids: all.clone(), read: false }]);
        assert_round_trip(vec![LocalChange::SetStarred { message_ids: all.clone(), starred: true }]);
        assert_round_trip(vec![LocalChange::AddLabel { message_ids: all.clone(), label_id: work.clone() }]);
        assert_round_trip(vec![LocalChange::RemoveLabel { message_ids: all.clone(), label_id: work.clone() }]);
        assert_round_trip(vec![LocalChange::Archive { message_ids: all.clone() }]);
        assert_round_trip(vec![LocalChange::Trash { message_ids: ids(&["in", "in-work", "archived"]) }]);
        assert_round_trip(vec![LocalChange::Untrash { message_ids: ids(&["trashed"]) }]);
    }

    #[test]
    fn a_move_to_a_label_round_trips() {
        // What "Move to Work" does: label, then take out of the Inbox.
        assert_round_trip(vec![
            LocalChange::AddLabel { message_ids: ids(&["in", "in-work", "archived"]), label_id: LabelId::from("work") },
            LocalChange::Archive { message_ids: ids(&["in", "in-work"]) },
        ]);
    }

    #[test]
    fn untouched_messages_are_left_out_of_the_reversal() {
        let reversal = reversal_for(&[LocalChange::Archive { message_ids: ids(&["in", "archived"]) }], &index());
        assert_eq!(reversal, vec![LocalChange::AddLabel { message_ids: ids(&["in"]), label_id: LabelId::from("inbox") }]);
        assert!(reversal_for(&[LocalChange::DeleteForever { message_ids: ids(&["in"]), folder: None }], &index()).is_empty());
    }

    fn store() -> Store {
        let store = Store::open_in_memory().unwrap();
        store
            .upsert_account(&Account { id: AccountId::from("a"), provider: ProviderKind::Sample, display_name: "A".into(), email: "a@example.com".into(), created_at: 1 })
            .unwrap();
        store
    }

    #[test]
    fn entries_are_listed_newest_first_and_undo_once() {
        let store = store();
        let a = AccountId::from("a");
        let changes = Reversal::Changes { changes: vec![LocalChange::Untrash { message_ids: ids(&["m"]) }] };
        // Written out of order: the action time decides, not the write. One clock read,
        // or a tick between reads would reorder them.
        let t = now();
        let third = store.record_history(&a, t + 2, "Sent", "Hello", &Reversal::CancelSend { draft_id: DraftId::from("d") }).unwrap();
        let first = store.record_history(&a, t, "Moved to Trash", "Subject m", &changes).unwrap();
        let second = store.record_history(&a, t + 1, "Deleted forever", "Subject n", &Reversal::None).unwrap();

        let list = store.history(&a, 10).unwrap();
        assert_eq!(list.iter().map(|e| e.id).collect::<Vec<_>>(), vec![third, second, first]);
        assert_eq!(list[2].reversal, changes);
        assert_eq!(store.last_undoable(&a).unwrap().unwrap().id, third);

        assert_eq!(store.begin_undo_last(&a).unwrap().unwrap().id, third);
        assert!(store.begin_undo(third).unwrap().is_none(), "an entry is undone once");
        assert!(store.begin_undo(second).unwrap().is_none(), "a permanent change can't be undone");
        // The entry that can't be undone is skipped.
        assert_eq!(store.last_undoable(&a).unwrap().unwrap().id, first);
        assert_eq!(store.undoable_since(&a, first).unwrap().iter().map(|e| e.id).collect::<Vec<_>>(), vec![first]);

        store.clear_undone(third).unwrap();
        assert_eq!(store.begin_undo_since(&a, first).unwrap().iter().map(|e| e.id).collect::<Vec<_>>(), vec![third, first]);
        assert!(store.last_undoable(&a).unwrap().is_none());

        // Too late: the entry stays in the timeline but is no longer offered.
        store.clear_undone(third).unwrap();
        store.forget_reversal(third).unwrap();
        let entry = store.history_entry(third).unwrap().unwrap();
        assert_eq!((entry.reversal, entry.undone_at), (Reversal::None, None));
    }

    #[test]
    fn history_follows_a_renamed_folder() {
        let store = store();
        let a = AccountId::from("a");
        let (old, renamed, parent) = (LabelId::from("a:Trash/Work"), LabelId::from("a:Trash/Old work"), LabelId::from("a:Projects"));
        for l in [&old, &parent] {
            store.upsert_label(&Label { id: l.clone(), account_id: a.clone(), kind: LabelKind::User, name: l.0.clone(), provider_id: None, color: None, sort_order: 0 }).unwrap();
        }
        let folder = store.record_history(&a, now(), "Deleted folder", "", &Reversal::RestoreFolder { label_id: old.clone(), parent: Some(parent.clone()) }).unwrap();
        let moved = store.record_history(&a, now(), "Moved to Work", "", &Reversal::Changes { changes: vec![LocalChange::RemoveLabel { message_ids: ids(&["m"]), label_id: old.clone() }] }).unwrap();
        let label = Label { id: renamed.clone(), account_id: a.clone(), kind: LabelKind::User, name: "Trash / Old work".into(), provider_id: None, color: None, sort_order: 0 };
        store.rename_folders(&a, &[(old.clone(), label)], &[]).unwrap();
        assert_eq!(store.history_entry(folder).unwrap().unwrap().reversal, Reversal::RestoreFolder { label_id: renamed.clone(), parent: Some(parent) });
        assert_eq!(store.history_entry(moved).unwrap().unwrap().reversal, Reversal::Changes { changes: vec![LocalChange::RemoveLabel { message_ids: ids(&["m"]), label_id: renamed }] });
    }

    #[test]
    fn history_survives_a_restart_and_is_pruned() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("overlook.sqlite");
        let a = AccountId::from("a");
        {
            let store = Store::open(&path).unwrap();
            store.upsert_account(&Account { id: a.clone(), provider: ProviderKind::Sample, display_name: "A".into(), email: "a@example.com".into(), created_at: 1 }).unwrap();
            store.record_history(&a, now(), "Archived", "Subject", &Reversal::RestoreFolder { label_id: LabelId::from("x"), parent: None }).unwrap();
            store.with_tx(|tx| Ok(tx.execute("INSERT INTO change_history(account_id, at, title, detail, reversal_json) VALUES('a', 1, 'Old', '', '{\"kind\":\"none\"}')", [])?)).unwrap();
        }
        let store = Store::open(&path).unwrap();
        assert_eq!(store.history(&a, 10).unwrap().len(), 2);
        store.prune_history().unwrap();
        let left = store.history(&a, 10).unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].reversal, Reversal::RestoreFolder { label_id: LabelId::from("x"), parent: None });
    }
}
