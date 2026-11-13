//! A Microsoft account's sign-in, as the mail protocols use it: the token source behind the
//! account's IMAP or POP3 connections, its `IDLE` thread and its SMTP submissions.
//!
//! Access tokens last about an hour. Whoever needs one asks [`MicrosoftSession`], which
//! refreshes under a lock (Microsoft rotates the refresh token on every refresh, so two
//! threads must never refresh with the same one), hands every refreshed set to the token
//! sink for the keyring, and ends the session for good when Microsoft answers
//! `invalid_grant`. Microsoft offers no endpoint for revoking one refresh token, so signing
//! out only forgets the tokens here; people remove the app's access in their Microsoft
//! account settings.

use super::oauth::{self, AuthError, ClientConfig};
use crate::credential::AccessTokenSource;
use crate::oauth::{session_expiry, OAuthTokens, SessionExpiry};
use crate::ProviderError;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

/// Called with every refreshed token set, so the caller can persist it.
pub type TokenSink = Box<dyn Fn(&OAuthTokens) + Send + Sync>;
/// Called once when Microsoft rejects the refresh token, so the stored copy can be deleted.
pub type InvalidationSink = Box<dyn Fn() + Send + Sync>;
/// Performs one refresh: the network call in production, a script in tests.
pub type Refresher =
    Box<dyn Fn(&ClientConfig, &str, &str) -> Result<OAuthTokens, AuthError> + Send + Sync>;

pub struct MicrosoftSession {
    config: ClientConfig,
    /// The scopes the sign-in granted, asked for again on every refresh.
    scopes: String,
    tokens: Mutex<OAuthTokens>,
    /// Held for a refresh and for sign-out, so neither overlaps another.
    refresh_lock: Mutex<()>,
    on_tokens: TokenSink,
    on_invalidated: Option<InvalidationSink>,
    refresher: Refresher,
    active: AtomicBool,
}

impl std::fmt::Debug for MicrosoftSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MicrosoftSession")
            .field("config", &self.config)
            .field("scopes", &self.scopes)
            .field("tokens", &self.tokens())
            .field("active", &self.is_active())
            .finish()
    }
}

impl MicrosoftSession {
    /// `on_tokens` receives every refreshed token set so it can be persisted.
    pub fn new(
        config: ClientConfig,
        scopes: impl Into<String>,
        mut tokens: OAuthTokens,
        on_tokens: TokenSink,
    ) -> Self {
        if tokens.signed_in_at == 0 {
            tokens.signed_in_at = overlook_core::time::now();
        }
        Self {
            config,
            scopes: scopes.into(),
            tokens: Mutex::new(tokens),
            refresh_lock: Mutex::new(()),
            on_tokens,
            on_invalidated: None,
            refresher: Box::new(oauth::refresh),
            active: AtomicBool::new(true),
        }
    }

    pub fn with_on_invalidated(mut self, sink: InvalidationSink) -> Self {
        self.on_invalidated = Some(sink);
        self
    }

    /// Replaces the network refresh, for tests that script Microsoft's answers.
    pub fn with_refresher(mut self, refresher: Refresher) -> Self {
        self.refresher = refresher;
        self
    }

    pub fn tokens(&self) -> OAuthTokens {
        self.tokens
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::SeqCst)
    }

    /// Whether the session has ended on its own (see [`SessionExpiry`]).
    pub fn session_expiry(&self, now: i64) -> Option<SessionExpiry> {
        session_expiry(&self.tokens(), now)
    }

    /// Ends the session: no connection signs in with it again, including connections of
    /// cloned providers, and the tokens are dropped from memory. A refresh in flight finishes
    /// first. The caller deletes the stored copy.
    pub fn disconnect(&self) {
        self.active.store(false, Ordering::SeqCst);
        let _refreshing = self.refresh_lock.lock().unwrap_or_else(|p| p.into_inner());
        self.clear_tokens();
        tracing::info!(target: "overlook::signin", issuer = "microsoft", "session ended; tokens dropped from memory");
    }

    fn clear_tokens(&self) {
        let mut tokens = self.tokens.lock().unwrap_or_else(|p| p.into_inner());
        tokens.access_token.clear();
        tokens.refresh_token = None;
        tokens.expires_at = 0;
    }

    /// Ends a session Microsoft has already invalidated, and has the stored copy deleted.
    fn invalidate_locally(&self) {
        if !self.active.swap(false, Ordering::SeqCst) {
            return;
        }
        self.clear_tokens();
        tracing::info!(target: "overlook::signin", issuer = "microsoft", "Microsoft rejected the refresh token (invalid_grant); session ended and stored token deleted");
        if let Some(sink) = &self.on_invalidated {
            sink();
        }
    }
}

impl AccessTokenSource for MicrosoftSession {
    fn access_token(&self, rejected: Option<&str>) -> Result<String, ProviderError> {
        if !self.is_active() {
            return Err(ProviderError::SignInRequired);
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
        if !self.is_active() {
            return Err(ProviderError::SignInRequired);
        }
        // Another connection may have refreshed while this one waited for the lock.
        let current = self.tokens();
        if usable(&current) {
            return Ok(current.access_token);
        }
        let Some(refresh_token) = current.refresh_token.clone() else {
            return Err(ProviderError::SignInRequired);
        };
        match (self.refresher)(&self.config, &refresh_token, &self.scopes) {
            Ok(mut fresh) => {
                fresh.signed_in_at = current.signed_in_at;
                if fresh.refresh_token.is_none() {
                    fresh.refresh_token = Some(refresh_token);
                }
                (self.on_tokens)(&fresh);
                let token = fresh.access_token.clone();
                *self.tokens.lock().unwrap_or_else(|p| p.into_inner()) = fresh;
                Ok(token)
            }
            Err(e) if e.is_invalid_grant() => {
                self.invalidate_locally();
                Err(ProviderError::SignInRequired)
            }
            Err(AuthError::Denied(_)) => Err(ProviderError::SignInRequired),
            Err(AuthError::Network(_)) => Err(ProviderError::Offline),
            Err(AuthError::Protocol(m)) => Err(ProviderError::Temporary(m)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Arc;

    fn tokens(access: &str, refresh: &str, expires_at: i64) -> OAuthTokens {
        OAuthTokens {
            access_token: access.into(),
            refresh_token: Some(refresh.into()),
            expires_at,
            scope: None,
            signed_in_at: 1_000,
        }
    }

    fn config() -> ClientConfig {
        ClientConfig::new("synthetic-client", "common").unwrap()
    }

    /// A session whose refreshes hand out `access-N`/`refresh-N` and record what they were given.
    fn scripted(
        start: OAuthTokens,
        saved: Arc<Mutex<Vec<OAuthTokens>>>,
        refreshed_with: Arc<Mutex<Vec<String>>>,
    ) -> MicrosoftSession {
        let count = Arc::new(AtomicUsize::new(0));
        MicrosoftSession::new(
            config(),
            "scope-a scope-b",
            start,
            Box::new(move |t| saved.lock().unwrap().push(t.clone())),
        )
        .with_refresher(Box::new(move |_, refresh, scopes| {
            assert_eq!(
                scopes, "scope-a scope-b",
                "a refresh asks for the signed-in scopes"
            );
            refreshed_with.lock().unwrap().push(refresh.to_owned());
            let n = count.fetch_add(1, Ordering::SeqCst) + 1;
            std::thread::sleep(std::time::Duration::from_millis(20));
            Ok(OAuthTokens {
                access_token: format!("access-{n}"),
                refresh_token: Some(format!("refresh-{n}")),
                expires_at: overlook_core::time::now() + 3_600_000,
                scope: None,
                signed_in_at: overlook_core::time::now(),
            })
        }))
    }

    #[test]
    fn valid_tokens_are_reused_and_expired_ones_refreshed_once_across_threads() {
        let saved = Arc::new(Mutex::new(Vec::new()));
        let refreshed_with = Arc::new(Mutex::new(Vec::new()));
        let fresh = scripted(
            tokens(
                "access-0",
                "refresh-0",
                overlook_core::time::now() + 3_600_000,
            ),
            saved.clone(),
            refreshed_with.clone(),
        );
        assert_eq!(fresh.access_token(None), Ok("access-0".into()));
        assert!(
            refreshed_with.lock().unwrap().is_empty(),
            "a valid token needs no refresh"
        );

        let session = Arc::new(scripted(
            tokens("access-0", "refresh-0", 0),
            saved.clone(),
            refreshed_with.clone(),
        ));
        let threads: Vec<_> = (0..4)
            .map(|_| {
                let s = session.clone();
                std::thread::spawn(move || s.access_token(None))
            })
            .collect();
        for t in threads {
            assert_eq!(t.join().unwrap(), Ok("access-1".into()));
        }
        assert_eq!(
            *refreshed_with.lock().unwrap(),
            ["refresh-0"],
            "the sync, IDLE and SMTP threads share one refresh"
        );
        let stored = saved.lock().unwrap().last().cloned().unwrap();
        assert_eq!(
            (
                stored.access_token.as_str(),
                stored.refresh_token.as_deref(),
                stored.signed_in_at
            ),
            ("access-1", Some("refresh-1"), 1_000),
            "the rotated refresh token is persisted with the original sign-in time"
        );

        // A refused token is replaced even before it expires, with the rotated refresh token…
        assert_eq!(
            session.access_token(Some("access-1")),
            Ok("access-2".into())
        );
        assert_eq!(
            refreshed_with.lock().unwrap().last().map(String::as_str),
            Some("refresh-1")
        );
        // …but a refusal of a token another connection already replaced costs no refresh.
        assert_eq!(
            session.access_token(Some("access-1")),
            Ok("access-2".into())
        );
        assert_eq!(refreshed_with.lock().unwrap().len(), 2);
    }

    #[test]
    fn invalid_grant_ends_the_session_and_deletes_the_stored_copy() {
        let deleted = Arc::new(AtomicUsize::new(0));
        let counter = deleted.clone();
        let session = MicrosoftSession::new(
            config(),
            "s",
            tokens("a", "r", 0),
            Box::new(|_| panic!("nothing to persist")),
        )
        .with_on_invalidated(Box::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        }))
        .with_refresher(Box::new(|_, _, _| {
            Err(AuthError::Denied(
                "invalid_grant: AADSTS700082: expired".into(),
            ))
        }));
        assert_eq!(
            session.access_token(None),
            Err(ProviderError::SignInRequired)
        );
        assert_eq!(
            session.access_token(None),
            Err(ProviderError::SignInRequired)
        );
        assert_eq!(deleted.load(Ordering::SeqCst), 1);
        assert!(!session.is_active() && session.tokens().refresh_token.is_none());
    }

    #[test]
    fn other_failures_keep_the_session() {
        for (error, expected) in [
            (AuthError::Network("down".into()), ProviderError::Offline),
            (
                AuthError::Protocol("temporarily_unavailable".into()),
                ProviderError::Temporary("temporarily_unavailable".into()),
            ),
            (
                AuthError::Denied("interaction_required: AADSTS50076: MFA".into()),
                ProviderError::SignInRequired,
            ),
        ] {
            let session =
                MicrosoftSession::new(config(), "s", tokens("a", "r", 0), Box::new(|_| {}))
                    .with_refresher(Box::new(move |_, _, _| Err(error.clone())));
            assert_eq!(session.access_token(None), Err(expected));
            assert!(
                session.is_active() && session.tokens().refresh_token.is_some(),
                "only invalid_grant discards the refresh token"
            );
        }
    }

    #[test]
    fn disconnect_stops_every_user_and_redacts_debug_output() {
        let session = MicrosoftSession::new(
            config(),
            "s",
            tokens(
                "synthetic-access-secret",
                "synthetic-refresh-secret",
                i64::MAX,
            ),
            Box::new(|_| {}),
        );
        assert!(!format!("{session:?}").contains("-secret"));
        session.disconnect();
        assert_eq!(
            session.access_token(None),
            Err(ProviderError::SignInRequired)
        );
        assert!(session.tokens().access_token.is_empty());
        let day = 24 * 60 * 60 * 1000;
        let old = MicrosoftSession::new(
            config(),
            "s",
            OAuthTokens {
                signed_in_at: 1,
                ..tokens("a", "r", 200 * day)
            },
            Box::new(|_| {}),
        );
        assert_eq!(old.session_expiry(200 * day), Some(SessionExpiry::MaxAge));
    }
}
