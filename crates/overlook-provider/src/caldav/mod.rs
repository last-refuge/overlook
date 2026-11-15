//! CalDAV discovery, snapshots and conditional writes, and CardDAV contacts over the same
//! transport (`carddav`). Run network work off the UI thread.
//!
//! The read service is independent of the mutation-oriented CalendarProvider trait.
//! Personal-event writes use a separate durable outbox; meeting scheduling is not enabled.

mod codec;
pub mod carddav;
mod collection_sync;
mod edit;
#[cfg(feature = "fake-servers")]
pub mod fake;
mod read;
pub mod vcard;
mod write;
mod xml;
pub use edit::{
    prepare_conflict_resource, prepare_resource, resource_equivalent, resource_preview,
};
pub use write::{CalDavWriteError, CalDavWriteResult};

use base64::Engine;
use overlook_core::{CalDavSettings, CalendarCredentialSource, MailServers};
use std::collections::HashSet;
use std::fmt;
use std::time::{Duration, Instant};
use url::Url;

pub const FASTMAIL_CALDAV_URL: &str = "https://caldav.fastmail.com/";
const MAX_RESPONSE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_CALENDARS: usize = 256;
const MAX_HOMES: usize = 8;
const MAX_REDIRECTS: usize = 5;
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(45);

/// A suggestion only: the caller must ask the user before reusing a mail credential.
/// Inspect the actual incoming host so custom email domains work without guessing MX.
pub fn fastmail_preset(servers: &MailServers) -> Option<CalDavSettings> {
    fastmail_preset_for_login(&servers.incoming.host, &servers.incoming.username)
}

/// The setup form can offer calendars as soon as the incoming server is known.
/// An email domain alone does not authorize sending a password to Fastmail.
pub fn fastmail_preset_for_login(host: &str, username: &str) -> Option<CalDavSettings> {
    if !host
        .trim_end_matches('.')
        .eq_ignore_ascii_case("imap.fastmail.com")
    {
        return None;
    }
    Some(CalDavSettings {
        server_url: FASTMAIL_CALDAV_URL.into(),
        username: username.to_owned(),
        credential_source: CalendarCredentialSource::Separate,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CalDavError {
    #[error("{0}")]
    InvalidConfiguration(&'static str),
    #[error("Calendar sign-in failed. Check the app password and its calendar permission.")]
    AuthenticationRequired,
    #[error("This sign-in does not have permission to access the calendar service.")]
    Forbidden,
    #[error("The calendar service is limiting requests. Try again later.")]
    RateLimited,
    #[error("The calendar service is temporarily unavailable.")]
    ServiceUnavailable,
    #[error("Could not reach the calendar service over a verified connection.")]
    Network,
    #[error("The calendar service returned HTTP {0}.")]
    UnexpectedStatus(u16),
    #[error("{0}")]
    InvalidResponse(&'static str),
    /// Contains only the origin, never a path, query, username or response body.
    #[error("Calendar discovery needs a different server ({0}). Configure that server explicitly before signing in.")]
    UnapprovedOrigin(String),
    #[error("Calendar discovery exceeded its redirect limit.")]
    RedirectLimit,
    #[error("Calendar discovery exceeded its time or collection limit.")]
    DiscoveryLimit,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CalendarPermissions {
    pub create: bool,
    pub update: bool,
    pub delete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredCalendar {
    pub url: String,
    pub name: String,
    pub color: Option<String>,
    pub permissions: CalendarPermissions,
    pub supports_collection_sync: bool,
    pub sync_token: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Discovery {
    pub principal_url: Option<String>,
    pub home_urls: Vec<String>,
    pub calendars: Vec<DiscoveredCalendar>,
}

pub struct CalDavClient {
    endpoint: Url,
    username: String,
    password: String,
    agent: ureq::Agent,
}

impl fmt::Debug for CalDavClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CalDavClient")
            .field("origin", &self.endpoint.origin().ascii_serialization())
            .finish_non_exhaustive()
    }
}

impl CalDavClient {
    pub fn new(settings: &CalDavSettings, password: String) -> Result<Self, CalDavError> {
        let endpoint = settings
            .validate()
            .map_err(CalDavError::InvalidConfiguration)?;
        if password.is_empty() {
            return Err(CalDavError::InvalidConfiguration(
                "Enter the calendar app password.",
            ));
        }
        Ok(Self {
            endpoint,
            username: settings.username.clone(),
            password,
            agent: agent(true),
        })
    }

    /// Resolves a principal, all advertised calendar homes, and VEVENT collections.
    /// Redirects and discovered URLs can never expand the credential's approved origin.
    pub fn discover(&self) -> Result<Discovery, CalDavError> {
        let started = Instant::now();
        let (root_url, roots) = match self.propfind(&self.endpoint, "0", started) {
            // A server origin is a valid setup hint, but need not itself be a DAV
            // resource (Fastmail returns 404 here). Use its RFC 6764 service hint
            // only for a bare origin, never to replace an explicit collection URL
            // or hide a later principal/home failure. Keep the original deadline
            // and credential origin through the ordinary guarded redirect path.
            Err(CalDavError::UnexpectedStatus(404 | 405)) if self.endpoint.path() == "/" => {
                let service = self.resolve(&self.endpoint, "/.well-known/caldav")?;
                self.propfind(&service, "0", started)?
            }
            result => result?,
        };
        let root = exact_response(&roots, &root_url, self)?;
        if root.is_calendar {
            let calendars = self.calendar(root, &root_url)?.into_iter().collect();
            return Ok(Discovery {
                principal_url: None,
                home_urls: Vec::new(),
                calendars,
            });
        }

        let principal = root
            .principal
            .as_deref()
            .map(|p| self.resolve(&root_url, p))
            .transpose()?;
        let mut homes: Vec<Url> = root
            .homes
            .iter()
            .map(|h| self.resolve(&root_url, h))
            .collect::<Result<_, _>>()?;
        if homes.is_empty() {
            if let Some(principal) = &principal {
                let (principal_url, responses) = self.propfind(principal, "0", started)?;
                let response = exact_response(&responses, &principal_url, self)?;
                homes = response
                    .homes
                    .iter()
                    .map(|h| self.resolve(&principal_url, h))
                    .collect::<Result<_, _>>()?;
            }
        }
        if homes.is_empty() && principal.is_some() {
            return Err(CalDavError::InvalidResponse(
                "The calendar service did not advertise a calendar home.",
            ));
        }
        // A manually supplied home URL need not advertise a principal or itself as a home.
        if homes.is_empty() {
            homes.push(root_url);
        }
        let mut seen_homes = HashSet::new();
        homes.retain(|h| seen_homes.insert(h.as_str().to_owned()));
        if homes.len() > MAX_HOMES {
            return Err(CalDavError::DiscoveryLimit);
        }
        let mut calendars = Vec::new();
        let mut seen_calendars = HashSet::new();
        let mut verified_homes = Vec::new();
        for home in homes {
            let (base, responses) = self.propfind(&home, "1", started)?;
            exact_response(&responses, &base, self)?;
            if responses.iter().any(|r| !r.resource_type_known) {
                return Err(CalDavError::InvalidResponse("The calendar collection listing was incomplete. Cached calendars have been kept."));
            }
            verified_homes.push(base.to_string());
            for response in &responses {
                if let Some(calendar) = self.calendar(response, &base)? {
                    if seen_calendars.insert(calendar.url.clone()) {
                        calendars.push(calendar);
                        if calendars.len() > MAX_CALENDARS {
                            return Err(CalDavError::DiscoveryLimit);
                        }
                    }
                }
            }
        }
        Ok(Discovery {
            principal_url: principal.map(|p| p.to_string()),
            home_urls: verified_homes,
            calendars,
        })
    }

    fn calendar(
        &self,
        response: &xml::DavResponse,
        base: &Url,
    ) -> Result<Option<DiscoveredCalendar>, CalDavError> {
        if !response.is_calendar || !response.supports_events {
            return Ok(None);
        }
        let url = self.resolve(base, &response.href)?;
        Ok(Some(DiscoveredCalendar {
            url: url.to_string(),
            name: response
                .name
                .clone()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| "Calendar".into()),
            color: response.color.clone(),
            permissions: response.permissions.clone(),
            supports_collection_sync: response.supports_sync,
            sync_token: response.sync_token.clone(),
        }))
    }

    fn resolve(&self, base: &Url, href: &str) -> Result<Url, CalDavError> {
        if href.len() > 4096 || href.chars().any(char::is_control) {
            return Err(CalDavError::InvalidResponse(
                "The calendar service returned an invalid resource URL.",
            ));
        }
        let url = base.join(href).map_err(|_| {
            CalDavError::InvalidResponse("The calendar service returned an invalid resource URL.")
        })?;
        if !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(CalDavError::InvalidResponse("Calendar resource URLs must not contain credentials, query parameters, or fragments."));
        }
        if url.origin() != self.endpoint.origin() {
            return Err(CalDavError::UnapprovedOrigin(
                url.origin().ascii_serialization(),
            ));
        }
        Ok(url)
    }

    fn propfind(
        &self,
        target: &Url,
        depth: &str,
        started: Instant,
    ) -> Result<(Url, Vec<xml::DavResponse>), CalDavError> {
        self.propfind_with(target, depth, xml::DISCOVERY_PROPERTIES, started)
    }

    /// A PROPFIND asking for `properties`: calendar discovery, or address books for CardDAV.
    fn propfind_with(
        &self,
        target: &Url,
        depth: &str,
        properties: &str,
        started: Instant,
    ) -> Result<(Url, Vec<xml::DavResponse>), CalDavError> {
        let (url, body) = self.request_xml(
            target,
            "PROPFIND",
            depth,
            properties,
            MAX_RESPONSE_BYTES,
            started,
        )?;
        Ok((url, xml::parse_multistatus(&body)?))
    }

    fn request_xml(
        &self,
        target: &Url,
        method: &str,
        depth: &str,
        body: &str,
        limit: u64,
        started: Instant,
    ) -> Result<(Url, String), CalDavError> {
        let mut url = self.resolve(&self.endpoint, target.as_str())?;
        let authorization = format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD
                .encode(format!("{}:{}", self.username, self.password))
        );
        let mut visited = HashSet::new();
        for _ in 0..=MAX_REDIRECTS {
            if started.elapsed() >= DISCOVERY_TIMEOUT {
                return Err(CalDavError::DiscoveryLimit);
            }
            if !visited.insert(url.to_string()) {
                return Err(CalDavError::RedirectLimit);
            }
            let request = ureq::http::Request::builder()
                .method(method)
                .uri(url.as_str())
                .header("Authorization", &authorization)
                .header("Depth", depth)
                .header("Content-Type", "application/xml; charset=utf-8")
                .header("Accept", "application/xml, text/xml")
                .body(body)
                .map_err(|_| {
                    CalDavError::InvalidConfiguration("Could not construct the calendar request.")
                })?;
            let remaining = DISCOVERY_TIMEOUT
                .checked_sub(started.elapsed())
                .ok_or(CalDavError::DiscoveryLimit)?;
            let request = self
                .agent
                .configure_request(request)
                .timeout_global(Some(remaining.min(Duration::from_secs(15))))
                .build();
            let mut response = self.agent.run(request).map_err(|_| CalDavError::Network)?;
            match response.status().as_u16() {
                301 | 302 | 307 | 308 => {
                    let location = response
                        .headers()
                        .get("location")
                        .and_then(|h| h.to_str().ok())
                        .ok_or(CalDavError::InvalidResponse(
                            "The calendar service returned an invalid redirect.",
                        ))?;
                    url = self.resolve(&url, location)?;
                }
                207 => {
                    let bytes = response
                        .body_mut()
                        .with_config()
                        .limit(limit)
                        .read_to_vec()
                        .map_err(|_| CalDavError::InvalidResponse("Could not read the calendar response within its size and time limits."))?;
                    let body = std::str::from_utf8(&bytes).map_err(|_| {
                        CalDavError::InvalidResponse("The calendar response is not UTF-8 XML.")
                    })?;
                    return Ok((url, body.to_owned()));
                }
                401 => return Err(CalDavError::AuthenticationRequired),
                403 => return Err(CalDavError::Forbidden),
                429 => return Err(CalDavError::RateLimited),
                500..=599 => return Err(CalDavError::ServiceUnavailable),
                status => return Err(CalDavError::UnexpectedStatus(status)),
            }
        }
        Err(CalDavError::RedirectLimit)
    }
}

fn agent(https_only: bool) -> ureq::Agent {
    ureq::Agent::config_builder()
        .https_only(https_only)
        .max_redirects(0)
        .http_status_as_error(false)
        .allow_non_standard_methods(true)
        .timeout_global(Some(Duration::from_secs(15)))
        .proxy(None)
        .build()
        .new_agent()
}

fn exact_response<'a>(
    responses: &'a [xml::DavResponse],
    target: &Url,
    client: &CalDavClient,
) -> Result<&'a xml::DavResponse, CalDavError> {
    let mut found = None;
    for response in responses {
        let href = client.resolve(target, &response.href)?;
        if &href == target {
            if found.is_some() {
                return Err(CalDavError::InvalidResponse(
                    "The calendar service returned duplicate resource properties.",
                ));
            }
            found = Some(response);
        }
    }
    found.ok_or(CalDavError::InvalidResponse(
        "The calendar service did not describe the requested resource.",
    ))
}

#[cfg(test)]
mod codec_tests;
#[cfg(test)]
mod tests;
