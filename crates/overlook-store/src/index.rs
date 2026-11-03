//! An in-memory index of one account's message headers, for building the conversation
//! list and sidebar counts without touching the database.
//!
//! The index holds every message without its body, grouped by conversation. The window
//! keeps one, applies each local change to it the moment the user acts (so the list
//! updates at once), and replaces it with a fresh copy built on a background thread once
//! the change is written and after every sync.

use crate::{LocalChange, MailboxView, ListFilter};
use overlook_core::focus::{classify_focus, FocusTab};
use overlook_core::*;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

#[derive(Clone, Debug, Default)]
pub struct MailIndex {
    pub(crate) labels: Vec<Label>,
    pub(crate) by_conv: BTreeMap<ConversationId, Vec<Message>>,
    /// Which conversation each message belongs to.
    pub(crate) conv_of: HashMap<MessageId, ConversationId>,
    pub(crate) pending: BTreeSet<MessageId>,
    pub(crate) rules: HashMap<String, bool>,
    pub(crate) correspondents: HashSet<String>,
}

impl MailIndex {
    pub(crate) fn new(labels: Vec<Label>, messages: Vec<Message>, pending: BTreeSet<MessageId>, rules: HashMap<String, bool>) -> Self {
        let mut correspondents = HashSet::new();
        let mut by_conv: BTreeMap<ConversationId, Vec<Message>> = BTreeMap::new();
        let mut conv_of = HashMap::new();
        for m in messages {
            if m.is_outgoing {
                for a in m.to.iter().chain(m.cc.iter()) {
                    correspondents.insert(a.email.to_ascii_lowercase());
                }
            }
            conv_of.insert(m.id.clone(), m.conversation_id.clone());
            by_conv.entry(m.conversation_id.clone()).or_default().push(m);
        }
        Self { labels, by_conv, conv_of, pending, rules, correspondents }
    }

    pub fn labels(&self) -> &[Label] {
        &self.labels
    }

    /// A message's headers, flags and labels (no body).
    pub fn message(&self, id: &MessageId) -> Option<&Message> {
        let conv = self.conv_of.get(id)?;
        self.by_conv.get(conv)?.iter().find(|m| &m.id == id)
    }

    /// Addresses the account has corresponded with matching `prefix`, most frequent first.
    pub fn known_recipients(&self, prefix: &str, limit: usize) -> Vec<Address> {
        crate::known_recipients(self.by_conv.values().flatten(), prefix, limit)
    }

    /// The ids of a conversation's messages, oldest first.
    pub fn message_ids_in(&self, conversation: &ConversationId) -> Vec<MessageId> {
        let mut msgs: Vec<&Message> = self.by_conv.get(conversation).map(|m| m.iter().collect()).unwrap_or_default();
        msgs.sort_by_key(|m| m.date);
        msgs.into_iter().map(|m| m.id.clone()).collect()
    }

    pub(crate) fn label_of_kind(&self, kind: LabelKind) -> Option<LabelId> {
        self.labels.iter().find(|l| l.kind == kind).map(|l| l.id.clone())
    }

    /// Conversation summaries for a mailbox view, newest first. `focus` splits the Inbox
    /// into Focused and Other; `None` shows everything.
    pub fn conversations(&self, owner_email: &str, view: &MailboxView, filter: ListFilter, focus: Option<FocusTab>) -> Vec<ConversationSummary> {
        let mut out = Vec::new();
        for visible in self.visible_conversations(view, focus) {
            let keep = match filter {
                ListFilter::All => true,
                ListFilter::Unread => visible.iter().any(|m| !m.is_read),
                ListFilter::Attachments => visible.iter().any(|m| Message::has_attachments(m)),
            };
            if !keep {
                continue;
            }
            let msgs: Vec<Message> = visible.into_iter().cloned().collect();
            if let Some(summary) = ConversationSummary::from_messages(&msgs, owner_email, &self.pending) {
                out.push(summary);
            }
        }
        out.sort_by_key(|c| std::cmp::Reverse(c.latest));
        out
    }

    /// The messages shown for each conversation in a view, in index order: the
    /// conversations with a message in the view, filtered by `focus`, each reduced to the
    /// messages visible there. Shared by the list and the sidebar counts so they agree.
    fn visible_conversations<'a>(&'a self, view: &'a MailboxView, focus: Option<FocusTab>) -> impl Iterator<Item = Vec<&'a Message>> + 'a {
        let kind_of = |id: &LabelId| self.labels.iter().find(|l| &l.id == id).map(|l| l.kind);
        let inbox = self.label_of_kind(LabelKind::Inbox);
        let trash = self.label_of_kind(LabelKind::Trash);
        let spam = self.label_of_kind(LabelKind::Spam);
        let sent = self.label_of_kind(LabelKind::Sent);
        let in_trash = move |m: &Message| trash.as_ref().is_some_and(|t| m.has_label(t));
        let in_spam = move |m: &Message| spam.as_ref().is_some_and(|t| m.has_label(t));

        // Every message of a conversation decides whether it belongs to the view. This
        // keeps conversation counts consistent across views (a thread in Inbox still shows
        // all its messages).
        self.by_conv.values().filter_map(move |msgs| {
            let matches_view = msgs.iter().any(|m| match view {
                MailboxView::Label(id) => m.has_label(id) && !in_trash(m),
                MailboxView::Kind(LabelKind::Inbox) => inbox.as_ref().is_some_and(|i| m.has_label(i)) && !in_trash(m),
                MailboxView::Kind(LabelKind::Starred) => m.is_starred && !in_trash(m),
                MailboxView::Kind(LabelKind::Sent) => sent.as_ref().is_some_and(|s| m.has_label(s)) && !in_trash(m),
                MailboxView::Kind(LabelKind::Trash) => in_trash(m),
                MailboxView::Kind(LabelKind::Spam) => in_spam(m),
                MailboxView::Kind(LabelKind::Archive) => {
                    !in_trash(m)
                        && !in_spam(m)
                        && !inbox.as_ref().is_some_and(|i| m.has_label(i))
                        && !sent.as_ref().is_some_and(|s| m.has_label(s))
                }
                MailboxView::Kind(LabelKind::Drafts | LabelKind::Outbox | LabelKind::Container) => false,
                MailboxView::Kind(LabelKind::User) => m.label_ids.iter().any(|l| kind_of(l) == Some(LabelKind::User)),
            });
            if !matches_view {
                return None;
            }
            // Inside Trash show only trashed messages; elsewhere hide trashed ones.
            let visible: Vec<&Message> = match view {
                MailboxView::Kind(LabelKind::Trash) => msgs.iter().filter(|m| in_trash(m)).collect(),
                _ => msgs.iter().filter(|m| !in_trash(m)).collect(),
            };
            if visible.is_empty() {
                return None;
            }
            if let Some(tab) = focus {
                // A conversation follows its newest incoming message; threads with only our
                // own mail count as Focused.
                let decisive = visible.iter().filter(|m| !m.is_outgoing).max_by_key(|m| m.date).or_else(|| visible.iter().max_by_key(|m| m.date));
                let focused = decisive.map(|m| classify_focus(m, &self.rules, &self.correspondents)).unwrap_or(true);
                if focused != (tab == FocusTab::Focused) {
                    return None;
                }
            }
            Some(visible)
        })
    }

    /// The messages of a conversation that `view` shows, oldest first: only the trashed
    /// ones inside Trash, only the others elsewhere. For deleting a row for good, which
    /// must not reach into other folders.
    pub fn visible_message_ids_in(&self, conversation: &ConversationId, view: &MailboxView) -> Vec<MessageId> {
        let trash = self.label_of_kind(LabelKind::Trash);
        let in_trash = |m: &Message| trash.as_ref().is_some_and(|t| m.has_label(t));
        let trash_view = matches!(view, MailboxView::Kind(LabelKind::Trash));
        let mut msgs: Vec<&Message> = self.by_conv.get(conversation).map(|m| m.iter().filter(|m| in_trash(m) == trash_view).collect()).unwrap_or_default();
        msgs.sort_by_key(|m| m.date);
        msgs.into_iter().map(|m| m.id.clone()).collect()
    }

    /// Every message filed in the mailbox of `kind` (Trash, Junk…), for "Empty Folder". Only
    /// messages carrying that label count, not the rest of their conversations.
    pub fn messages_labelled(&self, kind: LabelKind) -> Vec<MessageId> {
        let Some(label) = self.label_of_kind(kind) else { return Vec::new() };
        self.by_conv.values().flatten().filter(|m| m.has_label(&label)).map(|m| m.id.clone()).collect()
    }

    /// The unread messages shown in a view, for "Mark All as Read". Like the list, a
    /// conversation's trashed messages count only in Trash.
    pub fn unread_in(&self, owner_email: &str, view: &MailboxView) -> Vec<MessageId> {
        let trash = self.label_of_kind(LabelKind::Trash);
        let in_trash = |m: &Message| trash.as_ref().is_some_and(|t| m.has_label(t));
        let trash_view = matches!(view, MailboxView::Kind(LabelKind::Trash));
        self.conversations(owner_email, view, ListFilter::Unread, None)
            .iter()
            .filter_map(|c| self.by_conv.get(&c.id))
            .flatten()
            .filter(|m| !m.is_read && in_trash(m) == trash_view)
            .map(|m| m.id.clone())
            .collect()
    }

    /// Unread conversation count for Inbox and labels, conversation count otherwise, for
    /// sidebar badges.
    pub fn view_count(&self, _owner_email: &str, view: &MailboxView, focus: Option<FocusTab>) -> usize {
        let unread_only = matches!(view, MailboxView::Kind(LabelKind::Inbox) | MailboxView::Label(_));
        self.visible_conversations(view, focus).filter(|visible| !unread_only || visible.iter().any(|m| !m.is_read)).count()
    }

    /// Summaries of the conversations containing any of `hits` (search results), newest
    /// first. Trashed messages stay out unless they matched.
    pub fn conversations_matching(&self, hits: &BTreeSet<MessageId>, owner_email: &str, filter: ListFilter) -> Vec<ConversationSummary> {
        let mut convs: BTreeSet<&ConversationId> = BTreeSet::new();
        for id in hits {
            if let Some(c) = self.conv_of.get(id) {
                convs.insert(c);
            }
        }
        let mut out: Vec<ConversationSummary> = convs
            .into_iter()
            .filter_map(|c| ConversationSummary::from_messages(self.by_conv.get(c)?, owner_email, &self.pending))
            .filter(|c| match filter {
                ListFilter::All => true,
                ListFilter::Unread => c.is_unread(),
                ListFilter::Attachments => c.has_attachments,
            })
            .collect();
        out.sort_by_key(|c| std::cmp::Reverse(c.latest));
        out
    }

    /// Pins messages to Focused or Other (`None` clears it), as `Store::set_focus_override` writes.
    pub fn set_focus_override(&mut self, ids: &[MessageId], focused: Option<bool>) {
        for id in ids {
            let Some(conv) = self.conv_of.get(id) else { continue };
            if let Some(m) = self.by_conv.get_mut(conv).and_then(|msgs| msgs.iter_mut().find(|m| &m.id == id)) {
                m.focus_override = focused;
            }
        }
    }

    /// The standing Focused/Other rule for a sender, as `Store::set_focus_rule` writes it.
    pub fn set_focus_rule(&mut self, email: &str, focused: Option<bool>) {
        let email = email.to_ascii_lowercase();
        match focused {
            Some(f) => {
                self.rules.insert(email, f);
            }
            None => {
                self.rules.remove(&email);
            }
        }
    }

    /// Applies a local change the way [`crate::Store::apply_change`] writes it, and marks the
    /// messages as awaiting sync.
    pub fn apply(&mut self, change: &LocalChange) {
        let inbox = self.label_of_kind(LabelKind::Inbox);
        let trash = self.label_of_kind(LabelKind::Trash);
        let ids = change.message_ids().to_vec();
        if let LocalChange::DeleteForever { .. } = change {
            for id in &ids {
                let Some(conv) = self.conv_of.remove(id) else { continue };
                if let Some(msgs) = self.by_conv.get_mut(&conv) {
                    msgs.retain(|m| &m.id != id);
                    if msgs.is_empty() {
                        self.by_conv.remove(&conv);
                    }
                }
            }
            return;
        }
        for id in &ids {
            let Some(conv) = self.conv_of.get(id) else { continue };
            let Some(m) = self.by_conv.get_mut(conv).and_then(|msgs| msgs.iter_mut().find(|m| &m.id == id)) else { continue };
            apply_to_message(m, change, inbox.as_ref(), trash.as_ref());
        }
        self.pending.extend(ids);
    }
}

/// Applies a local change to one message in memory, mirroring the store's write.
pub fn apply_to_message(m: &mut Message, change: &LocalChange, inbox: Option<&LabelId>, trash: Option<&LabelId>) {
    match change {
        LocalChange::SetRead { read, .. } => m.is_read = *read,
        LocalChange::SetStarred { starred, .. } => m.is_starred = *starred,
        LocalChange::AddLabel { label_id, .. } => {
            m.label_ids.insert(label_id.clone());
        }
        LocalChange::RemoveLabel { label_id, .. } => {
            m.label_ids.remove(label_id);
        }
        LocalChange::Archive { .. } => {
            if let Some(i) = inbox {
                m.label_ids.remove(i);
            }
        }
        LocalChange::Trash { .. } => {
            if let (Some(i), Some(t)) = (inbox, trash) {
                m.label_ids.remove(i);
                m.label_ids.insert(t.clone());
            }
        }
        LocalChange::Untrash { .. } => {
            if let (Some(i), Some(t)) = (inbox, trash) {
                m.label_ids.remove(t);
                m.label_ids.insert(i.clone());
            }
        }
        // Removed from the index by `MailIndex::apply`; nothing to change on the message.
        LocalChange::DeleteForever { .. } => {}
    }
}
