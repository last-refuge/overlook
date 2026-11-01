//! `MailProvider` for IMAP accounts (plan milestones M2, the read path; M3, the write path
//! and sending; M5, `IDLE`, the `CONDSTORE`/`QRESYNC` fast paths and server-side search).
//!
//! - `connection`: TLS / STARTTLS / plaintext-to-localhost sessions and the error mapping.
//! - `folders`: `LIST` results as [`Label`] rows.
//! - `structure`: `BODYSTRUCTURE`-driven partial download of large messages.
//! - `uidset`: sequence sets for the cursors.
//! - `idle`: the per-account `IDLE` thread that asks for a sync when the Inbox changes.
//!
//! One connection per account serves every call, under a mutex; a dropped connection is
//! reopened once before the call reports [`ProviderError::Offline`]. The checkpoint the sync
//! engine stores is the JSON of all folder cursors ([`Checkpoint`]): per folder its
//! `UIDVALIDITY`, the highest UID seen, the `HIGHESTMODSEQ` last seen, and the UIDs
//! Overlook holds with their `\Seen` and `\Flagged` state, so `fetch_changes` can find new
//! mail, flag changes and deletions with a `UID SEARCH ALL` and a `UID FETCH (FLAGS)` per
//! folder on any server. When the server advertises `CONDSTORE`, one
//! `UID FETCH <known> (UID FLAGS) (CHANGEDSINCE <modseq>)` replaces the flag refresh; with
//! `QRESYNC` enabled, its `VANISHED` response replaces the `UID SEARCH ALL` diff for
//! deletions too, and new mail is read with `UID FETCH <highest+1>:* (UID)`.
//!
//! Server-side search is `UID SEARCH TEXT` per folder (Spam and Trash excluded), Inbox
//! first, with `CHARSET UTF-8` and a `LITERAL+` literal for non-ASCII words.
//!
//! Message identity, from the plan: local ids are `<account>:<uuid>` minted on first sight
//! (the sync engine adopts the stored id for a provider id already on disk); `provider_id`
//! is `<folder>\u{1f}<uidvalidity>\u{1f}<uid>`.
//!
//! Mailbox writes, from the plan's "IMAP operations" table: flags go through
//! `UID STORE ±FLAGS.SILENT` grouped by folder; Archive, Trash, Untrash, AddLabel and
//! RemoveLabel are moves (`UID MOVE` when advertised, else `UID COPY` + `\Deleted` +
//! `UID EXPUNGE`), whose `COPYUID` gives the new provider ids back through
//! [`AppliedChanges`]. A message that is already gone from its folder is a no-op (the next
//! change cycle removes the local row); a target folder that vanished is `Rejected`; the
//! Archive (and Trash) folder is created on demand. Every write is safe to replay: a
//! message moved by an interrupted attempt is found again in the target folder by its
//! Message-ID.
//!
//! Sending goes over SMTP ([`crate::smtp::Transport`]) with the shared [`crate::build_raw`]
//! and the deterministic [`crate::draft_message_id`]; the same bytes are then `APPEND`ed to
//! the Sent folder with `\Seen`, and `APPENDUID` names the Sent copy. When the `APPEND`
//! fails after SMTP accepted the message, the receipt still says Sent and the Sent copy is
//! built locally with a placeholder provider id ([`is_local_placeholder`]) that the sync
//! engine swaps for the server copy when one turns up. `reconcile_send` searches Sent for
//! the Message-ID; not finding it is not proof of non-delivery, so the outcome is
//! `NotDelivered` only when this process saw the failure happen before `DATA`, and
//! [`SendOutcome::Unknown`] otherwise.

pub mod connection;
pub mod folders;
pub mod idle;
pub mod structure;
pub mod uidset;

#[cfg(any(test, feature = "fake-servers"))]
pub mod fake;
#[cfg(test)]
mod tests;

use crate::mime::{headers_from_rfc822, message_from_rfc822, MessageFlags};
use crate::{build_raw, draft_message_id, smtp, AppliedChanges, BoxFuture, MailProvider, MailboxSnapshot, ProviderError, RemoteChanges, SendContext, SendOutcome, SentReceipt};
mod drafts;
use connection::{map_error, Session};
use folders::FolderInfo;
use imap::types::{Flag, UnsolicitedResponse};
use imap_proto::types::{MessageSection, Response, ResponseCode, SectionPath, UidSetMember};
use overlook_core::time::now;
use overlook_core::*;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Mutex;
use std::time::Duration;
use tracing::{debug, info, warn};

/// Socket connect/read/write timeout. A hung server must not stall the sync thread.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// How often the folder list is re-read.
const LIST_INTERVAL_MS: i64 = 60 * 60 * 1000;

/// Every nth change cycle refreshes flags in folders other than the Inbox.
const FLAG_REFRESH_EVERY: u64 = 10;

/// UIDs per `UID FETCH` command.
const FETCH_CHUNK: usize = 400;

const SEP: char = '\u{1f}';

/// Prefix of the provider id given to a Sent copy that exists only locally (the `APPEND`
/// failed after SMTP accepted the message). The sync engine adopts such a row for the
/// server copy with the same Message-ID when one appears.
const LOCAL_PLACEHOLDER: &str = "local\u{1f}";

/// Whether a provider id names a locally built Sent copy rather than a server message.
pub fn is_local_placeholder(id: &ProviderId) -> bool {
    id.0.starts_with(LOCAL_PLACEHOLDER)
}

// ------------------------------------------------------------------ checkpoint

/// The sync cursor for one folder.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FolderCursor {
    /// `LabelKind` key of the label this folder maps to.
    pub kind: String,
    pub uidvalidity: u32,
    /// Highest UID ever seen in this folder; new mail has a higher UID.
    pub highest_uid: u32,
    /// `HIGHESTMODSEQ` when the folder was last read, for `CHANGEDSINCE`; 0 when the server
    /// has no `CONDSTORE` or the folder has not been read since the extension was adopted.
    pub highest_modseq: u64,
    /// UIDs Overlook holds (sequence set).
    pub known: String,
    /// Subset of `known` that carried `\Seen` when last checked.
    pub seen: String,
    /// Subset of `known` that carried `\Flagged` when last checked.
    pub flagged: String,
    pub delimiter: Option<String>,
    pub special_use: Option<String>,
    pub subscribed: bool,
    /// Listed but not yet selected (added between folder listings).
    pub fresh: bool,
}

impl FolderCursor {
    fn label_kind(&self) -> LabelKind {
        LabelKind::from_key(&self.kind).unwrap_or(LabelKind::User)
    }

    fn known_set(&self) -> BTreeSet<u32> {
        uidset::decode(&self.known)
    }

    fn set_flags(&mut self, known: &BTreeSet<u32>, seen: &BTreeSet<u32>, flagged: &BTreeSet<u32>) {
        self.known = uidset::encode(known);
        self.seen = uidset::encode(&seen.intersection(known).copied().collect());
        self.flagged = uidset::encode(&flagged.intersection(known).copied().collect());
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Checkpoint {
    pub version: u32,
    /// When the folder list was last read (ms since the epoch).
    pub listed_at: i64,
    /// Change cycles run so far; drives the flag refresh cadence.
    pub cycle: u64,
    pub folders: BTreeMap<String, FolderCursor>,
}

impl Checkpoint {
    pub const VERSION: u32 = 1;

    pub fn parse(text: &str) -> Result<Self, ProviderError> {
        let cp: Checkpoint = serde_json::from_str(text).map_err(|_| ProviderError::CheckpointExpired)?;
        if cp.version != Self::VERSION {
            return Err(ProviderError::CheckpointExpired);
        }
        Ok(cp)
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

/// The folder rows the app mirrors into `imap_folders`, from a checkpoint.
pub fn folder_states(checkpoint: &str) -> Vec<ImapFolder> {
    let Ok(cp) = Checkpoint::parse(checkpoint) else { return Vec::new() };
    cp.folders
        .into_iter()
        .map(|(path, c)| ImapFolder { path, uidvalidity: (c.uidvalidity != 0).then_some(c.uidvalidity), highest_uid: c.highest_uid, highest_modseq: (c.highest_modseq != 0).then_some(c.highest_modseq), delimiter: c.delimiter, special_use: c.special_use, subscribed: c.subscribed })
        .collect()
}

// -------------------------------------------------------------- message refs

/// What a message's provider id names.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MessageRef {
    pub folder: String,
    pub uidvalidity: u32,
    pub uid: u32,
}

impl MessageRef {
    pub fn parse(id: &ProviderId) -> Option<Self> {
        let mut parts = id.0.rsplitn(3, SEP);
        let uid = parts.next()?.parse().ok()?;
        let uidvalidity = parts.next()?.parse().ok()?;
        let folder = parts.next()?.to_owned();
        Some(Self { folder, uidvalidity, uid })
    }

    pub fn provider_id(folder: &str, uidvalidity: u32, uid: u32) -> ProviderId {
        ProviderId(format!("{folder}{SEP}{uidvalidity}{SEP}{uid}"))
    }
}

// ------------------------------------------------------------------ provider

#[derive(Debug, Clone, Copy, Default)]
pub struct Capabilities {
    pub special_use: bool,
    pub move_: bool,
    pub uidplus: bool,
    /// `CONDSTORE` (or `QRESYNC`, which implies it): `CHANGEDSINCE` flag fetches work.
    pub condstore: bool,
    /// `QRESYNC` was advertised **and** enabled on this connection (`ENABLE QRESYNC`), so
    /// `VANISHED` reports deletions.
    pub qresync: bool,
    pub idle: bool,
    /// `LITERAL+` / `LITERAL-`: non-synchronising literals, used for non-ASCII search words.
    pub literal_plus: bool,
}

/// The folder a connection has open.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Selected {
    path: String,
    uidvalidity: u32,
    /// `SELECT` rather than `EXAMINE`.
    writable: bool,
}

struct Conn {
    session: Option<Session>,
    selected: Option<Selected>,
    caps: Capabilities,
    /// A write created a folder; the next change cycle must re-list so the checkpoint
    /// tracks it. Survives reconnects.
    relist_pending: bool,
    /// The Sent folder path from the last listing, for `APPEND` and `reconcile_send`.
    sent_path: Option<String>,
    /// Every selectable folder from the last listing with its label kind, for search.
    folders: Vec<(String, LabelKind)>,
}

impl Conn {
    fn session(&mut self) -> &mut Session {
        self.session.as_mut().expect("with_session connects before use")
    }

    fn open(&mut self, path: &str, writable: bool) -> Result<imap::types::Mailbox, ProviderError> {
        self.selected = None;
        let mailbox = if writable { self.session().select(path) } else { self.session().examine(path) }.map_err(|e| map_error(&e))?;
        self.selected = Some(Selected { path: path.to_owned(), uidvalidity: mailbox.uid_validity.unwrap_or(0), writable });
        Ok(mailbox)
    }

    /// `EXAMINE`s the folder (read-only) and returns its state. Always issued, since the
    /// callers want a fresh `UIDVALIDITY`.
    fn examine(&mut self, path: &str) -> Result<imap::types::Mailbox, ProviderError> {
        self.open(path, false)
    }

    /// `SELECT`s the folder read-write and returns its state.
    fn select(&mut self, path: &str) -> Result<imap::types::Mailbox, ProviderError> {
        self.open(path, true)
    }

    /// Makes `path` the open folder (read-only is enough), skipping the round trip when it
    /// already is. Returns `false` when its `UIDVALIDITY` is not `uidvalidity` any more or
    /// the folder is gone.
    fn examine_for(&mut self, path: &str, uidvalidity: u32) -> Result<bool, ProviderError> {
        self.open_for(path, uidvalidity, false)
    }

    /// Like [`Conn::examine_for`] but read-write, for `STORE`, `MOVE` and `EXPUNGE`.
    fn select_for(&mut self, path: &str, uidvalidity: u32) -> Result<bool, ProviderError> {
        self.open_for(path, uidvalidity, true)
    }

    fn open_for(&mut self, path: &str, uidvalidity: u32, writable: bool) -> Result<bool, ProviderError> {
        if self.selected.as_ref().is_some_and(|s| s.path == path && s.uidvalidity == uidvalidity && (s.writable || !writable)) {
            return Ok(true);
        }
        match self.open(path, writable) {
            Ok(_) => Ok(self.selected.as_ref().is_some_and(|s| s.uidvalidity == uidvalidity)),
            // The folder is gone: its messages are stale; the next change cycle cleans up.
            Err(ProviderError::Rejected(_)) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// `UID SEARCH HEADER Message-ID` in the open folder; the lowest matching UID.
    fn find_by_message_id(&mut self, rfc_id: &str) -> Result<Option<u32>, ProviderError> {
        let clean: String = rfc_id.chars().filter(|c| !matches!(c, '"' | '\\' | '\r' | '\n')).collect();
        let hits = self.session().uid_search(format!("HEADER Message-ID \"{clean}\"")).map_err(|e| map_error(&e))?;
        Ok(hits.into_iter().min())
    }

    /// Runs a command that may answer with `COPYUID` (`UID MOVE`, `UID COPY`) and returns
    /// the `(uidvalidity, source uids, destination uids)` it reported, if any.
    fn run_copyuid(&mut self, command: &str) -> Result<Option<(u32, Vec<u32>, Vec<u32>)>, imap::Error> {
        let (lines, _) = self.session().run(command)?;
        Ok(parse_copyuid(&lines))
    }

    /// `CREATE`s a folder; an existing folder is not an error.
    fn create_folder(&mut self, path: &str) -> Result<(), ProviderError> {
        match self.session().create(path) {
            Ok(()) => {
                info!("imap: created folder {path}");
                self.relist_pending = true;
                Ok(())
            }
            Err(imap::Error::No(no)) if no.information.to_ascii_uppercase().contains("ALREADYEXISTS") => Ok(()),
            Err(e) => Err(map_error(&e)),
        }
    }

    fn uid_search_all(&mut self) -> Result<BTreeSet<u32>, ProviderError> {
        Ok(self.session().uid_search("ALL").map_err(|e| map_error(&e))?.into_iter().collect())
    }

    /// UIDs above `highest` in the open folder (`UID FETCH <highest+1>:* (UID)`). A server
    /// answers `n:*` with its highest message even when that is below `n`, hence the filter.
    fn uids_above(&mut self, highest: u32) -> Result<BTreeSet<u32>, ProviderError> {
        let fetches = self.session().uid_fetch(format!("{}:*", highest.saturating_add(1)), "(UID)").map_err(|e| map_error(&e))?;
        Ok(fetches.iter().filter_map(|f| f.uid).filter(|u| *u > highest).collect())
    }

    /// `UID FETCH <uids> (UID FLAGS) (CHANGEDSINCE <modseq> [VANISHED])`: the UIDs whose
    /// flags changed since `modseq` (with their current `\Seen` / `\Flagged` state) and,
    /// with `vanished` (QRESYNC enabled), the UIDs of the set that were expunged.
    fn changed_since(&mut self, uids: &BTreeSet<u32>, modseq: u64, vanished: bool) -> Result<ChangedSince, ProviderError> {
        let mut out = ChangedSince::default();
        let modifier = if vanished { format!("(CHANGEDSINCE {modseq} VANISHED)") } else { format!("(CHANGEDSINCE {modseq})") };
        for chunk in uidset::chunks(uids, FETCH_CHUNK) {
            let fetches = self.session().uid_fetch(&chunk, format!("(UID FLAGS) {modifier}")).map_err(|e| map_error(&e))?;
            for f in fetches.iter() {
                let Some(uid) = f.uid else { continue };
                out.changed.insert(uid);
                if f.flags().contains(&Flag::Seen) {
                    out.seen.insert(uid);
                }
                if f.flags().contains(&Flag::Flagged) {
                    out.flagged.insert(uid);
                }
            }
            for response in self.session().take_all_unsolicited() {
                if let UnsolicitedResponse::Vanished { uids, .. } = response {
                    for range in uids {
                        out.vanished.extend(range);
                    }
                }
            }
        }
        Ok(out)
    }

    /// Keeps the selectable folders of a listing for search and the Sent `APPEND`.
    fn remember_folders(&mut self, classified: &[(FolderInfo, LabelKind)]) {
        self.sent_path = classified.iter().find(|(_, k)| *k == LabelKind::Sent).map(|(f, _)| f.path.clone());
        self.folders = classified.iter().filter(|(f, _)| f.selectable()).map(|(f, k)| (f.path.clone(), *k)).collect();
    }

    /// `(UID FLAGS)` for a set, as `(seen, flagged)`, plus every UID the server answered for.
    fn flags_for(&mut self, uids: &BTreeSet<u32>) -> Result<(BTreeSet<u32>, BTreeSet<u32>, BTreeSet<u32>), ProviderError> {
        let (mut present, mut seen, mut flagged) = (BTreeSet::new(), BTreeSet::new(), BTreeSet::new());
        for chunk in uidset::chunks(uids, FETCH_CHUNK) {
            let fetches = self.session().uid_fetch(&chunk, "(UID FLAGS)").map_err(|e| map_error(&e))?;
            for f in fetches.iter() {
                let Some(uid) = f.uid else { continue };
                present.insert(uid);
                if f.flags().contains(&Flag::Seen) {
                    seen.insert(uid);
                }
                if f.flags().contains(&Flag::Flagged) {
                    flagged.insert(uid);
                }
            }
        }
        Ok((present, seen, flagged))
    }

    fn list_folders(&mut self) -> Result<Vec<FolderInfo>, ProviderError> {
        let names = self.session().list(Some(""), Some("*")).map_err(|e| map_error(&e))?;
        let mut folders: Vec<FolderInfo> = names.iter().map(FolderInfo::from_name).collect();
        // Subscriptions are informational; a server without LSUB support is not an error.
        if let Ok(subscribed) = self.session().lsub(Some(""), Some("*")) {
            let set: HashSet<String> = subscribed.iter().map(|n| n.name().to_owned()).collect();
            for f in &mut folders {
                f.subscribed = set.contains(&f.path);
            }
        }
        Ok(folders)
    }
}

/// What a `CHANGEDSINCE` fetch reported.
#[derive(Debug, Default)]
struct ChangedSince {
    /// UIDs of the set whose MODSEQ is above the cursor (their flags may have changed).
    changed: BTreeSet<u32>,
    seen: BTreeSet<u32>,
    flagged: BTreeSet<u32>,
    /// UIDs of the set the server reported `VANISHED` (QRESYNC only).
    vanished: BTreeSet<u32>,
}

/// How far the last send attempt of a draft got in this process, for `reconcile_send`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SendPhase {
    /// SMTP failed before `DATA` was accepted: nothing left this machine.
    BeforeData,
    /// The body was written and the connection dropped: the server may have queued it.
    AfterData,
    /// SMTP accepted the message.
    Accepted,
}

pub struct ImapProvider {
    servers: MailServers,
    password: String,
    /// SMTP password when it differs from the incoming one.
    outgoing_password: Option<String>,
    inbox_limit: usize,
    timeout: Duration,
    conn: Mutex<Conn>,
    send_phases: Mutex<HashMap<String, SendPhase>>,
}

impl std::fmt::Debug for ImapProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The password is deliberately absent.
        f.debug_struct("ImapProvider").field("incoming", &self.servers.incoming).field("inbox_limit", &self.inbox_limit).finish()
    }
}

impl ImapProvider {
    pub fn new(servers: MailServers, password: impl Into<String>) -> Self {
        Self {
            servers,
            password: password.into(),
            outgoing_password: None,
            inbox_limit: 100,
            timeout: DEFAULT_TIMEOUT,
            conn: Mutex::new(Conn { session: None, selected: None, caps: Capabilities::default(), relist_pending: false, sent_path: None, folders: Vec::new() }),
            send_phases: Mutex::new(HashMap::new()),
        }
    }

    /// The SMTP password, when "use the same password" was not chosen at setup.
    pub fn with_outgoing_password(mut self, password: Option<String>) -> Self {
        self.outgoing_password = password.filter(|p| !p.is_empty());
        self
    }

    fn outgoing_password(&self) -> &str {
        self.outgoing_password.as_deref().unwrap_or(&self.password)
    }

    /// Newest Inbox messages to list on first download; other folders scale with it, with
    /// the same split the Gmail provider uses.
    pub fn with_inbox_limit(mut self, inbox_limit: usize) -> Self {
        self.inbox_limit = inbox_limit.max(20);
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn servers(&self) -> &MailServers {
        &self.servers
    }

    /// Starts the account's `IDLE` thread on its own connection (see [`idle`]). The
    /// callback runs on that thread; the app turns it into a sync request.
    pub fn idle_watcher(&self, on_event: impl Fn(idle::IdleEvent) + Send + 'static) -> idle::IdleWatcher {
        idle::watch(self.servers.incoming.clone(), self.password.clone(), self.timeout, on_event)
    }

    /// Server extensions seen at login, once connected.
    pub fn capabilities(&self) -> Capabilities {
        self.conn.lock().map(|c| c.caps).unwrap_or_default()
    }

    fn folder_limit(&self, kind: LabelKind) -> usize {
        let scale = self.inbox_limit as f32 / 100.;
        let base: usize = match kind {
            LabelKind::Inbox => return self.inbox_limit,
            LabelKind::Sent | LabelKind::Archive => 40,
            LabelKind::Trash | LabelKind::User => 20,
            LabelKind::Spam => 10,
            _ => 20,
        };
        ((base as f32) * scale).round().max(10.) as usize
    }

    fn ensure_connected(&self, conn: &mut Conn) -> Result<(), ProviderError> {
        if conn.session.is_some() {
            return Ok(());
        }
        let mut session = connection::connect(&self.servers.incoming, &self.password, self.timeout)?;
        let caps = session.capabilities().map_err(|e| map_error(&e))?;
        let mut capabilities = Capabilities {
            special_use: caps.has_str("SPECIAL-USE"),
            move_: caps.has_str("MOVE"),
            uidplus: caps.has_str("UIDPLUS"),
            condstore: caps.has_str("CONDSTORE"),
            qresync: false,
            idle: caps.has_str("IDLE"),
            literal_plus: caps.has_str("LITERAL+") || caps.has_str("LITERAL-"),
        };
        // QRESYNC must be enabled per connection (RFC 7162); a server that answers without
        // `* ENABLED QRESYNC` gets the CONDSTORE path, or the plain one.
        if caps.has_str("ENABLE") && caps.has_str("QRESYNC") {
            match session.run("ENABLE QRESYNC") {
                Ok((lines, _)) => {
                    let text = String::from_utf8_lossy(&lines).to_ascii_uppercase();
                    capabilities.qresync = text.contains("ENABLED") && text.contains("QRESYNC");
                    capabilities.condstore |= capabilities.qresync;
                }
                Err(e) => debug!("imap: ENABLE QRESYNC failed ({e}); using CONDSTORE or polling"),
            }
        }
        conn.caps = capabilities;
        conn.session = Some(session);
        conn.selected = None;
        Ok(())
    }

    /// Whether the next change cycle should re-list folders (a write created one).
    pub fn relist_pending(&self) -> bool {
        self.conn.lock().map(|c| c.relist_pending).unwrap_or(false)
    }

    /// Runs `f` on the account's connection, opening it when needed. A connection that was
    /// reused and turns out dead is reopened once; the closure must therefore be safe to run
    /// twice (build results locally, return them).
    fn with_session<T>(&self, mut f: impl FnMut(&mut Conn) -> Result<T, ProviderError>) -> Result<T, ProviderError> {
        let mut guard = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        let reused = guard.session.is_some();
        self.ensure_connected(&mut guard)?;
        let drop_session = |g: &mut Conn| {
            g.session = None;
            g.selected = None;
        };
        match f(&mut guard) {
            Err(ProviderError::Offline) if reused => {
                drop_session(&mut guard);
                self.ensure_connected(&mut guard)?;
                let result = f(&mut guard);
                if matches!(result, Err(ProviderError::Offline)) {
                    drop_session(&mut guard);
                }
                result
            }
            Err(ProviderError::Offline) => {
                drop_session(&mut guard);
                Err(ProviderError::Offline)
            }
            other => other,
        }
    }

    /// Closes the connection politely. The next call reconnects.
    pub fn disconnect(&self) {
        if let Ok(mut guard) = self.conn.lock() {
            if let Some(mut s) = guard.session.take() {
                let _ = s.logout();
            }
            guard.selected = None;
        }
    }

    fn local_id(account: &Account) -> MessageId {
        MessageId(format!("{}:{}", account.id.0, MessageId::generate().0))
    }

    fn flags_of(fetch: &imap::types::Fetch<'_>, label: &LabelId, provider_id: ProviderId) -> MessageFlags {
        MessageFlags {
            provider_id: Some(provider_id),
            label_ids: [label.clone()].into_iter().collect(),
            is_read: fetch.flags().contains(&Flag::Seen),
            is_starred: fetch.flags().contains(&Flag::Flagged),
            received_at: fetch.internal_date().map(|d| d.timestamp_millis()),
        }
    }

    /// Header-only rows for `uids` in the examined folder (`BODY.PEEK[HEADER]`), newest
    /// first. Also reports each UID's flags.
    fn fetch_headers(conn: &mut Conn, account: &Account, path: &str, uidvalidity: u32, uids: &BTreeSet<u32>) -> Result<(Vec<Message>, BTreeSet<u32>, BTreeSet<u32>), ProviderError> {
        let label = folders::label_id(&account.id, path);
        let mut rows = Vec::new();
        let (mut seen, mut flagged) = (BTreeSet::new(), BTreeSet::new());
        for chunk in uidset::chunks(uids, FETCH_CHUNK / 4) {
            let fetches = conn.session().uid_fetch(&chunk, "(UID FLAGS INTERNALDATE RFC822.SIZE BODY.PEEK[HEADER])").map_err(|e| map_error(&e))?;
            for f in fetches.iter() {
                if f.flags().contains(&Flag::Draft) { continue; }
                let Some(uid) = f.uid else { continue };
                if f.flags().contains(&Flag::Seen) {
                    seen.insert(uid);
                }
                if f.flags().contains(&Flag::Flagged) {
                    flagged.insert(uid);
                }
                let pid = MessageRef::provider_id(path, uidvalidity, uid);
                let flags = Self::flags_of(f, &label, pid);
                rows.push(headers_from_rfc822(account, f.header().unwrap_or_default(), Self::local_id(account), &flags));
            }
        }
        rows.sort_by(|a, b| b.date.cmp(&a.date));
        Ok((rows, seen, flagged))
    }

    /// Full messages for `uids` in the examined folder. Large messages take the
    /// `BODYSTRUCTURE` path. UIDs the server no longer has are skipped.
    fn fetch_bodies(conn: &mut Conn, account: &Account, path: &str, uidvalidity: u32, uids: &BTreeSet<u32>) -> Result<Vec<Message>, ProviderError> {
        let label = folders::label_id(&account.id, path);
        let mut out = Vec::new();
        let mut sizes: HashMap<u32, u32> = HashMap::new();
        for chunk in uidset::chunks(uids, FETCH_CHUNK) {
            let fetches = conn.session().uid_fetch(&chunk, "(UID FLAGS RFC822.SIZE)").map_err(|e| map_error(&e))?;
            for f in fetches.iter() {
                if f.flags().contains(&Flag::Draft) { continue; }
                if let Some(uid) = f.uid {
                    sizes.insert(uid, f.size.unwrap_or(0));
                }
            }
        }
        let small: BTreeSet<u32> = sizes.iter().filter(|(_, s)| **s < structure::LARGE_MESSAGE_BYTES).map(|(u, _)| *u).collect();
        let large: Vec<u32> = sizes.iter().filter(|(_, s)| **s >= structure::LARGE_MESSAGE_BYTES).map(|(u, _)| *u).collect();
        for chunk in uidset::chunks(&small, 20) {
            let fetches = conn.session().uid_fetch(&chunk, "(UID FLAGS INTERNALDATE RFC822.SIZE BODY.PEEK[])").map_err(|e| map_error(&e))?;
            for f in fetches.iter() {
                let (Some(uid), Some(body)) = (f.uid, f.body()) else { continue };
                if f.flags().contains(&Flag::Draft) { continue; }
                let flags = Self::flags_of(f, &label, MessageRef::provider_id(path, uidvalidity, uid));
                out.push(message_from_rfc822(account, body, Self::local_id(account), &flags));
            }
        }
        for uid in large {
            if let Some(m) = Self::fetch_large(conn, account, path, uidvalidity, uid, &label)? {
                out.push(m);
            }
        }
        Ok(out)
    }

    fn fetch_large(conn: &mut Conn, account: &Account, path: &str, uidvalidity: u32, uid: u32, label: &LabelId) -> Result<Option<Message>, ProviderError> {
        let fetches = conn.session().uid_fetch(uid.to_string(), "(UID FLAGS INTERNALDATE RFC822.SIZE BODYSTRUCTURE BODY.PEEK[HEADER])").map_err(|e| map_error(&e))?;
        let Some(f) = fetches.iter().find(|f| f.uid == Some(uid)) else { return Ok(None) };
        if f.flags().contains(&Flag::Draft) { return Ok(None); }
        let header = f.header().unwrap_or_default().to_vec();
        let flags = Self::flags_of(f, label, MessageRef::provider_id(path, uidvalidity, uid));
        let Some(bs) = f.bodystructure() else {
            warn!("imap: {path} uid {uid} is large and has no BODYSTRUCTURE; storing headers only");
            return Ok(Some(headers_from_rfc822(account, &header, Self::local_id(account), &flags)));
        };
        let plan = structure::plan(bs);
        let sections = plan.text_sections();
        let mut parts: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
        if !sections.is_empty() {
            let query = if plan.single_part {
                format!("(UID BODY.PEEK[TEXT]<0.{}>)", structure::TEXT_PART_LIMIT)
            } else {
                let items: Vec<String> = sections.iter().map(|s| format!("BODY.PEEK[{s}]<0.{}> BODY.PEEK[{s}.MIME]", structure::TEXT_PART_LIMIT)).collect();
                format!("(UID {})", items.join(" "))
            };
            let fetched = conn.session().uid_fetch(uid.to_string(), query).map_err(|e| map_error(&e))?;
            if let Some(f) = fetched.iter().find(|f| f.uid == Some(uid)) {
                if plan.single_part {
                    parts.push((Vec::new(), f.text().unwrap_or_default().to_vec()));
                } else {
                    for s in &sections {
                        let section_path: Vec<u32> = s.split('.').filter_map(|n| n.parse().ok()).collect();
                        let body = f.section(&SectionPath::Part(section_path.clone(), None)).unwrap_or_default().to_vec();
                        let mime = f.section(&SectionPath::Part(section_path, Some(MessageSection::Mime))).unwrap_or_default().to_vec();
                        parts.push((mime, body));
                    }
                }
            }
        }
        let bytes = structure::synthetic_message(&header, &parts, plan.single_part);
        let local_id = Self::local_id(account);
        let mut message = message_from_rfc822(account, &bytes, local_id.clone(), &flags);
        message.attachments = plan
            .attachments
            .iter()
            .enumerate()
            .map(|(n, a)| Attachment {
                id: AttachmentId(format!("{}:att-{n}", local_id.0)),
                message_id: local_id.clone(),
                filename: a.filename.clone().unwrap_or_else(|| format!("attachment-{}", n + 1)),
                mime_type: a.mime.clone(),
                size_bytes: a.size,
                local_path: None,
                provider_id: Some(ProviderId(a.section.clone())),
                content_id: a.content_id.clone(),
            })
            .collect();
        Ok(Some(message))
    }

    /// Lists folders, builds labels and resets `cp.folders` to match (keeping cursors of
    /// folders that are still there). Returns the labels and the provider ids of messages in
    /// folders that disappeared.
    fn relist(&self, conn: &mut Conn, account: &Account, cp: &mut Checkpoint) -> Result<(Vec<Label>, Vec<ProviderId>), ProviderError> {
        let listed = conn.list_folders()?;
        let classified = folders::classify(&listed, self.servers.imap.as_ref());
        let labels = folders::labels_for(&account.id, &classified);
        let mut deleted = Vec::new();
        let mut next: BTreeMap<String, FolderCursor> = BTreeMap::new();
        for (info, kind) in &classified {
            let mut cursor = cp.folders.remove(&info.path).unwrap_or_else(|| FolderCursor { fresh: true, ..Default::default() });
            cursor.kind = kind.as_key().to_owned();
            cursor.delimiter = info.delimiter.clone();
            cursor.special_use = info.special_use();
            cursor.subscribed = info.subscribed;
            next.insert(info.path.clone(), cursor);
        }
        for (path, gone) in std::mem::take(&mut cp.folders) {
            deleted.extend(gone.known_set().into_iter().map(|uid| MessageRef::provider_id(&path, gone.uidvalidity, uid)));
        }
        cp.folders = next;
        cp.listed_at = now();
        conn.remember_folders(&classified);
        conn.relist_pending = false;
        Ok((labels, deleted))
    }

    /// Selects the newest N of a folder as its cursor and returns their provider ids,
    /// newest first.
    fn reset_folder(&self, conn: &mut Conn, path: &str, cursor: &mut FolderCursor, uidvalidity: u32, modseq: u64, server: &BTreeSet<u32>) -> Result<Vec<ProviderId>, ProviderError> {
        let limit = self.folder_limit(cursor.label_kind());
        let keep: BTreeSet<u32> = server.iter().rev().take(limit).copied().collect();
        let (_, seen, flagged) = if keep.is_empty() { Default::default() } else { conn.flags_for(&keep)? };
        cursor.uidvalidity = uidvalidity;
        cursor.highest_uid = server.iter().next_back().copied().unwrap_or(0);
        cursor.highest_modseq = if conn.caps.condstore { modseq } else { 0 };
        cursor.set_flags(&keep, &seen, &flagged);
        cursor.fresh = false;
        Ok(keep.iter().rev().map(|uid| MessageRef::provider_id(path, uidvalidity, *uid)).collect())
    }

    fn snapshot(&self, account: &Account) -> Result<MailboxSnapshot, ProviderError> {
        self.with_session(|conn| {
            let mut cp = Checkpoint { version: Checkpoint::VERSION, ..Default::default() };
            let (labels, _) = self.relist(conn, account, &mut cp)?;
            let mut ids: Vec<ProviderId> = Vec::new();
            // Inbox first so the first bodies downloaded are the ones shown first.
            let mut order: Vec<String> = cp.folders.keys().cloned().collect();
            order.sort_by_key(|p| labels.iter().position(|l| l.provider_id.as_ref().map(|x| x.0.as_str()) == Some(p.as_str())).unwrap_or(usize::MAX));
            for path in order {
                let Some(cursor) = cp.folders.get_mut(&path) else { continue };
                let mailbox = match conn.examine(&path) {
                    Ok(m) => m,
                    Err(ProviderError::Rejected(reason)) => {
                        warn!("imap: skipping folder {path}: {reason}");
                        continue;
                    }
                    Err(e) => return Err(e),
                };
                let uidvalidity = mailbox.uid_validity.unwrap_or(0);
                let server = if mailbox.exists == 0 { BTreeSet::new() } else { conn.uid_search_all()? };
                ids.extend(self.reset_folder(conn, &path, cursor, uidvalidity, mailbox.highest_mod_seq.unwrap_or(0), &server)?);
            }
            info!("imap: {} folders, {} recent messages queued for download", cp.folders.len(), ids.len());
            Ok(MailboxSnapshot { labels, messages: Vec::new(), message_ids: ids, checkpoint: Some(cp.to_json()) })
        })
    }

    fn changes(&self, account: &Account, checkpoint: &str) -> Result<RemoteChanges, ProviderError> {
        let base = Checkpoint::parse(checkpoint)?;
        self.with_session(|conn| {
            let mut cp = base.clone();
            cp.cycle += 1;
            let mut changes = RemoteChanges::default();
            if cp.folders.is_empty() || conn.relist_pending || now() - cp.listed_at > LIST_INTERVAL_MS {
                let (labels, deleted) = self.relist(conn, account, &mut cp)?;
                changes.labels = Some(labels);
                changes.deleted_provider_ids.extend(deleted);
            }
            let mut vanished: Vec<String> = Vec::new();
            let mut relist_needed = false;
            let paths: Vec<String> = cp.folders.keys().cloned().collect();
            for path in paths {
                let cursor = cp.folders.get_mut(&path).expect("present");
                let mailbox = match conn.examine(&path) {
                    Ok(m) => m,
                    Err(ProviderError::Rejected(reason)) => {
                        info!("imap: folder {path} is gone ({reason}); removing its messages");
                        changes.deleted_provider_ids.extend(cursor.known_set().into_iter().map(|uid| MessageRef::provider_id(&path, cursor.uidvalidity, uid)));
                        vanished.push(path.clone());
                        relist_needed = true;
                        continue;
                    }
                    Err(e) => return Err(e),
                };
                let uidvalidity = mailbox.uid_validity.unwrap_or(0);
                let server_modseq = mailbox.highest_mod_seq.unwrap_or(0);
                if cursor.fresh || cursor.uidvalidity != uidvalidity {
                    if !cursor.fresh {
                        info!("imap: UIDVALIDITY of {path} changed; re-listing the folder");
                        changes.deleted_provider_ids.extend(cursor.known_set().into_iter().map(|uid| MessageRef::provider_id(&path, cursor.uidvalidity, uid)));
                    }
                    let server = if mailbox.exists == 0 { BTreeSet::new() } else { conn.uid_search_all()? };
                    changes.message_ids.extend(self.reset_folder(conn, &path, cursor, uidvalidity, server_modseq, &server)?);
                    continue;
                }
                let mut known = cursor.known_set();
                let (mut seen, mut flagged) = (uidset::decode(&cursor.seen), uidset::decode(&cursor.flagged));
                let limit = self.folder_limit(cursor.label_kind()).max(200);
                // The fast path needs CONDSTORE on both sides of the cursor; QRESYNC (enabled
                // on this connection) additionally makes VANISHED report deletions.
                let fast = conn.caps.condstore && cursor.highest_modseq > 0 && server_modseq > 0;
                let qresync = fast && conn.caps.qresync;
                // Without QRESYNC the whole UID list is the only way to see deletions.
                let server_all: Option<BTreeSet<u32>> = if qresync {
                    None
                } else if mailbox.exists == 0 {
                    Some(BTreeSet::new())
                } else {
                    Some(conn.uid_search_all()?)
                };
                let since = if fast && server_modseq != cursor.highest_modseq && !known.is_empty() { conn.changed_since(&known, cursor.highest_modseq, qresync)? } else { ChangedSince::default() };
                // Deletions: known locally, gone from the server.
                let gone: BTreeSet<u32> = match &server_all {
                    Some(all) => known.difference(all).copied().collect(),
                    None => since.vanished.intersection(&known).copied().collect(),
                };
                changes.deleted_provider_ids.extend(gone.iter().map(|uid| MessageRef::provider_id(&path, uidvalidity, *uid)));
                known.retain(|u| !gone.contains(u));
                // New mail: above the highest UID seen, newest first, bounded per cycle.
                let new: BTreeSet<u32> = match &server_all {
                    Some(all) => all.iter().rev().filter(|u| **u > cursor.highest_uid).take(limit).copied().collect(),
                    None if mailbox.uid_next.is_some_and(|next| next <= cursor.highest_uid.saturating_add(1)) => BTreeSet::new(),
                    None => conn.uids_above(cursor.highest_uid)?.into_iter().rev().take(limit).collect(),
                };
                if !new.is_empty() {
                    let (rows, new_seen, new_flagged) = Self::fetch_headers(conn, account, &path, uidvalidity, &new)?;
                    changes.message_ids.extend(rows.iter().filter_map(|m| m.provider_id.clone()));
                    changes.upserts.extend(rows);
                    known.extend(new.iter().copied());
                    seen.extend(new_seen);
                    flagged.extend(new_flagged);
                }
                let old: BTreeSet<u32> = known.difference(&new).copied().collect();
                if fast {
                    // Flag changes: only what CHANGEDSINCE reported, re-fetched when the
                    // \Seen or \Flagged state actually differs from the cursor.
                    let touched: BTreeSet<u32> = since.changed.intersection(&old).copied().collect();
                    let flipped: BTreeSet<u32> = touched.iter().filter(|u| seen.contains(u) != since.seen.contains(u) || flagged.contains(u) != since.flagged.contains(u)).copied().collect();
                    if !flipped.is_empty() {
                        let (rows, _, _) = Self::fetch_headers(conn, account, &path, uidvalidity, &flipped)?;
                        changes.upserts.extend(rows);
                    }
                    for uid in &touched {
                        if since.seen.contains(uid) {
                            seen.insert(*uid);
                        } else {
                            seen.remove(uid);
                        }
                        if since.flagged.contains(uid) {
                            flagged.insert(*uid);
                        } else {
                            flagged.remove(uid);
                        }
                    }
                } else {
                    // Flag changes: the Inbox every cycle, other folders now and then.
                    let refresh = cursor.label_kind() == LabelKind::Inbox || cp.cycle % FLAG_REFRESH_EVERY == 0;
                    if refresh && !old.is_empty() {
                        let (_, now_seen, now_flagged) = conn.flags_for(&old)?;
                        let changed: BTreeSet<u32> = old.iter().filter(|u| seen.contains(u) != now_seen.contains(u) || flagged.contains(u) != now_flagged.contains(u)).copied().collect();
                        if !changed.is_empty() {
                            let (rows, _, _) = Self::fetch_headers(conn, account, &path, uidvalidity, &changed)?;
                            changes.upserts.extend(rows);
                        }
                        seen = seen.difference(&old).copied().chain(now_seen).collect();
                        flagged = flagged.difference(&old).copied().chain(now_flagged).collect();
                    }
                }
                let top = match &server_all {
                    Some(all) => all.iter().next_back().copied(),
                    None => mailbox.uid_next.map(|n| n.saturating_sub(1)).or_else(|| new.iter().next_back().copied()),
                };
                cursor.highest_uid = cursor.highest_uid.max(top.unwrap_or(0));
                cursor.highest_modseq = if conn.caps.condstore { server_modseq } else { 0 };
                cursor.set_flags(&known, &seen, &flagged);
            }
            for path in vanished {
                cp.folders.remove(&path);
            }
            if relist_needed {
                let (labels, deleted) = self.relist(conn, account, &mut cp)?;
                changes.labels = Some(labels);
                changes.deleted_provider_ids.extend(deleted);
            }
            changes.checkpoint = Some(cp.to_json());
            Ok(changes)
        })
    }

    fn messages(&self, account: &Account, ids: &[ProviderId]) -> Result<Vec<Message>, ProviderError> {
        // Group by folder and UIDVALIDITY, keeping the first-seen order of folders.
        let mut groups: Vec<((String, u32), BTreeSet<u32>)> = Vec::new();
        for id in ids {
            let Some(r) = MessageRef::parse(id) else {
                warn!("imap: ignoring malformed provider id {:?}", id.0);
                continue;
            };
            let key = (r.folder, r.uidvalidity);
            match groups.iter_mut().find(|(k, _)| *k == key) {
                Some((_, set)) => {
                    set.insert(r.uid);
                }
                None => groups.push((key, [r.uid].into_iter().collect())),
            }
        }
        self.with_session(|conn| {
            let mut out = Vec::new();
            for ((path, uidvalidity), uids) in &groups {
                if !conn.examine_for(path, *uidvalidity)? {
                    info!("imap: {path} changed under us; {} queued messages skipped until the next sync", uids.len());
                    continue;
                }
                out.extend(Self::fetch_bodies(conn, account, path, *uidvalidity, uids)?);
            }
            Ok(out)
        })
    }

    /// `UID SEARCH TEXT` for every word, folder by folder (Inbox first, Spam and Trash
    /// skipped), newest UIDs first within a folder, up to `limit` provider ids.
    fn search_server(&self, query: &str, limit: usize) -> Result<Vec<ProviderId>, ProviderError> {
        let words: Vec<String> = query.split_whitespace().map(|w| w.trim_matches('"').to_owned()).filter(|w| !w.is_empty()).collect();
        if words.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        self.with_session(|conn| {
            if conn.folders.is_empty() {
                let listed = conn.list_folders()?;
                let classified = folders::classify(&listed, self.servers.imap.as_ref());
                conn.remember_folders(&classified);
            }
            let criteria = search_criteria(&words, conn.caps.literal_plus);
            if criteria.is_empty() {
                return Ok(Vec::new());
            }
            let mut out: Vec<ProviderId> = Vec::new();
            let folders = conn.folders.clone();
            for (path, _) in folders.iter().filter(|(_, kind)| !matches!(kind, LabelKind::Spam | LabelKind::Trash)) {
                let mailbox = match conn.examine(path) {
                    Ok(m) => m,
                    Err(ProviderError::Rejected(_)) => continue,
                    Err(e) => return Err(e),
                };
                if mailbox.exists == 0 {
                    continue;
                }
                let validity = mailbox.uid_validity.unwrap_or(0);
                let hits = match conn.session().uid_search(&criteria) {
                    Ok(hits) => hits,
                    // A server that cannot search this way (charset, TEXT) says so per folder.
                    Err(imap::Error::No(no)) => {
                        warn!("imap: search in {path} refused: {}", no.information);
                        continue;
                    }
                    Err(imap::Error::Bad(bad)) => {
                        warn!("imap: search in {path} rejected: {}", bad.information);
                        continue;
                    }
                    Err(e) => return Err(map_error(&e)),
                };
                let mut hits: Vec<u32> = hits.into_iter().collect();
                hits.sort_unstable_by(|a, b| b.cmp(a));
                out.extend(hits.into_iter().take(limit - out.len()).map(|uid| MessageRef::provider_id(path, validity, uid)));
                if out.len() >= limit {
                    break;
                }
            }
            Ok(out)
        })
    }

    fn attachment(&self, message: &Message, attachment: &Attachment) -> Result<Vec<u8>, ProviderError> {
        let r = message.provider_id.as_ref().and_then(MessageRef::parse).ok_or_else(|| ProviderError::Rejected("this message has no server copy".into()))?;
        let section = attachment.provider_id.as_ref().map(|p| p.0.clone()).ok_or_else(|| ProviderError::Rejected("this attachment has no section number".into()))?;
        let section_path: Vec<u32> = section.split('.').map(|n| n.parse::<u32>().map_err(|_| ProviderError::Rejected(format!("bad attachment section {section:?}")))).collect::<Result<_, _>>()?;
        self.with_session(|conn| {
            if !conn.examine_for(&r.folder, r.uidvalidity)? {
                return Err(ProviderError::Rejected("the message is no longer in that folder; sync and try again".into()));
            }
            let fetches = conn.session().uid_fetch(r.uid.to_string(), format!("(UID BODY.PEEK[{section}] BODY.PEEK[{section}.MIME])")).map_err(|e| map_error(&e))?;
            let Some(f) = fetches.iter().find(|f| f.uid == Some(r.uid)) else {
                return Err(ProviderError::Rejected("the message is no longer on the server".into()));
            };
            let Some(body) = f.section(&SectionPath::Part(section_path.clone(), None)) else {
                return Err(ProviderError::Rejected("the server returned no data for this attachment".into()));
            };
            let mime = f.section(&SectionPath::Part(section_path.clone(), Some(MessageSection::Mime))).unwrap_or_default();
            Ok(decode_part(mime, body))
        })
    }


    // ------------------------------------------------------------------ writes

    /// `messages` grouped by folder and UIDVALIDITY, in first-seen order. Messages without
    /// a server copy (no parseable provider id) are skipped.
    fn groups(messages: &[Message]) -> Vec<((String, u32), Vec<Item>)> {
        let mut groups: Vec<((String, u32), Vec<Item>)> = Vec::new();
        for m in messages {
            let Some(r) = m.provider_id.as_ref().and_then(MessageRef::parse) else { continue };
            let item = Item { id: m.id.clone(), uid: r.uid, rfc_message_id: m.rfc_message_id.clone() };
            let key = (r.folder, r.uidvalidity);
            match groups.iter_mut().find(|(k, _)| *k == key) {
                Some((_, items)) => items.push(item),
                None => groups.push((key, vec![item])),
            }
        }
        groups
    }

    fn apply_op(&self, account: &Account, op: &PendingOperation, messages: &[Message], labels: &[Label]) -> Result<AppliedChanges, ProviderError> {
        if op.kind.is_calendar() {
            return Err(ProviderError::Rejected("this account has no calendar".into()));
        }
        if op.kind.is_contact() {
            return Err(ProviderError::Rejected("this account cannot store contacts".into()));
        }
        match &op.kind {
            OperationKind::SetRead { read, .. } => self.store_flag(messages, "\\Seen", *read),
            OperationKind::SetStarred { starred, .. } => self.store_flag(messages, "\\Flagged", *starred),
            OperationKind::Archive { .. } => self.move_messages(account, messages, labels, Target::Archive),
            OperationKind::Trash { .. } => self.move_messages(account, messages, labels, Target::Trash),
            OperationKind::Untrash { .. } => self.move_messages(account, messages, labels, Target::Inbox),
            OperationKind::AddLabel { label_id, .. } => self.move_messages(account, messages, labels, Target::Label(label_id.clone())),
            OperationKind::RemoveLabel { label_id, .. } => self.move_messages(account, messages, labels, Target::RemoveLabel(label_id.clone())),
            OperationKind::SendDraft { .. } => Err(ProviderError::Rejected("drafts are sent through the Outbox".into())),
            _ => Err(ProviderError::Rejected("this account cannot apply that change".into())),
        }
    }

    /// `UID STORE ±FLAGS.SILENT (<flag>)` per folder. A folder that is gone or whose
    /// UIDVALIDITY changed is skipped: those rows are stale and the next cycle replaces them.
    fn store_flag(&self, messages: &[Message], flag: &str, on: bool) -> Result<AppliedChanges, ProviderError> {
        let groups = Self::groups(messages);
        self.with_session(|conn| {
            for ((path, uidvalidity), items) in &groups {
                if !conn.select_for(path, *uidvalidity)? {
                    info!("imap: {path} changed under us; {} flag changes skipped", items.len());
                    continue;
                }
                let uids: BTreeSet<u32> = items.iter().map(|i| i.uid).collect();
                for chunk in uidset::chunks(&uids, FETCH_CHUNK) {
                    conn.session().uid_store(&chunk, format!("{}FLAGS.SILENT ({flag})", if on { '+' } else { '-' })).map_err(|e| map_error(&e))?;
                }
            }
            Ok(AppliedChanges::default())
        })
    }

    /// The path a label maps to, from the labels the sync engine passed in.
    fn label_path<'a>(labels: &'a [Label], id: &LabelId) -> Option<&'a str> {
        labels.iter().find(|l| &l.id == id).and_then(|l| l.provider_id.as_ref()).map(|p| p.0.as_str())
    }

    fn kind_path<'a>(labels: &'a [Label], kind: LabelKind) -> Option<&'a str> {
        labels.iter().find(|l| l.kind == kind).and_then(|l| l.provider_id.as_ref()).map(|p| p.0.as_str())
    }

    /// `INBOX<delimiter>` when every folder lives under the Inbox (Courier, some Dovecot
    /// layouts), so a folder created on demand lands beside the others.
    fn namespace_prefix(labels: &[Label]) -> String {
        let paths: Vec<&str> = labels.iter().filter(|l| l.kind != LabelKind::Inbox).filter_map(|l| l.provider_id.as_ref()).map(|p| p.0.as_str()).collect();
        for prefix in ["INBOX.", "INBOX/"] {
            if !paths.is_empty() && paths.iter().all(|p| p.len() > prefix.len() && p[..prefix.len()].eq_ignore_ascii_case(prefix)) {
                return prefix.to_owned();
            }
        }
        String::new()
    }

    /// Where a move goes. `created` is the label to report when the folder has to be made.
    fn resolve_target(&self, account: &Account, target: &Target, labels: &[Label]) -> Result<(Destination, Option<Label>), ProviderError> {
        let policy = self.servers.imap.as_ref();
        let make = |path: String, kind: LabelKind, name: &str| {
            let label = Label { id: folders::label_id(&account.id, &path), account_id: account.id.clone(), kind, name: name.into(), provider_id: Some(ProviderId(path.clone())), color: None, sort_order: labels.len() as i32 };
            (Destination::Path(path), Some(label))
        };
        let known = |path: &str| labels.iter().any(|l| l.provider_id.as_ref().is_some_and(|p| p.0 == path));
        Ok(match target {
            Target::Inbox => (Destination::Path(Self::kind_path(labels, LabelKind::Inbox).unwrap_or("INBOX").to_owned()), None),
            Target::Archive => match policy.and_then(|p| p.archive_folder.clone()).filter(|p| !p.trim().is_empty()) {
                Some(path) if known(&path) => (Destination::Path(path), None),
                Some(path) => make(path, LabelKind::Archive, "Archive"),
                None => match Self::kind_path(labels, LabelKind::Archive) {
                    Some(path) => (Destination::Path(path.to_owned()), None),
                    None => make(format!("{}Archive", Self::namespace_prefix(labels)), LabelKind::Archive, "Archive"),
                },
            },
            Target::Trash => match policy.and_then(|p| p.trash_folder.clone()).filter(|p| !p.trim().is_empty()) {
                Some(path) if known(&path) => (Destination::Path(path), None),
                Some(path) => make(path, LabelKind::Trash, "Trash"),
                None => match Self::kind_path(labels, LabelKind::Trash) {
                    Some(path) => (Destination::Path(path.to_owned()), None),
                    None if policy.is_some_and(|p| p.expunge_on_trash) => (Destination::Expunge, None),
                    None => make(format!("{}Trash", Self::namespace_prefix(labels)), LabelKind::Trash, "Trash"),
                },
            },
            Target::Label(id) | Target::RemoveLabel(id) => match Self::label_path(labels, id) {
                Some(path) => (Destination::Path(path.to_owned()), None),
                None => return Err(ProviderError::Rejected("that label has no folder on the server".into())),
            },
        })
    }

    fn move_messages(&self, account: &Account, messages: &[Message], labels: &[Label], target: Target) -> Result<AppliedChanges, ProviderError> {
        let (destination, created) = self.resolve_target(account, &target, labels)?;
        let inbox = Self::kind_path(labels, LabelKind::Inbox).unwrap_or("INBOX").to_owned();
        let mut groups = Self::groups(messages);
        // Each group moves to one folder: RemoveLabel sends a message in that folder back to
        // the Inbox (or, for the Inbox itself, to the Archive) and leaves the others alone.
        let mut plan: Vec<((String, u32), Vec<Item>, Destination)> = Vec::new();
        for ((path, uidvalidity), items) in groups.drain(..) {
            let dest = match (&target, &destination) {
                (Target::RemoveLabel(_), Destination::Path(removed)) if &path == removed && path.eq_ignore_ascii_case(&inbox) => {
                    let (archive, _) = self.resolve_target(account, &Target::Archive, labels)?;
                    archive
                }
                (Target::RemoveLabel(_), Destination::Path(removed)) if &path == removed => Destination::Path(inbox.clone()),
                (Target::RemoveLabel(_), _) => continue,
                (_, Destination::Path(dest)) if &path == dest => continue,
                (_, dest) => dest.clone(),
            };
            plan.push(((path, uidvalidity), items, dest));
        }
        let created_for_archive = matches!(target, Target::RemoveLabel(_)).then(|| self.resolve_target(account, &Target::Archive, labels).ok().and_then(|(_, l)| l)).flatten();
        let new_label = created.or(created_for_archive);
        if plan.is_empty() {
            return Ok(AppliedChanges::default());
        }
        let label_for = |path: &str| labels.iter().find(|l| l.provider_id.as_ref().is_some_and(|p| p.0 == path)).map(|l| l.id.clone()).unwrap_or_else(|| folders::label_id(&account.id, path));
        self.with_session(|conn| {
            let mut changes = AppliedChanges::default();
            for ((path, uidvalidity), items, dest) in &plan {
                if !conn.select_for(path, *uidvalidity)? {
                    info!("imap: {path} changed under us; {} messages not moved (the next sync resolves them)", items.len());
                    continue;
                }
                match dest {
                    Destination::Expunge => {
                        let uids: BTreeSet<u32> = items.iter().map(|i| i.uid).collect();
                        Self::delete_and_expunge(conn, &uids)?;
                    }
                    Destination::Path(dest) => {
                        let may_create = new_label.as_ref().is_some_and(|l| l.provider_id.as_ref().is_some_and(|p| p.0 == *dest));
                        let moved = self.move_group(conn, path, items, dest, may_create)?;
                        let label = label_for(dest);
                        changes.provider_ids.extend(moved.into_iter().map(|(id, pid)| (id, pid, label.clone())));
                    }
                }
            }
            if let Some(label) = &new_label {
                if changes.provider_ids.iter().any(|(_, _, l)| l == &label.id) {
                    changes.labels.push(label.clone());
                }
            }
            Ok(changes)
        })
    }

    /// `\Deleted` + `UID EXPUNGE` (plain `EXPUNGE` without UIDPLUS).
    fn delete_and_expunge(conn: &mut Conn, uids: &BTreeSet<u32>) -> Result<(), ProviderError> {
        let uidplus = conn.caps.uidplus;
        for chunk in uidset::chunks(uids, FETCH_CHUNK) {
            conn.session().uid_store(&chunk, "+FLAGS.SILENT (\\Deleted)").map_err(|e| map_error(&e))?;
            if uidplus {
                conn.session().uid_expunge(&chunk).map_err(|e| map_error(&e))?;
            }
        }
        if !uidplus {
            conn.session().expunge().map_err(|e| map_error(&e))?;
        }
        Ok(())
    }

    /// Moves `items` from the open folder `source` to `dest` and returns the new provider
    /// id of every message it could account for. A target the server says to create is
    /// created when `may_create` (the Archive or Trash folder made on demand) and
    /// `Rejected` otherwise. A message the server no longer has in `source` (moved by an
    /// interrupted attempt) or that came back without `COPYUID` is looked up in `dest` by
    /// its Message-ID.
    fn move_group(&self, conn: &mut Conn, source: &str, items: &[Item], dest: &str, may_create: bool) -> Result<Vec<(MessageId, ProviderId)>, ProviderError> {
        let uids: BTreeSet<u32> = items.iter().map(|i| i.uid).collect();
        let quoted = quote_mailbox(dest);
        let mut mapping: HashMap<u32, (u32, u32)> = HashMap::new();
        for chunk in uidset::chunks(&uids, FETCH_CHUNK) {
            let command = if conn.caps.move_ { format!("UID MOVE {chunk} {quoted}") } else { format!("UID COPY {chunk} {quoted}") };
            let copyuid = match conn.run_copyuid(&command) {
                Ok(c) => c,
                Err(imap::Error::No(no)) if may_create && no.code == Some(ResponseCode::TryCreate) => {
                    conn.create_folder(dest)?;
                    conn.run_copyuid(&command).map_err(|e| map_error(&e))?
                }
                Err(e) => return Err(map_error(&e)),
            };
            if !conn.caps.move_ {
                let set: BTreeSet<u32> = uidset::decode(&chunk);
                Self::delete_and_expunge(conn, &set)?;
            }
            if let Some((validity, from, to)) = copyuid {
                for (f, t) in from.into_iter().zip(to) {
                    mapping.insert(f, (validity, t));
                }
            }
        }
        let mut out: Vec<(MessageId, ProviderId)> = Vec::new();
        let mut unresolved: Vec<&Item> = Vec::new();
        for item in items {
            match mapping.get(&item.uid) {
                Some((validity, uid)) => out.push((item.id.clone(), MessageRef::provider_id(dest, *validity, *uid))),
                None => unresolved.push(item),
            }
        }
        if unresolved.iter().any(|i| i.rfc_message_id.is_some()) {
            let validity = conn.select(dest)?.uid_validity.unwrap_or(0);
            for item in unresolved {
                let Some(rfc) = &item.rfc_message_id else { continue };
                if let Some(uid) = conn.find_by_message_id(rfc)? {
                    out.push((item.id.clone(), MessageRef::provider_id(dest, validity, uid)));
                }
            }
        }
        info!("imap: moved {} of {} messages from {source} to {dest}", out.len(), items.len());
        Ok(out)
    }

    // ----------------------------------------------------------------- sending

    fn set_phase(&self, draft: &Draft, phase: SendPhase) {
        if let Ok(mut phases) = self.send_phases.lock() {
            phases.insert(draft.id.0.clone(), phase);
        }
    }

    /// The Sent folder: the policy override, then the last listing, then a fresh `LIST`.
    fn sent_folder(&self, conn: &mut Conn) -> Result<Option<String>, ProviderError> {
        if let Some(path) = self.servers.imap.as_ref().and_then(|p| p.sent_folder.clone()).filter(|p| !p.trim().is_empty()) {
            return Ok(Some(path));
        }
        if conn.sent_path.is_none() {
            let listed = conn.list_folders()?;
            let classified = folders::classify(&listed, self.servers.imap.as_ref());
            conn.sent_path = classified.iter().find(|(_, k)| *k == LabelKind::Sent).map(|(f, _)| f.path.clone());
        }
        Ok(conn.sent_path.clone())
    }

    fn send_draft(&self, account: &Account, draft: &Draft, context: SendContext) -> Result<SentReceipt, ProviderError> {
        let rfc_id = draft_message_id(draft);
        let raw = build_raw(account, draft, context.in_reply_to.as_ref(), &rfc_id).map_err(ProviderError::Rejected)?;
        let recipients: Vec<String> = draft.to.iter().chain(&draft.cc).chain(&draft.bcc).map(|a| a.email.trim().to_owned()).filter(|e| !e.is_empty()).collect();
        let transport = smtp::Transport::new(self.servers.outgoing.clone(), self.outgoing_password()).with_timeout(self.timeout);
        self.set_phase(draft, SendPhase::BeforeData);
        match transport.send(&account.email, &recipients, &raw) {
            Ok(reply) => info!("smtp: {} accepted {:?}: {reply}", self.servers.outgoing.host, draft.display_subject()),
            Err(ProviderError::Uncertain) => {
                self.set_phase(draft, SendPhase::AfterData);
                return Err(ProviderError::Uncertain);
            }
            Err(e) => return Err(e),
        }
        self.set_phase(draft, SendPhase::Accepted);

        // The Sent copy: APPEND the same bytes; APPENDUID names the server copy.
        let from_labels = Self::kind_path(&context.labels, LabelKind::Sent).map(str::to_owned);
        let appended = self.with_session(|conn| {
            let path = match from_labels.clone() {
                Some(p) => Some(p),
                None => self.sent_folder(conn)?,
            };
            let Some(path) = path else { return Ok(None) };
            // This closure may run twice (a dropped connection is reopened once): an APPEND
            // whose reply was lost must not be repeated.
            let validity = conn.select(&path)?.uid_validity.unwrap_or(0);
            if let Some(uid) = conn.find_by_message_id(&rfc_id)? {
                return Ok(Some(MessageRef::provider_id(&path, validity, uid)));
            }
            let result = conn.session().append(&path, &raw).flag(Flag::Seen).finish();
            match result {
                Ok(appended) => {
                    let uid = appended.uids.as_ref().and_then(|u| u.first()).map(|m| match m {
                        UidSetMember::Uid(u) => *u,
                        UidSetMember::UidRange(r) => *r.start(),
                    });
                    match (appended.uid_validity, uid) {
                        (Some(validity), Some(uid)) => Ok(Some(MessageRef::provider_id(&path, validity, uid))),
                        // No UIDPLUS: look the copy up by its Message-ID.
                        _ => Ok(conn.find_by_message_id(&rfc_id)?.map(|uid| MessageRef::provider_id(&path, validity, uid))),
                    }
                }
                Err(e) => Err(map_error(&e)),
            }
        });
        let provider_id = match appended {
            Ok(Some(pid)) => pid,
            Ok(None) => {
                warn!("imap: no Sent folder; keeping a local copy of {:?}", draft.display_subject());
                ProviderId(format!("{LOCAL_PLACEHOLDER}{rfc_id}"))
            }
            Err(e) => {
                warn!("imap: the message was sent but could not be copied to Sent ({e}); keeping a local copy");
                ProviderId(format!("{LOCAL_PLACEHOLDER}{rfc_id}"))
            }
        };
        let sent_label = context.sent_label.clone().or_else(|| from_labels.as_ref().map(|p| folders::label_id(&account.id, p)));
        let mut message = crate::sample::sent_message(account, draft, context.in_reply_to.as_ref(), &provider_id, &rfc_id, sent_label.as_ref(), now());
        message.id = Self::local_id(account);
        if let Some(source) = &context.in_reply_to {
            message.in_reply_to = source.rfc_message_id.clone();
            message.references = source.references.iter().cloned().chain(source.rfc_message_id.clone()).collect();
        }
        Ok(SentReceipt { provider_id, rfc_message_id: Some(rfc_id), message })
    }

    fn reconcile(&self, draft: &Draft) -> Result<SendOutcome, ProviderError> {
        let rfc_id = draft_message_id(draft);
        let found = self.with_session(|conn| {
            let Some(path) = self.sent_folder(conn)? else { return Ok(None) };
            let validity = conn.select(&path)?.uid_validity.unwrap_or(0);
            Ok(conn.find_by_message_id(&rfc_id)?.map(|uid| MessageRef::provider_id(&path, validity, uid)))
        })?;
        if let Some(pid) = found {
            return Ok(SendOutcome::Delivered(pid));
        }
        let phase = self.send_phases.lock().ok().and_then(|p| p.get(&draft.id.0).copied());
        Ok(match phase {
            Some(SendPhase::BeforeData) => SendOutcome::NotDelivered,
            // After DATA, or a previous run of the app: the server may have queued it.
            _ => SendOutcome::Unknown,
        })
    }

    fn raw(&self, message: &Message) -> Result<Vec<u8>, ProviderError> {
        let r = message.provider_id.as_ref().and_then(MessageRef::parse).ok_or_else(|| ProviderError::Rejected("this message has no server copy".into()))?;
        self.with_session(|conn| {
            if !conn.examine_for(&r.folder, r.uidvalidity)? {
                return Err(ProviderError::Rejected("the message is no longer in that folder".into()));
            }
            let fetches = conn.session().uid_fetch(r.uid.to_string(), "(UID BODY.PEEK[])").map_err(|e| map_error(&e))?;
            fetches.iter().find(|f| f.uid == Some(r.uid)).and_then(|f| f.body().map(<[u8]>::to_vec)).ok_or_else(|| ProviderError::Rejected("the message is no longer on the server".into()))
        })
    }
}


/// One message of a write operation, inside its folder group.
#[derive(Debug, Clone)]
struct Item {
    id: MessageId,
    uid: u32,
    rfc_message_id: Option<String>,
}

/// What a move operation asks for, before folder resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Target {
    Inbox,
    Archive,
    Trash,
    Label(LabelId),
    RemoveLabel(LabelId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Destination {
    Path(String),
    /// No Trash folder and the policy says expunge: `\Deleted` + `UID EXPUNGE`.
    Expunge,
}

/// A mailbox name as a quoted string (RFC 3501 `quoted`), for commands the `imap` crate
/// passes through unquoted.
fn quote_mailbox(name: &str) -> String {
    format!("\"{}\"", name.replace('\\', "\\\\").replace('"', "\\\""))
}

/// `SEARCH` criteria for `words`: `TEXT "<word>"` per word (all must match). Non-ASCII words
/// need `CHARSET UTF-8` and a literal, which is only sent when the server takes
/// non-synchronising literals (`{n+}`); otherwise those words are left out. Empty when
/// nothing can be searched.
pub fn search_criteria(words: &[String], literal_plus: bool) -> String {
    let mut terms: Vec<String> = Vec::new();
    let mut non_ascii = false;
    for word in words {
        if word.is_ascii() {
            terms.push(format!("TEXT {}", quote_mailbox(word)));
        } else if literal_plus {
            non_ascii = true;
            terms.push(format!("TEXT {{{}+}}\r\n{}", word.len(), word));
        }
    }
    if terms.is_empty() {
        return String::new();
    }
    format!("{}{}", if non_ascii { "CHARSET UTF-8 " } else { "" }, terms.join(" "))
}

fn expand_uid_set(set: &[UidSetMember]) -> Vec<u32> {
    let mut out = Vec::new();
    for m in set {
        match m {
            UidSetMember::Uid(u) => out.push(*u),
            UidSetMember::UidRange(r) => out.extend(r.clone()),
        }
    }
    out
}

/// The `COPYUID` code in a command's response lines (untagged `OK` for `MOVE`, the tagged
/// `OK` for `COPY`): `(uidvalidity, source uids, destination uids)`.
pub fn parse_copyuid(mut lines: &[u8]) -> Option<(u32, Vec<u32>, Vec<u32>)> {
    while !lines.is_empty() {
        let Ok((rest, response)) = imap_proto::parser::parse_response(lines) else { return None };
        let code = match response {
            Response::Done { code, .. } | Response::Data { code, .. } => code,
            _ => None,
        };
        if let Some(ResponseCode::CopyUid(validity, from, to)) = code {
            return Some((validity, expand_uid_set(&from), expand_uid_set(&to)));
        }
        lines = rest;
    }
    None
}

/// Decodes a fetched body part using its MIME headers (transfer encoding, charset) through
/// the shared parser. Falls back to the raw bytes when the headers are unusable.
pub fn decode_part(mime_headers: &[u8], body: &[u8]) -> Vec<u8> {
    if mime_headers.trim_ascii().is_empty() {
        return body.to_vec();
    }
    let mut raw = mime_headers.trim_ascii_end().to_vec();
    raw.extend_from_slice(b"\r\n\r\n");
    raw.extend_from_slice(body);
    let Some(parsed) = mail_parser::MessageParser::default().parse(&raw) else { return body.to_vec() };
    match parsed.parts.first().map(|p| &p.body) {
        Some(mail_parser::PartType::Binary(b)) | Some(mail_parser::PartType::InlineBinary(b)) => b.to_vec(),
        Some(mail_parser::PartType::Text(t)) | Some(mail_parser::PartType::Html(t)) => t.as_bytes().to_vec(),
        Some(mail_parser::PartType::Message(m)) => m.raw_message.to_vec(),
        _ => body.to_vec(),
    }
}

impl MailProvider for ImapProvider {
    fn supports_drafts(&self) -> bool { true }

    fn list_drafts(&self, _account: &Account) -> BoxFuture<'_, Result<Vec<RemoteDraft>, ProviderError>> {
        Box::pin(async move { self.draft_listing() })
    }

    fn fetch_draft(&self, _account: &Account, remote: &RemoteDraft) -> BoxFuture<'_, Result<Option<Vec<u8>>, ProviderError>> {
        let remote = remote.clone();
        Box::pin(async move { self.draft_content(&remote) })
    }

    fn put_draft(&self, _account: &Account, remote: Option<&RemoteDraft>, raw: &[u8], message_id: &str, _thread_id: Option<&str>) -> BoxFuture<'_, Result<Option<RemoteDraft>, ProviderError>> {
        let (remote, raw, message_id) = (remote.cloned(), raw.to_vec(), message_id.to_owned());
        Box::pin(async move { self.write_draft(remote.as_ref(), &raw, &message_id) })
    }

    fn delete_remote_draft(&self, _account: &Account, remote: &RemoteDraft) -> BoxFuture<'_, Result<(), ProviderError>> {
        let remote = remote.clone();
        Box::pin(async move { self.remove_draft(&remote) })
    }
    fn kind(&self) -> ProviderKind {
        ProviderKind::Imap
    }

    fn probe(&self) -> BoxFuture<'_, Result<(), ProviderError>> {
        Box::pin(async move { self.with_session(|conn| conn.session().noop().map_err(|e| map_error(&e))) })
    }

    fn fetch_snapshot(&self, account: &Account) -> BoxFuture<'_, Result<MailboxSnapshot, ProviderError>> {
        let account = account.clone();
        Box::pin(async move { self.snapshot(&account) })
    }

    fn fetch_changes(&self, account: &Account, checkpoint: &str) -> BoxFuture<'_, Result<RemoteChanges, ProviderError>> {
        let account = account.clone();
        let checkpoint = checkpoint.to_owned();
        Box::pin(async move { self.changes(&account, &checkpoint) })
    }

    fn fetch_messages(&self, account: &Account, ids: &[ProviderId], _labels: &[Label]) -> BoxFuture<'_, Result<Vec<Message>, ProviderError>> {
        let account = account.clone();
        let ids = ids.to_vec();
        Box::pin(async move { self.messages(&account, &ids) })
    }

    fn apply(&self, account: &Account, op: &PendingOperation, messages: &[Message], labels: &[Label]) -> BoxFuture<'_, Result<AppliedChanges, ProviderError>> {
        let account = account.clone();
        let op = op.clone();
        let messages = messages.to_vec();
        let labels = labels.to_vec();
        Box::pin(async move { self.apply_op(&account, &op, &messages, &labels) })
    }

    fn fetch_raw(&self, _account: &Account, message: &Message) -> BoxFuture<'_, Result<Vec<u8>, ProviderError>> {
        let message = message.clone();
        Box::pin(async move {
            match self.raw(&message) {
                Ok(bytes) => Ok(bytes),
                Err(ProviderError::Rejected(_)) => Ok(crate::message_to_raw(&message)),
                Err(e) => Err(e),
            }
        })
    }

    fn fetch_attachment(&self, _account: &Account, message: &Message, attachment: &Attachment) -> BoxFuture<'_, Result<Vec<u8>, ProviderError>> {
        let message = message.clone();
        let attachment = attachment.clone();
        Box::pin(async move { self.attachment(&message, &attachment) })
    }

    fn search(&self, _account: &Account, query: &str, limit: usize) -> BoxFuture<'_, Result<Vec<ProviderId>, ProviderError>> {
        let query = query.to_owned();
        Box::pin(async move { self.search_server(&query, limit) })
    }

    fn send(&self, account: &Account, draft: &Draft, context: SendContext) -> BoxFuture<'_, Result<SentReceipt, ProviderError>> {
        let account = account.clone();
        let draft = draft.clone();
        Box::pin(async move { self.send_draft(&account, &draft, context) })
    }

    fn reconcile_send(&self, _account: &Account, draft: &Draft) -> BoxFuture<'_, Result<SendOutcome, ProviderError>> {
        let draft = draft.clone();
        Box::pin(async move { self.reconcile(&draft) })
    }
}

#[cfg(test)]
mod unit {
    use super::*;

    #[test]
    fn provider_ids_round_trip() {
        let id = MessageRef::provider_id("Work/Clients", 1760400000, 42);
        assert_eq!(id.0, "Work/Clients\u{1f}1760400000\u{1f}42");
        assert_eq!(MessageRef::parse(&id), Some(MessageRef { folder: "Work/Clients".into(), uidvalidity: 1760400000, uid: 42 }));
        assert_eq!(MessageRef::parse(&ProviderId("gmail-id".into())), None);
    }

    #[test]
    fn checkpoint_round_trips_and_rejects_other_versions() {
        let mut cp = Checkpoint { version: Checkpoint::VERSION, listed_at: 5, cycle: 2, folders: BTreeMap::new() };
        cp.folders.insert("INBOX".into(), FolderCursor { kind: "inbox".into(), uidvalidity: 7, highest_uid: 9, highest_modseq: 42, known: "3:9".into(), seen: "3:5".into(), flagged: "9".into(), delimiter: Some("/".into()), special_use: None, subscribed: true, fresh: false });
        let json = cp.to_json();
        assert_eq!(Checkpoint::parse(&json).unwrap(), cp);
        assert_eq!(Checkpoint::parse("12345"), Err(ProviderError::CheckpointExpired), "a Gmail history id is not ours");
        assert_eq!(Checkpoint::parse(r#"{"version":99}"#), Err(ProviderError::CheckpointExpired));
        let rows = folder_states(&json);
        assert_eq!(rows.len(), 1);
        assert_eq!((rows[0].path.as_str(), rows[0].uidvalidity, rows[0].highest_uid, rows[0].highest_modseq, rows[0].delimiter.as_deref()), ("INBOX", Some(7), 9, Some(42), Some("/")));
        let old: Checkpoint = Checkpoint::parse(r#"{"version":1,"folders":{"INBOX":{"kind":"inbox","uidvalidity":7,"highest_uid":9,"known":"3:9"}}}"#).unwrap();
        assert_eq!(old.folders["INBOX"].highest_modseq, 0, "a checkpoint written before CONDSTORE support still parses");
    }

    #[test]
    fn folder_limits_scale_with_the_inbox_preference() {
        let servers = MailServers { incoming: ServerEndpoint::new("localhost", 143, Security::None, "j"), outgoing: ServerEndpoint::new("localhost", 25, Security::None, "j"), pop3: None, imap: None };
        let p = ImapProvider::new(servers.clone(), "pw");
        assert_eq!((p.folder_limit(LabelKind::Inbox), p.folder_limit(LabelKind::Sent), p.folder_limit(LabelKind::Trash), p.folder_limit(LabelKind::Spam), p.folder_limit(LabelKind::User)), (100, 40, 20, 10, 20));
        let p = ImapProvider::new(servers, "pw").with_inbox_limit(300);
        assert_eq!((p.folder_limit(LabelKind::Inbox), p.folder_limit(LabelKind::Sent), p.folder_limit(LabelKind::Spam)), (300, 120, 30));
        assert!(!format!("{p:?}").contains("pw"), "the password never appears in Debug output");
    }

    #[test]
    fn search_criteria_quote_ascii_and_use_literals_for_the_rest() {
        let words = |w: &[&str]| w.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        assert_eq!(search_criteria(&words(&["invoice", "q\"uote"]), false), "TEXT \"invoice\" TEXT \"q\\\"uote\"");
        assert_eq!(search_criteria(&words(&["café"]), true), "CHARSET UTF-8 TEXT {5+}\r\ncafé");
        assert_eq!(search_criteria(&words(&["café", "bill"]), false), "TEXT \"bill\"", "a non-ASCII word is dropped without LITERAL+");
        assert_eq!(search_criteria(&words(&["café"]), false), "");
    }

    #[test]
    fn decodes_parts_by_their_mime_headers() {
        assert_eq!(decode_part(b"Content-Type: application/pdf\r\nContent-Transfer-Encoding: base64\r\n\r\n", b"JVBERi0xLjQK"), b"%PDF-1.4\n");
        assert_eq!(decode_part(b"Content-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: quoted-printable\r\n", b"Caf=C3=A9"), "Café".as_bytes());
        assert_eq!(decode_part(b"", b"raw"), b"raw");
    }
}
