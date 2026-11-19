//! Offline folders: mail moved off the server and kept in folders on this computer.
//!
//! The folders belong to one hidden account (`MailWindow::offline`, provider
//! [`ProviderKind::Local`]), so opening one shows it with the usual list, reader, search and
//! folder menus. Each top-level offline folder is a directory the person chose, with one
//! `.eml` file per message (see `crate::offline_disk`); its menu shows where it is, and it
//! can be disconnected (forgotten here, left on disk) and opened again.
//!
//! Dropping mail of a server account onto an offline folder copies each message's original
//! bytes from the server into the folder's directory (written and flushed to disk), files a
//! parsed copy, and only then deletes the message from the server: permanently where the
//! server allows it, otherwise into its Trash. A message that can't be downloaded
//! completely stays on the server untouched.

use super::folder_manage::{folder_error, FolderPromptMode};
use super::*;
use crate::offline_disk::{self, shown_path};
use overlook_provider::local;

/// How many messages are copied before the copied ones leave the server and the lists
/// update, so a large drag shows progress.
const BATCH: usize = 10;

/// The setting that remembers where new offline folders go.
const LOCATION_KEY: &str = "offline.location";

impl MailWindow {
    pub(crate) fn is_offline(&self, account: &AccountId) -> bool {
        account == &self.offline.id
    }

    /// Whether the window shows an offline folder.
    pub(crate) fn offline_active(&self) -> bool {
        self.is_offline(&self.account.id)
    }

    /// Whether offline folders are offered: there must be an account to move mail from.
    pub(crate) fn offers_offline(&self) -> bool {
        self.has_account() && self.providers.contains_key(&self.offline.id)
    }

    /// Notes how much offline mail there is when no account is connected: offline folders
    /// outlive the accounts their mail came from, and show again once one is added.
    pub(crate) fn count_kept_offline_mail(&mut self) {
        self.offline_kept = if self.has_account() { 0 } else { self.store.message_count(&self.offline.id).unwrap_or(0) };
    }

    /// One of the user's accounts, or the offline folders.
    pub(crate) fn known_account(&self, id: &AccountId) -> Option<Account> {
        if self.is_offline(id) {
            return Some(self.offline.clone());
        }
        self.accounts.iter().find(|a| &a.id == id).cloned()
    }

    /// The account mail is written from: the open one, or, while an offline folder is open,
    /// the mail account shown before it.
    pub(crate) fn writing_account(&self) -> Account {
        if !self.offline_active() {
            return self.account.clone();
        }
        self.last_mail_account
            .as_ref()
            .and_then(|id| self.accounts.iter().find(|a| &a.id == id))
            .or_else(|| self.accounts.first())
            .cloned()
            .unwrap_or_else(no_account)
    }

    /// Who mail is written as: the owner of [`Self::writing_account`].
    pub(crate) fn writing_owner(&self) -> Address {
        if !self.offline_active() {
            return self.owner();
        }
        let from = self.writing_account();
        Address::named(&from.display_name, &from.email)
    }

    /// Leaves the offline folders for the mail account shown before them (Calendar and
    /// Contacts belong to a mail account).
    pub(crate) fn leave_offline(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.offline_active() {
            let back = self.writing_account();
            if !back.id.0.is_empty() {
                self.switch_account(back.id, window, cx);
            }
        }
    }

    /// A folder's name for messages: "Taxes › 2025".
    pub(crate) fn offline_folder_name(name: &str) -> String {
        name.replace('/', " › ")
    }

    /// An offline folder by id.
    pub(crate) fn offline_folder(&self, id: &LabelId) -> Option<Label> {
        self.labels_of(&self.offline.id).iter().find(|l| &l.id == id && l.kind == LabelKind::User).cloned()
    }

    /// Where an offline folder is on disk.
    pub(crate) fn offline_dir(&self, id: &LabelId) -> Option<PathBuf> {
        self.offline_folder(id).as_ref().and_then(local::dir_of)
    }

    /// Where new top-level offline folders go: the last place picked, or the default.
    pub(crate) fn offline_location(&self) -> PathBuf {
        self.store.setting::<PathBuf>(LOCATION_KEY).ok().flatten().unwrap_or_else(offline_disk::default_location)
    }

    /// Shows an offline folder's directory in the file manager.
    pub(crate) fn reveal_offline_folder(&self, id: &LabelId, window: &mut Window, cx: &mut Context<Self>) {
        match self.offline_dir(id) {
            Some(dir) if dir.is_dir() => cx.reveal_path(&dir),
            Some(dir) => crate::ui::toast(window, cx, crate::notify::Level::Warning, format!("{} can’t be found. Is its drive connected?", shown_path(&dir))),
            None => {}
        }
    }

    /// Copies an offline folder's location to the clipboard.
    pub(crate) fn copy_offline_location(&self, id: &LabelId, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(dir) = self.offline_dir(id) {
            cx.write_to_clipboard(ClipboardItem::new_string(dir.display().to_string()));
            crate::ui::toast(window, cx, crate::notify::Level::Info, format!("Copied {}", shown_path(&dir)));
        }
    }

    // ------------------------------------------------------------------ moving mail in

    /// Mail dragged from the list onto offline folder `target`: the messages of the dragged
    /// conversations that the open view shows (a conversation's trashed messages go only
    /// when dragged from Trash).
    pub(crate) fn drop_on_offline(&mut self, drag: ConversationDrag, target: LabelId, window: &mut Window, cx: &mut Context<Self>) {
        let conversations: Vec<ConversationId> = match &drag.mail {
            DraggedMail::Conversation(id) => vec![id.clone()],
            DraggedMail::Selection => self.target_conversations(),
        };
        let ids: Vec<MessageId> = conversations.iter().flat_map(|c| self.mail_index.visible_message_ids_in(c, &self.view)).collect();
        if !ids.is_empty() {
            self.move_offline(ids, target, window, cx);
        }
    }

    /// Handles mail dropped (or moved) onto offline folder `target`: from the open server
    /// account it is copied here and deleted from the server; within offline folders it
    /// just changes folder.
    pub(crate) fn move_offline(&mut self, ids: Vec<MessageId>, target: LabelId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(folder) = self.offline_folder(&target) else { return };
        if !local::dir_of(&folder).is_some_and(|d| d.is_dir()) {
            let place = local::dir_of(&folder).map(|d| shown_path(&d)).unwrap_or_default();
            crate::ui::toast(window, cx, crate::notify::Level::Error, format!("“{}” can’t be found at {place}. Connect its drive, or disconnect the folder.", Self::offline_folder_name(&folder.name)));
            return;
        }
        if self.offline_active() {
            return self.move_within_offline(ids, folder, window, cx);
        }
        let source = self.account.clone();
        let ids: Vec<MessageId> = ids.into_iter().filter(|id| !self.offline_moving.contains(id)).collect();
        if ids.is_empty() {
            return;
        }
        let Some(provider) = self.providers.get(&source.id).map(|p| p.mail.clone()) else {
            crate::ui::toast(window, cx, crate::notify::Level::Error, format!("Sign in to {} again first: each message is downloaded in full before it leaves the server.", self.account_name(&source.id)));
            return;
        };
        let delete_forever = provider.supports_delete_forever();
        let name = Self::offline_folder_name(&folder.name);
        let total = ids.len();
        self.offline_moving.extend(ids.iter().cloned());
        crate::ui::toast(window, cx, crate::notify::Level::Info, format!("Moving {} to “{name}” on this computer…", messages_phrase(total)));
        let next = self.removal_neighbour();
        let (store, offline) = (self.store.clone(), self.offline.clone());
        cx.spawn_in(window, async move |this, cx| {
            let mut moved = 0usize;
            let mut failed: Vec<(MessageId, String)> = Vec::new();
            for chunk in ids.chunks(BATCH) {
                let chunk = chunk.to_vec();
                let result = cx.background_spawn(offline_disk::copy_to_offline(store.clone(), provider.clone(), source.clone(), offline.clone(), folder.clone(), chunk.clone())).await;
                let stopped = result.stopped.is_some();
                let alive = this.update_in(cx, |this, window, cx| {
                    for id in &chunk {
                        this.offline_moving.remove(id);
                    }
                    if !result.copied.is_empty() {
                        moved += result.copied.len();
                        // Every copy is on disk: now the server's can go.
                        let change = if delete_forever {
                            overlook_store::LocalChange::DeleteForever { message_ids: result.copied.clone(), folder: None }
                        } else {
                            overlook_store::LocalChange::Trash { message_ids: result.copied.clone() }
                        };
                        this.apply_local_for(&source.id, change, window, cx);
                        if this.offline_active() {
                            // Opened meanwhile: show the copies as they land.
                            this.reload(cx);
                        } else {
                            this.mark_counts_stale([offline.id.clone()]);
                            this.refresh_account_counts(cx);
                        }
                        // Move on only when the open conversation has gone; the rest of the
                        // selection is settled once the whole drag is done.
                        let open_gone = this.selected.as_ref().is_some_and(|s| !this.conversations.iter().any(|c| &c.id == s));
                        if this.account.id == source.id && open_gone && this.marked.len() <= 1 {
                            this.select_after_removal(next.clone(), window, cx);
                        }
                    }
                    cx.notify();
                });
                failed.extend(result.failed);
                if alive.is_err() || stopped {
                    break;
                }
            }
            let _ = this.update_in(cx, |this, window, cx| {
                // Whatever was not attempted is no longer on its way.
                for id in &ids {
                    this.offline_moving.remove(id);
                }
                if moved > 0 {
                    // The folder's manifest learns about the new mail.
                    this.sync_offline_files(cx);
                    if this.account.id == source.id {
                        this.select_after_removal(next.clone(), window, cx);
                    }
                    let title = format!("Moved {} to offline folder “{name}”", messages_phrase(moved));
                    let detail = if delete_forever { "Deleted from the server" } else { "Server copies moved to Trash" };
                    this.record(source.id.clone(), title, detail.into(), overlook_store::Reversal::None, None, window, cx);
                }
                let trash_note = if delete_forever { "" } else { " The server’s copies are in its Trash." };
                match (moved, failed.first()) {
                    (_, None) => crate::ui::toast(window, cx, crate::notify::Level::Success, format!("Moved {} to “{name}” on this computer.{trash_note}", messages_phrase(moved))),
                    (0, Some((_, why))) => crate::ui::toast(window, cx, crate::notify::Level::Error, format!("Couldn’t move {} offline: {why}. Nothing was removed from the server.", messages_phrase(failed.len()))),
                    (_, Some((_, why))) => crate::ui::toast(
                        window,
                        cx,
                        crate::notify::Level::Warning,
                        format!("Moved {moved} of {total} messages to “{name}”.{trash_note} {} stayed on the server: {why}.", messages_phrase(failed.len())),
                    ),
                }
            });
        })
        .detach();
    }

    /// Moves offline messages to another offline folder. A message is in one folder at a
    /// time, as on a server; its file follows once the change is written.
    fn move_within_offline(&mut self, ids: Vec<MessageId>, folder: Label, window: &mut Window, cx: &mut Context<Self>) {
        let mut changes = vec![overlook_store::LocalChange::AddLabel { message_ids: ids.clone(), label_id: folder.id.clone() }];
        for other in self.labels.iter().filter(|l| l.id != folder.id) {
            let inside: Vec<MessageId> = ids.iter().filter(|id| self.mail_index.message(id).is_some_and(|m| m.has_label(&other.id))).cloned().collect();
            if !inside.is_empty() {
                changes.push(overlook_store::LocalChange::RemoveLabel { message_ids: inside, label_id: other.id.clone() });
            }
        }
        let next = self.removal_neighbour();
        self.perform(format!("Moved to “{}”", Self::offline_folder_name(&folder.name)), changes, true, window, cx);
        self.select_after_removal(next, window, cx);
    }

    // ------------------------------------------------------------- keeping disk in step

    /// Marks the files of offline messages about to be deleted, so they go from disk once
    /// the deletion is written.
    pub(crate) fn doom_offline_files(&mut self, ids: &[MessageId]) {
        let files = ids.iter().filter_map(|id| self.mail_index.message(id)).filter_map(|m| m.provider_id.as_ref()).map(|p| PathBuf::from(&p.0)).filter(|p| p.is_absolute());
        self.offline_doomed.extend(files);
    }

    /// Moves files to the folders their messages are now in, deletes the files of deleted
    /// messages and rewrites the folders' manifests. Runs after every offline change.
    pub(crate) fn sync_offline_files(&mut self, cx: &mut Context<Self>) {
        let doomed = std::mem::take(&mut self.offline_doomed);
        let (store, offline) = (self.store.clone(), self.offline.id.clone());
        cx.spawn(async move |this, cx| {
            let result = cx.background_spawn(async move { offline_disk::reconcile(&store, &offline, &providers::offline_dir(), &doomed) }).await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    // Files that couldn't be deleted yet are tried again after the next change.
                    Ok(kept_back) => this.offline_doomed.extend(kept_back),
                    Err(e) => tracing::warn!("offline folders: {e}"),
                }
                // Files moved: the open messages read them from their new places.
                if this.offline_active() {
                    this.reload(cx);
                }
            });
        })
        .detach();
    }

    /// At startup: folders from the first version of offline folders get directories, then
    /// files and manifests are brought in step.
    pub(crate) fn start_offline_files(&mut self, cx: &mut Context<Self>) {
        let (store, offline, location) = (self.store.clone(), self.offline.id.clone(), self.offline_location());
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    let adopted = offline_disk::adopt_legacy_folders(&store, &offline, &location)?;
                    if adopted > 0 {
                        tracing::info!("offline folders: gave {adopted} folder(s) a place in {}", location.display());
                    }
                    offline_disk::reconcile(&store, &offline, &providers::offline_dir(), &[])?;
                    Ok::<usize, String>(adopted)
                })
                .await;
            match result {
                Ok(adopted) if adopted > 0 => {
                    let _ = this.update(cx, |this, cx| {
                        this.reload_account_labels();
                        this.reload(cx);
                    });
                }
                Ok(_) => {}
                Err(e) => tracing::warn!("offline folders: {e}"),
            }
        })
        .detach();
    }

    // ------------------------------------------------------------------------- folders

    /// Picks where new offline folders go, for the New Offline Folder dialog.
    pub(crate) fn choose_offline_location(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions { files: false, directories: true, multiple: false, prompt: Some("Choose".into()) });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = receiver.await else { return };
            let Some(dir) = paths.into_iter().next() else { return };
            let _ = this.update(cx, |this, cx| {
                this.save_setting(LOCATION_KEY, &dir);
                if let Some(prompt) = this.folder_prompt.as_mut() {
                    prompt.location = Some(dir);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The New and Rename dialog for an offline folder: the directory is made or renamed
    /// on disk.
    pub(crate) fn submit_offline_folder_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(prompt) = self.folder_prompt.as_mut() else { return };
        if prompt.busy {
            return;
        }
        let name = prompt.input.read(cx).value().trim().to_owned();
        if let Some(problem) = overlook_provider::folder_name_problem(&name, "/") {
            prompt.error = Some(problem);
            cx.notify();
            return;
        }
        prompt.busy = true;
        prompt.error = None;
        let (parent, renaming) = match &prompt.mode {
            FolderPromptMode::New { parent } => (parent.clone(), None),
            FolderPromptMode::Rename { label } => (None, Some(label.clone())),
        };
        let location = prompt.location.clone().unwrap_or_else(offline_disk::default_location);
        let asked = prompt.input.entity_id();
        let (store, offline) = (self.store.clone(), self.offline.id.clone());
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn({
                    let renaming = renaming.clone();
                    async move {
                        match renaming {
                            Some(label) => {
                                let labels = store.labels(&offline).unwrap_or_default();
                                let changed = local::rename_folder(&label, &name, &labels)?;
                                offline_disk::rename_folder(&store, &offline, &changed)?;
                                Ok(changed.first().map(|c| c.new.clone()).unwrap_or(label))
                            }
                            None => offline_disk::create_folder(&store, &offline, &name, parent.as_ref(), &location),
                        }
                    }
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                let current = this.folder_prompt.as_ref().is_some_and(|p| p.input.entity_id() == asked);
                match result {
                    Ok(label) => {
                        if current {
                            this.close_folder_prompt(window, cx);
                        }
                        let offline = this.offline.id.clone();
                        this.reload_account_labels();
                        this.mark_counts_stale([offline]);
                        this.reload(cx);
                        let place = local::dir_of(&label).map(|d| shown_path(&d)).unwrap_or_default();
                        let what = if renaming.is_some() { "Renamed to" } else { "Created" };
                        crate::ui::toast(window, cx, crate::notify::Level::Success, format!("{what} “{}” in {place}.", Self::offline_folder_name(&label.name)));
                    }
                    Err(e) => {
                        if current {
                            if let Some(prompt) = this.folder_prompt.as_mut() {
                                prompt.busy = false;
                                prompt.error = Some(folder_error(&e));
                            }
                            cx.notify();
                        } else {
                            crate::ui::toast(window, cx, crate::notify::Level::Error, folder_error(&e));
                        }
                    }
                }
            });
        })
        .detach();
    }

    /// Moves a dragged offline folder into another, after asking. Folders stay inside the
    /// top-level folder they are in, so their files never cross drives.
    pub(crate) fn move_offline_folder(&mut self, label: Label, parent: Option<Label>, window: &mut Window, cx: &mut Context<Self>) {
        let labels = self.labels_of(&self.offline.id).to_vec();
        let root = |l: &Label| local::root_of(l, &labels).map(|r| r.id.clone());
        let same_root = match &parent {
            Some(p) => root(p) == root(&label) && label.name.contains('/'),
            None => false,
        };
        if !same_root {
            crate::ui::toast(window, cx, crate::notify::Level::Info, "Folders move within the offline folder they are in. To move mail elsewhere, drag the messages instead.");
            return;
        }
        let changed = match local::move_folder(&label, parent.as_ref(), &labels) {
            Ok(changed) => changed,
            Err(e) => return crate::ui::toast(window, cx, crate::notify::Level::Error, folder_error(&e)),
        };
        let title = format!("Move “{}” into “{}”?", Self::offline_folder_name(&label.name), parent.as_ref().map(|p| Self::offline_folder_name(&p.name)).unwrap_or_default());
        let answer = window.prompt(PromptLevel::Info, &title, Some("Its messages and the folders inside it move with it, on disk too."), &["Move", "Cancel"], cx);
        let (store, offline) = (self.store.clone(), self.offline.id.clone());
        cx.spawn_in(window, async move |this, cx| {
            if answer.await != Ok(0) {
                return;
            }
            let result = cx.background_spawn(async move { offline_disk::rename_folder(&store, &offline, &changed) }).await;
            let _ = this.update_in(cx, |this, window, cx| match result {
                Ok(()) => {
                    this.reload_account_labels();
                    this.reload(cx);
                    crate::ui::toast(window, cx, crate::notify::Level::Success, format!("Moved “{}”.", Self::offline_folder_name(&label.name)));
                }
                Err(e) => crate::ui::toast(window, cx, crate::notify::Level::Error, format!("Couldn’t move “{}”: {}", Self::offline_folder_name(&label.name), folder_error(&e))),
            });
        })
        .detach();
    }

    /// Deletes an offline folder, the folders inside it and all their mail from this
    /// computer, after asking. There is no other copy, so it can't be undone.
    pub(crate) fn delete_offline_folder(&mut self, label: Label, window: &mut Window, cx: &mut Context<Self>) {
        let offline = self.offline.id.clone();
        let labels = self.labels_of(&offline).to_vec();
        let gone = local::folder_and_inside(&label, &labels);
        let Some(top) = local::root_of(&label, &labels).cloned() else { return };
        let count: usize = gone.iter().map(|l| self.store.message_ids_with_label(&offline, &l.id).map(|ids| ids.len()).unwrap_or(0)).sum();
        let name = Self::offline_folder_name(&label.name);
        let place = local::dir_of(&label).map(|d| format!(" in {}", shown_path(&d))).unwrap_or_default();
        let inside = if gone.len() > 1 { ", the folders inside it," } else { "" };
        let detail = format!(
            "The folder{inside} and {} in it are deleted from this computer{place}. They are no longer on any server, so this can’t be undone. To stop showing it but keep it on disk, choose Disconnect instead.",
            if count == 1 { "the 1 message".to_owned() } else { format!("the {count} messages") }
        );
        let answer = window.prompt(PromptLevel::Warning, &format!("Delete the offline folder “{name}”?"), Some(&detail), &["Delete", "Cancel"], cx);
        let store = self.store.clone();
        cx.spawn_in(window, async move |this, cx| {
            if answer.await != Ok(0) {
                return;
            }
            let files_gone = gone.clone();
            let offline_id = offline.clone();
            cx.background_spawn(async move {
                let files = offline_disk::files_in(&store, &offline_id, &files_gone);
                offline_disk::delete_files(&files, &files_gone, &top)
            })
            .await;
            let _ = this.update_in(cx, |this, window, cx| {
                let showing = this.offline_active() && gone.iter().any(|l| this.view == MailboxView::Label(l.id.clone()));
                for l in &gone {
                    this.forget_deleted_folder(&offline, l, true, window, cx);
                }
                if showing {
                    this.leave_offline(window, cx);
                }
                this.record(offline.clone(), format!("Deleted offline folder “{name}”"), format!("With {}", messages_phrase(count)), overlook_store::Reversal::None, None, window, cx);
                this.sync_offline_files(cx);
                crate::ui::toast(window, cx, crate::notify::Level::Success, format!("Deleted “{name}”."));
            });
        })
        .detach();
    }

    /// Stops showing a top-level offline folder, after asking. Its directory, mail and
    /// state stay on disk; Open Offline Folder brings it back.
    pub(crate) fn disconnect_offline_folder(&mut self, label: Label, window: &mut Window, cx: &mut Context<Self>) {
        let offline = self.offline.id.clone();
        let labels = self.labels_of(&offline).to_vec();
        let gone = local::folder_and_inside(&label, &labels);
        let place = local::dir_of(&label).map(|d| shown_path(&d)).unwrap_or_default();
        let detail = format!("It stays on disk in {place} with all its mail. Open Offline Folder… brings it back, on this computer or another.");
        let answer = window.prompt(PromptLevel::Info, &format!("Disconnect “{}”?", label.name), Some(&detail), &["Disconnect", "Cancel"], cx);
        let store = self.store.clone();
        cx.spawn_in(window, async move |this, cx| {
            if answer.await != Ok(0) {
                return;
            }
            let (offline_id, root) = (offline.clone(), label.clone());
            let saved = cx.background_spawn(async move { offline_disk::save_before_disconnect(&store, &offline_id, &root) }).await;
            let _ = this.update_in(cx, |this, window, cx| {
                if let Err(e) = saved {
                    // Without its state saved, opening it again would lose read and starred marks.
                    crate::ui::toast(window, cx, crate::notify::Level::Error, format!("Couldn’t disconnect “{}”: {e}", label.name));
                    return;
                }
                let showing = this.offline_active() && gone.iter().any(|l| this.view == MailboxView::Label(l.id.clone()));
                // The rows go; the files are left alone.
                for l in &gone {
                    this.forget_deleted_folder(&offline, l, true, window, cx);
                }
                if showing {
                    this.leave_offline(window, cx);
                }
                crate::ui::toast(window, cx, crate::notify::Level::Success, format!("Disconnected “{}”. It’s still in {place}.", label.name));
            });
        })
        .detach();
    }

    /// Opens a folder from disk: one disconnected earlier, or any folder of `.eml` files.
    pub(crate) fn open_offline_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions { files: false, directories: true, multiple: false, prompt: Some("Open".into()) });
        let (store, offline) = (self.store.clone(), self.offline.clone());
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = receiver.await else { return };
            let Some(dir) = paths.into_iter().next() else { return };
            let _ = this.update_in(cx, |_, window, cx| crate::ui::toast(window, cx, crate::notify::Level::Info, format!("Opening {}…", shown_path(&dir))));
            let result = cx.background_spawn(async move { offline_disk::open_folder(&store, &offline, &dir) }).await;
            let _ = this.update_in(cx, |this, window, cx| match result {
                Ok(opened) => {
                    let offline = this.offline.id.clone();
                    this.reload_account_labels();
                    this.mark_counts_stale([offline.clone()]);
                    this.open_folder(offline, MailboxView::Label(opened.root.id.clone()), window, cx);
                    let skipped = if opened.skipped > 0 { format!(" {} file(s) couldn’t be read.", opened.skipped) } else { String::new() };
                    crate::ui::toast(window, cx, crate::notify::Level::Success, format!("Opened “{}” with {}.{skipped}", opened.root.name, messages_phrase(opened.messages)));
                }
                Err(e) => crate::ui::toast(window, cx, crate::notify::Level::Error, e),
            });
        })
        .detach();
    }
}
