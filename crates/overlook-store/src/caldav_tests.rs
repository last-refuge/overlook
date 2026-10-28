use super::*;

fn account() -> Account {
    Account {
        id: AccountId::from("imap:fixture"),
        provider: ProviderKind::Imap,
        display_name: "Fixture".into(),
        email: "fixture@example.test".into(),
        created_at: 0,
    }
}
fn connection() -> CalendarConnection {
    CalendarConnection {
        id: CalendarConnectionId::generate(),
        account_id: account().id,
        settings: CalDavSettings {
            server_url: "https://caldav.fastmail.com/".into(),
            username: "fixture@example.test".into(),
            credential_source: CalendarCredentialSource::Separate,
        },
        enabled: true,
        generation: 1,
    }
}
fn snapshot(
    connection: &CalendarConnection,
    from: i64,
    to: i64,
    event_at: Option<i64>,
) -> CalDavSnapshot {
    let calendar = Calendar {
        id: CalendarId(format!("caldav:{}:calendar", connection.id)),
        account_id: connection.account_id.clone(),
        provider_id: Some(ProviderId::from("https://caldav.fastmail.com/calendar/")),
        name: "Personal".into(),
        color: Some("#abcd12".into()),
        is_primary: true,
        can_edit: false,
        visible: true,
        sort_order: 0,
        default_reminders: vec![],
    };
    let events = event_at
        .map(|at| Event {
            id: EventId(format!("{}:{at}", calendar.id)),
            account_id: connection.account_id.clone(),
            calendar_id: calendar.id.clone(),
            provider_id: Some(ProviderId(at.to_string())),
            ical_uid: Some("recurring".into()),
            summary: "Original title".into(),
            description: None,
            location: None,
            start: at,
            end: at + 10,
            all_day: false,
            status: EventStatus::Confirmed,
            organizer: None,
            attendees: vec![],
            html_link: None,
            conference_url: None,
            recurring_event_id: Some("recurring".into()),
            updated: 1,
            event_type: None,
            color: None,
            reminders: Some(vec![]),
            original_start: Some(at),
            wants_conference: false,
            pending: false,
        })
        .into_iter()
        .collect();
    CalDavSnapshot {
        from,
        to,
        timezone: "test-zone".into(),
        calendars: vec![CalDavCalendarSnapshot {
            calendar,
            resources: vec![CalDavResource {
                href: "https://caldav.fastmail.com/calendar/event.ics".into(),
                etag: "\"one\"".into(),
                icalendar: "BEGIN:VCALENDAR\nX-UNKNOWN:retain exactly\nEND:VCALENDAR\n".into(),
                events,
            }],
        }],
    }
}
fn install(store: &Store, connection: &CalendarConnection, snapshot: &CalDavSnapshot) {
    store.upsert_account(&account()).unwrap();
    store
        .reserve_caldav_setup(&connection.account_id, &connection.id)
        .unwrap();
    store
        .install_caldav_connection(connection, None, snapshot)
        .unwrap();
}

#[test]
fn cache_resources_visibility_and_coverage_survive_restart() {
    let path = std::env::temp_dir().join(format!(
        "overlook-caldav-cache-{}.sqlite",
        CalendarConnectionId::generate()
    ));
    let c = connection();
    let s = snapshot(&c, 0, 100, Some(20));
    {
        let store = Store::open(&path).unwrap();
        install(&store, &c, &s);
        store
            .set_calendar_visible(&c.account_id, &s.calendars[0].calendar.id, false)
            .unwrap();
    }
    {
        let store = Store::open(&path).unwrap();
        assert_eq!(
            store.calendar_connection(&c.account_id).unwrap(),
            Some(c.clone())
        );
        assert!(store
            .caldav_range_cached(&c.id, 1, 99, "test-zone")
            .unwrap());
        assert!(!store
            .caldav_range_cached(&c.id, 1, 101, "test-zone")
            .unwrap());
        assert!(!store.caldav_range_cached(&c.id, 1, 99, "new-zone").unwrap());
        assert!(store.caldav_last_sync(&c.id).unwrap().is_some());
        assert_eq!(
            store
                .event(&s.calendars[0].resources[0].events[0].id)
                .unwrap()
                .unwrap()
                .summary,
            "Original title"
        );
        store.apply_caldav_snapshot(&c, &s).unwrap();
        assert!(!store.calendars(&c.account_id).unwrap()[0].visible);
        store
            .with_read(|tx| {
                let original: String =
                    tx.query_row("SELECT icalendar FROM caldav_resources", [], |r| r.get(0))?;
                assert_eq!(original, s.calendars[0].resources[0].icalendar);
                Ok(())
            })
            .unwrap();
    }
    std::fs::remove_file(path).unwrap();
}

#[test]
fn remote_deletion_only_replaces_queried_range_and_changed_master_invalidates_other_projections() {
    let store = Store::open_in_memory().unwrap();
    let c = connection();
    let first = snapshot(&c, 0, 100, Some(20));
    let second = snapshot(&c, 200, 300, Some(220));
    install(&store, &c, &first);
    store.apply_caldav_snapshot(&c, &second).unwrap();
    let old_id = &first.calendars[0].resources[0].events[0].id;
    let newer_id = &second.calendars[0].resources[0].events[0].id;
    let mut empty = snapshot(&c, 0, 100, None);
    empty.calendars[0].resources.clear();
    store.apply_caldav_snapshot(&c, &empty).unwrap();
    assert!(store.event(old_id).unwrap().is_none());
    assert!(store.event(newer_id).unwrap().is_some());
    let mut changed = first.clone();
    changed.calendars[0].resources[0].etag = "\"two\"".into();
    changed.calendars[0].resources[0].events[0].summary = "Remote edit".into();
    store.apply_caldav_snapshot(&c, &changed).unwrap();
    assert_eq!(store.event(old_id).unwrap().unwrap().summary, "Remote edit");
    assert!(store.event(newer_id).unwrap().is_none());
    assert!(!store
        .caldav_range_cached(&c.id, 200, 300, "test-zone")
        .unwrap());
}

#[test]
fn malformed_snapshot_rolls_back_deletions_resource_changes_and_coverage() {
    let store = Store::open_in_memory().unwrap();
    let c = connection();
    let s = snapshot(&c, 0, 100, Some(20));
    install(&store, &c, &s);
    let id = s.calendars[0].resources[0].events[0].id.clone();
    let mut bad = s.clone();
    bad.calendars[0].resources[0].events[0].account_id = AccountId::from("another-account");
    bad.calendars[0].resources[0].etag = "\"invalid\"".into();
    assert!(store.apply_caldav_snapshot(&c, &bad).is_err());
    assert_eq!(store.event(&id).unwrap().unwrap().summary, "Original title");
    assert!(store
        .caldav_range_cached(&c.id, 0, 100, "test-zone")
        .unwrap());
}

#[test]
fn disconnect_and_reconnect_reject_late_downloads_and_status_without_touching_mail() {
    let store = Store::open_in_memory().unwrap();
    let c = connection();
    let s = snapshot(&c, 0, 100, Some(20));
    install(&store, &c, &s);
    store
        .set_sync_state(&c.account_id, &SyncState::SignInRequired, None)
        .unwrap();
    store.disconnect_caldav(&c).unwrap();
    assert!(store.apply_caldav_snapshot(&c, &s).is_err());
    let next = connection();
    let newer = snapshot(&next, 0, 100, Some(30));
    store
        .reserve_caldav_setup(&next.account_id, &next.id)
        .unwrap();
    store
        .install_caldav_connection(&next, None, &newer)
        .unwrap();
    store
        .set_caldav_status(&c, &SyncState::SyncFailed("late".into()))
        .unwrap();
    assert_eq!(
        store
            .setting::<SyncState>(&format!("cal_status:{}", c.account_id))
            .unwrap(),
        Some(SyncState::UpToDate)
    );
    assert_eq!(
        store.account_status(&c.account_id).unwrap().state,
        SyncState::SignInRequired
    );
    assert!(!store.caldav_connection_current(&c).unwrap());
    assert!(store.caldav_connection_current(&next).unwrap());
    store.delete_account(&next.account_id).unwrap();
    assert!(store.apply_caldav_snapshot(&next, &newer).is_err());
}

#[test]
fn superseded_setup_and_account_removal_cannot_resurrect_a_connection() {
    let store = Store::open_in_memory().unwrap();
    store.upsert_account(&account()).unwrap();
    let first = connection();
    let second = connection();
    store
        .reserve_caldav_setup(&first.account_id, &first.id)
        .unwrap();
    store
        .reserve_caldav_setup(&second.account_id, &second.id)
        .unwrap();
    assert!(store
        .install_caldav_connection(&first, None, &snapshot(&first, 0, 100, None))
        .is_err());
    store.cancel_caldav_setup(&second.account_id).unwrap();
    assert!(store
        .install_caldav_connection(&second, None, &snapshot(&second, 0, 100, None))
        .is_err());
    assert!(store
        .calendar_connection(&first.account_id)
        .unwrap()
        .is_none());
}

#[test]
fn read_only_caldav_cannot_enqueue_edit_delete_or_rsvp() {
    let store = Store::open_in_memory().unwrap();
    let c = connection();
    let s = snapshot(&c, 0, 100, Some(20));
    install(&store, &c, &s);
    let event = &s.calendars[0].resources[0].events[0];
    assert!(store
        .apply_event_change(
            &c.account_id,
            event,
            OperationKind::RespondToEvent {
                event_id: event.id.clone(),
                response: ResponseStatus::Accepted
            }
        )
        .is_err());
    assert!(store
        .apply_series_change(&c.account_id, event, RecurrenceScope::All)
        .is_err());
    assert!(store.delete_event_locally(&c.account_id, event).is_err());
    assert_eq!(store.pending_count(&c.account_id).unwrap(), 0);
    assert!(store.event(&event.id).unwrap().is_some());
}

#[test]
#[ignore = "Creates a disposable native UI fixture in an explicitly supplied new /tmp directory"]
fn export_caldav_native_fixture() {
    let dir =
        std::env::var("OVERLOOK_CALDAV_FIXTURE_DIR").expect("set the isolated fixture directory");
    assert!(dir.starts_with("/tmp/overlook-caldav-native-"));
    let dir = std::path::Path::new(&dir);
    assert!(!dir.exists(), "never overwrite an existing profile");
    std::fs::create_dir(dir).unwrap();
    let store = Store::open(&dir.join("overlook.sqlite")).unwrap();
    let mut c = connection();
    let mut account = account();
    account.id = AccountId(format!(
        "imap:caldav-demo-{}",
        CalendarConnectionId::generate()
    ));
    account.email = "demo@calendar.example.test".into();
    account.display_name = "Demo — Fastmail calendars".into();
    c.account_id = account.id.clone();
    c.settings.username = account.email.clone();
    store.upsert_account(&account).unwrap();
    store
        .set_servers(
            &account.id,
            &MailServers {
                incoming: ServerEndpoint::new(
                    "imap.fastmail.com",
                    993,
                    Security::Tls,
                    &account.email,
                ),
                outgoing: ServerEndpoint::new(
                    "smtp.fastmail.com",
                    465,
                    Security::Tls,
                    &account.email,
                ),
                imap: None,
                pop3: None,
            },
        )
        .unwrap();
    let at = tz::day_start(now()) + 15 * 3_600_000;
    let mut s = snapshot(&c, tz::add_days(at, -60), tz::add_days(at, 366), Some(at));
    s.timezone = format!("{:?}", tz::system_zone());
    s.calendars[0].calendar.name = "Demo · Personal".into();
    s.calendars[0].calendar.color = Some("#568b75".into());
    let e = &mut s.calendars[0].resources[0].events[0];
    e.summary = "Demo · Ridge planning".into();
    e.end = at + 3_600_000;
    e.description = Some("Synthetic calendar fixture for native verification. No real Fastmail account or password is connected.".into());
    e.location = Some("Overlook studio".into());
    if std::env::var_os("OVERLOOK_CALDAV_WRITE_FIXTURE").is_some() {
        e.recurring_event_id = None;
        e.original_start = None;
        e.ical_uid = Some("demo-personal@overlook.local".into());
        let body = format!("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:demo-personal@overlook.local\r\nDTSTART:{}\r\nDTEND:{}\r\nSUMMARY:Demo · Ridge planning\r\nLOCATION:Overlook studio\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n", tz::utc_basic(e.start),tz::utc_basic(e.end));
        s.calendars[0].calendar.can_edit = true;
        s.calendars[0].resources[0].icalendar = body;
    }
    store.reserve_caldav_setup(&c.account_id, &c.id).unwrap();
    store.install_caldav_connection(&c, None, &s).unwrap();
    if std::env::var_os("OVERLOOK_CALDAV_CONFLICT_FIXTURE").is_some() {
        assert!(s.calendars[0].calendar.can_edit);
        let event = store
            .event(&s.calendars[0].resources[0].events[0].id)
            .unwrap()
            .unwrap();
        let token = store.caldav_change_for_event(&event, false).unwrap();
        let mut after = event.clone();
        after.summary = "Demo · My ridge planning".into();
        store
            .queue_caldav_change(
                &token,
                Some(
                    s.calendars[0].resources[0]
                        .icalendar
                        .replace("Demo · Ridge planning", &after.summary),
                ),
                CalDavEdit {
                    before: Some(event),
                    after: Some(after),
                    scope: RecurrenceScope::This,
                },
            )
            .unwrap();
        let token = store
            .claim_caldav_change(&store.caldav_changes(&c.account_id).unwrap()[0])
            .unwrap();
        store.mark_caldav_change(&token,CalDavChangeState::Conflict,Some(CalDavRemote{etag:"\"remote-two\"".into(),icalendar:s.calendars[0].resources[0].icalendar.replace("Overlook studio","Provider · Mountain cabin")}),"Demo conflict: the location changed at the provider while your title was edited offline.".into()).unwrap();
    }
    store.set_setting("active_account", &account.id).unwrap();
    store.set_setting("last_module", &"calendar").unwrap();
    store.set_setting("appearance", &"light").unwrap();
    let prefs = Prefs {
        startup_module: StartupModule::Calendar,
        event_reminders: false,
        check_for_updates: false,
        ..Default::default()
    };
    store.set_setting("prefs", &prefs).unwrap();
}

fn writable_snapshot(c: &CalendarConnection) -> CalDavSnapshot {
    let mut s = snapshot(c, 0, 100, Some(10));
    s.calendars[0].calendar.can_edit = true;
    s.calendars[0].resources[0].events[0].recurring_event_id = None;
    s.calendars[0].resources[0].events[0].original_start = None;
    s
}
fn queue_title(store: &Store, event: &Event, title: &str) {
    let token = store.caldav_change_for_event(event, false).unwrap();
    let mut after = event.clone();
    after.summary = title.into();
    store
        .queue_caldav_change(
            &token,
            Some(format!("durable payload: {title}")),
            CalDavEdit {
                before: Some(event.clone()),
                after: Some(after),
                scope: RecurrenceScope::This,
            },
        )
        .unwrap();
}

#[test]
fn local_write_is_atomic_coalesces_and_survives_reopen_without_entering_mail_queue() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let c = connection();
    let s = writable_snapshot(&c);
    let e = s.calendars[0].resources[0].events[0].clone();
    {
        let store = Store::open(&path).unwrap();
        install(&store, &c, &s);
        queue_title(&store, &e, "First offline edit");
        let first = store.event(&e.id).unwrap().unwrap();
        assert!(first.pending);
        queue_title(&store, &first, "Second offline edit");
        assert_eq!(store.caldav_changes(&c.account_id).unwrap().len(), 1);
        assert_eq!(store.pending_count(&c.account_id).unwrap(), 0);
        assert!(store.disconnect_caldav(&c).is_err());
        assert!(store
            .reserve_caldav_setup(&c.account_id, &CalendarConnectionId::generate())
            .is_err());
    }
    let store = Store::open(&path).unwrap();
    let changes = store.caldav_changes(&c.account_id).unwrap();
    assert_eq!(changes[0].revision, 2);
    assert_eq!(changes[0].edits.len(), 2);
    assert_eq!(
        store.event(&e.id).unwrap().unwrap().summary,
        "Second offline edit"
    );
    let token = store
        .caldav_change_for_event(&store.event(&e.id).unwrap().unwrap(), false)
        .unwrap();
    let mut wrong = e.clone();
    wrong.account_id = AccountId::from("other");
    assert!(store
        .queue_caldav_change(
            &token,
            Some("bad".into()),
            CalDavEdit {
                before: Some(e.clone()),
                after: Some(wrong),
                scope: RecurrenceScope::This
            }
        )
        .is_err());
    assert_eq!(store.caldav_changes(&c.account_id).unwrap()[0], changes[0]);
}

#[test]
fn snapshots_preserve_local_edits_tombstones_and_disappearing_collections() {
    let store = Store::open_in_memory().unwrap();
    let c = connection();
    let s = writable_snapshot(&c);
    install(&store, &c, &s);
    let e = s.calendars[0].resources[0].events[0].clone();
    queue_title(&store, &e, "Offline title");
    let mut remote = s.clone();
    remote.calendars[0].resources[0].events[0].summary = "Remote title".into();
    remote.calendars[0].resources[0].etag = "\"remote\"".into();
    store.apply_caldav_snapshot(&c, &remote).unwrap();
    assert_eq!(
        store.event(&e.id).unwrap().unwrap().summary,
        "Offline title"
    );
    // A server-side MOVE must not replace the overlay even though its href changed.
    remote.calendars[0].resources[0].href = "https://caldav.fastmail.com/calendar/moved.ics".into();
    store.apply_caldav_snapshot(&c, &remote).unwrap();
    assert_eq!(
        store.event(&e.id).unwrap().unwrap().summary,
        "Offline title"
    );
    let current = store.event(&e.id).unwrap().unwrap();
    let token = store.caldav_change_for_event(&current, false).unwrap();
    store
        .queue_caldav_change(
            &token,
            None,
            CalDavEdit {
                before: Some(current),
                after: None,
                scope: RecurrenceScope::This,
            },
        )
        .unwrap();
    store.apply_caldav_snapshot(&c, &s).unwrap();
    assert!(store.event(&e.id).unwrap().is_none());
    remote.calendars.clear();
    store.apply_caldav_snapshot(&c, &remote).unwrap();
    assert_eq!(store.caldav_changes(&c.account_id).unwrap().len(), 1);
    assert_eq!(store.calendars(&c.account_id).unwrap().len(), 1);
}

#[test]
fn frozen_attempts_reject_edits_and_stale_results_conflicts_preserve_both_versions() {
    let store = Store::open_in_memory().unwrap();
    let c = connection();
    let s = writable_snapshot(&c);
    install(&store, &c, &s);
    let e = s.calendars[0].resources[0].events[0].clone();
    queue_title(&store, &e, "Local");
    let pending = store.caldav_changes(&c.account_id).unwrap().pop().unwrap();
    let checking = store.claim_caldav_change(&pending).unwrap();
    assert!(store
        .caldav_change_for_event(&store.event(&e.id).unwrap().unwrap(), false)
        .is_err());
    assert!(store.complete_caldav_change(&pending, None, false).is_err());
    let remote = CalDavRemote {
        etag: "\"two\"".into(),
        icalendar: "provider version".into(),
    };
    store
        .mark_caldav_change(
            &checking,
            CalDavChangeState::Conflict,
            Some(remote.clone()),
            "Remote changed".into(),
        )
        .unwrap();
    let conflict = store.caldav_changes(&c.account_id).unwrap().pop().unwrap();
    assert_eq!(conflict.remote, Some(remote.clone()));
    assert_eq!(conflict.desired, Some("durable payload: Local".into()));
    store
        .rebase_caldav_change(&conflict, Some("merged".into()))
        .unwrap();
    let retry = store.caldav_changes(&c.account_id).unwrap().pop().unwrap();
    assert_eq!(retry.base, Some(remote));
    assert_eq!(retry.revision, 2);
    assert_eq!(retry.state, CalDavChangeState::Pending);
    assert!(store
        .complete_caldav_change(&checking, None, false)
        .is_err());
    let retry = store.claim_caldav_change(&retry).unwrap();
    store
        .complete_caldav_change(
            &retry,
            Some(&CalDavRemote {
                etag: "\"three\"".into(),
                icalendar: "merged".into(),
            }),
            false,
        )
        .unwrap();
    assert!(!store.event(&e.id).unwrap().unwrap().pending);
    assert!(store.caldav_changes(&c.account_id).unwrap().is_empty());
    assert_eq!(
        store.caldav_last_sync(&c.id).unwrap(),
        None,
        "The timezone-only coverage marker is not a successful download timestamp"
    );
    assert!(!store
        .caldav_range_cached(&c.id, 0, 100, "test-zone")
        .unwrap());
}

#[test]
fn choosing_provider_discards_only_that_resource_and_offline_create_delete_cancels() {
    let store = Store::open_in_memory().unwrap();
    let c = connection();
    let s = writable_snapshot(&c);
    install(&store, &c, &s);
    let e = s.calendars[0].resources[0].events[0].clone();
    let mut new = e.clone();
    new.id = EventId::from("caldav:new");
    new.provider_id = Some(ProviderId::from("local-new"));
    new.ical_uid = Some("local-new@overlook.local".into());
    let token = store.caldav_change_for_event(&new, true).unwrap();
    store
        .queue_caldav_change(
            &token,
            Some("new payload".into()),
            CalDavEdit {
                before: None,
                after: Some(new.clone()),
                scope: RecurrenceScope::This,
            },
        )
        .unwrap();
    let saved = store.event(&new.id).unwrap().unwrap();
    let token = store.caldav_change_for_event(&saved, false).unwrap();
    store
        .queue_caldav_change(
            &token,
            None,
            CalDavEdit {
                before: Some(saved),
                after: None,
                scope: RecurrenceScope::This,
            },
        )
        .unwrap();
    assert!(store.caldav_changes(&c.account_id).unwrap().is_empty());
    assert!(store.event(&new.id).unwrap().is_none());
    assert!(store.event(&e.id).unwrap().is_some());
    queue_title(&store, &e, "Local");
    let token = store
        .claim_caldav_change(&store.caldav_changes(&c.account_id).unwrap()[0])
        .unwrap();
    store
        .mark_caldav_change(
            &token,
            CalDavChangeState::Conflict,
            None,
            "Deleted remotely".into(),
        )
        .unwrap();
    let conflict = store.caldav_changes(&c.account_id).unwrap().pop().unwrap();
    store.complete_caldav_change(&conflict, None, true).unwrap();
    assert!(store.event(&e.id).unwrap().is_none());
    assert!(store.caldav_changes(&c.account_id).unwrap().is_empty());
}

#[test]
#[ignore = "Invoked only by the crash persistence parent test"]
fn caldav_outbox_crash_child() {
    let dir = std::env::var("OVERLOOK_CALDAV_CRASH_DIR").unwrap();
    let store = Store::open(&std::path::Path::new(&dir).join("db")).unwrap();
    let c = connection();
    let s = writable_snapshot(&c);
    install(&store, &c, &s);
    queue_title(
        &store,
        &s.calendars[0].resources[0].events[0],
        "Acknowledged before kill",
    );
    if std::env::var_os("OVERLOOK_CALDAV_CRASH_CLAIM").is_some() {
        store
            .claim_caldav_change(&store.caldav_changes(&c.account_id).unwrap()[0])
            .unwrap();
    }
    std::fs::write(std::path::Path::new(&dir).join("saved"), b"acknowledged").unwrap();
    std::thread::sleep(std::time::Duration::from_secs(30));
}

#[test]
fn acknowledged_and_in_flight_changes_survive_process_kill() {
    for claimed in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let mut cmd = std::process::Command::new(std::env::current_exe().unwrap());
        cmd.args([
            "--exact",
            "caldav::tests::caldav_outbox_crash_child",
            "--ignored",
            "--nocapture",
        ])
        .env("OVERLOOK_CALDAV_CRASH_DIR", dir.path())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
        if claimed {
            cmd.env("OVERLOOK_CALDAV_CRASH_CLAIM", "1");
        }
        let mut child = cmd.spawn().unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !dir.path().join("saved").exists()
            && std::time::Instant::now() < deadline
            && child.try_wait().unwrap().is_none()
        {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let saved = dir.path().join("saved").exists();
        let _ = child.kill();
        let _ = child.wait();
        assert!(saved, "child did not acknowledge a durable save");
        let store = Store::open(&dir.path().join("db")).unwrap();
        let change = store.caldav_changes(&account().id).unwrap().pop().unwrap();
        assert_eq!(
            change.state,
            if claimed {
                CalDavChangeState::Checking
            } else {
                CalDavChangeState::Pending
            }
        );
        let event = change.edits[0].after.as_ref().unwrap();
        assert_eq!(
            store.event(&event.id).unwrap().unwrap().summary,
            "Acknowledged before kill"
        );
    }
}
