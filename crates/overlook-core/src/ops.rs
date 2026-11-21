//! Pending operations: local intent waiting to be applied at the provider.
//!
//! Every mailbox change the user makes is applied to local state and recorded here in the
//! same transaction. The sync engine replays operations per account in `seq` order.

use crate::ids::*;
use crate::model::LabelKind;
use crate::time::Timestamp;
use serde::{Deserialize, Serialize};

/// What the user asked for. Payloads reference local ids; the provider adapter maps them to
/// provider ids at replay time so that operations can be queued before a provider id exists.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "op")]
pub enum OperationKind {
    SetRead { message_ids: Vec<MessageId>, read: bool },
    SetStarred { message_ids: Vec<MessageId>, starred: bool },
    AddLabel { message_ids: Vec<MessageId>, label_id: LabelId },
    RemoveLabel { message_ids: Vec<MessageId>, label_id: LabelId },
    /// Remove from Inbox (Gmail: remove INBOX label).
    Archive { message_ids: Vec<MessageId> },
    /// Move to Trash.
    Trash { message_ids: Vec<MessageId> },
    /// Restore from Trash to Inbox.
    Untrash { message_ids: Vec<MessageId> },
    /// Delete for good ("Empty Folder"). The local rows go at once, so unlike the other
    /// changes this one carries the provider ids the server needs. With `folder` the
    /// server empties that whole folder, including messages the client never downloaded.
    DeleteForever {
        message_ids: Vec<MessageId>,
        #[serde(default)]
        provider_ids: Vec<ProviderId>,
        #[serde(default)]
        folder: Option<LabelKind>,
    },
    /// Submit a queued draft. The draft row holds the content.
    SendDraft { draft_id: DraftId },
    /// Answer an invitation.
    RespondToEvent { event_id: EventId, response: crate::calendar::ResponseStatus },
    /// Create an event that exists locally.
    CreateEvent { event_id: EventId },
    /// Push local edits of an event; `scope` widens it to the series.
    UpdateEvent {
        event_id: EventId,
        #[serde(default)]
        scope: crate::calendar::RecurrenceScope,
    },
    /// Delete an event (provider id kept so the row can already be gone locally). For a
    /// series the master id and the split point travel with the operation.
    DeleteEvent {
        event_id: EventId,
        calendar_id: CalendarId,
        provider_id: Option<ProviderId>,
        #[serde(default)]
        scope: crate::calendar::RecurrenceScope,
        #[serde(default)]
        recurring_event_id: Option<String>,
        #[serde(default)]
        original_start: Option<Timestamp>,
    },
    /// Push a contact created on this device.
    CreateContact {
        contact_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_book: Option<String>,
    },
    /// Push local edits of a contact.
    UpdateContact { contact_id: String },
    /// Delete a contact at the provider.
    DeleteContact { contact_id: String, resource_name: Option<String> },
}

impl OperationKind {
    /// Short human description for status surfaces and logs.
    pub fn describe(&self) -> String {
        fn n(ids: &[MessageId]) -> String {
            if ids.len() == 1 { "1 message".into() } else { format!("{} messages", ids.len()) }
        }
        match self {
            OperationKind::SetRead { message_ids, read: true } => format!("Mark {} as read", n(message_ids)),
            OperationKind::SetRead { message_ids, read: false } => format!("Mark {} as unread", n(message_ids)),
            OperationKind::SetStarred { message_ids, starred: true } => format!("Star {}", n(message_ids)),
            OperationKind::SetStarred { message_ids, starred: false } => format!("Unstar {}", n(message_ids)),
            OperationKind::AddLabel { message_ids, .. } => format!("Label {}", n(message_ids)),
            OperationKind::RemoveLabel { message_ids, .. } => format!("Remove label from {}", n(message_ids)),
            OperationKind::Archive { message_ids } => format!("Archive {}", n(message_ids)),
            OperationKind::Trash { message_ids } => format!("Delete {}", n(message_ids)),
            OperationKind::Untrash { message_ids } => format!("Restore {}", n(message_ids)),
            OperationKind::DeleteForever { folder: Some(LabelKind::Spam), .. } => "Empty Junk".into(),
            OperationKind::DeleteForever { folder: Some(kind), .. } => format!("Empty {}", crate::text::capitalize(kind.as_key())),
            OperationKind::DeleteForever { message_ids, .. } => format!("Permanently delete {}", n(message_ids)),
            OperationKind::SendDraft { .. } => "Send message".to_owned(),
            OperationKind::RespondToEvent { response, .. } => format!("Respond to invitation ({})", response.label()),
            OperationKind::CreateEvent { .. } => "Create event".to_owned(),
            OperationKind::UpdateEvent { .. } => "Update event".to_owned(),
            OperationKind::DeleteEvent { .. } => "Delete event".to_owned(),
            OperationKind::CreateContact { .. } => "Add contact".to_owned(),
            OperationKind::UpdateContact { .. } => "Update contact".to_owned(),
            OperationKind::DeleteContact { .. } => "Delete contact".to_owned(),
        }
    }

    pub fn is_contact(&self) -> bool {
        matches!(self, OperationKind::CreateContact { .. } | OperationKind::UpdateContact { .. } | OperationKind::DeleteContact { .. })
    }

    pub fn contact_id(&self) -> Option<&str> {
        match self {
            OperationKind::CreateContact { contact_id, .. } | OperationKind::UpdateContact { contact_id } | OperationKind::DeleteContact { contact_id, .. } => Some(contact_id),
            _ => None,
        }
    }

    pub fn is_calendar(&self) -> bool {
        matches!(
            self,
            OperationKind::RespondToEvent { .. } | OperationKind::CreateEvent { .. } | OperationKind::UpdateEvent { .. } | OperationKind::DeleteEvent { .. }
        )
    }

    pub fn event_id(&self) -> Option<&EventId> {
        match self {
            OperationKind::RespondToEvent { event_id, .. } | OperationKind::CreateEvent { event_id } | OperationKind::UpdateEvent { event_id, .. } | OperationKind::DeleteEvent { event_id, .. } => Some(event_id),
            _ => None,
        }
    }

    /// Message ids this operation touches (empty for sends).
    pub fn message_ids(&self) -> &[MessageId] {
        match self {
            OperationKind::SetRead { message_ids, .. }
            | OperationKind::SetStarred { message_ids, .. }
            | OperationKind::AddLabel { message_ids, .. }
            | OperationKind::RemoveLabel { message_ids, .. }
            | OperationKind::Archive { message_ids }
            | OperationKind::Trash { message_ids }
            | OperationKind::Untrash { message_ids }
            | OperationKind::DeleteForever { message_ids, .. } => message_ids,
            OperationKind::SendDraft { .. }
            | OperationKind::RespondToEvent { .. }
            | OperationKind::CreateEvent { .. }
            | OperationKind::UpdateEvent { .. }
            | OperationKind::DeleteEvent { .. }
            | OperationKind::CreateContact { .. }
            | OperationKind::UpdateContact { .. }
            | OperationKind::DeleteContact { .. } => &[],
        }
    }

    /// Whether the operation moves messages between folders at the provider, so a message
    /// vanishing from its folder is expected rather than a deletion made elsewhere.
    pub fn is_move(&self) -> bool {
        matches!(self, OperationKind::AddLabel { .. } | OperationKind::RemoveLabel { .. } | OperationKind::Archive { .. } | OperationKind::Trash { .. } | OperationKind::Untrash { .. })
    }

    /// Whether replaying the operation twice is harmless at the provider. Sends are not.
    pub fn is_idempotent(&self) -> bool {
        !matches!(self, OperationKind::SendDraft { .. })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OperationState {
    Pending,
    InFlight,
    /// A non-idempotent operation was interrupted; reconcile before retrying.
    Uncertain,
    Failed,
    Applied,
}

impl OperationState {
    pub fn as_key(self) -> &'static str {
        match self {
            OperationState::Pending => "pending",
            OperationState::InFlight => "in-flight",
            OperationState::Uncertain => "uncertain",
            OperationState::Failed => "failed",
            OperationState::Applied => "applied",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Some(match key {
            "pending" => OperationState::Pending,
            "in-flight" => OperationState::InFlight,
            "uncertain" => OperationState::Uncertain,
            "failed" => OperationState::Failed,
            "applied" => OperationState::Applied,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingOperation {
    pub id: OperationId,
    pub account_id: AccountId,
    /// Monotonic per-account sequence; replay order.
    pub seq: i64,
    pub kind: OperationKind,
    pub state: OperationState,
    pub attempts: u32,
    pub last_error: Option<String>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

/// A replay plan with fewer provider calls. `send` holds what to send, in order, each with
/// the queued operations it stands for; `superseded` are operations a later one makes
/// pointless, to be marked applied without contacting the provider.
#[derive(Debug, Default)]
pub struct ReplayPlan {
    pub send: Vec<(PendingOperation, Vec<OperationId>)>,
    pub superseded: Vec<OperationId>,
}

/// Plans the replay of pending mailbox changes:
/// - a read or star change is dropped when a later change of the same flag covers all of
///   its messages (only the final state matters);
/// - consecutive changes of the same kind (mark read, archive, add the same label…) merge
///   into one, so a burst of triage reaches the provider as one batch call.
///
/// Order between different kinds is kept, so the provider ends in the same state.
pub fn plan_replay(ops: Vec<PendingOperation>) -> ReplayPlan {
    #[derive(PartialEq, Eq)]
    enum MergeKey {
        Read(bool),
        Star(bool),
        AddLabel(LabelId),
        RemoveLabel(LabelId),
        Archive,
        Trash,
        Untrash,
    }
    fn merge_key(kind: &OperationKind) -> Option<MergeKey> {
        Some(match kind {
            OperationKind::SetRead { read, .. } => MergeKey::Read(*read),
            OperationKind::SetStarred { starred, .. } => MergeKey::Star(*starred),
            OperationKind::AddLabel { label_id, .. } => MergeKey::AddLabel(label_id.clone()),
            OperationKind::RemoveLabel { label_id, .. } => MergeKey::RemoveLabel(label_id.clone()),
            OperationKind::Archive { .. } => MergeKey::Archive,
            OperationKind::Trash { .. } => MergeKey::Trash,
            OperationKind::Untrash { .. } => MergeKey::Untrash,
            _ => return None,
        })
    }
    fn ids_mut(kind: &mut OperationKind) -> Option<&mut Vec<MessageId>> {
        match kind {
            OperationKind::SetRead { message_ids, .. }
            | OperationKind::SetStarred { message_ids, .. }
            | OperationKind::AddLabel { message_ids, .. }
            | OperationKind::RemoveLabel { message_ids, .. }
            | OperationKind::Archive { message_ids }
            | OperationKind::Trash { message_ids }
            | OperationKind::Untrash { message_ids } => Some(message_ids),
            _ => None,
        }
    }

    let mut plan = ReplayPlan::default();
    let mut seen_read: std::collections::HashSet<MessageId> = std::collections::HashSet::new();
    let mut seen_star: std::collections::HashSet<MessageId> = std::collections::HashSet::new();
    let mut kept: Vec<PendingOperation> = Vec::with_capacity(ops.len());
    for op in ops.into_iter().rev() {
        let superseded = match &op.kind {
            OperationKind::SetRead { message_ids, .. } => {
                let sup = !message_ids.is_empty() && message_ids.iter().all(|id| seen_read.contains(id));
                if !sup {
                    seen_read.extend(message_ids.iter().cloned());
                }
                sup
            }
            OperationKind::SetStarred { message_ids, .. } => {
                let sup = !message_ids.is_empty() && message_ids.iter().all(|id| seen_star.contains(id));
                if !sup {
                    seen_star.extend(message_ids.iter().cloned());
                }
                sup
            }
            _ => false,
        };
        if superseded {
            plan.superseded.push(op.id);
        } else {
            kept.push(op);
        }
    }
    plan.superseded.reverse();
    kept.reverse();

    let mut last_key: Option<MergeKey> = None;
    let mut last_ids: std::collections::HashSet<MessageId> = std::collections::HashSet::new();
    for op in kept {
        let key = merge_key(&op.kind);
        if key.is_some() && key == last_key {
            if let Some((last, covered)) = plan.send.last_mut() {
                let ids = ids_mut(&mut last.kind).expect("mergeable kinds carry message ids");
                for id in op.kind.message_ids() {
                    if last_ids.insert(id.clone()) {
                        ids.push(id.clone());
                    }
                }
                covered.push(op.id);
                continue;
            }
        }
        last_ids.clear();
        if key.is_some() {
            last_ids.extend(op.kind.message_ids().iter().cloned());
        }
        last_key = key;
        let id = op.id.clone();
        plan.send.push((op, vec![id]));
    }
    plan
}

#[cfg(test)]
mod plan_tests {
    use super::*;

    fn op(seq: i64, kind: OperationKind) -> PendingOperation {
        PendingOperation {
            id: OperationId(format!("op{seq}")),
            account_id: AccountId::from("a"),
            seq,
            kind,
            state: OperationState::Pending,
            attempts: 0,
            last_error: None,
            created_at: 0,
            updated_at: 0,
        }
    }
    fn ids(list: &[&str]) -> Vec<MessageId> {
        list.iter().map(|s| MessageId::from(*s)).collect()
    }

    #[test]
    fn a_burst_of_reads_becomes_one_call() {
        let plan = plan_replay(vec![
            op(1, OperationKind::SetRead { message_ids: ids(&["a"]), read: true }),
            op(2, OperationKind::SetRead { message_ids: ids(&["b"]), read: true }),
            op(3, OperationKind::SetRead { message_ids: ids(&["c", "a"]), read: true }),
        ]);
        assert!(plan.superseded.contains(&OperationId("op1".into())), "a is read again later");
        assert_eq!(plan.send.len(), 1);
        assert_eq!(plan.send[0].0.kind, OperationKind::SetRead { message_ids: ids(&["b", "c", "a"]), read: true });
        assert_eq!(plan.send[0].1.len(), 2);
    }

    #[test]
    fn only_the_final_star_state_is_sent() {
        let plan = plan_replay(vec![
            op(1, OperationKind::SetStarred { message_ids: ids(&["a"]), starred: true }),
            op(2, OperationKind::SetStarred { message_ids: ids(&["a"]), starred: false }),
        ]);
        assert_eq!(plan.superseded, vec![OperationId("op1".into())]);
        assert_eq!(plan.send.len(), 1);
        assert_eq!(plan.send[0].0.kind, OperationKind::SetStarred { message_ids: ids(&["a"]), starred: false });
    }

    #[test]
    fn different_kinds_keep_their_order_and_sends_never_merge() {
        let plan = plan_replay(vec![
            op(1, OperationKind::Archive { message_ids: ids(&["a"]) }),
            op(2, OperationKind::Untrash { message_ids: ids(&["a"]) }),
            op(3, OperationKind::Archive { message_ids: ids(&["b"]) }),
            op(4, OperationKind::SendDraft { draft_id: DraftId("d".into()) }),
            op(5, OperationKind::SendDraft { draft_id: DraftId("e".into()) }),
        ]);
        let kinds: Vec<i64> = plan.send.iter().map(|(o, _)| o.seq).collect();
        assert_eq!(kinds, vec![1, 2, 3, 4, 5]);
        assert!(plan.superseded.is_empty());
    }

    #[test]
    fn labels_merge_only_with_the_same_label() {
        let plan = plan_replay(vec![
            op(1, OperationKind::AddLabel { message_ids: ids(&["a"]), label_id: LabelId::from("x") }),
            op(2, OperationKind::AddLabel { message_ids: ids(&["b"]), label_id: LabelId::from("x") }),
            op(3, OperationKind::AddLabel { message_ids: ids(&["c"]), label_id: LabelId::from("y") }),
        ]);
        assert_eq!(plan.send.len(), 2);
        assert_eq!(plan.send[0].1, vec![OperationId("op1".into()), OperationId("op2".into())]);
    }

    #[test]
    fn reverse_pass_superseding_and_dedup_merging_preserve_semantics() {
        let plan = plan_replay(vec![
            op(1, OperationKind::SetRead { message_ids: ids(&["a", "b"]), read: true }),
            op(2, OperationKind::SetRead { message_ids: ids(&["a"]), read: false }),
            op(3, OperationKind::Archive { message_ids: ids(&["x", "y"]) }),
            op(4, OperationKind::Archive { message_ids: ids(&["y", "z", "x"]) }),
            op(5, OperationKind::SetRead { message_ids: ids(&["b"]), read: false }),
        ]);
        // op1 is fully superseded because both `a` (op2) and `b` (op5) are overwritten later.
        assert_eq!(plan.superseded, vec![OperationId("op1".into())]);
        assert_eq!(plan.send.len(), 3);
        assert_eq!(plan.send[0].0.seq, 2);
        assert_eq!(plan.send[1].0.kind, OperationKind::Archive { message_ids: ids(&["x", "y", "z"]) });
        assert_eq!(plan.send[1].1, vec![OperationId("op3".into()), OperationId("op4".into())]);
        assert_eq!(plan.send[2].0.seq, 5);
    }
}
