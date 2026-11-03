//! Durable draft reconciliation. Network effects never hold a database transaction.
use super::*;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftUpload {
    pub draft: Draft,
    pub message_id: String,
    pub thread_id: Option<String>,
    #[serde(skip)]
    pub raw: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct DraftSyncEntry {
    pub draft_id: DraftId,
    pub remote: Option<RemoteDraft>,
    pub base: Option<Draft>,
    pub pending: Option<DraftUpload>,
}

/// Ignore bookkeeping and cached Markdown HTML when comparing editable content.
pub fn same_draft_content(a: &Draft, b: &Draft) -> bool {
    let normalize = |d: &Draft| {
        let mut d = d.clone();
        d.provider_id = None;
        d.created_at = 0;
        d.updated_at = 0;
        d.state = DraftState::Editing;
        d.body_html = None;
        d
    };
    normalize(a) == normalize(b)
}

/// An open editor protects its draft from incoming replacements and deletions.
/// Dropping it (including on window close) releases that protection.
pub struct DraftEditGuard {
    store: Arc<Store>,
    id: DraftId,
}

impl Drop for DraftEditGuard {
    fn drop(&mut self) {
        let mut editing = self.store.editing_drafts.lock().unwrap();
        if let Some(count) = editing.get_mut(&self.id) {
            *count -= 1;
            if *count == 0 {
                editing.remove(&self.id);
            }
        }
    }
}

impl Store {
    pub fn edit_draft(self: &Arc<Self>, id: &DraftId) -> DraftEditGuard {
        *self
            .editing_drafts
            .lock()
            .unwrap()
            .entry(id.clone())
            .or_default() += 1;
        DraftEditGuard {
            store: self.clone(),
            id: id.clone(),
        }
    }

    pub fn draft_is_open(&self, id: &DraftId) -> bool {
        self.editing_drafts.lock().unwrap().contains_key(id)
    }

    pub fn draft_sync_entries(&self, account: &AccountId) -> Result<Vec<DraftSyncEntry>> {
        self.with_read(|c| {
            let mut stmt = c.prepare("SELECT draft_id,remote_json,base_json,pending_json,pending_raw FROM draft_sync WHERE account_id=?1 ORDER BY draft_id")?;
            let rows = stmt.query_map([&account.0], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?, r.get::<_, Option<String>>(2)?, r.get::<_, Option<String>>(3)?, r.get::<_, Option<Vec<u8>>>(4)?)))?;
            let mut entries = Vec::new();
            for row in rows {
                let (id, remote, base, pending, raw) = row?;
                let mut pending: Option<DraftUpload> = pending.as_deref().map(serde_json::from_str).transpose()?;
                if let Some(pending) = &mut pending { pending.raw = raw.ok_or_else(|| StoreError::Invalid("Draft upload bytes are missing".into()))?; }
                entries.push(DraftSyncEntry {
                    draft_id: DraftId(id),
                    remote: remote.as_deref().map(serde_json::from_str).transpose()?,
                    base: base.as_deref().map(serde_json::from_str).transpose()?,
                    pending,
                });
            }
            Ok(entries)
        })
    }

    /// Persist the exact upload before contacting the provider. A concurrent autosave,
    /// discard or send cannot change the bytes used to reconcile a lost response.
    pub fn begin_draft_upload(&self, upload: &DraftUpload) -> Result<bool> {
        self.with_tx(|tx| {
            let current = load_drafts(tx, "WHERE d.id=?1 AND d.account_id=?2", params![upload.draft.id.0, upload.draft.account_id.0])?.pop();
            if current.as_ref().is_none_or(|d| d.state != DraftState::Editing || !same_draft_content(d, &upload.draft)) { return Ok(false); }
            Ok(tx.execute("UPDATE draft_sync SET pending_json=?3,pending_raw=?4 WHERE draft_id=?1 AND account_id=?2 AND pending_json IS NULL",
                params![upload.draft.id.0, upload.draft.account_id.0, serde_json::to_string(upload)?, upload.raw])? == 1)
        })
    }

    pub fn finish_draft_upload(&self, upload: &DraftUpload, remote: &RemoteDraft) -> Result<()> {
        self.with_tx(|tx| {
            let changed = tx.execute("UPDATE draft_sync SET remote_json=?3,base_json=?4,pending_json=NULL,pending_raw=NULL WHERE draft_id=?1 AND account_id=?2 AND pending_json=?5",
                params![upload.draft.id.0, upload.draft.account_id.0, serde_json::to_string(remote)?, serde_json::to_string(&upload.draft)?, serde_json::to_string(upload)?])?;
            if changed != 1 { return Err(StoreError::Invalid("Draft upload changed before acknowledgment".into())); }
            tx.execute("UPDATE drafts SET provider_id=?3 WHERE id=?1 AND account_id=?2", params![upload.draft.id.0, upload.draft.account_id.0, remote.id.0])?;
            Ok(())
        })
    }

    /// A definitive content conflict, rather than a transport failure, cancels an upload.
    pub fn cancel_draft_upload(&self, upload: &DraftUpload) -> Result<()> {
        self.with_conn(|c| {
            c.execute("UPDATE draft_sync SET pending_json=NULL,pending_raw=NULL WHERE draft_id=?1 AND account_id=?2 AND pending_json=?3",
                params![upload.draft.id.0, upload.draft.account_id.0, serde_json::to_string(upload)?])?;
            Ok(())
        })
    }

    /// Import a server version only over the local snapshot that was examined. If both
    /// changed, the caller stages a separate local copy and both commits are atomic.
    pub fn import_remote_draft(
        &self,
        expected: Option<&Draft>,
        remote: &RemoteDraft,
        draft: &Draft,
        conflict: Option<&Draft>,
    ) -> Result<bool> {
        let editing = self.editing_drafts.lock().unwrap();
        if editing.contains_key(&draft.id) {
            return Ok(false);
        }
        self.with_tx(|tx| {
            let current = load_drafts(tx, "WHERE d.id=?1", params![draft.id.0])?.pop();
            if !matches_expected(current.as_ref(), expected) { return Ok(false); }
            let pending: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM draft_sync WHERE draft_id=?1 AND pending_json IS NOT NULL)", [&draft.id.0], |r| r.get(0))?;
            if pending { return Ok(false); }
            if let Some(conflict) = conflict {
                ensure_same_account(&conflict.account_id, &draft.account_id, "draft")?;
                save_draft_tx(tx, conflict)?;
            }
            save_draft_tx(tx, draft)?;
            tx.execute("UPDATE draft_sync SET remote_json=?3,base_json=?4 WHERE draft_id=?1 AND account_id=?2",
                params![draft.id.0, draft.account_id.0, serde_json::to_string(remote)?, serde_json::to_string(draft)?])?;
            tx.execute("UPDATE drafts SET provider_id=?2 WHERE id=?1", params![draft.id.0, remote.id.0])?;
            Ok(true)
        })
    }

    /// Remote deletion removes an unchanged closed draft. A modified draft is detached
    /// from the deleted server copy and will be uploaded as a new draft.
    pub fn remote_draft_removed(&self, expected: &Draft, recover: bool) -> Result<bool> {
        let editing = self.editing_drafts.lock().unwrap();
        if editing.contains_key(&expected.id) {
            return Ok(false);
        }
        self.with_tx(|tx| {
            let current = load_drafts(tx, "WHERE d.id=?1 AND d.account_id=?2", params![expected.id.0, expected.account_id.0])?.pop();
            if !matches_expected(current.as_ref(), Some(expected)) { return Ok(false); }
            if recover {
                tx.execute("UPDATE draft_sync SET remote_json=NULL,base_json=NULL WHERE draft_id=?1 AND account_id=?2 AND pending_json IS NULL", params![expected.id.0, expected.account_id.0])?;
                tx.execute("UPDATE drafts SET provider_id=NULL WHERE id=?1", [&expected.id.0])?;
            } else {
                tx.execute("DELETE FROM drafts WHERE id=?1", [&expected.id.0])?;
                tx.execute("DELETE FROM draft_sync WHERE draft_id=?1 AND account_id=?2 AND pending_json IS NULL", params![expected.id.0, expected.account_id.0])?;
            }
            Ok(true)
        })
    }

    pub fn finish_draft_deletion(&self, account: &AccountId, entry: &DraftSyncEntry) -> Result<()> {
        self.with_tx(|tx| {
            tx.execute("DELETE FROM draft_sync WHERE draft_id=?1 AND account_id=?2 AND pending_json IS NULL AND NOT EXISTS(SELECT 1 FROM drafts WHERE id=?1)", params![entry.draft_id.0, account.0])?;
            Ok(())
        })
    }

    /// A send queued offline must be reviewed if another client changed, removed or
    /// sent its server draft. Uncertain submissions use send reconciliation instead.
    pub fn pause_changed_draft_send(&self, expected: &Draft) -> Result<bool> {
        self.with_tx(|tx| {
            let current = load_drafts(tx, "WHERE d.id=?1 AND d.account_id=?2", params![expected.id.0, expected.account_id.0])?.pop();
            if !matches_expected(current.as_ref(), Some(expected)) || !matches!(expected.state, DraftState::Queued | DraftState::Failed(_)) { return Ok(false); }
            let reason = "This draft changed or was removed on another device. Review it before sending.";
            tx.execute("UPDATE drafts SET state_json=?2,provider_id=NULL WHERE id=?1", params![expected.id.0, serde_json::to_string(&DraftState::Failed(reason.into()))?])?;
            tx.execute("UPDATE draft_sync SET remote_json=NULL,base_json=NULL WHERE draft_id=?1 AND account_id=?2", params![expected.id.0, expected.account_id.0])?;
            // Remove the unsent intent entirely. The explicit Resend action can create a
            // new operation; an automatic replay must never submit this paused message.
            tx.execute("DELETE FROM pending_operations WHERE account_id=?1 AND kind_json=?2 AND state IN ('pending','failed')", params![expected.account_id.0, serde_json::to_string(&OperationKind::SendDraft { draft_id: expected.id.clone() })?])?;
            Ok(true)
        })
    }
}

fn matches_expected(current: Option<&Draft>, expected: Option<&Draft>) -> bool {
    match (current, expected) {
        (None, None) => true,
        (Some(a), Some(b)) => a.state == b.state && same_draft_content(a, b),
        _ => false,
    }
}
