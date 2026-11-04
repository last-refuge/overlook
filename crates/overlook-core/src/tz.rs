//! Local time with real time-zone rules (via `jiff`), for the calendar and for message
//! times. Timestamps are Unix milliseconds.

use crate::time::Timestamp;
use jiff::civil::{Date, Time};
pub use jiff::tz::TimeZone;
use jiff::{Timestamp as JTimestamp, ToSpan, Zoned};

mod settings;
pub use settings::{available_zone_names, cache_key, current_definition, current_zone, detect_system_zone, revision, set_override, system_definition, system_zone, update_system_zone, zone_label, zone_with_cache_key, DetectedZone, ZoneDefinition};

pub fn zoned(ts: Timestamp) -> Zoned {
    in_zone(ts, &current_zone())
}

pub fn in_zone(ts: Timestamp, zone: &TimeZone) -> Zoned {
    JTimestamp::from_millisecond(ts).unwrap_or(JTimestamp::UNIX_EPOCH).to_zoned(zone.clone())
}

pub fn to_millis(z: &Zoned) -> Timestamp {
    z.timestamp().as_millisecond()
}

/// Local calendar date of a timestamp.
pub fn local_date(ts: Timestamp) -> Date {
    zoned(ts).date()
}

/// Midnight at the start of the local day containing `ts`.
pub fn day_start(ts: Timestamp) -> Timestamp {
    let z = zoned(ts);
    z.date().to_zoned(current_zone()).map(|d| to_millis(&d)).unwrap_or(ts)
}

/// Midnight at the start of a local date.
pub fn date_start(date: Date) -> Timestamp {
    date.to_zoned(current_zone()).map(|d| to_millis(&d)).unwrap_or(0)
}

/// Start of the week (Monday by default) containing `ts`.
pub fn week_start(ts: Timestamp, monday: bool) -> Timestamp {
    let date = local_date(ts);
    let weekday = date.weekday().to_monday_zero_offset() as i64; // Monday = 0
    let back = if monday { weekday } else { (weekday + 1) % 7 };
    date_start(date.checked_sub((back as i32).days()).unwrap_or(date))
}

/// Midnight on the first day of the month containing `ts`.
pub fn month_start(ts: Timestamp) -> Timestamp {
    let d = local_date(ts);
    date_start(d.first_of_month())
}

/// The same day `n` months later (clamped to the month length), at local midnight.
pub fn add_months(ts: Timestamp, n: i32) -> Timestamp {
    let d = local_date(ts);
    date_start(d.checked_add(n.months()).unwrap_or(d))
}

pub fn add_days(ts: Timestamp, days: i32) -> Timestamp {
    let date = local_date(ts);
    date_start(date.checked_add(days.days()).unwrap_or(date))
}

/// Combines a local date and time into a timestamp.
pub fn local_datetime(date: Date, time: Time) -> Timestamp {
    local_datetime_in(date, time, &current_zone()).unwrap_or(0)
}

pub fn local_datetime_in(date: Date, time: Time, zone: &TimeZone) -> Option<Timestamp> {
    date.at(time.hour(), time.minute(), time.second(), 0)
        .to_zoned(zone.clone())
        .ok().map(|z| to_millis(&z))
}

/// The time `minutes` past midnight by the clock on `day`'s local date; 24:00 is the next
/// midnight.
pub fn at_minutes(day: Timestamp, minutes: i32) -> Timestamp {
    if minutes >= 24 * 60 {
        return add_days(day, 1);
    }
    let time = Time::new((minutes / 60) as i8, (minutes % 60) as i8, 0, 0).unwrap_or_default();
    local_datetime(local_date(day), time)
}

/// All-day dates are civil dates, not elapsed UTC hours. Keep their dates intact
/// when the display zone changes, including pending local event edits.
pub fn reproject_all_day(event: &mut crate::Event, old: &TimeZone, new: &TimeZone) -> Result<(), String> {
    if !event.all_day { return Ok(()); }
    let project = |ts| in_zone(ts, old).date().to_zoned(new.clone()).map(|z| to_millis(&z))
        .map_err(|_| "Could not convert an all-day calendar date.".to_owned());
    let (start, end) = (project(event.start)?, project(event.end)?);
    let original = event.original_start.map(project).transpose()?;
    event.start = start;
    event.end = end;
    event.original_start = original;
    Ok(())
}

/// Minutes since local midnight.
pub fn minutes_into_day(ts: Timestamp) -> i32 {
    let z = zoned(ts);
    i32::from(z.hour()) * 60 + i32::from(z.minute())
}

static CLOCK_24H: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Switches every clock rendering between 12-hour and 24-hour.
pub fn set_clock_24h(enabled: bool) {
    CLOCK_24H.store(enabled, std::sync::atomic::Ordering::Relaxed);
}

pub fn clock_24h() -> bool {
    CLOCK_24H.load(std::sync::atomic::Ordering::Relaxed)
}

pub fn format_clock(ts: Timestamp) -> String {
    let z = zoned(ts);
    let (h, m) = (z.hour(), z.minute());
    if clock_24h() {
        return format!("{h:02}:{m:02}");
    }
    let (h12, suffix) = match h {
        0 => (12, "AM"),
        1..=11 => (h, "AM"),
        12 => (12, "PM"),
        _ => (h - 12, "PM"),
    };
    if m == 0 { format!("{h12} {suffix}") } else { format!("{h12}:{m:02} {suffix}") }
}

/// `Mon, Oct 12` style.
pub fn format_day(ts: Timestamp) -> String {
    zoned(ts).strftime("%a, %b %-d").to_string()
}

/// `October 2026`.
pub fn format_month_year(ts: Timestamp) -> String {
    zoned(ts).strftime("%B %Y").to_string()
}

/// `Oct 12 – 18, 2026` or `Sep 29 – Oct 5, 2026` for a week starting at `start`.
pub fn format_week_range(start: Timestamp) -> String {
    let a = zoned(start);
    let b = zoned(add_days(start, 6));
    if a.month() == b.month() {
        format!("{} {} – {}, {}", a.strftime("%b"), a.day(), b.day(), a.year())
    } else if a.year() == b.year() {
        format!("{} {} – {} {}, {}", a.strftime("%b"), a.day(), b.strftime("%b"), b.day(), a.year())
    } else {
        format!("{} {}, {} – {} {}, {}", a.strftime("%b"), a.day(), a.year(), b.strftime("%b"), b.day(), b.year())
    }
}

/// Parses a clock time typed by a person: `9`, `9:30`, `9:30 am`, `14:00`, `2pm`.
pub fn parse_clock(input: &str) -> Option<Time> {
    let s = input.trim().to_ascii_lowercase();
    if s.is_empty() {
        return None;
    }
    let (digits, suffix) = if let Some(rest) = s.strip_suffix("am") {
        (rest.trim(), Some(false))
    } else if let Some(rest) = s.strip_suffix("pm") {
        (rest.trim(), Some(true))
    } else if let Some(rest) = s.strip_suffix('a') {
        (rest.trim(), Some(false))
    } else if let Some(rest) = s.strip_suffix('p') {
        (rest.trim(), Some(true))
    } else {
        (s.as_str(), None)
    };
    let (h, m) = match digits.split_once([':', '.']) {
        Some((h, m)) => (h.trim().parse::<i8>().ok()?, m.trim().parse::<i8>().ok()?),
        None => (digits.parse::<i8>().ok()?, 0),
    };
    let hour = match suffix {
        Some(true) if h < 12 => h + 12,
        Some(false) if h == 12 => 0,
        _ => h,
    };
    Time::new(hour, m, 0, 0).ok()
}

/// Parses a `YYYY-MM-DD` all-day date.
pub fn parse_date(s: &str) -> Option<Date> {
    s.parse().ok()
}

/// Parses an RFC 3339 date-time.
pub fn parse_rfc3339(s: &str) -> Option<Timestamp> {
    s.parse::<JTimestamp>().ok().map(|t| t.as_millisecond()).or_else(|| {
        s.parse::<Zoned>().ok().map(|z| to_millis(&z))
    })
}

/// RFC 3339 in UTC, as Google's API expects for `timeMin`/`timeMax`.
/// UTC in iCalendar basic form, `20261014T160000Z`, as RRULE `UNTIL` wants it.
pub fn utc_basic(ts: Timestamp) -> String {
    jiff::Timestamp::from_millisecond(ts).map(|t| t.strftime("%Y%m%dT%H%M%SZ").to_string()).unwrap_or_default()
}

pub fn to_rfc3339(ts: Timestamp) -> String {
    JTimestamp::from_millisecond(ts).unwrap_or(JTimestamp::UNIX_EPOCH).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minutes_on_the_clock_become_a_time_on_that_day() {
        let day = date_start(parse_date("2026-07-15").unwrap());
        assert_eq!(minutes_into_day(at_minutes(day, 570)), 570);
        assert_eq!(local_date(at_minutes(day, 570)), local_date(day));
        assert_eq!(at_minutes(day, 0), day);
        assert_eq!(at_minutes(day, 24 * 60), add_days(day, 1));
    }

    #[test]
    fn week_and_day_math() {
        // 2026-10-12 is a Monday.
        let monday = date_start(Date::constant(2026, 10, 12));
        let wednesday = add_days(monday, 2);
        assert_eq!(week_start(wednesday, true), monday);
        assert_eq!(day_start(wednesday + 5 * 3_600_000), wednesday);
        assert_eq!(minutes_into_day(wednesday + 9 * 3_600_000 + 30 * 60_000), 570);
        assert_eq!(local_date(wednesday).day(), 14);
        assert!(format_week_range(monday).starts_with("Oct 12 – 18"));
    }

    #[test]
    fn month_math() {
        let mid = date_start(Date::constant(2026, 10, 31));
        assert_eq!(local_date(month_start(mid)).day(), 1);
        assert_eq!(local_date(add_months(mid, 1)), Date::constant(2026, 11, 30));
        assert_eq!(local_date(add_months(month_start(mid), -1)), Date::constant(2026, 9, 1));
    }

    #[test]
    fn parses_typed_clock_times() {
        assert_eq!(parse_clock("9"), Some(Time::constant(9, 0, 0, 0)));
        assert_eq!(parse_clock("9:30 am"), Some(Time::constant(9, 30, 0, 0)));
        assert_eq!(parse_clock("2pm"), Some(Time::constant(14, 0, 0, 0)));
        assert_eq!(parse_clock("12:15 AM"), Some(Time::constant(0, 15, 0, 0)));
        assert_eq!(parse_clock("14:45"), Some(Time::constant(14, 45, 0, 0)));
        assert_eq!(parse_clock("noon"), None);
    }

    #[test]
    fn parses_google_times() {
        assert!(parse_rfc3339("2026-10-12T10:00:00-07:00").is_some());
        assert!(parse_rfc3339("2026-10-12T17:00:00Z").is_some());
        assert_eq!(parse_date("2026-10-12"), Some(Date::constant(2026, 10, 12)));
        assert!(to_rfc3339(0).starts_with("1970-01-01T00:00:00"));
    }

    #[test]
    fn an_open_event_editors_zone_keeps_the_same_instant_after_travel() {
        let toronto = TimeZone::get("America/Toronto").unwrap();
        let phoenix = TimeZone::get("America/Phoenix").unwrap();
        let date = Date::constant(2026, 11, 2);
        let time = Time::constant(9, 30, 0, 0);
        let entered = local_datetime_in(date, time, &toronto).unwrap();
        assert_eq!(in_zone(entered, &phoenix).hour(), 7);
        assert_eq!(in_zone(entered, &toronto).hour(), 9);
        assert_ne!(entered, local_datetime_in(date, time, &phoenix).unwrap());
    }
}
