//! Account and folder navigation.

use super::widgets::*;
use super::{MailWindow, Nav};
use std::collections::HashMap;
use crate::assets::icon;
use crate::theme::ActivePalette;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::menu::{ContextMenuExt, DropdownMenu, PopupMenuItem};
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::{Sizable, StyledExt};
use gpui_kit::prelude::*;
use gpui_kit::*;
use gpui_kit::base::{h_flex, v_flex};
use overlook_core::*;
use overlook_store::MailboxView;

/// How far each level of the folder tree is indented: a child's arrow sits under its parent's
/// icon.
const DEPTH_STEP: f32 = 16.;
/// Where a row's icon starts, past the column for the disclosure arrow. Section titles start
/// here too, so arrows, icons and titles each keep one left edge.
const ICON_INSET: f32 = 18.;

/// Name and icon of a mailbox that is not a user label; `None` for user labels.
pub(super) fn system_folder(kind: LabelKind) -> Option<(&'static str, &'static str)> {
    Some(match kind {
        LabelKind::Inbox => ("Inbox", "inbox"),
        LabelKind::Starred => ("Starred", "star"),
        LabelKind::Drafts => ("Drafts", "square-pen"),
        LabelKind::Outbox => ("Outbox", "clock"),
        LabelKind::Sent => ("Sent", "send"),
        LabelKind::Archive => ("Archive", "archive"),
        LabelKind::Spam => ("Junk", "shield-alert"),
        LabelKind::Trash => ("Trash", "trash"),
        LabelKind::User | LabelKind::Container => return None,
    })
}

/// The Favorites entry for a folder row.
fn favorite_for(account: &AccountId, view: &MailboxView) -> Favorite {
    match view {
        MailboxView::Kind(kind) => Favorite::kind(account.clone(), *kind),
        MailboxView::Label(id) => Favorite::label(account.clone(), id.clone()),
    }
}

/// Sidebar spacing for the Comfortable/Compact setting.
#[derive(Clone, Copy)]
struct Metrics {
    row_height: f32,
    row_gap: f32,
    header_top: f32,
    header_bottom: f32,
    header_padding: f32,
    /// Space on each side of the line under Favorites.
    divider_gap: f32,
    footer_top: f32,
    bottom: f32,
}

impl Metrics {
    fn for_density(density: Density) -> Self {
        match density {
            Density::Comfortable => Self { row_height: 32., row_gap: 2., header_top: 14., header_bottom: 4., header_padding: 4., divider_gap: 12., footer_top: 16., bottom: 14. },
            Density::Compact => Self { row_height: 26., row_gap: 1., header_top: 8., header_bottom: 2., header_padding: 2., divider_gap: 8., footer_top: 10., bottom: 8. },
        }
    }
}

impl MailWindow {
    /// What the sidebar calls an account: its account name, or else its address.
    pub(crate) fn account_name(&self, id: &AccountId) -> String {
        if let Some(name) = self.account_prefs.get(id).map(|p| p.name.trim()).filter(|n| !n.is_empty()) {
            return name.to_owned();
        }
        self.accounts.iter().find(|a| &a.id == id).map(|a| a.email.clone()).unwrap_or_else(|| id.0.clone())
    }

    /// Accounts in the order the sidebar lists them.
    pub(crate) fn sidebar_accounts(&self) -> Vec<Account> {
        let ids: Vec<AccountId> = self.accounts.iter().map(|a| a.id.clone()).collect();
        overlook_core::sidebar::ordered_accounts(&ids, &self.prefs.account_order)
            .into_iter()
            .filter_map(|id| self.accounts.iter().find(|a| a.id == id).cloned())
            .collect()
    }

    /// Pinned folders, or every account's Inbox until the user has changed them. The default
    /// follows the order accounts were added, not the sidebar's account order: rearranging
    /// accounts must not reorder Favorites.
    pub(crate) fn favorites(&self) -> Vec<Favorite> {
        match &self.prefs.favorites {
            Some(saved) => saved.clone(),
            None => overlook_core::sidebar::default_favorites(self.accounts.iter().map(|a| &a.id)),
        }
    }

    /// The folders of any account: the active one's from the index, the others' as stored.
    pub(crate) fn labels_of(&self, account: &AccountId) -> &[Label] {
        if account == &self.account.id {
            &self.labels
        } else {
            self.account_labels.get(account).map(Vec::as_slice).unwrap_or(&[])
        }
    }

    /// Re-reads the other accounts' folders for the account tree.
    pub(crate) fn reload_account_labels(&mut self) {
        let active = self.account.id.clone();
        let labels: HashMap<AccountId, Vec<Label>> = self
            .accounts
            .iter()
            .filter(|a| a.id != active)
            .map(|a| (a.id.clone(), self.store.labels(&a.id).unwrap_or_default()))
            .collect();
        self.account_labels = labels;
    }

    /// Asks for the badges of `accounts` to be recounted at the next `reload`: after a sync
    /// of that account, a change made to it from another window, or a switch away from it.
    pub(crate) fn mark_counts_stale(&mut self, accounts: impl IntoIterator<Item = AccountId>) {
        self.account_counts_stale.extend(accounts);
    }

    /// Every account's badges are recounted at the next `reload` (the Focused Inbox switch
    /// changes what the Inbox badge counts).
    pub(crate) fn mark_all_counts_stale(&mut self) {
        let ids: Vec<AccountId> = self.accounts.iter().map(|a| a.id.clone()).collect();
        self.mark_counts_stale(ids);
    }

    /// Recounts the badges of the accounts marked stale, off the UI thread. Loading an
    /// account's index reads every message header it has, so only accounts whose mail may
    /// have changed are recounted. Requests that arrive while one runs wait for the next.
    pub(crate) fn refresh_account_counts(&mut self, cx: &mut Context<Self>) {
        if self.account_counts_running {
            return;
        }
        let stale = std::mem::take(&mut self.account_counts_stale);
        let others: Vec<(AccountId, String)> = self.accounts.iter().filter(|a| a.id != self.account.id && stale.contains(&a.id)).map(|a| (a.id.clone(), a.email.clone())).collect();
        if others.is_empty() {
            return;
        }
        tracing::debug!("sidebar: recounting {} other account(s)", others.len());
        self.account_counts_running = true;
        let store = self.store.clone();
        let focused = self.focused_enabled;
        cx.spawn(async move |this, cx| {
            let counts = cx
                .background_spawn(async move {
                    others
                        .into_iter()
                        .map(|(id, email)| {
                            let mut counts = store.mail_index(&id).map(|index| super::sidebar_counts(&index, &email, focused)).unwrap_or_default();
                            counts.insert(MailboxView::Kind(LabelKind::Drafts), store.drafts(&id, false).map(|d| d.len()).unwrap_or(0));
                            counts.insert(MailboxView::Kind(LabelKind::Outbox), store.drafts(&id, true).map(|d| d.len()).unwrap_or(0));
                            (id, counts)
                        })
                        .collect::<HashMap<_, _>>()
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.account_counts_running = false;
                this.account_counts.extend(counts);
                if !this.account_counts_stale.is_empty() {
                    this.refresh_account_counts(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Opens a folder of any account, switching to that account first.
    pub(crate) fn open_folder(&mut self, account: AccountId, view: MailboxView, window: &mut Window, cx: &mut Context<Self>) {
        if account != self.account.id {
            self.switch_account(account.clone(), window, cx);
            // Switching lands on the Inbox; stop if the account is gone or that is the target.
            if self.account.id != account || view == MailboxView::Kind(LabelKind::Inbox) {
                return;
            }
        }
        self.select_view(view, window, cx);
    }

    /// Pins a folder to Favorites. The first change turns the default (every Inbox) into a
    /// saved list, so the defaults stay pinned. A folder-only container is not pinned:
    /// Favorites shows only folders that hold mail.
    pub(crate) fn pin_favorite(&mut self, favorite: Favorite, cx: &mut Context<Self>) {
        if let Some(id) = &favorite.label {
            if self.labels_of(&favorite.account).iter().any(|l| &l.id == id && l.kind == LabelKind::Container) {
                return;
            }
        }
        let mut favorites = self.favorites();
        if overlook_core::sidebar::pin(&mut favorites, favorite) {
            self.prefs.favorites = Some(favorites);
            self.save_prefs(cx);
        }
    }

    pub(crate) fn unpin_favorite(&mut self, favorite: &Favorite, cx: &mut Context<Self>) {
        let mut favorites = self.favorites();
        if overlook_core::sidebar::unpin(&mut favorites, favorite) {
            self.prefs.favorites = Some(favorites);
            self.save_prefs(cx);
        }
    }

    /// Puts the pinned `moving` where `target` is in Favorites (drag and drop).
    pub(crate) fn move_favorite_to(&mut self, moving: &Favorite, target: &Favorite, cx: &mut Context<Self>) {
        if moving == target {
            return;
        }
        let mut favorites = self.favorites();
        overlook_core::sidebar::move_item(&mut favorites, moving, target);
        self.prefs.favorites = Some(favorites);
        self.save_prefs(cx);
    }

    /// Moves a pinned folder one place up or down in Favorites.
    pub(crate) fn shift_favorite(&mut self, favorite: &Favorite, up: bool, cx: &mut Context<Self>) {
        let mut favorites = self.favorites();
        if overlook_core::sidebar::shift_item(&mut favorites, favorite, up) {
            self.prefs.favorites = Some(favorites);
            self.save_prefs(cx);
        }
    }

    /// Marks every unread message of a folder as read, opening the folder first when it
    /// belongs to another account: only the active account's mail is in memory.
    pub(crate) fn mark_folder_read(&mut self, account: AccountId, view: MailboxView, window: &mut Window, cx: &mut Context<Self>) {
        if account != self.account.id {
            self.open_folder(account.clone(), view.clone(), window, cx);
            if account != self.account.id {
                return;
            }
        }
        let unread = self.mail_index.unread_in(&self.account.email, &view);
        if !unread.is_empty() {
            self.apply_local(overlook_store::LocalChange::SetRead { message_ids: unread, read: true }, window, cx);
        }
    }

    /// Whether "Empty Folder" is offered: Trash and Junk of IMAP accounts. Gmail would need
    /// the full mail.google.com scope to delete for good, and empties both after 30 days.
    // TODO(gmail-full-scope): request https://mail.google.com/ (restricted scope: Google
    // security assessment, every Gmail user signs in again), implement `DeleteForever` in the
    // Gmail provider with `messages/batchDelete`, then offer Empty Folder for Gmail too.
    fn can_empty(&self, account: &AccountId, view: &MailboxView) -> bool {
        matches!(view, MailboxView::Kind(LabelKind::Trash | LabelKind::Spam)) && self.is_imap(account)
    }

    fn is_imap(&self, account: &AccountId) -> bool {
        self.accounts.iter().any(|a| &a.id == account && a.provider == ProviderKind::Imap)
    }

    /// Permanently deletes everything in Trash or Junk, after asking. The server empties
    /// the whole folder; the local rows are only the part of it that was downloaded.
    pub(crate) fn empty_folder(&mut self, account: AccountId, kind: LabelKind, window: &mut Window, cx: &mut Context<Self>) {
        if account != self.account.id {
            self.open_folder(account.clone(), MailboxView::Kind(kind), window, cx);
            if account != self.account.id {
                return;
            }
        }
        let ids = self.mail_index.messages_labelled(kind);
        let name = system_folder(kind).map(|(name, _)| name).unwrap_or("this folder");
        // Folders deleted into Trash go with it.
        let has_folders = kind == LabelKind::Trash
            && self.labels_of(&account).iter().find(|l| l.kind == LabelKind::Trash).is_some_and(|t| {
                self.labels_of(&account).iter().any(|l| l.kind == LabelKind::User && overlook_core::sidebar::folder_within(&l.name, &t.name))
            });
        if ids.is_empty() && !has_folders {
            crate::ui::toast(window, cx, crate::notify::Level::Info, format!("{name} is already empty."));
            return;
        }
        let detail = if has_folders {
            "Every message and folder in it is removed from the server, including any not downloaded yet. This can’t be undone."
        } else {
            "Every message in it is removed from the server, including any not downloaded yet. This can’t be undone."
        };
        let answer = window.prompt(
            PromptLevel::Warning,
            &format!("Permanently delete everything in {name}?"),
            Some(detail),
            &["Delete", "Cancel"],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            if answer.await != Ok(0) {
                return;
            }
            let _ = this.update_in(cx, |this, window, cx| {
                // The prompt was open a while: delete only what is still there, in that account.
                if this.account.id != account {
                    return;
                }
                let still: std::collections::HashSet<MessageId> = this.mail_index.messages_labelled(kind).into_iter().collect();
                let ids: Vec<MessageId> = ids.into_iter().filter(|id| still.contains(id)).collect();
                // Always queued: the server empties the whole folder, downloaded or not.
                this.apply_local(overlook_store::LocalChange::DeleteForever { message_ids: ids, folder: Some(kind) }, window, cx);
                this.record(account.clone(), format!("Emptied {name}"), "Every message in it, for good".into(), overlook_store::Reversal::None, None, window, cx);
                if kind == LabelKind::Trash {
                    this.delete_folders_in_trash(&account, window, cx);
                }
                if this.selected.as_ref().is_some_and(|s| !this.conversations.iter().any(|c| &c.id == s)) {
                    this.selected = this.conversations.first().map(|c| c.id.clone());
                    this.expanded.clear();
                    this.load_thread(cx);
                }
            });
        })
        .detach();
    }

    /// Puts `moving` where `target` is in the sidebar's account order and remembers it.
    pub(crate) fn move_account_to(&mut self, moving: &AccountId, target: &AccountId, cx: &mut Context<Self>) {
        if moving == target {
            return;
        }
        let mut order: Vec<AccountId> = self.sidebar_accounts().into_iter().map(|a| a.id).collect();
        overlook_core::sidebar::move_account(&mut order, moving, target);
        self.prefs.account_order = order;
        self.save_prefs(cx);
    }

    pub(crate) fn set_sidebar_mode(&mut self, mode: SidebarMode, cx: &mut Context<Self>) {
        if self.prefs.sidebar_mode != mode {
            self.prefs.sidebar_mode = mode;
            self.save_prefs(cx);
        }
    }

    /// One account's rows: its fixed mailboxes, with what is inside the Inbox and Trash, and
    /// then its own folders.
    fn account_folders(&self, account: &AccountId, p: &crate::theme::Palette) -> (Vec<Nav>, Vec<Nav>) {
        let active = account == &self.account.id;
        let show_outbox = if active {
            self.status.outbox_items > 0 || matches!(self.view, MailboxView::Kind(LabelKind::Outbox))
        } else {
            self.account_counts.get(account).and_then(|c| c.get(&MailboxView::Kind(LabelKind::Outbox))).is_some_and(|n| *n > 0)
        };
        use overlook_core::sidebar::{folder_tree, TreeRoot};
        let users: Vec<&overlook_core::Label> = self.labels_of(account).iter().filter(|l| matches!(l.kind, LabelKind::User | LabelKind::Container)).collect();
        let names: Vec<&str> = users.iter().map(|l| l.name.as_str()).collect();
        let trash_name = self.labels_of(account).iter().find(|l| l.kind == LabelKind::Trash).map(|l| l.name.clone());
        // One branch of the folder tree as rows, `base` levels in, without what is folded.
        let branch = |root: TreeRoot, base: usize| -> Vec<Nav> {
            let mut rows = Vec::new();
            let mut folded_at: Option<usize> = None;
            for row in folder_tree(&names, root, trash_name.as_deref()) {
                if folded_at.is_some_and(|d| row.depth > d) {
                    continue;
                }
                folded_at = None;
                let l = users[row.index];
                let container = l.kind == LabelKind::Container;
                let mut nav = Nav {
                    label: row.leaf.to_owned().into(),
                    icon: "folder",
                    view: MailboxView::Label(l.id.clone()),
                    color: if container { None } else { p.label_color(l.color.as_deref()) },
                    depth: base + row.depth,
                    expander: None,
                    container,
                };
                if row.has_children {
                    let key = format!("folder:{}:{}", account.0, l.id.0);
                    let collapsed = self.collapsed_sections.contains(&key);
                    if collapsed {
                        folded_at = Some(row.depth);
                    }
                    nav.expander = Some((key, collapsed));
                }
                rows.push(nav);
            }
            rows
        };
        let mut out = Vec::new();
        for k in [LabelKind::Inbox, LabelKind::Starred, LabelKind::Drafts, LabelKind::Outbox, LabelKind::Sent, LabelKind::Archive, LabelKind::Spam, LabelKind::Trash] {
            if k == LabelKind::Outbox && !show_outbox {
                continue;
            }
            let Some((name, icon)) = system_folder(k) else { continue };
            let mut nav = Nav::view(name, icon, MailboxView::Kind(k));
            // Folders inside the Inbox sit under it and fold away with it, as in Outlook, and
            // deleted folders likewise under Trash.
            let (inside, key) = match k {
                LabelKind::Inbox => (branch(TreeRoot::Inbox, 1), format!("inbox:{}", account.0)),
                LabelKind::Trash => (branch(TreeRoot::Trash, 1), format!("trash:{}", account.0)),
                _ => (Vec::new(), String::new()),
            };
            let collapsed = self.collapsed_sections.contains(&key);
            if !inside.is_empty() {
                nav.expander = Some((key, collapsed));
            }
            out.push(nav);
            if !collapsed {
                out.extend(inside);
            }
        }
        (out, branch(TreeRoot::Top, 0))
    }

    /// Favorites rows with their accounts; a folder name shared by several rows carries the
    /// account name. Pins of removed accounts or deleted labels are skipped.
    fn favorite_rows(&self, favorites: &[Favorite], p: &crate::theme::Palette) -> Vec<(AccountId, Nav)> {
        let mut rows: Vec<(AccountId, Nav)> = Vec::new();
        for f in favorites.iter().cloned() {
            if !self.accounts.iter().any(|a| a.id == f.account) {
                continue;
            }
            let nav = match (f.kind, &f.label) {
                (LabelKind::User, Some(id)) => match self.labels_of(&f.account).iter().find(|l| &l.id == id && l.kind == LabelKind::User) {
                    Some(l) => Nav { label: overlook_core::sidebar::folder_title(&l.name).to_owned().into(), icon: "folder", view: MailboxView::Label(id.clone()), color: p.label_color(l.color.as_deref()), depth: 0, expander: None, container: false },
                    None => continue,
                },
                (kind, _) => match system_folder(kind) {
                    Some((name, icon)) => Nav::view(name, icon, MailboxView::Kind(kind)),
                    None => continue,
                },
            };
            rows.push((f.account, nav));
        }
        let names = overlook_core::sidebar::favorite_names(&rows.iter().map(|(a, n)| (n.label.to_string(), self.account_name(a))).collect::<Vec<_>>());
        for ((_, nav), name) in rows.iter_mut().zip(names) {
            nav.label = name.into();
        }
        rows
    }

    pub(super) fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = cx.palette().navigation_palette();
        let m = Metrics::for_density(self.density);
        let status = self.status.clone();
        let outbox = status.outbox_items;
        let drafts = self.drafts.len();
        // Read once per render, not per row: every row consults the pinned list.
        let pinned_list: std::rc::Rc<Vec<Favorite>> = std::rc::Rc::new(self.favorites());
        let ordered_accounts = self.sidebar_accounts();

        // `section` and `account` keep element ids unique: a folder can be in Favorites and
        // in its account's list, and every account has an Inbox.
        let nav_button = |this: &Self, section: &str, account: &AccountId, nav: Nav, favorites: &std::rc::Rc<Vec<Favorite>>, cx: &mut Context<Self>| {
            let in_active = account == &this.account.id;
            let current = in_active && this.view == nav.view && this.search_query.is_empty();
            // The active account counts from its live index; the others from their last
            // recount. Drops go to the active account only.
            let count = if !in_active {
                this.account_counts.get(account).and_then(|c| c.get(&nav.view)).copied().unwrap_or(0)
            } else {
                match &nav.view {
                    MailboxView::Kind(LabelKind::Drafts) => drafts,
                    MailboxView::Kind(LabelKind::Outbox) => outbox,
                    v => this.counts.get(v).copied().unwrap_or(0),
                }
            };
            let view = nav.view.clone();
            let drop_view = nav.view.clone();
            let container = nav.container;
            // The same set `drop_on_view` accepts.
            let droppable = in_active && !container && matches!(nav.view, MailboxView::Kind(LabelKind::Inbox | LabelKind::Archive | LabelKind::Trash | LabelKind::Starred | LabelKind::Spam) | MailboxView::Label(_));
            let color = nav.color;
            let target = account.clone();
            let favorite = favorite_for(account, &nav.view);
            let pinned = favorites.contains(&favorite);
            // Favorites rows can be dragged within Favorites and moved from their menu.
            let in_favorites = section == "favorites";
            let place = favorites.iter().position(|f| f == &favorite);
            let (can_up, can_down) = (place.is_some_and(|i| i > 0), place.is_some_and(|i| i + 1 < favorites.len()));
            let order = favorites.clone();
            let fav_drag = super::FavoriteDrag { favorite: favorite.clone(), name: nav.label.to_string() };
            let drop_target = favorite.clone();
            let (menu_account, menu_view) = (account.clone(), nav.view.clone());
            // Drafts and the Outbox hold drafts, not received mail; a container holds folders.
            let has_mail = !container && !matches!(nav.view, MailboxView::Kind(LabelKind::Drafts | LabelKind::Outbox));
            // New, rename and delete apply to the account's own folders, where the server allows.
            let user_folder = match &nav.view {
                MailboxView::Label(id) if this.manages_folders(account) => this.user_label(account, id),
                _ => None,
            };
            // Folders can go inside your own folders, inside a container, and inside the
            // Inbox (a Gmail label named "Inbox/…", which Outlook shows there too).
            let new_inside = match &nav.view {
                MailboxView::Kind(LabelKind::Inbox) if this.manages_folders(account) => this.labels_of(account).iter().find(|l| l.kind == LabelKind::Inbox).cloned(),
                MailboxView::Label(id) if container && this.manages_folders(account) => this.labels_of(account).iter().find(|l| &l.id == id).cloned(),
                _ => user_folder.clone(),
            };
            let empty_kind = match &nav.view {
                MailboxView::Kind(kind) if this.can_empty(account, &nav.view) => Some(*kind),
                _ => None,
            };
            // Folders are dragged and dropped in the account tree; Favorites rows reorder.
            let folder_drag = user_folder.clone().filter(|_| !in_favorites).map(|label| super::FolderDrag { account: account.clone(), label });
            let folder_target = new_inside.clone().filter(|_| !in_favorites);
            let me = cx.entity().downgrade();
            // Only in the account tree: Favorites rows stand alone.
            let expander = nav.expander.clone().filter(|_| !in_favorites);
            // A container opens nothing: clicking it folds or unfolds the folders inside.
            let fold_key = expander.as_ref().filter(|_| container).map(|(key, _)| key.clone());
            div()
                .id(SharedString::from(format!("nav-{section}-{}-{:?}", account.0, nav.view)))
                .relative()
                .when(droppable, |d| {
                    d.drag_over::<super::ConversationDrag>(move |style, _, _, _| style.bg(p.selection).border_1().border_color(p.accent))
                        .on_drop(cx.listener(move |this, drag: &super::ConversationDrag, window, cx| this.drop_on_view(drag.clone(), drop_view.clone(), window, cx)))
                })
                .when_some(folder_drag, |d, drag| d.on_drag(drag, |d: &super::FolderDrag, _pos, _window, cx| cx.new(|_| super::DragPreview { label: d.label.name.clone() })))
                .when_some(folder_target, |d, target| {
                    let (over, over_account) = (target.clone(), account.clone());
                    d.drag_over::<super::FolderDrag>(move |style, drag, _, _| if drag.fits(&over_account, Some(&over)) { style.bg(p.selection).border_1().border_color(p.accent) } else { style })
                        .on_drop(cx.listener(move |this, drag: &super::FolderDrag, window, cx| this.move_folder(drag.clone(), Some(target.clone()), window, cx)))
                })
                .when(in_favorites, |d| {
                    // A transparent 2px band above and below takes the insert line, so showing
                    // it moves nothing; the row is 4px shorter to stay the same height.
                    let over = favorite.clone();
                    d.border_t(px(2.))
                        .border_b(px(2.))
                        .border_color(gpui_kit::transparent_black())
                        .on_drag(fav_drag, |d: &super::FavoriteDrag, _pos, _window, cx| cx.new(|_| super::DragPreview { label: d.name.clone() }))
                        .drag_over::<super::FavoriteDrag>(move |style, drag, _, _| {
                            let from = order.iter().position(|f| f == &drag.favorite);
                            let to = order.iter().position(|f| f == &over);
                            match (from, to) {
                                (Some(from), Some(to)) if from < to => style.border_b(px(2.)).border_color(p.accent),
                                (Some(from), Some(to)) if from > to => style.border_t(px(2.)).border_color(p.accent),
                                _ => style,
                            }
                        })
                        .on_drop(cx.listener(move |this, drag: &super::FavoriteDrag, _window, cx| this.move_favorite_to(&drag.favorite, &drop_target, cx)))
                })
                .h_flex()
                .items_center()
                .gap(px(9.))
                .min_h(px(if in_favorites { m.row_height - 4. } else { m.row_height }))
                .pl(px(ICON_INSET + DEPTH_STEP * nav.depth as f32))
                .pr(px(10.))
                .when_some(expander, |d, (key, collapsed)| {
                    // In the row's left padding, so the icons stay in line with the other rows.
                    d.child(
                        div()
                            .id(SharedString::from(format!("expand-{key}")))
                            .absolute()
                            .left(px(2. + DEPTH_STEP * nav.depth as f32))
                            .top_0()
                            .bottom_0()
                            .w(px(14.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(icon(if collapsed { "chevron-right" } else { "chevron-down" }).size(px(12.)).text_color(p.muted))
                            .on_click(cx.listener(move |this, _, _window, cx| {
                                cx.stop_propagation();
                                this.toggle_section(&key, cx);
                            })),
                    )
                })
                .rounded(px(7.))
                .cursor_pointer()
                .text_size(px(13.))
                .text_color(if current { p.accent } else if container { p.muted } else { p.foreground })
                .when(container, |d| d.tooltip(|window, cx| gpui_kit::component::tooltip::Tooltip::new("Holds folders only. Mail cannot be stored here. Right-click to learn more.").build(window, cx)))
                .font_weight(if current { FontWeight::SEMIBOLD } else { FontWeight::NORMAL })
                .when(current, |d| d.bg(p.selection))
                .when(!current, |d| d.hover(|d| d.bg(p.hover)))
                // One shape for every folder; a label's color only tints it.
                .child(icon(nav.icon).flex_none().size(px(16.)).text_color(color.unwrap_or(if current { p.accent } else { p.muted })))
                .child(div().flex_1().truncate().child(nav.label.clone()))
                // Unread mail in the Inbox is a filled pill; the unread in other folders is
                // set in the action colour; totals (Starred, Drafts, Outbox) stay quiet.
                .when(count > 0, |d| {
                    let unread = matches!(nav.view, MailboxView::Kind(LabelKind::Inbox) | MailboxView::Label(_)) && !container;
                    let inbox = nav.view == MailboxView::Kind(LabelKind::Inbox);
                    d.child(
                        div()
                            .flex_none()
                            .text_size(px(11.))
                            .when(inbox, |d| {
                                let (fill, ink) = count_pill(&p);
                                d.min_w(px(20.))
                                    .h(px(18.))
                                    .px(px(6.))
                                    .rounded(px(9.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .bg(fill)
                                    .text_color(ink)
                                    .font_weight(FontWeight::BOLD)
                            })
                            .when(!inbox && unread, |d| d.font_weight(FontWeight::SEMIBOLD).text_color(if current { p.accent } else { p.foreground }))
                            .when(!unread, |d| d.text_color(if current { p.accent } else { p.muted }))
                            .child(count.to_string()),
                    )
                })
                .on_click(cx.listener(move |this, _, window, cx| match &fold_key {
                    Some(key) => this.toggle_section(key, cx),
                    None => this.open_folder(target.clone(), view.clone(), window, cx),
                }))
                .context_menu(move |menu, _window, _cx| {
                    if container {
                        // A container has no mail to open or pin, and the server drops it
                        // once the last folder in it goes: only a new folder, and help. A pin
                        // from before the folder lost its mail is hidden, so it can be removed.
                        let mut menu = menu;
                        if pinned {
                            let (t, f) = (me.clone(), favorite.clone());
                            menu = menu.item(PopupMenuItem::new("Remove from Favorites").on_click(move |_, _window, cx| {
                                t.update(cx, |this, cx| this.unpin_favorite(&f, cx)).ok();
                            }));
                            menu = menu.separator();
                        }
                        if let Some(parent) = new_inside.clone() {
                            let (t, account) = (me.clone(), menu_account.clone());
                            menu = menu.item(PopupMenuItem::new("New Folder Inside…").on_click(move |_, window, cx| {
                                t.update(cx, |this, cx| this.open_folder_prompt(account.clone(), super::folder_manage::FolderPromptMode::New { parent: Some(parent.clone()) }, window, cx)).ok();
                            }));
                            menu = menu.separator();
                        }
                        return menu.item(PopupMenuItem::new("About Folder-Only Mailboxes…").on_click(|_, _window, cx| cx.open_url(super::list::FOLDER_ONLY_HELP_URL)));
                    }
                    let (t, account, view) = (me.clone(), menu_account.clone(), menu_view.clone());
                    let mut menu = menu.item(PopupMenuItem::new("Open in New Window").on_click(move |_, window, cx| {
                        t.update(cx, |this, cx| this.open_folder_window(account.clone(), view.clone(), window, cx)).ok();
                    }));
                    menu = menu.separator();
                    let (t, f) = (me.clone(), favorite.clone());
                    menu = if pinned {
                        menu.item(PopupMenuItem::new("Remove from Favorites").on_click(move |_, _window, cx| {
                            t.update(cx, |this, cx| this.unpin_favorite(&f, cx)).ok();
                        }))
                    } else {
                        menu.item(PopupMenuItem::new("Add to Favorites").on_click(move |_, _window, cx| {
                            t.update(cx, |this, cx| this.pin_favorite(f.clone(), cx)).ok();
                        }))
                    };
                    if in_favorites {
                        let (t, f) = (me.clone(), favorite.clone());
                        menu = menu.item(PopupMenuItem::new("Move Up").disabled(!can_up).on_click(move |_, _window, cx| {
                            t.update(cx, |this, cx| this.shift_favorite(&f, true, cx)).ok();
                        }));
                        let (t, f) = (me.clone(), favorite.clone());
                        menu = menu.item(PopupMenuItem::new("Move Down").disabled(!can_down).on_click(move |_, _window, cx| {
                            t.update(cx, |this, cx| this.shift_favorite(&f, false, cx)).ok();
                        }));
                    }
                    if has_mail {
                        let (t, account, view) = (me.clone(), menu_account.clone(), menu_view.clone());
                        menu = menu.separator().item(PopupMenuItem::new("Mark All as Read").on_click(move |_, window, cx| {
                            t.update(cx, |this, cx| this.mark_folder_read(account.clone(), view.clone(), window, cx)).ok();
                        }));
                    }
                    if let Some(parent) = new_inside.clone() {
                        let (t, account) = (me.clone(), menu_account.clone());
                        menu = menu.separator().item(PopupMenuItem::new("New Folder Inside…").on_click(move |_, window, cx| {
                            t.update(cx, |this, cx| this.open_folder_prompt(account.clone(), super::folder_manage::FolderPromptMode::New { parent: Some(parent.clone()) }, window, cx)).ok();
                        }));
                    }
                    if let Some(label) = user_folder.clone() {
                        let (t, account, renamed) = (me.clone(), menu_account.clone(), label.clone());
                        // F2 renames the open folder, so only its menu shows the key.
                        let mut rename = PopupMenuItem::new("Rename Folder…");
                        if current {
                            rename = rename.action(Box::new(super::RenameFolder));
                        }
                        menu = menu.item(rename.on_click(move |_, window, cx| {
                            t.update(cx, |this, cx| this.open_folder_prompt(account.clone(), super::folder_manage::FolderPromptMode::Rename { label: renamed.clone() }, window, cx)).ok();
                        }));
                        let (t, account) = (me.clone(), menu_account.clone());
                        menu = menu.item(PopupMenuItem::new("Delete Folder…").on_click(move |_, window, cx| {
                            t.update(cx, |this, cx| this.delete_folder(account.clone(), label.clone(), window, cx)).ok();
                        }));
                    }
                    if let Some(kind) = empty_kind {
                        let (t, account) = (me.clone(), menu_account.clone());
                        let label = format!("Empty {}…", system_folder(kind).map(|(name, _)| name).unwrap_or("Folder"));
                        menu = menu.item(PopupMenuItem::new(label).on_click(move |_, window, cx| {
                            t.update(cx, |this, cx| this.empty_folder(account.clone(), kind, window, cx)).ok();
                        }));
                    }
                    menu
                })
        };

        let owner = self.owner();
        let accounts: Vec<(Account, String, &'static str)> = ordered_accounts.iter().map(|a| (a.clone(), self.account_name(&a.id), self.account_kind_label(a))).collect();
        let sidebar_mode = self.prefs.sidebar_mode;
        let active_id = self.account.id.clone();
        let active_email = self.account.email.clone();
        let needs_sign_in = self.active_issue().is_some();
        let can_sign_out = self.account.provider != ProviderKind::Sample;
        let sign_in_label = if needs_sign_in { self.sign_in_again_label() } else { "Sign in again" };
        let this = cx.entity().downgrade();
        let state_color = match status.state {
            SyncState::UpToDate => p.success,
            SyncState::Syncing | SyncState::Paused(_) => p.accent,
            SyncState::Offline => p.muted,
            SyncState::SignInRequired | SyncState::SyncFailed(_) => p.warning,
        };

        // Puts a 12px arrow in the same column as the rows' arrows.
        const HEADER_ARROW_INSET: f32 = 3.;
        let section_header = |key: String, text: String, collapsed: bool, top: f32, mark: Option<AnyElement>, cx: &mut Context<Self>| {
            div()
                .id(SharedString::from(format!("section-{key}")))
                .h_flex()
                .items_center()
                .gap(px(ICON_INSET - HEADER_ARROW_INSET - 12.))
                .mt(px(top))
                .mb(px(m.header_bottom))
                .pl(px(HEADER_ARROW_INSET))
                .pr(px(10.))
                .py(px(m.header_padding))
                .rounded(px(6.))
                .cursor_pointer()
                .text_size(px(11.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(p.muted)
                .hover(|d| d.bg(p.hover))
                .child(icon(if collapsed { "chevron-right" } else { "chevron-down" }).size(px(12.)).text_color(p.muted))
                // An account's service mark sits in the rows' icon column, its name in their
                // label column.
                .child(match mark {
                    Some(mark) => h_flex().flex_1().min_w_0().items_center().gap(px(9.)).child(mark).child(div().flex_1().min_w_0().truncate().child(text)).into_any_element(),
                    None => div().flex_1().min_w_0().truncate().child(text).into_any_element(),
                })
                .on_click(cx.listener(move |this, _, _window, cx| this.toggle_section(&key, cx)))
        };

        // An account's title and its Folders title: right-click for a new top-level folder,
        // or drop a folder here to move it to the top level.
        let top_level_target = |title: Stateful<Div>, account: &AccountId, cx: &mut Context<Self>| -> AnyElement {
            if !self.manages_folders(account) {
                return title.into_any_element();
            }
            let (me, account) = (cx.entity().downgrade(), account.clone());
            let over_account = account.clone();
            title
                .drag_over::<super::FolderDrag>(move |style, drag, _, _| if drag.fits(&over_account, None) { style.bg(p.selection).border_1().border_color(p.accent) } else { style })
                .on_drop(cx.listener(|this, drag: &super::FolderDrag, window, cx| this.move_folder(drag.clone(), None, window, cx)))
                .context_menu(move |menu, _window, _cx| {
                    let (t, account) = (me.clone(), account.clone());
                    menu.item(PopupMenuItem::new("New Folder…").on_click(move |_, window, cx| {
                        t.update(cx, |this, cx| this.open_folder_prompt(account.clone(), super::folder_manage::FolderPromptMode::New { parent: None }, window, cx)).ok();
                    }))
                })
                .into_any_element()
        };

        let favorites_collapsed = self.collapsed_sections.contains("favorites");
        let favorites: Vec<AnyElement> = if favorites_collapsed {
            Vec::new()
        } else {
            self.favorite_rows(&pinned_list, &p).into_iter().map(|(account, nav)| nav_button(self, "favorites", &account, nav, &pinned_list, cx).into_any_element()).collect()
        };

        // The account tree: every account in the saved order, or just the one in use.
        let all_accounts = self.prefs.sidebar_mode == SidebarMode::AllAccounts;
        let shown: Vec<Account> = if all_accounts {
            ordered_accounts
        } else {
            self.accounts.iter().filter(|a| a.id == self.account.id).cloned().collect()
        };
        let reorderable = all_accounts && shown.len() > 1;
        let order: std::rc::Rc<Vec<AccountId>> = std::rc::Rc::new(shown.iter().map(|a| a.id.clone()).collect());
        // Room for the insert line. Each block keeps it as a transparent margin so showing
        // the line (a border in the same space) does not move anything.
        const INSERT_LINE: f32 = 2.;
        let mut account_blocks: Vec<AnyElement> = Vec::new();
        for (i, a) in shown.iter().enumerate() {
            let key = format!("account:{}", a.id.0);
            let collapsed = self.collapsed_sections.contains(&key);
            let name = self.account_name(&a.id);
            let target = a.id.clone();
            let drag = super::AccountDrag { account: a.id.clone(), name: name.clone() };
            // The first header sits right under the Favorites line, whose own margin is the gap.
            let top = if i == 0 { 0. } else { m.header_top };
            let mark = self.account_mark(a).render(16., &p);
            let header = section_header(key, name, collapsed, top, Some(mark), cx)
                .font_weight(FontWeight::SEMIBOLD)
                .when(a.id == self.account.id && all_accounts, |d| d.text_color(p.foreground))
                .when(reorderable, |d| d.on_drag(drag, |d: &super::AccountDrag, _pos, _window, cx| cx.new(|_| super::DragPreview { label: d.name.clone() })));
            let header = top_level_target(header, &a.id, cx);
            let folders = (!collapsed).then(|| {
                let (mailboxes, folders) = self.account_folders(&a.id, &p);
                let mailboxes = v_flex().gap(px(m.row_gap)).children(mailboxes.into_iter().map(|nav| nav_button(self, "account", &a.id, nav, &pinned_list, cx)));
                let mut column = v_flex().child(mailboxes);
                if !folders.is_empty() {
                    let key = format!("folders:{}", a.id.0);
                    let folded = self.collapsed_sections.contains(&key);
                    let title = section_header(key, "Folders".into(), folded, m.header_top, None, cx);
                    column = column.child(top_level_target(title, &a.id, cx));
                    if !folded {
                        column = column.child(v_flex().gap(px(m.row_gap)).children(folders.into_iter().map(|nav| nav_button(self, "account", &a.id, nav, &pinned_list, cx))));
                    }
                }
                column
            });
            // The whole block (header and folders) takes the drop. The line marks where the
            // account lands: `move_account` puts it after this block when it comes from above
            // and before it when it comes from below.
            let order = order.clone();
            let over = target.clone();
            let block = div()
                .id(SharedString::from(format!("account-block-{}", a.id.0)))
                .flex()
                .flex_col()
                .my(px(INSERT_LINE))
                .child(header)
                .children(folders)
                .when(reorderable, |d| {
                    d.drag_over::<super::AccountDrag>(move |style, drag, _, _| {
                        let from = order.iter().position(|id| id == &drag.account);
                        let to = order.iter().position(|id| id == &over);
                        match (from, to) {
                            (Some(from), Some(to)) if from < to => style.mb(px(0.)).border_b(px(INSERT_LINE)).border_color(p.accent),
                            (Some(from), Some(to)) if from > to => style.mt(px(0.)).border_t(px(INSERT_LINE)).border_color(p.accent),
                            _ => style,
                        }
                    })
                    .on_drop(cx.listener(move |this, drag: &super::AccountDrag, _window, cx| this.move_account_to(&drag.account, &target, cx)))
                });
            account_blocks.push(block.into_any_element());
        }

        let nav = v_flex()
            .id("sidebar-nav")
            .flex_1()
            .min_h_0()
            // First thing in the sidebar: no gap above, the panel padding is enough.
            .child(section_header("favorites".into(), "Favorites".into(), favorites_collapsed, 0., None, cx))
            .when(!favorites_collapsed, |d| d.child(v_flex().gap(px(m.row_gap)).children(favorites)))
            // Favorites span every account; the line separates them from the account tree,
            // with the same gap above and below.
            .child(div().flex_none().h(px(1.)).my(px(m.divider_gap)).mx(px(8.)).bg(p.divider))
            .children(account_blocks)
            .child(div().h(px(m.bottom)))
            .overflow_y_scrollbar();

        v_flex()
            .size_full()
            .bg(p.sidebar)
            .text_color(p.foreground)
            // Same top inset as the list and reader panes (`main` layout in mod.rs), so the first
            // header lines up with their top edge.
            .pt(px(8.))
            .px(px(6.))
            .pb(px(m.bottom))
            .child(nav)
            // The account switcher at the bottom is for one-at-a-time mode; with every
            // account in the tree, a click on another account's folder switches instead.
            .when(!all_accounts, |d| d.child(
                h_flex()
                    .items_center()
                    .gap(px(9.))
                    // The avatar is centered over the rows' icons.
                    .px(px(ICON_INSET + 8. - 15.))
                    .pt(px(m.footer_top))
                    .child(owner_avatar(&owner, 30., &p))
                    .child(
                        v_flex()
                            .min_w_0()
                            .child(div().text_size(px(13.)).font_weight(FontWeight::MEDIUM).truncate().child(owner.display_name()))
                            .child(
                                h_flex()
                                    .items_center()
                                    .gap(px(5.))
                                    .text_size(px(11.))
                                    .text_color(p.muted)
                                    .child(div().size(px(6.)).rounded_full().bg(state_color))
                                    .child(format!("{} · {}", self.account.provider.label(), status.state.label())),
                            ),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("account-menu")
                            .ghost()
                            .xsmall()
                            .icon(icon("chevrons-up-down").size(px(14.)).text_color(p.muted))
                            .tooltip("Accounts")
                            .dropdown_menu(move |mut menu, _window, _cx| {
                                menu = menu.label("Accounts");
                                for (a, name, kind) in &accounts {
                                    let id = a.id.clone();
                                    let t = this.clone();
                                    let text = if name == &a.email { format!("{} · {kind}", a.email) } else { format!("{name} · {}", a.email) };
                                    menu = menu.item(PopupMenuItem::new(text).checked(a.id == active_id).on_click(
                                        move |_, window, cx| {
                                            let _ = t.update(cx, |this, cx| this.switch_account(id.clone(), window, cx));
                                        },
                                    ));
                                }
                                menu = menu.separator();
                                for (text, mode) in [("Show all accounts", SidebarMode::AllAccounts), ("Show one at a time", SidebarMode::OneAccount)] {
                                    let t = this.clone();
                                    menu = menu.item(PopupMenuItem::new(text).checked(sidebar_mode == mode).on_click(move |_, _window, cx| {
                                        let _ = t.update(cx, |this, cx| this.set_sidebar_mode(mode, cx));
                                    }));
                                }
                                menu = menu.separator();
                                let t = this.clone();
                                menu = menu.item(PopupMenuItem::new("Add Gmail account…").on_click(move |_, window, cx| {
                                    let _ = t.update(cx, |this, cx| this.start_google_sign_in(None, window, cx));
                                }));
                                let t = this.clone();
                                menu = menu.item(PopupMenuItem::new("Add Microsoft account…").on_click(move |_, window, cx| {
                                    let _ = t.update(cx, |this, cx| this.open_microsoft_setup(window, cx));
                                }));
                                for &guide in overlook_provider::app_passwords::GUIDES {
                                    let t = this.clone();
                                    menu = menu.item(PopupMenuItem::new(format!("Add {} account…", guide.short_name)).on_click(move |_, window, cx| {
                                        let _ = t.update(cx, |this, cx| this.open_guided_setup(guide, window, cx));
                                    }));
                                }
                                let t = this.clone();
                                menu = menu.item(PopupMenuItem::new("Add IMAP account…").on_click(move |_, window, cx| {
                                    let _ = t.update(cx, |this, cx| this.open_account_setup(ProviderKind::Imap, super::SetupMode::New, window, cx));
                                }));
                                if needs_sign_in {
                                    let t = this.clone();
                                    let email = active_email.clone();
                                    menu = menu.item(PopupMenuItem::new(sign_in_label).on_click(move |_, window, cx| {
                                        let _ = t.update(cx, |this, cx| this.sign_in_again(Some(email.clone()), window, cx));
                                    }));
                                }
                                if !needs_sign_in && can_sign_out {
                                    let t = this.clone();
                                    let id = active_id.clone();
                                    menu = menu.item(PopupMenuItem::new("Sign out").on_click(move |_, window, cx| {
                                        let _ = t.update(cx, |this, cx| this.sign_out_account(id.clone(), None, window, cx));
                                    }));
                                }
                                let t = this.clone();
                                let id = active_id.clone();
                                menu.item(PopupMenuItem::new("Remove this account from this device").on_click(move |_, window, cx| {
                                    let _ = t.update(cx, |this, cx| this.remove_account(id.clone(), window, cx));
                                }))
                            }),
                    ),
            ))
    }
}

/// The fill and ink of the Inbox's unread pill: the theme's message accent (Alexander's
/// golden orange), with whichever ink reads on it.
fn count_pill(p: &crate::theme::Palette) -> (Hsla, Hsla) {
    let fill = p.message_accent();
    let ink = if crate::theme::relative_luminance(fill) > 0.4 { gpui_kit::rgb(0x1F2A33).into() } else { gpui_kit::white() };
    (fill, ink)
}
