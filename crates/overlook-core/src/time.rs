//! Minimal time helpers. Timestamps are Unix milliseconds in UTC.

use std::time::{SystemTime, UNIX_EPOCH};
use crate::tz::TimeZone;

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

/// Splits a timestamp using the zone's offset at that instant, including DST.
pub fn civil_in_zone(ts: Timestamp, zone: &TimeZone) -> (CivilDate, CivilTime, u32) {
    let z = crate::tz::in_zone(ts, zone);
    (CivilDate { year: i32::from(z.year()), month: z.month() as u32, day: z.day() as u32 },
        CivilTime { hour: z.hour() as u32, minute: z.minute() as u32 },
        z.weekday().to_sunday_zero_offset() as u32)
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
pub fn relative_label(ts: Timestamp, now: Timestamp, zone: &TimeZone) -> String {
    let (date, time, weekday) = civil_in_zone(ts, zone);
    let (today, _, _) = civil_in_zone(now, zone);
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
pub fn local_day(ts: Timestamp, zone: &TimeZone) -> i64 {
    let (date, _, _) = civil_in_zone(ts, zone);
    days_from_civil(date.year, date.month, date.day)
}

pub fn day_group(ts: Timestamp, now: Timestamp, zone: &TimeZone) -> DayGroup {
    let (date, _, weekday) = civil_in_zone(ts, zone);
    let (today, _, _) = civil_in_zone(now, zone);
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
pub fn format_full(ts: Timestamp, zone: &TimeZone) -> String {
    let (date, time, weekday) = civil_in_zone(ts, zone);
    format!(
        "{}, {} {}, {} at {}",
        WEEKDAYS[weekday as usize],
        MONTHS_SHORT[(date.month - 1) as usize],
        date.day,
        date.year,
        format_clock(time)
    )
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
        assert_eq!(local_day(midnight_utc, &TimeZone::UTC), days_from_civil(2026, 10, 12));
        assert_eq!(local_day(midnight_utc - 1, &TimeZone::UTC), days_from_civil(2026, 10, 11));
        // Five hours behind UTC, local midnight is 05:00 UTC.
        assert_eq!(local_day(midnight_utc + 4 * 3_600_000, &TimeZone::get("Etc/GMT+5").unwrap()), days_from_civil(2026, 10, 11));
        assert_eq!(local_day(midnight_utc + 5 * 3_600_000, &TimeZone::get("Etc/GMT+5").unwrap()), days_from_civil(2026, 10, 12));
    }

    #[test]
    fn relative_labels() {
        let base = days_from_civil(2026, 10, 12) * 86_400_000 + 15 * 3_600_000;
        assert_eq!(relative_label(base - 3_600_000, base, &TimeZone::UTC), "2:00 PM");
        assert_eq!(relative_label(base - 86_400_000, base, &TimeZone::UTC), "Yesterday");
        assert_eq!(relative_label(base - 3 * 86_400_000, base, &TimeZone::UTC), "Friday");
        assert_eq!(relative_label(base - 30 * 86_400_000, base, &TimeZone::UTC), "Sep 12");
        assert_eq!(day_group(base - 3 * 86_400_000, base, &TimeZone::UTC), DayGroup::ThisWeek("Friday".into()));
        assert_eq!(day_group(base - 40 * 86_400_000, base, &TimeZone::UTC), DayGroup::Earlier);
    }

    #[test]
    fn mail_dates_use_each_instants_offset_including_dst_and_fractional_zones() {
        let toronto = TimeZone::get("America/Toronto").unwrap();
        let summer = crate::tz::parse_rfc3339("2026-07-01T16:00:00Z").unwrap();
        let winter = crate::tz::parse_rfc3339("2026-12-01T16:00:00Z").unwrap();
        assert_eq!(civil_in_zone(summer, &toronto).1, CivilTime { hour: 12, minute: 0 });
        assert_eq!(civil_in_zone(winter, &toronto).1, CivilTime { hour: 11, minute: 0 });
        assert!(format_full(summer, &toronto).ends_with("12:00 PM"));
        assert_eq!(civil_in_zone(winter, &TimeZone::get("Asia/Kolkata").unwrap()).1, CivilTime { hour: 21, minute: 30 });
        let fold_a = crate::tz::parse_rfc3339("2026-11-01T05:30:00Z").unwrap();
        let fold_b = crate::tz::parse_rfc3339("2026-11-01T06:30:00Z").unwrap();
        assert_eq!(civil_in_zone(fold_a, &toronto).1, civil_in_zone(fold_b, &toronto).1);
        let now = crate::tz::parse_rfc3339("2026-11-02T05:30:00Z").unwrap();
        let previous = crate::tz::parse_rfc3339("2026-11-01T04:30:00Z").unwrap();
        assert_eq!(day_group(previous, now, &toronto), DayGroup::Yesterday);
        assert_eq!(relative_label(previous, now, &toronto), "Yesterday");
        let phoenix = TimeZone::get("America/Phoenix").unwrap();
        assert_eq!(civil_in_zone(summer, &phoenix).1.hour, 9);
        assert_eq!(civil_in_zone(winter, &phoenix).1.hour, 9);
    }
}
