//! Google sign-in for installed apps: system browser, PKCE, loopback redirect.
//!
//! The UI starts a [`PendingSignIn`], opens its URL in the system browser, and waits on a
//! background thread for the browser to hit the loopback listener ([`crate::loopback`]),
//! which answers with a small branded page and hands the authorization code back for the
//! token exchange.

use crate::loopback::{self, Redirect};
use overlook_provider::gmail::oauth::{exchange_code, fetch_userinfo, AuthorizationRequest, ClientConfig, Pkce, UserInfo};
use overlook_provider::gmail::OAuthTokens;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::Receiver;
use std::sync::Arc;

pub const CONFIG_FILE: &str = "google-oauth-client.json";

/// Where a Google OAuth client of the user's own goes, to use instead of the built-in one.
pub fn client_config_path() -> PathBuf {
    if let Ok(dir) = std::env::var("OVERLOOK_CONFIG_DIR") {
        return PathBuf::from(dir).join(CONFIG_FILE);
    }
    // directories maps ("ai", "Last Refuge", "Overlook") to ~/.config/overlook on Linux.
    directories::ProjectDirs::from("ai", "Last Refuge", "Overlook")
        .map(|d| d.config_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from(".config/overlook"))
        .join(CONFIG_FILE)
}

/// The client to sign in with: the user's own when they saved one, else the one built into
/// Overlook. Tokens refresh with the client that issued them, so someone who removes their
/// file signs in again.
pub fn load_client_config() -> Result<ClientConfig, String> {
    load_client_config_from(&client_config_path())
}

/// Whether sign-in uses the client compiled into Overlook rather than a file.
pub fn uses_built_in_client() -> bool {
    ClientConfig::built_in().is_some() && !client_config_path().exists()
}

fn load_client_config_from(path: &std::path::Path) -> Result<ClientConfig, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => ClientConfig::from_google_json(&text).map_err(|e| format!("{} could not be read: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => ClientConfig::built_in().ok_or_else(|| {
            format!(
                "This build of Overlook has no Google sign-in built in. Save the OAuth client JSON from your own Google Cloud project as {} (see “Connect Gmail” in the README).",
                path.display()
            )
        }),
        Err(_) => Err(format!("{} could not be read.", path.display())),
    }
}

pub struct SignInResult {
    pub tokens: OAuthTokens,
    pub user: UserInfo,
}

pub struct PendingSignIn {
    pub url: String,
    request: AuthorizationRequest,
    config: ClientConfig,
    cancel: Arc<AtomicBool>,
    receiver: Receiver<Result<String, String>>,
}

impl PendingSignIn {
    /// Binds the loopback listener and builds the authorization URL.
    pub fn start(config: ClientConfig, login_hint: Option<String>) -> Result<Self, String> {
        let (listeners, port) = loopback::bind(false)?;
        let redirect_uri = format!("http://127.0.0.1:{port}/callback");
        let request = AuthorizationRequest {
            client_id: config.client_id.clone(),
            redirect_uri,
            state: Pkce::random_state(),
            pkce: Pkce::random(),
            login_hint,
        };
        let url = request.url();
        tracing::info!(target: "overlook::signin", authorization_endpoint = "https://accounts.google.com/o/oauth2/v2/auth", redirect_uri = %request.redirect_uri, code_challenge_method = "S256", code_challenge = "[REDACTED]", state = "[REDACTED]", "sign-in started: Google consent opens in the system browser");
        let cancel = Arc::new(AtomicBool::new(false));
        let redirect = Redirect { authority: format!("127.0.0.1:{port}"), path: "/callback", state: request.state.clone() };
        let receiver = loopback::wait_for_redirect(listeners, redirect, cancel.clone())?;
        Ok(Self { url, request, config, cancel, receiver })
    }

    pub fn cancel_handle(&self) -> Arc<AtomicBool> {
        self.cancel.clone()
    }

    /// Waits for the browser callback, then exchanges the code and reads the user's email.
    /// Blocking; run on a background thread.
    pub fn wait(self) -> Result<SignInResult, String> {
        let query = self.receiver.recv().map_err(|_| "the sign-in listener stopped unexpectedly".to_owned())??;
        let code = self.request.parse_callback(&query).map_err(|e| e.to_string())?;
        let tokens = exchange_code(&self.config, &code, &self.request.redirect_uri, &self.request.pkce.verifier).map_err(|e| e.to_string())?;
        if tokens.refresh_token.is_none() {
            return Err("Google did not return a refresh token. Remove Overlook from your Google account's third-party access and sign in again.".into());
        }
        let user = fetch_userinfo(&tokens.access_token).map_err(|e| e.to_string())?;
        tracing::info!(target: "overlook::signin", "Google account identified; credentials go to the system keyring next");
        Ok(SignInResult { tokens, user })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_path_uses_the_overlook_config_directory() {
        let path = client_config_path();
        if let Ok(dir) = std::env::var("OVERLOOK_CONFIG_DIR") {
            assert_eq!(path, PathBuf::from(dir).join("google-oauth-client.json"));
        } else {
            let expected = if cfg!(target_os = "macos") {
                "Library/Application Support/ai.Last-Refuge.Overlook/google-oauth-client.json"
            } else {
                "overlook/google-oauth-client.json"
            };
            assert!(path.ends_with(expected), "{}", path.display());
        }
    }

    #[test]
    fn a_saved_client_overrides_the_built_in_one() {
        let dir = std::env::temp_dir().join(format!("overlook-google-config-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(CONFIG_FILE);
        match ClientConfig::built_in() {
            Some(built_in) => assert_eq!(load_client_config_from(&path), Ok(built_in)),
            None => assert!(load_client_config_from(&path).unwrap_err().contains("no Google sign-in built in")),
        }
        std::fs::write(&path, r#"{"installed":{"client_id":"own-client.apps.googleusercontent.com","client_secret":"GOCSPX-own"}}"#).unwrap();
        let config = load_client_config_from(&path).unwrap();
        assert_eq!((config.client_id.as_str(), config.client_secret.as_deref()), ("own-client.apps.googleusercontent.com", Some("GOCSPX-own")));
        std::fs::write(&path, "{").unwrap();
        assert!(load_client_config_from(&path).unwrap_err().contains("could not be read"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
