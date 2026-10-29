//! Minimal time helpers. Timestamps are Unix milliseconds in UTC.

use std::time::{SystemTime, UNIX_EPOCH};

/// Unix timestamp in milliseconds.
pub type Timestamp = i64;

pub fn now() -> Timestamp {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Civil date (year, month, day) derived from a timestamp and a UTC offset in minutes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CivilDate {
    pub year: i32,
    pub month: u32,
    pub day: u32,
}

/// Civil time of day.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CivilTime {
    pub hour: u32,
    pub minute: u32,
}

/// Splits a timestamp into a civil date and time using a fixed UTC offset.
pub fn civil(ts: Timestamp, utc_offset_minutes: i32) -> (CivilDate, CivilTime, u32) {
    let local_secs = ts.div_euclid(1000) + i64::from(utc_offset_minutes) * 60;
    let days = local_secs.div_euclid(86_400);
    let secs_of_day = local_secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let weekday = ((days + 4).rem_euclid(7)) as u32; // 0 = Sunday
    (
        CivilDate { year, month, day },
        CivilTime {
            hour: (secs_of_day / 3600) as u32,
            minute: ((secs_of_day % 3600) / 60) as u32,
        },
        weekday,
    )
}

/// Howard Hinnant's days-to-civil algorithm.
fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m, d)
}

/// Days-from-civil, inverse of [`civil_from_days`].
pub fn days_from_civil(year: i32, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year } as i64;
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let m = month as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

pub const MONTHS_SHORT: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
pub const WEEKDAYS: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];

/// Formats a clock time as `10:42 AM` (or `10:42` in 24-hour mode).
pub fn format_clock(t: CivilTime) -> String {
    if crate::tz::clock_24h() {
        return format!("{:02}:{:02}", t.hour, t.minute);
    }
    let (h12, suffix) = match t.hour {
        0 => (12, "AM"),
        1..=11 => (t.hour, "AM"),
        12 => (12, "PM"),
        _ => (t.hour - 12, "PM"),
    };
    format!("{h12}:{:02} {suffix}", t.minute)
}

/// Day bucket used by the message list: Today, Yesterday, a weekday within the last week,
/// or Earlier.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum DayGroup {
    Today,
    Yesterday,
    ThisWeek(String),
    Earlier,
}

impl DayGroup {
    pub fn label(&self) -> &str {
        match self {
            DayGroup::Today => "Today",
            DayGroup::Yesterday => "Yesterday",
            DayGroup::ThisWeek(day) => day,
            DayGroup::Earlier => "Earlier",
        }
    }
}

/// Describes a message time relative to `now`, the way the list column shows it:
/// clock time for today, weekday for the past week, otherwise a short date.
pub fn relative_label(ts: Timestamp, now: Timestamp, utc_offset_minutes: i32) -> String {
    let (date, time, weekday) = civil(ts, utc_offset_minutes);
    let (today, _, _) = civil(now, utc_offset_minutes);
    let day_delta = days_from_civil(today.year, today.month, today.day)
        - days_from_civil(date.year, date.month, date.day);
    if day_delta == 0 {
        format_clock(time)
    } else if day_delta == 1 {
        "Yesterday".to_owned()
    } else if (2..7).contains(&day_delta) {
        WEEKDAYS[weekday as usize].to_owned()
    } else if date.year == today.year {
        format!("{} {}", MONTHS_SHORT[(date.month - 1) as usize], date.day)
    } else {
        format!(
            "{} {}, {}",
            MONTHS_SHORT[(date.month - 1) as usize],
            date.day,
            date.year
        )
    }
}

/// The calendar day `ts` falls on in the given zone, as a day count. It changes exactly
/// when [`day_group`] can change its answer for a fixed message.
pub fn local_day(ts: Timestamp, utc_offset_minutes: i32) -> i64 {
    let (date, _, _) = civil(ts, utc_offset_minutes);
    days_from_civil(date.year, date.month, date.day)
}

pub fn day_group(ts: Timestamp, now: Timestamp, utc_offset_minutes: i32) -> DayGroup {
    let (date, _, weekday) = civil(ts, utc_offset_minutes);
    let (today, _, _) = civil(now, utc_offset_minutes);
    let day_delta = days_from_civil(today.year, today.month, today.day)
        - days_from_civil(date.year, date.month, date.day);
    match day_delta {
        i64::MIN..=0 => DayGroup::Today,
        1 => DayGroup::Yesterday,
        2..=6 => DayGroup::ThisWeek(WEEKDAYS[weekday as usize].to_owned()),
        _ => DayGroup::Earlier,
    }
}

/// Full date and time for the reader header, e.g. `Monday, Oct 12, 2026 at 10:42 AM`.
pub fn format_full(ts: Timestamp, utc_offset_minutes: i32) -> String {
    let (date, time, weekday) = civil(ts, utc_offset_minutes);
    format!(
        "{}, {} {}, {} at {}",
        WEEKDAYS[weekday as usize],
        MONTHS_SHORT[(date.month - 1) as usize],
        date.day,
        date.year,
        format_clock(time)
    )
}

/// Best-effort local UTC offset in minutes, read from the `TZ`-independent libc `localtime`.
/// Falls back to UTC when the offset cannot be determined.
pub fn local_utc_offset_minutes() -> i32 {
    // Avoid a libc dependency: read /etc/localtime is complex, so use the `date` convention
    // through the environment when present (`OVERLOOK_UTC_OFFSET_MINUTES`), else UTC.
    std::env::var("OVERLOOK_UTC_OFFSET_MINUTES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(system_offset_minutes)
}

#[cfg(unix)]
fn system_offset_minutes() -> i32 {
    // Parse the TZif footer of /etc/localtime is overkill for the prototype; instead ask the
    // platform via `date +%z`, which is universally available on Linux.
    std::process::Command::new("date")
        .arg("+%z")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| parse_offset(s.trim()))
        .unwrap_or(0)
}

#[cfg(not(unix))]
fn system_offset_minutes() -> i32 {
    0
}

fn parse_offset(s: &str) -> Option<i32> {
    if s.len() < 5 {
        return None;
    }
    let sign = match &s[0..1] {
        "+" => 1,
        "-" => -1,
        _ => return None,
    };
    let hours: i32 = s[1..3].parse().ok()?;
    let minutes: i32 = s[3..5].parse().ok()?;
    Some(sign * (hours * 60 + minutes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_round_trip() {
        let days = days_from_civil(2026, 10, 12);
        assert_eq!(civil_from_days(days), (2026, 10, 12));
        let (date, time, weekday) = civil(days * 86_400_000 + 10 * 3_600_000 + 42 * 60_000, 0);
        assert_eq!(date, CivilDate { year: 2026, month: 10, day: 12 });
        assert_eq!(format_clock(time), "10:42 AM");
        assert_eq!(WEEKDAYS[weekday as usize], "Monday");
    }

    #[test]
    fn local_day_turns_over_at_local_midnight() {
        let midnight_utc = days_from_civil(2026, 10, 12) * 86_400_000;
        assert_eq!(local_day(midnight_utc, 0), days_from_civil(2026, 10, 12));
        assert_eq!(local_day(midnight_utc - 1, 0), days_from_civil(2026, 10, 11));
        // Five hours behind UTC, local midnight is 05:00 UTC.
        assert_eq!(local_day(midnight_utc + 4 * 3_600_000, -300), days_from_civil(2026, 10, 11));
        assert_eq!(local_day(midnight_utc + 5 * 3_600_000, -300), days_from_civil(2026, 10, 12));
    }

    #[test]
    fn relative_labels() {
        let base = days_from_civil(2026, 10, 12) * 86_400_000 + 15 * 3_600_000;
        assert_eq!(relative_label(base - 3_600_000, base, 0), "2:00 PM");
        assert_eq!(relative_label(base - 86_400_000, base, 0), "Yesterday");
        assert_eq!(relative_label(base - 3 * 86_400_000, base, 0), "Friday");
        assert_eq!(relative_label(base - 30 * 86_400_000, base, 0), "Sep 12");
        assert_eq!(day_group(base - 3 * 86_400_000, base, 0), DayGroup::ThisWeek("Friday".into()));
        assert_eq!(day_group(base - 40 * 86_400_000, base, 0), DayGroup::Earlier);
    }

    #[test]
    fn offsets_parse() {
        assert_eq!(parse_offset("-0700"), Some(-420));
        assert_eq!(parse_offset("+0530"), Some(330));
        assert_eq!(parse_offset("nope"), None);
    }
}
