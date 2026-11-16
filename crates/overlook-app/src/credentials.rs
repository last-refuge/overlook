//! Secret storage in the system keyring (Secret Service, or the secrets portal in Flatpak).
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
//! Each secret is one keyring item with the attributes `(application, account, kind)`:
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
use std::collections::HashMap;
use std::sync::OnceLock;

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

/// True only when this process really runs inside a Flatpak or Snap sandbox, where the
/// secrets portal is the way to the keyring. The same test `oo7` makes through `ashpd`
/// (`/.flatpak-info`, or the process's own cgroup for a Snap) without its last clause.
/// Environment variables such as `FLATPAK_ID` or `SNAP` aren't read: a program started from a
/// Flatpak or Snap app's terminal inherits them without being sandboxed itself.
///
/// `GTK_USE_PORTAL` is deliberately not a signal. It asks GTK to send GTK's own dialogs
/// through the xdg-desktop-portal; it says nothing about the process being sandboxed. The
/// `oo7` 0.4 helper behind `Keyring::new` treats it as proof of a sandbox, so a user who set
/// it session-wide had mail passwords written to `~/.local/share/keyrings/default.keyring`
/// instead of the system keyring. That file is encrypted with the secrets portal's
/// per-application secret, and the portal derives the application id from how the app was
/// launched: once an `ai.lastrefuge.Overlook.desktop` entry exists, KDE starts the app as
/// `app-ai.lastrefuge.Overlook@<id>.service`, the portal hands it a different secret, and
/// every stored password then reads back as `IncorrectSecret`.
fn sandboxed() -> bool {
    let cgroup = std::fs::read_to_string("/proc/self/cgroup").unwrap_or_default();
    sandboxed_with(std::path::Path::new("/"), &cgroup)
}

/// [`sandboxed`] with its inputs passed in, so the rule can be checked without a container.
fn sandboxed_with(root: &std::path::Path, cgroup: &str) -> bool {
    root.join(".flatpak-info").exists() || in_snap(cgroup)
}

/// Whether a `/proc/self/cgroup` listing puts the process in a Snap's scope
/// (`snap.<name>.<app>…`), as `ashpd` checks.
fn in_snap(cgroup: &str) -> bool {
    cgroup.lines().any(|line| {
        let Some((number, rest)) = line.split_once(':') else { return false };
        let unit = match rest.split_once(':') {
            Some(("" | "freezer" | "name=systemd", unit)) if number.parse::<u32>().is_ok() => unit,
            _ => return false,
        };
        std::path::Path::new(unit).file_name().and_then(|n| n.to_str()).is_some_and(|scope| scope.starts_with("snap."))
    })
}

/// The keyring to use: the Secret Service outside a sandbox, the portal's file backend inside
/// one. Chosen here rather than by `oo7::Keyring::new`, which asks `ashpd` whether the process
/// is sandboxed and counts `GTK_USE_PORTAL` as proof. Like `oo7`, a sandbox without the
/// secrets portal uses the Secret Service, which the Flatpak is allowed to reach.
async fn keyring() -> Result<oo7::Keyring, oo7::Error> {
    if sandboxed() {
        match oo7::file::Keyring::load_default().await {
            Ok(file) => {
                tracing::debug!(target: "overlook::diag", "sandboxed: using the secrets portal file keyring");
                return Ok(oo7::Keyring::File(std::sync::Arc::new(file)));
            }
            Err(oo7::file::Error::Portal(oo7::ashpd::Error::PortalNotFound(_))) => {
                tracing::debug!(target: "overlook::diag", "sandboxed without the secrets portal: using the Secret Service");
            }
            Err(e) => return Err(oo7::Error::File(e)),
        }
    }
    let service = oo7::dbus::Service::<'static>::new().await?;
    Ok(oo7::Keyring::DBus(service.default_collection().await?))
}

fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .thread_name("overlook-keyring")
            .build()
            .expect("tokio runtime")
    })
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VaultError {
    #[error("Couldn't reach the system keyring. Check that it is installed and unlocked.")]
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

fn attributes(account: &AccountId, kind: &str) -> HashMap<&'static str, String> {
    HashMap::from([("application", APPLICATION.to_owned()), ("account", account.0.clone()), ("kind", kind.to_owned())])
}

/// Attributes shared by every secret of an account, for deleting them all at once.
fn account_attributes(account: &AccountId) -> HashMap<&'static str, String> {
    HashMap::from([("application", APPLICATION.to_owned()), ("account", account.0.clone())])
}

fn borrow<'a>(attrs: &'a HashMap<&'static str, String>) -> HashMap<&'a str, &'a str> {
    attrs.iter().map(|(k, v)| (*k, v.as_str())).collect()
}

/// The item label shown in keyring managers.
fn label(account: &AccountId, kind: &str) -> String {
    let what = match kind {
        KIND_OAUTH_TOKENS => "Gmail sign-in",
        KIND_OAUTH_MICROSOFT => "Microsoft sign-in",
        KIND_OAUTH_MICROSOFT_CONTACTS => "Microsoft contacts sign-in",
        KIND_PASSWORD_INCOMING => "mail server password",
        KIND_PASSWORD_OUTGOING => "outgoing server password",
        overlook_core::ContactsConnection::SECRET_KIND => "contacts server password",
        other => other,
    };
    format!("Overlook {what} ({})", account.0)
}

/// Saves a secret of `kind` for an account, replacing any previous entry. Blocking.
pub fn save_secret(account: &AccountId, kind: &str, secret: Vec<u8>) -> Result<(), VaultError> {
    let attrs = attributes(account, kind);
    let label = label(account, kind);
    let op = Operation::Write;
    let result = runtime().block_on(async move {
        let keyring = keyring().await.map_err(|e| unavailable(op, e))?;
        keyring.create_item(&label, &borrow(&attrs), secret, true).await.map_err(|e| unavailable(op, e))
    });
    audited(op, account, kind, result, |_| true)
}

/// Loads a secret of `kind` for an account. `Ok(None)` when nothing is stored. Blocking.
pub fn load_secret(account: &AccountId, kind: &str) -> Result<Option<Vec<u8>>, VaultError> {
    let attrs = attributes(account, kind);
    let op = Operation::Read;
    let result = runtime().block_on(async move {
        let keyring = keyring().await.map_err(|e| unavailable(op, e))?;
        let items = keyring.search_items(&borrow(&attrs)).await.map_err(|e| unavailable(op, e))?;
        let Some(item) = items.into_iter().next() else { return Ok(None) };
        let secret = item.secret().await.map_err(|e| unavailable(op, e))?;
        Ok(Some(secret.as_bytes().to_vec()))
    });
    audited(op, account, kind, result, Option::is_some)
}

/// Deletes one kind of secret for an account. Blocking.
#[allow(dead_code)] // Used by the IMAP/POP3 account setup (milestone 2).
pub fn delete_secret(account: &AccountId, kind: &str) -> Result<(), VaultError> {
    let attrs = attributes(account, kind);
    let op = Operation::Delete;
    let result = runtime().block_on(async move {
        let keyring = keyring().await.map_err(|e| unavailable(op, e))?;
        keyring.delete(&borrow(&attrs)).await.map_err(|e| unavailable(op, e))
    });
    audited(op, account, kind, result, |_| true)
}

/// Deletes every secret stored for an account, whatever its kind. Blocking.
pub fn delete_all_secrets(account: &AccountId) -> Result<(), VaultError> {
    let attrs = account_attributes(account);
    let op = Operation::DeleteAll;
    let result = runtime().block_on(async move {
        let keyring = keyring().await.map_err(|e| unavailable(op, e))?;
        keyring.delete(&borrow(&attrs)).await.map_err(|e| unavailable(op, e))
    });
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
        assert_eq!(label(&AccountId("imap:j@outlook.com".into()), KIND_OAUTH_MICROSOFT), "Overlook Microsoft sign-in (imap:j@outlook.com)");
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

    #[test]
    fn a_portal_hint_is_not_a_sandbox() {
        // GTK_USE_PORTAL=1, set session-wide, used to send every mail password to oo7's file
        // keyring: its key is the portal's per-application secret, and the portal changes the
        // application id once a .desktop entry lets it tell how the app was launched. The
        // decision reads no environment variable at all, so neither that hint nor a SNAP or
        // FLATPAK_ID inherited from a sandboxed app's terminal can move the passwords.
        let bare = std::path::Path::new("/nonexistent-overlook-sandbox-root");
        let desktop = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-ai.lastrefuge.Overlook@x.service\n";
        assert!(!sandboxed_with(bare, desktop));
        assert!(!sandboxed_with(bare, ""));
        assert!(sandboxed_with(bare, "0::/user.slice/user-1000.slice/user@1000.service/app.slice/snap.overlook.overlook-4b2c.scope\n"));
        assert!(sandboxed_with(bare, "12:freezer:/snap.overlook.overlook\n0::/user.slice\n"));
        assert!(!sandboxed_with(bare, "0::/user.slice/user-1000.slice/user@1000.service/app.slice/snap-code.scope\n"), "not a snap's own scope");

        let container = tempfile::tempdir().unwrap();
        std::fs::write(container.path().join(".flatpak-info"), b"[Application]\n").unwrap();
        assert!(sandboxed_with(container.path(), desktop));
    }
}
