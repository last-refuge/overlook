//! Secret storage in Apple Keychain on macOS and the system keyring on Linux.
//!
//! Secrets never touch the SQLite database, configuration files, or logs. When the keyring
//! is unavailable or locked the error is surfaced to the user instead of falling back to a
//! plain file.
//!
//! Every read, write and delete is recorded under the `overlook::vault_audit` log target
//! (operation, kind, account id, outcome; never the secret). Keyring library errors are
//! logged only at debug level under `overlook::diag`; callers and the UI see constant
//! messages that carry no library detail.
//!
//! Each secret is one keyring item scoped to Overlook, the account, and the secret kind.
//! Gmail accounts hold OAuth tokens under [`KIND_OAUTH_TOKENS`]; IMAP and POP3 accounts
//! hold their passwords under [`KIND_PASSWORD_INCOMING`] and, unless the outgoing server
//! shares it, [`KIND_PASSWORD_OUTGOING`]; IMAP and POP3 accounts that sign in with Microsoft
//! hold their OAuth tokens under [`KIND_OAUTH_MICROSOFT`] instead of a password. Contacts
//! connections keep their own: a contacts server's password under
//! [`overlook_core::ContactsConnection::SECRET_KIND`], and the Microsoft sign-in that allows
//! contacts under [`KIND_OAUTH_MICROSOFT_CONTACTS`].

use overlook_core::AccountId;
use overlook_provider::gmail::OAuthTokens;
use overlook_provider::microsoft::ClientConfig;

#[cfg(target_os = "macos")]
mod keychain;
#[cfg(not(target_os = "macos"))]
mod secret_service;

#[cfg(target_os = "macos")]
use keychain as backend;
#[cfg(not(target_os = "macos"))]
use secret_service as backend;

const APPLICATION: &str = "ai.lastrefuge.Overlook";

/// Gmail OAuth tokens, JSON-encoded.
pub const KIND_OAUTH_TOKENS: &str = "oauth-tokens";
/// Microsoft OAuth tokens of an IMAP or POP3 account, JSON-encoded.
pub const KIND_OAUTH_MICROSOFT: &str = "oauth-tokens:microsoft";
/// The Microsoft sign-in that allows contacts, apart from the mail one, JSON-encoded.
pub const KIND_OAUTH_MICROSOFT_CONTACTS: &str = "oauth-tokens:microsoft-contacts";
/// IMAP or POP3 password, UTF-8.
pub const KIND_PASSWORD_INCOMING: &str = "password:incoming";
/// SMTP password, UTF-8. Absent when "same as incoming" is ticked.
pub const KIND_PASSWORD_OUTGOING: &str = "password:outgoing";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VaultError {
    #[cfg_attr(target_os = "macos", error("Couldn't reach Apple Keychain. Check that your login keychain is unlocked."))]
    #[cfg_attr(not(target_os = "macos"), error("Couldn't reach the system keyring. Check that it is installed and unlocked."))]
    Unavailable,
    #[error("Stored sign-in data couldn't be read. Sign in again to replace it.")]
    Corrupt,
}

/// Which keyring operation an audit line or failure belongs to.
#[derive(Clone, Copy, Debug)]
enum Operation {
    Read,
    Write,
    Delete,
    DeleteAll,
}

impl Operation {
    fn name(self) -> &'static str {
        match self {
            Operation::Read => "read",
            Operation::Write => "write",
            Operation::Delete => "delete",
            Operation::DeleteAll => "delete-all",
        }
    }
}

/// Records one keyring access. Never receives secret material.
fn audit(op: Operation, account: &AccountId, kind: &str, outcome: &str) {
    tracing::info!(target: "overlook::vault_audit", op = op.name(), kind, account = %account.0, outcome, "keyring access");
}

/// Keeps library detail out of user-facing errors: logged at debug level only.
fn unavailable(op: Operation, detail: impl std::fmt::Display) -> VaultError {
    tracing::debug!(target: "overlook::diag", op = op.name(), "keyring failure: {detail}");
    VaultError::Unavailable
}

fn corrupt(detail: impl std::fmt::Display) -> VaultError {
    tracing::debug!(target: "overlook::diag", "stored keyring data unreadable: {detail}");
    VaultError::Corrupt
}

/// Audits the outcome of an operation and passes it through.
fn audited<T>(op: Operation, account: &AccountId, kind: &str, result: Result<T, VaultError>, found: impl Fn(&T) -> bool) -> Result<T, VaultError> {
    let outcome = match &result {
        Ok(value) if found(value) => "ok",
        Ok(_) => "not-found",
        Err(VaultError::Unavailable) => "keyring-unavailable",
        Err(VaultError::Corrupt) => "corrupt",
    };
    audit(op, account, kind, outcome);
    result
}

/// Saves a secret of `kind` for an account, replacing any previous entry. Blocking.
pub fn save_secret(account: &AccountId, kind: &str, secret: Vec<u8>) -> Result<(), VaultError> {
    let op = Operation::Write;
    let result = backend::save(account, kind, secret).map_err(|e| unavailable(op, e));
    audited(op, account, kind, result, |_| true)
}

/// Loads a secret of `kind` for an account. `Ok(None)` when nothing is stored. Blocking.
pub fn load_secret(account: &AccountId, kind: &str) -> Result<Option<Vec<u8>>, VaultError> {
    let op = Operation::Read;
    let result = backend::load(account, kind).map_err(|e| unavailable(op, e));
    audited(op, account, kind, result, Option::is_some)
}

/// Deletes one kind of secret for an account. Blocking.
pub fn delete_secret(account: &AccountId, kind: &str) -> Result<(), VaultError> {
    let op = Operation::Delete;
    let result = backend::delete(account, kind).map_err(|e| unavailable(op, e));
    audited(op, account, kind, result, |_| true)
}

/// Deletes every secret stored for an account, whatever its kind. Blocking.
pub fn delete_all_secrets(account: &AccountId) -> Result<(), VaultError> {
    let op = Operation::DeleteAll;
    let result = backend::delete_all(account).map_err(|e| unavailable(op, e));
    audited(op, account, "*", result, |_| true)
}

// ------------------------------------------------------------------ typed wrappers

/// Saves OAuth tokens for a Gmail account, replacing any previous entry. Blocking.
pub fn save_tokens(account: &AccountId, tokens: &OAuthTokens) -> Result<(), VaultError> {
    let secret = serde_json::to_vec(tokens).map_err(corrupt)?;
    save_secret(account, KIND_OAUTH_TOKENS, secret)
}

/// Loads OAuth tokens for a Gmail account. `Ok(None)` when nothing is stored. Blocking.
pub fn load_tokens(account: &AccountId) -> Result<Option<OAuthTokens>, VaultError> {
    match load_secret(account, KIND_OAUTH_TOKENS)? {
        Some(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(corrupt),
        None => Ok(None),
    }
}

/// Deletes stored OAuth tokens for an account. Blocking.
pub fn delete_tokens(account: &AccountId) -> Result<(), VaultError> {
    delete_secret(account, KIND_OAUTH_TOKENS)
}

/// A Microsoft sign-in as stored: the tokens, and the app registration that issued them, so
/// refreshes keep using that registration even if the built-in one or the configuration
/// file changes later. The client id and tenant are public; only the tokens are secret.
#[derive(serde::Serialize, serde::Deserialize)]
struct StoredMicrosoft {
    #[serde(flatten)]
    tokens: OAuthTokens,
    client_id: String,
    tenant: String,
}

/// Saves the Microsoft sign-in of an IMAP or POP3 account, replacing any previous entry.
/// Blocking.
pub fn save_microsoft_tokens(account: &AccountId, tokens: &OAuthTokens, config: &ClientConfig) -> Result<(), VaultError> {
    save_microsoft_in(account, KIND_OAUTH_MICROSOFT, tokens, config)
}

/// Loads the Microsoft sign-in of an account: its tokens and the registration that issued
/// them. `Ok(None)` when nothing is stored. Blocking.
pub fn load_microsoft_tokens(account: &AccountId) -> Result<Option<(OAuthTokens, ClientConfig)>, VaultError> {
    load_microsoft_from(account, KIND_OAUTH_MICROSOFT)
}

/// Saves the Microsoft sign-in that allows contacts, replacing any previous one. The mail
/// sign-in is a separate entry and never changes here. Blocking.
pub fn save_microsoft_contacts_tokens(account: &AccountId, tokens: &OAuthTokens, config: &ClientConfig) -> Result<(), VaultError> {
    save_microsoft_in(account, KIND_OAUTH_MICROSOFT_CONTACTS, tokens, config)
}

/// Loads the Microsoft sign-in that allows contacts. Blocking.
pub fn load_microsoft_contacts_tokens(account: &AccountId) -> Result<Option<(OAuthTokens, ClientConfig)>, VaultError> {
    load_microsoft_from(account, KIND_OAUTH_MICROSOFT_CONTACTS)
}

fn save_microsoft_in(account: &AccountId, kind: &str, tokens: &OAuthTokens, config: &ClientConfig) -> Result<(), VaultError> {
    let stored = StoredMicrosoft { tokens: tokens.clone(), client_id: config.client_id.clone(), tenant: config.tenant.clone() };
    let secret = serde_json::to_vec(&stored).map_err(corrupt)?;
    save_secret(account, kind, secret)
}

fn load_microsoft_from(account: &AccountId, kind: &str) -> Result<Option<(OAuthTokens, ClientConfig)>, VaultError> {
    match load_secret(account, kind)? {
        Some(bytes) => {
            let stored: StoredMicrosoft = serde_json::from_slice(&bytes).map_err(corrupt)?;
            let config = ClientConfig::new(stored.client_id, stored.tenant).map_err(corrupt)?;
            Ok(Some((stored.tokens, config)))
        }
        None => Ok(None),
    }
}

/// Saves a server password (`KIND_PASSWORD_INCOMING` or `KIND_PASSWORD_OUTGOING`). Blocking.
#[allow(dead_code)] // Used by the IMAP/POP3 account setup (milestone 2).
pub fn save_password(account: &AccountId, kind: &str, password: &str) -> Result<(), VaultError> {
    save_secret(account, kind, password.as_bytes().to_vec())
}

/// Loads a server password. `Ok(None)` when nothing is stored. Blocking.
#[allow(dead_code)] // Used by providers_for once IMAP/POP3 providers exist (milestone 2).
pub fn load_password(account: &AccountId, kind: &str) -> Result<Option<String>, VaultError> {
    match load_secret(account, kind)? {
        Some(bytes) => String::from_utf8(bytes).map(Some).map_err(corrupt),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::{Arc, Mutex};
    use tracing_subscriber::layer::SubscriberExt;

    #[derive(Clone)]
    struct Capture(Arc<Mutex<Vec<u8>>>);
    impl Write for Capture {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> { self.0.lock().unwrap().extend_from_slice(bytes); Ok(bytes.len()) }
        fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
    }

    fn capture(f: impl FnOnce()) -> String {
        let captured = Capture(Arc::new(Mutex::new(Vec::new())));
        let writer = captured.clone();
        let subscriber = tracing_subscriber::registry().with(tracing_subscriber::fmt::layer().without_time().with_ansi(false).with_writer(move || writer.clone()));
        tracing::subscriber::with_default(subscriber, f);
        let bytes = captured.0.lock().unwrap().clone();
        String::from_utf8(bytes).unwrap()
    }

    #[test]
    fn user_facing_vault_errors_are_constant_and_detail_stays_in_debug_log() {
        let log = capture(|| {
            let error = unavailable(Operation::Read, "org.freedesktop.DBus.Error.ServiceUnknown: synthetic-detail");
            assert_eq!(error, VaultError::Unavailable);
            assert!(!error.to_string().contains("synthetic-detail"));
            let error = corrupt("expected value at line 1 column 1 synthetic-detail");
            assert!(!error.to_string().contains("synthetic-detail"));
        });
        assert!(log.contains("DEBUG overlook::diag"));
        assert!(log.contains("synthetic-detail"));
    }

    #[test]
    fn microsoft_sign_ins_keep_their_registration_beside_the_tokens() {
        let tokens = OAuthTokens { access_token: "synthetic-access".into(), refresh_token: Some("synthetic-refresh".into()), expires_at: 5, scope: None, signed_in_at: 7 };
        let stored = StoredMicrosoft { tokens: tokens.clone(), client_id: "client-a".into(), tenant: "common".into() };
        let json = serde_json::to_string(&stored).unwrap();
        assert!(json.contains("\"refresh_token\":\"synthetic-refresh\"") && json.contains("\"client_id\":\"client-a\""), "{json}");
        let back: StoredMicrosoft = serde_json::from_str(&json).unwrap();
        assert!(back.tokens == tokens && back.client_id == "client-a" && back.tenant == "common");
    }

    #[test]
    fn keyring_access_is_audited_without_secret_material() {
        let account = AccountId("acct-synthetic".into());
        let log = capture(|| {
            let _ = audited(Operation::Read, &account, KIND_OAUTH_TOKENS, Ok(Some(b"synthetic-secret".to_vec())), Option::is_some);
            let _ = audited::<Option<Vec<u8>>>(Operation::Read, &account, KIND_PASSWORD_INCOMING, Ok(None), Option::is_some);
            let _ = audited::<()>(Operation::Write, &account, KIND_PASSWORD_OUTGOING, Err(VaultError::Unavailable), |_| true);
        });
        assert!(log.contains("overlook::vault_audit"));
        assert!(log.contains(r#"op="read" kind="oauth-tokens" account=acct-synthetic outcome="ok""#), "{log}");
        assert!(log.contains(r#"outcome="not-found""#));
        assert!(log.contains(r#"op="write" kind="password:outgoing" account=acct-synthetic outcome="keyring-unavailable""#), "{log}");
        assert!(!log.contains("synthetic-secret"));
    }
}
