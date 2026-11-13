//! Headless workflows through the same commands as the editor, real SQLite,
//! the real CalDAV client/parser and a stateful HTTPS server on loopback.
use super::*;
use overlook_provider::caldav::fake::CalendarServer;

/// Started and cleaned up by scripts/qualify-caldav.py, with synthetic data only.
#[test]
#[ignore = "requires the isolated Radicale runner"]
fn reference_server_round_trip() {
    use overlook_provider::caldav::fake::reference_client;
    let address = std::env::var("OVERLOOK_REFERENCE_CALDAV_ADDR")
        .expect("use scripts/qualify-caldav.py")
        .parse()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("reference.sqlite");
    let store = Store::open(&path).unwrap();
    let account = account();
    store.upsert_account(&account).unwrap();
    let connection = store
        .save_calendar_connection(
            &account.id,
            &CalDavSettings {
                server_url: "https://synthetic.example/".into(),
                username: "fixture@example.test".into(),
                credential_source: CalendarCredentialSource::Separate,
            },
            true,
            None,
        )
        .unwrap();
    let from = tz::date_start(tz::parse_date("2026-10-23").unwrap());
    let to = tz::add_days(from, 6);
    let sync = |store: &Store| {
        let retry = synchronize_using(store, &account, &connection, from, to, || {
            Ok(reference_client(address))
        });
        assert_eq!(
            retry,
            0,
            "{:?}",
            crate::sync::calendar_status(store, &account.id)
        );
    };
    let all = |store: &Store| store.events_between(&account.id, from, to).unwrap();
    sync(&store);
    assert_eq!(store.calendars(&account.id).unwrap().len(), 2);
    let initial = all(&store);
    assert_eq!(initial.len(), 5, "{initial:#?}");
    let series: Vec<_> = initial
        .iter()
        .filter(|e| e.summary == "Reference recurring")
        .cloned()
        .collect();
    assert_eq!(series.len(), 3);
    assert!(series.iter().all(|e| e.recurring_event_id.is_some()));
    // The server expands Europe/Berlin recurrence through its autumn DST transition.
    let mut starts: Vec<_> = series.iter().map(|e| e.start).collect();
    starts.sort();
    assert_eq!(starts[1] - starts[0], 25 * 3_600_000);
    assert_eq!(starts[2] - starts[1], 24 * 3_600_000);
    let day = initial.iter().find(|e| e.all_day).unwrap();
    assert_eq!(tz::local_date(day.start).to_string(), "2026-10-24");
    assert_eq!(tz::local_date(day.end).to_string(), "2026-10-27");
    let checkpoints = store
        .caldav_checkpoints(&connection, from, to, &tz::cache_key())
        .unwrap();
    assert_eq!(checkpoints.len(), 2);
    let unchanged = reference_client(address)
        .synchronize_collections(&account, &connection.id, from, to, &checkpoints, || true)
        .unwrap();
    assert_eq!(unchanged.reused.len(), 2);
    store
        .apply_caldav_download(&connection, &unchanged)
        .unwrap();
    assert_eq!(all(&store).len(), 5);

    let single = initial
        .iter()
        .find(|e| e.summary == "Reference single")
        .unwrap()
        .clone();
    let mut changed = single.clone();
    changed.summary = "Reference desktop edit".into();
    edit(&store, &account, Some(single), Some(changed));
    let occurrence = series
        .iter()
        .find(|e| e.start == starts[1])
        .unwrap()
        .clone();
    let mut changed = occurrence.clone();
    changed.summary = "Reference occurrence edit".into();
    changed.location = Some("Occurrence room".into());
    edit(&store, &account, Some(occurrence), Some(changed));
    drop(store);
    let store = Store::open(&path).unwrap();
    assert_eq!(store.caldav_changes(&account.id).unwrap().len(), 2);
    sync(&store);
    assert!(store.caldav_changes(&account.id).unwrap().is_empty());
    let updated = all(&store);
    assert_eq!(updated.len(), 5);
    assert!(updated
        .iter()
        .any(|e| e.summary == "Reference desktop edit" && !e.pending));
    assert!(updated
        .iter()
        .any(|e| e.summary == "Reference occurrence edit"
            && e.location.as_deref() == Some("Occurrence room")
            && !e.pending));
    let original = updated
        .iter()
        .find(|e| e.summary == "Reference desktop edit")
        .unwrap();
    let remote = store.caldav_change_for_event(original, false).unwrap();
    assert!(remote
        .base
        .unwrap()
        .icalendar
        .contains("X-QUALIFICATION:preserve-me"));
    let to_delete = updated
        .iter()
        .find(|e| e.summary == "Reference recurring" && e.start == starts[0])
        .unwrap()
        .clone();
    edit(&store, &account, Some(to_delete), None);
    sync(&store);
    assert_eq!(all(&store).len(), 4);
    assert!(store.caldav_changes(&account.id).unwrap().is_empty());

    let mut new_event = original.clone();
    new_event.id = EventId::from("caldav:reference-created");
    new_event.provider_id = None;
    new_event.summary = "Reference created offline".into();
    edit(&store, &account, None, Some(new_event));
    sync(&store);
    let created = all(&store)
        .into_iter()
        .find(|e| e.summary == "Reference created offline")
        .unwrap();
    assert!(!created.pending);
    edit(&store, &account, Some(created), None);
    sync(&store);
    assert_eq!(all(&store).len(), 4);
    assert!(store.caldav_changes(&account.id).unwrap().is_empty());

    // A second local client changes the same resource on the independent server.
    // Exercise both UI resolution commands against Radicale's normalization/ETags.
    for keep_local in [true, false] {
        let other = Store::open_in_memory().unwrap();
        other.upsert_account(&account).unwrap();
        let other_connection = other
            .save_calendar_connection(&account.id, &connection.settings, true, None)
            .unwrap();
        let sync_other = || {
            assert_eq!(
                synchronize_using(&other, &account, &other_connection, from, to, || Ok(
                    reference_client(address)
                )),
                0
            )
        };
        sync_other();
        let local = all(&store)
            .into_iter()
            .find(|e| e.ical_uid.as_deref() == Some("reference-single"))
            .unwrap();
        let mut desired = local.clone();
        desired.summary = format!("Local conflict choice {keep_local}");
        edit(&store, &account, Some(local), Some(desired));
        let remote = all(&other)
            .into_iter()
            .find(|e| e.ical_uid.as_deref() == Some("reference-single"))
            .unwrap();
        let mut remote_edit = remote.clone();
        remote_edit.summary = format!("Remote conflict choice {keep_local}");
        remote_edit.location = Some(format!("Remote location {keep_local}"));
        edit(&other, &account, Some(remote), Some(remote_edit));
        sync_other();
        sync(&store);
        let conflict = store.caldav_changes(&account.id).unwrap().remove(0);
        assert_eq!(conflict.state, CalDavChangeState::Conflict);
        resolve_change(&store, &account, &conflict, keep_local).unwrap();
        sync(&store);
        let result = all(&store)
            .into_iter()
            .find(|e| e.ical_uid.as_deref() == Some("reference-single"))
            .unwrap();
        assert_eq!(
            result.summary,
            if keep_local {
                "Local conflict choice true"
            } else {
                "Remote conflict choice false"
            }
        );
        assert_eq!(
            result.location,
            Some(format!("Remote location {keep_local}"))
        );
        assert!(!result.pending);
        assert!(store.caldav_changes(&account.id).unwrap().is_empty());
        sync_other();
        assert_eq!(
            all(&other)
                .into_iter()
                .find(|e| e.ical_uid.as_deref() == Some("reference-single"))
                .unwrap()
                .summary,
            result.summary
        );
    }
}

fn account() -> Account {
    Account {
        id: AccountId::from("imap:workflow@example.test"),
        provider: ProviderKind::Imap,
        display_name: "Simulated calendar test".into(),
        email: "workflow@example.test".into(),
        created_at: 0,
    }
}
fn servers() -> MailServers {
    MailServers {
        incoming: ServerEndpoint::new(
            "imap.fastmail.com",
            993,
            Security::Tls,
            "login@fastmail.test",
        ),
        outgoing: ServerEndpoint::new(
            "smtp.fastmail.com",
            465,
            Security::Tls,
            "login@fastmail.test",
        ),
        pop3: None,
        imap: None,
        oauth: None,
    }
}
fn seed(uid: &str, title: &str, location: &str) -> String {
    format!("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Overlook//Workflow fixture//EN\r\nBEGIN:VEVENT\r\nUID:{uid}\r\nDTSTAMP:20260101T000000Z\r\nDTSTART:20261024T160000Z\r\nDTEND:20261024T170000Z\r\nSUMMARY:{title}\r\nLOCATION:{location}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n")
}
fn range() -> (i64, i64) {
    let day = tz::date_start(tz::parse_date("2026-10-24").unwrap());
    (day - 86_400_000, day + 2 * 86_400_000)
}
fn events(store: &Store, account: &Account) -> Vec<Event> {
    let (from, to) = range();
    store.events_between(&account.id, from, to).unwrap()
}
fn event(store: &Store, account: &Account, title: &str) -> Event {
    events(store, account)
        .into_iter()
        .find(|e| e.summary == title)
        .unwrap_or_else(|| panic!("missing event: {title}"))
}
fn run(
    store: &Store,
    account: &Account,
    connection: &CalendarConnection,
    server: &CalendarServer,
) -> u64 {
    let (from, to) = range();
    synchronize_using(store, account, connection, from, to, || Ok(server.client()))
}
fn connect(store: &Store, server: &CalendarServer) -> (Account, CalendarConnection) {
    let account = account();
    store.upsert_account(&account).unwrap();
    store.set_servers(&account.id, &servers()).unwrap();
    store
        .set_sync_state(&account.id, &SyncState::UpToDate, None)
        .unwrap();
    let connection = store
        .save_calendar_connection(&account.id, &server.settings(), true, None)
        .unwrap();
    assert_eq!(run(store, &account, &connection, server), 0);
    (account, connection)
}
fn edit(store: &Store, account: &Account, before: Option<Event>, after: Option<Event>) {
    queue_change(
        store,
        account,
        CalDavEdit {
            before,
            after,
            scope: RecurrenceScope::This,
        },
    )
    .unwrap();
}

#[test]
fn collection_checkpoints_survive_restart_and_recover_without_losing_events() {
    let server = CalendarServer::start();
    server.put_remote("/one.ics", &seed("one", "Original", "Desk"));
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("checkpoints.sqlite");
    let store = Store::open(&path).unwrap();
    let (account, connection) = connect(&store, &server);
    let (from, to) = range();
    let zone = tz::cache_key();
    let checkpoint = store
        .caldav_checkpoints(&connection, from, to, &zone)
        .unwrap();
    assert_eq!(checkpoint.len(), 1);
    assert_eq!(server.report_counts(), (2, 0));
    drop(store);
    let store = Store::open(&path).unwrap();
    assert_eq!(run(&store, &account, &connection, &server), 0);
    assert_eq!(server.report_counts(), (2, 1));
    assert_eq!(
        event(&store, &account, "Original").location.as_deref(),
        Some("Desk")
    );

    server.put_remote("/one.ics", &seed("one", "Changed", "New desk"));
    server.put_remote("/two.ics", &seed("two", "Created", "Room"));
    // A failed refresh must not advance the token or replace the cached projection.
    server.fail_body_reports(true);
    assert!(run(&store, &account, &connection, &server) > 0);
    assert_eq!(
        store
            .caldav_checkpoints(&connection, from, to, &zone)
            .unwrap(),
        checkpoint
    );
    assert_eq!(events(&store, &account).len(), 1);
    event(&store, &account, "Original");
    server.fail_body_reports(false);
    assert_eq!(run(&store, &account, &connection, &server), 0);
    assert_eq!(events(&store, &account).len(), 2);
    event(&store, &account, "Changed");
    server.delete_remote("/two.ics");
    assert_eq!(run(&store, &account, &connection, &server), 0);
    assert_eq!(events(&store, &account).len(), 1);

    let before = server.report_counts();
    server.expire_next_token();
    assert_eq!(run(&store, &account, &connection, &server), 0);
    assert_eq!(server.report_counts(), (before.0 + 2, before.1 + 1));
    let before = server.report_counts();
    server.partial_next_sync();
    assert_eq!(run(&store, &account, &connection, &server), 0);
    assert_eq!(server.report_counts(), (before.0 + 2, before.1 + 1));
    assert_eq!(events(&store, &account).len(), 1);
    assert!(store
        .caldav_checkpoints(&connection, from - 1, to, &zone)
        .unwrap()
        .is_empty());
    assert!(store
        .caldav_checkpoints(&connection, from, to, "another-zone")
        .unwrap()
        .is_empty());

    // A local edit arriving during a no-change report invalidates reuse. The
    // transaction must preserve both the acknowledged edit and the old cursor.
    let cached = store
        .caldav_checkpoints(&connection, from, to, &zone)
        .unwrap();
    let download = server
        .client()
        .synchronize_collections(&account, &connection.id, from, to, &cached, || true)
        .unwrap();
    assert_eq!(download.reused.len(), 1);
    let before = event(&store, &account, "Changed");
    let mut after = before.clone();
    after.summary = "Concurrent local edit".into();
    edit(&store, &account, Some(before), Some(after));
    assert!(store.apply_caldav_download(&connection, &download).is_err());
    assert!(store
        .caldav_checkpoints(&connection, from, to, &zone)
        .unwrap()
        .is_empty());
    assert!(event(&store, &account, "Concurrent local edit").pending);
    assert_eq!(run(&store, &account, &connection, &server), 0);
    assert!(!event(&store, &account, "Concurrent local edit").pending);
    assert_eq!(
        store
            .caldav_checkpoints(&connection, from, to, &zone)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn onboarding_requires_opt_in_uses_incoming_login_and_never_replaces_a_connection() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("onboarding.sqlite");
    let store = Store::open(&path).unwrap();
    let account = account();
    let mut servers = servers();
    store.upsert_account(&account).unwrap();
    store.set_servers(&account.id, &servers).unwrap();
    assert!(add_fastmail_calendars(&store, &account, &servers, false)
        .unwrap()
        .is_none());
    assert!(store.calendar_connection(&account.id).unwrap().is_none());
    // An address or lookalike server is never enough to grant credential reuse.
    for host in [
        "mail.example.test",
        "imap.fastmail.com.evil.test",
        "fastmail.com",
    ] {
        servers.incoming.host = host.into();
        assert!(add_fastmail_calendars(&store, &account, &servers, true).is_err());
        assert!(store.calendar_connection(&account.id).unwrap().is_none());
    }
    servers.incoming.host = "IMAP.FASTMAIL.COM.".into();
    let saved = add_fastmail_calendars(&store, &account, &servers, true)
        .unwrap()
        .unwrap();
    assert_eq!(
        saved.settings.server_url,
        overlook_provider::caldav::FASTMAIL_CALDAV_URL
    );
    assert_eq!(saved.settings.username, "login@fastmail.test");
    assert_eq!(
        saved.settings.credential_source,
        CalendarCredentialSource::IncomingMail
    );
    assert!(add_fastmail_calendars(&store, &account, &servers, true).is_err());
    assert!(add_fastmail_calendars(&store, &account, &servers, false)
        .unwrap()
        .is_none());
    drop(store);
    let store = Store::open(&path).unwrap();
    assert_eq!(store.calendar_connection(&account.id).unwrap(), Some(saved));
    assert_eq!(
        store.servers(&account.id).unwrap().unwrap().incoming.host,
        "imap.fastmail.com"
    );
    assert!(store.pending_operations(&account.id).unwrap().is_empty());
}

#[test]
fn calendar_auth_failure_preserves_mail_and_cached_events_then_recovers() {
    let server = CalendarServer::start();
    server.put_remote("/existing.ics", &seed("existing", "Existing", "Room"));
    let store = Store::open_in_memory().unwrap();
    let (account, connection) = connect(&store, &server);
    server.deny_access(true);
    assert_eq!(run(&store, &account, &connection, &server), 900);
    assert_eq!(
        crate::sync::calendar_status(&store, &account.id),
        SyncState::SignInRequired
    );
    assert_eq!(
        store.account_status(&account.id).unwrap().state,
        SyncState::UpToDate
    );
    assert_eq!(events(&store, &account).len(), 1);
    assert_eq!(
        store.calendar_connection(&account.id).unwrap(),
        Some(connection.clone())
    );
    server.deny_access(false);
    assert_eq!(run(&store, &account, &connection, &server), 0);
    assert_eq!(
        crate::sync::calendar_status(&store, &account.id),
        SyncState::UpToDate
    );
    assert_eq!(server.write_count(), 0);
}

#[test]
fn offline_crud_survives_reopen_and_both_conflict_choices_round_trip_over_https() {
    let server = CalendarServer::start();
    server.put_remote("/keep.ics", &seed("keep", "Keep baseline", "Old room"));
    server.put_remote(
        "/provider.ics",
        &seed("provider", "Provider baseline", "Old room"),
    );
    server.put_remote(
        "/delete.ics",
        &seed("delete", "Delete baseline", "Old room"),
    );
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("offline.sqlite");
    let store = Store::open(&path).unwrap();
    let (account, connection) = connect(&store, &server);
    server.set_offline(true);
    for (old, new) in [
        ("Keep baseline", "Keep my title"),
        ("Provider baseline", "Discard my title"),
    ] {
        let before = event(&store, &account, old);
        let mut after = before.clone();
        after.summary = new.into();
        edit(&store, &account, Some(before), Some(after));
    }
    // Coalesce a second offline edit through the same editor command.
    let before = event(&store, &account, "Keep my title");
    let mut after = before.clone();
    after.description = Some("Local notes".into());
    edit(&store, &account, Some(before), Some(after));
    let mut created = event(&store, &account, "Keep my title");
    created.id = EventId::from("local-created");
    created.provider_id = None;
    created.summary = "Offline creation".into();
    created.all_day = true;
    created.start = tz::date_start(tz::parse_date("2026-10-24").unwrap());
    created.end = tz::add_days(created.start, 2);
    edit(&store, &account, None, Some(created));
    edit(
        &store,
        &account,
        Some(event(&store, &account, "Delete baseline")),
        None,
    );
    assert_eq!(run(&store, &account, &connection, &server), 60);
    assert_eq!(server.write_count(), 0);
    drop(store);
    let store = Store::open(&path).unwrap();
    let queued = store.caldav_changes(&account.id).unwrap();
    assert_eq!(queued.len(), 4);
    assert!(queued.iter().all(|c| c.state == CalDavChangeState::Pending));
    assert_eq!(events(&store, &account).len(), 3);
    assert!(events(&store, &account).iter().all(|e| e.pending));
    assert_eq!(
        event(&store, &account, "Keep my title")
            .description
            .as_deref(),
        Some("Local notes")
    );
    assert!(event(&store, &account, "Offline creation").all_day);
    assert!(server
        .resources()
        .values()
        .any(|r| r.icalendar.contains("SUMMARY:Delete baseline")));

    server.put_remote(
        "/keep.ics",
        &seed("keep", "Remote competing title", "Remote room"),
    );
    server.put_remote(
        "/provider.ics",
        &seed("provider", "Provider wins", "Chosen room"),
    );
    server.set_offline(false);
    assert_eq!(run(&store, &account, &connection, &server), 0);
    let conflicts = store.caldav_changes(&account.id).unwrap();
    assert_eq!(conflicts.len(), 2);
    assert!(conflicts
        .iter()
        .all(|c| c.state == CalDavChangeState::Conflict));
    assert_eq!(
        server.write_count(),
        2,
        "only creation and deletion may upload before review"
    );
    assert_eq!(
        event(&store, &account, "Keep my title").location.as_deref(),
        Some("Old room")
    );
    assert!(!server.resources().contains_key("/delete.ics"));
    assert_eq!(server.resources().len(), 3);
    for conflict in conflicts {
        let keep = conflict.href.ends_with("/keep.ics");
        resolve_change(&store, &account, &conflict, keep).unwrap();
    }
    assert_eq!(run(&store, &account, &connection, &server), 0);
    assert!(store.caldav_changes(&account.id).unwrap().is_empty());
    let kept = event(&store, &account, "Keep my title");
    assert_eq!(kept.location.as_deref(), Some("Remote room"));
    assert_eq!(kept.description.as_deref(), Some("Local notes"));
    assert_eq!(
        event(&store, &account, "Provider wins").location.as_deref(),
        Some("Chosen room")
    );
    let created = event(&store, &account, "Offline creation");
    assert!(created.all_day);
    // Calendar dates, not elapsed hours: this weekend crosses DST in some zones.
    assert_eq!(tz::local_date(created.start).to_string(), "2026-10-24");
    assert_eq!(tz::local_date(created.end).to_string(), "2026-10-26");
    assert!(events(&store, &account).iter().all(|e| !e.pending));
    assert_eq!(server.write_count(), 3);
    assert_eq!(server.resources().len(), 3);
    assert_eq!(
        store.account_status(&account.id).unwrap().state,
        SyncState::UpToDate
    );
}

#[test]
fn lost_put_and_delete_replies_are_reconciled_after_restart_without_repeating_writes() {
    let server = CalendarServer::start();
    server.put_remote("/baseline.ics", &seed("baseline", "Baseline", "Room"));
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("lost-reply.sqlite");
    let store = Store::open(&path).unwrap();
    let (account, connection) = connect(&store, &server);
    let mut created = event(&store, &account, "Baseline");
    created.id = EventId::from("local-created");
    created.provider_id = None;
    created.summary = "Create once".into();
    edit(&store, &account, None, Some(created));
    let href = store.caldav_changes(&account.id).unwrap()[0].href.clone();
    server.lose_next_write_reply();
    assert_eq!(run(&store, &account, &connection, &server), 60);
    assert_eq!(
        store.caldav_changes(&account.id).unwrap()[0].state,
        CalDavChangeState::Checking
    );
    assert_eq!(server.write_count(), 1);
    drop(store);
    let store = Store::open(&path).unwrap();
    assert_eq!(store.caldav_changes(&account.id).unwrap()[0].href, href);
    assert_eq!(run(&store, &account, &connection, &server), 0);
    assert!(store.caldav_changes(&account.id).unwrap().is_empty());
    assert_eq!(
        server.write_count(),
        1,
        "GET must reconcile the accepted PUT"
    );
    assert_eq!(events(&store, &account).len(), 2);
    edit(
        &store,
        &account,
        Some(event(&store, &account, "Create once")),
        None,
    );
    server.lose_next_write_reply();
    assert_eq!(run(&store, &account, &connection, &server), 60);
    assert_eq!(server.write_count(), 2);
    drop(store);
    let store = Store::open(&path).unwrap();
    assert_eq!(run(&store, &account, &connection, &server), 0);
    assert!(store.caldav_changes(&account.id).unwrap().is_empty());
    assert_eq!(
        server.write_count(),
        2,
        "GET must reconcile the accepted DELETE"
    );
    assert_eq!(events(&store, &account).len(), 1);
    assert_eq!(server.resources().len(), 1);
}
