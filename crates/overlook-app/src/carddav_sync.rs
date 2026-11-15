//! Contacts kept at a CardDAV server, for IMAP and POP3 accounts. Fastmail is the first
//! service: its app passwords allow contacts by default, so the mail password works there,
//! once the person agrees to use it.
//!
//! Each sync sends the edits queued here first, then downloads what changed. An edit is the
//! difference between the card as it was read and the contact as edited, applied to the
//! server's current copy: fields the edit didn't touch keep the server's values, and the
//! fields Overlook doesn't show (more phones and addresses, birthdays, photos) are never
//! rewritten. A failed or partial download keeps the contacts already saved.

use crate::credentials;
use overlook_core::time::now;
use overlook_core::*;
use overlook_provider::caldav::carddav::{self, AddressBook, CardDavClient, CardDavError, CardDavSnapshot, CardRemote, WriteCondition, WriteOutcome};
use overlook_provider::caldav::vcard;
use overlook_store::Store;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::{info, warn};

/// How often the address books are checked for changes. A check lists versions only and
/// downloads just the cards that changed, so it is light.
pub const REFRESH_MS: i64 = 15 * 60 * 1000;

/// What the last contacts sync found, for the account's settings page.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContactsStatus {
    /// Why the last sync stopped, in words for the person; `None` when it finished.
    pub error: Option<String>,
    pub at: i64,
}

pub fn status_key(account: &AccountId) -> String {
    format!("contacts_status:{}", account.0)
}

pub fn status(store: &Store, account: &AccountId) -> Option<ContactsStatus> {
    store.setting::<ContactsStatus>(&status_key(account)).ok().flatten()
}

/// The contacts server for an account, judged by its incoming mail server: Fastmail's, for
/// now. An account that signs in with Microsoft has no password to reuse.
pub fn contacts_server(servers: &MailServers) -> Option<&'static str> {
    if servers.oauth.is_some() {
        return None;
    }
    carddav::fastmail_contacts_server(&servers.incoming.host)
}

/// Connects the account's contacts. Called only after the person chose to use this
/// account's mail password for its contacts. Nothing is sent until the next sync.
pub fn connect(store: &Store, account: &Account, servers: &MailServers) -> Result<ContactsConnection, String> {
    if !matches!(account.provider, ProviderKind::Imap | ProviderKind::Pop3) {
        return Err("Contacts can be connected for IMAP and POP3 accounts.".into());
    }
    let server = contacts_server(servers).ok_or("Overlook can connect contacts for Fastmail accounts.")?;
    let connection = ContactsConnection { account_id: account.id.clone(), server_url: server.into(), username: servers.incoming.username.clone(), default_book: None };
    store.save_contacts_connection(&connection).map_err(|e| e.to_string())?;
    // Check straight away rather than at the next interval.
    let _ = store.set_setting(&crate::sync::contacts_synced_key(&account.id), &0i64);
    let _ = store.delete_setting(&status_key(&account.id));
    Ok(connection)
}

/// Stops syncing contacts. The contacts read from the server leave this device; they stay
/// at the server.
pub fn disconnect(store: &Store, account: &AccountId) -> Result<(), String> {
    store.remove_contacts_connection(account).map_err(|e| e.to_string())?;
    let _ = store.delete_setting(&status_key(account));
    let _ = store.delete_setting(&crate::sync::contacts_synced_key(account));
    Ok(())
}

/// What the sync needs from a contacts server; the real client, or a fixture in tests.
pub(crate) trait ContactsServer {
    fn address_books(&self) -> Result<Vec<AddressBook>, CardDavError>;
    fn snapshot(&self, known: &dyn Fn(&str) -> Option<String>) -> Result<CardDavSnapshot, CardDavError>;
    fn get(&self, url: &str) -> Result<Option<CardRemote>, CardDavError>;
    fn put(&self, url: &str, card: &str, condition: WriteCondition<'_>) -> Result<WriteOutcome, CardDavError>;
    fn delete(&self, url: &str, etag: &str) -> Result<WriteOutcome, CardDavError>;
}

impl ContactsServer for CardDavClient {
    fn address_books(&self) -> Result<Vec<AddressBook>, CardDavError> {
        CardDavClient::address_books(self)
    }
    fn snapshot(&self, known: &dyn Fn(&str) -> Option<String>) -> Result<CardDavSnapshot, CardDavError> {
        CardDavClient::snapshot(self, known)
    }
    fn get(&self, url: &str) -> Result<Option<CardRemote>, CardDavError> {
        CardDavClient::get(self, url)
    }
    fn put(&self, url: &str, card: &str, condition: WriteCondition<'_>) -> Result<WriteOutcome, CardDavError> {
        CardDavClient::put(self, url, card, condition)
    }
    fn delete(&self, url: &str, etag: &str) -> Result<WriteOutcome, CardDavError> {
        CardDavClient::delete(self, url, etag)
    }
}

/// Runs from the mail sync. Sends queued edits whenever there are any, and checks the server
/// every [`REFRESH_MS`] or when asked to ("Sync contacts now" clears the timestamp). Errors are
/// recorded for the settings page and never change the mail sync's state.
pub fn run(store: &Store, account: &Account, connection: &ContactsConnection) {
    let key = crate::sync::contacts_synced_key(&account.id);
    let last = store.setting::<i64>(&key).ok().flatten().unwrap_or(0);
    let due = now() - last >= REFRESH_MS;
    let queued = queued_contact_edits(store, &account.id) > 0;
    if !due && !queued {
        return;
    }
    let result = (|| {
        // The mail password, which the person agreed may be used for contacts.
        let password = credentials::load_password(&account.id, credentials::KIND_PASSWORD_INCOMING).ok().flatten().ok_or(CardDavError::AuthenticationRequired)?;
        let client = CardDavClient::new(&connection.server_url, &connection.username, password)?;
        synchronize(store, account, connection, &client, due)
    })();
    let status = match result {
        Ok(()) => {
            if due {
                let _ = store.set_setting(&key, &now());
            }
            ContactsStatus { error: None, at: now() }
        }
        Err(error) => {
            warn!("contacts sync stopped for {}: {error}", account.email);
            ContactsStatus { error: Some(error.to_string()), at: now() }
        }
    };
    let _ = store.set_setting(&status_key(&account.id), &status);
}

fn queued_contact_edits(store: &Store, account: &AccountId) -> usize {
    store.pending_operations(account).map(|ops| ops.iter().filter(|op| op.kind.is_contact() && op.state != OperationState::Failed).count()).unwrap_or(0)
}

/// Sends the queued edits, then downloads when `download` is set. Stops at the first error
/// that would stop every request (offline, refused password), keeping the rest queued.
pub(crate) fn synchronize(store: &Store, account: &Account, connection: &ContactsConnection, server: &dyn ContactsServer, download: bool) -> Result<(), CardDavError> {
    send_edits(store, account, connection, server)?;
    if download {
        let count = pull(store, account, connection, server)?;
        info!("synced {count} contacts for {}", account.email);
    }
    Ok(())
}

/// How one queued edit went.
enum Sent {
    Done,
    /// Try again later; nothing further is sent this time.
    Retry(CardDavError),
    /// This edit can't be made; the rest still go.
    Failed(String),
}

impl From<CardDavError> for Sent {
    fn from(error: CardDavError) -> Self {
        match error {
            CardDavError::Network | CardDavError::RateLimited | CardDavError::ServiceUnavailable | CardDavError::AuthenticationRequired | CardDavError::Limit => Self::Retry(error),
            CardDavError::Forbidden => Self::Failed("The server doesn’t allow changes to this contact.".into()),
            other => Self::Failed(other.to_string()),
        }
    }
}

fn send_edits(store: &Store, account: &Account, connection: &ContactsConnection, server: &dyn ContactsServer) -> Result<(), CardDavError> {
    let ops = store.pending_operations(&account.id).map_err(|_| CardDavError::InvalidResponse)?;
    for op in ops.into_iter().filter(|op| op.kind.is_contact() && op.state != OperationState::Failed) {
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
                warn!("a contact edit for {} could not be made: {reason}", account.email);
                let _ = store.set_operation_state(&op.id, OperationState::Failed, Some(&reason));
            }
            Sent::Retry(error) => {
                let _ = store.set_operation_state(&op.id, OperationState::Pending, Some(&error.to_string()));
                return Err(error);
            }
        }
    }
    Ok(())
}

/// Reads a card from the server into the contact it shows, or `None` for a group or a
/// card Overlook can't read.
fn read_card(account: &AccountId, address_book: &str, url: &str, etag: String, text: &str) -> Option<ContactCard> {
    let card = match vcard::Card::parse(text) {
        Ok(card) => card,
        Err(error) => {
            warn!("skipped a contact card that could not be read: {error}");
            return None;
        }
    };
    let fields = card.fields();
    if fields.is_group {
        return None;
    }
    let contact_id = format!("carddav:{url}");
    let base = vcard::contact_from_fields(&fields, contact_id.clone(), account.clone(), Some(etag.clone()), now());
    Some(ContactCard { contact_id, address_book: address_book.to_owned(), url: url.to_owned(), etag, base })
}

/// After a write: the server's copy becomes the version later edits are made against.
fn save_from_server(store: &Store, account: &Account, local_id: &str, sent: i64, address_book: &str, url: &str, server: &dyn ContactsServer) -> Sent {
    let remote = match server.get(url) {
        Ok(Some(remote)) => remote,
        Ok(None) => return Sent::Failed("The server didn’t keep the contact.".into()),
        Err(error) => return error.into(),
    };
    let Some(card) = read_card(&account.id, address_book, url, remote.etag, &remote.card) else {
        return Sent::Failed("The server changed the contact into something Overlook can’t read.".into());
    };
    match store.save_synced_contact(&account.id, local_id, sent, &card) {
        Ok(()) => Sent::Done,
        Err(error) => Sent::Failed(error.to_string()),
    }
}

/// The address book new contacts go to: the saved one, or the first that can be written to,
/// Fastmail's "Default" book ahead of others.
fn default_book(store: &Store, connection: &ContactsConnection, server: &dyn ContactsServer) -> Result<String, Sent> {
    if let Some(book) = &connection.default_book {
        return Ok(book.clone());
    }
    let books = server.address_books().map_err(Sent::from)?;
    let book = choose_default_book(&books).ok_or_else(|| Sent::Failed("There’s no address book here that new contacts can be added to.".into()))?;
    let _ = store.set_contacts_default_book(&connection.account_id, &book);
    Ok(book)
}

fn choose_default_book(books: &[AddressBook]) -> Option<String> {
    let writable: Vec<&AddressBook> = books.iter().filter(|b| b.writable).collect();
    writable.iter().find(|b| b.url.trim_end_matches('/').ends_with("/Default")).or(writable.first()).map(|b| b.url.clone())
}

fn card_url(book: &str, uid: &str) -> String {
    format!("{}/{uid}.vcf", book.trim_end_matches('/'))
}

fn create(store: &Store, account: &Account, connection: &ContactsConnection, server: &dyn ContactsServer, contact_id: &str) -> Sent {
    // Sent already (the contact then has the server's id), or deleted before it was sent.
    let Ok(Some(contact)) = store.contact_by_id(&account.id, contact_id) else { return Sent::Done };
    let book = match default_book(store, connection, server) {
        Ok(book) => book,
        Err(sent) => return sent,
    };
    let uid = new_provider_event_id();
    let url = card_url(&book, &uid);
    match server.put(&url, &vcard::new_card(&contact, &uid, now()), WriteCondition::Create) {
        Ok(WriteOutcome::Done) => save_from_server(store, account, contact_id, contact.updated, &book, &url, server),
        Ok(WriteOutcome::Conflict) => Sent::Failed("A contact with the same id is already on the server.".into()),
        Ok(WriteOutcome::Missing) => {
            // The address book is gone: find another next time.
            let _ = store.save_contacts_connection(&ContactsConnection { default_book: None, ..connection.clone() });
            Sent::Retry(CardDavError::UnexpectedStatus(404))
        }
        Err(error) => error.into(),
    }
}

fn update(store: &Store, account: &Account, connection: &ContactsConnection, server: &dyn ContactsServer, contact_id: &str) -> Sent {
    let Ok(Some(desired)) = store.contact_by_id(&account.id, contact_id) else { return Sent::Done };
    let Ok(Some(card)) = store.contact_card(&account.id, contact_id) else {
        // Never sent: a contact added here, so it goes as a new one.
        return if contact_id.starts_with("local:") { create(store, account, connection, server, contact_id) } else { Sent::Done };
    };
    // A copy that changes between reading and writing is read again; three tries are plenty.
    for _ in 0..3 {
        let remote = match server.get(&card.url) {
            Ok(remote) => remote,
            Err(error) => return error.into(),
        };
        let (text, condition) = match &remote {
            Some(remote) => match vcard::apply_changes(&remote.card, &card.base, &desired, now()) {
                Ok(text) if text == remote.card => return save_from_server(store, account, contact_id, desired.updated, &card.address_book, &card.url, server),
                Ok(text) => (text, WriteCondition::Replace(&remote.etag)),
                Err(error) => return Sent::Failed(error.to_string()),
            },
            // Deleted at the server since it was read: the edit brings it back.
            None => (vcard::new_card(&desired, &new_provider_event_id(), now()), WriteCondition::Create),
        };
        match server.put(&card.url, &text, condition) {
            Ok(WriteOutcome::Done) => return save_from_server(store, account, contact_id, desired.updated, &card.address_book, &card.url, server),
            Ok(WriteOutcome::Conflict | WriteOutcome::Missing) => continue,
            Err(error) => return error.into(),
        }
    }
    Sent::Retry(CardDavError::ServiceUnavailable)
}

/// Deleting here wins over a change made there since: the card goes either way.
fn delete(store: &Store, account: &Account, server: &dyn ContactsServer, contact_id: &str) -> Sent {
    let Ok(Some(card)) = store.contact_card(&account.id, contact_id) else { return Sent::Done };
    let mut etag = card.etag.clone();
    for _ in 0..3 {
        match server.delete(&card.url, &etag) {
            Ok(WriteOutcome::Done | WriteOutcome::Missing) => {
                let _ = store.delete_contact_card(&account.id, contact_id);
                return Sent::Done;
            }
            Ok(WriteOutcome::Conflict) => match server.get(&card.url) {
                Ok(Some(remote)) => etag = remote.etag,
                Ok(None) => {
                    let _ = store.delete_contact_card(&account.id, contact_id);
                    return Sent::Done;
                }
                Err(error) => return error.into(),
            },
            Err(error) => return error.into(),
        }
    }
    Sent::Retry(CardDavError::ServiceUnavailable)
}

/// Downloads every address book in full (only changed cards travel) and replaces what was
/// read before. Returns how many contacts there are.
fn pull(store: &Store, account: &Account, connection: &ContactsConnection, server: &dyn ContactsServer) -> Result<usize, CardDavError> {
    let saved: HashMap<String, ContactCard> = store.contact_cards(&account.id).map_err(|_| CardDavError::InvalidResponse)?.into_iter().map(|c| (c.url.clone(), c)).collect();
    let snapshot = server.snapshot(&|url| saved.get(url).map(|c| c.etag.clone()))?;
    let mut cards: Vec<ContactCard> = snapshot.unchanged.iter().filter_map(|u| saved.get(&u.url).cloned()).collect();
    cards.extend(snapshot.cards.iter().filter_map(|r| read_card(&account.id, &r.address_book, &r.url, r.etag.clone(), &r.card)));
    store.apply_contact_download(&account.id, &cards).map_err(|_| CardDavError::InvalidResponse)?;
    let still_there = connection.default_book.as_ref().is_some_and(|b| snapshot.address_books.iter().any(|a| &a.url == b && a.writable));
    if !still_there {
        if let Some(book) = choose_default_book(&snapshot.address_books) {
            let _ = store.set_contacts_default_book(&account.id, &book);
        }
    }
    Ok(cards.len())
}

#[cfg(test)]
#[path = "carddav_sync_tests.rs"]
mod tests;
