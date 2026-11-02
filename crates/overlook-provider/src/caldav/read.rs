//! Complete bounded snapshots. A failed/partial response never becomes an empty calendar.
use super::*;
use overlook_core::*;
use roxmltree::{Document, Node, ParsingOptions};
use std::collections::HashMap;

const REPORT_LIMIT: u64 = 8 * 1024 * 1024;
const PARTIAL: CalDavError = CalDavError::InvalidResponse(
    "The calendar download was incomplete. Your cached events have been kept; try syncing again.",
);

fn child<'a, 'i>(node: Node<'a, 'i>, ns: &str, name: &str) -> Option<Node<'a, 'i>> {
    node.children().find(|n| n.has_tag_name((ns, name)))
}
fn content(node: Node<'_, '_>) -> String {
    node.children()
        .filter(|n| n.is_text())
        .filter_map(|n| n.text())
        .collect()
}

fn report_resources(body: &str) -> Result<Vec<(String, String, String)>, CalDavError> {
    let doc = Document::parse_with_options(
        body,
        ParsingOptions {
            allow_dtd: false,
            nodes_limit: 100_000,
            ..Default::default()
        },
    )
    .map_err(|_| PARTIAL)?;
    let root = doc.root_element();
    if !root.has_tag_name(("DAV:", "multistatus"))
        || doc
            .descendants()
            .any(|n| n.ancestors().take(65).count() > 64)
    {
        return Err(PARTIAL);
    }
    let mut resources = Vec::new();
    for response in root.children().filter(|n| n.is_element()) {
        if !response.has_tag_name(("DAV:", "response")) {
            return Err(PARTIAL);
        }
        let href = content(child(response, "DAV:", "href").ok_or(PARTIAL)?);
        if child(response, "DAV:", "status").is_some() {
            return Err(PARTIAL);
        }
        let (mut etag, mut data) = (None, None);
        for propstat in response
            .children()
            .filter(|n| n.has_tag_name(("DAV:", "propstat")))
        {
            let status = content(child(propstat, "DAV:", "status").ok_or(PARTIAL)?);
            if status.split_whitespace().nth(1) != Some("200") {
                return Err(PARTIAL);
            }
            let props = child(propstat, "DAV:", "prop").ok_or(PARTIAL)?;
            for p in props.children().filter(|n| n.is_element()) {
                if p.has_tag_name(("DAV:", "getetag")) {
                    if etag.replace(content(p)).is_some() {
                        return Err(PARTIAL);
                    }
                } else if p.has_tag_name(("urn:ietf:params:xml:ns:caldav", "calendar-data"))
                    && data.replace(content(p)).is_some()
                {
                    return Err(PARTIAL);
                }
            }
        }
        let etag = etag
            .filter(|s| !s.is_empty() && s.len() <= 1024)
            .ok_or(PARTIAL)?;
        resources.push((href, etag, data.ok_or(PARTIAL)?));
        if resources.len() > 2048 {
            return Err(CalDavError::DiscoveryLimit);
        }
    }
    Ok(resources)
}

fn query(from: i64, to: i64, expand: bool) -> String {
    let (from, to) = (tz::utc_basic(from), tz::utc_basic(to));
    let expansion = if expand {
        format!(r#"<c:expand start="{from}" end="{to}"/>"#)
    } else {
        String::new()
    };
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?><c:calendar-query xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav"><d:prop><d:getetag/><c:calendar-data>{expansion}</c:calendar-data></d:prop><c:filter><c:comp-filter name="VCALENDAR"><c:comp-filter name="VEVENT"><c:time-range start="{from}" end="{to}"/></c:comp-filter></c:comp-filter></c:filter></c:calendar-query>"#
    )
}

impl CalDavClient {
    /// Complete discovery and two matching resource reports per collection. Keeping the
    /// original and expanded response separate preserves unknown properties and custom
    /// VTIMEZONE definitions. ETag mismatch means the server changed between reports.
    pub fn snapshot(
        &self,
        account: &Account,
        connection: &CalendarConnectionId,
        from: i64,
        to: i64,
        still_current: impl Fn() -> bool,
    ) -> Result<CalDavSnapshot, CalDavError> {
        self.synchronize_collections(account, connection, from, to, &[], still_current)
            .map(|download| download.snapshot)
    }

    /// RFC 6578 checkpoints avoid downloading bodies for unchanged collections.
    /// A changed collection is re-queried in full for the bounded display window;
    /// this also handles moved instances and server-side recurrence expansion.
    pub fn synchronize_collections(
        &self,
        account: &Account,
        connection: &CalendarConnectionId,
        from: i64,
        to: i64,
        cached: &[CalDavCheckpoint],
        still_current: impl Fn() -> bool,
    ) -> Result<CalDavDownload, CalDavError> {
        if from >= to || to.saturating_sub(from) > 500 * 86_400_000 {
            return Err(CalDavError::InvalidConfiguration(
                "Choose a calendar range shorter than 500 days.",
            ));
        }
        let (zone, timezone) = tz::zone_with_cache_key();
        let mut snapshot = CalDavSnapshot {
            from,
            to,
            timezone,
            calendars: Vec::new(),
        };
        let mut checkpoints = Vec::new();
        let mut reused = Vec::new();
        let discovery = self.discover()?;
        let started = Instant::now();
        let mut total_bytes = 0usize;
        // Padding ensures floating and date-only events at zone boundaries are included.
        let report_from = from.saturating_sub(2 * 86_400_000);
        let report_to = to.saturating_add(2 * 86_400_000);
        for (order, found) in discovery.calendars.into_iter().enumerate() {
            if !still_current() || started.elapsed() > Duration::from_secs(180) {
                return Err(CalDavError::DiscoveryLimit);
            }
            let url = self.resolve(&self.endpoint, &found.url)?;
            let id = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(url.as_str());
            let calendar = Calendar {
                id: CalendarId(format!("caldav:{connection}:{id}")),
                account_id: account.id.clone(),
                provider_id: Some(ProviderId(url.to_string())),
                name: found.name,
                color: found.color,
                is_primary: order == 0,
                can_edit: found.permissions.create
                    && found.permissions.update
                    && found.permissions.delete,
                visible: true,
                sort_order: order as i32,
                default_reminders: Vec::new(),
            };
            // Discovery precedes all event reads. Saving this earlier token can
            // cause a harmless extra refresh, but cannot skip a concurrent edit.
            let mut token = found.sync_token;
            let previous = cached.iter().find(|c| {
                c.calendar_id == calendar.id
                    && c.from == from
                    && c.to == to
                    && c.timezone == snapshot.timezone
            });
            if found.supports_collection_sync {
                if let Some(previous) = previous {
                    if let Some(next) = self.unchanged_collection(&url, &previous.token)? {
                        checkpoints.push(CalDavCheckpoint {
                            calendar_id: calendar.id.clone(),
                            token: next,
                            from,
                            to,
                            timezone: snapshot.timezone.clone(),
                        });
                        reused.push(previous.clone());
                        snapshot.calendars.push(CalDavCalendarSnapshot {
                            calendar,
                            resources: Vec::new(),
                        });
                        continue;
                    }
                    // Changed, expired or unsupported: refresh the collection.
                }
            } else {
                token = None;
            }
            let mut reports = Vec::new();
            for expand in [false, true] {
                if !still_current() {
                    return Err(CalDavError::DiscoveryLimit);
                }
                let (base, body) = self.request_xml(
                    &url,
                    "REPORT",
                    "1",
                    &query(report_from, report_to, expand),
                    REPORT_LIMIT,
                    Instant::now(),
                )?;
                total_bytes += body.len();
                if total_bytes > 32 * 1024 * 1024 {
                    return Err(CalDavError::DiscoveryLimit);
                }
                let mut resources = HashMap::new();
                for (href, etag, data) in report_resources(&body)? {
                    let href = self.resolve(&base, href.trim())?;
                    if !href
                        .path()
                        .starts_with(&format!("{}/", base.path().trim_end_matches('/')))
                        || href == base
                    {
                        return Err(PARTIAL);
                    }
                    if resources.insert(href.to_string(), (etag, data)).is_some() {
                        return Err(PARTIAL);
                    }
                }
                reports.push(resources);
            }
            let expanded = reports.pop().ok_or(PARTIAL)?;
            let original = reports.pop().ok_or(PARTIAL)?;
            if original.len() != expanded.len() {
                return Err(PARTIAL);
            }
            let mut resources = Vec::new();
            let mut seen = HashSet::new();
            for (href, (etag, icalendar)) in original {
                let (expanded_etag, data) = expanded.get(&href).ok_or(PARTIAL)?;
                if &etag != expanded_etag {
                    return Err(PARTIAL);
                }
                let events = codec::events(data, account, &calendar, &zone)?
                    .into_iter()
                    .filter(|e| e.start < to && e.end.max(e.start + 1) > from)
                    .collect::<Vec<_>>();
                for event in &events {
                    if !seen.insert(event.id.clone()) || seen.len() > 20_000 {
                        return Err(PARTIAL);
                    }
                }
                resources.push(CalDavResource {
                    href,
                    etag,
                    icalendar,
                    events,
                });
            }
            if let Some(token) = token {
                checkpoints.push(CalDavCheckpoint {
                    calendar_id: calendar.id.clone(),
                    token,
                    from,
                    to,
                    timezone: snapshot.timezone.clone(),
                });
            }
            snapshot.calendars.push(CalDavCalendarSnapshot {
                calendar,
                resources,
            });
        }
        Ok(CalDavDownload {
            snapshot,
            checkpoints,
            reused,
        })
    }
}
