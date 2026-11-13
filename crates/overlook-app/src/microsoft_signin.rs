//! Microsoft sign-in for installed apps: system browser, PKCE, loopback redirect to
//! `http://localhost:PORT` ([`crate::loopback`]).
//!
//! The app registration is Last Refuge's, compiled in ([`ClientConfig::built_in`]), unless
//! `~/.config/overlook/microsoft-oauth-client.json` names another:
//! `{"client_id": "…", "tenant": "common"}`. Either way there is no secret to keep.

use crate::loopback::{self, Redirect};
use overlook_core::ServerProtocol;
use overlook_provider::microsoft::oauth::{
    exchange_code, scopes, AuthorizationRequest, CallbackError, ClientConfig, IdClaims,
};
use overlook_provider::oauth::{OAuthTokens, Pkce};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::Receiver;
use std::sync::Arc;

pub const CONFIG_FILE: &str = "microsoft-oauth-client.json";

/// Shown when the browser comes back without a sign-in. The redirect's own error is never
/// shown: it is untrusted input.
pub const DECLINED_WORDING: &str = "Microsoft sign-in was cancelled. If your organization manages this account, an administrator may need to approve Overlook first.";

pub fn client_config_path() -> PathBuf {
    if let Ok(dir) = std::env::var("OVERLOOK_CONFIG_DIR") {
        return PathBuf::from(dir).join(CONFIG_FILE);
    }
    directories::ProjectDirs::from("ai", "Last Refuge", "Overlook")
        .map(|d| d.config_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from(".config/overlook"))
        .join(CONFIG_FILE)
}

/// The registration to sign in with: the file's when there is one, else the built-in one.
pub fn load_client_config() -> Result<ClientConfig, String> {
    load_client_config_from(&client_config_path())
}

fn load_client_config_from(path: &std::path::Path) -> Result<ClientConfig, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => ClientConfig::from_json(&text).map_err(|e| format!("{} could not be read: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => ClientConfig::built_in().ok_or_else(|| {
            format!("This build of Overlook has no Microsoft app registration. Save one as {} (see “Connect Microsoft” in the README).", path.display())
        }),
        Err(_) => Err(format!("{} could not be read.", path.display())),
    }
}

/// A completed sign-in, before anything is stored.
#[derive(Clone)]
pub struct SignInResult {
    pub tokens: OAuthTokens,
    pub claims: Option<IdClaims>,
    pub config: ClientConfig,
    /// The scopes granted, asked for again on every refresh.
    pub scopes: String,
}

pub struct PendingSignIn {
    pub url: String,
    request: AuthorizationRequest,
    cancel: Arc<AtomicBool>,
    receiver: Receiver<Result<String, String>>,
}

impl PendingSignIn {
    /// Binds the loopback listener and builds the authorization URL for an account that
    /// reads mail over `incoming`.
    pub fn start(
        config: ClientConfig,
        incoming: ServerProtocol,
        login_hint: Option<String>,
    ) -> Result<Self, String> {
        // The redirect goes to `localhost`, which a browser may reach on either address.
        let (listeners, port) = loopback::bind(true)?;
        let request = AuthorizationRequest {
            config,
            // Microsoft matches loopback redirects without the port; the registration lists
            // `http://localhost`.
            redirect_uri: format!("http://localhost:{port}"),
            state: Pkce::random_state(),
            pkce: Pkce::random(),
            login_hint,
            scopes: scopes(incoming),
        };
        let url = request.url();
        tracing::info!(target: "overlook::signin", issuer = "microsoft", authorization_endpoint = %request.config.authorize_endpoint(), redirect_uri = %request.redirect_uri, scope = %request.scopes, code_challenge_method = "S256", code_challenge = "[REDACTED]", state = "[REDACTED]", "sign-in started: Microsoft consent opens in the system browser");
        let cancel = Arc::new(AtomicBool::new(false));
        let redirect = Redirect {
            authority: format!("localhost:{port}"),
            path: "/",
            state: request.state.clone(),
        };
        let receiver = loopback::wait_for_redirect(listeners, redirect, cancel.clone())?;
        Ok(Self {
            url,
            request,
            cancel,
            receiver,
        })
    }

    pub fn cancel_handle(&self) -> Arc<AtomicBool> {
        self.cancel.clone()
    }

    /// Waits for the browser, then exchanges the code. Blocking; run on a background thread.
    pub fn wait(self) -> Result<SignInResult, String> {
        let query = self
            .receiver
            .recv()
            .map_err(|_| "the sign-in listener stopped unexpectedly".to_owned())??;
        let code = self.request.parse_callback(&query).map_err(|e| match e {
            CallbackError::Denied(_) => DECLINED_WORDING.to_owned(),
            other => other.to_string(),
        })?;
        let signed_in = exchange_code(
            &self.request.config,
            &code,
            &self.request.redirect_uri,
            &self.request.pkce.verifier,
            &self.request.scopes,
        )
        .map_err(|e| e.to_string())?;
        if signed_in.tokens.refresh_token.is_none() {
            return Err("Microsoft did not return a refresh token, so Overlook couldn’t stay signed in. Try signing in again.".into());
        }
        tracing::info!(target: "overlook::signin", issuer = "microsoft", personal = ?signed_in.claims.as_ref().and_then(IdClaims::is_personal), "Microsoft account signed in; credentials go to the system keyring next");
        Ok(SignInResult {
            tokens: signed_in.tokens,
            claims: signed_in.claims,
            config: self.request.config.clone(),
            scopes: self.request.scopes.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_file_overrides_the_built_in_registration() {
        assert!(
            client_config_path().ends_with("overlook/microsoft-oauth-client.json"),
            "{}",
            client_config_path().display()
        );
        let dir = std::env::temp_dir().join(format!("overlook-ms-config-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(CONFIG_FILE);
        match ClientConfig::built_in() {
            Some(built_in) => assert_eq!(load_client_config_from(&path), Ok(built_in)),
            None => assert!(load_client_config_from(&path)
                .unwrap_err()
                .contains("no Microsoft app registration")),
        }
        std::fs::write(
            &path,
            r#"{"client_id":"11111111-2222-3333-4444-555555555555","tenant":"organizations"}"#,
        )
        .unwrap();
        let config = load_client_config_from(&path).unwrap();
        assert_eq!(
            (config.client_id.as_str(), config.tenant.as_str()),
            ("11111111-2222-3333-4444-555555555555", "organizations")
        );
        std::fs::write(&path, "{").unwrap();
        assert!(load_client_config_from(&path)
            .unwrap_err()
            .contains("could not be read"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sign_in_url_targets_localhost_with_the_mail_scopes() {
        let pending = PendingSignIn::start(
            ClientConfig::new("synthetic-client", "common").unwrap(),
            ServerProtocol::Imap,
            Some("jason@outlook.com".into()),
        )
        .unwrap();
        assert!(pending.url.starts_with("https://login.microsoftonline.com/common/oauth2/v2.0/authorize?client_id=synthetic-client&"), "{}", pending.url);
        assert!(
            pending
                .url
                .contains("redirect_uri=http%3A%2F%2Flocalhost%3A")
                && pending.url.contains("IMAP.AccessAsUser.All")
                && pending.url.contains("login_hint=jason%40outlook.com")
        );
        pending
            .cancel_handle()
            .store(true, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(
            pending.wait().err().as_deref(),
            Some("Sign-in was cancelled.")
        );
    }
}
