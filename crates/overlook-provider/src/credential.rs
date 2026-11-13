//! What an IMAP, POP3 or SMTP connection signs in with: a password, or an OAuth access
//! token presented with SASL `XOAUTH2`.
//!
//! An OAuth credential holds a shared [`AccessTokenSource`] rather than a token, because
//! access tokens last about an hour: the sync connection, the `IDLE` thread and each SMTP
//! submission ask the source for a current one whenever they sign in, and the source
//! refreshes and persists it as needed.

use crate::ProviderError;
use std::sync::Arc;

/// Supplies current OAuth access tokens. Implementations refresh expired tokens, persist
/// what they refresh, and serialize refreshes across threads.
pub trait AccessTokenSource: Send + Sync {
    /// A token to sign in with: the current one while it is valid, otherwise a refreshed one.
    /// `rejected` is a token the server just refused; it is refreshed even when it has not
    /// expired yet, unless another connection already replaced it. A refresh token that no
    /// longer works is [`ProviderError::SignInRequired`].
    fn access_token(&self, rejected: Option<&str>) -> Result<String, ProviderError>;
}

/// The secret one account's connections present.
#[derive(Clone)]
pub enum Credential {
    /// `LOGIN`, `AUTH PLAIN`/`LOGIN` or POP3 `USER`/`PASS`.
    Password(String),
    /// SASL `XOAUTH2` with a token from the account's OAuth session.
    OAuth(Arc<dyn AccessTokenSource>),
}

impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Credential::Password(_) => f.write_str("Credential::Password([REDACTED])"),
            Credential::OAuth(_) => f.write_str("Credential::OAuth([token source])"),
        }
    }
}

impl From<String> for Credential {
    fn from(password: String) -> Self {
        Credential::Password(password)
    }
}

impl From<&str> for Credential {
    fn from(password: &str) -> Self {
        Credential::Password(password.to_owned())
    }
}

impl From<&String> for Credential {
    fn from(password: &String) -> Self {
        Credential::Password(password.clone())
    }
}

impl Credential {
    pub fn is_oauth(&self) -> bool {
        matches!(self, Credential::OAuth(_))
    }

    /// Signs in with `attempt`, which is given the password or a current access token. When
    /// the server refuses an access token, the attempt runs once more with a refreshed one:
    /// the token may have been revoked before it expired. A refused password is final.
    pub(crate) fn sign_in<T>(
        &self,
        mut attempt: impl FnMut(Secret<'_>) -> Result<T, ProviderError>,
    ) -> Result<T, ProviderError> {
        match self {
            Credential::Password(password) => attempt(Secret::Password(password)),
            Credential::OAuth(source) => {
                let token = source.access_token(None)?;
                match attempt(Secret::AccessToken(&token)) {
                    Err(ProviderError::SignInRequired) => {
                        tracing::info!(target: "overlook::signin", access_token = "[REDACTED]", "access token refused; retrying once with a refreshed token");
                        let fresh = source.access_token(Some(&token))?;
                        attempt(Secret::AccessToken(&fresh))
                    }
                    other => other,
                }
            }
        }
    }
}

/// What one sign-in attempt presents.
#[derive(Clone, Copy)]
pub(crate) enum Secret<'a> {
    Password(&'a str),
    AccessToken(&'a str),
}

impl Secret<'_> {
    /// The SASL mechanism or command, for credential-free sign-in log lines.
    pub(crate) fn mechanism(self, password_mechanism: &'static str) -> &'static str {
        match self {
            Secret::Password(_) => password_mechanism,
            Secret::AccessToken(_) => "XOAUTH2",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Hands out `tok-1`, then a new token per refresh, and records each request.
    struct Counter(Mutex<Vec<Option<String>>>);
    impl AccessTokenSource for Counter {
        fn access_token(&self, rejected: Option<&str>) -> Result<String, ProviderError> {
            let mut calls = self.0.lock().unwrap();
            calls.push(rejected.map(str::to_owned));
            Ok(format!("tok-{}", calls.len()))
        }
    }

    #[test]
    fn a_refused_token_is_refreshed_once_and_a_password_is_not_retried() {
        let source = Arc::new(Counter(Mutex::new(Vec::new())));
        let credential = Credential::OAuth(source.clone());
        let mut seen = Vec::new();
        let result: Result<(), _> = credential.sign_in(|secret| {
            let Secret::AccessToken(token) = secret else {
                panic!("an OAuth credential presents tokens")
            };
            seen.push(token.to_owned());
            Err(ProviderError::SignInRequired)
        });
        assert_eq!(result, Err(ProviderError::SignInRequired));
        assert_eq!(seen, ["tok-1", "tok-2"]);
        assert_eq!(
            *source.0.lock().unwrap(),
            [None, Some("tok-1".to_owned())],
            "the refusal names the token to replace"
        );

        let mut attempts = 0;
        let password: Result<(), _> = Credential::from("pw").sign_in(|_| {
            attempts += 1;
            Err(ProviderError::SignInRequired)
        });
        assert_eq!(
            (password, attempts),
            (Err(ProviderError::SignInRequired), 1)
        );
        let offline: Result<(), _> = credential.sign_in(|_| Err(ProviderError::Offline));
        assert_eq!(
            offline,
            Err(ProviderError::Offline),
            "only a refusal is retried"
        );
        assert!(!format!("{:?}", Credential::from("hunter2")).contains("hunter2"));
    }
}
