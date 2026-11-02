//! Independent CalDAV outbox reconciliation and read synchronization.
use crate::credentials;
use overlook_core::*;
use overlook_provider::caldav::{CalDavClient, CalDavError, CalDavWriteError, CalDavWriteResult};
use overlook_store::Store;

/// The native editor and headless workflow tests share the same durable command.
pub fn queue_change(
    store: &Store,
    account: &Account,
    mut edit: CalDavEdit,
) -> Result<EventId, String> {
    if edit.before.is_none() {
        if let Some(event) = edit.after.as_mut() {
            event.ical_uid = Some(format!("{}@overlook.local", new_provider_event_id()));
            event.organizer = None;
            event.attendees.retain(|a| !a.is_self);
            event.pending = true;
        }
    }
    let event = edit
        .after
        .as_ref()
        .or(edit.before.as_ref())
        .ok_or("Missing calendar event")?;
    if event.account_id != account.id {
        return Err("Calendar account changed. Reopen the event.".into());
    }
    let id = event.id.clone();
    let token = store
        .caldav_change_for_event(event, edit.before.is_none())
        .map_err(|e| e.to_string())?;
    let base = if token.revision == 0 {
        token.base.as_ref().map(|r| r.icalendar.as_str())
    } else {
        token.desired.as_deref()
    };
    let desired = overlook_provider::caldav::prepare_resource(
        base,
        std::slice::from_ref(&edit),
        &account.email,
    )
    .map_err(|e| e.to_string())?;
    store
        .queue_caldav_change(&token, desired, edit)
        .map_err(|e| e.to_string())?;
    Ok(id)
}

pub fn resolve_change(
    store: &Store,
    account: &Account,
    change: &CalDavChange,
    keep_local: bool,
) -> Result<(), String> {
    if change.account_id != account.id {
        return Err("Calendar account changed. Reopen the changes panel.".into());
    }
    if keep_local {
        if change.timezone != tz::cache_key() {
            return Err(
                "Return to the time zone in which this edit was saved before keeping your changes."
                    .into(),
            );
        }
        let desired = if let Some(remote) = &change.remote {
            overlook_provider::caldav::prepare_conflict_resource(
                change.base.as_ref().map(|r| r.icalendar.as_str()),
                &remote.icalendar,
                &change.edits,
                &account.email,
            )
            .map_err(|e| e.to_string())?
        } else {
            change.desired.clone()
        };
        store
            .rebase_caldav_change(change, desired)
            .map_err(|e| e.to_string())
    } else {
        store
            .complete_caldav_change(change, change.remote.as_ref(), true)
            .map_err(|e| e.to_string())
    }
}

/// Called only after a new mail account and its password have been saved. The
/// checkbox grants reuse of that credential at Fastmail's fixed calendar origin.
/// Discovery happens in the independent calendar worker, so offline setup or a
/// mail-only app password cannot prevent mail from starting.
pub fn add_fastmail_calendars(
    store: &Store,
    account: &Account,
    servers: &MailServers,
    opted_in: bool,
) -> Result<Option<CalendarConnection>, String> {
    if !opted_in {
        return Ok(None);
    }
    if account.provider != ProviderKind::Imap {
        return Err("Connect this account’s calendars in Settings → Accounts → Calendars.".into());
    }
    let mut settings = overlook_provider::caldav::fastmail_preset(servers)
        .ok_or("Connect this account’s calendars in Settings → Accounts → Calendars.")?;
    settings.credential_source = CalendarCredentialSource::IncomingMail;
    // Never replace an existing connection, including its credentials and outbox.
    store
        .save_calendar_connection(&account.id, &settings, true, None)
        .map(Some)
        .map_err(|e| e.to_string())
}

pub fn stored_password(connection: &CalendarConnection) -> Result<String, String> {
    let kind = match connection.settings.credential_source {
        CalendarCredentialSource::IncomingMail => credentials::KIND_PASSWORD_INCOMING.to_owned(),
        CalendarCredentialSource::Separate => connection.secret_kind(),
    };
    credentials::load_password(&connection.account_id, &kind)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| {
            "Enter an app password with calendar access in Settings → Accounts → Calendars.".into()
        })
}

/// Returns a retry delay. Commits and error/status writes all verify the connection's
/// identity and generation, including after account removal or reconnection.
pub fn synchronize(
    store: &Store,
    account: &Account,
    connection: &CalendarConnection,
    from: i64,
    to: i64,
) -> u64 {
    synchronize_using(store, account, connection, from, to, || {
        let password =
            stored_password(connection).map_err(|_| CalDavError::AuthenticationRequired)?;
        CalDavClient::new(&connection.settings, password)
    })
}

fn synchronize_using(
    store: &Store,
    account: &Account,
    connection: &CalendarConnection,
    from: i64,
    to: i64,
    client: impl FnOnce() -> Result<CalDavClient, CalDavError>,
) -> u64 {
    let mut write_error = None;
    let delay = synchronize_with(store, connection, || {
        let client = client()?;
        write_error = flush_changes(store, &account.id, |claimed| {
            client.reconcile_change(claimed, || {
                store.caldav_change_current(claimed).unwrap_or(false)
            })
        })?;
        let cached = store
            .caldav_checkpoints(connection, from, to, &tz::cache_key())
            .map_err(|_| {
                CalDavError::InvalidResponse("Saved calendar checkpoints could not be read.")
            })?;
        client.synchronize_collections(account, &connection.id, from, to, &cached, || {
            store.caldav_connection_current(connection).unwrap_or(false)
        })
    });
    if let Some(error) = write_error {
        let (state, retry) = error_status(&error);
        let _ = store.set_caldav_status(connection, &state);
        return delay.max(retry);
    }
    delay
}

#[cfg(test)]
#[path = "caldav_sync_tests.rs"]
mod integration_tests;

/// Shared coordinator for the real transport and deterministic failure tests.
fn flush_changes(
    store: &Store,
    account: &AccountId,
    mut reconcile: impl FnMut(&CalDavChange) -> Result<CalDavWriteResult, CalDavWriteError>,
) -> Result<Option<CalDavError>, CalDavError> {
    let mut write_error = None;
    for change in store
        .caldav_changes(account)
        .map_err(|_| CalDavError::InvalidResponse("Saved calendar changes could not be read."))?
        .into_iter()
        .filter(|c| {
            matches!(
                c.state,
                CalDavChangeState::Pending | CalDavChangeState::Checking
            )
        })
        .take(100)
    {
        if !matches!(
            change.state,
            CalDavChangeState::Pending | CalDavChangeState::Checking
        ) {
            continue;
        }
        let Ok(claimed) = store.claim_caldav_change(&change) else {
            continue;
        };
        let result = reconcile(&claimed);
        match result {
            Ok(CalDavWriteResult::Applied(remote)) => {
                if store
                    .complete_caldav_change(&claimed, remote.as_ref(), false)
                    .is_err()
                {
                    write_error = Some(CalDavError::InvalidResponse("The upload result could not be saved. Overlook will check the server before retrying."));
                    break;
                }
            }
            Ok(CalDavWriteResult::Conflict(remote)) => {
                let _ = store.mark_caldav_change(&claimed, CalDavChangeState::Conflict, remote, "This event changed at the provider. Your local changes have been kept. Choose which version to use.".into());
            }
            Err(failure) => {
                let error = failure.error;
                // A transport/server failure may follow a committed write. Keep the
                // frozen attempt and GET its URL before any subsequent mutation.
                let state = if matches!(
                    error,
                    CalDavError::Forbidden
                        | CalDavError::UnexpectedStatus(400..=499)
                        | CalDavError::InvalidConfiguration(_)
                ) {
                    CalDavChangeState::Failed
                } else if !failure.may_have_written && change.state == CalDavChangeState::Pending {
                    CalDavChangeState::Pending
                } else {
                    CalDavChangeState::Checking
                };
                let _ = store.mark_caldav_change(&claimed, state, None, error.to_string());
                let stop = matches!(
                    error,
                    CalDavError::Network
                        | CalDavError::AuthenticationRequired
                        | CalDavError::RateLimited
                        | CalDavError::ServiceUnavailable
                );
                write_error = Some(error);
                if stop {
                    break;
                }
            }
        }
    }
    Ok(write_error)
}

fn error_status(error: &CalDavError) -> (SyncState, u64) {
    match error {
        CalDavError::AuthenticationRequired | CalDavError::Forbidden => {
            (SyncState::SignInRequired, 900)
        }
        CalDavError::Network => (SyncState::Offline, 60),
        CalDavError::RateLimited => (SyncState::Paused(error.to_string()), 300),
        _ => (SyncState::SyncFailed(error.to_string()), 60),
    }
}

fn synchronize_with(
    store: &Store,
    connection: &CalendarConnection,
    download: impl FnOnce() -> Result<CalDavDownload, CalDavError>,
) -> u64 {
    if !store.caldav_connection_current(connection).unwrap_or(false) {
        return 0;
    }
    let _ = store.set_caldav_status(connection, &SyncState::Syncing);
    let result = download();
    match result {
        Ok(download) => match store.apply_caldav_download(connection, &download) {
            Ok(()) => 0,
            Err(_) => {
                let _ = store.set_caldav_status(
                    connection,
                    &SyncState::SyncFailed(
                        "Could not save the calendar download. Cached events have been kept."
                            .into(),
                    ),
                );
                60
            }
        },
        Err(error) => {
            let (state, retry) = error_status(&error);
            let _ = store.set_caldav_status(connection, &state);
            retry
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn calendar_sync_does_not_depend_on_mail_signin_and_failure_keeps_cache() {
        let store = Store::open_in_memory().unwrap();
        let account = Account {
            id: AccountId::from("imap:sync-fixture"),
            provider: ProviderKind::Imap,
            display_name: "Test".into(),
            email: "test@example.invalid".into(),
            created_at: 0,
        };
        store.upsert_account(&account).unwrap();
        store
            .set_sync_state(&account.id, &SyncState::SignInRequired, None)
            .unwrap();
        let connection = store
            .save_calendar_connection(
                &account.id,
                &CalDavSettings {
                    server_url: "https://caldav.fastmail.com/".into(),
                    username: account.email.clone(),
                    credential_source: CalendarCredentialSource::Separate,
                },
                true,
                None,
            )
            .unwrap();
        let snapshot = CalDavSnapshot {
            from: 0,
            to: 100,
            timezone: "test".into(),
            calendars: vec![],
        };
        assert_eq!(
            synchronize_with(&store, &connection, || Ok(snapshot.into())),
            0
        );
        assert!(store
            .caldav_range_cached(&connection.id, 0, 100, "test")
            .unwrap());
        assert_eq!(
            store.account_status(&account.id).unwrap().state,
            SyncState::SignInRequired
        );
        assert_eq!(
            synchronize_with(&store, &connection, || Err(CalDavError::Network)),
            60
        );
        assert!(store
            .caldav_range_cached(&connection.id, 0, 100, "test")
            .unwrap());
        assert_eq!(
            crate::sync::calendar_status(&store, &account.id),
            SyncState::Offline
        );
        assert_eq!(
            store.account_status(&account.id).unwrap().state,
            SyncState::SignInRequired
        );
        store.disconnect_caldav(&connection).unwrap();
        assert_eq!(
            synchronize_with(&store, &connection, || panic!(
                "removed account must not contact server"
            )),
            0
        );
    }
    fn write_event() -> overlook_core::Event {
        use overlook_core::*;
        Event {
            id: EventId::from("caldav:test:event"),
            account_id: AccountId::from("imap:test"),
            calendar_id: CalendarId::from("caldav:test"),
            provider_id: None,
            ical_uid: Some("event-1@example.test".into()),
            summary: "Original".into(),
            description: None,
            location: None,
            start: tz::parse_date("2026-10-22").map(tz::date_start).unwrap() + 12 * 3_600_000,
            end: tz::parse_date("2026-10-22").map(tz::date_start).unwrap() + 13 * 3_600_000,
            all_day: false,
            status: EventStatus::Confirmed,
            organizer: None,
            attendees: vec![],
            html_link: None,
            conference_url: None,
            recurring_event_id: None,
            updated: 0,
            event_type: None,
            color: None,
            reminders: Some(vec![]),
            original_start: None,
            wants_conference: false,
            pending: false,
        }
    }

    #[test]
    fn outbox_coordinator_retains_offline_edits_freezes_ambiguous_uploads_and_resolves_conflicts() {
        use overlook_provider::caldav::prepare_resource;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("calendar.sqlite");
        let store = Store::open(&path).unwrap();
        let account = Account {
            id: AccountId::from("imap:test"),
            provider: ProviderKind::Imap,
            display_name: "Test".into(),
            email: "me@example.test".into(),
            created_at: 0,
        };
        store.upsert_account(&account).unwrap();
        let connection = store
            .save_calendar_connection(
                &account.id,
                &CalDavSettings {
                    server_url: "https://caldav.fastmail.com/".into(),
                    username: account.email.clone(),
                    credential_source: CalendarCredentialSource::Separate,
                },
                true,
                None,
            )
            .unwrap();
        let mut event = write_event();
        event.calendar_id = CalendarId(format!("caldav:{}:personal", connection.id));
        let calendar = Calendar {
            id: event.calendar_id.clone(),
            account_id: account.id.clone(),
            provider_id: Some(ProviderId::from(
                "https://caldav.fastmail.com/calendars/personal/",
            )),
            name: "Personal".into(),
            color: None,
            is_primary: true,
            can_edit: true,
            visible: true,
            sort_order: 0,
            default_reminders: vec![],
        };
        let snapshot = CalDavSnapshot {
            from: event.start - 1,
            to: event.end + 1,
            timezone: tz::cache_key(),
            calendars: vec![CalDavCalendarSnapshot {
                calendar,
                resources: vec![],
            }],
        };
        store.apply_caldav_snapshot(&connection, &snapshot).unwrap();
        let edit = CalDavEdit {
            before: None,
            after: Some(event.clone()),
            scope: RecurrenceScope::This,
        };
        let body = prepare_resource(None, std::slice::from_ref(&edit), &account.email).unwrap();
        store
            .queue_caldav_change(
                &store.caldav_change_for_event(&event, true).unwrap(),
                body,
                edit,
            )
            .unwrap();
        assert!(
            flush_changes(&store, &account.id, |_| Err(CalDavWriteError {
                error: CalDavError::Network,
                may_have_written: false
            }))
            .unwrap()
            .is_some()
        );
        let saved = store.event(&event.id).unwrap().unwrap();
        assert!(store.caldav_change_for_event(&saved, false).is_ok());
        assert_eq!(
            store.caldav_changes(&account.id).unwrap()[0].state,
            CalDavChangeState::Pending
        );
        assert!(
            store
                .events_between(&account.id, event.start - 1, event.end + 1)
                .unwrap()[0]
                .pending
        );
        assert!(
            flush_changes(&store, &account.id, |_| Err(CalDavWriteError {
                error: CalDavError::Network,
                may_have_written: true
            }))
            .unwrap()
            .is_some()
        );
        assert!(store.caldav_change_for_event(&saved, false).is_err());
        drop(store);
        let store = Store::open(&path).unwrap();
        // Failure to GET after reopening cannot unfreeze an earlier uncertain mutation.
        flush_changes(&store, &account.id, |_| {
            Err(CalDavWriteError {
                error: CalDavError::Network,
                may_have_written: false,
            })
        })
        .unwrap();
        assert_eq!(
            store.caldav_changes(&account.id).unwrap()[0].state,
            CalDavChangeState::Checking
        );
        let change = store.caldav_changes(&account.id).unwrap().remove(0);
        let remote = CalDavRemote {
            etag: "\"remote\"".into(),
            icalendar: change.desired.as_ref().unwrap().replace(
                "SUMMARY:Original",
                "SUMMARY:Other client\r\nLOCATION:Remote room",
            ),
        };
        flush_changes(&store, &account.id, |_| {
            Ok(CalDavWriteResult::Conflict(Some(remote.clone())))
        })
        .unwrap();
        let conflict = store.caldav_changes(&account.id).unwrap().remove(0);
        assert_eq!(conflict.state, CalDavChangeState::Conflict);
        assert_eq!(store.event(&event.id).unwrap().unwrap().summary, "Original");
        flush_changes(&store, &account.id, |_| {
            panic!("conflicts require a user choice")
        })
        .unwrap();
        let merged =
            prepare_resource(Some(&remote.icalendar), &conflict.edits, &account.email).unwrap();
        store.rebase_caldav_change(&conflict, merged).unwrap();
        flush_changes(&store, &account.id, |c| {
            Ok(CalDavWriteResult::Applied(Some(CalDavRemote {
                etag: "\"saved\"".into(),
                icalendar: c.desired.clone().unwrap(),
            })))
        })
        .unwrap();
        assert!(store.caldav_changes(&account.id).unwrap().is_empty());
        assert!(!store.event(&event.id).unwrap().unwrap().pending);
        assert!(store.pending_operations(&account.id).unwrap().is_empty());
    }
}
