//! Provider adapters.
//!
//! A provider turns local intent (pending operations, queued drafts) into remote effects and
//! reports remote state back. The sync engine in the app drives providers; providers never
//! touch the store directly, which keeps "what we asked for" and "what the server has"
//! separate.

pub mod app_passwords;
pub mod autoconfig;
pub mod calendar;
pub mod caldav;
pub mod credential;
pub mod gmail;
pub mod imap;
pub mod microsoft;
pub mod mime;
pub mod oauth;
pub mod outgoing;
pub mod pinning;
pub mod drafts;
pub mod pop3;
pub mod sample;
pub mod secure_error;
pub mod smtp;

pub use calendar::{CalendarProvider, EventChanges};
pub use credential::{AccessTokenSource, Credential};
pub use outgoing::{build_raw, message_to_raw};

use overlook_core::*;
use std::future::Future;
use std::pin::Pin;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProviderError {
    /// The provider cannot be reached. Local work stays queued.
    #[error("offline")]
    Offline,
    /// Credentials expired or were revoked. Local work is preserved; the user must sign in.
    #[error("sign-in required")]
    SignInRequired,
    /// Temporary failure; retry with backoff.
    #[error("temporary failure: {0}")]
    Temporary(String),
    /// The provider rejected the operation. It will not succeed on retry without a change.
    #[error("{0}")]
    Rejected(String),
    /// The provider asked us to slow down (quota or rate limit). Retry after a longer pause.
    #[error("rate limited: {0}")]
    RateLimited(String),
    /// The connection dropped after the request was sent but before a response arrived.
    /// For non-idempotent operations (sending), the outcome must be reconciled before any
    /// retry.
    #[error("connection dropped while waiting for a response")]
    Uncertain,
    /// The saved sync checkpoint is no longer valid; a full resynchronization is required.
    #[error("sync checkpoint expired")]
    CheckpointExpired,
}

/// Snapshot delivered by an initial or full synchronization.
///
/// Providers may return messages directly (cheap local sources) or only `message_ids`,
/// which the sync engine then downloads in small persisted batches with
/// [`MailProvider::fetch_messages`], so quota limits and interruptions never lose progress.
#[derive(Debug, Clone, Default)]
pub struct MailboxSnapshot {
    pub labels: Vec<Label>,
    pub messages: Vec<Message>,
    /// Provider ids of recent messages still to download, newest first.
    pub message_ids: Vec<ProviderId>,
    pub checkpoint: Option<String>,
}

/// Incremental changes since a checkpoint.
#[derive(Debug, Clone, Default)]
pub struct RemoteChanges {
    /// Complete label list when the provider reports label changes (replaces the local list).
    pub labels: Option<Vec<Label>>,
    /// New or changed messages. A message without a cached body keeps the local body.
    pub upserts: Vec<Message>,
    /// Messages removed at the provider (local ids).
    pub deleted: Vec<MessageId>,
    /// Messages removed at the provider, by provider id, for providers that do not know
    /// local ids (IMAP mints them on first sight). The sync engine resolves these through
    /// the store and treats them like `deleted`.
    pub deleted_provider_ids: Vec<ProviderId>,
    /// Provider ids whose bodies still need downloading (new mail reported header-only).
    /// The sync engine adds them to the persisted backlog.
    pub message_ids: Vec<ProviderId>,
    pub checkpoint: Option<String>,
    /// Set only by [`MailProvider::verify_changes`]: every message the provider tracks for
    /// the account (the window it downloads), with its read and starred state. The sync
    /// engine treats anything stored that is in neither this nor `untracked` as stale,
    /// corrects flags that differ, and downloads what it lacks.
    pub complete: Option<Vec<RemoteState>>,
    /// With `complete`: messages the provider holds outside the window it tracks (older mail,
    /// or mail an online search downloaded). Stored copies of them are not stale, and the
    /// ones not stored are not downloaded.
    pub untracked: Vec<ProviderId>,
}

/// A folder a rename or move changed, before and after. For IMAP the id changes with the
/// path; for Gmail only the name does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenamedFolder {
    pub old: Label,
    pub new: Label,
}

/// One message as a verifying check found it at the provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteState {
    pub provider_id: ProviderId,
    pub read: bool,
    pub starred: bool,
}

/// What changed at the provider as a side effect of applying an operation. `Default` means
/// "nothing beyond the operation itself", which is what Gmail and Sample report.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AppliedChanges {
    /// Messages whose provider id changed, with the folder label they now live in. An IMAP
    /// move gives a message a new UID in another folder; the store rewrites its identity so
    /// the next sync recognises it.
    pub provider_ids: Vec<(MessageId, ProviderId, LabelId)>,
    /// Labels the operation created at the provider (an IMAP Archive folder made on demand).
    /// The sync engine stores them before `provider_ids`, which may reference them.
    pub labels: Vec<Label>,
}

/// Everything a provider needs to submit a draft besides the draft itself.
#[derive(Debug, Clone, Default)]
pub struct SendContext {
    pub sent_label: Option<LabelId>,
    /// The message being replied to or forwarded, for threading headers.
    pub in_reply_to: Option<Message>,
    /// The account's labels, for provider id lookups.
    pub labels: Vec<Label>,
}

/// What the provider recorded after accepting a send.
#[derive(Debug, Clone)]
pub struct SentReceipt {
    pub provider_id: ProviderId,
    pub rfc_message_id: Option<String>,
    /// The provider's copy of the message, ready to store in Sent.
    pub message: Message,
}

/// Result of reconciling an uncertain send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendOutcome {
    /// The provider has the message; do not resend.
    Delivered(ProviderId),
    /// The provider never received it; safe to resend.
    NotDelivered,
    /// The provider cannot tell (SMTP accepted the bytes but no copy can be found). The
    /// operation stays uncertain and the user decides between sending again and discarding.
    Unknown,
}

pub trait MailProvider: Send + Sync {
    fn kind(&self) -> ProviderKind;

    fn supports_drafts(&self) -> bool { false }

    /// A complete listing, including every page. Failure must never look like an empty
    /// mailbox: callers use absence to reconcile remote deletions.
    fn list_drafts(&self, _account: &Account) -> BoxFuture<'_, Result<Vec<RemoteDraft>, ProviderError>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn fetch_draft(&self, _account: &Account, _remote: &RemoteDraft) -> BoxFuture<'_, Result<Option<Vec<u8>>, ProviderError>> {
        Box::pin(async { Err(ProviderError::Rejected("Draft synchronization is unavailable".into())) })
    }

    /// The exact persisted RFC 822 bytes include a unique Message-ID for this upload.
    /// Reconcile that id before retrying a create/APPEND. `None` means the remote version
    /// changed or disappeared; preserve both sides rather than overwriting it.
    fn put_draft(&self, _account: &Account, _remote: Option<&RemoteDraft>, _raw: &[u8], _message_id: &str, _thread_id: Option<&str>) -> BoxFuture<'_, Result<Option<RemoteDraft>, ProviderError>> {
        Box::pin(async { Err(ProviderError::Rejected("Draft synchronization is unavailable".into())) })
    }

    fn delete_remote_draft(&self, _account: &Account, _remote: &RemoteDraft) -> BoxFuture<'_, Result<(), ProviderError>> {
        Box::pin(async { Err(ProviderError::Rejected("Draft synchronization is unavailable".into())) })
    }

    /// Checks actual provider reachability (not just network availability).
    fn probe(&self) -> BoxFuture<'_, Result<(), ProviderError>>;

    /// Downloads labels and recent messages for first use or full resynchronization.
    fn fetch_snapshot(&self, account: &Account) -> BoxFuture<'_, Result<MailboxSnapshot, ProviderError>>;

    /// Downloads changes since `checkpoint`.
    fn fetch_changes(&self, account: &Account, checkpoint: &str) -> BoxFuture<'_, Result<RemoteChanges, ProviderError>>;

    /// Like `fetch_changes`, but checks everything instead of trusting shortcuts, and fills
    /// `RemoteChanges::complete` so the engine can repair what drifted. Run at start-up and
    /// when the user asks for a sync. The default is an ordinary change fetch.
    fn verify_changes(&self, account: &Account, checkpoint: &str) -> BoxFuture<'_, Result<RemoteChanges, ProviderError>> {
        self.fetch_changes(account, checkpoint)
    }

    /// Downloads full messages for provider ids listed by a snapshot. Ids that no longer
    /// exist are skipped.
    fn fetch_messages(&self, _account: &Account, _ids: &[ProviderId], _labels: &[Label]) -> BoxFuture<'_, Result<Vec<Message>, ProviderError>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    /// Applies an idempotent mailbox operation. The result reports identity changes the
    /// store must record; most providers return [`AppliedChanges::default`].
    fn apply(&self, account: &Account, op: &PendingOperation, messages: &[Message], labels: &[Label]) -> BoxFuture<'_, Result<AppliedChanges, ProviderError>>;

    /// Whether automatic replies can be read and changed at the provider.
    fn has_settings_access(&self) -> bool {
        false
    }

    fn fetch_vacation(&self, _account: &Account) -> BoxFuture<'_, Result<Option<VacationSettings>, ProviderError>> {
        Box::pin(async { Ok(None) })
    }

    fn set_vacation(&self, _account: &Account, _settings: &VacationSettings) -> BoxFuture<'_, Result<(), ProviderError>> {
        Box::pin(async { Err(ProviderError::Rejected("this account cannot store automatic replies".into())) })
    }

    /// Whether the stored credentials allow reading contacts.
    fn has_contacts_access(&self) -> bool {
        false
    }

    /// The account's contacts: address book entries and people corresponded with. `None`
    /// when the provider has no contacts, or the sign-in lacks the scope.
    fn fetch_contacts(&self, _account: &Account) -> BoxFuture<'_, Result<Option<Vec<Contact>>, ProviderError>> {
        Box::pin(async { Ok(None) })
    }

    /// Whether contacts can be created, changed and deleted at the provider.
    fn has_contacts_write_access(&self) -> bool {
        false
    }

    /// Creates the contact at the provider; returns the provider's copy, with its id.
    fn create_contact(&self, _account: &Account, _contact: &Contact) -> BoxFuture<'_, Result<Contact, ProviderError>> {
        Box::pin(async { Err(ProviderError::Rejected("this account cannot store contacts".into())) })
    }

    fn update_contact(&self, _account: &Account, _contact: &Contact) -> BoxFuture<'_, Result<Contact, ProviderError>> {
        Box::pin(async { Err(ProviderError::Rejected("this account cannot store contacts".into())) })
    }

    fn delete_contact(&self, _account: &Account, _resource_name: &str) -> BoxFuture<'_, Result<(), ProviderError>> {
        Box::pin(async { Err(ProviderError::Rejected("this account cannot store contacts".into())) })
    }

    /// The message as RFC 822 bytes, for "Save as .eml" and "View source". The default
    /// rebuilds it from the stored headers and body.
    fn fetch_raw(&self, _account: &Account, message: &Message) -> BoxFuture<'_, Result<Vec<u8>, ProviderError>> {
        let bytes = message_to_raw(message);
        Box::pin(async move { Ok(bytes) })
    }

    /// Searches the whole mailbox at the provider. Returns provider ids, newest first.
    /// The default has no server-side search.
    fn search(&self, _account: &Account, _query: &str, _limit: usize) -> BoxFuture<'_, Result<Vec<ProviderId>, ProviderError>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn fetch_attachment(&self, _account: &Account, _message: &Message, _attachment: &Attachment) -> BoxFuture<'_, Result<Vec<u8>, ProviderError>> {
        Box::pin(async { Err(ProviderError::Rejected("this account cannot download attachments".into())) })
    }

    /// Whether folders (labels) can be created, renamed and deleted from Overlook. These are
    /// done online, not queued: the server picks the new folder's identity.
    fn manages_folders(&self) -> bool { false }

    /// Whether single messages can be deleted for good, skipping Trash
    /// (`OperationKind::DeleteForever` without a folder). False where the server would
    /// reject it (Gmail without the full scope) or quietly keep the mail (POP3).
    fn supports_delete_forever(&self) -> bool { false }

    /// Creates a user folder named `name` (one level, no delimiter), inside `parent` if given.
    /// `labels` is the account's current list. Returns the new label as the next sync will
    /// list it.
    fn create_folder(&self, _account: &Account, _name: &str, _parent: Option<&Label>, _labels: &[Label]) -> BoxFuture<'_, Result<Label, ProviderError>> {
        Box::pin(async { Err(ProviderError::Rejected("this account cannot create folders".into())) })
    }

    /// Renames the last part of a user folder's name, keeping where it is. Returns every
    /// folder that changed: this one first, then the folders inside it, which are renamed
    /// with it. Empty when nothing changed.
    fn rename_folder(&self, _account: &Account, _label: &Label, _name: &str, _labels: &[Label]) -> BoxFuture<'_, Result<Vec<RenamedFolder>, ProviderError>> {
        Box::pin(async { Err(ProviderError::Rejected("this account cannot rename folders".into())) })
    }

    /// Moves a user folder, with the folders inside it, into `parent` (a user folder or the
    /// Inbox) or to the top level. Returns every folder that changed, like
    /// [`MailProvider::rename_folder`].
    fn move_folder(&self, _account: &Account, _label: &Label, _parent: Option<&Label>, _labels: &[Label]) -> BoxFuture<'_, Result<Vec<RenamedFolder>, ProviderError>> {
        Box::pin(async { Err(ProviderError::Rejected("this account cannot move folders".into())) })
    }

    /// Deletes a user folder. IMAP deletes the messages in it; Gmail only removes the label.
    fn delete_folder(&self, _account: &Account, _label: &Label) -> BoxFuture<'_, Result<(), ProviderError>> {
        Box::pin(async { Err(ProviderError::Rejected("this account cannot delete folders".into())) })
    }

    /// Submits a message. Only a returned receipt means "Sent".
    fn send(&self, account: &Account, draft: &Draft, context: SendContext) -> BoxFuture<'_, Result<SentReceipt, ProviderError>>;

    /// Determines whether an uncertain send was delivered, by Message-ID.
    fn reconcile_send(&self, account: &Account, draft: &Draft) -> BoxFuture<'_, Result<SendOutcome, ProviderError>>;
}

/// Why `name` can't be one level of a folder name, if it can't: empty, too long, or it
/// contains the hierarchy `delimiter` (which would make nested folders instead).
pub fn folder_name_problem(name: &str, delimiter: &str) -> Option<String> {
    let name = name.trim();
    if name.is_empty() {
        return Some("Enter a folder name.".into());
    }
    if name.chars().count() > 200 {
        return Some("That name is too long.".into());
    }
    if !delimiter.is_empty() && name.contains(delimiter) {
        return Some(format!("Folder names can’t contain “{delimiter}”."));
    }
    if name.chars().any(char::is_control) {
        return Some("Folder names can’t contain control characters.".into());
    }
    None
}

/// Deterministic Message-ID for a draft, so an interrupted send can be reconciled.
pub fn draft_message_id(draft: &Draft) -> String {
    format!("<{}@overlook.lastrefuge.ai>", draft.id)
}

/// Backoff schedule for temporary failures: 2s, 5s, 15s, 60s, then every 5 minutes.
pub fn backoff_seconds(attempt: u32) -> u64 {
    match attempt {
        0 | 1 => 2,
        2 => 5,
        3 => 15,
        4 => 60,
        _ => 300,
    }
}
