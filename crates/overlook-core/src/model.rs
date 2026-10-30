//! Domain model shared by storage, providers, and the UI.

use crate::ids::*;
use crate::time::Timestamp;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// The kind of service or protocol an account uses. Gmail is a service; IMAP and POP3 are
/// protocols. The sample provider exists for the design prototype and automated tests.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderKind {
    Sample,
    Gmail,
    Imap,
    Pop3,
}

impl ProviderKind {
    pub fn label(self) -> &'static str {
        match self {
            ProviderKind::Sample => "Sample account",
            ProviderKind::Gmail => "Gmail",
            ProviderKind::Imap => "IMAP",
            ProviderKind::Pop3 => "POP3",
        }
    }

    pub fn as_key(self) -> &'static str {
        match self {
            ProviderKind::Sample => "sample",
            ProviderKind::Gmail => "gmail",
            ProviderKind::Imap => "imap",
            ProviderKind::Pop3 => "pop3",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "sample" => Some(ProviderKind::Sample),
            "gmail" => Some(ProviderKind::Gmail),
            "imap" => Some(ProviderKind::Imap),
            "pop3" => Some(ProviderKind::Pop3),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    pub id: AccountId,
    pub provider: ProviderKind,
    pub display_name: String,
    pub email: String,
    pub created_at: Timestamp,
}

/// An email address with an optional display name.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub struct Address {
    pub name: Option<String>,
    pub email: String,
}

impl Address {
    pub fn new(name: impl Into<Option<String>>, email: impl Into<String>) -> Self {
        Self { name: name.into(), email: email.into() }
    }

    pub fn named(name: &str, email: &str) -> Self {
        Self { name: Some(name.to_owned()), email: email.to_owned() }
    }

    /// Display name, falling back to the local part of the address.
    pub fn display_name(&self) -> String {
        match &self.name {
            Some(n) if !n.trim().is_empty() => n.trim().to_owned(),
            _ => self
                .email
                .split('@')
                .next()
                .unwrap_or(&self.email)
                .to_owned(),
        }
    }

    /// One or two uppercase initials for avatars.
    pub fn initials(&self) -> String {
        let name = self.display_name();
        let mut parts = name.split_whitespace().filter(|p| !p.is_empty());
        let first = parts.next().and_then(|p| p.chars().next());
        let last = parts.last().and_then(|p| p.chars().next());
        match (first, last) {
            (Some(a), Some(b)) => format!("{}{}", a.to_uppercase(), b.to_uppercase()),
            (Some(a), None) => a.to_uppercase().to_string(),
            _ => "?".to_owned(),
        }
    }

    /// `Name <email>` or just the address.
    pub fn to_header(&self) -> String {
        match &self.name {
            Some(n) if !n.trim().is_empty() => {
                let name = n.trim();
                // Composer text must round-trip through parse_list, including
                // display names such as Doe, Jane or names containing quotes.
                let name = if name.contains([',', ';', '"', '\\', '<', '>', '(', ')', ':', '@', '[', ']']) {
                    format!("\"{}\"", name.replace('\\', "\\\\").replace('"', "\\\""))
                } else {
                    name.to_owned()
                };
                format!("{name} <{}>", self.email)
            }
            _ => self.email.clone(),
        }
    }

    /// Parses a comma-separated recipient field. Accepts `Name <a@b>` and bare addresses.
    pub fn parse_list(input: &str) -> Vec<Address> {
        crate::recipients::recipient_ranges(input)
            .into_iter()
            .map(|range| &input[range])
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|part| {
                if let (Some(open), Some(close)) = (part.rfind('<'), part.rfind('>')) {
                    if open < close {
                        let raw_name = part[..open].trim();
                        let name = if let Some(quoted) = raw_name.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
                            let mut name = String::new();
                            let mut chars = quoted.chars();
                            while let Some(ch) = chars.next() {
                                name.push(if ch == '\\' { chars.next().unwrap_or(ch) } else { ch });
                            }
                            name
                        } else {
                            raw_name.to_owned()
                        };
                        let email = part[open + 1..close].trim().to_owned();
                        return Address {
                            name: if name.is_empty() { None } else { Some(name) },
                            email,
                        };
                    }
                }
                Address { name: None, email: part.to_owned() }
            })
            .collect()
    }

    /// Conservative validity check for composing: one `@`, non-empty local and domain
    /// parts, and a dot in the domain.
    pub fn looks_valid(&self) -> bool {
        let mut it = self.email.split('@');
        match (it.next(), it.next(), it.next()) {
            (Some(local), Some(domain), None) => {
                !local.is_empty()
                    && domain.contains('.')
                    && !domain.starts_with('.')
                    && !domain.ends_with('.')
                    && !self.email.chars().any(char::is_whitespace)
            }
            _ => false,
        }
    }
}

/// System labels map to fixed mailbox views. User labels are provider-defined.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LabelKind {
    Inbox,
    Starred,
    Drafts,
    Sent,
    Archive,
    Trash,
    Spam,
    Outbox,
    User,
}

impl LabelKind {
    pub fn as_key(self) -> &'static str {
        match self {
            LabelKind::Inbox => "inbox",
            LabelKind::Starred => "starred",
            LabelKind::Drafts => "drafts",
            LabelKind::Sent => "sent",
            LabelKind::Archive => "archive",
            LabelKind::Trash => "trash",
            LabelKind::Spam => "spam",
            LabelKind::Outbox => "outbox",
            LabelKind::User => "user",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Some(match key {
            "inbox" => LabelKind::Inbox,
            "starred" => LabelKind::Starred,
            "drafts" => LabelKind::Drafts,
            "sent" => LabelKind::Sent,
            "archive" => LabelKind::Archive,
            "trash" => LabelKind::Trash,
            "spam" => LabelKind::Spam,
            "outbox" => LabelKind::Outbox,
            "user" => LabelKind::User,
            _ => return None,
        })
    }

    /// Whether this kind is a view computed from message state rather than a stored label.
    pub fn is_virtual(self) -> bool {
        matches!(self, LabelKind::Starred | LabelKind::Drafts | LabelKind::Outbox)
    }
}

/// A Gmail label or a system mailbox view. For Gmail, `provider_id` is the label id.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Label {
    pub id: LabelId,
    pub account_id: AccountId,
    pub kind: LabelKind,
    pub name: String,
    pub provider_id: Option<ProviderId>,
    /// Optional color token name for user labels (e.g. "slate", "plum", "amber").
    pub color: Option<String>,
    pub sort_order: i32,
}

/// An IMAP folder. Defined now so phase 2 does not require redesigning the model; unused in
/// phase 1.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Folder {
    pub id: FolderId,
    pub account_id: AccountId,
    pub path: String,
    pub delimiter: Option<char>,
    pub provider_id: Option<ProviderId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    pub id: AttachmentId,
    pub message_id: MessageId,
    pub filename: String,
    pub mime_type: String,
    pub size_bytes: u64,
    /// Relative path inside the account's attachment store, once downloaded.
    pub local_path: Option<String>,
    pub provider_id: Option<ProviderId>,
    /// The `Content-ID` of an inline part, without angle brackets, so `cid:` images in the
    /// HTML body can be resolved to it.
    #[serde(default)]
    pub content_id: Option<String>,
}

impl Attachment {
    /// An image the HTML body embeds with a `cid:` reference.
    pub fn is_inline_image(&self) -> bool {
        self.content_id.is_some() && self.mime_type.starts_with("image/")
    }

    pub fn is_pdf(&self) -> bool {
        self.mime_type.split(';').next().unwrap_or("").trim().eq_ignore_ascii_case("application/pdf") || self.filename.to_ascii_lowercase().ends_with(".pdf")
    }

    /// An image the reader can show itself (thumbnail and quick look).
    pub fn is_image(&self) -> bool {
        let mime = self.mime_type.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
        if matches!(mime.as_str(), "image/png" | "image/jpeg" | "image/jpg" | "image/gif" | "image/webp" | "image/bmp") {
            return true;
        }
        let ext = self.filename.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
        matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp")
    }

    pub fn is_downloaded(&self) -> bool {
        self.local_path.is_some()
    }

    /// Short uppercase type badge for attachment cards: `PDF`, `PNG`, `FIG`, `FILE`.
    pub fn badge(&self) -> String {
        let ext = self
            .filename
            .rsplit('.')
            .next()
            .filter(|e| e.len() <= 4 && *e != self.filename)
            .map(|e| e.to_uppercase());
        ext.unwrap_or_else(|| "FILE".to_owned())
    }

    pub fn human_size(&self) -> String {
        human_size(self.size_bytes)
    }
}

pub fn human_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    let b = bytes as f64;
    if b < KB {
        format!("{bytes} B")
    } else if b < KB * KB {
        format!("{:.0} KB", b / KB)
    } else if b < KB * KB * KB {
        format!("{:.1} MB", b / KB / KB)
    } else {
        format!("{:.2} GB", b / KB / KB / KB)
    }
}

/// Message body as stored. Either part may be absent: an uncached message has only
/// headers and a snippet.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Body {
    pub text: Option<String>,
    pub html: Option<String>,
}

impl Body {
    pub fn is_cached(&self) -> bool {
        self.text.is_some() || self.html.is_some()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub id: MessageId,
    pub account_id: AccountId,
    pub conversation_id: ConversationId,
    pub provider_id: Option<ProviderId>,
    pub from: Address,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub bcc: Vec<Address>,
    pub reply_to: Option<Address>,
    pub subject: String,
    pub snippet: String,
    pub body: Body,
    pub date: Timestamp,
    pub is_read: bool,
    pub is_starred: bool,
    pub is_outgoing: bool,
    pub label_ids: BTreeSet<LabelId>,
    pub attachments: Vec<Attachment>,
    /// RFC 5322 Message-ID header, when known.
    pub rfc_message_id: Option<String>,
    pub in_reply_to: Option<String>,
    /// The `References` header as a list of Message-IDs, oldest first. Used to rebuild
    /// conversations for providers without thread ids (see `threading`).
    #[serde(default)]
    pub references: Vec<String>,
    /// Provider category hint (Gmail: personal, social, promotions, updates, forums).
    pub category: Option<String>,
    /// Provider importance hint (Gmail IMPORTANT).
    pub is_important: bool,
    /// User override for Focused Inbox: `Some(true)` forces Focused, `Some(false)` Other.
    pub focus_override: Option<bool>,
    /// Calendar invitation carried by the message (from a `text/calendar` part).
    pub invitation: Option<crate::ics::Invitation>,
}

impl Message {
    pub fn has_attachments(&self) -> bool {
        !self.attachments.is_empty()
    }

    pub fn has_label(&self, id: &LabelId) -> bool {
        self.label_ids.contains(id)
    }
}

/// A conversation summary computed from its messages. Never stored; always derived.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConversationSummary {
    pub id: ConversationId,
    pub account_id: AccountId,
    pub subject: String,
    pub snippet: String,
    pub latest: Timestamp,
    pub message_count: usize,
    pub unread_count: usize,
    pub starred: bool,
    pub has_attachments: bool,
    /// Participants other than the account owner, most recent sender first, de-duplicated.
    pub participants: Vec<Address>,
    pub latest_message_id: MessageId,
    pub label_ids: BTreeSet<LabelId>,
    /// True when any message in the conversation has local changes awaiting sync.
    pub pending: bool,
}

impl ConversationSummary {
    /// Builds a summary from the conversation's messages. `owner_email` lets the summary
    /// name the other participants rather than the account owner.
    pub fn from_messages(
        messages: &[Message],
        owner_email: &str,
        pending_ids: &BTreeSet<MessageId>,
    ) -> Option<Self> {
        let latest = messages.iter().max_by_key(|m| m.date)?;
        let mut sorted: Vec<&Message> = messages.iter().collect();
        sorted.sort_by_key(|m| std::cmp::Reverse(m.date));
        let mut participants = Vec::new();
        for m in &sorted {
            let addr = &m.from;
            if addr.email.eq_ignore_ascii_case(owner_email) {
                continue;
            }
            if !participants
                .iter()
                .any(|p: &Address| p.email.eq_ignore_ascii_case(&addr.email))
            {
                participants.push(addr.clone());
            }
        }
        if participants.is_empty() {
            // Conversation only has our own messages (e.g. Sent): show recipients.
            for m in &sorted {
                for addr in m.to.iter().chain(m.cc.iter()) {
                    if !participants
                        .iter()
                        .any(|p: &Address| p.email.eq_ignore_ascii_case(&addr.email))
                    {
                        participants.push(addr.clone());
                    }
                }
            }
        }
        let mut label_ids = BTreeSet::new();
        for m in messages {
            label_ids.extend(m.label_ids.iter().cloned());
        }
        Some(Self {
            id: latest.conversation_id.clone(),
            account_id: latest.account_id.clone(),
            subject: latest.subject.clone(),
            snippet: latest.snippet.clone(),
            latest: latest.date,
            message_count: messages.len(),
            unread_count: messages.iter().filter(|m| !m.is_read).count(),
            starred: messages.iter().any(|m| m.is_starred),
            has_attachments: messages.iter().any(Message::has_attachments),
            participants,
            latest_message_id: latest.id.clone(),
            label_ids,
            pending: messages.iter().any(|m| pending_ids.contains(&m.id)),
        })
    }

    pub fn is_unread(&self) -> bool {
        self.unread_count > 0
    }

    /// `Maya Chen`, `Maya Chen, Alex Rivera`, or `Maya Chen and 3 others`.
    pub fn participant_line(&self) -> String {
        let names: Vec<String> = self.participants.iter().map(Address::display_name).collect();
        match names.len() {
            0 => "You".to_owned(),
            1 => names[0].clone(),
            2 => format!("{}, {}", names[0], names[1]),
            n => format!("{} and {} others", names[0], n - 1),
        }
    }
}

/// How a draft came to exist. Determines quoting and recipient defaults.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DraftKind {
    New,
    Reply,
    ReplyAll,
    Forward,
}

impl DraftKind {
    pub fn as_key(self) -> &'static str {
        match self {
            DraftKind::New => "new",
            DraftKind::Reply => "reply",
            DraftKind::ReplyAll => "reply-all",
            DraftKind::Forward => "forward",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Some(match key {
            "new" => DraftKind::New,
            "reply" => DraftKind::Reply,
            "reply-all" => DraftKind::ReplyAll,
            "forward" => DraftKind::Forward,
            _ => return None,
        })
    }
}

/// Lifecycle of a draft. `Queued`, `Sending`, and `Uncertain` are Outbox states.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "state", content = "detail")]
pub enum DraftState {
    /// Being edited; saved on this device.
    Editing,
    /// Send was requested. Waiting for a connection or for its turn in the queue.
    Queued,
    /// Submission to the provider is in progress.
    Sending,
    /// The connection dropped during submission. Must be reconciled before any retry.
    Uncertain,
    /// The provider rejected the message. Kept in the Outbox for the user to fix.
    Failed(String),
    /// The provider accepted the message. The draft row is removed once a Sent message
    /// exists locally, so this state is transient.
    Sent,
}

impl DraftState {
    pub fn is_outbox(&self) -> bool {
        matches!(
            self,
            DraftState::Queued | DraftState::Sending | DraftState::Uncertain | DraftState::Failed(_)
        )
    }

    /// Whether the user may still edit or cancel the item.
    pub fn is_editable(&self) -> bool {
        matches!(self, DraftState::Editing | DraftState::Queued | DraftState::Failed(_))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftAttachment {
    pub id: AttachmentId,
    pub filename: String,
    pub mime_type: String,
    pub size_bytes: u64,
    /// Path inside the protected outgoing-attachment store. Never evicted as cache.
    pub local_path: String,
    /// Set on an image the quoted original embeds with a `cid:` reference. It is sent inline,
    /// next to the HTML that refers to it, and is not one of the files the user attached.
    #[serde(default)]
    pub content_id: Option<String>,
}

impl DraftAttachment {
    /// A file the user can see and remove, as opposed to an image embedded in the quote.
    pub fn is_listed(&self) -> bool {
        self.content_id.is_none()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Draft {
    pub id: DraftId,
    pub account_id: AccountId,
    pub kind: DraftKind,
    pub conversation_id: Option<ConversationId>,
    pub in_reply_to: Option<MessageId>,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub bcc: Vec<Address>,
    pub subject: String,
    pub body_text: String,
    /// The HTML rendering of `body_text`, which is Markdown-flavoured. Theme changes must
    /// never rewrite this.
    pub body_html: Option<String>,
    /// Whether the message this replies to or forwards is appended below the body when sent
    /// (and shown, collapsed, in the composer). The quoted text is not part of `body_text`.
    #[serde(default)]
    pub include_quote: bool,
    pub attachments: Vec<DraftAttachment>,
    pub state: DraftState,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    /// Set when the provider has a copy of this draft (Gmail draft id).
    pub provider_id: Option<ProviderId>,
}

impl Draft {
    pub fn new(account_id: AccountId, kind: DraftKind, now: Timestamp) -> Self {
        Self {
            id: DraftId::generate(),
            account_id,
            kind,
            conversation_id: None,
            in_reply_to: None,
            to: Vec::new(),
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: String::new(),
            body_text: String::new(),
            body_html: None,
            include_quote: false,
            attachments: Vec::new(),
            state: DraftState::Editing,
            created_at: now,
            updated_at: now,
            provider_id: None,
        }
    }

    /// Builds a reply/forward draft from a message. `owner` is the account address.
    pub fn in_response_to(
        account_id: AccountId,
        kind: DraftKind,
        source: &Message,
        owner: &Address,
        now: Timestamp,
    ) -> Self {
        Self::in_response_to_with(account_id, kind, source, owner, now, true)
    }

    /// Like [`Draft::in_response_to`]; `quote` controls whether replies carry the quoted
    /// original (forwards always include it).
    pub fn in_response_to_with(
        account_id: AccountId,
        kind: DraftKind,
        source: &Message,
        owner: &Address,
        now: Timestamp,
        quote: bool,
    ) -> Self {
        let mut draft = Self::new(account_id, kind, now);
        draft.conversation_id = Some(source.conversation_id.clone());
        draft.in_reply_to = Some(source.id.clone());
        let base_subject = strip_prefixes(&source.subject);
        match kind {
            DraftKind::Reply | DraftKind::ReplyAll => {
                draft.subject = format!("Re: {base_subject}");
                let reply_target = source.reply_to.clone().unwrap_or_else(|| source.from.clone());
                if source.is_outgoing {
                    draft.to = source.to.clone();
                } else {
                    draft.to = vec![reply_target];
                }
                if kind == DraftKind::ReplyAll {
                    for addr in source.to.iter().chain(source.cc.iter()) {
                        let is_owner = addr.email.eq_ignore_ascii_case(&owner.email);
                        let already = draft
                            .to
                            .iter()
                            .chain(draft.cc.iter())
                            .any(|a| a.email.eq_ignore_ascii_case(&addr.email));
                        if !is_owner && !already {
                            draft.cc.push(addr.clone());
                        }
                    }
                }
                draft.include_quote = quote;
            }
            DraftKind::Forward => {
                draft.subject = format!("Fwd: {base_subject}");
                draft.include_quote = true;
            }
            DraftKind::New => {}
        }
        draft
    }

    pub fn is_empty(&self) -> bool {
        self.to.is_empty()
            && self.cc.is_empty()
            && self.bcc.is_empty()
            && self.subject.trim().is_empty()
            && self.body_text.trim().is_empty()
            && self.attachments.iter().all(|a| !a.is_listed())
    }

    pub fn display_subject(&self) -> String {
        if self.subject.trim().is_empty() {
            "(No subject)".to_owned()
        } else {
            self.subject.clone()
        }
    }

    /// Validation performed before a draft may enter the Outbox.
    pub fn validate_for_send(&self) -> Result<(), SendValidationError> {
        if self.to.is_empty() && self.cc.is_empty() && self.bcc.is_empty() {
            return Err(SendValidationError::NoRecipients);
        }
        if let Some(bad) = self
            .to
            .iter()
            .chain(self.cc.iter())
            .chain(self.bcc.iter())
            .find(|a| !a.looks_valid())
        {
            return Err(SendValidationError::InvalidRecipient(bad.email.clone()));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SendValidationError {
    #[error("Add at least one recipient")]
    NoRecipients,
    #[error("“{0}” doesn’t look like an email address")]
    InvalidRecipient(String),
}

fn strip_prefixes(subject: &str) -> String {
    let mut s = subject.trim();
    loop {
        let lower = s.to_ascii_lowercase();
        let stripped = ["re:", "fwd:", "fw:"]
            .iter()
            .find(|p| lower.starts_with(**p))
            .map(|p| s[p.len()..].trim_start());
        match stripped {
            Some(rest) => s = rest,
            None => break,
        }
    }
    s.to_owned()
}

/// The attribution line above a quoted reply.
pub fn quote_header(source: &Message) -> String {
    format!(
        "On {}, {} wrote:",
        crate::time::format_full(source.date, crate::time::local_utc_offset_minutes()),
        source.from.to_header()
    )
}

/// The header block above forwarded content, as plain text lines.
pub fn forward_header(source: &Message) -> String {
    format!(
        "---------- Forwarded message ----------\nFrom: {}\nDate: {}\nSubject: {}\nTo: {}",
        source.from.to_header(),
        crate::time::format_full(source.date, crate::time::local_utc_offset_minutes()),
        source.subject,
        source.to.iter().map(Address::to_header).collect::<Vec<_>>().join(", ")
    )
}

/// Prefixes every line with `> `.
pub fn quote_text(text: &str) -> String {
    text.lines()
        .map(|l| if l.is_empty() { ">".to_owned() } else { format!("> {l}") })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Layout density.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Density {
    #[default]
    Comfortable,
    Compact,
}

/// Reading pane placement.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ReadingPane {
    #[default]
    Right,
    Bottom,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(from: Address, date: Timestamp) -> Message {
        Message {
            id: MessageId::generate(),
            account_id: AccountId::from("acct"),
            conversation_id: ConversationId::from("conv"),
            provider_id: None,
            from,
            to: vec![Address::named("Jason", "jason@example.com")],
            cc: vec![],
            bcc: vec![],
            reply_to: None,
            subject: "Re: Launch creative".into(),
            snippet: "snippet".into(),
            body: Body { text: Some("Hello\n\nThanks".into()), html: None },
            date,
            is_read: false,
            is_starred: false,
            is_outgoing: false,
            label_ids: BTreeSet::new(),
            attachments: vec![],
            rfc_message_id: None,
            in_reply_to: None,
            references: Vec::new(),
            category: None,
            is_important: false,
            focus_override: None,
            invitation: None,
        }
    }

    #[test]
    fn initials_and_names() {
        assert_eq!(Address::named("Maya Chen", "m@x.io").initials(), "MC");
        assert_eq!(Address::new(None, "figma@updates.io").initials(), "F");
        assert_eq!(Address::new(None, "figma@updates.io").display_name(), "figma");
    }

    #[test]
    fn parses_recipient_lists() {
        let list = Address::parse_list("Maya Chen <maya@northstar.example>, alex@studio.example");
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].name.as_deref(), Some("Maya Chen"));
        assert_eq!(list[1].email, "alex@studio.example");
        assert!(list[1].looks_valid());
        assert!(!Address::new(None, "nope").looks_valid());
    }

    #[test]
    fn conversation_summary_names_other_participants() {
        let a = msg(Address::named("Maya Chen", "maya@northstar.example"), 1000);
        let mut b = msg(Address::named("Jason", "jason@example.com"), 2000);
        b.is_outgoing = true;
        b.is_read = true;
        let c = msg(Address::named("Maya Chen", "maya@northstar.example"), 3000);
        let summary =
            ConversationSummary::from_messages(&[a, b, c], "jason@example.com", &BTreeSet::new())
                .unwrap();
        assert_eq!(summary.message_count, 3);
        assert_eq!(summary.unread_count, 2);
        assert_eq!(summary.participant_line(), "Maya Chen");
        assert_eq!(summary.latest, 3000);
    }

    #[test]
    fn reply_quotes_and_sets_subject() {
        let source = msg(Address::named("Maya Chen", "maya@northstar.example"), 1000);
        let owner = Address::named("Jason", "jason@example.com");
        let draft = Draft::in_response_to(
            AccountId::from("acct"),
            DraftKind::Reply,
            &source,
            &owner,
            5000,
        );
        assert_eq!(draft.subject, "Re: Launch creative");
        assert_eq!(draft.to[0].email, "maya@northstar.example");
        // The original is quoted at send time, not pasted into the editable body.
        assert!(draft.include_quote && draft.body_text.is_empty());
        assert!(draft.validate_for_send().is_ok());
        let fwd = Draft::in_response_to(
            AccountId::from("acct"),
            DraftKind::Forward,
            &source,
            &owner,
            5000,
        );
        assert_eq!(fwd.subject, "Fwd: Launch creative");
        assert_eq!(fwd.validate_for_send(), Err(SendValidationError::NoRecipients));
    }

    #[test]
    fn a_contact_is_known_by_its_name_or_its_given_and_family_names() {
        let mut c = Contact { email: "maya@northstar.example".into(), ..Default::default() };
        assert_eq!((c.known_name(), c.display_name().as_str()), (None, "maya@northstar.example"));
        c.given_name = Some("Maya".into());
        c.family_name = Some("Chen".into());
        assert_eq!(c.known_name().as_deref(), Some("Maya Chen"));
        c.name = Some("  ".into());
        assert_eq!(c.known_name().as_deref(), Some("Maya Chen"), "a blank name does not count");
        c.name = Some("Maya C.".into());
        assert_eq!(c.display_name(), "Maya C.");
    }

    #[test]
    fn images_embedded_in_the_quote_do_not_make_a_draft_non_empty() {
        let attachment = |content_id: Option<&str>| DraftAttachment {
            id: AttachmentId::generate(),
            filename: "logo.png".into(),
            mime_type: "image/png".into(),
            size_bytes: 8,
            local_path: "/outgoing/logo.png".into(),
            content_id: content_id.map(str::to_owned),
        };
        let mut draft = Draft::new(AccountId::from("acct"), DraftKind::Forward, 0);
        draft.attachments = vec![attachment(Some("logo@brand.example"))];
        assert!(draft.is_empty());
        draft.attachments.push(attachment(None));
        assert!(!draft.is_empty());
    }

    #[test]
    fn human_sizes() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(2_457_600), "2.3 MB");
    }
}

/// Someone from the account's address book (Google Contacts) or its "other contacts": people
/// the user has corresponded with. Address-book entries can be edited; the rest are read-only.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Contact {
    /// The provider's resource name (`people/c123`, `otherContacts/c456`) or `local:<uuid>` for
    /// one created on this device and not yet pushed.
    pub id: String,
    pub account_id: AccountId,
    /// Primary address; empty for a contact without one.
    pub email: String,
    pub emails: Vec<String>,
    pub name: Option<String>,
    pub given_name: Option<String>,
    pub family_name: Option<String>,
    pub phone: Option<String>,
    pub organization: Option<String>,
    pub title: Option<String>,
    pub notes: Option<String>,
    pub photo_url: Option<String>,
    /// In the address book proper, as opposed to inferred from correspondence.
    pub in_address_book: bool,
    /// The provider's version marker, needed to update.
    pub etag: Option<String>,
    pub updated: Timestamp,
    /// Changed on this device and not yet pushed.
    pub pending: bool,
}

impl Contact {
    pub fn new_local(account_id: AccountId, now: Timestamp) -> Self {
        Self { id: format!("local:{}", uuid::Uuid::new_v4()), account_id, in_address_book: true, updated: now, pending: true, ..Default::default() }
    }

    pub fn address(&self) -> Address {
        Address { name: self.name.clone(), email: self.email.clone() }
    }

    /// The name, or the given and family names together, when either is known.
    pub fn known_name(&self) -> Option<String> {
        self.name.clone().filter(|n| !n.trim().is_empty()).or_else(|| {
            let full = format!("{} {}", self.given_name.clone().unwrap_or_default(), self.family_name.clone().unwrap_or_default());
            let full = full.trim().to_owned();
            (!full.is_empty()).then_some(full)
        })
    }

    /// Name, or the address, or a placeholder.
    pub fn display_name(&self) -> String {
        self.known_name()
            .or_else(|| (!self.email.is_empty()).then(|| self.email.clone()))
            .unwrap_or_else(|| "(No name)".into())
    }

    pub fn is_editable(&self) -> bool {
        self.in_address_book
    }

    pub fn has_email(&self, email: &str) -> bool {
        self.email.eq_ignore_ascii_case(email) || self.emails.iter().any(|e| e.eq_ignore_ascii_case(email))
    }
}

/// Automatic replies ("vacation responder") as the provider stores them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct VacationSettings {
    pub enabled: bool,
    pub subject: String,
    pub body: String,
    pub start: Option<Timestamp>,
    pub end: Option<Timestamp>,
    pub contacts_only: bool,
}
