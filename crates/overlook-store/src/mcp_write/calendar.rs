//! Calendar mutations inside the MCP receipt transaction.

use super::*;
use crate::caldav_outbox;
use overlook_core::tz::ZoneDefinition;

pub struct McpCalendar {
    pub id: CalendarId,
    pub can_edit: bool,
    /// An enabled connection for this account owns the collection.
    pub caldav: bool,
}

impl McpWriteTxn<'_, '_> {
    pub fn find_calendar(
        &self,
        mut matches: impl FnMut(&str) -> bool,
    ) -> Result<Option<McpCalendar>> {
        let mut stmt = self.tx.prepare_cached(
            "SELECT c.id, c.can_edit, EXISTS(
                SELECT 1 FROM caldav_collections col
                JOIN calendar_connections conn ON conn.id=col.connection_id
                WHERE col.calendar_id=c.id AND conn.account_id=c.account_id AND conn.enabled=1
            ) FROM calendars c WHERE c.account_id=?1",
        )?;
        let mut rows = stmt.query([&self.account.0])?;
        while let Some(row) = rows.next()? {
            let id: String = row.get(0)?;
            if matches(&id) {
                return Ok(Some(McpCalendar {
                    id: CalendarId(id),
                    can_edit: row.get(1)?,
                    caldav: row.get(2)?,
                }));
            }
        }
        Ok(None)
    }

    pub fn calendar_projection(&self) -> Result<Option<ZoneDefinition>> {
        let value: Option<String> = self
            .tx
            .query_row(
                "SELECT value FROM settings WHERE key='calendar.time_zone'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        Ok(value.as_deref().map(serde_json::from_str).transpose()?)
    }

    pub fn create_native_event(&mut self, event: &Event) -> Result<PendingOperation> {
        Store::apply_event_change_tx(
            self.tx,
            self.account,
            event,
            OperationKind::CreateEvent {
                event_id: event.id.clone(),
            },
        )
    }

    pub fn update_native_event(
        &mut self,
        event: &Event,
        scope: RecurrenceScope,
    ) -> Result<PendingOperation> {
        Store::apply_series_change_tx(self.tx, self.account, event, scope)
    }

    pub fn delete_native_event(
        &mut self,
        event: &Event,
        scope: RecurrenceScope,
    ) -> Result<PendingOperation> {
        Store::delete_event_locally_scoped_tx(self.tx, self.account, event, scope)
    }

    pub fn caldav_change_for_event(&self, event: &Event, creating: bool) -> Result<CalDavChange> {
        ensure_same_account(&event.account_id, self.account, "event")?;
        let change = caldav_outbox::caldav_change_for_event_tx(self.tx, event, creating)?;
        // Date-only timestamps (including recurrence IDs) refer to the
        // zone that produced the cached resource projection.
        if !creating && event.all_day && change.revision == 0 {
            let projected: bool = self.tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM caldav_coverage WHERE connection_id=?1 AND timezone=?2)",
                params![change.connection_id.0, change.timezone],
                |row| row.get(0),
            )?;
            if !projected {
                return Err(StoreError::Invalid(
                    "Sync this calendar in the current time zone before editing the event.".into(),
                ));
            }
        }
        Ok(change)
    }

    pub fn queue_caldav_change(
        &mut self,
        token: &CalDavChange,
        desired: Option<String>,
        edit: CalDavEdit,
    ) -> Result<()> {
        ensure_same_account(&token.account_id, self.account, "calendar change")?;
        caldav_outbox::queue_caldav_change_tx(self.tx, token, desired, edit)
    }

    pub fn find_caldav_change(
        &self,
        matches: impl FnMut(&str) -> bool,
    ) -> Result<Option<CalDavChange>> {
        find_caldav_change(self.tx, self.account, matches)
    }

    pub fn resolve_caldav_conflict(
        &mut self,
        change: &CalDavChange,
        resolution: McpCalDavResolution,
    ) -> Result<()> {
        ensure_same_account(&change.account_id, self.account, "calendar change")?;
        if change.state != CalDavChangeState::Conflict {
            return Err(StoreError::Invalid("This change is not a conflict.".into()));
        }
        match resolution {
            McpCalDavResolution::KeepLocal(desired) => {
                caldav_outbox::rebase_caldav_change_tx(self.tx, change, desired)
            }
            McpCalDavResolution::TakeRemote => caldav_outbox::complete_caldav_change_tx(
                self.tx,
                change,
                change.remote.as_ref(),
                true,
            ),
        }
    }
}

pub enum McpCalDavResolution {
    KeepLocal(Option<String>),
    TakeRemote,
}

impl Store {
    pub fn find_caldav_change(
        &self,
        account: &AccountId,
        matches: impl FnMut(&str) -> bool,
    ) -> Result<Option<CalDavChange>> {
        self.with_read(|tx| find_caldav_change(tx, account, matches))
    }
}

fn find_caldav_change(
    tx: &Connection,
    account: &AccountId,
    mut matches: impl FnMut(&str) -> bool,
) -> Result<Option<CalDavChange>> {
    let mut stmt = tx.prepare_cached(
        "SELECT o.id, o.payload_json FROM caldav_outbox o
         JOIN calendar_connections conn ON conn.id=o.connection_id
         JOIN calendars c ON c.id=o.calendar_id AND c.account_id=conn.account_id
         WHERE conn.account_id=?1 AND conn.enabled=1",
    )?;
    let mut rows = stmt.query([&account.0])?;
    while let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        if matches(&id) {
            let payload: String = row.get(1)?;
            return Ok(Some(serde_json::from_str(&payload)?));
        }
    }
    Ok(None)
}
