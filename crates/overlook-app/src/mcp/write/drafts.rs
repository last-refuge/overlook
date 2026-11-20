//! Durable edits against the account's local drafts. The write dispatcher owns the
//! transaction and records the operation result with the mutation.

mod attachments;
#[cfg(test)]
mod tests;

use overlook_core::{compose::render_markdown, time::now, Address, Draft, DraftKind, DraftState};
use overlook_store::McpWriteTxn;
use serde::Deserialize;
use serde_json::{json, Map, Value};

use super::{parse, storage, ErrorCode, Failure, WriteContext, WritePermission, WriteTool};

const CREATE: &str = "mail_draft_create";
const UPDATE: &str = "mail_draft_update";
const REPLY: &str = "mail_draft_reply";
const REPLY_ALL: &str = "mail_draft_reply_all";
const FORWARD: &str = "mail_draft_forward";
const DISCARD: &str = "mail_draft_discard";
const ADD: &str = "mail_draft_attachment_add";
const REMOVE: &str = "mail_draft_attachment_remove";
const MAX_RECIPIENTS: usize = 50;
const MAX_SUBJECT: usize = 998;
const MAX_BODY: usize = 8 * 1024;

pub(super) fn tools() -> Vec<WriteTool> {
    let opaque = json!({"type":"string","minLength":1,"maxLength":128});
    let recipients = json!({"type":"array","maxItems":MAX_RECIPIENTS,"items":{
        "type":"object","required":["email"],"additionalProperties":false,"properties":{
            "email":{"type":"string","minLength":1,"maxLength":320},
            "name":{"type":["string","null"],"maxLength":300}
        }
    }});
    let fields = json!({
        "to": recipients, "cc": recipients, "bcc": recipients,
        "subject":{"type":"string","maxLength":MAX_SUBJECT},
        "body_text":{"type":"string","maxLength":MAX_BODY,"description":"Markdown body, up to 8 KiB in UTF-8."}
    });
    let existing = json!({
        "draft_id":opaque,
        "if_revision":{"type":"string","minLength":1,"maxLength":128,"description":"Current draft revision."}
    });
    let mut response = fields.clone();
    response["message_id"] = opaque.clone();
    let mut reply = response.clone();
    reply["include_quote"] =
        json!({"type":"boolean","description":"Defaults to the profile's reply quote preference."});
    let mut update = fields.clone();
    update
        .as_object_mut()
        .unwrap()
        .extend(existing.as_object().unwrap().clone());
    let mut add = existing.clone();
    add["filename"] = json!({"type":"string","minLength":1,"maxLength":255});
    add["mime_type"] = json!({"type":"string","minLength":3,"maxLength":127,"description":"Media type without parameters."});
    add["content_base64"] = json!({"type":"string","maxLength":attachments::MAX_ENCODED,
        "description":"Standard base64 for up to 8 KiB of file content."});
    let mut remove = existing.clone();
    remove["attachment_id"] = opaque;

    let tool = |name, description, properties, required: &[&str]| {
        WriteTool::new(
            name,
            description,
            json!({"type":"object","properties":properties,"required":required}),
            WritePermission::Modify,
        )
    };
    vec![
        tool(CREATE, "Create a local mail draft.", fields, &[]),
        tool(
            UPDATE,
            "Replace the supplied draft fields and preserve omitted fields.",
            update,
            &["draft_id", "if_revision"],
        ),
        tool(
            REPLY,
            "Create a reply draft for a cached message.",
            reply.clone(),
            &["message_id"],
        ),
        tool(
            REPLY_ALL,
            "Create a reply-all draft for a cached message.",
            reply,
            &["message_id"],
        ),
        tool(
            FORWARD,
            "Create a forward draft, copying eligible cached attachments.",
            response,
            &["message_id"],
        ),
        tool(
            DISCARD,
            "Discard a local editing draft.",
            existing,
            &["draft_id", "if_revision"],
        ),
        tool(
            ADD,
            "Attach up to 8 KiB of inline file content to an editing draft.",
            add,
            &[
                "draft_id",
                "if_revision",
                "filename",
                "mime_type",
                "content_base64",
            ],
        ),
        tool(
            REMOVE,
            "Remove a listed attachment from an editing draft.",
            remove,
            &["draft_id", "if_revision", "attachment_id"],
        ),
    ]
}

#[derive(Default, Deserialize)]
struct Fields {
    #[serde(default, deserialize_with = "present")]
    to: Option<Vec<Recipient>>,
    #[serde(default, deserialize_with = "present")]
    cc: Option<Vec<Recipient>>,
    #[serde(default, deserialize_with = "present")]
    bcc: Option<Vec<Recipient>>,
    #[serde(default, deserialize_with = "present")]
    subject: Option<String>,
    #[serde(default, deserialize_with = "present")]
    body_text: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Recipient {
    email: String,
    name: Option<String>,
}

#[derive(Deserialize)]
struct ResponseInput {
    message_id: String,
    #[serde(default, deserialize_with = "present")]
    include_quote: Option<bool>,
    #[serde(flatten)]
    fields: Fields,
}

#[derive(Deserialize)]
struct Existing {
    draft_id: String,
    if_revision: String,
}

#[derive(Deserialize)]
struct Update {
    #[serde(flatten)]
    target: Existing,
    #[serde(flatten)]
    fields: Fields,
}

fn present<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    de: D,
) -> Result<Option<T>, D::Error> {
    T::deserialize(de).map(Some)
}

impl Fields {
    fn is_empty(&self) -> bool {
        self.to.is_none()
            && self.cc.is_none()
            && self.bcc.is_none()
            && self.subject.is_none()
            && self.body_text.is_none()
    }

    fn apply(self, draft: &mut Draft) -> Result<(), Failure> {
        for (replacement, target) in [
            (self.to, &mut draft.to),
            (self.cc, &mut draft.cc),
            (self.bcc, &mut draft.bcc),
        ] {
            if let Some(replacement) = replacement {
                if replacement.len() > MAX_RECIPIENTS {
                    return Err(invalid("Too many recipients in a draft field"));
                }
                *target = replacement
                    .into_iter()
                    .map(Recipient::address)
                    .collect::<Result<_, _>>()?;
            }
        }
        if let Some(subject) = self.subject {
            if invalid_header(&subject, MAX_SUBJECT) {
                return Err(invalid("Invalid draft subject"));
            }
            draft.subject = subject;
        }
        if let Some(body) = self.body_text {
            if body.len() > MAX_BODY
                || body
                    .chars()
                    .any(|c| c.is_control() && !matches!(c, '\t' | '\r' | '\n'))
            {
                return Err(invalid("Invalid draft body"));
            }
            if body != draft.body_text {
                draft.body_html = (!body.trim().is_empty()).then(|| render_markdown(&body));
                draft.body_text = body;
            }
        }
        Ok(())
    }
}

impl Recipient {
    fn address(self) -> Result<Address, Failure> {
        let address = Address::new(self.name, self.email);
        if invalid_header(&address.email, 320)
            || address.email.contains(['<', '>', ',', ';'])
            || address
                .name
                .as_deref()
                .is_some_and(|n| invalid_header(n, 300))
            || !address.looks_valid()
        {
            return Err(invalid("Invalid draft recipient"));
        }
        Ok(address)
    }
}

fn invalid_header(value: &str, limit: usize) -> bool {
    value.len() > limit || value.chars().any(char::is_control)
}

pub(super) fn execute(
    ctx: &WriteContext<'_>,
    tx: &mut McpWriteTxn<'_, '_>,
    name: &str,
    args: &Map<String, Value>,
) -> Result<Value, Failure> {
    match name {
        CREATE => {
            let mut draft = Draft::new(ctx.account.id.clone(), DraftKind::New, now());
            parse::<Fields>(args)?.apply(&mut draft)?;
            save(ctx, tx, &mut draft)
        }
        REPLY | REPLY_ALL | FORWARD => response(ctx, tx, name, parse(args)?),
        UPDATE => {
            let input: Update = parse(args)?;
            let mut draft = load_editable(ctx, tx, &input.target)?;
            if input.fields.is_empty() {
                return Err(invalid("Supply at least one draft field to update"));
            }
            input.fields.apply(&mut draft)?;
            save(ctx, tx, &mut draft)
        }
        DISCARD => {
            let draft = load_editable(ctx, tx, &parse(args)?)?;
            tx.discard_mcp_draft(&draft.id).map_err(storage)?;
            for item in &draft.attachments {
                attachments::clean_after_commit(ctx.profile, tx, &draft.id, item);
            }
            Ok(result(ctx, &draft, "discarded"))
        }
        ADD => attachments::add(ctx, tx, parse(args)?),
        REMOVE => attachments::remove(ctx, tx, parse(args)?),
        _ => Err(invalid("Unknown draft write tool")),
    }
}

fn response(
    ctx: &WriteContext<'_>,
    tx: &mut McpWriteTxn<'_, '_>,
    name: &str,
    input: ResponseInput,
) -> Result<Value, Failure> {
    check_token(&input.message_id)?;
    let source = tx
        .find_message(|id| ctx.id("mail_message", id) == input.message_id)
        .map_err(storage)?
        .ok_or_else(|| {
            Failure::new(
                ErrorCode::NotCached,
                "Message is unavailable in this account's cache",
            )
        })?;
    let kind = match name {
        REPLY => DraftKind::Reply,
        REPLY_ALL => DraftKind::ReplyAll,
        _ => DraftKind::Forward,
    };
    let quote = match (kind, input.include_quote) {
        (DraftKind::Forward, None) => true,
        (DraftKind::Forward, Some(_)) => {
            return Err(invalid("Forwards always include the original message"))
        }
        (_, Some(quote)) => quote,
        (_, None) => tx.mcp_quote_on_reply().map_err(storage)?,
    };
    let owner = Address::named(&ctx.account.display_name, &ctx.account.email);
    let mut draft =
        Draft::in_response_to_with(ctx.account.id.clone(), kind, &source, &owner, now(), quote);
    input.fields.apply(&mut draft)?;
    let skipped = if kind == DraftKind::Forward {
        Some(attachments::copy_forwarded(ctx, tx, &mut draft, &source)?)
    } else {
        None
    };
    let mut result = save(ctx, tx, &mut draft)?;
    if let Some(skipped) = skipped {
        result["attachments_skipped"] = json!(skipped);
    }
    Ok(result)
}

fn load_editable(
    ctx: &WriteContext<'_>,
    tx: &McpWriteTxn<'_, '_>,
    target: &Existing,
) -> Result<Draft, Failure> {
    check_token(&target.draft_id)?;
    check_token(&target.if_revision)?;
    let draft = tx
        .find_draft(|id| ctx.id("draft", id) == target.draft_id)
        .map_err(storage)?
        .ok_or_else(|| {
            Failure::new(
                ErrorCode::AccessDenied,
                "Draft is unavailable for this account",
            )
        })?;
    if ctx.draft_revision(&draft) != target.if_revision {
        return Err(Failure::new(
            ErrorCode::Conflict,
            "Draft revision has changed",
        ));
    }
    if draft.state != DraftState::Editing
        || tx.draft_is_open(&draft.id)
        || tx.mcp_draft_has_active_send(&draft.id).map_err(storage)?
    {
        return Err(Failure::new(
            ErrorCode::Conflict,
            "Draft is unavailable for editing",
        ));
    }
    Ok(draft)
}

fn save(
    ctx: &WriteContext<'_>,
    tx: &McpWriteTxn<'_, '_>,
    draft: &mut Draft,
) -> Result<Value, Failure> {
    draft.updated_at = now().max(draft.updated_at);
    tx.save_mcp_draft(draft).map_err(storage)?;
    Ok(result(ctx, draft, "saved"))
}

fn result(ctx: &WriteContext<'_>, draft: &Draft, status: &str) -> Value {
    json!({"draft_id":ctx.id("draft", draft.id.as_str()),"revision":ctx.draft_revision(draft),"status":status})
}

fn check_token(value: &str) -> Result<(), Failure> {
    if value.is_empty() || value.len() > 128 {
        return Err(invalid("Invalid draft reference or revision"));
    }
    Ok(())
}

fn invalid(message: &str) -> Failure {
    Failure::new(ErrorCode::InvalidRequest, message)
}
