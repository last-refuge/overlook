//! HTTP fetcher and RFC 5545 parser/recurrence expander for read-only
//! subscribed iCalendar (`.ics` / `webcal://`) feeds, including Apple iCloud links.

use base64::Engine;
use ical::parser::ical::component::{IcalEvent, IcalTimeZone};
use ical::property::Property;
use jiff::civil::{Date, DateTime, Weekday};
use jiff::tz::{Offset, TimeZone};
use jiff::{Span, ToSpan};
use overlook_core::{
    normalize_hex_color, normalize_subscription_url, Address, Attendee, CalendarId,
    CalendarSubscription, CalendarSubscriptionId, Event, EventId, EventStatus, ResponseStatus,
};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::Read;
use std::net::IpAddr;
use std::time::Duration;
use url::{Host, Url};

const MAX_ICS_BYTES: u64 = 8 * 1024 * 1024;
const MAX_REDIRECTS: usize = 5;
const FETCH_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_FEED_EVENTS: usize = 20_000;
const MAX_RULE_STEPS: usize = 50_000;
const MAX_OCCURRENCES_PER_EVENT: usize = 2_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchOutcome {
    NotModified,
    Updated {
        body: String,
        etag: Option<String>,
        last_modified: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SubscriptionFetchError {
    #[error("{0}")]
    InvalidUrl(&'static str),
    #[error("Calendar not found at that URL (HTTP 404).")]
    NotFound,
    #[error("This calendar URL requires sign-in or permission (not public).")]
    Unauthorized,
    #[error("The calendar host is limiting requests. Try again shortly.")]
    RateLimited,
    #[error("The calendar host is temporarily unavailable.")]
    ServiceUnavailable,
    #[error("Could not reach the calendar URL over HTTPS.")]
    Network,
    #[error("The calendar feed is larger than the 8 MB limit.")]
    TooLarge,
    #[error("That link did not return an iCalendar (.ics) feed.")]
    NotICalendar,
    #[error("The calendar URL redirected too many times.")]
    RedirectLimit,
    #[error("The calendar host returned HTTP {0}.")]
    UnexpectedStatus(u16),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedSubscriptionFeed {
    pub calendar_name: Option<String>,
    pub calendar_color: Option<String>,
    pub events: Vec<Event>,
}

/// Fetches an iCalendar feed over HTTPS (or normalized `webcal://` -> `https://`),
/// using conditional `If-None-Match` / `If-Modified-Since` headers when cached validators exist.
pub fn fetch_ics(
    url: &str,
    etag: Option<&str>,
    last_modified: Option<&str>,
) -> Result<FetchOutcome, SubscriptionFetchError> {
    fetch_ics_with_policy(url, etag, last_modified, true)
}

fn fetch_ics_with_policy(
    raw_url: &str,
    etag: Option<&str>,
    last_modified: Option<&str>,
    https_only: bool,
) -> Result<FetchOutcome, SubscriptionFetchError> {
    let initial = parse_initial_url(raw_url, https_only)?;
    let initial_is_loopback = is_loopback_host(&initial);
    let agent = ureq::Agent::config_builder()
        .https_only(https_only)
        .max_redirects(0)
        .http_status_as_error(false)
        .timeout_global(Some(FETCH_TIMEOUT))
        .proxy(None)
        .build()
        .new_agent();

    let mut current = initial;
    let mut visited = HashSet::new();
    let user_agent = concat!("Overlook/", env!("CARGO_PKG_VERSION"));

    for _ in 0..=MAX_REDIRECTS {
        if !visited.insert(current.to_string()) {
            return Err(SubscriptionFetchError::RedirectLimit);
        }

        let mut builder = ureq::http::Request::builder()
            .method("GET")
            .uri(current.as_str())
            .header("User-Agent", user_agent)
            .header("Accept", "text/calendar, text/plain;q=0.5, */*;q=0.1");

        if let Some(tag) = clean_header_value(etag) {
            builder = builder.header("If-None-Match", tag);
        }
        if let Some(modified) = clean_header_value(last_modified) {
            builder = builder.header("If-Modified-Since", modified);
        }

        let request = builder
            .body(())
            .map_err(|_| SubscriptionFetchError::InvalidUrl("Enter a valid calendar URL."))?;
        let response = agent
            .run(request)
            .map_err(|_| SubscriptionFetchError::Network)?;

        match response.status().as_u16() {
            301 | 302 | 303 | 307 | 308 => {
                let location = response
                    .headers()
                    .get("location")
                    .and_then(|h| h.to_str().ok())
                    .ok_or(SubscriptionFetchError::Network)?
                    .to_owned();
                let mut body = response.into_body();
                let _ = body.with_config().limit(64 * 1024).read_to_vec();
                current = resolve_redirect_url(&current, &location, https_only, initial_is_loopback)?;
            }
            304 => return Ok(FetchOutcome::NotModified),
            200 => {
                let resp_etag = response
                    .headers()
                    .get("etag")
                    .and_then(|h| h.to_str().ok())
                    .and_then(|v| clean_header_value(Some(v)));
                let resp_last_modified = response
                    .headers()
                    .get("last-modified")
                    .and_then(|h| h.to_str().ok())
                    .and_then(|v| clean_header_value(Some(v)));

                let mut reader = response.into_body().into_reader().take(MAX_ICS_BYTES + 1);
                let mut bytes = Vec::new();
                reader
                    .read_to_end(&mut bytes)
                    .map_err(|_| SubscriptionFetchError::Network)?;
                if bytes.len() as u64 > MAX_ICS_BYTES {
                    return Err(SubscriptionFetchError::TooLarge);
                }

                let text = std::str::from_utf8(&bytes)
                    .map_err(|_| SubscriptionFetchError::NotICalendar)?;
                let stripped = text.strip_prefix('\u{FEFF}').unwrap_or(text);
                if !looks_like_icalendar(stripped) {
                    return Err(SubscriptionFetchError::NotICalendar);
                }

                return Ok(FetchOutcome::Updated {
                    body: stripped.to_owned(),
                    etag: resp_etag,
                    last_modified: resp_last_modified,
                });
            }
            401 | 403 => return Err(SubscriptionFetchError::Unauthorized),
            404 | 410 => return Err(SubscriptionFetchError::NotFound),
            429 => return Err(SubscriptionFetchError::RateLimited),
            500..=599 => return Err(SubscriptionFetchError::ServiceUnavailable),
            code => return Err(SubscriptionFetchError::UnexpectedStatus(code)),
        }
    }

    Err(SubscriptionFetchError::RedirectLimit)
}

fn parse_initial_url(raw: &str, https_only: bool) -> Result<Url, SubscriptionFetchError> {
    if https_only {
        let normalized =
            normalize_subscription_url(raw).map_err(SubscriptionFetchError::InvalidUrl)?;
        return Url::parse(&normalized)
            .map_err(|_| SubscriptionFetchError::InvalidUrl("Enter a valid calendar URL."));
    }
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.len() > 4096 || trimmed.chars().any(char::is_control) {
        return Err(SubscriptionFetchError::InvalidUrl("Enter a valid calendar URL."));
    }
    let rewritten = rewrite_webcal(trimmed);
    let url = Url::parse(&rewritten)
        .map_err(|_| SubscriptionFetchError::InvalidUrl("Enter a valid calendar URL."))?;
    let scheme_ok = url.scheme() == "https" || (url.scheme() == "http" && is_loopback_host(&url));
    if !scheme_ok || url.host_str().is_none_or(str::is_empty) {
        return Err(SubscriptionFetchError::InvalidUrl("Enter a valid calendar URL."));
    }
    if !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
        return Err(SubscriptionFetchError::InvalidUrl("Enter a valid calendar URL."));
    }
    Ok(url)
}

fn resolve_redirect_url(
    current: &Url,
    location: &str,
    https_only: bool,
    allow_loopback: bool,
) -> Result<Url, SubscriptionFetchError> {
    let trimmed = location.trim();
    if trimmed.is_empty() || trimmed.len() > 4096 || trimmed.chars().any(char::is_control) {
        return Err(SubscriptionFetchError::Network);
    }
    let rewritten = rewrite_webcal(trimmed);
    let next = current
        .join(&rewritten)
        .map_err(|_| SubscriptionFetchError::Network)?;
    let scheme_ok = next.scheme() == "https" || (!https_only && next.scheme() == "http" && allow_loopback && is_loopback_host(&next));
    if !scheme_ok
        || next.host_str().is_none_or(str::is_empty)
        || !next.username().is_empty()
        || next.password().is_some()
        || next.fragment().is_some()
    {
        return Err(SubscriptionFetchError::Network);
    }
    if !allow_loopback && is_loopback_or_private_host(&next) {
        return Err(SubscriptionFetchError::Network);
    }
    Ok(next)
}

fn rewrite_webcal(raw: &str) -> String {
    if let Some(rest) = raw.get(..10).filter(|h| h.eq_ignore_ascii_case("webcals://")).map(|_| &raw[10..]) {
        format!("https://{rest}")
    } else if let Some(rest) = raw.get(..9).filter(|h| h.eq_ignore_ascii_case("webcal://")).map(|_| &raw[9..]) {
        format!("https://{rest}")
    } else {
        raw.to_owned()
    }
}

fn is_loopback_host(url: &Url) -> bool {
    match url.host() {
        Some(Host::Domain(dom)) => dom.eq_ignore_ascii_case("localhost"),
        Some(Host::Ipv4(ip)) => ip.is_loopback(),
        Some(Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

fn is_loopback_or_private_host(url: &Url) -> bool {
    match url.host() {
        Some(Host::Domain(dom)) => {
            let lower = dom.to_ascii_lowercase();
            if lower == "localhost" || lower.ends_with(".localhost") || lower.ends_with(".local") {
                return true;
            }
            lower
                .parse::<IpAddr>()
                .is_ok_and(|ip| is_disallowed_ip(&ip))
        }
        Some(Host::Ipv4(ip)) => is_disallowed_ip(&IpAddr::V4(ip)),
        Some(Host::Ipv6(ip)) => is_disallowed_ip(&IpAddr::V6(ip)),
        None => true,
    }
}

fn is_disallowed_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, _, _] = v4.octets();
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_multicast()
                || (a == 100 && (b & 0xC0) == 64)
        }
        IpAddr::V6(v6) => {
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return is_disallowed_ip(&IpAddr::V4(mapped));
            }
            let first = v6.segments()[0];
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (first & 0xFE00) == 0xFC00
                || (first & 0xFFC0) == 0xFE80
        }
    }
}

fn clean_header_value(raw: Option<&str>) -> Option<String> {
    let v = raw?.trim();
    if v.is_empty() || v.len() > 512 || v.chars().any(char::is_control) {
        None
    } else {
        Some(v.to_owned())
    }
}

fn looks_like_icalendar(body: &str) -> bool {
    body.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .is_some_and(|first| {
            first
                .get(..15)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("BEGIN:VCALENDAR"))
        })
}

// =========================================================================
// iCalendar Parsing & RFC 5545 Recurrence Expansion
// =========================================================================

pub fn parse_and_expand_ics(
    body: &str,
    subscription_id: &CalendarSubscriptionId,
    window_from: i64,
    window_to: i64,
    display_zone: &TimeZone,
) -> Result<ParsedSubscriptionFeed, SubscriptionFetchError> {
    let stripped = body.strip_prefix('\u{FEFF}').unwrap_or(body);
    if !looks_like_icalendar(stripped) {
        return Err(SubscriptionFetchError::NotICalendar);
    }
    // Normalize bare CR line endings (if any) without mutating CRLF or LF.
    let normalized;
    let source = if stripped.contains('\r') && !stripped.contains("\r\n") {
        normalized = stripped.replace('\r', "\r\n");
        normalized.as_str()
    } else {
        stripped
    };

    let mut parser = ical::IcalParser::new(source.as_bytes());
    let calendar = parser
        .next()
        .ok_or(SubscriptionFetchError::NotICalendar)?
        .map_err(|_| SubscriptionFetchError::NotICalendar)?;

    let calendar_name = prop_value(&calendar.properties, "X-WR-CALNAME")
        .or_else(|| prop_value(&calendar.properties, "NAME"))
        .map(|s| unescape_text(s).trim().to_owned())
        .filter(|s| !s.is_empty());

    let calendar_color = prop_value(&calendar.properties, "X-APPLE-CALENDAR-COLOR")
        .or_else(|| prop_value(&calendar.properties, "COLOR"))
        .and_then(normalize_hex_color);

    let default_floating_zone = prop_value(&calendar.properties, "X-WR-TIMEZONE")
        .map(|tz| resolve_tzid(tz, &calendar.timezones, display_zone))
        .unwrap_or_else(|| display_zone.clone());

    let parsed_items: Vec<ParsedVEvent> = calendar
        .events
        .iter()
        .take(MAX_FEED_EVENTS)
        .enumerate()
        .filter_map(|(idx, ev)| {
            parse_vevent(ev, idx, &calendar.timezones, &default_floating_zone, display_zone)
        })
        .collect();

    // Group by UID into master events and RECURRENCE-ID overrides.
    let mut masters: HashMap<String, Vec<ParsedVEvent>> = HashMap::new();
    let mut overrides_by_uid: HashMap<String, Vec<ParsedVEvent>> = HashMap::new();
    for item in parsed_items {
        if item.recurrence_id.is_some() {
            overrides_by_uid
                .entry(item.uid.clone())
                .or_default()
                .push(item);
        } else {
            masters.entry(item.uid.clone()).or_default().push(item);
        }
    }

    let account_id = CalendarSubscription::account_id();
    let calendar_id = CalendarId(format!("{}{}", overlook_core::SUBSCRIPTION_CALENDAR_PREFIX, subscription_id));
    let mut events = Vec::new();
    let mut seen_ids = HashSet::new();

    // Emit expanded masters, skipping any instance superseded by an override (including CANCELLED).
    for (uid, uid_masters) in masters {
        let overrides = overrides_by_uid.get(&uid);
        let overridden_keys: HashSet<OccurrenceKey> = overrides
            .into_iter()
            .flatten()
            .filter_map(|ov| ov.recurrence_id.as_ref().map(|r| r.key))
            .collect();

        for master in uid_masters {
            if master.cancelled {
                continue;
            }
            let has_recurrence = !master.rrules.is_empty() || !master.rdates.is_empty();
            let occurrences = expand_master(&master, window_from, window_to, display_zone);
            for occ in occurrences {
                if master.exdates.contains(&occ.key) || overridden_keys.contains(&occ.key) {
                    continue;
                }
                if !overlaps(occ.start_ms, occ.end_ms, window_from, window_to) {
                    continue;
                }
                let recurring_event_id = has_recurrence.then(|| uid.clone());
                let original_start = has_recurrence.then_some(occ.start_ms);
                let id = unique_event_id(
                    subscription_id,
                    &uid,
                    occ.key,
                    false,
                    &mut seen_ids,
                );
                events.push(Event {
                    id,
                    account_id: account_id.clone(),
                    calendar_id: calendar_id.clone(),
                    provider_id: None,
                    ical_uid: Some(uid.clone()),
                    summary: master.summary.clone(),
                    description: master.description.clone(),
                    location: master.location.clone(),
                    start: occ.start_ms,
                    end: occ.end_ms,
                    all_day: occ.all_day,
                    status: master.status,
                    organizer: master.organizer.clone(),
                    attendees: master.attendees.clone(),
                    html_link: master.html_link.clone(),
                    conference_url: master.conference_url.clone(),
                    recurring_event_id,
                    updated: master.updated,
                    event_type: None,
                    color: None,
                    reminders: Some(master.reminders.clone()),
                    original_start,
                    wants_conference: false,
                    pending: false,
                });
            }
        }
    }

    // Emit non-cancelled RECURRENCE-ID overrides overlapping the window.
    for (uid, overrides) in overrides_by_uid {
        for ov in overrides {
            if ov.cancelled {
                continue;
            }
            if !overlaps(ov.start_ms, ov.end_ms, window_from, window_to) {
                continue;
            }
            let orig = ov.recurrence_id.as_ref().expect("checked");
            let id = unique_event_id(subscription_id, &uid, orig.key, true, &mut seen_ids);
            events.push(Event {
                id,
                account_id: account_id.clone(),
                calendar_id: calendar_id.clone(),
                provider_id: None,
                ical_uid: Some(uid.clone()),
                summary: ov.summary,
                description: ov.description,
                location: ov.location,
                start: ov.start_ms,
                end: ov.end_ms,
                all_day: ov.all_day,
                status: ov.status,
                organizer: ov.organizer,
                attendees: ov.attendees,
                html_link: ov.html_link,
                conference_url: ov.conference_url,
                recurring_event_id: Some(uid.clone()),
                updated: ov.updated,
                event_type: None,
                color: None,
                reminders: Some(ov.reminders),
                original_start: Some(orig.timestamp_ms),
                wants_conference: false,
                pending: false,
            });
        }
    }

    events.sort_by(|a, b| {
        a.start
            .cmp(&b.start)
            .then_with(|| a.end.cmp(&b.end))
            .then_with(|| a.id.cmp(&b.id))
    });

    Ok(ParsedSubscriptionFeed {
        calendar_name,
        calendar_color,
        events,
    })
}

fn unique_event_id(
    subscription_id: &CalendarSubscriptionId,
    uid: &str,
    key: OccurrenceKey,
    is_override: bool,
    seen: &mut HashSet<String>,
) -> EventId {
    let key_str = match key {
        OccurrenceKey::AllDay(d) => format!("D:{d}"),
        OccurrenceKey::Timed(ms) => format!("T:{ms}"),
    };
    let raw = serde_json::to_string(&(uid, &key_str, is_override)).unwrap_or_default();
    let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw);
    let mut candidate = format!(
        "{}{}:{encoded}",
        overlook_core::SUBSCRIPTION_CALENDAR_PREFIX,
        subscription_id
    );
    if !seen.insert(candidate.clone()) {
        let mut suffix = 2usize;
        loop {
            let next = format!("{candidate}:{suffix}");
            if seen.insert(next.clone()) {
                candidate = next;
                break;
            }
            suffix += 1;
        }
    }
    EventId(candidate)
}

#[inline]
fn overlaps(start: i64, end: i64, from: i64, to: i64) -> bool {
    start < to && end.max(start.saturating_add(1)) > from
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum OccurrenceKey {
    AllDay(Date),
    Timed(i64),
}

#[derive(Clone, Debug)]
struct ParsedDate {
    timestamp_ms: i64,
    all_day: bool,
    civil: DateTime,
    zone: TimeZone,
    key: OccurrenceKey,
}

#[derive(Clone, Debug)]
struct ParsedVEvent {
    uid: String,
    summary: String,
    description: Option<String>,
    location: Option<String>,
    start_ms: i64,
    end_ms: i64,
    all_day: bool,
    all_day_days: i64,
    timed_duration_ms: i64,
    start_civil: DateTime,
    start_zone: TimeZone,
    start_key: OccurrenceKey,
    status: EventStatus,
    cancelled: bool,
    organizer: Option<Address>,
    attendees: Vec<Attendee>,
    html_link: Option<String>,
    conference_url: Option<String>,
    reminders: Vec<i32>,
    updated: i64,
    recurrence_id: Option<ParsedDate>,
    rrules: Vec<RRule>,
    rdates: Vec<ParsedDate>,
    exdates: HashSet<OccurrenceKey>,
}

fn prop_first<'a>(props: &'a [Property], name: &str) -> Option<&'a Property> {
    props.iter().find(|p| p.name.eq_ignore_ascii_case(name))
}

fn prop_value<'a>(props: &'a [Property], name: &str) -> Option<&'a str> {
    prop_first(props, name)?.value.as_deref()
}

fn prop_param<'a>(p: &'a Property, name: &str) -> Option<&'a str> {
    p.params
        .as_ref()?
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))?
        .1
        .first()
        .map(String::as_str)
}

fn unescape_text(v: &str) -> String {
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

fn parse_vevent(
    source: &IcalEvent,
    index: usize,
    vtimezones: &[IcalTimeZone],
    default_floating_zone: &TimeZone,
    display_zone: &TimeZone,
) -> Option<ParsedVEvent> {
    let props = &source.properties;
    let dtstart_prop = prop_first(props, "DTSTART");
    let start = match dtstart_prop {
        Some(p) => parse_ics_datetime(p, vtimezones, default_floating_zone, display_zone)?,
        None => {
            // Cancelled overrides sometimes omit DTSTART and only specify RECURRENCE-ID.
            let rec_prop = prop_first(props, "RECURRENCE-ID")?;
            parse_ics_datetime(rec_prop, vtimezones, default_floating_zone, display_zone)?
        }
    };

    let mut all_day_days = 1i64;
    let mut end_ms = if let Some(dtend_prop) = prop_first(props, "DTEND") {
        let end = parse_ics_datetime(dtend_prop, vtimezones, default_floating_zone, display_zone)?;
        if start.all_day {
            let days = (end.civil.date() - start.civil.date()).get_days() as i64;
            all_day_days = days.max(1);
            let adjusted_end_date = start
                .civil
                .date()
                .checked_add(all_day_days.days())
                .unwrap_or(end.civil.date());
            adjusted_end_date
                .at(0, 0, 0, 0)
                .to_zoned(display_zone.clone())
                .ok()?
                .timestamp()
                .as_millisecond()
        } else {
            end.timestamp_ms.max(start.timestamp_ms)
        }
    } else if let Some(dur_raw) = prop_value(props, "DURATION") {
        let span = parse_ics_duration(dur_raw).unwrap_or_else(|| if start.all_day { 1.day() } else { 0.seconds() });
        if start.all_day {
            let days = (i64::from(span.get_weeks()) * 7 + i64::from(span.get_days())).max(1);
            all_day_days = days;
            let end_date = start
                .civil
                .date()
                .checked_add(days.days())
                .unwrap_or(start.civil.date());
            end_date
                .at(0, 0, 0, 0)
                .to_zoned(display_zone.clone())
                .ok()?
                .timestamp()
                .as_millisecond()
        } else {
            jiff::Timestamp::from_millisecond(start.timestamp_ms)
                .ok()?
                .to_zoned(start.zone.clone())
                .checked_add(span)
                .ok()?
                .timestamp()
                .as_millisecond()
                .max(start.timestamp_ms)
        }
    } else if start.all_day {
        start
            .civil
            .date()
            .checked_add(1.day())
            .ok()?
            .at(0, 0, 0, 0)
            .to_zoned(display_zone.clone())
            .ok()?
            .timestamp()
            .as_millisecond()
    } else {
        start.timestamp_ms
    };

    if end_ms < start.timestamp_ms {
        end_ms = start.timestamp_ms;
    }

    let timed_duration_ms = if start.all_day {
        0
    } else {
        end_ms.saturating_sub(start.timestamp_ms)
    };

    let uid = prop_value(props, "UID")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| format!("synthetic-{index}-{}", start.timestamp_ms));

    let raw_status = prop_value(props, "STATUS").map(str::trim);
    let cancelled = raw_status.is_some_and(|s| s.eq_ignore_ascii_case("CANCELLED"));
    let status = if raw_status.is_some_and(|s| s.eq_ignore_ascii_case("TENTATIVE")) {
        EventStatus::Tentative
    } else if cancelled {
        EventStatus::Cancelled
    } else {
        EventStatus::Confirmed
    };

    let recurrence_id = prop_first(props, "RECURRENCE-ID")
        .and_then(|p| parse_ics_datetime(p, vtimezones, default_floating_zone, display_zone));

    let rrules = props
        .iter()
        .filter(|p| p.name.eq_ignore_ascii_case("RRULE"))
        .filter_map(|p| p.value.as_deref())
        .filter_map(|raw| parse_rrule(raw, vtimezones, default_floating_zone, display_zone))
        .collect();

    let mut rdates = Vec::new();
    for p in props.iter().filter(|p| p.name.eq_ignore_ascii_case("RDATE")) {
        rdates.extend(parse_date_list(p, vtimezones, default_floating_zone, display_zone));
    }

    let mut exdates = HashSet::new();
    for p in props.iter().filter(|p| p.name.eq_ignore_ascii_case("EXDATE")) {
        for parsed in parse_date_list(p, vtimezones, default_floating_zone, display_zone) {
            exdates.insert(parsed.key);
        }
    }

    let organizer = prop_first(props, "ORGANIZER").and_then(parse_address);
    let attendees = props
        .iter()
        .filter(|p| p.name.eq_ignore_ascii_case("ATTENDEE"))
        .filter_map(|p| {
            let addr = parse_address(p)?;
            Some(Attendee {
                is_self: false,
                is_organizer: organizer
                    .as_ref()
                    .is_some_and(|o| o.email.eq_ignore_ascii_case(&addr.email)),
                email: addr.email,
                name: addr.name,
                response: match prop_param(p, "PARTSTAT").map(|s| s.to_ascii_uppercase()).as_deref() {
                    Some("ACCEPTED") => ResponseStatus::Accepted,
                    Some("DECLINED") => ResponseStatus::Declined,
                    Some("TENTATIVE") => ResponseStatus::Tentative,
                    _ => ResponseStatus::NeedsAction,
                },
                optional: prop_param(p, "ROLE").is_some_and(|r| r.eq_ignore_ascii_case("OPT-PARTICIPANT")),
            })
        })
        .collect();

    let html_link = prop_value(props, "URL").and_then(sanitize_http_url);
    let conference_url = prop_value(props, "CONFERENCE")
        .or_else(|| prop_value(props, "X-GOOGLE-CONFERENCE"))
        .and_then(sanitize_http_url);

    let mut reminders = Vec::new();
    for alarm in &source.alarms {
        if !prop_value(&alarm.properties, "ACTION")
            .is_some_and(|a| a.trim().eq_ignore_ascii_case("DISPLAY"))
        {
            continue;
        }
        let Some(trigger) = prop_first(&alarm.properties, "TRIGGER") else {
            continue;
        };
        if prop_param(trigger, "RELATED").is_some_and(|v| !v.eq_ignore_ascii_case("START")) {
            continue;
        }
        let Some(raw) = trigger.value.as_deref().map(str::trim).and_then(|v| v.strip_prefix('-')) else {
            continue;
        };
        let Some(span) = parse_ics_duration(raw) else {
            continue;
        };
        let minutes = i64::from(span.get_weeks()) * 10_080
            + i64::from(span.get_days()) * 1_440
            + i64::from(span.get_hours()) * 60
            + i64::from(span.get_minutes());
        if (0..=525_600).contains(&minutes) {
            reminders.push(minutes as i32);
        }
    }
    reminders.sort_unstable();
    reminders.dedup();

    let updated = prop_first(props, "LAST-MODIFIED")
        .or_else(|| prop_first(props, "DTSTAMP"))
        .and_then(|p| parse_ics_datetime(p, vtimezones, default_floating_zone, display_zone))
        .map(|d| d.timestamp_ms)
        .unwrap_or(0);

    Some(ParsedVEvent {
        uid,
        summary: prop_value(props, "SUMMARY").map(unescape_text).unwrap_or_default(),
        description: prop_value(props, "DESCRIPTION").map(unescape_text),
        location: prop_value(props, "LOCATION").map(unescape_text),
        start_ms: start.timestamp_ms,
        end_ms,
        all_day: start.all_day,
        all_day_days,
        timed_duration_ms,
        start_civil: start.civil,
        start_zone: start.zone,
        start_key: start.key,
        status,
        cancelled,
        organizer,
        attendees,
        html_link,
        conference_url,
        reminders,
        updated,
        recurrence_id,
        rrules,
        rdates,
        exdates,
    })
}

fn sanitize_http_url(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.len() > 4096 || trimmed.chars().any(char::is_control) {
        return None;
    }
    let parsed = Url::parse(trimmed).ok()?;
    if matches!(parsed.scheme(), "https" | "http") && parsed.host_str().is_some_and(|h| !h.is_empty()) {
        Some(parsed.into())
    } else {
        None
    }
}

fn parse_address(p: &Property) -> Option<Address> {
    let raw = p.value.as_deref()?.trim();
    let (scheme, email) = raw.split_once(':')?;
    if !scheme.eq_ignore_ascii_case("mailto")
        || email.is_empty()
        || email.chars().any(char::is_control)
    {
        return None;
    }
    Some(Address {
        email: email.to_owned(),
        name: prop_param(p, "CN")
            .map(|s| s.replace("^n", "\n").replace("^'", "\"").replace("^^", "^")),
    })
}

fn parse_ics_duration(raw: &str) -> Option<Span> {
    let trimmed = raw.trim();
    let unsigned = trimmed.strip_prefix('+').unwrap_or(trimmed);
    let days_part = unsigned.split('T').next().unwrap_or(unsigned);
    if days_part.contains(['Y', 'M']) {
        return None;
    }
    unsigned.parse::<Span>().ok()
}

fn parse_date_list(
    p: &Property,
    vtimezones: &[IcalTimeZone],
    default_floating_zone: &TimeZone,
    display_zone: &TimeZone,
) -> Vec<ParsedDate> {
    let Some(raw) = p.value.as_deref() else {
        return Vec::new();
    };
    raw.split(',')
        .filter_map(|item| {
            parse_ics_value_str(
                item.trim(),
                prop_param(p, "VALUE"),
                prop_param(p, "TZID"),
                vtimezones,
                default_floating_zone,
                display_zone,
            )
        })
        .collect()
}

fn parse_ics_datetime(
    p: &Property,
    vtimezones: &[IcalTimeZone],
    default_floating_zone: &TimeZone,
    display_zone: &TimeZone,
) -> Option<ParsedDate> {
    let raw = p.value.as_deref()?.trim();
    parse_ics_value_str(
        raw,
        prop_param(p, "VALUE"),
        prop_param(p, "TZID"),
        vtimezones,
        default_floating_zone,
        display_zone,
    )
}

fn parse_ics_value_str(
    raw: &str,
    value_param: Option<&str>,
    tzid_param: Option<&str>,
    vtimezones: &[IcalTimeZone],
    default_floating_zone: &TimeZone,
    display_zone: &TimeZone,
) -> Option<ParsedDate> {
    if !raw.is_ascii() || raw.is_empty() {
        return None;
    }
    let is_date = value_param.is_some_and(|v| v.eq_ignore_ascii_case("DATE")) || raw.len() == 8;
    if is_date {
        if raw.len() < 8 {
            return None;
        }
        let date = Date::strptime("%Y%m%d", &raw[..8]).ok()?;
        let civil = date.at(0, 0, 0, 0);
        let ts = civil
            .to_zoned(display_zone.clone())
            .ok()?
            .timestamp()
            .as_millisecond();
        return Some(ParsedDate {
            timestamp_ms: ts,
            all_day: true,
            civil,
            zone: display_zone.clone(),
            key: OccurrenceKey::AllDay(date),
        });
    }

    let (dt_str, zone) = if let Some(utc) = raw.strip_suffix('Z') {
        (utc, TimeZone::UTC)
    } else if let Some(tzid) = tzid_param {
        (raw, resolve_tzid(tzid, vtimezones, default_floating_zone))
    } else {
        (raw, default_floating_zone.clone())
    };

    let civil = if dt_str.len() == 15 {
        DateTime::strptime("%Y%m%dT%H%M%S", dt_str).ok()?
    } else if dt_str.len() == 13 {
        DateTime::strptime("%Y%m%dT%H%M", dt_str).ok()?
    } else {
        return None;
    };

    let ts = civil
        .to_zoned(zone.clone())
        .ok()?
        .timestamp()
        .as_millisecond();

    Some(ParsedDate {
        timestamp_ms: ts,
        all_day: false,
        civil,
        zone,
        key: OccurrenceKey::Timed(ts),
    })
}

fn resolve_tzid(tzid: &str, vtimezones: &[IcalTimeZone], fallback: &TimeZone) -> TimeZone {
    let cleaned = tzid.trim().trim_matches('"').trim();
    if cleaned.is_empty() {
        return fallback.clone();
    }
    if let Ok(z) = TimeZone::get(cleaned) {
        return z;
    }

    // Handle Apple/Mozilla prefixed TZIDs like `/apple.com/20050126_1/America/Denver`.
    let segments: Vec<&str> = cleaned.split('/').filter(|s| !s.is_empty()).collect();
    for take in [3usize, 2, 1] {
        if segments.len() >= take {
            let candidate = segments[segments.len() - take..].join("/");
            if let Ok(z) = TimeZone::get(&candidate) {
                return z;
            }
        }
    }

    if let Some(iana) = windows_or_alias_tz(cleaned) {
        if let Ok(z) = TimeZone::get(iana) {
            return z;
        }
    }

    // Fall back to VTIMEZONE TZOFFSETTO if defined in the feed.
    for vtz in vtimezones {
        let Some(vtz_id) = prop_value(&vtz.properties, "TZID").map(|s| s.trim().trim_matches('"')) else {
            continue;
        };
        if !vtz_id.eq_ignore_ascii_case(cleaned) {
            continue;
        }
        let transition = vtz
            .transitions
            .iter()
            .find(|t| matches!(t.transition, ical::parser::ical::component::IcalTimeZoneTransitionType::STANDARD))
            .or_else(|| vtz.transitions.first());
        if let Some(offset_raw) = transition.and_then(|t| prop_value(&t.properties, "TZOFFSETTO")) {
            if let Some(offset) = parse_tz_offset(offset_raw) {
                return TimeZone::fixed(offset);
            }
        }
    }

    fallback.clone()
}

fn parse_tz_offset(raw: &str) -> Option<Offset> {
    let s = raw.trim();
    let (sign, digits) = match s.as_bytes().first()? {
        b'+' => (1i32, &s[1..]),
        b'-' => (-1i32, &s[1..]),
        _ => return None,
    };
    if digits.len() != 4 && digits.len() != 6 {
        return None;
    }
    let hours: i32 = digits[0..2].parse().ok()?;
    let mins: i32 = digits[2..4].parse().ok()?;
    let secs: i32 = if digits.len() == 6 {
        digits[4..6].parse().ok()?
    } else {
        0
    };
    let total = sign * (hours * 3600 + mins * 60 + secs);
    Offset::from_seconds(total).ok()
}

fn windows_or_alias_tz(tzid: &str) -> Option<&'static str> {
    match tzid.to_ascii_lowercase().as_str() {
        "utc" | "etc/utc" | "etc/gmt" | "z" | "gmt" | "greenwich standard time" => Some("UTC"),
        "pacific standard time" | "us/pacific" => Some("America/Los_Angeles"),
        "mountain standard time" | "us/mountain" => Some("America/Denver"),
        "us mountain standard time" | "us/arizona" => Some("America/Phoenix"),
        "central standard time" | "us/central" => Some("America/Chicago"),
        "eastern standard time" | "us/eastern" => Some("America/New_York"),
        "alaskan standard time" | "us/alaska" => Some("America/Anchorage"),
        "hawaiian standard time" | "us/hawaii" => Some("Pacific/Honolulu"),
        "atlantic standard time" => Some("America/Halifax"),
        "newfoundland standard time" => Some("America/St_Johns"),
        "gmt standard time" => Some("Europe/London"),
        "w. europe standard time" => Some("Europe/Berlin"),
        "romance standard time" => Some("Europe/Paris"),
        "central europe standard time" => Some("Europe/Prague"),
        "central european standard time" => Some("Europe/Warsaw"),
        "e. europe standard time" => Some("Europe/Chisinau"),
        "gtb standard time" => Some("Europe/Bucharest"),
        "russian standard time" => Some("Europe/Moscow"),
        "tokyo standard time" => Some("Asia/Tokyo"),
        "china standard time" => Some("Asia/Shanghai"),
        "korea standard time" => Some("Asia/Seoul"),
        "singapore standard time" => Some("Asia/Singapore"),
        "india standard time" => Some("Asia/Kolkata"),
        "arabian standard time" => Some("Asia/Dubai"),
        "israel standard time" => Some("Asia/Jerusalem"),
        "aus eastern standard time" => Some("Australia/Sydney"),
        "new zealand standard time" => Some("Pacific/Auckland"),
        "e. south america standard time" => Some("America/Sao_Paulo"),
        _ => None,
    }
}

// =========================================================================
// RRULE Parser & Expander
// =========================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Freq {
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ByDay {
    ordinal: Option<i8>,
    weekday: Weekday,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UntilBound {
    Date(Date),
    Timestamp(i64),
}

#[derive(Clone, Debug)]
struct RRule {
    freq: Freq,
    interval: i32,
    count: Option<usize>,
    until: Option<UntilBound>,
    by_month: Vec<i8>,
    by_month_day: Vec<i8>,
    by_day: Vec<ByDay>,
    by_set_pos: Vec<i32>,
    wkst: Weekday,
}

fn parse_weekday(raw: &str) -> Option<Weekday> {
    match raw.to_ascii_uppercase().as_str() {
        "MO" => Some(Weekday::Monday),
        "TU" => Some(Weekday::Tuesday),
        "WE" => Some(Weekday::Wednesday),
        "TH" => Some(Weekday::Thursday),
        "FR" => Some(Weekday::Friday),
        "SA" => Some(Weekday::Saturday),
        "SU" => Some(Weekday::Sunday),
        _ => None,
    }
}

fn parse_byday(raw: &str) -> Option<ByDay> {
    let trimmed = raw.trim();
    if trimmed.len() < 2 {
        return None;
    }
    let split = trimmed.len() - 2;
    let weekday = parse_weekday(&trimmed[split..])?;
    let ord_str = trimmed[..split].trim();
    let ordinal = if ord_str.is_empty() {
        None
    } else {
        let ord: i8 = ord_str.parse().ok()?;
        if ord == 0 || !(-53..=53).contains(&ord) {
            return None;
        }
        Some(ord)
    };
    Some(ByDay { ordinal, weekday })
}

fn parse_rrule(
    raw: &str,
    vtimezones: &[IcalTimeZone],
    default_floating_zone: &TimeZone,
    display_zone: &TimeZone,
) -> Option<RRule> {
    let mut freq = None;
    let mut interval = 1i32;
    let mut count = None;
    let mut until = None;
    let mut by_month = Vec::new();
    let mut by_month_day = Vec::new();
    let mut by_day = Vec::new();
    let mut by_set_pos = Vec::new();
    let mut wkst = Weekday::Monday;

    for part in raw.split(';').map(str::trim).filter(|s| !s.is_empty()) {
        let Some((k, v)) = part.split_once('=') else {
            continue;
        };
        let v = v.trim();
        match k.trim().to_ascii_uppercase().as_str() {
            "FREQ" => {
                freq = match v.to_ascii_uppercase().as_str() {
                    "DAILY" => Some(Freq::Daily),
                    "WEEKLY" => Some(Freq::Weekly),
                    "MONTHLY" => Some(Freq::Monthly),
                    "YEARLY" => Some(Freq::Yearly),
                    _ => return None,
                };
            }
            "INTERVAL" => {
                if let Ok(n) = v.parse::<i32>() {
                    interval = n.clamp(1, 10_000);
                }
            }
            "COUNT" => {
                if let Ok(n) = v.parse::<usize>() {
                    count = Some(n);
                }
            }
            "UNTIL" => {
                if let Some(parsed) = parse_ics_value_str(
                    v,
                    None,
                    None,
                    vtimezones,
                    default_floating_zone,
                    display_zone,
                ) {
                    until = Some(if parsed.all_day {
                        UntilBound::Date(parsed.civil.date())
                    } else {
                        UntilBound::Timestamp(parsed.timestamp_ms)
                    });
                }
            }
            "BYMONTH" => {
                for item in v.split(',') {
                    if let Ok(m) = item.trim().parse::<i8>() {
                        if (1..=12).contains(&m) {
                            by_month.push(m);
                        }
                    }
                }
            }
            "BYMONTHDAY" => {
                for item in v.split(',') {
                    if let Ok(d) = item.trim().parse::<i8>() {
                        if d != 0 && (-31..=31).contains(&d) {
                            by_month_day.push(d);
                        }
                    }
                }
            }
            "BYDAY" => {
                for item in v.split(',') {
                    if let Some(bd) = parse_byday(item) {
                        by_day.push(bd);
                    }
                }
            }
            "BYSETPOS" => {
                for item in v.split(',') {
                    if let Ok(pos) = item.trim().parse::<i32>() {
                        if pos != 0 {
                            by_set_pos.push(pos);
                        }
                    }
                }
            }
            "WKST" => {
                if let Some(w) = parse_weekday(v) {
                    wkst = w;
                }
            }
            _ => {}
        }
    }

    Some(RRule {
        freq: freq?,
        interval,
        count,
        until,
        by_month,
        by_month_day,
        by_day,
        by_set_pos,
        wkst,
    })
}

#[derive(Clone, Copy, Debug)]
struct ExpandedOccurrence {
    key: OccurrenceKey,
    start_ms: i64,
    end_ms: i64,
    all_day: bool,
}

fn occurrence_from_date(
    date: Date,
    master: &ParsedVEvent,
    display_zone: &TimeZone,
) -> Option<ExpandedOccurrence> {
    if master.all_day {
        let start_ms = date
            .at(0, 0, 0, 0)
            .to_zoned(display_zone.clone())
            .ok()?
            .timestamp()
            .as_millisecond();
        let end_date = date.checked_add(master.all_day_days.days()).ok()?;
        let end_ms = end_date
            .at(0, 0, 0, 0)
            .to_zoned(display_zone.clone())
            .ok()?
            .timestamp()
            .as_millisecond();
        Some(ExpandedOccurrence {
            key: OccurrenceKey::AllDay(date),
            start_ms,
            end_ms: end_ms.max(start_ms),
            all_day: true,
        })
    } else {
        let civil = date.at(
            master.start_civil.hour(),
            master.start_civil.minute(),
            master.start_civil.second(),
            0,
        );
        let start_ms = civil
            .to_zoned(master.start_zone.clone())
            .ok()?
            .timestamp()
            .as_millisecond();
        let end_ms = start_ms.saturating_add(master.timed_duration_ms);
        Some(ExpandedOccurrence {
            key: OccurrenceKey::Timed(start_ms),
            start_ms,
            end_ms,
            all_day: false,
        })
    }
}

fn expand_master(
    master: &ParsedVEvent,
    window_from: i64,
    window_to: i64,
    display_zone: &TimeZone,
) -> Vec<ExpandedOccurrence> {
    if master.rrules.is_empty() && master.rdates.is_empty() {
        return vec![ExpandedOccurrence {
            key: master.start_key,
            start_ms: master.start_ms,
            end_ms: master.end_ms,
            all_day: master.all_day,
        }];
    }

    let mut out = Vec::new();
    let mut seen_keys = HashSet::new();

    if master.rrules.is_empty() {
        // RDATE-only event still includes its own DTSTART.
        seen_keys.insert(master.start_key);
        out.push(ExpandedOccurrence {
            key: master.start_key,
            start_ms: master.start_ms,
            end_ms: master.end_ms,
            all_day: master.all_day,
        });
    } else {
        for rule in &master.rrules {
            for occ in expand_single_rrule(master, rule, window_from, window_to, display_zone) {
                if seen_keys.insert(occ.key) {
                    out.push(occ);
                    if out.len() >= MAX_OCCURRENCES_PER_EVENT {
                        break;
                    }
                }
            }
            if out.len() >= MAX_OCCURRENCES_PER_EVENT {
                break;
            }
        }
    }

    for rdate in &master.rdates {
        if out.len() >= MAX_OCCURRENCES_PER_EVENT {
            break;
        }
        let occ = if rdate.all_day && master.all_day {
            occurrence_from_date(rdate.civil.date(), master, display_zone)
        } else if !rdate.all_day && !master.all_day {
            let start_ms = rdate.timestamp_ms;
            Some(ExpandedOccurrence {
                key: OccurrenceKey::Timed(start_ms),
                start_ms,
                end_ms: start_ms.saturating_add(master.timed_duration_ms),
                all_day: false,
            })
        } else {
            occurrence_from_date(rdate.civil.date(), master, display_zone)
        };
        if let Some(occ) = occ {
            if seen_keys.insert(occ.key) {
                out.push(occ);
            }
        }
    }

    out
}

fn expand_single_rrule(
    master: &ParsedVEvent,
    rule: &RRule,
    window_from: i64,
    window_to: i64,
    display_zone: &TimeZone,
) -> Vec<ExpandedOccurrence> {
    let start_date = master.start_civil.date();
    let mut out = Vec::new();

    if rule.count == Some(0) {
        return out;
    }

    // Approximate lower-bound date so occurrences overlapping `window_from` aren't skipped.
    let min_start_ms = window_from.saturating_sub(if master.all_day {
        master.all_day_days.saturating_mul(86_400_000)
    } else {
        master.timed_duration_ms
    });
    let window_min_date = jiff::Timestamp::from_millisecond(min_start_ms)
        .ok()
        .map(|ts| ts.to_zoned(master.start_zone.clone()).date())
        .and_then(|d| d.checked_sub(7.days()).ok())
        .unwrap_or(start_date);
    let window_max_date = jiff::Timestamp::from_millisecond(window_to)
        .ok()
        .map(|ts| ts.to_zoned(master.start_zone.clone()).date())
        .and_then(|d| d.checked_add(2.days()).ok());

    let mut period_index: i64 = 0;
    if rule.count.is_none() && window_min_date > start_date {
        period_index = fast_forward_period(rule, start_date, window_min_date);
    }

    let mut total_for_count = 0usize;

    for _step in 0..MAX_RULE_STEPS {
        if let Some(max_date) = window_max_date {
            if period_start_upper_check(rule, start_date, period_index, max_date) {
                break;
            }
        }

        let dates = period_candidate_dates(rule, start_date, period_index);

        for date in dates {
            if date < start_date {
                continue;
            }
            let Some(occ) = occurrence_from_date(date, master, display_zone) else {
                continue;
            };
            if let Some(until) = rule.until {
                let past_until = match until {
                    UntilBound::Date(d) => date > d,
                    UntilBound::Timestamp(until_ms) => occ.start_ms > until_ms,
                };
                if past_until {
                    return out;
                }
            }
            if occ.start_ms >= window_to {
                return out;
            }
            if let Some(max_count) = rule.count {
                total_for_count += 1;
                if total_for_count > max_count {
                    return out;
                }
            }
            if overlaps(occ.start_ms, occ.end_ms, window_from, window_to) {
                out.push(occ);
                if out.len() >= MAX_OCCURRENCES_PER_EVENT {
                    return out;
                }
            }
        }

        period_index += 1;
    }

    out
}

fn fast_forward_period(rule: &RRule, start_date: Date, target: Date) -> i64 {
    let interval = i64::from(rule.interval.max(1));
    let delta_days = (target - start_date).get_days() as i64;
    if delta_days <= 0 {
        return 0;
    }
    match rule.freq {
        Freq::Daily => (delta_days / interval).saturating_sub(1).max(0),
        Freq::Weekly => (delta_days / (7 * interval)).saturating_sub(1).max(0),
        Freq::Monthly => {
            let months = (i64::from(target.year()) - i64::from(start_date.year())) * 12
                + (i64::from(target.month()) - i64::from(start_date.month()));
            (months / interval).saturating_sub(1).max(0)
        }
        Freq::Yearly => {
            let years = i64::from(target.year()) - i64::from(start_date.year());
            (years / interval).saturating_sub(1).max(0)
        }
    }
}

fn period_start_upper_check(rule: &RRule, start_date: Date, period: i64, max_date: Date) -> bool {
    let step = period.saturating_mul(i64::from(rule.interval.max(1)));
    match rule.freq {
        Freq::Daily => start_date.checked_add(step.days()).is_ok_and(|d| d > max_date),
        Freq::Weekly => {
            let week0_start = week_start_date(start_date, rule.wkst);
            week0_start
                .checked_add((step * 7).days())
                .is_ok_and(|d| d > max_date)
        }
        Freq::Monthly => add_months(start_date.year(), start_date.month(), step)
            .and_then(|(y, m)| Date::new(y, m, 1).ok())
            .is_some_and(|d| d > max_date),
        Freq::Yearly => i16::try_from(i64::from(start_date.year()) + step)
            .ok()
            .and_then(|y| Date::new(y, 1, 1).ok())
            .is_some_and(|d| d > max_date),
    }
}

fn week_start_date(date: Date, wkst: Weekday) -> Date {
    let offset = (date.weekday().to_monday_zero_offset() - wkst.to_monday_zero_offset()).rem_euclid(7);
    date.checked_sub(i64::from(offset).days()).unwrap_or(date)
}

fn add_months(year: i16, month: i8, offset_months: i64) -> Option<(i16, i8)> {
    let total = i64::from(year) * 12 + i64::from(month - 1) + offset_months;
    let y = i16::try_from(total.div_euclid(12)).ok()?;
    let m = (total.rem_euclid(12) + 1) as i8;
    Some((y, m))
}

fn days_in_month(year: i16, month: i8) -> i8 {
    Date::new(year, month, 1).map(|d| d.days_in_month()).unwrap_or(30)
}

fn apply_bysetpos(sorted_dates: Vec<Date>, by_set_pos: &[i32]) -> Vec<Date> {
    if by_set_pos.is_empty() {
        return sorted_dates;
    }
    let len = sorted_dates.len() as i32;
    let mut picked = BTreeSet::new();
    for &pos in by_set_pos {
        let idx = if pos > 0 { pos - 1 } else { len + pos };
        if idx >= 0 && idx < len {
            picked.insert(sorted_dates[idx as usize]);
        }
    }
    picked.into_iter().collect()
}

fn period_candidate_dates(rule: &RRule, start_date: Date, period: i64) -> Vec<Date> {
    let step = period.saturating_mul(i64::from(rule.interval.max(1)));
    let candidates: Vec<Date> = match rule.freq {
        Freq::Daily => {
            let Ok(d) = start_date.checked_add(step.days()) else {
                return Vec::new();
            };
            if !rule.by_month.is_empty() && !rule.by_month.contains(&d.month()) {
                return Vec::new();
            }
            if !rule.by_month_day.is_empty() && !matches_bymonthday(d, &rule.by_month_day) {
                return Vec::new();
            }
            if !rule.by_day.is_empty() && !rule.by_day.iter().any(|bd| bd.weekday == d.weekday()) {
                return Vec::new();
            }
            vec![d]
        }
        Freq::Weekly => {
            let week0 = week_start_date(start_date, rule.wkst);
            let Ok(ws) = week0.checked_add((step * 7).days()) else {
                return Vec::new();
            };
            let target_weekdays: Vec<Weekday> = if rule.by_day.is_empty() {
                vec![start_date.weekday()]
            } else {
                rule.by_day.iter().map(|bd| bd.weekday).collect()
            };
            let mut set = BTreeSet::new();
            for offset in 0..7i64 {
                if let Ok(d) = ws.checked_add(offset.days()) {
                    if target_weekdays.contains(&d.weekday())
                        && (rule.by_month.is_empty() || rule.by_month.contains(&d.month()))
                        && (rule.by_month_day.is_empty() || matches_bymonthday(d, &rule.by_month_day))
                    {
                        set.insert(d);
                    }
                }
            }
            set.into_iter().collect()
        }
        Freq::Monthly => {
            let Some((y, m)) = add_months(start_date.year(), start_date.month(), step) else {
                return Vec::new();
            };
            if !rule.by_month.is_empty() && !rule.by_month.contains(&m) {
                return Vec::new();
            }
            monthly_candidates(y, m, start_date.day(), rule)
        }
        Freq::Yearly => {
            let Ok(y) = i16::try_from(i64::from(start_date.year()) + step) else {
                return Vec::new();
            };
            let months: Vec<i8> = if !rule.by_month.is_empty() {
                let mut ms = rule.by_month.clone();
                ms.sort_unstable();
                ms.dedup();
                ms
            } else if !rule.by_day.is_empty() && rule.by_month_day.is_empty() {
                // YEARLY with BYDAY and no BYMONTH scopes ordinal BYDAY to the full year.
                return yearly_byday_candidates(y, &rule.by_day, &rule.by_set_pos);
            } else {
                vec![start_date.month()]
            };
            let mut set = BTreeSet::new();
            for m in months {
                for d in monthly_candidates(y, m, start_date.day(), rule) {
                    set.insert(d);
                }
            }
            set.into_iter().collect()
        }
    };

    apply_bysetpos(candidates, &rule.by_set_pos)
}

fn monthly_candidates(year: i16, month: i8, default_day: i8, rule: &RRule) -> Vec<Date> {
    let dim = days_in_month(year, month);
    let mut set = BTreeSet::new();

    if rule.by_day.is_empty() && rule.by_month_day.is_empty() {
        if default_day <= dim {
            if let Ok(d) = Date::new(year, month, default_day) {
                set.insert(d);
            }
        }
    } else if !rule.by_month_day.is_empty() {
        for &md in &rule.by_month_day {
            let resolved = if md > 0 { md } else { dim + 1 + md };
            if resolved >= 1 && resolved <= dim {
                if let Ok(d) = Date::new(year, month, resolved) {
                    if rule.by_day.is_empty()
                        || rule.by_day.iter().any(|bd| bd.weekday == d.weekday())
                    {
                        set.insert(d);
                    }
                }
            }
        }
    } else {
        for bd in &rule.by_day {
            let matching: Vec<Date> = (1..=dim)
                .filter_map(|day| Date::new(year, month, day).ok())
                .filter(|d| d.weekday() == bd.weekday)
                .collect();
            match bd.ordinal {
                None => {
                    for d in matching {
                        set.insert(d);
                    }
                }
                Some(ord) if ord > 0 => {
                    if let Some(d) = matching.get((ord - 1) as usize) {
                        set.insert(*d);
                    }
                }
                Some(ord) => {
                    let idx = matching.len() as isize + ord as isize;
                    if idx >= 0 {
                        if let Some(d) = matching.get(idx as usize) {
                            set.insert(*d);
                        }
                    }
                }
            }
        }
    }

    set.into_iter().collect()
}

fn yearly_byday_candidates(year: i16, by_day: &[ByDay], by_set_pos: &[i32]) -> Vec<Date> {
    let mut set = BTreeSet::new();
    let all_days: Vec<Date> = (1..=12i8)
        .flat_map(|m| {
            let dim = days_in_month(year, m);
            (1..=dim).filter_map(move |day| Date::new(year, m, day).ok())
        })
        .collect();

    for bd in by_day {
        let matching: Vec<Date> = all_days
            .iter()
            .copied()
            .filter(|d| d.weekday() == bd.weekday)
            .collect();
        match bd.ordinal {
            None => {
                for d in matching {
                    set.insert(d);
                }
            }
            Some(ord) if ord > 0 => {
                if let Some(d) = matching.get((ord - 1) as usize) {
                    set.insert(*d);
                }
            }
            Some(ord) => {
                let idx = matching.len() as isize + ord as isize;
                if idx >= 0 {
                    if let Some(d) = matching.get(idx as usize) {
                        set.insert(*d);
                    }
                }
            }
        }
    }
    apply_bysetpos(set.into_iter().collect(), by_set_pos)
}

fn matches_bymonthday(date: Date, by_month_day: &[i8]) -> bool {
    let dim = date.days_in_month();
    by_month_day.iter().any(|&md| {
        let resolved = if md > 0 { md } else { dim + 1 + md };
        date.day() == resolved
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use overlook_core::tz;
    use std::io::Write;
    use std::net::TcpListener;
    use std::thread;

    fn sub_id() -> CalendarSubscriptionId {
        CalendarSubscriptionId::from("apple-test")
    }

    fn ts(s: &str) -> i64 {
        tz::parse_rfc3339(s).unwrap()
    }

    #[test]
    fn parses_apple_icloud_style_feed_with_metadata_vtimezone_all_day_and_valarm() {
        let ics = "\
BEGIN:VCALENDAR\r\n\
VERSION:2.0\r\n\
PRODID:-//Apple Inc.//macOS 15.0//EN\r\n\
X-WR-CALNAME:Family\\, Events\r\n\
X-APPLE-CALENDAR-COLOR:#1BADF8FF\r\n\
BEGIN:VTIMEZONE\r\n\
TZID:/apple.com/20050126_1/America/Denver\r\n\
BEGIN:STANDARD\r\n\
TZOFFSETFROM:-0600\r\n\
TZOFFSETTO:-0700\r\n\
END:STANDARD\r\n\
END:VTIMEZONE\r\n\
BEGIN:VEVENT\r\n\
UID:apple-1@icloud.com\r\n\
SUMMARY:Soccer Practice\r\n\
LOCATION:Field\\; North\r\n\
DESCRIPTION:Bring cleats\\nWater bottle\r\n\
URL:https://example.com/schedule\r\n\
DTSTART;TZID=/apple.com/20050126_1/America/Denver:20261014T160000\r\n\
DTEND;TZID=/apple.com/20050126_1/America/Denver:20261014T173000\r\n\
BEGIN:VALARM\r\n\
ACTION:DISPLAY\r\n\
TRIGGER:-PT15M\r\n\
DESCRIPTION:Reminder\r\n\
END:VALARM\r\n\
END:VEVENT\r\n\
BEGIN:VEVENT\r\n\
UID:apple-allday@icloud.com\r\n\
SUMMARY:Fall Break\r\n\
DTSTART;VALUE=DATE:20261015\r\n\
DTEND;VALUE=DATE:20261017\r\n\
END:VEVENT\r\n\
END:VCALENDAR\r\n";

        let zone = TimeZone::get("America/Phoenix").unwrap();
        let feed = parse_and_expand_ics(
            ics,
            &sub_id(),
            ts("2026-10-01T00:00:00Z"),
            ts("2026-10-31T23:59:59Z"),
            &zone,
        )
        .unwrap();

        assert_eq!(feed.calendar_name.as_deref(), Some("Family, Events"));
        assert_eq!(feed.calendar_color.as_deref(), Some("#1BADF8"));
        assert_eq!(feed.events.len(), 2);

        let timed = &feed.events[0];
        assert_eq!(timed.account_id, CalendarSubscription::account_id());
        assert_eq!(timed.calendar_id, CalendarId::from("sub:apple-test"));
        assert_eq!(timed.summary, "Soccer Practice");
        assert_eq!(timed.location.as_deref(), Some("Field; North"));
        assert_eq!(timed.description.as_deref(), Some("Bring cleats\nWater bottle"));
        assert_eq!(timed.html_link.as_deref(), Some("https://example.com/schedule"));
        assert_eq!(timed.start, ts("2026-10-14T22:00:00Z"));
        assert_eq!(timed.end, ts("2026-10-14T23:30:00Z"));
        assert_eq!(timed.reminders, Some(vec![15]));

        let all_day = &feed.events[1];
        assert!(all_day.all_day);
        assert_eq!(all_day.start, ts("2026-10-15T07:00:00Z"));
        assert_eq!(all_day.end, ts("2026-10-17T07:00:00Z"));
        assert_eq!(all_day.reminders, Some(vec![]));
    }

    #[test]
    fn expands_daily_weekly_monthly_and_yearly_rrules_including_ordinals_and_bysetpos() {
        let ics = "\
BEGIN:VCALENDAR\r\n\
VERSION:2.0\r\n\
BEGIN:VEVENT\r\n\
UID:daily\r\n\
SUMMARY:Daily standup\r\n\
DTSTART:20261101T150000Z\r\n\
DURATION:PT30M\r\n\
RRULE:FREQ=DAILY;INTERVAL=2;COUNT=3\r\n\
END:VEVENT\r\n\
BEGIN:VEVENT\r\n\
UID:weekly\r\n\
SUMMARY:MWF sync\r\n\
DTSTART:20261102T160000Z\r\n\
DURATION:PT1H\r\n\
RRULE:FREQ=WEEKLY;BYDAY=MO,WE,FR;UNTIL=20261106T160000Z\r\n\
END:VEVENT\r\n\
BEGIN:VEVENT\r\n\
UID:monthly-ord\r\n\
SUMMARY:Second Tuesday\r\n\
DTSTART:20261101T170000Z\r\n\
DURATION:PT1H\r\n\
RRULE:FREQ=MONTHLY;BYDAY=2TU;COUNT=2\r\n\
END:VEVENT\r\n\
BEGIN:VEVENT\r\n\
UID:monthly-last\r\n\
SUMMARY:Last weekday\r\n\
DTSTART:20261101T180000Z\r\n\
DURATION:PT1H\r\n\
RRULE:FREQ=MONTHLY;BYDAY=MO,TU,WE,TH,FR;BYSETPOS=-1;COUNT=1\r\n\
END:VEVENT\r\n\
BEGIN:VEVENT\r\n\
UID:yearly-thanksgiving\r\n\
SUMMARY:Thanksgiving\r\n\
DTSTART;VALUE=DATE:20261126\r\n\
RRULE:FREQ=YEARLY;BYMONTH=11;BYDAY=4TH;COUNT=2\r\n\
END:VEVENT\r\n\
END:VCALENDAR\r\n";

        let feed = parse_and_expand_ics(
            ics,
            &sub_id(),
            ts("2026-11-01T00:00:00Z"),
            ts("2027-12-31T23:59:59Z"),
            &TimeZone::UTC,
        )
        .unwrap();

        let by_uid = |uid: &str| -> Vec<i64> {
            feed.events
                .iter()
                .filter(|e| e.ical_uid.as_deref() == Some(uid))
                .map(|e| e.start)
                .collect()
        };

        assert_eq!(
            by_uid("daily"),
            vec![
                ts("2026-11-01T15:00:00Z"),
                ts("2026-11-03T15:00:00Z"),
                ts("2026-11-05T15:00:00Z"),
            ]
        );
        assert_eq!(
            by_uid("weekly"),
            vec![
                ts("2026-11-02T16:00:00Z"),
                ts("2026-11-04T16:00:00Z"),
                ts("2026-11-06T16:00:00Z"),
            ]
        );
        assert_eq!(
            by_uid("monthly-ord"),
            vec![
                ts("2026-11-10T17:00:00Z"),
                ts("2026-12-08T17:00:00Z"),
            ]
        );
        assert_eq!(by_uid("monthly-last"), vec![ts("2026-11-30T18:00:00Z")]);
        assert_eq!(
            by_uid("yearly-thanksgiving"),
            vec![
                ts("2026-11-26T00:00:00Z"),
                ts("2027-11-25T00:00:00Z"),
            ]
        );
    }

    #[test]
    fn preserves_wall_clock_time_across_dst_and_applies_exdate_rdate_and_overrides() {
        let ics = "\
BEGIN:VCALENDAR\r\n\
VERSION:2.0\r\n\
BEGIN:VEVENT\r\n\
UID:series-dst\r\n\
SUMMARY:Morning Standup\r\n\
DTSTART;TZID=America/Denver:20260307T090000\r\n\
DURATION:PT30M\r\n\
RRULE:FREQ=DAILY;COUNT=4\r\n\
EXDATE;TZID=America/Denver:20260309T090000\r\n\
RDATE;TZID=America/Denver:20260312T100000\r\n\
END:VEVENT\r\n\
BEGIN:VEVENT\r\n\
UID:series-dst\r\n\
SUMMARY:Morning Standup (Moved)\r\n\
RECURRENCE-ID;TZID=America/Denver:20260308T090000\r\n\
DTSTART;TZID=America/Denver:20260308T110000\r\n\
DURATION:PT45M\r\n\
END:VEVENT\r\n\
BEGIN:VEVENT\r\n\
UID:series-dst\r\n\
RECURRENCE-ID;TZID=America/Denver:20260310T090000\r\n\
STATUS:CANCELLED\r\n\
END:VEVENT\r\n\
END:VCALENDAR\r\n";

        let feed = parse_and_expand_ics(
            ics,
            &sub_id(),
            ts("2026-03-01T00:00:00Z"),
            ts("2026-03-20T00:00:00Z"),
            &TimeZone::get("America/Denver").unwrap(),
        )
        .unwrap();

        // Day 1 (Mar 7, MST -07:00): 09:00
        // Day 2 (Mar 8, MDT -06:00): overridden & moved to 11:00 MDT (45m)
        // Day 3 (Mar 9): excluded by EXDATE
        // Day 4 (Mar 10): suppressed by STATUS:CANCELLED override
        // RDATE (Mar 12, MDT -06:00): 10:00 MDT
        assert_eq!(feed.events.len(), 3);
        assert_eq!(feed.events[0].start, ts("2026-03-07T09:00:00-07:00"));
        assert_eq!(feed.events[1].start, ts("2026-03-08T11:00:00-06:00"));
        assert_eq!(feed.events[1].summary, "Morning Standup (Moved)");
        assert_eq!(feed.events[1].duration_minutes(), 45);
        assert_eq!(feed.events[2].start, ts("2026-03-12T10:00:00-06:00"));
    }

    #[test]
    fn supports_windows_tzids_skips_malformed_vevents_and_rejects_html() {
        assert_eq!(
            parse_and_expand_ics("<html><body>Login</body></html>", &sub_id(), 0, 1000, &TimeZone::UTC),
            Err(SubscriptionFetchError::NotICalendar)
        );

        let ics = "\
BEGIN:VCALENDAR\r\n\
VERSION:2.0\r\n\
BEGIN:VEVENT\r\n\
UID:broken\r\n\
DTSTART:not-a-date\r\n\
END:VEVENT\r\n\
BEGIN:VEVENT\r\n\
UID:good\r\n\
SUMMARY:Valid Event\r\n\
DTSTART;TZID=\"Mountain Standard Time\":20261115T090000\r\n\
DURATION:PT1H\r\n\
END:VEVENT\r\n\
END:VCALENDAR\r\n";

        let feed = parse_and_expand_ics(
            ics,
            &sub_id(),
            ts("2026-11-01T00:00:00Z"),
            ts("2026-12-01T00:00:00Z"),
            &TimeZone::UTC,
        )
        .unwrap();
        assert_eq!(feed.events.len(), 1);
        assert_eq!(feed.events[0].summary, "Valid Event");
        assert_eq!(feed.events[0].start, ts("2026-11-15T09:00:00-07:00"));
    }

    fn serve_once(responses: Vec<String>) -> (String, thread::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            let mut requests = Vec::new();
            for resp in responses {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buf = [0u8; 4096];
                let n = stream.read(&mut buf).unwrap();
                requests.push(String::from_utf8_lossy(&buf[..n]).into_owned());
                let _ = stream.write_all(resp.as_bytes());
            }
            requests
        });
        (format!("http://{addr}/feed.ics"), handle)
    }

    #[test]
    fn http_fetcher_handles_200_304_redirects_errors_and_limits() {
        let ics = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nEND:VCALENDAR\r\n";
        let ok_resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nETag: \"v1\"\r\nLast-Modified: Mon, 01 Nov 2026 00:00:00 GMT\r\nConnection: close\r\n\r\n{}",
            ics.len(),
            ics
        );
        let not_mod = "HTTP/1.1 304 Not Modified\r\nConnection: close\r\n\r\n".to_owned();
        let (url, handle) = serve_once(vec![ok_resp, not_mod]);
        match fetch_ics_with_policy(&url, None, None, false).unwrap() {
            FetchOutcome::Updated { body, etag, last_modified } => {
                assert_eq!(body, ics);
                assert_eq!(etag.as_deref(), Some("\"v1\""));
                assert_eq!(last_modified.as_deref(), Some("Mon, 01 Nov 2026 00:00:00 GMT"));
            }
            FetchOutcome::NotModified => panic!("expected updated"),
        }
        assert_eq!(
            fetch_ics_with_policy(&url, Some("\"v1\""), None, false).unwrap(),
            FetchOutcome::NotModified
        );
        let reqs = handle.join().unwrap();
        assert!(reqs[1].to_ascii_lowercase().contains("if-none-match: \"v1\""));

        // Redirect + HTML rejection + 404 + 401
        let redirect = "HTTP/1.1 302 Found\r\nLocation: /moved.ics\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned();
        let ok_moved = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", ics.len(), ics);
        let html_resp = "HTTP/1.1 200 OK\r\nContent-Length: 13\r\nConnection: close\r\n\r\n<html></html>".to_owned();
        let not_found = "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned();
        let unauth = "HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned();
        let (url, handle) = serve_once(vec![redirect, ok_moved, html_resp, not_found, unauth]);
        assert!(matches!(fetch_ics_with_policy(&url, None, None, false), Ok(FetchOutcome::Updated { .. })));
        assert_eq!(fetch_ics_with_policy(&url, None, None, false), Err(SubscriptionFetchError::NotICalendar));
        assert_eq!(fetch_ics_with_policy(&url, None, None, false), Err(SubscriptionFetchError::NotFound));
        assert_eq!(fetch_ics_with_policy(&url, None, None, false), Err(SubscriptionFetchError::Unauthorized));
        handle.join().unwrap();
    }

    #[test]
    fn expands_negative_bymonthday_and_enforces_size_and_redirect_limits() {
        let ics_monthday = "\
BEGIN:VCALENDAR\r\n\
VERSION:2.0\r\n\
BEGIN:VEVENT\r\n\
UID:last-dom\r\n\
SUMMARY:Month End\r\n\
DTSTART;VALUE=DATE:20260131\r\n\
RRULE:FREQ=MONTHLY;BYMONTHDAY=-1;COUNT=2\r\n\
END:VEVENT\r\n\
END:VCALENDAR\r\n";
        let md_feed = parse_and_expand_ics(
            ics_monthday,
            &sub_id(),
            ts("2026-01-01T00:00:00Z"),
            ts("2026-04-01T00:00:00Z"),
            &TimeZone::UTC,
        )
        .unwrap();
        assert_eq!(
            md_feed.events.iter().map(|e| e.start).collect::<Vec<_>>(),
            vec![ts("2026-01-31T00:00:00Z"), ts("2026-02-28T00:00:00Z")]
        );

        // Redirect loop
        let loop_resp = "HTTP/1.1 302 Found\r\nLocation: /loop\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned();
        let (url, handle) = serve_once(vec![loop_resp.clone(), loop_resp]);
        assert_eq!(
            fetch_ics_with_policy(&url, None, None, false),
            Err(SubscriptionFetchError::RedirectLimit)
        );
        handle.join().unwrap();

        // > 8 MiB payload rejected
        let big_body = "BEGIN:VCALENDAR\r\n".to_owned() + &"A".repeat(8 * 1024 * 1024 + 32);
        let big_resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            big_body.len(),
            big_body
        );
        let (url, handle) = serve_once(vec![big_resp]);
        assert_eq!(
            fetch_ics_with_policy(&url, None, None, false),
            Err(SubscriptionFetchError::TooLarge)
        );
        handle.join().unwrap();
    }
}
