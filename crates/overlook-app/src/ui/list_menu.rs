//! What the selected rows have in common, for wording the row menu and the toolbar:
//! "Mark as Read" when any is unread, "Unstar" only when all are starred, and so on.

use overlook_core::*;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct SelectionState {
    pub count: usize,
    pub any_unread: bool,
    pub all_starred: bool,
    pub all_in_trash: bool,
    pub any_in_inbox: bool,
    pub all_in_junk: bool,
    pub has_junk_label: bool,
    pub any_snoozed: bool,
    pub all_snoozed: bool,
}

/// Summarises `rows` against a single account's labels. Trash and Junk are judged by the
/// labels the summary carries, so a row needs no thread loaded.
pub(super) fn selection_state<'a>(rows: impl IntoIterator<Item = &'a ConversationSummary>, labels: &[Label]) -> SelectionState {
    selection_state_for(rows, |_| labels)
}

fn label_of_kind(labels: &[Label], kind: LabelKind) -> Option<&LabelId> {
    labels.iter().find(|l| l.kind == kind).map(|l| &l.id)
}

/// Summarises `rows` against each row's owning account's labels, so multi-account
/// selections in All Inboxes check each account's own Inbox/Trash/Junk label IDs.
pub(super) fn selection_state_for<'a, 'b>(
    rows: impl IntoIterator<Item = &'a ConversationSummary>,
    labels_of: impl Fn(&AccountId) -> &'b [Label],
) -> SelectionState {
    let has = |c: &ConversationSummary, id: Option<&LabelId>| id.is_some_and(|id| c.label_ids.contains(id));
    let mut state = SelectionState {
        all_starred: true,
        all_in_trash: true,
        all_in_junk: true,
        all_snoozed: true,
        ..Default::default()
    };
    for c in rows {
        let labels = labels_of(&c.account_id);
        let trash = label_of_kind(labels, LabelKind::Trash);
        let inbox = label_of_kind(labels, LabelKind::Inbox);
        let spam = label_of_kind(labels, LabelKind::Spam);
        state.count += 1;
        state.any_unread |= c.is_unread();
        state.all_starred &= c.starred;
        state.all_in_trash &= has(c, trash);
        state.any_in_inbox |= has(c, inbox);
        state.all_in_junk &= has(c, spam);
        state.has_junk_label |= spam.is_some();
        state.any_snoozed |= c.snoozed_until.is_some();
        state.all_snoozed &= c.snoozed_until.is_some();
    }
    if state.count == 0 {
        state.all_starred = false;
        state.all_in_trash = false;
        state.all_in_junk = false;
        state.all_snoozed = false;
        state.has_junk_label = label_of_kind(labels_of(&AccountId(String::new())), LabelKind::Spam).is_some();
    }
    state
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    // The parent's GPUI glob also exports a `test` proc macro.
    use std::prelude::v1::test;

    fn label(kind: LabelKind, id: &str) -> Label {
        Label { id: LabelId::from(id), account_id: AccountId::from("a"), kind, name: id.into(), provider_id: None, color: None, sort_order: 0 }
    }

    fn labels(with_junk: bool) -> Vec<Label> {
        let mut out = vec![label(LabelKind::Inbox, "inbox"), label(LabelKind::Trash, "trash")];
        if with_junk {
            out.push(label(LabelKind::Spam, "junk"));
        }
        out
    }

    fn row(id: &str, unread: usize, starred: bool, label_ids: &[&str]) -> ConversationSummary {
        ConversationSummary {
            id: ConversationId::from(id),
            account_id: AccountId::from("a"),
            subject: String::new(),
            snippet: String::new(),
            latest: 0,
            message_count: 1,
            unread_count: unread,
            starred,
            has_attachments: false,
            attachment_names: Vec::new(),
            invitation: None,
            participants: Vec::new(),
            latest_message_id: MessageId::from(id),
            label_ids: label_ids.iter().map(|l| LabelId::from(*l)).collect::<BTreeSet<_>>(),
            pending: false,
            snoozed_until: None,
        }
    }

    #[test]
    fn a_single_inbox_row() {
        let s = selection_state([&row("c1", 1, false, &["inbox"])], &labels(true));
        assert_eq!(s, SelectionState { count: 1, any_unread: true, all_starred: false, all_in_trash: false, any_in_inbox: true, all_in_junk: false, has_junk_label: true, any_snoozed: false, all_snoozed: false });
    }

    #[test]
    fn a_single_trash_row_cannot_be_trashed_again() {
        let s = selection_state([&row("c1", 0, true, &["trash"])], &labels(false));
        assert!(s.all_in_trash && s.all_starred && !s.any_unread && !s.any_in_inbox, "Delete gives way to Restore");
        assert!(!s.has_junk_label && !s.all_in_junk, "no Junk folder: nothing counts as junk");
    }

    #[test]
    fn a_mixed_set_takes_any_for_read_and_inbox_and_all_for_star_and_trash() {
        let rows = [row("c1", 0, true, &["trash"]), row("c2", 2, false, &["inbox"])];
        let s = selection_state(&rows, &labels(true));
        assert_eq!(s.count, 2);
        assert!(s.any_unread, "one unread row is enough to offer Mark as Read");
        assert!(!s.all_starred, "Unstar only when every row is starred");
        assert!(!s.all_in_trash, "Delete stays available while one row is outside Trash");
        assert!(s.any_in_inbox);
    }

    #[test]
    fn junk_rows_offer_not_junk_only_when_all_are_junk() {
        let junk = labels(true);
        assert!(selection_state([&row("c1", 0, false, &["junk"])], &junk).all_in_junk);
        assert!(!selection_state([&row("c1", 0, false, &["junk"]), &row("c2", 0, false, &["inbox"])], &junk).all_in_junk);
    }

    #[test]
    fn an_empty_selection_claims_nothing() {
        let s = selection_state(std::iter::empty(), &labels(true));
        assert_eq!(s, SelectionState { has_junk_label: true, ..Default::default() });
    }

    #[test]
    fn multi_account_selection_checks_each_rows_own_account_labels() {
        let labels_a = vec![
            Label { id: LabelId::from("a:INBOX"), account_id: AccountId::from("a"), kind: LabelKind::Inbox, name: "Inbox".into(), provider_id: None, color: None, sort_order: 0 },
            Label { id: LabelId::from("a:TRASH"), account_id: AccountId::from("a"), kind: LabelKind::Trash, name: "Trash".into(), provider_id: None, color: None, sort_order: 1 },
        ];
        let labels_b = vec![
            Label { id: LabelId::from("b:INBOX"), account_id: AccountId::from("b"), kind: LabelKind::Inbox, name: "Inbox".into(), provider_id: None, color: None, sort_order: 0 },
            Label { id: LabelId::from("b:TRASH"), account_id: AccountId::from("b"), kind: LabelKind::Trash, name: "Trash".into(), provider_id: None, color: None, sort_order: 1 },
            Label { id: LabelId::from("b:SPAM"), account_id: AccountId::from("b"), kind: LabelKind::Spam, name: "Spam".into(), provider_id: None, color: None, sort_order: 2 },
        ];
        let mut r1 = row("c1", 0, true, &["a:INBOX"]);
        r1.account_id = AccountId::from("a");
        let mut r2 = row("c2", 1, true, &["b:INBOX"]);
        r2.account_id = AccountId::from("b");
        let rows = [r1, r2];
        let s = selection_state_for(&rows, |acct| if acct.0 == "a" { &labels_a } else { &labels_b });
        assert_eq!(s.count, 2);
        assert!(s.any_unread);
        assert!(s.all_starred);
        assert!(s.any_in_inbox);
        assert!(!s.all_in_trash);
        assert!(s.has_junk_label);
    }
}
