//! `MailProvider` for POP3 accounts (plan milestone M4).
//!
//! POP3 is download-and-keep-locally, so this provider is small and the local store is the
//! primary copy:
//!
//! - **Labels are local only.** Inbox, Sent, Trash and Archive are created once, with no
//!   `provider_id`; every mailbox operation is complete once the store transaction commits,
//!   so [`MailProvider::apply`] returns `Ok` at once. The one exception is Trash with
//!   [`Pop3Policy::delete_on_server_when_trashed`], which issues `DELE` by UIDL.
//! - **Identity.** `provider_id` is the server's `UIDL`; the local id is
//!   `<account>:<uuid>`, minted when the UIDL is first listed and recorded in the
//!   checkpoint, so a header-only row from `TOP` and the later full download share it.
//! - **One routine for first and later syncs.** `fetch_snapshot` and `fetch_changes` both
//!   run `UIDL`, diff against the ledger of every UIDL ever listed ([`Checkpoint`]), and
//!   return the unseen ones newest first as `message_ids` for the engine's persisted
//!   backlog, which `RETR`s them through `fetch_messages`. Messages above
//!   [`DEFAULT_TOP_ABOVE`] bytes get a `TOP n 0` header row first. `deleted` is always
//!   empty: mail removed from the server stays local.
//! - **Raw bytes are the record.** Every downloaded message is written to
//!   `<raw dir>/<message id>.eml` before it is parsed; `fetch_raw` and `fetch_attachment`
//!   read that file and never need the server again. A message whose file is on disk is
//!   served from it (so a retried download never asks the server), and a message the
//!   server no longer has is skipped.
//! - **Retention** ([`Pop3Policy`]): `leave_on_server` false, or `delete_after_days` past
//!   since the UIDL was first listed, deletes the server copy with `DELE`, committed by
//!   `QUIT`, at the end of the listing session of the next cycle. Only messages whose
//!   `.eml` is on disk are ever deleted from the server.
//! - **Sending** reuses [`crate::smtp::Transport`]; the receipt's message is the locally
//!   built copy with the local Sent label (and its bytes saved as an `.eml` too). There is
//!   no server Sent folder to search, so `reconcile_send` answers
//!   [`SendOutcome::Unknown`] and the draft stays Uncertain for the user to decide.
//!
//! The ledger is mirrored into the store's `pop3_seen` table by the sync engine after each
//! cycle ([`seen_rows`]); the checkpoint stays authoritative.

pub mod client;

#[cfg(any(test, feature = "fake-servers"))]
pub mod fake;
#[cfg(test)]
mod tests;

use crate::mime::{headers_from_rfc822, message_from_rfc822, MessageFlags};
use crate::{build_raw, draft_message_id, smtp, AppliedChanges, BoxFuture, Credential, MailProvider, MailboxSnapshot, ProviderError, RemoteChanges, SendContext, SendOutcome, SentReceipt};
use client::Client;
use overlook_core::time::now;
use overlook_core::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;
use tracing::{info, warn};

/// Socket connect/read/write timeout.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Messages larger than this get a `TOP n 0` header row before the body download.
pub const DEFAULT_TOP_ABOVE: u64 = 1024 * 1024;

/// Prefix of the provider id of a Sent copy, which exists only locally.
const SENT_PREFIX: &str = "sent\u{1f}";

/// Whether a provider id names a locally built Sent copy rather than a UIDL.
pub fn is_sent_copy(id: &ProviderId) -> bool {
    id.0.starts_with(SENT_PREFIX)
}

// --------------------------------------------------------------------- labels

/// The local-only labels every POP3 account has, in sidebar order.
pub fn labels_for(account: &AccountId) -> Vec<Label> {
    [(LabelKind::Inbox, "Inbox"), (LabelKind::Sent, "Sent"), (LabelKind::Archive, "Archive"), (LabelKind::Trash, "Trash")]
        .into_iter()
        .enumerate()
        .map(|(i, (kind, name))| Label { id: label_id(account, kind), account_id: account.clone(), kind, name: name.into(), provider_id: None, color: None, sort_order: i as i32 })
        .collect()
}

pub fn label_id(account: &AccountId, kind: LabelKind) -> LabelId {
    LabelId(format!("{}:{}", account.0, kind.as_key()))
}

// ----------------------------------------------------------------- checkpoint

/// One UIDL the account has listed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SeenEntry {
    /// The local message id minted for it (`<account>:<uuid>`); also names its `.eml`.
    pub id: String,
    /// When the UIDL was first listed (ms since the epoch).
    pub seen_at: i64,
    /// `LIST` size, when known.
    pub size: u64,
    /// The server no longer lists it.
    pub deleted_on_server: bool,
    /// When the server was first seen not to list it (ms since the epoch); cleared when it
    /// reappears. Drives [`Checkpoint::prune`].
    pub deleted_at: Option<i64>,
}

/// The ledger of every UIDL ever listed: the sync checkpoint. Entries for UIDLs the server
/// stopped listing are pruned once they have been gone for [`Checkpoint::PRUNE_AFTER_MS`],
/// and the oldest of them go first when more than [`Checkpoint::DELETED_CAP`] remain, so the
/// JSON parsed every cycle stays bounded by the maildrop's size.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Checkpoint {
    pub version: u32,
    pub entries: BTreeMap<String, SeenEntry>,
}

impl Checkpoint {
    pub const VERSION: u32 = 1;
    /// How long a UIDL stays in the ledger after the server stopped listing it (30 days):
    /// long enough that a maildrop restored from a backup does not download its mail twice.
    pub const PRUNE_AFTER_MS: i64 = 30 * 86_400_000;
    /// Entries for vanished UIDLs kept at most, oldest pruned first.
    pub const DELETED_CAP: usize = 10_000;

    /// Drops ledger entries for UIDLs that have been gone from the server long enough, and
    /// the oldest gone ones beyond the cap. Entries the server still lists are never
    /// touched. Returns how many were removed.
    pub fn prune(&mut self, now: i64) -> usize {
        let before = self.entries.len();
        self.entries.retain(|_, e| !(e.deleted_on_server && e.deleted_at.is_some_and(|at| now.saturating_sub(at) >= Self::PRUNE_AFTER_MS)));
        let mut gone: Vec<(i64, String)> = self.entries.iter().filter(|(_, e)| e.deleted_on_server).map(|(u, e)| (e.deleted_at.unwrap_or(e.seen_at), u.clone())).collect();
        if gone.len() > Self::DELETED_CAP {
            gone.sort();
            for (_, uidl) in gone.iter().take(gone.len() - Self::DELETED_CAP) {
                self.entries.remove(uidl);
            }
        }
        before - self.entries.len()
    }

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

/// The ledger as `pop3_seen` rows, for the store mirror. An unreadable checkpoint gives no rows.
pub fn seen_rows(checkpoint: &str) -> Vec<Pop3Seen> {
    Checkpoint::parse(checkpoint).map(|cp| cp.entries.into_iter().map(|(uidl, e)| Pop3Seen { uidl, seen_at: e.seen_at, deleted_on_server: e.deleted_on_server }).collect()).unwrap_or_default()
}

/// The ledger in memory, with the JSON it came from so a checkpoint the engine hands back
/// unchanged is not re-parsed and entries added by `fetch_messages` survive.
#[derive(Default)]
struct Ledger {
    json: Option<String>,
    cp: Checkpoint,
}

// ------------------------------------------------------------------ provider

pub struct Pop3Provider {
    servers: MailServers,
    /// The password, or the OAuth session's token source.
    credential: Credential,
    outgoing_password: Option<String>,
    /// Where this account's `.eml` files live.
    raw_dir: PathBuf,
    timeout: Duration,
    top_above: u64,
    ledger: Mutex<Ledger>,
}

impl std::fmt::Debug for Pop3Provider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The password is deliberately absent.
        f.debug_struct("Pop3Provider").field("incoming", &self.servers.incoming).field("raw_dir", &self.raw_dir).finish()
    }
}

impl Pop3Provider {
    /// `raw_dir` is the account's own directory for `.eml` files (`<data dir>/raw/<account>`).
    /// `credential` is the account's password, or its OAuth token source.
    pub fn new(servers: MailServers, credential: impl Into<Credential>, raw_dir: impl Into<PathBuf>) -> Self {
        Self { servers, credential: credential.into(), outgoing_password: None, raw_dir: raw_dir.into(), timeout: DEFAULT_TIMEOUT, top_above: DEFAULT_TOP_ABOVE, ledger: Mutex::new(Ledger::default()) }
    }

    /// The SMTP password, when "use the same password" was not chosen at setup.
    pub fn with_outgoing_password(mut self, password: Option<String>) -> Self {
        self.outgoing_password = password.filter(|p| !p.is_empty());
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Size above which a message gets a `TOP` header row before its download.
    pub fn with_top_above(mut self, bytes: u64) -> Self {
        self.top_above = bytes;
        self
    }

    pub fn servers(&self) -> &MailServers {
        &self.servers
    }

    pub fn raw_dir(&self) -> &Path {
        &self.raw_dir
    }

    /// Where a message's bytes are kept.
    pub fn raw_path(&self, id: &MessageId) -> PathBuf {
        self.raw_dir.join(format!("{}.eml", id.0.replace([':', '/'], "_")))
    }

    fn policy(&self) -> Pop3Policy {
        self.servers.pop3.clone().unwrap_or_default()
    }

    /// What SMTP signs in with: the separate outgoing password, or the incoming credential.
    fn outgoing_credential(&self) -> Credential {
        match &self.outgoing_password {
            Some(password) => Credential::Password(password.clone()),
            None => self.credential.clone(),
        }
    }

    fn connect(&self) -> Result<Client, ProviderError> {
        Client::connect(&self.servers.incoming, &self.credential, self.timeout)
    }

    fn local_id(account: &Account) -> MessageId {
        MessageId(format!("{}:{}", account.id.0, MessageId::generate().0))
    }

    fn inbox_flags(account: &Account, uidl: &str, received_at: i64) -> MessageFlags {
        MessageFlags { provider_id: Some(ProviderId(uidl.to_owned())), label_ids: [label_id(&account.id, LabelKind::Inbox)].into_iter().collect(), is_read: false, is_starred: false, received_at: Some(received_at) }
    }

    fn write_raw(&self, id: &MessageId, bytes: &[u8]) -> Result<(), ProviderError> {
        let path = self.raw_path(id);
        let io = |e: std::io::Error| ProviderError::Temporary(format!("could not save the message to {}: {e}", path.display()));
        std::fs::create_dir_all(&self.raw_dir).map_err(io)?;
        // Write, then rename, so a crash never leaves a half-written primary copy.
        let tmp = path.with_extension("eml.part");
        std::fs::write(&tmp, bytes).map_err(io)?;
        std::fs::rename(&tmp, &path).map_err(io)
    }

    /// Whether the server copy may be deleted now, per the policy.
    fn should_delete(policy: &Pop3Policy, seen_at: i64, now: i64) -> bool {
        !policy.leave_on_server || policy.delete_after_days.is_some_and(|days| seen_at.saturating_add(i64::from(days) * 86_400_000) <= now)
    }

    /// The UIDL diff: lists the maildrop, records the unseen UIDLs (newest first) with
    /// header rows for the large ones, notes what vanished, runs retention, and `QUIT`s.
    fn cycle(&self, account: &Account, mut cp: Checkpoint) -> Result<(Vec<Message>, Vec<ProviderId>, Checkpoint), ProviderError> {
        let policy = self.policy();
        let now = now();
        let mut client = self.connect()?;
        let listing = client.uidl()?;
        let on_server: BTreeMap<&str, u32> = listing.iter().map(|(n, u)| (u.as_str(), *n)).collect();

        let mut unseen: Vec<(u32, &str)> = listing.iter().filter(|(_, u)| !cp.entries.contains_key(u)).map(|(n, u)| (*n, u.as_str())).collect();
        unseen.sort_by(|a, b| b.0.cmp(&a.0));
        let sizes = if unseen.is_empty() { BTreeMap::new() } else { client.list()? };
        let mut upserts = Vec::new();
        let mut ids = Vec::with_capacity(unseen.len());
        for (n, uidl) in &unseen {
            let size = sizes.get(n).copied().unwrap_or(0);
            let id = Self::local_id(account);
            if size > self.top_above {
                match client.top(*n, 0) {
                    Ok(bytes) => upserts.push(headers_from_rfc822(account, &bytes, id.clone(), &Self::inbox_flags(account, uidl, now))),
                    // TOP is optional in RFC 1939; the backlog downloads the whole message.
                    Err(ProviderError::Rejected(_)) => {}
                    Err(e) => return Err(e),
                }
            }
            cp.entries.insert((*uidl).to_owned(), SeenEntry { id: id.0, seen_at: now, size, deleted_on_server: false, deleted_at: None });
            ids.push(ProviderId((*uidl).to_owned()));
        }
        for (uidl, entry) in cp.entries.iter_mut() {
            entry.deleted_on_server = !on_server.contains_key(uidl.as_str());
            entry.deleted_at = match (entry.deleted_on_server, entry.deleted_at) {
                (false, _) => None,
                (true, Some(at)) => Some(at),
                (true, None) => Some(now),
            };
        }

        // Retention: only messages whose bytes are on disk, never ones listed just now.
        let mut deleted = 0;
        for (uidl, entry) in cp.entries.iter_mut() {
            let Some(n) = on_server.get(uidl.as_str()) else { continue };
            if ids.iter().any(|p| &p.0 == uidl) || !Self::should_delete(&policy, entry.seen_at, now) || !self.raw_path(&MessageId(entry.id.clone())).exists() {
                continue;
            }
            match client.dele(*n) {
                Ok(()) => {
                    entry.deleted_on_server = true;
                    entry.deleted_at = Some(now);
                    deleted += 1;
                }
                Err(ProviderError::Rejected(words)) => warn!("pop3: {} would not delete message {n}: {words}", self.servers.incoming.host),
                Err(e) => return Err(e),
            }
        }
        // QUIT commits the deletions; a lost connection here means they may not have happened.
        client.quit()?;
        let pruned = cp.prune(now);
        if pruned > 0 {
            info!("pop3: dropped {pruned} ledger entries for mail gone from {} for over 30 days", self.servers.incoming.host);
        }
        if !ids.is_empty() || deleted > 0 {
            info!("pop3: {} new of {} on {}, {deleted} deleted there", ids.len(), listing.len(), self.servers.incoming.host);
        }
        Ok((upserts, ids, cp))
    }

    fn with_ledger<T>(&self, f: impl FnOnce(&mut Ledger) -> T) -> T {
        let mut guard = self.ledger.lock().unwrap_or_else(|p| p.into_inner());
        f(&mut guard)
    }

    fn run_cycle(&self, account: &Account, cp: Checkpoint) -> Result<(Vec<Message>, Vec<ProviderId>, String), ProviderError> {
        let (upserts, ids, cp) = self.cycle(account, cp)?;
        let json = cp.to_json();
        self.with_ledger(|l| {
            l.cp = cp;
            l.json = Some(json.clone());
        });
        Ok((upserts, ids, json))
    }

    fn snapshot(&self, account: &Account) -> Result<MailboxSnapshot, ProviderError> {
        let (messages, message_ids, checkpoint) = self.run_cycle(account, Checkpoint { version: Checkpoint::VERSION, ..Default::default() })?;
        Ok(MailboxSnapshot { labels: labels_for(&account.id), messages, message_ids, checkpoint: Some(checkpoint) })
    }

    fn changes(&self, account: &Account, checkpoint: &str) -> Result<RemoteChanges, ProviderError> {
        let cp = self.with_ledger(|l| match &l.json {
            Some(json) if json == checkpoint => Ok(l.cp.clone()),
            _ => Checkpoint::parse(checkpoint),
        })?;
        let (upserts, message_ids, checkpoint) = self.run_cycle(account, cp)?;
        Ok(RemoteChanges { labels: None, upserts, deleted: Vec::new(), deleted_provider_ids: Vec::new(), message_ids, checkpoint: Some(checkpoint), complete: None, untracked: Vec::new() })
    }

    /// Full messages: from disk when the `.eml` exists, otherwise `RETR` (saved first). Ids
    /// the server no longer has and that were never saved are skipped.
    fn messages(&self, account: &Account, ids: &[ProviderId]) -> Result<Vec<Message>, ProviderError> {
        let mut guard = self.ledger.lock().unwrap_or_else(|p| p.into_inner());
        let mut out = Vec::with_capacity(ids.len());
        let mut need = Vec::new();
        for pid in ids {
            if is_sent_copy(pid) {
                continue;
            }
            match guard.cp.entries.get(&pid.0) {
                Some(entry) if self.raw_path(&MessageId(entry.id.clone())).exists() => {
                    let id = MessageId(entry.id.clone());
                    match std::fs::read(self.raw_path(&id)) {
                        Ok(bytes) => out.push(message_from_rfc822(account, &bytes, id, &Self::inbox_flags(account, &pid.0, entry.seen_at))),
                        Err(_) => need.push(pid.clone()),
                    }
                }
                _ => need.push(pid.clone()),
            }
        }
        if need.is_empty() {
            return Ok(out);
        }
        let mut client = self.connect()?;
        let numbers: BTreeMap<String, u32> = client.uidl()?.into_iter().map(|(n, u)| (u, n)).collect();
        for pid in need {
            let Some(n) = numbers.get(&pid.0) else {
                info!("pop3: {} is no longer on the server; skipped", pid.0);
                continue;
            };
            let bytes = match client.retr(*n) {
                Ok(b) => b,
                Err(ProviderError::Rejected(words)) => {
                    warn!("pop3: could not download message {n}: {words}");
                    continue;
                }
                Err(e) => return Err(e),
            };
            let entry = guard.cp.entries.entry(pid.0.clone()).or_insert_with(|| SeenEntry { id: Self::local_id(account).0, seen_at: now(), size: bytes.len() as u64, deleted_on_server: false, deleted_at: None });
            let id = MessageId(entry.id.clone());
            self.write_raw(&id, &bytes)?;
            out.push(message_from_rfc822(account, &bytes, id, &Self::inbox_flags(account, &pid.0, entry.seen_at)));
        }
        // Nothing to commit; a dropped QUIT is not worth failing the batch over.
        let _ = client.quit();
        Ok(out)
    }

    fn apply_op(&self, op: &PendingOperation, messages: &[Message]) -> Result<AppliedChanges, ProviderError> {
        if op.kind.is_calendar() {
            return Err(ProviderError::Rejected("this account has no calendar".into()));
        }
        if op.kind.is_contact() {
            return Err(ProviderError::Rejected("this account cannot store contacts".into()));
        }
        if matches!(op.kind, OperationKind::Trash { .. }) && self.policy().delete_on_server_when_trashed {
            let uidls: Vec<&str> = messages.iter().filter_map(|m| m.provider_id.as_ref()).filter(|p| !is_sent_copy(p)).map(|p| p.0.as_str()).collect();
            if !uidls.is_empty() {
                let mut client = self.connect()?;
                let numbers: BTreeMap<String, u32> = client.uidl()?.into_iter().map(|(n, u)| (u, n)).collect();
                for uidl in uidls {
                    // Already gone from the server is the wanted end state.
                    if let Some(n) = numbers.get(uidl) {
                        match client.dele(*n) {
                            Ok(()) | Err(ProviderError::Rejected(_)) => {}
                            Err(e) => return Err(e),
                        }
                    }
                }
                client.quit()?;
            }
        }
        Ok(AppliedChanges::default())
    }

    fn send_draft(&self, account: &Account, draft: &Draft, context: SendContext) -> Result<SentReceipt, ProviderError> {
        let rfc_id = draft_message_id(draft);
        let raw = build_raw(account, draft, context.in_reply_to.as_ref(), &rfc_id).map_err(ProviderError::Rejected)?;
        let recipients: Vec<String> = draft.to.iter().chain(&draft.cc).chain(&draft.bcc).map(|a| a.email.trim().to_owned()).filter(|e| !e.is_empty()).collect();
        let transport = smtp::Transport::new(self.servers.outgoing.clone(), self.outgoing_credential()).with_timeout(self.timeout);
        let reply = transport.send(&account.email, &recipients, &raw)?;
        info!("smtp: {} accepted {:?}: {reply}", self.servers.outgoing.host, draft.display_subject());

        let provider_id = ProviderId(format!("{SENT_PREFIX}{rfc_id}"));
        let sent_label = context.sent_label.clone().unwrap_or_else(|| label_id(&account.id, LabelKind::Sent));
        let mut message = crate::sample::sent_message(account, draft, context.in_reply_to.as_ref(), &provider_id, &rfc_id, Some(&sent_label), now());
        message.id = Self::local_id(account);
        if let Some(source) = &context.in_reply_to {
            message.in_reply_to = source.rfc_message_id.clone();
            message.references = source.references.iter().cloned().chain(source.rfc_message_id.clone()).collect();
        }
        if let Err(e) = self.write_raw(&message.id, &raw) {
            warn!("pop3: the message was sent but its copy could not be saved: {e}");
        }
        Ok(SentReceipt { provider_id, rfc_message_id: Some(rfc_id), message })
    }

    fn raw(&self, message: &Message) -> Result<Vec<u8>, ProviderError> {
        let path = self.raw_path(&message.id);
        std::fs::read(&path).map_err(|e| ProviderError::Rejected(format!("no saved copy of this message at {}: {e}", path.display())))
    }

    fn attachment(&self, message: &Message, attachment: &Attachment) -> Result<Vec<u8>, ProviderError> {
        let bytes = self.raw(message)?;
        let section = attachment.provider_id.as_ref().map(|p| p.0.clone()).ok_or_else(|| ProviderError::Rejected("this attachment has no part number".into()))?;
        crate::mime::part_bytes(&bytes, &section).ok_or_else(|| ProviderError::Rejected(format!("the saved message has no part {section}")))
    }
}

impl MailProvider for Pop3Provider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Pop3
    }

    /// Connects, signs in and leaves. POP3 locks the maildrop per session, so nothing is
    /// kept open between calls.
    fn probe(&self) -> BoxFuture<'_, Result<(), ProviderError>> {
        Box::pin(async move {
            let client = self.connect()?;
            client.quit()
        })
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

    fn apply(&self, _account: &Account, op: &PendingOperation, messages: &[Message], _labels: &[Label]) -> BoxFuture<'_, Result<AppliedChanges, ProviderError>> {
        let op = op.clone();
        let messages = messages.to_vec();
        Box::pin(async move { self.apply_op(&op, &messages) })
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

    fn send(&self, account: &Account, draft: &Draft, context: SendContext) -> BoxFuture<'_, Result<SentReceipt, ProviderError>> {
        let account = account.clone();
        let draft = draft.clone();
        Box::pin(async move { self.send_draft(&account, &draft, context) })
    }

    /// There is no server copy to look for: the draft stays Uncertain for the user.
    fn reconcile_send(&self, _account: &Account, _draft: &Draft) -> BoxFuture<'_, Result<SendOutcome, ProviderError>> {
        Box::pin(async { Ok(SendOutcome::Unknown) })
    }
}

#[cfg(test)]
mod unit {
    use super::*;

    #[test]
    fn checkpoint_round_trips_and_rejects_other_shapes() {
        let mut cp = Checkpoint { version: Checkpoint::VERSION, ..Default::default() };
        cp.entries.insert("u1".into(), SeenEntry { id: "pop3:j:abc".into(), seen_at: 5, size: 10, deleted_on_server: true, deleted_at: Some(6) });
        let json = cp.to_json();
        assert_eq!(Checkpoint::parse(&json).unwrap(), cp);
        let old = Checkpoint::parse(r#"{"version":1,"entries":{"u2":{"id":"pop3:j:def","seen_at":1,"size":2,"deleted_on_server":true}}}"#).unwrap();
        assert_eq!(old.entries["u2"].deleted_at, None, "a ledger written before pruning existed still parses");
        assert_eq!(Checkpoint::parse("12345"), Err(ProviderError::CheckpointExpired));
        assert_eq!(Checkpoint::parse(r#"{"version":9}"#), Err(ProviderError::CheckpointExpired));
        let rows = seen_rows(&json);
        assert_eq!(rows, vec![Pop3Seen { uidl: "u1".into(), seen_at: 5, deleted_on_server: true }]);
        assert!(seen_rows("garbage").is_empty());
    }

    #[test]
    fn ledger_prunes_long_gone_uidls_and_caps_the_rest() {
        let day = 86_400_000;
        let entry = |seen_at: i64, deleted_at: Option<i64>| SeenEntry { id: "x".into(), seen_at, size: 1, deleted_on_server: deleted_at.is_some(), deleted_at };
        let mut cp = Checkpoint { version: Checkpoint::VERSION, ..Default::default() };
        cp.entries.insert("live-old".into(), entry(0, None));
        cp.entries.insert("gone-recently".into(), entry(0, Some(100 * day - 29 * day)));
        cp.entries.insert("gone-long-ago".into(), entry(0, Some(100 * day - 31 * day)));
        // Written before `deleted_at` existed: counts as gone since first seen.
        cp.entries.insert("gone-untimed".into(), SeenEntry { id: "y".into(), seen_at: 0, size: 1, deleted_on_server: true, deleted_at: None });
        assert_eq!(cp.prune(100 * day), 1);
        assert_eq!(cp.entries.keys().collect::<Vec<_>>(), vec!["gone-recently", "gone-untimed", "live-old"], "a live UIDL is never pruned however old; recent and untimed gone ones stay");

        let mut big = Checkpoint { version: Checkpoint::VERSION, ..Default::default() };
        for i in 0..(Checkpoint::DELETED_CAP + 5) {
            big.entries.insert(format!("g{i:06}"), entry(0, Some(i as i64)));
        }
        big.entries.insert("live".into(), entry(0, None));
        assert_eq!(big.prune(10), 5, "the oldest beyond the cap go first");
        assert!(!big.entries.contains_key("g000000") && big.entries.contains_key("g000005") && big.entries.contains_key("live"));
    }

    #[test]
    fn retention_policy_decisions() {
        let day = 86_400_000;
        let keep = Pop3Policy::default();
        assert!(!Pop3Provider::should_delete(&keep, 0, 100 * day));
        let delete_now = Pop3Policy { leave_on_server: false, ..Default::default() };
        assert!(Pop3Provider::should_delete(&delete_now, 100 * day, 100 * day));
        let after_30 = Pop3Policy { delete_after_days: Some(30), ..Default::default() };
        assert!(!Pop3Provider::should_delete(&after_30, 0, 29 * day));
        assert!(Pop3Provider::should_delete(&after_30, 0, 30 * day));
    }

    #[test]
    fn labels_and_paths() {
        let account = AccountId::from("pop3:jason@example.com");
        let labels = labels_for(&account);
        assert_eq!(labels.iter().map(|l| (l.id.as_str(), l.kind, l.provider_id.is_none())).collect::<Vec<_>>(), vec![("pop3:jason@example.com:inbox", LabelKind::Inbox, true), ("pop3:jason@example.com:sent", LabelKind::Sent, true), ("pop3:jason@example.com:archive", LabelKind::Archive, true), ("pop3:jason@example.com:trash", LabelKind::Trash, true)]);
        let servers = MailServers { incoming: ServerEndpoint::new("localhost", 110, Security::None, "j"), outgoing: ServerEndpoint::new("localhost", 25, Security::None, "j"), pop3: None, imap: None, oauth: None };
        let p = Pop3Provider::new(servers, "pw", "/tmp/raw");
        assert_eq!(p.raw_path(&MessageId::from("pop3:jason@example.com:abc")), PathBuf::from("/tmp/raw/pop3_jason@example.com_abc.eml"));
        assert!(!format!("{p:?}").contains("pw"));
        assert!(is_sent_copy(&ProviderId(format!("{SENT_PREFIX}<x@y>"))) && !is_sent_copy(&ProviderId("UID1".into())));
    }
}
