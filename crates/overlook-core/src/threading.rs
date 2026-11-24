//! Conversation grouping for providers that do not supply one.
//!
//! Gmail hands every message a thread id. IMAP and POP3 only give the RFC 5322 headers, so
//! conversations are rebuilt from `In-Reply-To` and `References`. Subject matching is
//! deliberately not used: it merges unrelated mail with the same subject, which is worse
//! than an occasional broken thread.

use crate::ids::{ConversationId, MessageId};
use crate::model::Message;
use std::collections::HashMap;

/// Normalises a Message-ID for comparison: trimmed and wrapped in angle brackets.
pub fn normalize_message_id(raw: &str) -> String {
    let inner = raw.trim().trim_matches(|c| c == '<' || c == '>').trim();
    if inner.is_empty() {
        return String::new();
    }
    format!("<{inner}>")
}

/// The ids a message points at, most specific first: `In-Reply-To`, then `References`
/// newest first. De-duplicated and normalised.
pub fn parent_candidates(message: &Message) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |raw: &str| {
        let id = normalize_message_id(raw);
        if !id.is_empty() && !out.contains(&id) {
            out.push(id);
        }
    };
    if let Some(parent) = &message.in_reply_to {
        push(parent);
    }
    for r in message.references.iter().rev() {
        push(r);
    }
    out
}

/// Assigns a conversation to every message whose `conversation_id` is empty.
///
/// For each message, the first of `In-Reply-To` and `References` (newest first) that
/// resolves to a known conversation wins. `lookup` answers for messages already on disk;
/// messages assigned earlier in this call are also known, including the ids they point at,
/// so a parent that arrives after its reply joins the reply's conversation. A message that
/// resolves nothing starts a conversation named `<account>:<its own Message-ID>`, or a
/// fresh uuid when it has no Message-ID.
///
/// Messages are processed oldest first so replies within one batch find their parents.
/// Messages that already carry a conversation id are left alone.
pub fn assign_conversations(incoming: &mut [Message], lookup: impl Fn(&str) -> Option<ConversationId>) {
    let mut order: Vec<usize> = (0..incoming.len()).collect();
    order.sort_by_key(|&i| incoming[i].date);

    // Message-ID (own or referenced) -> conversation, for everything seen in this batch.
    let mut known: HashMap<String, ConversationId> = HashMap::new();
    for m in incoming.iter().filter(|m| !m.conversation_id.0.is_empty()) {
        if let Some(id) = &m.rfc_message_id {
            let norm = normalize_message_id(id);
            if !norm.is_empty() {
                known.entry(norm).or_insert_with(|| m.conversation_id.clone());
            }
        }
    }

    for i in order {
        let own = incoming[i].rfc_message_id.as_deref().map(normalize_message_id).filter(|s| !s.is_empty());
        let candidates = parent_candidates(&incoming[i]);
        let mut resolved: Option<ConversationId> = None;
        if incoming[i].conversation_id.0.is_empty() {
            for c in &candidates {
                if let Some(conv) = known.get(c).cloned().or_else(|| lookup(c)) {
                    resolved = Some(conv);
                    break;
                }
            }
            if resolved.is_none() {
                if let Some(own_id) = &own {
                    // A reply that arrived earlier may already have named this conversation.
                    resolved = known.get(own_id).cloned().or_else(|| lookup(own_id));
                }
            }
            let conversation = resolved.unwrap_or_else(|| {
                let seed = own.clone().unwrap_or_else(|| MessageId::generate().0);
                ConversationId(format!("{}:{}", incoming[i].account_id.0, seed))
            });
            incoming[i].conversation_id = conversation;
        }
        let conversation = incoming[i].conversation_id.clone();
        if let Some(own_id) = own {
            known.entry(own_id).or_insert_with(|| conversation.clone());
        }
        for c in candidates {
            known.entry(c).or_insert_with(|| conversation.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::AccountId;
    use crate::model::{Address, Body};
    use std::collections::BTreeSet;

    fn msg(id: &str, rfc: Option<&str>, in_reply_to: Option<&str>, references: &[&str], date: i64) -> Message {
        Message {
            id: MessageId::from(id),
            account_id: AccountId::from("imap:j"),
            conversation_id: ConversationId(String::new()),
            provider_id: None,
            from: Address::named("Maya", "maya@northstar.example"),
            to: vec![],
            cc: vec![],
            bcc: vec![],
            reply_to: None,
            subject: "s".into(),
            snippet: String::new(),
            body: Body::default(),
            date,
            is_read: false,
            is_starred: false,
            is_outgoing: false,
            label_ids: BTreeSet::new(),
            attachments: vec![],
            rfc_message_id: rfc.map(str::to_owned),
            in_reply_to: in_reply_to.map(str::to_owned),
            references: references.iter().map(|r| r.to_string()).collect(),
            category: None,
            is_important: false,
            focus_override: None,
            invitation: None,
            read_receipt: None,
        }
    }

    fn no_lookup(_: &str) -> Option<ConversationId> {
        None
    }

    #[test]
    fn in_reply_to_joins_a_known_conversation() {
        let mut batch = vec![msg("m2", Some("<b@x>"), Some("<a@x>"), &["<a@x>"], 2)];
        assign_conversations(&mut batch, |id| (id == "<a@x>").then(|| ConversationId::from("imap:j:<a@x>")));
        assert_eq!(batch[0].conversation_id.as_str(), "imap:j:<a@x>");
    }

    #[test]
    fn references_are_tried_newest_first() {
        // In-Reply-To is unknown; References lists root, middle, newest. Both root and middle
        // are known but in different conversations (a split thread): the newest wins.
        let mut batch = vec![msg("m3", Some("<d@x>"), Some("<gone@x>"), &["<root@x>", "<mid@x>"], 3)];
        assign_conversations(&mut batch, |id| match id {
            "<root@x>" => Some(ConversationId::from("conv-root")),
            "<mid@x>" => Some(ConversationId::from("conv-mid")),
            _ => None,
        });
        assert_eq!(batch[0].conversation_id.as_str(), "conv-mid");
    }

    #[test]
    fn unknown_parents_start_a_conversation_named_after_the_message() {
        let mut batch = vec![msg("m1", Some("<a@x>"), Some("<nobody@x>"), &["<nobody@x>"], 1), msg("m0", None, None, &[], 0)];
        assign_conversations(&mut batch, no_lookup);
        assert_eq!(batch[0].conversation_id.as_str(), "imap:j:<a@x>");
        assert!(batch[1].conversation_id.as_str().starts_with("imap:j:"), "{}", batch[1].conversation_id);
        assert_ne!(batch[0].conversation_id, batch[1].conversation_id);
    }

    #[test]
    fn replies_in_the_same_batch_follow_their_parent_regardless_of_order() {
        let mut batch = vec![
            msg("reply", Some("<b@x>"), Some("<a@x>"), &["<a@x>"], 20),
            msg("root", Some("<a@x>"), None, &[], 10),
            msg("other", Some("<z@x>"), None, &[], 15),
        ];
        assign_conversations(&mut batch, no_lookup);
        assert_eq!(batch[0].conversation_id, batch[1].conversation_id);
        assert_eq!(batch[1].conversation_id.as_str(), "imap:j:<a@x>");
        assert_ne!(batch[2].conversation_id, batch[1].conversation_id);
    }

    #[test]
    fn parent_arriving_after_its_reply_joins_the_reply() {
        // The reply was downloaded in an earlier cycle and named the conversation after
        // itself; the lookup knows the parent id only through that reply's References.
        let mut batch = vec![msg("root", Some("<a@x>"), None, &[], 10)];
        assign_conversations(&mut batch, |id| (id == "<a@x>").then(|| ConversationId::from("imap:j:<b@x>")));
        assert_eq!(batch[0].conversation_id.as_str(), "imap:j:<b@x>");
        // Within one batch, a later-dated parent still joins.
        let mut batch = vec![msg("reply", Some("<b@x>"), Some("<a@x>"), &[], 5), msg("root", Some("<a@x>"), None, &[], 10)];
        assign_conversations(&mut batch, no_lookup);
        assert_eq!(batch[0].conversation_id, batch[1].conversation_id);
    }

    #[test]
    fn existing_conversation_ids_are_kept_and_ids_are_normalised() {
        let mut keep = msg("g", Some("<a@x>"), None, &[], 1);
        keep.conversation_id = ConversationId::from("gmail:thread");
        let mut batch = vec![keep, msg("r", Some("b@x"), Some(" a@x "), &[], 2)];
        assign_conversations(&mut batch, no_lookup);
        assert_eq!(batch[0].conversation_id.as_str(), "gmail:thread");
        assert_eq!(batch[1].conversation_id.as_str(), "gmail:thread", "bare ids match bracketed ones");
        assert_eq!(normalize_message_id("<x@y>"), "<x@y>");
        assert_eq!(normalize_message_id("x@y"), "<x@y>");
        assert_eq!(parent_candidates(&msg("p", None, Some("<c@x>"), &["<a@x>", "<b@x>", "<c@x>"], 0)), vec!["<c@x>", "<b@x>", "<a@x>"]);
    }

    #[test]
    fn empty_or_bracket_only_message_ids_never_collapse_unrelated_threads() {
        for empty in ["", "  ", "<>", "<", ">", "<  >", "<<<>>>"] {
            assert_eq!(normalize_message_id(empty), "", "{empty:?}");
        }
        let mut batch = vec![
            msg("m1", Some("<>"), Some("<"), &["<>"], 1),
            msg("m2", Some("<"), Some("<>"), &[">"], 2),
        ];
        assign_conversations(&mut batch, no_lookup);
        assert_ne!(batch[0].conversation_id, batch[1].conversation_id);
    }
}
