//! Schema migrations. `PRAGMA user_version` tracks the applied version.

use rusqlite::Connection;

const MIGRATIONS: &[&str] = &[
    V1, V2, V3, V4, V5, V6, V7, V8, V9, V10, V11, V12, V13, V14, V15, V16, V17, V18, V19, V20,
    V21, V22, V23, V24, V25, V26, V27,
];

/// Device-local email snoozes: one active snooze per (account_id, conversation_id) with
/// baseline message fingerprints so reconnects/resyncs never mistake existing or older
/// messages for a new inbound reply.
const V26: &str = r#"
CREATE TABLE conversation_snoozes (
    account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    conversation_id TEXT NOT NULL,
    wake_at INTEGER NOT NULL,
    snoozed_at INTEGER NOT NULL,
    baseline_latest_inbound INTEGER,
    baseline_message_ids_json TEXT NOT NULL DEFAULT '[]',
    baseline_rfc_ids_json TEXT NOT NULL DEFAULT '[]',
    PRIMARY KEY (account_id, conversation_id)
);
CREATE INDEX conversation_snoozes_wake ON conversation_snoozes(wake_at);
"#;

/// Opt-in per-message RFC 8098 read receipt requests on drafts, and parsed incoming
/// RFC 8098 MDN receipt metadata on messages. Tables of their own, so nothing that
/// exists is altered.
const V27: &str = r#"
CREATE TABLE draft_read_receipts (
    draft_id TEXT PRIMARY KEY REFERENCES drafts(id) ON DELETE CASCADE,
    request_read_receipt INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE message_read_receipts (
    message_id TEXT PRIMARY KEY REFERENCES messages(id) ON DELETE CASCADE,
    receipt_json TEXT NOT NULL,
    original_message_id TEXT
);
CREATE INDEX message_read_receipts_original
    ON message_read_receipts(original_message_id)
    WHERE original_message_id IS NOT NULL;
"#;

const V25: &str = r#"
ALTER TABLE mcp_operation_status ADD COLUMN source TEXT NOT NULL DEFAULT 'queue';

CREATE TRIGGER mcp_caldav_status_update AFTER UPDATE OF payload_json ON caldav_outbox
BEGIN
    UPDATE mcp_operation_status SET state = json_extract(new.payload_json, '$.state'),
        updated_at = CAST(unixepoch('subsec') * 1000 AS INTEGER)
    WHERE id = new.id AND source = 'caldav';
END;

CREATE TRIGGER mcp_caldav_status_delete AFTER DELETE ON caldav_outbox
BEGIN
    UPDATE mcp_operation_status SET state = 'cancelled',
        updated_at = CAST(unixepoch('subsec') * 1000 AS INTEGER)
    WHERE id = old.id AND source = 'caldav' AND state NOT IN ('applied', 'discarded');
END;
"#;

const V24: &str = r#"
CREATE TABLE mcp_write_receipts (
    account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    client TEXT NOT NULL,
    key_digest BLOB NOT NULL,
    request_digest BLOB NOT NULL,
    receipt_id TEXT NOT NULL,
    action TEXT NOT NULL,
    result_json TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (account_id, client, key_digest)
);
CREATE INDEX mcp_write_receipts_activity ON mcp_write_receipts(account_id, created_at, receipt_id);

CREATE TABLE mcp_operation_status (
    id TEXT PRIMARY KEY,
    account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    action TEXT NOT NULL,
    state TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE INDEX mcp_operation_status_account ON mcp_operation_status(account_id);

CREATE TRIGGER mcp_operation_status_update AFTER UPDATE OF state, updated_at ON pending_operations
BEGIN
    UPDATE mcp_operation_status SET state = new.state, updated_at = new.updated_at WHERE id = new.id;
END;
CREATE TRIGGER mcp_operation_status_delete AFTER DELETE ON pending_operations
WHEN old.state != 'applied'
BEGIN
    UPDATE mcp_operation_status SET state = CASE WHEN old.state IN ('uncertain','in-flight')
        THEN 'discarded' ELSE 'cancelled' END, updated_at = CAST(unixepoch('subsec') * 1000 AS INTEGER)
    WHERE id = old.id;
END;
"#;

/// Split message bodies out of `messages`, add `WHEN`-guarded FTS5 triggers, add
/// `message_references` and missing indexes, and add monotonic counters for contacts.
const V23: &str = r#"
CREATE TABLE message_bodies (
    message_id TEXT PRIMARY KEY REFERENCES messages(id) ON DELETE CASCADE,
    body_text TEXT,
    body_html TEXT,
    search_text TEXT NOT NULL
);
INSERT INTO message_bodies(message_id, body_text, body_html, search_text)
    SELECT id, body_text, body_html, search_text FROM messages;

DROP TRIGGER IF EXISTS messages_fts_insert;
DROP TRIGGER IF EXISTS messages_fts_delete;
DROP TRIGGER IF EXISTS messages_fts_update;
DROP TABLE IF EXISTS messages_fts;

ALTER TABLE messages DROP COLUMN body_text;
ALTER TABLE messages DROP COLUMN body_html;
ALTER TABLE messages DROP COLUMN search_text;

CREATE VIRTUAL TABLE messages_fts USING fts5(
    subject, search_text,
    content='', contentless_delete=1,
    tokenize='unicode61 remove_diacritics 2'
);
INSERT INTO messages_fts(rowid, subject, search_text)
    SELECT m.rowid, m.subject, COALESCE(b.search_text, '')
    FROM messages m LEFT JOIN message_bodies b ON b.message_id = m.id;

CREATE TRIGGER messages_fts_insert AFTER INSERT ON messages BEGIN
    INSERT INTO messages_fts(rowid, subject, search_text) VALUES (new.rowid, new.subject, '');
END;
CREATE TRIGGER messages_fts_delete AFTER DELETE ON messages BEGIN
    DELETE FROM messages_fts WHERE rowid = old.rowid;
END;
CREATE TRIGGER messages_fts_update AFTER UPDATE OF subject ON messages WHEN old.subject IS NOT new.subject BEGIN
    DELETE FROM messages_fts WHERE rowid = new.rowid;
    INSERT INTO messages_fts(rowid, subject, search_text)
        VALUES (new.rowid, new.subject, COALESCE((SELECT search_text FROM message_bodies WHERE message_id = new.id), ''));
END;
CREATE TRIGGER message_bodies_fts_insert AFTER INSERT ON message_bodies WHEN new.search_text != '' BEGIN
    DELETE FROM messages_fts WHERE rowid = (SELECT rowid FROM messages WHERE id = new.message_id);
    INSERT INTO messages_fts(rowid, subject, search_text)
        SELECT rowid, subject, new.search_text FROM messages WHERE id = new.message_id;
END;
CREATE TRIGGER message_bodies_fts_update AFTER UPDATE OF search_text ON message_bodies WHEN old.search_text IS NOT new.search_text BEGIN
    DELETE FROM messages_fts WHERE rowid = (SELECT rowid FROM messages WHERE id = new.message_id);
    INSERT INTO messages_fts(rowid, subject, search_text)
        SELECT rowid, subject, new.search_text FROM messages WHERE id = new.message_id;
END;
CREATE TRIGGER message_bodies_fts_delete AFTER DELETE ON message_bodies BEGIN
    DELETE FROM messages_fts WHERE rowid = (SELECT rowid FROM messages WHERE id = old.message_id);
    INSERT INTO messages_fts(rowid, subject, search_text)
        SELECT rowid, subject, '' FROM messages WHERE id = old.message_id;
END;

CREATE INDEX messages_in_reply_to ON messages(account_id, in_reply_to, date DESC) WHERE in_reply_to IS NOT NULL;
CREATE TABLE message_references (
    account_id TEXT NOT NULL,
    rfc_id TEXT NOT NULL,
    message_id TEXT NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    PRIMARY KEY (account_id, rfc_id, message_id)
) WITHOUT ROWID;
CREATE INDEX message_references_message ON message_references(message_id);
INSERT OR IGNORE INTO message_references(account_id, rfc_id, message_id)
    SELECT m.account_id, j.value, m.id
    FROM messages m, json_each(m.references_json) j
    WHERE m.references_json IS NOT NULL
      AND json_valid(m.references_json)
      AND json_type(m.references_json) = 'array'
      AND typeof(j.value) = 'text';

CREATE INDEX events_calendar_range ON events(calendar_id, start, end);
CREATE INDEX draft_attachments_draft ON draft_attachments(draft_id);
CREATE INDEX pending_operations_active ON pending_operations(account_id, seq) WHERE state != 'applied';
CREATE INDEX pending_operations_applied ON pending_operations(state, updated_at) WHERE state = 'applied';
CREATE INDEX caldav_collections_connection ON caldav_collections(connection_id);
CREATE INDEX caldav_outbox_connection ON caldav_outbox(connection_id);
CREATE INDEX calendars_account ON calendars(account_id);
CREATE INDEX change_history_at ON change_history(at);

ALTER TABLE contact_connections ADD COLUMN generation INTEGER NOT NULL DEFAULT 1;
CREATE TABLE contact_connection_generations (
    account_id TEXT PRIMARY KEY REFERENCES accounts(id) ON DELETE CASCADE,
    generation INTEGER NOT NULL,
    last_connected_at INTEGER NOT NULL
);
INSERT OR IGNORE INTO contact_connection_generations(account_id, generation, last_connected_at)
    SELECT account_id, generation, created_at FROM contact_connections;
ALTER TABLE people ADD COLUMN edit_revision INTEGER NOT NULL DEFAULT 0;
"#;

/// Contacts servers other than Fastmail may take a password of their own.
const V22: &str = "
ALTER TABLE contact_connections ADD COLUMN credential TEXT NOT NULL DEFAULT 'incoming-mail';
";

/// Contacts from a CardDAV server (Fastmail first): the connection the person agreed to, and
/// each card's version as last seen, which edits are applied against. Tables of their own,
/// so nothing that exists is altered.
const V21: &str = "
CREATE TABLE contact_connections (
    account_id TEXT PRIMARY KEY REFERENCES accounts(id) ON DELETE CASCADE,
    server_url TEXT NOT NULL,
    username TEXT NOT NULL,
    default_book TEXT,
    created_at INTEGER NOT NULL
);
CREATE TABLE contact_cards (
    account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    contact_id TEXT NOT NULL,
    address_book TEXT NOT NULL,
    url TEXT NOT NULL,
    etag TEXT NOT NULL,
    base_json TEXT NOT NULL,
    PRIMARY KEY(account_id, contact_id)
);
";

/// The change history behind Undo, Ctrl+Z and the History view.
const V20: &str = "
CREATE TABLE change_history (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    at INTEGER NOT NULL,
    title TEXT NOT NULL,
    detail TEXT NOT NULL,
    reversal_json TEXT NOT NULL,
    undone_at INTEGER
);
CREATE INDEX change_history_account ON change_history(account_id, at);
";

const V19: &str = "
CREATE TABLE draft_imports (
    draft_id TEXT PRIMARY KEY REFERENCES drafts(id) ON DELETE CASCADE,
    source_json TEXT NOT NULL
);
-- Deliberately independent of drafts: discarding/sending during an upload must
-- retain the remote identity and the exact bytes needed to reconcile a lost reply.
CREATE TABLE draft_sync (
    draft_id TEXT PRIMARY KEY,
    account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    remote_json TEXT,
    base_json TEXT,
    pending_json TEXT,
    pending_raw BLOB
);
CREATE INDEX draft_sync_account ON draft_sync(account_id);
INSERT INTO draft_sync(draft_id, account_id) SELECT d.id, d.account_id FROM drafts d JOIN accounts a ON a.id=d.account_id WHERE a.provider IN ('gmail','imap');
";

/// The signature chosen for a draft, as it was when chosen. A table of its own, so nothing
/// that exists is altered.
const V18: &str = "
CREATE TABLE draft_signatures (
    draft_id TEXT PRIMARY KEY REFERENCES drafts(id) ON DELETE CASCADE,
    signature_id TEXT,
    html TEXT NOT NULL
);
";

const V17: &str = "
ALTER TABLE draft_attachments ADD COLUMN content_id TEXT;
";

const V16: &str = "
CREATE TABLE caldav_checkpoints (
    calendar_id TEXT PRIMARY KEY REFERENCES caldav_collections(calendar_id) ON DELETE CASCADE,
    token TEXT NOT NULL,
    range_start INTEGER NOT NULL,
    range_end INTEGER NOT NULL,
    timezone TEXT NOT NULL
);
";

const V15: &str = "
CREATE TABLE caldav_outbox (
    id TEXT PRIMARY KEY,
    connection_id TEXT NOT NULL REFERENCES calendar_connections(id) ON DELETE CASCADE,
    calendar_id TEXT NOT NULL REFERENCES calendars(id) ON DELETE CASCADE,
    href TEXT NOT NULL,
    revision INTEGER NOT NULL,
    payload_json TEXT NOT NULL,
    UNIQUE(calendar_id,href)
);
";

/// Check before changing journal mode or migrating, leaving newer databases untouched.
/// Returns the current `PRAGMA user_version`.
pub fn ensure_supported(conn: &Connection) -> super::Result<i64> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version > MIGRATIONS.len() as i64 {
        return Err(super::StoreError::Invalid("This database was created by a newer version of Overlook. Update Overlook before opening it.".into()));
    }
    Ok(version)
}

/// Applies each pending migration atomically inside a transaction together with
/// `PRAGMA user_version`, so DDL can never commit without advancing the schema version.
pub fn migrate(conn: &mut Connection) -> rusqlite::Result<()> {
    let current: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    for (i, sql) in MIGRATIONS.iter().enumerate() {
        let version = (i + 1) as i64;
        if version > current {
            let tx = conn.transaction()?;
            tx.execute_batch(sql)?;
            tx.pragma_update(None, "user_version", version)?;
            tx.commit()?;
        }
    }
    Ok(())
}

const V1: &str = r#"
CREATE TABLE accounts (
    id TEXT PRIMARY KEY,
    provider TEXT NOT NULL,
    display_name TEXT NOT NULL,
    email TEXT NOT NULL,
    created_at INTEGER NOT NULL
);

CREATE TABLE labels (
    id TEXT PRIMARY KEY,
    account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    kind TEXT NOT NULL,
    name TEXT NOT NULL,
    provider_id TEXT,
    color TEXT,
    sort_order INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX labels_account ON labels(account_id, kind);

-- Downloaded mail. Replaceable cache, except that rows referenced by pending
-- operations must not be evicted (enforced by the eviction routine, not the schema).
CREATE TABLE messages (
    id TEXT PRIMARY KEY,
    account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    conversation_id TEXT NOT NULL,
    provider_id TEXT,
    from_json TEXT NOT NULL,
    to_json TEXT NOT NULL,
    cc_json TEXT NOT NULL,
    bcc_json TEXT NOT NULL,
    reply_to_json TEXT,
    subject TEXT NOT NULL,
    snippet TEXT NOT NULL,
    body_text TEXT,
    body_html TEXT,
    search_text TEXT NOT NULL,
    date INTEGER NOT NULL,
    is_read INTEGER NOT NULL DEFAULT 0,
    is_starred INTEGER NOT NULL DEFAULT 0,
    is_outgoing INTEGER NOT NULL DEFAULT 0,
    rfc_message_id TEXT,
    in_reply_to TEXT
);
CREATE INDEX messages_conversation ON messages(account_id, conversation_id);
CREATE INDEX messages_date ON messages(account_id, date DESC);
CREATE UNIQUE INDEX messages_provider ON messages(account_id, provider_id) WHERE provider_id IS NOT NULL;

CREATE TABLE message_labels (
    message_id TEXT NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    label_id TEXT NOT NULL REFERENCES labels(id) ON DELETE CASCADE,
    PRIMARY KEY (message_id, label_id)
);
CREATE INDEX message_labels_label ON message_labels(label_id);

CREATE TABLE attachments (
    id TEXT PRIMARY KEY,
    message_id TEXT NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    filename TEXT NOT NULL,
    mime_type TEXT NOT NULL,
    size_bytes INTEGER NOT NULL,
    local_path TEXT,
    provider_id TEXT
);

-- Protected local work. Never evicted as cache.
CREATE TABLE drafts (
    id TEXT PRIMARY KEY,
    account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    kind TEXT NOT NULL,
    conversation_id TEXT,
    in_reply_to TEXT,
    to_json TEXT NOT NULL,
    cc_json TEXT NOT NULL,
    bcc_json TEXT NOT NULL,
    subject TEXT NOT NULL,
    body_text TEXT NOT NULL,
    body_html TEXT,
    state_json TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    provider_id TEXT
);
CREATE INDEX drafts_account ON drafts(account_id, updated_at DESC);

CREATE TABLE draft_attachments (
    id TEXT PRIMARY KEY,
    draft_id TEXT NOT NULL REFERENCES drafts(id) ON DELETE CASCADE,
    filename TEXT NOT NULL,
    mime_type TEXT NOT NULL,
    size_bytes INTEGER NOT NULL,
    local_path TEXT NOT NULL
);

CREATE TABLE pending_operations (
    id TEXT PRIMARY KEY,
    account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    seq INTEGER NOT NULL,
    kind_json TEXT NOT NULL,
    state TEXT NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    UNIQUE (account_id, seq)
);

CREATE TABLE sync_state (
    account_id TEXT PRIMARY KEY REFERENCES accounts(id) ON DELETE CASCADE,
    state_json TEXT NOT NULL,
    last_successful_sync INTEGER,
    checkpoint TEXT
);

CREATE TABLE settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

-- Full-text index over subject, people, and body text (external content table).
CREATE VIRTUAL TABLE messages_fts USING fts5(
    subject, search_text,
    content='messages', content_rowid='rowid',
    tokenize='unicode61 remove_diacritics 2'
);
CREATE TRIGGER messages_fts_insert AFTER INSERT ON messages BEGIN
    INSERT INTO messages_fts(rowid, subject, search_text) VALUES (new.rowid, new.subject, new.search_text);
END;
CREATE TRIGGER messages_fts_delete AFTER DELETE ON messages BEGIN
    INSERT INTO messages_fts(messages_fts, rowid, subject, search_text) VALUES ('delete', old.rowid, old.subject, old.search_text);
END;
CREATE TRIGGER messages_fts_update AFTER UPDATE OF subject, search_text ON messages BEGIN
    INSERT INTO messages_fts(messages_fts, rowid, subject, search_text) VALUES ('delete', old.rowid, old.subject, old.search_text);
    INSERT INTO messages_fts(rowid, subject, search_text) VALUES (new.rowid, new.subject, new.search_text);
END;
"#;

const V2: &str = r#"
ALTER TABLE messages ADD COLUMN category TEXT;
ALTER TABLE messages ADD COLUMN is_important INTEGER NOT NULL DEFAULT 0;
ALTER TABLE messages ADD COLUMN focus_override INTEGER;
CREATE TABLE focus_rules (
    account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    email TEXT NOT NULL,
    focused INTEGER NOT NULL,
    PRIMARY KEY (account_id, email)
);
"#;

const V3: &str = r#"
CREATE TABLE calendars (
    id TEXT PRIMARY KEY,
    account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    provider_id TEXT,
    name TEXT NOT NULL,
    color TEXT,
    is_primary INTEGER NOT NULL DEFAULT 0,
    can_edit INTEGER NOT NULL DEFAULT 0,
    visible INTEGER NOT NULL DEFAULT 1,
    sort_order INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE events (
    id TEXT PRIMARY KEY,
    account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    calendar_id TEXT NOT NULL REFERENCES calendars(id) ON DELETE CASCADE,
    provider_id TEXT,
    ical_uid TEXT,
    summary TEXT NOT NULL,
    description TEXT,
    location TEXT,
    start INTEGER NOT NULL,
    end INTEGER NOT NULL,
    all_day INTEGER NOT NULL DEFAULT 0,
    status TEXT NOT NULL,
    organizer_json TEXT,
    attendees_json TEXT NOT NULL,
    html_link TEXT,
    conference_url TEXT,
    recurring_event_id TEXT,
    updated INTEGER NOT NULL
);
CREATE INDEX events_range ON events(account_id, start, end);
CREATE UNIQUE INDEX events_provider ON events(calendar_id, provider_id) WHERE provider_id IS NOT NULL;
"#;

const V4: &str = r#"
ALTER TABLE events ADD COLUMN event_type TEXT;
"#;

const V5: &str = r#"
ALTER TABLE events ADD COLUMN color TEXT;
ALTER TABLE events ADD COLUMN reminders_json TEXT;
ALTER TABLE calendars ADD COLUMN default_reminders_json TEXT NOT NULL DEFAULT '[]';
ALTER TABLE messages ADD COLUMN invitation_json TEXT;
CREATE INDEX events_ical ON events(account_id, ical_uid);
"#;

const V6: &str = r#"
ALTER TABLE attachments ADD COLUMN content_id TEXT;
"#;

const V7: &str = r#"
ALTER TABLE drafts ADD COLUMN include_quote INTEGER NOT NULL DEFAULT 0;
"#;

const V8: &str = r#"
ALTER TABLE events ADD COLUMN original_start INTEGER;
ALTER TABLE events ADD COLUMN wants_conference INTEGER NOT NULL DEFAULT 0;
CREATE INDEX events_series ON events(account_id, recurring_event_id);
"#;

const V9: &str = r#"
CREATE TABLE contacts (
    account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    email TEXT NOT NULL,
    name TEXT,
    photo_url TEXT,
    in_address_book INTEGER NOT NULL DEFAULT 0,
    updated INTEGER NOT NULL,
    PRIMARY KEY(account_id, email)
);
"#;

const V10: &str = r#"
CREATE TABLE people (
    id TEXT NOT NULL,
    account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    email TEXT NOT NULL DEFAULT '',
    emails_json TEXT NOT NULL DEFAULT '[]',
    name TEXT,
    given_name TEXT,
    family_name TEXT,
    phone TEXT,
    organization TEXT,
    title TEXT,
    notes TEXT,
    photo_url TEXT,
    in_address_book INTEGER NOT NULL DEFAULT 0,
    etag TEXT,
    updated INTEGER NOT NULL,
    pending INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY(account_id, id)
);
INSERT INTO people(id, account_id, email, emails_json, name, photo_url, in_address_book, updated)
    SELECT 'legacy:' || email, account_id, email, '["' || email || '"]', name, photo_url, in_address_book, updated FROM contacts;
DROP TABLE contacts;
CREATE INDEX people_email ON people(account_id, email);
"#;

// IMAP/POP3 plumbing: server settings, folder sync cursors, the POP3 download ledger,
// the References header for threading, and a fast lookup by Message-ID for move
// reconciliation.
const V11: &str = r#"
CREATE TABLE account_servers (
    account_id TEXT PRIMARY KEY REFERENCES accounts(id) ON DELETE CASCADE,
    servers_json TEXT NOT NULL
);
CREATE TABLE imap_folders (
    account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    path TEXT NOT NULL,
    uidvalidity INTEGER,
    highest_uid INTEGER NOT NULL DEFAULT 0,
    highest_modseq INTEGER,
    delimiter TEXT,
    special_use TEXT,
    subscribed INTEGER NOT NULL DEFAULT 1,
    PRIMARY KEY (account_id, path)
);
CREATE TABLE pop3_seen (
    account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    uidl TEXT NOT NULL,
    seen_at INTEGER NOT NULL,
    deleted_on_server INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (account_id, uidl)
);
ALTER TABLE messages ADD COLUMN references_json TEXT;
CREATE INDEX messages_rfc_id ON messages(account_id, rfc_message_id);
"#;

/// Loading a message's attachments looks them up by message.
const V12: &str = r#"
CREATE INDEX attachments_message ON attachments(message_id);
"#;

// CalDAV setup only. Existing Google calendars, tokens and queued operations are untouched.
const V13: &str = r#"
CREATE TABLE calendar_connections (
    id TEXT PRIMARY KEY,
    account_id TEXT NOT NULL UNIQUE REFERENCES accounts(id) ON DELETE CASCADE,
    settings_json TEXT NOT NULL,
    enabled INTEGER NOT NULL CHECK(enabled IN (0, 1)),
    generation INTEGER NOT NULL CHECK(generation > 0)
);
"#;

const V14: &str = r#"
CREATE TABLE caldav_collections (
    calendar_id TEXT PRIMARY KEY REFERENCES calendars(id) ON DELETE CASCADE,
    connection_id TEXT NOT NULL REFERENCES calendar_connections(id) ON DELETE CASCADE
);
CREATE TABLE caldav_resources (
    calendar_id TEXT NOT NULL REFERENCES caldav_collections(calendar_id) ON DELETE CASCADE,
    href TEXT NOT NULL,
    etag TEXT NOT NULL,
    icalendar TEXT NOT NULL,
    PRIMARY KEY(calendar_id, href)
);
CREATE TABLE caldav_event_resources (
    event_id TEXT PRIMARY KEY REFERENCES events(id) ON DELETE CASCADE,
    calendar_id TEXT NOT NULL,
    href TEXT NOT NULL,
    FOREIGN KEY(calendar_id, href) REFERENCES caldav_resources(calendar_id, href) ON DELETE CASCADE
);
CREATE INDEX caldav_event_resource ON caldav_event_resources(calendar_id, href);
CREATE TABLE caldav_coverage (
    connection_id TEXT NOT NULL REFERENCES calendar_connections(id) ON DELETE CASCADE,
    range_start INTEGER NOT NULL,
    range_end INTEGER NOT NULL,
    timezone TEXT NOT NULL,
    synced_at INTEGER NOT NULL,
    PRIMARY KEY(connection_id, range_start, range_end, timezone)
);
"#;

#[cfg(test)]
mod calendar_migration_tests {
    use super::*;

    #[test]
    fn upgrade_from_v12_preserves_accounts_events_and_pending_operations() {
        let mut conn = Connection::open_in_memory().unwrap();
        for sql in &MIGRATIONS[..12] {
            conn.execute_batch(sql).unwrap();
        }
        conn.pragma_update(None, "user_version", 12).unwrap();
        conn.execute("INSERT INTO accounts(id, provider, display_name, email, created_at) VALUES('test', 'gmail', 'Test', 'test@example.com', 1)", []).unwrap();
        conn.execute("INSERT INTO calendars(id, account_id, provider_id, name) VALUES('calendar', 'test', 'primary', 'Personal')", []).unwrap();
        conn.execute("INSERT INTO events(id, account_id, calendar_id, provider_id, summary, start, end, status, attendees_json, updated)
            VALUES('event', 'test', 'calendar', 'remote-event', 'Unsynced title', 100, 200, 'confirmed', '[]', 1)", []).unwrap();
        let operation = overlook_core::OperationKind::UpdateEvent {
            event_id: overlook_core::EventId::from("event"),
            scope: overlook_core::RecurrenceScope::default(),
        };
        let queued_json = serde_json::to_string(&operation).unwrap();
        conn.execute("INSERT INTO pending_operations(id, account_id, seq, kind_json, state, created_at, updated_at)
            VALUES('operation', 'test', 1, ?1, 'pending', 1, 1)", [&queued_json]).unwrap();
        // Existing unrelated tables must retain their definitions; queued operation JSON needs no rewrite.
        let before: Vec<(String, String)> = conn.prepare("SELECT name, sql FROM sqlite_master WHERE type='table' ORDER BY name").unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().map(Result::unwrap).collect();
        migrate(&mut conn).unwrap();
        assert_eq!(conn.query_row("PRAGMA user_version", [], |r| r.get::<_, u32>(0)).unwrap(), MIGRATIONS.len() as u32);
        assert_eq!(conn.query_row("SELECT email FROM accounts WHERE id='test'", [], |r| r.get::<_, String>(0)).unwrap(), "test@example.com");
        assert_eq!(conn.query_row("SELECT summary FROM events WHERE id='event'", [], |r| r.get::<_, String>(0)).unwrap(), "Unsynced title");
        assert_eq!(conn.query_row("SELECT kind_json FROM pending_operations WHERE id='operation' AND state='pending'", [], |r| r.get::<_, String>(0)).unwrap(), queued_json);
        for (name, definition) in before {
            // V17 adds content_id to draft_attachments; V23 splits messages bodies / rebuilds messages_fts / adds edit_revision to people.
            if matches!(name.as_str(), "draft_attachments" | "messages" | "people") || name.starts_with("messages_fts") {
                continue;
            }
            assert_eq!(conn.query_row("SELECT sql FROM sqlite_master WHERE type='table' AND name=?1", [&name], |r| r.get::<_, String>(0)).unwrap(), definition);
        }
        migrate(&mut conn).unwrap(); // Reopening does not replay the new table creation.
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM calendar_connections", [], |r| r.get::<_, u32>(0)).unwrap(), 0);
    }

    #[test]
    fn upgrade_from_v16_keeps_draft_attachments_without_a_content_id() {
        let mut conn = Connection::open_in_memory().unwrap();
        for sql in &MIGRATIONS[..16] {
            conn.execute_batch(sql).unwrap();
        }
        conn.pragma_update(None, "user_version", 16).unwrap();
        conn.execute("INSERT INTO accounts(id, provider, display_name, email, created_at) VALUES('test', 'gmail', 'Test', 'test@example.com', 1)", []).unwrap();
        conn.execute("INSERT INTO drafts(id, account_id, kind, to_json, cc_json, bcc_json, subject, body_text, state_json, created_at, updated_at)
            VALUES('draft', 'test', 'forward', '[]', '[]', '[]', 'Fwd: Plans', '', '{\"state\":\"editing\"}', 1, 1)", []).unwrap();
        conn.execute("INSERT INTO draft_attachments(id, draft_id, filename, mime_type, size_bytes, local_path) VALUES('att', 'draft', 'plan.pdf', 'application/pdf', 8, '/outgoing/plan.pdf')", []).unwrap();
        migrate(&mut conn).unwrap();
        assert_eq!(conn.query_row("PRAGMA user_version", [], |r| r.get::<_, u32>(0)).unwrap(), MIGRATIONS.len() as u32);
        let (name, content_id) = conn.query_row("SELECT filename, content_id FROM draft_attachments WHERE id='att'", [], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))).unwrap();
        assert_eq!((name.as_str(), content_id), ("plan.pdf", None));
        let (account, base, pending) = conn.query_row("SELECT account_id,base_json,pending_json FROM draft_sync WHERE draft_id='draft'", [], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?, r.get::<_, Option<String>>(2)?))).unwrap();
        assert_eq!((account.as_str(), base, pending), ("test", None, None), "existing offline drafts are enrolled without claiming they are uploaded");
    }

    #[test]
    fn upgrade_from_v22_splits_bodies_backfills_references_and_keeps_fts_and_relations() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("upgrade-v22.sqlite");
        {
            let conn = Connection::open(&path).unwrap();
            conn.pragma_update(None, "foreign_keys", "ON").unwrap();
            for sql in &MIGRATIONS[..22] {
                conn.execute_batch(sql).unwrap();
            }
            conn.pragma_update(None, "user_version", 22).unwrap();
            conn.execute(
                "INSERT INTO accounts(id, provider, display_name, email, created_at) VALUES('acct', 'imap', 'Me', 'me@example.com', 1)",
                [],
            ).unwrap();
            conn.execute(
                "INSERT INTO labels(id, account_id, kind, name) VALUES('inbox', 'acct', 'inbox', 'Inbox')",
                [],
            ).unwrap();
            conn.execute(
                "INSERT INTO messages(id, account_id, conversation_id, provider_id, from_json, to_json, cc_json, bcc_json, subject, snippet, body_text, body_html, search_text, date, rfc_message_id, in_reply_to, references_json)
                 VALUES('m1', 'acct', 'conv-1', 'INBOX\x1f1\x1f10', '{\"name\":\"Maya\",\"email\":\"maya@example.com\"}', '[]', '[]', '[]', 'Quarterly roadmap', 'Launch snippet', 'Full text body about rockets', '<p>HTML rockets</p>', 'Maya maya@example.com\nFull text body about rockets', 1000, '<m1@x>', '<root@x>', '[\"<grand@x>\",\"<root@x>\"]')",
                [],
            ).unwrap();
            conn.execute("INSERT INTO message_labels(message_id, label_id) VALUES('m1', 'inbox')", []).unwrap();
            conn.execute(
                "INSERT INTO attachments(id, message_id, filename, mime_type, size_bytes) VALUES('att1', 'm1', 'doc.pdf', 'application/pdf', 42)",
                [],
            ).unwrap();
            conn.execute(
                "INSERT INTO contact_connections(account_id, server_url, username, created_at, credential) VALUES('acct', 'https://dav.test/', 'me', 500, 'incoming-mail')",
                [],
            ).unwrap();
            conn.execute(
                "INSERT INTO people(id, account_id, email, emails_json, name, updated) VALUES('p1', 'acct', 'maya@example.com', '[\"maya@example.com\"]', 'Maya', 10)",
                [],
            ).unwrap();
        }

        let store = crate::Store::open(&path).unwrap();
        let acct = overlook_core::AccountId::from("acct");

        // Verify messages columns no longer contain body_text, body_html, search_text.
        let cols: Vec<String> = store.with_read(|c| {
            let mut stmt = c.prepare("PRAGMA table_info(messages)")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        }).unwrap();
        assert!(!cols.contains(&"body_text".to_owned()));
        assert!(!cols.contains(&"body_html".to_owned()));
        assert!(!cols.contains(&"search_text".to_owned()));

        // Verify full message, labels, attachments, and bodies survived intact.
        let m = store.message(&overlook_core::MessageId::from("m1")).unwrap().unwrap();
        assert_eq!(m.body.text.as_deref(), Some("Full text body about rockets"));
        assert_eq!(m.body.html.as_deref(), Some("<p>HTML rockets</p>"));
        assert!(m.has_label(&overlook_core::LabelId::from("inbox")));
        assert_eq!(m.attachments.len(), 1);
        assert_eq!(m.attachments[0].filename, "doc.pdf");

        // Verify FTS5 works after upgrade.
        assert_eq!(store.search(&acct, "rockets").unwrap(), vec![overlook_core::MessageId::from("m1")]);
        assert_eq!(store.search(&acct, "roadmap").unwrap(), vec![overlook_core::MessageId::from("m1")]);

        // Verify message_references backfill and conversation_for_rfc_id.
        assert_eq!(store.conversation_for_rfc_id(&acct, "<grand@x>").unwrap(), Some(overlook_core::ConversationId::from("conv-1")));
        assert_eq!(store.conversation_for_rfc_id(&acct, "<root@x>").unwrap(), Some(overlook_core::ConversationId::from("conv-1")));

        // Verify has_cached_body & search_coverage.
        assert!(store.has_cached_body(&acct, &overlook_core::ProviderId("INBOX\x1f1\x1f10".into())).unwrap());
        let cov = store.search_coverage(&acct).unwrap();
        assert_eq!((cov.messages_total, cov.bodies_cached), (1, 1));

        // Verify contact_connections generation and contact_connection_generations backfill.
        let conn = store.contacts_connection(&acct).unwrap().unwrap();
        assert_eq!(conn.connected_at, 500);
        let gen: i64 = store.with_read(|c| Ok(c.query_row("SELECT generation FROM contact_connections WHERE account_id='acct'", [], |r| r.get(0))?)).unwrap();
        assert_eq!(gen, 1);

        // Verify foreign key integrity across the entire upgraded database.
        let fk_issues: i64 = store.with_read(|c| {
            let mut stmt = c.prepare("PRAGMA foreign_key_check")?;
            let count = stmt.query_map([], |_| Ok(()))?.count() as i64;
            Ok(count)
        }).unwrap();
        assert_eq!(fk_issues, 0);
    }

    #[test]
    fn fresh_file_db_sets_expected_pragmas_tables_and_indexes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fresh.sqlite");
        let store = crate::Store::open(&path).unwrap();
        let (version, sync_mode, auto_vacuum, journal_mode): (i64, i64, i64, String) = store.with_conn(|c| {
            Ok((
                c.query_row("PRAGMA user_version", [], |r| r.get(0))?,
                c.query_row("PRAGMA synchronous", [], |r| r.get(0))?,
                c.query_row("PRAGMA auto_vacuum", [], |r| r.get(0))?,
                c.query_row("PRAGMA journal_mode", [], |r| r.get(0))?,
            ))
        }).unwrap();
        assert_eq!(version, MIGRATIONS.len() as i64);
        assert_eq!(sync_mode, 2, "PRAGMA synchronous must remain FULL");
        assert_eq!(auto_vacuum, 2, "fresh DBs enable INCREMENTAL auto_vacuum");
        assert_eq!(journal_mode.to_ascii_lowercase(), "wal");

        let indexes: std::collections::HashSet<String> = store.with_read(|c| {
            let mut stmt = c.prepare("SELECT name FROM sqlite_master WHERE type='index'")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        }).unwrap();
        for expected in [
            "messages_in_reply_to",
            "message_references_message",
            "events_calendar_range",
            "draft_attachments_draft",
            "pending_operations_active",
            "pending_operations_applied",
            "caldav_collections_connection",
            "caldav_outbox_connection",
            "calendars_account",
            "change_history_at",
            "conversation_snoozes_wake",
        ] {
            assert!(indexes.contains(expected), "missing index {expected}");
        }
    }

    #[test]
    fn newer_schema_is_rejected_before_journal_mode_changes() {
        let path = std::env::temp_dir().join(format!("overlook-newer-schema-{}.sqlite", overlook_core::CalendarConnectionId::generate()));
        {
            let conn = Connection::open(&path).unwrap();
            conn.pragma_update(None, "journal_mode", "DELETE").unwrap();
            conn.pragma_update(None, "user_version", 999).unwrap();
        }
        assert!(matches!(crate::Store::open(&path), Err(crate::StoreError::Invalid(_))));
        {
            let conn = Connection::open(&path).unwrap();
            assert_eq!(conn.query_row("PRAGMA user_version", [], |r| r.get::<_, u32>(0)).unwrap(), 999);
            assert_eq!(conn.query_row("PRAGMA journal_mode", [], |r| r.get::<_, String>(0)).unwrap(), "delete");
        }
        std::fs::remove_file(path).unwrap();
    }
}
