//! Account-scoped MCP mail reads from the owner's Store and provider.

mod attachment;
mod search;
#[cfg(test)]
mod tests;

use overlook_core::{Address, Message, MessageId};
use serde::Deserialize;
use serde_json::{json, Map, Value};

use super::super::contract::ErrorCode;
use super::{parse, storage, Failure, ReadContext, ReadTool};

const FOLDER: &str = "mail_folder";
const CONVERSATION: &str = "mail_conversation";
const MESSAGE: &str = "mail_message";
const ATTACHMENT: &str = "mail_attachment";
const FOLDERS_LIST: &str = "mail_folders_list";
const MESSAGES_LIST: &str = "mail_messages_list";
const SEARCH: &str = "mail_search";
const CONVERSATION_GET: &str = "mail_conversation_get";
const MESSAGE_GET: &str = "mail_message_get";
const ATTACHMENT_GET: &str = "mail_attachment_get";

pub(super) fn tools() -> Vec<ReadTool> {
    vec![
        ReadTool::new(FOLDERS_LIST, "List cached mail folders for the selected account.", schema(json!({}), &[], true)),
        ReadTool::new(MESSAGES_LIST, "List cached message headers for the selected folder or account.", search::schema(false)),
        ReadTool::new(SEARCH, "Search cached mail or explicitly request a bounded provider search. Check coverage before interpreting zero hits.", search::schema(true)),
        ReadTool::new(CONVERSATION_GET, "List cached message headers in a conversation, oldest first.", schema(json!({
            "conversation_id": {"type": "string", "description": "Opaque ID from a search result."}
        }), &["conversation_id"], true)),
        ReadTool::new(MESSAGE_GET, "Read cached plain text and a page of attachment metadata without marking a message read.", schema(json!({
            "message_id": {"type": "string", "description": "Opaque ID from search or conversation results."}
        }), &["message_id"], true)),
        ReadTool::new(ATTACHMENT_GET, "Read up to 24 KiB of attachment content from the local cache as base64.", schema(json!({
            "message_id": {"type": "string"},
            "attachment_id": {"type": "string"}
        }), &["message_id", "attachment_id"], false)),
    ]
}

fn schema(mut properties: Value, required: &[&str], paginated: bool) -> Value {
    if paginated {
        properties["limit"] = json!({"type": "integer", "minimum": 1});
        properties["cursor"] = json!({"type": "string"});
    }
    json!({"type": "object", "properties": properties, "required": required, "additionalProperties": false})
}

pub(super) fn execute(
    ctx: &ReadContext<'_>,
    name: &str,
    args: &Map<String, Value>,
) -> Result<Value, Failure> {
    match name {
        FOLDERS_LIST => folders(ctx, args),
        MESSAGES_LIST | SEARCH => search::execute(ctx, name, args),
        CONVERSATION_GET => conversation(ctx, args),
        MESSAGE_GET => message(ctx, args),
        ATTACHMENT_GET => attachment::execute(ctx, args),
        _ => Err(Failure::new(ErrorCode::Unsupported, "Unknown mail tool")),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Page {
    #[serde(rename = "limit")]
    _limit: Option<usize>,
    #[serde(rename = "cursor")]
    _cursor: Option<String>,
}

fn folders(ctx: &ReadContext<'_>, args: &Map<String, Value>) -> Result<Value, Failure> {
    let _: Page = parse(args)?;
    let mut labels = ctx.store.labels(&ctx.account.id).map_err(storage)?;
    labels.sort_unstable_by(|a, b| a.sort_order.cmp(&b.sort_order).then(a.id.cmp(&b.id)));
    let items = labels.into_iter().map(|label| json!({
        "account_id": ctx.account_id,
        "folder_id": ctx.id(FOLDER, label.id.as_str()),
        "name": display(&label.name, 240),
        "kind": label.kind.as_key(),
        "searchable_in_cache": !label.kind.is_virtual() && label.kind != overlook_core::LabelKind::Container,
    })).collect();
    ctx.page::<Value>(items, FOLDERS_LIST, args)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConversationArgs {
    conversation_id: String,
    #[serde(rename = "limit")]
    _limit: Option<usize>,
    #[serde(rename = "cursor")]
    _cursor: Option<String>,
}

fn conversation(ctx: &ReadContext<'_>, args: &Map<String, Value>) -> Result<Value, Failure> {
    let input: ConversationArgs = parse(args)?;
    let (_, conversation) = ctx
        .store
        .mcp_mail_keys(&ctx.account.id)
        .map_err(storage)?
        .into_iter()
        .find(|(_, conv)| ctx.id(CONVERSATION, conv.as_str()) == input.conversation_id)
        .ok_or_else(|| not_cached("Conversation is unavailable in the selected account's cache"))?;
    let mut messages = ctx
        .store
        .mcp_mail_conversation_headers(&ctx.account.id, &conversation)
        .map_err(storage)?;
    if messages.is_empty() {
        return Err(not_cached(
            "Conversation is unavailable in the selected account's cache",
        ));
    }
    messages.sort_unstable_by(|a, b| a.date.cmp(&b.date).then(a.id.cmp(&b.id)));
    let mut data = ctx.page(
        messages.iter().map(|m| summary(ctx, m)).collect(),
        CONVERSATION_GET,
        args,
    )?;
    data["account_id"] = json!(ctx.account_id);
    data["conversation_id"] = json!(input.conversation_id);
    data["coverage"] = json!({"scope": "cached_messages", "cached_message_count": messages.len()});
    Ok(data)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MessageArgs {
    message_id: String,
    #[serde(rename = "limit")]
    _limit: Option<usize>,
    #[serde(rename = "cursor")]
    _cursor: Option<String>,
}

fn message(ctx: &ReadContext<'_>, args: &Map<String, Value>) -> Result<Value, Failure> {
    let input: MessageArgs = parse(args)?;
    let message = load_message(ctx, &input.message_id)?;
    let mut result = summary(ctx, &message);
    for (field, addresses) in [("to", &message.to), ("cc", &message.cc)] {
        result[field] = json!(addresses.iter().take(4).map(address).collect::<Vec<_>>());
    }
    result["recipients_truncated"] = json!(message.to.len() > 4 || message.cc.len() > 4);
    // Always return metadata, even when only a header has synchronized.
    let text = message.body.text.clone().or_else(|| {
        message
            .body
            .html
            .as_deref()
            .map(overlook_core::sanitize::html_to_text)
    });
    result["body_status"] = json!(if text.is_some() {
        "cached"
    } else {
        "not_cached"
    });
    result["body_truncated"] = json!(text.as_ref().is_some_and(|s| s.len() > 4_096));
    result["body_text"] = json!(text.as_deref().map(|s| display(s, 4_096)));
    if let Some(invitation) = &message.invitation {
        result["invitation"] = json!({
            "invitation_id":ctx.id("mail_invitation", &invitation.uid),
            "method":display(&invitation.method, 16),
            "summary":invitation.summary.as_deref().map(|s| display(s, 192)),
            "start_ms":invitation.start,
            "start_date":invitation.start_date.as_deref()
                .and_then(overlook_core::tz::parse_date).map(|date| date.to_string()),
            "all_day":invitation.all_day,
            "organizer_email":invitation.organizer_email.as_deref().map(|s| display(s, 128))
        });
    }
    result["attachments"] = ctx.page(
        message
            .attachments
            .iter()
            .map(|a| attachment::metadata(ctx, a))
            .collect(),
        MESSAGE_GET,
        args,
    )?;
    Ok(result)
}

fn load_message(ctx: &ReadContext<'_>, id: &str) -> Result<Message, Failure> {
    let key = ctx
        .store
        .mcp_mail_keys(&ctx.account.id)
        .map_err(storage)?
        .into_iter()
        .map(|(message, _)| message)
        .find(|key: &MessageId| ctx.id(MESSAGE, key.as_str()) == id);
    let unavailable = || not_cached("Message is unavailable in the selected account's cache");
    ctx.store
        .message(&key.ok_or_else(unavailable)?)
        .map_err(storage)?
        .filter(|m| m.account_id == ctx.account.id)
        .ok_or_else(unavailable)
}

fn summary(ctx: &ReadContext<'_>, m: &Message) -> Value {
    json!({
        "account_id": ctx.account_id,
        "message_id": ctx.id(MESSAGE, m.id.as_str()),
        "conversation_id": ctx.id(CONVERSATION, m.conversation_id.as_str()),
        "subject": display(&m.subject, 192),
        "snippet": display(&m.snippet, 224),
        "from": address(&m.from),
        "date": m.date,
        "is_read": m.is_read,
        "is_starred": m.is_starred,
        "has_attachments": m.has_attachments(),
    })
}

fn address(a: &Address) -> Value {
    json!({"name": a.name.as_deref().map(|s| display(s, 80)), "email": display(&a.email, 128)})
}

fn display(s: &str, max_bytes: usize) -> String {
    s[..s.floor_char_boundary(max_bytes)]
        .chars()
        .filter(|c| !c.is_control() || matches!(c, '\t' | '\n' | '\r'))
        .collect()
}

fn not_cached(message: &'static str) -> Failure {
    Failure::new(ErrorCode::NotCached, message)
}

pub(super) fn seed_sample(
    store: &overlook_store::Store,
    account: &overlook_core::Account,
) -> anyhow::Result<()> {
    attachment::seed_sample(store, account)
}
