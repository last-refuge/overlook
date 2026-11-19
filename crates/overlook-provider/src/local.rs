//! Offline folders: mail kept on this device only.
//!
//! An offline folder is a label of the offline account ([`Account::offline`]). Each message
//! in one is the original message as downloaded from its server, saved as `<id>.eml` in the
//! provider's directory, plus the usual parsed row in the store so lists, search and the
//! reader work as for any other mail. Attachments and "View source" are served from the
//! saved file. Nothing here talks to a network, and the sync engine never runs for this
//! account: changes are complete once the store has them.
//!
//! Folders nest by "/" in their names, as Gmail labels do. Their ids never change, so a
//! rename or move only rewrites names.

use crate::*;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Where offline mail lives and how to read it back.
pub struct LocalProvider {
    dir: PathBuf,
}

impl LocalProvider {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The saved original of a message.
    pub fn raw_path(&self, id: &MessageId) -> PathBuf {
        raw_path_in(&self.dir, id)
    }

    fn raw(&self, message: &Message) -> Result<Vec<u8>, ProviderError> {
        let path = self.raw_path(&message.id);
        std::fs::read(&path).map_err(|e| ProviderError::Rejected(format!("the saved copy of this message is missing ({}): {e}", path.display())))
    }
}

/// The file that holds message `id` in `dir`.
pub fn raw_path_in(dir: &Path, id: &MessageId) -> PathBuf {
    dir.join(format!("{}.eml", file_stem(id)))
}

/// The file name (without `.eml`) used for message `id`.
pub fn file_stem(id: &MessageId) -> String {
    id.0.replace([':', '/', '\\'], "_")
}

/// Writes `bytes` as message `id`'s saved original and makes sure it is on disk before
/// returning: the copy on the server is deleted once this succeeds, so a crash or power
/// loss right after must not lose it. Written to a temporary file, flushed, then renamed.
pub fn write_raw_durably(dir: &Path, id: &MessageId, bytes: &[u8]) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = raw_path_in(dir, id);
    let tmp = path.with_extension("eml.part");
    {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, &path)?;
    // The new name must reach the disk too, or a power loss can leave the directory without
    // it. A failure here is a failed save: the server's copy is then kept.
    #[cfg(unix)]
    if let Err(e) = std::fs::File::open(dir).and_then(|d| d.sync_all()) {
        let _ = std::fs::remove_file(&path);
        return Err(e);
    }
    Ok(path)
}

/// Removes saved originals in `dir` that no message refers to any more (`keep` holds the
/// ids of every offline message). Files touched in the last `grace` are left alone, so a
/// message being saved right now, whose row is not written yet, is not taken. Returns how
/// many files were removed.
pub fn remove_unreferenced(dir: &Path, keep: &std::collections::HashSet<MessageId>, grace: std::time::Duration) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else { return 0 };
    let keep: std::collections::HashSet<String> = keep.iter().map(file_stem).collect();
    let now = std::time::SystemTime::now();
    let mut removed = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        let stem = match name.strip_suffix(".eml").or_else(|| name.strip_suffix(".eml.part")) {
            Some(stem) => stem.to_owned(),
            None => continue,
        };
        if keep.contains(&stem) {
            continue;
        }
        let recent = entry.metadata().and_then(|m| m.modified()).ok().and_then(|t| now.duration_since(t).ok()).is_none_or(|age| age < grace);
        if recent {
            continue;
        }
        if std::fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// A new offline folder's label.
pub fn new_folder(account: &AccountId, name: &str, parent: Option<&Label>, labels: &[Label]) -> Result<Label, ProviderError> {
    if let Some(problem) = folder_name_problem(name, "/") {
        return Err(ProviderError::Rejected(problem));
    }
    let full = match parent {
        Some(p) => format!("{}/{}", p.name, name.trim()),
        None => name.trim().to_owned(),
    };
    if let Some(problem) = reserved(&full) {
        return Err(ProviderError::Rejected(problem));
    }
    if name_taken(labels, None, &full) {
        return Err(ProviderError::Rejected(format!("There is already an offline folder called “{full}”.")));
    }
    Ok(Label {
        id: LabelId(format!("offline-folder:{}", LabelId::generate().0)),
        account_id: account.clone(),
        kind: LabelKind::User,
        name: full,
        provider_id: None,
        color: None,
        sort_order: labels.len() as i32,
    })
}

/// The folders that change when `label` is renamed to `full`: it, then those inside it.
fn renamed(label: &Label, full: &str, labels: &[Label]) -> Result<Vec<RenamedFolder>, ProviderError> {
    if full == label.name {
        return Ok(Vec::new());
    }
    if let Some(problem) = reserved(full) {
        return Err(ProviderError::Rejected(problem));
    }
    if name_taken(labels, Some(&label.id), full) {
        return Err(ProviderError::Rejected(format!("There is already an offline folder called “{full}”.")));
    }
    let mut out = vec![RenamedFolder { old: label.clone(), new: Label { name: full.to_owned(), ..label.clone() } }];
    let prefix = format!("{}/", label.name);
    for inside in labels.iter().filter(|l| l.id != label.id && l.name.starts_with(&prefix)) {
        let name = format!("{full}/{}", &inside.name[prefix.len()..]);
        out.push(RenamedFolder { old: inside.clone(), new: Label { name, ..inside.clone() } });
    }
    Ok(out)
}

/// Renames the last part of `label`'s name.
pub fn rename_folder(label: &Label, name: &str, labels: &[Label]) -> Result<Vec<RenamedFolder>, ProviderError> {
    if let Some(problem) = folder_name_problem(name, "/") {
        return Err(ProviderError::Rejected(problem));
    }
    let full = match label.name.rsplit_once('/') {
        Some((parent, _)) => format!("{parent}/{}", name.trim()),
        None => name.trim().to_owned(),
    };
    renamed(label, &full, labels)
}

/// Moves `label` (and what is inside it) into `parent`, or to the top level.
pub fn move_folder(label: &Label, parent: Option<&Label>, labels: &[Label]) -> Result<Vec<RenamedFolder>, ProviderError> {
    let leaf = label.name.rsplit('/').next().unwrap_or(&label.name);
    if let Some(p) = parent {
        if p.id == label.id || p.name.starts_with(&format!("{}/", label.name)) {
            return Err(ProviderError::Rejected("A folder can’t go inside itself.".into()));
        }
    }
    let full = match parent {
        Some(p) => format!("{}/{leaf}", p.name),
        None => leaf.to_owned(),
    };
    renamed(label, &full, labels)
}

/// Every folder at or inside `label`, innermost first, for deleting.
pub fn folder_and_inside(label: &Label, labels: &[Label]) -> Vec<Label> {
    let prefix = format!("{}/", label.name);
    let mut out: Vec<Label> = labels.iter().filter(|l| l.id == label.id || l.name.starts_with(&prefix)).cloned().collect();
    out.sort_by_key(|l| std::cmp::Reverse(l.name.len()));
    out
}

/// The sidebar files folders named "Inbox…" under an Inbox; offline folders have none.
fn reserved(full: &str) -> Option<String> {
    let top = full.split('/').next().unwrap_or(full);
    overlook_core::sidebar::is_inbox(top.trim()).then(|| "“Inbox” can’t be the name of an offline folder.".to_owned())
}

fn name_taken(labels: &[Label], except: Option<&LabelId>, full: &str) -> bool {
    labels.iter().any(|l| Some(&l.id) != except && l.name.eq_ignore_ascii_case(full))
}

impl MailProvider for LocalProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Local
    }

    fn probe(&self) -> BoxFuture<'_, Result<(), ProviderError>> {
        Box::pin(async { Ok(()) })
    }

    /// Never called for real: the sync engine skips offline folders. Empty, never a list
    /// that could be read as "the server deleted everything".
    fn fetch_snapshot(&self, _account: &Account) -> BoxFuture<'_, Result<MailboxSnapshot, ProviderError>> {
        Box::pin(async { Err(ProviderError::Rejected("offline folders are not synchronized".into())) })
    }

    fn fetch_changes(&self, _account: &Account, _checkpoint: &str) -> BoxFuture<'_, Result<RemoteChanges, ProviderError>> {
        Box::pin(async { Err(ProviderError::Rejected("offline folders are not synchronized".into())) })
    }

    /// Changes are complete in the store; there is no server to tell.
    fn apply(&self, _account: &Account, _op: &PendingOperation, _messages: &[Message], _labels: &[Label]) -> BoxFuture<'_, Result<AppliedChanges, ProviderError>> {
        Box::pin(async { Ok(AppliedChanges::default()) })
    }

    fn fetch_raw(&self, _account: &Account, message: &Message) -> BoxFuture<'_, Result<Vec<u8>, ProviderError>> {
        let result = self.raw(message);
        Box::pin(async move { result })
    }

    fn fetch_original(&self, _account: &Account, message: &Message) -> BoxFuture<'_, Result<Vec<u8>, ProviderError>> {
        let result = self.raw(message);
        Box::pin(async move { result })
    }

    fn fetch_attachment(&self, _account: &Account, message: &Message, attachment: &Attachment) -> BoxFuture<'_, Result<Vec<u8>, ProviderError>> {
        let result = self.raw(message).and_then(|bytes| {
            let section = attachment.provider_id.as_ref().map(|p| p.0.clone()).ok_or_else(|| ProviderError::Rejected("this attachment has no part number".into()))?;
            crate::mime::part_bytes(&bytes, &section).ok_or_else(|| ProviderError::Rejected(format!("the saved message has no part {section}")))
        });
        Box::pin(async move { result })
    }

    fn manages_folders(&self) -> bool {
        true
    }

    fn supports_delete_forever(&self) -> bool {
        true
    }

    fn create_folder(&self, account: &Account, name: &str, parent: Option<&Label>, labels: &[Label]) -> BoxFuture<'_, Result<Label, ProviderError>> {
        let result = new_folder(&account.id, name, parent, labels);
        Box::pin(async move { result })
    }

    fn rename_folder(&self, _account: &Account, label: &Label, name: &str, labels: &[Label]) -> BoxFuture<'_, Result<Vec<RenamedFolder>, ProviderError>> {
        let result = rename_folder(label, name, labels);
        Box::pin(async move { result })
    }

    fn move_folder(&self, _account: &Account, label: &Label, parent: Option<&Label>, labels: &[Label]) -> BoxFuture<'_, Result<Vec<RenamedFolder>, ProviderError>> {
        let result = move_folder(label, parent, labels);
        Box::pin(async move { result })
    }

    /// The store removes the folder and its mail; the saved files go with the next cleanup.
    fn delete_folder(&self, _account: &Account, _label: &Label) -> BoxFuture<'_, Result<(), ProviderError>> {
        Box::pin(async { Ok(()) })
    }

    fn send(&self, _account: &Account, _draft: &Draft, _context: SendContext) -> BoxFuture<'_, Result<SentReceipt, ProviderError>> {
        Box::pin(async { Err(ProviderError::Rejected("offline folders can’t send mail; send from one of your accounts".into())) })
    }

    fn reconcile_send(&self, _account: &Account, _draft: &Draft) -> BoxFuture<'_, Result<SendOutcome, ProviderError>> {
        Box::pin(async { Ok(SendOutcome::NotDelivered) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(id: &str, name: &str) -> Label {
        Label { id: LabelId::from(id), account_id: AccountId::from(OFFLINE_ACCOUNT_ID), kind: LabelKind::User, name: name.into(), provider_id: None, color: None, sort_order: 0 }
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("overlook-local-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn folders_nest_by_name_and_refuse_duplicates() {
        let account = AccountId::from(OFFLINE_ACCOUNT_ID);
        let taxes = new_folder(&account, "Taxes", None, &[]).unwrap();
        assert_eq!(taxes.name, "Taxes");
        assert_eq!(taxes.kind, LabelKind::User);
        assert!(taxes.id.0.starts_with("offline-folder:"));
        let y2025 = new_folder(&account, " 2025 ", Some(&taxes), std::slice::from_ref(&taxes)).unwrap();
        assert_eq!(y2025.name, "Taxes/2025");
        assert!(new_folder(&account, "taxes", None, &[taxes.clone(), y2025]).is_err(), "names differing only in case clash");
        assert!(new_folder(&account, "a/b", None, &[]).is_err(), "the delimiter is not allowed in one level");
        assert!(new_folder(&account, "  ", None, &[]).is_err());
        assert!(new_folder(&account, "INBOX", None, &[]).is_err(), "the tree would file it as an Inbox");
    }

    #[test]
    fn renaming_and_moving_carry_the_folders_inside() {
        let labels = vec![folder("a", "Work"), folder("b", "Work/Clients"), folder("c", "Work/Clients/Acme"), folder("d", "Workshop"), folder("e", "Home")];
        let changed = rename_folder(&labels[0], "Jobs", &labels).unwrap();
        let names: Vec<(&str, &str)> = changed.iter().map(|c| (c.old.id.0.as_str(), c.new.name.as_str())).collect();
        assert_eq!(names, vec![("a", "Jobs"), ("b", "Jobs/Clients"), ("c", "Jobs/Clients/Acme")], "Workshop only shares a prefix");
        assert!(changed.iter().all(|c| c.old.id == c.new.id), "ids never change");

        let moved = move_folder(&labels[1], Some(&labels[4]), &labels).unwrap();
        let names: Vec<&str> = moved.iter().map(|c| c.new.name.as_str()).collect();
        assert_eq!(names, vec!["Home/Clients", "Home/Clients/Acme"]);
        let top = move_folder(&labels[2], None, &labels).unwrap();
        assert_eq!(top[0].new.name, "Acme");
        assert!(move_folder(&labels[0], Some(&labels[2]), &labels).is_err(), "not into itself");
        assert!(rename_folder(&labels[0], "home", &labels).is_err(), "not onto another folder's name");
        assert!(rename_folder(&labels[0], "Work", &labels).unwrap().is_empty());
        let gone = folder_and_inside(&labels[0], &labels);
        let gone: Vec<&str> = gone.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(gone, vec!["Work/Clients/Acme", "Work/Clients", "Work"], "innermost first");
    }

    #[test]
    fn saved_originals_serve_raw_and_attachments_and_unreferenced_ones_are_cleaned_up() {
        let dir = temp_dir("raw");
        let id = MessageId::from("offline:1");
        let raw = b"From: a@example.com\r\nTo: b@example.com\r\nSubject: Report\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=X\r\n\r\n--X\r\nContent-Type: text/plain\r\n\r\nSee attached.\r\n--X\r\nContent-Type: application/pdf; name=r.pdf\r\nContent-Disposition: attachment; filename=r.pdf\r\nContent-Transfer-Encoding: base64\r\n\r\nJVBERi0=\r\n--X--\r\n";
        write_raw_durably(&dir, &id, raw).unwrap();
        let account = Account::offline();
        let message = crate::mime::message_from_rfc822(&account, raw, id.clone(), &crate::mime::MessageFlags::default());
        assert_eq!(message.attachments.len(), 1);
        let provider = LocalProvider::new(dir.clone());
        assert_eq!(futures_lite_block(provider.fetch_raw(&account, &message)).unwrap(), raw.to_vec());
        assert_eq!(futures_lite_block(provider.fetch_attachment(&account, &message, &message.attachments[0])).unwrap(), b"%PDF-".to_vec());

        let other = MessageId::from("offline:2");
        write_raw_durably(&dir, &other, b"Subject: x\r\n\r\n").unwrap();
        let keep: std::collections::HashSet<MessageId> = [id.clone()].into_iter().collect();
        assert_eq!(remove_unreferenced(&dir, &keep, std::time::Duration::from_secs(600)), 0, "a fresh file may still be getting its row");
        assert_eq!(remove_unreferenced(&dir, &keep, std::time::Duration::ZERO), 1);
        assert!(raw_path_in(&dir, &id).exists());
        assert!(!raw_path_in(&dir, &other).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Every future here is ready at once.
    fn futures_lite_block<T>(mut f: BoxFuture<'_, T>) -> T {
        let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
        match f.as_mut().poll(&mut cx) {
            std::task::Poll::Ready(v) => v,
            std::task::Poll::Pending => panic!("offline folders never wait"),
        }
    }
}
