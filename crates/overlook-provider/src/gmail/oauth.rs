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
    Ok(OAuthTokens {
        access_token,
        refresh_token,
        expires_at: overlook_core::time::now() + expires_in * 1000,
        scope: body.get("scope").and_then(|v| v.as_str()).map(str::to_owned),
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
    if let Some(secret) = &config.client_secret {
        body.push_str("&client_secret=");
        body.push_str(&url_encode(secret));
    }
    token_response(post_form(TOKEN_ENDPOINT, &body)?, Some(refresh_token))
}

/// Revoke the Google grant before discarding the refresh token. No secret in the URL.
pub fn revoke(tokens: &OAuthTokens) -> Result<(), AuthError> {
    let token = tokens.refresh_token.as_deref().unwrap_or(&tokens.access_token);
    if token.is_empty() { return Ok(()); }
    let mut response = post_form("https://oauth2.googleapis.com/revoke", &format!("token={}", url_encode(token)))?;
    let status = response.status().as_u16();
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

/// SHA-256 (FIPS 180-4). Small and dependency-free; PKCE only hashes one short string.
fn sha256(data: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
        0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
        0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
        0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
        0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
        0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
        0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
    ];
    let mut msg = data.to_vec();
    let bit_len = (data.len() as u64) * 8;
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());
    for block in msg.chunks(64) {
        let mut w = [0u32; 64];
        for (i, word) in block.chunks(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh.wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (slot, v) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *slot = slot.wrapping_add(v);
        }
    }
    let mut out = [0u8; 32];
    for (i, v) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&v.to_be_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(!OAuthTokens { access_token: "a".into(), refresh_token: None, expires_at: 10_000_000, scope: None }.is_expired(1_000_000));
        assert!(OAuthTokens { access_token: "a".into(), refresh_token: None, expires_at: 1_050_000, scope: None }.is_expired(1_000_000));
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
        let tokens = OAuthTokens { access_token: "synthetic-access-secret".into(), refresh_token: Some("synthetic-refresh-secret".into()), expires_at: 123, scope: None };
        let config = ClientConfig { client_id: "public-client".into(), client_secret: Some("synthetic-client-secret".into()) };
        let pkce = Pkce::from_random(&[7; 48]);
        let output = format!("{tokens:?} {config:?} {pkce:?}");
        for secret in [tokens.access_token.as_str(), tokens.refresh_token.as_deref().unwrap(), config.client_secret.as_deref().unwrap(), &pkce.verifier] {
            assert!(!output.contains(secret));
        }
        assert!(output.contains("[REDACTED]"));
    }
}
