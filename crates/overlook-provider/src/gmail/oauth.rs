//! OAuth 2.0 for installed applications: PKCE, loopback redirect, token exchange and refresh.
//!
//! The client id comes from the Google Cloud OAuth client the user created; it is read from
//! `~/.config/overlook/google-oauth-client.json` by the app and never embedded in the binary.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::time::Duration;

/// The OAuth client created in Google Cloud ("Desktop app" type).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientConfig {
    pub client_id: String,
    /// Google issues a "secret" for desktop clients but treats it as non-confidential.
    pub client_secret: Option<String>,
}

impl ClientConfig {
    /// Parses the JSON file downloaded from Google Cloud (`{"installed": {...}}`).
    pub fn from_google_json(json: &str) -> Result<Self, String> {
        let value: serde_json::Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
        let inner = value.get("installed").or_else(|| value.get("web")).unwrap_or(&value);
        let client_id = inner
            .get("client_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or("the file has no client_id")?
            .to_owned();
        let client_secret = inner.get("client_secret").and_then(|v| v.as_str()).map(str::to_owned);
        Ok(Self { client_id, client_secret })
    }
}

/// Tokens for one account. Stored in the system keyring, never in ordinary configuration.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OAuthTokens {
    pub access_token: String,
    pub refresh_token: Option<String>,
    /// Unix milliseconds.
    pub expires_at: i64,
    pub scope: Option<String>,
    /// Unix milliseconds of the sign-in that issued the refresh token; kept across
    /// refreshes. 0 for tokens stored before this field existed.
    #[serde(default)]
    pub signed_in_at: i64,
}

impl std::fmt::Debug for ClientConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientConfig").field("client_id", &self.client_id).field("client_secret", &"[REDACTED]").finish()
    }
}

impl std::fmt::Debug for OAuthTokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuthTokens").field("access_token", &"[REDACTED]").field("refresh_token", &"[REDACTED]").field("expires_at", &self.expires_at).field("scope", &self.scope).finish()
    }
}

impl OAuthTokens {
    /// Treats the token as expired one minute early to avoid racing the deadline.
    pub fn is_expired(&self, now_ms: i64) -> bool {
        now_ms + 60_000 >= self.expires_at
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UserInfo {
    pub email: String,
    pub name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AuthError {
    #[error("could not reach Google: {0}")]
    Network(String),
    /// `invalid_grant` and friends: the user must sign in again.
    #[error("Google rejected the sign-in: {0}")]
    Denied(String),
    #[error("unexpected response from Google: {0}")]
    Protocol(String),
}

pub const USERINFO_ENDPOINT: &str = "https://openidconnect.googleapis.com/v1/userinfo";

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .https_only(true)
        .max_redirects(0)
        .timeout_global(Some(Duration::from_secs(30)))
        .build()
        .into()
}

fn token_response(mut response: ureq::http::Response<ureq::Body>, previous_refresh: Option<&str>) -> Result<OAuthTokens, AuthError> {
    let status = response.status().as_u16();
    let body: serde_json::Value = response
        .body_mut()
        .read_json()
        .map_err(|e| AuthError::Protocol(crate::secure_error::ureq("Google token response", e).into()))?;
    if status != 200 {
        let code = body.get("error").and_then(|v| v.as_str()).unwrap_or("unknown_error");
        tracing::info!(target: "overlook::signin", status, error = code, "token response: rejected");
        let description = body.get("error_description").and_then(|v| v.as_str()).unwrap_or("");
        let message = if description.is_empty() { code.to_owned() } else { format!("{code}: {description}") };
        return Err(if matches!(code, "invalid_grant" | "invalid_client" | "unauthorized_client" | "access_denied") {
            AuthError::Denied(message)
        } else {
            AuthError::Protocol(message)
        });
    }
    let access_token = body
        .get("access_token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AuthError::Protocol("no access_token".into()))?
        .to_owned();
    let expires_in = body.get("expires_in").and_then(|v| v.as_i64()).unwrap_or(3600);
    let refresh_token = body
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .map(str::to_owned)
        .or_else(|| previous_refresh.map(str::to_owned));
    // Credential-free record of the exchange: which fields arrived, never their values.
    tracing::info!(
        target: "overlook::signin",
        status,
        token_type = body.get("token_type").and_then(|v| v.as_str()).unwrap_or(""),
        expires_in,
        scope = body.get("scope").and_then(|v| v.as_str()).unwrap_or(""),
        access_token = "[REDACTED]",
        refresh_token = if body.get("refresh_token").is_some() { "[REDACTED, new]" } else if previous_refresh.is_some() { "[not returned, previous kept]" } else { "[absent]" },
        id_token = if body.get("id_token").is_some() { "[REDACTED]" } else { "[absent]" },
        "token response: accepted"
    );
    Ok(OAuthTokens {
        access_token,
        refresh_token,
        expires_at: overlook_core::time::now() + expires_in * 1000,
        scope: body.get("scope").and_then(|v| v.as_str()).map(str::to_owned),
        // A refresh keeps the original sign-in time (see `GmailClient::accept_refresh`).
        signed_in_at: overlook_core::time::now(),
    })
}

fn post_form(url: &str, body: &str) -> Result<ureq::http::Response<ureq::Body>, AuthError> {
    agent()
        .post(url)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .send(body.as_bytes())
        .map_err(|e| AuthError::Network(crate::secure_error::ureq("Google OAuth request", e).into()))
}

/// Exchanges an authorization code for tokens (blocking; run on a background thread).
pub fn exchange_code(config: &ClientConfig, code: &str, redirect_uri: &str, verifier: &str) -> Result<OAuthTokens, AuthError> {
    let mut body = token_request_body(&config.client_id, code, redirect_uri, verifier);
    log_token_request("authorization_code", config, &[("code", "[REDACTED]"), ("code_verifier", "[REDACTED]"), ("redirect_uri", redirect_uri)]);
    if let Some(secret) = &config.client_secret {
        body.push_str("&client_secret=");
        body.push_str(&url_encode(secret));
    }
    token_response(post_form(TOKEN_ENDPOINT, &body)?, None)
}

/// Refreshes an access token (blocking).
pub fn refresh(config: &ClientConfig, refresh_token: &str) -> Result<OAuthTokens, AuthError> {
    let mut body = [
        ("client_id", config.client_id.as_str()),
        ("refresh_token", refresh_token),
        ("grant_type", "refresh_token"),
    ]
    .iter()
    .map(|(k, v)| format!("{k}={}", url_encode(v)))
    .collect::<Vec<_>>()
    .join("&");
    log_token_request("refresh_token", config, &[("refresh_token", "[REDACTED]")]);
    if let Some(secret) = &config.client_secret {
        body.push_str("&client_secret=");
        body.push_str(&url_encode(secret));
    }
    token_response(post_form(TOKEN_ENDPOINT, &body)?, Some(refresh_token))
}

/// Records a token request's shape. Values that are credentials appear only as
/// `[REDACTED]`; the client id is the public installed-app identifier.
fn log_token_request(grant_type: &str, config: &ClientConfig, fields: &[(&str, &str)]) {
    let fields = fields.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(" ");
    let client_secret = if config.client_secret.is_some() { "[REDACTED]" } else { "[absent]" };
    tracing::info!(target: "overlook::signin", endpoint = TOKEN_ENDPOINT, grant_type, client_id = %config.client_id, client_secret, "token request: {fields}");
}

/// Revoke the Google grant before discarding the refresh token. No secret in the URL.
pub fn revoke(tokens: &OAuthTokens) -> Result<(), AuthError> {
    let token = tokens.refresh_token.as_deref().unwrap_or(&tokens.access_token);
    if token.is_empty() { return Ok(()); }
    let mut response = post_form("https://oauth2.googleapis.com/revoke", &format!("token={}", url_encode(token)))?;
    let status = response.status().as_u16();
    tracing::info!(target: "overlook::signin", endpoint = "https://oauth2.googleapis.com/revoke", token = "[REDACTED]", status, "token revocation");
    if status == 200 { return Ok(()); }
    let body: serde_json::Value = response.body_mut().read_json().unwrap_or_default();
    if status == 400 && body.get("error").and_then(|v| v.as_str()) == Some("invalid_token") { return Ok(()); }
    Err(AuthError::Protocol(format!("token revocation returned HTTP {status}; reconnect and retry account removal")))
}

/// Fetches the signed-in user's email and name (blocking).
pub fn fetch_userinfo(access_token: &str) -> Result<UserInfo, AuthError> {
    let mut response = agent()
        .get(USERINFO_ENDPOINT)
        .header("Authorization", &format!("Bearer {access_token}"))
        .call()
        .map_err(|e| AuthError::Network(crate::secure_error::ureq("Google OAuth request", e).into()))?;
    tracing::info!(target: "overlook::signin", endpoint = USERINFO_ENDPOINT, authorization = "Bearer [REDACTED]", status = response.status().as_u16(), "userinfo request");
    if response.status().as_u16() != 200 {
        return Err(AuthError::Protocol(format!("userinfo returned {}", response.status())));
    }
    let body: serde_json::Value = response.body_mut().read_json().map_err(|e| AuthError::Protocol(crate::secure_error::ureq("Google userinfo response", e).into()))?;
    let email = body
        .get("email")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AuthError::Protocol("userinfo had no email".into()))?
        .to_owned();
    Ok(UserInfo { email, name: body.get("name").and_then(|v| v.as_str()).map(str::to_owned) })
}

/// Scopes requested at sign-in. `gmail.modify` covers reading, labels, archive, trash, and
/// drafts; `gmail.send` covers sending. Both are restricted scopes and require Google's
/// verification before public release.
pub const SCOPES: &[&str] = &[
    "https://www.googleapis.com/auth/gmail.modify",
    "https://www.googleapis.com/auth/gmail.send",
    "https://www.googleapis.com/auth/calendar.events",
    "https://www.googleapis.com/auth/calendar.readonly",
    "https://www.googleapis.com/auth/contacts",
    "https://www.googleapis.com/auth/contacts.other.readonly",
    "https://www.googleapis.com/auth/gmail.settings.basic",
    "https://www.googleapis.com/auth/userinfo.email",
    "openid",
];

pub const CALENDAR_SCOPE_MARKER: &str = "auth/calendar";
pub const CONTACTS_SCOPE_MARKER: &str = "auth/contacts";
/// The read-write contacts scope, exactly; `contacts.readonly` and `contacts.other.readonly`
/// share the marker above.
pub const CONTACTS_WRITE_SCOPE: &str = "https://www.googleapis.com/auth/contacts";
pub const SETTINGS_SCOPE_MARKER: &str = "gmail.settings.basic";

pub const AUTH_ENDPOINT: &str = "https://accounts.google.com/o/oauth2/v2/auth";
pub const TOKEN_ENDPOINT: &str = "https://oauth2.googleapis.com/token";

/// PKCE verifier/challenge pair (RFC 7636, S256).
#[derive(Clone, PartialEq, Eq)]
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

impl std::fmt::Debug for Pkce {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pkce").field("verifier", &"[REDACTED]").field("challenge", &self.challenge).finish()
    }
}

impl Pkce {
    /// Builds a PKCE pair from fresh randomness.
    pub fn random() -> Self {
        let bytes: [u8; 48] = rand::random();
        Self::from_random(&bytes)
    }

    /// Random opaque `state` value for the authorization request.
    pub fn random_state() -> String {
        let bytes: [u8; 24] = rand::random();
        base64_url(&bytes)
    }

    /// Builds a PKCE pair from caller-supplied random bytes (32 to 96 bytes).
    pub fn from_random(random: &[u8]) -> Self {
        let verifier = base64_url(random);
        let challenge = base64_url(&sha256(verifier.as_bytes()));
        Self { verifier, challenge }
    }
}

/// Parameters of an authorization request to open in the system browser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizationRequest {
    pub client_id: String,
    /// Loopback redirect, e.g. `http://127.0.0.1:PORT/callback`. Google requires the
    /// loopback IP literal rather than `localhost` for installed apps.
    pub redirect_uri: String,
    pub state: String,
    pub pkce: Pkce,
    /// Optional account hint to preselect a Google account.
    pub login_hint: Option<String>,
}

impl AuthorizationRequest {
    pub fn url(&self) -> String {
        let mut params: BTreeMap<&str, String> = BTreeMap::new();
        params.insert("client_id", self.client_id.clone());
        params.insert("redirect_uri", self.redirect_uri.clone());
        params.insert("response_type", "code".into());
        params.insert("scope", SCOPES.join(" "));
        params.insert("state", self.state.clone());
        params.insert("code_challenge", self.pkce.challenge.clone());
        params.insert("code_challenge_method", "S256".into());
        params.insert("access_type", "offline".into());
        params.insert("prompt", "consent".into());
        if let Some(hint) = &self.login_hint {
            params.insert("login_hint", hint.clone());
        }
        let query: Vec<String> = params.iter().map(|(k, v)| format!("{k}={}", url_encode(v))).collect();
        format!("{AUTH_ENDPOINT}?{}", query.join("&"))
    }

    /// Validates the loopback callback query and returns the authorization code.
    pub fn parse_callback(&self, query: &str) -> Result<String, CallbackError> {
        let mut code = None;
        let mut state = None;
        let mut error = None;
        for pair in query.trim_start_matches('?').split('&') {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            let v = url_decode(v);
            match url_decode(k).as_str() {
                "code" if code.is_none() => code = Some(v),
                "state" if state.is_none() => state = Some(v),
                "error" if error.is_none() => error = Some(v),
                "code" | "state" | "error" => return Err(CallbackError::Ambiguous),
                _ => {}
            }
        }
        if state.as_deref() != Some(self.state.as_str()) {
            return Err(CallbackError::StateMismatch);
        }
        if code.is_some() && error.is_some() {
            return Err(CallbackError::Ambiguous);
        }
        if error.is_some() {
            // Never reflect an untrusted callback value into UI or logs.
            return Err(CallbackError::Denied("access_denied".into()));
        }
        code.filter(|c| !c.is_empty()).ok_or(CallbackError::MissingCode)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CallbackError {
    #[error("the sign-in response contained ambiguous parameters")]
    Ambiguous,
    #[error("sign-in was cancelled or denied: {0}")]
    Denied(String),
    #[error("the sign-in response did not match this request")]
    StateMismatch,
    #[error("the sign-in response had no authorization code")]
    MissingCode,
}

/// Form body for exchanging the code for tokens.
pub fn token_request_body(client_id: &str, code: &str, redirect_uri: &str, verifier: &str) -> String {
    [
        ("client_id", client_id),
        ("code", code),
        ("code_verifier", verifier),
        ("grant_type", "authorization_code"),
        ("redirect_uri", redirect_uri),
    ]
    .iter()
    .map(|(k, v)| format!("{k}={}", url_encode(v)))
    .collect::<Vec<_>>()
    .join("&")
}

pub fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

pub fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = |b: u8| (b as char).to_digit(16).map(|v| v as u8);
                if let (Some(high), Some(low)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                    let v = high * 16 + low;
                    out.push(v);
                    i += 3;
                    continue;
                }
                out.push(b'%');
                i += 1;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn base64_url(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity((bytes.len() + 2) / 3 * 4);
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        if chunk.len() > 1 {
            out.push(TABLE[(n >> 6) as usize & 63] as char);
        }
        if chunk.len() > 2 {
            out.push(TABLE[n as usize & 63] as char);
        }
    }
    out
}

/// SHA-256 for the PKCE S256 challenge, from the RustCrypto `sha2` crate.
fn sha256(data: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    Sha256::digest(data).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone)]
    struct Capture(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
    impl std::io::Write for Capture {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> { self.0.lock().unwrap().extend_from_slice(bytes); Ok(bytes.len()) }
        fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
    }

    /// The token exchange is Overlook's credential-submission step (the password itself goes
    /// to Google in the browser). Its log lines must show the exchange without any value.
    #[test]
    fn token_exchange_logs_shape_without_credentials() {
        use tracing_subscriber::layer::SubscriberExt;
        let captured = Capture(Default::default());
        let writer = captured.clone();
        let subscriber = tracing_subscriber::registry().with(tracing_subscriber::fmt::layer().without_time().with_ansi(false).with_writer(move || writer.clone()));
        let config = ClientConfig { client_id: "synthetic-client.apps.googleusercontent.com".into(), client_secret: Some("synthetic-client-secret".into()) };
        tracing::subscriber::with_default(subscriber, || {
            log_token_request("authorization_code", &config, &[("code", "[REDACTED]"), ("code_verifier", "[REDACTED]"), ("redirect_uri", "http://127.0.0.1:43123/callback")]);
            let body = r#"{"access_token":"synthetic-access-secret","refresh_token":"synthetic-refresh-secret","id_token":"synthetic-id-secret","expires_in":3599,"token_type":"Bearer","scope":"https://mail.google.com/"}"#;
            let response = ureq::http::Response::builder().status(200).body(ureq::Body::builder().mime_type("application/json").data(body)).unwrap();
            let tokens = token_response(response, None).unwrap();
            assert_eq!(tokens.access_token, "synthetic-access-secret");
            let denied = ureq::http::Response::builder().status(400).body(ureq::Body::builder().mime_type("application/json").data(r#"{"error":"invalid_grant","error_description":"Bad Request"}"#)).unwrap();
            assert!(matches!(token_response(denied, Some("synthetic-refresh-secret")), Err(AuthError::Denied(_))));
        });
        let bytes = captured.0.lock().unwrap().clone();
        let log = String::from_utf8(bytes).unwrap();
        println!("{log}");
        assert!(log.contains("overlook::signin"));
        assert!(log.contains("grant_type=\"authorization_code\""), "{log}");
        assert!(log.contains("code=[REDACTED] code_verifier=[REDACTED]"));
        assert!(log.contains("token response: accepted") && log.contains("token response: rejected"));
        assert!(log.contains("error=\"invalid_grant\""));
        assert!(!log.contains("-secret"), "{log}");
    }

    #[test]
    fn pkce_matches_rfc_7636_appendix_b() {
        // RFC 7636 Appendix B verifier and expected S256 challenge.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let challenge = base64_url(&sha256(verifier.as_bytes()));
        assert_eq!(challenge, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }

    #[test]
    fn sha256_known_vector() {
        let digest = sha256(b"abc");
        assert_eq!(
            digest.iter().map(|b| format!("{b:02x}")).collect::<String>(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn parses_google_client_json() {
        let json = r#"{"installed":{"client_id":"123-abc.apps.googleusercontent.com","project_id":"overlook","client_secret":"GOCSPX-x","redirect_uris":["http://localhost"]}}"#;
        let cfg = ClientConfig::from_google_json(json).unwrap();
        assert_eq!(cfg.client_id, "123-abc.apps.googleusercontent.com");
        assert_eq!(cfg.client_secret.as_deref(), Some("GOCSPX-x"));
        assert!(ClientConfig::from_google_json("{}").is_err());
        assert!(!OAuthTokens { access_token: "a".into(), refresh_token: None, expires_at: 10_000_000, scope: None, signed_in_at: 0 }.is_expired(1_000_000));
        assert!(OAuthTokens { access_token: "a".into(), refresh_token: None, expires_at: 1_050_000, scope: None, signed_in_at: 0 }.is_expired(1_000_000));
    }

    #[test]
    fn authorization_url_and_callback() {
        let req = AuthorizationRequest {
            client_id: "client.apps.googleusercontent.com".into(),
            redirect_uri: "http://127.0.0.1:43123/callback".into(),
            state: "s t@te".into(),
            pkce: Pkce::from_random(&[7u8; 32]),
            login_hint: None,
        };
        let url = req.url();
        assert!(url.starts_with(AUTH_ENDPOINT));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A43123%2Fcallback"));
        assert!(url.contains("scope=https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fgmail.modify%20"));
        assert_eq!(req.parse_callback("?code=4%2Fabc&state=s%20t%40te"), Ok("4/abc".into()));
        assert_eq!(req.parse_callback("code=x&state=other"), Err(CallbackError::StateMismatch));
        assert_eq!(req.parse_callback("error=access_denied&state=s%20t%40te"), Err(CallbackError::Denied("access_denied".into())));
        for query in [
            "code=x&state=s%20t%40te&state=other",
            "code=x&state=other&state=s%20t%40te",
            "code=x&code=y&state=s%20t%40te",
            "code=x&%63ode=y&state=s%20t%40te",
            "code=x&error=denied&state=s%20t%40te",
            "error=a&error=b&state=s%20t%40te",
        ] {
            assert_eq!(req.parse_callback(query), Err(CallbackError::Ambiguous));
        }
        assert_eq!(req.parse_callback("error=untrusted-secret"), Err(CallbackError::StateMismatch));
        assert_eq!(url_decode("%é"), "%é");
    }

    #[test]
    fn secret_debug_output_is_redacted() {
        let tokens = OAuthTokens { access_token: "synthetic-access-secret".into(), refresh_token: Some("synthetic-refresh-secret".into()), expires_at: 123, scope: None, signed_in_at: 0 };
        let config = ClientConfig { client_id: "public-client".into(), client_secret: Some("synthetic-client-secret".into()) };
        let pkce = Pkce::from_random(&[7; 48]);
        let output = format!("{tokens:?} {config:?} {pkce:?}");
        for secret in [tokens.access_token.as_str(), tokens.refresh_token.as_deref().unwrap(), config.client_secret.as_deref().unwrap(), &pkce.verifier] {
            assert!(!output.contains(secret));
        }
        assert!(output.contains("[REDACTED]"));
    }
}
