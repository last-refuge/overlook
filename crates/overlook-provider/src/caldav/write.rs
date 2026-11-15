use super::*;
use overlook_core::{CalDavChange, CalDavRemote};

#[derive(Debug, PartialEq, Eq)]
pub enum CalDavWriteResult {
    Applied(Option<CalDavRemote>),
    Conflict(Option<CalDavRemote>),
}

#[derive(Debug)]
pub struct CalDavWriteError {
    pub error: CalDavError,
    /// True once a mutation may have reached the provider. Keep that revision
    /// frozen until reconciliation; a failed preflight alone permits offline edits.
    pub may_have_written: bool,
}

impl From<CalDavError> for CalDavWriteError {
    fn from(error: CalDavError) -> Self {
        Self {
            error,
            may_have_written: false,
        }
    }
}

pub(super) fn strong_etag(etag: &str) -> bool {
    etag.len() >= 2
        && etag.len() <= 1024
        && etag.starts_with('"')
        && etag.ends_with('"')
        && etag[1..etag.len() - 1]
            .bytes()
            .all(|c| c == 0x21 || (0x23..=0x7e).contains(&c) || c >= 0x80)
}
impl CalDavClient {
    fn resource_request(
        &self,
        href: &str,
        method: &str,
        body: &str,
        condition: Option<(&str, &str)>,
    ) -> Result<(u16, Option<String>, String), CalDavError> {
        self.typed_resource_request(href, method, body, condition, "text/calendar", 2 * 1024 * 1024)
    }

    /// One resource: an iCalendar event, or a vCard for CardDAV, up to `limit` bytes.
    pub(super) fn typed_resource_request(
        &self,
        href: &str,
        method: &str,
        body: &str,
        condition: Option<(&str, &str)>,
        media_type: &str,
        limit: u64,
    ) -> Result<(u16, Option<String>, String), CalDavError> {
        let url = self.resolve(&self.endpoint, href)?;
        let auth = format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD
                .encode(format!("{}:{}", self.username, self.password))
        );
        let mut builder = ureq::http::Request::builder()
            .method(method)
            .uri(url.as_str())
            .header("Authorization", auth)
            .header("Content-Type", format!("{media_type}; charset=utf-8"))
            .header("Accept", media_type);
        if let Some((key, value)) = condition {
            builder = builder.header(key, value);
        }
        let request = builder
            .body(body)
            .map_err(|_| CalDavError::InvalidConfiguration("Invalid calendar write request."))?;
        // Mutations never follow redirects. The resolved resource URL is the write identity.
        let mut response = self.agent.run(request).map_err(|_| CalDavError::Network)?;
        let status = response.status().as_u16();
        match status {
            401 => return Err(CalDavError::AuthenticationRequired),
            403 => return Err(CalDavError::Forbidden),
            429 => return Err(CalDavError::RateLimited),
            500..=599 => return Err(CalDavError::ServiceUnavailable),
            _ => {}
        }
        let etag = response
            .headers()
            .get("etag")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let data = if method == "GET" && status == 200 {
            let bytes = response
                .body_mut()
                .with_config()
                .limit(limit)
                .read_to_vec()
                .map_err(|_| CalDavError::Network)?;
            String::from_utf8(bytes)
                .map_err(|_| CalDavError::InvalidResponse("The calendar resource is not UTF-8."))?
        } else {
            String::new()
        };
        Ok((status, etag, data))
    }

    pub fn fetch_resource(&self, href: &str) -> Result<Option<CalDavRemote>, CalDavError> {
        let (status, etag, icalendar) = self.resource_request(href, "GET", "", None)?;
        match status {
            404 | 410 => Ok(None),
            200 => {
                let etag=etag.filter(|s|strong_etag(s)).ok_or(CalDavError::InvalidResponse("The calendar server did not supply a strong ETag. Local changes have been kept."))?;
                Ok(Some(CalDavRemote { etag, icalendar }))
            }
            _ => Err(CalDavError::UnexpectedStatus(status)),
        }
    }

    /// Every retry first reads the exact URL. Conditional requests remain safe across
    /// timeouts and process death; a lost response never becomes a blind new insert.
    pub fn reconcile_change(
        &self,
        change: &CalDavChange,
        current: impl Fn() -> bool,
    ) -> Result<CalDavWriteResult, CalDavWriteError> {
        let guard = || {
            if current() {
                Ok(())
            } else {
                Err(CalDavError::InvalidConfiguration(
                    "Calendar change was cancelled or superseded.",
                ))
            }
        };
        guard()?;
        let remote = self.fetch_resource(&change.href)?;
        match (&change.desired, &remote) {
            (None, None) => return Ok(CalDavWriteResult::Applied(None)),
            (Some(body), Some(r)) if resource_equivalent(body, &r.icalendar) => {
                return Ok(CalDavWriteResult::Applied(remote))
            }
            _ => {}
        }
        if change.base.as_ref().map(|r| &r.etag) != remote.as_ref().map(|r| &r.etag) {
            return Ok(CalDavWriteResult::Conflict(remote));
        }
        let condition = match &change.base {
            Some(base) if strong_etag(&base.etag) => ("If-Match", base.etag.as_str()),
            Some(_) => {
                return Err(CalDavError::InvalidResponse(
                    "A strong ETag is required before updating this event.",
                )
                .into())
            }
            None => ("If-None-Match", "*"),
        };
        guard()?;
        let result = (|| -> Result<CalDavWriteResult, CalDavError> {
            let (status, _, _) = self.resource_request(
                &change.href,
                if change.desired.is_some() {
                    "PUT"
                } else {
                    "DELETE"
                },
                change.desired.as_deref().unwrap_or(""),
                Some(condition),
            )?;
            match status {
                200 | 201 | 204 => {
                    guard()?;
                    Ok(CalDavWriteResult::Applied(if change.desired.is_some() {
                        self.fetch_resource(&change.href)?
                    } else {
                        None
                    }))
                }
                404 | 410 if change.desired.is_none() => Ok(CalDavWriteResult::Applied(None)),
                409 | 412 => {
                    guard()?;
                    Ok(CalDavWriteResult::Conflict(
                        self.fetch_resource(&change.href)?,
                    ))
                }
                _ => Err(CalDavError::UnexpectedStatus(status)),
            }
        })();
        result.map_err(|error| CalDavWriteError {
            error,
            may_have_written: true,
        })
    }
}
