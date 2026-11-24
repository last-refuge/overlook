//! Focused Inbox classification.
//!
//! Focused Inbox is a local view, not a provider feature. A message lands in Focused when
//! the user said so (per-message override or per-sender rule), when it comes from someone
//! the user has written to, or when the provider marks it personal or important. Bulk
//! categories (promotions, social, forums, updates) go to Other unless overridden.

use crate::model::Message;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum FocusTab {
    #[default]
    Focused,
    Other,
}

/// A standing rule for a sender address.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FocusRule {
    pub email: String,
    pub focused: bool,
}

/// Returns true when `message` belongs in Focused.
///
/// `rules` maps lowercase sender addresses to the user's standing choice, and
/// `correspondents` holds lowercase addresses the user has sent mail to.
pub fn classify_focus(message: &Message, rules: &HashMap<String, bool>, correspondents: &HashSet<String>) -> bool {
    if let Some(forced) = message.focus_override {
        return forced;
    }
    let sender = message.from.email.to_ascii_lowercase();
    if let Some(&focused) = rules.get(&sender) {
        return focused;
    }
    if message.is_outgoing {
        return true;
    }
    if correspondents.contains(&sender) {
        return true;
    }
    match message.category.as_deref() {
        Some("promotions") | Some("social") | Some("forums") | Some("updates") => false,
        Some("personal") => true,
        _ => message.is_important || looks_personal(&sender),
    }
}

/// Without provider hints, automated senders go to Other.
fn looks_personal(sender: &str) -> bool {
    let local = sender.split('@').next().unwrap_or("");
    !["noreply", "no-reply", "donotreply", "do-not-reply", "notifications", "notification", "newsletter", "news", "digest", "mailer", "bounce", "alerts", "alert", "updates", "info", "marketing", "support", "billing", "receipts", "orders"]
        .iter()
        .any(|p| local.starts_with(p) || local.ends_with(p))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::*;

    fn msg(from: &str) -> Message {
        Message {
            id: MessageId::from("m"),
            account_id: AccountId::from("a"),
            conversation_id: ConversationId::from("c"),
            provider_id: None,
            from: Address::new(None, from),
            to: vec![],
            cc: vec![],
            bcc: vec![],
            reply_to: None,
            subject: String::new(),
            snippet: String::new(),
            body: Body::default(),
            date: 0,
            is_read: false,
            is_starred: false,
            is_outgoing: false,
            label_ids: Default::default(),
            attachments: vec![],
            rfc_message_id: None,
            in_reply_to: None,
            references: Vec::new(),
            category: None,
            is_important: false,
            focus_override: None,
            invitation: None,
            read_receipt: None,
        }
    }

    #[test]
    fn overrides_and_rules_win() {
        let mut m = msg("deals@shop.example");
        m.category = Some("promotions".into());
        assert!(!classify_focus(&m, &HashMap::new(), &HashSet::new()));
        m.focus_override = Some(true);
        assert!(classify_focus(&m, &HashMap::new(), &HashSet::new()));
        m.focus_override = None;
        let rules = HashMap::from([("deals@shop.example".to_owned(), true)]);
        assert!(classify_focus(&m, &rules, &HashSet::new()));
    }

    #[test]
    fn correspondents_and_categories() {
        let mut m = msg("maya@northstar.example");
        assert!(classify_focus(&m, &HashMap::new(), &HashSet::new()), "unknown human-looking sender defaults to Focused");
        m.category = Some("updates".into());
        assert!(!classify_focus(&m, &HashMap::new(), &HashSet::new()));
        let known = HashSet::from(["maya@northstar.example".to_owned()]);
        assert!(classify_focus(&m, &HashMap::new(), &known), "people you write to stay Focused");
        let auto = msg("no-reply@service.example");
        assert!(!classify_focus(&auto, &HashMap::new(), &HashSet::new()));
        let mut important = msg("alerts@bank.example");
        important.is_important = true;
        assert!(classify_focus(&important, &HashMap::new(), &HashSet::new()));
    }
}
