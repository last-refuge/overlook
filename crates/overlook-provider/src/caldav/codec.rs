//! Read-only mapping of server-expanded iCalendar. Never serialize these projections
//! back to the server: the complete original resource lives alongside them.
use super::CalDavError;
use base64::Engine;
use ical::parser::ical::component::IcalCalendar;
use ical::property::Property;
use jiff::{
    civil::{Date, DateTime},
    tz::TimeZone,
    Span, ToSpan,
};
use overlook_core::*;
use std::collections::HashSet;

const INVALID: CalDavError = CalDavError::InvalidResponse("The server returned calendar data Overlook could not read safely. Your cached events have been kept.");

fn one<'a>(properties: &'a [Property], name: &str) -> Result<Option<&'a Property>, CalDavError> {
    let mut found = properties
        .iter()
        .filter(|p| p.name.eq_ignore_ascii_case(name));
    let first = found.next();
    if found.next().is_some() {
        return Err(INVALID);
    }
    Ok(first)
}

fn value<'a>(properties: &'a [Property], name: &str) -> Result<Option<&'a str>, CalDavError> {
    Ok(one(properties, name)?.and_then(|p| p.value.as_deref()))
}

fn param<'a>(p: &'a Property, name: &str) -> Option<&'a str> {
    p.params
        .as_ref()?
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))?
        .1
        .first()
        .map(String::as_str)
}

fn text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n' | 'N') => out.push('\n'),
                Some(next) => out.push(next),
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn parse_calendar(body: &str) -> Result<IcalCalendar, CalDavError> {
    if body.len() > 2 * 1024 * 1024 {
        return Err(INVALID);
    }
    // Validate complete, balanced framing and component limits using the library's
    // property parser, including quoted parameters and folded lines.
    let mut stack = Vec::new();
    let mut complete = false;
    for (count, property) in ical::PropertyParser::from_reader(body.as_bytes()).enumerate() {
        if count > 50_000 || complete {
            return Err(INVALID);
        }
        let property = property.map_err(|_| INVALID)?;
        match property.name.to_ascii_uppercase().as_str() {
            "BEGIN" => {
                let name = property.value.ok_or(INVALID)?.to_ascii_uppercase();
                if stack.is_empty() && name != "VCALENDAR" {
                    return Err(INVALID);
                }
                stack.push(name);
                if stack.len() > 8 {
                    return Err(INVALID);
                }
            }
            "END" => {
                if stack.pop() != property.value.map(|v| v.to_ascii_uppercase()) {
                    return Err(INVALID);
                }
                if stack.is_empty() {
                    complete = true;
                }
            }
            _ if stack.is_empty() => return Err(INVALID),
            _ => {}
        }
    }
    if !complete || !stack.is_empty() {
        return Err(INVALID);
    }
    let mut parser = ical::IcalParser::new(body.as_bytes());
    let calendar = parser.next().ok_or(INVALID)?.map_err(|_| INVALID)?;
    if parser.next().is_some() || calendar.events.len() > 10_000 || !calendar.timezones.is_empty() {
        return Err(INVALID);
    }
    Ok(calendar)
}

struct DateValue {
    timestamp: i64,
    all_day: bool,
    zone: TimeZone,
}

pub(super) fn property_timestamp(p: &Property) -> Result<i64, CalDavError> {
    Ok(date(p, &tz::current_zone())?.timestamp)
}

fn date(p: &Property, local_zone: &TimeZone) -> Result<DateValue, CalDavError> {
    let raw = p.value.as_deref().ok_or(INVALID)?;
    let is_date = param(p, "VALUE").is_some_and(|v| v.eq_ignore_ascii_case("DATE"));
    if is_date {
        if raw.len() != 8 || param(p, "TZID").is_some() {
            return Err(INVALID);
        }
        let day = Date::strptime("%Y%m%d", raw).map_err(|_| INVALID)?;
        let timestamp = day
            .to_zoned(local_zone.clone())
            .map_err(|_| INVALID)?
            .timestamp()
            .as_millisecond();
        return Ok(DateValue {
            timestamp,
            all_day: true,
            zone: local_zone.clone(),
        });
    }
    let (raw, zone) = if let Some(utc) = raw.strip_suffix('Z') {
        if param(p, "TZID").is_some() {
            return Err(INVALID);
        }
        (utc, TimeZone::UTC)
    } else if let Some(id) = param(p, "TZID") {
        (raw, TimeZone::get(id).map_err(|_| INVALID)?)
    } else {
        (raw, local_zone.clone())
    };
    if raw.len() != 15 {
        return Err(INVALID);
    }
    let timestamp = DateTime::strptime("%Y%m%dT%H%M%S", raw)
        .map_err(|_| INVALID)?
        .to_zoned(zone.clone())
        .map_err(|_| INVALID)?
        .timestamp()
        .as_millisecond();
    Ok(DateValue {
        timestamp,
        all_day: false,
        zone,
    })
}

fn duration(raw: &str) -> Result<Span, CalDavError> {
    // iCalendar durations allow weeks/days/time, never years or calendar months.
    let days = raw.split('T').next().unwrap_or(raw);
    if days.contains(['Y', 'M']) {
        return Err(INVALID);
    }
    raw.parse::<Span>().map_err(|_| INVALID)
}

fn address(p: &Property) -> Option<Address> {
    let raw = p.value.as_deref()?;
    let (scheme, email) = raw.split_once(':')?;
    if !scheme.eq_ignore_ascii_case("mailto")
        || email.is_empty()
        || email.chars().any(char::is_control)
    {
        return None;
    }
    Some(Address {
        email: email.to_owned(),
        name: param(p, "CN").map(|s| s.replace("^n", "\n").replace("^'", "\"").replace("^^", "^")),
    })
}

pub(super) fn events(
    body: &str,
    account: &Account,
    calendar: &Calendar,
    zone: &TimeZone,
) -> Result<Vec<Event>, CalDavError> {
    let parsed = parse_calendar(body)?;
    let mut result = Vec::new();
    let mut identities = HashSet::new();
    for source in parsed.events {
        let props = &source.properties;
        if props.iter().any(|p| {
            ["RRULE", "RDATE", "EXRULE", "EXDATE"]
                .iter()
                .any(|name| p.name.eq_ignore_ascii_case(name))
        }) {
            return Err(CalDavError::InvalidResponse("The calendar server did not expand repeating events. Your cached events have been kept."));
        }
        let uid = value(props, "UID")?
            .filter(|v| !v.is_empty())
            .ok_or(INVALID)?;
        if value(props, "STATUS")? == Some("CANCELLED") {
            continue;
        }
        let start = date(one(props, "DTSTART")?.ok_or(INVALID)?, zone)?;
        let end = if let Some(p) = one(props, "DTEND")? {
            if one(props, "DURATION")?.is_some() {
                return Err(INVALID);
            }
            let end = date(p, zone)?;
            if end.all_day != start.all_day {
                return Err(INVALID);
            }
            end.timestamp
        } else {
            let span = match value(props, "DURATION")? {
                Some(raw) => duration(raw)?,
                None if start.all_day => 1.day(),
                None => 0.seconds(),
            };
            if start.all_day
                && (span.get_hours() != 0 || span.get_minutes() != 0 || span.get_seconds() != 0)
            {
                return Err(INVALID);
            }
            jiff::Timestamp::from_millisecond(start.timestamp)
                .map_err(|_| INVALID)?
                .to_zoned(start.zone)
                .checked_add(span)
                .map_err(|_| INVALID)?
                .timestamp()
                .as_millisecond()
        };
        if end < start.timestamp || (start.all_day && end == start.timestamp) {
            return Err(INVALID);
        }
        let recurrence = one(props, "RECURRENCE-ID")?
            .map(|p| date(p, zone))
            .transpose()?;
        let original = recurrence.as_ref().map(|r| r.timestamp);
        // Recurrence identity is independent of an occurrence's moved DTSTART. The first
        // instance may omit RECURRENCE-ID; its original DTSTART is then the identity.
        let identity =
            serde_json::to_string(&(uid, original.unwrap_or(start.timestamp), start.all_day))
                .map_err(|_| INVALID)?;
        let identity = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(identity);
        if !identities.insert(identity.clone()) {
            return Err(INVALID);
        }
        let organizer = one(props, "ORGANIZER")?.and_then(address);
        let attendees = props
            .iter()
            .filter(|p| p.name.eq_ignore_ascii_case("ATTENDEE"))
            .filter_map(|p| {
                let addr = address(p)?;
                Some(Attendee {
                    is_self: addr.email.eq_ignore_ascii_case(&account.email),
                    is_organizer: organizer
                        .as_ref()
                        .is_some_and(|o| o.email.eq_ignore_ascii_case(&addr.email)),
                    email: addr.email,
                    name: addr.name,
                    response: match param(p, "PARTSTAT") {
                        Some("ACCEPTED") => ResponseStatus::Accepted,
                        Some("DECLINED") => ResponseStatus::Declined,
                        Some("TENTATIVE") => ResponseStatus::Tentative,
                        _ => ResponseStatus::NeedsAction,
                    },
                    optional: param(p, "ROLE") == Some("OPT-PARTICIPANT"),
                })
            })
            .collect();
        let mut reminders = Vec::new();
        for alarm in source.alarms {
            if value(&alarm.properties, "ACTION")? != Some("DISPLAY") {
                continue;
            }
            let Some(trigger) = one(&alarm.properties, "TRIGGER")? else {
                continue;
            };
            if param(trigger, "RELATED").is_some_and(|v| v != "START") {
                continue;
            }
            let Some(raw) = trigger.value.as_deref().and_then(|v| v.strip_prefix('-')) else {
                continue;
            };
            let Ok(span) = duration(raw) else { continue };
            let minutes = i64::from(span.get_weeks()) * 10080
                + i64::from(span.get_days()) * 1440
                + i64::from(span.get_hours()) * 60
                + span.get_minutes();
            if (0..=525600).contains(&minutes) {
                reminders.push(minutes as i32);
            }
        }
        reminders.sort_unstable();
        reminders.dedup();
        let updated = one(props, "LAST-MODIFIED")?
            .or(one(props, "DTSTAMP")?)
            .map(|p| date(p, zone))
            .transpose()?
            .map(|v| v.timestamp)
            .unwrap_or(0);
        result.push(Event {
            id: EventId(format!("{}:{identity}", calendar.id)),
            account_id: account.id.clone(),
            calendar_id: calendar.id.clone(),
            provider_id: Some(ProviderId(identity)),
            ical_uid: Some(uid.to_owned()),
            summary: text(value(props, "SUMMARY")?.unwrap_or("")),
            description: value(props, "DESCRIPTION")?.map(text),
            location: value(props, "LOCATION")?.map(text),
            start: start.timestamp,
            end,
            all_day: start.all_day,
            status: if value(props, "STATUS")? == Some("TENTATIVE") {
                EventStatus::Tentative
            } else {
                EventStatus::Confirmed
            },
            organizer,
            attendees,
            html_link: None,
            conference_url: None,
            recurring_event_id: recurrence.map(|_| uid.to_owned()),
            original_start: original,
            updated,
            event_type: None,
            color: None,
            reminders: Some(reminders),
            wants_conference: false,
            pending: false,
        });
    }
    Ok(result)
}
