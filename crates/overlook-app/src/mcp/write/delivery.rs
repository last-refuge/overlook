//! Queue and cancel delivery using the shared Outbox.

use super::{ErrorCode, Failure, WriteContext, WritePermission, WriteTool, parse, storage};
use overlook_core::ProviderKind;
use overlook_store::{McpWriteTxn, StoreError};
use serde::Deserialize;
use serde_json::{Map, Value, json};

const SEND: &str = "mail_send";
const CANCEL: &str = "mail_send_cancel";

pub(super) fn tools() -> Vec<WriteTool> {
    vec![
        WriteTool::new(
            SEND,
            "Queue a saved draft for sending after explicit confirmation.",
            json!({"type":"object","properties":{
                "draft_id":{"type":"string"},
                "if_revision":{"type":"string"},
                "confirmed":{"type":"boolean","const":true}
            },"required":["draft_id","if_revision","confirmed"]}),
            WritePermission::Send,
        ),
        WriteTool::new(
            CANCEL,
            "Cancel a queued send before submission and return the draft to editing.",
            json!({"type":"object","properties":{
                "draft_id":{"type":"string"},
                "if_revision":{"type":"string"}
            },"required":["draft_id","if_revision"]}),
            WritePermission::Send,
        ),
    ]
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SendArgs {
    draft_id: String,
    if_revision: String,
    confirmed: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CancelArgs {
    draft_id: String,
    if_revision: String,
}

pub(super) fn execute(
    ctx: &WriteContext<'_>,
    tx: &mut McpWriteTxn<'_, '_>,
    name: &str,
    args: &Map<String, Value>,
) -> Result<Value, Failure> {
    let (draft_id, if_revision) = match name {
        SEND => {
            let input: SendArgs = parse(args)?;
            if !input.confirmed {
                return Err(Failure::new(
                    ErrorCode::InvalidRequest,
                    "Sending requires explicit confirmation",
                ));
            }
            (input.draft_id, input.if_revision)
        }
        CANCEL => {
            let input: CancelArgs = parse(args)?;
            (input.draft_id, input.if_revision)
        }
        _ => {
            return Err(Failure::new(
                ErrorCode::InvalidRequest,
                "Unknown delivery tool",
            ));
        }
    };
    if ctx.account.provider == ProviderKind::Local {
        return Err(Failure::new(
            ErrorCode::Unsupported,
            "This account cannot send mail",
        ));
    }
    let draft = tx
        .find_draft(|id| ctx.id("draft", id) == draft_id)
        .map_err(storage)?
        .ok_or_else(|| {
            Failure::new(
                ErrorCode::AccessDenied,
                "Draft is not available for this account",
            )
        })?;
    if tx.draft_is_open(&draft.id) {
        return Err(Failure::new(
            ErrorCode::Conflict,
            "Close the draft's composer before changing delivery",
        ));
    }
    let revision = ctx.draft_revision(&draft);
    if revision != if_revision {
        return Err(Failure::new(
            ErrorCode::Conflict,
            "The draft changed. Read it again before changing delivery",
        ));
    }
    let (draft, operation_id, status) = if name == SEND {
        if draft.body_text.trim().is_empty() && draft.subject.trim().is_empty() {
            return Err(Failure::new(
                ErrorCode::InvalidRequest,
                "Add a subject or a message before sending",
            ));
        }
        let (queued, operation) = tx.queue_send(&draft).map_err(delivery_error)?;
        (queued, operation.id, "queued")
    } else {
        let (editing, operation_id) = tx.cancel_queued_send(draft).map_err(delivery_error)?;
        (editing, operation_id, "cancelled")
    };
    Ok(json!({
        "draft_id": draft_id,
        "revision": ctx.draft_revision(&draft),
        "operation_id": ctx.id("operation", &operation_id.0),
        "status": status
    }))
}

fn delivery_error(error: StoreError) -> Failure {
    match error {
        StoreError::Invalid(message) => Failure::new(ErrorCode::InvalidRequest, message),
        StoreError::NotOwned(_) => Failure::new(
            ErrorCode::AccessDenied,
            "Draft is not available for this account",
        ),
        other => storage(other),
    }
}
