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
    v.replace("\\n", "\n").replace("\\,", ",").replace("\\;", ";").replace("\\\\", "\\")
}

fn parse_dt(key: &str, value: &str) -> Option<(Timestamp, Option<String>)> {
    use jiff::civil::{Date, DateTime};
    use jiff::tz::TimeZone;
    let params: Vec<&str> = key.split(';').skip(1).collect();
    let tzid = params.iter().find_map(|p| p.strip_prefix("TZID="));
    let is_date = params.iter().any(|p| *p == "VALUE=DATE") || value.len() == 8;
    if is_date {
        let date: Date = format!("{}-{}-{}", &value[0..4], &value[4..6], &value[6..8]).parse().ok()?;
        return Some((crate::tz::date_start(date), Some(date.to_string())));
    }
    let utc = value.ends_with('Z');
    let v = value.trim_end_matches('Z');
    if v.len() < 15 {
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
    let mut in_event = false;
    for (key, value) in lines(text) {
        let name = key.split(';').next().unwrap_or("").to_ascii_uppercase();
        match name.as_str() {
            "METHOD" => method = value.trim().to_ascii_uppercase(),
            "BEGIN" if value.trim().eq_ignore_ascii_case("VEVENT") => in_event = true,
            "END" if value.trim().eq_ignore_ascii_case("VEVENT") => break,
            "UID" if in_event => uid = Some(value.trim().to_owned()),
            "SUMMARY" if in_event => summary = Some(unescape(value.trim())),
            "DTSTART" if in_event => {
                if let Some((ts, date)) = parse_dt(&key, value.trim()) {
                    start = Some(ts);
                    all_day = date.is_some();
                    start_date = date;
                }
            }
            "ORGANIZER" if in_event => {
                organizer = value.trim().strip_prefix("mailto:").or_else(|| value.trim().strip_prefix("MAILTO:")).map(|e| e.to_ascii_lowercase());
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
}
