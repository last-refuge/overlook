//! HTTP credentials are separate from the credential used to reach the profile owner.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{ensure, Context};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use url::Url;

pub(super) const METADATA_PATH: &str = "/.well-known/oauth-protected-resource/mcp";
const ACCESS_SCOPE: &str = "overlook:mcp";
const WRITE_SCOPE: &str = "overlook:mcp:write";

#[derive(Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Settings {
    Loopback {
        bearer_token: String,
    },
    TlsProxy {
        resource: String,
        issuer: String,
        introspection_url: String,
        introspection_bearer: String,
        subject: String,
    },
}

pub(super) enum Auth {
    Loopback([u8; 32]),
    OAuth(Box<OAuth>),
}

pub(super) struct OAuth {
    resource: String,
    issuer: String,
    introspection_url: String,
    introspection_bearer: String,
    subject: String,
    pub(super) host: String,
    metadata_url: String,
    agent: ureq::Agent,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Denial {
    Missing,
    Invalid,
    Scope,
    WriteScope,
    Unavailable,
}

pub(super) struct Access {
    pub(super) may_write: bool,
}

impl Auth {
    pub(super) fn new(settings: Settings, grant_token: &str) -> anyhow::Result<Self> {
        match settings {
            Settings::Loopback { bearer_token } => {
                ensure!(
                    is_local_secret(&bearer_token) && !same_secret(&bearer_token, grant_token),
                    "HTTP bearer_token must be a separate 32-byte unpadded base64url secret"
                );
                Ok(Self::Loopback(digest(&bearer_token)))
            }
            Settings::TlsProxy {
                resource,
                issuer,
                introspection_url,
                introspection_bearer,
                subject,
            } => {
                let url = https_url(&resource).context("Invalid OAuth resource")?;
                ensure!(
                    url.path() == "/mcp" && url.as_str() == resource,
                    "OAuth resource must be a canonical HTTPS URL ending in /mcp"
                );
                let origin = url.origin().ascii_serialization();
                let host = origin
                    .strip_prefix("https://")
                    .context("Invalid OAuth resource authority")?
                    .to_owned();
                https_url(&issuer).context("Invalid OAuth issuer")?;
                https_url(&introspection_url).context("Invalid OAuth introspection URL")?;
                ensure!(
                    valid_bearer(&introspection_bearer)
                        && !same_secret(&introspection_bearer, grant_token),
                    "Configure a separate OAuth introspection_bearer credential"
                );
                ensure!(
                    !subject.trim().is_empty() && !subject.chars().any(char::is_control),
                    "Configure an exact OAuth subject"
                );
                let agent = ureq::Agent::new_with_config(introspection_config().build());
                Ok(Self::OAuth(Box::new(OAuth {
                    resource,
                    issuer,
                    introspection_url,
                    introspection_bearer,
                    subject,
                    host,
                    metadata_url: format!("{origin}{METADATA_PATH}"),
                    agent,
                })))
            }
        }
    }

    pub(super) fn authenticate(&self, token: &str, grant_token: &str) -> Result<Access, Denial> {
        if same_secret(token, grant_token) {
            return Err(Denial::Invalid);
        }
        match self {
            Self::Loopback(expected) => {
                if bool::from(expected.ct_eq(&digest(token))) {
                    Ok(Access { may_write: true })
                } else {
                    Err(Denial::Invalid)
                }
            }
            Self::OAuth(oauth) => oauth.authenticate(token),
        }
    }

    pub(super) fn challenge(&self, reason: &Denial) -> String {
        let mut challenge = match self {
            Self::Loopback(_) => "Bearer realm=\"Overlook MCP\"".to_owned(),
            Self::OAuth(oauth) => {
                let scopes = if *reason == Denial::WriteScope {
                    format!("{ACCESS_SCOPE} {WRITE_SCOPE}")
                } else {
                    ACCESS_SCOPE.to_owned()
                };
                format!(
                    "Bearer resource_metadata=\"{}\", scope=\"{scopes}\"",
                    oauth.metadata_url
                )
            }
        };
        match reason {
            Denial::Missing | Denial::Unavailable => {}
            Denial::Invalid => challenge.push_str(", error=\"invalid_token\""),
            Denial::Scope | Denial::WriteScope => {
                challenge.push_str(", error=\"insufficient_scope\"")
            }
        }
        challenge
    }
}

fn introspection_config() -> ureq::config::ConfigBuilder<ureq::typestate::AgentScope> {
    ureq::Agent::config_builder()
        .https_only(true)
        .proxy(None)
        .max_redirects(0)
        .max_response_header_size(16 * 1024)
        .timeout_global(Some(Duration::from_secs(5)))
}

impl OAuth {
    pub(super) fn metadata(&self) -> Value {
        json!({
            "resource": self.resource,
            "authorization_servers": [self.issuer],
            "scopes_supported": [ACCESS_SCOPE],
            "bearer_methods_supported": ["header"],
        })
    }

    fn authenticate(&self, token: &str) -> Result<Access, Denial> {
        if same_secret(token, &self.introspection_bearer) {
            return Err(Denial::Invalid);
        }
        // The configured, certificate-validated server determines token validity.
        // Disable redirects and ambient proxies to keep both credentials at that server.
        let mut response = self
            .agent
            .post(&self.introspection_url)
            .header(
                "Authorization",
                format!("Bearer {}", self.introspection_bearer),
            )
            .header("Accept", "application/json")
            .send_form([("token", token), ("token_type_hint", "access_token")])
            .map_err(|_| Denial::Unavailable)?;
        if response.status() != 200 {
            return Err(Denial::Unavailable);
        }
        let claims: Claims = response
            .body_mut()
            .with_config()
            .limit(16 * 1024)
            .read_json()
            .map_err(|_| Denial::Unavailable)?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| Denial::Unavailable)?
            .as_secs();
        self.authorize(claims, now)
    }

    fn authorize(&self, claims: Claims, now: u64) -> Result<Access, Denial> {
        if !claims.active
            || claims.iss.as_deref() != Some(&self.issuer)
            || !claims.aud.is_some_and(|aud| aud.matches(&self.resource))
            || claims.sub.as_deref() != Some(&self.subject)
            || claims.exp.is_none_or(|exp| exp <= now)
            || claims.nbf.is_some_and(|nbf| nbf > now)
            || claims
                .token_type
                .is_some_and(|kind| !kind.eq_ignore_ascii_case("Bearer"))
            || !claims.cnf.is_null()
        {
            return Err(Denial::Invalid);
        }
        let scopes = claims.scope.as_deref().unwrap_or_default();
        if !scopes.split(' ').any(|scope| scope == ACCESS_SCOPE) {
            return Err(Denial::Scope);
        }
        Ok(Access {
            may_write: scopes.split(' ').any(|scope| scope == WRITE_SCOPE),
        })
    }
}

#[derive(Deserialize)]
struct Claims {
    active: bool,
    iss: Option<String>,
    aud: Option<Audience>,
    sub: Option<String>,
    exp: Option<u64>,
    nbf: Option<u64>,
    token_type: Option<String>,
    scope: Option<String>,
    #[serde(default)]
    cnf: Value,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Audience {
    One(String),
    List(Vec<String>),
}

impl Audience {
    fn matches(self, resource: &str) -> bool {
        match self {
            Self::One(audience) => audience == resource,
            Self::List(audiences) => audiences.len() == 1 && audiences[0] == resource,
        }
    }
}

pub(super) fn is_local_secret(secret: &str) -> bool {
    let mut bytes = [0; 32];
    URL_SAFE_NO_PAD.decode_slice(secret, &mut bytes) == Ok(32)
}

pub(super) fn valid_bearer(token: &str) -> bool {
    let core = token.trim_end_matches('=');
    !core.is_empty()
        && token.len() <= 8192
        && core
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-._~+/".contains(&byte))
}

fn digest(secret: &str) -> [u8; 32] {
    Sha256::digest(secret.as_bytes()).into()
}

fn same_secret(left: &str, right: &str) -> bool {
    bool::from(digest(left).ct_eq(&digest(right)))
}

fn https_url(value: &str) -> anyhow::Result<Url> {
    // Avoid parser diagnostics containing the input and reject ambiguous URL forms.
    let url = Url::parse(value).ok().context("Expected an HTTPS URL")?;
    ensure!(
        value.starts_with("https://")
            && !value
                .bytes()
                .any(|byte| byte.is_ascii_control() || byte == b' ')
            && url.scheme() == "https"
            && url.host().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "Expected an HTTPS URL without credentials, query or fragment"
    );
    Ok(url)
}

#[cfg(test)]
mod tests;
