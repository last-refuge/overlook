//! Account-scoped writes through the profile owner's Store.

mod calendar;
mod contacts;
mod delivery;
mod drafts;
mod invitations;
mod organization;

use std::{fmt::Display, path::Path, sync::OnceLock};

use overlook_core::{Account, Contact, Draft, Event};
use overlook_store::{McpWriteKey, McpWriteOutcome, Store, StoreError};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use super::contract::{ErrorCode, Failure};
use super::read::{invalid, parse, record_id, ReadTool};

pub fn storage(_error: impl Display) -> Failure {
    Failure::new(ErrorCode::Internal, "Could not commit the change")
}

impl From<StoreError> for Failure {
    fn from(error: StoreError) -> Self {
        storage(error)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WritePermission {
    Modify,
    Send,
    PermanentDelete,
    Calendar,
    Invite,
    Contacts,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Domain {
    Mail,
    Calendar,
    Contacts,
}

impl WritePermission {
    pub fn domain(self) -> Domain {
        match self {
            Self::Calendar | Self::Invite => Domain::Calendar,
            Self::Contacts => Domain::Contacts,
            _ => Domain::Mail,
        }
    }
}

pub struct WriteTool {
    pub name: &'static str,
    pub description: &'static str,
    pub schema: Map<String, Value>,
    pub permission: WritePermission,
}

impl WriteTool {
    pub fn new(
        name: &'static str,
        description: &'static str,
        schema: Value,
        permission: WritePermission,
    ) -> Self {
        let mut schema = ReadTool::new(name, description, schema).schema;
        schema["properties"]["idempotency_key"] = json!({
            "type":"string","minLength":1,"maxLength":128,
            "description":"Keep the same key for retries of the same request. Use a new key for each new change."
        });
        schema["required"]
            .as_array_mut()
            .expect("write tool schema")
            .push(json!("idempotency_key"));
        Self {
            name,
            description,
            schema,
            permission,
        }
    }
}

pub fn tools() -> impl Iterator<Item = &'static WriteTool> {
    static TOOLS: OnceLock<Vec<WriteTool>> = OnceLock::new();
    TOOLS
        .get_or_init(|| {
            organization::tools()
                .into_iter()
                .chain(drafts::tools())
                .chain(delivery::tools())
                .chain(calendar::tools())
                .chain(invitations::tools())
                .chain(contacts::tools())
                .collect()
        })
        .iter()
}

pub struct WriteContext<'a> {
    pub account: &'a Account,
    pub account_id: &'a str,
    pub profile: &'a Path,
    pub key: &'a [u8; 32],
}

impl WriteContext<'_> {
    pub fn id(&self, namespace: &str, raw: &str) -> String {
        record_id(self.key, self.account_id, namespace, raw)
    }

    pub fn draft_revision(&self, draft: &Draft) -> String {
        draft_revision(self.key, self.account_id, draft)
    }

    pub fn event_revision(&self, event: &Event) -> String {
        event_revision(self.key, self.account_id, event)
    }

    pub fn contact_revision(&self, contact: &Contact) -> String {
        contact_revision(self.key, self.account_id, contact)
    }

    pub fn execute(
        &self,
        store: &Store,
        client: &str,
        tool: &WriteTool,
        idempotency_key: &str,
        args: &Map<String, Value>,
    ) -> Result<McpWriteOutcome, Failure> {
        if idempotency_key.trim().is_empty()
            || idempotency_key.len() > 128
            || idempotency_key.chars().any(char::is_control)
        {
            return Err(invalid(
                "Provide a nonempty idempotency_key of up to 128 bytes",
            ));
        }
        let properties = &tool.schema["properties"];
        if args.keys().any(|key| {
            matches!(key.as_str(), "account_id" | "idempotency_key")
                || properties.get(key).is_none()
        }) {
            return Err(invalid("Unknown tool argument"));
        }
        let mut request = Value::Object(args.clone());
        request.sort_all_objects();
        let canonical = serde_json::to_vec(&request).map_err(storage)?;
        let request_digest: [u8; 32] = Sha256::new()
            .chain_update(b"overlook-mcp-write\0")
            .chain_update(tool.name.as_bytes())
            .chain_update([0])
            .chain_update(&canonical)
            .chain_update(self.key)
            .finalize()
            .into();
        let key_digest: [u8; 32] = Sha256::new()
            .chain_update(b"overlook-mcp-idempotency\0")
            .chain_update(idempotency_key.as_bytes())
            .chain_update(self.key)
            .finalize()
            .into();

        store.mcp_write(
            McpWriteKey {
                account: &self.account.id,
                client,
                key_digest: &key_digest,
                request_digest: &request_digest,
                action: tool.name,
            },
            |tx, receipt_id| {
                let mut data = match tool.permission {
                    WritePermission::Calendar => calendar::execute(self, tx, tool.name, args),
                    WritePermission::Invite => invitations::execute(self, tx, tool.name, args),
                    WritePermission::Contacts => contacts::execute(self, tx, tool.name, args),
                    WritePermission::Send => delivery::execute(self, tx, tool.name, args),
                    WritePermission::Modify if tool.name.starts_with("mail_draft_") => {
                        drafts::execute(self, tx, tool.name, args)
                    }
                    _ => organization::execute(self, tx, tool.name, args),
                }?;
                let fields = data
                    .as_object_mut()
                    .ok_or_else(|| storage("Expected a write result object"))?;
                fields.insert("account_id".into(), json!(self.account_id));
                fields.insert(
                    "write_id".into(),
                    json!(self.id("write_receipt", receipt_id)),
                );
                Ok(data)
            },
        )
    }
}

pub(super) fn draft_revision(key: &[u8; 32], account_id: &str, draft: &Draft) -> String {
    revision(key, account_id, "draft_revision", draft)
}

pub(super) fn event_revision(key: &[u8; 32], account_id: &str, event: &Event) -> String {
    revision(key, account_id, "calendar_event_revision", event)
}

pub(super) fn contact_revision(key: &[u8; 32], account_id: &str, contact: &Contact) -> String {
    revision(key, account_id, "contact_revision", contact)
}

fn revision(key: &[u8; 32], account_id: &str, namespace: &str, entity: &impl Serialize) -> String {
    let content = serde_json::to_string(entity).expect("cached entities are serializable");
    record_id(key, account_id, namespace, &content)
}
