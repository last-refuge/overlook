//! Personal calendar writes using the desktop event and CalDAV outboxes.

mod conflicts;
mod fields;
#[cfg(test)]
mod tests;

pub(in crate::mcp) use conflicts::conflict_details;

use overlook_core::{
    new_provider_event_id, time::now, Address, CalDavEdit, Event, EventId, EventStatus, ProviderId,
    ProviderKind, RecurrenceScope,
};
use overlook_store::{McpCalendar, McpWriteTxn, StoreError};
use serde_json::{json, Map, Value};

use super::{ErrorCode, Failure, WriteContext, WritePermission, WriteTool};
use crate::caldav_sync;

const CREATE: &str = "calendar_event_create";
const UPDATE: &str = "calendar_event_update";
const DELETE: &str = "calendar_event_delete";
const RESOLVE: &str = "calendar_conflict_resolve";
const MAX_ID: usize = 128;

pub(super) fn tools() -> Vec<WriteTool> {
    let mut create = fields::schema();
    create["calendar_id"] = id_schema("Exact calendar ID from calendar_calendars_list.");
    let existing = json!({
        "event_id": id_schema("Exact event or occurrence ID from a calendar read."),
        "if_revision": id_schema("Current event revision from a calendar read or write."),
        "scope": {"enum": ["this", "following", "series"], "default": "this"}
    });
    let mut update = fields::schema();
    update
        .as_object_mut()
        .unwrap()
        .extend(existing.as_object().unwrap().clone());
    vec![
        tool(
            CREATE,
            "Create a personal event in the selected writable calendar.",
            create,
            &["calendar_id", "title"],
        ),
        tool(
            UPDATE,
            "Update selected fields on a cached event. CalDAV edits one occurrence at a time.",
            update,
            &["event_id", "if_revision"],
        ),
        tool(
            DELETE,
            "Delete a cached event or occurrence. CalDAV also supports deleting the entire series.",
            existing,
            &["event_id", "if_revision"],
        ),
        conflicts::tool(),
    ]
}

fn tool(
    name: &'static str,
    description: &'static str,
    properties: Value,
    required: &[&str],
) -> WriteTool {
    WriteTool::new(
        name,
        description,
        json!({"type": "object", "properties": properties, "required": required}),
        WritePermission::Calendar,
    )
}

fn id_schema(description: &str) -> Value {
    json!({"type": "string", "minLength": 1, "maxLength": MAX_ID, "description": description})
}

pub(super) fn execute(
    ctx: &WriteContext<'_>,
    tx: &mut McpWriteTxn<'_, '_>,
    name: &str,
    args: &Map<String, Value>,
) -> Result<Value, Failure> {
    match name {
        CREATE => create(ctx, tx, args),
        UPDATE | DELETE => change(ctx, tx, name, args),
        RESOLVE => conflicts::resolve(ctx, tx, args),
        _ => Err(invalid("Unknown calendar write tool")),
    }
}

fn create(
    ctx: &WriteContext<'_>,
    tx: &mut McpWriteTxn<'_, '_>,
    args: &Map<String, Value>,
) -> Result<Value, Failure> {
    let calendar_id = id(args, "calendar_id")?;
    let calendar = tx
        .find_calendar(|raw| ctx.id("calendar", raw) == calendar_id)?
        .ok_or_else(|| not_cached("Calendar"))?;
    let backend = backend(ctx, &calendar)?;
    let provider_id = new_provider_event_id();
    let mut event = Event {
        id: EventId(format!("{}:{provider_id}", calendar.id.0)),
        account_id: ctx.account.id.clone(),
        calendar_id: calendar.id,
        provider_id: Some(ProviderId(provider_id)),
        ical_uid: None,
        summary: String::new(),
        description: None,
        location: None,
        start: 0,
        end: 0,
        all_day: false,
        status: EventStatus::Confirmed,
        organizer: (backend == Backend::Sample)
            .then(|| Address::new(Some(ctx.account.display_name.clone()), &ctx.account.email)),
        attendees: vec![],
        html_link: None,
        conference_url: None,
        recurring_event_id: None,
        updated: now(),
        event_type: None,
        color: None,
        reminders: None,
        original_start: None,
        wants_conference: false,
        pending: true,
    };
    fields::apply(tx, args, &mut event, backend, true)?;
    if backend == Backend::CalDav {
        return queue_caldav(ctx, tx, None, Some(event), RecurrenceScope::This);
    }
    let operation = tx.create_native_event(&event)?;
    Ok(result(ctx, &event, &operation.id.0, "queued", true))
}

fn change(
    ctx: &WriteContext<'_>,
    tx: &mut McpWriteTxn<'_, '_>,
    name: &str,
    args: &Map<String, Value>,
) -> Result<Value, Failure> {
    let event_id = id(args, "event_id")?;
    let revision = id(args, "if_revision")?;
    let scope = match args.get("scope") {
        None => RecurrenceScope::This,
        Some(value) => match value.as_str() {
            Some("this") => RecurrenceScope::This,
            Some("following") => RecurrenceScope::Following,
            Some("series") => RecurrenceScope::All,
            _ => return Err(invalid("Choose this, following or series for scope")),
        },
    };
    let before = tx
        .find_event(|raw| ctx.id("event", raw) == event_id)?
        .filter(|e| e.status != EventStatus::Cancelled)
        .ok_or_else(|| not_cached("Event"))?;
    let calendar = tx
        .find_calendar(|raw| raw == before.calendar_id.0)?
        .ok_or_else(|| not_cached("Calendar"))?;
    let backend = backend(ctx, &calendar)?;
    if revision != ctx.event_revision(&before) {
        return Err(Failure::new(
            ErrorCode::Conflict,
            "The event changed. Read the current event before retrying.",
        ));
    }
    let deleting = name == DELETE;
    if scope != RecurrenceScope::This
        && !(backend == Backend::CalDav && deleting && scope == RecurrenceScope::All)
    {
        return Err(Failure::new(ErrorCode::Unsupported, "The calendar provider does not support this recurrence scope for the requested change."));
    }
    if before.is_working_location()
        || before.attendees.iter().any(|attendee| !attendee.is_self)
        || before
            .organizer
            .as_ref()
            .is_some_and(|owner| !owner.email.eq_ignore_ascii_case(&ctx.account.email))
    {
        return Err(Failure::new(
            ErrorCode::Unsupported,
            "Calendar event writes support personal events without guests.",
        ));
    }
    if backend == Backend::Sample && before.provider_id.is_none() {
        return Err(Failure::new(
            ErrorCode::Unsupported,
            "The cached event has no provider identity for this change.",
        ));
    }

    let after = if deleting {
        None
    } else {
        let mut after = before.clone();
        fields::apply(tx, args, &mut after, backend, false)?;
        after.updated = now();
        after.pending = true;
        Some(after)
    };
    if backend == Backend::CalDav {
        return queue_caldav(ctx, tx, Some(before), after, scope);
    }
    let operation = if let Some(event) = &after {
        tx.update_native_event(event, scope)?
    } else {
        tx.delete_native_event(&before, scope)?
    };
    Ok(result(
        ctx,
        after.as_ref().unwrap_or(&before),
        &operation.id.0,
        "queued",
        !deleting,
    ))
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Backend {
    Sample,
    CalDav,
}

fn backend(ctx: &WriteContext<'_>, calendar: &McpCalendar) -> Result<Backend, Failure> {
    let backend = match (ctx.account.provider, calendar.caldav) {
        (ProviderKind::Imap | ProviderKind::Pop3, true) => Backend::CalDav,
        (ProviderKind::Sample, false) if !calendar.id.0.starts_with("caldav:") => Backend::Sample,
        _ => {
            return Err(Failure::new(
                ErrorCode::Unsupported,
                "Calendar writes are unavailable for this calendar provider.",
            ))
        }
    };
    if !calendar.can_edit {
        return Err(Failure::new(
            ErrorCode::AccessDenied,
            "This calendar is read-only.",
        ));
    }
    Ok(backend)
}

fn queue_caldav(
    ctx: &WriteContext<'_>,
    tx: &mut McpWriteTxn<'_, '_>,
    before: Option<Event>,
    after: Option<Event>,
    scope: RecurrenceScope,
) -> Result<Value, Failure> {
    let creating = before.is_none();
    let mut edit = CalDavEdit {
        before,
        after,
        scope,
    };
    let event = caldav_sync::initialize_edit(ctx.account, &mut edit).map_err(invalid)?;
    let change = tx
        .caldav_change_for_event(event, creating)
        .map_err(caldav_storage)?;
    let desired = caldav_sync::prepare_change(ctx.account, &change, &edit)
        .map_err(|error| Failure::new(ErrorCode::Unsupported, error.to_string()))?;
    let status = if change.base.is_none() && desired.is_none() {
        "cancelled"
    } else {
        "queued"
    };
    let event = edit
        .after
        .as_ref()
        .or(edit.before.as_ref())
        .expect("initialized edit");
    let response = result(ctx, event, change.id.as_str(), status, edit.after.is_some());
    tx.queue_caldav_change(&change, desired, edit)
        .map_err(caldav_storage)?;
    Ok(response)
}

fn result(
    ctx: &WriteContext<'_>,
    event: &Event,
    operation: &str,
    status: &str,
    include_revision: bool,
) -> Value {
    let mut value = json!({
        "event_id": ctx.id("event", &event.id.0),
        "calendar_id": ctx.id("calendar", &event.calendar_id.0),
        "operation_id": ctx.id("operation", operation),
        "status": status,
    });
    if include_revision {
        value["revision"] = json!(ctx.event_revision(event));
    }
    value
}

fn invalid(message: impl Into<String>) -> Failure {
    Failure::new(ErrorCode::InvalidRequest, message)
}

fn check_id(input: &str) -> bool {
    !input.is_empty() && input.len() <= MAX_ID && !input.chars().any(char::is_control)
}

fn id<'a>(args: &'a Map<String, Value>, field: &str) -> Result<&'a str, Failure> {
    args.get(field)
        .and_then(Value::as_str)
        .filter(|input| check_id(input))
        .ok_or_else(|| invalid(format!("Provide a valid {field}")))
}

fn not_cached(kind: &str) -> Failure {
    Failure::new(
        ErrorCode::NotCached,
        format!("{kind} is not cached for this account"),
    )
}

fn caldav_storage(error: StoreError) -> Failure {
    match error {
        StoreError::Invalid(message) => Failure::new(ErrorCode::Conflict, message),
        StoreError::NotFound(_) => not_cached("CalDAV resource"),
        error => Failure::new(ErrorCode::Internal, {
            tracing::warn!("Could not commit calendar change: {error}");
            "Could not commit the calendar change"
        }),
    }
}
