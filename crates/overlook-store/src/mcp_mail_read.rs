//! Account-scoped cache queries for the MCP mail tools.

use super::*;
use rusqlite::ToSql;

/// Result and coverage for the selected cache scope, before applying any text query.
pub struct McpMailCache {
    pub messages: Vec<Message>,
    pub coverage: SearchCoverage,
    pub oldest: Option<Timestamp>,
    pub newest: Option<Timestamp>,
    pub result_limit_reached: bool,
}

pub enum McpMailSelection<'a> {
    Coverage,
    Headers,
    Search(&'a str),
}

impl Store {
    /// Resolve opaque IDs against lightweight local keys, without reading bodies or addresses.
    pub fn mcp_mail_keys(&self, account: &AccountId) -> Result<Vec<(MessageId, ConversationId)>> {
        self.with_read(|conn| {
            let mut stmt =
                conn.prepare("SELECT id, conversation_id FROM messages WHERE account_id=?1")?;
            let rows = stmt.query_map([account.as_str()], |r| {
                Ok((MessageId(r.get(0)?), ConversationId(r.get(1)?)))
            })?;
            Ok(rows.collect::<std::result::Result<_, _>>()?)
        })
    }

    pub fn mcp_mail_conversation_headers(
        &self,
        account: &AccountId,
        conversation: &ConversationId,
    ) -> Result<Vec<Message>> {
        self.with_read_tx(|tx| {
            load_messages_with(
                tx,
                "WHERE m.account_id=?1 AND m.conversation_id=?2",
                params![account.0, conversation.0],
                false,
            )
        })
    }

    /// Load only the requested provider matches, without downloading or inserting any mail.
    pub fn mcp_mail_provider_headers(
        &self,
        account: &AccountId,
        ids: &[ProviderId],
    ) -> Result<Vec<Message>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        // A single parameter works with all three passes of the normal message loader.
        let ids = serde_json::to_string(ids)?;
        self.with_read_tx(|tx| {
            load_messages_with(
                tx,
                "WHERE m.account_id=?1 AND m.provider_id IN (SELECT value FROM json_each(?2))",
                params![account.0, ids],
                false,
            )
        })
    }

    /// Scope both the FTS hit window and coverage to the requested folder and dates.
    pub fn mcp_mail_cache_search(
        &self,
        account: &AccountId,
        folder: Option<&LabelId>,
        after: Option<Timestamp>,
        before: Option<Timestamp>,
        selection: McpMailSelection<'_>,
    ) -> Result<McpMailCache> {
        const MAX_HITS: usize = 500;
        const SCOPE: &str = "m.account_id=?1 AND (?2 IS NULL OR EXISTS
            (SELECT 1 FROM message_labels l WHERE l.message_id=m.id AND l.label_id=?2))
            AND (?3 IS NULL OR m.date>=?3) AND (?4 IS NULL OR m.date<?4)";
        let folder = folder.map(LabelId::as_str);
        let scope_args: &[&dyn ToSql] = params![account.0, folder, after, before];
        self.with_read_tx(|tx| {
            let (indexed, bodies_cached, oldest, newest) = tx.query_row(
                &format!("SELECT count(*), count(CASE WHEN b.body_text IS NOT NULL OR b.body_html IS NOT NULL THEN 1 END),
                         min(m.date), max(m.date) FROM messages m
                         LEFT JOIN message_bodies b ON b.message_id=m.id WHERE {SCOPE}"),
                scope_args,
                |r| Ok((r.get::<_, i64>(0)? as usize, r.get::<_, i64>(1)? as usize, r.get(2)?, r.get(3)?)),
            )?;
            let count = MAX_HITS as i64 + 1;
            let mut messages = match selection {
                McpMailSelection::Search(query) => load_messages_with(
                    tx,
                    &format!("WHERE m.id IN (
                        SELECT m.id FROM messages_fts JOIN messages m ON m.rowid=messages_fts.rowid
                        WHERE {SCOPE} AND messages_fts MATCH ?5
                        ORDER BY m.date DESC, m.id LIMIT ?6)"),
                    params![account.0, folder, after, before, fts_query(query), count],
                    false,
                )?,
                McpMailSelection::Headers => load_messages_with(
                    tx,
                    &format!("WHERE m.id IN (
                        SELECT m.id FROM messages m WHERE {SCOPE}
                        ORDER BY m.date DESC, m.id LIMIT ?5)"),
                    params![account.0, folder, after, before, count],
                    false,
                )?,
                McpMailSelection::Coverage => Vec::new(),
            };
            messages.sort_unstable_by(|a, b| b.date.cmp(&a.date).then(a.id.cmp(&b.id)));
            let result_limit_reached = messages.len() > MAX_HITS;
            messages.truncate(MAX_HITS);
            Ok(McpMailCache {
                messages,
                coverage: SearchCoverage {
                    messages_indexed: indexed,
                    messages_total: indexed,
                    bodies_cached,
                },
                oldest,
                newest,
                result_limit_reached,
            })
        })
    }
}
