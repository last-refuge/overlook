//! Draft persistence inside the write receipt's transaction.

use super::*;

impl McpWriteTxn<'_, '_> {
    pub fn save_mcp_draft(&self, draft: &Draft) -> Result<()> {
        if &draft.account_id != self.account {
            return Err(StoreError::NotOwned("draft"));
        }
        save_draft_tx(self.tx, draft)
    }

    pub fn discard_mcp_draft(&self, id: &DraftId) -> Result<()> {
        delete_draft_tx(self.tx, self.account, id)
    }

    pub fn mcp_draft_has_active_send(&self, id: &DraftId) -> Result<bool> {
        Ok(self.tx.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM pending_operations
                WHERE account_id=?1 AND state IN ('pending','in-flight','uncertain')
                  AND json_valid(kind_json)
                  AND json_extract(kind_json,'$.op')='send-draft'
                  AND json_extract(kind_json,'$.draft_id')=?2
            )",
            params![self.account.0, id.0],
            |row| row.get(0),
        )?)
    }

    pub fn mcp_quote_on_reply(&self) -> Result<bool> {
        let stored: Option<String> = self
            .tx
            .query_row("SELECT value FROM settings WHERE key='prefs'", [], |row| {
                row.get(0)
            })
            .optional()?;
        let prefs = stored
            .and_then(|raw| serde_json::from_str::<overlook_core::prefs::Prefs>(&raw).ok())
            .unwrap_or_default();
        Ok(prefs.quote_on_reply)
    }
}
