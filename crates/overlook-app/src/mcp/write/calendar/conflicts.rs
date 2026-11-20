use overlook_core::{CalDavChange, CalDavChangeState};
use overlook_store::{McpCalDavResolution, McpWriteTxn};
use serde_json::{json, Map, Value};

use super::{
    backend, caldav_storage, id, id_schema, invalid, not_cached, ErrorCode, Failure, WriteContext,
    WriteTool, RESOLVE,
};
use crate::caldav_sync;
use crate::mcp::read::ReadContext;

pub(super) fn tool() -> WriteTool {
    super::tool(
        RESOLVE,
        "Resolve a CalDAV conflict after reviewing the local and provider versions.",
        json!({
            "operation_id": id_schema("Operation ID from the queued calendar write."),
            "if_revision": id_schema("Current conflict revision from calendar_conflict_get."),
            "resolution": {"enum": ["keep-local", "take-remote"]}
        }),
        &["operation_id", "if_revision", "resolution"],
    )
}

/// Detail handler for the account-scoped calendar_conflict_get read tool.
pub(in crate::mcp) fn conflict_details(
    ctx: &ReadContext<'_>,
    args: &Map<String, Value>,
) -> Result<Value, Failure> {
    let operation_id = id(args, "operation_id")?;
    let change = ctx
        .store
        .find_caldav_change(&ctx.account.id, |raw| {
            ctx.id("operation", raw) == operation_id
        })?
        .filter(|change| change.state == CalDavChangeState::Conflict)
        .ok_or_else(|| not_cached("Calendar conflict"))?;
    let edit = change
        .edits
        .last()
        .ok_or_else(|| not_cached("Calendar change"))?;
    let local = edit.after.as_ref().or(edit.before.as_ref()).map(|event| {
        json!({
            "event_id": ctx.id("event", &event.id.0),
            "title": clean(&event.summary, 512),
            "description": event.description.as_deref().map(|text| clean(text, 4096)),
            "location": event.location.as_deref().map(|text| clean(text, 512)),
            "start": overlook_core::tz::to_rfc3339(event.start),
            "end": overlook_core::tz::to_rfc3339(event.end),
            "all_day": event.all_day,
        })
    });
    let remote = change.remote.as_ref().map(|resource| {
        clean(
            &overlook_provider::caldav::resource_preview(&resource.icalendar),
            2300,
        )
    });
    Ok(json!({
        "operation_id": operation_id,
        "calendar_id": ctx.id("calendar", &change.calendar_id.0),
        "revision": revision(&change, |raw| ctx.id("calendar-conflict-revision", raw)),
        "local": local,
        "local_deleted": change.desired.is_none() || edit.after.is_none(),
        "remote_preview": remote,
        "remote_deleted": change.remote.is_none(),
    }))
}

pub(super) fn resolve(
    ctx: &WriteContext<'_>,
    tx: &mut McpWriteTxn<'_, '_>,
    args: &Map<String, Value>,
) -> Result<Value, Failure> {
    let operation_id = id(args, "operation_id")?;
    let expected = id(args, "if_revision")?;
    let keep_local = match args.get("resolution").and_then(Value::as_str) {
        Some("keep-local") => true,
        Some("take-remote") => false,
        _ => return Err(invalid("Choose keep-local or take-remote for resolution")),
    };
    let change = tx
        .find_caldav_change(|raw| ctx.id("operation", raw) == operation_id)?
        .filter(|change| change.state == CalDavChangeState::Conflict)
        .ok_or_else(|| not_cached("Calendar conflict"))?;
    if expected != revision(&change, |raw| ctx.id("calendar-conflict-revision", raw)) {
        return Err(Failure::new(
            ErrorCode::Conflict,
            "The conflict changed. Read the current conflict before retrying.",
        ));
    }
    let (resolution, status) = if keep_local {
        let calendar = tx
            .find_calendar(|raw| raw == change.calendar_id.0)?
            .ok_or_else(|| not_cached("Calendar"))?;
        backend(ctx, &calendar)?;
        let desired = caldav_sync::prepare_rebase(ctx.account, &change)
            .map_err(|error| Failure::new(ErrorCode::Conflict, error.to_string()))?;
        (McpCalDavResolution::KeepLocal(desired), "queued")
    } else {
        (McpCalDavResolution::TakeRemote, "resolved")
    };
    tx.resolve_caldav_conflict(&change, resolution)
        .map_err(caldav_storage)?;
    Ok(json!({
        "operation_id": operation_id,
        "calendar_id": ctx.id("calendar", &change.calendar_id.0),
        "status": status,
        "resolution": if keep_local { "keep-local" } else { "take-remote" },
    }))
}

fn revision(change: &CalDavChange, opaque_id: impl FnOnce(&str) -> String) -> String {
    let token = serde_json::to_string(change).expect("calendar change is serializable");
    opaque_id(&token)
}

fn clean(input: &str, max: usize) -> String {
    input[..input.floor_char_boundary(input.len().min(max))]
        .chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .collect()
}
