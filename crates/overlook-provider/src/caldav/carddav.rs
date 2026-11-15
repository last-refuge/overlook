//! CardDAV contacts (RFC 6352) over the CalDAV client's transport: the same HTTPS-only agent
//! with no proxy, the same redirect, time and size limits, and the same refusal of any origin
//! other than the one the person approved. Run network work off the UI thread.
//!
//! A download is complete or it is an error. A failed listing never reads as an empty
//! address book, so a bad connection can't delete contacts.

use super::*;
use roxmltree::{Document, Node, ParsingOptions};

/// Fastmail's contacts server. Its app passwords allow contacts by default.
pub const FASTMAIL_CARDDAV_URL: &str = "https://carddav.fastmail.com/";
const CARD_LIMIT: u64 = vcard::MAX_CARD_BYTES as u64;
const LISTING_LIMIT: u64 = 16 * 1024 * 1024;
const REPORT_LIMIT: u64 = 32 * 1024 * 1024;
const TOTAL_LIMIT: usize = 192 * 1024 * 1024;
const BATCH: usize = 25;
const MAX_BOOKS: usize = 64;
const MAX_CARDS: usize = 50_000;

/// The contacts server for an account whose incoming mail server is Fastmail's. Judged by
/// the server, never by the address: a custom domain at Fastmail still counts, and an address
/// that merely looks like Fastmail's doesn't send its password there.
pub fn fastmail_contacts_server(incoming_host: &str) -> Option<&'static str> {
    crate::app_passwords::FASTMAIL.owns_host(incoming_host).then_some(FASTMAIL_CARDDAV_URL)
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CardDavError {
    #[error("{0}")]
    InvalidConfiguration(&'static str),
    #[error("The contacts server refused the password. Check that the app password allows contacts.")]
    AuthenticationRequired,
    #[error("This sign-in doesn’t have permission to use contacts.")]
    Forbidden,
    #[error("The contacts server is limiting requests. Try again later.")]
    RateLimited,
    #[error("The contacts server is temporarily unavailable.")]
    ServiceUnavailable,
    #[error("Could not reach the contacts server over a verified connection.")]
    Network,
    #[error("The contacts server returned HTTP {0}.")]
    UnexpectedStatus(u16),
    #[error("The contacts server sent an incomplete or unexpected reply. Your saved contacts have been kept.")]
    InvalidResponse,
    /// Contains only the origin, never a path, query, user name or response body.
    #[error("The contacts server pointed to a different server ({0}), which Overlook won’t send your password to.")]
    UnapprovedOrigin(String),
    #[error("The contacts download went past its time or size limit. Your saved contacts have been kept.")]
    Limit,
}

impl From<CalDavError> for CardDavError {
    fn from(error: CalDavError) -> Self {
        match error {
            CalDavError::InvalidConfiguration(_) => Self::InvalidConfiguration("The contacts server settings aren’t valid."),
            CalDavError::AuthenticationRequired => Self::AuthenticationRequired,
            CalDavError::Forbidden => Self::Forbidden,
            CalDavError::RateLimited => Self::RateLimited,
            CalDavError::ServiceUnavailable => Self::ServiceUnavailable,
            CalDavError::Network => Self::Network,
            CalDavError::UnexpectedStatus(status) => Self::UnexpectedStatus(status),
            CalDavError::InvalidResponse(_) => Self::InvalidResponse,
            CalDavError::UnapprovedOrigin(origin) => Self::UnapprovedOrigin(origin),
            CalDavError::RedirectLimit | CalDavError::DiscoveryLimit => Self::Limit,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddressBook {
    pub url: String,
    pub name: String,
    /// The sign-in may add, change and remove cards here.
    pub writable: bool,
}

/// One card as the server holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardResource {
    pub address_book: String,
    pub url: String,
    pub etag: String,
    pub card: String,
}

/// A card the listing shows with the version already saved, so its body wasn't downloaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnchangedCard {
    pub address_book: String,
    pub url: String,
    pub etag: String,
}

/// Everything in every address book: new and changed cards with their text, the rest by
/// name. A card in neither list is gone from the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardDavSnapshot {
    pub address_books: Vec<AddressBook>,
    pub cards: Vec<CardResource>,
    pub unchanged: Vec<UnchangedCard>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardRemote {
    pub etag: String,
    pub card: String,
}

/// What a write asks of the server's copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteCondition<'a> {
    /// Only if nothing is at the address yet.
    Create,
    /// Only if the server still has this version.
    Replace(&'a str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteOutcome {
    Done,
    /// The server's copy changed since the version the write named.
    Conflict,
    /// Nothing is at the address.
    Missing,
}

pub struct CardDavClient {
    dav: CalDavClient,
}

impl fmt::Debug for CardDavClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CardDavClient").field("origin", &self.dav.endpoint.origin().ascii_serialization()).finish_non_exhaustive()
    }
}

impl CardDavClient {
    /// `server_url` must be HTTPS. Requests go only to its origin, signed in as `username`.
    pub fn new(server_url: &str, username: &str, password: String) -> Result<Self, CardDavError> {
        let settings = CalDavSettings { server_url: server_url.into(), username: username.into(), credential_source: CalendarCredentialSource::IncomingMail };
        if password.is_empty() {
            return Err(CardDavError::InvalidConfiguration("Enter the app password."));
        }
        Ok(Self { dav: CalDavClient::new(&settings, password)? })
    }

    /// Finds the principal's address books: through the server's root, its RFC 6764
    /// `/.well-known/carddav` hint, the principal and its address book homes.
    pub fn address_books(&self) -> Result<Vec<AddressBook>, CardDavError> {
        let dav = &self.dav;
        let started = Instant::now();
        let properties = xml::CARD_DISCOVERY_PROPERTIES;
        let (root_url, roots) = match dav.propfind_with(&dav.endpoint, "0", properties, started) {
            Err(CalDavError::UnexpectedStatus(404 | 405)) if dav.endpoint.path() == "/" => {
                let service = dav.resolve(&dav.endpoint, "/.well-known/carddav")?;
                dav.propfind_with(&service, "0", properties, started)?
            }
            result => result?,
        };
        let root = exact_response(&roots, &root_url, dav)?;
        if root.is_address_book {
            return Ok(vec![self.book(root, &root_url)?]);
        }
        let principal = root.principal.as_deref().map(|p| dav.resolve(&root_url, p)).transpose()?;
        let mut homes: Vec<Url> = root.card_homes.iter().map(|h| dav.resolve(&root_url, h)).collect::<Result<_, _>>()?;
        if homes.is_empty() {
            if let Some(principal) = &principal {
                let (principal_url, responses) = dav.propfind_with(principal, "0", properties, started)?;
                let response = exact_response(&responses, &principal_url, dav)?;
                homes = response.card_homes.iter().map(|h| dav.resolve(&principal_url, h)).collect::<Result<_, _>>()?;
            }
        }
        if homes.is_empty() && principal.is_some() {
            return Err(CardDavError::InvalidResponse);
        }
        if homes.is_empty() {
            homes.push(root_url);
        }
        let mut seen = HashSet::new();
        homes.retain(|h| seen.insert(h.as_str().to_owned()));
        if homes.len() > MAX_HOMES {
            return Err(CardDavError::Limit);
        }
        let mut books = Vec::new();
        let mut seen_books = HashSet::new();
        for home in homes {
            let (base, responses) = dav.propfind_with(&home, "1", properties, started)?;
            exact_response(&responses, &base, dav)?;
            if responses.iter().any(|r| !r.resource_type_known) {
                return Err(CardDavError::InvalidResponse);
            }
            for response in responses.iter().filter(|r| r.is_address_book) {
                let book = self.book(response, &base)?;
                if seen_books.insert(book.url.clone()) {
                    books.push(book);
                    if books.len() > MAX_BOOKS {
                        return Err(CardDavError::Limit);
                    }
                }
            }
        }
        Ok(books)
    }

    fn book(&self, response: &xml::DavResponse, base: &Url) -> Result<AddressBook, CardDavError> {
        let url = self.dav.resolve(base, &response.href)?;
        let p = &response.permissions;
        Ok(AddressBook {
            url: url.to_string(),
            name: response.name.clone().filter(|n| !n.trim().is_empty()).unwrap_or_else(|| "Contacts".into()),
            writable: p.create && p.update && p.delete,
        })
    }

    /// Every card in every address book. `known` gives the version already saved for a card's
    /// address, so only new and changed cards are downloaded.
    pub fn snapshot(&self, known: impl Fn(&str) -> Option<String>) -> Result<CardDavSnapshot, CardDavError> {
        let address_books = self.address_books()?;
        let (mut cards, mut unchanged) = (Vec::new(), Vec::new());
        let mut total = 0usize;
        let started = Instant::now();
        for book in &address_books {
            let url = self.dav.resolve(&self.dav.endpoint, &book.url)?;
            let mut fetch = Vec::new();
            for (card, etag) in self.list(&url)? {
                if known(card.as_str()).as_deref() == Some(etag.as_str()) {
                    unchanged.push(UnchangedCard { address_book: book.url.clone(), url: card.to_string(), etag });
                } else {
                    fetch.push(card);
                }
            }
            for chunk in fetch.chunks(BATCH) {
                if started.elapsed() > Duration::from_secs(300) {
                    return Err(CardDavError::Limit);
                }
                for (card_url, etag, card) in self.multiget(&url, chunk)? {
                    total += card.len();
                    if total > TOTAL_LIMIT {
                        return Err(CardDavError::Limit);
                    }
                    cards.push(CardResource { address_book: book.url.clone(), url: card_url.to_string(), etag, card });
                }
            }
            if cards.len() + unchanged.len() > MAX_CARDS {
                return Err(CardDavError::Limit);
            }
        }
        Ok(CardDavSnapshot { address_books, cards, unchanged })
    }

    /// The cards in one address book, by address and version.
    fn list(&self, book: &Url) -> Result<Vec<(Url, String)>, CardDavError> {
        const LISTING: &str = r#"<?xml version="1.0" encoding="utf-8"?><d:propfind xmlns:d="DAV:"><d:prop><d:getetag/><d:getcontenttype/><d:resourcetype/></d:prop></d:propfind>"#;
        let (base, body) = self.dav.request_xml(book, "PROPFIND", "1", LISTING, LISTING_LIMIT, Instant::now())?;
        let mut seen = HashSet::new();
        let mut cards = Vec::new();
        for entry in parse_listing(&body)? {
            let url = self.dav.resolve(&base, entry.href.trim())?;
            if url == base || entry.collection {
                continue;
            }
            if !inside(&url, &base) {
                return Err(CardDavError::InvalidResponse);
            }
            let is_card = entry.content_type.as_deref().map(|t| t.to_ascii_lowercase().contains("vcard")).unwrap_or_else(|| url.path().to_ascii_lowercase().ends_with(".vcf"));
            if !is_card {
                continue;
            }
            let etag = entry.etag.filter(|e| !e.is_empty() && e.len() <= 1024).ok_or(CardDavError::InvalidResponse)?;
            if !seen.insert(url.to_string()) {
                return Err(CardDavError::InvalidResponse);
            }
            cards.push((url, etag));
            if cards.len() > MAX_CARDS {
                return Err(CardDavError::Limit);
            }
        }
        Ok(cards)
    }

    /// The text of some cards from one address book. A card deleted since the listing is
    /// left out; any other gap is an error.
    fn multiget(&self, book: &Url, cards: &[Url]) -> Result<Vec<(Url, String, String)>, CardDavError> {
        let mut body = String::from(r#"<?xml version="1.0" encoding="utf-8"?><card:addressbook-multiget xmlns:d="DAV:" xmlns:card="urn:ietf:params:xml:ns:carddav"><d:prop><d:getetag/><card:address-data/></d:prop>"#);
        for card in cards {
            body.push_str("<d:href>");
            body.push_str(&xml_escape(card.path()));
            body.push_str("</d:href>");
        }
        body.push_str("</card:addressbook-multiget>");
        let (base, text) = self.dav.request_xml(book, "REPORT", "0", &body, REPORT_LIMIT, Instant::now())?;
        let wanted: HashSet<&str> = cards.iter().map(Url::as_str).collect();
        let mut found = Vec::new();
        let mut seen = HashSet::new();
        for entry in parse_cards(&text)? {
            let url = self.dav.resolve(&base, entry.href.trim())?;
            if !wanted.contains(url.as_str()) || !seen.insert(url.to_string()) {
                return Err(CardDavError::InvalidResponse);
            }
            if let Some((etag, card)) = entry.found {
                found.push((url, etag, card));
            }
        }
        Ok(found)
    }

    /// The card at `url` with its version, or `None` when there's none.
    pub fn get(&self, url: &str) -> Result<Option<CardRemote>, CardDavError> {
        let (status, etag, card) = self.dav.typed_resource_request(url, "GET", "", None, "text/vcard", CARD_LIMIT)?;
        match status {
            404 | 410 => Ok(None),
            200 => {
                // Updates are conditional on the version, which needs a strong one.
                let etag = etag.filter(|e| write::strong_etag(e)).ok_or(CardDavError::InvalidResponse)?;
                Ok(Some(CardRemote { etag, card }))
            }
            status => Err(CardDavError::UnexpectedStatus(status)),
        }
    }

    /// Writes a card, only under `condition`, so a change made elsewhere is never overwritten
    /// unseen. Writes never follow redirects.
    pub fn put(&self, url: &str, card: &str, condition: WriteCondition<'_>) -> Result<WriteOutcome, CardDavError> {
        let header = match condition {
            WriteCondition::Create => ("If-None-Match", "*"),
            WriteCondition::Replace(etag) if write::strong_etag(etag) => ("If-Match", etag),
            WriteCondition::Replace(_) => return Err(CardDavError::InvalidResponse),
        };
        let (status, _, _) = self.dav.typed_resource_request(url, "PUT", card, Some(header), "text/vcard", CARD_LIMIT)?;
        match status {
            200 | 201 | 204 => Ok(WriteOutcome::Done),
            409 | 412 => Ok(WriteOutcome::Conflict),
            404 | 410 => Ok(WriteOutcome::Missing),
            status => Err(CardDavError::UnexpectedStatus(status)),
        }
    }

    /// Deletes the card at `url` if the server still has version `etag`.
    pub fn delete(&self, url: &str, etag: &str) -> Result<WriteOutcome, CardDavError> {
        if !write::strong_etag(etag) {
            return Err(CardDavError::InvalidResponse);
        }
        let (status, _, _) = self.dav.typed_resource_request(url, "DELETE", "", Some(("If-Match", etag)), "text/vcard", CARD_LIMIT)?;
        match status {
            200 | 204 => Ok(WriteOutcome::Done),
            409 | 412 => Ok(WriteOutcome::Conflict),
            404 | 410 => Ok(WriteOutcome::Missing),
            status => Err(CardDavError::UnexpectedStatus(status)),
        }
    }
}

/// Whether `url` is a resource inside the collection `base`.
fn inside(url: &Url, base: &Url) -> bool {
    url.path().starts_with(&format!("{}/", base.path().trim_end_matches('/')))
}

fn xml_escape(value: &str) -> String {
    value.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn document(body: &str) -> Result<Document<'_>, CardDavError> {
    let doc = Document::parse_with_options(body, ParsingOptions { allow_dtd: false, nodes_limit: 200_000, ..Default::default() }).map_err(|_| CardDavError::InvalidResponse)?;
    if !doc.root_element().has_tag_name(("DAV:", "multistatus")) || doc.descendants().any(|n| n.ancestors().take(65).count() > 64) {
        return Err(CardDavError::InvalidResponse);
    }
    Ok(doc)
}

fn status_code(node: Node<'_, '_>) -> Result<u16, CardDavError> {
    xml::status(node).map_err(|_| CardDavError::InvalidResponse)
}

struct ListingEntry {
    href: String,
    etag: Option<String>,
    content_type: Option<String>,
    collection: bool,
}

fn parse_listing(body: &str) -> Result<Vec<ListingEntry>, CardDavError> {
    let doc = document(body)?;
    let mut entries = Vec::new();
    for response in doc.root_element().children().filter(|n| n.is_element()) {
        if !response.has_tag_name(("DAV:", "response")) {
            continue;
        }
        let href = xml::child(response, "DAV:", "href").map(xml::text).filter(|h| !h.is_empty()).ok_or(CardDavError::InvalidResponse)?;
        if let Some(status) = xml::child(response, "DAV:", "status") {
            if status_code(status)? != 200 {
                return Err(CardDavError::InvalidResponse);
            }
        }
        let mut entry = ListingEntry { href, etag: None, content_type: None, collection: false };
        for propstat in response.children().filter(|n| n.has_tag_name(("DAV:", "propstat"))) {
            let code = status_code(xml::child(propstat, "DAV:", "status").ok_or(CardDavError::InvalidResponse)?)?;
            match code {
                200 => {}
                404 => continue, // Optional properties are commonly absent.
                _ => return Err(CardDavError::InvalidResponse),
            }
            let props = xml::child(propstat, "DAV:", "prop").ok_or(CardDavError::InvalidResponse)?;
            for prop in props.children().filter(Node::is_element) {
                if prop.has_tag_name(("DAV:", "getetag")) {
                    entry.etag = Some(xml::text(prop));
                } else if prop.has_tag_name(("DAV:", "getcontenttype")) {
                    entry.content_type = Some(xml::text(prop)).filter(|t| !t.is_empty());
                } else if prop.has_tag_name(("DAV:", "resourcetype")) {
                    entry.collection = xml::child(prop, "DAV:", "collection").is_some();
                }
            }
        }
        entries.push(entry);
        if entries.len() > MAX_CARDS + 1 {
            return Err(CardDavError::Limit);
        }
    }
    Ok(entries)
}

struct CardEntry {
    href: String,
    /// The version and text, or `None` for a card the server no longer has.
    found: Option<(String, String)>,
}

fn parse_cards(body: &str) -> Result<Vec<CardEntry>, CardDavError> {
    let doc = document(body)?;
    let mut entries = Vec::new();
    for response in doc.root_element().children().filter(|n| n.is_element()) {
        if !response.has_tag_name(("DAV:", "response")) {
            continue;
        }
        let href = xml::child(response, "DAV:", "href").map(xml::text).filter(|h| !h.is_empty()).ok_or(CardDavError::InvalidResponse)?;
        if let Some(status) = xml::child(response, "DAV:", "status") {
            match status_code(status)? {
                404 | 410 => {
                    entries.push(CardEntry { href, found: None });
                    continue;
                }
                200 => {}
                _ => return Err(CardDavError::InvalidResponse),
            }
        }
        let (mut etag, mut card) = (None, None);
        for propstat in response.children().filter(|n| n.has_tag_name(("DAV:", "propstat"))) {
            let code = status_code(xml::child(propstat, "DAV:", "status").ok_or(CardDavError::InvalidResponse)?)?;
            if code != 200 {
                return Err(CardDavError::InvalidResponse);
            }
            let props = xml::child(propstat, "DAV:", "prop").ok_or(CardDavError::InvalidResponse)?;
            for prop in props.children().filter(Node::is_element) {
                if prop.has_tag_name(("DAV:", "getetag")) {
                    etag = Some(xml::text(prop));
                } else if prop.has_tag_name(("urn:ietf:params:xml:ns:carddav", "address-data")) {
                    // The card's own text, not trimmed like other values. XML reading has
                    // already turned its CRLF line ends into LF, which vCard parsing accepts.
                    let text: String = prop.children().filter(|n| n.is_text()).filter_map(|n| n.text()).collect();
                    if text.len() as u64 > CARD_LIMIT {
                        return Err(CardDavError::Limit);
                    }
                    card = Some(text);
                }
            }
        }
        let etag = etag.filter(|e| !e.is_empty() && e.len() <= 1024).ok_or(CardDavError::InvalidResponse)?;
        entries.push(CardEntry { href, found: Some((etag, card.ok_or(CardDavError::InvalidResponse)?)) });
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    const CARD: &str = "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:a1\r\nFN:Maya Chen\r\nN:Chen;Maya;;;\r\nEMAIL;TYPE=INTERNET:maya@northstar.example\r\nEND:VCARD\r\n";

    /// A scripted plain-HTTP server: one reply per request, every request recorded.
    struct Script {
        url: Url,
        requests: Arc<Mutex<Vec<String>>>,
        worker: Option<std::thread::JoinHandle<()>>,
    }

    impl Script {
        fn new(replies: Vec<String>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let url = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
            let requests = Arc::new(Mutex::new(Vec::new()));
            let seen = requests.clone();
            let worker = std::thread::spawn(move || {
                for reply in replies {
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
                    stream.write_all(reply.as_bytes()).unwrap();
                }
            });
            Self { url, requests, worker: Some(worker) }
        }

        fn client(&self) -> CardDavClient {
            // Plain HTTP only inside unit tests, never through a public constructor.
            CardDavClient { dav: CalDavClient { endpoint: self.url.clone(), username: "me@example.test".into(), password: "test-app-password".into(), agent: agent(false) } }
        }

        fn finish(mut self) -> Vec<String> {
            self.worker.take().unwrap().join().unwrap();
            self.requests.lock().unwrap().clone()
        }
    }

    fn reply(code: u16, body: &str, headers: &str) -> String {
        format!("HTTP/1.1 {code} Test\r\nContent-Length: {}\r\nContent-Type: application/xml\r\nConnection: close\r\n{headers}\r\n{body}", body.len())
    }

    fn multistatus(inner: &str) -> String {
        format!(r#"<d:multistatus xmlns:d="DAV:" xmlns:card="urn:ietf:params:xml:ns:carddav">{inner}</d:multistatus>"#)
    }

    fn response(href: &str, props: &str) -> String {
        format!("<d:response><d:href>{href}</d:href><d:propstat><d:prop>{props}</d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>")
    }

    fn discovery() -> Vec<String> {
        vec![
            // The root isn't a DAV resource (Fastmail answers 404), so the well-known hint is used.
            reply(404, "", ""),
            reply(301, "", "Location: /dav/principals/user/me/\r\n"),
            reply(207, &multistatus(&response("/dav/principals/user/me/", "<card:addressbook-home-set><d:href>/dav/addressbooks/user/me/</d:href></card:addressbook-home-set>")), ""),
            reply(
                207,
                &multistatus(&format!(
                    "{}{}{}",
                    response("/dav/addressbooks/user/me/", "<d:resourcetype><d:collection/></d:resourcetype>"),
                    response("/dav/addressbooks/user/me/Default/", "<d:resourcetype><d:collection/><card:addressbook/></d:resourcetype><d:displayname>Personal</d:displayname><d:current-user-privilege-set><d:privilege><d:all/></d:privilege></d:current-user-privilege-set>"),
                    response("/dav/addressbooks/user/me/Shared/", "<d:resourcetype><d:collection/><card:addressbook/></d:resourcetype><d:displayname>Shared</d:displayname><d:current-user-privilege-set><d:privilege><d:read/></d:privilege></d:current-user-privilege-set>"),
                )),
                "",
            ),
        ]
    }

    #[test]
    fn finds_address_books_through_the_well_known_hint() {
        let server = Script::new(discovery());
        let books = server.client().address_books().unwrap();
        let requests = server.finish();
        assert_eq!(books.len(), 2);
        assert_eq!((books[0].name.as_str(), books[0].writable), ("Personal", true));
        assert_eq!((books[1].name.as_str(), books[1].writable), ("Shared", false));
        assert!(requests[1].starts_with("PROPFIND /.well-known/carddav "));
        assert!(requests[0].contains("addressbook-home-set") && requests[0].to_ascii_lowercase().contains("authorization: basic"));
    }

    #[test]
    fn follows_fastmails_redirect_through_the_principal() {
        // Fastmail: 404 at the root, the well-known address redirects to /dav/addressbooks,
        // which names the principal, which names the address book home.
        let server = Script::new(vec![
            reply(404, "", ""),
            reply(301, "", "Location: /dav/addressbooks\r\n"),
            reply(207, &multistatus(&response("/dav/addressbooks", "<d:current-user-principal><d:href>/dav/principals/user/me/</d:href></d:current-user-principal>")), ""),
            reply(207, &multistatus(&response("/dav/principals/user/me/", "<card:addressbook-home-set><d:href>/dav/addressbooks/user/me/</d:href></card:addressbook-home-set>")), ""),
            reply(207, &multistatus(&format!("{}{}", response("/dav/addressbooks/user/me/", "<d:resourcetype><d:collection/></d:resourcetype>"), response("/dav/addressbooks/user/me/Default/", "<d:resourcetype><d:collection/><card:addressbook/></d:resourcetype><d:current-user-privilege-set><d:privilege><d:all/></d:privilege></d:current-user-privilege-set>"))), ""),
        ]);
        let books = server.client().address_books().unwrap();
        let requests = server.finish();
        assert_eq!(books.len(), 1);
        assert_eq!(books[0].name, "Contacts", "an unnamed book gets a plain name");
        assert!(books[0].url.ends_with("/dav/addressbooks/user/me/Default/") && books[0].writable);
        assert!(requests[2].starts_with("PROPFIND /dav/addressbooks HTTP/1.1"));
        assert!(requests[3].starts_with("PROPFIND /dav/principals/user/me/ HTTP/1.1"));
    }

    #[test]
    fn downloads_only_new_and_changed_cards() {
        let mut replies = discovery();
        let listing = multistatus(&format!(
            "{}{}{}",
            response("/dav/addressbooks/user/me/Default/", "<d:resourcetype><d:collection/><card:addressbook/></d:resourcetype><d:getetag>\"book\"</d:getetag>"),
            response("/dav/addressbooks/user/me/Default/a1.vcf", "<d:getetag>\"v1\"</d:getetag><d:getcontenttype>text/vcard; charset=utf-8</d:getcontenttype><d:resourcetype/>"),
            response("/dav/addressbooks/user/me/Default/b2.vcf", "<d:getetag>\"v7\"</d:getetag><d:getcontenttype>text/vcard</d:getcontenttype><d:resourcetype/>"),
        ));
        replies.push(reply(207, &listing, ""));
        let cards = multistatus(&response("/dav/addressbooks/user/me/Default/a1.vcf", &format!("<d:getetag>\"v1\"</d:getetag><card:address-data>{CARD}</card:address-data>")));
        replies.push(reply(207, &cards, ""));
        replies.push(reply(207, &multistatus(&response("/dav/addressbooks/user/me/Shared/", "<d:resourcetype><d:collection/><card:addressbook/></d:resourcetype>")), ""));
        let server = Script::new(replies);
        let base = server.url.clone();
        let known = base.join("/dav/addressbooks/user/me/Default/b2.vcf").unwrap().to_string();
        let snapshot = server.client().snapshot(|url| (url == known).then(|| "\"v7\"".to_owned())).unwrap();
        let requests = server.finish();
        assert_eq!(snapshot.cards.len(), 1);
        // XML reading turns CRLF into LF; the card's lines are otherwise as sent.
        assert_eq!(snapshot.cards[0].card.replace("\r\n", "\n"), CARD.replace("\r\n", "\n"));
        assert_eq!(snapshot.unchanged.len(), 1);
        assert_eq!(snapshot.unchanged[0].url, known);
        let report = requests.iter().find(|r| r.starts_with("REPORT ")).unwrap();
        assert!(report.contains("<d:href>/dav/addressbooks/user/me/Default/a1.vcf</d:href>") && !report.contains("b2.vcf"));
    }

    #[test]
    fn an_incomplete_listing_is_an_error_not_an_empty_book() {
        let mut replies = discovery();
        replies.push(reply(207, &multistatus("<d:response><d:href>/dav/addressbooks/user/me/Default/a1.vcf</d:href><d:propstat><d:prop><d:getetag/></d:prop><d:status>HTTP/1.1 507 Insufficient Storage</d:status></d:propstat></d:response>"), ""));
        let server = Script::new(replies);
        assert_eq!(server.client().snapshot(|_| None).unwrap_err(), CardDavError::InvalidResponse);
        server.finish();
    }

    #[test]
    fn writes_are_conditional_on_the_version() {
        let server = Script::new(vec![reply(201, "", "ETag: \"v2\"\r\n"), reply(412, "", ""), reply(204, "", ""), reply(404, "", "")]);
        let client = server.client();
        let url = server.url.join("/dav/addressbooks/user/me/Default/new.vcf").unwrap().to_string();
        assert_eq!(client.put(&url, CARD, WriteCondition::Create).unwrap(), WriteOutcome::Done);
        assert_eq!(client.put(&url, CARD, WriteCondition::Replace("\"v2\"")).unwrap(), WriteOutcome::Conflict);
        assert_eq!(client.delete(&url, "\"v3\"").unwrap(), WriteOutcome::Done);
        assert_eq!(client.delete(&url, "\"v3\"").unwrap(), WriteOutcome::Missing);
        let requests = server.finish();
        assert!(requests[0].starts_with("PUT ") && requests[0].to_ascii_lowercase().contains("if-none-match: *") && requests[0].contains("text/vcard"));
        assert!(requests[1].to_ascii_lowercase().contains("if-match: \"v2\""));
        assert!(requests[2].starts_with("DELETE ") && requests[2].to_ascii_lowercase().contains("if-match: \"v3\""));
        // A weak version can't make a write conditional, so nothing is sent.
        assert_eq!(client.put(&url, CARD, WriteCondition::Replace("W/\"weak\"")).unwrap_err(), CardDavError::InvalidResponse);
    }

    #[test]
    fn another_origin_is_refused_before_any_request() {
        let server = Script::new(vec![]);
        let client = server.client();
        assert!(matches!(client.get("https://elsewhere.example/card.vcf"), Err(CardDavError::UnapprovedOrigin(_))));
        assert!(server.finish().is_empty());
    }

    #[test]
    fn only_fastmail_servers_get_the_fastmail_contacts_server() {
        assert_eq!(fastmail_contacts_server("imap.fastmail.com"), Some(FASTMAIL_CARDDAV_URL));
        assert_eq!(fastmail_contacts_server("eu-imap.fastmail.com."), Some(FASTMAIL_CARDDAV_URL));
        assert_eq!(fastmail_contacts_server("pop.fastmail.com"), Some(FASTMAIL_CARDDAV_URL));
        assert_eq!(fastmail_contacts_server("mail.messagingengine.com"), Some(FASTMAIL_CARDDAV_URL));
        assert_eq!(fastmail_contacts_server("fastmail.com.attacker.example"), None);
        assert_eq!(fastmail_contacts_server("imap.example.com"), None);
    }
}
