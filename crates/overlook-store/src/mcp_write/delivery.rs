//! Delivery changes made inside the MCP write transaction.

use super::McpWriteTxn;
use crate::{Result, StoreError, cancel_send_tx, ensure_same_account, queue_send_tx};
use overlook_core::{Draft, OperationId, PendingOperation};

impl McpWriteTxn<'_, '_> {
    pub fn queue_send(&mut self, draft: &Draft) -> Result<(Draft, PendingOperation)> {
        ensure_same_account(&draft.account_id, self.account(), "draft")?;
        queue_send_tx(self.tx, draft)
    }

    pub fn cancel_queued_send(&mut self, draft: Draft) -> Result<(Draft, OperationId)> {
        ensure_same_account(&draft.account_id, self.account(), "draft")?;
        let (draft, operation) = cancel_send_tx(self.tx, draft, false)?;
        let operation = operation
            .ok_or_else(|| StoreError::Invalid("This draft has no queued send.".into()))?;
        Ok((draft, operation))
    }
}
