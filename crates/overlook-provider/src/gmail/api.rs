//! Authenticated Gmail REST client (blocking; call from a background thread).

use super::oauth::{self, url_encode, AuthError, ClientConfig, OAuthTokens};
use crate::ProviderError;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
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

/// Minimum spacing between requests. Gmail meters usage in quota units per user per minute;
/// a `messages.get` costs 5 units, so 6 requests per second stays well inside the default
/// allowance while keeping a 150-message download under half a minute.
pub const MIN_REQUEST_INTERVAL: Duration = Duration::from_millis(170);

pub struct GmailClient {
    config: ClientConfig,
    tokens: Mutex<OAuthTokens>,
    on_tokens: TokenSink,
    agent: ureq::Agent,
    last_request: Mutex<Option<Instant>>,
    /// Serializes request/refresh persistence with grant revocation.
    session_lock: Mutex<()>,
    active: AtomicBool,
}

impl GmailClient {
    /// `on_tokens` is called whenever tokens are refreshed so the caller can persist them.
    pub fn new(config: ClientConfig, tokens: OAuthTokens, on_tokens: TokenSink) -> Self {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .https_only(true)
            .max_redirects(0)
            .timeout_global(Some(Duration::from_secs(40)))
            .build()
            .into();
        Self { config, tokens: Mutex::new(tokens), on_tokens, agent, last_request: Mutex::new(None), session_lock: Mutex::new(()), active: AtomicBool::new(true) }
    }

    /// Blocks future use (including cloned providers), drains an in-flight request, then
    /// revokes the remote grant. On failure the keyring entry is retained for an explicit retry.
    pub fn disconnect(&self) -> Result<(), AuthError> {
        self.disconnect_with(oauth::revoke)
    }

    fn disconnect_with(&self, revoke: impl FnOnce(&OAuthTokens) -> Result<(), AuthError>) -> Result<(), AuthError> {
        self.active.store(false, Ordering::SeqCst);
        let _session = self.session_lock.lock().unwrap();
        let tokens = self.tokens();
        revoke(&tokens)?;
        let mut tokens = self.tokens.lock().unwrap();
        tokens.access_token.clear();
        tokens.refresh_token = None;
        tokens.expires_at = 0;
        Ok(())
    }

    fn throttle(&self) {
        let mut last = self.last_request.lock().unwrap();
        if let Some(prev) = *last {
            let elapsed = prev.elapsed();
            if elapsed < MIN_REQUEST_INTERVAL {
                std::thread::sleep(MIN_REQUEST_INTERVAL - elapsed);
            }
        }
        *last = Some(Instant::now());
    }

    pub fn tokens(&self) -> OAuthTokens {
        self.tokens.lock().unwrap().clone()
    }

    fn access_token(&self, force_refresh: bool) -> Result<String, ApiError> {
        if !self.active.load(Ordering::SeqCst) { return Err(ApiError::Unauthorized); }
        let current = self.tokens();
        if !force_refresh && !current.is_expired(overlook_core::time::now()) {
            return Ok(current.access_token);
        }
        let Some(refresh) = current.refresh_token.as_deref() else {
            return Err(ApiError::Unauthorized);
        };
        match oauth::refresh(&self.config, refresh) {
            Ok(fresh) => {
                (self.on_tokens)(&fresh);
                let token = fresh.access_token.clone();
                *self.tokens.lock().unwrap() = fresh;
                Ok(token)
            }
            Err(AuthError::Denied(_)) => Err(ApiError::Unauthorized),
            Err(AuthError::Network(m)) => Err(ApiError::Transport(m)),
            Err(AuthError::Protocol(m)) => Err(ApiError::Server(m)),
        }
    }

    fn url(path: &str, query: &[(&str, &str)]) -> String {
        let mut url = format!("{API_BASE}/{path}");
        if !query.is_empty() {
            url.push('?');
            url.push_str(&query.iter().map(|(k, v)| format!("{k}={}", url_encode(v))).collect::<Vec<_>>().join("&"));
        }
        url
    }

    pub fn get(&self, path: &str, query: &[(&str, &str)]) -> Result<serde_json::Value, ApiError> {
        self.get_url(&Self::url(path, query))
    }

    pub fn post(&self, path: &str, body: &serde_json::Value) -> Result<serde_json::Value, ApiError> {
        self.post_url(&Self::url(path, &[]), body)
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

    pub fn get_url(&self, url: &str) -> Result<serde_json::Value, ApiError> {
        self.with_retry(|token| {
            self.agent
                .get(url)
                .header("Authorization", &format!("Bearer {token}"))
                .call()
                .map_err(|e| ApiError::Transport(crate::secure_error::ureq("Google API request", e).into()))
        })
    }

    pub fn post_url(&self, url: &str, body: &serde_json::Value) -> Result<serde_json::Value, ApiError> {
        self.with_retry(|token| {
            self.agent
                .post(url)
                .header("Authorization", &format!("Bearer {token}"))
                .send_json(body)
                .map_err(|e| ApiError::Transport(crate::secure_error::ureq("Google API request", e).into()))
        })
    }

    pub fn patch_url(&self, url: &str, body: &serde_json::Value) -> Result<serde_json::Value, ApiError> {
        self.with_retry(|token| {
            self.agent
                .patch(url)
                .header("Authorization", &format!("Bearer {token}"))
                .send_json(body)
                .map_err(|e| ApiError::Transport(crate::secure_error::ureq("Google API request", e).into()))
        })
    }

    pub fn put_url(&self, url: &str, body: &serde_json::Value) -> Result<serde_json::Value, ApiError> {
        self.with_retry(|token| {
            self.agent
                .put(url)
                .header("Authorization", &format!("Bearer {token}"))
                .send_json(body)
                .map_err(|e| ApiError::Transport(crate::secure_error::ureq("Google API request", e).into()))
        })
    }

    pub fn delete_url(&self, url: &str) -> Result<serde_json::Value, ApiError> {
        self.with_retry(|token| {
            self.agent
                .delete(url)
                .header("Authorization", &format!("Bearer {token}"))
                .call()
                .map_err(|e| ApiError::Transport(crate::secure_error::ureq("Google API request", e).into()))
        })
    }

    /// Runs the request, refreshing the token once on 401 and mapping statuses to errors.
    fn with_retry(
        &self,
        request: impl Fn(&str) -> Result<ureq::http::Response<ureq::Body>, ApiError>,
    ) -> Result<serde_json::Value, ApiError> {
        let _session = self.session_lock.lock().unwrap();
        let mut token = self.access_token(false)?;
        for attempt in 0..2 {
            self.throttle();
            let mut response = request(&token)?;
            let status = response.status().as_u16();
            let text = response.body_mut().read_to_string().map_err(|e| ApiError::Transport(crate::secure_error::ureq("Google API request", e).into()))?;
            match status {
                200..=299 => {
                    if text.trim().is_empty() {
                        return Ok(serde_json::Value::Null);
                    }
                    return serde_json::from_str(&text).map_err(|e| ApiError::Server(format!("invalid JSON from Gmail: {e}")));
                }
                401 if attempt == 0 => {
                    token = self.access_token(true)?;
                    continue;
                }
                401 => return Err(ApiError::Unauthorized),
                404 => return Err(ApiError::NotFound),
                410 => return Err(ApiError::Gone),
                403 | 429 => {
                    let message = api_message(&text);
                    let lower = message.to_ascii_lowercase();
                    if status == 429 || lower.contains("ratelimit") || lower.contains("quota") || lower.contains("rate limit") {
                        return Err(ApiError::RateLimited(message));
                    }
                    if lower.contains("insufficient") && lower.contains("scope") {
                        return Err(ApiError::Unauthorized);
                    }
                    return Err(ApiError::Client(status, message));
                }
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
        let _session = self.session_lock.lock().unwrap();
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        let boundary = format!("overlook_batch_{}", overlook_core::time::now());
        let mut body = String::new();
        for (i, path) in paths.iter().enumerate() {
            body.push_str(&format!("--{boundary}\r\nContent-Type: application/http\r\nContent-ID: <item{i}>\r\n\r\nGET /gmail/v1/users/me/{path} HTTP/1.1\r\n\r\n"));
        }
        body.push_str(&format!("--{boundary}--\r\n"));
        let mut token = self.access_token(false)?;
        for attempt in 0..2 {
            self.throttle();
            let mut response = self
                .agent
                .post(BATCH_URL)
                .header("Authorization", &format!("Bearer {token}"))
                .header("Content-Type", &format!("multipart/mixed; boundary={boundary}"))
                .send(body.as_bytes())
                .map_err(|e| ApiError::Transport(crate::secure_error::ureq("Google API request", e).into()))?;
            let status = response.status().as_u16();
            let content_type = response.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_owned();
            let text = response.body_mut().read_to_string().map_err(|e| ApiError::Transport(crate::secure_error::ureq("Google API request", e).into()))?;
            match status {
                200..=299 => {
                    let Some(reply_boundary) = content_type.split(';').map(str::trim).find_map(|p| p.strip_prefix("boundary=")).map(|b| b.trim_matches('"').to_owned()) else {
                        return Err(ApiError::Server("batch reply without a boundary".into()));
                    };
                    return Ok(parse_batch_reply(&text, &reply_boundary, paths.len()));
                }
                401 if attempt == 0 => {
                    token = self.access_token(true)?;
                    continue;
                }
                401 => return Err(ApiError::Unauthorized),
                403 | 429 => {
                    let message = api_message(&text);
                    return Err(ApiError::RateLimited(message));
                }
                500..=599 => return Err(ApiError::Server(api_message(&text))),
                _ => return Err(ApiError::Client(status, api_message(&text))),
            }
        }
        Err(ApiError::Unauthorized)
    }
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
            200..=299 => serde_json::from_str(body).map_err(|e| ApiError::Server(format!("invalid JSON in batch part: {e}"))),
            401 => Err(ApiError::Unauthorized),
            404 => Err(ApiError::NotFound),
            410 => Err(ApiError::Gone),
            403 | 429 => {
                let message = api_message(body);
                let lower = message.to_ascii_lowercase();
                if status == 429 || lower.contains("ratelimit") || lower.contains("quota") || lower.contains("rate limit") {
                    Err(ApiError::RateLimited(message))
                } else {
                    Err(ApiError::Client(status, message))
                }
            }
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

    #[test]
    fn disconnect_blocks_cloned_client_and_retains_retry_on_failure() {
        let client = std::sync::Arc::new(GmailClient::new(ClientConfig { client_id: "synthetic".into(), client_secret: None }, OAuthTokens { access_token: "synthetic-access".into(), refresh_token: Some("synthetic-refresh".into()), expires_at: i64::MAX, scope: None }, Box::new(|_| panic!("no refresh may persist after disconnect"))));
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
}
