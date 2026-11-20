//! Spending Gmail's quota deliberately.
//!
//! Google meters the Gmail API in quota units per user per minute. Since May 2026 the limit
//! is 6,000 units, and a message downloaded in full costs 60 (`format=full`), its headers 20
//! (`format=metadata`), a page of change history 2. Requests inside a batch count one by one.
//! See <https://developers.google.com/workspace/gmail/api/reference/quota>.
//!
//! [`QuotaBudget`] is a token bucket in those units: it refills at [`RATE_PER_SECOND`] up to
//! [`BURST`], so no minute can spend more than `BURST + 60 × RATE_PER_SECOND` (5,800), under
//! Google's limit with room to spare. Background work (catching up, downloading bodies) also
//! leaves [`BACKGROUND_RESERVE`] untouched, so what the user does meanwhile (opening a
//! message, sending) doesn't queue behind it. Calendar and People requests have their own
//! quotas at Google and cost nothing here.

use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Units the bucket refills per second: 4,800 a minute.
pub const RATE_PER_SECOND: f64 = 80.;
/// The most the bucket holds, so the most a quiet account can spend at once.
pub const BURST: f64 = 1_000.;
/// What background work leaves for the user's own requests.
pub const BACKGROUND_RESERVE: f64 = 400.;

pub struct QuotaBudget {
    state: Mutex<State>,
}

struct State {
    tokens: f64,
    updated: Instant,
}

impl Default for QuotaBudget {
    fn default() -> Self {
        Self::new()
    }
}

impl QuotaBudget {
    pub fn new() -> Self {
        Self { state: Mutex::new(State { tokens: BURST, updated: Instant::now() }) }
    }

    /// Waits until `cost` units can be spent, then spends them. Work running inside
    /// [`crate::as_background`] also leaves the reserve.
    pub fn spend(&self, cost: u32) {
        self.spend_as(cost, crate::is_background());
    }

    fn spend_as(&self, cost: u32, background: bool) {
        if cost == 0 {
            return;
        }
        loop {
            let wait = {
                let mut s = self.state.lock().unwrap_or_else(|p| p.into_inner());
                let now = Instant::now();
                s.tokens = refilled(s.tokens, now.duration_since(s.updated));
                s.updated = now;
                match wait_for(s.tokens, cost, background) {
                    None => {
                        s.tokens -= f64::from(cost);
                        return;
                    }
                    Some(wait) => wait,
                }
            };
            std::thread::sleep(wait);
        }
    }

    /// Google said the minute's quota is spent: everything waits for the bucket to refill.
    pub fn exhausted(&self) {
        let mut s = self.state.lock().unwrap_or_else(|p| p.into_inner());
        s.tokens = s.tokens.min(0.);
        s.updated = Instant::now();
    }
}

fn refilled(tokens: f64, elapsed: Duration) -> f64 {
    (tokens + elapsed.as_secs_f64() * RATE_PER_SECOND).min(BURST)
}

/// How long to wait before `cost` can be spent, or `None` to spend now. A cost larger than
/// the bucket waits for a full bucket and overdraws it; the next requests wait it off.
fn wait_for(tokens: f64, cost: u32, background: bool) -> Option<Duration> {
    let reserve = if background { BACKGROUND_RESERVE } else { 0. };
    let needed = (f64::from(cost) + reserve).min(BURST);
    if tokens >= needed {
        None
    } else {
        Some(Duration::from_secs_f64((needed - tokens) / RATE_PER_SECOND))
    }
}

/// What a Gmail API call costs, from the method and the path below `users/me` (with its
/// query). Unknown calls cost 20, Google's usual price, so a new call errs on the safe side.
pub fn cost(method: &str, path_and_query: &str) -> u32 {
    let (path, query) = path_and_query.split_once('?').unwrap_or((path_and_query, ""));
    let format = query.split('&').find_map(|pair| pair.strip_prefix("format=")).unwrap_or("full");
    let full = matches!(format, "full" | "raw" | "html");
    let segments: Vec<&str> = path.trim_matches('/').split('/').filter(|s| !s.is_empty()).collect();
    match (method, segments.as_slice()) {
        ("GET", ["profile"]) => 1,
        ("GET", ["history"]) => 2,
        ("GET", ["labels", ..]) => 1,
        ("GET", ["settings", ..]) => 1,
        ("GET", ["messages"]) => 5,
        ("GET", ["messages", _, "attachments", _]) => 20,
        ("GET", ["messages", _]) => if full { 60 } else { 20 },
        ("GET", ["threads"]) => 10,
        ("GET", ["threads", _]) => if format == "full" { 120 } else { 40 },
        ("GET", ["drafts"]) => 5,
        ("GET", ["drafts", _]) => if full { 60 } else { 20 },
        ("POST", ["messages", "send"]) | ("POST", ["drafts", "send"]) => 100,
        ("POST", ["drafts"]) => 10,
        ("POST", ["messages", "batchModify"]) | ("POST", ["messages", "batchDelete"]) => 50,
        ("POST", ["messages", _, "modify"]) => 5,
        ("POST", ["messages", _, "trash"]) => 20,
        ("POST", ["messages", _, "untrash"]) => 5,
        ("POST", ["messages"]) | ("POST", ["messages", "import"]) => 25,
        ("POST", ["threads", _, "modify"]) => 10,
        ("POST", ["threads", _, "trash"]) => 20,
        ("POST", ["threads", _, "untrash"]) => 10,
        ("POST", ["labels"]) => 5,
        ("POST", ["watch"]) => 100,
        ("POST", ["stop"]) => 50,
        ("PUT", ["drafts", _]) => 15,
        ("PUT", ["labels", _]) | ("PATCH", ["labels", _]) => 5,
        ("PUT", ["settings", "vacation"]) => 5,
        ("DELETE", ["drafts", _]) | ("DELETE", ["messages", _]) => 10,
        ("DELETE", ["labels", _]) => 5,
        ("DELETE", ["threads", _]) => 20,
        _ => 20,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calls_cost_what_google_charges() {
        assert_eq!(cost("GET", "messages/abc?format=full"), 60);
        assert_eq!(cost("GET", "messages/abc"), 60, "full is the default format");
        assert_eq!(cost("GET", "messages/abc?format=metadata"), 20);
        assert_eq!(cost("GET", "messages/abc?format=minimal"), 20);
        assert_eq!(cost("GET", "messages?q=in%3Ainbox&maxResults=100"), 5);
        assert_eq!(cost("GET", "messages/abc/attachments/def"), 20);
        assert_eq!(cost("GET", "history?startHistoryId=5&maxResults=500"), 2);
        assert_eq!(cost("GET", "labels"), 1);
        assert_eq!(cost("GET", "profile"), 1);
        assert_eq!(cost("GET", "drafts/x?format=raw"), 60);
        assert_eq!(cost("POST", "messages/send"), 100);
        assert_eq!(cost("POST", "messages/batchModify"), 50);
        assert_eq!(cost("POST", "messages/abc/trash"), 20);
        assert_eq!(cost("PUT", "settings/vacation"), 5);
        assert_eq!(cost("GET", "something/new"), 20);
    }

    #[test]
    fn the_bucket_never_lets_a_minute_spend_more_than_google_allows() {
        assert!(BURST + 60. * RATE_PER_SECOND < 6_000.);
        assert!(BACKGROUND_RESERVE < BURST);
    }

    #[test]
    fn background_work_leaves_the_reserve() {
        // The user's request fits; background work at the same level waits.
        assert_eq!(wait_for(100., 60, false), None);
        let wait = wait_for(100., 60, true).unwrap();
        assert!((wait.as_secs_f64() - (460. - 100.) / RATE_PER_SECOND).abs() < 1e-9);
    }

    #[test]
    fn a_cost_above_the_bucket_waits_for_a_full_bucket() {
        assert_eq!(wait_for(BURST, 3_000, true), None);
        assert!(wait_for(BURST - 1., 3_000, false).is_some());
    }

    #[test]
    fn refilling_stops_at_the_burst() {
        assert_eq!(refilled(0., Duration::from_secs(1)), RATE_PER_SECOND);
        assert_eq!(refilled(900., Duration::from_secs(60)), BURST);
    }

    #[test]
    fn spending_paces_requests() {
        let budget = QuotaBudget::new();
        let start = Instant::now();
        // A full bucket covers 1,000 units at once; 80 more need about a second.
        budget.spend_as(1_000, false);
        budget.spend_as(80, false);
        let elapsed = start.elapsed().as_secs_f64();
        assert!(elapsed > 0.9 && elapsed < 2., "{elapsed}");
    }
}
