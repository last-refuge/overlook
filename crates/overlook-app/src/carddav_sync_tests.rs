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
struct Server<'a> {
    cards: RefCell<BTreeMap<String, (String, String)>>,
    version: RefCell<u32>,
    offline: RefCell<bool>,
    writes: RefCell<usize>,
    /// The next write is made, but its reply is lost on the way back.
    lose_reply: RefCell<bool>,
    /// Cards that can't be changed or deleted, as in a read-only shared book.
    read_only: RefCell<Vec<String>>,
    /// Runs while the server lists its cards: something done here meanwhile.
    on_snapshot: RefCell<Option<Box<dyn FnOnce() + 'a>>>,
}

impl Server<'_> {
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

impl ContactsServer for Server<'_> {
    fn address_books(&self) -> Result<Vec<AddressBook>, CardDavError> {
        self.check()?;
        Ok(vec![AddressBook { url: BOOK.into(), name: "Personal".into(), writable: true }])
    }

    fn snapshot(&self, known: &dyn Fn(&str) -> Option<String>) -> Result<CardDavSnapshot, CardDavError> {
        let address_books = self.address_books()?;
        if let Some(meanwhile) = self.on_snapshot.borrow_mut().take() {
            meanwhile();
        }
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
        if self.read_only.borrow().iter().any(|u| u == url) {
            return Err(CardDavError::Forbidden);
        }
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
        if std::mem::take(&mut *self.lose_reply.borrow_mut()) {
            return Err(CardDavError::Network);
        }
        Ok(outcome)
    }

    fn delete(&self, url: &str, etag: &str) -> Result<WriteOutcome, CardDavError> {
        self.check()?;
        if self.read_only.borrow().iter().any(|u| u == url) {
            return Err(CardDavError::Forbidden);
        }
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
    let connection = ContactsConnection { account_id: account.id.clone(), server_url: "https://contacts.example.test/".into(), username: "me@example.test".into(), default_book: None, credential: ContactsCredential::IncomingMail, connected_at: 0 };
    store.save_contacts_connection(&connection).unwrap();
    // As a sync has it: read back, with the time it was made.
    let connection = store.contacts_connection(&account.id).unwrap().unwrap();
    (store, account, connection)
}

fn connection(store: &Store, account: &Account) -> ContactsConnection {
    store.contacts_connection(&account.id).unwrap().unwrap()
}

/// One sync; returns the edits the server refused for good.
fn sync(store: &Store, account: &Account, connection: &ContactsConnection, server: &dyn ContactsServer, download: bool) -> Result<Vec<String>, CardDavError> {
    let mut refused = Vec::new();
    synchronize(store, account, connection, server, download, &mut refused).map(|()| refused)
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
    sync(&store, &account, &conn, &server, true).unwrap();
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
    sync(&store, &account, &conn, &server, true).unwrap();

    let mut maya = store.contacts(&account.id).unwrap().remove(0);
    maya.title = Some("Design Director".into());
    maya.phone = Some("+1 555 0111".into());
    store.save_contact_locally(&account.id, &maya).unwrap();
    sync(&store, &account, &connection(&store, &account), &server, false).unwrap();

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
    sync(&store, &account, &conn, &server, true).unwrap();

    // A phone adds a note; here the title changes before the next sync.
    server.store("maya", card("maya", "Maya Chen", "TITLE:Designer\r\nNOTE:Prefers mornings\r\n"));
    let mut maya = store.contacts(&account.id).unwrap().remove(0);
    maya.title = Some("Design Director".into());
    store.save_contact_locally(&account.id, &maya).unwrap();
    sync(&store, &account, &connection(&store, &account), &server, true).unwrap();

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
    sync(&store, &account, &conn, &server, false).unwrap();

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
    sync(&store, &account, &conn, &server, true).unwrap();
    let alex = store.contacts(&account.id).unwrap().into_iter().find(|c| c.display_name() == "Alex Rivera").unwrap();
    store.delete_contact_locally(&account.id, &alex).unwrap();
    sync(&store, &account, &connection(&store, &account), &server, true).unwrap();
    assert_eq!(server.cards.borrow().len(), 1);
    assert_eq!(names(&store, &account), vec!["Maya Chen"]);
    assert!(store.contact_card(&account.id, &alex.id).unwrap().is_none());
}

#[test]
fn a_dropped_connection_keeps_edits_queued_and_contacts_saved() {
    let (store, account, conn) = setup();
    let server = Server::default();
    let url = server.store("maya", card("maya", "Maya Chen", ""));
    sync(&store, &account, &conn, &server, true).unwrap();
    let mut maya = store.contacts(&account.id).unwrap().remove(0);
    maya.title = Some("Director".into());
    store.save_contact_locally(&account.id, &maya).unwrap();

    *server.offline.borrow_mut() = true;
    assert_eq!(sync(&store, &account, &connection(&store, &account), &server, true).unwrap_err(), CardDavError::Network);
    assert_eq!(names(&store, &account), vec!["Maya Chen"], "a failed download deletes nothing");
    let ops = store.pending_operations(&account.id).unwrap();
    assert_eq!(ops.len(), 1);
    assert_eq!(ops[0].state, OperationState::Pending, "the edit waits for the connection");
    assert_eq!(*server.writes.borrow(), 0);

    *server.offline.borrow_mut() = false;
    sync(&store, &account, &connection(&store, &account), &server, true).unwrap();
    assert!(server.text(&url).contains("TITLE:Director\r\n"));
    assert!(store.pending_operations(&account.id).unwrap().is_empty());
}

#[test]
fn a_create_retried_after_a_lost_reply_finds_its_first_card() {
    let (store, account, conn) = setup();
    let server = Server::default();
    let mut sophie = Contact::new_local(account.id.clone(), now());
    sophie.name = Some("Sophie Williams".into());
    store.save_contact_locally(&account.id, &sophie).unwrap();
    // The card is made, but the reply never arrives: the edit stays queued.
    *server.lose_reply.borrow_mut() = true;
    assert_eq!(sync(&store, &account, &conn, &server, false).unwrap_err(), CardDavError::Network);
    assert_eq!(server.cards.borrow().len(), 1);
    // Edited again before the retry: the card made then is brought up to date, not taken as is.
    let mut again = store.contact_by_id(&account.id, &sophie.id).unwrap().unwrap();
    again.title = Some("Producer".into());
    store.save_contact_locally(&account.id, &again).unwrap();
    sync(&store, &account, &connection(&store, &account), &server, true).unwrap();
    assert_eq!(server.cards.borrow().len(), 1, "no second copy");
    let url = server.cards.borrow().keys().next().unwrap().clone();
    assert!(server.text(&url).contains("TITLE:Producer\r\n"), "{}", server.text(&url));
    let shown = store.contacts(&account.id).unwrap();
    assert_eq!(shown.len(), 1);
    assert_eq!((shown[0].id.clone(), shown[0].title.as_deref(), shown[0].pending), (format!("carddav:{url}"), Some("Producer"), false));
    assert!(store.pending_operations(&account.id).unwrap().iter().all(|op| op.state == OperationState::Applied));
}

#[test]
fn a_refused_edit_is_reported_and_the_server_version_comes_back() {
    let (store, account, conn) = setup();
    let server = Server::default();
    let url = server.store("maya", card("maya", "Maya Chen", "TITLE:Designer\r\n"));
    sync(&store, &account, &conn, &server, true).unwrap();
    server.read_only.borrow_mut().push(url.clone());
    let mut maya = store.contacts(&account.id).unwrap().remove(0);
    maya.title = Some("Design Director".into());
    store.save_contact_locally(&account.id, &maya).unwrap();
    let refused = sync(&store, &account, &connection(&store, &account), &server, true).unwrap();
    assert_eq!(refused, vec!["changes to Maya Chen: The server doesn’t allow changes to this contact. It shows the server’s version again.".to_owned()]);
    let shown = store.contact_by_id(&account.id, &maya.id).unwrap().unwrap();
    assert_eq!((shown.title.as_deref(), shown.pending), (Some("Designer"), false));
    // Nothing holds it now: a change made at the server arrives.
    server.store("maya", card("maya", "Maya Chen", "TITLE:Head of Design\r\n"));
    assert!(sync(&store, &account, &connection(&store, &account), &server, true).unwrap().is_empty(), "a refused edit isn't sent again");
    assert_eq!(store.contact_by_id(&account.id, &maya.id).unwrap().unwrap().title.as_deref(), Some("Head of Design"));
    // A refused deletion brings the contact back.
    let maya = store.contact_by_id(&account.id, &maya.id).unwrap().unwrap();
    store.delete_contact_locally(&account.id, &maya).unwrap();
    let refused = sync(&store, &account, &connection(&store, &account), &server, false).unwrap();
    assert!(refused[0].starts_with("deleting Maya Chen: ") && refused[0].ends_with("It’s back in your contacts."), "{refused:?}");
    assert_eq!(names(&store, &account), vec!["Maya Chen"]);
}

#[test]
fn contacts_downloaded_after_disconnecting_dont_come_back() {
    let (store, account, conn) = setup();
    let server = Server::default();
    server.store("maya", card("maya", "Maya Chen", ""));
    let (shared, id) = (&store, account.id.clone());
    *server.on_snapshot.borrow_mut() = Some(Box::new(move || disconnect(shared, &id).unwrap()));
    sync(&store, &account, &conn, &server, true).unwrap();
    assert!(store.contacts(&account.id).unwrap().is_empty());
    assert!(store.contact_cards(&account.id).unwrap().is_empty());
    record_status(&store, &conn, &ContactsStatus { error: None, at: 1 });
    assert_eq!(status(&store, &account.id), None, "no status for a connection that's gone");
}

#[test]
fn contacts_connect_for_the_services_overlook_knows() {
    use overlook_core::servers::{MailServers, Security, ServerEndpoint};
    let servers = |host: &str| MailServers {
        incoming: ServerEndpoint::new(host, 993, Security::Tls, "me@example.test"),
        outgoing: ServerEndpoint::new("smtp.example.test", 465, Security::Tls, "me@example.test"),
        pop3: None,
        imap: None,
        oauth: None,
    };
    assert_eq!(contacts_service(&servers("imap.fastmail.com")).map(|s| s.server_url), Some(carddav::FASTMAIL_CARDDAV_URL));
    assert_eq!(contacts_service(&servers("imap.mail.me.com")).map(|s| s.name), Some("iCloud"));
    assert_eq!(contacts_service(&servers("imap.mail.yahoo.com")).map(|s| s.name), Some("Yahoo"));
    assert_eq!(contacts_service(&servers("imap.aol.com")).map(|s| s.name), Some("AOL"));
    assert_eq!(contacts_service(&servers("imap.example.test")), None);
    let mut microsoft = servers("imap.fastmail.com");
    microsoft.oauth = Some(overlook_core::servers::OAuthIssuer::Microsoft);
    assert_eq!(contacts_service(&microsoft), None, "no password to reuse");
    // iCloud signs in to mail with the name alone but to contacts with the whole address.
    let mut icloud = servers("imap.mail.me.com");
    icloud.incoming.username = "me".into();
    icloud.outgoing.username = "me@icloud.com".into();
    assert_eq!(user_name(&carddav::ICLOUD_CONTACTS, &icloud), "me@icloud.com");
    assert_eq!(user_name(&carddav::FASTMAIL_CONTACTS, &servers("imap.fastmail.com")), "me@example.test");

    let (store, account, _) = setup();
    store.remove_contacts_connection(&account.id).unwrap();
    assert!(connect(&store, &account, &servers("imap.example.test")).is_err());
    let connected = connect(&store, &account, &servers("imap.fastmail.com")).unwrap();
    assert_eq!((connected.server_url.as_str(), connected.username.as_str()), (carddav::FASTMAIL_CARDDAV_URL, "me@example.test"));
    disconnect(&store, &account.id).unwrap();
    assert!(store.contacts_connection(&account.id).unwrap().is_none());
}
