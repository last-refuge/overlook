//! Folder windows ("Open in New Window"): one more mail window showing a chosen folder.
//!
//! Like a pop-out composer, a folder window is a detached `MailWindow` that shares the store
//! and providers with the main window and never syncs by itself. The main window keeps a list
//! of its folder windows. It reloads them when a sync or a local change lands, and it passes
//! preference changes (Favorites, account order, …) along so every sidebar agrees.

use super::composer_window::{WindowOwner, WindowSetup};
use super::*;

pub(super) struct FolderWindow {
    window: AnyWindowHandle,
    editor: WeakEntity<MailWindow>,
    account: AccountId,
    view: MailboxView,
    _closed: Subscription,
}

impl MailWindow {
    /// The main window this window belongs to, if it is a composer or folder window.
    pub(super) fn window_owner(&self) -> Option<WindowOwner> {
        self.composer_owner.clone().or_else(|| self.folder_owner.clone())
    }

    /// Opens `view` of `account` in a window of its own, or brings an open one forward.
    /// Folder windows always belong to the main window, even when opened from another
    /// folder window.
    pub(crate) fn open_folder_window(&mut self, account: AccountId, view: MailboxView, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(owner) = self.window_owner() {
            let parent = owner.parent.clone();
            let opened = owner
                .window
                .update(cx, |_, window, cx| parent.update(cx, |this, cx| this.open_folder_window(account.clone(), view.clone(), window, cx)))
                .is_ok();
            if !opened {
                crate::ui::toast(window, cx, crate::notify::Level::Warning, "Open the main Overlook window to open folders in new windows.");
            }
            return;
        }
        self.folder_windows.retain(|f| f.editor.upgrade().is_some());
        if let Some(open) = self.folder_windows.iter().find(|f| f.account == account && f.view == view) {
            if open.window.update(cx, |_, window, _| window.activate_window()).is_ok() {
                return;
            }
        }

        let store = self.store.clone();
        let sample = self.sample.clone();
        let accounts = self.accounts.clone();
        let prefs = self.prefs.clone();
        let account_prefs = self.account_prefs.clone();
        let provider_issues = self.provider_issues.clone();
        let remote_allowed = self.remote_allowed.clone();
        let setup = WindowSetup::Folder {
            appearance: self.appearance,
            owner: WindowOwner { parent: cx.entity(), window: window.window_handle() },
            providers: self.providers.clone(),
            account: account.clone(),
            view: view.clone(),
        };
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(1120.), px(800.)), cx))),
            window_min_size: Some(size(px(880.), px(560.))),
            app_id: Some(crate::APP_ID.into()),
            ..TitleBar::window_options()
        };
        let opened = gpui_kit::open_window(options, cx, move |window, cx| {
            cx.new(|cx| {
                let mut this = MailWindow::new(store, sample, accounts, Some(account.clone()), setup, window, cx);
                // Settings saved moments ago may not be on disk yet; start from the main window's.
                this.prefs = prefs;
                this.account_prefs = account_prefs;
                this.provider_issues = provider_issues;
                this.remote_allowed = remote_allowed;
                this.reload_account_labels();
                this
            })
        });
        match opened {
            Ok((handle, editor)) => {
                let parent = cx.entity().downgrade();
                let closed = cx.on_window_closed(move |cx, closed| {
                    if closed == handle.window_id() {
                        parent
                            .update(cx, |this, _| this.folder_windows.retain(|f| f.window.window_id() != closed))
                            .ok();
                    }
                });
                let (account, view) = editor.read(cx).folder_key();
                self.folder_windows.push(FolderWindow { window: handle, editor: editor.downgrade(), account, view, _closed: closed });
            }
            Err(error) => {
                tracing::warn!("could not open a folder window: {error}");
                crate::ui::toast(window, cx, crate::notify::Level::Error, "Couldn’t open a new window.");
            }
        }
    }

    /// What a folder window was opened on, used to bring it forward instead of opening it twice.
    fn folder_key(&self) -> (AccountId, MailboxView) {
        (self.account.id.clone(), self.view.clone())
    }

    /// After a rename changed a folder's id (IMAP): the other windows showing that folder
    /// move to the new id before they reload, or they would look for a label that is gone
    /// and show nothing under a blank title. A folder window hands this to the main window,
    /// which keeps the list of folder windows.
    pub(super) fn follow_renamed_folder_in_other_windows(&mut self, account: &AccountId, old: &LabelId, new: &LabelId, cx: &mut Context<Self>) {
        let (account, old, new) = (account.clone(), old.clone(), new.clone());
        if let Some(owner) = self.folder_owner.clone() {
            cx.defer(move |cx| {
                owner.parent.update(cx, |main, cx| {
                    main.follow_view(&account, &old, &new);
                    main.retarget_folder_windows(&account, &old, &new, cx);
                });
            });
            return;
        }
        self.retarget_folder_windows(&account, &old, &new, cx);
    }

    /// In the main window: points the folder windows open on `old` at `new`, in the list
    /// used to bring them forward and in the windows themselves.
    fn retarget_folder_windows(&mut self, account: &AccountId, old: &LabelId, new: &LabelId, cx: &mut Context<Self>) {
        let mut editors: Vec<WeakEntity<MailWindow>> = Vec::new();
        for f in self.folder_windows.iter_mut().filter(|f| &f.account == account && f.view == MailboxView::Label(old.clone())) {
            f.view = MailboxView::Label(new.clone());
            editors.push(f.editor.clone());
        }
        if editors.is_empty() {
            return;
        }
        let (account, old, new) = (account.clone(), old.clone(), new.clone());
        cx.defer(move |cx| {
            for editor in editors {
                editor.update(cx, |this, _| this.follow_view(&account, &old, &new)).ok();
            }
        });
    }

    /// Re-reads the store in every folder window, after this (main) window synced or wrote.
    pub(crate) fn reload_folder_windows(&self, cx: &mut Context<Self>) {
        let editors: Vec<WeakEntity<MailWindow>> = self.folder_windows.iter().map(|f| f.editor.clone()).collect();
        if editors.is_empty() {
            return;
        }
        cx.defer(move |cx| {
            for editor in editors {
                editor
                    .update(cx, |this, cx| {
                        this.reload_account_labels();
                        this.reload(cx);
                        cx.notify();
                    })
                    .ok();
            }
        });
    }

    /// Sends this window's preferences to the others after `save_prefs`: a folder window
    /// sends them to the main window, and the main window passes them to every folder window.
    pub(crate) fn share_prefs(&self, cx: &mut Context<Self>) {
        let prefs = self.prefs.clone();
        let account_prefs = self.account_prefs.clone();
        if let Some(owner) = &self.folder_owner {
            let parent = owner.parent.clone();
            cx.defer(move |cx| {
                parent.update(cx, |this, cx| {
                    this.adopt_prefs(prefs, account_prefs, cx);
                    this.share_prefs(cx);
                });
            });
            return;
        }
        let editors: Vec<WeakEntity<MailWindow>> = self.folder_windows.iter().map(|f| f.editor.clone()).collect();
        if editors.is_empty() {
            return;
        }
        cx.defer(move |cx| {
            for editor in editors {
                editor.update(cx, |this, cx| this.adopt_prefs(prefs.clone(), account_prefs.clone(), cx)).ok();
            }
        });
    }

    /// Passes the Focused Inbox switch to the other windows: it decides what the Inbox badge
    /// counts, so every window must agree. A folder window sends it through the main window.
    pub(crate) fn share_focused_inbox(&self, cx: &mut Context<Self>) {
        let enabled = self.focused_enabled;
        let targets: Vec<WeakEntity<MailWindow>> = match &self.folder_owner {
            Some(owner) => vec![owner.parent.downgrade()],
            None => self.folder_windows.iter().map(|f| f.editor.clone()).collect(),
        };
        cx.defer(move |cx| {
            for target in targets {
                target
                    .update(cx, |this, cx| {
                        if this.focused_enabled != enabled {
                            this.focused_enabled = enabled;
                            this.inbox_tab = FocusTab::Focused;
                            this.reload(cx);
                            // The main window passes it on to the other folder windows.
                            this.share_focused_inbox(cx);
                        }
                    })
                    .ok();
            }
        });
    }

    /// Takes preferences changed in another window, without saving them again.
    fn adopt_prefs(&mut self, prefs: Prefs, account_prefs: HashMap<AccountId, AccountPrefs>, cx: &mut Context<Self>) {
        if self.prefs == prefs && self.account_prefs == account_prefs {
            return;
        }
        self.prefs = prefs;
        self.account_prefs = account_prefs;
        self.reminders_enabled = self.prefs.event_reminders;
        self.reload(cx);
        cx.notify();
    }

    /// Closes the folder windows when the main window goes. A folder window does not sync
    /// by itself, so it must not stay behind showing mail that no longer updates.
    pub(super) fn close_folder_windows(&mut self, cx: &mut Context<Self>) {
        for f in self.folder_windows.drain(..) {
            f.window.update(cx, |_, window, _| window.remove_window()).ok();
        }
    }

    /// In a folder window, runs `f` in the main window instead and brings that window
    /// forward. Returns false in the main window, where the caller does the work itself.
    pub(super) fn in_main_window(&self, cx: &mut Context<Self>, f: impl FnOnce(&mut MailWindow, &mut Window, &mut Context<MailWindow>) + 'static) -> bool {
        let Some(owner) = self.folder_owner.clone() else {
            return false;
        };
        let parent = owner.parent.clone();
        cx.defer(move |cx| {
            owner
                .window
                .update(cx, |_, window, cx| {
                    window.activate_window();
                    parent.update(cx, |this, cx| f(this, window, cx));
                })
                .ok();
        });
        true
    }
}
