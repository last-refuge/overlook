//! Local mailbox organization and history. The write dispatcher owns the grant
//! and the transaction that commits the result and its idempotency record.

mod messages;
#[cfg(test)]
mod tests;

use super::{parse, storage, ErrorCode, Failure, WriteContext, WritePermission, WriteTool};
use overlook_core::{Label, LabelKind, OperationId, ProviderKind};
use overlook_store::{LocalChange, McpWriteTxn, Reversal};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

const ORGANIZE: &str = "mail_messages_organize";
const FOCUS: &str = "mail_focus_set";
const UNDO: &str = "mail_history_undo";
const DELETE: &str = "mail_messages_delete_permanently";
const CREATE_FOLDER: &str = "mail_folder_create";
const RENAME_FOLDER: &str = "mail_folder_rename";
const MESSAGE_ID: &str = "mail_message";
const FOLDER_ID: &str = "mail_folder";
const HISTORY_ID: &str = "history";
const MAX_MESSAGES: usize = 50;

pub(super) fn tools() -> Vec<WriteTool> {
    let message_ids = json!({
        "type":"array","minItems":1,"maxItems":MAX_MESSAGES,"uniqueItems":true,
        "items":{"type":"string","minLength":1,"maxLength":128},
        "description":"Opaque message IDs from the selected account."
    });
    let folder_id = json!({"type":"string","minLength":1,"maxLength":128,
        "description":"Opaque folder ID from mail_folders_list."});
    let folder_name = json!({"type":"string","minLength":1,"maxLength":200});
    vec![
        WriteTool::new(
            ORGANIZE,
            "Organize cached messages. IMAP moves require cached server folders. Label addition and removal are for accounts with local labels.",
            json!({
                "type":"object","required":["message_ids","action"],
                "properties":{
                    "message_ids": message_ids,
                    "action":{"type":"string","enum":["read","unread","star","unstar","archive",
                        "trash","restore","move","add_label","remove_label","junk","not_junk"]},
                    "folder_id":folder_id
                }
            }),
            WritePermission::Modify,
        ),
        WriteTool::new(
            FOCUS,
            "Set or clear the local Focused Inbox override on cached Inbox messages. Optionally apply the same preference to their incoming senders.",
            json!({
                "type":"object","required":["message_ids","focus"],
                "properties":{
                    "message_ids":message_ids,
                    "focus":{"type":"string","enum":["focused","other","automatic"]},
                    "train_senders":{"type":"boolean","default":false}
                }
            }),
            WritePermission::Modify,
        ),
        WriteTool::new(
            UNDO,
            "Undo a reversible mailbox entry from mail_history_list.",
            json!({
                "type":"object","required":["entry_id"],
                "properties":{"entry_id":{"type":"string","minLength":1,"maxLength":128}}
            }),
            WritePermission::Modify,
        ),
        WriteTool::new(
            DELETE,
            "Permanently delete the specified cached Sample messages from Trash. Every target must be eligible.",
            json!({
                "type":"object","required":["message_ids","confirm"],
                "properties":{"message_ids":message_ids,"confirm":{"type":"boolean","const":true}}
            }),
            WritePermission::PermanentDelete,
        ),
        WriteTool::new(
            CREATE_FOLDER,
            "Create a top-level local folder in a POP3 account.",
            json!({"type":"object","required":["name"],"properties":{"name":folder_name}}),
            WritePermission::Modify,
        ),
        WriteTool::new(
            RENAME_FOLDER,
            "Rename a local user folder in a POP3 account.",
            json!({"type":"object","required":["folder_id","name"],
                "properties":{"folder_id":folder_id,"name":folder_name}}),
            WritePermission::Modify,
        ),
    ]
}

pub(super) fn execute(
    ctx: &WriteContext<'_>,
    tx: &mut McpWriteTxn<'_, '_>,
    name: &str,
    args: &Map<String, Value>,
) -> Result<Value, Failure> {
    if tx.account() != &ctx.account.id {
        return Err(Failure::new(
            ErrorCode::AccessDenied,
            "The selected account is unavailable",
        ));
    }
    if !matches!(
        ctx.account.provider,
        ProviderKind::Sample | ProviderKind::Pop3 | ProviderKind::Imap
    ) {
        return Err(unsupported("Organization is unavailable for this account"));
    }
    match name {
        ORGANIZE => messages::organize(ctx, tx, parse(args)?),
        FOCUS => messages::focus(ctx, tx, parse(args)?),
        DELETE => messages::delete(ctx, tx, parse(args)?),
        UNDO => undo(ctx, tx, parse(args)?),
        CREATE_FOLDER => {
            let CreateFolder { name } = parse(args)?;
            folder(ctx, tx, None, &name)
        }
        RENAME_FOLDER => {
            let RenameFolder { folder_id, name } = parse(args)?;
            folder(ctx, tx, Some(&folder_id), &name)
        }
        _ => Err(invalid("Unknown organization tool")),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Undo {
    entry_id: String,
}

fn undo(ctx: &WriteContext<'_>, tx: &McpWriteTxn<'_, '_>, input: Undo) -> Result<Value, Failure> {
    validate_id(&input.entry_id)?;
    let entry = tx
        .organization_find_history(|id| ctx.id(HISTORY_ID, &id.to_string()) == input.entry_id)
        .map_err(storage)?
        .ok_or_else(|| not_cached("History entry is unavailable in this account"))?;
    if entry.undone_at.is_some() {
        return Err(conflict("This history entry has already been undone"));
    }
    let Reversal::Changes { changes } = entry.reversal else {
        return Err(unsupported(
            "This history entry has no supported mailbox reversal",
        ));
    };
    if changes.is_empty() {
        return Err(unsupported(
            "This history entry has no supported mailbox reversal",
        ));
    }
    // Desktop Trash operations can expunge on IMAP or send POP3 DELE. The
    // reversals generated by these tools use explicit folder changes there.
    if ctx.account.provider != ProviderKind::Sample
        && changes
            .iter()
            .any(|change| matches!(change, LocalChange::Trash { .. }))
    {
        return Err(unsupported(
            "The recorded Trash reversal depends on provider policy",
        ));
    }
    if !tx.organization_can_apply(&changes).map_err(storage)? {
        return Err(conflict(
            "The history entry can no longer be applied to the cached messages and folders",
        ));
    }
    let operations = tx
        .organization_apply(changes, ctx.account.provider == ProviderKind::Imap)
        .map_err(storage)?;
    if !tx.organization_mark_undone(entry.id).map_err(storage)? {
        return Err(conflict("This history entry has already been undone"));
    }
    Ok(json!({
        "status":"undone",
        "entry_id":input.entry_id,
        "operation_ids":operation_ids(ctx, &operations),
    }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateFolder {
    name: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RenameFolder {
    folder_id: String,
    name: String,
}

fn folder(
    ctx: &WriteContext<'_>,
    tx: &McpWriteTxn<'_, '_>,
    folder_id: Option<&str>,
    name: &str,
) -> Result<Value, Failure> {
    if ctx.account.provider != ProviderKind::Pop3 {
        return Err(unsupported(
            "Folder creation and renaming are available for local POP3 folders",
        ));
    }
    let name = name.trim();
    if overlook_provider::folder_name_problem(name, "/").is_some() {
        return Err(invalid("Invalid folder name"));
    }
    let labels = tx.organization_labels().map_err(storage)?;
    let current = if let Some(id) = folder_id {
        let label = lookup_folder(ctx, &labels, id)?;
        if label.kind != LabelKind::User || label.provider_id.is_some() {
            return Err(unsupported("Only a local user folder can be renamed"));
        }
        if label.name == name {
            return Ok(
                json!({"folder_id":id,"status":"unchanged","operation_ids":[],"entry_id":null}),
            );
        }
        Some(label)
    } else {
        None
    };
    if labels
        .iter()
        .any(|label| Some(label) != current && label.name.eq_ignore_ascii_case(name))
    {
        return Err(conflict("A folder with that name already exists"));
    }
    let (id, status, title) = if let Some(label) = current {
        if !tx
            .organization_rename_folder(&label.id, name)
            .map_err(storage)?
        {
            return Err(conflict("The folder is no longer available for renaming"));
        }
        (label.id.clone(), "renamed", "Renamed folder")
    } else {
        (
            tx.organization_create_folder(name).map_err(storage)?,
            "created",
            "Created folder",
        )
    };
    let entry = tx
        .organization_record(title, "Local POP3 folder", &Reversal::None)
        .map_err(storage)?;
    Ok(json!({
        "folder_id":ctx.id(FOLDER_ID, id.as_str()), "status":status, "operation_ids":[],
        "entry_id":ctx.id(HISTORY_ID, &entry.to_string())
    }))
}

fn lookup_folder<'a>(
    ctx: &WriteContext<'_>,
    labels: &'a [Label],
    id: &str,
) -> Result<&'a Label, Failure> {
    validate_id(id)?;
    labels
        .iter()
        .find(|label| ctx.id(FOLDER_ID, label.id.as_str()) == id)
        .ok_or_else(|| not_cached("Folder is unavailable in this account"))
}

fn validate_id(id: &str) -> Result<(), Failure> {
    if id.is_empty() || id.len() > 128 {
        return Err(invalid("Invalid record ID"));
    }
    Ok(())
}

fn operation_ids(ctx: &WriteContext<'_>, ids: &[OperationId]) -> Vec<String> {
    ids.iter()
        .map(|id| ctx.id("operation", id.as_str()))
        .collect()
}

#[derive(Serialize)]
struct Item<'a> {
    message_id: &'a str,
    status: &'static str,
    operation_ids: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    code: Option<ErrorCode>,
}

impl<'a> Item<'a> {
    fn success(message_id: &'a str, status: &'static str, operation_ids: Vec<String>) -> Self {
        Self {
            message_id,
            status,
            operation_ids,
            code: None,
        }
    }

    fn failure(message_id: &'a str, code: ErrorCode) -> Self {
        Self {
            message_id,
            status: "failed",
            operation_ids: Vec::new(),
            code: Some(code),
        }
    }
}

fn bulk_result(ctx: &WriteContext<'_>, items: &[Item<'_>], entry: Option<i64>) -> Value {
    let ops: Vec<_> = items.iter().flat_map(|item| &item.operation_ids).collect();
    // A single bulk-delete operation may be associated with every item.
    let mut seen = std::collections::HashSet::new();
    let ops: Vec<_> = ops.into_iter().filter(|id| seen.insert(*id)).collect();
    json!({
        "items":items,"operation_ids":ops,
        "entry_id":entry.map(|id| ctx.id(HISTORY_ID, &id.to_string())),
    })
}

fn invalid(message: &str) -> Failure {
    Failure::new(ErrorCode::InvalidRequest, message)
}

fn not_cached(message: &str) -> Failure {
    Failure::new(ErrorCode::NotCached, message)
}

fn unsupported(message: &str) -> Failure {
    Failure::new(ErrorCode::Unsupported, message)
}

fn conflict(message: &str) -> Failure {
    Failure::new(ErrorCode::Conflict, message)
}
