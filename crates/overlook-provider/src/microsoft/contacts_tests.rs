//! Graph contacts: what's read, what an edit sends, and how the client talks to a scripted
//! plain-HTTP stand-in for Graph.

use super::*;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

fn maya() -> GraphContact {
    serde_json::from_value(json!({
        "id": "AAMkAD-maya=",
        "changeKey": "EQAAABYAAAA1",
        "displayName": "Maya Chen",
        "givenName": "Maya",
        "surname": "Chen",
        "emailAddresses": [
            {"address": "Maya@Northstar.example", "name": "Maya (work)"},
            {"address": "maya.chen@home.example", "name": "Maya Chen"}
        ],
        "mobilePhone": null,
        "businessPhones": ["+1 555 0199", "+1 555 0100"],
        "homePhones": null,
        "companyName": "Northstar Studio",
        "jobTitle": "Design Director",
        "personalNotes": ""
    }))
    .unwrap()
}

fn shown(g: &GraphContact) -> Contact {
    contact_from_graph(g, format!("graph:{GRAPH_CONTACTS_URL}/{}", g.id), AccountId::from("imap:outlook-fixture"), 1)
}

#[test]
fn the_sign_in_asks_for_contacts_alone_with_a_refresh_token() {
    let scopes = contacts_scopes();
    assert!(scopes.starts_with("https://graph.microsoft.com/Contacts.ReadWrite ") && scopes.contains("offline_access") && scopes.contains("openid"));
    assert!(!scopes.contains("outlook.office.com"), "one resource per token");
}

#[test]
fn reads_the_fields_overlook_shows() {
    let c = shown(&maya());
    assert_eq!(c.emails, vec!["maya@northstar.example", "maya.chen@home.example"]);
    assert_eq!(c.email, "maya@northstar.example");
    assert_eq!(c.phone.as_deref(), Some("+1 555 0199"), "no mobile, so the first business phone");
    assert_eq!((c.given_name.as_deref(), c.family_name.as_deref(), c.name.as_deref()), (Some("Maya"), Some("Chen"), Some("Maya Chen")));
    assert_eq!((c.organization.as_deref(), c.title.as_deref(), c.notes), (Some("Northstar Studio"), Some("Design Director"), None));
    assert_eq!(c.etag.as_deref(), Some("EQAAABYAAAA1"));
    assert!(c.in_address_book && !c.pending);
}

#[test]
fn an_edit_sends_only_what_changed() {
    let current = maya();
    let before = shown(&current);
    assert!(graph_changes(&current, &before, &before).is_empty());
    let mut after = before.clone();
    after.title = Some("Head of Design".into());
    after.notes = Some("Prefers mornings".into());
    let patch = graph_changes(&current, &before, &after);
    assert_eq!(Value::Object(patch), json!({"jobTitle": "Head of Design", "personalNotes": "Prefers mornings"}));
    let mut cleared = before.clone();
    cleared.organization = None;
    assert_eq!(Value::Object(graph_changes(&current, &before, &cleared)), json!({"companyName": ""}));
}

#[test]
fn an_edited_address_keeps_its_place_and_name_and_the_others_stay() {
    let current = maya();
    let before = shown(&current);
    let mut after = before.clone();
    after.email = "maya@northstar-studio.example".into();
    after.emails = vec![after.email.clone(), "maya.chen@home.example".into()];
    let patch = graph_changes(&current, &before, &after);
    assert_eq!(
        patch["emailAddresses"],
        json!([{"address": "maya@northstar-studio.example", "name": "Maya (work)"}, {"address": "maya.chen@home.example", "name": "Maya Chen"}])
    );
    // Cleared, as the form clears it (its whole list too): only the shown address goes.
    let mut cleared = before.clone();
    cleared.email.clear();
    cleared.emails.clear();
    assert_eq!(graph_changes(&current, &before, &cleared)["emailAddresses"], json!([{"address": "maya.chen@home.example", "name": "Maya Chen"}]));
    // Already gone at the server: nothing to send.
    let mut gone = maya();
    gone.email_addresses.remove(0);
    assert!(!graph_changes(&gone, &before, &cleared).contains_key("emailAddresses"));
}

#[test]
fn choosing_one_of_the_other_addresses_moves_it_up_without_doubling_it() {
    let current = maya();
    let before = shown(&current);
    let mut after = before.clone();
    // What the form does: the old main address leaves the list, the new one goes first.
    after.email = "maya.chen@home.example".into();
    after.emails = vec!["maya.chen@home.example".into(), "maya.chen@home.example".into()];
    assert_eq!(graph_changes(&current, &before, &after)["emailAddresses"], json!([{"address": "maya.chen@home.example", "name": "Maya Chen"}]));
}

#[test]
fn a_phone_edit_lands_where_the_phone_came_from() {
    let current = maya();
    let before = shown(&current);
    let mut after = before.clone();
    after.phone = Some("+1 555 0111".into());
    assert_eq!(Value::Object(graph_changes(&current, &before, &after)), json!({"businessPhones": ["+1 555 0111", "+1 555 0100"]}));
    // Its number changed in Outlook meanwhile, and a mobile was added there: the edit can't
    // replace a number it never saw, and the mobile stays.
    let mut moved = maya();
    moved.business_phones = vec!["+1 555 0198".into()];
    moved.mobile_phone = Some("+1 555 0177".into());
    assert_eq!(Value::Object(graph_changes(&moved, &before, &after)), json!({"businessPhones": ["+1 555 0198", "+1 555 0111"]}));
    let mut cleared = before.clone();
    cleared.phone = None;
    assert!(graph_changes(&moved, &before, &cleared).is_empty(), "nothing left to clear");
    // A contact without phones gets a mobile number.
    let mut bare = maya();
    bare.business_phones.clear();
    let before = shown(&bare);
    let mut after = before.clone();
    after.phone = Some("+1 555 0123".into());
    assert_eq!(Value::Object(graph_changes(&bare, &before, &after)), json!({"mobilePhone": "+1 555 0123"}));
}

#[test]
fn a_name_edit_keeps_the_servers_other_name_part() {
    let before = shown(&maya());
    // Married name added on a phone while a given-name edit waited here.
    let mut current = maya();
    current.surname = Some("Chen-Rivera".into());
    let mut after = before.clone();
    after.given_name = Some("Maya R.".into());
    after.name = Some("Maya R. Chen".into());
    assert_eq!(Value::Object(graph_changes(&current, &before, &after)), json!({"givenName": "Maya R.", "displayName": "Maya R. Chen-Rivera"}));
}

#[test]
fn new_contacts_carry_what_was_entered() {
    let mut c = Contact::new_local(AccountId::from("imap:outlook-fixture"), 0);
    c.given_name = Some("Sophie".into());
    c.family_name = Some("Williams".into());
    c.email = "sophie@example.test".into();
    c.emails = vec!["Sophie@Example.test".into(), "s.williams@example.test".into()];
    c.phone = Some("+1 555 0142".into());
    let body = graph_new(&c);
    assert_eq!(
        Value::Object(body),
        json!({
            "givenName": "Sophie",
            "surname": "Williams",
            "displayName": "Sophie Williams",
            "mobilePhone": "+1 555 0142",
            "emailAddresses": [{"address": "sophie@example.test"}, {"address": "s.williams@example.test"}]
        })
    );
}

/// A scripted plain-HTTP server: one reply per request, every request recorded.
struct Script {
    url: String,
    requests: Arc<Mutex<Vec<String>>>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Script {
    fn new(replies: Vec<(u16, String)>) -> Self {
        Self::with(|_| replies)
    }

    /// Replies that need the server's own address, such as a page link.
    fn with(replies: impl FnOnce(&str) -> Vec<(u16, String)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let replies = replies(&url);
        let requests = Arc::new(Mutex::new(Vec::new()));
        let seen = requests.clone();
        let worker = std::thread::spawn(move || {
            for (status, body) in replies {
                let (mut stream, _) = listener.accept().unwrap();
                stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
                let mut data = Vec::new();
                let mut buffer = [0u8; 8192];
                loop {
                    let n = stream.read(&mut buffer).unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    data.extend_from_slice(&buffer[..n]);
                    let text = String::from_utf8_lossy(&data);
                    if let Some(end) = text.find("\r\n\r\n") {
                        let length = text[..end].lines().find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap_or(0))).unwrap_or(0);
                        if data.len() >= end + 4 + length {
                            break;
                        }
                    }
                }
                seen.lock().unwrap().push(String::from_utf8_lossy(&data).into_owned());
                let reply = format!("HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{body}", body.len());
                stream.write_all(reply.as_bytes()).unwrap();
            }
        });
        Self { url, requests, worker: Some(worker) }
    }

    fn client(&self) -> GraphContacts {
        GraphContacts::new("synthetic-access-token".into()).with_test_endpoint(&self.url)
    }

    fn finish(mut self) -> Vec<String> {
        self.worker.take().unwrap().join().unwrap();
        self.requests.lock().unwrap().clone()
    }
}

/// A recorded request's JSON body.
fn sent_json(request: &str) -> Value {
    serde_json::from_str(&request[request.find("\r\n\r\n").unwrap() + 4..]).unwrap()
}

fn contact_json(id: &str, name: &str) -> Value {
    json!({"id": id, "changeKey": format!("ck-{id}"), "displayName": name, "emailAddresses": [{"address": format!("{id}@example.test")}]})
}

#[test]
fn lists_every_folder_and_follows_page_links_only_to_graph() {
    let server = Script::with(|url| {
        vec![
            (200, json!({"value": [{"id": "AAMkFolder1", "displayName": "Studio"}]}).to_string()),
            (200, json!({"value": [contact_json("maya", "Maya Chen")], "@odata.nextLink": format!("{url}/me/contacts?$skiptoken=page2")}).to_string()),
            (200, json!({"value": [contact_json("alex", "Alex Rivera")]}).to_string()),
            (200, json!({"value": [contact_json("daniel", "Daniel Park")]}).to_string()),
        ]
    });
    let url = server.url.clone();
    let items = server.client().list().unwrap();
    let requests = server.finish();
    let found: Vec<(&str, &str)> = items.iter().map(|i| (i.contact.id.as_str(), i.url.as_str())).collect();
    let (maya, alex, daniel) = (format!("{url}/me/contacts/maya"), format!("{url}/me/contacts/alex"), format!("{url}/me/contactFolders/AAMkFolder1/contacts/daniel"));
    assert_eq!(found, vec![("maya", maya.as_str()), ("alex", alex.as_str()), ("daniel", daniel.as_str())]);
    assert_eq!(items[2].folder, format!("{url}/me/contactFolders/AAMkFolder1/contacts"));
    assert!(requests[0].starts_with("GET /me/contactFolders?$top=100&$select=id HTTP/1.1"), "{}", requests[0]);
    assert!(requests[1].starts_with("GET /me/contacts?$top=250&$select=id,changeKey,"), "{}", requests[1]);
    assert!(requests[2].starts_with("GET /me/contacts?$skiptoken=page2 HTTP/1.1"));
    assert!(requests[3].starts_with("GET /me/contactFolders/AAMkFolder1/contacts?$top=250&"));
    assert!(requests.iter().all(|r| r.to_ascii_lowercase().contains("authorization: bearer synthetic-access-token")));

    // A page link anywhere else ends the download: the token never goes there.
    let server = Script::new(vec![
        (200, json!({"value": []}).to_string()),
        (200, json!({"value": [contact_json("maya", "Maya Chen")], "@odata.nextLink": "https://elsewhere.example/me/contacts?page=2"}).to_string()),
    ]);
    assert_eq!(server.client().list().unwrap_err(), GraphError::InvalidResponse);
    assert_eq!(server.finish().len(), 2);
}

#[test]
fn a_listing_reply_without_its_contacts_is_an_error_not_an_empty_book() {
    for incomplete in [json!({}), json!({"value": null})] {
        let server = Script::new(vec![(200, json!({"value": []}).to_string()), (200, incomplete.to_string())]);
        assert_eq!(server.client().list().unwrap_err(), GraphError::InvalidResponse);
        server.finish();
    }
}

#[test]
fn reads_changes_and_deletes_one_contact() {
    let server = Script::new(vec![
        (404, json!({"error": {"code": "ErrorItemNotFound", "message": "The specified object was not found in the store."}}).to_string()),
        (200, contact_json("maya", "Maya Chen").to_string()),
        (404, String::new()),
        (400, json!({"error": {"code": "ErrorInvalidProperty", "message": "The email address isn't valid.\nRequest id: 0000"}}).to_string()),
        (401, String::new()),
        (429, String::new()),
        (403, String::new()),
        (503, String::new()),
    ]);
    let client = server.client();
    let item = format!("{}/me/contacts/maya", server.url);
    let mut patch = Map::new();
    patch.insert("jobTitle".into(), json!("Director"));
    assert_eq!(client.get(&item).unwrap(), None, "gone");
    assert_eq!(client.update(&item, &patch).unwrap().unwrap().id, "maya");
    client.delete(&item).unwrap();
    assert_eq!(client.update(&item, &patch).unwrap_err(), GraphError::Rejected("The email address isn't valid.".into()));
    assert_eq!(client.get(&item).unwrap_err(), GraphError::SignInRequired);
    assert_eq!(client.get(&item).unwrap_err(), GraphError::Throttled);
    assert_eq!(client.delete(&item).unwrap_err(), GraphError::Forbidden);
    assert_eq!(client.get(&item).unwrap_err(), GraphError::Unavailable);
    let requests = server.finish();
    assert!(requests[1].starts_with("PATCH /me/contacts/maya HTTP/1.1"));
    assert_eq!(sent_json(&requests[1]), json!({"jobTitle": "Director"}));
    assert!(requests[2].starts_with("DELETE /me/contacts/maya HTTP/1.1"));
    assert_eq!(client.get("https://elsewhere.example/me/contacts/maya").unwrap_err(), GraphError::InvalidResponse, "never anywhere but Graph");
}

#[test]
fn a_new_contact_is_marked_so_a_retry_finds_it() {
    let server = Script::new(vec![
        (201, contact_json("sophie", "Sophie Williams").to_string()),
        (200, json!({"value": [contact_json("sophie", "Sophie Williams")]}).to_string()),
        (200, json!({"value": []}).to_string()),
    ]);
    let client = server.client();
    let mut body = Map::new();
    body.insert("displayName".into(), json!("Sophie Williams"));
    let made = client.create("local:5f0c", body).unwrap();
    assert!(made.url.ends_with("/me/contacts/sophie") && made.folder.ends_with("/me/contacts"));
    assert_eq!(client.find_created("local:5f0c").unwrap().map(|i| i.contact.id), Some("sophie".into()));
    assert_eq!(client.find_created("local:it's").unwrap(), None);
    let requests = server.finish();
    assert!(requests[0].starts_with("POST /me/contacts HTTP/1.1"));
    assert_eq!(sent_json(&requests[0]), json!({"displayName": "Sophie Williams", "singleValueExtendedProperties": [{"id": "String {6b0c4d3e-2f7a-4c59-9e1d-8a4b7c2f5e10} Name OverlookLocalId", "value": "local:5f0c"}]}));
    assert!(requests[1].contains("&$filter=singleValueExtendedProperties%2FAny%28ep%3A%20ep%2Fid%20eq%20%27String%20%7B6b0c4d3e-"), "{}", requests[1]);
    assert!(requests[2].contains("ep%2Fvalue%20eq%20%27local%3Ait%27%27s%27%29"), "a quote is doubled: {}", requests[2]);
}

#[test]
fn ids_are_escaped_into_one_path_segment() {
    assert_eq!(segment("AAMkAD-x_y=="), "AAMkAD-x_y==");
    assert_eq!(segment("a/b?c#d e"), "a%2Fb%3Fc%23d%20e");
}
