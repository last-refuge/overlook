use super::*;

impl McpWriteTxn<'_, '_> {
    /// Save a scheduling change and queue native calendar replay with the MCP receipt.
    /// A previous operation on the same event must finish before its replay data can change.
    pub fn queue_invitation_change(
        &mut self,
        event: &Event,
        response: Option<ResponseStatus>,
    ) -> Result<Option<PendingOperation>> {
        ensure_same_account(&event.account_id, self.account, "event")?;
        caldav::reject_caldav_write(self.tx, &event.calendar_id)?;
        let writable = self.tx.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM events e JOIN calendars c ON e.calendar_id=c.id AND e.account_id=c.account_id
                WHERE e.id=?1 AND c.id=?2 AND c.account_id=?3 AND c.can_edit=1
             )",
            params![event.id.0, event.calendar_id.0, self.account.0],
            |row| row.get::<_, bool>(0),
        )?;
        if !writable {
            return Err(StoreError::NotOwned("writable scheduling calendar"));
        }
        let busy = self.tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM pending_operations
             WHERE account_id=?1 AND state IN ('pending','in-flight','uncertain')
             AND json_extract(kind_json,'$.event_id')=?2)",
            params![self.account.0, event.id.0],
            |row| row.get::<_, bool>(0),
        )?;
        if busy {
            return Ok(None);
        }
        let event_id = event.id.clone();
        let kind = match response {
            Some(response) => OperationKind::RespondToEvent { event_id, response },
            None => OperationKind::UpdateEvent {
                event_id,
                scope: RecurrenceScope::This,
            },
        };
        upsert_event_tx(self.tx, event)?;
        insert_operation_tx(self.tx, self.account, kind, now()).map(Some)
    }
}
