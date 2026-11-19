//! Offline folders: mail moved off the server and kept only on this computer.
//!
//! The folders belong to one hidden account (`MailWindow::offline`, provider
//! [`ProviderKind::Local`]), so opening one shows it with the usual list, reader, search and
//! folder menus. Dropping mail of a server account onto an offline folder copies each
//! message's original bytes from the server to `raw/local_offline/` (written and flushed to
//! disk), files a parsed copy in the folder, and only then deletes the message from the
//! server: permanently where the server allows it, otherwise into its Trash. A message that
//! can't be downloaded completely stays on the server untouched.

use super::*;
use overlook_provider::local;
use overlook_provider::mime::{message_from_rfc822, MessageFlags};
use overlook_provider::{MailProvider, ProviderError};

/// How many messages are copied before the copied ones leave the server and the lists
/// update, so a large drag shows progress.
const BATCH: usize = 10;

/// Held while a saved original is written and its row stored, and while unreferenced files
/// are removed, so a cleanup never takes a file whose row is about to be written.
static OFFLINE_FILES: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// What copying one batch into an offline folder did.
#[derive(Default)]
struct Copied {
    /// Source messages now safely offline, ready to leave the server.
    copied: Vec<MessageId>,
    /// Source messages left where they are, and why.
    failed: Vec<(MessageId, String)>,
    /// Set when the server can't be reached: the rest of the drag is not attempted.
    stopped: Option<String>,
}

fn reason(e: &ProviderError) -> String {
    match e {
        ProviderError::Offline => "you’re offline".into(),
        ProviderError::SignInRequired => "the account needs you to sign in again".into(),
        ProviderError::RateLimited(_) => "the server asked Overlook to slow down".into(),
        other => other.to_string(),
    }
}

/// Copies `ids` of `source` into offline folder `target`. Runs off the UI thread.
async fn copy_to_offline(store: Arc<Store>, provider: Arc<dyn MailProvider>, source: Account, offline: Account, dir: PathBuf, target: LabelId, ids: Vec<MessageId>) -> Copied {
    let mut out = Copied::default();
    for (i, id) in ids.iter().enumerate() {
        let message = match store.message(id) {
            Ok(Some(m)) => m,
            // Deleted or moved meanwhile: nothing to copy, and nothing to delete.
            Ok(None) => continue,
            Err(e) => {
                out.failed.push((id.clone(), e.to_string()));
                continue;
            }
        };
        let raw = match provider.fetch_raw(&source, &message).await {
            Ok(raw) if !raw.is_empty() => raw,
            Ok(_) => {
                out.failed.push((id.clone(), "the server sent an empty message".into()));
                continue;
            }
            Err(e) => {
                let why = reason(&e);
                if matches!(e, ProviderError::Offline | ProviderError::SignInRequired | ProviderError::RateLimited(_)) {
                    out.failed.extend(ids[i..].iter().map(|id| (id.clone(), why.clone())));
                    out.stopped = Some(why);
                    break;
                }
                out.failed.push((id.clone(), why));
                continue;
            }
        };
        let copy_id = MessageId(format!("offline:{}", MessageId::generate().0));
        let flags = MessageFlags {
            provider_id: None,
            label_ids: [target.clone()].into_iter().collect(),
            is_read: message.is_read,
            is_starred: message.is_starred,
            received_at: Some(message.date),
        };
        let mut copy = message_from_rfc822(&offline, &raw, copy_id, &flags);
        // A provider that can't serve the original rebuilds it from the stored text, which
        // has no attachments. Such a copy is not complete enough to delete the server's.
        let files = |m: &Message| m.attachments.iter().filter(|a| !a.mime_type.to_ascii_lowercase().starts_with("text/calendar") && !a.filename.to_ascii_lowercase().ends_with(".ics")).count();
        if files(&message) > 0 && files(&copy) == 0 {
            out.failed.push((id.clone(), "the server didn’t send the attachments".into()));
            continue;
        }
        // Keep it with the rest of its conversation, and as the account showed it.
        copy.conversation_id = ConversationId(format!("offline:{}", message.conversation_id.0));
        copy.is_outgoing = message.is_outgoing;
        copy.category = message.category.clone();
        copy.is_important = message.is_important;
        if copy.date == 0 {
            copy.date = message.date;
        }
        let _files = OFFLINE_FILES.lock().unwrap_or_else(|e| e.into_inner());
        let saved = local::write_raw_durably(&dir, &copy.id, &raw).map_err(|e| format!("couldn’t save it on this computer: {e}")).and_then(|path| {
            store.upsert_messages(std::slice::from_ref(&copy)).map_err(|e| {
                let _ = std::fs::remove_file(&path);
                format!("couldn’t file it: {e}")
            })
        });
        match saved {
            Ok(()) => out.copied.push(id.clone()),
            Err(why) => out.failed.push((id.clone(), why)),
        }
    }
    out
}

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
        let Some(folder) = self.labels_of(&self.offline.id).iter().find(|l| l.id == target && l.kind == LabelKind::User).cloned() else { return };
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
        let (store, offline, dir) = (self.store.clone(), self.offline.clone(), providers::offline_dir());
        cx.spawn_in(window, async move |this, cx| {
            let mut moved = 0usize;
            let mut failed: Vec<(MessageId, String)> = Vec::new();
            for chunk in ids.chunks(BATCH) {
                let chunk = chunk.to_vec();
                let result = cx
                    .background_spawn(copy_to_offline(store.clone(), provider.clone(), source.clone(), offline.clone(), dir.clone(), folder.id.clone(), chunk.clone()))
                    .await;
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
                if moved > 0 && this.account.id == source.id {
                    this.select_after_removal(next.clone(), window, cx);
                }
                if moved > 0 {
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
    /// time, as on a server.
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

    /// Deletes an offline folder, the folders inside it and all their mail from this
    /// computer, after asking. There is no other copy, so it can't be undone.
    pub(crate) fn delete_offline_folder(&mut self, label: Label, window: &mut Window, cx: &mut Context<Self>) {
        let offline = self.offline.id.clone();
        let labels = self.labels_of(&offline).to_vec();
        let gone = local::folder_and_inside(&label, &labels);
        let count: usize = gone.iter().map(|l| self.store.message_ids_with_label(&offline, &l.id).map(|ids| ids.len()).unwrap_or(0)).sum();
        let name = Self::offline_folder_name(&label.name);
        let inside = if gone.len() > 1 { ", the folders inside it," } else { "" };
        let detail = format!(
            "The folder{inside} and {} in it are deleted from this computer. They are no longer on any server, so this can’t be undone.",
            if count == 1 { "the 1 message".to_owned() } else { format!("the {count} messages") }
        );
        let answer = window.prompt(PromptLevel::Warning, &format!("Delete the offline folder “{name}”?"), Some(&detail), &["Delete", "Cancel"], cx);
        cx.spawn_in(window, async move |this, cx| {
            if answer.await != Ok(0) {
                return;
            }
            let _ = this.update_in(cx, |this, window, cx| {
                let showing = this.offline_active() && gone.iter().any(|l| this.view == MailboxView::Label(l.id.clone()));
                for l in &gone {
                    this.forget_deleted_folder(&offline, l, true, window, cx);
                }
                if showing {
                    this.leave_offline(window, cx);
                }
                this.record(offline.clone(), format!("Deleted offline folder “{name}”"), format!("With {}", messages_phrase(count)), overlook_store::Reversal::None, None, window, cx);
                this.clean_offline_files(cx);
                crate::ui::toast(window, cx, crate::notify::Level::Success, format!("Deleted “{name}”."));
            });
        })
        .detach();
    }

    /// Removes saved originals whose messages are gone (deleted from an offline folder).
    pub(crate) fn clean_offline_files(&self, cx: &mut Context<Self>) {
        let (store, offline) = (self.store.clone(), self.offline.id.clone());
        cx.background_spawn(async move {
            let _files = OFFLINE_FILES.lock().unwrap_or_else(|e| e.into_inner());
            match store.message_ids_of(&offline) {
                Ok(keep) => {
                    let removed = local::remove_unreferenced(&providers::offline_dir(), &keep, std::time::Duration::ZERO);
                    if removed > 0 {
                        tracing::info!("offline folders: removed {removed} saved message(s) no longer in any folder");
                    }
                }
                Err(e) => tracing::warn!("offline folders: could not list messages for cleanup: {e}"),
            }
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::{copy_to_offline, local};
    use overlook_core::*;
    use overlook_provider::sample::SampleProvider;
    use overlook_provider::MailProvider;
    use overlook_store::Store;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("overlook-offline-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn copies_keep_the_original_and_refuse_incomplete_downloads() {
        let store = Arc::new(Store::open_in_memory().unwrap());
        let source = crate::seed_sample_account(&store).unwrap();
        let offline = store.offline_account().unwrap();
        let folder = local::new_folder(&offline.id, "Archive 2025", None, &[]).unwrap();
        store.upsert_label(&folder).unwrap();
        let all: Vec<Message> = store.message_ids_of(&source.id).unwrap().into_iter().filter_map(|id| store.message(&id).unwrap()).collect();
        let plain = all.iter().find(|m| m.attachments.is_empty() && m.body.is_cached()).expect("a sample message without attachments").clone();
        let with_files = all.iter().find(|m| !m.attachments.is_empty()).expect("a sample message with attachments").clone();
        let dir = temp_dir("copy");
        let provider: Arc<dyn MailProvider> = Arc::new(SampleProvider::new());

        // The sample provider rebuilds messages from stored text, without attachments: one
        // that has files must stay on the server.
        let out = crate::sync::pollster_block(copy_to_offline(store.clone(), provider, source.clone(), offline.clone(), dir.clone(), folder.id.clone(), vec![plain.id.clone(), with_files.id.clone()]));
        assert_eq!(out.copied, vec![plain.id.clone()]);
        assert_eq!(out.failed.len(), 1);
        assert_eq!(out.failed[0].0, with_files.id);
        assert!(out.stopped.is_none());

        let copies: Vec<Message> = store.message_ids_of(&offline.id).unwrap().into_iter().filter_map(|id| store.message(&id).unwrap()).collect();
        assert_eq!(copies.len(), 1);
        let copy = &copies[0];
        assert_eq!(copy.subject, plain.subject);
        assert_eq!(copy.is_read, plain.is_read);
        assert!(copy.label_ids.contains(&folder.id) && copy.label_ids.len() == 1, "filed in the folder only");
        assert_eq!(copy.conversation_id.0, format!("offline:{}", plain.conversation_id.0));
        assert!(local::raw_path_in(&dir, &copy.id).exists(), "the original is saved beside the row");
        // The source is untouched: deleting it from the server is the caller's next step.
        assert!(store.message(&plain.id).unwrap().is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
