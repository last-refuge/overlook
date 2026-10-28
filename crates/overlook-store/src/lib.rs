//! Durable local storage.
//!
//! One SQLite database per profile holds every account. The interface reads from here
//! immediately, online or offline. Each local mailbox change is written together with its
//! pending operation in a single transaction; callers may only report "Saved on this
//! device" after the call returns `Ok`.
//!
//! Protected data (drafts, Outbox items, outgoing attachments, pending operations) lives in
//! its own tables and is never touched by cache eviction.
//!
//! Downloaded mail is a replaceable cache for Gmail and IMAP accounts, where the server keeps
//! the primary copy. For POP3 accounts the local store *is* the primary copy: nothing may
//! evict their messages. Any eviction routine must check [`Store::is_local_primary`] first.

mod index;
mod calendar_connections;
mod caldav;
mod caldav_outbox;
mod schema;

pub use index::{apply_to_message, MailIndex};

use overlook_core::focus::{FocusRule, FocusTab};
use overlook_core::sanitize::html_to_text;
use overlook_core::time::{now, Timestamp};
use overlook_core::*;
use rusqlite::{params, Connection, OptionalExtension, Row, Transaction};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;
use std::sync::Mutex;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("serialization error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("{0}")]
    Invalid(String),
    /// Object-level authorization failed: the record belongs to another account.
    #[error("that {0} belongs to a different account")]
    NotOwned(&'static str),
}

/// Tables whose rows carry an `account_id`, for object-level authorization. The table name
/// comes from this fixed list, never from input.
#[derive(Clone, Copy, Debug)]
enum Owned {
    Message,
    Label,
    Draft,
    Calendar,
    Event,
}

impl Owned {
    fn table(self) -> &'static str {
        match self {
            Owned::Message => "messages",
            Owned::Label => "labels",
            Owned::Draft => "drafts",
            Owned::Calendar => "calendars",
            Owned::Event => "events",
        }
    }

    fn noun(self) -> &'static str {
        match self {
            Owned::Message => "message",
            Owned::Label => "label",
            Owned::Draft => "draft",
            Owned::Calendar => "calendar",
            Owned::Event => "event",
        }
    }
}

/// Object-level authorization for create, read, update and delete by id: every existing
/// record named must belong to `account`. Ids that do not exist are left to the caller
/// (a create, or a no-op). Unpredictable ids are not relied on.
fn ensure_owned<'a>(tx: &Connection, what: Owned, account: &AccountId, ids: impl IntoIterator<Item = &'a str>) -> Result<()> {
    let sql = format!("SELECT account_id FROM {} WHERE id=?1", what.table());
    let mut statement = tx.prepare_cached(&sql)?;
    for id in ids {
        let owner: Option<String> = statement.query_row([id], |r| r.get(0)).optional()?;
        if owner.is_some_and(|owner| owner != account.0) {
            tracing::warn!(target: "overlook::access", kind = what.noun(), "refused a change to another account's record");
            return Err(StoreError::NotOwned(what.noun()));
        }
    }
    Ok(())
}

/// Refuses a record whose own `account_id` names a different account than the caller's.
fn ensure_same_account(record_account: &AccountId, account: &AccountId, noun: &'static str) -> Result<()> {
    if record_account != account {
        tracing::warn!(target: "overlook::access", kind = noun, "refused a change to another account's record");
        return Err(StoreError::NotOwned(noun));
    }
    Ok(())
}

pub type Result<T> = std::result::Result<T, StoreError>;

/// Which mailbox the list is showing.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum MailboxView {
    Kind(LabelKind),
    Label(LabelId),
}

/// Secondary filter tabs above the message list.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum ListFilter {
    #[default]
    All,
    Unread,
    Attachments,
}

/// A local mailbox change. Each variant maps to one [`OperationKind`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LocalChange {
    SetRead { message_ids: Vec<MessageId>, read: bool },
    SetStarred { message_ids: Vec<MessageId>, starred: bool },
    AddLabel { message_ids: Vec<MessageId>, label_id: LabelId },
    RemoveLabel { message_ids: Vec<MessageId>, label_id: LabelId },
    Archive { message_ids: Vec<MessageId> },
    Trash { message_ids: Vec<MessageId> },
    Untrash { message_ids: Vec<MessageId> },
}

impl LocalChange {
    pub fn message_ids(&self) -> &[MessageId] {
        match self {
            LocalChange::SetRead { message_ids, .. }
            | LocalChange::SetStarred { message_ids, .. }
            | LocalChange::AddLabel { message_ids, .. }
            | LocalChange::RemoveLabel { message_ids, .. }
            | LocalChange::Archive { message_ids }
            | LocalChange::Trash { message_ids }
            | LocalChange::Untrash { message_ids } => message_ids,
        }
    }

    fn into_operation(self) -> OperationKind {
        match self {
            LocalChange::SetRead { message_ids, read } => OperationKind::SetRead { message_ids, read },
            LocalChange::SetStarred { message_ids, starred } => OperationKind::SetStarred { message_ids, starred },
            LocalChange::AddLabel { message_ids, label_id } => OperationKind::AddLabel { message_ids, label_id },
            LocalChange::RemoveLabel { message_ids, label_id } => OperationKind::RemoveLabel { message_ids, label_id },
            LocalChange::Archive { message_ids } => OperationKind::Archive { message_ids },
            LocalChange::Trash { message_ids } => OperationKind::Trash { message_ids },
            LocalChange::Untrash { message_ids } => OperationKind::Untrash { message_ids },
        }
    }
}

pub struct Store {
    conn: Mutex<Connection>,
    /// A second connection for the reads the window does on every action (the mail index,
    /// the open conversation). With WAL it reads the last committed state while a sync
    /// holds `conn` for a write, so the UI never waits for a download. `None` in memory.
    reader: Option<Mutex<Connection>>,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let mut store = Self::init(Connection::open(path)?)?;
        let reader = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
        reader.busy_timeout(std::time::Duration::from_secs(5))?;
        store.reader = Some(Mutex::new(reader));
        Ok(store)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        schema::ensure_supported(&conn)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        schema::migrate(&conn)?;
        Ok(Self { conn: Mutex::new(conn), reader: None })
    }

    fn with_tx<T>(&self, f: impl FnOnce(&Transaction) -> Result<T>) -> Result<T> {
        let mut conn = self.conn.lock().expect("store mutex poisoned");
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let out = f(&tx)?;
        tx.commit()?;
        Ok(out)
    }

    fn with_conn<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        f(&conn)
    }

    /// Runs a read-only query on the reader connection when there is one.
    fn with_read<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        match &self.reader {
            Some(reader) => f(&reader.lock().expect("store reader mutex poisoned")),
            None => self.with_conn(f),
        }
    }

    // ----------------------------------------------------------------- accounts

    pub fn upsert_account(&self, account: &Account) -> Result<()> {
        self.with_conn(|c| {
            c.execute(
                "INSERT INTO accounts(id, provider, display_name, email, created_at) VALUES(?1,?2,?3,?4,?5)
                 ON CONFLICT(id) DO UPDATE SET provider=excluded.provider, display_name=excluded.display_name, email=excluded.email",
                params![account.id.0, account.provider.as_key(), account.display_name, account.email, account.created_at],
            )?;
            Ok(())
        })
    }

    pub fn accounts(&self) -> Result<Vec<Account>> {
        self.with_read(|c| {
            let mut stmt = c.prepare("SELECT id, provider, display_name, email, created_at FROM accounts ORDER BY created_at")?;
            let rows = stmt.query_map([], |r| {
                Ok(Account {
                    id: AccountId(r.get(0)?),
                    provider: ProviderKind::from_key(&r.get::<_, String>(1)?).unwrap_or(ProviderKind::Sample),
                    display_name: r.get(2)?,
                    email: r.get(3)?,
                    created_at: r.get(4)?,
                })
            })?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
    }

    pub fn account_by_email(&self, provider: ProviderKind, email: &str) -> Result<Option<Account>> {
        Ok(self
            .accounts()?
            .into_iter()
            .find(|a| a.provider == provider && a.email.eq_ignore_ascii_case(email)))
    }

    /// Removes an account and everything that belongs to it (cascade).
    pub fn delete_account(&self, account: &AccountId) -> Result<()> {
        self.with_tx(|tx| {
            tx.execute("DELETE FROM accounts WHERE id=?1", [&account.0])?;
            Ok(())
        })
    }

    // ------------------------------------------------------------------- labels

    /// Replaces the account's label list with the provider's current list. Labels that no
    /// longer exist are removed (and detached from messages by cascade).
    pub fn replace_labels(&self, account: &AccountId, labels: &[Label]) -> Result<()> {
        self.with_tx(|tx| {
            for label in labels {
                tx.execute(
                    "INSERT INTO labels(id, account_id, kind, name, provider_id, color, sort_order) VALUES(?1,?2,?3,?4,?5,?6,?7)
                     ON CONFLICT(id) DO UPDATE SET kind=excluded.kind, name=excluded.name, provider_id=excluded.provider_id, color=excluded.color, sort_order=excluded.sort_order",
                    params![label.id.0, label.account_id.0, label.kind.as_key(), label.name, label.provider_id.as_ref().map(|p| &p.0), label.color, label.sort_order],
                )?;
            }
            let keep: Vec<String> = labels.iter().map(|l| l.id.0.clone()).collect();
            let mut stmt = tx.prepare("SELECT id FROM labels WHERE account_id=?1")?;
            let existing: Vec<String> = stmt.query_map([&account.0], |r| r.get(0))?.collect::<std::result::Result<_, _>>()?;
            for id in existing.into_iter().filter(|id| !keep.contains(id)) {
                tx.execute("DELETE FROM labels WHERE id=?1", [&id])?;
            }
            Ok(())
        })
    }

    pub fn upsert_label(&self, label: &Label) -> Result<()> {
        self.with_conn(|c| {
            c.execute(
                "INSERT INTO labels(id, account_id, kind, name, provider_id, color, sort_order) VALUES(?1,?2,?3,?4,?5,?6,?7)
                 ON CONFLICT(id) DO UPDATE SET kind=excluded.kind, name=excluded.name, provider_id=excluded.provider_id, color=excluded.color, sort_order=excluded.sort_order",
                params![label.id.0, label.account_id.0, label.kind.as_key(), label.name, label.provider_id.as_ref().map(|p| &p.0), label.color, label.sort_order],
            )?;
            Ok(())
        })
    }

    pub fn labels(&self, account: &AccountId) -> Result<Vec<Label>> {
        self.with_read(|c| {
            let mut stmt = c.prepare("SELECT id, account_id, kind, name, provider_id, color, sort_order FROM labels WHERE account_id=?1 ORDER BY sort_order, name")?;
            let rows = stmt.query_map([&account.0], row_to_label)?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
    }

    pub fn label_by_kind(&self, account: &AccountId, kind: LabelKind) -> Result<Option<Label>> {
        self.with_read(|c| {
            Ok(c.query_row(
                "SELECT id, account_id, kind, name, provider_id, color, sort_order FROM labels WHERE account_id=?1 AND kind=?2 LIMIT 1",
                params![account.0, kind.as_key()],
                row_to_label,
            )
            .optional()?)
        })
    }

    // ----------------------------------------------------------------- messages

    /// Inserts or replaces messages, including their labels and attachments.
    pub fn upsert_messages(&self, messages: &[Message]) -> Result<()> {
        self.with_tx(|tx| {
            for m in messages {
                upsert_message_tx(tx, m)?;
            }
            Ok(())
        })
    }

    /// Merges messages downloaded from the provider into local state.
    ///
    /// Messages referenced by pending local operations keep their local flags and labels
    /// (the pending intent wins until it is replayed). A download without a cached body
    /// keeps the body already on disk.
    pub fn merge_remote_messages(&self, messages: &[Message], protected: &BTreeSet<MessageId>) -> Result<()> {
        // For local-primary accounts (POP3) read, star and label state exists only here: a
        // message served again by the provider (a retried download, a re-parse) must not
        // reset what the user did to it.
        let mut local_primary: HashSet<AccountId> = HashSet::new();
        for account in messages.iter().map(|m| &m.account_id).collect::<HashSet<_>>() {
            if self.is_local_primary(account)? {
                local_primary.insert(account.clone());
            }
        }
        self.with_tx(|tx| {
            for incoming in messages {
                let mut existing = load_messages(tx, "WHERE m.id=?1", params![incoming.id.0])?.pop();
                let mut merged = incoming.clone();
                if let Some(local) = existing.take() {
                    if merged.conversation_id.0.is_empty() {
                        // A header-only refresh from a provider without thread ids keeps the
                        // conversation the message was threaded into.
                        merged.conversation_id = local.conversation_id.clone();
                    }
                    if !merged.body.is_cached() {
                        // A header-only row (IMAP flag refresh, new mail before its body
                        // arrives) must not blank what the body download already filled in.
                        merged.body = local.body;
                        if merged.snippet.is_empty() {
                            merged.snippet = local.snippet;
                        }
                        if merged.invitation.is_none() {
                            merged.invitation = local.invitation;
                        }
                    }
                    if merged.attachments.is_empty() {
                        merged.attachments = local.attachments;
                    } else {
                        // Keep downloaded attachment files.
                        for a in &mut merged.attachments {
                            if let Some(l) = local.attachments.iter().find(|l| l.filename == a.filename && l.size_bytes == a.size_bytes) {
                                a.local_path = l.local_path.clone();
                            }
                        }
                    }
                    if merged.focus_override.is_none() {
                        merged.focus_override = local.focus_override;
                    }
                    if protected.contains(&incoming.id) || local_primary.contains(&incoming.account_id) {
                        merged.is_read = local.is_read;
                        merged.is_starred = local.is_starred;
                        merged.label_ids = local.label_ids;
                    }
                }
                upsert_message_tx(tx, &merged)?;
            }
            Ok(())
        })
    }

    /// Removes messages deleted at the provider.
    pub fn delete_messages(&self, ids: &[MessageId]) -> Result<usize> {
        self.with_tx(|tx| {
            let mut n = 0;
            for id in ids {
                n += tx.execute("DELETE FROM messages WHERE id=?1", [&id.0])?;
            }
            Ok(n)
        })
    }

    pub fn message_count(&self, account: &AccountId) -> Result<usize> {
        self.with_read(|c| Ok(c.query_row("SELECT COUNT(*) FROM messages WHERE account_id=?1", [&account.0], |r| r.get::<_, i64>(0))? as usize))
    }

    pub fn message(&self, id: &MessageId) -> Result<Option<Message>> {
        self.with_read(|c| {
            let mut found = load_messages(c, "WHERE m.id=?1", params![id.0])?;
            Ok(found.pop())
        })
    }

    pub fn messages_in_conversation(&self, account: &AccountId, conversation: &ConversationId) -> Result<Vec<Message>> {
        self.with_read(|c| {
            let mut msgs = load_messages(c, "WHERE m.account_id=?1 AND m.conversation_id=?2", params![account.0, conversation.0])?;
            msgs.sort_by_key(|m| m.date);
            Ok(msgs)
        })
    }

    /// All messages of an account (prototype scale). Used to compute conversation lists.
    pub fn messages_for_account(&self, account: &AccountId) -> Result<Vec<Message>> {
        self.with_read(|c| load_messages(c, "WHERE m.account_id=?1", params![account.0]))
    }

    /// Every message of an account without bodies: enough for lists, counts and address
    /// lookups, at a fraction of the cost of [`Store::messages_for_account`].
    pub fn message_headers_for_account(&self, account: &AccountId) -> Result<Vec<Message>> {
        self.with_read(|c| load_messages_with(c, "WHERE m.account_id=?1", params![account.0], false))
    }

    /// Every message of an account without its body, with labels, pending changes and
    /// Focused Inbox rules: enough to build any conversation list or count in memory.
    pub fn mail_index(&self, account: &AccountId) -> Result<MailIndex> {
        let labels = self.labels(account)?;
        let messages = self.message_headers_for_account(account)?;
        let pending = self.pending_message_ids(account)?;
        let rules = self.focus_rule_map(account)?;
        Ok(MailIndex::new(labels, messages, pending, rules))
    }

    /// Conversation summaries for a mailbox view, newest first. `focus` splits the Inbox
    /// into Focused and Other; `None` shows everything.
    pub fn conversations(
        &self,
        account: &AccountId,
        owner_email: &str,
        view: &MailboxView,
        filter: ListFilter,
        focus: Option<FocusTab>,
    ) -> Result<Vec<ConversationSummary>> {
        Ok(self.mail_index(account)?.conversations(owner_email, view, filter, focus))
    }

    /// Unread conversation count for Inbox, starred count, and so on, for sidebar badges.
    pub fn view_count(&self, account: &AccountId, owner_email: &str, view: &MailboxView, focus: Option<FocusTab>) -> Result<usize> {
        Ok(self.mail_index(account)?.view_count(owner_email, view, focus))
    }

    // ------------------------------------------------------------ focused inbox

    pub fn focus_rules(&self, account: &AccountId) -> Result<Vec<FocusRule>> {
        self.with_read(|c| {
            let mut stmt = c.prepare("SELECT email, focused FROM focus_rules WHERE account_id=?1 ORDER BY email")?;
            let rows = stmt.query_map([&account.0], |r| Ok(FocusRule { email: r.get(0)?, focused: r.get(1)? }))?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
    }

    fn focus_rule_map(&self, account: &AccountId) -> Result<HashMap<String, bool>> {
        Ok(self.focus_rules(account)?.into_iter().map(|r| (r.email.to_ascii_lowercase(), r.focused)).collect())
    }

    /// Sets (or with `None` clears) the standing rule for a sender.
    pub fn set_focus_rule(&self, account: &AccountId, email: &str, focused: Option<bool>) -> Result<()> {
        let email = email.to_ascii_lowercase();
        self.with_conn(|c| {
            match focused {
                Some(f) => {
                    c.execute(
                        "INSERT INTO focus_rules(account_id, email, focused) VALUES(?1,?2,?3) ON CONFLICT(account_id, email) DO UPDATE SET focused=excluded.focused",
                        params![account.0, email, f],
                    )?;
                }
                None => {
                    c.execute("DELETE FROM focus_rules WHERE account_id=?1 AND email=?2", params![account.0, email])?;
                }
            }
            Ok(())
        })
    }

    /// Per-message override for Focused Inbox (`None` clears it).
    pub fn set_focus_override(&self, account: &AccountId, ids: &[MessageId], focused: Option<bool>) -> Result<()> {
        self.with_tx(|tx| {
            ensure_owned(tx, Owned::Message, account, ids.iter().map(|i| i.0.as_str()))?;
            for id in ids {
                tx.execute("UPDATE messages SET focus_override=?2 WHERE id=?1", params![id.0, focused])?;
            }
            Ok(())
        })
    }

    /// Lowercase addresses the account owner has sent mail to.
    pub fn sent_recipients(&self, account: &AccountId) -> Result<HashSet<String>> {
        let mut out = HashSet::new();
        for m in self.message_headers_for_account(account)? {
            if m.is_outgoing {
                for a in m.to.iter().chain(m.cc.iter()) {
                    out.insert(a.email.to_ascii_lowercase());
                }
            }
        }
        Ok(out)
    }

    // ---------------------------------------------------------------- calendars

    /// Replaces the account's calendars with the provider's list, keeping the user's
    /// visibility choices for calendars that still exist.
    pub fn replace_calendars(&self, account: &AccountId, calendars: &[Calendar]) -> Result<()> {
        self.with_tx(|tx| {
            let mut stmt = tx.prepare("SELECT id, visible FROM calendars WHERE account_id=?1")?;
            let existing: HashMap<String, bool> = stmt
                .query_map([&account.0], |r| Ok((r.get::<_, String>(0)?, r.get::<_, bool>(1)?)))?
                .collect::<std::result::Result<_, _>>()?;
            drop(stmt);
            for c in calendars {
                let visible = existing.get(&c.id.0).copied().unwrap_or(c.visible);
                tx.execute(
                    "INSERT INTO calendars(id, account_id, provider_id, name, color, is_primary, can_edit, visible, sort_order, default_reminders_json) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)
                     ON CONFLICT(id) DO UPDATE SET provider_id=excluded.provider_id, name=excluded.name, color=excluded.color, is_primary=excluded.is_primary,
                       can_edit=excluded.can_edit, visible=excluded.visible, sort_order=excluded.sort_order, default_reminders_json=excluded.default_reminders_json",
                    params![c.id.0, c.account_id.0, c.provider_id.as_ref().map(|p| &p.0), c.name, c.color, c.is_primary, c.can_edit, visible, c.sort_order, serde_json::to_string(&c.default_reminders)?],
                )?;
            }
            let keep: Vec<&str> = calendars.iter().map(|c| c.id.0.as_str()).collect();
            for id in existing.keys().filter(|id| !keep.contains(&id.as_str())) {
                tx.execute("DELETE FROM calendars WHERE id=?1", [id])?;
            }
            Ok(())
        })
    }

    pub fn calendars(&self, account: &AccountId) -> Result<Vec<Calendar>> {
        self.with_read(|c| {
            let mut stmt = c.prepare("SELECT id, account_id, provider_id, name, color, is_primary, can_edit, visible, sort_order, default_reminders_json FROM calendars WHERE account_id=?1 ORDER BY is_primary DESC, sort_order, name")?;
            let rows = stmt.query_map([&account.0], |r| {
                Ok(Calendar {
                    id: CalendarId(r.get(0)?),
                    account_id: AccountId(r.get(1)?),
                    provider_id: r.get::<_, Option<String>>(2)?.map(ProviderId),
                    name: r.get(3)?,
                    color: r.get(4)?,
                    is_primary: r.get(5)?,
                    can_edit: r.get(6)?,
                    visible: r.get(7)?,
                    sort_order: r.get(8)?,
                    default_reminders: serde_json::from_str(&r.get::<_, String>(9)?).unwrap_or_default(),
                })
            })?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
    }

    pub fn set_calendar_visible(&self, account: &AccountId, id: &CalendarId, visible: bool) -> Result<()> {
        self.with_tx(|tx| {
            ensure_owned(tx, Owned::Calendar, account, [id.0.as_str()])?;
            tx.execute("UPDATE calendars SET visible=?2 WHERE id=?1 AND account_id=?3", params![id.0, visible, account.0])?;
            Ok(())
        })
    }

    pub fn upsert_events(&self, events: &[Event]) -> Result<()> {
        self.with_tx(|tx| {
            for e in events {
                upsert_event_tx(tx, e)?;
            }
            Ok(())
        })
    }

    pub fn delete_events(&self, ids: &[EventId]) -> Result<usize> {
        self.with_tx(|tx| {
            let mut n = 0;
            for id in ids {
                n += tx.execute("DELETE FROM events WHERE id=?1", [&id.0])?;
            }
            Ok(n)
        })
    }

    pub fn event_by_ical_uid(&self, account: &AccountId, uid: &str) -> Result<Option<Event>> {
        self.with_read(|c| {
            let mut found = load_events(c, "WHERE e.account_id=?1 AND e.ical_uid=?2 AND e.status != 'cancelled' ORDER BY e.start", params![account.0, uid])?;
            // Prefer the next upcoming instance for recurring series.
            let now_ms = now();
            let upcoming = found.iter().position(|e| e.end >= now_ms);
            Ok(match upcoming {
                Some(i) => Some(found.swap_remove(i)),
                None => found.pop(),
            })
        })
    }

    /// Events (visible calendars) starting within `[from, to)`, for reminders.
    pub fn events_starting_between(&self, account: &AccountId, from: Timestamp, to: Timestamp) -> Result<Vec<Event>> {
        self.with_read(|c| {
            load_events(
                c,
                "JOIN calendars cal ON cal.id = e.calendar_id WHERE e.account_id=?1 AND cal.visible=1 AND e.start >= ?2 AND e.start < ?3 AND e.status != 'cancelled' AND e.all_day = 0",
                params![account.0, from, to],
            )
        })
    }

    pub fn event(&self, id: &EventId) -> Result<Option<Event>> {
        self.with_read(|c| Ok(load_events(c, "WHERE e.id=?1", params![id.0])?.pop()))
    }

    /// Events of visible calendars overlapping `[from, to)`, cancelled ones excluded, with
    /// `pending` set for events referenced by queued operations.
    pub fn events_between(&self, account: &AccountId, from: Timestamp, to: Timestamp) -> Result<Vec<Event>> {
        let pending: HashSet<EventId> = self
            .pending_operations(account)?
            .iter()
            .filter_map(|op| op.kind.event_id().cloned())
            .collect();
        self.with_read(|c| {
            let mut events = load_events(
                c,
                "JOIN calendars cal ON cal.id = e.calendar_id WHERE e.account_id=?1 AND cal.visible=1 AND e.start < ?3 AND e.end > ?2 AND e.status != 'cancelled'",
                params![account.0, from, to],
            )?;
            for e in &mut events {
                e.pending |= pending.contains(&e.id);
            }
            events.sort_by_key(|e| (e.start, e.end));
            Ok(events)
        })
    }

    /// Writes a local event change and its pending operation atomically.
    pub fn apply_event_change(&self, account: &AccountId, event: &Event, kind: OperationKind) -> Result<PendingOperation> {
        let ts = now();
        ensure_same_account(&event.account_id, account, "event")?;
        self.with_tx(|tx| {
            ensure_owned(tx, Owned::Calendar, account, [event.calendar_id.0.as_str()])?;
            ensure_owned(tx, Owned::Event, account, [event.id.0.as_str()])?;
            caldav::reject_caldav_write(tx, &event.calendar_id)?;
            upsert_event_tx(tx, event)?;
            insert_operation_tx(tx, account, kind, ts)
        })
    }

    /// Removes an event locally and queues its deletion.
    pub fn delete_event_locally(&self, account: &AccountId, event: &Event) -> Result<PendingOperation> {
        self.delete_event_locally_scoped(account, event, RecurrenceScope::This)
    }

    /// The local occurrences of `event`'s series that `scope` covers, `event` included.
    fn series_members_tx(tx: &Connection, account: &AccountId, event: &Event, scope: RecurrenceScope) -> Result<Vec<Event>> {
        let Some(master) = event.recurring_event_id.as_deref().filter(|_| scope != RecurrenceScope::This) else { return Ok(vec![event.clone()]) };
        let mut members = load_events(tx, "WHERE e.account_id=?1 AND e.recurring_event_id=?2 ORDER BY e.start", params![account.0, master])?;
        if scope == RecurrenceScope::Following {
            let split = event.split_point();
            members.retain(|m| m.id == event.id || m.split_point() >= split);
        }
        if !members.iter().any(|m| m.id == event.id) {
            members.push(event.clone());
        }
        Ok(members)
    }

    /// Removes an event, or the part of its series `scope` names, and queues one deletion.
    pub fn delete_event_locally_scoped(&self, account: &AccountId, event: &Event, scope: RecurrenceScope) -> Result<PendingOperation> {
        let ts = now();
        self.with_tx(|tx| {
            ensure_owned(tx, Owned::Calendar, account, [event.calendar_id.0.as_str()])?;
            ensure_owned(tx, Owned::Event, account, [event.id.0.as_str()])?;
            caldav::reject_caldav_write(tx, &event.calendar_id)?;
            for member in Self::series_members_tx(tx, account, event, scope)? {
                tx.execute("DELETE FROM events WHERE id=?1", [&member.id.0])?;
            }
            insert_operation_tx(
                tx,
                account,
                OperationKind::DeleteEvent {
                    event_id: event.id.clone(),
                    calendar_id: event.calendar_id.clone(),
                    provider_id: event.provider_id.clone(),
                    scope,
                    recurring_event_id: event.recurring_event_id.clone(),
                    original_start: Some(event.split_point()),
                },
                ts,
            )
        })
    }

    /// Saves an edited occurrence and, for a wider scope, applies the same edit to the other
    /// local occurrences the scope covers: texts and settings as given, times shifted by the
    /// same amount, with the duration of the edited one. One operation carries the scope.
    pub fn apply_series_change(&self, account: &AccountId, event: &Event, scope: RecurrenceScope) -> Result<PendingOperation> {
        let ts = now();
        let before = self.event(&event.id)?;
        let delta = before.as_ref().map(|b| event.start - b.start).unwrap_or(0);
        let duration = event.end - event.start;
        self.with_tx(|tx| {
            ensure_owned(tx, Owned::Calendar, account, [event.calendar_id.0.as_str()])?;
            ensure_owned(tx, Owned::Event, account, [event.id.0.as_str()])?;
            caldav::reject_caldav_write(tx, &event.calendar_id)?;
            let members = before.as_ref().map(|b| Self::series_members_tx(tx, account, b, scope)).transpose()?.unwrap_or_default();
            for member in members {
                if member.id == event.id {
                    continue;
                }
                let mut m = member;
                m.summary = event.summary.clone();
                m.description = event.description.clone();
                m.location = event.location.clone();
                m.color = event.color.clone();
                m.reminders = event.reminders.clone();
                m.attendees = event.attendees.clone();
                m.wants_conference = event.wants_conference;
                m.start += delta;
                m.end = m.start + duration;
                m.all_day = event.all_day;
                m.updated = ts;
                upsert_event_tx(tx, &m)?;
            }
            upsert_event_tx(tx, event)?;
            insert_operation_tx(tx, account, OperationKind::UpdateEvent { event_id: event.id.clone(), scope }, ts)
        })
    }

    /// Provider ids of messages that carry a calendar attachment but no parsed invitation
    /// (stored before invitation parsing existed).
    /// Provider ids of messages whose HTML embeds `cid:` images but whose attachments were
    /// stored before `Content-ID` was recorded.
    pub fn messages_needing_inline_parse(&self, account: &AccountId) -> Result<Vec<ProviderId>> {
        self.with_read(|c| {
            let mut stmt = c.prepare(
                "SELECT DISTINCT m.provider_id FROM messages m
                 WHERE m.account_id=?1 AND m.provider_id IS NOT NULL AND m.body_html LIKE '%cid:%'
                   AND NOT EXISTS (SELECT 1 FROM attachments a WHERE a.message_id = m.id AND a.content_id IS NOT NULL)",
            )?;
            let rows = stmt.query_map([&account.0], |r| Ok(ProviderId(r.get(0)?)))?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
    }

    pub fn messages_needing_invitation_parse(&self, account: &AccountId) -> Result<Vec<ProviderId>> {
        self.with_read(|c| {
            let mut stmt = c.prepare(
                "SELECT DISTINCT m.provider_id FROM messages m JOIN attachments a ON a.message_id = m.id
                 WHERE m.account_id=?1 AND m.invitation_json IS NULL AND m.provider_id IS NOT NULL
                   AND (a.mime_type LIKE 'text/calendar%' OR a.mime_type LIKE 'application/ics%' OR lower(a.filename) LIKE '%.ics')",
            )?;
            let rows = stmt.query_map([&account.0], |r| Ok(ProviderId(r.get(0)?)))?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
    }

    // -------------------------------------------------------------- attachments

    pub fn set_attachment_local_path(&self, id: &AttachmentId, path: Option<&str>) -> Result<()> {
        self.with_conn(|c| {
            c.execute("UPDATE attachments SET local_path=?2 WHERE id=?1", params![id.0, path])?;
            Ok(())
        })
    }

    // ------------------------------------------------------- local changes + ops

    /// Applies a local change and records its pending operation atomically.
    pub fn apply_change(&self, account: &AccountId, change: LocalChange) -> Result<PendingOperation> {
        let ts = now();
        self.with_tx(|tx| {
            apply_change_tx(tx, account, &change)?;
            let op = insert_operation_tx(tx, account, change.into_operation(), ts)?;
            Ok(op)
        })
    }

    pub fn pending_operations(&self, account: &AccountId) -> Result<Vec<PendingOperation>> {
        self.with_read(|c| {
            let mut stmt = c.prepare(
                "SELECT id, account_id, seq, kind_json, state, attempts, last_error, created_at, updated_at FROM pending_operations
                 WHERE account_id=?1 AND state != 'applied' ORDER BY seq",
            )?;
            let rows = stmt.query_map([&account.0], row_to_operation)?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
    }

    pub fn pending_count(&self, account: &AccountId) -> Result<usize> {
        self.with_read(|c| {
            Ok(c.query_row(
                "SELECT COUNT(*) FROM pending_operations WHERE account_id=?1 AND state != 'applied'",
                [&account.0],
                |r| r.get::<_, i64>(0),
            )? as usize)
        })
    }

    /// Message ids referenced by operations that have not been applied yet.
    /// Events touched by operations that have not reached the provider yet. A download must
    /// not overwrite or resurrect them (for example a deleted event the server still lists).
    pub fn pending_event_ids(&self, account: &AccountId) -> Result<BTreeSet<EventId>> {
        let ops = self.pending_operations(account)?;
        Ok(ops.iter().filter_map(|op| op.kind.event_id().cloned()).collect())
    }

    pub fn pending_message_ids(&self, account: &AccountId) -> Result<BTreeSet<MessageId>> {
        let ops = self.pending_operations(account)?;
        let mut ids = BTreeSet::new();
        for op in ops {
            ids.extend(op.kind.message_ids().iter().cloned());
        }
        Ok(ids)
    }

    pub fn set_operation_state(&self, id: &OperationId, state: OperationState, error: Option<&str>) -> Result<()> {
        self.with_conn(|c| {
            let bump = matches!(state, OperationState::InFlight) as i64;
            c.execute(
                "UPDATE pending_operations SET state=?2, last_error=?3, attempts=attempts+?4, updated_at=?5 WHERE id=?1",
                params![id.0, state.as_key(), error, bump, now()],
            )?;
            Ok(())
        })
    }

    /// Removes applied operations older than `before`.
    pub fn prune_applied(&self, before: Timestamp) -> Result<usize> {
        self.with_conn(|c| Ok(c.execute("DELETE FROM pending_operations WHERE state='applied' AND updated_at < ?1", [before])?))
    }

    // ------------------------------------------------------------------- drafts

    pub fn save_draft(&self, draft: &Draft) -> Result<()> {
        self.with_tx(|tx| save_draft_tx(tx, draft))
    }

    /// Saves a draft only while it is still being edited (or new) and only over an older
    /// copy. A background save that lands late must neither turn a queued message back into
    /// a draft nor replace text saved after it.
    pub fn save_editing_draft(&self, draft: &Draft) -> Result<bool> {
        self.with_tx(|tx| {
            let current = load_drafts(tx, "WHERE d.id=?1", params![draft.id.0])?.pop();
            if current.is_some_and(|d| d.state != DraftState::Editing || d.updated_at > draft.updated_at) {
                return Ok(false);
            }
            save_draft_tx(tx, draft)?;
            Ok(true)
        })
    }

    pub fn draft(&self, id: &DraftId) -> Result<Option<Draft>> {
        self.with_read(|c| {
            let mut found = load_drafts(c, "WHERE d.id=?1", params![id.0])?;
            Ok(found.pop())
        })
    }

    /// Drafts of an account. `outbox` selects Outbox states instead of editing drafts.
    pub fn drafts(&self, account: &AccountId, outbox: bool) -> Result<Vec<Draft>> {
        self.with_read(|c| {
            let mut all = load_drafts(c, "WHERE d.account_id=?1", params![account.0])?;
            all.retain(|d| d.state.is_outbox() == outbox);
            all.sort_by_key(|d| std::cmp::Reverse(d.updated_at));
            Ok(all)
        })
    }

    pub fn delete_draft(&self, account: &AccountId, id: &DraftId) -> Result<()> {
        self.with_tx(|tx| {
            ensure_owned(tx, Owned::Draft, account, [id.0.as_str()])?;
            tx.execute("DELETE FROM drafts WHERE id=?1", [&id.0])?;
            // Any queued send for this draft is cancelled with it.
            tx.execute(
                "DELETE FROM pending_operations WHERE state='pending' AND kind_json LIKE ?1",
                [format!("%\"draft_id\":\"{}\"%", id.0)],
            )?;
            Ok(())
        })
    }

    /// Moves a draft to the Outbox and queues its send in the same transaction.
    pub fn queue_send(&self, draft: &Draft) -> Result<PendingOperation> {
        draft.validate_for_send().map_err(|e| StoreError::Invalid(e.to_string()))?;
        let ts = now();
        let mut queued = draft.clone();
        queued.state = DraftState::Queued;
        queued.updated_at = ts;
        self.with_tx(|tx| {
            save_draft_tx(tx, &queued)?;
            insert_operation_tx(tx, &draft.account_id, OperationKind::SendDraft { draft_id: draft.id.clone() }, ts)
        })
    }

    /// Returns a queued (not yet in-flight) draft to editing and removes its send operation.
    pub fn cancel_send(&self, account: &AccountId, id: &DraftId) -> Result<Draft> {
        self.with_tx(|tx| {
            ensure_owned(tx, Owned::Draft, account, [id.0.as_str()])?;
            let mut found = load_drafts(tx, "WHERE d.id=?1", params![id.0])?;
            let mut draft = found.pop().ok_or_else(|| StoreError::NotFound(id.0.clone()))?;
            if !matches!(draft.state, DraftState::Queued | DraftState::Failed(_)) {
                return Err(StoreError::Invalid("This message is already being sent.".into()));
            }
            draft.state = DraftState::Editing;
            draft.updated_at = now();
            save_draft_tx(tx, &draft)?;
            tx.execute(
                "DELETE FROM pending_operations WHERE state IN ('pending','failed') AND kind_json LIKE ?1",
                [format!("%\"draft_id\":\"{}\"%", id.0)],
            )?;
            Ok(draft)
        })
    }

    /// Queues an uncertain or failed send again: the draft returns to `Queued` and its
    /// operation to `Pending`, so the next sync submits it (a fresh attempt, no reconcile).
    pub fn resend(&self, account: &AccountId, id: &DraftId) -> Result<Draft> {
        self.with_tx(|tx| {
            ensure_owned(tx, Owned::Draft, account, [id.0.as_str()])?;
            let mut found = load_drafts(tx, "WHERE d.id=?1", params![id.0])?;
            let mut draft = found.pop().ok_or_else(|| StoreError::NotFound(id.0.clone()))?;
            if !matches!(draft.state, DraftState::Uncertain | DraftState::Failed(_)) {
                return Err(StoreError::Invalid("This message is not waiting on a decision.".into()));
            }
            draft.state = DraftState::Queued;
            draft.updated_at = now();
            save_draft_tx(tx, &draft)?;
            let n = tx.execute(
                "UPDATE pending_operations SET state='pending', last_error=NULL, updated_at=?2 WHERE state IN ('uncertain','failed') AND kind_json LIKE ?1",
                params![format!("%\"draft_id\":\"{}\"%", id.0), now()],
            )?;
            if n == 0 {
                insert_operation_tx(tx, &draft.account_id, OperationKind::SendDraft { draft_id: draft.id.clone() }, now())?;
            }
            Ok(draft)
        })
    }

    /// Drops an uncertain or failed send together with its draft. The message may or may
    /// not have left the server; the user has decided not to send it again.
    pub fn discard_send(&self, account: &AccountId, id: &DraftId) -> Result<()> {
        self.with_tx(|tx| {
            ensure_owned(tx, Owned::Draft, account, [id.0.as_str()])?;
            let mut found = load_drafts(tx, "WHERE d.id=?1", params![id.0])?;
            let draft = found.pop().ok_or_else(|| StoreError::NotFound(id.0.clone()))?;
            if !matches!(draft.state, DraftState::Uncertain | DraftState::Failed(_) | DraftState::Queued) {
                return Err(StoreError::Invalid("This message is already being sent.".into()));
            }
            tx.execute("DELETE FROM drafts WHERE id=?1", [&id.0])?;
            tx.execute(
                "DELETE FROM pending_operations WHERE state IN ('pending','uncertain','failed') AND kind_json LIKE ?1",
                [format!("%\"draft_id\":\"{}\"%", id.0)],
            )?;
            Ok(())
        })
    }

    pub fn set_draft_state(&self, id: &DraftId, state: &DraftState) -> Result<()> {
        self.with_conn(|c| {
            c.execute(
                "UPDATE drafts SET state_json=?2, updated_at=?3 WHERE id=?1",
                params![id.0, serde_json::to_string(state)?, now()],
            )?;
            Ok(())
        })
    }

    /// Records provider acceptance: stores the sent message, removes the draft, and marks
    /// the send operation applied, all atomically.
    pub fn complete_send(&self, draft_id: &DraftId, op_id: &OperationId, sent: &Message) -> Result<()> {
        self.with_tx(|tx| {
            upsert_message_tx(tx, sent)?;
            tx.execute("DELETE FROM drafts WHERE id=?1", [&draft_id.0])?;
            tx.execute(
                "UPDATE pending_operations SET state='applied', last_error=NULL, updated_at=?2 WHERE id=?1",
                params![op_id.0, now()],
            )?;
            Ok(())
        })
    }

    // --------------------------------------------------------------- sync state

    pub fn account_status(&self, account: &AccountId) -> Result<AccountStatus> {
        let (state, last) = self.with_read(|c| {
            Ok(c.query_row(
                "SELECT state_json, last_successful_sync FROM sync_state WHERE account_id=?1",
                [&account.0],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<i64>>(1)?)),
            )
            .optional()?)
        })?
        .map(|(s, l)| (serde_json::from_str::<SyncState>(&s).unwrap_or(SyncState::Offline), l))
        .unwrap_or((SyncState::Offline, None));
        Ok(AccountStatus {
            state,
            last_successful_sync: last,
            pending_operations: self.pending_count(account)?,
            outbox_items: self.drafts(account, true)?.len(),
        })
    }

    pub fn set_sync_state(&self, account: &AccountId, state: &SyncState, successful_at: Option<Timestamp>) -> Result<()> {
        self.with_conn(|c| {
            c.execute(
                "INSERT INTO sync_state(account_id, state_json, last_successful_sync) VALUES(?1,?2,?3)
                 ON CONFLICT(account_id) DO UPDATE SET state_json=excluded.state_json,
                   last_successful_sync=COALESCE(excluded.last_successful_sync, sync_state.last_successful_sync)",
                params![account.0, serde_json::to_string(state)?, successful_at],
            )?;
            Ok(())
        })
    }

    /// Provider sync checkpoint (for Gmail, the history id). Saved transactionally with
    /// the downloaded changes by callers that use `with_tx` through [`Store::upsert_messages`].
    pub fn checkpoint(&self, account: &AccountId) -> Result<Option<String>> {
        self.with_read(|c| {
            Ok(c.query_row("SELECT checkpoint FROM sync_state WHERE account_id=?1", [&account.0], |r| r.get::<_, Option<String>>(0))
                .optional()?
                .flatten())
        })
    }

    pub fn set_checkpoint(&self, account: &AccountId, checkpoint: Option<&str>) -> Result<()> {
        self.with_conn(|c| {
            c.execute(
                "INSERT INTO sync_state(account_id, state_json, checkpoint) VALUES(?1, '{\"state\":\"offline\"}', ?2)
                 ON CONFLICT(account_id) DO UPDATE SET checkpoint=excluded.checkpoint",
                params![account.0, checkpoint],
            )?;
            Ok(())
        })
    }

    // ---------------------------------------------------------- server settings

    /// IMAP/POP3/SMTP server settings for an account. `None` for Gmail and Sample accounts,
    /// or before setup finished. Passwords are in the keyring, never here.
    pub fn servers(&self, account: &AccountId) -> Result<Option<MailServers>> {
        self.with_read(|c| {
            let raw: Option<String> = c.query_row("SELECT servers_json FROM account_servers WHERE account_id=?1", [&account.0], |r| r.get(0)).optional()?;
            Ok(match raw {
                Some(json) => Some(serde_json::from_str(&json)?),
                None => None,
            })
        })
    }

    pub fn set_servers(&self, account: &AccountId, servers: &MailServers) -> Result<()> {
        self.with_conn(|c| {
            c.execute(
                "INSERT INTO account_servers(account_id, servers_json) VALUES(?1,?2) ON CONFLICT(account_id) DO UPDATE SET servers_json=excluded.servers_json",
                params![account.0, serde_json::to_string(servers)?],
            )?;
            Ok(())
        })
    }

    /// Whether this device holds the only copy of the account's mail (POP3). Eviction must
    /// never touch such accounts, and [`Store::merge_remote_messages`] keeps their local
    /// flags and labels, which the provider cannot know.
    pub fn is_local_primary(&self, account: &AccountId) -> Result<bool> {
        self.with_read(|c| {
            let provider: Option<String> = c.query_row("SELECT provider FROM accounts WHERE id=?1", [&account.0], |r| r.get(0)).optional()?;
            Ok(provider.as_deref().and_then(ProviderKind::from_key) == Some(ProviderKind::Pop3))
        })
    }

    // ------------------------------------------------- identity and threading

    /// All messages of the account with this Message-ID, newest first. Several can exist
    /// legitimately (a Sent copy and a received copy; a message mid-move seen in two folders).
    pub fn messages_by_rfc_id(&self, account: &AccountId, rfc_id: &str) -> Result<Vec<Message>> {
        self.with_read(|c| load_messages(c, "WHERE m.account_id=?1 AND m.rfc_message_id=?2", params![account.0, rfc_id]))
    }

    /// The newest message of the account with this Message-ID.
    pub fn message_by_rfc_id(&self, account: &AccountId, rfc_id: &str) -> Result<Option<Message>> {
        Ok(self.messages_by_rfc_id(account, rfc_id)?.into_iter().next())
    }

    /// The local id of the message the provider knows by `provider_id`, if any.
    pub fn message_id_for_provider_id(&self, account: &AccountId, provider_id: &ProviderId) -> Result<Option<MessageId>> {
        self.with_read(|c| {
            Ok(c.query_row("SELECT id FROM messages WHERE account_id=?1 AND provider_id=?2", params![account.0, provider_id.0], |r| r.get::<_, String>(0))
                .optional()?
                .map(MessageId))
        })
    }

    /// The conversation a Message-ID belongs to: the conversation of the message that
    /// carries it, or of any stored message that replies to or references it. This is the
    /// lookup `overlook_core::threading::assign_conversations` needs, and the second clause
    /// is what lets a parent downloaded after its reply join the reply's conversation.
    pub fn conversation_for_rfc_id(&self, account: &AccountId, rfc_id: &str) -> Result<Option<ConversationId>> {
        self.with_read(|c| {
            let direct: Option<String> = c
                .query_row(
                    "SELECT conversation_id FROM messages WHERE account_id=?1 AND (rfc_message_id=?2 OR in_reply_to=?2) ORDER BY date DESC LIMIT 1",
                    params![account.0, rfc_id],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(conv) = direct {
                return Ok(Some(ConversationId(conv)));
            }
            let quoted = serde_json::to_string(rfc_id)?;
            let pattern = format!("%{}%", quoted.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"));
            let via_references: Option<String> = c
                .query_row(
                    "SELECT conversation_id FROM messages WHERE account_id=?1 AND references_json LIKE ?2 ESCAPE '\\' ORDER BY date DESC LIMIT 1",
                    params![account.0, pattern],
                    |r| r.get(0),
                )
                .optional()?;
            Ok(via_references.map(ConversationId))
        })
    }

    /// Provider ids of every message carrying `label`. For IMAP this is the local view of a
    /// folder, diffed against the server's UID list to find deletions.
    pub fn provider_ids_for_label(&self, label: &LabelId) -> Result<Vec<ProviderId>> {
        self.with_read(|c| {
            let mut stmt = c.prepare("SELECT m.provider_id FROM messages m JOIN message_labels ml ON ml.message_id = m.id WHERE ml.label_id=?1 AND m.provider_id IS NOT NULL ORDER BY m.date DESC")?;
            let rows = stmt.query_map([&label.0], |r| Ok(ProviderId(r.get(0)?)))?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
    }

    /// Records provider-side identity changes reported after an operation was applied
    /// (an IMAP move gives the message a new UID in a new folder). Each message gets the new
    /// provider id and exactly that label, since a message lives in one folder.
    pub fn apply_provider_id_changes(&self, changes: &[(MessageId, ProviderId, LabelId)]) -> Result<()> {
        if changes.is_empty() {
            return Ok(());
        }
        self.with_tx(|tx| {
            for (message, provider_id, label) in changes {
                let n = tx.execute("UPDATE messages SET provider_id=?2 WHERE id=?1", params![message.0, provider_id.0])?;
                if n == 0 {
                    continue;
                }
                tx.execute("DELETE FROM message_labels WHERE message_id=?1", [&message.0])?;
                tx.execute("INSERT OR IGNORE INTO message_labels(message_id, label_id) VALUES(?1,?2)", params![message.0, label.0])?;
            }
            Ok(())
        })
    }

    // ------------------------------------------------------------ IMAP folders

    /// The account's IMAP folder rows, mirrored from the provider's checkpoint after each
    /// cycle. Diagnostic and settings-dialog reading only: the checkpoint is authoritative.
    pub fn imap_folders(&self, account: &AccountId) -> Result<Vec<ImapFolder>> {
        self.with_read(|c| {
            let mut stmt = c.prepare("SELECT path, uidvalidity, highest_uid, highest_modseq, delimiter, special_use, subscribed FROM imap_folders WHERE account_id=?1 ORDER BY path")?;
            let rows = stmt.query_map([&account.0], |r| {
                Ok(ImapFolder {
                    path: r.get(0)?,
                    uidvalidity: r.get::<_, Option<i64>>(1)?.map(|v| v as u32),
                    highest_uid: r.get::<_, i64>(2)? as u32,
                    highest_modseq: r.get::<_, Option<i64>>(3)?.map(|v| v as u64),
                    delimiter: r.get(4)?,
                    special_use: r.get(5)?,
                    subscribed: r.get::<_, i64>(6)? != 0,
                })
            })?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
    }

    /// Replaces the account's IMAP folder rows.
    pub fn replace_imap_folders(&self, account: &AccountId, folders: &[ImapFolder]) -> Result<()> {
        self.with_tx(|tx| {
            tx.execute("DELETE FROM imap_folders WHERE account_id=?1", [&account.0])?;
            for f in folders {
                tx.execute(
                    "INSERT INTO imap_folders(account_id, path, uidvalidity, highest_uid, highest_modseq, delimiter, special_use, subscribed) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
                    params![account.0, f.path, f.uidvalidity.map(i64::from), i64::from(f.highest_uid), f.highest_modseq.map(|v| v as i64), f.delimiter, f.special_use, f.subscribed as i64],
                )?;
            }
            Ok(())
        })
    }

    /// The POP3 ledger rows mirrored from the provider's checkpoint (see [`Pop3Seen`]).
    pub fn pop3_seen(&self, account: &AccountId) -> Result<Vec<Pop3Seen>> {
        self.with_read(|c| {
            let mut stmt = c.prepare("SELECT uidl, seen_at, deleted_on_server FROM pop3_seen WHERE account_id=?1 ORDER BY seen_at, uidl")?;
            let rows = stmt.query_map([&account.0], |r| Ok(Pop3Seen { uidl: r.get(0)?, seen_at: r.get(1)?, deleted_on_server: r.get::<_, i64>(2)? != 0 }))?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
    }

    /// Replaces the account's POP3 ledger rows.
    pub fn replace_pop3_seen(&self, account: &AccountId, rows: &[Pop3Seen]) -> Result<()> {
        self.with_tx(|tx| {
            tx.execute("DELETE FROM pop3_seen WHERE account_id=?1", [&account.0])?;
            for r in rows {
                tx.execute("INSERT INTO pop3_seen(account_id, uidl, seen_at, deleted_on_server) VALUES(?1,?2,?3,?4)", params![account.0, r.uidl, r.seen_at, r.deleted_on_server as i64])?;
            }
            Ok(())
        })
    }

    /// Writes a consistent copy of the whole database to `path` (`VACUUM INTO`), for the
    /// "Back up mail" action. The copy is a plain SQLite file that `Store::open` reads.
    pub fn backup_to(&self, path: &Path) -> Result<()> {
        if path.exists() {
            std::fs::remove_file(path).map_err(|e| StoreError::Sqlite(rusqlite::Error::InvalidPath(std::path::PathBuf::from(format!("{}: {e}", path.display())))))?;
        }
        self.with_conn(|c| {
            c.execute("VACUUM INTO ?1", [path.to_string_lossy().as_ref()])?;
            Ok(())
        })
    }

    // ----------------------------------------------------------------- settings

    pub fn setting<T: serde::de::DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        self.with_read(|c| {
            let raw: Option<String> = c.query_row("SELECT value FROM settings WHERE key=?1", [key], |r| r.get(0)).optional()?;
            Ok(match raw {
                Some(s) => serde_json::from_str(&s).ok(),
                None => None,
            })
        })
    }

    pub fn set_setting<T: serde::Serialize>(&self, key: &str, value: &T) -> Result<()> {
        self.with_conn(|c| {
            c.execute(
                "INSERT INTO settings(key, value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                params![key, serde_json::to_string(value)?],
            )?;
            Ok(())
        })
    }

    // ------------------------------------------------------------------- search

    /// Full-text search over downloaded mail. Returns matching message ids, best first.
    pub fn search(&self, account: &AccountId, query: &str) -> Result<Vec<MessageId>> {
        let fts = fts_query(query);
        if fts.is_empty() {
            return Ok(Vec::new());
        }
        self.with_read(|c| {
            let mut stmt = c.prepare(
                "SELECT m.id FROM messages_fts f JOIN messages m ON m.rowid = f.rowid
                 WHERE messages_fts MATCH ?1 AND m.account_id = ?2 ORDER BY rank LIMIT 500",
            )?;
            let rows = stmt.query_map(params![fts, account.0], |r| Ok(MessageId(r.get(0)?)))?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
    }

    pub fn search_coverage(&self, account: &AccountId) -> Result<SearchCoverage> {
        self.with_read(|c| {
            let total: i64 = c.query_row("SELECT COUNT(*) FROM messages WHERE account_id=?1", [&account.0], |r| r.get(0))?;
            let cached: i64 = c.query_row(
                "SELECT COUNT(*) FROM messages WHERE account_id=?1 AND (body_text IS NOT NULL OR body_html IS NOT NULL)",
                [&account.0],
                |r| r.get(0),
            )?;
            Ok(SearchCoverage { messages_indexed: total as usize, messages_total: total as usize, bodies_cached: cached as usize })
        })
    }

    /// Replaces the account's contacts with the provider's list. Contacts changed on this device
    /// and not yet pushed are kept.
    pub fn replace_contacts(&self, account: &AccountId, contacts: &[Contact]) -> Result<()> {
        self.with_tx(|tx| {
            tx.execute("DELETE FROM people WHERE account_id=?1 AND pending=0", [&account.0])?;
            for c in contacts {
                if tx.query_row("SELECT COUNT(*) FROM people WHERE account_id=?1 AND id=?2", params![account.0, c.id], |r| r.get::<_, i64>(0))? > 0 {
                    continue; // a pending local edit wins until it is pushed
                }
                upsert_contact_tx(tx, c)?;
            }
            Ok(())
        })
    }

    pub fn contact_count(&self, account: &AccountId) -> Result<usize> {
        self.with_read(|c| Ok(c.query_row("SELECT COUNT(*) FROM people WHERE account_id=?1", [&account.0], |r| r.get::<_, i64>(0))? as usize))
    }

    pub fn contacts(&self, account: &AccountId) -> Result<Vec<Contact>> {
        let mut all = self.contacts_where(account, "", &[])?;
        all.sort_by_key(|c| c.display_name().to_lowercase());
        Ok(all)
    }

    pub fn contact_by_id(&self, account: &AccountId, id: &str) -> Result<Option<Contact>> {
        Ok(self.contacts_where(account, "AND id=?2", &[id])?.into_iter().next())
    }

    /// The contact with this address, address-book entries first.
    pub fn contact(&self, account: &AccountId, email: &str) -> Result<Option<Contact>> {
        let needle = email.trim().to_ascii_lowercase();
        let mut hits: Vec<Contact> = self.contacts_where(account, "", &[])?.into_iter().filter(|c| c.has_email(&needle)).collect();
        hits.sort_by(|a, b| b.in_address_book.cmp(&a.in_address_book));
        Ok(hits.into_iter().next())
    }

    /// Contacts whose name or address starts with `prefix` (any word of the name), address
    /// book entries first.
    pub fn contacts_matching(&self, account: &AccountId, prefix: &str, limit: usize) -> Result<Vec<Contact>> {
        Ok(contacts_matching(&self.contacts_where(account, "", &[])?, prefix, limit))
    }

    /// Saves a contact changed on this device and queues the push.
    pub fn save_contact_locally(&self, account: &AccountId, contact: &Contact) -> Result<PendingOperation> {
        ensure_same_account(&contact.account_id, account, "contact")?;
        let ts = now();
        let mut c = contact.clone();
        c.pending = true;
        c.updated = ts;
        let kind = if c.id.starts_with("local:") { OperationKind::CreateContact { contact_id: c.id.clone() } } else { OperationKind::UpdateContact { contact_id: c.id.clone() } };
        self.with_tx(|tx| {
            upsert_contact_tx(tx, &c)?;
            insert_operation_tx(tx, account, kind, ts)
        })
    }

    /// Removes a contact locally and queues its deletion.
    pub fn delete_contact_locally(&self, account: &AccountId, contact: &Contact) -> Result<PendingOperation> {
        let ts = now();
        self.with_tx(|tx| {
            tx.execute("DELETE FROM people WHERE account_id=?1 AND id=?2", params![account.0, contact.id])?;
            let resource_name = (!contact.id.starts_with("local:")).then(|| contact.id.clone());
            insert_operation_tx(tx, account, OperationKind::DeleteContact { contact_id: contact.id.clone(), resource_name }, ts)
        })
    }

    /// The provider accepted a local change: the row takes the provider's copy (and id).
    pub fn mark_contact_synced(&self, account: &AccountId, local_id: &str, synced: &Contact) -> Result<()> {
        let mut c = synced.clone();
        c.pending = false;
        self.with_tx(|tx| {
            if local_id != c.id {
                tx.execute("DELETE FROM people WHERE account_id=?1 AND id=?2", params![account.0, local_id])?;
            }
            upsert_contact_tx(tx, &c)
        })
    }

    fn contacts_where(&self, account: &AccountId, clause: &str, args: &[&str]) -> Result<Vec<Contact>> {
        self.with_read(|c| {
            let sql = format!(
                "SELECT id, account_id, email, emails_json, name, given_name, family_name, phone, organization, title, notes, photo_url, in_address_book, etag, updated, pending
                 FROM people WHERE account_id=?1 {clause}"
            );
            let mut stmt = c.prepare(&sql)?;
            let map = |r: &rusqlite::Row| {
                Ok(Contact {
                    id: r.get(0)?,
                    account_id: AccountId(r.get(1)?),
                    email: r.get(2)?,
                    emails: serde_json::from_str(&r.get::<_, String>(3)?).unwrap_or_default(),
                    name: r.get(4)?,
                    given_name: r.get(5)?,
                    family_name: r.get(6)?,
                    phone: r.get(7)?,
                    organization: r.get(8)?,
                    title: r.get(9)?,
                    notes: r.get(10)?,
                    photo_url: r.get(11)?,
                    in_address_book: r.get::<_, i64>(12)? != 0,
                    etag: r.get(13)?,
                    updated: r.get(14)?,
                    pending: r.get::<_, i64>(15)? != 0,
                })
            };
            let rows: Vec<Contact> = match args {
                [] => stmt.query_map([&account.0], map)?.collect::<std::result::Result<Vec<_>, _>>()?,
                [a] => stmt.query_map(params![account.0, a], map)?.collect::<std::result::Result<Vec<_>, _>>()?,
                _ => Vec::new(),
            };
            Ok(rows)
        })
    }

    /// Addresses the user has corresponded with, most frequent first, for recipient
    /// autocomplete.
    pub fn known_recipients(&self, account: &AccountId, prefix: &str, limit: usize) -> Result<Vec<Address>> {
        Ok(known_recipients(&self.message_headers_for_account(account)?, prefix, limit))
    }
}

/// Contacts whose address or name starts with `prefix`, address book first. The window
/// runs this on the contacts it already holds; the store on a fresh read.
pub fn contacts_matching(contacts: &[Contact], prefix: &str, limit: usize) -> Vec<Contact> {
    let needle = prefix.trim().to_ascii_lowercase();
    if needle.is_empty() {
        return Vec::new();
    }
    let mut hits: Vec<Contact> = contacts
        .iter()
        .filter(|c| {
            c.emails.iter().any(|e| e.to_ascii_lowercase().starts_with(&needle))
                || c.email.to_ascii_lowercase().starts_with(&needle)
                || c.display_name().to_lowercase().split_whitespace().any(|w| w.starts_with(&needle))
                || c.display_name().to_lowercase().starts_with(&needle)
        })
        .cloned()
        .collect();
    hits.sort_by(|a, b| b.in_address_book.cmp(&a.in_address_book).then_with(|| a.name.is_none().cmp(&b.name.is_none())).then_with(|| a.display_name().cmp(&b.display_name())));
    hits.truncate(limit);
    hits
}

/// Addresses seen in `msgs` matching `prefix`, most frequent first.
pub fn known_recipients<'a>(msgs: impl IntoIterator<Item = &'a Message>, prefix: &str, limit: usize) -> Vec<Address> {
    let needle = prefix.trim().to_ascii_lowercase();
    let mut counts: BTreeMap<String, (Address, usize)> = BTreeMap::new();
    for m in msgs {
        for a in std::iter::once(&m.from).chain(m.to.iter()).chain(m.cc.iter()) {
            let key = a.email.to_ascii_lowercase();
            let entry = counts.entry(key).or_insert_with(|| (a.clone(), 0));
            entry.1 += 1;
            if entry.0.name.is_none() && a.name.is_some() {
                entry.0 = a.clone();
            }
        }
    }
    let mut list: Vec<(Address, usize)> = counts
        .into_values()
        .filter(|(a, _)| {
            needle.is_empty()
                || a.email.to_ascii_lowercase().contains(&needle)
                || a.name.as_deref().is_some_and(|n| n.to_ascii_lowercase().contains(&needle))
        })
        .collect();
    list.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.email.cmp(&b.0.email)));
    list.into_iter().take(limit).map(|(a, _)| a).collect()
}

// ---------------------------------------------------------------------- helpers

fn fts_query(input: &str) -> String {
    input
        .split_whitespace()
        .map(|w| w.replace('"', ""))
        .filter(|w| !w.is_empty())
        .map(|w| format!("\"{w}\"*"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn row_to_label(r: &Row<'_>) -> rusqlite::Result<Label> {
    Ok(Label {
        id: LabelId(r.get(0)?),
        account_id: AccountId(r.get(1)?),
        kind: LabelKind::from_key(&r.get::<_, String>(2)?).unwrap_or(LabelKind::User),
        name: r.get(3)?,
        provider_id: r.get::<_, Option<String>>(4)?.map(ProviderId),
        color: r.get(5)?,
        sort_order: r.get(6)?,
    })
}

fn row_to_operation(r: &Row<'_>) -> rusqlite::Result<PendingOperation> {
    let kind_json: String = r.get(3)?;
    let kind: OperationKind = serde_json::from_str(&kind_json)
        .map_err(|e| rusqlite::Error::FromSqlConversionFailure(3, rusqlite::types::Type::Text, Box::new(e)))?;
    Ok(PendingOperation {
        id: OperationId(r.get(0)?),
        account_id: AccountId(r.get(1)?),
        seq: r.get(2)?,
        kind,
        state: OperationState::from_key(&r.get::<_, String>(4)?).unwrap_or(OperationState::Pending),
        attempts: r.get::<_, i64>(5)? as u32,
        last_error: r.get(6)?,
        created_at: r.get(7)?,
        updated_at: r.get(8)?,
    })
}

fn upsert_message_tx(tx: &Connection, m: &Message) -> Result<()> {
    tx.execute(
        "INSERT INTO messages(id, account_id, conversation_id, provider_id, from_json, to_json, cc_json, bcc_json, reply_to_json,
            subject, snippet, body_text, body_html, search_text, date, is_read, is_starred, is_outgoing, rfc_message_id, in_reply_to,
            category, is_important, focus_override, invitation_json, references_json)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24,?25)
         ON CONFLICT(id) DO UPDATE SET conversation_id=excluded.conversation_id, provider_id=excluded.provider_id,
            from_json=excluded.from_json, to_json=excluded.to_json, cc_json=excluded.cc_json, bcc_json=excluded.bcc_json,
            reply_to_json=excluded.reply_to_json, subject=excluded.subject, snippet=excluded.snippet,
            body_text=excluded.body_text, body_html=excluded.body_html, search_text=excluded.search_text, date=excluded.date,
            is_read=excluded.is_read, is_starred=excluded.is_starred, is_outgoing=excluded.is_outgoing,
            rfc_message_id=excluded.rfc_message_id, in_reply_to=excluded.in_reply_to,
            category=excluded.category, is_important=excluded.is_important, focus_override=excluded.focus_override, invitation_json=excluded.invitation_json,
            references_json=excluded.references_json",
        params![
            m.id.0,
            m.account_id.0,
            m.conversation_id.0,
            m.provider_id.as_ref().map(|p| &p.0),
            serde_json::to_string(&m.from)?,
            serde_json::to_string(&m.to)?,
            serde_json::to_string(&m.cc)?,
            serde_json::to_string(&m.bcc)?,
            m.reply_to.as_ref().map(serde_json::to_string).transpose()?,
            m.subject,
            m.snippet,
            m.body.text,
            m.body.html,
            search_text(m),
            m.date,
            m.is_read,
            m.is_starred,
            m.is_outgoing,
            m.rfc_message_id,
            m.in_reply_to,
            m.category,
            m.is_important,
            m.focus_override,
            m.invitation.as_ref().map(serde_json::to_string).transpose()?,
            if m.references.is_empty() { None } else { Some(serde_json::to_string(&m.references)?) },
        ],
    )?;
    tx.execute("DELETE FROM message_labels WHERE message_id=?1", [&m.id.0])?;
    for label in &m.label_ids {
        tx.execute("INSERT INTO message_labels(message_id, label_id) VALUES(?1,?2)", params![m.id.0, label.0])?;
    }
    tx.execute("DELETE FROM attachments WHERE message_id=?1", [&m.id.0])?;
    for a in &m.attachments {
        tx.execute(
            "INSERT INTO attachments(id, message_id, filename, mime_type, size_bytes, local_path, provider_id, content_id) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
            params![a.id.0, m.id.0, a.filename, a.mime_type, a.size_bytes as i64, a.local_path, a.provider_id.as_ref().map(|p| &p.0), a.content_id],
        )?;
    }
    Ok(())
}

fn upsert_contact_tx(tx: &Connection, c: &Contact) -> Result<()> {
    tx.execute(
        "INSERT INTO people(id, account_id, email, emails_json, name, given_name, family_name, phone, organization, title, notes, photo_url, in_address_book, etag, updated, pending)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)
         ON CONFLICT(account_id, id) DO UPDATE SET email=excluded.email, emails_json=excluded.emails_json, name=excluded.name, given_name=excluded.given_name,
            family_name=excluded.family_name, phone=excluded.phone, organization=excluded.organization, title=excluded.title, notes=excluded.notes,
            photo_url=excluded.photo_url, in_address_book=excluded.in_address_book, etag=excluded.etag, updated=excluded.updated, pending=excluded.pending",
        params![
            c.id,
            c.account_id.0,
            c.email.to_ascii_lowercase(),
            serde_json::to_string(&c.emails)?,
            c.name,
            c.given_name,
            c.family_name,
            c.phone,
            c.organization,
            c.title,
            c.notes,
            c.photo_url,
            c.in_address_book as i64,
            c.etag,
            c.updated,
            c.pending as i64,
        ],
    )?;
    Ok(())
}

fn upsert_event_tx(tx: &Connection, e: &Event) -> Result<()> {
    tx.execute(
        "INSERT INTO events(id, account_id, calendar_id, provider_id, ical_uid, summary, description, location, start, end, all_day, status,
            organizer_json, attendees_json, html_link, conference_url, recurring_event_id, updated, event_type, color, reminders_json, original_start, wants_conference)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23)
         ON CONFLICT(id) DO UPDATE SET calendar_id=excluded.calendar_id, provider_id=excluded.provider_id, ical_uid=excluded.ical_uid,
            summary=excluded.summary, description=excluded.description, location=excluded.location, start=excluded.start, end=excluded.end,
            all_day=excluded.all_day, status=excluded.status, organizer_json=excluded.organizer_json, attendees_json=excluded.attendees_json,
            html_link=excluded.html_link, conference_url=excluded.conference_url, recurring_event_id=excluded.recurring_event_id, updated=excluded.updated,
            event_type=excluded.event_type, color=excluded.color, reminders_json=excluded.reminders_json,
            original_start=excluded.original_start, wants_conference=excluded.wants_conference",
        params![
            e.id.0,
            e.account_id.0,
            e.calendar_id.0,
            e.provider_id.as_ref().map(|p| &p.0),
            e.ical_uid,
            e.summary,
            e.description,
            e.location,
            e.start,
            e.end,
            e.all_day,
            serde_json::to_string(&e.status)?.trim_matches('"').to_owned(),
            e.organizer.as_ref().map(serde_json::to_string).transpose()?,
            serde_json::to_string(&e.attendees)?,
            e.html_link,
            e.conference_url,
            e.recurring_event_id,
            e.updated,
            e.event_type,
            e.color,
            e.reminders.as_ref().map(serde_json::to_string).transpose()?,
            e.original_start,
            e.wants_conference,
        ],
    )?;
    Ok(())
}

fn load_events(c: &Connection, clause: &str, args: impl rusqlite::Params) -> Result<Vec<Event>> {
    let sql = format!(
        "SELECT e.id, e.account_id, e.calendar_id, e.provider_id, e.ical_uid, e.summary, e.description, e.location, e.start, e.end, e.all_day, e.status,
                e.organizer_json, e.attendees_json, e.html_link, e.conference_url, e.recurring_event_id, e.updated, e.event_type, e.color, e.reminders_json,
                e.original_start, e.wants_conference,
                EXISTS(SELECT 1 FROM caldav_event_resources m JOIN caldav_outbox o ON o.calendar_id=m.calendar_id AND o.href=m.href WHERE m.event_id=e.id)
                FROM events e {clause}"
    );
    let mut stmt = c.prepare(&sql)?;
    let rows = stmt.query_map(args, |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, Option<String>>(3)?,
            r.get::<_, Option<String>>(4)?,
            r.get::<_, String>(5)?,
            r.get::<_, Option<String>>(6)?,
            r.get::<_, Option<String>>(7)?,
            r.get::<_, i64>(8)?,
            r.get::<_, i64>(9)?,
            r.get::<_, bool>(10)?,
            r.get::<_, String>(11)?,
            r.get::<_, Option<String>>(12)?,
            r.get::<_, String>(13)?,
            r.get::<_, Option<String>>(14)?,
            r.get::<_, Option<String>>(15)?,
            r.get::<_, Option<String>>(16)?,
            r.get::<_, i64>(17)?,
            r.get::<_, Option<String>>(18)?,
            r.get::<_, Option<String>>(19)?,
            r.get::<_, Option<String>>(20)?,
            (r.get::<_, Option<i64>>(21)?, r.get::<_, i64>(22)? != 0, r.get::<_, bool>(23)?),
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (id, account, calendar, provider, ical, summary, description, location, start, end, all_day, status, organizer, attendees, link, conf, recurring, updated, event_type, color, reminders, (original_start, wants_conference, pending)) = row?;
        out.push(Event {
            id: EventId(id),
            account_id: AccountId(account),
            calendar_id: CalendarId(calendar),
            provider_id: provider.map(ProviderId),
            ical_uid: ical,
            summary,
            description,
            location,
            start,
            end,
            all_day,
            status: serde_json::from_str(&format!("\"{status}\"")).unwrap_or_default(),
            organizer: organizer.as_deref().map(serde_json::from_str).transpose()?,
            attendees: serde_json::from_str(&attendees)?,
            html_link: link,
            conference_url: conf,
            recurring_event_id: recurring,
            updated,
            event_type,
            color,
            reminders: reminders.as_deref().map(serde_json::from_str).transpose()?,
            original_start,
            wants_conference,
            pending,
        });
    }
    Ok(out)
}

fn search_text(m: &Message) -> String {
    let body = match (&m.body.text, &m.body.html) {
        (Some(t), _) => t.clone(),
        (None, Some(h)) => html_to_text(h),
        (None, None) => m.snippet.clone(),
    };
    let people: Vec<String> = std::iter::once(&m.from)
        .chain(m.to.iter())
        .chain(m.cc.iter())
        .map(|a| format!("{} {}", a.display_name(), a.email))
        .collect();
    format!("{}\n{}", people.join(" "), body)
}

fn load_messages(c: &Connection, where_clause: &str, args: &[&dyn rusqlite::ToSql]) -> Result<Vec<Message>> {
    load_messages_with(c, where_clause, args, true)
}

/// Loads messages with labels and attachments; `bodies: false` leaves the bodies empty,
/// which is all a list or count needs and far less to read.
fn load_messages_with(c: &Connection, where_clause: &str, args: &[&dyn rusqlite::ToSql], bodies: bool) -> Result<Vec<Message>> {
    let body_columns = if bodies { "m.body_text, m.body_html" } else { "NULL, NULL" };
    let sql = format!(
        "SELECT m.id, m.account_id, m.conversation_id, m.provider_id, m.from_json, m.to_json, m.cc_json, m.bcc_json, m.reply_to_json,
                m.subject, m.snippet, {body_columns}, m.date, m.is_read, m.is_starred, m.is_outgoing, m.rfc_message_id, m.in_reply_to,
                m.category, m.is_important, m.focus_override, m.invitation_json, m.references_json
         FROM messages m {where_clause} ORDER BY m.date DESC"
    );
    let mut stmt = c.prepare(&sql)?;
    let mut messages: Vec<Message> = stmt
        .query_map(args, |r| {
            let parse = |i: usize| -> rusqlite::Result<String> { r.get(i) };
            Ok(RawMessage {
                id: parse(0)?,
                account_id: parse(1)?,
                conversation_id: parse(2)?,
                provider_id: r.get(3)?,
                from_json: parse(4)?,
                to_json: parse(5)?,
                cc_json: parse(6)?,
                bcc_json: parse(7)?,
                reply_to_json: r.get(8)?,
                subject: parse(9)?,
                snippet: parse(10)?,
                body_text: r.get(11)?,
                body_html: r.get(12)?,
                date: r.get(13)?,
                is_read: r.get(14)?,
                is_starred: r.get(15)?,
                is_outgoing: r.get(16)?,
                rfc_message_id: r.get(17)?,
                in_reply_to: r.get(18)?,
                references_json: r.get(23)?,
                category: r.get(19)?,
                is_important: r.get(20)?,
                focus_override: r.get(21)?,
                invitation_json: r.get(22)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?
        .into_iter()
        .map(RawMessage::into_message)
        .collect::<Result<Vec<_>>>()?;

    if messages.is_empty() {
        return Ok(messages);
    }
    // Labels and attachments in two passes to avoid N+1 queries.
    let mut by_id: BTreeMap<String, usize> = BTreeMap::new();
    for (i, m) in messages.iter().enumerate() {
        by_id.insert(m.id.0.clone(), i);
    }
    // Labels and attachments of the same messages only, not of the whole database.
    let mut stmt = c.prepare(&format!("SELECT ml.message_id, ml.label_id FROM message_labels ml JOIN messages m ON m.id = ml.message_id {where_clause}"))?;
    for row in stmt.query_map(args, |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
        let (mid, lid) = row?;
        if let Some(&i) = by_id.get(&mid) {
            messages[i].label_ids.insert(LabelId(lid));
        }
    }
    let mut stmt = c.prepare(&format!(
        "SELECT a.id, a.message_id, a.filename, a.mime_type, a.size_bytes, a.local_path, a.provider_id, a.content_id
         FROM attachments a JOIN messages m ON m.id = a.message_id {where_clause} ORDER BY a.rowid"
    ))?;
    for row in stmt.query_map(args, |r| {
        Ok(Attachment {
            id: AttachmentId(r.get(0)?),
            message_id: MessageId(r.get(1)?),
            filename: r.get(2)?,
            mime_type: r.get(3)?,
            size_bytes: r.get::<_, i64>(4)? as u64,
            local_path: r.get(5)?,
            provider_id: r.get::<_, Option<String>>(6)?.map(ProviderId),
            content_id: r.get(7)?,
        })
    })? {
        let a = row?;
        if let Some(&i) = by_id.get(&a.message_id.0) {
            messages[i].attachments.push(a);
        }
    }
    Ok(messages)
}

struct RawMessage {
    id: String,
    account_id: String,
    conversation_id: String,
    provider_id: Option<String>,
    from_json: String,
    to_json: String,
    cc_json: String,
    bcc_json: String,
    reply_to_json: Option<String>,
    subject: String,
    snippet: String,
    body_text: Option<String>,
    body_html: Option<String>,
    date: i64,
    is_read: bool,
    is_starred: bool,
    is_outgoing: bool,
    rfc_message_id: Option<String>,
    in_reply_to: Option<String>,
    references_json: Option<String>,
    category: Option<String>,
    is_important: bool,
    focus_override: Option<bool>,
    invitation_json: Option<String>,
}

impl RawMessage {
    fn into_message(self) -> Result<Message> {
        Ok(Message {
            id: MessageId(self.id),
            account_id: AccountId(self.account_id),
            conversation_id: ConversationId(self.conversation_id),
            provider_id: self.provider_id.map(ProviderId),
            from: serde_json::from_str(&self.from_json)?,
            to: serde_json::from_str(&self.to_json)?,
            cc: serde_json::from_str(&self.cc_json)?,
            bcc: serde_json::from_str(&self.bcc_json)?,
            reply_to: self.reply_to_json.as_deref().map(serde_json::from_str).transpose()?,
            subject: self.subject,
            snippet: self.snippet,
            body: Body { text: self.body_text, html: self.body_html },
            date: self.date,
            is_read: self.is_read,
            is_starred: self.is_starred,
            is_outgoing: self.is_outgoing,
            label_ids: BTreeSet::new(),
            attachments: Vec::new(),
            rfc_message_id: self.rfc_message_id,
            in_reply_to: self.in_reply_to,
            references: self.references_json.as_deref().and_then(|j| serde_json::from_str(j).ok()).unwrap_or_default(),
            category: self.category,
            is_important: self.is_important,
            focus_override: self.focus_override,
            invitation: self.invitation_json.as_deref().and_then(|j| serde_json::from_str(j).ok()),
        })
    }
}

fn apply_change_tx(tx: &Connection, account: &AccountId, change: &LocalChange) -> Result<()> {
    // Object-level authorization: every message and label the change names must be this
    // account's, before anything is written.
    ensure_owned(tx, Owned::Message, account, change.message_ids().iter().map(|i| i.0.as_str()))?;
    if let LocalChange::AddLabel { label_id, .. } | LocalChange::RemoveLabel { label_id, .. } = change {
        ensure_owned(tx, Owned::Label, account, [label_id.0.as_str()])?;
    }
    let label_id_of = |kind: LabelKind| -> Result<LabelId> {
        tx.query_row(
            "SELECT id FROM labels WHERE account_id=?1 AND kind=?2 LIMIT 1",
            params![account.0, kind.as_key()],
            |r| Ok(LabelId(r.get(0)?)),
        )
        .optional()?
        .ok_or_else(|| StoreError::Invalid(format!("account has no {} label", kind.as_key())))
    };
    let add_label = |ids: &[MessageId], label: &LabelId| -> Result<()> {
        for id in ids {
            tx.execute("INSERT OR IGNORE INTO message_labels(message_id, label_id) VALUES(?1,?2)", params![id.0, label.0])?;
        }
        Ok(())
    };
    let remove_label = |ids: &[MessageId], label: &LabelId| -> Result<()> {
        for id in ids {
            tx.execute("DELETE FROM message_labels WHERE message_id=?1 AND label_id=?2", params![id.0, label.0])?;
        }
        Ok(())
    };
    match change {
        LocalChange::SetRead { message_ids, read } => {
            for id in message_ids {
                tx.execute("UPDATE messages SET is_read=?2 WHERE id=?1", params![id.0, read])?;
            }
        }
        LocalChange::SetStarred { message_ids, starred } => {
            for id in message_ids {
                tx.execute("UPDATE messages SET is_starred=?2 WHERE id=?1", params![id.0, starred])?;
            }
        }
        LocalChange::AddLabel { message_ids, label_id } => add_label(message_ids, label_id)?,
        LocalChange::RemoveLabel { message_ids, label_id } => remove_label(message_ids, label_id)?,
        LocalChange::Archive { message_ids } => remove_label(message_ids, &label_id_of(LabelKind::Inbox)?)?,
        LocalChange::Trash { message_ids } => {
            let trash = label_id_of(LabelKind::Trash)?;
            let inbox = label_id_of(LabelKind::Inbox)?;
            remove_label(message_ids, &inbox)?;
            add_label(message_ids, &trash)?;
        }
        LocalChange::Untrash { message_ids } => {
            let trash = label_id_of(LabelKind::Trash)?;
            let inbox = label_id_of(LabelKind::Inbox)?;
            remove_label(message_ids, &trash)?;
            add_label(message_ids, &inbox)?;
        }
    }
    Ok(())
}

fn insert_operation_tx(tx: &Connection, account: &AccountId, kind: OperationKind, ts: Timestamp) -> Result<PendingOperation> {
    let seq: i64 = tx.query_row(
        "SELECT COALESCE(MAX(seq), 0) + 1 FROM pending_operations WHERE account_id=?1",
        [&account.0],
        |r| r.get(0),
    )?;
    let op = PendingOperation {
        id: OperationId::generate(),
        account_id: account.clone(),
        seq,
        kind,
        state: OperationState::Pending,
        attempts: 0,
        last_error: None,
        created_at: ts,
        updated_at: ts,
    };
    tx.execute(
        "INSERT INTO pending_operations(id, account_id, seq, kind_json, state, attempts, last_error, created_at, updated_at)
         VALUES(?1,?2,?3,?4,?5,0,NULL,?6,?6)",
        params![op.id.0, op.account_id.0, op.seq, serde_json::to_string(&op.kind)?, op.state.as_key(), ts],
    )?;
    Ok(op)
}

fn save_draft_tx(tx: &Connection, d: &Draft) -> Result<()> {
    // A draft is saved under its own account; an id already used by another account's
    // draft is refused rather than overwritten.
    ensure_owned(tx, Owned::Draft, &d.account_id, [d.id.0.as_str()])?;
    // Its conversation and the message it replies to must be the same account's.
    ensure_owned(tx, Owned::Message, &d.account_id, d.in_reply_to.iter().map(|m| m.0.as_str()))?;
    tx.execute(
        "INSERT INTO drafts(id, account_id, kind, conversation_id, in_reply_to, to_json, cc_json, bcc_json, subject, body_text, body_html,
            state_json, created_at, updated_at, provider_id, include_quote)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)
         ON CONFLICT(id) DO UPDATE SET kind=excluded.kind, conversation_id=excluded.conversation_id, in_reply_to=excluded.in_reply_to,
            to_json=excluded.to_json, cc_json=excluded.cc_json, bcc_json=excluded.bcc_json, subject=excluded.subject,
            body_text=excluded.body_text, body_html=excluded.body_html, state_json=excluded.state_json,
            updated_at=excluded.updated_at, provider_id=excluded.provider_id, include_quote=excluded.include_quote",
        params![
            d.id.0,
            d.account_id.0,
            d.kind.as_key(),
            d.conversation_id.as_ref().map(|c| &c.0),
            d.in_reply_to.as_ref().map(|m| &m.0),
            serde_json::to_string(&d.to)?,
            serde_json::to_string(&d.cc)?,
            serde_json::to_string(&d.bcc)?,
            d.subject,
            d.body_text,
            d.body_html,
            serde_json::to_string(&d.state)?,
            d.created_at,
            d.updated_at,
            d.provider_id.as_ref().map(|p| &p.0),
            d.include_quote as i64,
        ],
    )?;
    tx.execute("DELETE FROM draft_attachments WHERE draft_id=?1", [&d.id.0])?;
    for a in &d.attachments {
        tx.execute(
            "INSERT INTO draft_attachments(id, draft_id, filename, mime_type, size_bytes, local_path) VALUES(?1,?2,?3,?4,?5,?6)",
            params![a.id.0, d.id.0, a.filename, a.mime_type, a.size_bytes as i64, a.local_path],
        )?;
    }
    Ok(())
}

fn load_drafts(c: &Connection, where_clause: &str, args: impl rusqlite::Params) -> Result<Vec<Draft>> {
    let sql = format!(
        "SELECT d.id, d.account_id, d.kind, d.conversation_id, d.in_reply_to, d.to_json, d.cc_json, d.bcc_json, d.subject, d.body_text,
                d.body_html, d.state_json, d.created_at, d.updated_at, d.provider_id, d.include_quote FROM drafts d {where_clause}"
    );
    let mut stmt = c.prepare(&sql)?;
    let rows = stmt.query_map(args, |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, Option<String>>(3)?,
            r.get::<_, Option<String>>(4)?,
            r.get::<_, String>(5)?,
            r.get::<_, String>(6)?,
            r.get::<_, String>(7)?,
            r.get::<_, String>(8)?,
            r.get::<_, String>(9)?,
            r.get::<_, Option<String>>(10)?,
            r.get::<_, String>(11)?,
            r.get::<_, i64>(12)?,
            r.get::<_, i64>(13)?,
            r.get::<_, Option<String>>(14)?,
            r.get::<_, i64>(15)? != 0,
        ))
    })?;
    let mut drafts = Vec::new();
    for row in rows {
        let (id, account_id, kind, conv, reply, to, cc, bcc, subject, body_text, body_html, state, created, updated, provider, include_quote) = row?;
        drafts.push(Draft {
            id: DraftId(id),
            account_id: AccountId(account_id),
            kind: DraftKind::from_key(&kind).unwrap_or(DraftKind::New),
            conversation_id: conv.map(ConversationId),
            in_reply_to: reply.map(MessageId),
            to: serde_json::from_str(&to)?,
            cc: serde_json::from_str(&cc)?,
            bcc: serde_json::from_str(&bcc)?,
            subject,
            body_text,
            body_html,
            include_quote,
            attachments: Vec::new(),
            state: serde_json::from_str(&state)?,
            created_at: created,
            updated_at: updated,
            provider_id: provider.map(ProviderId),
        });
    }
    let mut stmt = c.prepare("SELECT id, draft_id, filename, mime_type, size_bytes, local_path FROM draft_attachments ORDER BY rowid")?;
    for row in stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            DraftAttachment {
                id: AttachmentId(r.get(0)?),
                filename: r.get(2)?,
                mime_type: r.get(3)?,
                size_bytes: r.get::<_, i64>(4)? as u64,
                local_path: r.get(5)?,
            },
        ))
    })? {
        let (_, draft_id, att) = row?;
        if let Some(d) = drafts.iter_mut().find(|d| d.id.0 == draft_id) {
            d.attachments.push(att);
        }
    }
    Ok(drafts)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account() -> Account {
        Account {
            id: AccountId::from("acct-1"),
            provider: ProviderKind::Sample,
            display_name: "Jason".into(),
            email: "jason@example.com".into(),
            created_at: 1,
        }
    }

    fn label(kind: LabelKind, name: &str) -> Label {
        Label {
            id: LabelId::from(format!("label-{}", kind.as_key()).as_str()),
            account_id: AccountId::from("acct-1"),
            kind,
            name: name.into(),
            provider_id: None,
            color: None,
            sort_order: 0,
        }
    }

    fn message(id: &str, subject: &str, body: &str, labels: &[&str]) -> Message {
        Message {
            id: MessageId::from(id),
            account_id: AccountId::from("acct-1"),
            conversation_id: ConversationId::from(format!("conv-{id}").as_str()),
            provider_id: None,
            from: Address::named("Maya Chen", "maya@northstar.example"),
            to: vec![Address::named("Jason", "jason@example.com")],
            cc: vec![],
            bcc: vec![],
            reply_to: None,
            subject: subject.into(),
            snippet: body.chars().take(40).collect(),
            body: Body { text: Some(body.into()), html: None },
            date: 1_000_000,
            is_read: false,
            is_starred: false,
            is_outgoing: false,
            label_ids: labels.iter().map(|l| LabelId::from(*l)).collect(),
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

    fn seeded(store: &Store) {
        store.upsert_account(&account()).unwrap();
        for (kind, name) in [
            (LabelKind::Inbox, "Inbox"),
            (LabelKind::Sent, "Sent"),
            (LabelKind::Trash, "Trash"),
            (LabelKind::User, "Projects"),
        ] {
            store.upsert_label(&label(kind, name)).unwrap();
        }
        store
            .upsert_messages(&[
                message("m1", "Launch creative — final review", "The updated launch materials are ready.", &["label-inbox", "label-user"]),
                message("m2", "Saturday plans?", "Coffee first, then a trail with a view?", &["label-inbox"]),
            ])
            .unwrap();
    }

    /// CASA 3.1.4: every create, read-for-update, update and delete by id is refused when
    /// the record belongs to another account, and nothing changes or is queued.
    #[test]
    fn records_of_another_account_cannot_be_changed() {
        let store = Store::open_in_memory().unwrap();
        seeded(&store);
        let owner = AccountId::from("acct-1");
        let other = AccountId::from("acct-2");
        store.upsert_account(&Account { id: other.clone(), provider: ProviderKind::Sample, display_name: "Other".into(), email: "other@example.com".into(), created_at: 2 }).unwrap();
        let not_owned = |r: Result<()>| assert!(matches!(r, Err(StoreError::NotOwned(_))), "{r:?}");
        let m1 = MessageId::from("m1");

        // Update and delete of messages and labels through local changes.
        for change in [
            LocalChange::SetRead { message_ids: vec![m1.clone()], read: true },
            LocalChange::SetStarred { message_ids: vec![m1.clone()], starred: true },
            LocalChange::Archive { message_ids: vec![m1.clone()] },
            LocalChange::Trash { message_ids: vec![m1.clone()] },
            LocalChange::AddLabel { message_ids: vec![], label_id: LabelId::from("label-user") },
        ] {
            not_owned(store.apply_change(&other, change).map(|_| ()));
        }
        not_owned(store.set_focus_override(&other, &[m1.clone()], Some(true)));
        let m1_now = store.message(&m1).unwrap().unwrap();
        assert!(!m1_now.is_read && !m1_now.is_starred && m1_now.focus_override.is_none());
        assert!(m1_now.label_ids.contains(&LabelId::from("label-inbox")));
        assert!(store.pending_operations(&other).unwrap().is_empty());

        // Drafts: overwrite, delete, and every Outbox action.
        let mut draft = Draft::new(owner.clone(), DraftKind::New, 0);
        draft.to = vec![Address::named("Maya", "maya@northstar.example")];
        draft.subject = "Owner's draft".into();
        store.queue_send(&draft).unwrap();
        let mut hijack = draft.clone();
        hijack.account_id = other.clone();
        hijack.subject = "Replaced".into();
        not_owned(store.save_draft(&hijack));
        not_owned(store.delete_draft(&other, &draft.id));
        not_owned(store.cancel_send(&other, &draft.id).map(|_| ()));
        not_owned(store.resend(&other, &draft.id).map(|_| ()));
        not_owned(store.discard_send(&other, &draft.id));
        let kept = store.draft(&draft.id).unwrap().unwrap();
        assert_eq!((kept.account_id, kept.subject.as_str(), kept.state), (owner.clone(), "Owner's draft", DraftState::Queued));
        // A reply may not point at another account's message.
        let mut reply = Draft::new(other.clone(), DraftKind::Reply, 0);
        reply.in_reply_to = Some(m1.clone());
        not_owned(store.save_draft(&reply));

        // Calendars and events.
        let cal = Calendar { id: CalendarId::from("acct-1:cal"), account_id: owner.clone(), provider_id: None, name: "Cal".into(), color: None, is_primary: true, can_edit: true, visible: true, sort_order: 0, default_reminders: vec![] };
        store.replace_calendars(&owner, &[cal.clone()]).unwrap();
        let event = Event {
            id: EventId::from("acct-1:cal:e1"), account_id: owner.clone(), calendar_id: cal.id.clone(), provider_id: None, ical_uid: None,
            summary: "Standup".into(), description: None, location: None, start: 0, end: 1_800_000, all_day: false,
            status: EventStatus::Confirmed, organizer: None, attendees: vec![], html_link: None, conference_url: None, recurring_event_id: None, updated: 0,
            event_type: None, color: None, reminders: None, original_start: None, wants_conference: false, pending: false,
        };
        store.upsert_events(&[event.clone()]).unwrap();
        not_owned(store.set_calendar_visible(&other, &cal.id, false));
        not_owned(store.delete_event_locally_scoped(&other, &event, RecurrenceScope::This).map(|_| ()));
        let mut edited = event.clone();
        edited.summary = "Renamed".into();
        not_owned(store.apply_series_change(&other, &edited, RecurrenceScope::This).map(|_| ()));
        not_owned(store.apply_event_change(&other, &edited, OperationKind::UpdateEvent { event_id: edited.id.clone(), scope: RecurrenceScope::This }).map(|_| ()));
        let mut foreign = edited.clone();
        foreign.account_id = other.clone();
        not_owned(store.apply_event_change(&other, &foreign, OperationKind::UpdateEvent { event_id: foreign.id.clone(), scope: RecurrenceScope::This }).map(|_| ()));
        let mut planted = edited.clone();
        planted.id = EventId::from("acct-2:cal:planted");
        planted.account_id = other.clone();
        not_owned(store.apply_event_change(&other, &planted, OperationKind::CreateEvent { event_id: planted.id.clone() }).map(|_| ()));

        // Contacts.
        let contact = Contact::new_local(owner.clone(), 0);
        not_owned(store.save_contact_locally(&other, &contact).map(|_| ()));
        assert!(store.contacts(&owner).unwrap().is_empty() && store.pending_operations(&other).unwrap().is_empty());
        assert_eq!(store.event(&event.id).unwrap().unwrap().summary, "Standup");
        assert!(store.calendars(&owner).unwrap()[0].visible);
        assert!(store.pending_operations(&other).unwrap().is_empty());

        // The owner can still do all of it.
        store.apply_change(&owner, LocalChange::SetRead { message_ids: vec![m1.clone()], read: true }).unwrap();
        store.set_calendar_visible(&owner, &cal.id, false).unwrap();
        store.discard_send(&owner, &draft.id).unwrap();
    }

    #[test]
    fn change_and_operation_survive_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("overlook.sqlite");
        {
            let store = Store::open(&path).unwrap();
            seeded(&store);
            let op = store
                .apply_change(&AccountId::from("acct-1"), LocalChange::SetStarred { message_ids: vec![MessageId::from("m1")], starred: true })
                .unwrap();
            assert_eq!(op.seq, 1);
            store
                .apply_change(&AccountId::from("acct-1"), LocalChange::Archive { message_ids: vec![MessageId::from("m2")] })
                .unwrap();
        }
        // "Crash" and reopen.
        let store = Store::open(&path).unwrap();
        let acct = AccountId::from("acct-1");
        let ops = store.pending_operations(&acct).unwrap();
        assert_eq!(ops.len(), 2);
        assert_eq!(ops[0].seq, 1);
        assert!(matches!(ops[0].kind, OperationKind::SetStarred { starred: true, .. }));
        assert!(matches!(ops[1].kind, OperationKind::Archive { .. }));
        assert!(store.message(&MessageId::from("m1")).unwrap().unwrap().is_starred);
        let inbox = store.conversations(&acct, "jason@example.com", &MailboxView::Kind(LabelKind::Inbox), ListFilter::All, None).unwrap();
        assert_eq!(inbox.len(), 1, "archived message left the inbox");
        assert!(inbox[0].pending, "pending indicator is derived from the queue");
        let archive = store.conversations(&acct, "jason@example.com", &MailboxView::Kind(LabelKind::Archive), ListFilter::All, None).unwrap();
        assert_eq!(archive.len(), 1);
        assert_eq!(store.account_status(&acct).unwrap().pending_operations, 2);
    }

    #[test]
    fn in_memory_index_matches_the_store_after_each_change() {
        let store = Store::open_in_memory().unwrap();
        seeded(&store);
        let acct = AccountId::from("acct-1");
        let owner = "jason@example.com";
        let m1 = || vec![MessageId::from("m1")];
        let m2 = || vec![MessageId::from("m2")];
        let changes = [
            LocalChange::SetRead { message_ids: m1(), read: true },
            LocalChange::SetStarred { message_ids: m2(), starred: true },
            LocalChange::Archive { message_ids: m1() },
            LocalChange::AddLabel { message_ids: m2(), label_id: LabelId::from("label-user") },
            LocalChange::Trash { message_ids: m2() },
            LocalChange::Untrash { message_ids: m2() },
            LocalChange::RemoveLabel { message_ids: m1(), label_id: LabelId::from("label-user") },
        ];
        let views = [
            MailboxView::Kind(LabelKind::Inbox),
            MailboxView::Kind(LabelKind::Archive),
            MailboxView::Kind(LabelKind::Starred),
            MailboxView::Kind(LabelKind::Trash),
            MailboxView::Label(LabelId::from("label-user")),
        ];
        let mut index = store.mail_index(&acct).unwrap();
        for change in changes {
            index.apply(&change);
            store.apply_change(&acct, change.clone()).unwrap();
            let fresh = store.mail_index(&acct).unwrap();
            for view in &views {
                assert_eq!(
                    index.conversations(owner, view, ListFilter::All, None),
                    fresh.conversations(owner, view, ListFilter::All, None),
                    "{view:?} after {change:?}"
                );
                assert_eq!(index.view_count(owner, view, None), fresh.view_count(owner, view, None));
            }
        }
    }

    #[test]
    fn index_answers_recipients_and_thread_members_like_the_store() {
        let store = Store::open_in_memory().unwrap();
        seeded(&store);
        let acct = AccountId::from("acct-1");
        let index = store.mail_index(&acct).unwrap();
        for prefix in ["", "ma", "jason", "nobody"] {
            assert_eq!(index.known_recipients(prefix, 5), store.known_recipients(&acct, prefix, 5).unwrap(), "{prefix}");
        }
        assert_eq!(index.message_ids_in(&ConversationId::from("conv-m1")), vec![MessageId::from("m1")]);
        assert!(index.message_ids_in(&ConversationId::from("missing")).is_empty());
    }

    #[test]
    fn a_late_autosave_does_not_unqueue_a_sent_draft() {
        let store = Store::open_in_memory().unwrap();
        seeded(&store);
        let acct = AccountId::from("acct-1");
        let mut draft = Draft::new(acct.clone(), DraftKind::New, 10);
        draft.to = vec![Address::named("Maya Chen", "maya@northstar.example")];
        draft.subject = "Agenda".into();
        draft.body_text = "Points.".into();
        assert!(store.save_editing_draft(&draft).unwrap(), "a new draft is saved");
        store.queue_send(&draft).unwrap();
        assert!(!store.save_editing_draft(&draft).unwrap(), "the queued message is left alone");
        assert_eq!(store.drafts(&acct, true).unwrap().len(), 1);
        assert_eq!(store.pending_count(&acct).unwrap(), 1);
    }

    #[test]
    fn a_late_background_save_never_replaces_newer_text() {
        let store = Store::open_in_memory().unwrap();
        seeded(&store);
        let acct = AccountId::from("acct-1");
        let mut older = Draft::new(acct.clone(), DraftKind::New, 10);
        older.body_text = "Hel".into();
        older.updated_at = 100;
        let mut newer = older.clone();
        newer.body_text = "Hello there".into();
        newer.updated_at = 200;
        store.save_draft(&newer).unwrap();
        assert!(!store.save_editing_draft(&older).unwrap());
        assert_eq!(store.draft(&older.id).unwrap().unwrap().body_text, "Hello there");
    }

    #[test]
    fn index_leaves_bodies_out() {
        let store = Store::open_in_memory().unwrap();
        seeded(&store);
        let index = store.mail_index(&AccountId::from("acct-1")).unwrap();
        let m = index.message(&MessageId::from("m1")).unwrap();
        assert!(m.body.text.is_none() && m.body.html.is_none());
        assert!(m.has_label(&LabelId::from("label-inbox")));
    }

    #[test]
    fn failed_change_leaves_no_operation() {
        let store = Store::open_in_memory().unwrap();
        seeded(&store);
        let acct = AccountId::from("acct-1");
        let err = store.apply_change(&acct, LocalChange::AddLabel { message_ids: vec![MessageId::from("m1")], label_id: LabelId::from("missing") });
        assert!(err.is_err(), "foreign key violation must fail the transaction");
        assert_eq!(store.pending_count(&acct).unwrap(), 0);
        assert!(!store.message(&MessageId::from("m1")).unwrap().unwrap().has_label(&LabelId::from("missing")));
    }

    #[test]
    fn draft_queue_and_cancel_are_atomic_and_durable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("overlook.sqlite");
        let acct = AccountId::from("acct-1");
        let draft_id;
        {
            let store = Store::open(&path).unwrap();
            seeded(&store);
            let mut draft = Draft::new(acct.clone(), DraftKind::New, 10);
            draft.to = vec![Address::named("Maya Chen", "maya@northstar.example")];
            draft.subject = "Agenda".into();
            draft.body_text = "A few things to cover.".into();
            store.save_draft(&draft).unwrap();
            draft_id = draft.id.clone();
            let op = store.queue_send(&draft).unwrap();
            assert!(matches!(op.kind, OperationKind::SendDraft { .. }));
        }
        let store = Store::open(&path).unwrap();
        let outbox = store.drafts(&acct, true).unwrap();
        assert_eq!(outbox.len(), 1);
        assert_eq!(outbox[0].state, DraftState::Queued);
        assert_eq!(store.drafts(&acct, false).unwrap().len(), 0);
        assert_eq!(store.account_status(&acct).unwrap().outbox_items, 1);

        let restored = store.cancel_send(&acct, &draft_id).unwrap();
        assert_eq!(restored.state, DraftState::Editing);
        assert_eq!(store.pending_count(&acct).unwrap(), 0);
        assert_eq!(store.drafts(&acct, false).unwrap().len(), 1);

        // Empty recipients are rejected before anything is written.
        let mut bad = restored.clone();
        bad.to.clear();
        assert!(store.queue_send(&bad).is_err());
        assert_eq!(store.pending_count(&acct).unwrap(), 0);
    }

    #[test]
    fn complete_send_stores_message_and_clears_draft() {
        let store = Store::open_in_memory().unwrap();
        seeded(&store);
        let acct = AccountId::from("acct-1");
        let mut draft = Draft::new(acct.clone(), DraftKind::New, 10);
        draft.to = vec![Address::named("Maya Chen", "maya@northstar.example")];
        draft.subject = "Agenda".into();
        store.save_draft(&draft).unwrap();
        let op = store.queue_send(&draft).unwrap();
        let mut sent = message("sent-1", "Agenda", "body", &["label-sent"]);
        sent.from = Address::named("Jason", "jason@example.com");
        sent.to = vec![Address::named("Maya Chen", "maya@northstar.example")];
        sent.is_outgoing = true;
        sent.is_read = true;
        store.complete_send(&draft.id, &op.id, &sent).unwrap();
        assert!(store.draft(&draft.id).unwrap().is_none());
        assert_eq!(store.pending_count(&acct).unwrap(), 0);
        let sent_view = store.conversations(&acct, "jason@example.com", &MailboxView::Kind(LabelKind::Sent), ListFilter::All, None).unwrap();
        assert_eq!(sent_view.len(), 1);
        assert_eq!(sent_view[0].participant_line(), "Maya Chen", "sent conversations name the recipients");
    }

    #[test]
    fn search_and_coverage() {
        let store = Store::open_in_memory().unwrap();
        seeded(&store);
        let acct = AccountId::from("acct-1");
        let hits = store.search(&acct, "launch mater").unwrap();
        assert_eq!(hits, vec![MessageId::from("m1")]);
        let hits = store.search(&acct, "maya").unwrap();
        assert_eq!(hits.len(), 2, "sender names are indexed");
        assert!(store.search(&acct, "   ").unwrap().is_empty());
        let mut headers_only = message("m3", "Receipt", "", &["label-inbox"]);
        headers_only.body = Body::default();
        store.upsert_messages(&[headers_only]).unwrap();
        let coverage = store.search_coverage(&acct).unwrap();
        assert_eq!(coverage.messages_total, 3);
        assert_eq!(coverage.bodies_cached, 2);
        assert!(coverage.sentence().contains("1 have headers only"));
    }

    #[test]
    fn settings_round_trip_and_recipients() {
        let store = Store::open_in_memory().unwrap();
        seeded(&store);
        for pref in AppearancePreference::ALL {
            store.set_setting("appearance", &pref).unwrap();
            assert_eq!(store.setting::<AppearancePreference>("appearance").unwrap(), Some(pref));
        }
        assert_eq!(store.setting::<AppearancePreference>("nope").unwrap(), None);
        let recipients = store.known_recipients(&AccountId::from("acct-1"), "ma", 5).unwrap();
        assert_eq!(recipients[0].email, "maya@northstar.example");
    }

    #[test]
    fn remote_merge_respects_pending_intent_and_keeps_bodies() {
        let store = Store::open_in_memory().unwrap();
        seeded(&store);
        let acct = AccountId::from("acct-1");
        // Local change pending: unread message starred by the user.
        store.apply_change(&acct, LocalChange::SetStarred { message_ids: vec![MessageId::from("m1")], starred: true }).unwrap();
        let protected = store.pending_message_ids(&acct).unwrap();
        // Remote says m1 is read, not starred, and only sends headers; m2 was deleted remotely.
        let mut remote = message("m1", "Launch creative — final review", "", &["label-inbox"]);
        remote.body = Body::default();
        remote.is_read = true;
        remote.is_starred = false;
        store.merge_remote_messages(&[remote], &protected).unwrap();
        let m1 = store.message(&MessageId::from("m1")).unwrap().unwrap();
        assert!(m1.is_starred, "pending local intent wins");
        assert!(!m1.is_read);
        assert!(m1.body.is_cached(), "existing body kept when remote sends headers only");
        assert_eq!(store.delete_messages(&[MessageId::from("m2")]).unwrap(), 1);
        assert!(store.message(&MessageId::from("m2")).unwrap().is_none());

        // Unprotected merge takes remote flags.
        let mut remote = message("m1", "Launch creative — final review", "new body", &["label-inbox"]);
        remote.is_read = true;
        remote.is_starred = false;
        store.merge_remote_messages(&[remote], &BTreeSet::new()).unwrap();
        let m1 = store.message(&MessageId::from("m1")).unwrap().unwrap();
        assert!(m1.is_read && !m1.is_starred);
        assert_eq!(m1.body.text.as_deref(), Some("new body"));

        store.replace_labels(&acct, &[label(LabelKind::Inbox, "Inbox")]).unwrap();
        assert_eq!(store.labels(&acct).unwrap().len(), 1);
        assert!(store.account_by_email(ProviderKind::Sample, "JASON@example.com").unwrap().is_some());
        store.delete_account(&acct).unwrap();
        assert!(store.accounts().unwrap().is_empty());
        assert_eq!(store.message_count(&acct).unwrap(), 0);
    }

    #[test]
    fn focused_inbox_splits_and_respects_rules() {
        let store = Store::open_in_memory().unwrap();
        seeded(&store);
        let acct = AccountId::from("acct-1");
        let mut promo = message("m3", "Big sale", "Everything must go", &["label-inbox"]);
        promo.from = Address::named("Shop", "deals@shop.example");
        promo.category = Some("promotions".into());
        store.upsert_messages(&[promo]).unwrap();
        let focused = store.conversations(&acct, "jason@example.com", &MailboxView::Kind(LabelKind::Inbox), ListFilter::All, Some(FocusTab::Focused)).unwrap();
        let other = store.conversations(&acct, "jason@example.com", &MailboxView::Kind(LabelKind::Inbox), ListFilter::All, Some(FocusTab::Other)).unwrap();
        assert_eq!(focused.len(), 2);
        assert_eq!(other.len(), 1);
        assert_eq!(other[0].subject, "Big sale");
        store.set_focus_rule(&acct, "DEALS@shop.example", Some(true)).unwrap();
        let other = store.conversations(&acct, "jason@example.com", &MailboxView::Kind(LabelKind::Inbox), ListFilter::All, Some(FocusTab::Other)).unwrap();
        assert!(other.is_empty(), "rule moves the sender to Focused");
        store.set_focus_rule(&acct, "deals@shop.example", None).unwrap();
        store.set_focus_override(&acct, &[MessageId::from("m1")], Some(false)).unwrap();
        let other = store.conversations(&acct, "jason@example.com", &MailboxView::Kind(LabelKind::Inbox), ListFilter::All, Some(FocusTab::Other)).unwrap();
        assert_eq!(other.len(), 2);
        assert_eq!(store.view_count(&acct, "jason@example.com", &MailboxView::Kind(LabelKind::Inbox), Some(FocusTab::Focused)).unwrap(), 1);
    }

    #[test]
    fn series_edits_and_deletes_follow_the_scope() {
        let store = Store::open_in_memory().unwrap();
        let acct = AccountId::from("acct");
        store.upsert_account(&Account { id: acct.clone(), provider: ProviderKind::Sample, display_name: "Me".into(), email: "me@example.com".into(), created_at: 0 }).unwrap();
        let cal = Calendar { id: CalendarId::from("acct:cal"), account_id: acct.clone(), provider_id: None, name: "Cal".into(), color: None, is_primary: true, can_edit: true, visible: true, sort_order: 0, default_reminders: vec![] };
        store.replace_calendars(&acct, &[cal.clone()]).unwrap();
        let day = 86_400_000;
        let instance = |n: i64| Event {
            id: EventId(format!("acct:cal:m_{n}")), account_id: acct.clone(), calendar_id: cal.id.clone(), provider_id: Some(ProviderId(format!("m_{n}"))), ical_uid: None,
            summary: "Standup".into(), description: None, location: None, start: n * day + 9 * 3_600_000, end: n * day + 9 * 3_600_000 + 1_800_000, all_day: false,
            status: EventStatus::Confirmed, organizer: None, attendees: vec![], html_link: None, conference_url: None, recurring_event_id: Some("m".into()), updated: 0,
            event_type: None, color: None, reminders: None, original_start: Some(n * day + 9 * 3_600_000), wants_conference: false, pending: false,
        };
        store.upsert_events(&[instance(1), instance(2), instance(3), instance(4)]).unwrap();
        // Rename and shift the third one by an hour, for it and everything after.
        let mut edited = instance(3);
        edited.summary = "Sync".into();
        edited.start += 3_600_000;
        edited.end += 3_600_000;
        let op = store.apply_series_change(&acct, &edited, RecurrenceScope::Following).unwrap();
        assert!(matches!(op.kind, OperationKind::UpdateEvent { scope: RecurrenceScope::Following, .. }));
        let all = store.events_between(&acct, 0, 10 * day).unwrap();
        let by = |n: i64| all.iter().find(|e| e.provider_id.as_ref().unwrap().0 == format!("m_{n}")).unwrap().clone();
        assert_eq!(by(2).summary, "Standup");
        assert_eq!(by(4).summary, "Sync");
        assert_eq!(by(4).start, instance(4).start + 3_600_000);
        // Delete all: every occurrence goes, one operation carries the series id.
        let op = store.delete_event_locally_scoped(&acct, &by(2), RecurrenceScope::All).unwrap();
        assert!(matches!(op.kind, OperationKind::DeleteEvent { scope: RecurrenceScope::All, recurring_event_id: Some(_), .. }));
        assert!(store.events_between(&acct, 0, 10 * day).unwrap().is_empty());
    }

    #[test]
    fn calendar_round_trip_and_pending_flags() {
        let store = Store::open_in_memory().unwrap();
        seeded(&store);
        let acct = AccountId::from("acct-1");
        let cal = Calendar {
            id: CalendarId::from("acct-1:cal:primary"),
            account_id: acct.clone(),
            provider_id: Some(ProviderId::from("primary")),
            name: "Jason".into(),
            color: Some("#9fe1e7".into()),
            is_primary: true,
            can_edit: true,
            visible: true,
            sort_order: 0,
            default_reminders: vec![10],
        };
        store.replace_calendars(&acct, &[cal.clone()]).unwrap();
        store.set_calendar_visible(&acct, &cal.id, false).unwrap();
        let mut hidden = cal.clone();
        hidden.name = "Jason Alexander".into();
        store.replace_calendars(&acct, &[hidden]).unwrap();
        assert!(!store.calendars(&acct).unwrap()[0].visible, "visibility survives provider refresh");
        store.set_calendar_visible(&acct, &cal.id, true).unwrap();
        let event = Event {
            id: EventId::from("acct-1:cal:primary:e1"),
            account_id: acct.clone(),
            calendar_id: cal.id.clone(),
            provider_id: Some(ProviderId::from("e1")),
            ical_uid: Some("e1@google.com".into()),
            summary: "Design review".into(),
            description: None,
            location: Some("Room 4".into()),
            start: 1_000_000,
            end: 1_000_000 + 3_600_000,
            all_day: false,
            status: EventStatus::Confirmed,
            organizer: Some(Address::named("Maya Chen", "maya@northstar.example")),
            attendees: vec![Attendee { email: "jason@example.com".into(), name: None, response: ResponseStatus::NeedsAction, is_self: true, is_organizer: false, optional: false }],
            html_link: None,
            conference_url: None,
            recurring_event_id: None,
            updated: 0,
            event_type: None,
            color: None,
            reminders: None,
            original_start: None,
            wants_conference: false,
            pending: false,
        };
        store.upsert_events(&[event.clone()]).unwrap();
        assert_eq!(store.event_by_ical_uid(&acct, "e1@google.com").unwrap().unwrap().id, event.id);
        assert_eq!(store.events_starting_between(&acct, 0, 2_000_000).unwrap().len(), 1);
        assert_eq!(store.calendars(&acct).unwrap()[0].default_reminders, vec![10]);
        let found = store.events_between(&acct, 0, 2_000_000).unwrap();
        assert_eq!(found.len(), 1);
        assert!(found[0].is_invitation());
        assert!(store.events_between(&acct, 5_000_000, 6_000_000).unwrap().is_empty());
        let mut accepted = event.clone();
        accepted.attendees[0].response = ResponseStatus::Accepted;
        store.apply_event_change(&acct, &accepted, OperationKind::RespondToEvent { event_id: event.id.clone(), response: ResponseStatus::Accepted }).unwrap();
        let found = store.events_between(&acct, 0, 2_000_000).unwrap();
        assert_eq!(found[0].my_response(), Some(ResponseStatus::Accepted));
        assert!(found[0].pending);
        store.delete_event_locally(&acct, &event).unwrap();
        assert!(store.pending_event_ids(&acct).unwrap().contains(&event.id), "a locally deleted event is protected until replayed");
        assert!(store.event(&event.id).unwrap().is_none());
        assert_eq!(store.pending_count(&acct).unwrap(), 2);
    }

    #[test]
    fn trash_and_untrash_round_trip() {
        let store = Store::open_in_memory().unwrap();
        seeded(&store);
        let acct = AccountId::from("acct-1");
        store.apply_change(&acct, LocalChange::Trash { message_ids: vec![MessageId::from("m1")] }).unwrap();
        let trash = store.conversations(&acct, "jason@example.com", &MailboxView::Kind(LabelKind::Trash), ListFilter::All, None).unwrap();
        assert_eq!(trash.len(), 1);
        assert_eq!(store.view_count(&acct, "jason@example.com", &MailboxView::Kind(LabelKind::Inbox), None).unwrap(), 1);
        store.apply_change(&acct, LocalChange::Untrash { message_ids: vec![MessageId::from("m1")] }).unwrap();
        assert_eq!(store.view_count(&acct, "jason@example.com", &MailboxView::Kind(LabelKind::Inbox), None).unwrap(), 2);
    }

    #[test]
    fn server_settings_round_trip_and_cascade() {
        let store = Store::open_in_memory().unwrap();
        seeded(&store);
        let acct = AccountId::from("acct-1");
        assert!(store.servers(&acct).unwrap().is_none());
        let servers = MailServers {
            incoming: ServerEndpoint::new("imap.example.com", 993, Security::Tls, "jason@example.com"),
            outgoing: ServerEndpoint::new("smtp.example.com", 587, Security::StartTls, "jason@example.com"),
            pop3: None,
            imap: Some(ImapPolicy::default()),
        };
        store.set_servers(&acct, &servers).unwrap();
        assert_eq!(store.servers(&acct).unwrap(), Some(servers.clone()));
        let changed = MailServers { pop3: Some(Pop3Policy { delete_after_days: Some(30), ..Default::default() }), ..servers };
        store.set_servers(&acct, &changed).unwrap();
        assert_eq!(store.servers(&acct).unwrap().unwrap().pop3.unwrap().delete_after_days, Some(30));
        store.delete_account(&acct).unwrap();
        assert!(store.servers(&acct).unwrap().is_none(), "settings go with the account");
    }

    #[test]
    fn local_primary_is_pop3_only() {
        let store = Store::open_in_memory().unwrap();
        seeded(&store);
        assert!(!store.is_local_primary(&AccountId::from("acct-1")).unwrap());
        let pop = Account { id: AccountId::from("pop3:j"), provider: ProviderKind::Pop3, ..account() };
        store.upsert_account(&pop).unwrap();
        assert!(store.is_local_primary(&pop.id).unwrap());
        assert!(!store.is_local_primary(&AccountId::from("nobody")).unwrap());
    }

    #[test]
    fn local_primary_accounts_keep_their_flags_when_a_message_is_served_again() {
        let store = Store::open_in_memory().unwrap();
        let pop = Account { id: AccountId::from("pop3:j"), provider: ProviderKind::Pop3, ..account() };
        store.upsert_account(&pop).unwrap();
        for (kind, name) in [(LabelKind::Inbox, "Inbox"), (LabelKind::Trash, "Trash")] {
            store.upsert_label(&Label { id: LabelId::from(format!("pop3:j:{}", kind.as_key()).as_str()), account_id: pop.id.clone(), kind, name: name.into(), provider_id: None, color: None, sort_order: 0 }).unwrap();
        }
        let mut m = message("pop3:j:m1", "Hello", "Body", &[]);
        m.account_id = pop.id.clone();
        m.label_ids = [LabelId::from("pop3:j:inbox")].into_iter().collect();
        store.merge_remote_messages(&[m.clone()], &BTreeSet::new()).unwrap();
        let mut read = store.message(&m.id).unwrap().unwrap();
        read.is_read = true;
        read.is_starred = true;
        read.label_ids = [LabelId::from("pop3:j:trash")].into_iter().collect();
        store.upsert_messages(&[read]).unwrap();
        store.merge_remote_messages(&[m.clone()], &BTreeSet::new()).unwrap();
        let again = store.message(&m.id).unwrap().unwrap();
        assert!(again.is_read && again.is_starred, "the provider's unread copy did not reset the local state");
        assert_eq!(again.label_ids.iter().map(|l| l.as_str()).collect::<Vec<_>>(), vec!["pop3:j:trash"]);

        // A cache account (Gmail-like) takes the provider's flags as before.
        store.upsert_account(&account()).unwrap();
        let m2 = message("acct-1:m2", "Hi", "Body", &[]);
        store.merge_remote_messages(&[m2.clone()], &BTreeSet::new()).unwrap();
        let mut read = store.message(&m2.id).unwrap().unwrap();
        read.is_read = true;
        store.upsert_messages(&[read]).unwrap();
        store.merge_remote_messages(&[m2.clone()], &BTreeSet::new()).unwrap();
        assert!(!store.message(&m2.id).unwrap().unwrap().is_read);
    }

    #[test]
    fn pop3_seen_rows_round_trip_and_backup_reopens() {
        let store = Store::open_in_memory().unwrap();
        let pop = Account { id: AccountId::from("pop3:j"), provider: ProviderKind::Pop3, ..account() };
        store.upsert_account(&pop).unwrap();
        let rows = vec![Pop3Seen { uidl: "b".into(), seen_at: 2, deleted_on_server: true }, Pop3Seen { uidl: "a".into(), seen_at: 1, deleted_on_server: false }];
        store.replace_pop3_seen(&pop.id, &rows).unwrap();
        let back = store.pop3_seen(&pop.id).unwrap();
        assert_eq!(back.iter().map(|r| (r.uidl.as_str(), r.seen_at, r.deleted_on_server)).collect::<Vec<_>>(), vec![("a", 1, false), ("b", 2, true)]);
        store.replace_pop3_seen(&pop.id, &rows[..1]).unwrap();
        assert_eq!(store.pop3_seen(&pop.id).unwrap().len(), 1);
        assert!(store.pop3_seen(&AccountId::from("nobody")).unwrap().is_empty());

        let dir = std::env::temp_dir().join(format!("overlook-store-backup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let copy = dir.join("overlook.sqlite");
        store.backup_to(&copy).unwrap();
        let reopened = Store::open(&copy).unwrap();
        assert_eq!(reopened.accounts().unwrap().len(), 1);
        assert_eq!(reopened.pop3_seen(&pop.id).unwrap().len(), 1);
        // A second backup over the same file replaces it.
        store.backup_to(&copy).unwrap();
        drop(reopened);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn v11_tables_accept_folder_and_pop3_rows() {
        let store = Store::open_in_memory().unwrap();
        seeded(&store);
        store
            .with_conn(|c| {
                c.execute("INSERT INTO imap_folders(account_id, path, uidvalidity, highest_uid, delimiter, special_use) VALUES('acct-1','INBOX',17,42,'/','\\Inbox')", [])?;
                c.execute("INSERT INTO pop3_seen(account_id, uidl, seen_at) VALUES('acct-1','UIDL-1',5)", [])?;
                Ok(())
            })
            .unwrap();
        store.delete_account(&AccountId::from("acct-1")).unwrap();
        let (folders, seen): (i64, i64) = store
            .with_conn(|c| Ok((c.query_row("SELECT COUNT(*) FROM imap_folders", [], |r| r.get(0))?, c.query_row("SELECT COUNT(*) FROM pop3_seen", [], |r| r.get(0))?)))
            .unwrap();
        assert_eq!((folders, seen), (0, 0), "cursor rows cascade with the account");
    }

    #[test]
    fn imap_folder_rows_round_trip() {
        let store = Store::open_in_memory().unwrap();
        seeded(&store);
        let acct = AccountId::from("acct-1");
        let rows = vec![
            ImapFolder { path: "INBOX".into(), uidvalidity: Some(17), highest_uid: 42, highest_modseq: None, delimiter: Some("/".into()), special_use: None, subscribed: true },
            ImapFolder { path: "Sent".into(), uidvalidity: Some(18), highest_uid: 3, highest_modseq: Some(900), delimiter: Some("/".into()), special_use: Some("\\Sent".into()), subscribed: false },
        ];
        store.replace_imap_folders(&acct, &rows).unwrap();
        assert_eq!(store.imap_folders(&acct).unwrap(), rows);
        store.replace_imap_folders(&acct, &rows[1..]).unwrap();
        assert_eq!(store.imap_folders(&acct).unwrap().len(), 1, "replaced, not appended");
        assert!(store.imap_folders(&AccountId::from("nobody")).unwrap().is_empty());
    }

    #[test]
    fn header_only_refresh_keeps_snippet_and_invitation() {
        let store = Store::open_in_memory().unwrap();
        seeded(&store);
        let mut full = message("h1", "Invite", "You are invited", &["label-inbox"]);
        full.invitation = overlook_core::ics::parse_invitation("BEGIN:VCALENDAR\r\nMETHOD:REQUEST\r\nBEGIN:VEVENT\r\nUID:ev1\r\nSUMMARY:Standup\r\nDTSTART:20261014T090000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n");
        assert!(full.invitation.is_some());
        store.merge_remote_messages(&[full.clone()], &BTreeSet::new()).unwrap();
        let mut headers_only = full.clone();
        headers_only.body = Body::default();
        headers_only.snippet = String::new();
        headers_only.invitation = None;
        headers_only.is_read = true;
        store.merge_remote_messages(&[headers_only], &BTreeSet::new()).unwrap();
        let stored = store.message(&full.id).unwrap().unwrap();
        assert!(stored.is_read, "flags come from the refresh");
        assert_eq!(stored.body, full.body);
        assert_eq!(stored.snippet, full.snippet);
        assert_eq!(stored.invitation, full.invitation);
    }

    #[test]
    fn references_persist_and_message_ids_resolve() {
        let store = Store::open_in_memory().unwrap();
        seeded(&store);
        let acct = AccountId::from("acct-1");
        let mut root = message("r1", "Plan", "root", &["label-inbox"]);
        root.rfc_message_id = Some("<root@x>".into());
        root.provider_id = Some(ProviderId("INBOX\u{1f}7\u{1f}100".into()));
        root.conversation_id = ConversationId::from("conv-root");
        let mut reply = message("r2", "Re: Plan", "reply", &["label-inbox"]);
        reply.rfc_message_id = Some("<reply@x>".into());
        reply.in_reply_to = Some("<root@x>".into());
        reply.references = vec!["<grandparent@x>".into(), "<root@x>".into()];
        reply.conversation_id = ConversationId::from("conv-root");
        reply.date = 2_000_000;
        let mut sent_copy = message("r3", "Plan", "root", &["label-sent"]);
        sent_copy.rfc_message_id = Some("<root@x>".into());
        sent_copy.provider_id = Some(ProviderId("Sent\u{1f}3\u{1f}9".into()));
        sent_copy.conversation_id = ConversationId::from("conv-root");
        sent_copy.date = 999;
        store.upsert_messages(&[root, reply, sent_copy]).unwrap();

        assert_eq!(store.message(&MessageId::from("r2")).unwrap().unwrap().references, vec!["<grandparent@x>".to_owned(), "<root@x>".to_owned()]);
        assert!(store.message(&MessageId::from("r1")).unwrap().unwrap().references.is_empty());

        let both = store.messages_by_rfc_id(&acct, "<root@x>").unwrap();
        assert_eq!(both.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), vec!["r1", "r3"], "newest first");
        assert_eq!(store.message_by_rfc_id(&acct, "<root@x>").unwrap().unwrap().id.as_str(), "r1");
        assert!(store.message_by_rfc_id(&acct, "<missing@x>").unwrap().is_none());
        assert_eq!(store.message_id_for_provider_id(&acct, &ProviderId("Sent\u{1f}3\u{1f}9".into())).unwrap(), Some(MessageId::from("r3")));
        assert!(store.message_id_for_provider_id(&acct, &ProviderId("nope".into())).unwrap().is_none());

        // Threading lookup: by own id, by being replied to, and by appearing in References.
        assert_eq!(store.conversation_for_rfc_id(&acct, "<reply@x>").unwrap(), Some(ConversationId::from("conv-root")));
        assert_eq!(store.conversation_for_rfc_id(&acct, "<grandparent@x>").unwrap(), Some(ConversationId::from("conv-root")));
        assert_eq!(store.conversation_for_rfc_id(&acct, "<grand%@x>").unwrap(), None, "LIKE wildcards are escaped");
        assert_eq!(store.conversation_for_rfc_id(&acct, "<nobody@x>").unwrap(), None);

        // Folder contents by provider id.
        let inbox = store.provider_ids_for_label(&LabelId::from("label-inbox")).unwrap();
        assert_eq!(inbox, vec![ProviderId("INBOX\u{1f}7\u{1f}100".into())], "r2 has no provider id and is skipped");
        assert_eq!(store.provider_ids_for_label(&LabelId::from("label-sent")).unwrap().len(), 1);
    }

    #[test]
    fn provider_id_changes_move_the_message() {
        let store = Store::open_in_memory().unwrap();
        seeded(&store);
        let acct = AccountId::from("acct-1");
        let mut m = message("mv", "Moving", "body", &["label-inbox"]);
        m.provider_id = Some(ProviderId("INBOX\u{1f}7\u{1f}5".into()));
        store.upsert_messages(&[m]).unwrap();
        store
            .apply_provider_id_changes(&[
                (MessageId::from("mv"), ProviderId("Projects\u{1f}2\u{1f}31".into()), LabelId::from("label-user")),
                (MessageId::from("ghost"), ProviderId("x".into()), LabelId::from("label-user")),
            ])
            .unwrap();
        let moved = store.message(&MessageId::from("mv")).unwrap().unwrap();
        assert_eq!(moved.provider_id.as_ref().unwrap().as_str(), "Projects\u{1f}2\u{1f}31");
        assert_eq!(moved.label_ids.iter().map(|l| l.as_str()).collect::<Vec<_>>(), vec!["label-user"]);
        assert!(store.message_id_for_provider_id(&acct, &ProviderId("INBOX\u{1f}7\u{1f}5".into())).unwrap().is_none());
    }

    #[test]
    fn merge_keeps_local_conversation_for_header_only_refresh() {
        let store = Store::open_in_memory().unwrap();
        seeded(&store);
        let mut m = message("t1", "Threaded", "body", &["label-inbox"]);
        m.conversation_id = ConversationId::from("conv-threaded");
        store.upsert_messages(&[m.clone()]).unwrap();
        let mut refresh = m.clone();
        refresh.conversation_id = ConversationId(String::new());
        refresh.body = Body::default();
        refresh.is_read = true;
        store.merge_remote_messages(&[refresh], &BTreeSet::new()).unwrap();
        let stored = store.message(&MessageId::from("t1")).unwrap().unwrap();
        assert_eq!(stored.conversation_id.as_str(), "conv-threaded");
        assert!(stored.is_read && stored.body.is_cached());
    }
}
