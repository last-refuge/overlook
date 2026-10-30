//! Schema migrations. `PRAGMA user_version` tracks the applied version.

use rusqlite::Connection;

const MIGRATIONS: &[&str] = &[V1, V2, V3, V4, V5, V6, V7, V8, V9, V10, V11, V12, V13, V14, V15, V16, V17, V18];

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
BEGIN;
CREATE TABLE caldav_checkpoints (
    calendar_id TEXT PRIMARY KEY REFERENCES caldav_collections(calendar_id) ON DELETE CASCADE,
    token TEXT NOT NULL,
    range_start INTEGER NOT NULL,
    range_end INTEGER NOT NULL,
    timezone TEXT NOT NULL
);
PRAGMA user_version = 16;
COMMIT;
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
pub fn ensure_supported(conn: &Connection) -> super::Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version > MIGRATIONS.len() as i64 {
        return Err(super::StoreError::Invalid("This database was created by a newer version of Overlook. Update Overlook before opening it.".into()));
    }
    Ok(())
}

pub fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    let current: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    for (i, sql) in MIGRATIONS.iter().enumerate() {
        let version = (i + 1) as i64;
        if version > current {
            conn.execute_batch(sql)?;
            conn.pragma_update(None, "user_version", version)?;
        }
    }
    Ok(())
}

const V1: &str = r#"
BEGIN;
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
COMMIT;
"#;

const V2: &str = r#"
BEGIN;
ALTER TABLE messages ADD COLUMN category TEXT;
ALTER TABLE messages ADD COLUMN is_important INTEGER NOT NULL DEFAULT 0;
ALTER TABLE messages ADD COLUMN focus_override INTEGER;
CREATE TABLE focus_rules (
    account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    email TEXT NOT NULL,
    focused INTEGER NOT NULL,
    PRIMARY KEY (account_id, email)
);
COMMIT;
"#;

const V3: &str = r#"
BEGIN;
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
COMMIT;
"#;

const V4: &str = r#"
BEGIN;
ALTER TABLE events ADD COLUMN event_type TEXT;
COMMIT;
"#;

const V5: &str = r#"
BEGIN;
ALTER TABLE events ADD COLUMN color TEXT;
ALTER TABLE events ADD COLUMN reminders_json TEXT;
ALTER TABLE calendars ADD COLUMN default_reminders_json TEXT NOT NULL DEFAULT '[]';
ALTER TABLE messages ADD COLUMN invitation_json TEXT;
CREATE INDEX events_ical ON events(account_id, ical_uid);
COMMIT;
"#;

const V6: &str = r#"
BEGIN;
ALTER TABLE attachments ADD COLUMN content_id TEXT;
COMMIT;
"#;

const V7: &str = r#"
BEGIN;
ALTER TABLE drafts ADD COLUMN include_quote INTEGER NOT NULL DEFAULT 0;
COMMIT;
"#;

const V8: &str = r#"
BEGIN;
ALTER TABLE events ADD COLUMN original_start INTEGER;
ALTER TABLE events ADD COLUMN wants_conference INTEGER NOT NULL DEFAULT 0;
CREATE INDEX events_series ON events(account_id, recurring_event_id);
COMMIT;
"#;

const V9: &str = r#"
BEGIN;
CREATE TABLE contacts (
    account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    email TEXT NOT NULL,
    name TEXT,
    photo_url TEXT,
    in_address_book INTEGER NOT NULL DEFAULT 0,
    updated INTEGER NOT NULL,
    PRIMARY KEY(account_id, email)
);
COMMIT;
"#;

const V10: &str = r#"
BEGIN;
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
COMMIT;
"#;

// IMAP/POP3 plumbing: server settings, folder sync cursors, the POP3 download ledger,
// the References header for threading, and a fast lookup by Message-ID for move
// reconciliation.
const V11: &str = r#"
BEGIN;
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
COMMIT;
"#;

/// Loading a message's attachments looks them up by message.
const V12: &str = r#"
BEGIN;
CREATE INDEX attachments_message ON attachments(message_id);
COMMIT;
"#;

// CalDAV setup only. Existing Google calendars, tokens and queued operations are untouched.
// Keep the version in this transaction so reopening after a crash cannot replay the DDL.
const V13: &str = r#"
BEGIN;
CREATE TABLE calendar_connections (
    id TEXT PRIMARY KEY,
    account_id TEXT NOT NULL UNIQUE REFERENCES accounts(id) ON DELETE CASCADE,
    settings_json TEXT NOT NULL,
    enabled INTEGER NOT NULL CHECK(enabled IN (0, 1)),
    generation INTEGER NOT NULL CHECK(generation > 0)
);
PRAGMA user_version = 13;
COMMIT;
"#;

const V14: &str = r#"
BEGIN;
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
PRAGMA user_version = 14;
COMMIT;
"#;

#[cfg(test)]
mod calendar_migration_tests {
    use super::*;

    #[test]
    fn upgrade_from_v12_preserves_accounts_events_and_pending_operations() {
        let conn = Connection::open_in_memory().unwrap();
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
        // Existing tables must retain their definitions; queued operation JSON needs no rewrite.
        let before: Vec<(String, String)> = conn.prepare("SELECT name, sql FROM sqlite_master WHERE type='table' ORDER BY name").unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().map(Result::unwrap).collect();
        migrate(&conn).unwrap();
        assert_eq!(conn.query_row("PRAGMA user_version", [], |r| r.get::<_, u32>(0)).unwrap(), MIGRATIONS.len() as u32);
        assert_eq!(conn.query_row("SELECT email FROM accounts WHERE id='test'", [], |r| r.get::<_, String>(0)).unwrap(), "test@example.com");
        assert_eq!(conn.query_row("SELECT summary FROM events WHERE id='event'", [], |r| r.get::<_, String>(0)).unwrap(), "Unsynced title");
        assert_eq!(conn.query_row("SELECT kind_json FROM pending_operations WHERE id='operation' AND state='pending'", [], |r| r.get::<_, String>(0)).unwrap(), queued_json);
        for (name, definition) in before {
            // V17 adds a column to the draft attachments, which have nothing to do with calendars.
            if name == "draft_attachments" {
                continue;
            }
            assert_eq!(conn.query_row("SELECT sql FROM sqlite_master WHERE type='table' AND name=?1", [name], |r| r.get::<_, String>(0)).unwrap(), definition);
        }
        migrate(&conn).unwrap(); // Reopening does not replay the new table creation.
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM calendar_connections", [], |r| r.get::<_, u32>(0)).unwrap(), 0);
    }

    #[test]
    fn upgrade_from_v16_keeps_draft_attachments_without_a_content_id() {
        let conn = Connection::open_in_memory().unwrap();
        for sql in &MIGRATIONS[..16] {
            conn.execute_batch(sql).unwrap();
        }
        conn.pragma_update(None, "user_version", 16).unwrap();
        conn.execute("INSERT INTO accounts(id, provider, display_name, email, created_at) VALUES('test', 'gmail', 'Test', 'test@example.com', 1)", []).unwrap();
        conn.execute("INSERT INTO drafts(id, account_id, kind, to_json, cc_json, bcc_json, subject, body_text, state_json, created_at, updated_at)
            VALUES('draft', 'test', 'forward', '[]', '[]', '[]', 'Fwd: Plans', '', '{\"state\":\"editing\"}', 1, 1)", []).unwrap();
        conn.execute("INSERT INTO draft_attachments(id, draft_id, filename, mime_type, size_bytes, local_path) VALUES('att', 'draft', 'plan.pdf', 'application/pdf', 8, '/outgoing/plan.pdf')", []).unwrap();
        migrate(&conn).unwrap();
        assert_eq!(conn.query_row("PRAGMA user_version", [], |r| r.get::<_, u32>(0)).unwrap(), MIGRATIONS.len() as u32);
        let (name, content_id) = conn.query_row("SELECT filename, content_id FROM draft_attachments WHERE id='att'", [], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))).unwrap();
        assert_eq!((name.as_str(), content_id), ("plan.pdf", None));
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
