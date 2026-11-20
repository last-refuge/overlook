//! Local draft, Outbox, and change history reads.

use overlook_core::{time::now, Account, Address, Draft, DraftKind, DraftState};
use overlook_store::Store;
use serde::Deserialize;
use serde_json::{json, Map, Value};

use super::{parse, storage, ErrorCode, Failure, ReadContext, ReadTool};

const DRAFTS: &str = "mail_drafts_list";
const OUTBOX: &str = "mail_outbox_list";
const GET: &str = "mail_draft_get";
const HISTORY: &str = "mail_history_list";
const HISTORY_WINDOW: usize = overlook_store::HISTORY_MAX_ENTRIES;

pub(super) fn tools() -> Vec<ReadTool> {
    let paged = json!({"type":"object","properties":{
        "limit":{"type":"integer","minimum":1,"maximum":50},
        "cursor":{"type":"string"}
    }});
    vec![
        ReadTool::new(DRAFTS, "List drafts saved on this device.", paged.clone()),
        ReadTool::new(
            OUTBOX,
            "List queued, uncertain, and failed outgoing mail.",
            paged.clone(),
        ),
        ReadTool::new(
            HISTORY,
            "List recent local mail changes for this account.",
            paged,
        ),
        ReadTool::new(
            GET,
            "Read a saved draft or Outbox item as plain text.",
            json!({"type":"object","properties":{"draft_id":{"type":"string"}},"required":["draft_id"]}),
        ),
    ]
}

pub(super) fn execute(
    ctx: &ReadContext<'_>,
    name: &str,
    args: &Map<String, Value>,
) -> Result<Value, Failure> {
    let account = &ctx.account.id;
    match name {
        DRAFTS | OUTBOX => {
            let mut drafts = ctx.store.drafts(account, name == OUTBOX).map_err(storage)?;
            drafts.sort_by(|a, b| {
                b.updated_at
                    .cmp(&a.updated_at)
                    .then_with(|| a.id.cmp(&b.id))
            });
            ctx.page(
                drafts.iter().map(|draft| summary(ctx, draft)).collect(),
                name,
                args,
            )
        }
        GET => {
            #[derive(Deserialize)]
            struct Args {
                draft_id: String,
            }
            let input: Args = parse(args)?;
            let draft = ctx
                .store
                .drafts_for_account(account)
                .map_err(storage)?
                .into_iter()
                .find(|draft| ctx.id("draft", draft.id.as_str()) == input.draft_id)
                .ok_or_else(|| {
                    Failure::new(
                        ErrorCode::AccessDenied,
                        "Draft is not available for this account",
                    )
                })?;
            let mut entry = summary(ctx, &draft);
            entry["body_text"] = json!(bounded(&draft.body_text, 12 * 1024));
            entry["body_truncated"] = json!(draft.body_text.len() > 12 * 1024);
            entry["to"] = addresses(&draft.to);
            entry["cc"] = addresses(&draft.cc);
            entry["bcc"] = addresses(&draft.bcc);
            entry["recipients_truncated"] = json!([&draft.to, &draft.cc, &draft.bcc]
                .iter()
                .any(|items| items.len() > 20));
            Ok(json!({"draft":entry}))
        }
        HISTORY => {
            let history = ctx
                .store
                .history(account, HISTORY_WINDOW + 1)
                .map_err(storage)?;
            let partial = history.len() > HISTORY_WINDOW;
            let items = history.into_iter().take(HISTORY_WINDOW).map(|entry| json!({
                "entry_id":ctx.id("history", &entry.id.to_string()),
                "at_ms":entry.at,"title":bounded(&entry.title, 300),"detail":bounded(&entry.detail, 500),
                "undone_at_ms":entry.undone_at
            })).collect();
            let mut result = ctx.page(items, name, args)?;
            result["coverage"] =
                json!({"scope":"local_history","partial":partial,"maximum_entries":HISTORY_WINDOW});
            Ok(result)
        }
        _ => Err(super::invalid("Unknown mail record tool")),
    }
}

fn summary(ctx: &ReadContext<'_>, draft: &Draft) -> Value {
    let state = match &draft.state {
        DraftState::Editing => "editing",
        DraftState::Queued => "queued",
        DraftState::Sending => "sending",
        DraftState::Uncertain => "uncertain",
        DraftState::Failed(_) => "failed",
        DraftState::Sent => "sent",
    };
    json!({"draft_id":ctx.id("draft", draft.id.as_str()),"subject":bounded(&draft.subject, 500),
        "updated_at_ms":draft.updated_at,"state":state,"attachment_count":draft.attachments.iter().filter(|item| item.is_listed()).count()})
}

fn bounded(text: &str, limit: usize) -> &str {
    &text[..text.floor_char_boundary(limit)]
}

fn addresses(addresses: &[Address]) -> Value {
    json!(addresses
        .iter()
        .take(20)
        .map(|address| json!({
            "email":bounded(&address.email, 320),
            "name":address.name.as_deref().map(|name| bounded(name, 300))
        }))
        .collect::<Vec<_>>())
}

pub(super) fn seed_sample(store: &Store, account: &Account) -> anyhow::Result<()> {
    if account.id.as_str() == "sample" {
        let mut draft = Draft::new(account.id.clone(), DraftKind::New, now());
        draft
            .to
            .push(Address::named("Alex Rivera", "alex@studio.example"));
        draft.subject = "Launch day follow-up".into();
        draft.body_text = "Thanks for your help with the launch.".into();
        store.queue_send(&draft)?;
    }
    Ok(())
}
