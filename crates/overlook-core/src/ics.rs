//! Minimal iCalendar (RFC 5545) reading for invitation emails: enough to recognise an
//! invitation, its UID, method, title, and start time. Full parsing is not needed because
//! the calendar itself is synchronized from the provider.

use crate::time::Timestamp;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Invitation {
    pub uid: String,
    /// `REQUEST` (invite/update), `CANCEL`, `REPLY`, or `PUBLISH`.
    pub method: String,
    pub summary: Option<String>,
    pub start: Option<Timestamp>,
    /// Date-only invitations keep their civil date across display-zone changes.
    #[serde(default)]
    pub start_date: Option<String>,
    pub all_day: bool,
    pub organizer_email: Option<String>,
}

impl Invitation {
    pub fn is_cancellation(&self) -> bool {
        self.method.eq_ignore_ascii_case("CANCEL")
    }

    pub fn start_in_zone(&self, zone: &crate::tz::TimeZone) -> Option<Timestamp> {
        if self.all_day {
            if let Some(date) = self.start_date.as_deref().and_then(crate::tz::parse_date) {
                return date.to_zoned(zone.clone()).ok().map(|date| crate::tz::to_millis(&date));
            }
        }
        self.start
    }
}

/// Unfolds continuation lines and returns `(NAME;PARAMS, VALUE)` pairs.
fn lines(text: &str) -> Vec<(String, String)> {
    let mut unfolded: Vec<String> = Vec::new();
    for raw in text.split(['\n']) {
        let line = raw.trim_end_matches('\r');
        if (line.starts_with(' ') || line.starts_with('\t')) && !unfolded.is_empty() {
            unfolded.last_mut().unwrap().push_str(&line[1..]);
        } else {
            unfolded.push(line.to_owned());
        }
    }
    unfolded
        .into_iter()
        .filter_map(|l| l.split_once(':').map(|(k, v)| (k.to_owned(), v.to_owned())))
        .collect()
}

fn unescape(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    let mut chars = v.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n' | 'N') => out.push('\n'),
                Some(',') => out.push(','),
                Some(';') => out.push(';'),
                Some('\\') => out.push('\\'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn parse_dt(key: &str, value: &str) -> Option<(Timestamp, Option<String>)> {
    use jiff::civil::{Date, DateTime};
    use jiff::tz::TimeZone;
    if !value.is_ascii() {
        return None;
    }
    let mut tzid = None;
    let mut has_value_date = false;
    for p in key.split(';').skip(1) {
        if let Some((k, v)) = p.split_once('=') {
            if k.eq_ignore_ascii_case("TZID") {
                let cleaned = v.trim_matches('"');
                if !cleaned.is_empty() {
                    tzid = Some(cleaned);
                }
            } else if k.eq_ignore_ascii_case("VALUE") && v.eq_ignore_ascii_case("DATE") {
                has_value_date = true;
            }
        }
    }
    let is_date = has_value_date || value.len() == 8;
    if is_date {
        if value.len() < 8 {
            return None;
        }
        let date: Date = format!("{}-{}-{}", &value[0..4], &value[4..6], &value[6..8]).parse().ok()?;
        return Some((crate::tz::date_start(date), Some(date.to_string())));
    }
    let utc = value.ends_with('Z');
    let v = value.strip_suffix('Z').unwrap_or(value);
    if v.len() < 15 || v.as_bytes()[8] != b'T' {
        return None;
    }
    let dt: DateTime = format!("{}-{}-{}T{}:{}:{}", &v[0..4], &v[4..6], &v[6..8], &v[9..11], &v[11..13], &v[13..15]).parse().ok()?;
    let zone = if utc {
        TimeZone::UTC
    } else {
        tzid.and_then(|t| TimeZone::get(t).ok()).unwrap_or_else(crate::tz::current_zone)
    };
    let zoned = dt.to_zoned(zone).ok()?;
    Some((crate::tz::to_millis(&zoned), None))
}

/// Parses the first VEVENT of an iCalendar text.
pub fn parse_invitation(text: &str) -> Option<Invitation> {
    let mut method = "PUBLISH".to_owned();
    let mut uid = None;
    let mut summary = None;
    let mut start = None;
    let mut start_date = None;
    let mut all_day = false;
    let mut organizer = None;
    let mut event_depth = 0usize;
    for (key, value) in lines(text) {
        let name = key.split(';').next().unwrap_or("").to_ascii_uppercase();
        match name.as_str() {
            "METHOD" if event_depth == 0 => method = value.trim().to_ascii_uppercase(),
            "BEGIN" => {
                if event_depth == 0 {
                    if value.trim().eq_ignore_ascii_case("VEVENT") {
                        event_depth = 1;
                    }
                } else {
                    event_depth += 1;
                }
            }
            "END" => {
                if event_depth == 1 && value.trim().eq_ignore_ascii_case("VEVENT") {
                    break;
                } else if event_depth > 1 {
                    event_depth -= 1;
                }
            }
            "UID" if event_depth == 1 => uid = Some(value.trim().to_owned()),
            "SUMMARY" if event_depth == 1 => summary = Some(unescape(value.trim())),
            "DTSTART" if event_depth == 1 => {
                if let Some((ts, date)) = parse_dt(&key, value.trim()) {
                    start = Some(ts);
                    all_day = date.is_some();
                    start_date = date;
                }
            }
            "ORGANIZER" if event_depth == 1 => {
                let trimmed = value.trim();
                if trimmed.get(..7).is_some_and(|p| p.eq_ignore_ascii_case("mailto:")) {
                    organizer = Some(trimmed[7..].to_ascii_lowercase());
                }
            }
            _ => {}
        }
    }
    Some(Invitation { uid: uid?, method, summary, start, start_date, all_day, organizer_email: organizer })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_google_style_invite() {
        let ics = "BEGIN:VCALENDAR\r\nMETHOD:REQUEST\r\nBEGIN:VEVENT\r\nDTSTART:20261014T160000Z\r\nDTEND:20261014T163000Z\r\nORGANIZER;CN=Maya Chen:mailto:maya@northstar.example\r\nUID:abc123@google.com\r\nSUMMARY:Design review\\, round 2\r\n  and more\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let inv = parse_invitation(ics).unwrap();
        assert_eq!(inv.uid, "abc123@google.com");
        assert_eq!(inv.method, "REQUEST");
        assert_eq!(inv.summary.as_deref(), Some("Design review, round 2 and more"));
        assert_eq!(inv.organizer_email.as_deref(), Some("maya@northstar.example"));
        assert!(!inv.all_day);
        assert!(inv.start.is_some());
        assert!(!inv.is_cancellation());
    }

    #[test]
    fn parses_tzid_and_dates() {
        let ics = "BEGIN:VCALENDAR\nMETHOD:CANCEL\nBEGIN:VEVENT\nUID:x\nDTSTART;TZID=America/Phoenix:20261014T090000\nEND:VEVENT\nEND:VCALENDAR";
        let inv = parse_invitation(ics).unwrap();
        assert!(inv.is_cancellation());
        assert_eq!(inv.start, Some(crate::tz::parse_rfc3339("2026-10-14T09:00:00-07:00").unwrap()));
        let all_day = parse_invitation("BEGIN:VEVENT\nUID:y\nDTSTART;VALUE=DATE:20261020\nEND:VEVENT").unwrap();
        assert!(all_day.all_day);
        assert!(parse_invitation("BEGIN:VEVENT\nSUMMARY:no uid\nEND:VEVENT").is_none());
    }

    #[test]
    fn all_day_invitation_dates_survive_travel_and_json_roundtrips() {
        let inv = parse_invitation("BEGIN:VEVENT\nUID:day\nDTSTART;VALUE=DATE:20261102\nEND:VEVENT").unwrap();
        let inv: Invitation = serde_json::from_str(&serde_json::to_string(&inv).unwrap()).unwrap();
        for name in ["Asia/Tokyo", "America/Phoenix"] {
            let zone = crate::tz::TimeZone::get(name).unwrap();
            let start = inv.start_in_zone(&zone).unwrap();
            assert_eq!(crate::tz::in_zone(start, &zone).date().to_string(), "2026-11-02");
        }
        let legacy: Invitation = serde_json::from_value(serde_json::json!({"uid":"old", "method":"REQUEST", "summary":null, "start":0, "all_day":true, "organizer_email":null})).unwrap();
        assert_eq!(legacy.start_date, None);
        assert_eq!(legacy.start_in_zone(&crate::tz::TimeZone::UTC), Some(0));
    }

    #[test]
    fn handles_malformed_dates_valarm_nesting_case_and_escaping() {
        // Non-ASCII and malformed date values must not panic.
        for bad in [
            "DTSTART;VALUE=DATE:2026",
            "DTSTART;VALUE=DATE:2026éééé",
            "DTSTART:🎉🎉🎉🎉🎉🎉🎉🎉",
            "DTSTART:20261014X160000Z",
            "DTSTART:20261014T16éé00Z",
        ] {
            let ics = format!("BEGIN:VEVENT\nUID:bad\n{bad}\nEND:VEVENT");
            let inv = parse_invitation(&ics).unwrap();
            assert_eq!(inv.start, None, "{bad}");
        }

        // Single-pass unescaping preserves literal `\\n` as `\n`, handles `\N`, `\,`, `\;`, `\\`.
        assert_eq!(unescape(r"a\\nb\nc\Nd\,e\;f\\g"), "a\\nb\nc\nd,e;f\\g");

        // Nested VALARM must not clobber VEVENT UID/SUMMARY; quoted/mixed-case TZID and MailTo work.
        let ics = "\
BEGIN:VCALENDAR\r\n\
METHOD:REQUEST\r\n\
BEGIN:VEVENT\r\n\
UID:event-uid@example\r\n\
SUMMARY:Project\\\\notes\\NLine 2\r\n\
DTSTART;tzid=\"America/Phoenix\":20261014T090000\r\n\
ORGANIZER:MailTo:Maya@Northstar.Example\r\n\
BEGIN:VALARM\r\n\
ACTION:DISPLAY\r\n\
UID:alarm-uid\r\n\
SUMMARY:Alarm reminder\r\n\
END:VALARM\r\n\
END:VEVENT\r\n\
END:VCALENDAR\r\n";
        let inv = parse_invitation(ics).unwrap();
        assert_eq!(inv.uid, "event-uid@example");
        assert_eq!(inv.summary.as_deref(), Some("Project\\notes\nLine 2"));
        assert_eq!(inv.organizer_email.as_deref(), Some("maya@northstar.example"));
        assert_eq!(inv.start, Some(crate::tz::parse_rfc3339("2026-10-14T09:00:00-07:00").unwrap()));

        let lower_date = parse_invitation("BEGIN:VEVENT\nUID:d\nDTSTART;value=date:20261020\nEND:VEVENT").unwrap();
        assert!(lower_date.all_day);
        assert_eq!(lower_date.start_date.as_deref(), Some("2026-10-20"));
    }
}
