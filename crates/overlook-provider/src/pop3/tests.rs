//! `Pop3Provider` against the scripted fake server, plus an ignored round trip against a
//! real server configured through `OVERLOOK_POP3_TEST_*`.

use super::fake::{serve, FakeMessage, FakeServer, FakeState, Login};
use super::*;
use crate::smtp::fake::{serve_many, Behaviour};
use crate::ProviderError;
use std::time::Duration;

fn account() -> Account {
    Account { id: AccountId::from("pop3:jason@example.com"), provider: ProviderKind::Pop3, display_name: "Jason".into(), email: "jason@example.com".into(), created_at: 0 }
}

fn msg(headers: &[&str], body: &str) -> Vec<u8> {
    format!("{}\r\n\r\n{}", headers.join("\r\n"), body).into_bytes()
}

const MULTIPART_BODY: &str = "--outer\r\nContent-Type: multipart/alternative; boundary=\"inner\"\r\n\r\n--inner\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nText part.\r\n--inner\r\nContent-Type: text/html; charset=utf-8\r\n\r\n<p>HTML part with <img src=\"cid:logo\"></p>\r\n--inner--\r\n--outer\r\nContent-Type: image/png; name=\"logo.png\"\r\nContent-ID: <logo>\r\nContent-Disposition: inline; filename=\"logo.png\"\r\nContent-Transfer-Encoding: base64\r\n\r\niVBORw0KGgo=\r\n--outer\r\nContent-Type: application/pdf; name=\"report.pdf\"\r\nContent-Disposition: attachment; filename=\"report.pdf\"\r\nContent-Transfer-Encoding: base64\r\n\r\nJVBERi0xLjQK\r\n--outer--\r\n";

fn maildrop() -> FakeState {
    let mut state = FakeState::default();
    state.add(FakeMessage::new("UIDL-1", msg(&["From: Ada Lovelace <ada@example.com>", "To: jason@example.com", "Subject: Plain hello", "Date: Sat, 10 Oct 2026 09:12:00 +0000", "Message-ID: <plain-1@example.com>", "Content-Type: text/plain; charset=utf-8"], "Hello from a plain message.\r\n.Leading dot line\r\n")));
    state.add(FakeMessage::new(
        "UIDL-2",
        msg(
            &["From: =?UTF-8?Q?Gr=C3=A9goire?= <greg@example.com>", "To: jason@example.com", "Subject: =?UTF-8?B?UmU6IFBsYWluIGhlbGxv?=", "Date: Mon, 12 Oct 2026 14:30:00 +0000", "Message-ID: <multi-2@example.com>", "In-Reply-To: <plain-1@example.com>", "References: <plain-1@example.com>", "MIME-Version: 1.0", "Content-Type: multipart/mixed; boundary=\"outer\""],
            MULTIPART_BODY,
        ),
    ));
    let mut big = FakeMessage::new("UIDL-3", msg(&["From: bot@example.com", "To: jason@example.com", "Subject: Big one", "Message-ID: <big-3@example.com>", "Date: Tue, 13 Oct 2026 08:00:00 +0000"], "A large body.\r\n"));
    big.size_override = Some(5_000_000);
    state.add(big);
    state
}

struct Env {
    server: FakeServer,
    raw_dir: std::path::PathBuf,
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.raw_dir);
    }
}

fn make_env(name: &str, state: FakeState) -> Env {
    let raw_dir = std::env::temp_dir().join(format!("overlook-pop3-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&raw_dir);
    Env { server: serve(state), raw_dir }
}

fn servers(env: &Env, smtp_port: u16, policy: Option<Pop3Policy>) -> MailServers {
    MailServers {
        incoming: ServerEndpoint::new("127.0.0.1", env.server.addr.port(), Security::None, "jason@example.com"),
        outgoing: ServerEndpoint::new("127.0.0.1", smtp_port, Security::None, "jason@example.com"),
        pop3: policy,
        imap: None,
        oauth: None,
    }
}

fn provider(env: &Env, policy: Option<Pop3Policy>) -> Pop3Provider {
    Pop3Provider::new(servers(env, 1, policy), "hunter2", env.raw_dir.clone()).with_timeout(Duration::from_secs(5))
}

fn block<T>(f: BoxFuture<'_, T>) -> T {
    use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};
    fn noop(_: *const ()) {}
    fn clone(p: *const ()) -> RawWaker {
        RawWaker::new(p, &VTABLE)
    }
    static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, noop, noop, noop);
    let waker = unsafe { Waker::from_raw(RawWaker::new(std::ptr::null(), &VTABLE)) };
    let mut cx = Context::from_waker(&waker);
    let mut f = f;
    loop {
        if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
    }
}

fn pending(acct: &Account, kind: OperationKind) -> PendingOperation {
    PendingOperation { id: OperationId::from("op"), account_id: acct.id.clone(), seq: 1, kind, state: OperationState::Pending, attempts: 0, last_error: None, created_at: 0, updated_at: 0 }
}

fn pids(ids: &[&str]) -> Vec<ProviderId> {
    ids.iter().map(|s| ProviderId((*s).to_owned())).collect()
}

#[test]
fn snapshot_lists_newest_first_tops_large_messages_and_retr_saves_eml_files() {
    let env = make_env("snapshot", maildrop());
    let p = provider(&env, None);
    let acct = account();
    block(p.probe()).unwrap();
    assert_eq!(env.server.lock().quits, 1, "a probe signs in and leaves");

    let snap = block(p.fetch_snapshot(&acct)).unwrap();
    assert_eq!(snap.labels.iter().map(|l| (l.kind, l.provider_id.is_none())).collect::<Vec<_>>(), vec![(LabelKind::Inbox, true), (LabelKind::Sent, true), (LabelKind::Archive, true), (LabelKind::Trash, true)]);
    assert_eq!(snap.message_ids, pids(&["UIDL-3", "UIDL-2", "UIDL-1"]), "newest first");
    assert_eq!(snap.messages.len(), 1, "only the large message has a header row");
    let header = &snap.messages[0];
    assert_eq!((header.subject.as_str(), header.provider_id.as_ref().unwrap().as_str(), header.body.is_cached(), header.is_read), ("Big one", "UIDL-3", false, false));
    assert_eq!(header.label_ids.iter().map(|l| l.as_str()).collect::<Vec<_>>(), vec!["pop3:jason@example.com:inbox"]);
    let cp = Checkpoint::parse(snap.checkpoint.as_deref().unwrap()).unwrap();
    assert_eq!(cp.entries.len(), 3);
    assert_eq!(cp.entries["UIDL-3"].id, header.id.0, "the header row and the ledger share the minted id");
    assert_eq!(cp.entries["UIDL-3"].size, 5_000_000);
    {
        let s = env.server.lock();
        assert_eq!((s.commands("UIDL"), s.commands("LIST"), s.commands("TOP 3 0"), s.commands("RETR")), (1, 1, 1, 0));
        assert_eq!(s.quits, 2);
    }

    let messages = block(p.fetch_messages(&acct, &snap.message_ids, &snap.labels)).unwrap();
    assert_eq!(messages.iter().map(|m| m.subject.as_str()).collect::<Vec<_>>(), vec!["Big one", "Re: Plain hello", "Plain hello"]);
    assert_eq!(messages[0].id, header.id, "the body download carries the header row's id");
    assert!(messages[0].body.is_cached());
    assert_eq!(messages[2].body.text.as_deref(), Some("Hello from a plain message.\r\n.Leading dot line\r\n"), "byte stuffing undone");
    assert_eq!(messages[1].attachments.len(), 2);
    assert_eq!(messages[1].in_reply_to.as_deref(), Some("<plain-1@example.com>"));
    for m in &messages {
        assert!(p.raw_path(&m.id).exists(), "{} saved", m.subject);
    }
    assert_eq!(std::fs::read(p.raw_path(&messages[2].id)).unwrap(), env.server.lock().messages[0].raw);
    {
        let s = env.server.lock();
        assert_eq!(s.commands("RETR"), 3);
        assert_eq!(s.quits, 3);
    }

    // Attachments and the source come from the saved file, not the server.
    let pdf = messages[1].attachments.iter().find(|a| a.filename == "report.pdf").unwrap();
    assert_eq!(block(p.fetch_attachment(&acct, &messages[1], pdf)).unwrap(), b"%PDF-1.4\n");
    assert_eq!(block(p.fetch_raw(&acct, &messages[2])).unwrap(), env.server.lock().messages[0].raw);
    assert_eq!(env.server.lock().connections, 3, "no connection for attachments or source");

    // A second download of the same ids is served from disk.
    let again = block(p.fetch_messages(&acct, &snap.message_ids, &snap.labels)).unwrap();
    assert_eq!(again.iter().map(|m| m.id.clone()).collect::<Vec<_>>(), messages.iter().map(|m| m.id.clone()).collect::<Vec<_>>());
    assert_eq!(env.server.lock().connections, 3);
}

#[test]
fn repeated_polls_never_list_a_message_twice_and_new_mail_appears() {
    let env = make_env("polls", maildrop());
    let p = provider(&env, None);
    let acct = account();
    let snap = block(p.fetch_snapshot(&acct)).unwrap();
    let cp = snap.checkpoint.unwrap();
    block(p.fetch_messages(&acct, &snap.message_ids, &[])).unwrap();

    let quiet = block(p.fetch_changes(&acct, &cp)).unwrap();
    assert!(quiet.message_ids.is_empty() && quiet.upserts.is_empty() && quiet.deleted.is_empty() && quiet.labels.is_none());
    assert_eq!(quiet.checkpoint.as_deref(), Some(cp.as_str()), "nothing changed, so the checkpoint is the same");
    assert_eq!(env.server.lock().commands("LIST"), 1, "LIST is skipped when nothing is new");

    // New mail, and one message removed by another client.
    {
        let mut s = env.server.lock();
        s.messages.remove(0);
        s.add(FakeMessage::new("UIDL-4", msg(&["From: new@example.com", "To: jason@example.com", "Subject: Fourth", "Message-ID: <fourth@example.com>", "Date: Wed, 14 Oct 2026 10:00:00 +0000"], "New.\r\n")));
    }
    let changes = block(p.fetch_changes(&acct, &cp)).unwrap();
    assert_eq!(changes.message_ids, pids(&["UIDL-4"]));
    assert!(changes.deleted.is_empty() && changes.deleted_provider_ids.is_empty(), "mail removed from the server stays local");
    let cp2 = Checkpoint::parse(changes.checkpoint.as_deref().unwrap()).unwrap();
    assert!(cp2.entries["UIDL-1"].deleted_on_server && !cp2.entries["UIDL-4"].deleted_on_server);
    assert_eq!(cp2.entries.len(), 4);
    let rows = seen_rows(changes.checkpoint.as_deref().unwrap());
    assert_eq!(rows.iter().filter(|r| r.deleted_on_server).map(|r| r.uidl.as_str()).collect::<Vec<_>>(), vec!["UIDL-1"]);

    // The message that was removed comes back (restored from a backup): still not re-listed.
    env.server.lock().messages.insert(0, maildrop().messages.remove(0));
    let back = block(p.fetch_changes(&acct, changes.checkpoint.as_deref().unwrap())).unwrap();
    assert!(back.message_ids.is_empty());
    assert!(!Checkpoint::parse(back.checkpoint.as_deref().unwrap()).unwrap().entries["UIDL-1"].deleted_on_server);

    // A fresh provider (restart) parses the stored checkpoint and agrees.
    let restarted = provider(&env, None);
    let after = block(restarted.fetch_changes(&acct, back.checkpoint.as_deref().unwrap())).unwrap();
    assert!(after.message_ids.is_empty());
    assert_eq!(env.server.lock().commands("RETR"), 3);
}

#[test]
fn retention_deletes_downloaded_messages_only_after_the_bytes_are_on_disk() {
    let env = make_env("retention", maildrop());
    let delete_now = Some(Pop3Policy { leave_on_server: false, ..Default::default() });
    let p = provider(&env, delete_now.clone());
    let acct = account();
    let snap = block(p.fetch_snapshot(&acct)).unwrap();
    assert!(env.server.lock().deleted.is_empty(), "nothing is deleted before it is downloaded");
    // Download two of the three; the third stays in the backlog.
    block(p.fetch_messages(&acct, &snap.message_ids[..2], &[])).unwrap();

    let changes = block(p.fetch_changes(&acct, snap.checkpoint.as_deref().unwrap())).unwrap();
    {
        let s = env.server.lock();
        let mut deleted = s.deleted.clone();
        deleted.sort();
        assert_eq!(deleted, vec!["UIDL-2", "UIDL-3"], "DELE for the downloaded ones, committed by QUIT");
        assert_eq!(s.uidls(), vec!["UIDL-1"]);
        let dele = s.log.iter().position(|l| l.starts_with("DELE")).unwrap();
        let quit = s.log.iter().rposition(|l| l == "QUIT").unwrap();
        assert!(dele < quit);
    }
    let cp = Checkpoint::parse(changes.checkpoint.as_deref().unwrap()).unwrap();
    assert!(cp.entries["UIDL-2"].deleted_on_server && cp.entries["UIDL-3"].deleted_on_server && !cp.entries["UIDL-1"].deleted_on_server);
    assert!(changes.message_ids.is_empty(), "the deleted messages are not new");

    // The backlog still downloads the third; the next cycle deletes it too.
    let rest = block(p.fetch_messages(&acct, &snap.message_ids[2..], &[])).unwrap();
    assert_eq!(rest[0].subject, "Plain hello");
    block(p.fetch_changes(&acct, changes.checkpoint.as_deref().unwrap())).unwrap();
    assert!(env.server.lock().uidls().is_empty());

    // delete_after_days: an entry seen long ago goes, a recent one stays.
    let env2 = make_env("retention-days", maildrop());
    let p2 = provider(&env2, Some(Pop3Policy { delete_after_days: Some(30), ..Default::default() }));
    let snap = block(p2.fetch_snapshot(&acct)).unwrap();
    block(p2.fetch_messages(&acct, &snap.message_ids, &[])).unwrap();
    let mut cp = Checkpoint::parse(snap.checkpoint.as_deref().unwrap()).unwrap();
    cp.entries.get_mut("UIDL-1").unwrap().seen_at -= 31 * 86_400_000;
    cp.entries.get_mut("UIDL-2").unwrap().seen_at -= 29 * 86_400_000;
    block(p2.fetch_changes(&acct, &cp.to_json())).unwrap();
    assert_eq!(env2.server.lock().deleted, vec!["UIDL-1"]);

    // Default policy: leave on server forever.
    let env3 = make_env("retention-keep", maildrop());
    let p3 = provider(&env3, None);
    let snap = block(p3.fetch_snapshot(&acct)).unwrap();
    block(p3.fetch_messages(&acct, &snap.message_ids, &[])).unwrap();
    block(p3.fetch_changes(&acct, snap.checkpoint.as_deref().unwrap())).unwrap();
    let s = env3.server.lock();
    assert!(s.deleted.is_empty() && s.commands("DELE") == 0);
}

#[test]
fn trash_deletes_on_the_server_only_when_the_policy_says_so() {
    let env = make_env("trash", maildrop());
    let acct = account();
    let p = provider(&env, Some(Pop3Policy { delete_on_server_when_trashed: true, ..Default::default() }));
    let snap = block(p.fetch_snapshot(&acct)).unwrap();
    let messages = block(p.fetch_messages(&acct, &snap.message_ids, &[])).unwrap();
    let plain = messages.iter().find(|m| m.subject == "Plain hello").unwrap().clone();
    let op = pending(&acct, OperationKind::Trash { message_ids: vec![plain.id.clone()] });
    let labels = labels_for(&acct.id);
    assert_eq!(block(p.apply(&acct, &op, &[plain.clone()], &labels)).unwrap(), AppliedChanges::default());
    assert_eq!(env.server.lock().deleted, vec!["UIDL-1"]);
    // Replaying the same operation is harmless: the message is already gone.
    assert!(block(p.apply(&acct, &op, &[plain.clone()], &labels)).is_ok());
    // Every other mailbox operation is local only.
    let star = pending(&acct, OperationKind::SetStarred { message_ids: vec![plain.id.clone()], starred: true });
    let before = env.server.lock().connections;
    assert!(block(p.apply(&acct, &star, &[plain.clone()], &labels)).is_ok());
    let archive = pending(&acct, OperationKind::Archive { message_ids: vec![plain.id.clone()] });
    assert!(block(p.apply(&acct, &archive, &[plain.clone()], &labels)).is_ok());
    assert_eq!(env.server.lock().connections, before, "no connection for local operations");
    let event = pending(&acct, OperationKind::RespondToEvent { event_id: EventId::from("e"), response: ResponseStatus::Accepted });
    assert!(matches!(block(p.apply(&acct, &event, &[], &labels)), Err(ProviderError::Rejected(_))));

    let env2 = make_env("trash-keep", maildrop());
    let p2 = provider(&env2, None);
    let snap = block(p2.fetch_snapshot(&acct)).unwrap();
    let messages = block(p2.fetch_messages(&acct, &snap.message_ids, &[])).unwrap();
    let op = pending(&acct, OperationKind::Trash { message_ids: vec![messages[0].id.clone()] });
    let before = env2.server.lock().connections;
    assert!(block(p2.apply(&acct, &op, &messages[..1], &labels)).is_ok());
    let s = env2.server.lock();
    assert!(s.deleted.is_empty() && s.connections == before);
}

#[test]
fn connection_drops_and_refusals_map_to_provider_errors_without_duplicates() {
    let env = make_env("drops", maildrop());
    let p = provider(&env, Some(Pop3Policy { leave_on_server: false, ..Default::default() }));
    let acct = account();

    env.server.lock().login = Login::AuthFailed;
    assert_eq!(block(p.fetch_snapshot(&acct)).err(), Some(ProviderError::SignInRequired));
    env.server.lock().login = Login::InUse;
    assert!(matches!(block(p.probe()), Err(ProviderError::Temporary(_))));
    env.server.lock().login = Login::Ok;

    // Dropped during the listing: Offline, nothing recorded.
    env.server.lock().drop_after_next = Some("UIDL".into());
    assert_eq!(block(p.fetch_snapshot(&acct)).err(), Some(ProviderError::Offline));
    let snap = block(p.fetch_snapshot(&acct)).unwrap();
    assert_eq!(snap.message_ids.len(), 3);

    // Dropped mid-download: the batch fails, what was saved is served from disk next time.
    env.server.lock().drop_after_next = Some("RETR 2".into());
    assert_eq!(block(p.fetch_messages(&acct, &snap.message_ids, &[])).err(), Some(ProviderError::Offline));
    let saved = std::fs::read_dir(&env.raw_dir).map(|d| d.count()).unwrap_or(0);
    assert_eq!(saved, 1, "the first message was saved before the drop");
    let messages = block(p.fetch_messages(&acct, &snap.message_ids, &[])).unwrap();
    assert_eq!(messages.len(), 3);
    assert_eq!(env.server.lock().commands("RETR 3"), 1, "the saved one was not fetched again");

    // The QUIT reply is lost: the server committed the deletions, we report Offline and
    // record nothing; the next cycle finds the messages gone and marks them so.
    env.server.lock().drop_after_next = Some("QUIT".into());
    assert_eq!(block(p.fetch_changes(&acct, snap.checkpoint.as_deref().unwrap())).err(), Some(ProviderError::Offline));
    assert!(env.server.lock().uidls().is_empty(), "the server executed DELE + QUIT before the reply was lost");
    let changes = block(p.fetch_changes(&acct, snap.checkpoint.as_deref().unwrap())).unwrap();
    assert!(changes.message_ids.is_empty());
    assert!(Checkpoint::parse(changes.checkpoint.as_deref().unwrap()).unwrap().entries.values().all(|e| e.deleted_on_server));

    // Dropped before the DELE: nothing deleted, the next cycle does it.
    let env4 = make_env("drop-before-dele", maildrop());
    let p4 = provider(&env4, Some(Pop3Policy { leave_on_server: false, ..Default::default() }));
    let snap4 = block(p4.fetch_snapshot(&acct)).unwrap();
    block(p4.fetch_messages(&acct, &snap4.message_ids, &[])).unwrap();
    env4.server.lock().drop_after_next = Some("UIDL".into());
    assert_eq!(block(p4.fetch_changes(&acct, snap4.checkpoint.as_deref().unwrap())).err(), Some(ProviderError::Offline));
    assert_eq!(env4.server.lock().uidls().len(), 3, "a session that ends without QUIT deletes nothing");
    block(p4.fetch_changes(&acct, snap4.checkpoint.as_deref().unwrap())).unwrap();
    assert!(env4.server.lock().uidls().is_empty());

    // A scripted -ERR on DELE is reported but does not fail the cycle.
    let env2 = make_env("dele-err", maildrop());
    let p2 = provider(&env2, Some(Pop3Policy { leave_on_server: false, ..Default::default() }));
    let snap = block(p2.fetch_snapshot(&acct)).unwrap();
    block(p2.fetch_messages(&acct, &snap.message_ids, &[])).unwrap();
    env2.server.lock().fail_next = Some(("DELE".into(), "[SYS/PERM] cannot delete".into()));
    let changes = block(p2.fetch_changes(&acct, snap.checkpoint.as_deref().unwrap())).unwrap();
    let cp = Checkpoint::parse(changes.checkpoint.as_deref().unwrap()).unwrap();
    assert_eq!(cp.entries.values().filter(|e| e.deleted_on_server).count(), 2);
    assert_eq!(env2.server.lock().uidls().len(), 1);

    // A server without TOP: large messages simply wait for RETR.
    let mut state = maildrop();
    state.no_top = true;
    let env3 = make_env("no-top", state);
    let p3 = provider(&env3, None);
    let snap = block(p3.fetch_snapshot(&acct)).unwrap();
    assert!(snap.messages.is_empty() && snap.message_ids.len() == 3);
    assert_eq!(block(p3.fetch_messages(&acct, &snap.message_ids, &[])).unwrap().len(), 3);
}

#[test]
fn a_message_gone_from_the_server_before_its_download_is_skipped() {
    let env = make_env("gone", maildrop());
    let p = provider(&env, None);
    let acct = account();
    let snap = block(p.fetch_snapshot(&acct)).unwrap();
    env.server.lock().messages.remove(1);
    let messages = block(p.fetch_messages(&acct, &snap.message_ids, &[])).unwrap();
    assert_eq!(messages.iter().map(|m| m.subject.as_str()).collect::<Vec<_>>(), vec!["Big one", "Plain hello"]);
    // A backlog id from before a checkpoint reset still downloads and joins the ledger.
    let fresh = provider(&env, None);
    let messages = block(fresh.fetch_messages(&acct, &pids(&["UIDL-1"]), &[])).unwrap();
    assert_eq!(messages.len(), 1);
    assert!(fresh.raw_path(&messages[0].id).exists());
}

#[test]
fn send_goes_over_smtp_and_keeps_a_local_sent_copy() {
    let env = make_env("send", maildrop());
    let smtp = serve_many(Behaviour::Normal);
    let acct = account();
    let p = Pop3Provider::new(servers(&env, smtp.addr.port(), None), "hunter2", env.raw_dir.clone()).with_outgoing_password(Some("other".into())).with_timeout(Duration::from_secs(5));
    let mut draft = Draft::new(acct.id.clone(), DraftKind::New, 0);
    draft.to = vec![Address::named("Ada", "ada@example.com")];
    draft.bcc = vec![Address::named("Me", "jason@example.com")];
    draft.subject = "Hello".into();
    draft.body_text = "Sent over SMTP.".into();
    let context = SendContext { sent_label: None, in_reply_to: None, labels: labels_for(&acct.id) };
    let receipt = block(p.send(&acct, &draft, context)).unwrap();
    assert!(is_sent_copy(&receipt.provider_id));
    assert_eq!(receipt.rfc_message_id.as_deref(), Some(draft_message_id(&draft).as_str()));
    let m = &receipt.message;
    assert_eq!(m.label_ids.iter().map(|l| l.as_str()).collect::<Vec<_>>(), vec!["pop3:jason@example.com:sent"]);
    assert!(m.is_read && m.is_outgoing && m.id.0.starts_with("pop3:jason@example.com:"));
    let seen = smtp.wait_for_connections(1);
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].auth.as_ref().map(|(u, p)| (u.as_str(), p.as_str())), Some(("jason@example.com", "other")));
    assert_eq!(seen[0].rcpt, vec!["RCPT TO:<ada@example.com>", "RCPT TO:<jason@example.com>"]);
    drop(seen);
    // The sent bytes are the message's saved source.
    let raw = block(p.fetch_raw(&acct, m)).unwrap();
    assert!(String::from_utf8_lossy(&raw).contains("Subject: Hello"));
    assert_eq!(block(p.reconcile_send(&acct, &draft)).unwrap(), SendOutcome::Unknown);

    smtp.set(Behaviour::DropAfterData);
    assert_eq!(block(p.send(&acct, &draft, SendContext::default())).err(), Some(ProviderError::Uncertain));
    smtp.set(Behaviour::RejectAuth);
    assert_eq!(block(p.send(&acct, &draft, SendContext::default())).err(), Some(ProviderError::SignInRequired));
    assert_eq!(env.server.lock().connections, 0, "sending never touches the POP3 server");
}

/// Round trip against a real POP3 server: `OVERLOOK_POP3_TEST_HOST`, `_PORT` (default by
/// security), `_SECURITY` (`tls` default, `starttls`, `none`), `_USER`, `_PASSWORD`.
/// Lists the maildrop, downloads the newest messages, polls again. Deletes nothing.
#[test]
#[ignore]
fn real_pop3_server_round_trip() {
    let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let (Some(host), Some(user), Some(password)) = (var("OVERLOOK_POP3_TEST_HOST"), var("OVERLOOK_POP3_TEST_USER"), var("OVERLOOK_POP3_TEST_PASSWORD")) else {
        panic!("set OVERLOOK_POP3_TEST_HOST, OVERLOOK_POP3_TEST_USER and OVERLOOK_POP3_TEST_PASSWORD");
    };
    let security = var("OVERLOOK_POP3_TEST_SECURITY").and_then(|s| Security::from_key(&s)).unwrap_or(Security::Tls);
    let port = var("OVERLOOK_POP3_TEST_PORT").and_then(|p| p.parse().ok()).unwrap_or(ServerProtocol::Pop3.default_port(security));
    let raw_dir = std::env::temp_dir().join(format!("overlook-pop3-real-{}", std::process::id()));
    let servers = MailServers { incoming: ServerEndpoint::new(host, port, security, user.clone()), outgoing: ServerEndpoint::new("localhost", 25, Security::None, user), pop3: None, imap: None, oauth: None };
    let p = Pop3Provider::new(servers, password, &raw_dir);
    let acct = account();
    block(p.probe()).unwrap();
    let snap = block(p.fetch_snapshot(&acct)).unwrap();
    eprintln!("{} messages on the server", snap.message_ids.len());
    let take = snap.message_ids.len().min(5);
    let messages = block(p.fetch_messages(&acct, &snap.message_ids[..take], &snap.labels)).unwrap();
    for m in &messages {
        eprintln!("  {:?} from {} ({} attachments) saved at {}", m.subject, m.from.email, m.attachments.len(), p.raw_path(&m.id).display());
    }
    let changes = block(p.fetch_changes(&acct, snap.checkpoint.as_deref().unwrap())).unwrap();
    assert!(changes.message_ids.is_empty(), "a second poll lists nothing new");
    let _ = std::fs::remove_dir_all(&raw_dir);
}

#[test]
fn oauth_accounts_sign_in_with_xoauth2() {
    use crate::microsoft::{oauth::AuthError, ClientConfig, MicrosoftSession};
    use crate::oauth::OAuthTokens;
    for error_challenge in [false, true] {
        let mut state = maildrop();
        state.password = "fresh-token".into();
        state.xoauth2_error_challenge = error_challenge;
        let env = make_env(&format!("oauth-{error_challenge}"), state);
        let stale = OAuthTokens { access_token: "revoked-token".into(), refresh_token: Some("refresh-1".into()), expires_at: overlook_core::time::now() + 3_600_000, scope: None, signed_in_at: 1 };
        let session = MicrosoftSession::new(ClientConfig::new("synthetic-client", "common").unwrap(), "scopes", stale, Box::new(|_| {})).with_refresher(Box::new(|_, _, _| {
            Ok(OAuthTokens { access_token: "fresh-token".into(), refresh_token: Some("refresh-2".into()), expires_at: overlook_core::time::now() + 3_600_000, scope: None, signed_in_at: 0 })
        }));
        let p = Pop3Provider::new(servers(&env, 1, None).with_oauth(OAuthIssuer::Microsoft), Credential::OAuth(std::sync::Arc::new(session)), env.raw_dir.clone()).with_timeout(Duration::from_secs(5));
        block(p.probe()).unwrap();
        let state = env.server.lock();
        assert_eq!(state.tokens_seen, ["revoked-token", "fresh-token"]);
        assert_eq!(state.commands("PASS"), 0, "no password is ever sent");
        assert_eq!(state.commands("AUTH XOAUTH2"), 2);
        if error_challenge {
            assert!(state.log.iter().any(|l| l == "*"), "the error challenge is cancelled with *: {:?}", state.log);
        }
        assert!(state.log.iter().all(|l| !l.contains("token")), "{:?}", state.log);
        drop(state);

        let dead = MicrosoftSession::new(ClientConfig::new("synthetic-client", "common").unwrap(), "scopes", OAuthTokens { access_token: String::new(), refresh_token: Some("r".into()), expires_at: 0, scope: None, signed_in_at: 1 }, Box::new(|_| {}))
            .with_refresher(Box::new(|_, _, _| Err(AuthError::Denied("invalid_grant: AADSTS70008: expired".into()))));
        let p = Pop3Provider::new(servers(&env, 1, None), Credential::OAuth(std::sync::Arc::new(dead)), env.raw_dir.clone()).with_timeout(Duration::from_secs(5));
        assert_eq!(block(p.probe()), Err(ProviderError::SignInRequired));
    }
}
