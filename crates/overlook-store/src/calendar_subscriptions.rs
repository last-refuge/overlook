//! Durable storage for read-only subscribed iCalendar (`.ics` / `webcal://`) feeds
//! and their expanded local event projections.

use super::*;

const SUB_COLUMNS: &str =
    "id, url, name, color, visible, sort_order, etag, last_modified, expanded_from, expanded_to, expanded_tz, last_synced_at, last_error, created_at";

fn subscription_from_row(r: &Row<'_>) -> rusqlite::Result<CalendarSubscription> {
    Ok(CalendarSubscription {
        id: CalendarSubscriptionId(r.get(0)?),
        url: r.get(1)?,
        name: r.get(2)?,
        color: r.get(3)?,
        visible: r.get(4)?,
        sort_order: r.get(5)?,
        etag: r.get(6)?,
        last_modified: r.get(7)?,
        expanded_from: r.get(8)?,
        expanded_to: r.get(9)?,
        expanded_tz: r.get(10)?,
        last_synced_at: r.get(11)?,
        last_error: r.get(12)?,
        created_at: r.get(13)?,
    })
}

fn normalize_name(name: &str) -> Result<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(StoreError::Invalid("Enter a calendar name.".into()));
    }
    Ok(trimmed.to_owned())
}

fn validate_optional_color(color: Option<&str>) -> Result<Option<String>> {
    match color {
        None => Ok(None),
        Some(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return Ok(None);
            }
            normalize_hex_color(trimmed)
                .map(Some)
                .ok_or_else(|| StoreError::Invalid("Choose a valid calendar color.".into()))
        }
    }
}

fn replace_subscription_events_tx(
    tx: &Connection,
    subscription_id: &CalendarSubscriptionId,
    events: &[Event],
) -> Result<()> {
    tx.execute(
        "DELETE FROM subscription_events WHERE subscription_id=?1",
        [&subscription_id.0],
    )?;
    let expected_calendar = CalendarId(format!("{SUBSCRIPTION_CALENDAR_PREFIX}{subscription_id}"));
    let expected_account = CalendarSubscription::account_id();
    let mut stmt = tx.prepare_cached(
        "INSERT OR REPLACE INTO subscription_events(
            id, subscription_id, ical_uid, summary, description, location, start, end, all_day,
            status, organizer_json, attendees_json, html_link, conference_url, recurring_event_id,
            original_start, reminders_json, updated
         ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18)",
    )?;
    for e in events {
        if e.account_id != expected_account || e.calendar_id != expected_calendar {
            return Err(StoreError::Invalid(
                "Subscribed event identity mismatch.".into(),
            ));
        }
        stmt.execute(params![
            e.id.0,
            subscription_id.0,
            e.ical_uid,
            e.summary,
            e.description,
            e.location,
            e.start,
            e.end,
            e.all_day,
            serde_json::to_string(&e.status)?.trim_matches('"').to_owned(),
            e.organizer.as_ref().map(serde_json::to_string).transpose()?,
            serde_json::to_string(&e.attendees)?,
            e.html_link,
            e.conference_url,
            e.recurring_event_id,
            e.original_start,
            e.reminders.as_ref().map(serde_json::to_string).transpose()?,
            e.updated,
        ])?;
    }
    Ok(())
}

fn load_subscription_events(
    c: &Connection,
    clause: &str,
    args: impl rusqlite::Params,
) -> Result<Vec<Event>> {
    let sql = format!(
        "SELECT e.id, e.subscription_id, e.ical_uid, e.summary, e.description, e.location,
                e.start, e.end, e.all_day, e.status, e.organizer_json, e.attendees_json,
                e.html_link, e.conference_url, e.recurring_event_id, e.original_start,
                e.reminders_json, e.updated
         FROM subscription_events e {clause}"
    );
    let mut stmt = c.prepare(&sql)?;
    let rows = stmt.query_map(args, |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<String>>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, Option<String>>(4)?,
            r.get::<_, Option<String>>(5)?,
            r.get::<_, i64>(6)?,
            r.get::<_, i64>(7)?,
            r.get::<_, bool>(8)?,
            r.get::<_, String>(9)?,
            r.get::<_, Option<String>>(10)?,
            r.get::<_, String>(11)?,
            r.get::<_, Option<String>>(12)?,
            r.get::<_, Option<String>>(13)?,
            r.get::<_, Option<String>>(14)?,
            r.get::<_, Option<i64>>(15)?,
            r.get::<_, Option<String>>(16)?,
            r.get::<_, i64>(17)?,
        ))
    })?;
    let account_id = CalendarSubscription::account_id();
    let mut out = Vec::new();
    for row in rows {
        let (
            id,
            sub_id,
            ical_uid,
            summary,
            description,
            location,
            start,
            end,
            all_day,
            status,
            organizer,
            attendees,
            html_link,
            conference_url,
            recurring_event_id,
            original_start,
            reminders,
            updated,
        ) = row?;
        out.push(Event {
            id: EventId(id),
            account_id: account_id.clone(),
            calendar_id: CalendarId(format!("{SUBSCRIPTION_CALENDAR_PREFIX}{sub_id}")),
            provider_id: None,
            ical_uid,
            summary,
            description,
            location,
            start,
            end,
            all_day,
            status: serde_json::from_str(&format!("\"{status}\"")).unwrap_or_default(),
            organizer: organizer.as_deref().map(serde_json::from_str).transpose()?,
            attendees: serde_json::from_str(&attendees)?,
            html_link,
            conference_url,
            recurring_event_id,
            updated,
            event_type: None,
            color: None,
            reminders: reminders.as_deref().map(serde_json::from_str).transpose()?,
            original_start,
            wants_conference: false,
            pending: false,
        });
    }
    Ok(out)
}

impl Store {
    pub fn calendar_subscriptions(&self) -> Result<Vec<CalendarSubscription>> {
        self.with_read(|c| {
            let sql = format!(
                "SELECT {SUB_COLUMNS} FROM calendar_subscriptions ORDER BY sort_order, LOWER(name), created_at"
            );
            let mut stmt = c.prepare(&sql)?;
            let rows = stmt.query_map([], subscription_from_row)?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
    }

    pub fn calendar_subscription(
        &self,
        id: &CalendarSubscriptionId,
    ) -> Result<Option<CalendarSubscription>> {
        self.with_read(|c| {
            let sql = format!("SELECT {SUB_COLUMNS} FROM calendar_subscriptions WHERE id=?1");
            Ok(c.query_row(&sql, [&id.0], subscription_from_row).optional()?)
        })
    }

    pub fn calendar_subscription_raw_ics(
        &self,
        id: &CalendarSubscriptionId,
    ) -> Result<Option<String>> {
        self.with_read(|c| {
            let raw: Option<Option<String>> = c
                .query_row(
                    "SELECT raw_ics FROM calendar_subscriptions WHERE id=?1",
                    [&id.0],
                    |r| r.get(0),
                )
                .optional()?;
            Ok(raw.flatten())
        })
    }

    pub fn create_calendar_subscription(
        &self,
        sub: &CalendarSubscription,
        raw_ics: &str,
        events: &[Event],
    ) -> Result<()> {
        let url = normalize_subscription_url(&sub.url).map_err(|e| StoreError::Invalid(e.into()))?;
        let name = normalize_name(&sub.name)?;
        let color = validate_optional_color(sub.color.as_deref())?;
        self.with_tx(|tx| {
            tx.execute(
                "INSERT INTO calendar_subscriptions(
                    id, url, name, color, visible, sort_order, etag, last_modified, raw_ics,
                    expanded_from, expanded_to, expanded_tz, last_synced_at, last_error, created_at
                 ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
                params![
                    sub.id.0,
                    url,
                    name,
                    color,
                    sub.visible,
                    sub.sort_order,
                    sub.etag,
                    sub.last_modified,
                    raw_ics,
                    sub.expanded_from,
                    sub.expanded_to,
                    sub.expanded_tz,
                    sub.last_synced_at,
                    sub.last_error,
                    sub.created_at,
                ],
            )?;
            replace_subscription_events_tx(tx, &sub.id, events)
        })
    }

    pub fn update_calendar_subscription_meta(
        &self,
        id: &CalendarSubscriptionId,
        name: &str,
        color: Option<&str>,
    ) -> Result<()> {
        let name = normalize_name(name)?;
        let color = validate_optional_color(color)?;
        self.with_tx(|tx| {
            let changed = tx.execute(
                "UPDATE calendar_subscriptions SET name=?2, color=?3 WHERE id=?1",
                params![id.0, name, color],
            )?;
            if changed == 0 {
                return Err(StoreError::NotFound(format!("calendar subscription {id}")));
            }
            Ok(())
        })
    }

    pub fn set_calendar_subscription_visible(
        &self,
        id: &CalendarSubscriptionId,
        visible: bool,
    ) -> Result<()> {
        self.with_tx(|tx| {
            let changed = tx.execute(
                "UPDATE calendar_subscriptions SET visible=?2 WHERE id=?1",
                params![id.0, visible],
            )?;
            if changed == 0 {
                return Err(StoreError::NotFound(format!("calendar subscription {id}")));
            }
            Ok(())
        })
    }

    pub fn set_calendar_subscription_color(
        &self,
        id: &CalendarSubscriptionId,
        color: Option<&str>,
    ) -> Result<()> {
        let color = validate_optional_color(color)?;
        self.with_tx(|tx| {
            let changed = tx.execute(
                "UPDATE calendar_subscriptions SET color=?2 WHERE id=?1",
                params![id.0, color],
            )?;
            if changed == 0 {
                return Err(StoreError::NotFound(format!("calendar subscription {id}")));
            }
            Ok(())
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn replace_calendar_subscription_url(
        &self,
        id: &CalendarSubscriptionId,
        url: &str,
        name: &str,
        color: Option<&str>,
        etag: Option<&str>,
        last_modified: Option<&str>,
        raw_ics: &str,
        expanded_from: Timestamp,
        expanded_to: Timestamp,
        expanded_tz: &str,
        synced_at: Timestamp,
        events: &[Event],
    ) -> Result<()> {
        let url = normalize_subscription_url(url).map_err(|e| StoreError::Invalid(e.into()))?;
        let name = normalize_name(name)?;
        let color = validate_optional_color(color)?;
        self.with_tx(|tx| {
            let changed = tx.execute(
                "UPDATE calendar_subscriptions
                 SET url=?2, name=?3, color=?4, etag=?5, last_modified=?6, raw_ics=?7,
                     expanded_from=?8, expanded_to=?9, expanded_tz=?10, last_synced_at=?11,
                     last_error=NULL
                 WHERE id=?1",
                params![
                    id.0,
                    url,
                    name,
                    color,
                    etag,
                    last_modified,
                    raw_ics,
                    expanded_from,
                    expanded_to,
                    expanded_tz,
                    synced_at,
                ],
            )?;
            if changed == 0 {
                return Err(StoreError::NotFound(format!("calendar subscription {id}")));
            }
            replace_subscription_events_tx(tx, id, events)
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn apply_subscription_fetch(
        &self,
        id: &CalendarSubscriptionId,
        expected_url: &str,
        etag: Option<&str>,
        last_modified: Option<&str>,
        raw_ics: &str,
        expanded_from: Timestamp,
        expanded_to: Timestamp,
        expanded_tz: &str,
        synced_at: Timestamp,
        events: &[Event],
    ) -> Result<bool> {
        self.with_tx(|tx| {
            let changed = tx.execute(
                "UPDATE calendar_subscriptions
                 SET etag=?3, last_modified=?4, raw_ics=?5, expanded_from=?6, expanded_to=?7,
                     expanded_tz=?8, last_synced_at=?9, last_error=NULL
                 WHERE id=?1 AND url=?2",
                params![
                    id.0,
                    expected_url,
                    etag,
                    last_modified,
                    raw_ics,
                    expanded_from,
                    expanded_to,
                    expanded_tz,
                    synced_at,
                ],
            )?;
            if changed == 0 {
                return Ok(false);
            }
            replace_subscription_events_tx(tx, id, events)?;
            Ok(true)
        })
    }

    pub fn apply_subscription_not_modified(
        &self,
        id: &CalendarSubscriptionId,
        expected_url: &str,
        synced_at: Timestamp,
    ) -> Result<bool> {
        self.with_tx(|tx| {
            let changed = tx.execute(
                "UPDATE calendar_subscriptions
                 SET last_synced_at=?3, last_error=NULL
                 WHERE id=?1 AND url=?2",
                params![id.0, expected_url, synced_at],
            )?;
            Ok(changed > 0)
        })
    }

    pub fn apply_subscription_reexpansion(
        &self,
        id: &CalendarSubscriptionId,
        expected_synced_at: Option<Timestamp>,
        expanded_from: Timestamp,
        expanded_to: Timestamp,
        expanded_tz: &str,
        events: &[Event],
    ) -> Result<bool> {
        self.with_tx(|tx| {
            let changed = tx.execute(
                "UPDATE calendar_subscriptions
                 SET expanded_from=?3, expanded_to=?4, expanded_tz=?5
                 WHERE id=?1 AND last_synced_at IS ?2",
                params![id.0, expected_synced_at, expanded_from, expanded_to, expanded_tz],
            )?;
            if changed == 0 {
                return Ok(false);
            }
            replace_subscription_events_tx(tx, id, events)?;
            Ok(true)
        })
    }

    pub fn set_calendar_subscription_error(
        &self,
        id: &CalendarSubscriptionId,
        expected_url: &str,
        error: &str,
    ) -> Result<()> {
        self.with_tx(|tx| {
            tx.execute(
                "UPDATE calendar_subscriptions SET last_error=?3 WHERE id=?1 AND url=?2",
                params![id.0, expected_url, error],
            )?;
            Ok(())
        })
    }

    pub fn delete_calendar_subscription(&self, id: &CalendarSubscriptionId) -> Result<()> {
        self.with_tx(|tx| {
            tx.execute("DELETE FROM calendar_subscriptions WHERE id=?1", [&id.0])?;
            Ok(())
        })
    }

    pub fn subscription_events_between(
        &self,
        from: Timestamp,
        to: Timestamp,
    ) -> Result<Vec<Event>> {
        self.with_read(|c| {
            load_subscription_events(
                c,
                "JOIN calendar_subscriptions s ON s.id = e.subscription_id
                 WHERE s.visible = 1 AND e.start < ?2 AND e.end > ?1 AND e.status != 'cancelled'
                 ORDER BY e.start, e.end, e.id",
                params![from, to],
            )
        })
    }

    pub fn subscription_events_starting_between(
        &self,
        from: Timestamp,
        to: Timestamp,
    ) -> Result<Vec<Event>> {
        self.with_read(|c| {
            load_subscription_events(
                c,
                "JOIN calendar_subscriptions s ON s.id = e.subscription_id
                 WHERE s.visible = 1 AND e.start >= ?1 AND e.start < ?2
                   AND e.status != 'cancelled' AND e.all_day = 0
                 ORDER BY e.start, e.end, e.id",
                params![from, to],
            )
        })
    }

    pub fn subscription_event(&self, id: &EventId) -> Result<Option<Event>> {
        self.with_read(|c| Ok(load_subscription_events(c, "WHERE e.id=?1", params![id.0])?.pop()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sub(id: &str, url: &str, name: &str) -> CalendarSubscription {
        CalendarSubscription {
            id: CalendarSubscriptionId::from(id),
            url: url.into(),
            name: name.into(),
            color: Some("#3B82F6".into()),
            visible: true,
            sort_order: 0,
            etag: Some("\"v1\"".into()),
            last_modified: None,
            expanded_from: Some(0),
            expanded_to: Some(10_000),
            expanded_tz: Some("America/Denver".into()),
            last_synced_at: Some(500),
            last_error: None,
            created_at: 100,
        }
    }

    fn sub_event(sub_id: &str, event_id: &str, summary: &str, start: i64, end: i64) -> Event {
        Event {
            id: EventId::from(event_id),
            account_id: CalendarSubscription::account_id(),
            calendar_id: CalendarId(format!("{SUBSCRIPTION_CALENDAR_PREFIX}{sub_id}")),
            provider_id: None,
            ical_uid: Some(format!("uid-{event_id}")),
            summary: summary.into(),
            description: None,
            location: Some("Zoom".into()),
            start,
            end,
            all_day: false,
            status: EventStatus::Confirmed,
            organizer: None,
            attendees: Vec::new(),
            html_link: Some("https://example.com/ev".into()),
            conference_url: None,
            recurring_event_id: None,
            updated: 500,
            event_type: None,
            color: None,
            reminders: Some(vec![15]),
            original_start: None,
            wants_conference: false,
            pending: false,
        }
    }

    #[test]
    fn subscription_persists_across_reopen_visibility_color_and_cascade_delete() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("subs.sqlite");
        let s = sub("apple-1", "webcal://p12-caldav.icloud.com/published/2/token", "Family");
        let ev = sub_event("apple-1", "sub:apple-1:ev1", "Soccer", 1_000, 2_000);
        {
            let store = Store::open(&path).unwrap();
            store
                .create_calendar_subscription(&s, "BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n", &[ev.clone()])
                .unwrap();
        }
        let store = Store::open(&path).unwrap();
        let list = store.calendar_subscriptions().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(
            list[0].url,
            "https://p12-caldav.icloud.com/published/2/token"
        );
        assert_eq!(
            store.calendar_subscription_raw_ics(&s.id).unwrap().as_deref(),
            Some("BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n")
        );
        assert_eq!(store.subscription_events_between(0, 5_000).unwrap(), vec![ev.clone()]);
        assert_eq!(store.event(&ev.id).unwrap(), Some(ev.clone()));

        store.set_calendar_subscription_visible(&s.id, false).unwrap();
        assert!(store.subscription_events_between(0, 5_000).unwrap().is_empty());
        assert!(store
            .subscription_events_starting_between(0, 5_000)
            .unwrap()
            .is_empty());

        store.set_calendar_subscription_visible(&s.id, true).unwrap();
        store
            .set_calendar_subscription_color(&s.id, Some("#10B981"))
            .unwrap();
        assert_eq!(
            store.calendar_subscription(&s.id).unwrap().unwrap().color.as_deref(),
            Some("#10B981")
        );
        store.set_calendar_subscription_color(&s.id, None).unwrap();
        assert_eq!(store.calendar_subscription(&s.id).unwrap().unwrap().color, None);

        store.delete_calendar_subscription(&s.id).unwrap();
        assert!(store.calendar_subscriptions().unwrap().is_empty());
        assert!(store.event(&ev.id).unwrap().is_none());
    }

    #[test]
    fn fetch_and_reexpansion_respect_cas_guards_and_errors_keep_cache() {
        let store = Store::open_in_memory().unwrap();
        let s = sub("s1", "https://example.com/one.ics", "One");
        let ev1 = sub_event("s1", "sub:s1:e1", "Cached", 1_000, 2_000);
        store.create_calendar_subscription(&s, "RAW1", &[ev1.clone()]).unwrap();

        let stored = store.calendar_subscription(&s.id).unwrap().unwrap();
        store
            .set_calendar_subscription_error(&s.id, &stored.url, "Offline")
            .unwrap();
        assert_eq!(
            store.calendar_subscription(&s.id).unwrap().unwrap().last_error.as_deref(),
            Some("Offline")
        );
        assert_eq!(store.subscription_events_between(0, 5_000).unwrap(), vec![ev1.clone()]);

        // Changing the URL supersedes an in-flight fetch for the old URL.
        let ev2 = sub_event("s1", "sub:s1:e2", "New URL", 2_000, 3_000);
        store
            .replace_calendar_subscription_url(
                &s.id,
                "https://example.com/two.ics",
                "Two",
                Some("#F59E0B"),
                Some("\"v2\""),
                None,
                "RAW2",
                0,
                20_000,
                "UTC",
                900,
                &[ev2.clone()],
            )
            .unwrap();

        let applied = store
            .apply_subscription_fetch(
                &s.id,
                "https://example.com/one.ics",
                Some("\"stale\""),
                None,
                "STALE",
                0,
                10_000,
                "UTC",
                950,
                &[ev1.clone()],
            )
            .unwrap();
        assert!(!applied);
        assert_eq!(store.subscription_events_between(0, 5_000).unwrap(), vec![ev2.clone()]);

        // Stale synced_at prevents an older offline re-expansion from overwriting newer data.
        let reexpanded_stale = store
            .apply_subscription_reexpansion(&s.id, Some(500), 0, 30_000, "Europe/London", &[ev1])
            .unwrap();
        assert!(!reexpanded_stale);
        assert_eq!(store.subscription_events_between(0, 5_000).unwrap(), vec![ev2.clone()]);

        let ev3 = sub_event("s1", "sub:s1:e3", "Reexpanded", 3_000, 4_000);
        let reexpanded = store
            .apply_subscription_reexpansion(&s.id, Some(900), 0, 30_000, "Europe/London", &[ev3.clone()])
            .unwrap();
        assert!(reexpanded);
        assert_eq!(store.subscription_events_between(0, 5_000).unwrap(), vec![ev3]);
    }
}
