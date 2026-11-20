//! Offline folders on disk: keeps the store and the folders' directories in step.
//!
//! Each top-level offline folder is a directory the person chose, its subfolders are
//! subdirectories, and each message is an `.eml` file (see `overlook_provider::local`). The
//! store is what the window shows and changes; after every change `reconcile` moves files
//! to the folders their messages are now in, deletes the files of deleted messages and
//! rewrites each folder's manifest, so the directories always hold everything needed to
//! open the folder again after it is disconnected.
//!
//! Everything here touches the disk and runs off the UI thread. One lock keeps two of these
//! from working on the same files at once.

use overlook_core::*;
use overlook_provider::local::{self, Kept, Manifest};
use overlook_provider::mime::{message_from_rfc822, MessageFlags};
use overlook_provider::{MailProvider, ProviderError, RenamedFolder};
use overlook_store::{MessagePlace, Store};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

static FILES: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn lock() -> std::sync::MutexGuard<'static, ()> {
    FILES.lock().unwrap_or_else(|e| e.into_inner())
}

/// Where new offline folders go unless the person picks another place:
/// `~/Documents/Overlook Offline Folders`.
pub fn default_location() -> PathBuf {
    directories::UserDirs::new()
        .map(|u| u.document_dir().map(Path::to_path_buf).unwrap_or_else(|| u.home_dir().to_path_buf()))
        .unwrap_or_else(crate::data_dir)
        .join("Overlook Offline Folders")
}

/// Whether `dir` is, is inside, or holds a folder already open (the same files would then
/// belong to two folders). Paths are compared resolved where they exist.
fn overlaps_open(dir: &Path, labels: &[Label]) -> bool {
    let resolve = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let dir = resolve(dir);
    labels.iter().filter_map(local::dir_of).map(|open| resolve(&open)).any(|open| dir.starts_with(&open) || open.starts_with(&dir))
}

/// `path` for people: the home directory as `~`.
pub fn shown_path(path: &Path) -> String {
    if let Some(home) = directories::UserDirs::new().map(|u| u.home_dir().to_path_buf()) {
        if let Ok(rest) = path.strip_prefix(&home) {
            return format!("~/{}", rest.display());
        }
    }
    path.display().to_string()
}

fn path_id(path: &Path) -> ProviderId {
    ProviderId(path.to_string_lossy().into_owned())
}

/// O(1) lookup index over an account's offline folder labels.
struct LabelIndex<'a> {
    by_id: HashMap<&'a LabelId, &'a Label>,
    roots_by_name: HashMap<&'a str, &'a Label>,
}

impl<'a> LabelIndex<'a> {
    fn new(labels: &'a [Label]) -> Self {
        let by_id = labels.iter().map(|l| (&l.id, l)).collect();
        let roots_by_name = labels.iter().filter(|l| !l.name.contains('/')).map(|l| (l.name.as_str(), l)).collect();
        Self { by_id, roots_by_name }
    }

    /// The folder (label) a message is filed in.
    fn folder_of(&self, place: &MessagePlace) -> Option<&'a Label> {
        place.label_ids.iter().find_map(|id| self.by_id.get(id).copied())
    }

    /// The top-level folder `label` is in (itself, at the top).
    fn root_of(&self, label: &Label) -> Option<&'a Label> {
        let top = label.name.split('/').next().unwrap_or(&label.name);
        self.roots_by_name.get(top).copied()
    }
}

/// Writes the manifest of top-level folder `root` from the messages belonging to it.
fn write_manifest_for_root(root: &Label, places: &[&MessagePlace]) -> std::io::Result<()> {
    let Some(dir) = local::dir_of(root) else { return Ok(()) };
    if !dir.is_dir() {
        return Ok(());
    }
    let mut manifest = Manifest { version: 1, ..Default::default() };
    for place in places {
        let Some(rel) = place.provider_id.as_ref().and_then(|p| local::relative(&dir, Path::new(&p.0))) else { continue };
        manifest.messages.insert(rel, Kept { id: place.id.0.clone(), conversation: place.conversation_id.0.clone(), read: place.is_read, starred: place.is_starred });
    }
    local::write_manifest(&dir, &manifest)
}

/// Brings the directories in line with the store: files follow their messages to the
/// folders they are now in, the files in `doomed` go when no message uses them any more,
/// mail saved by the first version of offline folders (in `legacy_dir`) moves into its
/// folder, and every folder's manifest is rewritten. Folders whose directory is missing (a
/// drive not connected) are left alone.
/// Returns the files in `doomed` that could not be deleted (a drive not connected, say), to
/// try again after the next change.
pub fn reconcile(store: &Store, offline: &AccountId, legacy_dir: &Path, doomed: &[PathBuf]) -> Result<Vec<PathBuf>, String> {
    let _files = lock();
    let labels = store.labels(offline).map_err(|e| e.to_string())?;
    let index = LabelIndex::new(&labels);
    let mut places = store.message_places(offline).map_err(|e| e.to_string())?;
    let mut moved: Vec<(MessageId, Option<ProviderId>)> = Vec::new();
    for place in places.iter_mut() {
        let Some(dir) = index.folder_of(place).and_then(local::dir_of) else { continue };
        if !dir.is_dir() {
            continue;
        }
        let current = match &place.provider_id {
            Some(p) if Path::new(&p.0).is_absolute() => PathBuf::from(&p.0),
            _ => local::raw_path_in(legacy_dir, &place.id),
        };
        if current.parent() == Some(dir.as_path()) || !current.is_file() {
            continue;
        }
        let legacy = current.starts_with(legacy_dir);
        let result = if legacy {
            // Saved under its id by the first version: give it a readable name on the way.
            store.message(&place.id).ok().flatten().map(|m| local::file_name(&m)).map_or(Err(std::io::Error::other("message gone")), |name| {
                std::fs::create_dir_all(&dir)?;
                let to = local::unique_path(&dir, &name);
                std::fs::rename(&current, &to).map(|_| to.clone()).or_else(|_| {
                    let bytes = std::fs::read(&current)?;
                    let to = local::write_durably(&dir, &name, &bytes)?;
                    std::fs::remove_file(&current)?;
                    Ok(to)
                })
            })
        } else {
            local::move_file(&current, &dir)
        };
        match result {
            Ok(to) => {
                place.provider_id = Some(path_id(&to));
                moved.push((place.id.clone(), place.provider_id.clone()));
            }
            Err(e) => tracing::warn!("offline folders: could not move {} into {}: {e}", current.display(), dir.display()),
        }
    }
    if !moved.is_empty() {
        store.set_provider_ids(&moved).map_err(|e| e.to_string())?;
    }
    let used: HashSet<&str> = places.iter().filter_map(|p| p.provider_id.as_ref().map(|p| p.0.as_str())).collect();
    let mut kept_back = Vec::new();
    for path in doomed.iter().filter(|p| !used.contains(p.to_string_lossy().as_ref())) {
        if let Err(e) = std::fs::remove_file(path) {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!("offline folders: could not delete {} (will try again): {e}", path.display());
                kept_back.push(path.clone());
            }
        }
    }
    let mut by_root: HashMap<&LabelId, Vec<&MessagePlace>> = HashMap::new();
    for place in &places {
        if let Some(root) = index.folder_of(place).and_then(|f| index.root_of(f)) {
            by_root.entry(&root.id).or_default().push(place);
        }
    }
    for root in labels.iter().filter(|l| !l.name.contains('/')) {
        let root_places = by_root.get(&root.id).map(Vec::as_slice).unwrap_or(&[]);
        if let Err(e) = write_manifest_for_root(root, root_places) {
            tracing::warn!("offline folders: could not save the state of “{}”: {e}", root.name);
        }
    }
    Ok(kept_back)
}

/// Gives top-level folders made by the first version of offline folders (which kept every
/// message in one hidden directory) a directory of their own under `location`, with one
/// subdirectory per subfolder. `reconcile` then moves their messages in.
pub fn adopt_legacy_folders(store: &Store, offline: &AccountId, location: &Path) -> Result<usize, String> {
    let _files = lock();
    let labels = store.labels(offline).map_err(|e| e.to_string())?;
    let mut adopted = 0;
    for root in labels.iter().filter(|l| !l.name.contains('/') && l.provider_id.is_none()) {
        std::fs::create_dir_all(location).map_err(|e| e.to_string())?;
        let dir = local::unique_path(location, &root.name);
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        for label in labels.iter().filter(|l| l.id == root.id || l.name.starts_with(&format!("{}/", root.name))) {
            let rest = label.name[root.name.len()..].trim_start_matches('/');
            let sub = if rest.is_empty() { dir.clone() } else { dir.join(rest) };
            std::fs::create_dir_all(&sub).map_err(|e| e.to_string())?;
            store.upsert_label(&Label { provider_id: Some(path_id(&sub)), ..label.clone() }).map_err(|e| e.to_string())?;
        }
        adopted += 1;
    }
    Ok(adopted)
}

/// Makes a folder: at the top level in `location` (a directory named after it), or inside
/// `parent`. A top-level directory that already holds mail is refused: it should be opened.
pub fn create_folder(store: &Store, offline: &AccountId, name: &str, parent: Option<&Label>, location: &Path) -> Result<Label, ProviderError> {
    let _files = lock();
    let labels = store.labels(offline).map_err(|e| ProviderError::Rejected(e.to_string()))?;
    let mut label = local::new_folder(offline, name, parent, &labels)?;
    let dir = match parent {
        Some(p) => local::dir_of(p).ok_or_else(|| ProviderError::Rejected("That folder has no place on disk.".into()))?.join(name.trim()),
        None => location.join(name.trim()),
    };
    if parent.is_none() && overlaps_open(&dir, &labels) {
        return Err(ProviderError::Rejected(format!("{} is inside or around an offline folder that is already open. Choose another place.", shown_path(&dir))));
    }
    if dir.exists() && local::scan(&dir).is_ok_and(|s| !s.files.is_empty()) {
        return Err(ProviderError::Rejected(format!("{} already holds mail. Use Open Offline Folder… to add it.", shown_path(&dir))));
    }
    std::fs::create_dir_all(&dir).map_err(|e| ProviderError::Rejected(format!("Couldn’t make the folder {}: {e}", shown_path(&dir))))?;
    label.provider_id = Some(path_id(&dir));
    store.upsert_label(&label).map_err(|e| ProviderError::Rejected(e.to_string()))?;
    if parent.is_none() {
        let _ = local::write_manifest(&dir, &Manifest { version: 1, ..Default::default() });
    }
    Ok(label)
}

/// Renames or moves a folder's directory and records the change (`changed` from
/// `local::rename_folder` or `local::move_folder`, the folder first). The files inside
/// keep their names; their recorded paths follow the directory.
pub fn rename_folder(store: &Store, offline: &AccountId, changed: &[RenamedFolder]) -> Result<(), ProviderError> {
    let Some(first) = changed.first() else { return Ok(()) };
    let _files = lock();
    let (Some(from), Some(to)) = (local::dir_of(&first.old), local::dir_of(&first.new)) else {
        return Err(ProviderError::Rejected("That folder has no place on disk.".into()));
    };
    if from != to {
        if to.exists() {
            return Err(ProviderError::Rejected(format!("{} already exists.", shown_path(&to))));
        }
        if !from.is_dir() {
            return Err(ProviderError::Rejected(format!("{} can’t be found. Is its drive connected?", shown_path(&from))));
        }
        std::fs::rename(&from, &to).map_err(|e| ProviderError::Rejected(format!("Couldn’t rename {}: {e}", shown_path(&from))))?;
    }
    let renames: Vec<(LabelId, Label)> = changed.iter().map(|c| (c.old.id.clone(), c.new.clone())).collect();
    let sep = std::path::MAIN_SEPARATOR;
    let prefixes = vec![(format!("{}{sep}", from.display()), format!("{}{sep}", to.display()))];
    if let Err(e) = store.rename_folders(offline, &renames, &prefixes) {
        // The store still names the old place: put the directory back, so every recorded
        // path stays true.
        if from != to {
            if let Err(back) = std::fs::rename(&to, &from) {
                tracing::error!("offline folders: could not move {} back to {} after a failed save: {back}", to.display(), from.display());
            }
        }
        return Err(ProviderError::Rejected(format!("Couldn’t save the change: {e}")));
    }
    drop(_files);
    reconcile(store, offline, &crate::providers::offline_dir(), &[]).map(|_| ()).map_err(ProviderError::Rejected)
}

/// The mail files of the folders in `gone`, for deleting them.
pub fn files_in(store: &Store, offline: &AccountId, gone: &[Label]) -> Vec<PathBuf> {
    let ids: HashSet<LabelId> = gone.iter().map(|l| l.id.clone()).collect();
    store
        .message_places(offline)
        .unwrap_or_default()
        .into_iter()
        .filter(|p| p.label_ids.iter().any(|l| ids.contains(l)))
        .filter_map(|p| p.provider_id.map(|p| PathBuf::from(p.0)))
        .filter(|p| p.is_absolute())
        .collect()
}

/// Deletes the mail files of folders being deleted, then their directories when nothing
/// else is in them (a file the person put there keeps its directory). `top` is the
/// top-level folder they are in. The store rows are removed by the caller.
pub fn delete_files(files: &[PathBuf], gone: &[Label], top: &Label) -> usize {
    let _files = lock();
    let mut deleted = 0;
    for f in files {
        if std::fs::remove_file(f).is_ok() {
            deleted += 1;
        }
    }
    let Some(root) = local::dir_of(top) else { return deleted };
    let stop = if gone.iter().any(|l| l.id == top.id) {
        let _ = std::fs::remove_file(root.join(local::MANIFEST));
        root.parent().map(Path::to_path_buf).unwrap_or(root.clone())
    } else {
        root
    };
    // Innermost first, so a parent is empty once its children are gone.
    for dir in gone.iter().filter_map(local::dir_of) {
        local::remove_empty_dirs(&dir, &stop);
    }
    deleted
}

/// Saves everything Overlook knows about a top-level folder beside its files before it is
/// disconnected, so opening it again restores it.
///
/// Refused while any message of the folder has no file in it (a move that could not
/// finish, a file deleted outside Overlook) or a message filed elsewhere still has its file
/// here: after disconnecting, Overlook would forget mail that exists nowhere else, or the
/// folder would come back with mail that left it.
pub fn save_before_disconnect(store: &Store, offline: &AccountId, root: &Label) -> Result<(), String> {
    reconcile(store, offline, &crate::providers::offline_dir(), &[])?;
    let _files = lock();
    let labels = store.labels(offline).map_err(|e| e.to_string())?;
    let index = LabelIndex::new(&labels);
    let places = store.message_places(offline).map_err(|e| e.to_string())?;
    let dir = local::dir_of(root).ok_or_else(|| "this folder has no place on disk".to_owned())?;
    let mut unsaved = 0;
    let mut strays = 0;
    let mut root_places = Vec::new();
    for place in &places {
        let file = place.provider_id.as_ref().map(|p| PathBuf::from(&p.0)).filter(|p| p.is_absolute());
        let mine = index.folder_of(place).and_then(|f| index.root_of(f)).is_some_and(|r| r.id == root.id);
        let here = file.as_ref().is_some_and(|f| f.starts_with(&dir));
        if mine {
            if !(here && file.as_ref().is_some_and(|f| f.is_file())) {
                unsaved += 1;
            }
            root_places.push(place);
        } else if here {
            strays += 1;
        }
    }
    if unsaved > 0 {
        return Err(format!("{} in it {} no file in {} yet. Connect the drive and try again, or delete {} first.", messages_phrase(unsaved), if unsaved == 1 { "has" } else { "have" }, shown_path(&dir), if unsaved == 1 { "it" } else { "them" }));
    }
    if strays > 0 {
        return Err(format!("{} moved out of it {} still on disk in {}. Try again once {} been moved.", messages_phrase(strays), if strays == 1 { "is" } else { "are" }, shown_path(&dir), if strays == 1 { "it has" } else { "they have" }));
    }
    write_manifest_for_root(root, &root_places).map_err(|e| e.to_string())
}

fn messages_phrase(n: usize) -> String {
    if n == 1 { "1 message".to_owned() } else { format!("{n} messages") }
}

/// What opening a folder from disk found.
#[derive(Debug)]
pub struct Opened {
    pub root: Label,
    pub messages: usize,
    /// Files that are not mail Overlook could read.
    pub skipped: usize,
}

/// Opens the folder at `dir`: an offline folder disconnected earlier, here or on another
/// computer, or any folder of `.eml` files. Its subdirectories become subfolders. The
/// manifest, when there is one, restores ids, conversations, and read and starred state.
pub fn open_folder(store: &Store, offline: &Account, dir: &Path) -> Result<Opened, String> {
    let dir = dir.canonicalize().map_err(|e| format!("{} can’t be opened: {e}", shown_path(dir)))?;
    if !dir.is_dir() {
        return Err(format!("{} is not a folder.", shown_path(&dir)));
    }
    {
        let _files = lock();
        let existing = store.labels(&offline.id).map_err(|e| e.to_string())?;
        if overlaps_open(&dir, &existing) {
            return Err(format!("{} is already open in Overlook, or inside or around a folder that is.", shown_path(&dir)));
        }
    }
    // Scan and parse .eml files off the global FILES lock so large imports don't stall
    // unrelated offline folder operations.
    let scanned = local::scan(&dir).map_err(|e| format!("{} can’t be read: {e}", shown_path(&dir)))?;
    let manifest = local::read_manifest(&dir);

    let mut raw_messages: Vec<(String, Message)> = Vec::new();
    let mut skipped = 0;
    let mut taken: BTreeSet<String> = BTreeSet::new();
    for (rel_dir, path) in &scanned.files {
        let Ok(bytes) = std::fs::read(path) else { skipped += 1; continue };
        let kept = local::relative(&dir, path).and_then(|rel| manifest.messages.get(&rel).cloned());
        // A remembered id is used again unless another message already has it.
        let id = kept
            .as_ref()
            .map(|k| MessageId(k.id.clone()))
            .filter(|id| id.0.starts_with("offline:") && !taken.contains(&id.0) && store.message(id).ok().flatten().is_none())
            .unwrap_or_else(|| MessageId(format!("offline:{}", MessageId::generate().0)));
        taken.insert(id.0.clone());
        let flags = MessageFlags {
            provider_id: Some(path_id(path)),
            label_ids: Default::default(),
            is_read: kept.as_ref().is_none_or(|k| k.read),
            is_starred: kept.as_ref().is_some_and(|k| k.starred),
            received_at: path.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_millis() as i64),
        };
        let mut message = message_from_rfc822(offline, &bytes, id, &flags);
        if let Some(conversation) = kept.as_ref().map(|k| k.conversation.clone()).filter(|c| !c.is_empty()) {
            message.conversation_id = ConversationId(conversation);
        }
        raw_messages.push((rel_dir.clone(), message));
    }

    let (root, count) = {
        let _files = lock();
        let mut labels = store.labels(&offline.id).map_err(|e| e.to_string())?;
        if overlaps_open(&dir, &labels) {
            return Err(format!("{} is already open in Overlook, or inside or around a folder that is.", shown_path(&dir)));
        }
        let name = local::opened_name(&dir, &labels);
        let root = Label { provider_id: Some(path_id(&dir)), ..local::new_folder(&offline.id, &name, None, &labels).map_err(|e| e.to_string())? };
        labels.push(root.clone());
        let mut by_rel: HashMap<String, LabelId> = HashMap::new();
        by_rel.insert(String::new(), root.id.clone());
        let mut new_labels = vec![root.clone()];
        for rel in &scanned.folders {
            let (parent_rel, leaf) = rel.rsplit_once('/').map(|(p, l)| (p.to_owned(), l.to_owned())).unwrap_or((String::new(), rel.clone()));
            let Some(parent) = by_rel.get(&parent_rel).and_then(|id| labels.iter().find(|l| &l.id == id)).cloned() else { continue };
            let Ok(mut label) = local::new_folder(&offline.id, &leaf, Some(&parent), &labels) else { continue };
            label.provider_id = Some(path_id(&dir.join(rel)));
            by_rel.insert(rel.clone(), label.id.clone());
            labels.push(label.clone());
            new_labels.push(label);
        }

        let mut messages = Vec::with_capacity(raw_messages.len());
        for (rel_dir, mut message) in raw_messages {
            let Some(folder) = by_rel.get(&rel_dir) else { skipped += 1; continue };
            message.label_ids.insert(folder.clone());
            messages.push(message);
        }
        overlook_core::threading::assign_conversations(&mut messages, |_| None);

        for label in &new_labels {
            store.upsert_label(label).map_err(|e| e.to_string())?;
        }
        for chunk in messages.chunks(200) {
            store.upsert_messages(chunk).map_err(|e| e.to_string())?;
        }
        (root, messages.len())
    };
    reconcile(store, &offline.id, &crate::providers::offline_dir(), &[])?;
    Ok(Opened { root, messages: count, skipped })
}

/// What copying one batch into an offline folder did.
#[derive(Default)]
pub struct Copied {
    /// Source messages now safely offline, ready to leave the server.
    pub copied: Vec<MessageId>,
    /// Source messages left where they are, and why.
    pub failed: Vec<(MessageId, String)>,
    /// Set when the server can't be reached: the rest of the drag is not attempted.
    pub stopped: Option<String>,
}

fn reason(e: &ProviderError) -> String {
    match e {
        ProviderError::Offline => "you’re offline".into(),
        ProviderError::SignInRequired => "the account needs you to sign in again".into(),
        ProviderError::RateLimited(_) => "the server asked Overlook to slow down".into(),
        other => other.to_string(),
    }
}

/// Copies `ids` of `source` into offline folder `target`: each original is downloaded,
/// checked, written to the folder's directory and flushed to disk, then filed in the
/// store. The source messages are not touched; deleting them from the server is the
/// caller's next step, for the ones in `copied` only.
pub async fn copy_to_offline(store: Arc<Store>, provider: Arc<dyn MailProvider>, source: Account, offline: Account, target: Label, ids: Vec<MessageId>) -> Copied {
    let mut out = Copied::default();
    let Some(dir) = local::dir_of(&target).filter(|d| d.is_dir()) else {
        let why = format!("the folder {} can’t be found", local::dir_of(&target).map(|d| shown_path(&d)).unwrap_or_default());
        out.failed = ids.iter().map(|id| (id.clone(), why.clone())).collect();
        out.stopped = Some(why);
        return out;
    };
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
        let raw = match provider.fetch_original(&source, &message).await {
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
            label_ids: [target.id.clone()].into_iter().collect(),
            is_read: message.is_read,
            is_starred: message.is_starred,
            received_at: Some(message.date),
        };
        let mut copy = message_from_rfc822(&offline, &raw, copy_id, &flags);
        // A second guard after `fetch_original`: a copy that lost the attachments the stored
        // message lists is not complete enough to delete the server's.
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
        let _files = lock();
        let saved = local::write_durably(&dir, &local::file_name(&copy), &raw).map_err(|e| format!("couldn’t save it in {}: {e}", shown_path(&dir))).and_then(|path| {
            copy.provider_id = Some(path_id(&path));
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

#[cfg(test)]
mod tests {
    use super::*;
    use overlook_provider::sample::SampleProvider;
    use overlook_store::LocalChange;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("overlook-offline-disk-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.canonicalize().unwrap()
    }

    fn files_under(dir: &Path) -> Vec<String> {
        let mut out: Vec<String> = local::scan(dir).unwrap().files.iter().map(|(_, p)| local::relative(dir, p).unwrap()).collect();
        out.sort();
        out
    }

    #[test]
    fn mail_lives_in_folders_on_disk_and_survives_disconnecting_and_opening_again() {
        let base = temp_dir("roundtrip");
        let legacy = base.join("legacy");
        let store = Arc::new(Store::open_in_memory().unwrap());
        let source = crate::seed_sample_account(&store).unwrap();
        let offline = store.offline_account().unwrap();
        let taxes = create_folder(&store, &offline.id, "Taxes", None, &base).unwrap();
        let y2025 = create_folder(&store, &offline.id, "2025", Some(&taxes), &base).unwrap();
        assert_eq!(local::dir_of(&y2025).unwrap(), base.join("Taxes/2025"));
        assert!(base.join("Taxes").join(local::MANIFEST).exists());
        assert!(create_folder(&store, &offline.id, "Taxes", None, &base).is_err(), "names are unique");

        // Copy two plain messages in; one with attachments stays on the server, because the
        // sample provider can only rebuild messages from their text.
        let all: Vec<Message> = store.message_ids_of(&source.id).unwrap().into_iter().filter_map(|id| store.message(&id).unwrap()).collect();
        let plain: Vec<MessageId> = all.iter().filter(|m| m.attachments.is_empty() && m.body.is_cached()).take(2).map(|m| m.id.clone()).collect();
        let with_files = all.iter().find(|m| !m.attachments.is_empty()).unwrap().id.clone();
        let provider: Arc<dyn MailProvider> = Arc::new(SampleProvider::new());
        let ids = [plain.clone(), vec![with_files.clone()]].concat();
        let out = crate::sync::pollster_block(copy_to_offline(store.clone(), provider, source.clone(), offline.clone(), y2025.clone(), ids));
        assert_eq!(out.copied, plain);
        assert_eq!(out.failed.iter().map(|(id, _)| id.clone()).collect::<Vec<_>>(), vec![with_files]);
        assert_eq!(files_under(&base.join("Taxes")).len(), 2);
        assert!(files_under(&base.join("Taxes")).iter().all(|f| f.starts_with("2025/") && f.ends_with(".eml")));
        assert!(store.message(&plain[0]).unwrap().is_some(), "the source is untouched until the caller deletes it");

        // Moving a message to another folder moves its file; the manifest keeps its state.
        let copies = store.message_places(&offline.id).unwrap();
        let first = copies[0].id.clone();
        store.apply_change(&offline.id, LocalChange::AddLabel { message_ids: vec![first.clone()], label_id: taxes.id.clone() }).unwrap();
        store.apply_change(&offline.id, LocalChange::RemoveLabel { message_ids: vec![first.clone()], label_id: y2025.id.clone() }).unwrap();
        store.apply_change(&offline.id, LocalChange::SetStarred { message_ids: vec![first.clone()], starred: true }).unwrap();
        reconcile(&store, &offline.id, &legacy, &[]).unwrap();
        let files = files_under(&base.join("Taxes"));
        assert_eq!(files.iter().filter(|f| !f.contains('/')).count(), 1, "one file moved up: {files:?}");
        let manifest = local::read_manifest(&base.join("Taxes"));
        let kept = manifest.messages.values().find(|k| k.id == first.0).unwrap();
        assert!(kept.starred);

        // Renaming the subfolder renames its directory, and the remaining file follows.
        let labels = store.labels(&offline.id).unwrap();
        let changed = local::rename_folder(&y2025, "Year 2025", &labels).unwrap();
        rename_folder(&store, &offline.id, &changed).unwrap();
        assert!(base.join("Taxes/Year 2025").is_dir() && !base.join("Taxes/2025").exists());
        for place in store.message_places(&offline.id).unwrap() {
            assert!(Path::new(&place.provider_id.unwrap().0).is_file(), "every recorded path is a file");
        }

        // Disconnect: the store forgets the folder, the files stay.
        save_before_disconnect(&store, &offline.id, &taxes).unwrap();
        for label in local::folder_and_inside(&taxes, &store.labels(&offline.id).unwrap()) {
            store.delete_label(&offline.id, &label.id, true).unwrap();
        }
        assert!(store.message_places(&offline.id).unwrap().is_empty());
        assert_eq!(files_under(&base.join("Taxes")).len(), 2, "nothing on disk was touched");

        // Open it again: the same messages, folders and stars come back.
        let opened = open_folder(&store, &offline, &base.join("Taxes")).unwrap();
        assert_eq!((opened.root.name.as_str(), opened.messages, opened.skipped), ("Taxes", 2, 0));
        let names: Vec<String> = store.labels(&offline.id).unwrap().into_iter().map(|l| l.name).collect();
        assert!(names.contains(&"Taxes/Year 2025".to_owned()), "{names:?}");
        let back = store.message(&first).unwrap().expect("the remembered id is used again");
        assert!(back.is_starred);
        assert!(back.label_ids.contains(&opened.root.id));
        assert!(open_folder(&store, &offline, &base.join("Taxes/Year 2025")).is_err(), "already open");

        // Deleting the folder deletes its mail files and the empty directories, nothing else.
        std::fs::write(base.join("Taxes/notes.txt"), "mine").unwrap();
        let labels = store.labels(&offline.id).unwrap();
        let root = labels.iter().find(|l| l.name == "Taxes").unwrap().clone();
        let gone = local::folder_and_inside(&root, &labels);
        let files = files_in(&store, &offline.id, &gone);
        assert_eq!(delete_files(&files, &gone, &root), 2);
        assert!(files_under(&base.join("Taxes")).is_empty());
        assert!(!base.join("Taxes/Year 2025").exists());
        assert!(base.join("Taxes/notes.txt").exists(), "a file the person put there stays, and so does its folder");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn overlapping_folders_unsaved_mail_and_stuck_deletions_are_caught() {
        let base = temp_dir("guards");
        let legacy = base.join("legacy");
        let store = Store::open_in_memory().unwrap();
        let offline = store.offline_account().unwrap();
        let work = create_folder(&store, &offline.id, "Work", None, &base).unwrap();
        assert!(create_folder(&store, &offline.id, "Receipts", None, &base.join("Work")).is_err(), "not inside an open folder");
        assert!(create_folder(&store, &offline.id, "Receipts", None, &base).is_ok(), "beside it is fine");

        // A message whose file is gone can't be disconnected away silently.
        let raw = b"Subject: Hello\r\n\r\nHi.\r\n";
        let dir = local::dir_of(&work).unwrap();
        let path = local::write_durably(&dir, "hello.eml", raw).unwrap();
        let m = message_from_rfc822(&offline, raw, MessageId::from("offline:1"), &MessageFlags { provider_id: Some(path_id(&path)), label_ids: [work.id.clone()].into_iter().collect(), ..Default::default() });
        store.upsert_messages(&[Message { conversation_id: ConversationId::from("offline:c"), ..m }]).unwrap();
        save_before_disconnect(&store, &offline.id, &work).unwrap();
        std::fs::remove_file(&path).unwrap();
        let refused = save_before_disconnect(&store, &offline.id, &work).unwrap_err();
        assert!(refused.contains("1 message"), "{refused}");

        // A file that can't be deleted yet comes back, to be tried again. (Something in the
        // way that can't be removed as a file, even by root as in CI.)
        let stuck = base.join("stuck.eml");
        std::fs::create_dir_all(stuck.join("inside")).unwrap();
        let kept = reconcile(&store, &offline.id, &legacy, std::slice::from_ref(&stuck)).unwrap();
        assert_eq!(kept, vec![stuck.clone()]);
        std::fs::remove_dir_all(&stuck).unwrap();
        std::fs::write(&stuck, b"x").unwrap();
        assert!(reconcile(&store, &offline.id, &legacy, &kept).unwrap().is_empty(), "deleted once it can be");
        assert!(!stuck.exists());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn folders_from_the_first_version_move_onto_disk() {
        let base = temp_dir("legacy");
        let legacy = base.join("raw");
        let store = Store::open_in_memory().unwrap();
        let offline = store.offline_account().unwrap();
        let old = local::new_folder(&offline.id, "Receipts", None, &[]).unwrap();
        store.upsert_label(&old).unwrap();
        let raw = b"From: a@example.com\r\nSubject: Receipt\r\nDate: Sat, 1 Mar 2025 14:32:00 +0000\r\n\r\nThanks.\r\n";
        let id = MessageId::from("offline:legacy-1");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(local::raw_path_in(&legacy, &id), raw).unwrap();
        let m = message_from_rfc822(&offline, raw, id.clone(), &MessageFlags { label_ids: [old.id.clone()].into_iter().collect(), ..Default::default() });
        store.upsert_messages(&[Message { conversation_id: ConversationId::from("offline:c"), ..m }]).unwrap();

        assert_eq!(adopt_legacy_folders(&store, &offline.id, &base.join("Offline")).unwrap(), 1);
        reconcile(&store, &offline.id, &legacy, &[]).unwrap();
        let moved = store.message(&id).unwrap().unwrap().provider_id.unwrap();
        assert!(moved.0.starts_with(base.join("Offline/Receipts").to_str().unwrap()), "{}", moved.0);
        assert!(moved.0.ends_with("Receipt [legacy1].eml"), "{}", moved.0);
        assert!(!local::raw_path_in(&legacy, &id).exists());
        assert_eq!(adopt_legacy_folders(&store, &offline.id, &base.join("Offline")).unwrap(), 0, "only once");
        let _ = std::fs::remove_dir_all(&base);
    }
}
