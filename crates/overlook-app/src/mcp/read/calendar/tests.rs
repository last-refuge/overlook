use super::*;
use overlook_core::{
    AccountId, CalDavCalendarSnapshot, CalDavResource, CalDavSettings, CalDavSnapshot,
    CalendarCredentialSource, CalendarId, EventId,
};

#[test]
fn range_uses_explicit_offsets_and_rejects_unbounded_requests() {
    let w = Window::parse("2026-11-01T01:00:00-04:00", "2026-11-01T01:00:00-05:00").unwrap();
    assert_eq!(w.end - w.start, 3_600_000);
    for (start, end) in [
        ("2026-11-01", "2026-11-02T00:00:00Z"),
        ("2026-11-01T00:00:00", "2026-11-02T00:00:00Z"),
        ("2026-11-01T00:00:00Z[UTC]", "2026-11-02T00:00:00Z"),
        ("2026-11-01T00:00:00.0001Z", "2026-11-02T00:00:00Z"),
        ("2026-11-01T00:00:00Z", "2026-11-01T00:00:00Z"),
        ("2026-11-01T00:00:00Z", "2028-01-01T00:00:00Z"),
    ] {
        assert_eq!(
            Window::parse(start, end).unwrap_err().code,
            ErrorCode::InvalidRequest
        );
    }
}

#[test]
fn availability_coalesces_overlaps_without_claiming_uncovered_gaps() {
    let range = Window::new(10, 90);
    let covered = vec![Window::new(30, 70)];
    let busy = vec![
        Window::new(20, 35),
        Window::new(32, 40),
        Window::new(60, 80),
    ];
    assert_eq!(
        gaps(range, &covered),
        vec![Window::new(10, 30), Window::new(70, 90)]
    );
    let slots = availability(range, &covered, busy);
    let expected = [
        (10, 20, "unknown"),
        (20, 40, "busy"),
        (40, 60, "free_in_cache"),
        (60, 80, "busy"),
        (80, 90, "unknown"),
    ];
    assert_eq!(
        slots,
        expected.map(|(start, end, status)| json!({
            "start": tz::to_rfc3339(start), "end": tz::to_rfc3339(end), "status": status,
        }))
    );
    assert_eq!(availability(range, &[], vec![])[0]["status"], "unknown");
}

#[test]
fn caldav_snapshot_covers_hidden_collections_and_reads_only_owned_events() {
    let store = Store::open_in_memory().unwrap();
    let account = account("non-google", ProviderKind::Imap);
    store.upsert_account(&account).unwrap();
    let connection = store
        .save_calendar_connection(
            &account.id,
            &CalDavSettings {
                server_url: "https://caldav.example/".to_owned(),
                username: "person@example.com".to_owned(),
                credential_source: CalendarCredentialSource::Separate,
            },
            true,
            None,
        )
        .unwrap();
    let calendar = Calendar {
        id: CalendarId::from(format!("caldav:{}:hidden", connection.id)),
        account_id: account.id.clone(),
        provider_id: None,
        name: "Hidden".to_owned(),
        color: None,
        is_primary: true,
        can_edit: false,
        visible: false,
        sort_order: 0,
        default_reminders: vec![],
    };
    let (_zone, key) = tz::zone_with_cache_key();
    let mut event = sample_events(&account, now()).remove(0);
    event.id = EventId::from("hidden-event");
    event.calendar_id = calendar.id.clone();
    event.start = 100;
    event.end = 200;
    store
        .apply_caldav_snapshot(
            &connection,
            &CalDavSnapshot {
                from: 50,
                to: 300,
                timezone: key.clone(),
                calendars: vec![CalDavCalendarSnapshot {
                    calendar: calendar.clone(),
                    resources: vec![CalDavResource {
                        href: "/event.ics".to_owned(),
                        etag: "1".to_owned(),
                        icalendar: "private".to_owned(),
                        events: vec![event.clone()],
                    }],
                }],
            },
        )
        .unwrap();

    let cached = store
        .calendar_read_cache(&account.id, None, 150, 400, &key)
        .unwrap();
    assert_eq!(cached.calendar_ids, vec![calendar.id.clone()]);
    assert!(cached.caldav_windows_apply_to_all_selected);
    assert_eq!(cached.caldav_windows, vec![(50, 300)]);
    assert_eq!(cached.events, vec![event.clone()]);
    assert!(store
        .calendar_read_cache(&account.id, None, 200, 300, &key)
        .unwrap()
        .events
        .is_empty());
    assert!(store
        .calendar_read_cache(&account.id, None, 150, 300, "other zone")
        .unwrap()
        .caldav_windows
        .is_empty());
    let (found, _, zone_verified) = store
        .find_cached_calendar_event(&account.id, &key, |id| id == event.id.0)
        .unwrap();
    assert!(zone_verified);
    assert_eq!(found, Some(event));

    let other = AccountId::from("different-account");
    let unrelated = store
        .calendar_read_cache(&other, Some(&calendar.id), 50, 300, &key)
        .unwrap();
    assert!(unrelated.calendar_ids.is_empty());
    assert!(unrelated.events.is_empty());
    assert!(unrelated.caldav_windows.is_empty());
    assert!(store
        .find_cached_calendar_event(&other, &key, |_| true)
        .unwrap()
        .0
        .is_none());
}

#[test]
fn sample_accounts_get_distinct_calendars_and_bounded_coverage() {
    let store = Store::open_in_memory().unwrap();
    let work = account("sample", ProviderKind::Sample);
    let personal = account("sample-secondary", ProviderKind::Sample);
    store.upsert_account(&work).unwrap();
    store.upsert_account(&personal).unwrap();
    seed_sample(&store, &work).unwrap();
    seed_sample(&store, &personal).unwrap();
    let (zone, key) = tz::zone_with_cache_key();
    let mut calendars = Vec::new();
    for (account, name) in [(&work, "Work Calendar"), (&personal, "Personal Calendar")] {
        let calendar = store.calendars(&account.id).unwrap().remove(0);
        assert_eq!(calendar.name, name);
        let w = store
            .setting::<Window>(&sample_key(&calendar.id))
            .unwrap()
            .unwrap();
        let cached = store
            .calendar_read_cache(&account.id, None, w.start, w.end, &key)
            .unwrap();
        assert!(!cached.events.is_empty());
        assert!(cached.events.iter().all(|e| e.account_id == account.id));
        if account.id == work.id {
            let occurrences: Vec<_> = cached
                .events
                .iter()
                .filter(|e| e.is_recurring_instance())
                .collect();
            assert_eq!(occurrences.len(), 2);
            assert_ne!(occurrences[0].id, occurrences[1].id);
            assert_eq!(
                occurrences[0].recurring_event_id,
                occurrences[1].recurring_event_id
            );
            assert_eq!(occurrences[1].original_start, Some(occurrences[1].start));
        }
        assert_eq!(
            tz::in_zone(w.start, &zone)
                .date()
                .weekday()
                .to_monday_zero_offset(),
            0
        );
        assert_eq!(w.end, tz::add_days(w.start, 14));
        calendars.push(calendar.id);
    }
    assert_ne!(calendars[0], calendars[1]);
}

fn account(id: &str, provider: ProviderKind) -> Account {
    Account {
        id: AccountId::from(id),
        provider,
        display_name: "Test".to_owned(),
        email: format!("{id}@example.com"),
        created_at: 1,
    }
}
