//! Outlook contacts through Microsoft Graph, for accounts that sign in with Microsoft. Their
//! contacts have no CardDAV server; Graph is how Outlook's own apps reach them.
//!
//! Access comes from a sign-in of its own that asks only for `Contacts.ReadWrite` (one
//! resource per token, as Microsoft requires), kept apart from the mail sign-in so neither
//! disturbs the other. Requests go to `graph.microsoft.com` only, over HTTPS, without
//! following redirects; a page link anywhere else ends the download rather than taking the
//! token there.
//!
//! Contacts are read from the default Contacts folder and the folders inside it. An edit is a
//! PATCH of only the fields that changed, worked out against the server's current copy: the
//! fields Overlook doesn't show (addresses, birthdays, other phones, categories, photos) are
//! never sent, so never rewritten, and the lists it does touch (email addresses, phones) keep
//! every entry the edit didn't change.

use super::oauth::SIGN_IN_SCOPES;
use overlook_core::time::Timestamp;
use overlook_core::{AccountId, Contact};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{json, Map, Value};
use std::time::Duration;

/// Reads and changes the signed-in person's contacts (Graph, delegated).
pub const CONTACTS_SCOPE: &str = "https://graph.microsoft.com/Contacts.ReadWrite";
/// Where a Microsoft contacts connection points; also the default folder's contacts.
pub const GRAPH_CONTACTS_URL: &str = "https://graph.microsoft.com/v1.0/me/contacts";
const API: &str = "https://graph.microsoft.com/v1.0";

const PAGE: usize = 250;
const MAX_CONTACTS: usize = 50_000;
const MAX_FOLDERS: usize = 200;
const RESPONSE_LIMIT: u64 = 32 * 1024 * 1024;
const SELECT: &str = "id,changeKey,displayName,givenName,surname,emailAddresses,mobilePhone,businessPhones,homePhones,companyName,jobTitle,personalNotes";
/// Outlook keeps three email addresses on a contact.
const MAX_EMAILS: usize = 3;
/// Marks a contact made here with the id it had before Microsoft gave it one, so a retry
/// after a reply that never arrived finds that contact instead of adding a second.
const LOCAL_ID_PROPERTY: &str = "String {6b0c4d3e-2f7a-4c59-9e1d-8a4b7c2f5e10} Name OverlookLocalId";

/// The scopes the contacts sign-in asks for: contacts, plus a refresh token and the ID token
/// that names the account.
pub fn contacts_scopes() -> String {
    std::iter::once(CONTACTS_SCOPE).chain(SIGN_IN_SCOPES.iter().copied()).collect::<Vec<_>>().join(" ")
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GraphError {
    #[error("Couldn’t reach Microsoft over a verified connection.")]
    Network,
    #[error("Microsoft needs you to connect contacts again in Settings → Accounts.")]
    SignInRequired,
    #[error("Microsoft refused access to the contacts. If an organization manages this account, an administrator may need to allow it.")]
    Forbidden,
    #[error("Microsoft is limiting requests. Contacts sync again shortly.")]
    Throttled,
    #[error("Microsoft’s contacts service is temporarily unavailable.")]
    Unavailable,
    #[error("Microsoft sent an incomplete or unexpected reply. Your saved contacts have been kept.")]
    InvalidResponse,
    #[error("The contacts download went past its size limit. Your saved contacts have been kept.")]
    Limit,
    /// Microsoft's own message, shortened.
    #[error("Microsoft didn’t accept the change: {0}")]
    Rejected(String),
    #[error("Microsoft answered with HTTP {0}.")]
    UnexpectedStatus(u16),
}

fn nullable_vec<'de, D: Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<Vec<T>, D::Error> {
    Ok(Option::<Vec<T>>::deserialize(d)?.unwrap_or_default())
}

fn nullable_string<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    Ok(Option::<String>::deserialize(d)?.unwrap_or_default())
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphEmail {
    #[serde(default, deserialize_with = "nullable_string")]
    pub address: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// The parts of an Outlook contact Overlook reads.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphContact {
    pub id: String,
    #[serde(default)]
    pub change_key: Option<String>,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub given_name: Option<String>,
    #[serde(default)]
    pub surname: Option<String>,
    #[serde(default, deserialize_with = "nullable_vec")]
    pub email_addresses: Vec<GraphEmail>,
    #[serde(default)]
    pub mobile_phone: Option<String>,
    #[serde(default, deserialize_with = "nullable_vec")]
    pub business_phones: Vec<String>,
    #[serde(default, deserialize_with = "nullable_vec")]
    pub home_phones: Vec<String>,
    #[serde(default)]
    pub company_name: Option<String>,
    #[serde(default)]
    pub job_title: Option<String>,
    #[serde(default)]
    pub personal_notes: Option<String>,
}

/// A contact and where it lives: its folder's contacts address, and its own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphItem {
    pub folder: String,
    pub url: String,
    pub contact: GraphContact,
}

#[derive(Deserialize)]
struct Page<T> {
    #[serde(default = "Option::default")]
    value: Option<Vec<T>>,
    #[serde(rename = "@odata.nextLink", default)]
    next_link: Option<String>,
}

#[derive(Deserialize)]
struct Folder {
    id: String,
}

fn agent(https_only: bool) -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .https_only(https_only)
        .max_redirects(0)
        .timeout_global(Some(Duration::from_secs(40)))
        .build()
        .into()
}

/// An id as one path segment. Outlook ids are base64 with `-`, `_` and `=`; anything else is
/// escaped rather than trusted.
fn segment(id: &str) -> String {
    let mut out = String::with_capacity(id.len());
    for b in id.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~' | b'=') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// A query value, escaped for a URL.
fn query_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for b in value.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The first line of Graph's error message, shortened, for a refused edit.
fn error_message(text: &str) -> String {
    let message = serde_json::from_str::<Value>(text)
        .ok()
        .and_then(|v| v.pointer("/error/message").and_then(Value::as_str).map(str::to_owned))
        .unwrap_or_default();
    let line: String = message.lines().next().unwrap_or("").trim().chars().take(200).collect();
    if line.is_empty() { "the change wasn’t valid.".into() } else { line }
}

fn check(status: u16, text: &str) -> Result<(), GraphError> {
    match status {
        200..=299 => Ok(()),
        401 => Err(GraphError::SignInRequired),
        403 => Err(GraphError::Forbidden),
        429 => Err(GraphError::Throttled),
        500..=599 => Err(GraphError::Unavailable),
        400 | 409 | 422 => Err(GraphError::Rejected(error_message(text))),
        other => Err(GraphError::UnexpectedStatus(other)),
    }
}

fn parse<T: DeserializeOwned>(text: &str) -> Result<T, GraphError> {
    serde_json::from_str(text).map_err(|_| GraphError::InvalidResponse)
}

/// The signed-in person's Outlook contacts.
pub struct GraphContacts {
    agent: ureq::Agent,
    token: String,
    api: String,
}

impl GraphContacts {
    pub fn new(access_token: String) -> Self {
        Self { agent: agent(true), token: access_token, api: API.into() }
    }

    #[cfg(any(test, feature = "fake-servers"))]
    pub fn with_test_endpoint(mut self, endpoint: &str) -> Self {
        assert!(endpoint.starts_with("http://127.0.0.1:"));
        self.api = endpoint.trim_end_matches('/').into();
        self.agent = agent(false);
        self
    }

    /// The default folder's contacts address, where new contacts go.
    pub fn default_folder(&self) -> String {
        format!("{}/me/contacts", self.api)
    }

    /// Every contact in the default folder and the folders inside it. Complete or an error:
    /// a download that stops partway never counts as the whole list.
    pub fn list(&self) -> Result<Vec<GraphItem>, GraphError> {
        let mut folders = vec![self.default_folder()];
        let listed = self.pages::<Folder>(&format!("{}/me/contactFolders?$top=100&$select=id", self.api), MAX_FOLDERS)?;
        folders.extend(listed.iter().map(|f| format!("{}/me/contactFolders/{}/contacts", self.api, segment(&f.id))));
        let mut items = Vec::new();
        for folder in folders {
            let first = format!("{folder}?$top={PAGE}&$select={SELECT}");
            for contact in self.pages::<GraphContact>(&first, MAX_CONTACTS.saturating_sub(items.len()))? {
                if contact.id.is_empty() {
                    return Err(GraphError::InvalidResponse);
                }
                items.push(GraphItem { url: format!("{folder}/{}", segment(&contact.id)), folder: folder.clone(), contact });
            }
        }
        Ok(items)
    }

    /// Whether this sign-in can read the contacts: one small request, made before a new
    /// connection is saved, since an organization can refuse contacts and still allow the
    /// sign-in.
    pub fn check_access(&self) -> Result<(), GraphError> {
        let (status, text) = self.send("GET", &format!("{}?$top=1&$select=id", self.default_folder()), None)?;
        check(status, &text)
    }

    /// One contact as the server has it now; `None` when it's gone.
    pub fn get(&self, url: &str) -> Result<Option<GraphContact>, GraphError> {
        self.ensure_ours(url)?;
        let (status, text) = self.send("GET", &format!("{url}?$select={SELECT}"), None)?;
        if matches!(status, 404 | 410) {
            return Ok(None);
        }
        check(status, &text)?;
        parse(&text).map(Some)
    }

    /// Adds a contact to the default folder, marked with `local_id` so a retry can find it.
    pub fn create(&self, local_id: &str, mut body: Map<String, Value>) -> Result<GraphItem, GraphError> {
        body.insert("singleValueExtendedProperties".into(), json!([{ "id": LOCAL_ID_PROPERTY, "value": local_id }]));
        let folder = self.default_folder();
        let (status, text) = self.send("POST", &folder, Some(&Value::Object(body)))?;
        check(status, &text)?;
        let contact: GraphContact = parse(&text)?;
        if contact.id.is_empty() {
            return Err(GraphError::InvalidResponse);
        }
        Ok(GraphItem { url: format!("{folder}/{}", segment(&contact.id)), folder, contact })
    }

    /// The contact an earlier [`Self::create`] made for `local_id`, if one did.
    pub fn find_created(&self, local_id: &str) -> Result<Option<GraphItem>, GraphError> {
        let folder = self.default_folder();
        let filter = format!("singleValueExtendedProperties/Any(ep: ep/id eq '{LOCAL_ID_PROPERTY}' and ep/value eq '{}')", local_id.replace('\'', "''"));
        let url = format!("{folder}?$top=1&$select={SELECT}&$filter={}", query_value(&filter));
        let (status, text) = self.send("GET", &url, None)?;
        check(status, &text)?;
        let page: Page<GraphContact> = parse(&text)?;
        Ok(page.value.unwrap_or_default().into_iter().find(|c| !c.id.is_empty()).map(|contact| GraphItem { url: format!("{folder}/{}", segment(&contact.id)), folder, contact }))
    }

    /// Changes the fields in `patch`; `None` when the contact is gone.
    pub fn update(&self, url: &str, patch: &Map<String, Value>) -> Result<Option<GraphContact>, GraphError> {
        self.ensure_ours(url)?;
        let (status, text) = self.send("PATCH", url, Some(&Value::Object(patch.clone())))?;
        if matches!(status, 404 | 410) {
            return Ok(None);
        }
        check(status, &text)?;
        parse(&text).map(Some)
    }

    /// Deletes a contact; one already gone counts as deleted.
    pub fn delete(&self, url: &str) -> Result<(), GraphError> {
        self.ensure_ours(url)?;
        let (status, text) = self.send("DELETE", url, None)?;
        if matches!(status, 404 | 410) {
            return Ok(());
        }
        check(status, &text)
    }

    /// The token goes to Graph's contacts and nowhere else.
    fn ensure_ours(&self, url: &str) -> Result<(), GraphError> {
        if url.starts_with(&format!("{}/me/", self.api)) { Ok(()) } else { Err(GraphError::InvalidResponse) }
    }

    fn pages<T: DeserializeOwned>(&self, first: &str, limit: usize) -> Result<Vec<T>, GraphError> {
        let mut out = Vec::new();
        let mut next = Some(first.to_owned());
        while let Some(url) = next.take() {
            let (status, text) = self.send("GET", &url, None)?;
            check(status, &text)?;
            let page: Page<T> = parse(&text)?;
            out.extend(page.value.unwrap_or_default());
            if out.len() > limit {
                return Err(GraphError::Limit);
            }
            if let Some(link) = page.next_link {
                self.ensure_ours(&link)?;
                next = Some(link);
            }
        }
        Ok(out)
    }

    fn send(&self, method: &str, url: &str, body: Option<&Value>) -> Result<(u16, String), GraphError> {
        let auth = format!("Bearer {}", self.token);
        let sent = match (method, body) {
            ("GET", _) => self.agent.get(url).header("Authorization", &auth).header("Accept", "application/json").call(),
            ("DELETE", _) => self.agent.delete(url).header("Authorization", &auth).call(),
            ("POST", Some(body)) => self.agent.post(url).header("Authorization", &auth).send_json(body),
            ("PATCH", Some(body)) => self.agent.patch(url).header("Authorization", &auth).send_json(body),
            _ => return Err(GraphError::InvalidResponse),
        };
        // The detail is logged there; the person sees a fixed message.
        let mut response = sent.map_err(|e| {
            let _ = crate::secure_error::ureq("Graph contacts request", e);
            GraphError::Network
        })?;
        let status = response.status().as_u16();
        let text = response.body_mut().with_config().limit(RESPONSE_LIMIT).read_to_string().map_err(|e| match e {
            ureq::Error::BodyExceedsLimit(_) => GraphError::Limit,
            _ => GraphError::Network,
        })?;
        Ok((status, text))
    }
}

fn clean(value: &Option<String>) -> Option<String> {
    value.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned)
}

fn same(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

/// The contact a Graph contact shows. Addresses are kept in Outlook's order, lowercased like
/// CardDAV's; the phone shown is the mobile one, else the first business or home one.
pub fn contact_from_graph(g: &GraphContact, id: String, account: AccountId, updated: Timestamp) -> Contact {
    let mut emails: Vec<String> = Vec::new();
    for email in &g.email_addresses {
        let address = email.address.trim().to_lowercase();
        if !address.is_empty() && !emails.contains(&address) {
            emails.push(address);
        }
    }
    let phone = clean(&g.mobile_phone)
        .or_else(|| g.business_phones.iter().map(|p| p.trim()).find(|p| !p.is_empty()).map(str::to_owned))
        .or_else(|| g.home_phones.iter().map(|p| p.trim()).find(|p| !p.is_empty()).map(str::to_owned));
    Contact {
        id,
        account_id: account,
        email: emails.first().cloned().unwrap_or_default(),
        emails,
        name: clean(&g.display_name),
        given_name: clean(&g.given_name),
        family_name: clean(&g.surname),
        phone,
        organization: clean(&g.company_name),
        title: clean(&g.job_title),
        notes: clean(&g.personal_notes),
        photo_url: None,
        in_address_book: true,
        etag: g.change_key.clone(),
        updated,
        pending: false,
    }
}

/// A cleared text field. Outlook stores an empty string as no value.
fn text_value(value: &Option<String>) -> Value {
    Value::String(clean(value).unwrap_or_default())
}

/// The PATCH that applies an edit to the server's copy `current`: only what differs between
/// `before` (the contact as last read) and `after` (as edited here). Empty when nothing
/// Overlook shows changed.
pub fn graph_changes(current: &GraphContact, before: &Contact, after: &Contact) -> Map<String, Value> {
    let changed = |a: &Option<String>, b: &Option<String>| clean(a) != clean(b);
    let mut patch = Map::new();
    let (given, family) = (changed(&before.given_name, &after.given_name), changed(&before.family_name, &after.family_name));
    if given {
        patch.insert("givenName".into(), text_value(&after.given_name));
    }
    if family {
        patch.insert("surname".into(), text_value(&after.family_name));
    }
    if given || family {
        // The display name follows the names as they will be: the part edited here and the
        // server's own other part.
        let first = if given { clean(&after.given_name) } else { clean(&current.given_name) };
        let last = if family { clean(&after.family_name) } else { clean(&current.surname) };
        let joined = format!("{} {}", first.unwrap_or_default(), last.unwrap_or_default()).trim().to_owned();
        let display = Some(joined).filter(|d| !d.is_empty()).or_else(|| clean(&after.name)).or_else(|| clean(&Some(after.email.clone())));
        if let Some(display) = display {
            patch.insert("displayName".into(), Value::String(display));
        }
    }
    if let Some(emails) = edited_emails(&current.email_addresses, before, after) {
        patch.insert("emailAddresses".into(), serde_json::to_value(emails).unwrap_or_default());
    }
    if changed(&before.phone, &after.phone) {
        edit_phone(current, before.phone.as_deref(), after.phone.as_deref(), &mut patch);
    }
    if changed(&before.organization, &after.organization) {
        patch.insert("companyName".into(), text_value(&after.organization));
    }
    if changed(&before.title, &after.title) {
        patch.insert("jobTitle".into(), text_value(&after.title));
    }
    if changed(&before.notes, &after.notes) {
        patch.insert("personalNotes".into(), text_value(&after.notes));
    }
    patch
}

fn all_emails(contact: &Contact) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for email in std::iter::once(&contact.email).chain(&contact.emails) {
        let email = email.trim().to_lowercase();
        if !email.is_empty() && !out.contains(&email) {
            out.push(email);
        }
    }
    out
}

/// The server's address list with the edit applied: an edited address takes the old one's
/// place and name, a removed one goes, an added one joins (first when it's the main address).
/// `None` when the addresses weren't edited.
fn edited_emails(current: &[GraphEmail], before: &Contact, after: &Contact) -> Option<Vec<GraphEmail>> {
    let (was, now) = (all_emails(before), all_emails(after));
    let primary_changed = !same(&before.email, &after.email);
    if was == now && !primary_changed {
        return None;
    }
    let mut added: Vec<String> = now.iter().filter(|e| !was.contains(e)).cloned().collect();
    let removed: Vec<String> = was.iter().filter(|e| !now.contains(e)).cloned().collect();
    let mut list: Vec<GraphEmail> = current.to_vec();
    if primary_changed && !before.email.trim().is_empty() && !after.email.trim().is_empty() {
        if let Some(slot) = list.iter_mut().find(|e| same(&e.address, &before.email)) {
            slot.address = after.email.trim().to_owned();
            added.retain(|a| !same(a, &after.email));
        }
    }
    list.retain(|e| !removed.iter().any(|r| same(&e.address, r)));
    for address in added {
        if list.iter().any(|e| same(&e.address, &address)) {
            continue;
        }
        let entry = GraphEmail { address: address.clone(), name: None };
        if same(&address, &after.email) { list.insert(0, entry) } else { list.push(entry) }
    }
    list.truncate(MAX_EMAILS);
    Some(list)
}

/// The phone edit lands where the phone shown came from: the mobile number, or its entry in
/// the business or home list. A phone the server no longer has (or none before) is the mobile.
fn edit_phone(current: &GraphContact, before: Option<&str>, after: Option<&str>, patch: &mut Map<String, Value>) {
    let new = after.map(str::trim).filter(|p| !p.is_empty());
    let replace_in = |list: &[String], old: &str| -> Option<Vec<String>> {
        let at = list.iter().position(|p| p.trim() == old)?;
        let mut list = list.to_vec();
        match new {
            Some(phone) => list[at] = phone.to_owned(),
            None => {
                list.remove(at);
            }
        }
        Some(list)
    };
    if let Some(old) = before.map(str::trim).filter(|p| !p.is_empty()) {
        if current.mobile_phone.as_deref().map(str::trim) != Some(old) {
            if let Some(list) = replace_in(&current.business_phones, old) {
                patch.insert("businessPhones".into(), json!(list));
                return;
            }
            if let Some(list) = replace_in(&current.home_phones, old) {
                patch.insert("homePhones".into(), json!(list));
                return;
            }
        }
    }
    patch.insert("mobilePhone".into(), Value::String(new.unwrap_or_default().to_owned()));
}

/// A new Outlook contact for one made here.
pub fn graph_new(contact: &Contact) -> Map<String, Value> {
    let mut body = Map::new();
    let mut put = |key: &str, value: &Option<String>| {
        if let Some(value) = clean(value) {
            body.insert(key.into(), Value::String(value));
        }
    };
    put("givenName", &contact.given_name);
    put("surname", &contact.family_name);
    put("displayName", &contact.known_name().or_else(|| clean(&Some(contact.email.clone()))));
    put("mobilePhone", &contact.phone);
    put("companyName", &contact.organization);
    put("jobTitle", &contact.title);
    put("personalNotes", &contact.notes);
    let emails: Vec<GraphEmail> = all_emails(contact).into_iter().take(MAX_EMAILS).map(|address| GraphEmail { address, name: None }).collect();
    if !emails.is_empty() {
        body.insert("emailAddresses".into(), serde_json::to_value(emails).unwrap_or_default());
    }
    body
}

#[cfg(test)]
#[path = "contacts_tests.rs"]
mod tests;
