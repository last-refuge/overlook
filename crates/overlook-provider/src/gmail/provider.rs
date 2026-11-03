//! `MailProvider` for Gmail.

use super::api::{ApiError, GmailClient};
use super::mapping::{self, base64url_encode, gmail_id, inside_inbox, label_from_json, message_from_json};
use crate::outgoing::build_gmail_raw as build_raw;
use crate::{draft_message_id, folder_name_problem, AppliedChanges, BoxFuture, MailProvider, MailboxSnapshot, ProviderError, RemoteChanges, RenamedFolder, SendContext, SendOutcome, SentReceipt};
use overlook_core::time::Timestamp;
use overlook_core::*;
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use tracing::{info, warn};

/// How many recent messages the first download fetches. Progressive history download is a
/// phase 1 item; this keeps the first sync to a few minutes under Gmail's quota.
pub const INITIAL_MESSAGES: usize = 150;

/// Views covered by the first download: (Gmail search, how many newest messages). Gmail's
/// list excludes Trash and Spam unless asked, and a plain "newest 150" would rarely reach
/// archived or trashed mail, so each view is listed on its own.
pub const INITIAL_VIEWS: &[(&str, usize)] = &[
    ("in:inbox -in:chats", 100),
    ("in:sent", 40),
    ("-in:inbox -in:sent -in:trash -in:spam -in:drafts -in:chats", 40),
    ("is:starred -in:trash", 20),
    ("in:trash", 20),
    ("in:spam", 10),
];

pub struct GmailProvider {
    client: Arc<GmailClient>,
    initial_messages: usize,
    inbox_limit: usize,
}

impl GmailProvider {
    pub fn new(client: Arc<GmailClient>) -> Self {
        Self { client, initial_messages: INITIAL_MESSAGES, inbox_limit: 100 }
    }

    /// Newest Inbox messages to list on first download; other views scale with it.
    pub fn with_inbox_limit(mut self, inbox_limit: usize) -> Self {
        self.inbox_limit = inbox_limit.max(20);
        self.initial_messages = self.initial_messages.max(inbox_limit * 2);
        self
    }

    pub fn client(&self) -> &Arc<GmailClient> {
        &self.client
    }

    fn labels(&self, account: &Account) -> Result<Vec<Label>, ApiError> {
        let v = self.client.get("labels", &[])?;
        let mut labels: Vec<Label> = v
            .get("labels")
            .and_then(|l| l.as_array())
            .map(|arr| arr.iter().enumerate().filter_map(|(i, l)| label_from_json(&account.id, l, i as i32)).collect())
            .unwrap_or_default();
        // Stable order: system labels first in a fixed order, then user labels by name.
        labels.sort_by(|a, b| a.sort_order.cmp(&b.sort_order).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
        for (i, l) in labels.iter_mut().enumerate() {
            l.sort_order = i as i32;
        }
        Ok(labels)
    }

    fn label_index(labels: &[Label]) -> HashMap<String, LabelId> {
        labels
            .iter()
            .filter_map(|l| l.provider_id.as_ref().map(|p| (p.0.clone(), l.id.clone())))
            .collect()
    }

    fn fetch_message(&self, account: &Account, id: &str, labels: &HashMap<String, LabelId>) -> Result<Option<Message>, ApiError> {
        let message = match self.client.get(&format!("messages/{id}"), &[("format", "full")]) {
            Ok(v) => message_from_json(account, &v, labels),
            Err(ApiError::NotFound) => return Ok(None),
            Err(e) => return Err(e),
        };
        Ok(self.finish_message(message, id))
    }

    /// Gmail often ships the calendar part as an attachment reference instead of inline
    /// text; fetch it so invitations are recognised.
    fn finish_message(&self, mut message: Option<Message>, id: &str) -> Option<Message> {
        if let Some(m) = message.as_mut() {
            if m.invitation.is_none() {
                let ics = m
                    .attachments
                    .iter()
                    .find(|a| a.mime_type.starts_with("text/calendar") || a.mime_type.starts_with("application/ics") || a.filename.to_ascii_lowercase().ends_with(".ics"))
                    .and_then(|a| a.provider_id.clone());
                if let Some(aid) = ics {
                    if let Ok(v) = self.client.get(&format!("messages/{id}/attachments/{}", aid.0), &[]) {
                        if let Some(data) = v.get("data").and_then(|d| d.as_str()) {
                            let text = String::from_utf8_lossy(&mapping::base64url_decode(data)).into_owned();
                            m.invitation = overlook_core::ics::parse_invitation(&text);
                        }
                    }
                }
            }
        }
        message
    }

    /// Lists ids for every view in [`INITIAL_VIEWS`], newest first, de-duplicated.
    fn list_initial_ids(&self) -> Result<Vec<String>, ApiError> {
        let mut seen = BTreeSet::new();
        let mut ids = Vec::new();
        let scale = self.inbox_limit as f32 / 100.;
        for (search, limit) in INITIAL_VIEWS {
            let limit = ((*limit as f32) * scale).round().max(10.) as usize;
            for id in self.list_ids(search, limit)? {
                if seen.insert(id.clone()) {
                    ids.push(id);
                }
            }
        }
        Ok(ids)
    }

    fn list_ids(&self, search: &str, limit: usize) -> Result<Vec<String>, ApiError> {
        let mut ids = Vec::new();
        let mut page: Option<String> = None;
        while ids.len() < limit {
            let remaining = (limit - ids.len()).min(100).to_string();
            let mut query = vec![("maxResults", remaining.as_str()), ("q", search), ("includeSpamTrash", "true")];
            if let Some(p) = page.as_deref() {
                query.push(("pageToken", p));
            }
            let v = self.client.get("messages", &query)?;
            let batch: Vec<String> = v
                .get("messages")
                .and_then(|m| m.as_array())
                .map(|a| a.iter().filter_map(|m| m.get("id").and_then(|i| i.as_str()).map(str::to_owned)).collect())
                .unwrap_or_default();
            if batch.is_empty() {
                break;
            }
            ids.extend(batch);
            page = v.get("nextPageToken").and_then(|t| t.as_str()).map(str::to_owned);
            if page.is_none() {
                break;
            }
        }
        ids.truncate(limit);
        Ok(ids)
    }

    fn batch_modify(&self, ids: &[String], add: &[&str], remove: &[&str]) -> Result<(), ApiError> {
        if ids.is_empty() {
            return Ok(());
        }
        for chunk in ids.chunks(500) {
            self.client.post("messages/batchModify", &json!({ "ids": chunk, "addLabelIds": add, "removeLabelIds": remove }))?;
        }
        Ok(())
    }

    fn gmail_ids(account: &Account, messages: &[Message]) -> Vec<String> {
        messages
            .iter()
            .filter_map(|m| m.provider_id.as_ref().map(|p| p.0.clone()).or_else(|| gmail_id(&account.id, m.id.as_str())))
            .collect()
    }
}

/// The user labels nested inside `label` (Gmail nests by "/" in the name).
fn labels_inside(label: &Label, labels: &[Label]) -> Vec<Label> {
    let prefix = format!("{}/", label.name);
    labels.iter().filter(|l| l.kind == LabelKind::User && l.id != label.id && l.name.starts_with(&prefix)).cloned().collect()
}

/// The Gmail id of `label` when it is one of the user's own labels; system labels and
/// labels not yet on the server can't be `verb` ("renamed", "moved", "deleted").
fn own_label<'a>(label: &'a Label, verb: &str) -> Result<&'a ProviderId, ProviderError> {
    label.provider_id.as_ref().filter(|_| label.kind == LabelKind::User).ok_or_else(|| ProviderError::Rejected(format!("only your own labels can be {verb}")))
}

/// Whether another label than `except` already has `name` (Gmail ignores case).
fn name_taken(labels: &[Label], except: &LabelId, name: &str) -> bool {
    labels.iter().any(|l| &l.id != except && l.name.eq_ignore_ascii_case(name))
}

impl GmailProvider {
    /// Renames `label` to `target` and the labels `inside` it to match: Gmail nests by name
    /// and does not rename children itself. Returns every label renamed, `label` first. A
    /// child that fails is logged and left under its old name, as the server has it.
    fn rename_with_children(&self, account: &Account, label: &Label, target: &str, inside: Vec<Label>) -> Result<Vec<RenamedFolder>, ProviderError> {
        let id = own_label(label, "renamed")?;
        let v = with_inbox_spelling(target, |name| self.client.patch(&format!("labels/{}", id.0), &json!({ "name": name })))?;
        let renamed = label_from_json(&account.id, &v, label.sort_order).ok_or_else(|| ProviderError::Temporary("Gmail did not return the renamed label".into()))?;
        let mut changed = vec![RenamedFolder { old: label.clone(), new: renamed.clone() }];
        for child in inside {
            let Some(child_id) = child.provider_id.as_ref() else { continue };
            let name = format!("{}{}", renamed.name, &child.name[label.name.len()..]);
            match with_inbox_spelling(&name, |name| self.client.patch(&format!("labels/{}", child_id.0), &json!({ "name": name }))) {
                Ok(v) => match label_from_json(&account.id, &v, child.sort_order) {
                    Some(new) => changed.push(RenamedFolder { old: child, new }),
                    None => warn!("gmail: no label returned for {} after renaming it", child.name),
                },
                Err(e) => warn!("gmail: could not rename label {} with its parent: {e:?}", child.name),
            }
        }
        Ok(changed)
    }
}

impl MailProvider for GmailProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Gmail
    }

    fn supports_drafts(&self) -> bool { true }

    fn list_drafts(&self, _account: &Account) -> BoxFuture<'_, Result<Vec<RemoteDraft>, ProviderError>> {
        Box::pin(async move { super::drafts::list(&self.client) })
    }

    fn fetch_draft(&self, _account: &Account, remote: &RemoteDraft) -> BoxFuture<'_, Result<Option<Vec<u8>>, ProviderError>> {
        let remote = remote.clone();
        Box::pin(async move { super::drafts::fetch(&self.client, &remote) })
    }

    fn put_draft(&self, _account: &Account, remote: Option<&RemoteDraft>, raw: &[u8], message_id: &str, thread_id: Option<&str>) -> BoxFuture<'_, Result<Option<RemoteDraft>, ProviderError>> {
        let (remote, raw, message_id, thread_id) = (remote.cloned(), raw.to_vec(), message_id.to_owned(), thread_id.map(str::to_owned));
        Box::pin(async move { super::drafts::put(&self.client, remote.as_ref(), &raw, &message_id, thread_id.as_deref()) })
    }

    fn delete_remote_draft(&self, _account: &Account, remote: &RemoteDraft) -> BoxFuture<'_, Result<(), ProviderError>> {
        let remote = remote.clone();
        Box::pin(async move { super::drafts::delete(&self.client, &remote) })
    }

    fn probe(&self) -> BoxFuture<'_, Result<(), ProviderError>> {
        Box::pin(async move {
            self.client.get("profile", &[]).map(|_| ()).map_err(ProviderError::from)
        })
    }

    fn fetch_snapshot(&self, account: &Account) -> BoxFuture<'_, Result<MailboxSnapshot, ProviderError>> {
        let account = account.clone();
        Box::pin(async move {
            // Take the history position before listing so no change is missed.
            let profile = self.client.get("profile", &[])?;
            let checkpoint = profile.get("historyId").and_then(|h| h.as_str().map(str::to_owned).or_else(|| h.as_u64().map(|n| n.to_string())));
            let labels = self.labels(&account)?;
            let mut ids = self.list_initial_ids()?;
            ids.truncate(self.initial_messages.max(ids.len().min(self.inbox_limit * 3)));
            info!("gmail: {} recent messages queued for download", ids.len());
            Ok(MailboxSnapshot {
                labels,
                messages: Vec::new(),
                message_ids: ids.into_iter().map(ProviderId).collect(),
                checkpoint,
            })
        })
    }

    fn has_settings_access(&self) -> bool {
        self.client.has_scope(super::oauth::SETTINGS_SCOPE_MARKER)
    }

    fn fetch_vacation(&self, _account: &Account) -> BoxFuture<'_, Result<Option<VacationSettings>, ProviderError>> {
        Box::pin(async move {
            if !self.has_settings_access() {
                return Ok(None);
            }
            let v = self.client.get("settings/vacation", &[])?;
            let ms = |k: &str| v.get(k).and_then(|x| x.as_str()).and_then(|s| s.parse::<i64>().ok()).filter(|n| *n > 0);
            Ok(Some(VacationSettings {
                enabled: v.get("enableAutoReply").and_then(|b| b.as_bool()).unwrap_or(false),
                subject: v.get("responseSubject").and_then(|s| s.as_str()).unwrap_or("").to_owned(),
                body: v
                    .get("responseBodyPlainText")
                    .and_then(|s| s.as_str())
                    .map(str::to_owned)
                    .or_else(|| v.get("responseBodyHtml").and_then(|s| s.as_str()).map(overlook_core::sanitize::html_to_text))
                    .unwrap_or_default(),
                start: ms("startTime"),
                end: ms("endTime"),
                contacts_only: v.get("restrictToContacts").and_then(|b| b.as_bool()).unwrap_or(false),
            }))
        })
    }

    fn set_vacation(&self, _account: &Account, settings: &VacationSettings) -> BoxFuture<'_, Result<(), ProviderError>> {
        let settings = settings.clone();
        Box::pin(async move {
            if !self.has_settings_access() {
                return Err(ProviderError::SignInRequired);
            }
            let mut body = json!({
                "enableAutoReply": settings.enabled,
                "responseSubject": settings.subject,
                "responseBodyPlainText": settings.body,
                "restrictToContacts": settings.contacts_only,
                "restrictToDomain": false,
            });
            if let Some(s) = settings.start {
                body["startTime"] = Value::String(s.to_string());
            }
            if let Some(e) = settings.end {
                body["endTime"] = Value::String(e.to_string());
            }
            let url = format!("{}/settings/vacation", super::api::API_BASE);
            self.client.put_url(&url, &body)?;
            Ok(())
        })
    }

    fn has_contacts_access(&self) -> bool {
        self.client.has_scope(super::oauth::CONTACTS_SCOPE_MARKER)
    }

    fn has_contacts_write_access(&self) -> bool {
        self.client.tokens().scope.as_deref().is_some_and(|s| s.split_whitespace().any(|x| x == super::oauth::CONTACTS_WRITE_SCOPE))
    }

    fn fetch_contacts(&self, account: &Account) -> BoxFuture<'_, Result<Option<Vec<Contact>>, ProviderError>> {
        let account = account.clone();
        Box::pin(async move {
            if !self.has_contacts_access() {
                return Ok(None);
            }
            let now = overlook_core::time::now();
            let mut out: Vec<Contact> = Vec::new();
            for (url, list, in_book) in [
                (format!("{PEOPLE_BASE}/people/me/connections?personFields={PERSON_FIELDS}&pageSize=1000"), "connections", true),
                (format!("{PEOPLE_BASE}/otherContacts?readMask=names,emailAddresses,phoneNumbers&pageSize=1000"), "otherContacts", false),
            ] {
                let mut page: Option<String> = None;
                loop {
                    let full = match &page {
                        Some(p) => format!("{url}&pageToken={}", super::oauth::url_encode(p)),
                        None => url.clone(),
                    };
                    let v = match self.client.get_url(&full) {
                        Ok(v) => v,
                        // A missing People API or scope must not break the mail sync.
                        Err(ApiError::Client(403, _)) | Err(ApiError::Unauthorized) if in_book => return Ok(None),
                        Err(ApiError::Client(403, _)) | Err(ApiError::Unauthorized) => break,
                        Err(e) => return Err(e.into()),
                    };
                    for person in v.get(list).and_then(|c| c.as_array()).into_iter().flatten() {
                        if let Some(c) = contact_from_person(&account.id, person, in_book, now) {
                            out.push(c);
                        }
                    }
                    page = v.get("nextPageToken").and_then(|t| t.as_str()).map(str::to_owned);
                    if page.is_none() {
                        break;
                    }
                }
            }
            Ok(Some(out))
        })
    }

    fn create_contact(&self, account: &Account, contact: &Contact) -> BoxFuture<'_, Result<Contact, ProviderError>> {
        let account = account.clone();
        let contact = contact.clone();
        Box::pin(async move {
            if !self.has_contacts_write_access() {
                return Err(ProviderError::SignInRequired);
            }
            let url = format!("{PEOPLE_BASE}/people:createContact?personFields={PERSON_FIELDS}");
            let v = self.client.post_url(&url, &person_json(&contact, None))?;
            contact_from_person(&account.id, &v, true, overlook_core::time::now()).ok_or_else(|| ProviderError::Rejected("Google returned an unreadable contact".into()))
        })
    }

    fn update_contact(&self, account: &Account, contact: &Contact) -> BoxFuture<'_, Result<Contact, ProviderError>> {
        let account = account.clone();
        let contact = contact.clone();
        Box::pin(async move {
            if !self.has_contacts_write_access() {
                return Err(ProviderError::SignInRequired);
            }
            // The update needs the current etag; fetch it when the local copy has none or
            // when Google says ours is stale.
            let fetch_etag = || -> Result<Option<String>, ApiError> {
                let v = self.client.get_url(&format!("{PEOPLE_BASE}/{}?personFields=metadata", contact.id))?;
                Ok(v.get("etag").and_then(|e| e.as_str()).map(str::to_owned))
            };
            let mut etag = contact.etag.clone();
            if etag.is_none() {
                etag = fetch_etag()?;
            }
            let url = format!("{PEOPLE_BASE}/{}:updateContact?updatePersonFields={UPDATE_FIELDS}&personFields={PERSON_FIELDS}", contact.id);
            let v = match self.client.patch_url(&url, &person_json(&contact, etag.as_deref())) {
                Ok(v) => v,
                Err(ApiError::Client(400, m)) if m.to_ascii_lowercase().contains("etag") => {
                    let fresh = fetch_etag()?;
                    self.client.patch_url(&url, &person_json(&contact, fresh.as_deref()))?
                }
                Err(e) => return Err(e.into()),
            };
            contact_from_person(&account.id, &v, true, overlook_core::time::now()).ok_or_else(|| ProviderError::Rejected("Google returned an unreadable contact".into()))
        })
    }

    fn delete_contact(&self, _account: &Account, resource_name: &str) -> BoxFuture<'_, Result<(), ProviderError>> {
        let resource_name = resource_name.to_owned();
        Box::pin(async move {
            if !self.has_contacts_write_access() {
                return Err(ProviderError::SignInRequired);
            }
            match self.client.delete_url(&format!("{PEOPLE_BASE}/{resource_name}:deleteContact")) {
                Ok(_) | Err(ApiError::NotFound) => Ok(()),
                Err(e) => Err(e.into()),
            }
        })
    }

    fn fetch_raw(&self, _account: &Account, message: &Message) -> BoxFuture<'_, Result<Vec<u8>, ProviderError>> {
        let fallback = crate::message_to_raw(message);
        let id = message.provider_id.clone();
        Box::pin(async move {
            let Some(id) = id else { return Ok(fallback) };
            let v = self.client.get(&format!("messages/{}", id.0), &[("format", "raw")])?;
            Ok(v.get("raw").and_then(|r| r.as_str()).map(mapping::base64url_decode).filter(|b| !b.is_empty()).unwrap_or(fallback))
        })
    }

    fn search(&self, _account: &Account, query: &str, limit: usize) -> BoxFuture<'_, Result<Vec<ProviderId>, ProviderError>> {
        let query = query.to_owned();
        Box::pin(async move {
            let ids = self.list_ids(&query, limit)?;
            Ok(ids.into_iter().map(ProviderId).collect())
        })
    }

    fn fetch_messages(&self, account: &Account, ids: &[ProviderId], labels: &[Label]) -> BoxFuture<'_, Result<Vec<Message>, ProviderError>> {
        let account = account.clone();
        let ids: Vec<String> = ids.iter().map(|p| p.0.clone()).collect();
        let index = Self::label_index(labels);
        Box::pin(async move {
            let mut out = Vec::with_capacity(ids.len());
            if ids.len() == 1 {
                if let Some(m) = self.fetch_message(&account, &ids[0], &index)? {
                    out.push(m);
                }
                return Ok(out);
            }
            // One HTTP request per batch instead of one per message.
            for chunk in ids.chunks(super::api::BATCH_SIZE) {
                let paths: Vec<String> = chunk.iter().map(|id| format!("messages/{id}?format=full")).collect();
                let results = self.client.batch_get(&paths)?;
                for (id, result) in chunk.iter().zip(results) {
                    match result {
                        Ok(v) => {
                            if let Some(m) = self.finish_message(message_from_json(&account, &v, &index), id) {
                                out.push(m);
                            }
                        }
                        Err(ApiError::NotFound) => {}
                        Err(e) => return Err(e.into()),
                    }
                }
            }
            Ok(out)
        })
    }

    fn fetch_changes(&self, account: &Account, checkpoint: &str) -> BoxFuture<'_, Result<RemoteChanges, ProviderError>> {
        let account = account.clone();
        let checkpoint = checkpoint.to_owned();
        Box::pin(async move {
            let mut added: BTreeSet<String> = BTreeSet::new();
            let mut changed: BTreeSet<String> = BTreeSet::new();
            let mut deleted: BTreeSet<String> = BTreeSet::new();
            let mut labels_touched = false;
            let mut new_checkpoint = checkpoint.clone();
            let mut page: Option<String> = None;
            loop {
                let mut query = vec![("startHistoryId", checkpoint.as_str()), ("maxResults", "500")];
                if let Some(p) = page.as_deref() {
                    query.push(("pageToken", p));
                }
                let v = match self.client.get("history", &query) {
                    Ok(v) => v,
                    Err(ApiError::NotFound) => return Err(ProviderError::CheckpointExpired),
                    Err(e) => return Err(e.into()),
                };
                if let Some(h) = v.get("historyId").and_then(|h| h.as_str()) {
                    new_checkpoint = h.to_owned();
                }
                for record in v.get("history").and_then(|h| h.as_array()).into_iter().flatten() {
                    let ids_in = |key: &str| -> Vec<String> {
                        record
                            .get(key)
                            .and_then(|a| a.as_array())
                            .map(|a| a.iter().filter_map(|x| x.get("message").and_then(|m| m.get("id")).and_then(|i| i.as_str()).map(str::to_owned)).collect())
                            .unwrap_or_default()
                    };
                    added.extend(ids_in("messagesAdded"));
                    deleted.extend(ids_in("messagesDeleted"));
                    let label_changes: Vec<String> = ids_in("labelsAdded").into_iter().chain(ids_in("labelsRemoved")).collect();
                    if !label_changes.is_empty() {
                        labels_touched = true;
                    }
                    changed.extend(label_changes);
                }
                page = v.get("nextPageToken").and_then(|t| t.as_str()).map(str::to_owned);
                if page.is_none() {
                    break;
                }
            }
            for id in &deleted {
                added.remove(id);
                changed.remove(id);
            }
            let labels = if labels_touched || !added.is_empty() { Some(self.labels(&account)?) } else { None };
            let index = match &labels {
                Some(l) => Self::label_index(l),
                None => HashMap::new(),
            };
            let mut upserts = Vec::new();
            for id in added.iter().chain(changed.iter()) {
                match self.fetch_message(&account, id, &index) {
                    Ok(Some(m)) => upserts.push(m),
                    Ok(None) => {}
                    Err(e) => {
                        warn!("gmail: could not fetch {id}: {e:?}");
                        return Err(e.into());
                    }
                }
            }
            Ok(RemoteChanges {
                labels,
                upserts,
                deleted: deleted.iter().map(|id| mapping::local_message_id(&account.id, id)).collect(),
                message_ids: Vec::new(),
                deleted_provider_ids: Vec::new(),
                checkpoint: Some(new_checkpoint),
                complete: None,
                untracked: Vec::new(),
            })
        })
    }

    fn apply(&self, account: &Account, op: &PendingOperation, messages: &[Message], labels: &[Label]) -> BoxFuture<'_, Result<AppliedChanges, ProviderError>> {
        let account = account.clone();
        let op = op.clone();
        let ids = Self::gmail_ids(&account, messages);
        let labels = labels.to_vec();
        Box::pin(async move {
            let provider_label = |id: &LabelId| -> Result<String, ProviderError> {
                labels
                    .iter()
                    .find(|l| &l.id == id)
                    .and_then(|l| l.provider_id.as_ref().map(|p| p.0.clone()))
                    .ok_or_else(|| ProviderError::Rejected("label no longer exists".into()))
            };
            match &op.kind {
                OperationKind::SetRead { read: true, .. } => self.batch_modify(&ids, &[], &["UNREAD"])?,
                OperationKind::SetRead { read: false, .. } => self.batch_modify(&ids, &["UNREAD"], &[])?,
                OperationKind::SetStarred { starred: true, .. } => self.batch_modify(&ids, &["STARRED"], &[])?,
                OperationKind::SetStarred { starred: false, .. } => self.batch_modify(&ids, &[], &["STARRED"])?,
                OperationKind::Archive { .. } => self.batch_modify(&ids, &[], &["INBOX"])?,
                OperationKind::AddLabel { label_id, .. } => {
                    let l = provider_label(label_id)?;
                    self.batch_modify(&ids, &[l.as_str()], &[])?
                }
                OperationKind::RemoveLabel { label_id, .. } => {
                    let l = provider_label(label_id)?;
                    self.batch_modify(&ids, &[], &[l.as_str()])?
                }
                OperationKind::Trash { .. } => {
                    for id in &ids {
                        match self.client.post(&format!("messages/{id}/trash"), &Value::Null) {
                            Ok(_) | Err(ApiError::NotFound) => {}
                            Err(e) => return Err(e.into()),
                        }
                    }
                }
                OperationKind::Untrash { .. } => {
                    for id in &ids {
                        match self.client.post(&format!("messages/{id}/untrash"), &Value::Null) {
                            Ok(_) | Err(ApiError::NotFound) => {}
                            Err(e) => return Err(e.into()),
                        }
                    }
                }
                OperationKind::SendDraft { .. } => return Err(ProviderError::Rejected("sends go through send()".into())),
                // Needs the full mail.google.com scope, which Overlook does not request.
                OperationKind::DeleteForever { .. } => return Err(ProviderError::Rejected("Gmail messages can only be moved to Trash".into())),
                kind if kind.is_calendar() => return Err(ProviderError::Rejected("calendar operations go through the calendar provider".into())),
                _ => {}
            }
            Ok(AppliedChanges::default())
        })
    }

    fn fetch_attachment(&self, account: &Account, message: &Message, attachment: &Attachment) -> BoxFuture<'_, Result<Vec<u8>, ProviderError>> {
        let message_id = message.provider_id.as_ref().map(|p| p.0.clone()).or_else(|| gmail_id(&account.id, message.id.as_str()));
        let attachment_id = attachment.provider_id.as_ref().map(|p| p.0.clone());
        Box::pin(async move {
            let (Some(mid), Some(aid)) = (message_id, attachment_id) else {
                return Err(ProviderError::Rejected("this attachment has no Gmail id".into()));
            };
            let v = self.client.get(&format!("messages/{mid}/attachments/{aid}"), &[])?;
            let data = v.get("data").and_then(|d| d.as_str()).ok_or_else(|| ProviderError::Rejected("Gmail returned no attachment data".into()))?;
            Ok(mapping::base64url_decode(data))
        })
    }

    fn send(&self, account: &Account, draft: &Draft, context: SendContext) -> BoxFuture<'_, Result<SentReceipt, ProviderError>> {
        let account = account.clone();
        let draft = draft.clone();
        Box::pin(async move {
            let rfc_id = draft_message_id(&draft);
            let raw = build_raw(&account, &draft, context.in_reply_to.as_ref(), &rfc_id).map_err(ProviderError::Rejected)?;
            let mut body = json!({ "raw": base64url_encode(&raw) });
            if let Some(source) = &context.in_reply_to {
                if let Some(thread) = gmail_id(&account.id, source.conversation_id.as_str()) {
                    body["threadId"] = Value::String(thread);
                }
            }
            if body.get("threadId").is_none() {
                if let Some(thread) = draft.conversation_id.as_ref().and_then(|id| gmail_id(&account.id, id.as_str())) { body["threadId"] = json!(thread); }
            }
            let (endpoint, request) = match &draft.provider_id {
                Some(id) => ("drafts/send", json!({"id": id.0, "message": body})),
                None => ("messages/send", body),
            };
            let response = match self.client.post(endpoint, &request) {
                Ok(v) => v,
                // A transport failure after the request left this machine is not a clean
                // failure: Gmail may have accepted the message.
                Err(ApiError::Transport(_)) => return Err(ProviderError::Uncertain),
                Err(e) => return Err(e.into()),
            };
            let id = response
                .get("id")
                .and_then(|i| i.as_str())
                .ok_or_else(|| ProviderError::Rejected("Gmail did not return a message id".into()))?
                .to_owned();
            let index = Self::label_index(&context.labels);
            let mut message = match self.fetch_message(&account, &id, &index) {
                Ok(Some(m)) => m,
                _ => {
                    // Accepted but not re-fetchable right now: build the Sent copy locally.
                    let mut m = crate::sample::sent_message(&account, &draft, context.in_reply_to.as_ref(), &ProviderId(id.clone()), &rfc_id, context.sent_label.as_ref(), overlook_core::time::now());
                    m.id = mapping::local_message_id(&account.id, &id);
                    if let Some(thread) = response.get("threadId").and_then(|t| t.as_str()) {
                        m.conversation_id = mapping::local_conversation_id(&account.id, thread);
                    }
                    m
                }
            };
            if let Some(sent) = &context.sent_label {
                message.label_ids.insert(sent.clone());
            }
            message.is_read = true;
            message.is_outgoing = true;
            Ok(SentReceipt { provider_id: ProviderId(id), rfc_message_id: Some(rfc_id), message })
        })
    }

    fn manages_folders(&self) -> bool {
        true
    }

    fn create_folder(&self, account: &Account, name: &str, parent: Option<&Label>, labels: &[Label]) -> BoxFuture<'_, Result<Label, ProviderError>> {
        let account = account.clone();
        let full = match parent {
            Some(p) => format!("{}/{}", p.name, name.trim()),
            None => name.trim().to_owned(),
        };
        let leaf_ok = folder_name_problem(name, "/");
        let order = labels.len() as i32;
        Box::pin(async move {
            if let Some(problem) = leaf_ok {
                return Err(ProviderError::Rejected(problem));
            }
            let v = with_inbox_spelling(&full, |name| self.client.post("labels", &json!({ "name": name, "labelListVisibility": "labelShow", "messageListVisibility": "show" })))?;
            label_from_json(&account.id, &v, order).ok_or_else(|| ProviderError::Temporary("Gmail did not return the new label".into()))
        })
    }

    fn rename_folder(&self, account: &Account, label: &Label, name: &str, labels: &[Label]) -> BoxFuture<'_, Result<Vec<RenamedFolder>, ProviderError>> {
        let account = account.clone();
        let label = label.clone();
        let problem = folder_name_problem(name, "/");
        // Gmail nests by "/" in the name: keep the parent part, replace the last.
        let full = match label.name.rsplit_once('/') {
            Some((parent, _)) => format!("{parent}/{}", name.trim()),
            None => name.trim().to_owned(),
        };
        let inside = labels_inside(&label, labels);
        let clash = name_taken(labels, &label.id, &full);
        Box::pin(async move {
            if let Some(problem) = problem {
                return Err(ProviderError::Rejected(problem));
            }
            own_label(&label, "renamed")?;
            if full == label.name {
                return Ok(Vec::new());
            }
            if clash {
                return Err(ProviderError::Rejected(format!("There is already a label called “{full}”.")));
            }
            self.rename_with_children(&account, &label, &full, inside)
        })
    }

    fn move_folder(&self, account: &Account, label: &Label, parent: Option<&Label>, labels: &[Label]) -> BoxFuture<'_, Result<Vec<RenamedFolder>, ProviderError>> {
        let account = account.clone();
        let label = label.clone();
        let leaf = label.name.rsplit('/').next().unwrap_or(&label.name).to_owned();
        let target = match parent {
            Some(p) if matches!(p.kind, LabelKind::User | LabelKind::Inbox) => Ok(format!("{}/{leaf}", p.name)),
            Some(_) => Err(ProviderError::Rejected("labels can go only inside your own labels or the Inbox".into())),
            None => Ok(leaf.clone()),
        };
        let inside = labels_inside(&label, labels);
        let clash = target.as_ref().ok().filter(|t| name_taken(labels, &label.id, t)).cloned();
        Box::pin(async move {
            let target = target?;
            own_label(&label, "moved")?;
            if target.eq_ignore_ascii_case(&label.name) {
                return Ok(Vec::new());
            }
            if target.to_ascii_lowercase().starts_with(&format!("{}/", label.name.to_ascii_lowercase())) {
                return Err(ProviderError::Rejected(format!("“{}” can’t go inside itself.", label.name)));
            }
            if let Some(name) = clash {
                return Err(ProviderError::Rejected(format!("There is already a label called “{name}”.")));
            }
            self.rename_with_children(&account, &label, &target, inside)
        })
    }

    fn delete_folder(&self, _account: &Account, label: &Label) -> BoxFuture<'_, Result<(), ProviderError>> {
        let label = label.clone();
        Box::pin(async move {
            let id = own_label(&label, "deleted")?;
            match self.client.delete(&format!("labels/{}", id.0)) {
                Ok(_) | Err(ApiError::NotFound) => Ok(()),
                Err(e) => Err(e.into()),
            }
        })
    }

    fn reconcile_send(&self, _account: &Account, draft: &Draft) -> BoxFuture<'_, Result<SendOutcome, ProviderError>> {
        let rfc_id = draft_message_id(draft);
        Box::pin(async move {
            let query = format!("rfc822msgid:{}", rfc_id.trim_matches(|c| c == '<' || c == '>'));
            let v = self.client.get("messages", &[("q", query.as_str()), ("maxResults", "1")])?;
            let found = v
                .get("messages")
                .and_then(|m| m.as_array())
                .and_then(|a| a.first())
                .and_then(|m| m.get("id"))
                .and_then(|i| i.as_str())
                .map(str::to_owned);
            Ok(match found {
                Some(id) => SendOutcome::Delivered(ProviderId(id)),
                None => SendOutcome::NotDelivered,
            })
        })
    }
}

pub const PEOPLE_BASE: &str = "https://people.googleapis.com/v1";
const PERSON_FIELDS: &str = "names,emailAddresses,phoneNumbers,organizations,biographies,photos,metadata";
const UPDATE_FIELDS: &str = "names,emailAddresses,phoneNumbers,organizations,biographies";

/// A People API person as a contact. `None` when it has neither a name nor an address.
pub fn contact_from_person(account: &AccountId, person: &Value, in_book: bool, now: Timestamp) -> Option<Contact> {
    let id = person.get("resourceName")?.as_str()?.to_owned();
    let name_obj = person.get("names").and_then(|n| n.as_array()).and_then(|a| a.first());
    let str_at = |obj: Option<&Value>, key: &str| obj.and_then(|o| o.get(key)).and_then(|v| v.as_str()).map(str::to_owned).filter(|s| !s.trim().is_empty());
    let name = str_at(name_obj, "displayName");
    let emails: Vec<String> = person
        .get("emailAddresses")
        .and_then(|e| e.as_array())
        .map(|arr| arr.iter().filter_map(|e| e.get("value").and_then(|v| v.as_str())).map(|v| v.trim().to_ascii_lowercase()).filter(|v| v.contains('@')).collect())
        .unwrap_or_default();
    if name.is_none() && emails.is_empty() {
        return None;
    }
    let org = person.get("organizations").and_then(|o| o.as_array()).and_then(|a| a.first());
    let photo = person
        .get("photos")
        .and_then(|p| p.as_array())
        .and_then(|a| a.iter().find(|p| p.get("default").and_then(|d| d.as_bool()) != Some(true)).or(a.first()))
        .and_then(|p| p.get("url"))
        .and_then(|u| u.as_str())
        .map(str::to_owned);
    Some(Contact {
        id,
        account_id: account.clone(),
        email: emails.first().cloned().unwrap_or_default(),
        emails,
        name,
        given_name: str_at(name_obj, "givenName"),
        family_name: str_at(name_obj, "familyName"),
        phone: person.get("phoneNumbers").and_then(|p| p.as_array()).and_then(|a| a.first()).and_then(|p| p.get("value")).and_then(|v| v.as_str()).map(str::to_owned),
        organization: str_at(org, "name"),
        title: str_at(org, "title"),
        notes: person.get("biographies").and_then(|b| b.as_array()).and_then(|a| a.first()).and_then(|b| b.get("value")).and_then(|v| v.as_str()).map(str::to_owned),
        photo_url: photo,
        in_address_book: in_book,
        etag: person.get("etag").and_then(|e| e.as_str()).map(str::to_owned),
        updated: now,
        pending: false,
    })
}

/// The People API body for creating or updating a contact.
pub fn person_json(c: &Contact, etag: Option<&str>) -> Value {
    let mut body = json!({});
    if let Some(e) = etag {
        body["etag"] = Value::String(e.to_owned());
    }
    let mut names = json!({});
    if let Some(g) = c.given_name.as_deref().filter(|s| !s.trim().is_empty()) {
        names["givenName"] = Value::String(g.trim().to_owned());
    }
    if let Some(f) = c.family_name.as_deref().filter(|s| !s.trim().is_empty()) {
        names["familyName"] = Value::String(f.trim().to_owned());
    }
    if names.as_object().is_some_and(|o| o.is_empty()) {
        if let Some(n) = c.name.as_deref().filter(|s| !s.trim().is_empty()) {
            let mut parts = n.trim().splitn(2, ' ');
            names["givenName"] = Value::String(parts.next().unwrap_or("").to_owned());
            if let Some(rest) = parts.next() {
                names["familyName"] = Value::String(rest.to_owned());
            }
        }
    }
    body["names"] = if names.as_object().is_some_and(|o| o.is_empty()) { json!([]) } else { json!([names]) };
    let emails: Vec<Value> = std::iter::once(c.email.clone()).chain(c.emails.iter().cloned()).filter(|e| !e.trim().is_empty()).fold(Vec::<String>::new(), |mut acc, e| { if !acc.iter().any(|x| x.eq_ignore_ascii_case(&e)) { acc.push(e); } acc }).into_iter().map(|e| json!({ "value": e })).collect();
    body["emailAddresses"] = Value::Array(emails);
    body["phoneNumbers"] = match c.phone.as_deref().filter(|p| !p.trim().is_empty()) {
        Some(p) => json!([{ "value": p.trim() }]),
        None => json!([]),
    };
    let mut org = json!({});
    if let Some(o) = c.organization.as_deref().filter(|s| !s.trim().is_empty()) {
        org["name"] = Value::String(o.trim().to_owned());
    }
    if let Some(t) = c.title.as_deref().filter(|s| !s.trim().is_empty()) {
        org["title"] = Value::String(t.trim().to_owned());
    }
    body["organizations"] = if org.as_object().is_some_and(|o| o.is_empty()) { json!([]) } else { json!([org]) };
    body["biographies"] = match c.notes.as_deref().filter(|n| !n.trim().is_empty()) {
        Some(n) => json!([{ "value": n.trim(), "contentType": "TEXT_PLAIN" }]),
        None => json!([]),
    };
    body
}

/// Gmail has no folders inside the Inbox, but a label named "Inbox/Name" is what IMAP
/// clients such as Outlook create there and show under the Inbox. "INBOX" is a reserved
/// label name, so try "Inbox/…" first and the IMAP spelling "INBOX/…" if Google refuses it.
fn with_inbox_spelling(full: &str, mut call: impl FnMut(&str) -> Result<serde_json::Value, ApiError>) -> Result<serde_json::Value, ApiError> {
    let Some(rest) = inside_inbox(full) else {
        return call(full);
    };
    match call(&format!("Inbox/{rest}")) {
        Err(ApiError::Client(400, _)) => call(&format!("INBOX/{rest}")),
        other => other,
    }
}
