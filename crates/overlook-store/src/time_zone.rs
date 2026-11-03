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
        self.with_tx(|tx| {
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
            let zone = current.and_then(|value| serde_json::from_str::<Prefs>(&value).ok()).and_then(|p| p.time_zone);
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
                    let Ok(mut invitation) = serde_json::from_str::<overlook_core::ics::Invitation>(&json) else { continue };
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
            tx.execute("INSERT INTO messages(id,account_id,conversation_id,from_json,to_json,cc_json,bcc_json,subject,snippet,search_text,date,invitation_json) VALUES('invite',?1,'thread','{}','[]','[]','[]','','','',0,?2)", params![account.id.0, legacy.to_string()])?;
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
