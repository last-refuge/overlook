//! Builds the providers for each configured account.

use crate::credentials;
use overlook_core::*;
use overlook_provider::gmail::{GmailClient, GmailProvider, GoogleCalendarProvider, OAuthTokens};
use overlook_provider::imap::ImapProvider;
use overlook_provider::pop3::Pop3Provider;
use overlook_provider::sample::SampleProvider;
use overlook_provider::{CalendarProvider, MailProvider};
use overlook_store::Store;
use std::sync::Arc;
use tracing::warn;

/// What the Status row asks for when an IMAP or POP3 account has no usable password.
pub const PASSWORD_NEEDED_WORDING: &str = "Enter this account’s password again to continue syncing. Your local drafts are safe.";

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
}

impl AccountProviders {
    pub fn new(mail: Arc<dyn MailProvider>, calendar: Option<Arc<dyn CalendarProvider>>) -> Self {
        Self { mail, calendar, imap: None, gmail: None }
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
pub fn imap_providers(servers: MailServers, password: String, outgoing_password: Option<String>) -> AccountProviders {
    let inbox_limit = crate::store_prefs_inbox_limit();
    let provider = Arc::new(ImapProvider::new(servers, password).with_outgoing_password(outgoing_password).with_inbox_limit(inbox_limit));
    AccountProviders { mail: provider.clone(), calendar: None, imap: Some(provider), gmail: None }
}

/// Where a POP3 account's downloaded messages live: `<data dir>/raw/<account>/`, one `.eml`
/// per message. This is the account's primary copy; "Back up mail" includes it.
pub fn raw_dir_for(account: &AccountId) -> std::path::PathBuf {
    crate::data_dir().join("raw").join(account.as_str().replace([':', '/'], "_"))
}

/// A POP3 account: one session per sync, SMTP for sending, no calendar. The local store
/// and `raw/` hold the only copy of its mail.
pub fn pop3_providers(account: &AccountId, servers: MailServers, password: String, outgoing_password: Option<String>) -> AccountProviders {
    AccountProviders::new(Arc::new(Pop3Provider::new(servers, password, raw_dir_for(account)).with_outgoing_password(outgoing_password)), None)
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
            let (password, outgoing) = stored_passwords(account)?;
            Ok(if account.provider == ProviderKind::Imap { imap_providers(servers, password, outgoing) } else { pop3_providers(&account.id, servers, password, outgoing) })
        }
    }
}
