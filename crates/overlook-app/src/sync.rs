//! Synchronization engine for one account.
//!
//! Order of a cycle: probe → download remote changes (merged without clobbering pending
//! local intent) → replay pending operations in sequence → record the outcome. Provider
//! acceptance is required before anything is called Sent, and uncertain sends are
//! reconciled before any retry. Runs on a background thread; the window reloads from the
//! store afterwards.

use overlook_core::threading::assign_conversations;
use overlook_core::time::now;
use overlook_core::*;
use overlook_provider::{CalendarProvider, MailProvider, ProviderError, SendContext, SendOutcome};
use overlook_store::Store;
use std::sync::Arc;
use tracing::{info, warn};

#[derive(Debug, Default, Clone)]
pub struct SyncOutcome {
    pub state: Option<SyncState>,
    pub calendar_changes: usize,
    /// The provider asked us to slow down; the caller should wait longer before retrying.
    pub rate_limited: bool,
    pub applied: usize,
    pub downloaded: usize,
    pub sent: Vec<String>,
    pub uncertain: Vec<String>,
    pub failed: Vec<String>,
    pub stopped_offline: bool,
}

/// Recorded on a send operation the provider could neither confirm nor rule out.
pub const UNCERTAIN_SEND_WORDING: &str = "The server accepted the message but no copy was found. Send it again or discard it.";

pub const CALENDAR_SIGNIN_WORDING: &str = "Sign in again to connect your calendar. Mail keeps working meanwhile.";

pub fn calendar_status_key(account: &AccountId) -> String {
    format!("cal_status:{}", account.0)
}

pub fn calendar_status(store: &Store, account: &AccountId) -> SyncState {
    store.setting::<SyncState>(&calendar_status_key(account)).ok().flatten().unwrap_or(SyncState::Offline)
}

fn set_calendar_status(store: &Store, account: &AccountId, state: &SyncState) {
    let _ = store.set_setting(&calendar_status_key(account), state);
}

fn calendar_token_key(calendar: &CalendarId) -> String {
    format!("cal_sync:{}", calendar.0)
}

pub fn contacts_synced_key(account: &AccountId) -> String {
    format!("contacts_synced_at:{}", account.0)
}

/// Refreshes the address book once a day. Failures are logged and retried next time; they
/// never affect the mail sync state.
async fn sync_contacts(store: &Store, provider: &dyn MailProvider, account: &Account) {
    const DAY: i64 = 24 * 3_600_000;
    let key = contacts_synced_key(&account.id);
    let last = store.setting::<i64>(&key).ok().flatten().unwrap_or(0);
    if now() - last < DAY {
        return;
    }
    match provider.fetch_contacts(account).await {
        Ok(Some(contacts)) => {
            if let Err(e) = store.replace_contacts(&account.id, &contacts) {
                warn!("could not store contacts for {}: {e}", account.email);
                return;
            }
            info!("synced {} contacts for {}", contacts.len(), account.email);
            let _ = store.set_setting(&key, &now());
        }
        Ok(None) => {
            let _ = store.set_setting(&key, &now());
        }
        Err(e) => warn!("contacts sync failed for {}: {e}", account.email),
    }
}

/// Downloads calendars and events. Errors are recorded in the calendar status only; they
/// never block mail.
fn sync_calendar(store: &Store, provider: &dyn CalendarProvider, account: &Account, outcome: &mut SyncOutcome) {
    let acct = &account.id;
    if !provider.has_calendar_access() {
        set_calendar_status(store, acct, &SyncState::SignInRequired);
        return;
    }
    set_calendar_status(store, acct, &SyncState::Syncing);
    let calendars = match pollster_block(provider.list_calendars(account)) {
        Ok(c) => c,
        Err(e) => {
            set_calendar_status(store, acct, &state_for(&e));
            return;
        }
    };
    if let Err(e) = store.replace_calendars(acct, &calendars) {
        set_calendar_status(store, acct, &SyncState::SyncFailed(e.to_string()));
        return;
    }
    // Bumped when the event mapping gains fields: existing sync tokens are dropped once so
    // the window is downloaded again with the new fields.
    const CALENDAR_COVERAGE: u32 = 3;
    let coverage_key = format!("cal_coverage:{}", acct.0);
    let refresh = store.setting::<u32>(&coverage_key).ok().flatten().unwrap_or(1) < CALENDAR_COVERAGE;
    let calendars = store.calendars(acct).unwrap_or_default();
    for calendar in calendars.iter().filter(|c| c.visible) {
        let key = calendar_token_key(&calendar.id);
        let token = if refresh { None } else { store.setting::<String>(&key).ok().flatten() };
        let changes = match pollster_block(provider.fetch_events(account, calendar, token.as_deref())) {
            Ok(c) => c,
            Err(ProviderError::CheckpointExpired) => match pollster_block(provider.fetch_events(account, calendar, None)) {
                Ok(c) => c,
                Err(e) => {
                    set_calendar_status(store, acct, &state_for(&e));
                    return;
                }
            },
            Err(e) => {
                set_calendar_status(store, acct, &state_for(&e));
                return;
            }
        };
        // Local intent wins until it has been replayed: a locally deleted or edited event
        // that the server still reports must not come back in this download.
        let protected = store.pending_event_ids(acct).unwrap_or_default();
        let upserts: Vec<_> = changes.upserts.iter().filter(|e| !protected.contains(&e.id)).cloned().collect();
        let _ = store.delete_events(&changes.deleted);
        if let Err(e) = store.upsert_events(&upserts) {
            set_calendar_status(store, acct, &SyncState::SyncFailed(e.to_string()));
            return;
        }
        outcome.calendar_changes += changes.upserts.len() + changes.deleted.len();
        if let Some(t) = changes.next_sync_token {
            let _ = store.set_setting(&key, &t);
        }
    }
    let _ = store.set_setting(&coverage_key, &CALENDAR_COVERAGE);
    set_calendar_status(store, acct, &SyncState::UpToDate);
}

pub const RATE_LIMIT_WORDING: &str = "Waiting: the mail service is limiting requests. Downloading continues in a moment.";

/// Messages downloaded per batch before progress is committed to the store.
const DOWNLOAD_BATCH: usize = 20;

/// Bump when the set of views the first download covers changes; accounts downloaded with
/// an older version list the new views and queue what is missing, keeping their checkpoint.
const COVERAGE_VERSION: u32 = 4;

fn coverage_key(account: &AccountId) -> String {
    format!("download_coverage:{}", account.0)
}

/// Adds snapshot ids to the backlog (de-duplicated). With `only_missing`, ids already on
/// disk are skipped; otherwise they are re-downloaded (used when the mapping gained fields).
fn enqueue(store: &Store, account: &AccountId, ids: Vec<ProviderId>, only_missing: bool) {
    let mut backlog = backlog(store, account);
    for pid in ids {
        let present = store.message_id_for_provider_id(account, &pid).ok().flatten().is_some();
        if (!only_missing || !present) && !backlog.contains(&pid) {
            backlog.push(pid);
        }
    }
    save_backlog(store, account, &backlog);
}

/// Queues bodies for messages a provider reported header-only (`RemoteChanges::message_ids`).
/// Unlike [`enqueue`], a row that exists without a cached body is still queued.
fn enqueue_bodies(store: &Store, account: &AccountId, ids: Vec<ProviderId>) {
    let mut backlog = backlog(store, account);
    for pid in ids {
        let cached = store
            .message_id_for_provider_id(account, &pid)
            .ok()
            .flatten()
            .and_then(|id| store.message(&id).ok().flatten())
            .is_some_and(|m| m.body.is_cached());
        if !cached && !backlog.contains(&pid) {
            backlog.push(pid);
        }
    }
    save_backlog(store, account, &backlog);
}

/// Lines downloaded messages up with what is already stored, for providers whose local
/// ids are minted on first sight and whose conversations are not supplied (IMAP, POP3).
/// Gmail ids and thread ids are deterministic, so every step is a no-op for Gmail.
///
/// 1. A message whose provider id is already stored under another local id adopts that id
///    (a body download completing a header-only row).
/// 2. A new message whose Message-ID belongs to a message vanishing in the same cycle is
///    that message after a move made elsewhere: it keeps the local id and leaves `deleted`.
/// 3. A message without a conversation keeps the conversation its row already has;
///    the rest are threaded by `In-Reply-To` / `References` against the store.
pub(crate) fn reconcile_incoming(store: &Store, account: &AccountId, messages: &mut [Message], deleted: &mut Vec<MessageId>) {
    let vanished: Vec<Message> = if deleted.is_empty() || messages.iter().all(|m| m.rfc_message_id.is_none()) {
        Vec::new()
    } else {
        deleted.iter().filter_map(|id| store.message(id).ok().flatten()).collect()
    };
    for m in messages.iter_mut() {
        let mut existing = store.message(&m.id).ok().flatten();
        if existing.is_none() {
            if let Some(pid) = &m.provider_id {
                if let Ok(Some(local)) = store.message_id_for_provider_id(account, pid) {
                    if local != m.id {
                        rename_message(m, local.clone());
                        existing = store.message(&local).ok().flatten();
                    }
                }
            }
        }
        if existing.is_none() {
            if let Some(rfc) = &m.rfc_message_id {
                if let Some(moved) = vanished.iter().find(|v| v.rfc_message_id.as_deref() == Some(rfc.as_str()) && v.provider_id != m.provider_id) {
                    info!("message {} moved at the provider; keeping local id {}", rfc, moved.id);
                    deleted.retain(|d| d != &moved.id);
                    rename_message(m, moved.id.clone());
                    existing = Some(moved.clone());
                } else if let Some(local) = store.messages_by_rfc_id(account, rfc).ok().into_iter().flatten().find(|l| l.provider_id.as_ref().is_some_and(overlook_provider::imap::is_local_placeholder)) {
                    // A Sent copy kept locally because its APPEND failed: the server copy
                    // that turned up is the same message.
                    info!("server copy of {} found; replacing the local Sent copy {}", rfc, local.id);
                    rename_message(m, local.id.clone());
                    existing = Some(local);
                }
            }
        }
        if m.conversation_id.0.is_empty() {
            if let Some(local) = &existing {
                m.conversation_id = local.conversation_id.clone();
            }
        }
    }
    if messages.iter().any(|m| m.conversation_id.0.is_empty()) {
        assign_conversations(messages, |rfc_id| store.conversation_for_rfc_id(account, rfc_id).ok().flatten());
    }
}

fn rename_message(m: &mut Message, id: MessageId) {
    for (n, a) in m.attachments.iter_mut().enumerate() {
        a.message_id = id.clone();
        a.id = AttachmentId(format!("{}:att-{n}", id.0));
    }
    m.id = id;
}

fn state_for(err: &ProviderError) -> SyncState {
    match err {
        ProviderError::Offline | ProviderError::Uncertain => SyncState::Offline,
        ProviderError::SignInRequired => SyncState::SignInRequired,
        ProviderError::RateLimited(_) => SyncState::Paused(RATE_LIMIT_WORDING.into()),
        ProviderError::Temporary(m) | ProviderError::Rejected(m) => SyncState::SyncFailed(m.clone()),
        ProviderError::CheckpointExpired => SyncState::SyncFailed("sync checkpoint expired".into()),
    }
}

fn backlog_key(account: &AccountId) -> String {
    format!("download_backlog:{}", account.0)
}

pub fn backlog(store: &Store, account: &AccountId) -> Vec<ProviderId> {
    store.setting::<Vec<String>>(&backlog_key(account)).ok().flatten().unwrap_or_default().into_iter().map(ProviderId).collect()
}

fn save_backlog(store: &Store, account: &AccountId, ids: &[ProviderId]) {
    let raw: Vec<&str> = ids.iter().map(|p| p.0.as_str()).collect();
    let _ = store.set_setting(&backlog_key(account), &raw);
}

/// Downloads queued message bodies in small batches, committing after each batch so an
/// interruption (quota, offline, crash) keeps what was fetched.
fn drain_backlog(store: &Store, provider: &dyn MailProvider, account: &Account, outcome: &mut SyncOutcome) -> Result<(), ProviderError> {
    let acct = &account.id;
    let mut ids = backlog(store, acct);
    if ids.is_empty() {
        return Ok(());
    }
    let labels = store.labels(acct).unwrap_or_default();
    while !ids.is_empty() {
        let protected = store.pending_message_ids(acct).unwrap_or_default();
        let take = ids.len().min(DOWNLOAD_BATCH);
        let batch: Vec<ProviderId> = ids[..take].to_vec();
        let mut messages = match pollster_block(provider.fetch_messages(account, &batch, &labels)) {
            Ok(m) => m,
            Err(e) => {
                save_backlog(store, acct, &ids);
                return Err(e);
            }
        };
        reconcile_incoming(store, acct, &mut messages, &mut Vec::new());
        store.merge_remote_messages(&messages, &protected).map_err(|e| ProviderError::Temporary(e.to_string()))?;
        outcome.downloaded += messages.len();
        ids.drain(..take);
        save_backlog(store, acct, &ids);
    }
    Ok(())
}

/// Keeps the store's `imap_folders` (IMAP) or `pop3_seen` (POP3) rows in step with the
/// provider's checkpoint, so that state can be read without parsing the checkpoint. The
/// checkpoint stays authoritative; other providers have no such rows.
fn mirror_provider_state(store: &Store, provider: &dyn MailProvider, account: &AccountId, checkpoint: Option<&str>) {
    let Some(cp) = checkpoint else { return };
    let result = match provider.kind() {
        ProviderKind::Imap => store.replace_imap_folders(account, &overlook_provider::imap::folder_states(cp)),
        ProviderKind::Pop3 => store.replace_pop3_seen(account, &overlook_provider::pop3::seen_rows(cp)),
        _ => return,
    };
    if let Err(e) = result {
        warn!("could not record {} state for {}: {e}", provider.kind().label(), account.0);
    }
}

/// Stores a snapshot's labels. For a local-primary account (POP3) the provider's labels are
/// local too, so they are merged rather than replacing what the user made here.
fn store_snapshot_labels(store: &Store, account: &AccountId, labels: &[Label]) -> Result<(), ProviderError> {
    let local_primary = store.is_local_primary(account).unwrap_or(false);
    if local_primary {
        for label in labels {
            store.upsert_label(label).map_err(|e| ProviderError::Temporary(e.to_string()))?;
        }
        Ok(())
    } else {
        store.replace_labels(account, labels).map_err(|e| ProviderError::Temporary(e.to_string()))
    }
}

/// Downloads remote state. Returns the number of messages written.
/// Messages a queued move still has to send to the provider. Their vanishing from a folder
/// is the move's own doing (or an earlier attempt's, interrupted before the store heard),
/// so the download must not delete them: the replay re-identifies them.
fn pending_moves(store: &Store, account: &AccountId) -> std::collections::BTreeSet<MessageId> {
    store.pending_operations(account).unwrap_or_default().iter().filter(|op| op.kind.is_move()).flat_map(|op| op.kind.message_ids().iter().cloned()).collect()
}

/// `deferred` collects messages whose remote deletion was held back for a pending move,
/// with the provider id that vanished; `run_sync` deletes them after the replay when the
/// move could not find them either.
fn download(store: &Store, provider: &dyn MailProvider, account: &Account, outcome: &mut SyncOutcome, deferred: &mut Vec<(MessageId, ProviderId)>) -> Result<usize, ProviderError> {
    let acct = &account.id;
    let protected = store.pending_message_ids(acct).unwrap_or_default();
    let moving = pending_moves(store, acct);
    let checkpoint = store.checkpoint(acct).ok().flatten();
    let have_messages = store.message_count(acct).unwrap_or(0) > 0 || !backlog(store, acct).is_empty();

    let full = |outcome: &mut SyncOutcome| -> Result<usize, ProviderError> {
        let mut snapshot = pollster_block(provider.fetch_snapshot(account))?;
        store_snapshot_labels(store, acct, &snapshot.labels)?;
        reconcile_incoming(store, acct, &mut snapshot.messages, &mut Vec::new());
        store.merge_remote_messages(&snapshot.messages, &protected).map_err(|e| ProviderError::Temporary(e.to_string()))?;
        store.set_checkpoint(acct, snapshot.checkpoint.as_deref()).map_err(|e| ProviderError::Temporary(e.to_string()))?;
        mirror_provider_state(store, provider, acct, snapshot.checkpoint.as_deref());
        outcome.downloaded += snapshot.messages.len();
        // Queue bodies that are not on disk yet; the backlog survives restarts.
        enqueue(store, acct, snapshot.message_ids, true);
        let _ = store.set_setting(&coverage_key(acct), &COVERAGE_VERSION);
        drain_backlog(store, provider, account, outcome)?;
        Ok(outcome.downloaded)
    };

    // An account downloaded before the current view coverage lists the new views and queues
    // what is missing. The existing checkpoint stays valid, so nothing is skipped.
    let extend_coverage = |outcome: &mut SyncOutcome| -> Result<(), ProviderError> {
        let covered = store.setting::<u32>(&coverage_key(acct)).ok().flatten().unwrap_or(1);
        if covered >= COVERAGE_VERSION {
            return Ok(());
        }
        let mut snapshot = pollster_block(provider.fetch_snapshot(account))?;
        store_snapshot_labels(store, acct, &snapshot.labels)?;
        reconcile_incoming(store, acct, &mut snapshot.messages, &mut Vec::new());
        store.merge_remote_messages(&snapshot.messages, &protected).map_err(|e| ProviderError::Temporary(e.to_string()))?;
        // Version 3 added category/importance hints to the mapping: refresh what is stored.
        enqueue(store, acct, snapshot.message_ids, covered >= 4);
        let _ = store.set_setting(&coverage_key(acct), &COVERAGE_VERSION);
        info!("extended download coverage for {}", account.email);
        drain_backlog(store, provider, account, outcome)
    };

    match checkpoint {
        Some(cp) if have_messages => match pollster_block(provider.fetch_changes(account, &cp)) {
            Ok(mut changes) => {
                if let Some(labels) = &changes.labels {
                    store.replace_labels(acct, labels).map_err(|e| ProviderError::Temporary(e.to_string()))?;
                }
                // Providers that mint local ids on first sight report deletions by provider id.
                for pid in std::mem::take(&mut changes.deleted_provider_ids) {
                    if let Ok(Some(id)) = store.message_id_for_provider_id(acct, &pid) {
                        if moving.contains(&id) {
                            deferred.push((id, pid));
                        } else if !changes.deleted.contains(&id) {
                            changes.deleted.push(id);
                        }
                    }
                }
                // A message that vanished from one folder and appeared in another is a move,
                // not a deletion plus a new message.
                reconcile_incoming(store, acct, &mut changes.upserts, &mut changes.deleted);
                store.merge_remote_messages(&changes.upserts, &protected).map_err(|e| ProviderError::Temporary(e.to_string()))?;
                if !changes.deleted.is_empty() {
                    // Respect remote deletions even if a local change was pending on them.
                    let _ = store.delete_messages(&changes.deleted);
                }
                if let Some(new_cp) = changes.checkpoint.as_deref() {
                    store.set_checkpoint(acct, Some(new_cp)).map_err(|e| ProviderError::Temporary(e.to_string()))?;
                    mirror_provider_state(store, provider, acct, Some(new_cp));
                }
                outcome.downloaded += changes.upserts.len();
                if !changes.message_ids.is_empty() {
                    enqueue_bodies(store, acct, std::mem::take(&mut changes.message_ids));
                }
                drain_backlog(store, provider, account, outcome)?;
                extend_coverage(outcome)?;
                // One-time: re-fetch messages whose calendar part was stored before
                // invitation parsing existed.
                let invite_key = format!("invites_parsed:{}", acct.0);
                if store.setting::<bool>(&invite_key).ok().flatten() != Some(true) {
                    let ids = store.messages_needing_invitation_parse(acct).unwrap_or_default();
                    if !ids.is_empty() {
                        enqueue(store, acct, ids, false);
                        drain_backlog(store, provider, account, outcome)?;
                    }
                    let _ = store.set_setting(&invite_key, &true);
                }
                // One-time: re-fetch messages with cid: images stored before Content-ID was kept.
                let inline_key = format!("inline_cids_parsed:{}", acct.0);
                if store.setting::<bool>(&inline_key).ok().flatten() != Some(true) {
                    let ids = store.messages_needing_inline_parse(acct).unwrap_or_default();
                    if !ids.is_empty() {
                        enqueue(store, acct, ids, false);
                        drain_backlog(store, provider, account, outcome)?;
                    }
                    let _ = store.set_setting(&inline_key, &true);
                }
                Ok(outcome.downloaded)
            }
            Err(ProviderError::CheckpointExpired) => {
                info!("checkpoint expired for {}; full resynchronization", account.email);
                full(outcome)
            }
            Err(e) => Err(e),
        },
        _ => full(outcome),
    }
}

/// Sends pending local changes (sends, label changes, calendar and contact edits) to the
/// provider in order, and returns the state to report.
async fn replay_operations(
    store: &Arc<Store>,
    provider: &Arc<dyn MailProvider>,
    calendar: &Option<Arc<dyn CalendarProvider>>,
    account: &Account,
    outcome: &mut SyncOutcome,
) -> SyncState {
    let acct = account.id.clone();
    let labels = store.labels(&acct).unwrap_or_default();
    let sent_label = labels.iter().find(|l| l.kind == LabelKind::Sent).map(|l| l.id.clone());
    let calendars = store.calendars(&acct).unwrap_or_default();
    let ops = match store.pending_operations(&acct) {
        Ok(ops) => ops,
        Err(e) => return SyncState::SyncFailed(e.to_string()),
    };

    // Fewer provider calls: overridden flag changes are skipped and runs of the same change
    // go as one batch. Each sent operation stands for the queued ones in `covered`.
    let plan = overlook_core::ops::plan_replay(ops.into_iter().filter(|op| op.state != OperationState::Applied).collect());
    for id in &plan.superseded {
        let _ = store.set_operation_state(id, OperationState::Applied, None);
    }
    let set_states = |covered: &[OperationId], state: OperationState, error: Option<&str>| {
        for id in covered {
            let _ = store.set_operation_state(id, state.clone(), error);
        }
    };
    // A send waits out the undo delay in the Outbox; the window syncs again when it ends.
    let undo_ms = i64::from(store.setting::<overlook_core::prefs::Prefs>("prefs").ok().flatten().map(|p| p.undo_send_secs).unwrap_or(0)) * 1000;
    let mut final_state = SyncState::UpToDate;
    for (op, covered) in plan.send {
        if matches!(op.kind, OperationKind::SendDraft { .. }) && overlook_core::time::now() - op.created_at < undo_ms {
            continue;
        }
        set_states(&covered, OperationState::InFlight, None);
        match &op.kind {
            OperationKind::SendDraft { draft_id } => {
                let Some(draft) = store.draft(draft_id).ok().flatten() else {
                    let _ = store.set_operation_state(&op.id, OperationState::Applied, None);
                    continue;
                };
                let subject = draft.display_subject();
                let _ = store.set_draft_state(&draft.id, &DraftState::Sending);
                let context = SendContext {
                    sent_label: sent_label.clone(),
                    in_reply_to: draft.in_reply_to.as_ref().and_then(|id| store.message(id).ok().flatten()),
                    labels: labels.clone(),
                };
                let needs_reconcile = op.state == OperationState::Uncertain || draft.state == DraftState::Uncertain;
                let send_result = if needs_reconcile {
                    match provider.reconcile_send(&account, &draft).await {
                        Ok(SendOutcome::Delivered(provider_id)) => {
                            let rfc = overlook_provider::draft_message_id(&draft);
                            Ok(overlook_provider::SentReceipt {
                                message: overlook_provider::sample::sent_message(&account, &draft, context.in_reply_to.as_ref(), &provider_id, &rfc, sent_label.as_ref(), now()),
                                provider_id,
                                rfc_message_id: Some(rfc),
                            })
                        }
                        Ok(SendOutcome::NotDelivered) => provider.send(&account, &draft, context).await,
                        Ok(SendOutcome::Unknown) => {
                            // The provider cannot tell; the Outbox offers "Send again" and
                            // "Discard". Later operations are not held up.
                            let _ = store.set_operation_state(&op.id, OperationState::Uncertain, Some(UNCERTAIN_SEND_WORDING));
                            let _ = store.set_draft_state(&draft.id, &DraftState::Uncertain);
                            continue;
                        }
                        Err(e) => Err(e),
                    }
                } else {
                    provider.send(&account, &draft, context).await
                };
                match send_result {
                    Ok(receipt) => {
                        if let Err(e) = store.complete_send(&draft.id, &op.id, &receipt.message) {
                            warn!("complete_send failed: {e}");
                            let _ = store.set_operation_state(&op.id, OperationState::Failed, Some(&e.to_string()));
                            let _ = store.set_draft_state(&draft.id, &DraftState::Failed(e.to_string()));
                            outcome.failed.push(subject);
                        } else {
                            info!("sent {subject}");
                            outcome.sent.push(subject);
                            outcome.applied += 1;
                        }
                    }
                    Err(ProviderError::Uncertain) => {
                        let _ = store.set_operation_state(&op.id, OperationState::Uncertain, Some("connection dropped during sending"));
                        let _ = store.set_draft_state(&draft.id, &DraftState::Uncertain);
                        outcome.uncertain.push(subject);
                        final_state = SyncState::Offline;
                        break;
                    }
                    Err(ProviderError::Rejected(reason)) => {
                        let _ = store.set_operation_state(&op.id, OperationState::Failed, Some(&reason));
                        let _ = store.set_draft_state(&draft.id, &DraftState::Failed(reason));
                        outcome.failed.push(subject);
                    }
                    Err(err) => {
                        let _ = store.set_operation_state(&op.id, OperationState::Pending, Some(&err.to_string()));
                        let _ = store.set_draft_state(&draft.id, &DraftState::Queued);
                        final_state = state_for(&err);
                        outcome.stopped_offline = matches!(err, ProviderError::Offline);
                        outcome.rate_limited = matches!(err, ProviderError::RateLimited(_));
                        break;
                    }
                }
            }
            kind if kind.is_calendar() => {
                let Some(cal) = calendar.as_ref().filter(|c| c.has_calendar_access()) else {
                    let _ = store.set_operation_state(&op.id, OperationState::Pending, Some("calendar access needed"));
                    set_calendar_status(&store, &acct, &SyncState::SignInRequired);
                    continue;
                };
                let event = kind.event_id().and_then(|id| store.event(id).ok().flatten());
                let find_calendar = |id: &CalendarId| calendars.iter().find(|c| &c.id == id).cloned();
                let result: Result<(), ProviderError> = match kind {
                    OperationKind::RespondToEvent { response, .. } => match event.as_ref().and_then(|e| find_calendar(&e.calendar_id).map(|c| (e, c))) {
                        Some((e, c)) => cal.respond(&account, &c, e, *response).await,
                        None => Ok(()),
                    },
                    OperationKind::CreateEvent { .. } => match event.as_ref().and_then(|e| find_calendar(&e.calendar_id).map(|c| (e, c))) {
                        Some((e, c)) => cal.create_event(&account, &c, e).await.and_then(|created| store.upsert_events(&[created]).map_err(|e| ProviderError::Temporary(e.to_string()))),
                        None => Ok(()),
                    },
                    OperationKind::UpdateEvent { scope, .. } => match event.as_ref().and_then(|e| find_calendar(&e.calendar_id).map(|c| (e, c))) {
                        Some((e, c)) => cal.update_event_scoped(&account, &c, e, *scope).await.and_then(|updated| store.upsert_events(&[updated]).map_err(|e| ProviderError::Temporary(e.to_string()))),
                        None => Ok(()),
                    },
                    OperationKind::DeleteEvent { calendar_id, provider_id, scope, recurring_event_id, original_start, .. } => match (find_calendar(calendar_id), provider_id) {
                        (Some(c), Some(pid)) => cal.delete_event_scoped(&account, &c, pid, *scope, recurring_event_id.as_deref(), *original_start).await,
                        _ => Ok(()),
                    },
                    _ => Ok(()),
                };
                match result {
                    Ok(()) => {
                        let _ = store.set_operation_state(&op.id, OperationState::Applied, None);
                        outcome.applied += 1;
                    }
                    Err(ProviderError::Rejected(reason)) => {
                        let _ = store.set_operation_state(&op.id, OperationState::Failed, Some(&reason));
                        outcome.failed.push(kind.describe());
                    }
                    Err(err) => {
                        let _ = store.set_operation_state(&op.id, OperationState::Pending, Some(&err.to_string()));
                        final_state = state_for(&err);
                        outcome.stopped_offline = matches!(err, ProviderError::Offline);
                        outcome.rate_limited = matches!(err, ProviderError::RateLimited(_));
                        break;
                    }
                }
            }
            kind if kind.is_contact() => {
                let contact = kind.contact_id().and_then(|id| store.contact_by_id(&acct, id).ok().flatten());
                let result: Result<(), ProviderError> = match kind {
                    OperationKind::CreateContact { contact_id } => match &contact {
                        Some(c) => provider.create_contact(&account, c).await.and_then(|created| store.mark_contact_synced(&acct, contact_id, &created).map_err(|e| ProviderError::Temporary(e.to_string()))),
                        None => Ok(()),
                    },
                    OperationKind::UpdateContact { contact_id } => match &contact {
                        Some(c) => provider.update_contact(&account, c).await.and_then(|updated| store.mark_contact_synced(&acct, contact_id, &updated).map_err(|e| ProviderError::Temporary(e.to_string()))),
                        None => Ok(()),
                    },
                    OperationKind::DeleteContact { resource_name, .. } => match resource_name {
                        Some(r) => provider.delete_contact(&account, r).await,
                        None => Ok(()),
                    },
                    _ => Ok(()),
                };
                match result {
                    Ok(()) => {
                        let _ = store.set_operation_state(&op.id, OperationState::Applied, None);
                        outcome.applied += 1;
                    }
                    Err(ProviderError::Rejected(reason)) => {
                        let _ = store.set_operation_state(&op.id, OperationState::Failed, Some(&reason));
                        outcome.failed.push(kind.describe());
                    }
                    Err(ProviderError::SignInRequired) => {
                        let _ = store.set_operation_state(&op.id, OperationState::Pending, Some("sign in again to change contacts"));
                    }
                    Err(err) => {
                        let _ = store.set_operation_state(&op.id, OperationState::Pending, Some(&err.to_string()));
                        final_state = state_for(&err);
                        outcome.stopped_offline = matches!(err, ProviderError::Offline);
                        outcome.rate_limited = matches!(err, ProviderError::RateLimited(_));
                        break;
                    }
                }
            }
            kind => {
                let messages: Vec<Message> = kind.message_ids().iter().filter_map(|id| store.message(id).ok().flatten()).collect();
                match provider.apply(&account, &op, &messages, &labels).await {
                    Ok(applied) => {
                        for label in &applied.labels {
                            if let Err(e) = store.upsert_label(label) {
                                warn!("could not record label {}: {e}", label.name);
                            }
                        }
                        if let Err(e) = store.apply_provider_id_changes(&applied.provider_ids) {
                            warn!("could not record provider id changes: {e}");
                        }
                        set_states(&covered, OperationState::Applied, None);
                        outcome.applied += covered.len();
                    }
                    Err(ProviderError::Rejected(reason)) => {
                        set_states(&covered, OperationState::Failed, Some(&reason));
                        outcome.failed.push(kind.describe());
                        final_state = SyncState::SyncFailed(reason);
                        break;
                    }
                    Err(err) => {
                        set_states(&covered, OperationState::Pending, Some(&err.to_string()));
                        final_state = state_for(&err);
                        outcome.stopped_offline = matches!(err, ProviderError::Offline);
                        outcome.rate_limited = matches!(err, ProviderError::RateLimited(_));
                        break;
                    }
                }
            }
        }
    }
    final_state
}

pub async fn run_sync(store: Arc<Store>, provider: Arc<dyn MailProvider>, calendar: Option<Arc<dyn CalendarProvider>>, account: Account) -> SyncOutcome {
    let mut outcome = SyncOutcome::default();
    let acct = account.id.clone();

    if let Err(err) = provider.probe().await {
        let state = state_for(&err);
        let _ = store.set_sync_state(&acct, &state, None);
        outcome.state = Some(state);
        outcome.stopped_offline = true;
        return outcome;
    }
    let _ = store.set_sync_state(&acct, &SyncState::Syncing, None);

    // What the user already did goes to the server first, so an archive or a send does not
    // wait behind a download. Anything queued meanwhile goes in the pass after it.
    let pre_state = if store.pending_count(&acct).unwrap_or(0) > 0 {
        replay_operations(&store, &provider, &calendar, &account, &mut outcome).await
    } else {
        SyncState::UpToDate
    };
    if outcome.stopped_offline || outcome.rate_limited {
        let _ = store.set_sync_state(&acct, &pre_state, None);
        outcome.state = Some(pre_state);
        return outcome;
    }

    let mut deferred: Vec<(MessageId, ProviderId)> = Vec::new();
    if let Err(err) = download(&store, provider.as_ref(), &account, &mut outcome, &mut deferred) {
        warn!("download paused for {}: {err}", account.email);
        outcome.rate_limited = matches!(err, ProviderError::RateLimited(_));
        let state = state_for(&err);
        let _ = store.set_sync_state(&acct, &state, None);
        outcome.state = Some(state);
        outcome.stopped_offline = matches!(err, ProviderError::Offline);
        return outcome;
    }

    if let Some(cal) = &calendar {
        sync_calendar(&store, cal.as_ref(), &account, &mut outcome);
    }
    sync_contacts(&store, provider.as_ref(), &account).await;

    let final_state = if pre_state == SyncState::UpToDate {
        replay_operations(&store, &provider, &calendar, &account, &mut outcome).await
    } else {
        pre_state
    };

    // Held-back deletions: a move that found its message gave it a new provider id; one
    // that did not means the message really is gone.
    let gone: Vec<MessageId> = deferred.into_iter().filter(|(id, pid)| store.message(id).ok().flatten().is_some_and(|m| m.provider_id.as_ref() == Some(pid))).map(|(id, _)| id).collect();
    if !gone.is_empty() {
        info!("{} messages with a pending move are gone from the server", gone.len());
        let _ = store.delete_messages(&gone);
    }

    let success_at = if final_state == SyncState::UpToDate { Some(now()) } else { None };
    let _ = store.set_sync_state(&acct, &final_state, success_at);
    let _ = store.prune_applied(now() - 7 * 86_400_000);
    outcome.state = Some(final_state);
    outcome
}

/// Minimal executor for provider futures inside the already-blocking download step.
/// Provider futures do blocking I/O and never return `Pending`.
pub(crate) fn pollster_block<F: std::future::Future>(f: F) -> F::Output {
    use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};
    fn noop(_: *const ()) {}
    fn clone(p: *const ()) -> RawWaker {
        RawWaker::new(p, &VTABLE)
    }
    static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, noop, noop, noop);
    let waker = unsafe { Waker::from_raw(RawWaker::new(std::ptr::null(), &VTABLE)) };
    let mut cx = Context::from_waker(&waker);
    let mut f = Box::pin(f);
    loop {
        if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
        std::thread::yield_now();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use overlook_provider::sample::{sample_mailbox, SampleProvider};
    use std::time::Duration;

    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        pollster_block(f)
    }

    fn seeded() -> (Arc<Store>, Account, SampleProvider) {
        let store = Arc::new(Store::open_in_memory().unwrap());
        let account = Account {
            id: AccountId::from("sample"),
            provider: ProviderKind::Sample,
            display_name: "Jason Alexander".into(),
            email: "jason@example.com".into(),
            created_at: 0,
        };
        store.upsert_account(&account).unwrap();
        let snap = sample_mailbox(&account, now());
        for l in &snap.labels {
            store.upsert_label(l).unwrap();
        }
        store.upsert_messages(&snap.messages).unwrap();
        store.set_checkpoint(&account.id, snap.checkpoint.as_deref()).unwrap();
        (store, account, SampleProvider::new().with_latency(Duration::ZERO))
    }

    fn queued_draft(store: &Store, account: &Account) -> Draft {
        let mut draft = Draft::new(account.id.clone(), DraftKind::New, now());
        draft.to = vec![Address::named("Maya Chen", "maya@northstar.example")];
        draft.subject = "Agenda".into();
        draft.body_text = "A few things to cover.".into();
        store.save_draft(&draft).unwrap();
        store.queue_send(&draft).unwrap();
        draft
    }

    fn arc(p: &SampleProvider) -> Arc<dyn MailProvider> {
        Arc::new(p.clone())
    }

    #[test]
    fn offline_sync_leaves_everything_queued() {
        let (store, account, provider) = seeded();
        let first = store.messages_for_account(&account.id).unwrap()[0].id.clone();
        store.apply_change(&account.id, overlook_store::LocalChange::SetStarred { message_ids: vec![first], starred: true }).unwrap();
        queued_draft(&store, &account);
        let outcome = block_on(run_sync(store.clone(), arc(&provider), None, account.clone()));
        assert!(outcome.stopped_offline);
        assert_eq!(store.pending_count(&account.id).unwrap(), 2);
        assert_eq!(store.account_status(&account.id).unwrap().state, SyncState::Offline);
        assert_eq!(store.drafts(&account.id, true).unwrap()[0].state, DraftState::Queued);
    }

    #[test]
    fn first_sync_downloads_snapshot_when_store_is_empty() {
        let store = Arc::new(Store::open_in_memory().unwrap());
        let account = Account {
            id: AccountId::from("sample"),
            provider: ProviderKind::Sample,
            display_name: "Jason".into(),
            email: "jason@example.com".into(),
            created_at: 0,
        };
        store.upsert_account(&account).unwrap();
        let provider = SampleProvider::new().with_latency(Duration::ZERO);
        provider.set_online(true);
        let outcome = block_on(run_sync(store.clone(), arc(&provider), None, account.clone()));
        assert!(outcome.downloaded > 0);
        assert!(store.message_count(&account.id).unwrap() > 0);
        assert_eq!(store.checkpoint(&account.id).unwrap().as_deref(), Some("sample-1"));
        assert_eq!(store.account_status(&account.id).unwrap().state, SyncState::UpToDate);
    }

    #[test]
    fn replay_is_ordered_and_send_needs_provider_acceptance() {
        let (store, account, provider) = seeded();
        let first = store.messages_for_account(&account.id).unwrap()[0].id.clone();
        store.apply_change(&account.id, overlook_store::LocalChange::SetStarred { message_ids: vec![first.clone()], starred: true }).unwrap();
        let draft = queued_draft(&store, &account);
        store.apply_change(&account.id, overlook_store::LocalChange::Archive { message_ids: vec![first] }).unwrap();
        provider.set_online(true);
        let outcome = block_on(run_sync(store.clone(), arc(&provider), None, account.clone()));
        assert_eq!(outcome.applied, 3);
        assert_eq!(outcome.sent, vec!["Agenda".to_owned()]);
        assert_eq!(store.pending_count(&account.id).unwrap(), 0);
        assert!(store.draft(&draft.id).unwrap().is_none(), "draft row removed after acceptance");
        let sent = store.conversations(&account.id, &account.email, &overlook_store::MailboxView::Kind(LabelKind::Sent), overlook_store::ListFilter::All, None).unwrap();
        assert!(sent.iter().any(|c| c.subject == "Agenda"));
        assert_eq!(store.account_status(&account.id).unwrap().state, SyncState::UpToDate);
        assert_eq!(provider.applied_operations().len(), 2, "two idempotent operations reached the provider in order");
    }

    #[test]
    fn dropped_connection_during_send_is_uncertain_then_reconciled_without_duplicates() {
        let (store, account, provider) = seeded();
        let draft = queued_draft(&store, &account);
        provider.set_online(true);
        provider.drop_next_send();
        let outcome = block_on(run_sync(store.clone(), arc(&provider), None, account.clone()));
        assert_eq!(outcome.uncertain, vec!["Agenda".to_owned()]);
        assert_eq!(store.draft(&draft.id).unwrap().unwrap().state, DraftState::Uncertain);
        assert_eq!(store.pending_operations(&account.id).unwrap()[0].state, OperationState::Uncertain);

        let outcome = block_on(run_sync(store.clone(), arc(&provider), None, account.clone()));
        assert_eq!(outcome.sent, vec!["Agenda".to_owned()]);
        assert!(store.draft(&draft.id).unwrap().is_none());
        let sent: Vec<_> = store
            .messages_for_account(&account.id)
            .unwrap()
            .into_iter()
            .filter(|m| m.is_outgoing && m.subject == "Agenda")
            .collect();
        assert_eq!(sent.len(), 1, "exactly one Sent copy");
        assert_eq!(store.account_status(&account.id).unwrap().state, SyncState::UpToDate);
    }

    #[test]
    fn cancelled_send_is_not_sent() {
        let (store, account, provider) = seeded();
        let draft = queued_draft(&store, &account);
        store.cancel_send(&account.id, &draft.id).unwrap();
        provider.set_online(true);
        let outcome = block_on(run_sync(store.clone(), arc(&provider), None, account.clone()));
        assert!(outcome.sent.is_empty());
        assert_eq!(store.drafts(&account.id, false).unwrap().len(), 1);
    }

    // ---------------------------------------------------------------- scripted provider

    use overlook_provider::{AppliedChanges, BoxFuture, MailboxSnapshot, RemoteChanges, SentReceipt};
    use std::collections::{BTreeSet, HashMap};
    use std::sync::Mutex;

    /// A provider shaped like IMAP: local ids minted on first sight, no thread ids, header-only
    /// changes with bodies downloaded later, and provider ids that change on move.
    #[derive(Default)]
    struct Scripted {
        snapshot: Mutex<MailboxSnapshot>,
        changes: Mutex<Option<RemoteChanges>>,
        bodies: Mutex<HashMap<String, Message>>,
        applied: Mutex<AppliedChanges>,
        fetched: Mutex<Vec<String>>,
    }

    impl MailProvider for Scripted {
        fn kind(&self) -> ProviderKind {
            ProviderKind::Imap
        }
        fn probe(&self) -> BoxFuture<'_, Result<(), ProviderError>> {
            Box::pin(async { Ok(()) })
        }
        fn fetch_snapshot(&self, _account: &Account) -> BoxFuture<'_, Result<MailboxSnapshot, ProviderError>> {
            let snap = self.snapshot.lock().unwrap().clone();
            Box::pin(async move { Ok(snap) })
        }
        fn fetch_changes(&self, _account: &Account, checkpoint: &str) -> BoxFuture<'_, Result<RemoteChanges, ProviderError>> {
            let changes = self.changes.lock().unwrap().take().unwrap_or_else(|| RemoteChanges { checkpoint: Some(checkpoint.to_owned()), ..Default::default() });
            Box::pin(async move { Ok(changes) })
        }
        fn fetch_messages(&self, _account: &Account, ids: &[ProviderId], _labels: &[Label]) -> BoxFuture<'_, Result<Vec<Message>, ProviderError>> {
            let bodies = self.bodies.lock().unwrap();
            self.fetched.lock().unwrap().extend(ids.iter().map(|p| p.0.clone()));
            let found: Vec<Message> = ids.iter().filter_map(|p| bodies.get(&p.0).cloned()).collect();
            Box::pin(async move { Ok(found) })
        }
        fn apply(&self, _account: &Account, _op: &PendingOperation, _messages: &[Message], _labels: &[Label]) -> BoxFuture<'_, Result<AppliedChanges, ProviderError>> {
            let applied = std::mem::take(&mut *self.applied.lock().unwrap());
            Box::pin(async move { Ok(applied) })
        }
        fn send(&self, _account: &Account, _draft: &Draft, _context: SendContext) -> BoxFuture<'_, Result<SentReceipt, ProviderError>> {
            Box::pin(async { Err(ProviderError::Rejected("not in this test".into())) })
        }
        fn reconcile_send(&self, _account: &Account, _draft: &Draft) -> BoxFuture<'_, Result<SendOutcome, ProviderError>> {
            Box::pin(async { Ok(SendOutcome::NotDelivered) })
        }
    }

    fn imap_account() -> Account {
        Account { id: AccountId::from("imap:j"), provider: ProviderKind::Imap, display_name: "Jason".into(), email: "jason@example.com".into(), created_at: 0 }
    }

    fn folder(kind: LabelKind, path: &str) -> Label {
        Label { id: LabelId(format!("imap:j:{path}")), account_id: AccountId::from("imap:j"), kind, name: path.into(), provider_id: Some(ProviderId(path.into())), color: None, sort_order: 0 }
    }

    fn raw_message(uid: &str, folder: &str, rfc: &str, in_reply_to: Option<&str>, references: &[&str], date: i64, cached: bool) -> Message {
        Message {
            id: MessageId(format!("imap:j:{}", MessageId::generate())),
            account_id: AccountId::from("imap:j"),
            conversation_id: ConversationId(String::new()),
            provider_id: Some(ProviderId(format!("{folder}\u{1f}1\u{1f}{uid}"))),
            from: Address::named("Maya Chen", "maya@northstar.example"),
            to: vec![Address::named("Jason", "jason@example.com")],
            cc: vec![],
            bcc: vec![],
            reply_to: None,
            subject: format!("Message {uid}"),
            snippet: String::new(),
            body: if cached { Body { text: Some(format!("body {uid}")), html: None } } else { Body::default() },
            date,
            is_read: false,
            is_starred: false,
            is_outgoing: false,
            label_ids: [LabelId(format!("imap:j:{folder}"))].into_iter().collect(),
            attachments: vec![],
            rfc_message_id: Some(rfc.into()),
            in_reply_to: in_reply_to.map(str::to_owned),
            references: references.iter().map(|r| r.to_string()).collect(),
            category: None,
            is_important: false,
            focus_override: None,
            invitation: None,
        }
    }

    fn imap_seeded() -> (Arc<Store>, Account, Arc<Scripted>) {
        let store = Arc::new(Store::open_in_memory().unwrap());
        let account = imap_account();
        store.upsert_account(&account).unwrap();
        let provider = Arc::new(Scripted::default());
        *provider.snapshot.lock().unwrap() = MailboxSnapshot {
            labels: vec![folder(LabelKind::Inbox, "INBOX"), folder(LabelKind::Archive, "Archive")],
            messages: vec![
                raw_message("1", "INBOX", "<a@x>", None, &[], 1_000, true),
                raw_message("2", "INBOX", "<b@x>", Some("<a@x>"), &["<a@x>"], 2_000, true),
                raw_message("3", "INBOX", "<z@x>", None, &["<p@x>"], 3_000, true),
            ],
            message_ids: vec![],
            checkpoint: Some("cursor-1".into()),
        };
        (store, account, provider)
    }

    fn by_rfc(store: &Store, rfc: &str) -> Message {
        store.message_by_rfc_id(&AccountId::from("imap:j"), rfc).unwrap().unwrap_or_else(|| panic!("no message {rfc}"))
    }

    #[test]
    fn threads_messages_for_providers_without_thread_ids() {
        let (store, account, provider) = imap_seeded();
        let outcome = block_on(run_sync(store.clone(), provider.clone(), None, account.clone()));
        assert_eq!(outcome.state, Some(SyncState::UpToDate));
        let (a, b, z) = (by_rfc(&store, "<a@x>"), by_rfc(&store, "<b@x>"), by_rfc(&store, "<z@x>"));
        assert_eq!(a.conversation_id.as_str(), "imap:j:<a@x>");
        assert_eq!(b.conversation_id, a.conversation_id, "the reply joined its parent");
        assert_ne!(z.conversation_id, a.conversation_id);
        assert!(store.messages_for_account(&account.id).unwrap().iter().all(|m| !m.conversation_id.0.is_empty()));

        // Next cycle: a reply to b arrives, and the parent z referenced shows up late.
        *provider.changes.lock().unwrap() = Some(RemoteChanges {
            upserts: vec![raw_message("4", "INBOX", "<c@x>", Some("<b@x>"), &["<a@x>", "<b@x>"], 4_000, true), raw_message("5", "INBOX", "<p@x>", None, &[], 500, true)],
            checkpoint: Some("cursor-2".into()),
            ..Default::default()
        });
        block_on(run_sync(store.clone(), provider.clone(), None, account.clone()));
        assert_eq!(by_rfc(&store, "<c@x>").conversation_id, a.conversation_id);
        assert_eq!(by_rfc(&store, "<p@x>").conversation_id, z.conversation_id, "a parent downloaded after its reply joins the reply's conversation");
        assert_eq!(store.messages_in_conversation(&account.id, &a.conversation_id).unwrap().len(), 3);
    }

    #[test]
    fn move_seen_elsewhere_keeps_the_local_message() {
        let (store, account, provider) = imap_seeded();
        block_on(run_sync(store.clone(), provider.clone(), None, account.clone()));
        let before = by_rfc(&store, "<b@x>");
        assert!(!before.is_starred);

        // Another client starred and archived it: the UID vanished from INBOX and a new one
        // appeared in Archive. (With a local operation still pending on the message, the
        // store would keep the local flags and labels until it replays; that rule is tested
        // in the store crate and is unchanged here.)
        let mut reappeared = raw_message("9", "Archive", "<b@x>", Some("<a@x>"), &["<a@x>"], 2_000, false);
        reappeared.is_starred = true;
        *provider.changes.lock().unwrap() = Some(RemoteChanges { upserts: vec![reappeared], deleted: vec![before.id.clone()], checkpoint: Some("cursor-2".into()), ..Default::default() });
        block_on(run_sync(store.clone(), provider.clone(), None, account.clone()));

        let all = store.messages_by_rfc_id(&account.id, "<b@x>").unwrap();
        assert_eq!(all.len(), 1, "no duplicate after the move");
        let after = &all[0];
        assert_eq!(after.id, before.id, "the local id survives the move");
        assert_eq!(after.provider_id.as_ref().unwrap().as_str(), "Archive\u{1f}1\u{1f}9");
        assert_eq!(after.label_ids.iter().map(|l| l.as_str()).collect::<Vec<_>>(), vec!["imap:j:Archive"]);
        assert_eq!(after.conversation_id, before.conversation_id);
        assert!(after.is_starred, "flags come from the server copy");
        assert_eq!(after.body.text.as_deref(), Some("body 2"), "a header-only reappearance keeps the cached body");
        assert_eq!(store.message_count(&account.id).unwrap(), 3);
    }

    #[test]
    fn header_only_changes_queue_bodies_that_complete_the_same_row() {
        let (store, account, provider) = imap_seeded();
        block_on(run_sync(store.clone(), provider.clone(), None, account.clone()));
        let header = raw_message("7", "INBOX", "<h@x>", None, &[], 7_000, false);
        let mut full = raw_message("7", "INBOX", "<h@x>", None, &[], 7_000, true);
        full.id = MessageId::from("imap:j:some-other-uuid");
        provider.bodies.lock().unwrap().insert("INBOX\u{1f}1\u{1f}7".into(), full);
        *provider.changes.lock().unwrap() = Some(RemoteChanges { upserts: vec![header.clone()], message_ids: vec![ProviderId("INBOX\u{1f}1\u{1f}7".into())], checkpoint: Some("cursor-2".into()), ..Default::default() });
        let outcome = block_on(run_sync(store.clone(), provider.clone(), None, account.clone()));
        assert_eq!(outcome.state, Some(SyncState::UpToDate));
        assert_eq!(provider.fetched.lock().unwrap().as_slice(), ["INBOX\u{1f}1\u{1f}7"]);
        let rows = store.messages_by_rfc_id(&account.id, "<h@x>").unwrap();
        assert_eq!(rows.len(), 1, "the body download updated the header row instead of adding one");
        assert_eq!(rows[0].id, header.id);
        assert_eq!(rows[0].body.text.as_deref(), Some("body 7"));
        assert!(!rows[0].conversation_id.0.is_empty());
        assert!(backlog(&store, &account.id).is_empty());
    }

    /// The whole read path against a real IMAP server through the store: two syncs on a
    /// store that is closed and reopened in between, like an app restart. Run with the same
    /// `OVERLOOK_IMAP_TEST_*` variables as the provider's ignored test.
    #[test]
    #[ignore]
    fn imap_account_syncs_through_the_store_across_a_restart() {
        let var = |n: &str| std::env::var(n).ok().filter(|v| !v.trim().is_empty());
        let (Some(host), Some(user), Some(password)) = (var("OVERLOOK_IMAP_TEST_HOST"), var("OVERLOOK_IMAP_TEST_USER"), var("OVERLOOK_IMAP_TEST_PASSWORD")) else {
            panic!("set OVERLOOK_IMAP_TEST_HOST, OVERLOOK_IMAP_TEST_USER and OVERLOOK_IMAP_TEST_PASSWORD");
        };
        let security = var("OVERLOOK_IMAP_TEST_SECURITY").and_then(|s| Security::from_key(&s)).unwrap_or(Security::Tls);
        let port = var("OVERLOOK_IMAP_TEST_PORT").and_then(|p| p.parse().ok()).unwrap_or(ServerProtocol::Imap.default_port(security));
        let servers = MailServers { incoming: ServerEndpoint::new(host, port, security, user.clone()), outgoing: ServerEndpoint::new("", 0, security, user.clone()), pop3: None, imap: None };
        let account = Account { id: AccountId(format!("imap:{user}")), provider: ProviderKind::Imap, display_name: user.clone(), email: user, created_at: 0 };
        let dir = std::env::temp_dir().join(format!("overlook-imap-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("overlook.sqlite");
        let provider = Arc::new(overlook_provider::imap::ImapProvider::new(servers.clone(), password.clone()).with_inbox_limit(20));

        let store = Arc::new(Store::open(&path).unwrap());
        store.upsert_account(&account).unwrap();
        store.set_servers(&account.id, &servers).unwrap();
        let outcome = block_on(run_sync(store.clone(), provider.clone(), None, account.clone()));
        assert_eq!(outcome.state, Some(SyncState::UpToDate), "{outcome:?}");
        let labels = store.labels(&account.id).unwrap();
        assert!(labels.iter().any(|l| l.kind == LabelKind::Inbox));
        let messages = store.messages_for_account(&account.id).unwrap();
        eprintln!("first sync: {} labels, {} messages, {} folder rows", labels.len(), messages.len(), store.imap_folders(&account.id).unwrap().len());
        assert!(messages.iter().all(|m| !m.conversation_id.0.is_empty() && m.provider_id.is_some()));
        assert!(backlog(&store, &account.id).is_empty(), "every body was downloaded");
        let bodies = messages.iter().filter(|m| m.body.is_cached()).count();
        assert_eq!(bodies, messages.len(), "all bodies cached");
        let threads: std::collections::BTreeSet<_> = messages.iter().map(|m| m.conversation_id.clone()).collect();
        eprintln!("{} conversations; replies joined their parents: {}", threads.len(), messages.iter().filter(|m| m.in_reply_to.is_some()).all(|m| messages.iter().any(|p| p.rfc_message_id == m.in_reply_to && p.conversation_id == m.conversation_id) || !messages.iter().any(|p| p.rfc_message_id == m.in_reply_to)));
        if let Some((m, a)) = messages.iter().find_map(|m| m.attachments.first().map(|a| (m, a))) {
            let bytes = pollster_block(provider.fetch_attachment(&account, m, a)).unwrap();
            eprintln!("attachment {} fetched: {} bytes", a.filename, bytes.len());
            assert!(!bytes.is_empty());
        }
        drop(store);

        // "Restart": reopen the database; the checkpoint drives an incremental cycle.
        let store = Arc::new(Store::open(&path).unwrap());
        let before = store.message_count(&account.id).unwrap();
        let outcome = block_on(run_sync(store.clone(), provider.clone(), None, account.clone()));
        assert_eq!(outcome.state, Some(SyncState::UpToDate), "{outcome:?}");
        assert_eq!(store.message_count(&account.id).unwrap(), before, "a quiet cycle changes nothing");
        assert!(!store.imap_folders(&account.id).unwrap().is_empty(), "folder rows are mirrored from the checkpoint");
        eprintln!("second sync after reopen: {} downloaded, state {:?}", outcome.downloaded, outcome.state);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn deletions_by_provider_id_and_folder_rows_are_recorded() {
        let (store, account, provider) = imap_seeded();
        block_on(run_sync(store.clone(), provider.clone(), None, account.clone()));
        assert!(store.imap_folders(&account.id).unwrap().is_empty(), "the scripted checkpoint is not an IMAP checkpoint");
        let z = by_rfc(&store, "<z@x>");
        let mut cp = overlook_provider::imap::Checkpoint { version: overlook_provider::imap::Checkpoint::VERSION, ..Default::default() };
        cp.folders.insert("INBOX".into(), overlook_provider::imap::FolderCursor { kind: "inbox".into(), uidvalidity: 1, highest_uid: 3, known: "1:3".into(), delimiter: Some("/".into()), ..Default::default() });
        *provider.changes.lock().unwrap() = Some(RemoteChanges {
            deleted_provider_ids: vec![ProviderId("INBOX\u{1f}1\u{1f}3".into()), ProviderId("INBOX\u{1f}1\u{1f}99".into())],
            checkpoint: Some(cp.to_json()),
            ..Default::default()
        });
        let outcome = block_on(run_sync(store.clone(), provider.clone(), None, account.clone()));
        assert_eq!(outcome.state, Some(SyncState::UpToDate));
        assert!(store.message(&z.id).unwrap().is_none(), "resolved through the provider id");
        assert_eq!(store.message_count(&account.id).unwrap(), 2, "an unknown provider id is ignored");
        let rows = store.imap_folders(&account.id).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!((rows[0].path.as_str(), rows[0].uidvalidity, rows[0].highest_uid), ("INBOX", Some(1), 3));
    }

    #[test]
    fn applied_changes_rewrite_provider_ids() {
        let (store, account, provider) = imap_seeded();
        block_on(run_sync(store.clone(), provider.clone(), None, account.clone()));
        let m = by_rfc(&store, "<z@x>");
        store.apply_change(&account.id, overlook_store::LocalChange::Archive { message_ids: vec![m.id.clone()] }).unwrap();
        *provider.applied.lock().unwrap() = AppliedChanges { provider_ids: vec![(m.id.clone(), ProviderId("Archive\u{1f}1\u{1f}3".into()), LabelId::from("imap:j:Archive"))], labels: Vec::new() };
        let outcome = block_on(run_sync(store.clone(), provider.clone(), None, account.clone()));
        assert_eq!(outcome.applied, 1);
        let moved = store.message(&m.id).unwrap().unwrap();
        assert_eq!(moved.provider_id.as_ref().unwrap().as_str(), "Archive\u{1f}1\u{1f}3");
        assert_eq!(moved.label_ids.iter().map(|l| l.as_str()).collect::<BTreeSet<_>>(), ["imap:j:Archive"].into_iter().collect());
        assert_eq!(store.pending_count(&account.id).unwrap(), 0);
    }

    // ------------------------------------------ the real IMAP provider, fake servers

    use overlook_provider::imap::fake::{serve, FakeFolder, FakeMessage, FakeServer, FakeState};
    use overlook_provider::imap::{is_local_placeholder, ImapProvider, MessageRef};
    use overlook_provider::smtp::fake::{serve_many, Behaviour, FakeSmtp};

    struct ImapEnv {
        dir: std::path::PathBuf,
        path: std::path::PathBuf,
        imap: FakeServer,
        smtp: FakeSmtp,
        servers: MailServers,
        account: Account,
    }

    impl Drop for ImapEnv {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn raw(from: &str, subject: &str, rfc: &str, date: &str) -> Vec<u8> {
        format!("From: {from}\r\nTo: jason@example.com\r\nSubject: {subject}\r\nMessage-ID: {rfc}\r\nDate: {date}\r\n\r\nBody of {subject}.\r\n").into_bytes()
    }

    fn imap_env(name: &str) -> ImapEnv {
        let mut state = FakeState::default();
        let mut inbox = FakeFolder::new("INBOX", &["\\HasNoChildren"], 100);
        inbox.add(FakeMessage::new(&[], "10-Oct-2026 09:00:00 +0000", raw("ada@example.com", "First", "<first@example.com>", "Sat, 10 Oct 2026 09:00:00 +0000")));
        inbox.add(FakeMessage::new(&["\\Seen"], "11-Oct-2026 09:00:00 +0000", raw("bob@example.com", "Second", "<second@example.com>", "Sun, 11 Oct 2026 09:00:00 +0000")));
        inbox.add(FakeMessage::new(&[], "12-Oct-2026 09:00:00 +0000", raw("cat@example.com", "Third", "<third@example.com>", "Mon, 12 Oct 2026 09:00:00 +0000")));
        state.folders = vec![inbox, FakeFolder::new("Sent", &["\\Sent"], 200), FakeFolder::new("Trash", &["\\Trash"], 300)];
        state.subscribed = ["INBOX", "Sent", "Trash"].into_iter().map(str::to_owned).collect();
        let imap = serve(state);
        let smtp = serve_many(Behaviour::Normal);
        let servers = MailServers {
            incoming: ServerEndpoint::new("127.0.0.1", imap.addr.port(), Security::None, "jason@example.com"),
            outgoing: ServerEndpoint::new("127.0.0.1", smtp.addr.port(), Security::None, "jason@example.com"),
            pop3: None,
            imap: None,
        };
        let account = Account { id: AccountId::from("imap:jason@example.com"), provider: ProviderKind::Imap, display_name: "Jason".into(), email: "jason@example.com".into(), created_at: 0 };
        let dir = std::env::temp_dir().join(format!("overlook-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("overlook.sqlite");
        ImapEnv { dir, path, imap, smtp, servers, account }
    }

    fn imap_provider(env: &ImapEnv) -> Arc<ImapProvider> {
        Arc::new(ImapProvider::new(env.servers.clone(), "hunter2").with_timeout(Duration::from_secs(5)).with_inbox_limit(20))
    }

    /// A provider whose server cannot be reached: the account is offline.
    fn offline_provider(env: &ImapEnv) -> Arc<ImapProvider> {
        let dead = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
        let mut servers = env.servers.clone();
        servers.incoming = ServerEndpoint::new("127.0.0.1", dead.port(), Security::None, "jason@example.com");
        Arc::new(ImapProvider::new(servers, "hunter2").with_timeout(Duration::from_secs(2)))
    }

    fn open_store(env: &ImapEnv) -> Arc<Store> {
        let store = Arc::new(Store::open(&env.path).unwrap());
        store.upsert_account(&env.account).unwrap();
        store
    }

    fn imap_message(store: &Store, env: &ImapEnv, subject: &str) -> Message {
        store.messages_for_account(&env.account.id).unwrap().into_iter().find(|m| m.subject == subject).unwrap_or_else(|| panic!("no message {subject}"))
    }

    fn label_of(m: &Message) -> String {
        m.label_ids.iter().map(|l| l.as_str().rsplit(':').next().unwrap().to_owned()).collect::<Vec<_>>().join(",")
    }

    fn log_index(env: &ImapEnv, needle: &str) -> usize {
        let s = env.imap.lock();
        s.log.iter().position(|l| l.contains(needle)).unwrap_or_else(|| panic!("no {needle:?} in {:?}", s.log))
    }

    #[test]
    fn imap_offline_queue_survives_a_restart_and_replays_in_order() {
        let env = imap_env("replay");
        let live = imap_provider(&env);
        let store = open_store(&env);
        let outcome = block_on(run_sync(store.clone(), live.clone(), None, env.account.clone()));
        assert_eq!(outcome.state, Some(SyncState::UpToDate), "{outcome:?}");
        assert!(backlog(&store, &env.account.id).is_empty());
        assert_eq!(store.message_count(&env.account.id).unwrap(), 3);

        // Offline: star and archive one message, trash another, write a reply. Everything
        // stays queued.
        let first = imap_message(&store, &env, "First");
        let second = imap_message(&store, &env, "Second");
        store.apply_change(&env.account.id, overlook_store::LocalChange::SetStarred { message_ids: vec![first.id.clone()], starred: true }).unwrap();
        store.apply_change(&env.account.id, overlook_store::LocalChange::Archive { message_ids: vec![first.id.clone()] }).unwrap();
        store.apply_change(&env.account.id, overlook_store::LocalChange::Trash { message_ids: vec![second.id.clone()] }).unwrap();
        let mut draft = Draft::new(env.account.id.clone(), DraftKind::Reply, now());
        draft.to = vec![Address::named("Ada", "ada@example.com")];
        draft.subject = "Re: First".into();
        draft.body_text = "Replying while offline.".into();
        draft.in_reply_to = Some(first.id.clone());
        draft.conversation_id = Some(first.conversation_id.clone());
        store.save_draft(&draft).unwrap();
        store.queue_send(&draft).unwrap();
        let outcome = block_on(run_sync(store.clone(), offline_provider(&env), None, env.account.clone()));
        assert_eq!(outcome.state, Some(SyncState::Offline));
        assert_eq!(store.pending_count(&env.account.id).unwrap(), 4);
        assert!(env.imap.lock().commands("UID STORE") == 0 && env.smtp.seen.lock().unwrap().is_empty(), "nothing reached the servers");
        drop(store);

        // Restart while offline: the queue is read back from disk, then the server is back.
        let store = open_store(&env);
        assert_eq!(store.pending_count(&env.account.id).unwrap(), 4);
        let outcome = block_on(run_sync(store.clone(), live.clone(), None, env.account.clone()));
        assert_eq!(outcome.state, Some(SyncState::UpToDate), "{outcome:?}");
        assert_eq!((outcome.applied, outcome.sent.clone(), outcome.failed.clone()), (4, vec!["Re: First".to_owned()], vec![]));
        assert_eq!(store.pending_count(&env.account.id).unwrap(), 0);
        let (star, archive, trash, append) = (log_index(&env, "UID STORE 1 +FLAGS.SILENT (\\Flagged)"), log_index(&env, "UID MOVE 1 \"Archive\""), log_index(&env, "UID MOVE 2 \"Trash\""), log_index(&env, "APPEND \"Sent\""));
        assert!(star < archive && archive < trash && trash < append, "replayed in the order they were made");
        assert_eq!(env.smtp.seen.lock().unwrap().len(), 1);
        {
            let s = env.imap.lock();
            assert!(s.has_folder("Archive"), "created on demand");
            assert!(s.folder("Archive").messages[&1].has_flag("\\Flagged"));
            assert_eq!(s.folder("INBOX").messages.keys().copied().collect::<Vec<_>>(), vec![3]);
            assert_eq!(s.folder("Trash").messages.len(), 1);
            assert_eq!(s.folder("Sent").messages.len(), 1);
        }
        let first = store.message(&first.id).unwrap().unwrap();
        let archive_validity = env.imap.lock().folder("Archive").uidvalidity;
        assert_eq!(first.provider_id, Some(MessageRef::provider_id("Archive", archive_validity, 1)), "COPYUID updated the identity at once");
        assert_eq!(label_of(&first), "Archive");
        assert!(store.labels(&env.account.id).unwrap().iter().any(|l| l.kind == LabelKind::Archive), "the created folder is a label straight away");
        let second = store.message(&second.id).unwrap().unwrap();
        assert_eq!((second.provider_id.clone(), label_of(&second)), (Some(MessageRef::provider_id("Trash", 300, 1)), "Trash".to_owned()));
        let sent = imap_message(&store, &env, "Re: First");
        assert!(sent.is_outgoing && sent.provider_id == Some(MessageRef::provider_id("Sent", 200, 1)));
        assert_eq!(label_of(&sent), "Sent");
        assert_eq!(sent.conversation_id, first.conversation_id, "the reply threads with its parent");
        assert!(store.draft(&draft.id).unwrap().is_none());

        // The next cycle sees the server's view of the same moves and changes nothing.
        let before = store.message_count(&env.account.id).unwrap();
        let outcome = block_on(run_sync(store.clone(), live.clone(), None, env.account.clone()));
        assert_eq!(outcome.state, Some(SyncState::UpToDate), "{outcome:?}");
        assert_eq!(store.message_count(&env.account.id).unwrap(), before, "no duplicates after the moves and the Sent copy");
        let sent = imap_message(&store, &env, "Re: First");
        assert!(sent.body.is_cached(), "the Sent copy kept its body");
        assert_eq!(label_of(&imap_message(&store, &env, "First")), "Archive");
        let rows = store.imap_folders(&env.account.id).unwrap();
        assert!(rows.iter().any(|r| r.path == "Archive"), "the checkpoint tracks the new folder: {rows:?}");
    }

    #[test]
    fn imap_replay_after_a_crash_between_server_success_and_local_commit() {
        let env = imap_env("crash");
        let live = imap_provider(&env);
        let store = open_store(&env);
        block_on(run_sync(store.clone(), live.clone(), None, env.account.clone()));
        let third = imap_message(&store, &env, "Third");
        store.apply_change(&env.account.id, overlook_store::LocalChange::Archive { message_ids: vec![third.id.clone()] }).unwrap();
        let labels = store.labels(&env.account.id).unwrap();
        let op = store.pending_operations(&env.account.id).unwrap().remove(0);
        // The server applied the move, then the process died before the store heard.
        let applied = pollster_block(live.apply(&env.account, &op, &[third.clone()], &labels)).unwrap();
        assert_eq!(applied.provider_ids.len(), 1);
        assert_eq!(store.message(&third.id).unwrap().unwrap().provider_id, third.provider_id, "nothing committed locally");
        drop(store);

        let store = open_store(&env);
        let fresh = imap_provider(&env);
        let outcome = block_on(run_sync(store.clone(), fresh.clone(), None, env.account.clone()));
        assert_eq!(outcome.state, Some(SyncState::UpToDate), "{outcome:?}");
        assert_eq!(outcome.applied, 1);
        let s = env.imap.lock();
        assert_eq!(s.folder("Archive").messages.len(), 1, "one copy, found again by Message-ID");
        assert_eq!(s.commands("UID MOVE"), 3, "the first attempt's TRYCREATE retry, then the replay: {:?}", s.log);
        let validity = s.folder("Archive").uidvalidity;
        drop(s);
        let third = store.message(&third.id).unwrap().unwrap();
        assert_eq!((third.provider_id.clone(), label_of(&third)), (Some(MessageRef::provider_id("Archive", validity, 1)), "Archive".to_owned()));
        assert_eq!(store.pending_count(&env.account.id).unwrap(), 0);
        assert_eq!(store.message_count(&env.account.id).unwrap(), 3);
    }

    #[test]
    fn imap_uncertain_send_waits_for_the_user_without_blocking_other_changes() {
        let env = imap_env("uncertain");
        let live = imap_provider(&env);
        let store = open_store(&env);
        block_on(run_sync(store.clone(), live.clone(), None, env.account.clone()));
        let draft = queued_draft(&store, &env.account);
        env.smtp.set(Behaviour::DropAfterData);
        let outcome = block_on(run_sync(store.clone(), live.clone(), None, env.account.clone()));
        assert_eq!(outcome.uncertain, vec!["Agenda".to_owned()]);
        assert_eq!(store.draft(&draft.id).unwrap().unwrap().state, DraftState::Uncertain);

        // The next cycle cannot find a copy in Sent: the send stays uncertain, is not
        // repeated, and a change queued behind it still goes through.
        env.smtp.set(Behaviour::Normal);
        let first = imap_message(&store, &env, "First");
        store.apply_change(&env.account.id, overlook_store::LocalChange::SetRead { message_ids: vec![first.id.clone()], read: true }).unwrap();
        let outcome = block_on(run_sync(store.clone(), live.clone(), None, env.account.clone()));
        assert_eq!(outcome.state, Some(SyncState::UpToDate), "{outcome:?}");
        assert_eq!((outcome.applied, outcome.sent.len(), outcome.uncertain.len()), (1, 0, 0));
        assert_eq!(env.smtp.seen.lock().unwrap().len(), 1, "not sent again on its own");
        assert!(env.imap.lock().folder("INBOX").messages[&1].has_flag("\\Seen"));
        let ops = store.pending_operations(&env.account.id).unwrap();
        assert_eq!(ops.len(), 1);
        assert_eq!((ops[0].state, ops[0].last_error.as_deref()), (OperationState::Uncertain, Some(UNCERTAIN_SEND_WORDING)));
        assert_eq!(store.draft(&draft.id).unwrap().unwrap().state, DraftState::Uncertain);

        // "Send again": a fresh attempt that succeeds and lands in Sent.
        store.resend(&draft.account_id, &draft.id).unwrap();
        assert_eq!(store.draft(&draft.id).unwrap().unwrap().state, DraftState::Queued);
        let outcome = block_on(run_sync(store.clone(), live.clone(), None, env.account.clone()));
        assert_eq!(outcome.sent, vec!["Agenda".to_owned()]);
        assert_eq!(env.smtp.seen.lock().unwrap().len(), 2);
        assert_eq!(env.imap.lock().folder("Sent").messages.len(), 1);
        assert!(store.draft(&draft.id).unwrap().is_none());
        assert_eq!(store.messages_for_account(&env.account.id).unwrap().iter().filter(|m| m.is_outgoing).count(), 1);

        // "Discard": the draft and its send are gone.
        let draft = queued_draft(&store, &env.account);
        env.smtp.set(Behaviour::DropAfterData);
        block_on(run_sync(store.clone(), live.clone(), None, env.account.clone()));
        assert_eq!(store.draft(&draft.id).unwrap().unwrap().state, DraftState::Uncertain);
        store.discard_send(&draft.account_id, &draft.id).unwrap();
        assert!(store.draft(&draft.id).unwrap().is_none());
        assert_eq!(store.pending_count(&env.account.id).unwrap(), 0);
        env.smtp.set(Behaviour::Normal);
        let outcome = block_on(run_sync(store.clone(), live.clone(), None, env.account.clone()));
        assert_eq!((outcome.state, outcome.sent.len()), (Some(SyncState::UpToDate), 0));
        assert_eq!(env.smtp.seen.lock().unwrap().len(), 3, "the discarded message was never sent again");
    }

    #[test]
    fn imap_local_sent_copy_is_replaced_by_the_server_copy() {
        let env = imap_env("sentcopy");
        let live = imap_provider(&env);
        let store = open_store(&env);
        block_on(run_sync(store.clone(), live.clone(), None, env.account.clone()));
        let draft = queued_draft(&store, &env.account);
        env.imap.lock().fail_next = Some(("APPEND".into(), "NO [OVERQUOTA] Quota exceeded".into()));
        let outcome = block_on(run_sync(store.clone(), live.clone(), None, env.account.clone()));
        assert_eq!(outcome.sent, vec!["Agenda".to_owned()]);
        let local = imap_message(&store, &env, "Agenda");
        assert!(local.provider_id.as_ref().is_some_and(is_local_placeholder));
        assert_eq!(label_of(&local), "Sent");

        // The server copy shows up later (a retried APPEND elsewhere, or another client):
        // it is the same message, not a second one.
        let rfc = local.rfc_message_id.clone().unwrap();
        env.smtp.wait_for_connections(1);
        let data = env.smtp.messages().remove(0);
        env.imap.lock().folder_mut("Sent").add(FakeMessage::new(&["\\Seen"], "14-Oct-2026 12:00:00 +0000", data));
        let outcome = block_on(run_sync(store.clone(), live.clone(), None, env.account.clone()));
        assert_eq!(outcome.state, Some(SyncState::UpToDate), "{outcome:?}");
        let sent: Vec<Message> = store.messages_for_account(&env.account.id).unwrap().into_iter().filter(|m| m.subject == "Agenda").collect();
        assert_eq!(sent.len(), 1, "de-duplicated by Message-ID");
        assert_eq!((sent[0].id.clone(), sent[0].provider_id.clone(), sent[0].rfc_message_id.clone()), (local.id, Some(MessageRef::provider_id("Sent", 200, 1)), Some(rfc)));
        assert!(sent[0].body.is_cached() && sent[0].is_outgoing);
        let _ = draft;
    }

    // ------------------------------------------- the spike servers (manual, ignored)

    /// A raw IMAP or SMTP client for the spike script: another mail client, or its fault
    /// injection (`XDROPAFTER`, `XFAILNEXT`, `XDROPAFTERDATA`).
    struct Raw {
        reader: std::io::BufReader<std::net::TcpStream>,
        writer: std::net::TcpStream,
        tag: u32,
    }

    impl Raw {
        fn connect(port: u16) -> Self {
            let stream = std::net::TcpStream::connect(("127.0.0.1", port)).expect("spike server running");
            stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut raw = Self { reader: std::io::BufReader::new(stream.try_clone().unwrap()), writer: stream, tag: 0 };
            raw.line();
            raw
        }

        fn line(&mut self) -> String {
            use std::io::BufRead;
            let mut line = String::new();
            self.reader.read_line(&mut line).unwrap();
            line.trim_end().to_owned()
        }

        /// Sends an IMAP command and returns the untagged lines before its OK.
        fn imap(&mut self, command: &str) -> Vec<String> {
            use std::io::Write;
            self.tag += 1;
            let tag = format!("x{}", self.tag);
            self.writer.write_all(format!("{tag} {command}\r\n").as_bytes()).unwrap();
            let mut lines = Vec::new();
            loop {
                let l = self.line();
                if let Some(rest) = l.strip_prefix(&format!("{tag} ")) {
                    assert!(rest.starts_with("OK"), "{command}: {l}");
                    return lines;
                }
                lines.push(l);
            }
        }

        fn imap_login(port: u16) -> Self {
            let mut raw = Self::connect(port);
            raw.imap("LOGIN spike secret");
            raw
        }

        /// UIDs in `folder` carrying this Message-ID.
        fn search(&mut self, folder: &str, rfc: &str) -> Vec<u32> {
            self.imap(&format!("SELECT \"{folder}\""));
            let lines = self.imap(&format!("UID SEARCH HEADER Message-ID \"{rfc}\""));
            lines.iter().find_map(|l| l.strip_prefix("* SEARCH")).map(|rest| rest.split_whitespace().filter_map(|u| u.parse().ok()).collect()).unwrap_or_default()
        }

        fn smtp(&mut self, command: &str) -> String {
            use std::io::Write;
            self.writer.write_all(format!("{command}\r\n").as_bytes()).unwrap();
            self.line()
        }
    }

    /// IDLE against `scripts/spike-mail-servers.py` (start it first): a watcher on its own
    /// connection reports new mail within seconds of `XDELIVER`, and the sync that follows
    /// stores the message with its body. Run with
    /// `cargo test -p overlook imap_idle -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn imap_idle_delivers_new_mail_against_the_spike_servers() {
        use overlook_provider::imap::idle::IdleEvent;
        let imap_port: u16 = std::env::var("OVERLOOK_SPIKE_IMAP_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(1143);
        let servers = MailServers { incoming: ServerEndpoint::new("127.0.0.1", imap_port, Security::None, "spike"), outgoing: ServerEndpoint::new("127.0.0.1", 2525, Security::None, "spike"), pop3: None, imap: None };
        let account = Account { id: AccountId::from("imap:spike@example.com"), provider: ProviderKind::Imap, display_name: "Spike".into(), email: "spike@example.com".into(), created_at: 0 };
        let dir = std::env::temp_dir().join(format!("overlook-spike-idle-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = Arc::new(Store::open(&dir.join("overlook.sqlite")).unwrap());
        store.upsert_account(&account).unwrap();
        store.set_servers(&account.id, &servers).unwrap();
        let live = Arc::new(ImapProvider::new(servers, "secret").with_timeout(Duration::from_secs(5)).with_inbox_limit(20));
        let outcome = block_on(run_sync(store.clone(), live.clone(), None, account.clone()));
        assert_eq!(outcome.state, Some(SyncState::UpToDate), "{outcome:?}");
        eprintln!("capabilities: {:?}", live.capabilities());

        let (tx, rx) = std::sync::mpsc::channel();
        let mut watcher = live.idle_watcher(move |event| {
            let _ = tx.send(event);
        });
        std::thread::sleep(Duration::from_secs(1));
        assert!(rx.try_recv().is_err(), "nothing to report yet");
        let subject = format!("Idle delivery {}", std::process::id());
        let started = std::time::Instant::now();
        Raw::connect(imap_port).imap(&format!("XDELIVER {subject}"));
        assert_eq!(rx.recv_timeout(Duration::from_secs(10)), Ok(IdleEvent::Changed));
        let latency = started.elapsed();
        assert!(latency < Duration::from_secs(5), "reported after {latency:?}");
        let outcome = block_on(run_sync(store.clone(), live.clone(), None, account.clone()));
        assert_eq!(outcome.state, Some(SyncState::UpToDate), "{outcome:?}");
        let delivered = store.messages_for_account(&account.id).unwrap().into_iter().find(|m| m.subject == subject).expect("the delivered message is in the store");
        assert!(delivered.body.is_cached() && delivered.label_ids.iter().any(|l| l.as_str().ends_with(":INBOX")));
        eprintln!("IDLE reported the delivery after {latency:?}; the following sync stored {:?} (sync took {:?})", delivered.subject, started.elapsed() - latency);
        watcher.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The plan's M3 exit criterion against `scripts/spike-mail-servers.py` (start it first;
    /// it keeps state between runs, so use a fresh process per run). Run with
    /// `cargo test -p overlook imap_exit_criteria -- --ignored --nocapture`; optional
    /// `OVERLOOK_SPIKE_IMAP_PORT` / `OVERLOOK_SPIKE_SMTP_PORT` (1143 / 2525).
    #[test]
    #[ignore]
    fn imap_exit_criteria_against_the_spike_servers() {
        let port = |name: &str, default: u16| std::env::var(name).ok().and_then(|p| p.parse().ok()).unwrap_or(default);
        let (imap_port, smtp_port) = (port("OVERLOOK_SPIKE_IMAP_PORT", 1143), port("OVERLOOK_SPIKE_SMTP_PORT", 2525));
        let servers = MailServers {
            incoming: ServerEndpoint::new("127.0.0.1", imap_port, Security::None, "spike"),
            outgoing: ServerEndpoint::new("127.0.0.1", smtp_port, Security::None, "spike"),
            pop3: None,
            imap: None,
        };
        let account = Account { id: AccountId::from("imap:spike@example.com"), provider: ProviderKind::Imap, display_name: "Spike".into(), email: "spike@example.com".into(), created_at: 0 };
        let dir = std::env::temp_dir().join(format!("overlook-spike-m3-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("overlook.sqlite");
        let live = Arc::new(ImapProvider::new(servers.clone(), "secret").with_timeout(Duration::from_secs(5)).with_inbox_limit(20));
        let open = || {
            let store = Arc::new(Store::open(&path).unwrap());
            store.upsert_account(&account).unwrap();
            store.set_servers(&account.id, &servers).unwrap();
            store
        };
        let find = |store: &Store, rfc: &str| store.message_by_rfc_id(&account.id, rfc).unwrap().unwrap_or_else(|| panic!("no message {rfc}"));
        let folder_of = |m: &Message| MessageRef::parse(m.provider_id.as_ref().unwrap()).unwrap().folder;
        let pid = std::process::id();

        let store = open();
        let outcome = block_on(run_sync(store.clone(), live.clone(), None, account.clone()));
        assert_eq!(outcome.state, Some(SyncState::UpToDate), "{outcome:?}");
        let baseline = store.message_count(&account.id).unwrap();
        eprintln!("initial sync: {baseline} messages");

        // 1. Disconnect during a download: new mail arrives, both attempts to fetch it die.
        let new_rfc = format!("<new-{pid}@example.com>");
        let new_raw = format!("From: late@example.com\r\nTo: spike@example.com\r\nSubject: Arrived late {pid}\r\nMessage-ID: {new_rfc}\r\nDate: Wed, 14 Oct 2026 12:00:00 +0000\r\n\r\nLate body.\r\n");
        {
            use std::io::Write;
            let mut other = Raw::imap_login(imap_port);
            other.tag += 1;
            other.writer.write_all(format!("x{} APPEND \"INBOX\" () {{{}}}\r\n", other.tag, new_raw.len()).as_bytes()).unwrap();
            assert!(other.line().starts_with('+'));
            other.writer.write_all(new_raw.as_bytes()).unwrap();
            other.writer.write_all(b"\r\n").unwrap();
            assert!(other.line().contains("APPENDUID"));
        }
        Raw::connect(imap_port).imap("XDROPAFTER UID FETCH 2");
        let outcome = block_on(run_sync(store.clone(), live.clone(), None, account.clone()));
        assert_eq!(outcome.state, Some(SyncState::Offline), "both fetch attempts were cut: {outcome:?}");
        let outcome = block_on(run_sync(store.clone(), live.clone(), None, account.clone()));
        assert_eq!(outcome.state, Some(SyncState::UpToDate), "{outcome:?}");
        let late = find(&store, &new_rfc);
        assert!(late.body.is_cached(), "resumed where it left off");
        assert_eq!(store.message_count(&account.id).unwrap(), baseline + 1);
        eprintln!("1. disconnect during download: recovered on the next sync");

        // 2. Disconnect during a move: the server moves, the reply is lost, the retry finds
        // the message in the (newly created) Archive folder.
        let plain = find(&store, "<plain-1@example.com>");
        store.apply_change(&account.id, overlook_store::LocalChange::SetStarred { message_ids: vec![plain.id.clone()], starred: true }).unwrap();
        store.apply_change(&account.id, overlook_store::LocalChange::Archive { message_ids: vec![plain.id.clone()] }).unwrap();
        Raw::connect(imap_port).imap("XDROPAFTER UID MOVE");
        let outcome = block_on(run_sync(store.clone(), live.clone(), None, account.clone()));
        assert_eq!((outcome.state.clone(), outcome.applied), (Some(SyncState::UpToDate), 2), "{outcome:?}");
        let plain = store.message(&plain.id).unwrap().unwrap();
        assert_eq!(folder_of(&plain), "Archive");
        assert!(plain.label_ids.iter().any(|l| l.as_str().ends_with(":Archive")));
        assert_eq!(Raw::imap_login(imap_port).search("Archive", "<plain-1@example.com>").len(), 1, "one copy on the server");
        assert!(Raw::imap_login(imap_port).search("INBOX", "<plain-1@example.com>").is_empty());
        let outcome = block_on(run_sync(store.clone(), live.clone(), None, account.clone()));
        assert_eq!(outcome.state, Some(SyncState::UpToDate), "{outcome:?}");
        assert_eq!(store.message_count(&account.id).unwrap(), baseline + 1, "the move did not duplicate or lose anything");
        assert!(store.labels(&account.id).unwrap().iter().any(|l| l.kind == LabelKind::Archive));
        eprintln!("2. disconnect during a move: converged, Archive created on demand");

        // 3. Disconnect during a send: uncertain until the user sends again.
        let mut draft = Draft::new(account.id.clone(), DraftKind::New, now());
        draft.to = vec![Address::named("Maya", "maya@northstar.example")];
        draft.subject = format!("Spike send {pid}");
        draft.body_text = "Sent through the spike servers.".into();
        store.save_draft(&draft).unwrap();
        store.queue_send(&draft).unwrap();
        let rfc = overlook_provider::draft_message_id(&draft);
        assert!(Raw::connect(smtp_port).smtp("XDROPAFTERDATA").starts_with("250"));
        let outcome = block_on(run_sync(store.clone(), live.clone(), None, account.clone()));
        assert_eq!(outcome.uncertain, vec![draft.subject.clone()], "{outcome:?}");
        let outcome = block_on(run_sync(store.clone(), live.clone(), None, account.clone()));
        assert_eq!((outcome.state.clone(), outcome.sent.len()), (Some(SyncState::UpToDate), 0), "{outcome:?}");
        assert_eq!(store.draft(&draft.id).unwrap().unwrap().state, DraftState::Uncertain);
        assert!(Raw::imap_login(imap_port).search("Sent", &rfc).is_empty(), "nothing in Sent yet");
        store.resend(&draft.account_id, &draft.id).unwrap();
        let outcome = block_on(run_sync(store.clone(), live.clone(), None, account.clone()));
        assert_eq!(outcome.sent, vec![draft.subject.clone()], "{outcome:?}");
        let on_server = Raw::imap_login(imap_port).search("Sent", &rfc);
        assert_eq!(on_server.len(), 1);
        let sent = find(&store, &rfc);
        assert_eq!(folder_of(&sent), "Sent");
        assert!(sent.label_ids.iter().any(|l| l.as_str().ends_with(":Sent")) && sent.is_outgoing);
        let outcome = block_on(run_sync(store.clone(), live.clone(), None, account.clone()));
        assert_eq!(outcome.state, Some(SyncState::UpToDate), "{outcome:?}");
        let again = find(&store, &rfc);
        assert_eq!((again.id.clone(), again.body.is_cached(), again.label_ids.len()), (sent.id.clone(), true, 1), "the Sent copy is one message after the next sync");
        eprintln!("3. disconnect during a send: uncertain, then sent again and in Sent with the Sent label");

        // 4. Restart while offline: queued changes survive the restart and replay in order.
        let contract = find(&store, "<work-1@example.com>");
        let invite = find(&store, "<invite-3@example.com>");
        store.apply_change(&account.id, overlook_store::LocalChange::SetRead { message_ids: vec![invite.id.clone()], read: true }).unwrap();
        store.apply_change(&account.id, overlook_store::LocalChange::Trash { message_ids: vec![contract.id.clone()] }).unwrap();
        let dead = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
        let mut unreachable = servers.clone();
        unreachable.incoming = ServerEndpoint::new("127.0.0.1", dead.port(), Security::None, "spike");
        let offline = Arc::new(ImapProvider::new(unreachable, "secret").with_timeout(Duration::from_secs(2)));
        let outcome = block_on(run_sync(store.clone(), offline, None, account.clone()));
        assert_eq!(outcome.state, Some(SyncState::Offline));
        assert_eq!(store.pending_count(&account.id).unwrap(), 2);
        drop(store);
        let store = open();
        assert_eq!(store.pending_count(&account.id).unwrap(), 2);
        let outcome = block_on(run_sync(store.clone(), live.clone(), None, account.clone()));
        assert_eq!((outcome.state.clone(), outcome.applied), (Some(SyncState::UpToDate), 2), "{outcome:?}");
        assert_eq!(folder_of(&store.message(&contract.id).unwrap().unwrap()), "Trash");
        assert_eq!(Raw::imap_login(imap_port).search("Trash", "<work-1@example.com>").len(), 1);
        eprintln!("4. restart while offline: the queue replayed after the restart");

        // 5. The same folder changed from another client: a flag and a move made elsewhere
        // show up locally without duplicates.
        let multi = find(&store, "<multi-2@example.com>");
        let late = find(&store, &new_rfc);
        let (multi_uid, late_uid) = (MessageRef::parse(multi.provider_id.as_ref().unwrap()).unwrap().uid, MessageRef::parse(late.provider_id.as_ref().unwrap()).unwrap().uid);
        {
            let mut other = Raw::imap_login(imap_port);
            other.imap("SELECT \"INBOX\"");
            other.imap(&format!("UID STORE {late_uid} +FLAGS (\\Seen)"));
            other.imap(&format!("UID MOVE {multi_uid} \"Work/Clients\""));
        }
        let before = store.message_count(&account.id).unwrap();
        let outcome = block_on(run_sync(store.clone(), live.clone(), None, account.clone()));
        assert_eq!(outcome.state, Some(SyncState::UpToDate), "{outcome:?}");
        assert!(store.message(&late.id).unwrap().unwrap().is_read, "the flag set elsewhere arrived");
        let moved = store.message(&multi.id).unwrap().unwrap();
        assert_eq!(folder_of(&moved), "Work/Clients", "the move made elsewhere kept the local id");
        assert_eq!(moved.conversation_id, multi.conversation_id);
        assert_eq!(store.message_count(&account.id).unwrap(), before);
        eprintln!("5. folder changed from another client: flag and move reflected, no duplicates");

        live.disconnect();
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ------------------------------------------ the real POP3 provider, fake servers

    mod pop3 {
        use super::*;
        use overlook_provider::pop3::fake::{serve as serve_pop3, FakeMessage as PopMessage, FakeServer as PopServer, FakeState as PopState};
        use overlook_provider::pop3::{is_sent_copy, Pop3Provider};

        struct PopEnv {
            dir: std::path::PathBuf,
            pop3: PopServer,
            smtp: FakeSmtp,
            account: Account,
        }

        impl Drop for PopEnv {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.dir);
            }
        }

        fn pop_raw(from: &str, subject: &str, rfc: &str, date: &str) -> Vec<u8> {
            format!("From: {from}\r\nTo: jason@example.com\r\nSubject: {subject}\r\nMessage-ID: {rfc}\r\nDate: {date}\r\n\r\nBody of {subject}.\r\n").into_bytes()
        }

        fn pop_env(name: &str) -> PopEnv {
            let mut state = PopState::default();
            state.add(PopMessage::new("U1", pop_raw("ada@example.com", "First", "<first@example.com>", "Sat, 10 Oct 2026 09:00:00 +0000")));
            state.add(PopMessage::new("U2", pop_raw("bob@example.com", "Second", "<second@example.com>", "Sun, 11 Oct 2026 09:00:00 +0000")));
            let mut reply = pop_raw("ada@example.com", "Re: First", "<third@example.com>", "Mon, 12 Oct 2026 09:00:00 +0000");
            let headers_end = reply.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
            reply.splice(headers_end..headers_end, b"\r\nIn-Reply-To: <first@example.com>\r\nReferences: <first@example.com>".iter().copied());
            state.add(PopMessage::new("U3", reply));
            let pop3 = serve_pop3(state);
            let smtp = serve_many(Behaviour::Normal);
            let account = Account { id: AccountId::from("pop3:jason@example.com"), provider: ProviderKind::Pop3, display_name: "Jason".into(), email: "jason@example.com".into(), created_at: 0 };
            let dir = std::env::temp_dir().join(format!("overlook-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            PopEnv { dir, pop3, smtp, account }
        }

        fn pop_servers(env: &PopEnv, policy: Option<Pop3Policy>) -> MailServers {
            MailServers {
                incoming: ServerEndpoint::new("127.0.0.1", env.pop3.addr.port(), Security::None, "jason@example.com"),
                outgoing: ServerEndpoint::new("127.0.0.1", env.smtp.addr.port(), Security::None, "jason@example.com"),
                pop3: policy,
                imap: None,
            }
        }

        fn pop_provider(env: &PopEnv, policy: Option<Pop3Policy>) -> Arc<Pop3Provider> {
            Arc::new(Pop3Provider::new(pop_servers(env, policy), "hunter2", env.dir.join("raw")).with_timeout(Duration::from_secs(5)))
        }

        fn pop_store(env: &PopEnv) -> Arc<Store> {
            let store = Arc::new(Store::open(&env.dir.join("overlook.sqlite")).unwrap());
            store.upsert_account(&env.account).unwrap();
            store
        }

        fn imap_message_named(store: &Store, account: &AccountId, subject: &str) -> Message {
            store.messages_for_account(account).unwrap().into_iter().find(|m| m.subject == subject && !m.is_outgoing).unwrap_or_else(|| panic!("no message {subject}"))
        }

        fn subjects(store: &Store, env: &PopEnv) -> Vec<String> {
            let mut s: Vec<String> = store.messages_for_account(&env.account.id).unwrap().into_iter().map(|m| m.subject).collect();
            s.sort();
            s
        }

        #[test]
        fn pop3_account_downloads_once_and_never_duplicates_across_polls_and_restarts() {
            let env = pop_env("pop3-once");
            let provider = pop_provider(&env, None);
            let store = pop_store(&env);
            let outcome = block_on(run_sync(store.clone(), provider.clone(), None, env.account.clone()));
            assert_eq!(outcome.state, Some(SyncState::UpToDate), "{outcome:?}");
            assert_eq!(subjects(&store, &env), vec!["First", "Re: First", "Second"]);
            assert!(backlog(&store, &env.account.id).is_empty());
            let labels = store.labels(&env.account.id).unwrap();
            assert_eq!(labels.iter().map(|l| (l.kind, l.provider_id.is_none())).collect::<Vec<_>>(), vec![(LabelKind::Inbox, true), (LabelKind::Sent, true), (LabelKind::Archive, true), (LabelKind::Trash, true)]);
            let first = imap_message_named(&store, &env.account.id, "First");
            let reply = imap_message_named(&store, &env.account.id, "Re: First");
            assert_eq!(first.conversation_id, reply.conversation_id, "threaded by References");
            assert_eq!(first.label_ids.iter().map(|l| l.as_str()).collect::<Vec<_>>(), vec!["pop3:jason@example.com:inbox"]);
            assert!(!first.is_read);
            assert!(env.dir.join("raw").join(format!("{}.eml", first.id.as_str().replace([':', '/'], "_"))).exists());
            let mut seen: Vec<String> = store.pop3_seen(&env.account.id).unwrap().into_iter().map(|r| r.uidl).collect();
            seen.sort();
            assert_eq!(seen, vec!["U1", "U2", "U3"]);

            // Poll again, several times: nothing new, nothing doubled.
            for _ in 0..3 {
                let outcome = block_on(run_sync(store.clone(), provider.clone(), None, env.account.clone()));
                assert_eq!((outcome.state, outcome.downloaded), (Some(SyncState::UpToDate), 0));
            }
            assert_eq!(store.message_count(&env.account.id).unwrap(), 3);
            assert_eq!(env.pop3.lock().commands("RETR"), 3);

            // Read one, then restart with a fresh provider and a reopened store; new mail arrives.
            store.apply_change(&env.account.id, overlook_store::LocalChange::SetRead { message_ids: vec![first.id.clone()], read: true }).unwrap();
            drop(store);
            env.pop3.lock().add(PopMessage::new("U4", pop_raw("new@example.com", "Fourth", "<fourth@example.com>", "Tue, 13 Oct 2026 09:00:00 +0000")));
            let store = pop_store(&env);
            let restarted = pop_provider(&env, None);
            let outcome = block_on(run_sync(store.clone(), restarted.clone(), None, env.account.clone()));
            assert_eq!(outcome.state, Some(SyncState::UpToDate), "{outcome:?}");
            assert_eq!(store.message_count(&env.account.id).unwrap(), 4);
            assert!(store.message(&first.id).unwrap().unwrap().is_read, "local flags survive the replayed SetRead and the restart");
            assert_eq!(store.pending_count(&env.account.id).unwrap(), 0, "a local-only operation is applied at once");
            assert_eq!(env.pop3.lock().commands("RETR"), 4);
            assert!(env.pop3.lock().deleted.is_empty(), "the default policy leaves mail on the server");
        }

        #[test]
        fn pop3_retention_honours_leave_on_server_and_delete_after_days() {
            let env = pop_env("pop3-retention");
            let store = pop_store(&env);
            let delete_now = pop_provider(&env, Some(Pop3Policy { leave_on_server: false, ..Default::default() }));
            block_on(run_sync(store.clone(), delete_now.clone(), None, env.account.clone()));
            assert_eq!(store.message_count(&env.account.id).unwrap(), 3);
            assert!(env.pop3.lock().deleted.is_empty(), "deletion waits for the next cycle, after the store committed");
            block_on(run_sync(store.clone(), delete_now.clone(), None, env.account.clone()));
            {
                let s = env.pop3.lock();
                assert!(s.uidls().is_empty(), "every downloaded message was deleted from the server");
                assert_eq!(s.deleted.len(), 3);
            }
            assert_eq!(store.message_count(&env.account.id).unwrap(), 3, "and all of them stay local");
            assert!(store.pop3_seen(&env.account.id).unwrap().iter().all(|r| r.deleted_on_server));

            // delete_after_days on a fresh account: nothing goes until the entry is old enough.
            let env2 = pop_env("pop3-days");
            let store2 = pop_store(&env2);
            let after_30 = pop_provider(&env2, Some(Pop3Policy { delete_after_days: Some(30), ..Default::default() }));
            block_on(run_sync(store2.clone(), after_30.clone(), None, env2.account.clone()));
            block_on(run_sync(store2.clone(), after_30.clone(), None, env2.account.clone()));
            assert_eq!(env2.pop3.lock().uidls().len(), 3);
            // Age one entry by editing the stored checkpoint, as time would.
            let cp = store2.checkpoint(&env2.account.id).unwrap().unwrap();
            let mut parsed = overlook_provider::pop3::Checkpoint::parse(&cp).unwrap();
            parsed.entries.get_mut("U1").unwrap().seen_at -= 31 * 86_400_000;
            store2.set_checkpoint(&env2.account.id, Some(&parsed.to_json())).unwrap();
            let fresh = pop_provider(&env2, Some(Pop3Policy { delete_after_days: Some(30), ..Default::default() }));
            block_on(run_sync(store2.clone(), fresh, None, env2.account.clone()));
            assert_eq!(env2.pop3.lock().deleted, vec!["U1"]);
            assert_eq!(store2.message_count(&env2.account.id).unwrap(), 3);
        }

        #[test]
        fn pop3_trash_deletes_on_the_server_when_asked_and_sends_through_smtp() {
            let env = pop_env("pop3-trash-send");
            let store = pop_store(&env);
            let provider = pop_provider(&env, Some(Pop3Policy { delete_on_server_when_trashed: true, ..Default::default() }));
            block_on(run_sync(store.clone(), provider.clone(), None, env.account.clone()));
            let second = imap_message_named(&store, &env.account.id, "Second");
            store.apply_change(&env.account.id, overlook_store::LocalChange::Trash { message_ids: vec![second.id.clone()] }).unwrap();
            let first = imap_message_named(&store, &env.account.id, "First");
            store.apply_change(&env.account.id, overlook_store::LocalChange::Archive { message_ids: vec![first.id.clone()] }).unwrap();
            let mut draft = Draft::new(env.account.id.clone(), DraftKind::Reply, now());
            draft.to = vec![Address::named("Ada", "ada@example.com")];
            draft.subject = "Re: First".into();
            draft.body_text = "Replying.".into();
            draft.in_reply_to = Some(first.id.clone());
            draft.conversation_id = Some(first.conversation_id.clone());
            store.save_draft(&draft).unwrap();
            store.queue_send(&draft).unwrap();

            let outcome = block_on(run_sync(store.clone(), provider.clone(), None, env.account.clone()));
            assert_eq!(outcome.state, Some(SyncState::UpToDate), "{outcome:?}");
            assert_eq!((outcome.applied, outcome.sent.clone(), outcome.failed.clone()), (3, vec!["Re: First".to_owned()], vec![]));
            assert_eq!(env.pop3.lock().deleted, vec!["U2"], "only the trashed message left the server");
            assert_eq!(store.message(&second.id).unwrap().unwrap().label_ids.iter().map(|l| l.as_str()).collect::<Vec<_>>(), vec!["pop3:jason@example.com:trash"]);
            assert!(store.message(&first.id).unwrap().unwrap().label_ids.is_empty(), "archived = out of the Inbox, as for Gmail");
            let archived = store.conversations(&env.account.id, "jason@example.com", &overlook_store::MailboxView::Kind(LabelKind::Archive), overlook_store::ListFilter::All, None).unwrap();
            assert!(archived.iter().any(|c| c.id == first.conversation_id), "shown in the Archive view");
            assert_eq!(env.smtp.seen.lock().unwrap().len(), 1);
            let sent = imap_message_named(&store, &env.account.id, "Re: First");
            let sent = store.messages_for_account(&env.account.id).unwrap().into_iter().find(|m| m.is_outgoing && m.subject == "Re: First").unwrap_or(sent);
            assert!(sent.provider_id.as_ref().is_some_and(is_sent_copy));
            assert_eq!(sent.label_ids.iter().map(|l| l.as_str()).collect::<Vec<_>>(), vec!["pop3:jason@example.com:sent"]);
            assert_eq!(sent.conversation_id, first.conversation_id, "the reply threads with the original");
            assert_eq!(store.drafts(&env.account.id, true).unwrap().len(), 0, "the draft left the Outbox");
            assert_eq!(store.message_count(&env.account.id).unwrap(), 4);

            // Another poll: the Trash and Archive labels hold, the sent copy is not re-downloaded.
            block_on(run_sync(store.clone(), provider.clone(), None, env.account.clone()));
            assert_eq!(store.message_count(&env.account.id).unwrap(), 4);
            assert_eq!(store.message(&second.id).unwrap().unwrap().label_ids.len(), 1);

            // An uncertain send stays Uncertain for the user (nothing to reconcile against).
            env.smtp.set(Behaviour::DropAfterData);
            let mut draft2 = Draft::new(env.account.id.clone(), DraftKind::New, now());
            draft2.to = vec![Address::named("Bob", "bob@example.com")];
            draft2.subject = "Uncertain".into();
            draft2.body_text = "?".into();
            store.save_draft(&draft2).unwrap();
            store.queue_send(&draft2).unwrap();
            let outcome = block_on(run_sync(store.clone(), provider.clone(), None, env.account.clone()));
            assert_eq!(outcome.uncertain, vec!["Uncertain".to_owned()]);
            env.smtp.set(Behaviour::Normal);
            let outcome = block_on(run_sync(store.clone(), provider.clone(), None, env.account.clone()));
            assert!(outcome.sent.is_empty(), "not resent on its own");
            assert_eq!(store.drafts(&env.account.id, true).unwrap()[0].state, DraftState::Uncertain);
        }

        /// The plan's M4 exit criterion against `scripts/spike-mail-servers.py` (start it
        /// first; it keeps state between runs, so use a fresh process per run). Run with
        /// `cargo test -p overlook pop3_exit_criteria -- --ignored --nocapture`; optional
        /// `OVERLOOK_SPIKE_POP3_PORT` / `OVERLOOK_SPIKE_SMTP_PORT` (1110 / 2525). Downloads
        /// the maildrop once, polls without duplicates, sends through SMTP, honours
        /// delete-after-N-days and then leave-on-server off (the spike's INBOX ends empty),
        /// and writes a backup that opens on a clean directory.
        #[test]
        #[ignore]
        fn pop3_exit_criteria_against_the_spike_servers() {
            let port = |name: &str, default: u16| std::env::var(name).ok().and_then(|p| p.parse().ok()).unwrap_or(default);
            let (pop3_port, smtp_port) = (port("OVERLOOK_SPIKE_POP3_PORT", 1110), port("OVERLOOK_SPIKE_SMTP_PORT", 2525));
            let servers = |policy: Option<Pop3Policy>| MailServers {
                incoming: ServerEndpoint::new("127.0.0.1", pop3_port, Security::None, "spike"),
                outgoing: ServerEndpoint::new("127.0.0.1", smtp_port, Security::None, "spike"),
                pop3: policy,
                imap: None,
            };
            let account = Account { id: AccountId::from("pop3:spike@example.com"), provider: ProviderKind::Pop3, display_name: "Spike".into(), email: "spike@example.com".into(), created_at: 0 };
            let dir = std::env::temp_dir().join(format!("overlook-spike-m4-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let provider = |policy: Option<Pop3Policy>| Arc::new(Pop3Provider::new(servers(policy), "secret", dir.join("raw")).with_timeout(Duration::from_secs(5)));
            let store = Arc::new(Store::open(&dir.join("overlook.sqlite")).unwrap());
            store.upsert_account(&account).unwrap();

            let keep = provider(None);
            let outcome = block_on(run_sync(store.clone(), keep.clone(), None, account.clone()));
            assert_eq!(outcome.state, Some(SyncState::UpToDate), "{outcome:?}");
            let count = store.message_count(&account.id).unwrap();
            eprintln!("initial download: {count} messages, {} .eml files", std::fs::read_dir(dir.join("raw")).map(|d| d.count()).unwrap_or(0));
            assert!(count >= 3);
            for _ in 0..2 {
                let outcome = block_on(run_sync(store.clone(), keep.clone(), None, account.clone()));
                assert_eq!((outcome.state, outcome.downloaded), (Some(SyncState::UpToDate), 0), "repeated polls download nothing");
            }
            assert_eq!(store.message_count(&account.id).unwrap(), count);
            let restarted = provider(None);
            block_on(run_sync(store.clone(), restarted, None, account.clone()));
            assert_eq!(store.message_count(&account.id).unwrap(), count, "a fresh provider reads the ledger from the checkpoint");
            eprintln!("repeated polls: still {count} messages");

            // Send through SMTP: the Sent copy is local.
            let mut draft = Draft::new(account.id.clone(), DraftKind::New, now());
            draft.to = vec![Address::named("Ada", "ada@example.com")];
            draft.subject = format!("From POP3 {}", std::process::id());
            draft.body_text = "Sent through the spike SMTP.".into();
            store.save_draft(&draft).unwrap();
            store.queue_send(&draft).unwrap();
            let outcome = block_on(run_sync(store.clone(), keep.clone(), None, account.clone()));
            assert_eq!(outcome.sent, vec![draft.subject.clone()], "{outcome:?}");
            let sent = store.messages_for_account(&account.id).unwrap().into_iter().find(|m| m.subject == draft.subject).unwrap();
            assert!(sent.label_ids.iter().any(|l| l.as_str().ends_with(":sent")));
            assert!(dir.join("raw").join(format!("{}.eml", sent.id.as_str().replace([':', '/'], "_"))).exists(), "the sent bytes are kept");
            eprintln!("sent {:?}; local Sent copy {}", draft.subject, sent.id.0);

            // delete-after-N-days: age one ledger entry, the next cycle deletes only that one.
            let cp = store.checkpoint(&account.id).unwrap().unwrap();
            let mut parsed = overlook_provider::pop3::Checkpoint::parse(&cp).unwrap();
            let oldest = parsed.entries.keys().next().unwrap().clone();
            parsed.entries.get_mut(&oldest).unwrap().seen_at -= 31 * 86_400_000;
            store.set_checkpoint(&account.id, Some(&parsed.to_json())).unwrap();
            let after_30 = provider(Some(Pop3Policy { delete_after_days: Some(30), ..Default::default() }));
            block_on(run_sync(store.clone(), after_30, None, account.clone()));
            let rows = store.pop3_seen(&account.id).unwrap();
            assert_eq!(rows.iter().filter(|r| r.deleted_on_server).map(|r| r.uidl.clone()).collect::<Vec<_>>(), vec![oldest.clone()]);
            assert_eq!(store.message_count(&account.id).unwrap(), count + 1, "deleting on the server removes nothing locally");
            eprintln!("delete after 30 days: {oldest} deleted on the server, the rest kept");

            // leave-on-server off: everything downloaded leaves the server.
            let delete = provider(Some(Pop3Policy { leave_on_server: false, ..Default::default() }));
            block_on(run_sync(store.clone(), delete.clone(), None, account.clone()));
            let rows = store.pop3_seen(&account.id).unwrap();
            assert!(rows.iter().all(|r| r.deleted_on_server), "{rows:?}");
            let outcome = block_on(run_sync(store.clone(), delete, None, account.clone()));
            assert_eq!((outcome.state, outcome.downloaded), (Some(SyncState::UpToDate), 0));
            assert_eq!(store.message_count(&account.id).unwrap(), count + 1);
            eprintln!("leave on server off: the spike INBOX is empty, {} messages still local", count + 1);

            // Backup opens on a clean directory and serves message sources from its own raw/.
            let target = dir.join("backup");
            let files = crate::backup::write_backup(&store, &dir, &target).unwrap();
            let restored = Store::open(&target.join("overlook.sqlite")).unwrap();
            assert_eq!(restored.message_count(&account.id).unwrap(), count + 1);
            let restored_provider = Pop3Provider::new(servers(None), "secret", target.join("raw"));
            let m = restored.messages_for_account(&account.id).unwrap().into_iter().find(|m| !m.is_outgoing).unwrap();
            let raw = block_on(restored_provider.fetch_raw(&account, &m)).unwrap();
            assert!(String::from_utf8_lossy(&raw).contains("Message-ID:"));
            eprintln!("backup: {files} files at {}; reopened with {} messages", target.display(), count + 1);
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[test]
        fn pop3_user_labels_survive_a_full_resync_and_backup_opens_on_a_clean_dir() {
            let env = pop_env("pop3-labels-backup");
            let store = pop_store(&env);
            let provider = pop_provider(&env, None);
            block_on(run_sync(store.clone(), provider.clone(), None, env.account.clone()));
            let user = Label { id: LabelId::from("pop3:jason@example.com:user:projects"), account_id: env.account.id.clone(), kind: LabelKind::User, name: "Projects".into(), provider_id: None, color: Some("plum".into()), sort_order: 10 };
            store.upsert_label(&user).unwrap();
            // A lost checkpoint forces a full snapshot: the local label stays, nothing doubles.
            store.set_checkpoint(&env.account.id, Some("not a pop3 checkpoint")).unwrap();
            let outcome = block_on(run_sync(store.clone(), pop_provider(&env, None), None, env.account.clone()));
            assert_eq!(outcome.state, Some(SyncState::UpToDate), "{outcome:?}");
            assert_eq!(store.message_count(&env.account.id).unwrap(), 3);
            assert!(store.labels(&env.account.id).unwrap().iter().any(|l| l.id == user.id));
            assert_eq!(env.pop3.lock().commands("RETR"), 3, "messages already on disk are not fetched again");

            let target = env.dir.join("backup");
            let files = crate::backup::write_backup(&store, &env.dir, &target).unwrap();
            assert_eq!(files, 1 + 3, "the database and three .eml files");
            let restored = Store::open(&target.join("overlook.sqlite")).unwrap();
            assert_eq!(restored.message_count(&env.account.id).unwrap(), 3);
            assert_eq!(restored.pop3_seen(&env.account.id).unwrap().len(), 3);
            let message = restored.messages_for_account(&env.account.id).unwrap().remove(0);
            let restored_provider = Pop3Provider::new(pop_servers(&env, None), "hunter2", target.join("raw"));
            let raw = block_on(restored_provider.fetch_raw(&env.account, &message)).unwrap();
            assert!(String::from_utf8_lossy(&raw).contains(&format!("Subject: {}", message.subject)), "the restored copy serves message sources from its own raw/");
        }
    }
}
