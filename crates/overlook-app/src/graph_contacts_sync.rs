//! Outlook contacts for accounts that sign in with Microsoft, through Microsoft Graph
//! ([`overlook_provider::microsoft::contacts`]). The same shape as the CardDAV sync
//! ([`crate::carddav_sync`]), sharing its store: queued edits go first, then a complete
//! download replaces what was read before, every 15 minutes or when asked. Each contact's
//! version as last read is kept, and an edit sends only the fields that differ from it.
//!
//! Access is a sign-in of its own that allows contacts only, made when the person connects
//! contacts. Its access token is refreshed here and the refreshed sign-in saved back to the
//! keyring. The mail sign-in is never used or changed.

use crate::carddav_sync::{self, ContactsStatus, REFRESH_MS};
use crate::credentials;
use crate::microsoft_signin::SignInResult;
use overlook_core::time::{now, Timestamp};
use overlook_core::*;
use overlook_provider::microsoft::contacts::{self as graph, GraphContact, GraphContacts, GraphError, GraphItem};
use overlook_provider::microsoft::oauth::{self, AuthError};
use overlook_store::Store;
use serde_json::{Map, Value};
use tracing::{info, warn};

/// What the sync needs from Graph; the real client, or a fixture in tests.
pub(crate) trait GraphServer {
    fn list(&self) -> Result<Vec<GraphItem>, GraphError>;
    fn get(&self, url: &str) -> Result<Option<GraphContact>, GraphError>;
    fn create(&self, marker: &str, body: Map<String, Value>) -> Result<GraphItem, GraphError>;
    fn find_created(&self, marker: &str) -> Result<Option<GraphItem>, GraphError>;
    fn update(&self, url: &str, patch: &Map<String, Value>) -> Result<Option<GraphContact>, GraphError>;
    fn delete(&self, url: &str) -> Result<(), GraphError>;
}

impl GraphServer for GraphContacts {
    fn list(&self) -> Result<Vec<GraphItem>, GraphError> {
        GraphContacts::list(self)
    }
    fn get(&self, url: &str) -> Result<Option<GraphContact>, GraphError> {
        GraphContacts::get(self, url)
    }
    fn create(&self, marker: &str, body: Map<String, Value>) -> Result<GraphItem, GraphError> {
        GraphContacts::create(self, marker, body)
    }
    fn find_created(&self, marker: &str) -> Result<Option<GraphItem>, GraphError> {
        GraphContacts::find_created(self, marker)
    }
    fn update(&self, url: &str, patch: &Map<String, Value>) -> Result<Option<GraphContact>, GraphError> {
        GraphContacts::update(self, url, patch)
    }
    fn delete(&self, url: &str) -> Result<(), GraphError> {
        GraphContacts::delete(self, url)
    }
}

/// An access token from the contacts sign-in, refreshed when it is about to expire; the
/// refreshed sign-in is saved. Blocking.
pub fn access_token(account: &AccountId) -> Result<String, GraphError> {
    let (tokens, config) = credentials::load_microsoft_contacts_tokens(account).ok().flatten().ok_or(GraphError::SignInRequired)?;
    if !tokens.access_token.is_empty() && tokens.expires_at - 120_000 > now() {
        return Ok(tokens.access_token);
    }
    let refresh_token = tokens.refresh_token.clone().ok_or(GraphError::SignInRequired)?;
    match oauth::refresh(&config, &refresh_token, &graph::contacts_scopes()) {
        Ok(mut fresh) => {
            fresh.signed_in_at = tokens.signed_in_at;
            if let Err(error) = credentials::save_microsoft_contacts_tokens(account, &fresh, &config) {
                warn!("couldn’t save the refreshed contacts sign-in: {error}");
            }
            Ok(fresh.access_token)
        }
        Err(AuthError::Network(_)) => Err(GraphError::Network),
        Err(AuthError::Denied(_)) => Err(GraphError::SignInRequired),
        Err(AuthError::Protocol(_)) => Err(GraphError::Unavailable),
    }
}

/// Checks that a new contacts sign-in can read the contacts. Network work: run it off the UI
/// thread.
pub fn verify(access_token: &str) -> Result<(), GraphError> {
    GraphContacts::new(access_token.to_owned()).check_access()
}

/// Saves a Microsoft contacts connection after the person signed in to allow contacts: the
/// sign-in to the keyring, then the connection. The first download follows at once.
pub fn save(store: &Store, account: &Account, signed_in: &SignInResult) -> Result<ContactsConnection, String> {
    credentials::save_microsoft_contacts_tokens(&account.id, &signed_in.tokens, &signed_in.config).map_err(|e| e.to_string())?;
    let username = signed_in.claims.as_ref().and_then(|c| c.address()).unwrap_or_else(|| account.email.clone());
    let connection = ContactsConnection { account_id: account.id.clone(), server_url: graph::GRAPH_CONTACTS_URL.into(), username, default_book: None, credential: ContactsCredential::Microsoft };
    carddav_sync::save(store, account, connection, None)
}

/// Runs from the mail sync, like [`carddav_sync::run`]. Returns the edits Microsoft refused
/// for good, in words for the person.
pub fn run(store: &Store, account: &Account, connection: &ContactsConnection) -> Vec<String> {
    let key = crate::sync::contacts_synced_key(&account.id);
    let last = store.setting::<i64>(&key).ok().flatten().unwrap_or(0);
    let due = now() - last >= REFRESH_MS;
    if !due && carddav_sync::queued_contact_edits(store, &account.id) == 0 {
        return Vec::new();
    }
    let mut refused = Vec::new();
    let result = access_token(&account.id).and_then(|token| synchronize(store, account, connection, &GraphContacts::new(token), due, &mut refused));
    let status = match result {
        Ok(()) => {
            if due {
                let _ = store.set_setting(&key, &now());
            }
            ContactsStatus { error: refused.first().map(|r| format!("Couldn’t sync {r}")), at: now() }
        }
        Err(error) => {
            warn!("Microsoft contacts sync stopped for {}: {error}", account.email);
            ContactsStatus { error: Some(error.to_string()), at: now() }
        }
    };
    carddav_sync::record_status(store, &account.id, &connection.server_url, &status);
    refused
}

/// Sends the queued edits, then downloads when `download` is set. Stops at the first error
/// that would stop every request, keeping the rest queued.
pub(crate) fn synchronize(store: &Store, account: &Account, connection: &ContactsConnection, server: &dyn GraphServer, download: bool, refused: &mut Vec<String>) -> Result<(), GraphError> {
    send_edits(store, account, connection, server, refused)?;
    if download {
        let count = pull(store, account, connection, server)?;
        info!("synced {count} Microsoft contacts for {}", account.email);
    }
    Ok(())
}

/// How one queued edit went.
enum Sent {
    Done,
    /// Try again later; nothing further is sent this time.
    Retry(GraphError),
    /// This edit can't be made; the rest still go.
    Failed(String),
}

impl From<GraphError> for Sent {
    fn from(error: GraphError) -> Self {
        match error {
            GraphError::Forbidden => Self::Failed("Microsoft doesn’t allow changes to this contact.".into()),
            GraphError::Rejected(_) => Self::Failed(error.to_string()),
            GraphError::UnexpectedStatus(status) if status < 500 => Self::Failed(error.to_string()),
            other => Self::Retry(other),
        }
    }
}

fn send_edits(store: &Store, account: &Account, connection: &ContactsConnection, server: &dyn GraphServer, refused: &mut Vec<String>) -> Result<(), GraphError> {
    let ops = store.pending_operations(&account.id).map_err(|_| GraphError::InvalidResponse)?;
    for op in ops.into_iter().filter(|op| op.kind.is_contact() && op.state != OperationState::Failed) {
        let label = op.kind.contact_id().map(|id| carddav_sync::edit_label(store, &account.id, id)).unwrap_or_default();
        let _ = store.set_operation_state(&op.id, OperationState::InFlight, None);
        let sent = match &op.kind {
            OperationKind::CreateContact { contact_id } => create(store, account, connection, server, contact_id),
            OperationKind::UpdateContact { contact_id } => update(store, account, connection, server, contact_id),
            OperationKind::DeleteContact { contact_id, .. } => delete(store, account, server, contact_id),
            _ => Sent::Done,
        };
        match sent {
            Sent::Done => {
                let _ = store.set_operation_state(&op.id, OperationState::Applied, None);
            }
            Sent::Failed(reason) => {
                warn!("a Microsoft contact edit for {} could not be made: {reason}", account.email);
                let _ = store.set_operation_state(&op.id, OperationState::Failed, Some(&reason));
                refused.push(carddav_sync::refusal(store, &account.id, &op.kind, &label, &reason));
            }
            Sent::Retry(error) => {
                let _ = store.set_operation_state(&op.id, OperationState::Pending, Some(&error.to_string()));
                return Err(error);
            }
        }
    }
    Ok(())
}

/// A Graph contact's id here: `graph:` and its address.
fn contact_id(url: &str) -> String {
    format!("graph:{url}")
}

fn card_from(account: &AccountId, item: &GraphItem) -> ContactCard {
    let contact_id = contact_id(&item.url);
    let base = graph::contact_from_graph(&item.contact, contact_id.clone(), account.clone(), now());
    ContactCard { contact_id, address_book: item.folder.clone(), url: item.url.clone(), etag: item.contact.change_key.clone().unwrap_or_default(), base }
}

/// After a write: Microsoft's copy becomes the version later edits are made against.
fn save_item(store: &Store, account: &Account, connection: &ContactsConnection, local_id: &str, sent: Timestamp, item: &GraphItem) -> Sent {
    match store.save_synced_contact(&account.id, &connection.server_url, local_id, sent, &card_from(&account.id, item)) {
        Ok(()) => Sent::Done,
        Err(error) => Sent::Failed(error.to_string()),
    }
}

/// Adds `contact` at Microsoft, marked with `marker`, unless an earlier try whose reply never
/// arrived already did; that copy is then brought up to date with the contact here.
fn create_once(server: &dyn GraphServer, marker: &str, contact: &Contact) -> Result<GraphItem, GraphError> {
    match server.find_created(marker)? {
        Some(found) => {
            let made = graph::contact_from_graph(&found.contact, String::new(), contact.account_id.clone(), 0);
            let patch = graph::graph_changes(&found.contact, &made, contact);
            if patch.is_empty() {
                return Ok(found);
            }
            match server.update(&found.url, &patch)? {
                Some(updated) => Ok(GraphItem { contact: updated, ..found }),
                None => server.create(marker, graph::graph_new(contact)),
            }
        }
        None => server.create(marker, graph::graph_new(contact)),
    }
}

fn create(store: &Store, account: &Account, connection: &ContactsConnection, server: &dyn GraphServer, contact_id: &str) -> Sent {
    // Sent already (the contact then has Microsoft's id), or deleted before it was sent.
    let Ok(Some(contact)) = store.contact_by_id(&account.id, contact_id) else { return Sent::Done };
    match create_once(server, contact_id, &contact) {
        Ok(item) => save_item(store, account, connection, contact_id, contact.updated, &item),
        Err(error) => error.into(),
    }
}

fn update(store: &Store, account: &Account, connection: &ContactsConnection, server: &dyn GraphServer, contact_id: &str) -> Sent {
    let Ok(Some(desired)) = store.contact_by_id(&account.id, contact_id) else { return Sent::Done };
    let Ok(Some(card)) = store.contact_card(&account.id, contact_id) else {
        // Never sent: a contact added here, so it goes as a new one.
        return if contact_id.starts_with("local:") { create(store, account, connection, server, contact_id) } else { Sent::Done };
    };
    let current = match server.get(&card.url) {
        Ok(current) => current,
        Err(error) => return error.into(),
    };
    let updated = match current {
        Some(current) => {
            let patch = graph::graph_changes(&current, &card.base, &desired);
            if patch.is_empty() {
                Some(current)
            } else {
                match server.update(&card.url, &patch) {
                    Ok(updated) => updated,
                    Err(error) => return error.into(),
                }
            }
        }
        None => None,
    };
    let item = match updated {
        Some(contact) => GraphItem { folder: card.address_book.clone(), url: card.url.clone(), contact },
        // Deleted at Microsoft since it was read: the edit brings it back, as a new contact.
        None => match create_once(server, contact_id, &desired) {
            Ok(item) => item,
            Err(error) => return error.into(),
        },
    };
    save_item(store, account, connection, contact_id, desired.updated, &item)
}

/// Deleting here wins over a change made there since: the contact goes either way.
fn delete(store: &Store, account: &Account, server: &dyn GraphServer, contact_id: &str) -> Sent {
    let Ok(Some(card)) = store.contact_card(&account.id, contact_id) else { return Sent::Done };
    match server.delete(&card.url) {
        Ok(()) => {
            let _ = store.delete_contact_card(&account.id, contact_id);
            Sent::Done
        }
        Err(error) => error.into(),
    }
}

/// Downloads every contact and replaces what was read before. Returns how many there are.
fn pull(store: &Store, account: &Account, connection: &ContactsConnection, server: &dyn GraphServer) -> Result<usize, GraphError> {
    let cards: Vec<ContactCard> = server.list()?.iter().map(|item| card_from(&account.id, item)).collect();
    store.apply_contact_download(&account.id, &connection.server_url, &cards).map_err(|_| GraphError::InvalidResponse)?;
    Ok(cards.len())
}

#[cfg(test)]
#[path = "graph_contacts_sync_tests.rs"]
mod tests;
