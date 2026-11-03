//! The mail sidebar's account tree and Favorites: which accounts are shown and in what
//! order, which folders are pinned, and how pinned folders with the same name are told apart.

use crate::{AccountId, LabelId, LabelKind};
use serde::{Deserialize, Serialize};

/// Whether the sidebar lists every account's folders or only the active account's.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum SidebarMode {
    #[default]
    AllAccounts,
    OneAccount,
}

/// A folder pinned to Favorites. `label` is set for user labels (`kind == User`); system
/// mailboxes and computed views (Inbox, Starred, Drafts, …) are named by `kind` alone.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Favorite {
    pub account: AccountId,
    pub kind: LabelKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<LabelId>,
}

impl Favorite {
    pub fn kind(account: AccountId, kind: LabelKind) -> Self {
        Self { account, kind, label: None }
    }

    pub fn label(account: AccountId, label: LabelId) -> Self {
        Self { account, kind: LabelKind::User, label: Some(label) }
    }
}

/// Where a branch of the folder tree hangs: the account's top level, or under the Inbox.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TreeRoot {
    Top,
    Inbox,
}

/// One row of the folder tree, in display order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeRow<'a> {
    /// Position in the names given to [`folder_tree`].
    pub index: usize,
    /// 0 for a folder directly under the root.
    pub depth: usize,
    /// The name below its parent: "Clients" for "Work / Clients" under "Work".
    pub leaf: &'a str,
    pub has_children: bool,
}

/// The rows under `root`, parents before their children, as Outlook nests them. A folder
/// sits under the nearest ancestor that exists ("Work / A / B" under "Work" when there is no
/// "Work / A"); "Inbox / …" names with no such ancestor sit under the Inbox. Names nest by
/// " / " (IMAP) or "/" (Gmail). Siblings keep the order given.
pub fn folder_tree<'a>(names: &[&'a str], root: TreeRoot) -> Vec<TreeRow<'a>> {
    // Each folder's parent: another folder, or the root it hangs under, and its leaf name.
    let placed: Vec<(Option<usize>, TreeRoot, &str)> = names
        .iter()
        .map(|name| {
            let mut at = *name;
            while let Some(parent) = folder_parent(at) {
                let leaf = name[parent.len()..].trim_start_matches(" / ").trim_start_matches('/');
                if let Some(i) = names.iter().position(|n| *n == parent) {
                    return (Some(i), TreeRoot::Top, leaf);
                }
                if parent == "Inbox" {
                    return (None, TreeRoot::Inbox, leaf);
                }
                at = parent;
            }
            (None, TreeRoot::Top, *name)
        })
        .collect();
    let children = |of: Option<usize>| -> Vec<usize> { (0..names.len()).filter(|&i| placed[i].0 == of && (of.is_some() || placed[i].1 == root)).collect() };
    let mut rows = Vec::new();
    let mut stack: Vec<(usize, usize)> = children(None).into_iter().rev().map(|i| (i, 0)).collect();
    while let Some((i, depth)) = stack.pop() {
        let kids = children(Some(i));
        rows.push(TreeRow { index: i, depth, leaf: placed[i].2, has_children: !kids.is_empty() });
        stack.extend(kids.into_iter().rev().map(|k| (k, depth + 1)));
    }
    rows
}

/// The folder a folder sits in, by name: "Work" for IMAP "Work / Clients" or Gmail
/// "Work/Clients"; `None` at the top level.
pub fn folder_parent(name: &str) -> Option<&str> {
    name.rsplit_once(" / ").or_else(|| name.rsplit_once('/')).map(|(parent, _)| parent)
}

/// Whether `name` is `ancestor` or somewhere inside it.
pub fn folder_within(name: &str, ancestor: &str) -> bool {
    name == ancestor || name.strip_prefix(ancestor).is_some_and(|rest| rest.starts_with(" / ") || rest.starts_with('/'))
}

/// Whether moving folder `moving` into `target` (`None`: the top level) changes anything
/// and keeps the folder out of itself.
pub fn can_move_folder(moving: &str, target: Option<&str>) -> bool {
    match target {
        Some(t) => !folder_within(t, moving) && folder_parent(moving) != Some(t),
        None => folder_parent(moving).is_some(),
    }
}

/// Favorites before the user has pinned or unpinned anything: each account's Inbox.
pub fn default_favorites<'a>(accounts: impl IntoIterator<Item = &'a AccountId>) -> Vec<Favorite> {
    accounts.into_iter().map(|a| Favorite::kind(a.clone(), LabelKind::Inbox)).collect()
}

/// Adds `favorite` at the end of `favorites` unless it is already pinned; true if it was added.
pub fn pin(favorites: &mut Vec<Favorite>, favorite: Favorite) -> bool {
    if favorites.contains(&favorite) {
        return false;
    }
    favorites.push(favorite);
    true
}

/// Removes `favorite` from `favorites`; true if it was pinned.
pub fn unpin(favorites: &mut Vec<Favorite>, favorite: &Favorite) -> bool {
    let before = favorites.len();
    favorites.retain(|f| f != favorite);
    favorites.len() != before
}

/// `accounts` in the saved order. Accounts the saved order doesn't know (added since) follow
/// in their given order; saved ids for accounts that no longer exist are dropped.
pub fn ordered_accounts(accounts: &[AccountId], saved: &[AccountId]) -> Vec<AccountId> {
    let mut out: Vec<AccountId> = saved.iter().filter(|id| accounts.contains(id)).cloned().collect();
    out.dedup();
    for id in accounts {
        if !out.contains(id) {
            out.push(id.clone());
        }
    }
    out
}

/// Moves `moving` to where `target` is, shifting the others along: dropping on an item
/// lower down puts it after that item, higher up puts it before. No-op if either is missing.
pub fn move_item<T: PartialEq>(order: &mut Vec<T>, moving: &T, target: &T) {
    let (Some(from), Some(to)) = (order.iter().position(|a| a == moving), order.iter().position(|a| a == target)) else { return };
    let item = order.remove(from);
    order.insert(to, item);
}

/// [`move_item`] for the sidebar's account order.
pub fn move_account(order: &mut Vec<AccountId>, moving: &AccountId, target: &AccountId) {
    move_item(order, moving, target);
}

/// Moves `item` one place up (`up`) or down; false at the end of the list or if missing.
pub fn shift_item<T: PartialEq>(order: &mut [T], item: &T, up: bool) -> bool {
    let Some(at) = order.iter().position(|a| a == item) else { return false };
    let to = if up { at.checked_sub(1) } else { Some(at + 1).filter(|t| *t < order.len()) };
    let Some(to) = to else { return false };
    order.swap(at, to);
    true
}

/// Display names for Favorites rows. `rows` is `(folder name, account name)`; a folder name
/// that appears more than once (ignoring case) gets " · account name" so the rows differ.
pub fn favorite_names(rows: &[(String, String)]) -> Vec<String> {
    rows.iter()
        .map(|(folder, account)| {
            let shared = rows.iter().filter(|(other, _)| other.eq_ignore_ascii_case(folder)).count() > 1;
            if shared { format!("{folder} · {account}") } else { folder.clone() }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folders_move_anywhere_but_into_themselves_or_where_they_are() {
        assert_eq!(folder_parent("Work / Clients"), Some("Work"));
        assert_eq!(folder_parent("Inbox/Receipts"), Some("Inbox"));
        assert_eq!(folder_parent("Work"), None);
        assert!(can_move_folder("Work", Some("Inbox")));
        assert!(can_move_folder("Inbox / Work", None));
        assert!(!can_move_folder("Work", None), "already at the top level");
        assert!(!can_move_folder("Work / Clients", Some("Work")), "already there");
        assert!(!can_move_folder("Work", Some("Work")));
        assert!(!can_move_folder("Work", Some("Work / Clients")));
        assert!(!can_move_folder("Work", Some("Work/Clients/2026")));
        assert!(can_move_folder("Work", Some("Workshop")));
    }

    fn ids(names: &[&str]) -> Vec<AccountId> {
        names.iter().map(|n| AccountId((*n).into())).collect()
    }

    #[test]
    fn folders_nest_under_their_parents_and_the_inbox() {
        fn rows<'a>(names: &[&'a str], root: TreeRoot) -> Vec<(&'a str, usize, &'a str, bool)> {
            folder_tree(names, root).into_iter().map(|r| (names[r.index], r.depth, r.leaf, r.has_children)).collect()
        }
        let names = ["inbox2", "inbox2 / sub3", "Other", "Inbox / inbox-sub2", "inbox2 / sub3 / deep", "Work/A/B"];
        assert_eq!(
            rows(&names, TreeRoot::Top),
            [("inbox2", 0, "inbox2", true), ("inbox2 / sub3", 1, "sub3", true), ("inbox2 / sub3 / deep", 2, "deep", false), ("Other", 0, "Other", false), ("Work/A/B", 0, "Work/A/B", false)]
        );
        assert_eq!(rows(&names, TreeRoot::Inbox), [("Inbox / inbox-sub2", 0, "inbox-sub2", false)]);
        // Gmail names, a missing middle level, and "Inbox/…" with its own children.
        let gmail = ["Work", "Work/A/B", "Inbox/Receipts", "Inbox/Receipts/2026"];
        assert_eq!(rows(&gmail, TreeRoot::Top), [("Work", 0, "Work", true), ("Work/A/B", 1, "A/B", false)]);
        assert_eq!(rows(&gmail, TreeRoot::Inbox), [("Inbox/Receipts", 0, "Receipts", true), ("Inbox/Receipts/2026", 1, "2026", false)]);
    }

    #[test]
    fn saved_order_wins_and_new_accounts_follow() {
        let accounts = ids(&["a", "b", "c"]);
        assert_eq!(ordered_accounts(&accounts, &ids(&["c", "a"])), ids(&["c", "a", "b"]));
    }

    #[test]
    fn removed_accounts_drop_out_of_the_order() {
        assert_eq!(ordered_accounts(&ids(&["a", "b"]), &ids(&["gone", "b", "a"])), ids(&["b", "a"]));
    }

    #[test]
    fn moving_down_lands_after_the_target_and_up_lands_before() {
        let mut order = ids(&["a", "b", "c"]);
        move_account(&mut order, &AccountId("a".into()), &AccountId("c".into()));
        assert_eq!(order, ids(&["b", "c", "a"]));
        move_account(&mut order, &AccountId("a".into()), &AccountId("b".into()));
        assert_eq!(order, ids(&["a", "b", "c"]));
    }

    #[test]
    fn moving_an_unknown_account_changes_nothing() {
        let mut order = ids(&["a", "b"]);
        move_account(&mut order, &AccountId("x".into()), &AccountId("a".into()));
        assert_eq!(order, ids(&["a", "b"]));
    }

    #[test]
    fn only_shared_folder_names_get_the_account() {
        let rows = vec![("Inbox".into(), "Work".into()), ("Inbox".into(), "Personal".into()), ("Receipts".into(), "Work".into())];
        assert_eq!(favorite_names(&rows), vec!["Inbox · Work", "Inbox · Personal", "Receipts"]);
    }

    #[test]
    fn names_differing_only_in_case_count_as_shared() {
        let rows = vec![("Archive".into(), "Work".into()), ("archive".into(), "Home".into())];
        assert_eq!(favorite_names(&rows), vec!["Archive · Work", "archive · Home"]);
    }

    #[test]
    fn every_account_starts_with_its_inbox() {
        let accounts = ids(&["a", "b"]);
        assert_eq!(default_favorites(&accounts), vec![Favorite::kind(AccountId("a".into()), LabelKind::Inbox), Favorite::kind(AccountId("b".into()), LabelKind::Inbox)]);
    }

    #[test]
    fn pinning_appends_once() {
        let a = AccountId("a".into());
        let mut favs = default_favorites([&a]);
        assert!(pin(&mut favs, Favorite::kind(a.clone(), LabelKind::Sent)));
        assert!(!pin(&mut favs, Favorite::kind(a.clone(), LabelKind::Sent)));
        assert_eq!(favs, vec![Favorite::kind(a.clone(), LabelKind::Inbox), Favorite::kind(a, LabelKind::Sent)]);
    }

    #[test]
    fn unpinning_removes_only_that_folder() {
        let (a, b) = (AccountId("a".into()), AccountId("b".into()));
        let mut favs = default_favorites([&a, &b]);
        assert!(unpin(&mut favs, &Favorite::kind(a.clone(), LabelKind::Inbox)));
        assert!(!unpin(&mut favs, &Favorite::kind(a, LabelKind::Inbox)));
        assert_eq!(favs, vec![Favorite::kind(b, LabelKind::Inbox)]);
    }

    #[test]
    fn labels_with_the_same_account_are_told_apart() {
        let a = AccountId("a".into());
        let mut favs = vec![Favorite::label(a.clone(), LabelId::from("a:Work"))];
        assert!(pin(&mut favs, Favorite::label(a.clone(), LabelId::from("a:Home"))));
        assert!(unpin(&mut favs, &Favorite::label(a.clone(), LabelId::from("a:Work"))));
        assert_eq!(favs, vec![Favorite::label(a, LabelId::from("a:Home"))]);
    }

    #[test]
    fn favorites_move_by_drop_and_by_one_place() {
        let a = AccountId("a".into());
        let (inbox, sent, trash) = (Favorite::kind(a.clone(), LabelKind::Inbox), Favorite::kind(a.clone(), LabelKind::Sent), Favorite::kind(a.clone(), LabelKind::Trash));
        let mut favs = vec![inbox.clone(), sent.clone(), trash.clone()];
        move_item(&mut favs, &inbox, &trash);
        assert_eq!(favs, vec![sent.clone(), trash.clone(), inbox.clone()]);
        assert!(shift_item(&mut favs, &inbox, true));
        assert_eq!(favs, vec![sent.clone(), inbox.clone(), trash.clone()]);
        assert!(!shift_item(&mut favs, &sent, true), "already first");
        assert!(!shift_item(&mut favs, &trash, false), "already last");
        assert!(!shift_item(&mut favs, &Favorite::kind(a, LabelKind::Spam), false), "not pinned");
        assert_eq!(favs, vec![sent, inbox, trash]);
    }

    #[test]
    fn favorites_round_trip_through_json() {
        let favs = vec![Favorite::kind(AccountId("a".into()), LabelKind::Sent), Favorite::label(AccountId("a".into()), LabelId::from("a:Work"))];
        let json = serde_json::to_string(&favs).unwrap();
        assert_eq!(serde_json::from_str::<Vec<Favorite>>(&json).unwrap(), favs);
    }
}
