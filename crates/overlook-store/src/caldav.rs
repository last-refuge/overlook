//! Atomic CalDAV snapshot commits and lifecycle guards. Mail state is independent.
use super::*;
#[cfg(test)]
#[path = "caldav_tests.rs"]
mod tests;

pub(super) fn reject_caldav_write(tx: &Connection, calendar: &CalendarId) -> Result<()> {
    let is_caldav: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM caldav_collections WHERE calendar_id=?1)",
        [&calendar.0],
        |r| r.get(0),
    )?;
    if is_caldav {
        return Err(StoreError::Invalid(
            "This action is not supported through the CalDAV event editor. No change was queued."
                .into(),
        ));
    }
    Ok(())
}

fn setup_current(tx: &Connection, connection: &CalendarConnection) -> Result<bool> {
    let expected = serde_json::to_string(&connection.id)?;
    Ok(tx.query_row("SELECT EXISTS(SELECT 1 FROM settings WHERE key=?1 AND value=?2) AND EXISTS(SELECT 1 FROM accounts WHERE id=?3)",
        params![format!("caldav_setup:{}", connection.account_id), expected, connection.account_id.0], |r| r.get(0))?)
}

fn current(tx: &Connection, connection: &CalendarConnection) -> Result<bool> {
    Ok(tx.query_row("SELECT EXISTS(SELECT 1 FROM calendar_connections WHERE id=?1 AND account_id=?2 AND generation=?3 AND enabled=1)",
        params![connection.id.0, connection.account_id.0, connection.generation], |r| r.get(0))?)
}

fn check_account(tx: &Connection, account: &AccountId) -> Result<()> {
    let kind: String = tx.query_row(
        "SELECT provider FROM accounts WHERE id=?1",
        [&account.0],
        |r| r.get(0),
    )?;
    if kind != "imap" && kind != "pop3" {
        return Err(StoreError::Invalid(
            "CalDAV setup belongs to an IMAP or POP3 account.".into(),
        ));
    }
    let mut stmt = tx.prepare(
        "SELECT kind_json FROM pending_operations WHERE account_id=?1 AND state!='applied'",
    )?;
    for row in stmt.query_map([&account.0], |r| r.get::<_, String>(0))? {
        if serde_json::from_str::<OperationKind>(&row?)?.is_calendar() {
            return Err(StoreError::Invalid(
                "Resolve pending calendar changes before replacing or disconnecting calendars."
                    .into(),
            ));
        }
    }
    Ok(())
}

fn status_tx(tx: &Connection, connection: &CalendarConnection, state: &SyncState) -> Result<()> {
    tx.execute("INSERT INTO settings(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        params![format!("cal_status:{}", connection.account_id), serde_json::to_string(state)?])?;
    Ok(())
}

fn checkpoints_tx(
    tx: &Connection,
    connection: &CalendarConnection,
    from: i64,
    to: i64,
    timezone: &str,
) -> Result<Vec<CalDavCheckpoint>> {
    if !current(tx, connection)? {
        return Ok(Vec::new());
    }
    let mut statement = tx.prepare("SELECT p.calendar_id,p.token FROM caldav_checkpoints p JOIN caldav_collections c ON c.calendar_id=p.calendar_id WHERE c.connection_id=?1 AND p.range_start=?2 AND p.range_end=?3 AND p.timezone=?4 AND NOT EXISTS(SELECT 1 FROM caldav_outbox o WHERE o.calendar_id=p.calendar_id) AND EXISTS(SELECT 1 FROM caldav_coverage v WHERE v.connection_id=?1 AND v.range_start<=?2 AND v.range_end>=?3 AND v.timezone=?4 AND v.synced_at>0)")?;
    let rows = statement
        .query_map(params![connection.id.0, from, to, timezone], |row| {
            Ok(CalDavCheckpoint {
                calendar_id: CalendarId(row.get(0)?),
                token: row.get(1)?,
                from,
                to,
                timezone: timezone.into(),
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

fn snapshot_tx(
    tx: &Connection,
    connection: &CalendarConnection,
    snapshot: &CalDavSnapshot,
    reused: &[CalDavCheckpoint],
) -> Result<()> {
    if !current(tx, connection)? {
        return Err(StoreError::Invalid(
            "Calendar connection changed; the old download was discarded.".into(),
        ));
    }
    check_account(tx, &connection.account_id)?;
    if snapshot.from >= snapshot.to {
        return Err(StoreError::Invalid("Invalid calendar coverage.".into()));
    }
    let available = checkpoints_tx(
        tx,
        connection,
        snapshot.from,
        snapshot.to,
        &snapshot.timezone,
    )?;
    let mut retained = HashSet::new();
    for checkpoint in reused {
        if !available.contains(checkpoint)
            || !retained.insert(checkpoint.calendar_id.clone())
            || !snapshot.calendars.iter().any(|entry| {
                entry.calendar.id == checkpoint.calendar_id && entry.resources.is_empty()
            })
        {
            return Err(StoreError::Invalid(
                "Calendar cache changed during sync. The download will be retried.".into(),
            ));
        }
    }
    // Checkpoints and their event projections are replaced in the same transaction.
    tx.execute("DELETE FROM caldav_checkpoints WHERE calendar_id IN (SELECT calendar_id FROM caldav_collections WHERE connection_id=?1)", [&connection.id.0])?;
    let old: Vec<String> = tx
        .prepare("SELECT calendar_id FROM caldav_collections WHERE connection_id=?1")?
        .query_map([&connection.id.0], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut keep = HashSet::new();
    let mut invalidated = false;
    for entry in &snapshot.calendars {
        let c = &entry.calendar;
        if c.account_id != connection.account_id
            || !c.id.0.starts_with(&format!("caldav:{}:", connection.id))
            || !keep.insert(c.id.0.clone())
        {
            return Err(StoreError::Invalid(
                "Calendar snapshot identity mismatch.".into(),
            ));
        }
        invalidated |= !old.contains(&c.id.0);
        tx.execute("INSERT INTO calendars(id, account_id, provider_id, name, color, is_primary, can_edit, visible, sort_order, default_reminders_json)
            VALUES(?1,?2,?3,?4,?5,?6,?9,?8,?7,'[]') ON CONFLICT(id) DO UPDATE SET name=excluded.name, color=excluded.color, is_primary=excluded.is_primary, can_edit=excluded.can_edit, sort_order=excluded.sort_order",
            params![c.id.0, c.account_id.0, c.provider_id.as_ref().map(|p| &p.0), c.name, c.color, c.is_primary, c.sort_order, c.visible,c.can_edit])?;
        tx.execute(
            "INSERT OR IGNORE INTO caldav_collections(calendar_id,connection_id) VALUES(?1,?2)",
            params![c.id.0, connection.id.0],
        )?;
        if retained.contains(&c.id) {
            continue;
        }
        // A bounded complete query authorizes replacement only in this coverage window.
        tx.execute(
            "DELETE FROM events WHERE calendar_id=?1 AND start<?3 AND MAX(end,start+1)>?2 AND NOT EXISTS(SELECT 1 FROM caldav_event_resources m JOIN caldav_outbox o ON o.calendar_id=m.calendar_id AND o.href=m.href WHERE m.event_id=events.id)",
            params![c.id.0, snapshot.from, snapshot.to],
        )?;
        // Preload outbox state and existing resource etags once per calendar instead of per resource.
        let outbox_hrefs: HashSet<String> = {
            let mut stmt = tx.prepare_cached("SELECT href FROM caldav_outbox WHERE calendar_id=?1")?;
            let rows = stmt.query_map([&c.id.0], |r| r.get(0))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        let outbox_uids: HashSet<String> = {
            let mut stmt = tx.prepare_cached(
                "SELECT COALESCE(json_extract(payload_json,'$.edits[0].after.ical_uid'),json_extract(payload_json,'$.edits[0].before.ical_uid'))
                 FROM caldav_outbox WHERE calendar_id=?1",
            )?;
            let rows = stmt.query_map([&c.id.0], |r| r.get::<_, Option<String>>(0))?;
            let mut set = HashSet::new();
            for row in rows {
                if let Some(uid) = row? {
                    set.insert(uid);
                }
            }
            set
        };
        let mut existing_etags: HashMap<String, String> = {
            let mut stmt = tx.prepare_cached("SELECT href, etag FROM caldav_resources WHERE calendar_id=?1")?;
            let rows = stmt.query_map([&c.id.0], |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        for resource in &entry.resources {
            if outbox_hrefs.contains(&resource.href) {
                continue;
            }
            let moved_pending = resource
                .events
                .iter()
                .filter_map(|e| e.ical_uid.as_ref())
                .any(|uid| outbox_uids.contains(uid));
            if moved_pending {
                continue;
            }
            if existing_etags.get(&resource.href).is_some_and(|etag| etag != &resource.etag) {
                // A changed master invalidates older projections outside this range too.
                tx.prepare_cached("DELETE FROM events WHERE id IN (SELECT event_id FROM caldav_event_resources WHERE calendar_id=?1 AND href=?2)")?
                    .execute(params![c.id.0, resource.href])?;
                invalidated = true;
            }
            tx.prepare_cached("INSERT INTO caldav_resources(calendar_id,href,etag,icalendar) VALUES(?1,?2,?3,?4) ON CONFLICT(calendar_id,href) DO UPDATE SET etag=excluded.etag,icalendar=excluded.icalendar")?
                .execute(params![c.id.0, resource.href, resource.etag, resource.icalendar])?;
            existing_etags.insert(resource.href.clone(), resource.etag.clone());
            for e in &resource.events {
                if e.account_id != connection.account_id
                    || e.calendar_id != c.id
                    || e.pending
                    || e.start >= snapshot.to
                    || e.end.max(e.start.saturating_add(1)) <= snapshot.from
                {
                    return Err(StoreError::Invalid(
                        "Calendar event is outside its snapshot.".into(),
                    ));
                }
                upsert_event_tx(tx, e)?;
                tx.prepare_cached("INSERT INTO caldav_event_resources(event_id,calendar_id,href) VALUES(?1,?2,?3) ON CONFLICT(event_id) DO UPDATE SET calendar_id=excluded.calendar_id,href=excluded.href")?
                    .execute(params![e.id.0, c.id.0, resource.href])?;
            }
        }
    }
    for gone in old.iter().filter(|id| !keep.contains(*id)) {
        let protected: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM caldav_outbox WHERE calendar_id=?1)",
            [gone],
            |r| r.get(0),
        )?;
        if protected {
            continue;
        }
        tx.execute("DELETE FROM calendars WHERE id=?1", [gone])?;
        invalidated = true;
    }
    if invalidated {
        tx.execute(
            "DELETE FROM caldav_coverage WHERE connection_id=?1",
            [&connection.id.0],
        )?;
    }
    tx.execute("DELETE FROM caldav_coverage WHERE connection_id=?1 AND (timezone!=?4 OR (range_start<?3 AND range_end>?2))",
        params![connection.id.0, snapshot.from, snapshot.to, snapshot.timezone])?;
    tx.execute("INSERT INTO caldav_coverage(connection_id,range_start,range_end,timezone,synced_at) VALUES(?1,?2,?3,?4,?5)",
        params![connection.id.0, snapshot.from, snapshot.to, snapshot.timezone, now()])?;
    let pending: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM caldav_outbox WHERE connection_id=?1)",
        [&connection.id.0],
        |r| r.get(0),
    )?;
    status_tx(
        tx,
        connection,
        &if pending {
            SyncState::Paused("Saved calendar changes are waiting to sync or need review.".into())
        } else {
            SyncState::UpToDate
        },
    )
}

impl Store {
    pub fn caldav_checkpoints(
        &self,
        connection: &CalendarConnection,
        from: i64,
        to: i64,
        timezone: &str,
    ) -> Result<Vec<CalDavCheckpoint>> {
        self.with_read(|tx| checkpoints_tx(tx, connection, from, to, timezone))
    }

    pub fn apply_caldav_download(
        &self,
        connection: &CalendarConnection,
        download: &CalDavDownload,
    ) -> Result<()> {
        self.with_tx(|tx| {
            let snapshot = &download.snapshot;
            snapshot_tx(tx, connection, snapshot, &download.reused)?;
            let mut seen = HashSet::new();
            for checkpoint in &download.checkpoints {
                if checkpoint.from != snapshot.from || checkpoint.to != snapshot.to || checkpoint.timezone != snapshot.timezone
                    || checkpoint.token.is_empty() || checkpoint.token.len() > 4096
                    || checkpoint.token.chars().any(char::is_control)
                    || !seen.insert(&checkpoint.calendar_id)
                    || !snapshot.calendars.iter().any(|entry| entry.calendar.id == checkpoint.calendar_id) {
                    return Err(StoreError::Invalid("Invalid calendar checkpoint.".into()));
                }
                // A snapshot deliberately preserves pending local edits. It cannot
                // certify that collection's cache as matching the server token.
                tx.execute("INSERT INTO caldav_checkpoints(calendar_id,token,range_start,range_end,timezone) SELECT ?1,?2,?3,?4,?5 WHERE NOT EXISTS(SELECT 1 FROM caldav_outbox WHERE calendar_id=?1)",
                    params![checkpoint.calendar_id.0, checkpoint.token, checkpoint.from, checkpoint.to, checkpoint.timezone])?;
            }
            Ok(())
        })
    }

    pub fn reserve_caldav_setup(
        &self,
        account: &AccountId,
        request: &CalendarConnectionId,
    ) -> Result<()> {
        self.with_tx(|tx| {
            check_account(tx, account)?;
            super::caldav_outbox::ensure_no_changes(tx,account)?;
            tx.execute("INSERT INTO settings(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                params![format!("caldav_setup:{account}"), serde_json::to_string(request)?])?;
            Ok(())
        })
    }

    pub fn cancel_caldav_setup(&self, account: &AccountId) -> Result<()> {
        self.with_tx(|tx| {
            tx.execute(
                "DELETE FROM settings WHERE key=?1",
                [format!("caldav_setup:{account}")],
            )?;
            Ok(())
        })
    }

    pub fn caldav_setup_current(&self, connection: &CalendarConnection) -> Result<bool> {
        self.with_read(|tx| setup_current(tx, connection))
    }

    pub fn caldav_connection_current(&self, connection: &CalendarConnection) -> Result<bool> {
        self.with_read(|tx| current(tx, connection))
    }

    pub fn set_caldav_status(
        &self,
        connection: &CalendarConnection,
        state: &SyncState,
    ) -> Result<()> {
        self.with_tx(|tx| {
            if current(tx, connection)? {
                status_tx(tx, connection, state)?;
            }
            Ok(())
        })
    }

    pub fn apply_caldav_snapshot(
        &self,
        connection: &CalendarConnection,
        snapshot: &CalDavSnapshot,
    ) -> Result<()> {
        self.with_tx(|tx| snapshot_tx(tx, connection, snapshot, &[]))
    }

    /// The new connection owns a fresh keyring item. A failed CAS cannot change the old
    /// connection's password, calendar cache, or mail settings. Caller cleans its new secret.
    pub fn install_caldav_connection(
        &self,
        connection: &CalendarConnection,
        previous: Option<&CalendarConnection>,
        snapshot: &CalDavSnapshot,
    ) -> Result<()> {
        connection
            .settings
            .validate()
            .map_err(|e| StoreError::Invalid(e.into()))?;
        self.with_tx(|tx| {
            check_account(tx, &connection.account_id)?;
            super::caldav_outbox::ensure_no_changes(tx,&connection.account_id)?;
            if !setup_current(tx, connection)? { return Err(StoreError::Invalid("Calendar setup was superseded or the account is being removed.".into())); }
            let actual = super::calendar_connections::read_connection(tx, &connection.account_id)?;
            if actual.as_ref() != previous { return Err(StoreError::Invalid("Calendar settings changed. Reopen settings and try again.".into())); }
            if let Some(old) = &actual {
                if old.id == connection.id { return Err(StoreError::Invalid("Replacement connection must own a fresh credential identity.".into())); }
                tx.execute("DELETE FROM calendars WHERE id IN (SELECT calendar_id FROM caldav_collections WHERE connection_id=?1)", [&old.id.0])?;
                tx.execute("DELETE FROM calendar_connections WHERE id=?1", [&old.id.0])?;
            }
            tx.execute("INSERT INTO calendar_connections(id,account_id,settings_json,enabled,generation) VALUES(?1,?2,?3,1,?4)",
                params![connection.id.0, connection.account_id.0, serde_json::to_string(&connection.settings)?, connection.generation])?;
            snapshot_tx(tx, connection, snapshot, &[])
        })
    }

    pub fn disconnect_caldav(&self, connection: &CalendarConnection) -> Result<()> {
        self.with_tx(|tx| {
            if !current(tx, connection)? { return Err(StoreError::Invalid("Calendar settings changed. Reopen settings and try again.".into())); }
            check_account(tx, &connection.account_id)?;
            super::caldav_outbox::ensure_no_changes(tx,&connection.account_id)?;
            tx.execute("DELETE FROM calendars WHERE id IN (SELECT calendar_id FROM caldav_collections WHERE connection_id=?1)", [&connection.id.0])?;
            tx.execute("DELETE FROM calendar_connections WHERE id=?1", [&connection.id.0])?;
            status_tx(tx, connection, &SyncState::Offline)
        })
    }

    pub fn caldav_range_cached(
        &self,
        connection: &CalendarConnectionId,
        from: i64,
        to: i64,
        timezone: &str,
    ) -> Result<bool> {
        self.with_read(|c| Ok(c.query_row("SELECT EXISTS(SELECT 1 FROM caldav_coverage WHERE connection_id=?1 AND range_start<=?2 AND range_end>=?3 AND timezone=?4)",
            params![connection.0, from, to, timezone], |r| r.get(0))?))
    }

    pub fn caldav_last_sync(&self, connection: &CalendarConnectionId) -> Result<Option<i64>> {
        self.with_read(|c| {
            Ok(c.query_row(
                "SELECT MAX(synced_at) FROM caldav_coverage WHERE connection_id=?1 AND synced_at>0",
                [&connection.0],
                |r| r.get(0),
            )?)
        })
    }

    pub fn caldav_timezone_matches(
        &self,
        connection: &CalendarConnectionId,
        timezone: &str,
    ) -> Result<bool> {
        self.with_read(|c| Ok(c.query_row("SELECT EXISTS(SELECT 1 FROM caldav_coverage WHERE connection_id=?1 AND timezone=?2)", params![connection.0, timezone], |r| r.get(0))?))
    }
}
