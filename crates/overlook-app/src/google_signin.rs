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

pub fn client_config_path() -> PathBuf {
    if let Ok(dir) = std::env::var("OVERLOOK_CONFIG_DIR") {
        return PathBuf::from(dir).join("google-oauth-client.json");
    }
    // directories maps ("ai", "Last Refuge", "Overlook") to ~/.config/overlook on Linux.
    directories::ProjectDirs::from("ai", "Last Refuge", "Overlook")
        .map(|d| d.config_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from(".config/overlook"))
        .join("google-oauth-client.json")
}

/// Reads the OAuth client the user created in Google Cloud.
pub fn load_client_config() -> Result<ClientConfig, String> {
    let path = client_config_path();
    let text = std::fs::read_to_string(&path).map_err(|_| {
        format!(
            "No Google OAuth client found. Save the JSON file downloaded from Google Cloud as {}.",
            path.display()
        )
    })?;
    ClientConfig::from_google_json(&text).map_err(|e| format!("{} could not be read: {e}", path.display()))
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
    fn config_path_is_under_dot_config_overlook() {
        let path = client_config_path();
        assert!(path.ends_with(".config/overlook/google-oauth-client.json") || path.ends_with("overlook/google-oauth-client.json"), "{}", path.display());
    }
}
