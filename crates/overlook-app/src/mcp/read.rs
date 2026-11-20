//! Shared account, cursor and result handling for the MCP read tools.

mod calendar;
pub(super) mod contacts;
pub(super) mod mail;
mod mail_records;

use std::fmt::Display;
use std::sync::{Arc, OnceLock};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use overlook_core::{time::now, Account, ProviderKind, SyncState};
use overlook_provider::{sample::SampleProvider, MailProvider, ProviderError};
use overlook_store::Store;
use serde::{de::DeserializeOwned, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use tokio::runtime::Runtime;

use super::contract::{Capabilities, ErrorCode, Failure};

pub const MAX_ACCOUNTS: usize = 4;
const MAX_PAGE_SIZE: usize = 50;
const PAGE_BYTES: usize = 12 * 1024;
const STALE_AFTER_MS: i64 = 86_400_000;

pub struct ReadTool {
    pub name: &'static str,
    pub description: &'static str,
    pub schema: Map<String, Value>,
}

impl ReadTool {
    pub fn new(name: &'static str, description: &'static str, schema: Value) -> Self {
        let Value::Object(mut schema) = schema else {
            panic!("Read tool schemas must be objects");
        };
        let mut account_id = json!({"type":"string","minLength":1,"maxLength":128});
        account_id["description"] = json!("Opaque account ID from accounts_list");
        let properties = schema
            .entry("properties")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .expect("Read tool schema properties");
        properties.insert("account_id".into(), account_id);
        if batch_tool(name) {
            properties.insert(
                "account_ids".into(),
                json!({"type":"array","items":{"type":"string","minLength":1,"maxLength":128},
                    "minItems":1,"maxItems":MAX_ACCOUNTS,"uniqueItems":true,
                    "description":"Search these explicitly selected accounts. Use account_id to continue an individual account's page."}),
            );
        }
        let required = schema
            .entry("required")
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .expect("Read tool schema required fields");
        if batch_tool(name) {
            schema.insert(
                "oneOf".into(),
                json!([{"required":["account_id"]},{"required":["account_ids"]}]),
            );
        } else {
            required.push(json!("account_id"));
        }
        schema.insert("additionalProperties".into(), json!(false));
        Self {
            name,
            description,
            schema,
        }
    }
}

pub fn batch_tool(name: &str) -> bool {
    matches!(
        name,
        "mail_search" | "contacts_search" | "calendar_events_list"
    )
}

pub fn tools() -> impl Iterator<Item = &'static ReadTool> {
    static TOOLS: OnceLock<Vec<ReadTool>> = OnceLock::new();
    TOOLS
        .get_or_init(|| {
            mail::tools()
                .into_iter()
                .chain(calendar::tools())
                .chain(contacts::tools())
                .chain(mail_records::tools())
                .chain([
                    ReadTool::new(
                        "account_status",
                        "Inspect cached account and provider status.",
                        json!({"type":"object"}),
                    ),
                    ReadTool::new(
                        "mail_refresh",
                        "Download mail changes into the cache for one account.",
                        json!({"type":"object"}),
                    ),
                ])
                .collect()
        })
        .iter()
}

pub fn seed_sample(store: &Store, account: &Account) -> anyhow::Result<()> {
    mail::seed_sample(store, account)?;
    calendar::seed_sample(store, account)?;
    contacts::seed_sample(store, account)?;
    mail_records::seed_sample(store, account)
}

pub fn parse<T: DeserializeOwned>(args: &Map<String, Value>) -> Result<T, Failure> {
    serde_json::from_value(Value::Object(args.clone()))
        .map_err(|_| invalid("Invalid tool arguments"))
}

pub fn storage(_error: impl Display) -> Failure {
    Failure::new(ErrorCode::Internal, "Could not read the account cache")
}

pub fn invalid(message: &str) -> Failure {
    Failure::new(ErrorCode::InvalidRequest, message)
}

pub struct ReadContext<'a> {
    pub store: &'a Store,
    pub account: &'a Account,
    pub account_id: &'a str,
    pub sample: &'a SampleProvider,
    pub runtime: &'a Runtime,
    pub key: &'a [u8; 32],
}

impl ReadContext<'_> {
    pub fn execute(&self, tool: &ReadTool, args: &Map<String, Value>) -> Result<Value, Failure> {
        let name = tool.name;
        let properties = &tool.schema["properties"];
        if args.keys().any(|key| properties.get(key).is_none()) {
            return Err(invalid("Unknown tool argument"));
        }
        let domain = name.split('_').next().unwrap_or_default();
        let supported = match domain {
            "calendar" => self.calendar_enabled()?,
            "contacts" => self.contacts_enabled()?,
            _ => true,
        };
        if !supported {
            return Err(Failure::new(
                ErrorCode::Unsupported,
                "This account does not have an eligible connection for this tool",
            ));
        }
        let data = match name {
            "account_status" => {
                let status = self
                    .store
                    .account_status(&self.account.id)
                    .map_err(storage)?;
                let mut value = serde_json::to_value(self.capabilities()?).map_err(storage)?;
                value["pending_operations"] = json!(status.pending_operations);
                value["outbox_items"] = json!(status.outbox_items);
                value
            }
            "mail_refresh" => self.refresh_mail()?,
            "mail_drafts_list"
            | "mail_outbox_list"
            | "mail_draft_get"
            | "mail_history_list"
            | "mail_operation_get"
            | "mail_write_activity_list"
            | "operation_get"
            | "write_activity_list" => mail_records::execute(self, name, args)?,
            _ => match domain {
                "mail" => mail::execute(self, name, args)?,
                "calendar" => calendar::execute(self, name, args)?,
                "contacts" => contacts::execute(self, name, args)?,
                _ => return Err(invalid("Unknown read tool")),
            },
        };
        let mut data = data
            .as_object()
            .cloned()
            .ok_or_else(|| storage("Expected an object"))?;
        data.insert("account_id".into(), json!(self.account_id));
        if matches!(domain, "mail" | "calendar" | "contacts") {
            data.insert("cache".into(), self.cache_status(domain)?);
            if name != "mail_refresh" {
                data.insert("content_is_untrusted".into(), json!(true));
            }
        }
        Ok(Value::Object(data))
    }

    pub fn capabilities(&self) -> Result<Capabilities, Failure> {
        let calendar = self.calendar_enabled()?;
        let contacts = self.contacts_enabled()?;
        Ok(Capabilities {
            account_id: self.account_id.to_owned(),
            mail: true,
            calendar,
            contacts,
            details: json!({
                "mail": {
                    "cached_read":true,
                    "provider_search":matches!(self.account.provider, ProviderKind::Imap | ProviderKind::Sample),
                    "refresh":matches!(self.account.provider, ProviderKind::Imap | ProviderKind::Sample),
                    "cache":self.cache_status("mail")?,
                },
                "calendar":{"cached_read":calendar,"cache":self.cache_status("calendar")?},
                "contacts":{"cached_read":contacts,"cache":self.cache_status("contacts")?},
            }),
        })
    }

    pub(super) fn calendar_enabled(&self) -> Result<bool, Failure> {
        let connection = self
            .store
            .calendar_connection(&self.account.id)
            .map_err(storage)?;
        Ok(self.account.provider == ProviderKind::Sample
            || connection.is_some_and(|c| c.enabled && eligible_url(&c.settings.server_url)))
    }

    pub(super) fn contacts_enabled(&self) -> Result<bool, Failure> {
        let connection = self
            .store
            .contacts_connection(&self.account.id)
            .map_err(storage)?;
        Ok(connection.is_none_or(|c| eligible_url(&c.server_url)))
    }

    fn cache_status(&self, domain: &str) -> Result<Value, Failure> {
        let account = &self.account.id;
        let (state, last_success) = match domain {
            "mail" => {
                let status = self.store.account_status(account).map_err(storage)?;
                let refresh = self
                    .store
                    .setting::<i64>(&refresh_key(self.account))
                    .map_err(storage)?;
                (Some(status.state), status.last_successful_sync.max(refresh))
            }
            "calendar" => {
                if let Some(connection) =
                    self.store.calendar_connection(account).map_err(storage)?
                {
                    let status = self
                        .store
                        .caldav_last_sync(&connection.id)
                        .map_err(storage)?;
                    (None, status)
                } else {
                    (
                        Some(crate::sync::calendar_status(self.store, account)),
                        None,
                    )
                }
            }
            _ => {
                let last = self
                    .store
                    .setting::<i64>(&crate::sync::contacts_synced_key(account))
                    .map_err(storage)?;
                // A generic contacts timestamp does not describe the connectivity of DAV or Graph.
                (None, last)
            }
        };
        let freshness = match last_success {
            Some(last) if now().saturating_sub(last) <= STALE_AFTER_MS => "recent",
            Some(_) => "stale",
            None => "unknown",
        };
        Ok(json!({
            "freshness": freshness,
            "last_successful_sync_ms":last_success,
            "sync_state":match state {
                Some(SyncState::Offline) => "offline",
                Some(SyncState::Syncing) => "syncing",
                Some(SyncState::Paused(_)) => "paused",
                Some(SyncState::UpToDate) => "up_to_date",
                Some(SyncState::SignInRequired) => "sign_in_required",
                Some(SyncState::SyncFailed(_)) => "sync_failed",
                None => "unknown",
            },
        }))
    }

    pub fn id(&self, namespace: &str, raw: &str) -> String {
        record_id(self.key, self.account_id, namespace, raw)
    }

    pub fn page<T: Serialize>(
        &self,
        items: Vec<T>,
        name: &str,
        args: &Map<String, Value>,
    ) -> Result<Value, Failure> {
        let limit = match args.get("limit") {
            None => 20,
            Some(value) => value
                .as_u64()
                .and_then(|n| usize::try_from(n).ok())
                .filter(|n| (1..=MAX_PAGE_SIZE).contains(n))
                .ok_or_else(|| invalid("limit must be between 1 and 50"))?,
        };
        let filters: Map<_, _> = args
            .iter()
            .filter(|(key, _)| !matches!(key.as_str(), "cursor" | "limit"))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        let mut scope = name.to_owned();
        scope.push_str(&serde_json::to_string(&filters).map_err(storage)?);
        let offset = match args.get("cursor") {
            None => 0,
            Some(value) => {
                let token = value.as_str().ok_or_else(|| invalid("Invalid cursor"))?;
                let (offset, _) = token
                    .split_once('.')
                    .ok_or_else(|| invalid("Invalid cursor"))?;
                let offset = offset
                    .parse::<usize>()
                    .map_err(|_| invalid("Invalid cursor"))?;
                if token != self.cursor(&scope, offset) || offset >= items.len() {
                    return Err(invalid(
                        "Cursor does not match this query or the cache has changed",
                    ));
                }
                offset
            }
        };
        let total = items.len();
        let mut page = Vec::new();
        let mut bytes = 0;
        for item in items.into_iter().skip(offset).take(limit) {
            let value = serde_json::to_value(item).map_err(storage)?;
            let size = serde_json::to_vec(&value).map_err(storage)?.len() + 1;
            if bytes + size > PAGE_BYTES {
                if page.is_empty() {
                    return Err(storage("Page item exceeds the byte budget"));
                }
                break;
            }
            page.push(value);
            bytes += size;
        }
        let next = offset + page.len();
        let next_cursor = (next < total).then(|| self.cursor(&scope, next));
        Ok(json!({"items":page,"next_cursor":next_cursor}))
    }

    fn cursor(&self, scope: &str, offset: usize) -> String {
        let signed = format!("{offset}:{scope}");
        format!("{offset}.{}", self.id("cursor", &signed))
    }

    pub fn mail_provider(&self) -> Result<Arc<dyn MailProvider>, Failure> {
        crate::providers::providers_for(self.account, self.sample, self.store)
            .map(|providers| providers.mail)
            .map_err(|_| {
                Failure::new(
                    ErrorCode::ProviderUnavailable,
                    "Mail provider is unavailable",
                )
            })
    }

    fn refresh_mail(&self) -> Result<Value, Failure> {
        if !matches!(
            self.account.provider,
            ProviderKind::Sample | ProviderKind::Imap
        ) {
            return Err(Failure::new(
                ErrorCode::Unsupported,
                "Read-only mail refresh is unavailable for this provider",
            ));
        }
        let provider = self.mail_provider()?;
        let Some(_lease) = self.store.try_mail_sync(&self.account.id) else {
            return Ok(json!({"status":"already_syncing"}));
        };
        let downloaded = self
            .runtime
            .block_on(crate::sync::refresh_mail_cache(
                self.store,
                provider.as_ref(),
                self.account,
            ))
            .map_err(provider_failure)?;
        self.store
            .set_setting(&refresh_key(self.account), &now())
            .map_err(storage)?;
        let queued = crate::sync::queued_downloads(self.store, &self.account.id);
        Ok(
            json!({"status":"updated_cache","downloaded":downloaded,"queued_downloads":queued,"partial":queued > 0}),
        )
    }
}

pub(super) fn record_id(key: &[u8; 32], account_id: &str, namespace: &str, raw: &str) -> String {
    let digest = Sha256::new()
        .chain_update(b"overlook-mcp-record\0")
        .chain_update(account_id.as_bytes())
        .chain_update([0])
        .chain_update(namespace.as_bytes())
        .chain_update([0])
        .chain_update(raw.as_bytes())
        .chain_update(key)
        .finalize();
    URL_SAFE_NO_PAD.encode(digest)
}

fn refresh_key(account: &Account) -> String {
    format!("mcp_read_refresh:{}", account.id.0)
}

pub fn provider_failure(_error: ProviderError) -> Failure {
    Failure::new(
        ErrorCode::ProviderUnavailable,
        "The provider could not complete the requested read",
    )
}

pub fn eligible_account(store: &Store, account: &Account) -> Result<bool, Failure> {
    match account.provider {
        ProviderKind::Sample => Ok(matches!(account.id.as_str(), "sample" | "sample-secondary")),
        ProviderKind::Imap | ProviderKind::Pop3 => {
            let servers = store.servers(&account.id).map_err(storage)?;
            Ok(servers.is_some_and(|s| {
                eligible_host(&s.incoming.host)
                    && eligible_host(&s.outgoing.host)
                    && s.incoming
                        .username
                        .rsplit_once('@')
                        .is_none_or(|(_, host)| eligible_host(host))
                    && s.outgoing
                        .username
                        .rsplit_once('@')
                        .is_none_or(|(_, host)| eligible_host(host))
            }) && account
                .email
                .rsplit_once('@')
                .is_some_and(|(_, host)| eligible_host(host)))
        }
        ProviderKind::Gmail | ProviderKind::Local => Ok(false),
    }
}

pub(super) fn eligible_url(url: &str) -> bool {
    url::Url::parse(url)
        .ok()
        .is_some_and(|url| url.host_str().is_some_and(eligible_host))
}

fn eligible_host(host: &str) -> bool {
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    !host.is_empty()
        && ![
            "gmail.com",
            "googlemail.com",
            "google.com",
            "googleapis.com",
            "googleusercontent.com",
        ]
        .iter()
        .any(|blocked| {
            host == *blocked
                || host
                    .strip_suffix(blocked)
                    .is_some_and(|prefix| prefix.ends_with('.'))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use overlook_core::{AccountId, MailServers, Security, ServerEndpoint};

    #[test]
    fn account_eligibility_uses_current_provider_configuration() {
        let store = Store::open_in_memory().unwrap();
        let mut account = Account {
            id: AccountId::from("imap:example@example.test"),
            display_name: "IMAP test".into(),
            email: "example@example.test".into(),
            provider: ProviderKind::Imap,
            created_at: 1,
        };
        store.upsert_account(&account).unwrap();
        assert!(!eligible_account(&store, &account).unwrap());
        let mut servers = MailServers {
            incoming: ServerEndpoint::new("imap.example.test", 993, Security::Tls, &account.email),
            outgoing: ServerEndpoint::new("smtp.example.test", 465, Security::Tls, &account.email),
            pop3: None,
            imap: None,
            oauth: None,
        };
        store.set_servers(&account.id, &servers).unwrap();
        assert!(eligible_account(&store, &account).unwrap());
        servers.incoming.host = "Imap.Googlemail.com.".into();
        store.set_servers(&account.id, &servers).unwrap();
        assert!(!eligible_account(&store, &account).unwrap());
        servers.incoming.host = "imap.example.test".into();
        servers.outgoing.username = "example@gmail.com".into();
        store.set_servers(&account.id, &servers).unwrap();
        assert!(!eligible_account(&store, &account).unwrap());
        account.provider = ProviderKind::Gmail;
        assert!(!eligible_account(&store, &account).unwrap());
    }
}
