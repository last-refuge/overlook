use super::*;
use overlook_core::tz::{self, TimeZone, ZoneDefinition};

const PROJECTION: &str = "calendar.time_zone";

impl Store {
    pub fn calendar_time_zone(&self) -> Result<Option<ZoneDefinition>> {
        self.setting(PROJECTION)
    }

    /// Read the saved event and the zone used by its date-only values together.
    /// Called by the background operation replayer, never the UI thread.
    pub fn calendar_event_for_replay(
        &self,
        id: &EventId,
        fallback: &ZoneDefinition,
    ) -> Result<(Option<Event>, ZoneDefinition)> {
        self.with_read_tx(|tx| {
            let projection: Option<String> = tx
                .query_row(
                    "SELECT value FROM settings WHERE key=?1",
                    [PROJECTION],
                    |r| r.get(0),
                )
                .optional()?;
            let projection = projection
                .as_deref()
                .map(serde_json::from_str)
                .transpose()?
                .unwrap_or_else(|| fallback.clone());
            let event = load_events(tx, "WHERE e.id=?1", [&id.0])?.pop();
            Ok((event, projection))
        })
    }

    /// A successful upload still matters after a zone change. Reproject its
    /// returned date-only values before merging the provider's copy.
    pub fn finish_calendar_write(&self, event: &Event, source: &ZoneDefinition) -> Result<()> {
        let source_zone = source
            .resolve()
            .map_err(|_| StoreError::Invalid("Invalid calendar time zone.".into()))?;
        self.with_tx(|tx| {
            let current: Option<String> = tx
                .query_row(
                    "SELECT value FROM settings WHERE key=?1",
                    [PROJECTION],
                    |r| r.get(0),
                )
                .optional()?;
            let current: Option<ZoneDefinition> =
                current.as_deref().map(serde_json::from_str).transpose()?;
            let mut event = event.clone();
            if let Some(zone) = current {
                let zone = zone
                    .resolve()
                    .map_err(|_| StoreError::Invalid("Invalid calendar time zone.".into()))?;
                tz::reproject_all_day(&mut event, &source_zone, &zone)
                    .map_err(StoreError::Invalid)?;
            }
            upsert_event_tx(tx, &event)
        })
    }
    /// General preference writes share the ordered writer with zone changes, and
    /// preserve the zone committed by that dedicated, atomic operation.
    pub fn save_preferences(&self, prefs: &Prefs) -> Result<()> {
        self.with_tx(|tx| {
            let current: Option<String> = tx.query_row("SELECT value FROM settings WHERE key='prefs'", [], |r| r.get(0)).optional()?;
            let zone = current.and_then(|value| match serde_json::from_str::<Prefs>(&value) {
                Ok(p) => p.time_zone,
                Err(e) => {
                    tracing::warn!(error = %e, "ignoring corrupt prefs JSON when preserving time_zone");
                    None
                }
            });
            let mut prefs = prefs.clone();
            prefs.time_zone = zone;
            tx.execute("INSERT INTO settings(key,value) VALUES('prefs',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value", [serde_json::to_string(&prefs)?])?;
            Ok(())
        })
    }
    /// Reproject date-only Google/sample events atomically with the projection marker.
    /// CalDAV has its own zone-qualified resources and coverage, and resynchronizes them.
    pub fn project_calendar_time_zone(
        &self,
        fallback: &TimeZone,
        next: &ZoneDefinition,
    ) -> Result<usize> {
        self.project_time_zone(fallback, next, None)
    }

    pub fn set_time_zone_preferences(
        &self,
        fallback: &TimeZone,
        next: &ZoneDefinition,
        prefs: &Prefs,
    ) -> Result<usize> {
        self.project_time_zone(fallback, next, Some(prefs))
    }

    fn project_time_zone(
        &self,
        fallback: &TimeZone,
        next: &ZoneDefinition,
        prefs: Option<&Prefs>,
    ) -> Result<usize> {
        let zone = next
            .resolve()
            .map_err(|_| StoreError::Invalid("Invalid calendar time zone.".into()))?;
        self.with_tx(|tx| {
            let previous: Option<String> = tx.query_row("SELECT value FROM settings WHERE key=?1", [PROJECTION], |r| r.get(0)).optional()?;
            let previous: Option<ZoneDefinition> = previous.as_deref().map(serde_json::from_str).transpose()?;
            let old = previous.as_ref().and_then(|p| p.resolve().ok()).unwrap_or_else(|| fallback.clone());
            let mut changed = 0;
            // Upgrade older invitation JSON once, before interpreting its
            // projected midnight in a different zone. New invitations store dates.
            if previous.is_none() || old != zone {
                let invitations: Vec<(String, String)> = {
                    let mut stmt = tx.prepare("SELECT id, invitation_json FROM messages WHERE invitation_json IS NOT NULL")?;
                    let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
                    rows.collect::<std::result::Result<_, _>>()?
                };
                for (id, json) in invitations {
                    let mut invitation = match serde_json::from_str::<overlook_core::ics::Invitation>(&json) {
                        Ok(inv) => inv,
                        Err(e) => {
                            tracing::warn!(message_id = %id, error = %e, "ignoring corrupt invitation_json during time zone projection");
                            continue;
                        }
                    };
                    if invitation.all_day && invitation.start_date.is_none() {
                        if let Some(start) = invitation.start {
                            invitation.start_date = Some(tz::in_zone(start, &old).date().to_string());
                            tx.execute("UPDATE messages SET invitation_json=?2 WHERE id=?1", params![id, serde_json::to_string(&invitation)?])?;
                        }
                    }
                }
            }
            if old != zone {
                for mut event in load_events(tx, "WHERE e.all_day=1 AND e.calendar_id NOT LIKE 'caldav:%'", [])? {
                    tz::reproject_all_day(&mut event, &old, &zone).map_err(StoreError::Invalid)?;
                    upsert_event_tx(tx, &event)?;
                    changed += 1;
                }
            }
            tx.execute("INSERT INTO settings(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value", params![PROJECTION, serde_json::to_string(next)?])?;
            if let Some(prefs) = prefs {
                tx.execute("INSERT INTO settings(key,value) VALUES('prefs',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value", [serde_json::to_string(prefs)?])?;
            }
            Ok(changed)
        })
    }

    /// A download started under another zone must neither overwrite the new projection
    /// nor advance its token. The check and writes share the same transaction.
    pub fn apply_calendar_download(
        &self,
        account: &AccountId,
        projection: &ZoneDefinition,
        events: &[Event],
        deleted: &[EventId],
        token: Option<(&str, &str)>,
    ) -> Result<bool> {
        self.with_tx(|tx| {
            let current: Option<String> = tx.query_row("SELECT value FROM settings WHERE key=?1", [PROJECTION], |r| r.get(0)).optional()?;
            if current.as_deref().map(serde_json::from_str::<ZoneDefinition>).transpose()?.as_ref().is_some_and(|current| current != projection) { return Ok(false); }
            let protected: HashSet<EventId> = {
                let mut stmt = tx.prepare("SELECT kind_json FROM pending_operations WHERE account_id=?1 AND state != 'applied'")?;
                let values = stmt.query_map([&account.0], |r| r.get::<_, String>(0))?;
                let mut ids = HashSet::new();
                for value in values { if let Some(id) = serde_json::from_str::<OperationKind>(&value?)?.event_id() { ids.insert(id.clone()); } }
                ids
            };
            for event in events {
                ensure_same_account(&event.account_id, account, "event")?;
                if !protected.contains(&event.id) { upsert_event_tx(tx, event)?; }
            }
            for id in deleted {
                if !protected.contains(id) { tx.execute("DELETE FROM events WHERE id=?1 AND account_id=?2", params![id.0, account.0])?; }
            }
            if let Some((key, value)) = token {
                tx.execute("INSERT INTO settings(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value", params![key, serde_json::to_string(value)?])?;
            }
            Ok(true)
        })
    }
}

/// One account's local calendar projection, read from a single SQLite snapshot.
pub struct CalendarReadCache {
    pub calendar_ids: Vec<CalendarId>,
    pub events: Vec<Event>,
    pub projection: Option<ZoneDefinition>,
    pub caldav_windows: Vec<(Timestamp, Timestamp)>,
    pub caldav_windows_apply_to_all_selected: bool,
}

impl Store {
    /// Read overlaps on all cached collections, including calendars hidden in the UI.
    /// An optional collection must belong to the account to produce any results.
    pub fn calendar_read_cache(
        &self,
        account: &AccountId,
        calendar: Option<&CalendarId>,
        from: Timestamp,
        to: Timestamp,
        timezone: &str,
    ) -> Result<CalendarReadCache> {
        self.with_read_tx(|tx| {
            let calendar = calendar.map(|id| id.0.as_str());
            let mut stmt = tx.prepare(
                "SELECT c.id, conn.enabled, EXISTS(SELECT 1 FROM caldav_outbox o WHERE o.calendar_id=c.id)
                 FROM calendars c
                 LEFT JOIN caldav_collections col ON col.calendar_id=c.id
                 LEFT JOIN calendar_connections conn ON conn.id=col.connection_id AND conn.account_id=c.account_id
                 WHERE c.account_id=?1 AND (?2 IS NULL OR c.id=?2)
                 ORDER BY c.id",
            )?;
            let mut calendar_ids = Vec::new();
            let mut caldav_windows_apply_to_all_selected = true;
            for row in stmt.query_map(params![account.0, calendar], |r| {
                Ok((CalendarId(r.get(0)?), r.get::<_, Option<bool>>(1)?, r.get::<_, bool>(2)?))
            })? {
                let (id, enabled, pending) = row?;
                calendar_ids.push(id);
                caldav_windows_apply_to_all_selected &= enabled == Some(true) && !pending;
            }
            let mut events = load_events(
                tx,
                "JOIN calendars c ON c.id=e.calendar_id AND c.account_id=e.account_id
                 WHERE e.account_id=?1 AND (?2 IS NULL OR c.id=?2)
                   AND e.start<?4 AND e.end>=e.start AND (e.end>?3 OR e.start>=?3)
                   AND e.status!='cancelled'
                 ORDER BY e.start,e.end,e.id",
                params![account.0, calendar, from, to],
            )?;
            let pending = pending_calendar_events(tx, account)?;
            for event in &mut events {
                event.pending |= pending.contains(&event.id);
            }
            let mut windows = tx.prepare(
                "SELECT v.range_start,v.range_end
                 FROM caldav_coverage v JOIN calendar_connections conn ON conn.id=v.connection_id
                 WHERE conn.account_id=?1 AND v.timezone=?2 AND v.synced_at>0
                   AND v.range_start<?4 AND v.range_end>?3 AND v.range_start<v.range_end
                 ORDER BY v.range_start
                 LIMIT 32",
            )?;
            let caldav_windows = windows
                .query_map(params![account.0, timezone, from, to], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?;
            Ok(CalendarReadCache {
                caldav_windows_apply_to_all_selected: caldav_windows_apply_to_all_selected && !calendar_ids.is_empty(),
                calendar_ids,
                events,
                projection: read_projection(tx)?,
                caldav_windows,
            })
        })
    }

    /// Resolve an opaque ID within an account without materializing unrelated event content.
    /// The predicate must be pure and must not call the Store while its reader is held.
    pub fn find_cached_calendar_event(
        &self,
        account: &AccountId,
        timezone: &str,
        matches: impl Fn(&str) -> bool,
    ) -> Result<(Option<Event>, Option<ZoneDefinition>, bool)> {
        self.with_read_tx(|tx| {
            let mut stmt = tx.prepare(
                "SELECT e.id FROM events e JOIN calendars c ON c.id=e.calendar_id AND c.account_id=e.account_id
                 WHERE e.account_id=?1 AND e.status!='cancelled'",
            )?;
            let mut rows = stmt.query([&account.0])?;
            while let Some(row) = rows.next()? {
                let id: String = row.get(0)?;
                if !matches(&id) {
                    continue;
                }
                let event = load_events(tx, "WHERE e.id=?1 AND e.account_id=?2", params![id, account.0])?.pop();
                if let Some(mut event) = event {
                    event.pending |= pending_calendar_events(tx, account)?.contains(&event.id);
                    let known_caldav_projection = tx.query_row(
                        "SELECT EXISTS(
                            SELECT 1 FROM caldav_coverage v
                            JOIN caldav_collections col ON col.connection_id=v.connection_id
                            WHERE col.calendar_id=?1 AND v.timezone=?2 AND v.synced_at>0
                              AND v.range_start<?4 AND v.range_end>?3 AND v.range_start<v.range_end
                        )",
                        params![event.calendar_id.0, timezone, event.start, event.end.max(event.start.saturating_add(1))],
                        |r| r.get(0),
                    )?;
                    return Ok((Some(event), read_projection(tx)?, known_caldav_projection));
                }
            }
            Ok((None, None, false))
        })
    }
}

fn read_projection(tx: &Connection) -> Result<Option<ZoneDefinition>> {
    let value: Option<String> = tx
        .query_row("SELECT value FROM settings WHERE key=?1", [PROJECTION], |r| r.get(0))
        .optional()?;
    Ok(value.as_deref().map(serde_json::from_str).transpose()?)
}

fn pending_calendar_events(tx: &Connection, account: &AccountId) -> Result<HashSet<EventId>> {
    Ok(pending_operations_tx(tx, account)?
        .iter()
        .filter_map(|op| op.kind.event_id().cloned())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (Store, Account, Calendar, Event) {
        let store = Store::open_in_memory().unwrap();
        let account = Account {
            id: AccountId::from("tz-test"),
            provider: ProviderKind::Gmail,
            display_name: "Test".into(),
            email: "test@example.com".into(),
            created_at: 0,
        };
        store.upsert_account(&account).unwrap();
        let calendar = Calendar {
            id: CalendarId::from("tz-calendar"),
            account_id: account.id.clone(),
            provider_id: None,
            name: "Test".into(),
            color: None,
            is_primary: true,
            can_edit: true,
            visible: true,
            sort_order: 0,
            default_reminders: Vec::new(),
        };
        store
            .replace_calendars(&account.id, std::slice::from_ref(&calendar))
            .unwrap();
        let event = Event {
            id: EventId::from("tz-event"),
            account_id: account.id.clone(),
            calendar_id: calendar.id.clone(),
            provider_id: None,
            ical_uid: None,
            summary: "Three-day event".into(),
            description: None,
            location: None,
            start: tz::parse_rfc3339("2026-10-31T04:00:00Z").unwrap(),
            end: tz::parse_rfc3339("2026-11-03T05:00:00Z").unwrap(),
            all_day: true,
            status: EventStatus::Confirmed,
            organizer: None,
            attendees: Vec::new(),
            html_link: None,
            conference_url: None,
            recurring_event_id: None,
            updated: 0,
            event_type: None,
            color: None,
            reminders: None,
            original_start: Some(tz::parse_rfc3339("2026-11-01T04:00:00Z").unwrap()),
            wants_conference: false,
            pending: false,
        };
        (store, account, calendar, event)
    }

    #[test]
    fn changing_zone_preserves_all_day_dates_and_offline_edits_across_dst() {
        let (store, account, _, event) = fixture();
        let old = ZoneDefinition::Named("America/Toronto".into());
        let next = ZoneDefinition::Named("America/Phoenix".into());
        store
            .project_calendar_time_zone(&old.resolve().unwrap(), &old)
            .unwrap();
        store.upsert_events(std::slice::from_ref(&event)).unwrap();
        store
            .apply_series_change(&account.id, &event, RecurrenceScope::This)
            .unwrap();
        let mut timed = event.clone();
        timed.id = EventId::from("timed");
        timed.all_day = false;
        store.upsert_events(&[timed.clone()]).unwrap();
        let prefs = Prefs {
            time_zone: Some(next.clone()),
            ..Prefs::default()
        };
        assert_eq!(
            store
                .set_time_zone_preferences(&old.resolve().unwrap(), &next, &prefs)
                .unwrap(),
            1
        );
        let projected = store.event(&event.id).unwrap().unwrap();
        assert_eq!(
            tz::in_zone(projected.start, &next.resolve().unwrap())
                .date()
                .to_string(),
            "2026-10-31"
        );
        assert_eq!(
            tz::in_zone(projected.end, &next.resolve().unwrap())
                .date()
                .to_string(),
            "2026-11-03"
        );
        assert_eq!(projected.end - projected.start, 72 * 3_600_000);
        assert_eq!(store.pending_count(&account.id).unwrap(), 1);
        assert_eq!(store.event(&timed.id).unwrap().unwrap().start, timed.start);
        assert_eq!(
            store.setting::<Prefs>("prefs").unwrap().unwrap().time_zone,
            prefs.time_zone
        );
        assert!(!store
            .apply_calendar_download(
                &account.id,
                &old,
                std::slice::from_ref(&event),
                &[],
                Some(("token", "stale"))
            )
            .unwrap());
        assert!(store.setting::<String>("token").unwrap().is_none());
        assert_eq!(
            store.event(&event.id).unwrap().unwrap().start,
            projected.start
        );
        // Even a fresh server snapshot cannot clobber pending local intent.
        assert!(store
            .apply_calendar_download(
                &account.id,
                &next,
                std::slice::from_ref(&event),
                std::slice::from_ref(&event.id),
                None
            )
            .unwrap());
        assert_eq!(
            store.event(&event.id).unwrap().unwrap().start,
            projected.start
        );
    }

    #[test]
    fn projection_survives_restart_and_resuming_a_zone_does_not_shift_dates_again() {
        let dir = tempfile::tempdir().unwrap();
        let database = dir.path().join("time-zones.sqlite");
        let (memory, account, calendar, event) = fixture();
        drop(memory);
        let store = Store::open(&database).unwrap();
        store.upsert_account(&account).unwrap();
        store.replace_calendars(&account.id, &[calendar]).unwrap();
        store.upsert_events(std::slice::from_ref(&event)).unwrap();
        let old = ZoneDefinition::Named("America/Toronto".into());
        let next = ZoneDefinition::Named("Asia/Tokyo".into());
        store
            .project_calendar_time_zone(&old.resolve().unwrap(), &next)
            .unwrap();
        let start = store.event(&event.id).unwrap().unwrap().start;
        drop(store);
        let store = Store::open(&database).unwrap();
        assert_eq!(
            store
                .project_calendar_time_zone(&TimeZone::UTC, &next)
                .unwrap(),
            0
        );
        assert_eq!(store.event(&event.id).unwrap().unwrap().start, start);
        store
            .project_calendar_time_zone(&TimeZone::UTC, &old)
            .unwrap();
        assert_eq!(store.event(&event.id).unwrap().unwrap().start, event.start);
    }

    #[test]
    fn other_preference_saves_cannot_undo_a_timezone_decision() {
        let (store, _, _, _) = fixture();
        let system = ZoneDefinition::Named("America/Toronto".into());
        let manual = ZoneDefinition::Named("Asia/Tokyo".into());
        let kept = Prefs {
            time_zone: Some(manual.clone()),
            ..Prefs::default()
        };
        store
            .set_time_zone_preferences(&TimeZone::UTC, &manual, &kept)
            .unwrap();
        // This UI snapshot was queued while the timezone writer was still running.
        let stale = Prefs {
            clock_24h: true,
            ..Prefs::default()
        };
        store.save_preferences(&stale).unwrap();
        let saved: Prefs = store.setting("prefs").unwrap().unwrap();
        assert!(saved.clock_24h);
        assert_eq!(saved.time_zone, Some(manual));
        // The same rule protects returning to Follow system.
        let follows = Prefs {
            time_zone: None,
            ..saved.clone()
        };
        store
            .set_time_zone_preferences(&TimeZone::UTC, &system, &follows)
            .unwrap();
        store.save_preferences(&saved).unwrap();
        assert_eq!(
            store.setting::<Prefs>("prefs").unwrap().unwrap().time_zone,
            None
        );
        assert_eq!(store.calendar_time_zone().unwrap(), Some(system));
    }

    #[test]
    fn calendar_upload_keeps_its_dates_when_the_zone_changes_in_flight() {
        let (store, _, _, event) = fixture();
        let old = ZoneDefinition::Named("America/Toronto".into());
        let next = ZoneDefinition::Named("America/Phoenix".into());
        store
            .project_calendar_time_zone(&old.resolve().unwrap(), &old)
            .unwrap();
        store.upsert_events(std::slice::from_ref(&event)).unwrap();
        let (saved, source) = store.calendar_event_for_replay(&event.id, &next).unwrap();
        let mut response = saved.unwrap();
        assert_eq!(source, old);
        // The user accepts the new zone while Google is processing the request.
        store
            .project_calendar_time_zone(&TimeZone::UTC, &next)
            .unwrap();
        let projected = store.event(&event.id).unwrap().unwrap();
        response.html_link = Some("https://calendar.google.com/returned".into());
        store.finish_calendar_write(&response, &source).unwrap();
        let merged = store.event(&event.id).unwrap().unwrap();
        assert_eq!(
            (merged.start, merged.end, merged.original_start),
            (projected.start, projected.end, projected.original_start)
        );
        assert_eq!(merged.html_link, response.html_link);
    }

    #[test]
    fn legacy_all_day_invitations_keep_their_date_after_a_zone_change() {
        let (store, account, _, event) = fixture();
        let old = ZoneDefinition::Named("America/Toronto".into());
        let next = ZoneDefinition::Named("America/Phoenix".into());
        let legacy = serde_json::json!({"uid":"old", "method":"REQUEST", "start":event.start, "all_day":true});
        store.with_tx(|tx| {
            tx.execute("INSERT INTO messages(id,account_id,conversation_id,from_json,to_json,cc_json,bcc_json,subject,snippet,date,invitation_json) VALUES('invite',?1,'thread','{}','[]','[]','[]','','',0,?2)", params![account.id.0, legacy.to_string()])?;
            Ok(())
        }).unwrap();
        store
            .project_calendar_time_zone(&old.resolve().unwrap(), &next)
            .unwrap();
        let json: String = store
            .with_read(|c| {
                Ok(c.query_row(
                    "SELECT invitation_json FROM messages WHERE id='invite'",
                    [],
                    |r| r.get(0),
                )?)
            })
            .unwrap();
        let inv: overlook_core::ics::Invitation = serde_json::from_str(&json).unwrap();
        assert_eq!(inv.start_date.as_deref(), Some("2026-10-31"));
        assert_eq!(
            tz::in_zone(
                inv.start_in_zone(&next.resolve().unwrap()).unwrap(),
                &next.resolve().unwrap()
            )
            .date()
            .to_string(),
            "2026-10-31"
        );
    }
}
