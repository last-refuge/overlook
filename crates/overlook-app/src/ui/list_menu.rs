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
}

/// Summarises `rows` against the account's labels. Trash and Junk are judged by the
/// labels the summary carries, so a row needs no thread loaded.
pub(super) fn selection_state<'a>(rows: impl IntoIterator<Item = &'a ConversationSummary>, labels: &[Label]) -> SelectionState {
    let of_kind = |kind: LabelKind| labels.iter().find(|l| l.kind == kind).map(|l| &l.id);
    let (trash, inbox, spam) = (of_kind(LabelKind::Trash), of_kind(LabelKind::Inbox), of_kind(LabelKind::Spam));
    let has = |c: &ConversationSummary, id: Option<&LabelId>| id.is_some_and(|id| c.label_ids.contains(id));
    let mut state = SelectionState { all_starred: true, all_in_trash: true, all_in_junk: true, has_junk_label: spam.is_some(), ..Default::default() };
    for c in rows {
        state.count += 1;
        state.any_unread |= c.is_unread();
        state.all_starred &= c.starred;
        state.all_in_trash &= has(c, trash);
        state.any_in_inbox |= has(c, inbox);
        state.all_in_junk &= has(c, spam);
    }
    if state.count == 0 {
        state.all_starred = false;
        state.all_in_trash = false;
        state.all_in_junk = false;
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
        }
    }

    #[test]
    fn a_single_inbox_row() {
        let s = selection_state([&row("c1", 1, false, &["inbox"])], &labels(true));
        assert_eq!(s, SelectionState { count: 1, any_unread: true, all_starred: false, all_in_trash: false, any_in_inbox: true, all_in_junk: false, has_junk_label: true });
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
}
