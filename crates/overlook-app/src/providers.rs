//! Builds the providers for each configured account.

use crate::credentials;
use overlook_core::*;
use overlook_provider::gmail::{GmailClient, GmailProvider, GoogleCalendarProvider, OAuthTokens};
use overlook_provider::imap::ImapProvider;
use overlook_provider::microsoft::{ClientConfig, MicrosoftSession};
use overlook_provider::pop3::Pop3Provider;
use overlook_provider::sample::SampleProvider;
use overlook_provider::{CalendarProvider, Credential, MailProvider};
use overlook_store::Store;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use tracing::{info, warn};

/// What the Status row asks for when an IMAP or POP3 account has no usable password.
pub const PASSWORD_NEEDED_WORDING: &str = "Enter this account’s password again to continue syncing. Your local drafts are safe.";

/// What the Status row asks for when a Microsoft account's sign-in is missing or rejected.
pub const MICROSOFT_SIGN_IN_WORDING: &str = "Sign in with Microsoft again to continue syncing. Your local drafts are safe.";

/// Why an account has no usable provider right now.
#[derive(Debug, Clone)]
pub enum ProviderIssue {
    /// No tokens are stored, or the keyring could not be read.
    SignInRequired(String),
}

#[derive(Clone)]
pub struct AccountProviders {
    pub mail: Arc<dyn MailProvider>,
    pub calendar: Option<Arc<dyn CalendarProvider>>,
    /// The same provider as `mail` when it is an IMAP one, for the `IDLE` watcher.
    pub imap: Option<Arc<ImapProvider>>,
    /// Shared lifecycle handle so removing an account disables every cloned provider.
    pub gmail: Option<Arc<GmailClient>>,
    /// The sign-in of an IMAP or POP3 account that signs in with Microsoft, for the same
    /// purpose, and for the session limits.
    pub microsoft: Option<Arc<MicrosoftSession>>,
}

impl AccountProviders {
    pub fn new(mail: Arc<dyn MailProvider>, calendar: Option<Arc<dyn CalendarProvider>>) -> Self {
        Self { mail, calendar, imap: None, gmail: None, microsoft: None }
    }
}

pub fn gmail_providers(account: &Account, tokens: OAuthTokens) -> Result<AccountProviders, String> {
    let config = crate::google_signin::load_client_config()?;
    let account_id = account.id.clone();
    let invalidated_id = account.id.clone();
    let client = Arc::new(
        GmailClient::new(
            config,
            tokens,
            Box::new(move |fresh| {
                if let Err(e) = credentials::save_tokens(&account_id, fresh) {
                    warn!("could not persist refreshed tokens: {e}");
                }
            }),
        )
        // Google rejected the refresh token (expired or revoked): delete the dead copy.
        .with_on_invalidated(Box::new(move || {
            if let Err(e) = credentials::delete_tokens(&invalidated_id) {
                warn!("could not delete the rejected sign-in: {e}");
            }
        })),
    );
    let inbox_limit = crate::store_prefs_inbox_limit();
    let mut providers = AccountProviders::new(Arc::new(GmailProvider::new(client.clone()).with_inbox_limit(inbox_limit)), Some(Arc::new(GoogleCalendarProvider::new(client.clone()))));
    providers.gmail = Some(client);
    Ok(providers)
}

/// An IMAP account: one connection for mail, SMTP for sending, no calendar. The outgoing
/// password is the incoming one unless a separate one was stored at setup.
pub fn imap_providers(servers: MailServers, credential: impl Into<Credential>, outgoing_password: Option<String>) -> AccountProviders {
    let inbox_limit = crate::store_prefs_inbox_limit();
    let provider = Arc::new(ImapProvider::new(servers, credential).with_outgoing_password(outgoing_password).with_inbox_limit(inbox_limit));
    AccountProviders { mail: provider.clone(), calendar: None, imap: Some(provider), gmail: None, microsoft: None }
}

/// Which Microsoft sign-in of each account may write to the keyring: only the newest. A
/// session replaced by a new sign-in, or whose account was signed out or removed, can still
/// finish a refresh it started, but never stores it or deletes its successor's sign-in.
/// Microsoft offers no revocation that would stop it otherwise.
#[derive(Default)]
struct SignInGenerations {
    current: HashMap<AccountId, u64>,
    last: u64,
}

impl SignInGenerations {
    fn begin(&mut self, account: &AccountId) -> u64 {
        self.last += 1;
        self.current.insert(account.clone(), self.last);
        self.last
    }

    fn is_current(&self, account: &AccountId, generation: u64) -> bool {
        self.current.get(account) == Some(&generation)
    }
}

fn generations() -> std::sync::MutexGuard<'static, SignInGenerations> {
    static GENERATIONS: OnceLock<Mutex<SignInGenerations>> = OnceLock::new();
    GENERATIONS.get_or_init(Default::default).lock().unwrap_or_else(|p| p.into_inner())
}

type SaveSignIn = dyn Fn(&AccountId, &OAuthTokens, &ClientConfig) -> Result<(), credentials::VaultError> + Send + Sync;
type DeleteSignIn = dyn Fn(&AccountId) -> Result<(), credentials::VaultError> + Send + Sync;

/// How a session writes to the keyring: the system keyring, or a recording in tests.
#[derive(Clone)]
pub struct SignInVault {
    save: Arc<SaveSignIn>,
    delete: Arc<DeleteSignIn>,
}

impl SignInVault {
    pub fn system() -> Self {
        Self { save: Arc::new(credentials::save_microsoft_tokens), delete: Arc::new(|id| credentials::delete_secret(id, credentials::KIND_OAUTH_MICROSOFT)) }
    }
}

/// The scopes a Microsoft account's sign-in holds: mail over its incoming protocol, and SMTP.
pub fn microsoft_scopes(kind: ProviderKind) -> String {
    overlook_provider::microsoft::oauth::scopes(if kind == ProviderKind::Pop3 { ServerProtocol::Pop3 } else { ServerProtocol::Imap })
}

/// Stores a new Microsoft sign-in for `account` and returns the session that uses it. Earlier
/// sessions of the account may finish what they are doing, but stop storing refreshes.
pub fn adopt_microsoft_sign_in(account: &AccountId, kind: ProviderKind, tokens: OAuthTokens, config: ClientConfig) -> Result<Arc<MicrosoftSession>, credentials::VaultError> {
    adopt_with(account, kind, tokens, config, SignInVault::system())
}

fn adopt_with(account: &AccountId, kind: ProviderKind, tokens: OAuthTokens, config: ClientConfig, vault: SignInVault) -> Result<Arc<MicrosoftSession>, credentials::VaultError> {
    let mut generations = generations();
    (vault.save)(account, &tokens, &config)?;
    let generation = generations.begin(account);
    drop(generations);
    Ok(Arc::new(build_session(account, kind, tokens, config, generation, vault)))
}

/// The session for an account's stored sign-in (start-up, or the setup dialog when the
/// account has none running). It becomes the one whose refreshes are stored.
pub fn microsoft_session(account: &AccountId, kind: ProviderKind, tokens: OAuthTokens, config: ClientConfig) -> Arc<MicrosoftSession> {
    let generation = generations().begin(account);
    Arc::new(build_session(account, kind, tokens, config, generation, SignInVault::system()))
}

/// Stops every session of the account from writing to the keyring. Sign-out and removal call
/// it before deleting the stored sign-in.
pub fn end_microsoft_sign_ins(account: &AccountId) {
    generations().current.remove(account);
}

/// Refreshed tokens go to the keyring with the registration that issued them, and a refresh
/// token Microsoft rejects is deleted there, both only while `generation` is current.
fn build_session(account: &AccountId, kind: ProviderKind, tokens: OAuthTokens, config: ClientConfig, generation: u64, vault: SignInVault) -> MicrosoftSession {
    let (save_id, invalid_id, save_config, save_vault) = (account.clone(), account.clone(), config.clone(), vault.clone());
    MicrosoftSession::new(
            config,
            microsoft_scopes(kind),
            tokens,
            Box::new(move |fresh| {
                let generations = generations();
                if !generations.is_current(&save_id, generation) {
                    info!(target: "overlook::signin", account = %save_id.0, "a replaced or ended Microsoft session refreshed its token; not stored");
                    return;
                }
                if let Err(e) = (save_vault.save)(&save_id, fresh, &save_config) {
                    warn!("could not persist refreshed Microsoft tokens: {e}");
                }
            }),
        )
    // Microsoft rejected the refresh token (expired, revoked, password changed).
    .with_on_invalidated(Box::new(move || {
        let generations = generations();
        if !generations.is_current(&invalid_id, generation) {
            return;
        }
        if let Err(e) = (vault.delete)(&invalid_id) {
            warn!("could not delete the rejected Microsoft sign-in: {e}");
        }
    }))
}

/// An IMAP or POP3 account that signs in with Microsoft: the usual providers, signing in
/// with tokens from `session`, which also sends.
pub fn microsoft_providers(account: &Account, servers: MailServers, session: Arc<MicrosoftSession>) -> AccountProviders {
    let credential = Credential::OAuth(session.clone());
    let mut providers = match account.provider {
        ProviderKind::Pop3 => pop3_providers(&account.id, servers, credential, None),
        _ => imap_providers(servers, credential, None),
    };
    providers.microsoft = Some(session);
    providers
}

/// Builds a Microsoft account's providers from its stored sign-in. A running session of the
/// account should be reused instead (see `MailWindow::rebuild_providers_for_download_size`).
fn stored_microsoft_providers(account: &Account, servers: MailServers) -> Result<AccountProviders, ProviderIssue> {
    match credentials::load_microsoft_tokens(&account.id) {
        Ok(Some((tokens, config))) => Ok(microsoft_providers(account, servers, microsoft_session(&account.id, account.provider, tokens, config))),
        Ok(None) => Err(ProviderIssue::SignInRequired(MICROSOFT_SIGN_IN_WORDING.into())),
        Err(e) => Err(ProviderIssue::SignInRequired(e.to_string())),
    }
}

/// Where a POP3 account's downloaded messages live: `<data dir>/raw/<account>/`, one `.eml`
/// per message. This is the account's primary copy; "Back up mail" includes it.
pub fn raw_dir_for(account: &AccountId) -> std::path::PathBuf {
    crate::data_dir().join("raw").join(account.as_str().replace([':', '/'], "_"))
}

/// A POP3 account: one session per sync, SMTP for sending, no calendar. The local store
/// and `raw/` hold the only copy of its mail.
pub fn pop3_providers(account: &AccountId, servers: MailServers, credential: impl Into<Credential>, outgoing_password: Option<String>) -> AccountProviders {
    AccountProviders::new(Arc::new(Pop3Provider::new(servers, credential, raw_dir_for(account)).with_outgoing_password(outgoing_password)), None)
}

/// The stored passwords of an IMAP or POP3 account: the incoming one, and the outgoing one
/// when a separate one was saved.
fn stored_passwords(account: &Account) -> Result<(String, Option<String>), ProviderIssue> {
    match credentials::load_password(&account.id, credentials::KIND_PASSWORD_INCOMING) {
        Ok(Some(password)) => {
            let outgoing = match credentials::load_password(&account.id, credentials::KIND_PASSWORD_OUTGOING) {
                Ok(found) => found,
                Err(e) => {
                    // A failed vault read is not the user's "same as incoming" choice.
                    return Err(ProviderIssue::SignInRequired(e.to_string()));
                }
            };
            Ok((password, outgoing))
        }
        Ok(None) => Err(ProviderIssue::SignInRequired(PASSWORD_NEEDED_WORDING.into())),
        Err(e) => Err(ProviderIssue::SignInRequired(e.to_string())),
    }
}

/// Builds the providers for `account`, reading stored tokens or passwords where needed.
pub fn providers_for(account: &Account, sample: &SampleProvider, store: &Store) -> Result<AccountProviders, ProviderIssue> {
    match account.provider {
        ProviderKind::Sample => Ok(AccountProviders::new(Arc::new(sample.clone()), Some(Arc::new(sample.clone())))),
        ProviderKind::Gmail => match credentials::load_tokens(&account.id) {
            Ok(Some(tokens)) => gmail_providers(account, tokens).map_err(ProviderIssue::SignInRequired),
            Ok(None) => Err(ProviderIssue::SignInRequired("Sign in again to continue syncing. Your local drafts are safe.".into())),
            Err(e) => Err(ProviderIssue::SignInRequired(e.to_string())),
        },
        ProviderKind::Imap | ProviderKind::Pop3 => {
            let servers = match store.servers(&account.id) {
                Ok(Some(s)) => s,
                Ok(None) => return Err(ProviderIssue::SignInRequired("Finish setting up this account’s servers to start syncing.".into())),
                Err(e) => return Err(ProviderIssue::SignInRequired(e.to_string())),
            };
            if servers.oauth == Some(OAuthIssuer::Microsoft) {
                return stored_microsoft_providers(account, servers);
            }
            let (password, outgoing) = stored_passwords(account)?;
            Ok(if account.provider == ProviderKind::Imap { imap_providers(servers, password, outgoing) } else { pop3_providers(&account.id, servers, password, outgoing) })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use overlook_provider::microsoft::oauth::AuthError;
    use overlook_provider::AccessTokenSource;

    fn tokens(access: &str, refresh: &str) -> OAuthTokens {
        OAuthTokens { access_token: access.into(), refresh_token: Some(refresh.into()), expires_at: 0, scope: None, signed_in_at: 1 }
    }

    fn config() -> ClientConfig {
        ClientConfig::new("synthetic-client", "common").unwrap()
    }

    /// Records what reaches the keyring.
    fn recording() -> (SignInVault, Arc<Mutex<Vec<String>>>) {
        let log = Arc::new(Mutex::new(Vec::new()));
        let (saves, deletes) = (log.clone(), log.clone());
        let vault = SignInVault {
            save: Arc::new(move |_, t, _| {
                saves.lock().unwrap().push(format!("save {}", t.refresh_token.as_deref().unwrap_or("")));
                Ok(())
            }),
            delete: Arc::new(move |_| {
                deletes.lock().unwrap().push("delete".into());
                Ok(())
            }),
        };
        (vault, log)
    }

    /// A session at `generation` whose (expired) token refreshes to `rotated`.
    fn scripted(account: &AccountId, generation: u64, vault: SignInVault, rotated: &'static str) -> Arc<MicrosoftSession> {
        Arc::new(build_session(account, ProviderKind::Imap, tokens("old-access", "old-refresh"), config(), generation, vault).with_refresher(Box::new(move |_, _, _| {
            Ok(OAuthTokens { access_token: format!("{rotated}-access"), refresh_token: Some(rotated.into()), expires_at: overlook_core::time::now() + 3_600_000, scope: None, signed_in_at: 0 })
        })))
    }

    #[test]
    fn only_the_newest_sign_in_of_an_account_writes_to_the_keyring() {
        let account = AccountId::from("imap:generations-test@outlook.com");
        let (vault, log) = recording();
        let first = scripted(&account, generations().begin(&account), vault.clone(), "first-rotated");
        assert_eq!(first.access_token(None), Ok("first-rotated-access".into()));
        assert_eq!(*log.lock().unwrap(), ["save first-rotated"]);

        // A new sign-in replaces it: the first session keeps working but stores nothing.
        adopt_with(&account, ProviderKind::Imap, tokens("new-access", "new-refresh"), config(), vault.clone()).unwrap();
        assert_eq!(first.access_token(Some("first-rotated-access")), Ok("first-rotated-access".into()));
        assert_eq!(*log.lock().unwrap(), ["save first-rotated", "save new-refresh"], "the replaced session's refresh is not stored");

        // Nor can its invalid_grant delete the sign-in that replaced it.
        let replaced = Arc::new(build_session(&account, ProviderKind::Imap, tokens("a", "r"), config(), 0, vault.clone()).with_refresher(Box::new(|_, _, _| Err(AuthError::Denied("invalid_grant: AADSTS50173: revoked".into())))));
        assert!(replaced.access_token(None).is_err());
        assert!(!log.lock().unwrap().iter().any(|l| l == "delete"));

        // The current session's invalid_grant deletes; after sign-out nothing is stored.
        let current = Arc::new(build_session(&account, ProviderKind::Imap, tokens("a", "r"), config(), generations().begin(&account), vault.clone()).with_refresher(Box::new(|_, _, _| Err(AuthError::Denied("invalid_grant: AADSTS50173: revoked".into())))));
        assert!(current.access_token(None).is_err());
        assert_eq!(log.lock().unwrap().last().map(String::as_str), Some("delete"));
        let signed_out = scripted(&account, generations().begin(&account), vault, "after-sign-out");
        end_microsoft_sign_ins(&account);
        assert_eq!(signed_out.access_token(None), Ok("after-sign-out-access".into()));
        assert!(!log.lock().unwrap().iter().any(|l| l == "save after-sign-out"));
    }
}
