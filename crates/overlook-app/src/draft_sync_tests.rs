use super::*;
use overlook_provider::{
    AppliedChanges, BoxFuture, MailboxSnapshot, RemoteChanges, SendContext, SendOutcome,
    SentReceipt,
};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct State {
    drafts: HashMap<ProviderId, (RemoteDraft, Vec<u8>)>,
    next: usize,
    writes: usize,
    lose_reply: bool,
    list_error: bool,
    after_write: Option<Box<dyn FnOnce() + Send>>,
}

struct Fake {
    kind: ProviderKind,
    state: Mutex<State>,
}

impl Fake {
    fn new(kind: ProviderKind) -> Self {
        Self {
            kind,
            state: Mutex::new(State::default()),
        }
    }
    fn external(&self, previous: Option<&RemoteDraft>, body: &str) -> RemoteDraft {
        let mut s = self.state.lock().unwrap();
        s.next += 1;
        let id = previous
            .filter(|_| self.kind == ProviderKind::Gmail)
            .map(|r| r.id.clone())
            .unwrap_or_else(|| ProviderId(format!("draft-{}", s.next)));
        if let Some(old) = previous {
            s.drafts.remove(&old.id);
        }
        let remote = RemoteDraft {
            id: id.clone(),
            revision: format!("message-{}", s.next),
            thread_id: Some("thread-1".into()),
        };
        let raw = format!("From: me@example.com\r\nTo: friend@example.com\r\nBcc: hidden@example.com\r\nSubject: Remote\r\nIn-Reply-To: <parent@example.com>\r\nReferences: <root@example.com> <parent@example.com>\r\nContent-Type: text/html; charset=utf-8\r\n\r\n<p><b>{body}</b></p>").into_bytes();
        s.drafts.insert(id, (remote.clone(), raw));
        remote
    }
}

impl MailProvider for Fake {
    fn kind(&self) -> ProviderKind {
        self.kind
    }
    fn supports_drafts(&self) -> bool {
        true
    }
    fn probe(&self) -> BoxFuture<'_, Result<(), ProviderError>> {
        Box::pin(async { Ok(()) })
    }
    fn fetch_snapshot(&self, _: &Account) -> BoxFuture<'_, Result<MailboxSnapshot, ProviderError>> {
        Box::pin(async { Ok(MailboxSnapshot::default()) })
    }
    fn fetch_changes(
        &self,
        _: &Account,
        _: &str,
    ) -> BoxFuture<'_, Result<RemoteChanges, ProviderError>> {
        Box::pin(async { Ok(RemoteChanges::default()) })
    }
    fn apply(
        &self,
        _: &Account,
        _: &PendingOperation,
        _: &[Message],
        _: &[Label],
    ) -> BoxFuture<'_, Result<AppliedChanges, ProviderError>> {
        Box::pin(async { Ok(AppliedChanges::default()) })
    }
    fn send(
        &self,
        _: &Account,
        _: &Draft,
        _: SendContext,
    ) -> BoxFuture<'_, Result<SentReceipt, ProviderError>> {
        Box::pin(async { Err(ProviderError::Offline) })
    }
    fn reconcile_send(
        &self,
        _: &Account,
        _: &Draft,
    ) -> BoxFuture<'_, Result<SendOutcome, ProviderError>> {
        Box::pin(async { Ok(SendOutcome::NotDelivered) })
    }
    fn list_drafts(&self, _: &Account) -> BoxFuture<'_, Result<Vec<RemoteDraft>, ProviderError>> {
        Box::pin(async {
            let state = self.state.lock().unwrap();
            if state.list_error {
                return Err(ProviderError::Offline);
            }
            Ok(state.drafts.values().map(|(r, _)| r.clone()).collect())
        })
    }
    fn fetch_draft(
        &self,
        _: &Account,
        remote: &RemoteDraft,
    ) -> BoxFuture<'_, Result<Option<Vec<u8>>, ProviderError>> {
        let remote = remote.clone();
        Box::pin(async move {
            Ok(self
                .state
                .lock()
                .unwrap()
                .drafts
                .get(&remote.id)
                .filter(|(r, _)| r.revision == remote.revision)
                .map(|(_, raw)| raw.clone()))
        })
    }
    fn delete_remote_draft(
        &self,
        _: &Account,
        remote: &RemoteDraft,
    ) -> BoxFuture<'_, Result<(), ProviderError>> {
        let id = remote.id.clone();
        Box::pin(async move {
            self.state.lock().unwrap().drafts.remove(&id);
            Ok(())
        })
    }
    fn put_draft(
        &self,
        _: &Account,
        remote: Option<&RemoteDraft>,
        raw: &[u8],
        message_id: &str,
        _: Option<&str>,
    ) -> BoxFuture<'_, Result<Option<RemoteDraft>, ProviderError>> {
        let (remote, raw, message_id) = (remote.cloned(), raw.to_vec(), message_id.to_owned());
        Box::pin(async move {
            let (result, lose_reply, hook) = {
                let mut s = self.state.lock().unwrap();
                if let Some((r, _)) = s
                    .drafts
                    .values()
                    .find(|(_, bytes)| mail_id(bytes).as_deref() == Some(message_id.as_str()))
                {
                    return Ok(Some(r.clone()));
                }
                if let Some(old) = &remote {
                    if s.drafts
                        .get(&old.id)
                        .is_none_or(|(r, _)| r.revision != old.revision)
                    {
                        return Ok(None);
                    }
                }
                s.next += 1;
                s.writes += 1;
                let id = remote
                    .as_ref()
                    .filter(|_| self.kind == ProviderKind::Gmail)
                    .map(|r| r.id.clone())
                    .unwrap_or_else(|| ProviderId(format!("draft-{}", s.next)));
                if let Some(old) = remote {
                    s.drafts.remove(&old.id);
                }
                let result = RemoteDraft {
                    id: id.clone(),
                    revision: format!("message-{}", s.next),
                    thread_id: Some("thread-1".into()),
                };
                s.drafts.insert(id, (result.clone(), raw));
                let lose_reply = std::mem::take(&mut s.lose_reply);
                (result, lose_reply, s.after_write.take())
            };
            if let Some(hook) = hook {
                hook();
            }
            if lose_reply {
                Err(ProviderError::Offline)
            } else {
                Ok(Some(result))
            }
        })
    }
}

fn mail_id(bytes: &[u8]) -> Option<String> {
    // These tests compare the persisted upload token without exposing MIME internals.
    String::from_utf8_lossy(bytes).lines().find_map(|line| {
        line.strip_prefix("Message-ID: ")
            .map(|s| s.trim().to_owned())
    })
}

fn block<F: std::future::Future>(future: F) -> F::Output {
    let waker = std::task::Waker::noop();
    let mut cx = std::task::Context::from_waker(waker);
    let mut future = std::pin::pin!(future);
    match future.as_mut().poll(&mut cx) {
        std::task::Poll::Ready(result) => result,
        _ => panic!("test providers must complete synchronously"),
    }
}

fn account(kind: ProviderKind) -> Account {
    Account {
        id: AccountId::from("test-drafts"),
        provider: kind,
        display_name: "Test".into(),
        email: "me@example.com".into(),
        created_at: 0,
    }
}

fn sync(store: &Store, fake: &Fake, account: &Account, dir: &Path) -> Result<usize, ProviderError> {
    block(synchronize(store, fake, account, dir))
}

#[test]
fn local_drafts_upload_update_and_delete_for_both_providers() {
    for kind in [ProviderKind::Gmail, ProviderKind::Imap] {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in_memory().unwrap();
        let account = account(kind);
        store.upsert_account(&account).unwrap();
        let fake = Fake::new(kind);
        let mut draft = Draft::new(account.id.clone(), DraftKind::New, 1);
        draft.subject = "No recipient yet".into();
        draft.bcc = vec![Address::new(None, "hidden@example.com")];
        draft.body_text = "**First version**".into();
        store.save_draft(&draft).unwrap();
        sync(&store, &fake, &account, dir.path()).unwrap();
        let remote = store.draft_sync_entries(&account.id).unwrap()[0]
            .remote
            .clone()
            .unwrap();
        sync(&store, &fake, &account, dir.path()).unwrap();
        assert_eq!(
            fake.state.lock().unwrap().writes,
            1,
            "own upload must not echo back or upload twice"
        );
        draft.body_text = "Second version".into();
        store.save_draft(&draft).unwrap();
        sync(&store, &fake, &account, dir.path()).unwrap();
        let state = fake.state.lock().unwrap();
        assert_eq!(state.drafts.len(), 1);
        let saved = &state.drafts.values().next().unwrap().0;
        assert_ne!(saved.revision, remote.revision);
        assert_eq!(saved.id == remote.id, kind == ProviderKind::Gmail);
        drop(state);
        store.delete_draft(&account.id, &draft.id).unwrap();
        sync(&store, &fake, &account, dir.path()).unwrap();
        assert!(fake.state.lock().unwrap().drafts.is_empty());
        assert!(store.draft_sync_entries(&account.id).unwrap().is_empty());
    }
}

#[test]
fn remote_edits_and_deletions_preserve_offline_changes_for_both_providers() {
    for kind in [ProviderKind::Gmail, ProviderKind::Imap] {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in_memory().unwrap();
        let account = account(kind);
        store.upsert_account(&account).unwrap();
        let fake = Fake::new(kind);
        let remote = fake.external(None, "First");
        sync(&store, &fake, &account, dir.path()).unwrap();
        let mut local = store.drafts(&account.id, false).unwrap().pop().unwrap();
        assert_eq!(local.bcc[0].email, "hidden@example.com");
        assert!(local
            .imported
            .as_ref()
            .unwrap()
            .html
            .as_ref()
            .unwrap()
            .contains("<b>First</b>"));
        local.body_text = "My offline change".into();
        store.save_draft(&local).unwrap();
        fake.external(Some(&remote), "Other device");
        assert_eq!(sync(&store, &fake, &account, dir.path()).unwrap(), 1);
        let drafts = store.drafts(&account.id, false).unwrap();
        assert_eq!(drafts.len(), 2);
        assert!(drafts.iter().any(|d| d.body_text == "My offline change"));
        assert!(drafts.iter().any(|d| d.body_text.contains("Other device")));
        sync(&store, &fake, &account, dir.path()).unwrap();
        assert_eq!(fake.state.lock().unwrap().drafts.len(), 2);
        fake.state.lock().unwrap().drafts.clear();
        sync(&store, &fake, &account, dir.path()).unwrap();
        assert!(
            store.drafts(&account.id, false).unwrap().is_empty(),
            "unchanged server deletions propagate"
        );
    }
}

#[test]
fn open_editor_is_not_replaced_and_failed_listing_is_not_a_deletion() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open_in_memory().unwrap());
    let account = account(ProviderKind::Gmail);
    store.upsert_account(&account).unwrap();
    let fake = Fake::new(account.provider);
    let remote = fake.external(None, "First");
    sync(&store, &fake, &account, dir.path()).unwrap();
    let local = store.drafts(&account.id, false).unwrap().pop().unwrap();
    let first_guard = store.edit_draft(&local.id);
    let guard = store.edit_draft(&local.id);
    drop(first_guard);
    fake.external(Some(&remote), "Changed");
    sync(&store, &fake, &account, dir.path()).unwrap();
    assert_eq!(
        store.draft(&local.id).unwrap().unwrap().body_text,
        local.body_text
    );
    fake.state.lock().unwrap().list_error = true;
    assert!(sync(&store, &fake, &account, dir.path()).is_err());
    assert!(store.draft(&local.id).unwrap().is_some());
    drop(guard);
    fake.state.lock().unwrap().list_error = false;
    sync(&store, &fake, &account, dir.path()).unwrap();
    assert!(store
        .draft(&local.id)
        .unwrap()
        .unwrap()
        .body_text
        .contains("Changed"));
}

#[test]
fn lost_create_reply_survives_restart_and_discard_without_resurrection() {
    for kind in [ProviderKind::Gmail, ProviderKind::Imap] {
        let dir = tempfile::tempdir().unwrap();
        let database = dir.path().join("profile.sqlite");
        let store = Arc::new(Store::open(&database).unwrap());
        let account = account(kind);
        store.upsert_account(&account).unwrap();
        let fake = Fake::new(kind);
        let mut draft = Draft::new(account.id.clone(), DraftKind::New, 1);
        draft.body_text = "Keep exactly these bytes".into();
        store.save_draft(&draft).unwrap();
        {
            let mut state = fake.state.lock().unwrap();
            state.lose_reply = true;
            let (store, account, id) = (store.clone(), account.id.clone(), draft.id.clone());
            state.after_write = Some(Box::new(move || store.delete_draft(&account, &id).unwrap()));
        }
        assert!(sync(&store, &fake, &account, dir.path()).is_err());
        assert!(store.draft(&draft.id).unwrap().is_none());
        assert!(store.draft_sync_entries(&account.id).unwrap()[0]
            .pending
            .is_some());
        drop(store);
        let store = Store::open(&database).unwrap();
        sync(&store, &fake, &account, dir.path()).unwrap();
        assert!(store.drafts(&account.id, false).unwrap().is_empty());
        assert!(fake.state.lock().unwrap().drafts.is_empty());
        assert_eq!(fake.state.lock().unwrap().writes, 1);
    }
}

#[test]
fn upload_acknowledgment_does_not_replace_newer_autosave_and_send_cleanup_is_durable() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open_in_memory().unwrap());
    let account = account(ProviderKind::Gmail);
    store.upsert_account(&account).unwrap();
    let fake = Fake::new(account.provider);
    let mut draft = Draft::new(account.id.clone(), DraftKind::New, 1);
    draft.to.push(Address::new(None, "friend@example.com"));
    draft.body_text = "First".into();
    store.save_draft(&draft).unwrap();
    let mut newer = draft.clone();
    newer.body_text = "Typed during upload".into();
    let (write_store, write_draft) = (store.clone(), newer.clone());
    fake.state.lock().unwrap().after_write = Some(Box::new(move || {
        write_store.save_draft(&write_draft).unwrap()
    }));
    sync(&store, &fake, &account, dir.path()).unwrap();
    assert_eq!(
        store.draft(&draft.id).unwrap().unwrap().body_text,
        newer.body_text
    );
    sync(&store, &fake, &account, dir.path()).unwrap();
    assert_eq!(fake.state.lock().unwrap().writes, 2);
    let op = store.queue_send(&newer).unwrap();
    sync(&store, &fake, &account, dir.path()).unwrap();
    assert_eq!(
        fake.state.lock().unwrap().writes,
        2,
        "queued drafts are never re-uploaded"
    );
    let sent = overlook_provider::sample::sent_message(
        &account,
        &newer,
        None,
        &ProviderId("sent-1".into()),
        "<sent@example.com>",
        None,
        2,
    );
    store.complete_send(&newer.id, &op.id, &sent).unwrap();
    block(flush_deletions(&store, &fake, &account)).unwrap();
    assert!(fake.state.lock().unwrap().drafts.is_empty());
}

#[test]
fn imported_draft_attachments_and_html_survive_restart_and_conflicts() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("profile.sqlite");
    let store = Store::open(&database).unwrap();
    let account = account(ProviderKind::Gmail);
    store.upsert_account(&account).unwrap();
    let fake = Fake::new(account.provider);
    let remote = fake.external(None, "First");
    let raw = b"From: me@example.com\r\nSubject: Files\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=b\r\n\r\n--b\r\nContent-Type: text/html\r\n\r\n<p>Original <b>formatting</b><img src=\"cid:logo\"></p>\r\n--b\r\nContent-Type: image/png; name=logo.png\r\nContent-Disposition: inline; filename=logo.png\r\nContent-ID: <logo>\r\nContent-Transfer-Encoding: base64\r\n\r\niVBORw0KGgo=\r\n--b\r\nContent-Type: application/pdf; name=../../report.pdf\r\nContent-Disposition: attachment; filename=\"../../report.pdf\"\r\nContent-Transfer-Encoding: base64\r\n\r\nJVBERi0xLjQK\r\n--b--\r\n";
    fake.state
        .lock()
        .unwrap()
        .drafts
        .get_mut(&remote.id)
        .unwrap()
        .1 = raw.to_vec();
    sync(&store, &fake, &account, dir.path()).unwrap();
    drop(store);
    let store = Store::open(&database).unwrap();
    let mut local = store.drafts(&account.id, false).unwrap().pop().unwrap();
    assert_eq!(local.attachments.len(), 2);
    assert_eq!(local.attachments[0].content_id.as_deref(), Some("logo"));
    assert!(local
        .attachments
        .iter()
        .all(|a| Path::new(&a.local_path).starts_with(dir.path())));
    let original_paths: Vec<_> = local
        .attachments
        .iter()
        .map(|a| a.local_path.clone())
        .collect();
    let roundtrip = overlook_provider::outgoing::build_draft_raw(
        &account,
        &local,
        None,
        "<roundtrip@example.com>",
    )
    .unwrap();
    let imported = overlook_provider::drafts::import(&account, &remote, &roundtrip).unwrap();
    assert_eq!(imported.draft.attachments.len(), 2);
    assert_eq!(imported.files[0], b"\x89PNG\r\n\x1a\n");
    assert_eq!(imported.files[1], b"%PDF-1.4\n");
    assert!(imported
        .draft
        .imported
        .unwrap()
        .html
        .unwrap()
        .contains("<b>formatting</b>"));
    local.body_text = "Offline edits".into();
    store.save_draft(&local).unwrap();
    fake.external(Some(&remote), "Other device");
    assert_eq!(sync(&store, &fake, &account, dir.path()).unwrap(), 1);
    let copy = store
        .drafts(&account.id, false)
        .unwrap()
        .into_iter()
        .find(|d| d.body_text == "Offline edits")
        .unwrap();
    assert!(copy
        .attachments
        .iter()
        .all(|a| !original_paths.contains(&a.local_path)));
    for (a, old) in copy.attachments.iter().zip(original_paths) {
        assert_eq!(
            std::fs::read(&a.local_path).unwrap(),
            std::fs::read(old).unwrap()
        );
    }
}

#[test]
fn discarded_and_queued_drafts_do_not_destroy_changes_from_another_device() {
    for kind in [ProviderKind::Gmail, ProviderKind::Imap] {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in_memory().unwrap();
        let account = account(kind);
        store.upsert_account(&account).unwrap();
        let fake = Fake::new(kind);
        let first = fake.external(None, "First");
        sync(&store, &fake, &account, dir.path()).unwrap();
        let local = store.drafts(&account.id, false).unwrap().pop().unwrap();
        store.delete_draft(&account.id, &local.id).unwrap();
        let changed = fake.external(Some(&first), "Changed while discarded offline");
        sync(&store, &fake, &account, dir.path()).unwrap();
        assert_eq!(fake.state.lock().unwrap().drafts.len(), 1);
        let preserved = store.drafts(&account.id, false).unwrap().pop().unwrap();
        assert!(preserved
            .body_text
            .contains("Changed while discarded offline"));
        store.queue_send(&preserved).unwrap();
        fake.external(Some(&changed), "Changed while queued offline");
        sync(&store, &fake, &account, dir.path()).unwrap();
        let paused = store.drafts(&account.id, true).unwrap().pop().unwrap();
        assert!(matches!(paused.state, DraftState::Failed(_)));
        assert_eq!(store.pending_count(&account.id).unwrap(), 0);
        assert!(store
            .drafts(&account.id, false)
            .unwrap()
            .iter()
            .any(|d| d.body_text.contains("Changed while queued offline")));
        store.delete_draft(&account.id, &paused.id).unwrap();
        block(flush_deletions(&store, &fake, &account)).unwrap();
        assert_eq!(
            fake.state.lock().unwrap().drafts.len(),
            1,
            "discarding the paused local send must not delete the preserved server version"
        );
    }
}

#[test]
fn a_deleted_drafts_sync_identity_cannot_be_claimed_by_another_account() {
    let store = Store::open_in_memory().unwrap();
    let account = account(ProviderKind::Gmail);
    store.upsert_account(&account).unwrap();
    let mut other = account.clone();
    other.id = AccountId::from("other-account");
    store.upsert_account(&other).unwrap();
    let draft = Draft::new(account.id.clone(), DraftKind::New, 0);
    store.save_draft(&draft).unwrap();
    store.delete_draft(&account.id, &draft.id).unwrap();
    let mut stolen = draft.clone();
    stolen.account_id = other.id.clone();
    assert!(matches!(
        store.save_draft(&stolen),
        Err(overlook_store::StoreError::NotOwned("draft"))
    ));
    assert!(store.draft_sync_entries(&other.id).unwrap().is_empty());
    assert_eq!(store.draft_sync_entries(&account.id).unwrap().len(), 1);
}
