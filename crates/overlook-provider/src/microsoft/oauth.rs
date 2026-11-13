//! The Microsoft identity platform (v2.0 endpoints) for a public desktop client: the
//! authorization code flow with PKCE and a loopback redirect, the token exchange, refreshes,
//! and the account details in the ID token.
//!
//! Overlook is registered with Microsoft Entra as a public client (platform "Mobile and
//! desktop applications", redirect URI `http://localhost`, personal and work or school
//! accounts). A public client has no secret: its id is public, as in every desktop mail
//! client, and PKCE protects the authorization code. Builds carry Last Refuge's registration
//! ([`BUILT_IN_CLIENT_ID`]); a `microsoft-oauth-client.json` file overrides it (see
//! [`ClientConfig::from_json`]).

use crate::oauth::{form, OAuthTokens, Pkce};
use overlook_core::ServerProtocol;
use std::time::Duration;

pub use crate::oauth::CallbackError;

/// Where every sign-in starts; the tenant follows it in the path.
pub const AUTHORITY: &str = "https://login.microsoftonline.com";

/// Reads mail over IMAP (Exchange Online resource, delegated).
pub const IMAP_SCOPE: &str = "https://outlook.office.com/IMAP.AccessAsUser.All";
/// Downloads mail over POP3 (Exchange Online resource, delegated).
pub const POP_SCOPE: &str = "https://outlook.office.com/POP.AccessAsUser.All";
/// Sends mail through SMTP AUTH (Exchange Online resource, delegated).
pub const SMTP_SCOPE: &str = "https://outlook.office.com/SMTP.Send";
/// A refresh token (`offline_access`) and an ID token naming the account and its tenant.
pub const SIGN_IN_SCOPES: &[&str] = &["offline_access", "openid", "profile", "email"];

/// Last Refuge's Entra registration, compiled into every build. `None` until the
/// registration exists; then its public application (client) id goes here.
const LAST_REFUGE_CLIENT_ID: Option<&str> = None;

/// The client id builds use when no `microsoft-oauth-client.json` overrides it. Packagers can
/// supply their own registration with `OVERLOOK_MICROSOFT_CLIENT_ID` at build time.
pub const BUILT_IN_CLIENT_ID: Option<&str> = match option_env!("OVERLOOK_MICROSOFT_CLIENT_ID") {
    Some(id) => Some(id),
    None => LAST_REFUGE_CLIENT_ID,
};

/// The tenant a sign-in goes through when the configuration names none: personal and work
/// or school accounts alike.
pub const DEFAULT_TENANT: &str = "common";

/// The scopes a sign-in asks for: reading mail over the account's incoming protocol, sending
/// through SMTP, and the sign-in scopes. Nothing beyond mail access.
pub fn scopes(incoming: ServerProtocol) -> String {
    let read = if incoming == ServerProtocol::Pop3 {
        POP_SCOPE
    } else {
        IMAP_SCOPE
    };
    [read, SMTP_SCOPE]
        .iter()
        .chain(SIGN_IN_SCOPES)
        .copied()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Overlook's Entra app registration: which client id signs in, through which tenant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientConfig {
    /// The registration's application (client) id. Public: no secret goes with it.
    pub client_id: String,
    /// `common`, `organizations`, `consumers`, or one directory's id or domain.
    pub tenant: String,
}

impl ClientConfig {
    /// Validates both parts, which end up in URLs.
    pub fn new(client_id: impl Into<String>, tenant: impl Into<String>) -> Result<Self, String> {
        let client_id = client_id.into().trim().to_owned();
        let tenant = tenant.into().trim().to_owned();
        let tenant = if tenant.is_empty() {
            DEFAULT_TENANT.to_owned()
        } else {
            tenant
        };
        let token = |s: &str, extra: &[char]| {
            !s.is_empty()
                && s.len() <= 100
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || extra.contains(&c))
        };
        if !token(&client_id, &['-']) {
            return Err(
                "the client_id must be the application (client) id from Microsoft Entra".into(),
            );
        }
        if !token(&tenant, &['-', '.']) {
            return Err(
                "the tenant must be common, organizations, consumers, or a directory id or domain"
                    .into(),
            );
        }
        Ok(Self { client_id, tenant })
    }

    /// Reads `{"client_id": "…", "tenant": "common"}`; `tenant` is optional.
    pub fn from_json(json: &str) -> Result<Self, String> {
        let value: serde_json::Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
        let client_id = value
            .get("client_id")
            .and_then(|v| v.as_str())
            .ok_or("the file has no client_id")?;
        let tenant = value
            .get("tenant")
            .and_then(|v| v.as_str())
            .unwrap_or(DEFAULT_TENANT);
        Self::new(client_id, tenant)
    }

    /// The registration compiled into this build, if there is one.
    pub fn built_in() -> Option<Self> {
        BUILT_IN_CLIENT_ID
            .filter(|id| !id.trim().is_empty())
            .and_then(|id| Self::new(id, DEFAULT_TENANT).ok())
    }

    pub fn authorize_endpoint(&self) -> String {
        format!("{AUTHORITY}/{}/oauth2/v2.0/authorize", self.tenant)
    }

    pub fn token_endpoint(&self) -> String {
        format!("{AUTHORITY}/{}/oauth2/v2.0/token", self.tenant)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AuthError {
    #[error("could not reach Microsoft: {0}")]
    Network(String),
    /// `invalid_grant`, `interaction_required` and friends: the user must sign in again.
    #[error("Microsoft rejected the sign-in: {0}")]
    Denied(String),
    #[error("unexpected response from Microsoft: {0}")]
    Protocol(String),
}

impl AuthError {
    /// The refresh token itself is dead (expired, revoked, password changed): keeping it
    /// can never work again.
    pub fn is_invalid_grant(&self) -> bool {
        matches!(self, AuthError::Denied(m) if m.starts_with("invalid_grant"))
    }
}

/// Parameters of an authorization request to open in the system browser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizationRequest {
    pub config: ClientConfig,
    /// Loopback redirect, `http://localhost:PORT`. Microsoft matches loopback redirects
    /// without their port, so the registration's `http://localhost` covers every port.
    pub redirect_uri: String,
    pub state: String,
    pub pkce: Pkce,
    /// The address being added or signed in again, to preselect that account.
    pub login_hint: Option<String>,
    /// From [`scopes`].
    pub scopes: String,
}

impl AuthorizationRequest {
    pub fn url(&self) -> String {
        let mut params = vec![
            ("client_id", self.config.client_id.as_str()),
            ("response_type", "code"),
            ("redirect_uri", self.redirect_uri.as_str()),
            ("response_mode", "query"),
            ("scope", self.scopes.as_str()),
            ("state", self.state.as_str()),
            ("code_challenge", self.pkce.challenge.as_str()),
            ("code_challenge_method", "S256"),
        ];
        if let Some(hint) = &self.login_hint {
            params.push(("login_hint", hint.as_str()));
        }
        format!("{}?{}", self.config.authorize_endpoint(), form(&params))
    }

    /// Validates the loopback callback query and returns the authorization code.
    pub fn parse_callback(&self, query: &str) -> Result<String, CallbackError> {
        crate::oauth::parse_callback(query, &self.state)
    }
}

/// What a completed sign-in returns.
#[derive(Clone, Debug)]
pub struct SignedIn {
    pub tokens: OAuthTokens,
    /// Read from the ID token; `None` when Microsoft sent none.
    pub claims: Option<IdClaims>,
}

/// The account details an ID token carries.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IdClaims {
    /// `tid`: the directory the account belongs to ([`super::CONSUMER_TENANT_ID`] for
    /// personal accounts).
    pub tenant_id: Option<String>,
    /// `preferred_username`: usually the sign-in address.
    pub username: Option<String>,
    /// `email`, when the account has one on record.
    pub email: Option<String>,
    /// `name`: the display name.
    pub name: Option<String>,
}

impl IdClaims {
    /// Reads the claims of an ID token that came straight from the token endpoint over TLS.
    /// OpenID Connect Core 1.0 §3.1.3.7 lets TLS server validation stand in for checking the
    /// signature there; never use this for a token from anywhere else.
    pub fn from_token_response(id_token: &str) -> Option<Self> {
        use base64::engine::{
            general_purpose::GeneralPurpose, general_purpose::GeneralPurposeConfig,
            DecodePaddingMode,
        };
        use base64::{alphabet, Engine};
        let payload = id_token.split('.').nth(1)?;
        let engine = GeneralPurpose::new(
            &alphabet::URL_SAFE,
            GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
        );
        let bytes = engine.decode(payload).ok()?;
        let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
        let text = |name: &str| {
            value
                .get(name)
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        };
        Some(Self {
            tenant_id: text("tid"),
            username: text("preferred_username"),
            email: text("email"),
            name: text("name"),
        })
    }

    /// The account's address: `email`, else a `preferred_username` that is an address.
    pub fn address(&self) -> Option<String> {
        self.email
            .clone()
            .or_else(|| self.username.clone())
            .filter(|a| a.contains('@'))
            .map(|a| a.to_lowercase())
    }

    /// Whether the signed-in account goes by one of `addresses` (its `email` or its
    /// `preferred_username`, ignoring case). `None` when the token names no one.
    pub fn names_any(&self, addresses: &[&str]) -> Option<bool> {
        let names: Vec<String> = [&self.email, &self.username]
            .into_iter()
            .flatten()
            .map(|n| n.trim().to_lowercase())
            .collect();
        if names.is_empty() {
            return None;
        }
        Some(
            addresses
                .iter()
                .map(|a| a.trim().to_lowercase())
                .any(|a| !a.is_empty() && names.contains(&a)),
        )
    }

    /// Whether this is a personal Microsoft account, when the tenant says so.
    pub fn is_personal(&self) -> Option<bool> {
        self.tenant_id
            .as_deref()
            .map(|t| t.eq_ignore_ascii_case(super::CONSUMER_TENANT_ID))
    }
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .https_only(true)
        .max_redirects(0)
        .timeout_global(Some(Duration::from_secs(30)))
        .build()
        .into()
}

fn post_form(url: &str, body: &str) -> Result<ureq::http::Response<ureq::Body>, AuthError> {
    agent()
        .post(url)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .send(body.as_bytes())
        .map_err(|e| {
            AuthError::Network(crate::secure_error::ureq("Microsoft sign-in request", e).into())
        })
}

/// The first line of Microsoft's `error_description` ("AADSTS65001: The user or
/// administrator has not consented…"), without the trace and correlation ids after it.
fn description_line(body: &serde_json::Value) -> String {
    let text = body
        .get("error_description")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    text.lines()
        .next()
        .unwrap_or("")
        .trim()
        .chars()
        .take(240)
        .collect()
}

/// Reads a token endpoint response. `previous_refresh` is kept when a refresh returns no new
/// refresh token (Microsoft normally rotates it).
fn token_response(
    mut response: ureq::http::Response<ureq::Body>,
    previous_refresh: Option<&str>,
) -> Result<(OAuthTokens, Option<String>), AuthError> {
    let status = response.status().as_u16();
    let body: serde_json::Value = response.body_mut().read_json().map_err(|e| {
        AuthError::Protocol(crate::secure_error::ureq("Microsoft token response", e).into())
    })?;
    if status != 200 {
        let code = body
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown_error");
        let description = description_line(&body);
        tracing::info!(target: "overlook::signin", issuer = "microsoft", status, error = code, "token response: rejected");
        let message = if description.is_empty() {
            code.to_owned()
        } else {
            format!("{code}: {description}")
        };
        return Err(
            if matches!(
                code,
                "invalid_grant"
                    | "invalid_client"
                    | "unauthorized_client"
                    | "access_denied"
                    | "interaction_required"
                    | "consent_required"
            ) {
                AuthError::Denied(message)
            } else {
                AuthError::Protocol(message)
            },
        );
    }
    let access_token = body
        .get("access_token")
        .and_then(|v| v.as_str())
        .filter(|t| !t.is_empty())
        .ok_or_else(|| AuthError::Protocol("no access_token".into()))?
        .to_owned();
    let expires_in = body
        .get("expires_in")
        .and_then(|v| {
            v.as_i64()
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        })
        .unwrap_or(3600);
    let rotated = body
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .filter(|t| !t.is_empty());
    let id_token = body
        .get("id_token")
        .and_then(|v| v.as_str())
        .map(str::to_owned);
    // Credential-free record of the exchange: which fields arrived, never their values.
    tracing::info!(
        target: "overlook::signin",
        issuer = "microsoft",
        status,
        token_type = body.get("token_type").and_then(|v| v.as_str()).unwrap_or(""),
        expires_in,
        scope = body.get("scope").and_then(|v| v.as_str()).unwrap_or(""),
        access_token = "[REDACTED]",
        refresh_token = if rotated.is_some() { "[REDACTED, new]" } else if previous_refresh.is_some() { "[not returned, previous kept]" } else { "[absent]" },
        id_token = if id_token.is_some() { "[REDACTED]" } else { "[absent]" },
        "token response: accepted"
    );
    let tokens = OAuthTokens {
        access_token,
        refresh_token: rotated
            .map(str::to_owned)
            .or_else(|| previous_refresh.map(str::to_owned)),
        expires_at: overlook_core::time::now() + expires_in * 1000,
        scope: body
            .get("scope")
            .and_then(|v| v.as_str())
            .map(str::to_owned),
        // A refresh keeps the original sign-in time (see `MicrosoftSession`).
        signed_in_at: overlook_core::time::now(),
    };
    Ok((tokens, id_token))
}

/// Records a token request's shape. Credentials appear only as `[REDACTED]`; the client id
/// is the public registration id.
fn log_token_request(grant_type: &str, config: &ClientConfig, fields: &[(&str, &str)]) {
    let fields = fields
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(" ");
    tracing::info!(target: "overlook::signin", issuer = "microsoft", endpoint = %config.token_endpoint(), grant_type, client_id = %config.client_id, "token request: {fields}");
}

/// Exchanges an authorization code for tokens (blocking; run on a background thread).
pub fn exchange_code(
    config: &ClientConfig,
    code: &str,
    redirect_uri: &str,
    verifier: &str,
    scopes: &str,
) -> Result<SignedIn, AuthError> {
    let body = form(&[
        ("client_id", config.client_id.as_str()),
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("code_verifier", verifier),
        ("scope", scopes),
    ]);
    log_token_request(
        "authorization_code",
        config,
        &[
            ("code", "[REDACTED]"),
            ("code_verifier", "[REDACTED]"),
            ("redirect_uri", redirect_uri),
        ],
    );
    let (tokens, id_token) = token_response(post_form(&config.token_endpoint(), &body)?, None)?;
    Ok(SignedIn {
        tokens,
        claims: id_token.as_deref().and_then(IdClaims::from_token_response),
    })
}

/// Refreshes an access token (blocking). Microsoft returns a new refresh token as well.
pub fn refresh(
    config: &ClientConfig,
    refresh_token: &str,
    scopes: &str,
) -> Result<OAuthTokens, AuthError> {
    let body = form(&[
        ("client_id", config.client_id.as_str()),
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("scope", scopes),
    ]);
    log_token_request("refresh_token", config, &[("refresh_token", "[REDACTED]")]);
    token_response(
        post_form(&config.token_endpoint(), &body)?,
        Some(refresh_token),
    )
    .map(|(tokens, _)| tokens)
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    #[derive(Clone)]
    struct Capture(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
    impl std::io::Write for Capture {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn json_response(status: u16, body: &str) -> ureq::http::Response<ureq::Body> {
        ureq::http::Response::builder()
            .status(status)
            .body(
                ureq::Body::builder()
                    .mime_type("application/json")
                    .data(body.to_owned()),
            )
            .unwrap()
    }

    pub(crate) fn id_token(claims: &str) -> String {
        let segment = |s: &str| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(s);
        format!(
            "{}.{}.{}",
            segment(r#"{"alg":"RS256","typ":"JWT"}"#),
            segment(claims),
            segment("synthetic-signature")
        )
    }

    #[test]
    fn client_configuration_is_validated_before_it_reaches_a_url() {
        let config =
            ClientConfig::from_json(r#"{"client_id":" 00000000-1111-2222-3333-444444444444 "}"#)
                .unwrap();
        assert_eq!(config.tenant, "common");
        assert_eq!(
            config.token_endpoint(),
            "https://login.microsoftonline.com/common/oauth2/v2.0/token"
        );
        let org =
            ClientConfig::from_json(r#"{"client_id":"abc","tenant":"contoso.onmicrosoft.com"}"#)
                .unwrap();
        assert_eq!(
            org.authorize_endpoint(),
            "https://login.microsoftonline.com/contoso.onmicrosoft.com/oauth2/v2.0/authorize"
        );
        assert!(ClientConfig::from_json("{}").is_err());
        assert!(
            ClientConfig::from_json(r#"{"client_id":"abc","tenant":"evil.example/../x?y="}"#)
                .is_err()
        );
        assert!(ClientConfig::from_json(r#"{"client_id":"a b"}"#).is_err());
        assert!(ClientConfig::new("", "common").is_err());
    }

    #[test]
    fn scopes_ask_for_mail_access_only() {
        assert_eq!(scopes(ServerProtocol::Imap), "https://outlook.office.com/IMAP.AccessAsUser.All https://outlook.office.com/SMTP.Send offline_access openid profile email");
        assert!(scopes(ServerProtocol::Pop3).starts_with(
            "https://outlook.office.com/POP.AccessAsUser.All https://outlook.office.com/SMTP.Send "
        ));
    }

    #[test]
    fn authorization_url_and_callback() {
        let request = AuthorizationRequest {
            config: ClientConfig::new("client-id", "common").unwrap(),
            redirect_uri: "http://localhost:43123".into(),
            state: "synthetic-state".into(),
            pkce: Pkce::from_random(&[7u8; 32]),
            login_hint: Some("jason@outlook.com".into()),
            scopes: scopes(ServerProtocol::Imap),
        };
        let url = request.url();
        assert!(url.starts_with("https://login.microsoftonline.com/common/oauth2/v2.0/authorize?client_id=client-id&response_type=code&"), "{url}");
        for part in ["redirect_uri=http%3A%2F%2Flocalhost%3A43123", "response_mode=query", "scope=https%3A%2F%2Foutlook.office.com%2FIMAP.AccessAsUser.All%20https%3A%2F%2Foutlook.office.com%2FSMTP.Send%20offline_access", "code_challenge_method=S256", "login_hint=jason%40outlook.com", "state=synthetic-state"] {
            assert!(url.contains(part), "{part} missing from {url}");
        }
        assert!(
            !url.contains(&request.pkce.verifier),
            "the verifier stays on this device"
        );
        assert_eq!(
            request.parse_callback("code=synthetic-code&state=synthetic-state&session_state=x"),
            Ok("synthetic-code".into())
        );
        assert_eq!(
            request.parse_callback(
                "error=consent_required&error_description=AADSTS65001&state=synthetic-state"
            ),
            Err(CallbackError::Denied("access_denied".into()))
        );
        assert_eq!(
            request.parse_callback("code=x&state=other"),
            Err(CallbackError::StateMismatch)
        );
    }

    #[test]
    fn id_token_claims_name_the_account_and_its_tenant() {
        let personal = IdClaims::from_token_response(&id_token(r#"{"tid":"9188040d-6c67-4c5b-b112-36a304b66dad","preferred_username":"Jason@Outlook.com","name":"Jason A"}"#)).unwrap();
        assert_eq!(personal.is_personal(), Some(true));
        assert_eq!(personal.address().as_deref(), Some("jason@outlook.com"));
        assert_eq!(personal.name.as_deref(), Some("Jason A"));
        let work = IdClaims::from_token_response(&id_token(r#"{"tid":"72f988bf-86f1-41af-91ab-2d7cd011db47","preferred_username":"jason@contoso.com","email":"j.alexander@contoso.com"}"#)).unwrap();
        assert_eq!(
            (work.is_personal(), work.address().as_deref()),
            (Some(false), Some("j.alexander@contoso.com"))
        );
        assert_eq!(
            work.names_any(&["JASON@contoso.com"]),
            Some(true),
            "the UPN counts as well as the email claim"
        );
        assert_eq!(
            work.names_any(&["j.alexander@contoso.com", "x@y"]),
            Some(true)
        );
        assert_eq!(work.names_any(&["jason@outlook.com", ""]), Some(false));
        assert_eq!(
            IdClaims::default().names_any(&["jason@outlook.com"]),
            None,
            "no names, no verdict"
        );
        let phone = IdClaims {
            username: Some("+15550100".into()),
            ..Default::default()
        };
        assert_eq!(
            phone.address(),
            None,
            "a phone-number sign-in is not an address"
        );
        assert_eq!(IdClaims::from_token_response("not-a-jwt"), None);
        assert_eq!(IdClaims::default().is_personal(), None);
    }

    #[test]
    fn token_responses_log_their_shape_without_credentials() {
        use tracing_subscriber::layer::SubscriberExt;
        let captured = Capture(Default::default());
        let writer = captured.clone();
        let subscriber = tracing_subscriber::registry().with(
            tracing_subscriber::fmt::layer()
                .without_time()
                .with_ansi(false)
                .with_writer(move || writer.clone()),
        );
        let config = ClientConfig::new("synthetic-client", "common").unwrap();
        tracing::subscriber::with_default(subscriber, || {
            log_token_request(
                "authorization_code",
                &config,
                &[
                    ("code", "[REDACTED]"),
                    ("code_verifier", "[REDACTED]"),
                    ("redirect_uri", "http://localhost:43123"),
                ],
            );
            let body = format!(
                r#"{{"token_type":"Bearer","scope":"https://outlook.office.com/IMAP.AccessAsUser.All","expires_in":3599,"access_token":"synthetic-access-secret","refresh_token":"synthetic-refresh-secret","id_token":"{}"}}"#,
                id_token(r#"{"tid":"t"}"#)
            );
            let (tokens, id) = token_response(json_response(200, &body), None).unwrap();
            assert_eq!(
                (
                    tokens.access_token.as_str(),
                    tokens.refresh_token.as_deref()
                ),
                ("synthetic-access-secret", Some("synthetic-refresh-secret"))
            );
            assert!(id.is_some() && tokens.expires_at > overlook_core::time::now());
            // A refresh without a new refresh token keeps the old one.
            let (refreshed, _) = token_response(
                json_response(
                    200,
                    r#"{"access_token":"synthetic-access-secret-2","expires_in":"3600"}"#,
                ),
                Some("synthetic-refresh-secret"),
            )
            .unwrap();
            assert_eq!(
                refreshed.refresh_token.as_deref(),
                Some("synthetic-refresh-secret")
            );
            let denied = token_response(json_response(400, r#"{"error":"invalid_grant","error_description":"AADSTS700082: The refresh token has expired due to inactivity.\r\nTrace ID: synthetic-trace\r\nCorrelation ID: c"}"#), Some("synthetic-refresh-secret")).unwrap_err();
            assert_eq!(
                denied,
                AuthError::Denied(
                    "invalid_grant: AADSTS700082: The refresh token has expired due to inactivity."
                        .into()
                )
            );
            assert!(denied.is_invalid_grant());
            let busy = token_response(
                json_response(503, r#"{"error":"temporarily_unavailable"}"#),
                None,
            )
            .unwrap_err();
            assert!(matches!(busy, AuthError::Protocol(_)) && !busy.is_invalid_grant());
        });
        let log = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
        assert!(
            log.contains("overlook::signin") && log.contains("issuer=\"microsoft\""),
            "{log}"
        );
        assert!(log.contains("code=[REDACTED] code_verifier=[REDACTED]"));
        assert!(
            log.contains("token response: accepted") && log.contains("token response: rejected")
        );
        assert!(
            !log.contains("-secret") && !log.contains("synthetic-trace"),
            "{log}"
        );
    }
}
