//! Device-local email snooze storage and wakeup logic.

use crate::{load_messages_with, Result, Store, StoreError};
use overlook_core::snooze::{build_snooze_baseline, is_new_inbound_reply};
use overlook_core::time::{now, Timestamp};
use overlook_core::*;
use rusqlite::{params, Connection, OptionalExtension, Row};
use std::collections::{BTreeSet, HashMap, HashSet};

fn row_to_snooze(r: &Row<'_>) -> rusqlite::Result<ConversationSnooze> {
    let msg_ids_json: String = r.get(5)?;
    let rfc_ids_json: String = r.get(6)?;
    let baseline_message_ids: BTreeSet<MessageId> = serde_json::from_str(&msg_ids_json).unwrap_or_default();
    let baseline_rfc_ids: BTreeSet<String> = serde_json::from_str(&rfc_ids_json).unwrap_or_default();
    Ok(ConversationSnooze {
        account_id: AccountId(r.get(0)?),
        conversation_id: ConversationId(r.get(1)?),
        wake_at: r.get(2)?,
        snoozed_at: r.get(3)?,
        baseline_latest_inbound: r.get(4)?,
        baseline_message_ids,
        baseline_rfc_ids,
    })
}

fn label_id_of_kind_tx(tx: &Connection, account: &AccountId, kind: LabelKind) -> Result<Option<LabelId>> {
    Ok(tx
        .prepare_cached("SELECT id FROM labels WHERE account_id=?1 AND kind=?2 LIMIT 1")?
        .query_row(params![account.0, kind.as_key()], |r| Ok(LabelId(r.get(0)?)))
        .optional()?)
}

fn load_snooze_tx(tx: &Connection, account: &AccountId, conversation: &ConversationId) -> Result<Option<ConversationSnooze>> {
    Ok(tx
        .prepare_cached(
            "SELECT account_id, conversation_id, wake_at, snoozed_at, baseline_latest_inbound,
                    baseline_message_ids_json, baseline_rfc_ids_json
             FROM conversation_snoozes
             WHERE account_id=?1 AND conversation_id=?2",
        )?
        .query_row(params![account.0, conversation.0], row_to_snooze)
        .optional()?)
}

pub(crate) fn has_any_snoozes_tx(tx: &Connection) -> Result<bool> {
    let found: Option<i64> = tx
        .prepare_cached("SELECT 1 FROM conversation_snoozes LIMIT 1")?
        .query_row([], |r| r.get(0))
        .optional()?;
    Ok(found.is_some())
}

pub(crate) fn snoozes_map_tx(tx: &Connection, account: &AccountId) -> Result<HashMap<ConversationId, Timestamp>> {
    let mut stmt = tx.prepare_cached(
        "SELECT conversation_id, wake_at FROM conversation_snoozes WHERE account_id=?1",
    )?;
    let rows = stmt.query_map([&account.0], |r| Ok((ConversationId(r.get(0)?), r.get(1)?)))?;
    Ok(rows.collect::<std::result::Result<HashMap<_, _>, _>>()?)
}

/// Wakes any active snooze whose conversation received a genuinely new inbound reply in `messages`.
pub(crate) fn wake_snoozes_for_incoming_tx(
    tx: &Connection,
    messages: &[Message],
) -> Result<Vec<(AccountId, ConversationId)>> {
    if messages.is_empty() || !has_any_snoozes_tx(tx)? {
        return Ok(Vec::new());
    }
    let mut grouped: HashMap<(&AccountId, &ConversationId), Vec<&Message>> = HashMap::new();
    for m in messages {
        if m.conversation_id.0.is_empty() {
            continue;
        }
        grouped.entry((&m.account_id, &m.conversation_id)).or_default().push(m);
    }

    let mut label_cache: HashMap<AccountId, (Option<LabelId>, Option<LabelId>)> = HashMap::new();
    let mut woken = Vec::new();
    for ((account_id, conversation_id), msgs) in grouped {
        let Some(snooze) = load_snooze_tx(tx, account_id, conversation_id)? else {
            continue;
        };
        let (trash_id, spam_id) = match label_cache.get(account_id) {
            Some(pair) => pair.clone(),
            None => {
                let pair = (
                    label_id_of_kind_tx(tx, account_id, LabelKind::Trash)?,
                    label_id_of_kind_tx(tx, account_id, LabelKind::Spam)?,
                );
                label_cache.insert(account_id.clone(), pair.clone());
                pair
            }
        };
        let triggered = msgs
            .iter()
            .any(|m| is_new_inbound_reply(&snooze, m, trash_id.as_ref(), spam_id.as_ref()));
        if triggered {
            tx.prepare_cached(
                "DELETE FROM conversation_snoozes WHERE account_id=?1 AND conversation_id=?2",
            )?
            .execute(params![account_id.0, conversation_id.0])?;
            woken.push((account_id.clone(), conversation_id.clone()));
        }
    }
    Ok(woken)
}

/// Resolves distinct conversation IDs belonging to `message_ids` under `account`.
pub(crate) fn conversation_ids_for_messages_tx(
    tx: &Connection,
    account: &AccountId,
    message_ids: &[MessageId],
) -> Result<Vec<ConversationId>> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut stmt = tx.prepare_cached("SELECT conversation_id FROM messages WHERE account_id=?1 AND id=?2")?;
    for id in message_ids {
        if let Some(conv) = stmt
            .query_row(params![account.0, id.0], |r| Ok(ConversationId(r.get(0)?)))
            .optional()?
        {
            if seen.insert(conv.clone()) {
                out.push(conv);
            }
        }
    }
    Ok(out)
}

/// Clears active snoozes for conversations touched by `message_ids` (used on archive, trash,
/// spam, or permanent delete so snoozed rows don't linger in Snoozed).
pub(crate) fn clear_snoozes_for_messages_tx(
    tx: &Connection,
    account: &AccountId,
    message_ids: &[MessageId],
) -> Result<usize> {
    if message_ids.is_empty() || !has_any_snoozes_tx(tx)? {
        return Ok(0);
    }
    let convs = conversation_ids_for_messages_tx(tx, account, message_ids)?;
    let mut del = tx.prepare_cached(
        "DELETE FROM conversation_snoozes WHERE account_id=?1 AND conversation_id=?2",
    )?;
    let mut removed = 0;
    for conv in convs {
        removed += del.execute(params![account.0, conv.0])?;
    }
    Ok(removed)
}

/// Clears active snoozes for conversations in `convs` that no longer carry the Inbox label
/// on any message (e.g. after moving a snoozed conversation out of Inbox to a folder).
pub(crate) fn clear_snoozes_without_inbox_tx(
    tx: &Connection,
    account: &AccountId,
    convs: &[ConversationId],
) -> Result<usize> {
    if convs.is_empty() || !has_any_snoozes_tx(tx)? {
        return Ok(0);
    }
    let Some(inbox) = label_id_of_kind_tx(tx, account, LabelKind::Inbox)? else {
        return Ok(0);
    };
    let mut check = tx.prepare_cached(
        "SELECT 1 FROM messages m
         JOIN message_labels ml ON ml.message_id = m.id
         WHERE m.account_id=?1 AND m.conversation_id=?2 AND ml.label_id=?3
         LIMIT 1",
    )?;
    let mut del = tx.prepare_cached(
        "DELETE FROM conversation_snoozes WHERE account_id=?1 AND conversation_id=?2",
    )?;
    let mut removed = 0;
    for conv in convs {
        let still_inbox: Option<i64> = check
            .query_row(params![account.0, conv.0, inbox.0], |r| r.get(0))
            .optional()?;
        if still_inbox.is_none() {
            removed += del.execute(params![account.0, conv.0])?;
        }
    }
    Ok(removed)
}

/// Removes snoozes whose conversation has no messages left on this device (after server delete).
pub(crate) fn prune_empty_snoozes_tx(
    tx: &Connection,
    affected: &[(AccountId, ConversationId)],
) -> Result<usize> {
    if affected.is_empty() || !has_any_snoozes_tx(tx)? {
        return Ok(0);
    }
    let mut check = tx.prepare_cached(
        "SELECT 1 FROM messages WHERE account_id=?1 AND conversation_id=?2 LIMIT 1",
    )?;
    let mut del = tx.prepare_cached(
        "DELETE FROM conversation_snoozes WHERE account_id=?1 AND conversation_id=?2",
    )?;
    let mut removed = 0;
    for (acct, conv) in affected {
        let remaining: Option<i64> = check
            .query_row(params![acct.0, conv.0], |r| r.get(0))
            .optional()?;
        if remaining.is_none() {
            removed += del.execute(params![acct.0, conv.0])?;
        }
    }
    Ok(removed)
}

impl Store {
    /// Snoozes (or reschedules) a conversation until `wake_at`, recording the conversation's
    /// current inbound baseline so later syncs won't mistake existing mail for a new reply.
    pub fn snooze_conversation(
        &self,
        account: &AccountId,
        conversation: &ConversationId,
        wake_at: Timestamp,
    ) -> Result<ConversationSnooze> {
        self.snooze_conversation_at(account, conversation, wake_at, now())
    }

    /// Deterministic version of [`Store::snooze_conversation`] accepting an explicit `snoozed_at`.
    pub fn snooze_conversation_at(
        &self,
        account: &AccountId,
        conversation: &ConversationId,
        wake_at: Timestamp,
        snoozed_at: Timestamp,
    ) -> Result<ConversationSnooze> {
        if wake_at <= snoozed_at {
            return Err(StoreError::Invalid("Snooze time must be in the future.".into()));
        }
        self.with_tx(|tx| {
            let msgs = load_messages_with(
                tx,
                "WHERE m.account_id=?1 AND m.conversation_id=?2",
                params![account.0, conversation.0],
                false,
            )?;
            let trash_id = label_id_of_kind_tx(tx, account, LabelKind::Trash)?;
            let spam_id = label_id_of_kind_tx(tx, account, LabelKind::Spam)?;
            let (mut baseline_latest_inbound, mut baseline_message_ids, mut baseline_rfc_ids) =
                build_snooze_baseline(&msgs, trash_id.as_ref(), spam_id.as_ref());

            if let Some(existing) = load_snooze_tx(tx, account, conversation)? {
                baseline_message_ids.extend(existing.baseline_message_ids);
                baseline_rfc_ids.extend(existing.baseline_rfc_ids);
                baseline_latest_inbound = match (baseline_latest_inbound, existing.baseline_latest_inbound) {
                    (Some(a), Some(b)) => Some(a.max(b)),
                    (a, b) => a.or(b),
                };
            }

            let msg_ids_json = serde_json::to_string(&baseline_message_ids)?;
            let rfc_ids_json = serde_json::to_string(&baseline_rfc_ids)?;
            tx.execute(
                "INSERT INTO conversation_snoozes(
                    account_id, conversation_id, wake_at, snoozed_at,
                    baseline_latest_inbound, baseline_message_ids_json, baseline_rfc_ids_json
                 ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(account_id, conversation_id) DO UPDATE SET
                    wake_at = excluded.wake_at,
                    snoozed_at = excluded.snoozed_at,
                    baseline_latest_inbound = excluded.baseline_latest_inbound,
                    baseline_message_ids_json = excluded.baseline_message_ids_json,
                    baseline_rfc_ids_json = excluded.baseline_rfc_ids_json",
                params![
                    account.0,
                    conversation.0,
                    wake_at,
                    snoozed_at,
                    baseline_latest_inbound,
                    msg_ids_json,
                    rfc_ids_json,
                ],
            )?;
            Ok(ConversationSnooze {
                account_id: account.clone(),
                conversation_id: conversation.clone(),
                wake_at,
                snoozed_at,
                baseline_latest_inbound,
                baseline_message_ids,
                baseline_rfc_ids,
            })
        })
    }

    /// Cancels an active snooze, immediately returning the conversation to Inbox.
    /// Returns `true` if a snooze row was removed.
    pub fn cancel_snooze(
        &self,
        account: &AccountId,
        conversation: &ConversationId,
    ) -> Result<bool> {
        self.with_conn(|c| {
            let n = c.execute(
                "DELETE FROM conversation_snoozes WHERE account_id=?1 AND conversation_id=?2",
                params![account.0, conversation.0],
            )?;
            Ok(n > 0)
        })
    }

    /// Returns the active snooze for `(account, conversation)`, if any.
    pub fn snooze_for(
        &self,
        account: &AccountId,
        conversation: &ConversationId,
    ) -> Result<Option<ConversationSnooze>> {
        self.with_read(|c| load_snooze_tx(c, account, conversation))
    }

    /// All active snoozes for `account`, soonest-waking first.
    pub fn snoozes_for_account(&self, account: &AccountId) -> Result<Vec<ConversationSnooze>> {
        self.with_read(|c| {
            let mut stmt = c.prepare_cached(
                "SELECT account_id, conversation_id, wake_at, snoozed_at, baseline_latest_inbound,
                        baseline_message_ids_json, baseline_rfc_ids_json
                 FROM conversation_snoozes
                 WHERE account_id=?1
                 ORDER BY wake_at ASC, conversation_id ASC",
            )?;
            let rows = stmt.query_map([&account.0], row_to_snooze)?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
    }

    /// Earliest upcoming snooze wake timestamp across all accounts, if any.
    pub fn next_snooze_wake(&self) -> Result<Option<Timestamp>> {
        self.with_read(|c| {
            let ts: Option<Timestamp> = c
                .query_row("SELECT MIN(wake_at) FROM conversation_snoozes", [], |r| r.get(0))?;
            Ok(ts)
        })
    }

    /// Wakes every snooze whose `wake_at <= now_ms` by atomically deleting its row and
    /// returning the awakened snoozes. Each snooze wakes at most once.
    pub fn wake_due_snoozes(&self, now_ms: Timestamp) -> Result<Vec<ConversationSnooze>> {
        self.with_tx(|tx| {
            let mut stmt = tx.prepare_cached(
                "SELECT account_id, conversation_id, wake_at, snoozed_at, baseline_latest_inbound,
                        baseline_message_ids_json, baseline_rfc_ids_json
                 FROM conversation_snoozes
                 WHERE wake_at <= ?1
                 ORDER BY wake_at ASC, account_id ASC, conversation_id ASC",
            )?;
            let rows = stmt.query_map([now_ms], row_to_snooze)?;
            let due: Vec<ConversationSnooze> = rows.collect::<std::result::Result<Vec<_>, _>>()?;
            drop(stmt);
            if !due.is_empty() {
                tx.execute("DELETE FROM conversation_snoozes WHERE wake_at <= ?1", [now_ms])?;
            }
            Ok(due)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ListFilter, LocalChange, MailboxView};

    fn setup_account(store: &Store, id: &str, email: &str) -> Account {
        let acct = Account {
            id: AccountId::from(id),
            provider: ProviderKind::Gmail,
            display_name: id.into(),
            email: email.into(),
            created_at: 1,
        };
        store.upsert_account(&acct).unwrap();
        let labels = vec![
            Label { id: LabelId::from(format!("{id}:inbox")), account_id: acct.id.clone(), kind: LabelKind::Inbox, name: "Inbox".into(), provider_id: Some(ProviderId("INBOX".into())), color: None, sort_order: 0 },
            Label { id: LabelId::from(format!("{id}:sent")), account_id: acct.id.clone(), kind: LabelKind::Sent, name: "Sent".into(), provider_id: Some(ProviderId("SENT".into())), color: None, sort_order: 1 },
            Label { id: LabelId::from(format!("{id}:trash")), account_id: acct.id.clone(), kind: LabelKind::Trash, name: "Trash".into(), provider_id: Some(ProviderId("TRASH".into())), color: None, sort_order: 2 },
            Label { id: LabelId::from(format!("{id}:spam")), account_id: acct.id.clone(), kind: LabelKind::Spam, name: "Spam".into(), provider_id: Some(ProviderId("SPAM".into())), color: None, sort_order: 3 },
            Label { id: LabelId::from(format!("{id}:work")), account_id: acct.id.clone(), kind: LabelKind::User, name: "Work".into(), provider_id: Some(ProviderId("Label_1".into())), color: None, sort_order: 10 },
        ];
        store.replace_labels(&acct.id, &labels).unwrap();
        acct
    }

    fn make_msg(acct: &Account, id: &str, conv: &str, date: Timestamp, outgoing: bool, rfc: Option<&str>, labels: &[&str]) -> Message {
        Message {
            id: MessageId::from(id),
            account_id: acct.id.clone(),
            conversation_id: ConversationId::from(conv),
            provider_id: Some(ProviderId(format!("prov-{id}"))),
            from: if outgoing { Address::new(None, acct.email.clone()) } else { Address::new(Some("Maya".into()), "maya@example.com") },
            to: vec![Address::new(None, if outgoing { "maya@example.com" } else { acct.email.as_str() })],
            cc: vec![],
            bcc: vec![],
            reply_to: None,
            subject: format!("Subject {conv}"),
            snippet: "Snippet".into(),
            body: Body { text: Some("Body".into()), html: None },
            date,
            is_read: false,
            is_starred: false,
            is_outgoing: outgoing,
            label_ids: labels.iter().map(|l| LabelId::from(format!("{}:{l}", acct.id.0))).collect(),
            attachments: vec![],
            rfc_message_id: rfc.map(str::to_owned),
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
    fn snooze_survives_restart_excludes_from_inbox_and_sorts_snoozed_soonest_first() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("snooze.sqlite");
        let acct_id = AccountId::from("acct-a");
        let conv_late = ConversationId::from("conv-late");
        let conv_soon = ConversationId::from("conv-soon");
        let conv_open = ConversationId::from("conv-open");

        {
            let store = Store::open(&path).unwrap();
            let acct = setup_account(&store, "acct-a", "a@example.com");
            store
                .upsert_messages(&[
                    make_msg(&acct, "m1", "conv-late", 1000, false, Some("<m1@x>"), &["inbox"]),
                    make_msg(&acct, "m2", "conv-soon", 1100, false, Some("<m2@x>"), &["inbox"]),
                    make_msg(&acct, "m3", "conv-open", 1200, false, Some("<m3@x>"), &["inbox"]),
                ])
                .unwrap();
            store.snooze_conversation_at(&acct.id, &conv_late, 9000, 2000).unwrap();
            store.snooze_conversation_at(&acct.id, &conv_soon, 5000, 2000).unwrap();
        }

        // Reopen database (simulating app restart).
        let store = Store::open(&path).unwrap();
        assert_eq!(store.next_snooze_wake().unwrap(), Some(5000));
        assert_eq!(store.snoozes_for_account(&acct_id).unwrap().len(), 2);

        let index = store.mail_index(&acct_id).unwrap();
        let inbox = index.conversations("a@example.com", &MailboxView::Kind(LabelKind::Inbox), ListFilter::All, None);
        assert_eq!(inbox.iter().map(|c| c.id.clone()).collect::<Vec<_>>(), vec![conv_open.clone()]);
        assert_eq!(index.view_count("a@example.com", &MailboxView::Kind(LabelKind::Inbox), None), 1, "snoozed unread mail is excluded from Inbox unread count");

        let snoozed = index.conversations("a@example.com", &MailboxView::Kind(LabelKind::Snoozed), ListFilter::All, None);
        assert_eq!(snoozed.iter().map(|c| c.id.clone()).collect::<Vec<_>>(), vec![conv_soon.clone(), conv_late.clone()], "Snoozed view sorts soonest-waking first");
        assert_eq!(snoozed[0].snoozed_until, Some(5000));
        assert_eq!(snoozed[1].snoozed_until, Some(9000));
        assert_eq!(index.view_count("a@example.com", &MailboxView::Kind(LabelKind::Snoozed), None), 2);
    }

    #[test]
    fn snooze_wakes_once_at_deadline_and_reconnect_resync_does_not_resurrect_it() {
        let store = Store::open_in_memory().unwrap();
        let acct = setup_account(&store, "acct-a", "a@example.com");
        let conv = ConversationId::from("conv-1");
        let m1 = make_msg(&acct, "m1", "conv-1", 1000, false, Some("<m1@x>"), &["inbox"]);
        store.upsert_messages(&[m1.clone()]).unwrap();
        store.snooze_conversation_at(&acct.id, &conv, 5000, 2000).unwrap();

        // Not due yet at 4999
        assert!(store.wake_due_snoozes(4999).unwrap().is_empty());
        assert!(store.snooze_for(&acct.id, &conv).unwrap().is_some());

        // Due at 5000: wakes ONCE
        let woken = store.wake_due_snoozes(5000).unwrap();
        assert_eq!(woken.len(), 1);
        assert_eq!(woken[0].conversation_id, conv);
        assert!(store.snooze_for(&acct.id, &conv).unwrap().is_none());
        assert!(store.wake_due_snoozes(6000).unwrap().is_empty(), "must not wake a second time");

        // Reconnect / resync of the same conversation does not resurrect the expired snooze
        store.merge_remote_messages(&[m1], &BTreeSet::new()).unwrap();
        assert!(store.snooze_for(&acct.id, &conv).unwrap().is_none());
        let inbox = store.conversations(&acct.id, "a@example.com", &MailboxView::Kind(LabelKind::Inbox), ListFilter::All, None).unwrap();
        assert_eq!(inbox.len(), 1);
    }

    #[test]
    fn snooze_wakes_on_new_inbound_reply_but_ignores_resync_backfill_outgoing_trash_and_spam() {
        let store = Store::open_in_memory().unwrap();
        let acct = setup_account(&store, "acct-a", "a@example.com");
        let conv = ConversationId::from("conv-1");
        let existing = make_msg(&acct, "m1", "conv-1", 1000, false, Some("<M1@Example.com>"), &["inbox"]);
        store.upsert_messages(&[existing.clone()]).unwrap();
        store.snooze_conversation_at(&acct.id, &conv, 50_000, 2000).unwrap();

        // 1. Re-syncing existing message (same id / same rfc) -> stays snoozed
        store.merge_remote_messages(&[existing.clone()], &BTreeSet::new()).unwrap();
        assert!(store.snooze_for(&acct.id, &conv).unwrap().is_some(), "resync of existing message must not wake");

        // 2. Re-downloaded copy with a fresh local ID but same RFC Message-ID -> stays snoozed
        let rekeyed = make_msg(&acct, "m1-new-id", "conv-1", 2500, false, Some("m1@example.com"), &["inbox"]);
        store.merge_remote_messages(&[rekeyed], &BTreeSet::new()).unwrap();
        assert!(store.snooze_for(&acct.id, &conv).unwrap().is_some(), "re-keyed copy with same RFC id must not wake");

        // 3. Older backfill message arriving on reconnect -> stays snoozed
        let backfill = make_msg(&acct, "m0-backfill", "conv-1", 500, false, Some("<m0@example.com>"), &["inbox"]);
        store.merge_remote_messages(&[backfill], &BTreeSet::new()).unwrap();
        assert!(store.snooze_for(&acct.id, &conv).unwrap().is_some(), "older backfill message must not wake");

        // 4. Outgoing reply -> stays snoozed
        let outgoing = make_msg(&acct, "m2-out", "conv-1", 3000, true, Some("<m2@example.com>"), &["sent"]);
        store.merge_remote_messages(&[outgoing], &BTreeSet::new()).unwrap();
        assert!(store.snooze_for(&acct.id, &conv).unwrap().is_some(), "outgoing message must not wake");

        // 5. Trashed & spam inbound messages -> stays snoozed
        let trashed = make_msg(&acct, "m3-trash", "conv-1", 3100, false, Some("<m3@example.com>"), &["trash"]);
        let spammed = make_msg(&acct, "m4-spam", "conv-1", 3200, false, Some("<m4@example.com>"), &["spam"]);
        store.merge_remote_messages(&[trashed, spammed], &BTreeSet::new()).unwrap();
        assert!(store.snooze_for(&acct.id, &conv).unwrap().is_some(), "trashed/spam messages must not wake");

        // 6. Genuine new inbound reply -> wakes immediately!
        let reply = make_msg(&acct, "m5-reply", "conv-1", 3500, false, Some("<m5@example.com>"), &["inbox"]);
        store.merge_remote_messages(&[reply], &BTreeSet::new()).unwrap();
        assert!(store.snooze_for(&acct.id, &conv).unwrap().is_none(), "genuine new inbound reply must wake snooze");

        let inbox = store.conversations(&acct.id, "a@example.com", &MailboxView::Kind(LabelKind::Inbox), ListFilter::All, None).unwrap();
        assert_eq!(inbox.len(), 1);
        assert_eq!(inbox[0].id, conv);
    }

    #[test]
    fn cancel_reschedule_account_isolation_and_removal_clear_snoozes() {
        let store = Store::open_in_memory().unwrap();
        let a = setup_account(&store, "acct-a", "a@example.com");
        let b = setup_account(&store, "acct-b", "b@example.com");
        let shared_conv = ConversationId::from("same-thread");

        store
            .upsert_messages(&[
                make_msg(&a, "a-m1", "same-thread", 1000, false, Some("<a1@x>"), &["inbox"]),
                make_msg(&b, "b-m1", "same-thread", 1000, false, Some("<b1@x>"), &["inbox"]),
            ])
            .unwrap();

        // Snoozing in account A does not affect same conversation_id in account B
        store.snooze_conversation_at(&a.id, &shared_conv, 5000, 2000).unwrap();
        assert!(store.snooze_for(&a.id, &shared_conv).unwrap().is_some());
        assert!(store.snooze_for(&b.id, &shared_conv).unwrap().is_none());
        assert_eq!(store.conversations(&a.id, "a@example.com", &MailboxView::Kind(LabelKind::Inbox), ListFilter::All, None).unwrap().len(), 0);
        assert_eq!(store.conversations(&b.id, "b@example.com", &MailboxView::Kind(LabelKind::Inbox), ListFilter::All, None).unwrap().len(), 1);

        // Reschedule updates wake_at and preserves baseline
        let rescheduled = store.snooze_conversation_at(&a.id, &shared_conv, 8000, 2500).unwrap();
        assert_eq!(rescheduled.wake_at, 8000);
        assert!(rescheduled.baseline_message_ids.contains(&MessageId::from("a-m1")));
        assert!(store.wake_due_snoozes(6000).unwrap().is_empty(), "old deadline must no longer trigger");

        // Cancel immediately returns to Inbox
        assert!(store.cancel_snooze(&a.id, &shared_conv).unwrap());
        assert!(!store.cancel_snooze(&a.id, &shared_conv).unwrap());
        assert_eq!(store.conversations(&a.id, "a@example.com", &MailboxView::Kind(LabelKind::Inbox), ListFilter::All, None).unwrap().len(), 1);

        // Archiving / trashing / deleting forever clears snooze so it doesn't linger in Snoozed
        store.snooze_conversation_at(&a.id, &shared_conv, 9000, 3000).unwrap();
        store.apply_change(&a.id, LocalChange::Archive { message_ids: vec![MessageId::from("a-m1")] }).unwrap();
        assert!(store.snooze_for(&a.id, &shared_conv).unwrap().is_none(), "archive clears snooze");

        store.snooze_conversation_at(&b.id, &shared_conv, 9000, 3000).unwrap();
        store.apply_change(&b.id, LocalChange::Trash { message_ids: vec![MessageId::from("b-m1")] }).unwrap();
        assert!(store.snooze_for(&b.id, &shared_conv).unwrap().is_none(), "trash clears snooze");

        // DeleteForever clears snooze
        store.upsert_messages(&[make_msg(&b, "b-m2", "conv-del", 1000, false, None, &["inbox"])]).unwrap();
        store.snooze_conversation_at(&b.id, &ConversationId::from("conv-del"), 9000, 3000).unwrap();
        store.apply_change(&b.id, LocalChange::DeleteForever { message_ids: vec![MessageId::from("b-m2")], folder: None }).unwrap();
        assert!(store.snooze_for(&b.id, &ConversationId::from("conv-del")).unwrap().is_none(), "delete forever clears snooze");
    }

    #[test]
    fn mail_index_optimistic_snooze_and_apply_mirror_store() {
        let store = Store::open_in_memory().unwrap();
        let a = setup_account(&store, "acct-a", "a@example.com");
        let conv = ConversationId::from("c1");
        store.upsert_messages(&[make_msg(&a, "m1", "c1", 1000, false, None, &["inbox"])]).unwrap();

        let mut idx = store.mail_index(&a.id).unwrap();
        idx.set_snooze(conv.clone(), 5000);
        assert_eq!(idx.snoozed_until(&conv), Some(5000));
        assert!(idx.conversations("a@example.com", &MailboxView::Kind(LabelKind::Inbox), ListFilter::All, None).is_empty());
        assert_eq!(idx.conversations("a@example.com", &MailboxView::Kind(LabelKind::Snoozed), ListFilter::All, None).len(), 1);

        // Archiving clears optimistic snooze too
        idx.apply(&LocalChange::Archive { message_ids: vec![MessageId::from("m1")] });
        assert_eq!(idx.snoozed_until(&conv), None);
        assert!(idx.conversations("a@example.com", &MailboxView::Kind(LabelKind::Snoozed), ListFilter::All, None).is_empty());
    }
}
