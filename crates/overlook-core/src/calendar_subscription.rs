//! Domain model and URL/color validation for read-only subscribed iCalendar
//! (`.ics` / `webcal://`) feeds, including Apple iCloud public calendar links.

use crate::calendar::Calendar;
use crate::ids::{AccountId, CalendarId, CalendarSubscriptionId, EventId};
use crate::time::Timestamp;
use serde::{Deserialize, Serialize};
use url::Url;

pub const SUBSCRIPTION_ACCOUNT_ID: &str = "subscription";
pub const SUBSCRIPTION_CALENDAR_PREFIX: &str = "sub:";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalendarSubscription {
    pub id: CalendarSubscriptionId,
    pub url: String,
    pub name: String,
    /// Canonical `#RRGGBB` hex color chosen by the user, or `None` for auto-assignment.
    pub color: Option<String>,
    pub visible: bool,
    pub sort_order: i32,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub expanded_from: Option<Timestamp>,
    pub expanded_to: Option<Timestamp>,
    pub expanded_tz: Option<String>,
    pub last_synced_at: Option<Timestamp>,
    pub last_error: Option<String>,
    pub created_at: Timestamp,
}

impl CalendarSubscription {
    pub fn account_id() -> AccountId {
        AccountId::from(SUBSCRIPTION_ACCOUNT_ID)
    }

    pub fn calendar_id(&self) -> CalendarId {
        CalendarId(format!("{SUBSCRIPTION_CALENDAR_PREFIX}{}", self.id))
    }

    pub fn is_subscription_calendar_id(id: &CalendarId) -> bool {
        id.0.starts_with(SUBSCRIPTION_CALENDAR_PREFIX)
    }

    pub fn is_subscription_event_id(id: &EventId) -> bool {
        id.0.starts_with(SUBSCRIPTION_CALENDAR_PREFIX)
    }

    pub fn subscription_id_from_calendar(id: &CalendarId) -> Option<CalendarSubscriptionId> {
        id.0.strip_prefix(SUBSCRIPTION_CALENDAR_PREFIX)
            .filter(|raw| !raw.is_empty())
            .map(CalendarSubscriptionId::from)
    }

    pub fn to_calendar(&self) -> Calendar {
        Calendar {
            id: self.calendar_id(),
            account_id: Self::account_id(),
            provider_id: None,
            name: self.name.clone(),
            color: self.color.clone(),
            is_primary: false,
            can_edit: false,
            visible: self.visible,
            sort_order: self.sort_order,
            default_reminders: Vec::new(),
        }
    }

    /// Clean host label for display (never exposes paths or query tokens).
    pub fn host_label(&self) -> Option<String> {
        let parsed = Url::parse(&self.url).ok()?;
        let host = parsed.host_str()?.trim();
        if host.is_empty() {
            return None;
        }
        let lower = host.to_ascii_lowercase();
        if lower == "icloud.com" || lower.ends_with(".icloud.com") {
            Some("icloud.com".to_owned())
        } else {
            Some(lower)
        }
    }
}

fn strip_prefix_ignore_ascii_case<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    s.get(..prefix.len())
        .filter(|head| head.eq_ignore_ascii_case(prefix))
        .map(|_| &s[prefix.len()..])
}

/// Validates and normalizes an iCalendar subscription URL (`webcal://`, `webcals://`, or `https://`).
/// Query parameters are preserved (often used for published feed tokens), while embedded credentials,
/// fragments, control characters, and non-HTTPS schemes are rejected without echoing untrusted input.
pub fn normalize_subscription_url(raw: &str) -> Result<String, &'static str> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.len() > 4096 || trimmed.chars().any(char::is_control) {
        return Err("Enter a valid calendar URL.");
    }
    let rewritten = if let Some(rest) = strip_prefix_ignore_ascii_case(trimmed, "webcals://") {
        format!("https://{rest}")
    } else if let Some(rest) = strip_prefix_ignore_ascii_case(trimmed, "webcal://") {
        format!("https://{rest}")
    } else {
        trimmed.to_owned()
    };
    let url = Url::parse(&rewritten).map_err(|_| "Enter a valid calendar URL.")?;
    if url.scheme() != "https" {
        return Err("The calendar URL must use HTTPS or webcal://.");
    }
    if url.host_str().is_none_or(str::is_empty) {
        return Err("Enter a valid calendar URL.");
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("Use a calendar URL without embedded usernames or passwords.");
    }
    if url.fragment().is_some() {
        return Err("Use a calendar URL without a fragment.");
    }
    let normalized: String = url.into();
    if normalized.len() > 4096 {
        return Err("Enter a valid calendar URL.");
    }
    Ok(normalized)
}

/// Normalizes a 6-digit `#RRGGBB` or 8-digit `#RRGGBBAA` (Apple calendar) hex color to `#RRGGBB`.
pub fn normalize_hex_color(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let hex = trimmed.strip_prefix('#').unwrap_or(trimmed);
    if !(hex.len() == 6 || hex.len() == 8) || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Some(format!("#{}", hex[..6].to_ascii_uppercase()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_webcal_and_https_urls_and_keeps_query_params() {
        assert_eq!(
            normalize_subscription_url("  webcal://p123-caldav.icloud.com/published/2/abcDEF  ").unwrap(),
            "https://p123-caldav.icloud.com/published/2/abcDEF"
        );
        assert_eq!(
            normalize_subscription_url("WEBCALS://calendars.icloud.com/holidays/us.ics?lang=en").unwrap(),
            "https://calendars.icloud.com/holidays/us.ics?lang=en"
        );
        assert_eq!(
            normalize_subscription_url("https://calendar.example.com/feed.ics?token=xyz").unwrap(),
            "https://calendar.example.com/feed.ics?token=xyz"
        );
    }

    #[test]
    fn rejects_unsafe_urls_without_echoing_secrets() {
        for bad in [
            "",
            "   ",
            "http://example.com/secret.ics",
            "file:///tmp/secret.ics",
            "https://user:secret@example.com/cal.ics",
            "https://example.com/cal.ics#secret",
            "https://example.com/cal.ics\nsecret",
            "https://?secret=1",
            "webcal://?secret=1",
            "not a url secret",
        ] {
            let err = normalize_subscription_url(bad).unwrap_err();
            assert!(!err.contains("secret"), "error echoed input for {bad:?}: {err}");
        }
    }

    #[test]
    fn normalizes_hex_colors_and_apple_rgba() {
        assert_eq!(normalize_hex_color("#1badf8").as_deref(), Some("#1BADF8"));
        assert_eq!(normalize_hex_color("1badf8ff").as_deref(), Some("#1BADF8"));
        assert_eq!(normalize_hex_color(" #1BADF880 ").as_deref(), Some("#1BADF8"));
        assert_eq!(normalize_hex_color("#123"), None);
        assert_eq!(normalize_hex_color("#zzzzzz"), None);
        assert_eq!(normalize_hex_color(""), None);
    }

    #[test]
    fn derives_clean_host_label_and_calendar_projection() {
        let sub = CalendarSubscription {
            id: CalendarSubscriptionId::from("sub-1"),
            url: "https://p42-caldav.icloud.com/published/2/secret-token?foo=bar".into(),
            name: "Family".into(),
            color: Some("#3B82F6".into()),
            visible: true,
            sort_order: 0,
            etag: None,
            last_modified: None,
            expanded_from: None,
            expanded_to: None,
            expanded_tz: None,
            last_synced_at: None,
            last_error: None,
            created_at: 100,
        };
        assert_eq!(sub.host_label().as_deref(), Some("icloud.com"));
        let cal = sub.to_calendar();
        assert_eq!(cal.id, CalendarId::from("sub:sub-1"));
        assert_eq!(cal.account_id, AccountId::from("subscription"));
        assert!(!cal.can_edit);
        assert!(CalendarSubscription::is_subscription_calendar_id(&cal.id));
        assert_eq!(
            CalendarSubscription::subscription_id_from_calendar(&cal.id),
            Some(CalendarSubscriptionId::from("sub-1"))
        );
    }
}
