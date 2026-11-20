//! Account-scoped reads of the contacts already available to Overlook.

use super::{parse, storage, Failure, ReadContext, ReadTool};
use crate::mcp::contract::ErrorCode;
use crate::mcp::write::contact_revision;
use overlook_core::{Account, Contact, ContactsCredential, ProviderKind};
use overlook_provider::microsoft::contacts::GRAPH_CONTACTS_URL;
use overlook_store::Store;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

const LIST: &str = "contacts_address_books_list";
const SEARCH: &str = "contacts_search";
const GET: &str = "contacts_get";
const QUERY_LIMIT: usize = 256;
const DISPLAY_LIMIT: usize = 320;
const NOTES_LIMIT: usize = 2048;
const EMAIL_COUNT: usize = 16;

pub(super) fn tools() -> Vec<ReadTool> {
    Vec::from([
        ReadTool::new(
            LIST,
            "List address books and inferred contact collections visible in the local cache.",
            json!({
                "type": "object",
                "properties": {
                    "limit": {"type": "integer", "minimum": 1, "maximum": 50},
                    "cursor": {"type": "string"}
                }
            }),
        ),
        ReadTool::new(
            SEARCH,
            "Search cached contacts by name, email, or organization, optionally within one address book.",
            json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "minLength": 1, "maxLength": QUERY_LIMIT},
                    "address_book_id": {"type": "string"},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 50},
                    "cursor": {"type": "string"}
                },
                "required": ["query"]
            }),
        ),
        ReadTool::new(
            GET,
            "Read the cached display fields and source of a contact returned by search.",
            json!({
                "type": "object",
                "properties": {"contact_id": {"type": "string"}},
                "required": ["contact_id"]
            }),
        ),
    ])
}

#[derive(Deserialize)]
struct SearchArgs {
    query: String,
    address_book_id: Option<String>,
}

#[derive(Deserialize)]
struct GetArgs {
    contact_id: String,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
enum Source {
    Sample,
    Device,
    Google,
    #[serde(rename = "carddav")]
    CardDav,
    MicrosoftGraph,
    Inferred,
}

impl Source {
    fn key(self) -> &'static str {
        match self {
            Self::Sample => "sample",
            Self::Device => "device",
            Self::Google => "google",
            Self::CardDav => "carddav",
            Self::MicrosoftGraph => "microsoft_graph",
            Self::Inferred => "inferred",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Sample => "Sample contacts",
            Self::Device => "Contacts on this device",
            Self::Google => "Google Contacts",
            Self::CardDav => "CardDAV address book",
            Self::MicrosoftGraph => "Microsoft Graph contacts",
            Self::Inferred => "Inferred from correspondence",
        }
    }

    fn read_only(self) -> Option<bool> {
        match self {
            Self::Inferred => Some(true),
            Self::Sample | Self::Device => Some(false),
            // Provider permissions for these individual books are not retained in the cache.
            Self::Google | Self::CardDav | Self::MicrosoftGraph => None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(in crate::mcp) struct BookKey<'a> {
    source: Source,
    raw: &'a str,
}

impl<'a> BookKey<'a> {
    fn new(source: Source, raw: &'a str) -> Self {
        Self { source, raw }
    }

    pub(in crate::mcp) fn default_for(
        provider: ProviderKind,
        connection: Option<&'a (ContactsCredential, Option<String>)>,
    ) -> Option<Self> {
        match (provider, connection) {
            (ProviderKind::Sample, _) => Some(Self::new(Source::Sample, "")),
            (ProviderKind::Gmail, _) => Some(Self::new(Source::Google, "")),
            (ProviderKind::Local, _) => Some(Self::new(Source::Device, "")),
            (_, Some((ContactsCredential::Microsoft, _))) => {
                Some(Self::new(Source::MicrosoftGraph, GRAPH_CONTACTS_URL))
            }
            (_, Some((_, book))) => book
                .as_deref()
                .filter(|book| !book.is_empty())
                .map(|book| Self::new(Source::CardDav, book)),
            (_, None) => Some(Self::new(Source::Device, "")),
        }
    }

    pub(in crate::mcp) fn for_contact(
        provider: ProviderKind,
        contact: &Contact,
        card_book: &'a str,
        connection: Option<&'a (ContactsCredential, Option<String>)>,
    ) -> Self {
        if !contact.in_address_book {
            Self::new(Source::Inferred, "")
        } else if contact.id.starts_with("carddav:") {
            Self::new(Source::CardDav, card_book)
        } else if contact.id.starts_with("graph:") {
            Self::new(Source::MicrosoftGraph, card_book)
        } else if provider == ProviderKind::Sample {
            Self::new(Source::Sample, "")
        } else if contact.id.starts_with("local:") && contact.pending {
            match connection {
                Some((ContactsCredential::Microsoft, _)) if provider != ProviderKind::Local => {
                    Self::new(Source::MicrosoftGraph, GRAPH_CONTACTS_URL)
                }
                Some((_, book)) if provider != ProviderKind::Local => Self::new(
                    Source::CardDav,
                    if card_book.is_empty() {
                        book.as_deref().unwrap_or_default()
                    } else {
                        card_book
                    },
                ),
                _ => Self::new(Source::Device, ""),
            }
        } else {
            let source = if provider == ProviderKind::Gmail && !contact.id.starts_with("local:") {
                Source::Google
            } else {
                Source::Device
            };
            Self::new(source, "")
        }
    }

    pub(in crate::mcp) fn supports_write(self, default: Option<Self>) -> bool {
        match self.source {
            Source::Sample | Source::Device => true,
            Source::CardDav | Source::MicrosoftGraph => Some(self) == default,
            Source::Google | Source::Inferred => false,
        }
    }

    pub(in crate::mcp) fn remote(self) -> bool {
        matches!(self.source, Source::CardDav | Source::MicrosoftGraph)
    }

    pub(in crate::mcp) fn raw_key(self) -> String {
        format!("{}:{}", self.source.key(), self.raw)
    }

    fn read_only(self, default: Option<Self>) -> Option<bool> {
        if self.remote() && self.supports_write(default) {
            Some(false)
        } else {
            self.source.read_only()
        }
    }

    fn id(self, ctx: &ReadContext<'_>) -> String {
        ctx.id("address_book", &self.raw_key())
    }
}

#[derive(Serialize)]
struct BookSummary {
    address_book_id: String,
    name: String,
    source: Source,
    read_only: Option<bool>,
    is_default: bool,
    cached_contacts: usize,
}

#[derive(Serialize)]
struct ContactSummary {
    contact_id: String,
    address_book_id: String,
    source: Source,
    read_only: Option<bool>,
    display_name: String,
    email: Option<String>,
    organization: Option<String>,
}

impl ContactSummary {
    fn new(
        ctx: &ReadContext<'_>,
        contact: &Contact,
        book: BookKey<'_>,
        default: Option<BookKey<'_>>,
    ) -> Self {
        Self {
            contact_id: ctx.id("contact", &contact.id),
            address_book_id: book.id(ctx),
            source: book.source,
            read_only: book.read_only(default),
            display_name: bounded(&contact.display_name(), DISPLAY_LIMIT),
            email: display_field(Some(&contact.email)),
            organization: display_field(contact.organization.as_deref()),
        }
    }
}

#[derive(Serialize)]
struct ContactDetails {
    #[serde(flatten)]
    summary: ContactSummary,
    given_name: Option<String>,
    family_name: Option<String>,
    emails: Vec<String>,
    phone: Option<String>,
    title: Option<String>,
    notes: Option<String>,
    pending: bool,
    revision: String,
}

impl ContactDetails {
    fn new(
        ctx: &ReadContext<'_>,
        contact: &Contact,
        book: BookKey<'_>,
        default: Option<BookKey<'_>>,
    ) -> Self {
        let mut emails: Vec<String> = Vec::new();
        for email in std::iter::once(&contact.email).chain(&contact.emails) {
            let Some(email) = display_field(Some(email)) else {
                continue;
            };
            if !emails
                .iter()
                .any(|known| known.eq_ignore_ascii_case(&email))
            {
                emails.push(email);
                if emails.len() == EMAIL_COUNT {
                    break;
                }
            }
        }
        Self {
            summary: ContactSummary::new(ctx, contact, book, default),
            given_name: display_field(contact.given_name.as_deref()),
            family_name: display_field(contact.family_name.as_deref()),
            emails,
            phone: display_field(contact.phone.as_deref()),
            title: display_field(contact.title.as_deref()),
            notes: contact
                .notes
                .as_deref()
                .map(|notes| bounded(notes, NOTES_LIMIT)),
            pending: contact.pending,
            revision: contact_revision(ctx.key, ctx.account_id, contact),
        }
    }
}

pub(super) fn execute(
    ctx: &ReadContext<'_>,
    name: &str,
    args: &Map<String, Value>,
) -> Result<Value, Failure> {
    let cache = ctx
        .store
        .contact_cache_snapshot(&ctx.account.id)
        .map_err(storage)?;
    let provider = ctx.account.provider;
    let default_book = BookKey::default_for(provider, cache.connection.as_ref());
    let book_of = |contact: &Contact| {
        let book = cache.card_books.get(&contact.id).map_or("", String::as_str);
        BookKey::for_contact(provider, contact, book, cache.connection.as_ref())
    };
    let coverage = coverage(provider, cache.connection.as_ref().map(|(kind, _)| *kind));

    if name == GET {
        let input: GetArgs = parse(args)?;
        let contact = cache
            .contacts
            .iter()
            .find(|contact| ctx.id("contact", &contact.id) == input.contact_id)
            .ok_or_else(|| {
                Failure::new(
                    ErrorCode::AccessDenied,
                    "Contact is not available for this account.",
                )
            })?;
        return Ok(json!({
            "contact": ContactDetails::new(ctx, contact, book_of(contact), default_book),
            "coverage": coverage
        }));
    }

    let mut books = BTreeMap::new();
    if let Some(book) = default_book {
        books.insert(book, 0);
    }
    for contact in &cache.contacts {
        *books.entry(book_of(contact)).or_insert(0) += 1;
    }

    let mut result = match name {
        LIST => {
            let mut occurrences = BTreeMap::new();
            let items = books
                .into_iter()
                .map(|(book, cached_contacts)| {
                    let index = occurrences.entry(book.source).or_insert(0);
                    *index += 1;
                    let label = book.source.label();
                    let name = match book.source {
                        Source::CardDav | Source::MicrosoftGraph if book.raw.is_empty() => {
                            format!("{label} (collection unknown)")
                        }
                        Source::CardDav | Source::MicrosoftGraph => format!("{label} {index}"),
                        _ => label.into(),
                    };
                    BookSummary {
                        address_book_id: book.id(ctx),
                        name,
                        source: book.source,
                        read_only: book.read_only(default_book),
                        is_default: Some(book) == default_book,
                        cached_contacts,
                    }
                })
                .collect();
            ctx.page(items, name, args)?
        }
        SEARCH => {
            let input: SearchArgs = parse(args)?;
            let query = input.query.trim();
            if query.is_empty() || input.query.chars().count() > QUERY_LIMIT {
                return Err(Failure::new(
                    ErrorCode::InvalidRequest,
                    "Provide a nonempty query of at most 256 characters.",
                ));
            }
            let selected = input
                .address_book_id
                .as_deref()
                .map(|id| {
                    books
                        .keys()
                        .copied()
                        .find(|book| book.id(ctx) == id)
                        .ok_or_else(|| {
                            Failure::new(
                                ErrorCode::AccessDenied,
                                "Address book is not available for this account.",
                            )
                        })
                })
                .transpose()?;
            let query = query.to_lowercase();
            let terms: Vec<_> = query.split_whitespace().collect();
            let mut matches: Vec<_> = cache
                .contacts
                .iter()
                .filter(|contact| selected.is_none_or(|book| book == book_of(contact)))
                .filter(|contact| contact_matches(contact, &terms))
                .map(|contact| (contact.display_name().to_lowercase(), contact))
                .collect();
            matches.sort_unstable_by(|(a_name, a), (b_name, b)| {
                a_name.cmp(b_name).then_with(|| a.id.cmp(&b.id))
            });
            let items = matches
                .into_iter()
                .map(|(_, contact)| {
                    ContactSummary::new(ctx, contact, book_of(contact), default_book)
                })
                .collect();
            ctx.page(items, name, args)?
        }
        _ => {
            return Err(Failure::new(
                ErrorCode::Unsupported,
                "Unknown contact read tool.",
            ))
        }
    };
    result["coverage"] = coverage;
    Ok(result)
}

fn coverage(provider: ProviderKind, connection: Option<ContactsCredential>) -> Value {
    let limitations: &[&str] = match (provider, connection) {
        (ProviderKind::Sample, _) => &[],
        (ProviderKind::Gmail, _) => &[
            "The organizational directory is outside the contacts cache.",
            "The cache may omit inferred contacts if the provider did not grant access to them.",
        ],
        (_, Some(ContactsCredential::Microsoft)) => &[
            "Only the default folder and cached folders containing contacts are shown.",
            "The organizational directory and folders below the first level are outside the contacts cache.",
            "Per-book permissions are unavailable in the cache.",
        ],
        (_, Some(_)) => &[
            "Only the known default book and books with visible cached contacts are shown.",
            "Book names and permissions are unavailable in the cache.",
        ],
        _ => &["No remote contacts connection is configured."],
    };
    json!({"scope": "local_cache", "partial": provider != ProviderKind::Sample && connection.is_some(), "limitations": limitations})
}

fn contact_matches(contact: &Contact, terms: &[&str]) -> bool {
    let fields: Vec<_> = contact
        .name
        .iter()
        .chain(&contact.given_name)
        .chain(&contact.family_name)
        .chain(&contact.organization)
        .chain(std::iter::once(&contact.email))
        .chain(&contact.emails)
        .map(|field| field.to_lowercase())
        .collect();
    terms
        .iter()
        .all(|term| fields.iter().any(|field| field.contains(term)))
}

fn bounded(text: &str, limit: usize) -> String {
    text[..text.floor_char_boundary(limit)].to_owned()
}

fn display_field(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(|text| bounded(text, DISPLAY_LIMIT))
}

pub(super) fn seed_sample(store: &Store, account: &Account) -> anyhow::Result<()> {
    let make = |id: &str, name: &str, email: &str, organization: &str, in_address_book| {
        let (given, family) = name.split_once(' ').unwrap_or((name, ""));
        Contact {
            id: id.into(),
            account_id: account.id.clone(),
            name: Some(name.into()),
            given_name: Some(given.into()),
            family_name: Some(family.into()),
            email: email.into(),
            emails: vec![email.into()],
            organization: Some(organization.into()),
            in_address_book,
            updated: account.created_at,
            ..Default::default()
        }
    };
    let contacts = if account.id.0 == "sample-secondary" {
        let mut alex = make(
            "sample-contact:1",
            "Alex Rivera",
            "alex@studio.example",
            "Northstar Studio",
            true,
        );
        alex.title = Some("Creative Producer".into());
        alex.emails.push("alex.production@studio.example".into());
        vec![
            alex,
            make(
                "sample-contact:2",
                "Sophie Williams",
                "sophie@example.com",
                "Field and Vine",
                true,
            ),
        ]
    } else {
        vec![
            make(
                "sample-contact:1",
                "Maya Chen",
                "maya@northstar.example",
                "Northstar Studio",
                true,
            ),
            make(
                "sample-contact:2",
                "Daniel Park",
                "daniel@studio.example",
                "Northstar Studio",
                true,
            ),
            make(
                "sample-inferred:3",
                "Priya Natarajan",
                "priya@fieldandvine.example",
                "Field and Vine",
                false,
            ),
        ]
    };
    store.replace_contacts(&account.id, &contacts)?;
    Ok(())
}

#[cfg(test)]
#[path = "contacts/tests.rs"]
mod tests;
