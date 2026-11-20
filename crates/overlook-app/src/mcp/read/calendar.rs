//! Calendar views of the local Store. Both providers and the UI can change the cache
//! independently between requests, including between pages.

mod window;

use super::{parse, storage, ErrorCode, Failure, ReadContext, ReadTool};
use overlook_core::{
    time::now, tz, Account, Calendar, CalendarId, Event, ProviderKind, ResponseStatus,
};
use overlook_provider::sample::sample_events;
use overlook_store::Store;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use window::{availability, gaps, merge, Window};

const CALENDARS: &str = "calendar_calendars_list";
const EVENTS: &str = "calendar_events_list";
const EVENT: &str = "calendar_event_get";
const AVAILABILITY: &str = "calendar_availability";
const CHANGES: &str = "calendar_changes_list";
const MAX_PAGE: u64 = 50;
const MAX_QUERY: usize = 200;
const MAX_ID: usize = 256;

pub(super) fn tools() -> Vec<ReadTool> {
    let mut list = json!({
        "limit": {"type": "integer", "minimum": 1, "maximum": MAX_PAGE},
        "cursor": {"type": "string", "description": "Continuation cursor returned by this tool."}
    });
    let calendars = list.clone();
    list["start"] = json!({
        "type": "string", "format": "date-time",
        "description": "Inclusive RFC 3339 date-time with a UTC offset, at millisecond precision or coarser."
    });
    list["end"] = json!({
        "type": "string", "format": "date-time",
        "description": "Exclusive RFC 3339 date-time with a UTC offset, up to 366 elapsed days after start."
    });
    list["calendar_id"] = json!({
        "type": "string", "maxLength": MAX_ID,
        "description": "Opaque calendar ID. Omit to include all cached calendars, including calendars hidden in Overlook."
    });
    let mut events = list.clone();
    events["query"] = json!({
        "type": "string", "maxLength": MAX_QUERY,
        "description": "Case-insensitive substring in the cached title, location or description."
    });

    vec![
        tool(CALENDARS, "List calendars in the account's local cache, including hidden calendars.", calendars, &[]),
        tool(CHANGES, "List this account's queued CalDAV changes and conflicts.", json!({
            "limit":{"type":"integer","minimum":1,"maximum":MAX_PAGE},
            "cursor":{"type":"string"}
        }), &[]),
        tool(EVENTS, "List cached events and recurrence instances overlapping an explicit time range. Coverage describes where the local cache can account for missing events.", events, &["start", "end"]),
        tool(EVENT, "Read a cached event or recurrence instance by the opaque event ID from calendar_events_list. UTC times and any available all-day dates have exclusive ends.",
            json!({"event_id": {"type": "string", "maxLength": MAX_ID}}), &["event_id"]),
        tool(AVAILABILITY, "Derive busy, free-in-cache, and unknown intervals from cached events and verified calendar coverage. Availability is local and may be stale.", list, &["start", "end"]),
    ]
}

fn tool(
    name: &'static str,
    description: &'static str,
    properties: Value,
    required: &[&str],
) -> ReadTool {
    ReadTool::new(
        name,
        description,
        json!({
            "type": "object", "properties": properties, "required": required, "additionalProperties": false
        }),
    )
}

pub(super) fn execute(
    ctx: &ReadContext<'_>,
    name: &str,
    args: &Map<String, Value>,
) -> Result<Value, Failure> {
    match name {
        CALENDARS => calendars_list(ctx, args),
        CHANGES => changes_list(ctx, args),
        EVENTS | AVAILABILITY => range_read(ctx, name, args),
        EVENT => event_get(ctx, args),
        _ => Err(Failure::new(
            ErrorCode::InvalidRequest,
            "Unknown calendar tool",
        )),
    }
}

fn changes_list(ctx: &ReadContext<'_>, args: &Map<String, Value>) -> Result<Value, Failure> {
    let changes = ctx.store.caldav_changes(&ctx.account.id).map_err(storage)?;
    let items = changes
        .iter()
        .map(|change| {
            json!({
                "operation_id":ctx.id("operation", change.id.as_str()),
                "calendar_id":ctx.id("calendar", change.calendar_id.as_str()),
                "revision":change.revision,
                "status":change.state,
                "title":clean(&change.title(), 180)
            })
        })
        .collect();
    ctx.page::<Value>(items, CHANGES, args)
}

fn calendars_list(ctx: &ReadContext<'_>, args: &Map<String, Value>) -> Result<Value, Failure> {
    let mut calendars = ctx.store.calendars(&ctx.account.id).map_err(storage)?;
    calendars.sort_by(|a, b| {
        b.is_primary
            .cmp(&a.is_primary)
            .then_with(|| a.sort_order.cmp(&b.sort_order))
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.id.0.cmp(&b.id.0))
    });
    let count = calendars.len();
    let mut page = ctx.page(
        calendars.iter().map(|c| json!({
            "calendar_id": ctx.id("calendar", &c.id.0),
            "name": clean(&c.name, 220),
            "color": c.color.as_deref().filter(|s| s.len() == 7 && s.starts_with('#') && s.as_bytes()[1..].iter().all(u8::is_ascii_hexdigit)),
            "is_primary": c.is_primary,
            "can_edit": c.can_edit,
            "visible": c.visible,
        })).collect(),
        CALENDARS, args,
    )?;
    page["coverage"] = json!({"scope": "cached_calendar_directory", "calendar_count": count});
    Ok(page)
}

#[derive(Deserialize)]
struct RangeArgs {
    start: String,
    end: String,
    calendar_id: Option<String>,
    query: Option<String>,
}

fn range_read(
    ctx: &ReadContext<'_>,
    name: &str,
    args: &Map<String, Value>,
) -> Result<Value, Failure> {
    let request: RangeArgs = parse(args)?;
    let range = Window::parse(&request.start, &request.end)?;
    let query = match request.query {
        Some(query) if name == EVENTS && query.chars().count() <= MAX_QUERY => {
            let query = query.trim().to_lowercase();
            (!query.is_empty()).then_some(query)
        }
        Some(_) => return Err(invalid("Invalid calendar query")),
        None => None,
    };
    let calendar = if let Some(id) = &request.calendar_id {
        validate_id(id)?;
        let calendars = ctx.store.calendars(&ctx.account.id).map_err(storage)?;
        Some(
            calendars
                .into_iter()
                .find(|c| ctx.id("calendar", &c.id.0) == *id)
                .ok_or_else(|| not_cached("Calendar"))?
                .id,
        )
    } else {
        None
    };
    let (zone, zone_key) = tz::zone_with_cache_key();
    let cache = ctx
        .store
        .calendar_read_cache(
            &ctx.account.id,
            calendar.as_ref(),
            range.start,
            range.end,
            &zone_key,
        )
        .map_err(storage)?;
    if calendar.is_some() && cache.calendar_ids.is_empty() {
        return Err(not_cached("Calendar"));
    }

    let caldav_windows: Vec<_> = cache.caldav_windows.into_iter().map(Window::from).collect();
    let sample = if ctx.account.provider == ProviderKind::Sample && cache.calendar_ids.len() == 1 {
        ctx.store
            .setting::<Window>(&sample_key(&cache.calendar_ids[0]))
            .map_err(storage)?
    } else {
        None
    };
    let (source, windows) = if let Some(sample) = sample {
        ("sample_fixture", vec![sample])
    } else if cache.caldav_windows_apply_to_all_selected && !caldav_windows.is_empty() {
        ("caldav_snapshot", caldav_windows.clone())
    } else {
        ("unverified_cache", vec![])
    };
    let covered = merge(
        windows
            .into_iter()
            .filter_map(|w| w.intersection(range))
            .collect(),
    );
    let gaps = gaps(range, &covered);
    let status = if covered.is_empty() {
        "unknown"
    } else if gaps.is_empty() {
        "complete"
    } else {
        "partial"
    };

    let projected_zone = projection(cache.projection, &zone)?;
    let dates_zone = |event: &Event| {
        if !event.calendar_id.0.starts_with("caldav:") {
            Some(&projected_zone)
        } else if !event.pending
            && caldav_windows
                .iter()
                .any(|w| w.intersection(Window::event(event)).is_some())
        {
            Some(&zone)
        } else {
            None
        }
    };

    let items = if name == AVAILABILITY {
        let busy = cache
            .events
            .iter()
            .filter(|e| {
                !e.is_working_location() && e.my_response() != Some(ResponseStatus::Declined)
            })
            .filter(|e| !e.all_day || dates_zone(e).is_some())
            .filter_map(|e| Window::new(e.start, e.end).intersection(range))
            .collect();
        availability(range, &covered, busy)
    } else {
        cache
            .events
            .iter()
            .filter(|e| {
                query.as_ref().is_none_or(|q| {
                    [
                        Some(e.summary.as_str()),
                        e.location.as_deref(),
                        e.description.as_deref(),
                    ]
                    .into_iter()
                    .flatten()
                    .any(|s| s.to_lowercase().contains(q))
                })
            })
            .map(|e| event_value(ctx, e, dates_zone(e), false))
            .collect()
    };
    let mut page = ctx.page(items, name, args)?;
    page["coverage"] = json!({
        "status": status,
        "partial": status != "complete",
        "source": source,
        "calendar_id": request.calendar_id,
        "calendar_count": cache.calendar_ids.len(),
        "requested_window": range.value(),
        "covered_windows": covered.iter().map(Window::value).collect::<Vec<_>>(),
        "gaps": gaps.iter().map(Window::value).collect::<Vec<_>>(),
    });
    page["time_semantics"] = json!({
        "timestamps": "UTC instants, inclusive start and exclusive end. Zero-duration events are listed at their start instant.",
        "all_day": "Date-only spans use an exclusive end date in the cached calendar projection time zone, when known.",
        "recurrence": "Cached occurrence records, including moved instances. No expansion beyond the provider's cached instances.",
    });
    if name == AVAILABILITY {
        page["availability_semantics"] = json!("Busy intervals are inferred from cached nonzero-duration events, including tentative events. Cancelled and declined events and working-location entries are excluded. The cache does not include provider free/busy transparency. Free-in-cache means no overlapping blocking event within covered calendars and time.");
    }
    Ok(page)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EventArgs {
    event_id: String,
}

fn event_get(ctx: &ReadContext<'_>, args: &Map<String, Value>) -> Result<Value, Failure> {
    let args: EventArgs = parse(args)?;
    validate_id(&args.event_id)?;
    let (zone, zone_key) = tz::zone_with_cache_key();
    let (event, projected_zone, caldav_projection) = ctx
        .store
        .find_cached_calendar_event(&ctx.account.id, &zone_key, |raw| {
            ctx.id("event", raw) == args.event_id
        })
        .map_err(storage)?;
    let event = event.ok_or_else(|| not_cached("Event"))?;
    let projected_zone = projection(projected_zone, &zone)?;
    let dates_zone = if event.calendar_id.0.starts_with("caldav:") {
        (caldav_projection && !event.pending).then_some(&zone)
    } else {
        Some(&projected_zone)
    };
    Ok(event_value(ctx, &event, dates_zone, true))
}

fn event_value(
    ctx: &ReadContext<'_>,
    event: &Event,
    zone: Option<&tz::TimeZone>,
    detail: bool,
) -> Value {
    let recurrence = event.recurring_event_id.as_ref().map(|raw| {
        let cal = &event.calendar_id.0;
        json!({
            "series_id": ctx.id("calendar-series", &format!("{}:{cal}{raw}", cal.len())),
            "original_start": event.original_start.map(tz::to_rfc3339),
            "original_start_date": event.original_start.filter(|_| event.all_day)
                .and_then(|s| zone.map(|zone| tz::in_zone(s, zone).date().to_string())),
        })
    });
    let mut value = json!({
        "event_id": ctx.id("event", &event.id.0),
        "revision": crate::mcp::write::event_revision(ctx.key, ctx.account_id, event),
        "calendar_id": ctx.id("calendar", &event.calendar_id.0),
        "title": clean(&event.summary, 180),
        "location": event.location.as_deref().map(|s| clean(s, 140)),
        "start": tz::to_rfc3339(event.start),
        "end": tz::to_rfc3339(event.end),
        "all_day": event.all_day,
        "status": event.status,
        "my_response": event.my_response(),
        "pending": event.pending,
    });
    if event.all_day {
        value["all_day_dates"] = zone.map_or(Value::Null, |zone| {
            json!({
                "start_date": tz::in_zone(event.start, zone).date().to_string(),
                "end_date": tz::in_zone(event.end, zone).date().to_string(),
                "time_zone": zone.iana_name().filter(|name| *name != "Local"),
            })
        });
    }
    if let Some(recurrence) = recurrence {
        value["recurrence"] = recurrence;
    }
    if event.is_working_location() {
        value["event_type"] = json!("working-location");
    }
    if detail {
        value["description"] = json!(event.description.as_deref().map(|s| clean(s, 4096)));
        value["organizer"] = json!(event.organizer.as_ref().map(|o| json!({
            "name": o.name.as_deref().map(|s| clean(s, 140)), "email": clean(&o.email, 254)
        })));
        value["attendees"] = json!(event
            .attendees
            .iter()
            .take(24)
            .map(|a| json!({
                "email": clean(&a.email, 254), "name": a.name.as_deref().map(|s| clean(s, 140)),
                "response": a.response, "is_self": a.is_self, "optional": a.optional,
            }))
            .collect::<Vec<_>>());
        value["attendees_truncated"] = json!(event.attendees.len() > 24);
    }
    value
}

fn projection(
    saved: Option<tz::ZoneDefinition>,
    fallback: &tz::TimeZone,
) -> Result<tz::TimeZone, Failure> {
    saved
        .map(|zone| zone.resolve().map_err(storage))
        .unwrap_or_else(|| Ok(fallback.clone()))
}

fn clean(input: &str, max_bytes: usize) -> String {
    input[..input.floor_char_boundary(max_bytes.min(input.len()))]
        .chars()
        .filter(|&c| !c.is_control() || c == '\n' || c == '\t')
        .collect()
}

fn validate_id(id: &str) -> Result<(), Failure> {
    if id.is_empty() || id.len() > MAX_ID {
        return Err(invalid("Invalid calendar record ID"));
    }
    Ok(())
}

fn invalid(message: &'static str) -> Failure {
    Failure::new(ErrorCode::InvalidRequest, message)
}

fn not_cached(record: &str) -> Failure {
    Failure::new(
        ErrorCode::NotCached,
        format!("{record} is not cached for this account"),
    )
}

fn sample_key(calendar: &CalendarId) -> String {
    format!("mcp_sample_calendar_window:{}", calendar.0)
}

pub(super) fn seed_sample(store: &Store, account: &Account) -> anyhow::Result<()> {
    anyhow::ensure!(
        account.provider == ProviderKind::Sample,
        "Expected a fictional calendar account"
    );
    let anchor = now();
    let mut events = sample_events(account, anchor);
    let personal = account.id.0 == "sample-secondary";
    let Some(first_event) = events.first() else {
        anyhow::bail!("Sample calendar fixture has no events");
    };
    let calendar = Calendar {
        id: first_event.calendar_id.clone(),
        account_id: account.id.clone(),
        provider_id: None,
        name: if personal {
            "Personal Calendar"
        } else {
            "Work Calendar"
        }
        .into(),
        color: None,
        is_primary: true,
        can_edit: !personal,
        visible: true,
        sort_order: 0,
        default_reminders: Vec::new(),
    };
    events.retain(|e| {
        matches!(
            e.summary.as_str(),
            "Haircut" | "Lunch with Sophie" | "Trail walk"
        ) == personal
    });
    for event in &mut events {
        if event.summary == "Weekly build leaders" {
            event.recurring_event_id = Some("sample-build-leaders".into());
            event.original_start = Some(event.start);
        }
    }
    store.replace_calendars(&account.id, std::slice::from_ref(&calendar))?;
    store.upsert_events(&events)?;
    let start = tz::week_start(anchor, true);
    store.set_setting(
        &sample_key(&calendar.id),
        &Window::new(start, tz::add_days(start, 14)),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests;
