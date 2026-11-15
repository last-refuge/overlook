use super::{CalDavError, CalendarPermissions, MAX_RESPONSE_BYTES};
use roxmltree::{Document, Node, ParsingOptions};

const DAV: &str = "DAV:";
const CALDAV: &str = "urn:ietf:params:xml:ns:caldav";
const CARDDAV: &str = "urn:ietf:params:xml:ns:carddav";
const APPLE: &str = "http://apple.com/ns/ical/";

pub(super) const DISCOVERY_PROPERTIES: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<d:propfind xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav" xmlns:a="http://apple.com/ns/ical/">
<d:prop><d:current-user-principal/><c:calendar-home-set/><d:resourcetype/>
<d:displayname/><a:calendar-color/><d:current-user-privilege-set/>
<d:supported-report-set/><d:sync-token/><c:supported-calendar-component-set/></d:prop></d:propfind>"#;

/// The same discovery for address books (RFC 6352): where the principal keeps them, and
/// which collections are address books.
pub(super) const CARD_DISCOVERY_PROPERTIES: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<d:propfind xmlns:d="DAV:" xmlns:card="urn:ietf:params:xml:ns:carddav">
<d:prop><d:current-user-principal/><card:addressbook-home-set/><d:resourcetype/>
<d:displayname/><d:current-user-privilege-set/></d:prop></d:propfind>"#;

#[derive(Debug, Default)]
pub(super) struct DavResponse {
    pub href: String,
    pub principal: Option<String>,
    pub homes: Vec<String>,
    /// Where the principal keeps its address books (CardDAV).
    pub card_homes: Vec<String>,
    pub is_calendar: bool,
    pub is_address_book: bool,
    pub resource_type_known: bool,
    pub supports_events: bool,
    pub name: Option<String>,
    pub color: Option<String>,
    pub permissions: CalendarPermissions,
    pub supports_sync: bool,
    pub sync_token: Option<String>,
}

pub(super) fn child<'a, 'input>(node: Node<'a, 'input>, ns: &str, name: &str) -> Option<Node<'a, 'input>> {
    node.children().find(|n| n.has_tag_name((ns, name)))
}

pub(super) fn text(node: Node<'_, '_>) -> String {
    node.children()
        .filter(|n| n.is_text())
        .filter_map(|n| n.text())
        .collect::<String>()
        .trim()
        .to_owned()
}

pub(super) fn status(node: Node<'_, '_>) -> Result<u16, CalDavError> {
    let raw = text(node);
    let mut parts = raw.split_whitespace();
    let version = parts.next().unwrap_or_default();
    if !matches!(version, "HTTP/1.0" | "HTTP/1.1" | "HTTP/2" | "HTTP/2.0") {
        return Err(CalDavError::InvalidResponse(
            "The calendar service returned an invalid DAV status.",
        ));
    }
    parts
        .next()
        .and_then(|s| s.parse::<u16>().ok())
        .filter(|s| (100..600).contains(s))
        .ok_or(CalDavError::InvalidResponse(
            "The calendar service returned an invalid DAV status.",
        ))
}

pub(super) fn parse_multistatus(body: &str) -> Result<Vec<DavResponse>, CalDavError> {
    if body.len() as u64 > MAX_RESPONSE_BYTES {
        return Err(CalDavError::InvalidResponse(
            "The calendar response exceeds its size limit.",
        ));
    }
    let document = Document::parse_with_options(
        body,
        ParsingOptions {
            allow_dtd: false,
            nodes_limit: 50_000,
            ..Default::default()
        },
    )
    .map_err(|_| {
        CalDavError::InvalidResponse("The calendar service returned invalid or unsupported XML.")
    })?;
    if document
        .descendants()
        .any(|n| n.ancestors().take(65).count() > 64)
    {
        return Err(CalDavError::InvalidResponse(
            "The calendar response is nested too deeply.",
        ));
    }
    let root = document.root_element();
    if !root.has_tag_name((DAV, "multistatus")) {
        return Err(CalDavError::InvalidResponse(
            "The calendar service did not return a DAV multistatus response.",
        ));
    }
    let mut results = Vec::new();
    for response in root
        .children()
        .filter(|n| n.has_tag_name((DAV, "response")))
    {
        let href = child(response, DAV, "href")
            .map(text)
            .filter(|s| !s.is_empty())
            .ok_or(CalDavError::InvalidResponse(
                "The calendar service omitted a resource URL.",
            ))?;
        if let Some(code) = child(response, DAV, "status").map(status).transpose()? {
            if code != 200 {
                return Err(CalDavError::InvalidResponse(
                    "The calendar collection listing was incomplete. Try again later.",
                ));
            }
        }
        let mut item = DavResponse {
            href,
            supports_events: true,
            ..Default::default()
        };
        let mut successful_properties = false;
        for propstat in response
            .children()
            .filter(|n| n.has_tag_name((DAV, "propstat")))
        {
            let code = child(propstat, DAV, "status").ok_or(CalDavError::InvalidResponse(
                "The calendar service omitted a property status.",
            ))?;
            match status(code)? {
                200 => {}
                404 => continue, // Optional properties are commonly absent.
                _ => {
                    return Err(CalDavError::InvalidResponse(
                        "The calendar collection listing was incomplete. Try again later.",
                    ))
                }
            }
            let props = child(propstat, DAV, "prop").ok_or(CalDavError::InvalidResponse(
                "The calendar service omitted resource properties.",
            ))?;
            successful_properties = true;
            for property in props.children().filter(Node::is_element) {
                let tag = property.tag_name();
                match (tag.namespace(), tag.name()) {
                    (Some(DAV), "current-user-principal") => {
                        item.principal = child(property, DAV, "href").map(text)
                    }
                    (Some(CALDAV), "calendar-home-set") => item.homes.extend(
                        property
                            .children()
                            .filter(|n| n.has_tag_name((DAV, "href")))
                            .map(text),
                    ),
                    (Some(CARDDAV), "addressbook-home-set") => item.card_homes.extend(
                        property
                            .children()
                            .filter(|n| n.has_tag_name((DAV, "href")))
                            .map(text),
                    ),
                    (Some(DAV), "resourcetype") => {
                        item.is_calendar = child(property, CALDAV, "calendar").is_some();
                        item.is_address_book = child(property, CARDDAV, "addressbook").is_some();
                        item.resource_type_known = true;
                    }
                    (Some(DAV), "displayname") => item.name = Some(text(property)),
                    (Some(DAV), "sync-token") => {
                        let token = text(property);
                        if !token.is_empty()
                            && token.len() <= 4096
                            && !token.chars().any(char::is_control)
                        {
                            item.sync_token = Some(token);
                        }
                    }
                    (Some(APPLE), "calendar-color") => {
                        let color = text(property);
                        if matches!(color.len(), 7 | 9)
                            && color.starts_with('#')
                            && color[1..].bytes().all(|c| c.is_ascii_hexdigit())
                        {
                            item.color = Some(color);
                        }
                    }
                    (Some(CALDAV), "supported-calendar-component-set") => {
                        item.supports_events = property.children().any(|n| {
                            n.has_tag_name((CALDAV, "comp"))
                                && n.attribute("name") == Some("VEVENT")
                        });
                    }
                    (Some(DAV), "current-user-privilege-set") => {
                        for privilege in property
                            .children()
                            .filter(|n| n.has_tag_name((DAV, "privilege")))
                        {
                            let full = child(privilege, DAV, "all").is_some()
                                || child(privilege, DAV, "write").is_some();
                            item.permissions.create |=
                                full || child(privilege, DAV, "bind").is_some();
                            item.permissions.update |=
                                full || child(privilege, DAV, "write-content").is_some();
                            item.permissions.delete |=
                                full || child(privilege, DAV, "unbind").is_some();
                        }
                    }
                    (Some(DAV), "supported-report-set") => {
                        item.supports_sync = property
                            .descendants()
                            .any(|n| n.has_tag_name((DAV, "sync-collection")));
                    }
                    _ => {}
                }
            }
        }
        if !successful_properties {
            return Err(CalDavError::InvalidResponse(
                "The calendar service returned no readable resource properties.",
            ));
        }
        results.push(item);
        if results.len() > 1024 {
            return Err(CalDavError::DiscoveryLimit);
        }
    }
    Ok(results)
}
