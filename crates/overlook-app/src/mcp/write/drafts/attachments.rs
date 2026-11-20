//! Outgoing file lifecycle for MCP draft changes. Attachment bytes reach disk before
//! committing the referencing row. The transaction cleans up staged files on errors
//! and retires removed files only after commit. A hard crash can leave an unreferenced
//! private file; retrying a committed write uses the receipt and never restages it.

use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};

use base64::{engine::general_purpose::STANDARD, Engine};
use overlook_core::{
    attachment_policy::{check_outgoing, sanitize_filename, within_size_limit, TOO_LARGE},
    compose::{embeds_content_id, quoted_html},
    AttachmentId, Draft, DraftAttachment, DraftId, Message,
};
use overlook_store::McpWriteTxn;
use serde::Deserialize;
use serde_json::{json, Value};

use super::{
    check_token, invalid, load_editable, save, ErrorCode, Existing, Failure, WriteContext,
};
use crate::mcp::read::mail::attachment;

const MAX_BYTES: usize = 8 * 1024;
pub(super) const MAX_ENCODED: usize = MAX_BYTES.div_ceil(3) * 4;
const MAX_ATTACHMENTS: usize = 128;
const MAX_FORWARDED: usize = 8;
const NAMESPACE: &str = "draft_attachment";
const DIRECTORY_PREFIX: &str = "mcp-";
const PAYLOAD: &str = "content";

#[derive(Deserialize)]
pub(super) struct Add {
    #[serde(flatten)]
    target: Existing,
    filename: String,
    mime_type: String,
    content_base64: String,
}

#[derive(Deserialize)]
pub(super) struct Remove {
    #[serde(flatten)]
    target: Existing,
    attachment_id: String,
}

pub(super) fn add(
    ctx: &WriteContext<'_>,
    tx: &mut McpWriteTxn<'_, '_>,
    input: Add,
) -> Result<Value, Failure> {
    let mut draft = load_editable(ctx, tx, &input.target)?;
    if input.filename.trim().is_empty()
        || input.filename.len() > 255
        || input.filename.contains(['/', '\\'])
    {
        return Err(invalid("Provide an attachment filename of up to 255 bytes"));
    }
    if !valid_media_type(&input.mime_type) {
        return Err(invalid(
            "Provide an attachment media type without parameters",
        ));
    }
    if input.content_base64.len() > MAX_ENCODED {
        return Err(invalid("Attachment content exceeds 8 KiB"));
    }
    let bytes = STANDARD
        .decode(input.content_base64)
        .map_err(|_| invalid("Invalid base64 attachment content"))?;
    if bytes.len() > MAX_BYTES {
        return Err(invalid("Attachment content exceeds 8 KiB"));
    }
    let filename = approved_name(&input.filename, &bytes)?;
    if draft.attachments.len() >= MAX_ATTACHMENTS {
        return Err(invalid(
            "Draft already has the maximum number of attachments",
        ));
    }
    let total = draft
        .attachments
        .iter()
        .fold(bytes.len() as u64, |n, a| n.saturating_add(a.size_bytes));
    if !within_size_limit(total) {
        return Err(invalid(TOO_LARGE));
    }
    let item = stage(
        ctx.profile,
        tx,
        &draft.id,
        filename,
        input.mime_type,
        &bytes,
        None,
    )?;
    let id = ctx.id(NAMESPACE, item.id.as_str());
    let filename = item.filename.clone();
    draft.attachments.push(item);
    let mut result = save(ctx, tx, &mut draft)?;
    result["attachment_id"] = json!(id);
    result["filename"] = json!(filename);
    Ok(result)
}

pub(super) fn remove(
    ctx: &WriteContext<'_>,
    tx: &mut McpWriteTxn<'_, '_>,
    input: Remove,
) -> Result<Value, Failure> {
    check_token(&input.attachment_id)?;
    let mut draft = load_editable(ctx, tx, &input.target)?;
    let index = draft
        .attachments
        .iter()
        .position(|a| a.is_listed() && ctx.id(NAMESPACE, a.id.as_str()) == input.attachment_id)
        .ok_or_else(|| {
            Failure::new(
                ErrorCode::AccessDenied,
                "Attachment is unavailable in this draft",
            )
        })?;
    let item = draft.attachments.remove(index);
    clean_after_commit(ctx.profile, tx, &draft.id, &item);
    let mut result = save(ctx, tx, &mut draft)?;
    result["attachment_id"] = json!(input.attachment_id);
    result["filename"] = json!(sanitize_filename(&item.filename));
    Ok(result)
}

pub(super) fn copy_forwarded(
    ctx: &WriteContext<'_>,
    tx: &mut McpWriteTxn<'_, '_>,
    draft: &mut Draft,
    source: &Message,
) -> Result<usize, Failure> {
    if source.attachments.is_empty() {
        return Ok(0);
    }
    let quoted = quoted_html(source);
    for item in source
        .attachments
        .iter()
        .take(MAX_FORWARDED)
        .filter(|a| a.message_id == source.id)
    {
        // The read-side cache helper enforces the account's cache layout and its
        // 24 KiB per-file limit, and never fetches attachments from a provider.
        let Ok(bytes) = attachment::bytes(ctx.account, item, ctx.profile) else {
            continue;
        };
        let Ok(filename) = approved_name(&item.filename, &bytes) else {
            continue;
        };
        let mime_type = overlook_core::attachment_policy::mime_type_for(&filename).to_owned();
        let content_id = item
            .content_id
            .clone()
            .filter(|cid| item.is_inline_image() && embeds_content_id(&quoted, cid));
        let attachment = stage(
            ctx.profile,
            tx,
            &draft.id,
            filename,
            mime_type,
            &bytes,
            content_id,
        )?;
        draft.attachments.push(attachment);
    }
    Ok(source.attachments.len() - draft.attachments.len())
}

fn valid_media_type(value: &str) -> bool {
    fn token(value: &str) -> bool {
        !value.is_empty()
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"!#$&^_.+-".contains(&b))
    }
    value.len() <= 127
        && value
            .split_once('/')
            .is_some_and(|(kind, sub)| token(kind) && token(sub))
}

fn approved_name(original: &str, bytes: &[u8]) -> Result<String, Failure> {
    let filename = sanitize_filename(original);
    check_outgoing(&filename, bytes).map_err(|_| {
        Failure::new(
            ErrorCode::Unsupported,
            "Attachment was refused by the outgoing file policy",
        )
    })?;
    Ok(filename)
}

fn staged_path(profile: &Path, draft: &DraftId, id: &AttachmentId) -> Option<PathBuf> {
    fn normal_component(value: &str) -> bool {
        let mut parts = Path::new(value).components();
        matches!(parts.next(), Some(Component::Normal(part)) if part == value)
            && parts.next().is_none()
    }
    if !normal_component(draft.as_str()) || !normal_component(id.as_str()) {
        return None;
    }
    Some(
        profile
            .join("outgoing")
            .join(draft.as_str())
            .join(format!("{DIRECTORY_PREFIX}{id}"))
            .join(PAYLOAD),
    )
}

fn stage(
    profile: &Path,
    tx: &mut McpWriteTxn<'_, '_>,
    draft: &DraftId,
    filename: String,
    mime_type: String,
    bytes: &[u8],
    content_id: Option<String>,
) -> Result<DraftAttachment, Failure> {
    let unavailable = || {
        Failure::new(
            ErrorCode::Internal,
            "Could not stage the outgoing attachment",
        )
    };
    let id = AttachmentId::generate();
    let path = staged_path(profile, draft, &id).ok_or_else(unavailable)?;
    let local_path = path.to_str().ok_or_else(unavailable)?.to_owned();
    let attachment_dir = path.parent().unwrap();
    let draft_dir = attachment_dir.parent().unwrap();
    let outgoing = draft_dir.parent().unwrap();
    ensure_dir(outgoing)
        .and_then(|_| ensure_dir(draft_dir))
        .map_err(|_| unavailable())?;
    DirBuilder::new()
        .mode(0o700)
        .create(attachment_dir)
        .map_err(|_| unavailable())?;

    let rollback_path = path.clone();
    tx.on_rollback(move || clean(&rollback_path));
    let persist = || -> io::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        for dir in [attachment_dir, draft_dir, outgoing, profile] {
            File::open(dir)?.sync_all()?;
        }
        Ok(())
    };
    persist().map_err(|_| unavailable())?;
    Ok(DraftAttachment {
        id,
        filename,
        mime_type,
        size_bytes: bytes.len() as u64,
        local_path,
        content_id,
    })
}

fn ensure_dir(path: &Path) -> io::Result<()> {
    match DirBuilder::new().mode(0o700).create(path) {
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            if fs::symlink_metadata(path)?.is_dir() {
                Ok(())
            } else {
                Err(e)
            }
        }
        result => result,
    }
}

pub(super) fn clean_after_commit(
    profile: &Path,
    tx: &mut McpWriteTxn<'_, '_>,
    draft: &DraftId,
    item: &DraftAttachment,
) {
    if let Some(path) =
        staged_path(profile, draft, &item.id).filter(|path| path == Path::new(&item.local_path))
    {
        tx.on_commit(move || clean(&path));
    }
}

fn clean(path: &Path) {
    // Only MCP's generated layout belongs to this code. Older attachment layouts
    // may contain provider or composer files, which are managed by their owners.
    let attachment_dir = path.parent().unwrap();
    let draft_dir = attachment_dir.parent().unwrap();
    let outgoing = draft_dir.parent().unwrap();
    if [outgoing, draft_dir, attachment_dir]
        .into_iter()
        .all(|dir| fs::symlink_metadata(dir).is_ok_and(|m| m.is_dir()))
    {
        let _ = fs::remove_file(path);
        let _ = fs::remove_dir(attachment_dir);
    }
}
