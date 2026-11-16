//! The Microsoft contacts sync against an in-memory stand-in for Graph: downloads from every
//! folder, edits that send only what changed, new contacts made once, deletions, refusals and
//! a connection that drops.

use super::*;
use serde_json::json;
use std::cell::RefCell;
use std::collections::BTreeMap;

const DEFAULT: &str = "https://graph.microsoft.com/v1.0/me/contacts";
const STUDIO: &str = "https://graph.microsoft.com/v1.0/me/contactFolders/AAMkStudio/contacts";

/// Contacts as Graph keeps them: JSON objects by address. A PATCH replaces the fields it
/// names and leaves the rest.
#[derive(Default)]
struct Server {
    contacts: RefCell<BTreeMap<String, Value>>,
    markers: RefCell<BTreeMap<String, String>>,
    version: RefCell<u32>,
    offline: RefCell<bool>,
    /// The next new contact is made, but the reply is lost on the way back.
    lose_reply: RefCell<bool>,
    read_only: RefCell<Vec<String>>,
    patches: RefCell<Vec<Value>>,
}

impl Server {
    fn next(&self) -> u32 {
        *self.version.borrow_mut() += 1;
        *self.version.borrow()
    }

    /// A contact made elsewhere, in Outlook say.
    fn add(&self, folder: &str, id: &str, fields: Value) -> String {
        let url = format!("{folder}/{id}");
        let mut contact = fields;
        contact["id"] = json!(id);
        contact["changeKey"] = json!(format!("ck{}", self.next()));
        self.contacts.borrow_mut().insert(url.clone(), contact);
        url
    }

    fn field(&self, url: &str, name: &str) -> Value {
        self.contacts.borrow()[url][name].clone()
    }

    fn check(&self) -> Result<(), GraphError> {
        if *self.offline.borrow() { Err(GraphError::Network) } else { Ok(()) }
    }

    fn read(value: &Value) -> GraphContact {
        serde_json::from_value(value.clone()).unwrap()
    }

    fn item(url: &str, value: &Value) -> GraphItem {
        GraphItem { folder: url.rsplit_once('/').unwrap().0.to_owned(), url: url.to_owned(), contact: Self::read(value) }
    }
}

impl GraphServer for Server {
    fn list(&self) -> Result<Vec<GraphItem>, GraphError> {
        self.check()?;
        Ok(self.contacts.borrow().iter().map(|(url, c)| Self::item(url, c)).collect())
    }

    fn get(&self, url: &str) -> Result<Option<GraphContact>, GraphError> {
        self.check()?;
        Ok(self.contacts.borrow().get(url).map(Self::read))
    }

    fn create(&self, marker: &str, body: Map<String, Value>) -> Result<GraphItem, GraphError> {
        self.check()?;
        let id = format!("c{}", self.next());
        let url = self.add(DEFAULT, &id, Value::Object(body));
        self.markers.borrow_mut().insert(marker.to_owned(), url.clone());
        if std::mem::take(&mut *self.lose_reply.borrow_mut()) {
            return Err(GraphError::Network);
        }
        Ok(Self::item(&url, &self.contacts.borrow()[&url]))
    }

    fn find_created(&self, marker: &str) -> Result<Option<GraphItem>, GraphError> {
        self.check()?;
        let url = self.markers.borrow().get(marker).cloned();
        Ok(url.and_then(|url| self.contacts.borrow().get(&url).map(|c| Self::item(&url, c))))
    }

    fn update(&self, url: &str, patch: &Map<String, Value>) -> Result<Option<GraphContact>, GraphError> {
        self.check()?;
        if self.read_only.borrow().iter().any(|u| u == url) {
            return Err(GraphError::Forbidden);
        }
        self.patches.borrow_mut().push(Value::Object(patch.clone()));
        let key = format!("ck{}", self.next());
        let mut contacts = self.contacts.borrow_mut();
        let Some(contact) = contacts.get_mut(url) else { return Ok(None) };
        for (name, value) in patch {
            contact[name] = value.clone();
        }
        contact["changeKey"] = json!(key);
        Ok(Some(Self::read(contact)))
    }

    fn delete(&self, url: &str) -> Result<(), GraphError> {
        self.check()?;
        if self.read_only.borrow().iter().any(|u| u == url) {
            return Err(GraphError::Forbidden);
        }
        self.contacts.borrow_mut().remove(url);
        Ok(())
    }
}

fn setup() -> (Store, Account, ContactsConnection) {
    let store = Store::open_in_memory().unwrap();
    let account = Account { id: AccountId::from("imap:outlook-fixture"), provider: ProviderKind::Imap, display_name: "Test".into(), email: "mara@outlook-fixture.example".into(), created_at: 0 };
    store.upsert_account(&account).unwrap();
    let connection = ContactsConnection { account_id: account.id.clone(), server_url: graph::GRAPH_CONTACTS_URL.into(), username: account.email.clone(), default_book: None, credential: ContactsCredential::Microsoft, connected_at: 0 };
    store.save_contacts_connection(&connection).unwrap();
    let connection = store.contacts_connection(&account.id).unwrap().unwrap();
    (store, account, connection)
}

fn sync(store: &Store, account: &Account, connection: &ContactsConnection, server: &Server, download: bool) -> Result<Vec<String>, GraphError> {
    let mut refused = Vec::new();
    synchronize(store, account, connection, server, download, &mut refused).map(|()| refused)
}

fn names(store: &Store, account: &Account) -> Vec<String> {
    let mut names: Vec<String> = store.contacts(&account.id).unwrap().into_iter().map(|c| c.display_name()).collect();
    names.sort();
    names
}

fn maya() -> Value {
    json!({
        "displayName": "Maya Chen", "givenName": "Maya", "surname": "Chen",
        "emailAddresses": [{"address": "maya@northstar.example", "name": "Maya (work)"}, {"address": "maya.chen@home.example"}],
        "businessPhones": ["+1 555 0199"], "homePhones": ["+1 555 0142"],
        "jobTitle": "Designer", "birthday": "1988-04-12T00:00:00Z", "categories": ["Studio"]
    })
}

#[test]
fn downloads_contacts_from_every_folder() {
    let (store, account, conn) = setup();
    let server = Server::default();
    server.add(DEFAULT, "maya", maya());
    server.add(STUDIO, "alex", json!({"displayName": "Alex Rivera", "emailAddresses": [{"address": "alex@studio.example"}]}));
    sync(&store, &account, &conn, &server, true).unwrap();
    assert_eq!(names(&store, &account), vec!["Alex Rivera", "Maya Chen"]);
    let alex = store.contacts(&account.id).unwrap().into_iter().find(|c| c.display_name() == "Alex Rivera").unwrap();
    assert_eq!(alex.id, format!("graph:{STUDIO}/alex"));
    assert!(alex.is_editable() && !alex.pending);
    assert_eq!(store.contact_card(&account.id, &alex.id).unwrap().unwrap().address_book, STUDIO);
}

#[test]
fn an_edit_sends_only_what_changed_and_keeps_the_rest() {
    let (store, account, conn) = setup();
    let server = Server::default();
    let url = server.add(DEFAULT, "maya", maya());
    sync(&store, &account, &conn, &server, true).unwrap();
    let mut maya = store.contacts(&account.id).unwrap().remove(0);
    maya.title = Some("Design Director".into());
    maya.phone = Some("+1 555 0111".into());
    store.save_contact_locally(&account.id, &maya).unwrap();
    // Meanwhile, in Outlook: a note.
    server.contacts.borrow_mut().get_mut(&url).unwrap()["personalNotes"] = json!("Prefers mornings");
    sync(&store, &account, &conn, &server, true).unwrap();

    assert_eq!(server.patches.borrow().clone(), vec![json!({"jobTitle": "Design Director", "businessPhones": ["+1 555 0111"]})]);
    for (name, kept) in [("homePhones", json!(["+1 555 0142"])), ("birthday", json!("1988-04-12T00:00:00Z")), ("categories", json!(["Studio"])), ("personalNotes", json!("Prefers mornings"))] {
        assert_eq!(server.field(&url, name), kept, "kept {name}");
    }
    let shown = store.contact_by_id(&account.id, &maya.id).unwrap().unwrap();
    assert_eq!((shown.title.as_deref(), shown.notes.as_deref(), shown.pending), (Some("Design Director"), Some("Prefers mornings"), false));
    assert_eq!(store.contact_card(&account.id, &maya.id).unwrap().unwrap().etag, server.field(&url, "changeKey").as_str().unwrap());
    assert!(store.pending_operations(&account.id).unwrap().iter().all(|op| op.state == OperationState::Applied));
}

#[test]
fn a_contact_added_here_is_made_once_even_after_a_lost_reply() {
    let (store, account, conn) = setup();
    let server = Server::default();
    let mut sophie = Contact::new_local(account.id.clone(), now());
    sophie.given_name = Some("Sophie".into());
    sophie.family_name = Some("Williams".into());
    sophie.email = "sophie@example.test".into();
    sophie.emails = vec![sophie.email.clone()];
    store.save_contact_locally(&account.id, &sophie).unwrap();
    *server.lose_reply.borrow_mut() = true;
    assert_eq!(sync(&store, &account, &conn, &server, false).unwrap_err(), GraphError::Network);
    assert_eq!(server.contacts.borrow().len(), 1);

    // Edited again before the retry: the copy made then is brought up to date, not doubled.
    let mut again = store.contact_by_id(&account.id, &sophie.id).unwrap().unwrap();
    again.title = Some("Producer".into());
    store.save_contact_locally(&account.id, &again).unwrap();
    sync(&store, &account, &conn, &server, true).unwrap();
    assert_eq!(server.contacts.borrow().len(), 1, "no second copy");
    let url = server.contacts.borrow().keys().next().unwrap().clone();
    assert_eq!(server.field(&url, "jobTitle"), json!("Producer"));
    assert_eq!(server.field(&url, "displayName"), json!("Sophie Williams"));
    let shown = store.contacts(&account.id).unwrap();
    assert_eq!(shown.len(), 1);
    assert_eq!((shown[0].id.clone(), shown[0].title.as_deref(), shown[0].pending), (format!("graph:{url}"), Some("Producer"), false));
}

#[test]
fn a_contact_deleted_here_is_deleted_at_microsoft() {
    let (store, account, conn) = setup();
    let server = Server::default();
    server.add(DEFAULT, "maya", maya());
    server.add(DEFAULT, "alex", json!({"displayName": "Alex Rivera"}));
    sync(&store, &account, &conn, &server, true).unwrap();
    let alex = store.contacts(&account.id).unwrap().into_iter().find(|c| c.display_name() == "Alex Rivera").unwrap();
    store.delete_contact_locally(&account.id, &alex).unwrap();
    sync(&store, &account, &conn, &server, true).unwrap();
    assert_eq!(server.contacts.borrow().len(), 1);
    assert_eq!(names(&store, &account), vec!["Maya Chen"]);
}

#[test]
fn an_edit_to_a_contact_deleted_at_microsoft_brings_it_back() {
    let (store, account, conn) = setup();
    let server = Server::default();
    let url = server.add(DEFAULT, "maya", maya());
    sync(&store, &account, &conn, &server, true).unwrap();
    let mut maya = store.contacts(&account.id).unwrap().remove(0);
    maya.title = Some("Design Director".into());
    store.save_contact_locally(&account.id, &maya).unwrap();
    server.contacts.borrow_mut().remove(&url);
    sync(&store, &account, &conn, &server, true).unwrap();
    let shown = store.contacts(&account.id).unwrap();
    assert_eq!(shown.len(), 1);
    assert_eq!((shown[0].display_name(), shown[0].title.as_deref()), ("Maya Chen".to_owned(), Some("Design Director")));
    assert_ne!(shown[0].id, maya.id, "a new contact at Microsoft, with its own id");
}

#[test]
fn a_refused_edit_is_reported_and_microsofts_version_comes_back() {
    let (store, account, conn) = setup();
    let server = Server::default();
    let url = server.add(DEFAULT, "maya", maya());
    sync(&store, &account, &conn, &server, true).unwrap();
    server.read_only.borrow_mut().push(url);
    let mut maya = store.contacts(&account.id).unwrap().remove(0);
    maya.title = Some("Design Director".into());
    store.save_contact_locally(&account.id, &maya).unwrap();
    let refused = sync(&store, &account, &conn, &server, true).unwrap();
    assert_eq!(refused, vec!["changes to Maya Chen: Microsoft doesn’t allow changes to this contact. It shows the server’s version again.".to_owned()]);
    let shown = store.contact_by_id(&account.id, &maya.id).unwrap().unwrap();
    assert_eq!((shown.title.as_deref(), shown.pending), (Some("Designer"), false));
}

#[test]
fn a_dropped_connection_keeps_edits_queued_and_contacts_saved() {
    let (store, account, conn) = setup();
    let server = Server::default();
    let url = server.add(DEFAULT, "maya", maya());
    sync(&store, &account, &conn, &server, true).unwrap();
    let mut maya = store.contacts(&account.id).unwrap().remove(0);
    maya.title = Some("Director".into());
    store.save_contact_locally(&account.id, &maya).unwrap();
    *server.offline.borrow_mut() = true;
    assert_eq!(sync(&store, &account, &conn, &server, true).unwrap_err(), GraphError::Network);
    assert_eq!(names(&store, &account), vec!["Maya Chen"], "a failed download deletes nothing");
    assert_eq!(store.pending_operations(&account.id).unwrap()[0].state, OperationState::Pending);
    *server.offline.borrow_mut() = false;
    sync(&store, &account, &conn, &server, true).unwrap();
    assert_eq!(server.field(&url, "jobTitle"), json!("Director"));
}

#[test]
fn a_microsoft_connection_signs_in_with_its_own_sign_in() {
    let (store, account, conn) = setup();
    assert_eq!(store.contacts_connection(&account.id).unwrap().unwrap().credential, ContactsCredential::Microsoft);
    assert_eq!(carddav_sync::password(&conn), None, "no password goes anywhere");
}
