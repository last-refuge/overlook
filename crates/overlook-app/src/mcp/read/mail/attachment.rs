use std::ffi::CString;
use std::fs::File;
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::{ffi::OsStrExt, fs::MetadataExt};
use std::path::{Component, Path};

use base64::{engine::general_purpose::STANDARD, Engine};
use overlook_core::attachment_policy::{is_executable_content, risk, sanitize_filename, Risk};
use overlook_core::{Account, Attachment, AttachmentId, MessageId, ProviderId, ProviderKind};
use overlook_store::Store;
use serde::Deserialize;
use serde_json::{json, Map, Value};

use super::{
    display, load_message, not_cached, parse, ErrorCode, Failure, ReadContext, ATTACHMENT,
};

const MAX_BYTES: u64 = 24 * 1024;
const SAMPLE_PART: &str = "mcp-sample-attachment-v1";
const SAMPLE_FILENAME: &str = "sample-notes.txt";
const SAMPLE_BYTES: &[u8] =
    b"Overlook sample mailbox attachment.\nFictional notes for the project handoff.\n";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    message_id: String,
    attachment_id: String,
}

pub(super) fn metadata(ctx: &ReadContext<'_>, attachment: &Attachment) -> Value {
    json!({
        "account_id": ctx.account_id,
        "attachment_id": ctx.id(ATTACHMENT, attachment.id.as_str()),
        "filename": display(&sanitize_filename(&attachment.filename), 160),
        "mime_type": display(&attachment.mime_type, 80),
        "size_bytes": attachment.size_bytes,
        "cache_reference_present": attachment.local_path.is_some() || is_sample(ctx.account, attachment),
    })
}

pub(super) fn execute(ctx: &ReadContext<'_>, args: &Map<String, Value>) -> Result<Value, Failure> {
    let input: Input = parse(args)?;
    let message = load_message(ctx, &input.message_id)?;
    let attachment = message
        .attachments
        .iter()
        .find(|a| {
            a.message_id == message.id && ctx.id(ATTACHMENT, a.id.as_str()) == input.attachment_id
        })
        .ok_or_else(|| not_cached("Attachment is unavailable in the selected message's cache"))?;
    let bytes = bytes(ctx.account, attachment, &crate::data_dir())?;
    let mut result = metadata(ctx, attachment);
    result["message_id"] = json!(input.message_id);
    result["encoding"] = json!("base64");
    result["size_bytes"] = json!(bytes.len());
    result["content"] = json!(STANDARD.encode(bytes));
    Ok(result)
}

fn too_large() -> Failure {
    Failure::new(
        ErrorCode::Unsupported,
        "Attachment exceeds the 24 KiB transfer limit",
    )
}

pub(in crate::mcp) fn bytes(
    account: &Account,
    attachment: &Attachment,
    profile: &Path,
) -> Result<Vec<u8>, Failure> {
    if attachment.size_bytes > MAX_BYTES {
        return Err(too_large());
    }
    let refused = || {
        Failure::new(
            ErrorCode::Unsupported,
            "Executable attachments are unavailable for transfer",
        )
    };
    if risk(&attachment.filename) == Risk::Dangerous {
        return Err(refused());
    }
    if is_sample(account, attachment) {
        return Ok(SAMPLE_BYTES.to_vec());
    }
    let unavailable =
        || not_cached("Attachment bytes are unavailable in the selected account's cache");
    let Some(path) = attachment.local_path.as_deref().map(Path::new) else {
        return Err(unavailable());
    };

    // Match the desktop cache layout, then walk it with no symlink traversal. A stored
    // path is evidence of a cache entry and never directly becomes the path to open.
    let account_dir = account.id.as_str().replace([':', '/'], "_");
    let message_dir = attachment.message_id.as_str().replace([':', '/'], "_");
    let attachment_dir: String = attachment
        .id
        .as_str()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let attachment_dir = if attachment_dir.is_empty() {
        "att"
    } else {
        &attachment_dir
    };
    let filename: String = attachment
        .filename
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | '\0') {
                '_'
            } else {
                c
            }
        })
        .collect();
    let filename = if filename.trim().is_empty() || filename.starts_with('.') {
        format!("attachment{filename}")
    } else {
        filename
    };
    let root = profile.join("attachments");
    let parts = [
        account_dir.as_str(),
        &message_dir,
        attachment_dir,
        &filename,
    ];
    let expected = parts.iter().fold(root.clone(), |dir, part| dir.join(part));
    if expected != path {
        return Err(unavailable());
    }
    let file = open_cache_file(&root, &parts).map_err(|_| unavailable())?;
    let metadata = file.metadata().map_err(|_| unavailable())?;
    if !metadata.is_file() || metadata.nlink() != 1 {
        return Err(unavailable());
    }
    if metadata.len() > MAX_BYTES {
        return Err(too_large());
    }
    let mut out = Vec::new();
    file.take(MAX_BYTES + 1)
        .read_to_end(&mut out)
        .map_err(|_| unavailable())?;
    if out.len() as u64 > MAX_BYTES {
        return Err(too_large());
    }
    if is_executable_content(&out) {
        return Err(refused());
    }
    Ok(out)
}

fn open_cache_file(root: &Path, parts: &[&str]) -> std::io::Result<File> {
    fn open_at(parent: libc::c_int, path: &Path, directory: bool) -> std::io::Result<File> {
        let path = CString::new(path.as_os_str().as_bytes())?;
        let flags = libc::O_RDONLY
            | libc::O_NOFOLLOW
            | libc::O_CLOEXEC
            | libc::O_NONBLOCK
            | if directory { libc::O_DIRECTORY } else { 0 };
        // The resulting file owns the descriptor. Failed opens create no descriptor.
        let fd = unsafe { libc::openat(parent, path.as_ptr(), flags) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    let mut current = open_at(libc::AT_FDCWD, root, true)?;
    for (index, part) in parts.iter().enumerate() {
        let path = Path::new(part);
        if !matches!(
            (path.components().next(), path.components().count()),
            (Some(Component::Normal(_)), 1)
        ) {
            return Err(std::io::Error::from(std::io::ErrorKind::InvalidInput));
        }
        current = open_at(current.as_raw_fd(), path, index + 1 < parts.len())?;
    }
    Ok(current)
}

fn sample_id(message: &MessageId) -> AttachmentId {
    AttachmentId(format!("{message}:mcp-sample-notes"))
}

fn is_sample(account: &Account, attachment: &Attachment) -> bool {
    account.provider == ProviderKind::Sample
        && attachment.id == sample_id(&attachment.message_id)
        && attachment
            .provider_id
            .as_ref()
            .is_some_and(|id| id.as_str() == SAMPLE_PART)
}

pub(super) fn seed_sample(store: &Store, account: &Account) -> anyhow::Result<()> {
    anyhow::ensure!(
        account.provider == ProviderKind::Sample,
        "MCP attachment fixture requires a sample account"
    );
    let id = MessageId(format!("{}:msg-1", account.id));
    let mut message = store
        .message(&id)?
        .filter(|m| m.account_id == account.id)
        .ok_or_else(|| anyhow::anyhow!("The sample mailbox must be seeded first"))?;
    let attachment_id = sample_id(&id);
    if !message.attachments.iter().any(|a| a.id == attachment_id) {
        message.attachments.push(Attachment {
            id: attachment_id,
            message_id: id,
            filename: SAMPLE_FILENAME.into(),
            mime_type: "text/plain".into(),
            size_bytes: SAMPLE_BYTES.len() as u64,
            local_path: None,
            provider_id: Some(ProviderId::from(SAMPLE_PART)),
            content_id: None,
        });
        store.upsert_messages(&[message])?;
    }
    Ok(())
}
