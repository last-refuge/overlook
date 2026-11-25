//! Background synchronization and offline recurrence expansion for subscribed
//! iCalendar (`.ics` / `webcal://`) feeds.

use overlook_core::time::now;
use overlook_core::{
    normalize_hex_color, normalize_subscription_url, tz, CalendarSubscription,
    CalendarSubscriptionId, Event,
};
use overlook_provider::ics_subscription::{
    fetch_ics, parse_and_expand_ics, FetchOutcome, SubscriptionFetchError,
};
use overlook_store::Store;

pub const REFRESH_INTERVAL_MS: i64 = 15 * 60 * 1000;

/// Computes the expansion window covering ~6 months back to ~18 months ahead plus
/// 60 days on either side of the currently visible calendar range.
pub fn expansion_window(view_from: i64, view_to: i64, now_ms: i64) -> (i64, i64) {
    let today = tz::day_start(now_ms);
    let base_from = tz::add_days(today, -180);
    let base_to = tz::add_days(today, 545);
    let from = base_from.min(tz::add_days(view_from, -60));
    let to = base_to.max(tz::add_days(view_to, 60));
    (from, to)
}

/// Returns true when the subscription's expanded cache covers `[from, to)` in `tz_key`.
pub fn window_covers(sub: &CalendarSubscription, from: i64, to: i64, tz_key: &str) -> bool {
    sub.expanded_tz.as_deref() == Some(tz_key)
        && sub.expanded_from.is_some_and(|ef| ef <= from)
        && sub.expanded_to.is_some_and(|et| et >= to)
}

pub struct FetchedSubscription {
    pub normalized_url: String,
    pub name: String,
    pub color: Option<String>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub raw_ics: String,
    pub expanded_from: i64,
    pub expanded_to: i64,
    pub expanded_tz: String,
    pub synced_at: i64,
    pub events: Vec<Event>,
}

/// Fetches, parses, and expands a calendar subscription URL for initial creation or URL change.
pub fn fetch_for_modal(
    raw_url: &str,
    custom_name: Option<&str>,
    custom_color: Option<&str>,
    id: &CalendarSubscriptionId,
    view_from: i64,
    view_to: i64,
) -> Result<FetchedSubscription, String> {
    let normalized_url = normalize_subscription_url(raw_url).map_err(str::to_owned)?;
    let (raw_ics, etag, last_modified) = match fetch_ics(&normalized_url, None, None) {
        Ok(FetchOutcome::Updated {
            body,
            etag,
            last_modified,
        }) => (body, etag, last_modified),
        Ok(FetchOutcome::NotModified) => {
            return Err("The calendar URL returned an empty response.".to_owned())
        }
        Err(err) => return Err(err.to_string()),
    };

    let synced_at = now();
    let (expanded_from, expanded_to) = expansion_window(view_from, view_to, synced_at);
    let expanded_tz = tz::cache_key();
    let zone = tz::current_zone();
    let parsed = parse_and_expand_ics(&raw_ics, id, expanded_from, expanded_to, &zone)
        .map_err(|e| e.to_string())?;

    let host_fallback = CalendarSubscription {
        id: id.clone(),
        url: normalized_url.clone(),
        name: String::new(),
        color: None,
        visible: true,
        sort_order: 0,
        etag: None,
        last_modified: None,
        expanded_from: None,
        expanded_to: None,
        expanded_tz: None,
        last_synced_at: None,
        last_error: None,
        created_at: synced_at,
    }
    .host_label();

    let name = custom_name
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(str::to_owned)
        .or(parsed.calendar_name)
        .or(host_fallback)
        .unwrap_or_else(|| "Subscribed calendar".to_owned());

    let color = custom_color.and_then(normalize_hex_color);

    Ok(FetchedSubscription {
        normalized_url,
        name,
        color,
        etag,
        last_modified,
        raw_ics,
        expanded_from,
        expanded_to,
        expanded_tz,
        synced_at,
        events: parsed.events,
    })
}

/// Refreshes one subscription over HTTPS using conditional validators when `!force`.
/// Returns `Ok(true)` if updated/confirmed current, `Ok(false)` if superseded, or
/// `Err(retry_delay_secs)` after persisting a user-visible `last_error`.
pub fn refresh_subscription(
    store: &Store,
    sub: &CalendarSubscription,
    view_from: i64,
    view_to: i64,
    force: bool,
) -> Result<bool, u64> {
    let now_ms = now();
    let (from, to) = expansion_window(view_from, view_to, now_ms);
    let tz_key = tz::cache_key();
    let zone = tz::current_zone();

    let etag = (!force).then_some(sub.etag.as_deref()).flatten();
    let last_modified = (!force).then_some(sub.last_modified.as_deref()).flatten();

    match fetch_ics(&sub.url, etag, last_modified) {
        Ok(FetchOutcome::NotModified) => {
            let current = store
                .apply_subscription_not_modified(&sub.id, &sub.url, now_ms)
                .map_err(|_| 60u64)?;
            if !current {
                return Ok(false);
            }
            if !window_covers(sub, view_from, view_to, &tz_key) {
                if let Ok(Some(fresh)) = store.calendar_subscription(&sub.id) {
                    reexpand_subscription_offline(store, &fresh, view_from, view_to);
                }
            }
            Ok(true)
        }
        Ok(FetchOutcome::Updated {
            body,
            etag,
            last_modified,
        }) => {
            let parsed = match parse_and_expand_ics(&body, &sub.id, from, to, &zone) {
                Ok(parsed) => parsed,
                Err(err) => {
                    let _ = store.set_calendar_subscription_error(
                        &sub.id,
                        &sub.url,
                        &err.to_string(),
                    );
                    return Err(900);
                }
            };
            store
                .apply_subscription_fetch(
                    &sub.id,
                    &sub.url,
                    etag.as_deref(),
                    last_modified.as_deref(),
                    &body,
                    from,
                    to,
                    &tz_key,
                    now_ms,
                    &parsed.events,
                )
                .map_err(|_| 60u64)
        }
        Err(err) => {
            let _ = store.set_calendar_subscription_error(&sub.id, &sub.url, &err.to_string());
            let delay = match err {
                SubscriptionFetchError::RateLimited => 300,
                SubscriptionFetchError::Network
                | SubscriptionFetchError::ServiceUnavailable
                | SubscriptionFetchError::UnexpectedStatus(_) => 60,
                SubscriptionFetchError::InvalidUrl(_)
                | SubscriptionFetchError::NotFound
                | SubscriptionFetchError::Unauthorized
                | SubscriptionFetchError::TooLarge
                | SubscriptionFetchError::NotICalendar
                | SubscriptionFetchError::RedirectLimit => 900,
            };
            Err(delay)
        }
    }
}

/// Re-expands a cached raw `.ics` payload locally for a changed viewport range or time zone.
pub fn reexpand_subscription_offline(
    store: &Store,
    sub: &CalendarSubscription,
    view_from: i64,
    view_to: i64,
) -> bool {
    let Some(raw_ics) = store.calendar_subscription_raw_ics(&sub.id).ok().flatten() else {
        return false;
    };
    let (from, to) = expansion_window(view_from, view_to, now());
    let tz_key = tz::cache_key();
    let zone = tz::current_zone();
    let Ok(parsed) = parse_and_expand_ics(&raw_ics, &sub.id, from, to, &zone) else {
        return false;
    };
    store
        .apply_subscription_reexpansion(&sub.id, sub.last_synced_at, from, to, &tz_key, &parsed.events)
        .unwrap_or(false)
}
