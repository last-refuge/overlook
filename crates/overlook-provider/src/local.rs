//! Offline folders: mail kept on this computer, in ordinary folders on disk.
//!
//! An offline folder is a directory the person chose. Its subfolders are subdirectories,
//! and each message in it is the original message as downloaded from its server, saved as
//! a standalone `.eml` file any mail program can open. A hidden manifest
//! ([`MANIFEST`]) beside them remembers what Overlook knows that the files don't: each
//! message's id, conversation, and read and starred state. A folder can therefore be
//! disconnected (forgotten by Overlook, left on disk) and opened again later, here or on
//! another computer, without losing anything.
//!
//! In the store every offline folder is a label of the offline account
//! ([`Account::offline`]) whose `provider_id` is its directory, and every message's
//! `provider_id` is its file. The parsed rows make lists, search and the reader work as
//! for any other mail; attachments and "View source" come from the file. Nothing here
//! talks to a network, and the sync engine never runs for this account.
//!
//! Folders nest by "/" in their names, as Gmail labels do. Their ids never change, so a
//! rename or move rewrites names and paths only.

use crate::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

/// The hidden file in an offline folder's directory that keeps Overlook's state.
pub const MANIFEST: &str = ".overlook-offline.json";

/// Serves offline messages from their files.
pub struct LocalProvider {
    /// Where messages saved by the first version of offline folders live, until they are
    /// moved into their folders' directories.
    legacy_dir: PathBuf,
}

impl LocalProvider {
    pub fn new(legacy_dir: PathBuf) -> Self {
        Self { legacy_dir }
    }

    /// The file holding `message`.
    pub fn file_of(&self, message: &Message) -> PathBuf {
        match &message.provider_id {
            Some(p) if Path::new(&p.0).is_absolute() => PathBuf::from(&p.0),
            _ => raw_path_in(&self.legacy_dir, &message.id),
        }
    }
}

/// Where the first version of offline folders saved message `id` in `dir`.
pub fn raw_path_in(dir: &Path, id: &MessageId) -> PathBuf {
    dir.join(format!("{}.eml", id.0.replace([':', '/', '\\'], "_")))
}

// ------------------------------------------------------------------------ files

/// A readable file name for a message: `2025-03-01 1432 Quarterly report [1a2b3c4d].eml`.
/// The bracketed part keeps names apart; it comes from the message id.
pub fn file_name(message: &Message) -> String {
    let when = jiff::Timestamp::from_millisecond(message.date).ok().map(|t| t.to_zoned(jiff::tz::TimeZone::UTC).strftime("%Y-%m-%d %H%M").to_string()).unwrap_or_else(|| "0000-00-00 0000".into());
    let subject: String = message.subject.chars().map(|c| if c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') { ' ' } else { c }).collect();
    let subject = subject.split_whitespace().collect::<Vec<_>>().join(" ");
    let subject = subject.trim_start_matches('.').trim();
    // File names are limited to 255 bytes, not characters: 80 characters of Chinese are
    // 240 bytes. Keep the subject within 150 bytes so the date, tag and a " (2)" still fit.
    let subject: String = if subject.is_empty() {
        "No subject".into()
    } else {
        let mut out = String::new();
        for c in subject.chars().take(80) {
            if out.len() + c.len_utf8() > 150 {
                break;
            }
            out.push(c);
        }
        out.trim_end().to_owned()
    };
    let own = message.id.0.rsplit(':').next().unwrap_or(&message.id.0);
    let tag: String = own.chars().rev().filter(|c| c.is_ascii_alphanumeric()).take(8).collect::<Vec<_>>().into_iter().rev().collect();
    format!("{when} {subject} [{tag}].eml")
}

/// `name` in `dir`, or `name (2)`, `name (3)`… when it is taken.
pub fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let candidate = dir.join(name);
    if !candidate.exists() {
        return candidate;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_owned(), format!(".{e}")),
        _ => (name.to_owned(), String::new()),
    };
    (2..).map(|n| dir.join(format!("{stem} ({n}){ext}"))).find(|p| !p.exists()).expect("some name is free")
}

#[cfg(unix)]
fn sync_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::File::open(dir)?.sync_all()
}

#[cfg(not(unix))]
fn sync_dir(_dir: &Path) -> std::io::Result<()> {
    Ok(())
}

/// Writes `bytes` to a new file named after `name` in `dir` and makes sure it is on disk
/// before returning: the copy on the server is deleted once this succeeds, so a crash or
/// power loss right after must not lose it. Written to a temporary file, flushed, renamed,
/// and the directory flushed; any failure removes the file and is an error.
pub fn write_durably(dir: &Path, name: &str, bytes: &[u8]) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = unique_path(dir, name);
    let tmp = dir.join(format!(".{}.part", path.file_name().and_then(|n| n.to_str()).unwrap_or("message")));
    let written = (|| {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()
    })();
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    if let Err(e) = std::fs::rename(&tmp, &path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    // The new name must reach the disk too, or a power loss can leave the directory without it.
    if let Err(e) = sync_dir(dir) {
        let _ = std::fs::remove_file(&path);
        return Err(e);
    }
    Ok(path)
}

/// Moves a message file into `dir`, keeping its name unless taken. Between disks (where a
/// rename can't work) it is copied, flushed, and only then removed from where it was.
pub fn move_file(from: &Path, dir: &Path) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let name = from.file_name().and_then(|n| n.to_str()).unwrap_or("message.eml");
    let to = unique_path(dir, name);
    if std::fs::rename(from, &to).is_ok() {
        sync_dir(dir)?;
        return Ok(to);
    }
    let bytes = std::fs::read(from)?;
    let to = write_durably(dir, name, &bytes)?;
    std::fs::remove_file(from)?;
    Ok(to)
}

/// Removes `dir` and the empty directories above it, up to (not including) `stop`. A
/// directory with anything left in it, such as files the person put there, stays.
pub fn remove_empty_dirs(dir: &Path, stop: &Path) {
    let mut current = Some(dir.to_path_buf());
    while let Some(d) = current {
        if d == stop || !d.starts_with(stop) || std::fs::remove_dir(&d).is_err() {
            break;
        }
        current = d.parent().map(Path::to_path_buf);
    }
}

// --------------------------------------------------------------------- manifest

/// What Overlook keeps beside an offline folder's files.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub version: u32,
    /// Each message by its path inside the folder, with "/" between parts.
    #[serde(default)]
    pub messages: BTreeMap<String, Kept>,
}

/// One message's state.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Kept {
    pub id: String,
    #[serde(default)]
    pub conversation: String,
    #[serde(default)]
    pub read: bool,
    #[serde(default)]
    pub starred: bool,
}

/// The manifest of the folder at `root`; empty when there is none or it can't be read.
pub fn read_manifest(root: &Path) -> Manifest {
    std::fs::read(root.join(MANIFEST)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

/// Replaces the manifest of the folder at `root`.
pub fn write_manifest(root: &Path, manifest: &Manifest) -> std::io::Result<()> {
    let bytes = serde_json::to_vec_pretty(manifest).map_err(std::io::Error::other)?;
    let tmp = root.join(format!("{MANIFEST}.{}.part", MessageId::generate().0));
    let written = (|| {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(&bytes)?;
        file.sync_all()
    })();
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    if let Err(e) = std::fs::rename(&tmp, root.join(MANIFEST)) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    sync_dir(root)
}

/// `path` inside `root`, with "/" between parts, as the manifest names it.
pub fn relative(root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    let parts: Vec<String> = rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
    (!parts.is_empty()).then(|| parts.join("/"))
}

// ------------------------------------------------------------------------- scan

/// What an offline folder's directory holds.
#[derive(Debug, Default)]
pub struct Scanned {
    /// Every subdirectory, as "/"-joined paths inside the folder, parents first.
    pub folders: Vec<String>,
    /// Every `.eml` file, with the subdirectory it is in ("" for the folder itself).
    pub files: Vec<(String, PathBuf)>,
}

/// Lists the folders and `.eml` files under `root`. Hidden files and directories (the
/// manifest, unfinished writes) are skipped, and so are symbolic links.
pub fn scan(root: &Path) -> std::io::Result<Scanned> {
    let mut out = Scanned::default();
    let mut pending = vec![(root.to_path_buf(), String::new())];
    while let Some((dir, rel)) = pending.pop() {
        let mut entries: Vec<std::fs::DirEntry> = std::fs::read_dir(&dir)?.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let Ok(kind) = entry.file_type() else { continue };
            if kind.is_dir() {
                let sub = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
                out.folders.push(sub.clone());
                pending.push((entry.path(), sub));
            } else if kind.is_file() && name.to_ascii_lowercase().ends_with(".eml") {
                out.files.push((rel.clone(), entry.path()));
            }
        }
    }
    out.folders.sort_by_key(|f| f.matches('/').count());
    Ok(out)
}

// ---------------------------------------------------------------------- folders

/// A new offline folder's label, inside `parent` or at the top level. The caller sets
/// `provider_id` to its directory.
pub fn new_folder(account: &AccountId, name: &str, parent: Option<&Label>, labels: &[Label]) -> Result<Label, ProviderError> {
    if let Some(problem) = folder_name_problem(name, "/") {
        return Err(ProviderError::Rejected(problem));
    }
    if name.trim().starts_with('.') {
        return Err(ProviderError::Rejected("A folder name can’t start with a dot: the folder would be hidden on disk.".into()));
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

/// The directory of an offline folder's label.
pub fn dir_of(label: &Label) -> Option<PathBuf> {
    label.provider_id.as_ref().map(|p| PathBuf::from(&p.0))
}

/// The folders that change when `label` is renamed to `full` and its directory becomes
/// `dir`: it, then those inside it, with their names and directories.
fn renamed(label: &Label, full: &str, dir: Option<&Path>, labels: &[Label]) -> Result<Vec<RenamedFolder>, ProviderError> {
    if full == label.name {
        return Ok(Vec::new());
    }
    if let Some(problem) = reserved(full) {
        return Err(ProviderError::Rejected(problem));
    }
    if name_taken(labels, Some(&label.id), full) {
        return Err(ProviderError::Rejected(format!("There is already an offline folder called “{full}”.")));
    }
    let old_dir = dir_of(label);
    let moved = |l: &Label| -> Option<ProviderId> {
        let (old, new) = (old_dir.as_ref()?, dir?);
        let inside = dir_of(l)?;
        let rest = inside.strip_prefix(old).ok()?;
        let moved = if rest.as_os_str().is_empty() { new.to_path_buf() } else { new.join(rest) };
        Some(ProviderId(moved.to_string_lossy().into_owned()))
    };
    let mut out = vec![RenamedFolder { old: label.clone(), new: Label { name: full.to_owned(), provider_id: moved(label).or_else(|| label.provider_id.clone()), ..label.clone() } }];
    let prefix = format!("{}/", label.name);
    for inside in labels.iter().filter(|l| l.id != label.id && l.name.starts_with(&prefix)) {
        let name = format!("{full}/{}", &inside.name[prefix.len()..]);
        out.push(RenamedFolder { old: inside.clone(), new: Label { name, provider_id: moved(inside).or_else(|| inside.provider_id.clone()), ..inside.clone() } });
    }
    Ok(out)
}

/// Renames the last part of `label`'s name; its directory, when it has one, is renamed to
/// match in the same place.
pub fn rename_folder(label: &Label, name: &str, labels: &[Label]) -> Result<Vec<RenamedFolder>, ProviderError> {
    if let Some(problem) = folder_name_problem(name, "/") {
        return Err(ProviderError::Rejected(problem));
    }
    if name.trim().starts_with('.') {
        return Err(ProviderError::Rejected("A folder name can’t start with a dot: the folder would be hidden on disk.".into()));
    }
    let full = match label.name.rsplit_once('/') {
        Some((parent, _)) => format!("{parent}/{}", name.trim()),
        None => name.trim().to_owned(),
    };
    let dir = dir_of(label).and_then(|d| d.parent().map(|p| p.join(name.trim())));
    renamed(label, &full, dir.as_deref(), labels)
}

/// Moves `label` (and what is inside it) into `parent`, or to the top level. Its directory
/// goes inside the parent's.
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
    let dir = parent.and_then(dir_of).map(|p| p.join(leaf));
    renamed(label, &full, dir.as_deref(), labels)
}

/// Every folder at or inside `label`, innermost first, for deleting.
pub fn folder_and_inside(label: &Label, labels: &[Label]) -> Vec<Label> {
    let prefix = format!("{}/", label.name);
    let mut out: Vec<Label> = labels.iter().filter(|l| l.id == label.id || l.name.starts_with(&prefix)).cloned().collect();
    out.sort_by_key(|l| std::cmp::Reverse(l.name.len()));
    out
}

/// The top-level folder `label` is in (itself, at the top).
pub fn root_of<'a>(label: &Label, labels: &'a [Label]) -> Option<&'a Label> {
    let top = label.name.split('/').next().unwrap_or(&label.name);
    labels.iter().find(|l| l.name == top)
}

/// A top-level name for a folder opened from disk: its directory's name, made unique.
pub fn opened_name(dir: &Path, labels: &[Label]) -> String {
    let base = dir.file_name().map(|n| n.to_string_lossy().into_owned()).filter(|n| !n.trim().is_empty()).unwrap_or_else(|| "Offline Folder".into());
    let base = base.replace('/', " ");
    let base = if reserved(&base).is_some() { format!("{base} (offline)") } else { base };
    if !name_taken(labels, None, &base) {
        return base;
    }
    (2..).map(|n| format!("{base} ({n})")).find(|n| !name_taken(labels, None, n)).expect("some name is free")
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

    /// Never called for real: the sync engine skips offline folders. An error, never a
    /// list that could be read as "the server deleted everything".
    fn fetch_snapshot(&self, _account: &Account) -> BoxFuture<'_, Result<MailboxSnapshot, ProviderError>> {
        Box::pin(async { Err(ProviderError::Rejected("offline folders are not synchronized".into())) })
    }

    fn fetch_changes(&self, _account: &Account, _checkpoint: &str) -> BoxFuture<'_, Result<RemoteChanges, ProviderError>> {
        Box::pin(async { Err(ProviderError::Rejected("offline folders are not synchronized".into())) })
    }

    /// Changes are complete in the store; the window moves files to match.
    fn apply(&self, _account: &Account, _op: &PendingOperation, _messages: &[Message], _labels: &[Label]) -> BoxFuture<'_, Result<AppliedChanges, ProviderError>> {
        Box::pin(async { Ok(AppliedChanges::default()) })
    }

    fn fetch_raw(&self, _account: &Account, message: &Message) -> BoxFuture<'_, Result<Vec<u8>, ProviderError>> {
        let path = self.file_of(message);
        Box::pin(async move {
            std::fs::read(&path).map_err(|e| ProviderError::Rejected(format!("the message’s file can’t be read ({}): {e}", path.display())))
        })
    }

    fn fetch_original(&self, _account: &Account, message: &Message) -> BoxFuture<'_, Result<Vec<u8>, ProviderError>> {
        let path = self.file_of(message);
        Box::pin(async move {
            std::fs::read(&path).map_err(|e| ProviderError::Rejected(format!("the message’s file can’t be read ({}): {e}", path.display())))
        })
    }

    fn fetch_attachment(&self, _account: &Account, message: &Message, attachment: &Attachment) -> BoxFuture<'_, Result<Vec<u8>, ProviderError>> {
        let path = self.file_of(message);
        let section = attachment.provider_id.as_ref().map(|p| p.0.clone());
        Box::pin(async move {
            let bytes = std::fs::read(&path).map_err(|e| ProviderError::Rejected(format!("the message’s file can’t be read ({}): {e}", path.display())))?;
            let section = section.ok_or_else(|| ProviderError::Rejected("this attachment has no part number".into()))?;
            crate::mime::part_bytes(&bytes, &section).ok_or_else(|| ProviderError::Rejected(format!("the saved message has no part {section}")))
        })
    }

    /// Folders are made, renamed and deleted by the window, on disk; this only says the
    /// sidebar should offer it.
    fn manages_folders(&self) -> bool {
        true
    }

    fn supports_delete_forever(&self) -> bool {
        true
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

    fn folder(id: &str, name: &str, dir: Option<&str>) -> Label {
        Label { id: LabelId::from(id), account_id: AccountId::from(OFFLINE_ACCOUNT_ID), kind: LabelKind::User, name: name.into(), provider_id: dir.map(ProviderId::from), color: None, sort_order: 0 }
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("overlook-local-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn message(id: &str, subject: &str, date: i64) -> Message {
        let mut m = crate::mime::message_from_rfc822(&Account::offline(), b"Subject: x\r\n\r\nbody", MessageId::from(id), &crate::mime::MessageFlags::default());
        m.subject = subject.into();
        m.date = date;
        m
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
        assert!(new_folder(&account, ".hidden", None, &[]).is_err(), "a dot folder is hidden on disk");
    }

    #[test]
    fn renaming_and_moving_carry_names_and_directories_inside() {
        let labels = vec![
            folder("a", "Work", Some("/m/Work")),
            folder("b", "Work/Clients", Some("/m/Work/Clients")),
            folder("c", "Work/Clients/Acme", Some("/m/Work/Clients/Acme")),
            folder("d", "Workshop", Some("/x/Workshop")),
            folder("e", "Work/Home", Some("/m/Work/Home")),
        ];
        let changed = rename_folder(&labels[0], "Jobs", &labels).unwrap();
        let got: Vec<(&str, &str, &str)> = changed.iter().map(|c| (c.old.id.0.as_str(), c.new.name.as_str(), c.new.provider_id.as_ref().unwrap().0.as_str())).collect();
        assert_eq!(got, vec![("a", "Jobs", "/m/Jobs"), ("b", "Jobs/Clients", "/m/Jobs/Clients"), ("c", "Jobs/Clients/Acme", "/m/Jobs/Clients/Acme"), ("e", "Jobs/Home", "/m/Jobs/Home")], "Workshop only shares a prefix");
        assert!(changed.iter().all(|c| c.old.id == c.new.id), "ids never change");

        let moved = move_folder(&labels[2], Some(&labels[4]), &labels).unwrap();
        assert_eq!((moved[0].new.name.as_str(), moved[0].new.provider_id.as_ref().unwrap().0.as_str()), ("Work/Home/Acme", "/m/Work/Home/Acme"));
        assert!(move_folder(&labels[0], Some(&labels[2]), &labels).is_err(), "not into itself");
        assert!(rename_folder(&labels[0], "workshop", &labels).is_err(), "not onto another folder's name");
        assert!(rename_folder(&labels[0], "Work", &labels).unwrap().is_empty());
        let gone = folder_and_inside(&labels[0], &labels);
        let gone: Vec<&str> = gone.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(gone, vec!["Work/Clients/Acme", "Work/Clients", "Work/Home", "Work"], "innermost first");
        assert_eq!(root_of(&labels[2], &labels).unwrap().id.0, "a");
        assert_eq!(opened_name(Path::new("/elsewhere/Work"), &labels), "Work (2)");
        assert_eq!(opened_name(Path::new("/elsewhere/Inbox"), &labels), "Inbox (offline)");
    }

    #[test]
    fn file_names_are_readable_safe_and_distinct() {
        let m = message("offline:1234-abcd-ef56", "Re: Q3 / Q4 report: *final*?", 1_740_839_520_000);
        assert_eq!(file_name(&m), "2025-03-01 1432 Re Q3 Q4 report final [abcdef56].eml");
        let blank = message("offline:x", "  ...  ", 0);
        assert_eq!(file_name(&blank), "1970-01-01 0000 No subject [x].eml");
        let long = message("offline:y", &"word ".repeat(40), 0);
        assert!(file_name(&long).chars().count() < 120);
        let wide = message("offline:z", &"報告書".repeat(40), 0);
        assert!(file_name(&wide).len() <= 200, "{} bytes", file_name(&wide).len());
        assert!(file_name(&wide).contains("報告書"));
    }

    #[test]
    fn files_are_written_moved_scanned_and_served() {
        let root = temp_dir("files");
        let raw = b"From: a@example.com\r\nTo: b@example.com\r\nSubject: Report\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=X\r\n\r\n--X\r\nContent-Type: text/plain\r\n\r\nSee attached.\r\n--X\r\nContent-Type: application/pdf; name=r.pdf\r\nContent-Disposition: attachment; filename=r.pdf\r\nContent-Transfer-Encoding: base64\r\n\r\nJVBERi0=\r\n--X--\r\n";
        let first = write_durably(&root.join("2025"), "a.eml", raw).unwrap();
        let second = write_durably(&root.join("2025"), "a.eml", b"Subject: two\r\n\r\n").unwrap();
        assert_eq!(second.file_name().unwrap(), "a (2).eml", "a taken name gets a number");
        std::fs::write(root.join("notes.txt"), "mine").unwrap();
        write_manifest(&root, &Manifest { version: 1, messages: [("2025/a.eml".to_owned(), Kept { id: "offline:1".into(), conversation: "c".into(), read: true, starred: false })].into_iter().collect() }).unwrap();

        let scanned = scan(&root).unwrap();
        assert_eq!(scanned.folders, vec!["2025".to_owned()]);
        let mut files: Vec<String> = scanned.files.iter().map(|(dir, p)| format!("{dir}:{}", p.file_name().unwrap().to_string_lossy())).collect();
        files.sort();
        assert_eq!(files, vec!["2025:a (2).eml", "2025:a.eml"], "the manifest and other files are not mail");
        assert_eq!(read_manifest(&root).messages["2025/a.eml"].id, "offline:1");
        assert_eq!(relative(&root, &first).as_deref(), Some("2025/a.eml"));

        let mut message = crate::mime::message_from_rfc822(&Account::offline(), raw, MessageId::from("offline:1"), &crate::mime::MessageFlags::default());
        message.provider_id = Some(ProviderId(first.to_string_lossy().into_owned()));
        let provider = LocalProvider::new(root.join("legacy"));
        assert_eq!(block(provider.fetch_original(&Account::offline(), &message)).unwrap(), raw.to_vec());
        assert_eq!(block(provider.fetch_attachment(&Account::offline(), &message, &message.attachments[0])).unwrap(), b"%PDF-".to_vec());

        // Creating the future must not read the file synchronously before it is polled.
        let deferred_path = root.join("2025/deferred.eml");
        let mut deferred_msg = message.clone();
        deferred_msg.provider_id = Some(ProviderId(deferred_path.to_string_lossy().into_owned()));
        let fut = provider.fetch_original(&Account::offline(), &deferred_msg);
        std::fs::write(&deferred_path, b"Subject: Deferred\r\n\r\nLate write").unwrap();
        assert_eq!(block(fut).unwrap(), b"Subject: Deferred\r\n\r\nLate write".to_vec());
        std::fs::remove_file(&deferred_path).unwrap();

        let moved = move_file(&first, &root.join("2026")).unwrap();
        assert_eq!(moved, root.join("2026/a.eml"));
        assert!(!first.exists());
        std::fs::remove_file(&second).unwrap();
        remove_empty_dirs(&root.join("2025"), &root);
        assert!(!root.join("2025").exists(), "an emptied folder goes");
        remove_empty_dirs(&root.join("2026"), &root);
        assert!(root.join("2026").exists(), "a folder with mail stays");
        remove_empty_dirs(&root, &root);
        assert!(root.join("notes.txt").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Every future here is ready at once.
    fn block<T>(mut f: BoxFuture<'_, T>) -> T {
        let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
        match f.as_mut().poll(&mut cx) {
            std::task::Poll::Ready(v) => v,
            std::task::Poll::Pending => panic!("offline folders never wait"),
        }
    }
}
