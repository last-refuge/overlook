//! Authenticated Gmail REST client (blocking; call from a background thread).

use super::oauth::{self, url_encode, AuthError, ClientConfig, OAuthTokens};
use crate::ProviderError;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, RwLock};
use std::time::{Duration, Instant};

pub const API_BASE: &str = "https://gmail.googleapis.com/gmail/v1/users/me";
pub const BATCH_URL: &str = "https://www.googleapis.com/batch/gmail/v1";
/// Gmail accepts up to 100 calls per batch; smaller batches keep one 429 from spoiling much.
pub const BATCH_SIZE: usize = 20;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiError {
    NotFound,
    /// 410: the sync token or resource is gone; start over.
    Gone,
    Unauthorized,
    RateLimited(String),
    Server(String),
    Client(u16, String),
    Transport(String),
}

impl From<ApiError> for ProviderError {
    fn from(e: ApiError) -> Self {
        match e {
            ApiError::Transport(_) => ProviderError::Offline,
            ApiError::Unauthorized => ProviderError::SignInRequired,
            ApiError::RateLimited(m) => ProviderError::RateLimited(m),
            ApiError::Server(m) => ProviderError::Temporary(m),
            ApiError::NotFound => ProviderError::Rejected("not found at Google".into()),
            ApiError::Gone => ProviderError::CheckpointExpired,
            ApiError::Client(code, m) => ProviderError::Rejected(format!("Gmail returned {code}: {m}")),
        }
    }
}

pub type TokenSink = Box<dyn Fn(&OAuthTokens) + Send + Sync>;
/// Called once when Google rejects the refresh token, so the stored copy can be deleted.
pub type InvalidationSink = Box<dyn Fn() + Send + Sync>;

// Session limits are shared with Microsoft sign-ins; see `crate::oauth`.
pub use crate::oauth::{session_expiry, session_idle_expired, SessionExpiry, SESSION_IDLE_LIMIT_MS, SESSION_MAX_AGE_MS};

/// Minimum spacing between requests, on top of the quota budget (`quota.rs`), which is what
/// keeps an account under Google's per-minute limit; this only stops a run of cheap calls
/// from arriving all at once.
pub const MIN_REQUEST_INTERVAL: Duration = Duration::from_millis(170);

pub struct GmailClient {
    config: ClientConfig,
    tokens: Mutex<OAuthTokens>,
    on_tokens: TokenSink,
    on_invalidated: Option<InvalidationSink>,
    agent: ureq::Agent,
    last_request: Mutex<Option<Instant>>,
    /// Allows concurrent requests (read lock) while draining them before revocation (write lock).
    session_lock: RwLock<()>,
    /// Serializes token refreshes so concurrent 401s or expiries share a single refresh.
    refresh_lock: Mutex<()>,
    active: AtomicBool,
    api_base: String,
    batch_url: String,
    /// The account's share of Google's per-user quota; see `quota.rs`.
    budget: super::quota::QuotaBudget,
}

impl GmailClient {
    /// `on_tokens` is called whenever tokens are refreshed so the caller can persist them.
    pub fn new(config: ClientConfig, mut tokens: OAuthTokens, on_tokens: TokenSink) -> Self {
        // Tokens stored before sign-in times were recorded start their clock now; the next
        // refresh persists it.
        if tokens.signed_in_at == 0 {
            tokens.signed_in_at = overlook_core::time::now();
        }
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .https_only(true)
            .max_redirects(0)
            .timeout_global(Some(Duration::from_secs(40)))
            .build()
            .into();
        Self { config, tokens: Mutex::new(tokens), on_tokens, on_invalidated: None, agent, last_request: Mutex::new(None), session_lock: RwLock::new(()), refresh_lock: Mutex::new(()), active: AtomicBool::new(true), api_base: API_BASE.into(), batch_url: BATCH_URL.into(), budget: super::quota::QuotaBudget::new() }
    }

    pub fn with_on_invalidated(mut self, sink: InvalidationSink) -> Self {
        self.on_invalidated = Some(sink);
        self
    }

    /// Whether the session has ended on its own: idle past [`SESSION_IDLE_LIMIT_MS`] (every
    /// refresh moves `expires_at` forward, so its age measures the time since last use), or
    /// older than [`SESSION_MAX_AGE_MS`] since sign-in.
    pub fn session_expiry(&self, now: i64) -> Option<SessionExpiry> {
        let tokens = self.tokens();
        session_expiry(&tokens, now)
    }

    /// Blocks future use (including cloned providers), drains an in-flight request, then
    /// revokes the remote grant. On failure the keyring entry is retained for an explicit retry.
    pub fn disconnect(&self) -> Result<(), AuthError> {
        self.disconnect_with(oauth::revoke)
    }

    fn disconnect_with(&self, revoke: impl FnOnce(&OAuthTokens) -> Result<(), AuthError>) -> Result<(), AuthError> {
        self.active.store(false, Ordering::SeqCst);
        let _session = self.session_lock.write().unwrap_or_else(|p| p.into_inner());
        let tokens = self.tokens();
        revoke(&tokens)?;
        let mut tokens = self.tokens.lock().unwrap_or_else(|p| p.into_inner());
        tokens.access_token.clear();
        tokens.refresh_token = None;
        tokens.expires_at = 0;
        Ok(())
    }

    fn throttle(&self) {
        let mut last = self.last_request.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(prev) = *last {
            let elapsed = prev.elapsed();
            if elapsed < MIN_REQUEST_INTERVAL {
                std::thread::sleep(MIN_REQUEST_INTERVAL - elapsed);
            }
        }
        *last = Some(Instant::now());
    }

    pub fn tokens(&self) -> OAuthTokens {
        self.tokens.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    fn access_token(&self, rejected: Option<&str>) -> Result<String, ApiError> {
        if !self.active.load(Ordering::SeqCst) {
            return Err(ApiError::Unauthorized);
        }
        let usable = |tokens: &OAuthTokens| {
            !tokens.access_token.is_empty()
                && !tokens.is_expired(overlook_core::time::now())
                && rejected != Some(tokens.access_token.as_str())
        };
        let current = self.tokens();
        if usable(&current) {
            return Ok(current.access_token);
        }
        let _refreshing = self.refresh_lock.lock().unwrap_or_else(|p| p.into_inner());
        if !self.active.load(Ordering::SeqCst) {
            return Err(ApiError::Unauthorized);
        }
        // Another request may have refreshed while this one waited for the lock.
        let current = self.tokens();
        if usable(&current) {
            return Ok(current.access_token);
        }
        let Some(refresh) = current.refresh_token.as_deref() else {
            return Err(ApiError::Unauthorized);
        };
        let result = oauth::refresh(&self.config, refresh);
        self.accept_refresh(result)
    }

    fn accept_refresh(&self, result: Result<OAuthTokens, AuthError>) -> Result<String, ApiError> {
        match result {
            Ok(mut fresh) => {
                {
                    let prev = self.tokens.lock().unwrap_or_else(|p| p.into_inner());
                    fresh.signed_in_at = prev.signed_in_at;
                    if fresh.scope.is_none() {
                        fresh.scope = prev.scope.clone();
                    }
                }
                (self.on_tokens)(&fresh);
                let token = fresh.access_token.clone();
                *self.tokens.lock().unwrap_or_else(|p| p.into_inner()) = fresh;
                Ok(token)
            }
            Err(AuthError::Denied(m)) => {
                // invalid_grant: the refresh token expired or was revoked at Google. It can
                // never work again, so it is dropped from memory and from the keyring.
                if m.starts_with("invalid_grant") {
                    self.invalidate_locally();
                }
                Err(ApiError::Unauthorized)
            }
            Err(AuthError::Network(m)) => Err(ApiError::Transport(m)),
            Err(AuthError::Protocol(m)) => Err(ApiError::Server(m)),
        }
    }

    /// Ends a session Google has already invalidated: no revocation call is needed.
    fn invalidate_locally(&self) {
        if !self.active.swap(false, Ordering::SeqCst) {
            return;
        }
        {
            let mut tokens = self.tokens.lock().unwrap_or_else(|p| p.into_inner());
            tokens.access_token.clear();
            tokens.refresh_token = None;
            tokens.expires_at = 0;
        }
        tracing::info!(target: "overlook::signin", "Google rejected the refresh token (invalid_grant); session ended and stored token deleted");
        if let Some(sink) = &self.on_invalidated {
            sink();
        }
    }

    pub fn get(&self, path: &str, query: &[(&str, &str)]) -> Result<serde_json::Value, ApiError> {
        self.get_url(&format!("{}/{}{}", self.api_base, path, Self::query_string(query)))
    }

    pub fn post(&self, path: &str, body: &serde_json::Value) -> Result<serde_json::Value, ApiError> {
        self.post_url(&format!("{}/{path}", self.api_base), body)
    }

    pub fn put(&self, path: &str, body: &serde_json::Value) -> Result<serde_json::Value, ApiError> {
        self.put_url(&format!("{}/{path}", self.api_base), body)
    }

    pub fn patch(&self, path: &str, body: &serde_json::Value) -> Result<serde_json::Value, ApiError> {
        self.patch_url(&format!("{}/{path}", self.api_base), body)
    }

    pub fn delete(&self, path: &str) -> Result<serde_json::Value, ApiError> {
        self.delete_url(&format!("{}/{path}", self.api_base))
    }

    #[cfg(any(test, feature = "fake-servers"))]
    pub fn with_test_endpoint(mut self, endpoint: String) -> Self {
        assert!(endpoint.starts_with("http://127.0.0.1:"));
        self.batch_url = format!("{endpoint}/batch");
        self.api_base = endpoint;
        self.agent = ureq::Agent::config_builder().http_status_as_error(false).https_only(false).max_redirects(0).timeout_global(Some(Duration::from_secs(5))).build().into();
        self
    }

    /// Whether the stored grant covers calendar access.
    pub fn has_scope(&self, marker: &str) -> bool {
        self.tokens().scope.as_deref().is_some_and(|s| s.contains(marker))
    }

    pub fn query_string(query: &[(&str, &str)]) -> String {
        if query.is_empty() {
            return String::new();
        }
        format!("?{}", query.iter().map(|(k, v)| format!("{k}={}", url_encode(v))).collect::<Vec<_>>().join("&"))
    }

    /// What a request to `url` costs against the account's Gmail quota: nothing for the
    /// Calendar and People APIs, which Google meters separately.
    fn cost(&self, method: &str, url: &str) -> u32 {
        match url.strip_prefix(self.api_base.as_str()) {
            Some(rest) => super::quota::cost(method, rest.trim_start_matches('/')),
            None => 0,
        }
    }

    pub fn get_url(&self, url: &str) -> Result<serde_json::Value, ApiError> {
        self.with_retry(self.cost("GET", url), |token| {
            self.agent
                .get(url)
                .header("Authorization", &format!("Bearer {token}"))
                .call()
                .map_err(|e| ApiError::Transport(crate::secure_error::ureq("Google API request", e).into()))
        })
    }

    pub fn post_url(&self, url: &str, body: &serde_json::Value) -> Result<serde_json::Value, ApiError> {
        self.with_retry(self.cost("POST", url), |token| {
            self.agent
                .post(url)
                .header("Authorization", &format!("Bearer {token}"))
                .send_json(body)
                .map_err(|e| ApiError::Transport(crate::secure_error::ureq("Google API request", e).into()))
        })
    }

    pub fn patch_url(&self, url: &str, body: &serde_json::Value) -> Result<serde_json::Value, ApiError> {
        self.with_retry(self.cost("PATCH", url), |token| {
            self.agent
                .patch(url)
                .header("Authorization", &format!("Bearer {token}"))
                .send_json(body)
                .map_err(|e| ApiError::Transport(crate::secure_error::ureq("Google API request", e).into()))
        })
    }

    pub fn put_url(&self, url: &str, body: &serde_json::Value) -> Result<serde_json::Value, ApiError> {
        self.with_retry(self.cost("PUT", url), |token| {
            self.agent
                .put(url)
                .header("Authorization", &format!("Bearer {token}"))
                .send_json(body)
                .map_err(|e| ApiError::Transport(crate::secure_error::ureq("Google API request", e).into()))
        })
    }

    pub fn delete_url(&self, url: &str) -> Result<serde_json::Value, ApiError> {
        self.with_retry(self.cost("DELETE", url), |token| {
            self.agent
                .delete(url)
                .header("Authorization", &format!("Bearer {token}"))
                .call()
                .map_err(|e| ApiError::Transport(crate::secure_error::ureq("Google API request", e).into()))
        })
    }

    /// Runs the request, refreshing the token once on 401 and mapping statuses to errors.
    /// `cost` is spent from the quota budget first, outside the session lock, so a request
    /// waiting for quota doesn't hold up one that has it.
    fn with_retry(
        &self,
        cost: u32,
        request: impl Fn(&str) -> Result<ureq::http::Response<ureq::Body>, ApiError>,
    ) -> Result<serde_json::Value, ApiError> {
        self.budget.spend(cost);
        let result = self.send_with_retry(request);
        if matches!(result, Err(ApiError::RateLimited(_))) {
            self.budget.exhausted();
        }
        result
    }

    fn send_with_retry(
        &self,
        request: impl Fn(&str) -> Result<ureq::http::Response<ureq::Body>, ApiError>,
    ) -> Result<serde_json::Value, ApiError> {
        let _session = self.session_lock.read().unwrap_or_else(|p| p.into_inner());
        let mut token = self.access_token(None)?;
        for attempt in 0..2 {
            self.throttle();
            let mut response = request(&token)?;
            let status = response.status().as_u16();
            // Raw draft MIME is base64url inside JSON; ureq's default 10 MB limit
            // would reject valid attachments well below our outgoing size limit.
            let text = response.body_mut().with_config().limit(64 * 1024 * 1024).read_to_string().map_err(|e| ApiError::Transport(crate::secure_error::ureq("Google API request", e).into()))?;
            match status {
                200..=299 => {
                    if text.trim().is_empty() {
                        return Ok(serde_json::Value::Null);
                    }
                    return serde_json::from_str(&text).map_err(|e| ApiError::Server(format!("invalid JSON from Gmail: {e}")));
                }
                401 if attempt == 0 => {
                    token = self.access_token(Some(&token))?;
                    continue;
                }
                401 => return Err(ApiError::Unauthorized),
                404 => return Err(ApiError::NotFound),
                410 => return Err(ApiError::Gone),
                403 | 429 => return Err(classify_forbidden_or_limited(status, &text)),
                500..=599 => return Err(ApiError::Server(api_message(&text))),
                _ => return Err(ApiError::Client(status, api_message(&text))),
            }
        }
        Err(ApiError::Unauthorized)
    }
}

impl GmailClient {
    /// Several GETs in one HTTP request through Gmail's batch endpoint. Each `path` is
    /// relative to `users/me` (for example `messages/abc?format=full`). The outer result is the
    /// request itself; each inner result is that call's own outcome, in the order given.
    pub fn batch_get(&self, paths: &[String]) -> Result<Vec<Result<serde_json::Value, ApiError>>, ApiError> {
        self.batch_method("GET", paths)
    }

    /// Several body-less POSTs (e.g. `messages/{id}/trash`, `messages/{id}/untrash`) in one
    /// HTTP request through Gmail's batch endpoint.
    pub fn batch_post(&self, paths: &[String]) -> Result<Vec<Result<serde_json::Value, ApiError>>, ApiError> {
        self.batch_method("POST", paths)
    }

    fn batch_method(&self, method: &str, paths: &[String]) -> Result<Vec<Result<serde_json::Value, ApiError>>, ApiError> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        // Each call in a batch counts on its own against the quota.
        self.budget.spend(paths.iter().map(|p| super::quota::cost(method, p)).sum());
        let result = self.send_batch(method, paths);
        let limited = match &result {
            Err(ApiError::RateLimited(_)) => true,
            Ok(parts) => parts.iter().any(|p| matches!(p, Err(ApiError::RateLimited(_)))),
            Err(_) => false,
        };
        if limited {
            self.budget.exhausted();
        }
        result
    }

    fn send_batch(&self, method: &str, paths: &[String]) -> Result<Vec<Result<serde_json::Value, ApiError>>, ApiError> {
        let _session = self.session_lock.read().unwrap_or_else(|p| p.into_inner());
        let nonce: u64 = rand::random();
        let boundary = format!("overlook_batch_{}_{nonce:016x}", overlook_core::time::now());
        let mut body = String::new();
        for (i, path) in paths.iter().enumerate() {
            if method == "POST" {
                body.push_str(&format!("--{boundary}\r\nContent-Type: application/http\r\nContent-ID: <item{i}>\r\n\r\n{method} /gmail/v1/users/me/{path} HTTP/1.1\r\nContent-Length: 0\r\n\r\n"));
            } else {
                body.push_str(&format!("--{boundary}\r\nContent-Type: application/http\r\nContent-ID: <item{i}>\r\n\r\n{method} /gmail/v1/users/me/{path} HTTP/1.1\r\n\r\n"));
            }
        }
        body.push_str(&format!("--{boundary}--\r\n"));
        let mut token = self.access_token(None)?;
        for attempt in 0..2 {
            self.throttle();
            let mut response = self
                .agent
                .post(&self.batch_url)
                .header("Authorization", &format!("Bearer {token}"))
                .header("Content-Type", &format!("multipart/mixed; boundary={boundary}"))
                .send(body.as_bytes())
                .map_err(|e| ApiError::Transport(crate::secure_error::ureq("Google API request", e).into()))?;
            let status = response.status().as_u16();
            let content_type = response.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_owned();
            let text = response.body_mut().with_config().limit(64 * 1024 * 1024).read_to_string().map_err(|e| ApiError::Transport(crate::secure_error::ureq("Google API request", e).into()))?;
            match status {
                200..=299 => {
                    let Some(reply_boundary) = content_type.split(';').map(str::trim).find_map(|p| p.strip_prefix("boundary=")).map(|b| b.trim_matches('"').to_owned()) else {
                        return Err(ApiError::Server("batch reply without a boundary".into()));
                    };
                    return Ok(parse_batch_reply(&text, &reply_boundary, paths.len()));
                }
                401 if attempt == 0 => {
                    token = self.access_token(Some(&token))?;
                    continue;
                }
                401 => return Err(ApiError::Unauthorized),
                403 | 429 => return Err(classify_forbidden_or_limited(status, &text)),
                500..=599 => return Err(ApiError::Server(api_message(&text))),
                _ => return Err(ApiError::Client(status, api_message(&text))),
            }
        }
        Err(ApiError::Unauthorized)
    }
}

/// Classifies HTTP 403 / 429 replies consistently across single requests, batch outer
/// responses, and batch inner parts.
fn classify_forbidden_or_limited(status: u16, body: &str) -> ApiError {
    let message = api_message(body);
    let lower = message.to_ascii_lowercase();
    if status == 429 || lower.contains("ratelimit") || lower.contains("quota") || lower.contains("rate limit") {
        return ApiError::RateLimited(message);
    }
    if lower.contains("insufficient") && lower.contains("scope") {
        return ApiError::Unauthorized;
    }
    ApiError::Client(status, message)
}

/// Splits a multipart/mixed batch reply into one result per call, matched by `Content-ID`
/// (`<response-itemN>`), falling back to document order.
pub fn parse_batch_reply(text: &str, boundary: &str, expected: usize) -> Vec<Result<serde_json::Value, ApiError>> {
    let mut out: Vec<Option<Result<serde_json::Value, ApiError>>> = (0..expected).map(|_| None).collect();
    let delimiter = format!("--{boundary}");
    let mut order = 0usize;
    for part in text.split(delimiter.as_str()).skip(1) {
        let part = part.trim_start_matches("\r\n").trim_start_matches('\n');
        if part.starts_with("--") || part.trim().is_empty() {
            continue;
        }
        // Outer part headers, then the embedded HTTP response.
        let Some((outer_headers, http)) = split_once_blank(part) else { continue };
        let index = outer_headers
            .lines()
            .find_map(|l| l.strip_prefix("Content-ID:").or_else(|| l.strip_prefix("content-id:")))
            .and_then(|v| v.trim().trim_matches(['<', '>']).strip_prefix("response-item"))
            .and_then(|n| n.parse::<usize>().ok())
            .unwrap_or(order);
        order += 1;
        let (status, body) = match split_once_blank(http) {
            Some((head, body)) => {
                let status = head.lines().next().and_then(|l| l.split_whitespace().nth(1)).and_then(|s| s.parse::<u16>().ok()).unwrap_or(0);
                (status, body.trim())
            }
            None => (0, ""),
        };
        let result = match status {
            200..=299 => {
                if body.is_empty() {
                    Ok(serde_json::Value::Null)
                } else {
                    serde_json::from_str(body).map_err(|e| ApiError::Server(format!("invalid JSON in batch part: {e}")))
                }
            }
            401 => Err(ApiError::Unauthorized),
            404 => Err(ApiError::NotFound),
            410 => Err(ApiError::Gone),
            403 | 429 => Err(classify_forbidden_or_limited(status, body)),
            500..=599 => Err(ApiError::Server(api_message(body))),
            other => Err(ApiError::Client(other, api_message(body))),
        };
        if index < out.len() && out[index].is_none() {
            out[index] = Some(result);
        }
    }
    out.into_iter().map(|r| r.unwrap_or_else(|| Err(ApiError::Server("missing batch part".into())))).collect()
}

/// Splits at the first blank line (CRLF or LF).
fn split_once_blank(s: &str) -> Option<(&str, &str)> {
    let crlf = s.find("\r\n\r\n").map(|i| (i, 4));
    let lf = s.find("\n\n").map(|i| (i, 2));
    let (i, n) = match (crlf, lf) {
        (Some(a), Some(b)) => if a.0 <= b.0 { a } else { b },
        (Some(a), None) => a,
        (None, Some(b)) => b,
        (None, None) => return None,
    };
    Some((&s[..i], &s[i + n..]))
}

fn api_message(text: &str) -> String {
    serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .and_then(|v| v.get("error").and_then(|e| e.get("message")).and_then(|m| m.as_str()).map(str::to_owned))
        .unwrap_or_else(|| text.chars().take(200).collect())
}

#[cfg(test)]
mod batch_tests {
    use super::*;

    fn synthetic_client(on_tokens: TokenSink) -> GmailClient {
        GmailClient::new(ClientConfig { client_id: "synthetic".into(), client_secret: None }, OAuthTokens { access_token: "synthetic-access".into(), refresh_token: Some("synthetic-refresh".into()), expires_at: 0, scope: None, signed_in_at: 0 }, on_tokens)
    }

    #[test]
    fn invalid_grant_ends_the_session_and_deletes_the_stored_token() {
        let deleted = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = deleted.clone();
        let client = synthetic_client(Box::new(|_| panic!("a rejected refresh must not persist tokens")))
            .with_on_invalidated(Box::new(move || { counter.fetch_add(1, Ordering::SeqCst); }));
        assert_eq!(client.accept_refresh(Err(AuthError::Denied("invalid_grant: Token has been expired or revoked.".into()))), Err(ApiError::Unauthorized));
        assert_eq!(deleted.load(Ordering::SeqCst), 1);
        assert!(client.tokens().refresh_token.is_none() && client.tokens().access_token.is_empty());
        // The client stays disabled; no second deletion or network attempt follows.
        assert_eq!(client.get("messages", &[]), Err(ApiError::Unauthorized));
        client.invalidate_locally();
        assert_eq!(deleted.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn other_refusals_keep_the_stored_token() {
        let client = synthetic_client(Box::new(|_| {})).with_on_invalidated(Box::new(|| panic!("only invalid_grant deletes the token")));
        assert_eq!(client.accept_refresh(Err(AuthError::Denied("invalid_client".into()))), Err(ApiError::Unauthorized));
        assert_eq!(client.tokens().refresh_token.as_deref(), Some("synthetic-refresh"));
        assert!(matches!(client.accept_refresh(Err(AuthError::Network("offline".into()))), Err(ApiError::Transport(_))));
        assert_eq!(client.tokens().refresh_token.as_deref(), Some("synthetic-refresh"));
    }

    #[test]
    fn sessions_end_after_180_days_however_active_and_refresh_keeps_the_sign_in_time() {
        let day = 24 * 60 * 60 * 1000;
        let signed_in = 1_800_000_000_000;
        let tokens = |expires_at| OAuthTokens { access_token: "a".into(), refresh_token: Some("r".into()), expires_at, scope: None, signed_in_at: signed_in };
        assert_eq!(session_expiry(&tokens(signed_in + 179 * day), signed_in + 179 * day), None);
        assert_eq!(session_expiry(&tokens(signed_in + 181 * day), signed_in + 181 * day), Some(SessionExpiry::MaxAge), "active daily, still ends");
        assert_eq!(session_expiry(&tokens(signed_in + day), signed_in + 95 * day), Some(SessionExpiry::Idle));
        let persisted = std::sync::Arc::new(std::sync::Mutex::new(None));
        let sink = persisted.clone();
        let client = GmailClient::new(ClientConfig { client_id: "synthetic".into(), client_secret: None }, tokens(signed_in), Box::new(move |t| *sink.lock().unwrap() = Some(t.signed_in_at)));
        let mut fresh = tokens(signed_in + 2 * day);
        fresh.signed_in_at = signed_in + 2 * day;
        client.accept_refresh(Ok(fresh)).unwrap();
        assert_eq!(*persisted.lock().unwrap(), Some(signed_in));
        assert_eq!(client.tokens().signed_in_at, signed_in);
    }

    #[test]
    fn legacy_tokens_start_their_clock_when_loaded() {
        let client = synthetic_client(Box::new(|_| {}));
        assert!(client.tokens().signed_in_at > 0);
    }

    #[test]
    fn sessions_expire_after_ninety_idle_days() {
        let day = 24 * 60 * 60 * 1000;
        let last_refresh = 1_800_000_000_000;
        assert!(!session_idle_expired(last_refresh, last_refresh + 89 * day));
        assert!(!session_idle_expired(last_refresh, last_refresh + 90 * day));
        assert!(session_idle_expired(last_refresh, last_refresh + 91 * day));
        assert!(!session_idle_expired(0, last_refresh), "no session, nothing to expire");
    }

    #[test]
    fn disconnect_blocks_cloned_client_and_retains_retry_on_failure() {
        let client = std::sync::Arc::new(GmailClient::new(ClientConfig { client_id: "synthetic".into(), client_secret: None }, OAuthTokens { access_token: "synthetic-access".into(), refresh_token: Some("synthetic-refresh".into()), expires_at: i64::MAX, scope: None, signed_in_at: 0 }, Box::new(|_| panic!("no refresh may persist after disconnect"))));
        let clone = client.clone();
        assert!(client.disconnect_with(|_| Err(AuthError::Network("synthetic offline".into()))).is_err());
        assert_eq!(clone.get("messages", &[]), Err(ApiError::Unauthorized));
        assert_eq!(clone.batch_get(&["messages/test".into()]), Err(ApiError::Unauthorized));
        assert_eq!(clone.tokens().refresh_token.as_deref(), Some("synthetic-refresh"));
        client.disconnect_with(|tokens| { assert_eq!(tokens.refresh_token.as_deref(), Some("synthetic-refresh")); Ok(()) }).unwrap();
        assert!(clone.tokens().access_token.is_empty());
        assert!(clone.tokens().refresh_token.is_none());
        assert_eq!(clone.get("messages", &[]), Err(ApiError::Unauthorized));
    }

    #[test]
    fn parses_a_batch_reply_in_content_id_order() {
        let b = "batch_x";
        let reply = format!(
            "--{b}\r\nContent-Type: application/http\r\nContent-ID: <response-item1>\r\n\r\nHTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\n\r\n{{\"error\":{{\"message\":\"nope\"}}}}\r\n--{b}\r\nContent-Type: application/http\r\nContent-ID: <response-item0>\r\n\r\nHTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{{\"id\":\"m0\"}}\r\n--{b}\r\nContent-Type: application/http\r\nContent-ID: <response-item2>\r\n\r\nHTTP/1.1 429 Too Many Requests\r\n\r\n{{\"error\":{{\"message\":\"User-rate limit exceeded\"}}}}\r\n--{b}--\r\n"
        );
        let parts = parse_batch_reply(&reply, b, 3);
        assert_eq!(parts[0].as_ref().unwrap()["id"], "m0");
        assert_eq!(parts[1], Err(ApiError::NotFound));
        assert!(matches!(parts[2], Err(ApiError::RateLimited(_))));
    }

    #[test]
    fn batch_403_classification_matches_single_request_classification() {
        let b = "batch_403";
        let reply = format!(
            "--{b}\r\nContent-Type: application/http\r\nContent-ID: <response-item0>\r\n\r\nHTTP/1.1 403 Forbidden\r\n\r\n{{\"error\":{{\"message\":\"Request had insufficient authentication scopes.\"}}}}\r\n\
             --{b}\r\nContent-Type: application/http\r\nContent-ID: <response-item1>\r\n\r\nHTTP/1.1 403 Forbidden\r\n\r\n{{\"error\":{{\"message\":\"User-rate limit exceeded. Retry later.\"}}}}\r\n\
             --{b}\r\nContent-Type: application/http\r\nContent-ID: <response-item2>\r\n\r\nHTTP/1.1 403 Forbidden\r\n\r\n{{\"error\":{{\"message\":\"Delegation denied.\"}}}}\r\n\
             --{b}--\r\n"
        );
        let parts = parse_batch_reply(&reply, b, 3);
        assert_eq!(parts[0], Err(ApiError::Unauthorized));
        assert!(matches!(parts[1], Err(ApiError::RateLimited(_))));
        assert!(matches!(parts[2], Err(ApiError::Client(403, _))));
        assert_eq!(classify_forbidden_or_limited(403, r#"{"error":{"message":"Insufficient scope"}}"#), ApiError::Unauthorized);
        assert!(matches!(classify_forbidden_or_limited(403, r#"{"error":{"message":"Quota exceeded"}}"#), ApiError::RateLimited(_)));
        assert!(matches!(classify_forbidden_or_limited(403, r#"{"error":{"message":"Forbidden"}}"#), ApiError::Client(403, _)));
    }

    #[test]
    fn send_batch_reads_large_responses_above_10mb_and_classifies_outer_403() {
        use std::io::{BufRead, BufReader, Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let worker = std::thread::spawn(move || {
            for mode in ["large", "scope_403"] {
                let (mut stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut length = 0usize;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    if let Some((k, v)) = line.split_once(':') {
                        if k.eq_ignore_ascii_case("content-length") {
                            length = v.trim().parse().unwrap_or(0);
                        }
                    }
                }
                let mut req_body = vec![0u8; length];
                reader.read_exact(&mut req_body).unwrap();
                match mode {
                    "large" => {
                        let pad = "a".repeat(11 * 1024 * 1024);
                        let body = format!(
                            "--b\r\nContent-Type: application/http\r\nContent-ID: <response-item0>\r\n\r\nHTTP/1.1 200 OK\r\n\r\n{{\"id\":\"big\",\"pad\":\"{pad}\"}}\r\n--b--\r\n"
                        );
                        write!(
                            stream,
                            "HTTP/1.1 200 OK\r\nContent-Type: multipart/mixed; boundary=b\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        )
                        .unwrap();
                    }
                    "scope_403" => {
                        let body = r#"{"error":{"message":"Request had insufficient authentication scopes."}}"#;
                        write!(
                            stream,
                            "HTTP/1.1 403 Forbidden\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        )
                        .unwrap();
                    }
                    _ => unreachable!(),
                }
            }
        });
        let now = overlook_core::time::now();
        let client = GmailClient::new(
            ClientConfig { client_id: "synthetic".into(), client_secret: None },
            OAuthTokens { access_token: "tok".into(), refresh_token: None, expires_at: now + 3_600_000, scope: None, signed_in_at: now },
            Box::new(|_| {}),
        )
        .with_test_endpoint(endpoint);
        let res = client.batch_get(&["messages/big?format=full".into()]).unwrap();
        assert_eq!(res[0].as_ref().unwrap()["id"], "big");
        assert_eq!(client.batch_get(&["messages/x".into()]), Err(ApiError::Unauthorized));
        worker.join().unwrap();
    }

    #[test]
    fn accept_refresh_preserves_scope_when_omitted() {
        let now = overlook_core::time::now();
        let saved = std::sync::Arc::new(std::sync::Mutex::new(None));
        let sink = saved.clone();
        let client = GmailClient::new(
            ClientConfig { client_id: "synthetic".into(), client_secret: None },
            OAuthTokens {
                access_token: "old".into(),
                refresh_token: Some("ref".into()),
                expires_at: now,
                scope: Some("https://www.googleapis.com/auth/gmail.modify https://www.googleapis.com/auth/calendar.events".into()),
                signed_in_at: now,
            },
            Box::new(move |t| *sink.lock().unwrap() = t.scope.clone()),
        );
        let refreshed = OAuthTokens {
            access_token: "new".into(),
            refresh_token: Some("ref".into()),
            expires_at: now + 3_600_000,
            scope: None,
            signed_in_at: now + 1000,
        };
        assert_eq!(client.accept_refresh(Ok(refreshed)).unwrap(), "new");
        let expected = Some("https://www.googleapis.com/auth/gmail.modify https://www.googleapis.com/auth/calendar.events".to_owned());
        assert_eq!(client.tokens().scope, expected);
        assert_eq!(*saved.lock().unwrap(), expected);
        assert!(client.has_scope(super::super::oauth::CALENDAR_SCOPE_MARKER));
    }
}
