//! Provider-neutral OAuth 2.0 for installed apps: stored tokens, PKCE (RFC 7636), the
//! loopback redirect's query, form encoding, session limits, and the SASL `XOAUTH2`
//! initial response the mail protocols sign in with.
//!
//! Google's sign-in ([`crate::gmail::oauth`]) and Microsoft's ([`crate::microsoft::oauth`])
//! build their authorization requests and token exchanges on these. `gmail::oauth` and
//! `gmail::api` re-export the items they used to define, so their paths still work.

use serde::{Deserialize, Serialize};

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

impl std::fmt::Debug for OAuthTokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuthTokens")
            .field("access_token", &"[REDACTED]")
            .field("refresh_token", &"[REDACTED]")
            .field("expires_at", &self.expires_at)
            .field("scope", &self.scope)
            .finish()
    }
}

impl OAuthTokens {
    /// Treats the token as expired one minute early to avoid racing the deadline.
    pub fn is_expired(&self, now_ms: i64) -> bool {
        now_ms + 60_000 >= self.expires_at
    }
}

/// An OAuth session that has not refreshed its access token for this long is signed out:
/// the refresh token is deleted locally (and revoked where the issuer offers revocation).
/// Shorter than Google's own six-month expiry for unused refresh tokens, and the same as
/// Microsoft's 90-day inactivity limit.
pub const SESSION_IDLE_LIMIT_MS: i64 = 90 * 24 * 60 * 60 * 1000;

/// However active, an OAuth session ends this long after its sign-in: the refresh token is
/// deleted (revoked where possible) and the user signs in again.
pub const SESSION_MAX_AGE_MS: i64 = 180 * 24 * 60 * 60 * 1000;

/// Why a session has ended on its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionExpiry {
    /// Unused for [`SESSION_IDLE_LIMIT_MS`].
    Idle,
    /// Older than [`SESSION_MAX_AGE_MS`].
    MaxAge,
}

/// See [`SESSION_IDLE_LIMIT_MS`]. A zero `expires_at` means no session at all.
pub fn session_idle_expired(expires_at: i64, now: i64) -> bool {
    expires_at > 0 && now - expires_at > SESSION_IDLE_LIMIT_MS
}

/// See [`SessionExpiry`]. The absolute limit is checked first. Every refresh moves
/// `expires_at` forward, so its age measures the time since the session was last used.
pub fn session_expiry(tokens: &OAuthTokens, now: i64) -> Option<SessionExpiry> {
    if tokens.signed_in_at > 0 && now - tokens.signed_in_at > SESSION_MAX_AGE_MS {
        Some(SessionExpiry::MaxAge)
    } else if session_idle_expired(tokens.expires_at, now) {
        Some(SessionExpiry::Idle)
    } else {
        None
    }
}

/// PKCE verifier/challenge pair (RFC 7636, S256).
#[derive(Clone, PartialEq, Eq)]
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

impl std::fmt::Debug for Pkce {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pkce")
            .field("verifier", &"[REDACTED]")
            .field("challenge", &self.challenge)
            .finish()
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
        Self {
            verifier,
            challenge,
        }
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

/// Validates the loopback redirect's query against the `state` the authorization request
/// sent, and returns the authorization code. A repeated `code`, `state` or `error`, a code
/// beside an error, or a foreign state is refused, so a forged or polluted redirect cannot
/// smuggle in another code.
pub fn parse_callback(query: &str, expected_state: &str) -> Result<String, CallbackError> {
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
    if state.as_deref() != Some(expected_state) {
        return Err(CallbackError::StateMismatch);
    }
    if code.is_some() && error.is_some() {
        return Err(CallbackError::Ambiguous);
    }
    if error.is_some() {
        // Never reflect an untrusted callback value into UI or logs.
        return Err(CallbackError::Denied("access_denied".into()));
    }
    code.filter(|c| !c.is_empty())
        .ok_or(CallbackError::MissingCode)
}

/// `application/x-www-form-urlencoded` body (also a query string) from name/value pairs.
pub fn form(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{k}={}", url_encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

pub fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
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
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
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
pub(crate) fn sha256(data: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    Sha256::digest(data).into()
}

/// The SASL `XOAUTH2` client response, before base64: `user=…^Aauth=Bearer …^A^A`. IMAP
/// (`AUTHENTICATE XOAUTH2`), POP3 (`AUTH XOAUTH2`) and SMTP (`AUTH XOAUTH2`) all send it.
pub fn xoauth2_response(user: &str, access_token: &str) -> String {
    format!("user={user}\x01auth=Bearer {access_token}\x01\x01")
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
            digest
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn callback_query_is_validated_against_the_state() {
        assert_eq!(
            parse_callback("?code=4%2Fabc&state=s%20t%40te", "s t@te"),
            Ok("4/abc".into())
        );
        assert_eq!(
            parse_callback("code=x&state=other", "s"),
            Err(CallbackError::StateMismatch)
        );
        assert_eq!(
            parse_callback("state=s", "s"),
            Err(CallbackError::MissingCode)
        );
        assert_eq!(
            parse_callback("error=access_denied&error_description=secret&state=s", "s"),
            Err(CallbackError::Denied("access_denied".into()))
        );
        assert_eq!(
            parse_callback("code=x&code=y&state=s", "s"),
            Err(CallbackError::Ambiguous)
        );
        assert_eq!(
            form(&[("a", "b c"), ("scope", "x/y offline_access")]),
            "a=b%20c&scope=x%2Fy%20offline_access"
        );
    }

    #[test]
    fn xoauth2_response_has_the_sasl_layout() {
        assert_eq!(
            xoauth2_response("jason@outlook.com", "tok"),
            "user=jason@outlook.com\x01auth=Bearer tok\x01\x01"
        );
    }

    #[test]
    fn sessions_expire_when_idle_or_old() {
        let day = 24 * 60 * 60 * 1000;
        let now = 1_000 * day;
        let tokens = |expires_at, signed_in_at| OAuthTokens {
            access_token: "a".into(),
            refresh_token: Some("r".into()),
            expires_at,
            scope: None,
            signed_in_at,
        };
        assert_eq!(
            session_expiry(&tokens(now - day, now - 10 * day), now),
            None
        );
        assert_eq!(
            session_expiry(&tokens(now - 91 * day, now - 100 * day), now),
            Some(SessionExpiry::Idle)
        );
        assert_eq!(
            session_expiry(&tokens(now, now - 181 * day), now),
            Some(SessionExpiry::MaxAge)
        );
        assert_eq!(
            session_expiry(&tokens(0, 0), now),
            None,
            "no session at all is not an expired one"
        );
    }
}
