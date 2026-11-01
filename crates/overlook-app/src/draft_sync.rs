//! Two-way, offline-safe draft sync. A saved baseline distinguishes local edits from
//! remote edits; pending MIME bytes survive crashes, discards and sends during upload.
use overlook_core::*;
use overlook_provider::{MailProvider, ProviderError};
use overlook_store::{same_draft_content, DraftSyncEntry, DraftUpload, Store};
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::Path;

#[cfg(test)]
#[path = "draft_sync_tests.rs"]
mod tests;

fn storage(error: overlook_store::StoreError) -> ProviderError {
    ProviderError::Temporary(format!("Could not synchronize drafts: {error}"))
}
fn io(error: std::io::Error) -> ProviderError {
    ProviderError::Temporary(format!("Could not store draft attachments: {error}"))
}

/// Staging uses generated ids, never names/paths from email. Each version gets a new
/// directory, so an incoming version cannot overwrite a local/conflicting attachment.
fn stage(draft: &mut Draft, files: &[Vec<u8>], outgoing: &Path) -> Result<(), ProviderError> {
    if files.len() != draft.attachments.len() {
        return Err(ProviderError::Temporary(
            "Incomplete draft attachments".into(),
        ));
    }
    if files.is_empty() {
        return Ok(());
    }
    let parent = outgoing.join(draft.id.as_str());
    let folder = parent.join(DraftId::generate().as_str());
    std::fs::create_dir_all(&folder).map_err(io)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for path in [outgoing, parent.as_path(), folder.as_path()] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).map_err(io)?;
        }
    }
    let result = (|| {
        for (att, bytes) in draft.attachments.iter_mut().zip(files) {
            let path = folder.join(AttachmentId::generate().as_str());
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&path).map_err(io)?;
            file.write_all(bytes).map_err(io)?;
            file.sync_all().map_err(io)?;
            att.local_path = path.to_string_lossy().into_owned();
            att.size_bytes = bytes.len() as u64;
        }
        for path in [folder.as_path(), parent.as_path(), outgoing] {
            std::fs::File::open(path)
                .and_then(|f| f.sync_all())
                .map_err(io)?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_dir_all(folder);
    }
    result
}

fn conflict_copy(local: &Draft, outgoing: &Path) -> Result<Draft, ProviderError> {
    let mut copy = local.clone();
    copy.id = DraftId::generate();
    copy.provider_id = None;
    copy.subject = format!("{} (conflict copy)", copy.display_subject());
    copy.updated_at = overlook_core::time::now();
    for attachment in &mut copy.attachments {
        attachment.id = AttachmentId::generate();
    }
    let files = local
        .attachments
        .iter()
        .map(|a| std::fs::read(&a.local_path).map_err(io))
        .collect::<Result<Vec<_>, _>>()?;
    stage(&mut copy, &files, outgoing)?;
    Ok(copy)
}

async fn finish_upload(
    store: &Store,
    provider: &dyn MailProvider,
    account: &Account,
    entry: &DraftSyncEntry,
    pending: &DraftUpload,
) -> Result<bool, ProviderError> {
    let result = match provider
        .put_draft(
            account,
            entry.remote.as_ref(),
            &pending.raw,
            &pending.message_id,
            pending.thread_id.as_deref(),
        )
        .await
    {
        Err(error @ ProviderError::Rejected(_)) => {
            store.cancel_draft_upload(pending).map_err(storage)?;
            return Err(error);
        }
        other => other?,
    };
    match result {
        Some(remote) => {
            store
                .finish_draft_upload(pending, &remote)
                .map_err(storage)?;
            Ok(true)
        }
        None => {
            store.cancel_draft_upload(pending).map_err(storage)?;
            Ok(false)
        }
    }
}

fn prepare_upload(
    store: &Store,
    account: &Account,
    draft: &Draft,
    entry: &DraftSyncEntry,
) -> Result<DraftUpload, ProviderError> {
    let source = draft
        .in_reply_to
        .as_ref()
        .map(|id| store.message(id))
        .transpose()
        .map_err(storage)?
        .flatten();
    let message_id = format!("<{}@drafts.overlook.lastrefuge.ai>", DraftId::generate());
    let raw =
        overlook_provider::outgoing::build_draft_raw(account, draft, source.as_ref(), &message_id)
            .map_err(ProviderError::Rejected)?;
    let thread_id = entry
        .remote
        .as_ref()
        .and_then(|r| r.thread_id.clone())
        .or_else(|| {
            (account.provider == ProviderKind::Gmail)
                .then(|| {
                    draft.conversation_id.as_ref().and_then(|id| {
                        overlook_provider::gmail::mapping::gmail_id(&account.id, id.as_str())
                    })
                })
                .flatten()
        });
    Ok(DraftUpload {
        draft: draft.clone(),
        message_id,
        raw,
        thread_id,
    })
}

/// Return the number of local versions recovered or preserved as conflict copies.
pub async fn synchronize(
    store: &Store,
    provider: &dyn MailProvider,
    account: &Account,
    outgoing: &Path,
) -> Result<usize, ProviderError> {
    if !provider.supports_drafts() {
        return Ok(0);
    }
    let mut conflicts = 0;

    // Complete unknown outcomes before examining deletions or creating anything new.
    for entry in store.draft_sync_entries(&account.id).map_err(storage)? {
        if let Some(pending) = &entry.pending {
            finish_upload(store, provider, account, &entry, pending).await?;
        }
    }

    let remote = provider.list_drafts(account).await?;
    let mut by_id: HashMap<ProviderId, &RemoteDraft> = HashMap::new();
    for item in &remote {
        if by_id.insert(item.id.clone(), item).is_some() {
            return Err(ProviderError::Temporary(
                "The server listed a draft twice; draft synchronization will retry".into(),
            ));
        }
    }
    let entries = store.draft_sync_entries(&account.id).map_err(storage)?;
    let mut known: HashSet<ProviderId> = entries
        .iter()
        .filter_map(|e| e.remote.as_ref().map(|r| r.id.clone()))
        .collect();
    for entry in entries {
        let local = store.draft(&entry.draft_id).map_err(storage)?;
        let Some(local) = local else {
            if let Some(previous) = &entry.remote {
                if let Some(current) = by_id
                    .get(&previous.id)
                    .copied()
                    .filter(|r| r.revision != previous.revision)
                {
                    let Some(raw) = provider.fetch_draft(account, current).await? else {
                        continue;
                    };
                    let mut imported = overlook_provider::drafts::import(account, current, &raw)?;
                    stage(&mut imported.draft, &imported.files, outgoing)?;
                    if !store
                        .import_remote_draft(None, current, &imported.draft, None)
                        .map_err(storage)?
                    {
                        continue;
                    }
                    conflicts += 1;
                } else {
                    provider.delete_remote_draft(account, previous).await?;
                }
            }
            store
                .finish_draft_deletion(&account.id, &entry)
                .map_err(storage)?;
            continue;
        };
        // Sending stays in the Outbox's acceptance/reconciliation path. Its remote copy
        // is removed only after complete_send has committed, or an explicit discard.
        if local.state != DraftState::Editing {
            if let Some(previous) = &entry.remote {
                if by_id
                    .get(&previous.id)
                    .is_none_or(|current| current.revision != previous.revision)
                    && store.pause_changed_draft_send(&local).map_err(storage)?
                {
                    known.remove(&previous.id);
                    conflicts += 1;
                }
            }
            continue;
        }
        let dirty = entry
            .base
            .as_ref()
            .is_none_or(|base| !same_draft_content(&local, base));
        if let Some(previous) = &entry.remote {
            match by_id.get(&previous.id).copied() {
                None => {
                    if store.remote_draft_removed(&local, dirty).map_err(storage)? && dirty {
                        conflicts += 1;
                    }
                    continue;
                }
                Some(current) if current.revision != previous.revision => {
                    if store.draft_is_open(&local.id) {
                        continue;
                    }
                    let Some(raw) = provider.fetch_draft(account, current).await? else {
                        continue;
                    };
                    let mut imported = overlook_provider::drafts::import(account, current, &raw)?;
                    imported.draft.id = local.id.clone();
                    imported.draft.created_at = local.created_at;
                    stage(&mut imported.draft, &imported.files, outgoing)?;
                    let copy = dirty.then(|| conflict_copy(&local, outgoing)).transpose()?;
                    if store
                        .import_remote_draft(Some(&local), current, &imported.draft, copy.as_ref())
                        .map_err(storage)?
                        && dirty
                    {
                        conflicts += 1;
                    }
                    continue;
                }
                _ => {}
            }
        }
        if dirty && !local.is_empty() {
            let pending = prepare_upload(store, account, &local, &entry)?;
            if store.begin_draft_upload(&pending).map_err(storage)?
                && finish_upload(store, provider, account, &entry, &pending).await?
            {
                // New/updated copies were not in this listing. Old ids remain known
                // so a replaced IMAP UID is never re-imported from this snapshot.
                for saved in store.draft_sync_entries(&account.id).map_err(storage)? {
                    if let Some(remote) = saved.remote {
                        known.insert(remote.id);
                    }
                }
            }
        }
    }
    // Only untracked remote ids become new local drafts. Nothing is considered deleted
    // until the complete provider listing above has succeeded.
    for item in remote.iter().filter(|r| !known.contains(&r.id)) {
        let Some(raw) = provider.fetch_draft(account, item).await? else {
            continue;
        };
        let mut imported = overlook_provider::drafts::import(account, item, &raw)?;
        stage(&mut imported.draft, &imported.files, outgoing)?;
        store
            .import_remote_draft(None, item, &imported.draft, None)
            .map_err(storage)?;
    }
    // A discard/send can land while an upload is in flight. Clean up in this cycle;
    // failures leave the independent sync entry durable for the next cycle.
    flush_deletions(store, provider, account).await?;
    Ok(conflicts)
}

pub async fn flush_deletions(
    store: &Store,
    provider: &dyn MailProvider,
    account: &Account,
) -> Result<(), ProviderError> {
    if !provider.supports_drafts() {
        return Ok(());
    }
    for entry in store.draft_sync_entries(&account.id).map_err(storage)? {
        if entry.pending.is_none() && store.draft(&entry.draft_id).map_err(storage)?.is_none() {
            if let Some(remote) = &entry.remote {
                provider.delete_remote_draft(account, remote).await?;
            }
            store
                .finish_draft_deletion(&account.id, &entry)
                .map_err(storage)?;
        }
    }
    Ok(())
}
