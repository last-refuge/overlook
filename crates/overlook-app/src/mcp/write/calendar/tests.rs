use super::*;
use crate::mcp::read::ReadContext;
use overlook_core::{tz, Account, AccountId, CalDavChangeState, Calendar, CalendarConnection};
use overlook_provider::{
    caldav::{fake::CalendarServer, CalDavWriteResult},
    sample::SampleProvider,
};
use overlook_store::{McpWriteKey, McpWriteOutcome, Store};
use std::path::Path;

fn account(id: &str, provider: ProviderKind) -> Account {
    Account {
        id: AccountId::from(id),
        provider,
        display_name: "Fixture".into(),
        email: "fixture@example.test".into(),
        created_at: 0,
    }
}

fn context(account: &Account) -> WriteContext<'_> {
    WriteContext {
        account,
        account_id: &account.id.0,
        profile: Path::new("."),
        key: &[1; 32],
    }
}

fn calendar(account: &Account, id: &str, can_edit: bool) -> Calendar {
    Calendar {
        id: overlook_core::CalendarId::from(id),
        account_id: account.id.clone(),
        provider_id: None,
        name: "Same calendar name".into(),
        color: None,
        is_primary: false,
        can_edit,
        visible: true,
        sort_order: 0,
        default_reminders: vec![],
    }
}

fn request(
    store: &Store,
    ctx: &WriteContext<'_>,
    key: u8,
    name: &str,
    payload: Value,
) -> Result<Value, Failure> {
    let result = store.mcp_write::<Failure>(
        McpWriteKey {
            account: &ctx.account.id,
            client: "calendar-test",
            key_digest: &[key; 32],
            request_digest: &[key; 32],
            action: name,
        },
        |tx, _| execute(ctx, tx, name, payload.as_object().unwrap()),
    )?;
    match result {
        McpWriteOutcome::Applied { data, .. } | McpWriteOutcome::Replayed(data) => Ok(data),
        McpWriteOutcome::KeyConflict => panic!("unexpected receipt conflict"),
    }
}

#[test]
fn exact_sample_collection_and_revision_are_required() {
    let store = Store::open_in_memory().unwrap();
    let a = account("sample", ProviderKind::Sample);
    let b = account("sample-other", ProviderKind::Sample);
    let google = account("google", ProviderKind::Gmail);
    for owner in [&a, &b, &google] {
        store.upsert_account(owner).unwrap();
    }
    let writable = calendar(&a, "chosen", true);
    let readonly = calendar(&a, "readonly", false);
    store
        .replace_calendars(&a.id, &[writable.clone(), readonly.clone()])
        .unwrap();
    let foreign = calendar(&b, "foreign", true);
    store.replace_calendars(&b.id, &[foreign.clone()]).unwrap();
    let google_calendar = calendar(&google, "google-writable", true);
    store
        .replace_calendars(&google.id, &[google_calendar.clone()])
        .unwrap();

    let ctx = context(&a);
    let payload = |id| {
        json!({
            "calendar_id": id,
            "title": "Personal appointment",
            "start": "2026-10-24T09:00:00-07:00",
            "end": "2026-10-24T10:00:00-07:00"
        })
    };
    for (ctx, id, code) in [
        (
            &ctx,
            ctx.id("calendar", &readonly.id.0),
            ErrorCode::AccessDenied,
        ),
        (
            &ctx,
            context(&b).id("calendar", &foreign.id.0),
            ErrorCode::NotCached,
        ),
        (
            &ctx,
            ctx.id("calendar", &foreign.id.0),
            ErrorCode::NotCached,
        ),
        (
            &context(&google),
            context(&google).id("calendar", &google_calendar.id.0),
            ErrorCode::Unsupported,
        ),
    ] {
        assert_eq!(
            request(&store, ctx, 1, CREATE, payload(id))
                .unwrap_err()
                .code,
            code
        );
    }
    let chosen = ctx.id("calendar", &writable.id.0);
    let created = request(&store, &ctx, 1, CREATE, payload(chosen.clone())).unwrap();
    assert_eq!(
        created,
        request(&store, &ctx, 1, CREATE, payload(chosen.clone())).unwrap()
    );
    assert_eq!(created["calendar_id"], chosen);
    assert_eq!(store.pending_operations(&a.id).unwrap().len(), 1);
    let updated = request(
        &store,
        &ctx,
        2,
        UPDATE,
        json!({
            "event_id": created["event_id"], "if_revision": created["revision"],
            "start_date": "2026-10-24", "end_date": "2026-10-26",
            "time_zone": "Pacific/Auckland", "location": "Room 4",
        }),
    )
    .unwrap();
    assert_ne!(updated["revision"], created["revision"]);
    let event = store
        .events_between(&a.id, 0, i64::MAX)
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(event.calendar_id, writable.id);
    assert!(event.all_day);
    assert_eq!(tz::local_date(event.start).to_string(), "2026-10-24");
    assert_eq!(tz::local_date(event.end).to_string(), "2026-10-26");

    assert_eq!(
        request(
            &store,
            &ctx,
            3,
            DELETE,
            json!({
                "event_id": created["event_id"], "if_revision": created["revision"]
            })
        )
        .unwrap_err()
        .code,
        ErrorCode::Conflict
    );
    request(
        &store,
        &ctx,
        3,
        DELETE,
        json!({
            "event_id": created["event_id"], "if_revision": updated["revision"]
        }),
    )
    .unwrap();
    assert!(store.event(&event.id).unwrap().is_none());
}

fn seed(uid: &str, title: &str, extra: &str) -> String {
    format!("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:{uid}\r\nDTSTAMP:20260101T000000Z\r\nDTSTART:20261024T160000Z\r\nDTEND:20261024T170000Z\r\nSUMMARY:{title}\r\n{extra}END:VEVENT\r\nEND:VCALENDAR\r\n")
}

fn download(
    store: &Store,
    server: &CalendarServer,
    owner: &Account,
    connection: &CalendarConnection,
) {
    let request = server
        .client()
        .synchronize_collections(
            owner,
            &connection.id,
            tz::parse_rfc3339("2026-10-23T00:00:00Z").unwrap(),
            tz::parse_rfc3339("2026-10-29T00:00:00Z").unwrap(),
            &[],
            || true,
        )
        .unwrap();
    store.apply_caldav_download(connection, &request).unwrap();
}

fn reconcile(store: &Store, server: &CalendarServer, owner: &Account) {
    for change in store.caldav_changes(&owner.id).unwrap() {
        if change.state != CalDavChangeState::Pending {
            continue;
        }
        let claimed = store.claim_caldav_change(&change).unwrap();
        match server
            .client()
            .reconcile_change(&claimed, || store.caldav_change_current(&claimed).unwrap())
            .unwrap()
        {
            CalDavWriteResult::Applied(remote) => store
                .complete_caldav_change(&claimed, remote.as_ref(), false)
                .unwrap(),
            CalDavWriteResult::Conflict(remote) => store
                .mark_caldav_change(
                    &claimed,
                    CalDavChangeState::Conflict,
                    remote,
                    "fixture conflict".into(),
                )
                .unwrap(),
        }
    }
}

#[test]
fn caldav_receipts_reconcile_occurrences_series_and_conflicts_over_https() {
    let server = CalendarServer::start();
    for uid in ["keep", "remote"] {
        server.put_remote(
            &format!("/{uid}.ics"),
            &seed(uid, uid, "LOCATION:Original\r\n"),
        );
    }
    // The HTTPS fixture returns stored instances for both REPORT forms.
    let mut series = seed("series", "Series", "");
    let end = series.find("END:VCALENDAR").unwrap();
    let mut exceptions = String::new();
    for day in [25, 26] {
        exceptions.push_str(&format!(
            "BEGIN:VEVENT\r\nUID:series\r\nDTSTAMP:20260101T000000Z\r\nRECURRENCE-ID:202610{day}T160000Z\r\nDTSTART:202610{day}T160000Z\r\nDTEND:202610{day}T170000Z\r\nSUMMARY:Series\r\nEND:VEVENT\r\n"
        ));
    }
    series.insert_str(end, &exceptions);
    server.put_remote("/series.ics", &series);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("calendar.sqlite");
    let store = Store::open(&path).unwrap();
    let owner = account("imap:fixture", ProviderKind::Imap);
    store.upsert_account(&owner).unwrap();
    let connection = store
        .save_calendar_connection(&owner.id, &server.settings(), true, None)
        .unwrap();
    let ctx = context(&owner);
    download(&store, &server, &owner, &connection);
    let cached = |store: &Store| store.events_between(&owner.id, 0, i64::MAX).unwrap();
    let arguments = |event: &Event| {
        json!({
            "event_id": ctx.id("event", &event.id.0),
            "if_revision": ctx.event_revision(event)
        })
    };
    let initial = cached(&store);
    let series: Vec<_> = initial
        .iter()
        .filter(|event| event.summary == "Series")
        .collect();
    assert_eq!(series.len(), 3);

    let mut edit = arguments(series[1]);
    edit["title"] = json!("Edited occurrence");
    for scope in ["series", "following"] {
        edit["scope"] = json!(scope);
        assert_eq!(
            request(&store, &ctx, 1, UPDATE, edit.clone())
                .unwrap_err()
                .code,
            ErrorCode::Unsupported
        );
    }
    edit["scope"] = json!("this");
    request(&store, &ctx, 1, UPDATE, edit).unwrap();
    let cal = store.calendars(&owner.id).unwrap().pop().unwrap();
    let created = request(
        &store,
        &ctx,
        2,
        CREATE,
        json!({
            "calendar_id": ctx.id("calendar", &cal.id.0),
            "title": "All day",
            "start_date": "2026-10-24", "end_date": "2026-10-26", "time_zone": "Europe/Berlin"
        }),
    )
    .unwrap();
    assert_eq!(created["status"], "queued");

    for (key, uid) in [(3, "keep"), (4, "remote")] {
        let event = initial.iter().find(|event| event.summary == uid).unwrap();
        let mut edit = arguments(event);
        edit["title"] = json!(format!("Local {uid}"));
        request(&store, &ctx, key, UPDATE, edit).unwrap();
        server.put_remote(
            &format!("/{uid}.ics"),
            &seed(uid, "Provider changed", "LOCATION:Provider room\r\n"),
        );
    }
    assert_eq!(server.write_count(), 0);
    drop(store);
    let store = Store::open(&path).unwrap();
    assert_eq!(store.caldav_changes(&owner.id).unwrap().len(), 4);
    reconcile(&store, &server, &owner);
    assert_eq!(server.write_count(), 2);
    download(&store, &server, &owner, &connection);

    let sample = SampleProvider::new();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let reader = ReadContext {
        store: &store,
        account: &owner,
        account_id: ctx.account_id,
        sample: &sample,
        runtime: &runtime,
        key: ctx.key,
    };
    for (key, change) in store.caldav_changes(&owner.id).unwrap().iter().enumerate() {
        let operation_id = ctx.id("operation", change.id.as_str());
        let lookup = json!({"operation_id":operation_id});
        let detail = conflict_details(&reader, lookup.as_object().unwrap()).unwrap();
        assert!(detail["remote_preview"]
            .as_str()
            .unwrap()
            .contains("Provider room"));
        assert!(!detail.to_string().contains("synthetic.example"));
        let choice = if change.href.ends_with("/keep.ics") {
            "keep-local"
        } else {
            "take-remote"
        };
        let mut resolve =
            json!({"operation_id":operation_id, "if_revision":"stale", "resolution":choice});
        assert_eq!(
            request(&store, &ctx, 5 + key as u8, RESOLVE, resolve.clone())
                .unwrap_err()
                .code,
            ErrorCode::Conflict
        );
        resolve["if_revision"] = detail["revision"].clone();
        request(&store, &ctx, 5 + key as u8, RESOLVE, resolve).unwrap();
    }
    reconcile(&store, &server, &owner);
    download(&store, &server, &owner, &connection);
    let events = cached(&store);
    let kept = events
        .iter()
        .find(|event| event.summary == "Local keep")
        .unwrap();
    assert_eq!(kept.location.as_deref(), Some("Provider room"));
    assert!(events
        .iter()
        .any(|event| event.summary == "Provider changed"));
    assert!(events
        .iter()
        .any(|event| event.summary == "All day" && event.all_day));
    assert!(events
        .iter()
        .any(|event| event.summary == "Edited occurrence"));

    let mut delete = arguments(
        events
            .iter()
            .find(|event| event.summary == "Edited occurrence")
            .unwrap(),
    );
    delete["scope"] = json!("series");
    request(&store, &ctx, 7, DELETE, delete).unwrap();
    reconcile(&store, &server, &owner);
    assert!(!server.resources().contains_key("/series.ics"));
    assert!(cached(&store)
        .iter()
        .all(|event| event.recurring_event_id.is_none()));
}
