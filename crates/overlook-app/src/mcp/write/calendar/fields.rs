use overlook_core::{tz, Event};
use overlook_store::McpWriteTxn;
use serde_json::{json, Map, Value};

use super::{invalid, Backend, Failure};

const TITLE: usize = 512;
const LOCATION: usize = 512;
const DESCRIPTION: usize = 8 * 1024;
const ZONE: usize = 128;

pub(super) fn schema() -> Value {
    json!({
        "title": {"type": "string", "minLength": 1, "maxLength": TITLE},
        "location": {"type": ["string", "null"], "maxLength": LOCATION},
        "description": {"type": ["string", "null"], "maxLength": DESCRIPTION},
        "start": {"type": "string", "format": "date-time", "description": "Timed start in RFC 3339 with a UTC offset. CalDAV uses whole-second precision. Supply both start and end."},
        "end": {"type": "string", "format": "date-time", "description": "Exclusive timed end in RFC 3339 with a UTC offset."},
        "start_date": {"type": "string", "format": "date", "description": "All-day start in YYYY-MM-DD. Supply both dates and time_zone."},
        "end_date": {"type": "string", "format": "date", "description": "Exclusive all-day end in YYYY-MM-DD."},
        "time_zone": {"type": "string", "maxLength": ZONE, "description": "IANA time zone for all-day dates, for example America/Phoenix or UTC."},
    })
}

pub(super) fn apply(
    tx: &McpWriteTxn<'_, '_>,
    args: &Map<String, Value>,
    event: &mut Event,
    backend: Backend,
    creating: bool,
) -> Result<(), Failure> {
    let title = args.get("title");
    let location = args.get("location");
    let description = args.get("description");
    let timed = args.contains_key("start") || args.contains_key("end");
    let all_day = ["start_date", "end_date", "time_zone"]
        .iter()
        .any(|key| args.contains_key(*key));
    if timed && all_day {
        return Err(invalid(
            "Use either timed start and end or all-day dates with a time_zone",
        ));
    }
    if creating && (!timed && !all_day || title.is_none()) {
        return Err(invalid("Provide a title and the event's start and end"));
    }
    if !creating && !timed && !all_day && [title, location, description].iter().all(|v| v.is_none())
    {
        return Err(invalid("Provide at least one event field to update"));
    }
    if let Some(title) = title {
        let title = text(title, "title", TITLE, false)?;
        if title.trim().is_empty() {
            return Err(invalid("Provide a nonempty title"));
        }
        event.summary = title.to_owned();
    }
    if let Some(location) = location {
        event.location = optional_text(location, "location", LOCATION, false)?;
    }
    if let Some(description) = description {
        event.description = optional_text(description, "description", DESCRIPTION, true)?;
    }
    if timed {
        event.start = timestamp(args, "start", backend)?;
        event.end = timestamp(args, "end", backend)?;
        event.all_day = false;
    } else if all_day {
        let requested_zone = args
            .get("time_zone")
            .and_then(Value::as_str)
            .filter(|zone| zone.len() <= ZONE)
            .and_then(|zone| tz::TimeZone::get(zone).ok())
            .ok_or_else(|| invalid("Supply a valid IANA time_zone for all-day dates"))?;
        let projection = if backend == Backend::Sample {
            tx.calendar_projection()?
                .map(|zone| zone.resolve().map_err(super::super::storage))
                .transpose()?
                .unwrap_or_else(tz::current_zone)
        } else {
            tz::current_zone()
        };
        event.start = date_start(args, "start_date", &requested_zone, &projection)?;
        event.end = date_start(args, "end_date", &requested_zone, &projection)?;
        event.all_day = true;
    }
    if (timed || all_day) && event.start >= event.end {
        return Err(invalid("The exclusive end must be after the start"));
    }
    Ok(())
}

fn timestamp(args: &Map<String, Value>, field: &str, backend: Backend) -> Result<i64, Failure> {
    let invalid_time = || invalid(format!("Provide {field} in RFC 3339 with a UTC offset"));
    let input = args
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(invalid_time)?;
    let tail = match input.as_bytes() {
        [_, _, _, _, b'-', _, _, b'-', _, _, b'T' | b't', _, _, b':', _, _, b':', _, _, rest @ ..] => {
            rest
        }
        _ => return Err(invalid_time()),
    };
    let offset = if let Some(fraction) = tail.strip_prefix(b".") {
        let length = fraction.iter().take_while(|c| c.is_ascii_digit()).count();
        if !(1..=3).contains(&length) {
            return Err(invalid(
                "Use millisecond precision or coarser for timed events",
            ));
        }
        &fraction[length..]
    } else {
        tail
    };
    let millis = match offset {
        [b'Z' | b'z'] | [b'+' | b'-', _, _, b':', _, _] => tz::parse_rfc3339(input),
        _ => None,
    }
    .ok_or_else(invalid_time)?;
    if backend == Backend::CalDav && millis % 1000 != 0 {
        return Err(invalid(
            "CalDAV timed events require whole-second precision",
        ));
    }
    Ok(millis)
}

fn date_start(
    args: &Map<String, Value>,
    field: &str,
    requested_zone: &tz::TimeZone,
    projection: &tz::TimeZone,
) -> Result<i64, Failure> {
    let invalid_date = || invalid(format!("Provide {field} in YYYY-MM-DD"));
    let input = args
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(invalid_date)?;
    let date = tz::parse_date(input)
        .filter(|date| input.len() == 10 && date.to_string() == input)
        .ok_or_else(invalid_date)?;
    if date
        .to_zoned(requested_zone.clone())
        .ok()
        .is_none_or(|z| z.date() != date)
    {
        return Err(invalid(format!(
            "{field} does not exist in the requested time_zone"
        )));
    }
    // All-day values retain the supplied civil dates. Event rows use timestamps
    // at those dates in the calendar's display projection.
    date.to_zoned(projection.clone())
        .ok()
        .filter(|z| z.date() == date)
        .map(|z| tz::to_millis(&z))
        .ok_or_else(|| {
            invalid(format!(
                "{field} does not exist in the calendar display time zone"
            ))
        })
}

fn text<'a>(
    value: &'a Value,
    field: &str,
    max: usize,
    multiline: bool,
) -> Result<&'a str, Failure> {
    value
        .as_str()
        .filter(|s| {
            s.len() <= max
                && s.chars()
                    .all(|c| !c.is_control() || multiline && matches!(c, '\n' | '\r' | '\t'))
        })
        .ok_or_else(|| invalid(format!("Provide valid {field} text up to {max} bytes")))
}

fn optional_text(
    value: &Value,
    field: &str,
    max: usize,
    multiline: bool,
) -> Result<Option<String>, Failure> {
    if value.is_null() {
        return Ok(None);
    }
    Ok(Some(text(value, field, max, multiline)?.to_owned()))
}
