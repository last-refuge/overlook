//! The contacts sync against an in-memory CardDAV server: downloads, edits that keep what
//! Overlook doesn't show, changes made elsewhere meanwhile, new and deleted contacts, and a
//! connection that drops.

use super::*;
use overlook_provider::caldav::carddav::{CardResource, UnchangedCard};
use std::cell::RefCell;
use std::collections::BTreeMap;

const BOOK: &str = "https://contacts.example.test/dav/addressbooks/user/me/Default/";

fn card(uid: &str, name: &str, extra: &str) -> String {
    let (given, family) = name.split_once(' ').unwrap_or((name, ""));
    format!("BEGIN:VCARD\r\nVERSION:3.0\r\nUID:{uid}\r\nFN:{name}\r\nN:{family};{given};;;\r\nEMAIL;TYPE=INTERNET:{uid}@example.test\r\n{extra}END:VCARD\r\n")
}

/// Holds cards by address with a version each, like a CardDAV collection.
#[derive(Default)]
struct Server {
    cards: RefCell<BTreeMap<String, (String, String)>>,
    version: RefCell<u32>,
    offline: RefCell<bool>,
    writes: RefCell<usize>,
}

impl Server {
    fn next_etag(&self) -> String {
        *self.version.borrow_mut() += 1;
        format!("\"e{}\"", self.version.borrow())
    }

    /// A change made elsewhere, by a phone say.
    fn store(&self, uid: &str, text: String) -> String {
        let url = format!("{BOOK}{uid}.vcf");
        let etag = self.next_etag();
        self.cards.borrow_mut().insert(url.clone(), (etag, text));
        url
    }

    fn text(&self, url: &str) -> String {
        self.cards.borrow()[url].1.clone()
    }

    fn check(&self) -> Result<(), CardDavError> {
        if *self.offline.borrow() { Err(CardDavError::Network) } else { Ok(()) }
    }
}

impl ContactsServer for Server {
    fn address_books(&self) -> Result<Vec<AddressBook>, CardDavError> {
        self.check()?;
        Ok(vec![AddressBook { url: BOOK.into(), name: "Personal".into(), writable: true }])
    }

    fn snapshot(&self, known: &dyn Fn(&str) -> Option<String>) -> Result<CardDavSnapshot, CardDavError> {
        let address_books = self.address_books()?;
        let (mut cards, mut unchanged) = (Vec::new(), Vec::new());
        for (url, (etag, text)) in self.cards.borrow().iter() {
            if known(url).as_deref() == Some(etag.as_str()) {
                unchanged.push(UnchangedCard { address_book: BOOK.into(), url: url.clone(), etag: etag.clone() });
            } else {
                cards.push(CardResource { address_book: BOOK.into(), url: url.clone(), etag: etag.clone(), card: text.clone() });
            }
        }
        Ok(CardDavSnapshot { address_books, cards, unchanged })
    }

    fn get(&self, url: &str) -> Result<Option<CardRemote>, CardDavError> {
        self.check()?;
        Ok(self.cards.borrow().get(url).map(|(etag, card)| CardRemote { etag: etag.clone(), card: card.clone() }))
    }

    fn put(&self, url: &str, card: &str, condition: WriteCondition<'_>) -> Result<WriteOutcome, CardDavError> {
        self.check()?;
        let current = self.cards.borrow().get(url).map(|(etag, _)| etag.clone());
        let outcome = match (condition, current) {
            (WriteCondition::Create, Some(_)) => return Ok(WriteOutcome::Conflict),
            (WriteCondition::Replace(_), None) => return Ok(WriteOutcome::Missing),
            (WriteCondition::Replace(expected), Some(etag)) if expected != etag => return Ok(WriteOutcome::Conflict),
            _ => WriteOutcome::Done,
        };
        let etag = self.next_etag();
        self.cards.borrow_mut().insert(url.to_owned(), (etag, card.to_owned()));
        *self.writes.borrow_mut() += 1;
        Ok(outcome)
    }

    fn delete(&self, url: &str, etag: &str) -> Result<WriteOutcome, CardDavError> {
        self.check()?;
        let current = self.cards.borrow().get(url).map(|(e, _)| e.clone());
        match current {
            None => Ok(WriteOutcome::Missing),
            Some(e) if e != etag => Ok(WriteOutcome::Conflict),
            Some(_) => {
                self.cards.borrow_mut().remove(url);
                *self.writes.borrow_mut() += 1;
                Ok(WriteOutcome::Done)
            }
        }
    }
}

fn setup() -> (Store, Account, ContactsConnection) {
    let store = Store::open_in_memory().unwrap();
    let account = Account { id: AccountId::from("imap:fastmail-fixture"), provider: ProviderKind::Imap, display_name: "Test".into(), email: "me@example.test".into(), created_at: 0 };
    store.upsert_account(&account).unwrap();
    let connection = ContactsConnection { account_id: account.id.clone(), server_url: "https://contacts.example.test/".into(), username: "me@example.test".into(), default_book: None };
    store.save_contacts_connection(&connection).unwrap();
    (store, account, connection)
}

fn connection(store: &Store, account: &Account) -> ContactsConnection {
    store.contacts_connection(&account.id).unwrap().unwrap()
}

fn names(store: &Store, account: &Account) -> Vec<String> {
    store.contacts(&account.id).unwrap().into_iter().map(|c| c.display_name()).collect()
}

#[test]
fn downloads_contacts_skips_groups_and_picks_where_new_ones_go() {
    let (store, account, conn) = setup();
    let server = Server::default();
    server.store("maya", card("maya", "Maya Chen", ""));
    server.store("alex", card("alex", "Alex Rivera", ""));
    server.store("team", "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:team\r\nFN:Studio\r\nX-ADDRESSBOOKSERVER-KIND:group\r\nEND:VCARD\r\n".into());
    synchronize(&store, &account, &conn, &server, true).unwrap();
    assert_eq!(names(&store, &account), vec!["Alex Rivera", "Maya Chen"]);
    let maya = store.contacts(&account.id).unwrap().into_iter().find(|c| c.display_name() == "Maya Chen").unwrap();
    assert!(maya.id.starts_with("carddav:https://contacts.example.test/") && maya.is_editable() && !maya.pending);
    assert_eq!(connection(&store, &account).default_book.as_deref(), Some(BOOK));
}

#[test]
fn an_edit_reaches_the_server_without_losing_what_overlook_doesnt_show() {
    let (store, account, conn) = setup();
    let server = Server::default();
    let extra = "TEL;TYPE=CELL:+1 555 0100\r\nTEL;TYPE=HOME:+1 555 0199\r\nBDAY:1988-04-12\r\nPHOTO;ENCODING=b;TYPE=JPEG:/9j/4AAQSkZJRgABAQ\r\nTITLE:Designer\r\n";
    let url = server.store("maya", card("maya", "Maya Chen", extra));
    synchronize(&store, &account, &conn, &server, true).unwrap();

    let mut maya = store.contacts(&account.id).unwrap().remove(0);
    maya.title = Some("Design Director".into());
    maya.phone = Some("+1 555 0111".into());
    store.save_contact_locally(&account.id, &maya).unwrap();
    synchronize(&store, &account, &connection(&store, &account), &server, false).unwrap();

    let text = server.text(&url);
    assert!(text.contains("TITLE:Design Director\r\n") && text.contains("TEL;TYPE=CELL:+1 555 0111\r\n"));
    for kept in ["TEL;TYPE=HOME:+1 555 0199\r\n", "BDAY:1988-04-12\r\n", "PHOTO;ENCODING=b;TYPE=JPEG:/9j/4AAQSkZJRgABAQ\r\n", "EMAIL;TYPE=INTERNET:maya@example.test\r\n"] {
        assert!(text.contains(kept), "kept {kept:?}");
    }
    let saved = store.contact_by_id(&account.id, &maya.id).unwrap().unwrap();
    assert!(!saved.pending);
    assert_eq!(store.contact_card(&account.id, &maya.id).unwrap().unwrap().etag, server.cards.borrow()[&url].0, "later edits start from the new version");
    assert!(store.pending_operations(&account.id).unwrap().is_empty());
}

#[test]
fn a_change_made_elsewhere_meanwhile_survives_an_edit_made_here() {
    let (store, account, conn) = setup();
    let server = Server::default();
    let url = server.store("maya", card("maya", "Maya Chen", "TITLE:Designer\r\n"));
    synchronize(&store, &account, &conn, &server, true).unwrap();

    // A phone adds a note; here the title changes before the next sync.
    server.store("maya", card("maya", "Maya Chen", "TITLE:Designer\r\nNOTE:Prefers mornings\r\n"));
    let mut maya = store.contacts(&account.id).unwrap().remove(0);
    maya.title = Some("Design Director".into());
    store.save_contact_locally(&account.id, &maya).unwrap();
    synchronize(&store, &account, &connection(&store, &account), &server, true).unwrap();

    let text = server.text(&url);
    assert!(text.contains("NOTE:Prefers mornings\r\n") && text.contains("TITLE:Design Director\r\n"), "{text}");
    let shown = store.contact_by_id(&account.id, &maya.id).unwrap().unwrap();
    assert_eq!((shown.title.as_deref(), shown.notes.as_deref()), (Some("Design Director"), Some("Prefers mornings")));
}

#[test]
fn a_contact_added_here_is_created_in_the_default_book() {
    let (store, account, conn) = setup();
    let server = Server::default();
    let mut sophie = Contact::new_local(account.id.clone(), now());
    sophie.given_name = Some("Sophie".into());
    sophie.family_name = Some("Williams".into());
    sophie.name = Some("Sophie Williams".into());
    sophie.email = "sophie@example.test".into();
    sophie.emails = vec![sophie.email.clone()];
    store.save_contact_locally(&account.id, &sophie).unwrap();
    synchronize(&store, &account, &conn, &server, false).unwrap();

    let cards = server.cards.borrow().clone();
    assert_eq!(cards.len(), 1);
    let (url, (_, text)) = cards.iter().next().unwrap();
    assert!(url.starts_with(BOOK) && url.ends_with(".vcf"));
    assert!(text.contains("FN:Sophie Williams\r\n") && text.contains("EMAIL;TYPE=INTERNET:sophie@example.test\r\n"));
    let shown = store.contacts(&account.id).unwrap();
    assert_eq!(shown.len(), 1);
    assert_eq!(shown[0].id, format!("carddav:{url}"), "the contact takes the server's id");
    assert!(!shown[0].pending);
}

#[test]
fn a_contact_deleted_here_is_deleted_there_and_stays_deleted() {
    let (store, account, conn) = setup();
    let server = Server::default();
    server.store("maya", card("maya", "Maya Chen", ""));
    server.store("alex", card("alex", "Alex Rivera", ""));
    synchronize(&store, &account, &conn, &server, true).unwrap();
    let alex = store.contacts(&account.id).unwrap().into_iter().find(|c| c.display_name() == "Alex Rivera").unwrap();
    store.delete_contact_locally(&account.id, &alex).unwrap();
    synchronize(&store, &account, &connection(&store, &account), &server, true).unwrap();
    assert_eq!(server.cards.borrow().len(), 1);
    assert_eq!(names(&store, &account), vec!["Maya Chen"]);
    assert!(store.contact_card(&account.id, &alex.id).unwrap().is_none());
}

#[test]
fn a_dropped_connection_keeps_edits_queued_and_contacts_saved() {
    let (store, account, conn) = setup();
    let server = Server::default();
    let url = server.store("maya", card("maya", "Maya Chen", ""));
    synchronize(&store, &account, &conn, &server, true).unwrap();
    let mut maya = store.contacts(&account.id).unwrap().remove(0);
    maya.title = Some("Director".into());
    store.save_contact_locally(&account.id, &maya).unwrap();

    *server.offline.borrow_mut() = true;
    assert_eq!(synchronize(&store, &account, &connection(&store, &account), &server, true).unwrap_err(), CardDavError::Network);
    assert_eq!(names(&store, &account), vec!["Maya Chen"], "a failed download deletes nothing");
    let ops = store.pending_operations(&account.id).unwrap();
    assert_eq!(ops.len(), 1);
    assert_eq!(ops[0].state, OperationState::Pending, "the edit waits for the connection");
    assert_eq!(*server.writes.borrow(), 0);

    *server.offline.borrow_mut() = false;
    synchronize(&store, &account, &connection(&store, &account), &server, true).unwrap();
    assert!(server.text(&url).contains("TITLE:Director\r\n"));
    assert!(store.pending_operations(&account.id).unwrap().is_empty());
}

#[test]
fn contacts_connect_only_for_fastmail_mail_servers() {
    use overlook_core::servers::{MailServers, Security, ServerEndpoint};
    let servers = |host: &str| MailServers {
        incoming: ServerEndpoint::new(host, 993, Security::Tls, "me@example.test"),
        outgoing: ServerEndpoint::new("smtp.example.test", 465, Security::Tls, "me@example.test"),
        pop3: None,
        imap: None,
        oauth: None,
    };
    assert_eq!(contacts_server(&servers("imap.fastmail.com")), Some(carddav::FASTMAIL_CARDDAV_URL));
    assert_eq!(contacts_server(&servers("imap.example.test")), None);
    let mut microsoft = servers("imap.fastmail.com");
    microsoft.oauth = Some(overlook_core::servers::OAuthIssuer::Microsoft);
    assert_eq!(contacts_server(&microsoft), None, "no password to reuse");

    let (store, account, _) = setup();
    store.remove_contacts_connection(&account.id).unwrap();
    assert!(connect(&store, &account, &servers("imap.example.test")).is_err());
    let connected = connect(&store, &account, &servers("imap.fastmail.com")).unwrap();
    assert_eq!((connected.server_url.as_str(), connected.username.as_str()), (carddav::FASTMAIL_CARDDAV_URL, "me@example.test"));
    disconnect(&store, &account.id).unwrap();
    assert!(store.contacts_connection(&account.id).unwrap().is_none());
}
