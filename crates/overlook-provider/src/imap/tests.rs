//! `ImapProvider` against the scripted fake server, plus an ignored round trip against a
//! real server configured through `OVERLOOK_IMAP_TEST_*`.

use super::fake::{serve, FakeFolder, FakeMessage, FakeServer, FakeState, Login};
use super::*;
use crate::ProviderError;
use std::time::Duration;

fn account() -> Account {
    Account { id: AccountId::from("imap:jason@example.com"), provider: ProviderKind::Imap, display_name: "Jason".into(), email: "jason@example.com".into(), created_at: 0 }
}

fn msg(headers: &[&str], body: &str) -> Vec<u8> {
    format!("{}\r\n\r\n{}", headers.join("\r\n"), body).into_bytes()
}

const MULTIPART_BODY: &str = "--outer\r\nContent-Type: multipart/alternative; boundary=\"inner\"\r\n\r\n--inner\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nText part.\r\n--inner\r\nContent-Type: text/html; charset=utf-8\r\n\r\n<p>HTML part with <img src=\"cid:logo\"></p>\r\n--inner--\r\n--outer\r\nContent-Type: image/png; name=\"logo.png\"\r\nContent-ID: <logo>\r\nContent-Disposition: inline; filename=\"logo.png\"\r\nContent-Transfer-Encoding: base64\r\n\r\niVBORw0KGgo=\r\n--outer\r\nContent-Type: application/pdf; name=\"report.pdf\"\r\nContent-Disposition: attachment; filename=\"report.pdf\"\r\nContent-Transfer-Encoding: base64\r\n\r\nJVBERi0xLjQK\r\n--outer--\r\n";

fn mailbox() -> FakeState {
    let mut state = FakeState::default();
    let mut inbox = FakeFolder::new("INBOX", &["\\HasNoChildren"], 1760400000);
    inbox.add(FakeMessage::new(&["\\Seen"], "10-Oct-2026 09:12:00 +0000", msg(&["From: Ada Lovelace <ada@example.com>", "To: jason@example.com", "Subject: Plain hello", "Date: Sat, 10 Oct 2026 09:12:00 +0000", "Message-ID: <plain-1@example.com>", "Content-Type: text/plain; charset=utf-8"], "Hello from a plain message.\r\n")));
    inbox.add(FakeMessage::new(
        &[],
        "12-Oct-2026 14:30:00 +0000",
        msg(
            &["From: =?UTF-8?Q?Gr=C3=A9goire?= <greg@example.com>", "To: jason@example.com", "Subject: =?UTF-8?B?UmU6IFBsYWluIGhlbGxv?=", "Date: Mon, 12 Oct 2026 14:30:00 +0000", "Message-ID: <multi-2@example.com>", "In-Reply-To: <plain-1@example.com>", "References: <plain-1@example.com>", "MIME-Version: 1.0", "Content-Type: multipart/mixed; boundary=\"outer\""],
            MULTIPART_BODY,
        ),
    ));
    inbox.add(FakeMessage::new(
        &["\\Flagged"],
        "13-Oct-2026 08:00:00 +0000",
        msg(
            &["From: Calendar <cal@example.com>", "To: jason@example.com", "Subject: Invitation: Standup", "Date: Tue, 13 Oct 2026 08:00:00 +0000", "Message-ID: <invite-3@example.com>", "MIME-Version: 1.0", "Content-Type: multipart/alternative; boundary=\"b\""],
            "--b\r\nContent-Type: text/plain\r\n\r\nYou are invited.\r\n--b\r\nContent-Type: text/calendar; method=REQUEST\r\n\r\nBEGIN:VCALENDAR\r\nMETHOD:REQUEST\r\nBEGIN:VEVENT\r\nUID:ev1\r\nSUMMARY:Standup\r\nDTSTART:20261014T090000Z\r\nDTEND:20261014T091500Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n--b--\r\n",
        ),
    ));
    for i in 4..=5 {
        inbox.add(FakeMessage::new(&[], "14-Oct-2026 10:00:00 +0000", msg(&["From: bot@example.com", "To: jason@example.com", &format!("Subject: Note {i}"), &format!("Message-ID: <note-{i}@example.com>"), &format!("Date: Wed, 14 Oct 2026 10:0{i}:00 +0000")], "body\r\n")));
    }
    let mut sent = FakeFolder::new("Sent", &["\\HasNoChildren", "\\Sent"], 1760400001);
    sent.add(FakeMessage::new(&["\\Seen"], "11-Oct-2026 09:00:00 +0000", msg(&["From: Jason <jason@example.com>", "To: ada@example.com", "Subject: Re: Plain hello", "Message-ID: <sent-1@example.com>", "Date: Sun, 11 Oct 2026 09:00:00 +0000"], "Thanks Ada.\r\n")));
    let mut work = FakeFolder::new("Work/Clients", &["\\HasNoChildren"], 1760400002);
    work.add(FakeMessage::new(&[], "09-Oct-2026 09:00:00 +0000", msg(&["From: client@example.com", "To: jason@example.com", "Subject: Contract", "Message-ID: <work-1@example.com>", "Date: Fri, 09 Oct 2026 09:00:00 +0000"], "Please sign.\r\n")));
    work.add(FakeMessage::new(&["\\Seen"], "09-Oct-2026 10:00:00 +0000", msg(&["From: client@example.com", "To: jason@example.com", "Subject: Contract signed", "Message-ID: <work-2@example.com>", "Date: Fri, 09 Oct 2026 10:00:00 +0000"], "Done.\r\n")));
    state.folders = vec![inbox, sent, FakeFolder::new("Trash", &["\\HasNoChildren", "\\Trash"], 1760400003), FakeFolder::new("Drafts", &["\\HasNoChildren", "\\Drafts"], 1760400004), FakeFolder::new("[Gmail]", &["\\Noselect", "\\HasChildren"], 0), FakeFolder::new("Work", &["\\HasChildren"], 1760400005), work];
    state.subscribed = ["INBOX", "Sent", "Trash", "Work/Clients"].into_iter().map(str::to_owned).collect();
    state
}

fn provider(server: &FakeServer) -> ImapProvider {
    let servers = MailServers {
        incoming: ServerEndpoint::new("127.0.0.1", server.addr.port(), Security::None, "jason@example.com"),
        outgoing: ServerEndpoint::new("127.0.0.1", 1, Security::None, "jason@example.com"),
        pop3: None,
        imap: None,
    };
    ImapProvider::new(servers, "hunter2").with_timeout(Duration::from_secs(5)).with_inbox_limit(20)
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

fn pid(folder: &str, uidvalidity: u32, uid: u32) -> ProviderId {
    MessageRef::provider_id(folder, uidvalidity, uid)
}

#[test]
fn drafts_round_trip_localized_mailboxes_and_recover_lost_append_replies() {
    let mut state = mailbox();
    // IMAP4rev1 mailbox names use modified UTF-7 on the wire (Entwürfe).
    state.folder_mut("Drafts").path = "INBOX.Entw&APw-rfe".into();
    let server = serve(state);
    let p = provider(&server);
    let account = account();
    block(p.probe()).unwrap();
    let raw = |token: &str, body: &str| format!("From: jason@example.com\r\nMessage-ID: <{token}@example.com>\r\nSubject: Draft\r\n\r\n{body}").into_bytes();
    server.state.lock().unwrap().drop_after_next = Some("APPEND".into());
    let result = block(p.put_draft(&account, None, &raw("draft-first", "First"), "<draft-first@example.com>", None));
    assert!(result.is_ok(), "{result:?}; commands: {:?}", server.lock().log);
    let first = result.unwrap().unwrap();
    assert_eq!(server.state.lock().unwrap().commands("APPEND"), 1, "lost APPEND response must be reconciled before retry");
    let first_ref = MessageRef::parse(&first.id).unwrap();
    assert_eq!(first_ref.folder, "INBOX.Entw&APw-rfe");
    assert!(server.state.lock().unwrap().folder(&first_ref.folder).messages[&first_ref.uid].has_flag("\\Draft"));
    assert_eq!(block(p.list_drafts(&account)).unwrap(), vec![first.clone()]);
    assert!(String::from_utf8(block(p.fetch_draft(&account, &first)).unwrap().unwrap()).unwrap().contains("First"));
    let second = block(p.put_draft(&account, Some(&first), &raw("draft-edit", "Edited"), "<draft-edit@example.com>", None)).unwrap().unwrap();
    assert_ne!(second.id, first.id);
    assert_eq!(block(p.list_drafts(&account)).unwrap(), vec![second.clone()]);
    assert!(block(p.fetch_draft(&account, &first)).unwrap().is_none());
    assert_eq!(block(p.put_draft(&account, Some(&first), &raw("draft-edit", "Edited"), "<draft-edit@example.com>", None)).unwrap(), Some(second.clone()));
    assert!(block(p.put_draft(&account, Some(&first), &raw("stale", "Stale"), "<stale@example.com>", None)).unwrap().is_none());
    block(p.delete_remote_draft(&account, &second)).unwrap();
    block(p.delete_remote_draft(&account, &second)).unwrap();
    assert!(block(p.list_drafts(&account)).unwrap().is_empty());
}

#[test]
fn draft_deletion_without_uidplus_does_not_expunge_other_clients_messages() {
    let mut state = mailbox();
    state.capabilities = "IMAP4rev1".into();
    let unrelated = state.folder_mut("Drafts").add(FakeMessage::new(&["\\Deleted"], "14-Oct-2026 12:00:00 +0000", msg(&["Message-ID: <unrelated@example.com>"], "Other client's deletion")));
    let server = serve(state);
    let p = provider(&server);
    let account = account();
    let raw = msg(&["From: jason@example.com", "Message-ID: <no-uidplus@example.com>"], "Draft body");
    let saved = block(p.put_draft(&account, None, &raw, "<no-uidplus@example.com>", None)).unwrap().unwrap();
    block(p.delete_remote_draft(&account, &saved)).unwrap();
    assert!(block(p.list_drafts(&account)).unwrap().is_empty());
    let state = server.state.lock().unwrap();
    assert!(state.folder("Drafts").messages.contains_key(&unrelated));
    assert_eq!(state.commands("EXPUNGE"), 0);
}

#[test]
fn draft_folder_creation_and_uidvalidity_reset_are_safe() {
    let mut state = mailbox();
    state.folders.retain(|f| f.path != "Drafts");
    let server = serve(state);
    let p = provider(&server);
    let account = account();
    assert!(block(p.list_drafts(&account)).unwrap().is_empty());
    let raw = msg(&["From: jason@example.com", "Message-ID: <created-folder@example.com>"], "Draft body");
    let saved = block(p.put_draft(&account, None, &raw, "<created-folder@example.com>", None)).unwrap().unwrap();
    assert!(server.state.lock().unwrap().has_folder("Drafts"));
    server.state.lock().unwrap().folder_mut("Drafts").uidvalidity += 1;
    assert!(block(p.fetch_draft(&account, &saved)).unwrap().is_none());
    block(p.delete_remote_draft(&account, &saved)).unwrap();
    assert_eq!(block(p.list_drafts(&account)).unwrap().len(), 1, "stale UIDs must never delete the new mailbox's messages");
}

#[test]
fn draft_flags_do_not_leak_unsent_messages_into_regular_mail() {
    let mut state = mailbox();
    let uid = state.folder_mut("INBOX").add(FakeMessage::new(&["\\Draft"], "14-Oct-2026 12:00:00 +0000", msg(&["From: jason@example.com", "Subject: Not sent", "Message-ID: <unsent@example.com>"], "Draft body")));
    let server = serve(state);
    let p = provider(&server);
    let account = account();
    let snapshot = block(p.fetch_snapshot(&account)).unwrap();
    let messages = block(p.fetch_messages(&account, &[pid("INBOX", 1760400000, uid)], &snapshot.labels)).unwrap();
    assert!(messages.is_empty());
}

#[test]
fn snapshot_maps_folders_and_queues_newest_messages() {
    let server = serve(mailbox());
    let p = provider(&server);
    block(p.probe()).unwrap();
    let caps = p.capabilities();
    assert!(caps.special_use && caps.move_ && caps.uidplus && caps.idle && !caps.condstore);

    let snap = block(p.fetch_snapshot(&account())).unwrap();
    let names: Vec<(&str, LabelKind)> = snap.labels.iter().map(|l| (l.name.as_str(), l.kind)).collect();
    assert_eq!(names, vec![("Inbox", LabelKind::Inbox), ("Sent", LabelKind::Sent), ("Trash", LabelKind::Trash), ("Work", LabelKind::User), ("Work / Clients", LabelKind::User)], "Drafts and the unselectable [Gmail] container are not labels");
    assert_eq!(snap.labels[0].id.as_str(), "imap:jason@example.com:INBOX");
    assert_eq!(snap.labels[4].provider_id.as_ref().unwrap().as_str(), "Work/Clients");
    assert!(snap.messages.is_empty(), "bodies come through the backlog");
    let ids: Vec<&str> = snap.message_ids.iter().map(|p| p.0.as_str()).collect();
    assert_eq!(ids[..5], ["INBOX\u{1f}1760400000\u{1f}5", "INBOX\u{1f}1760400000\u{1f}4", "INBOX\u{1f}1760400000\u{1f}3", "INBOX\u{1f}1760400000\u{1f}2", "INBOX\u{1f}1760400000\u{1f}1"], "Inbox first, newest first");
    assert_eq!(ids.len(), 5 + 1 + 2);

    let cp = Checkpoint::parse(snap.checkpoint.as_deref().unwrap()).unwrap();
    assert_eq!(cp.folders.len(), 5);
    let inbox = &cp.folders["INBOX"];
    assert_eq!((inbox.kind.as_str(), inbox.uidvalidity, inbox.highest_uid, inbox.known.as_str(), inbox.seen.as_str(), inbox.flagged.as_str()), ("inbox", 1760400000, 5, "1:5", "1", "3"));
    assert!(inbox.subscribed && !inbox.fresh);
    assert_eq!(cp.folders["Sent"].special_use.as_deref(), Some("\\Sent"));
    assert_eq!(cp.folders["Trash"].known, "");
    assert_eq!(cp.folders["Work/Clients"].seen, "2");

    let state = server.state.lock().unwrap();
    assert_eq!(state.connections, 1, "one connection serves probe and snapshot");
    assert_eq!(state.commands(" LOGIN "), 1);
    assert_eq!(state.commands("EXAMINE"), 5, "unselectable folders are never examined");
}

#[test]
fn fetch_messages_parses_bodies_flags_and_attachments() {
    let server = serve(mailbox());
    let p = provider(&server);
    let acct = account();
    let ids = vec![pid("INBOX", 1760400000, 2), pid("INBOX", 1760400000, 3), pid("INBOX", 1760400000, 1), pid("Work/Clients", 1760400002, 1), pid("INBOX", 999, 1), ProviderId("gmail-looking-id".into()), pid("INBOX", 1760400000, 77)];
    let messages = block(p.fetch_messages(&acct, &ids, &[])).unwrap();
    assert_eq!(messages.len(), 4, "stale UIDVALIDITY, malformed and vanished ids are skipped: {:?}", messages.iter().map(|m| m.subject.clone()).collect::<Vec<_>>());

    let multi = messages.iter().find(|m| m.provider_id.as_ref().unwrap().as_str() == "INBOX\u{1f}1760400000\u{1f}2").unwrap();
    assert!(multi.id.as_str().starts_with("imap:jason@example.com:"), "local ids are minted under the account: {}", multi.id);
    assert!(multi.conversation_id.0.is_empty(), "threading is the sync engine's job");
    assert_eq!(multi.subject, "Re: Plain hello");
    assert_eq!(multi.from.name.as_deref(), Some("Grégoire"));
    assert_eq!(multi.in_reply_to.as_deref(), Some("<plain-1@example.com>"));
    assert_eq!(multi.body.text.as_deref().map(str::trim), Some("Text part."));
    assert!(multi.body.html.as_deref().unwrap().contains("cid:logo"));
    assert_eq!(multi.attachments.len(), 2);
    assert_eq!((multi.attachments[0].filename.as_str(), multi.attachments[0].content_id.as_deref(), multi.attachments[0].provider_id.as_ref().unwrap().as_str()), ("logo.png", Some("logo"), "2"));
    assert_eq!((multi.attachments[1].filename.as_str(), multi.attachments[1].provider_id.as_ref().unwrap().as_str()), ("report.pdf", "3"));
    assert_eq!(multi.attachments[1].id.as_str(), format!("{}:att-1", multi.id));
    assert!(!multi.is_read && !multi.is_starred);
    assert_eq!(multi.label_ids.iter().map(|l| l.as_str()).collect::<Vec<_>>(), vec!["imap:jason@example.com:INBOX"]);

    let invite = messages.iter().find(|m| m.subject == "Invitation: Standup").unwrap();
    assert!(invite.is_starred && !invite.is_read);
    assert_eq!(invite.invitation.as_ref().map(|i| i.summary.clone()).flatten().as_deref(), Some("Standup"));

    let plain = messages.iter().find(|m| m.subject == "Plain hello").unwrap();
    assert!(plain.is_read);
    assert_eq!(plain.date, overlook_core::tz::parse_rfc3339("2026-10-10T09:12:00Z").unwrap());

    let work = messages.iter().find(|m| m.subject == "Contract").unwrap();
    assert_eq!(work.label_ids.iter().next().unwrap().as_str(), "imap:jason@example.com:Work/Clients");

    let state = server.state.lock().unwrap();
    assert_eq!(state.commands("EXAMINE"), 3, "INBOX once (not again for the stale batch), Work/Clients once, plus the stale UIDVALIDITY probe");
    assert_eq!(state.commands("RFC822.SIZE)"), 2, "sizes are checked per folder before bodies are fetched");
}

#[test]
fn large_messages_take_the_bodystructure_path() {
    let mut state = mailbox();
    let inbox = state.folder_mut("INBOX");
    let mut big = FakeMessage::new(
        &[],
        "14-Oct-2026 12:00:00 +0000",
        msg(&["From: big@example.com", "To: jason@example.com", "Subject: Big one", "Message-ID: <big@example.com>", "Date: Wed, 14 Oct 2026 12:00:00 +0000", "MIME-Version: 1.0", "Content-Type: multipart/mixed; boundary=\"outer\""], MULTIPART_BODY),
    );
    big.size_override = Some(3 * 1024 * 1024);
    big.bodystructure = Some(r#"((("TEXT" "PLAIN" ("CHARSET" "utf-8") NIL NIL "7BIT" 12 1 NIL NIL NIL NIL)("TEXT" "HTML" ("CHARSET" "utf-8") NIL NIL "7BIT" 40 1 NIL NIL NIL NIL) "ALTERNATIVE" ("BOUNDARY" "inner") NIL NIL NIL)("IMAGE" "PNG" ("NAME" "logo.png") "<logo>" NIL "BASE64" 12 NIL ("INLINE" ("FILENAME" "logo.png")) NIL NIL)("APPLICATION" "PDF" ("NAME" "report.pdf") NIL NIL "BASE64" 3000000 NIL ("ATTACHMENT" ("FILENAME" "report.pdf")) NIL NIL) "MIXED" ("BOUNDARY" "outer") NIL NIL NIL)"#.into());
    let uid = inbox.add(big);
    let server = serve(state);
    let p = provider(&server);
    let messages = block(p.fetch_messages(&account(), &[pid("INBOX", 1760400000, uid)], &[])).unwrap();
    assert_eq!(messages.len(), 1);
    let m = &messages[0];
    assert_eq!(m.subject, "Big one");
    assert_eq!(m.rfc_message_id.as_deref(), Some("<big@example.com>"));
    assert_eq!(m.body.text.as_deref().map(str::trim), Some("Text part."));
    assert!(m.body.html.as_deref().unwrap().contains("HTML part"));
    assert_eq!(m.attachments.len(), 2);
    assert_eq!((m.attachments[0].filename.as_str(), m.attachments[0].provider_id.as_ref().unwrap().as_str(), m.attachments[0].content_id.as_deref()), ("logo.png", "2", Some("logo")));
    assert_eq!((m.attachments[1].filename.as_str(), m.attachments[1].size_bytes, m.attachments[1].provider_id.as_ref().unwrap().as_str()), ("report.pdf", 3_000_000, "3"));
    let state = server.state.lock().unwrap();
    assert!(state.log.iter().any(|l| l.contains("BODYSTRUCTURE") && l.contains("BODY.PEEK[HEADER]")), "{:?}", state.log);
    assert!(state.log.iter().any(|l| l.contains("BODY.PEEK[1.1]<0.") && l.contains("BODY.PEEK[1.1.MIME]") && l.contains("BODY.PEEK[1.2]<0.")), "{:?}", state.log);
    assert!(!state.log.iter().any(|l| l.contains("BODY.PEEK[])")), "the whole message is never fetched");
}

#[test]
fn changes_report_new_mail_flags_and_deletions() {
    let server = serve(mailbox());
    let p = provider(&server);
    let acct = account();
    let snap = block(p.fetch_snapshot(&acct)).unwrap();
    let cp = snap.checkpoint.unwrap();

    // Nothing changed: nothing reported, cursor cycle advances.
    let quiet = block(p.fetch_changes(&acct, &cp)).unwrap();
    assert!(quiet.labels.is_none() && quiet.upserts.is_empty() && quiet.deleted_provider_ids.is_empty() && quiet.message_ids.is_empty());
    let cp = quiet.checkpoint.unwrap();
    assert_eq!(Checkpoint::parse(&cp).unwrap().cycle, 1);

    {
        let mut s = server.state.lock().unwrap();
        let inbox = s.folder_mut("INBOX");
        inbox.messages.get_mut(&2).unwrap().flags = vec!["\\Seen".into()];
        inbox.messages.remove(&3);
        inbox.add(FakeMessage::new(&[], "15-Oct-2026 08:00:00 +0000", msg(&["From: new@example.com", "To: jason@example.com", "Subject: Fresh", "Message-ID: <fresh@example.com>", "Date: Thu, 15 Oct 2026 08:00:00 +0000"], "new body\r\n")));
        // A flag change outside the Inbox is only picked up every tenth cycle.
        s.folder_mut("Work/Clients").messages.get_mut(&1).unwrap().flags = vec!["\\Flagged".into()];
    }
    let changes = block(p.fetch_changes(&acct, &cp)).unwrap();
    assert!(changes.labels.is_none(), "the folder list is reread hourly, not every cycle");
    assert_eq!(changes.deleted_provider_ids, vec![pid("INBOX", 1760400000, 3)]);
    assert_eq!(changes.message_ids, vec![pid("INBOX", 1760400000, 6)], "only new mail needs a body");
    let subjects: Vec<(&str, bool, bool)> = changes.upserts.iter().map(|m| (m.subject.as_str(), m.is_read, m.body.is_cached())).collect();
    assert_eq!(subjects, vec![("Fresh", false, false), ("Re: Plain hello", true, false)], "new mail first, then the flag change, both header-only");
    assert_eq!(changes.upserts[0].rfc_message_id.as_deref(), Some("<fresh@example.com>"));
    assert_eq!(changes.upserts[1].provider_id, Some(pid("INBOX", 1760400000, 2)));
    let parsed = Checkpoint::parse(changes.checkpoint.as_deref().unwrap()).unwrap();
    let inbox = &parsed.folders["INBOX"];
    assert_eq!((inbox.known.as_str(), inbox.seen.as_str(), inbox.flagged.as_str(), inbox.highest_uid), ("1:2,4:6", "1:2", "", 6));
    assert_eq!(parsed.folders["Work/Clients"].flagged, "", "not refreshed this cycle");

    // Cycle 10 refreshes flags everywhere.
    let mut cp10 = parsed.clone();
    cp10.cycle = 9;
    let changes = block(p.fetch_changes(&acct, &cp10.to_json())).unwrap();
    assert_eq!(changes.upserts.iter().map(|m| (m.subject.as_str(), m.is_starred)).collect::<Vec<_>>(), vec![("Contract", true)]);
    assert!(changes.message_ids.is_empty());
    assert_eq!(Checkpoint::parse(changes.checkpoint.as_deref().unwrap()).unwrap().folders["Work/Clients"].flagged, "1");
}

#[test]
fn uidvalidity_change_and_folder_list_changes() {
    let server = serve(mailbox());
    let p = provider(&server);
    let acct = account();
    let cp = block(p.fetch_snapshot(&acct)).unwrap().checkpoint.unwrap();

    {
        let mut s = server.state.lock().unwrap();
        s.folder_mut("Work/Clients").uidvalidity = 42;
    }
    let changes = block(p.fetch_changes(&acct, &cp)).unwrap();
    assert_eq!(changes.deleted_provider_ids, vec![pid("Work/Clients", 1760400002, 1), pid("Work/Clients", 1760400002, 2)]);
    assert_eq!(changes.message_ids, vec![pid("Work/Clients", 42, 2), pid("Work/Clients", 42, 1)]);
    assert!(changes.upserts.is_empty());
    let mut parsed = Checkpoint::parse(changes.checkpoint.as_deref().unwrap()).unwrap();
    assert_eq!((parsed.folders["Work/Clients"].uidvalidity, parsed.folders["Work/Clients"].seen.as_str()), (42, "2"));

    // An hour later the folder list is reread: Work/Clients is gone, Archive appeared.
    {
        let mut s = server.state.lock().unwrap();
        s.folders.retain(|f| f.path != "Work/Clients");
        let mut archive = FakeFolder::new("Archive", &["\\Archive"], 7);
        archive.add(FakeMessage::new(&["\\Seen"], "01-Oct-2026 08:00:00 +0000", msg(&["From: old@example.com", "Subject: Old", "Message-ID: <old@example.com>"], "old\r\n")));
        s.folders.push(archive);
    }
    parsed.listed_at = 0;
    let changes = block(p.fetch_changes(&acct, &parsed.to_json())).unwrap();
    let labels = changes.labels.expect("labels are resent after a re-list");
    assert_eq!(labels.iter().map(|l| l.name.as_str()).collect::<Vec<_>>(), vec!["Inbox", "Sent", "Archive", "Trash", "Work"]);
    assert_eq!(changes.deleted_provider_ids, vec![pid("Work/Clients", 42, 1), pid("Work/Clients", 42, 2)]);
    assert_eq!(changes.message_ids, vec![pid("Archive", 7, 1)]);
    let parsed = Checkpoint::parse(changes.checkpoint.as_deref().unwrap()).unwrap();
    assert!(!parsed.folders.contains_key("Work/Clients"));
    assert_eq!((parsed.folders["Archive"].kind.as_str(), parsed.folders["Archive"].fresh, parsed.folders["Archive"].known.as_str()), ("archive", false, "1"));

    // A folder deleted between listings is noticed at once and triggers a re-list.
    {
        let mut s = server.state.lock().unwrap();
        s.folders.retain(|f| f.path != "Archive");
    }
    let changes = block(p.fetch_changes(&acct, &parsed.to_json())).unwrap();
    assert_eq!(changes.deleted_provider_ids, vec![pid("Archive", 7, 1)]);
    assert!(changes.labels.is_some_and(|l| !l.iter().any(|x| x.name == "Archive")));
    assert!(!Checkpoint::parse(changes.checkpoint.as_deref().unwrap()).unwrap().folders.contains_key("Archive"));
}

#[test]
fn attachments_and_raw_bytes_are_fetched_by_section() {
    let server = serve(mailbox());
    let p = provider(&server);
    let acct = account();
    let messages = block(p.fetch_messages(&acct, &[pid("INBOX", 1760400000, 2)], &[])).unwrap();
    let m = &messages[0];
    let pdf = m.attachments.iter().find(|a| a.filename == "report.pdf").unwrap();
    assert_eq!(block(p.fetch_attachment(&acct, m, pdf)).unwrap(), b"%PDF-1.4\n");
    let png = m.attachments.iter().find(|a| a.filename == "logo.png").unwrap();
    assert_eq!(block(p.fetch_attachment(&acct, m, png)).unwrap(), b"\x89PNG\r\n\x1a\n");
    let raw = block(p.fetch_raw(&acct, m)).unwrap();
    assert!(raw.starts_with(b"From: =?UTF-8?Q?Gr=C3=A9goire?=") && raw.ends_with(b"--outer--\r\n"));

    let mut stale = m.clone();
    stale.provider_id = Some(pid("INBOX", 1, 2));
    assert!(matches!(block(p.fetch_attachment(&acct, &stale, pdf)), Err(ProviderError::Rejected(_))));
    let rebuilt = block(p.fetch_raw(&acct, &stale)).unwrap();
    assert!(rebuilt.windows(16).any(|w| w == b"Re: Plain hello "[..16].as_ref() || w == b"Subject: Re: Pla"), "falls back to the stored message: {}", String::from_utf8_lossy(&rebuilt));
}

#[test]
fn server_failures_map_to_provider_errors() {
    let acct = account();
    let server = serve(FakeState { login: Login::AuthFailed, ..mailbox() });
    assert_eq!(block(provider(&server).probe()), Err(ProviderError::SignInRequired));
    let server = serve(FakeState { password: "other".into(), ..mailbox() });
    assert_eq!(block(provider(&server).probe()), Err(ProviderError::SignInRequired));
    let server = serve(FakeState { login: Login::Unavailable, ..mailbox() });
    assert_eq!(block(provider(&server).probe()), Err(ProviderError::Temporary("Temporary authentication failure".into())));
    let server = serve(FakeState { login: Login::Bye, ..mailbox() });
    assert_eq!(block(provider(&server).probe()), Err(ProviderError::Offline));
    let server = serve(FakeState { drop_next_command: true, ..mailbox() });
    assert_eq!(block(provider(&server).probe()), Err(ProviderError::Offline), "a fresh connection that dies is Offline");

    let unreachable = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
    let servers = MailServers { incoming: ServerEndpoint::new("127.0.0.1", unreachable.port(), Security::None, "j"), outgoing: ServerEndpoint::new("127.0.0.1", 1, Security::None, "j"), pop3: None, imap: None };
    assert_eq!(block(ImapProvider::new(servers, "pw").with_timeout(Duration::from_secs(2)).probe()), Err(ProviderError::Offline));

    // A folder the server reports as in use is a temporary failure, not a sign-in problem.
    let server = serve(mailbox());
    let p = provider(&server);
    let cp = block(p.fetch_snapshot(&acct)).unwrap().checkpoint.unwrap();
    {
        // Sent has to look changed, or STATUS lets the sync skip it without opening it.
        let mut s = server.state.lock().unwrap();
        s.in_use.insert("Sent".into());
        s.folder_mut("Sent").next_uid += 1;
    }
    assert_eq!(block(p.fetch_changes(&acct, &cp)).err(), Some(ProviderError::Temporary("Mailbox is in use by another process".into())));

    // A Gmail history id or anything else that is not our checkpoint asks for a resync.
    assert_eq!(block(p.fetch_changes(&acct, "1234567")).err(), Some(ProviderError::CheckpointExpired));

    // Calendar and contact operations have nowhere to go.
    let calendar = op(&acct, OperationKind::CreateEvent { event_id: EventId::from("e") });
    assert_eq!(block(p.apply(&acct, &calendar, &[], &[])), Err(ProviderError::Rejected("this account has no calendar".into())));
    let contact = op(&acct, OperationKind::CreateContact { contact_id: "c".into() });
    assert_eq!(block(p.apply(&acct, &contact, &[], &[])), Err(ProviderError::Rejected("this account cannot store contacts".into())));
}

// ------------------------------------------------------------------ writes

fn op(acct: &Account, kind: OperationKind) -> PendingOperation {
    PendingOperation { id: OperationId::from("op"), account_id: acct.id.clone(), seq: 1, kind, state: OperationState::Pending, attempts: 0, last_error: None, created_at: 0, updated_at: 0 }
}

/// Snapshot, then every message as the store would hold it (bodies downloaded).
fn synced(server: &FakeServer, p: &ImapProvider) -> (Vec<Label>, Vec<Message>, String) {
    let acct = account();
    let snap = block(p.fetch_snapshot(&acct)).unwrap();
    let messages = block(p.fetch_messages(&acct, &snap.message_ids, &snap.labels)).unwrap();
    assert_eq!(messages.len(), snap.message_ids.len(), "{:?}", server.lock().log);
    (snap.labels, messages, snap.checkpoint.unwrap())
}

fn by_pid<'a>(messages: &'a [Message], folder: &str, uid: u32) -> &'a Message {
    let uidvalidity = match folder {
        "INBOX" => 1760400000,
        "Sent" => 1760400001,
        "Work/Clients" => 1760400002,
        _ => panic!("{folder}"),
    };
    messages.iter().find(|m| m.provider_id == Some(pid(folder, uidvalidity, uid))).unwrap_or_else(|| panic!("no message {folder}/{uid}"))
}

fn ids(messages: &[&Message]) -> Vec<MessageId> {
    messages.iter().map(|m| m.id.clone()).collect()
}

#[test]
fn flags_are_stored_per_folder_and_stale_rows_are_skipped() {
    let server = serve(mailbox());
    let p = provider(&server);
    let acct = account();
    let (labels, messages, _) = synced(&server, &p);
    let inbox2 = by_pid(&messages, "INBOX", 2);
    let inbox3 = by_pid(&messages, "INBOX", 3);
    let work1 = by_pid(&messages, "Work/Clients", 1);
    let mut stale = work1.clone();
    stale.provider_id = Some(pid("Work/Clients", 7, 1));
    let mut local_only = work1.clone();
    local_only.id = MessageId::from("imap:jason@example.com:local");
    local_only.provider_id = Some(ProviderId("local\u{1f}<x@y>".into()));

    let read = op(&acct, OperationKind::SetRead { message_ids: ids(&[inbox2, work1, &stale, &local_only]), read: true });
    let applied = block(p.apply(&acct, &read, &[inbox2.clone(), work1.clone(), stale, local_only], &labels)).unwrap();
    assert_eq!(applied, AppliedChanges::default());
    {
        let s = server.lock();
        assert!(s.folder("INBOX").messages[&2].has_flag("\\Seen"));
        assert!(s.folder("Work/Clients").messages[&1].has_flag("\\Seen"));
        assert_eq!(s.commands("UID STORE"), 2, "one STORE per folder: {:?}", s.log);
        assert!(s.log.iter().any(|l| l.contains("SELECT \"INBOX\"")), "writes open the folder read-write: {:?}", s.log);
        assert!(s.log.iter().any(|l| l.contains("UID STORE 2 +FLAGS.SILENT (\\Seen)")), "{:?}", s.log);
    }
    let unstar = op(&acct, OperationKind::SetStarred { message_ids: ids(&[inbox3]), starred: false });
    block(p.apply(&acct, &unstar, &[inbox3.clone()], &labels)).unwrap();
    assert!(!server.lock().folder("INBOX").messages[&3].has_flag("\\Flagged"));
    assert!(server.lock().log.iter().any(|l| l.contains("UID STORE 3 -FLAGS.SILENT (\\Flagged)")));
    assert_eq!(server.lock().connections, 1);
}

#[test]
fn archive_creates_the_folder_moves_with_uid_move_and_reports_copyuid() {
    let server = serve(mailbox());
    let p = provider(&server);
    let acct = account();
    let (labels, messages, cp) = synced(&server, &p);
    assert!(!labels.iter().any(|l| l.kind == LabelKind::Archive));
    let (a, b) = (by_pid(&messages, "INBOX", 1), by_pid(&messages, "INBOX", 2));
    let archive = op(&acct, OperationKind::Archive { message_ids: ids(&[a, b]) });
    let applied = block(p.apply(&acct, &archive, &[a.clone(), b.clone()], &labels)).unwrap();

    let s = server.lock();
    assert!(s.has_folder("Archive"), "created on demand: {:?}", s.log);
    let archived = s.folder("Archive");
    assert_eq!(archived.messages.len(), 2);
    assert!(!s.folder("INBOX").messages.contains_key(&1) && !s.folder("INBOX").messages.contains_key(&2));
    assert!(s.log.iter().any(|l| l.contains("UID MOVE 1:2 \"Archive\"")), "{:?}", s.log);
    assert_eq!(s.commands("UID COPY"), 0);
    let uv = archived.uidvalidity;
    drop(s);
    let archive_label = LabelId::from("imap:jason@example.com:Archive");
    assert_eq!(applied.provider_ids, vec![(a.id.clone(), pid("Archive", uv, 1), archive_label.clone()), (b.id.clone(), pid("Archive", uv, 2), archive_label.clone())]);
    assert_eq!(applied.labels.len(), 1);
    assert_eq!((applied.labels[0].id.clone(), applied.labels[0].kind, applied.labels[0].name.as_str()), (archive_label, LabelKind::Archive, "Archive"));
    assert!(p.relist_pending());

    // The next change cycle re-lists at once, drops the moved UIDs from the Inbox and starts
    // tracking the new folder.
    let changes = block(p.fetch_changes(&acct, &cp)).unwrap();
    assert!(!p.relist_pending());
    let names: Vec<&str> = changes.labels.as_ref().unwrap().iter().map(|l| l.name.as_str()).collect();
    assert_eq!(names, vec!["Inbox", "Sent", "Archive", "Trash", "Work", "Work / Clients"]);
    assert_eq!(changes.deleted_provider_ids, vec![pid("INBOX", 1760400000, 1), pid("INBOX", 1760400000, 2)]);
    assert!(changes.message_ids.contains(&pid("Archive", uv, 1)) && changes.message_ids.contains(&pid("Archive", uv, 2)), "{:?}", changes.message_ids);
    assert_eq!(Checkpoint::parse(changes.checkpoint.as_deref().unwrap()).unwrap().folders["Archive"].known, "1:2");

    // Archiving again is a no-op at the server and still succeeds.
    let moved: Vec<Message> = [a, b].iter().map(|m| { let mut c = (*m).clone(); c.provider_id = Some(pid("Archive", uv, if m.id == a.id { 1 } else { 2 })); c }).collect();
    let labels: Vec<Label> = changes.labels.unwrap();
    let again = block(p.apply(&acct, &archive, &moved, &labels)).unwrap();
    assert!(again.provider_ids.is_empty() && again.labels.is_empty());
}

#[test]
fn moves_fall_back_to_copy_and_expunge_without_move() {
    // UIDPLUS without MOVE: COPY answers COPYUID, then \Deleted + UID EXPUNGE.
    let server = serve(FakeState { capabilities: "IMAP4rev1 UIDPLUS SPECIAL-USE".into(), ..mailbox() });
    let p = provider(&server);
    let acct = account();
    let (labels, messages, _) = synced(&server, &p);
    let (a, b) = (by_pid(&messages, "INBOX", 4), by_pid(&messages, "INBOX", 5));
    let trash = op(&acct, OperationKind::Trash { message_ids: ids(&[a, b]) });
    let applied = block(p.apply(&acct, &trash, &[a.clone(), b.clone()], &labels)).unwrap();
    let trash_label = LabelId::from("imap:jason@example.com:Trash");
    assert_eq!(applied.provider_ids, vec![(a.id.clone(), pid("Trash", 1760400003, 1), trash_label.clone()), (b.id.clone(), pid("Trash", 1760400003, 2), trash_label)]);
    {
        let s = server.lock();
        assert_eq!(s.folder("Trash").messages.len(), 2);
        assert_eq!(s.folder("INBOX").messages.len(), 3);
        assert!(s.log.iter().any(|l| l.contains("UID COPY 4:5 \"Trash\"")) && s.log.iter().any(|l| l.contains("UID STORE 4:5 +FLAGS.SILENT (\\Deleted)")) && s.log.iter().any(|l| l.contains("UID EXPUNGE 4:5")), "{:?}", s.log);
        assert_eq!(s.commands("UID SEARCH HEADER"), 0, "COPYUID answered every message");
    }

    // Neither MOVE nor UIDPLUS: plain EXPUNGE, and the copies are found by Message-ID.
    let server = serve(FakeState { capabilities: "IMAP4rev1 SPECIAL-USE".into(), ..mailbox() });
    let p = provider(&server);
    let (labels, messages, _) = synced(&server, &p);
    let a = by_pid(&messages, "Work/Clients", 2);
    let untrash = op(&acct, OperationKind::Untrash { message_ids: ids(&[a]) });
    let applied = block(p.apply(&acct, &untrash, &[a.clone()], &labels)).unwrap();
    assert_eq!(applied.provider_ids, vec![(a.id.clone(), pid("INBOX", 1760400000, 6), LabelId::from("imap:jason@example.com:INBOX"))]);
    let s = server.lock();
    assert!(s.log.iter().any(|l| l == "a UID COPY 2 \"INBOX\"" || l.contains("UID COPY 2 \"INBOX\"")));
    assert!(s.log.iter().any(|l| l.ends_with(" EXPUNGE")), "{:?}", s.log);
    assert!(s.log.iter().any(|l| l.contains("UID SEARCH HEADER Message-ID \"<work-2@example.com>\"")), "{:?}", s.log);
    assert_eq!(s.folder("INBOX").messages[&6].header("Message-ID").as_deref(), Some("<work-2@example.com>"));
}

#[test]
fn trash_without_a_trash_folder_follows_the_policy() {
    let mut state = mailbox();
    state.folders.retain(|f| f.path != "Trash");
    let server = serve(state);
    let mut servers = provider(&server).servers().clone();
    servers.imap = Some(ImapPolicy { expunge_on_trash: true, ..Default::default() });
    let p = ImapProvider::new(servers, "hunter2").with_timeout(Duration::from_secs(5));
    let acct = account();
    let (labels, messages, _) = synced(&server, &p);
    let a = by_pid(&messages, "INBOX", 5);
    let applied = block(p.apply(&acct, &op(&acct, OperationKind::Trash { message_ids: ids(&[a]) }), &[a.clone()], &labels)).unwrap();
    assert_eq!(applied, AppliedChanges::default(), "an expunged message has no new identity");
    {
        let s = server.lock();
        assert!(!s.folder("INBOX").messages.contains_key(&5));
        assert!(!s.has_folder("Trash"));
        assert!(s.log.iter().any(|l| l.contains("UID STORE 5 +FLAGS.SILENT (\\Deleted)")) && s.log.iter().any(|l| l.contains("UID EXPUNGE 5")), "{:?}", s.log);
    }

    // Without the expunge policy the Trash folder is created on demand (the policy's
    // override name wins when set).
    let mut state = mailbox();
    state.folders.retain(|f| f.path != "Trash");
    let server = serve(state);
    let mut servers = provider(&server).servers().clone();
    servers.imap = Some(ImapPolicy { trash_folder: Some("Deleted Items".into()), ..Default::default() });
    let p = ImapProvider::new(servers, "hunter2").with_timeout(Duration::from_secs(5));
    let (labels, messages, _) = synced(&server, &p);
    let a = by_pid(&messages, "INBOX", 5);
    let applied = block(p.apply(&acct, &op(&acct, OperationKind::Trash { message_ids: ids(&[a]) }), &[a.clone()], &labels)).unwrap();
    let s = server.lock();
    assert!(s.has_folder("Deleted Items"), "{:?}", s.log);
    assert_eq!(applied.labels.iter().map(|l| (l.kind, l.name.as_str())).collect::<Vec<_>>(), vec![(LabelKind::Trash, "Trash")]);
    assert_eq!(applied.provider_ids[0].2, LabelId::from("imap:jason@example.com:Deleted Items"));
    assert!(s.log.iter().any(|l| l.contains("CREATE \"Deleted Items\"")), "{:?}", s.log);
}

#[test]
fn labels_are_folders_to_move_between() {
    let server = serve(mailbox());
    let p = provider(&server);
    let acct = account();
    let (labels, messages, _) = synced(&server, &p);
    let work = labels.iter().find(|l| l.name == "Work / Clients").unwrap();
    let a = by_pid(&messages, "INBOX", 3);
    let add = op(&acct, OperationKind::AddLabel { message_ids: ids(&[a]), label_id: work.id.clone() });
    let applied = block(p.apply(&acct, &add, &[a.clone()], &labels)).unwrap();
    assert_eq!(applied.provider_ids, vec![(a.id.clone(), pid("Work/Clients", 1760400002, 3), work.id.clone())]);
    assert!(server.lock().log.iter().any(|l| l.contains("UID MOVE 3 \"Work/Clients\"")));

    // Removing the folder label puts the message back in the Inbox; removing a label the
    // message does not carry does nothing.
    let mut moved = a.clone();
    moved.provider_id = Some(pid("Work/Clients", 1760400002, 3));
    let remove = op(&acct, OperationKind::RemoveLabel { message_ids: ids(&[&moved]), label_id: work.id.clone() });
    let applied = block(p.apply(&acct, &remove, &[moved.clone()], &labels)).unwrap();
    assert_eq!(applied.provider_ids, vec![(a.id.clone(), pid("INBOX", 1760400000, 6), LabelId::from("imap:jason@example.com:INBOX"))]);
    let b = by_pid(&messages, "INBOX", 4);
    let remove_other = op(&acct, OperationKind::RemoveLabel { message_ids: ids(&[b]), label_id: work.id.clone() });
    assert_eq!(block(p.apply(&acct, &remove_other, &[b.clone()], &labels)).unwrap(), AppliedChanges::default());

    // A label without a server folder cannot be applied.
    let local = Label { id: LabelId::from("imap:jason@example.com:nowhere"), account_id: acct.id.clone(), kind: LabelKind::User, name: "Nowhere".into(), provider_id: None, color: None, sort_order: 9 };
    let mut with_local = labels.clone();
    with_local.push(local.clone());
    let add = op(&acct, OperationKind::AddLabel { message_ids: ids(&[b]), label_id: local.id.clone() });
    assert!(matches!(block(p.apply(&acct, &add, &[b.clone()], &with_local)), Err(ProviderError::Rejected(_))));
}

#[test]
fn write_failures_map_to_provider_errors() {
    let server = serve(mailbox());
    let p = provider(&server);
    let acct = account();
    let (labels, messages, _) = synced(&server, &p);
    let a = by_pid(&messages, "INBOX", 1);
    let work = labels.iter().find(|l| l.name == "Work / Clients").unwrap();

    // The target folder vanished (and is not one we create on demand): Rejected.
    server.lock().folders.retain(|f| f.path != "Work/Clients");
    let add = op(&acct, OperationKind::AddLabel { message_ids: ids(&[a]), label_id: work.id.clone() });
    assert_eq!(block(p.apply(&acct, &add, &[a.clone()], &labels)), Err(ProviderError::Rejected("that folder no longer exists".into())));
    assert!(!server.lock().has_folder("Work/Clients"), "not created");

    // The source folder vanished: nothing to do, the next cycle removes the rows.
    let w = by_pid(&messages, "Work/Clients", 1);
    let star = op(&acct, OperationKind::SetStarred { message_ids: ids(&[w]), starred: true });
    assert_eq!(block(p.apply(&acct, &star, &[w.clone()], &labels)), Ok(AppliedChanges::default()));
    let archive = op(&acct, OperationKind::Archive { message_ids: ids(&[w]) });
    assert_eq!(block(p.apply(&acct, &archive, &[w.clone()], &labels)), Ok(AppliedChanges::default()));

    // Server-side trouble is temporary; a dropped connection is Offline (after one retry).
    let archive = op(&acct, OperationKind::Archive { message_ids: ids(&[a]) });
    server.lock().in_use.insert("INBOX".into());
    assert_eq!(block(p.apply(&acct, &archive, &[a.clone()], &labels)), Err(ProviderError::Temporary("Mailbox is in use by another process".into())));
    server.lock().in_use.clear();
    server.lock().fail_next = Some(("UID MOVE".into(), "NO [UNAVAILABLE] Mailbox is being rebuilt".into()));
    assert_eq!(block(p.apply(&acct, &archive, &[a.clone()], &labels)), Err(ProviderError::Temporary("Mailbox is being rebuilt".into())));
    server.lock().fail_next = Some(("UID MOVE".into(), "NO [LIMIT] Too many moves".into()));
    assert!(matches!(block(p.apply(&acct, &archive, &[a.clone()], &labels)), Err(ProviderError::Temporary(_))));
    {
        let mut s = server.lock();
        s.drop_next_command = true;
        s.login = Login::Bye;
    }
    assert_eq!(block(p.apply(&acct, &archive, &[a.clone()], &labels)), Err(ProviderError::Offline));
    server.lock().login = Login::AuthFailed;
    assert_eq!(block(p.apply(&acct, &archive, &[a.clone()], &labels)), Err(ProviderError::SignInRequired));
    assert!(server.lock().folder("INBOX").messages.contains_key(&1), "nothing moved");
}

#[test]
fn an_interrupted_move_is_completed_on_the_retry() {
    let server = serve(mailbox());
    let p = provider(&server);
    let acct = account();
    let (labels, messages, _) = synced(&server, &p);
    let (a, b) = (by_pid(&messages, "INBOX", 1), by_pid(&messages, "INBOX", 2));
    // The server performs the MOVE and the connection dies before the reply.
    server.lock().drop_after_next = Some("UID MOVE".into());
    let trash = op(&acct, OperationKind::Trash { message_ids: ids(&[a, b]) });
    let applied = block(p.apply(&acct, &trash, &[a.clone(), b.clone()], &labels)).unwrap();
    let trash_label = LabelId::from("imap:jason@example.com:Trash");
    assert_eq!(applied.provider_ids, vec![(a.id.clone(), pid("Trash", 1760400003, 1), trash_label.clone()), (b.id.clone(), pid("Trash", 1760400003, 2), trash_label)], "found again by Message-ID");
    let s = server.lock();
    assert_eq!(s.connections, 2, "reconnected once");
    assert_eq!(s.commands("UID MOVE"), 2);
    assert_eq!(s.folder("Trash").messages.len(), 2, "no duplicates");
    assert_eq!(s.commands("UID SEARCH HEADER"), 2);
}

// ----------------------------------------------------------------- sending

fn draft(acct: &Account, subject: &str) -> Draft {
    let mut d = Draft::new(acct.id.clone(), DraftKind::New, 0);
    d.to = vec![Address::named("Maya Chen", "maya@northstar.example")];
    d.cc = vec![Address::named("Alex", "alex@studio.example")];
    d.subject = subject.into();
    d.body_text = "A few things to cover.".into();
    d
}

fn sending_provider(server: &FakeServer, smtp: &crate::smtp::fake::FakeSmtp) -> ImapProvider {
    let mut servers = provider(server).servers().clone();
    servers.outgoing = ServerEndpoint::new("127.0.0.1", smtp.addr.port(), Security::None, "jason@example.com");
    ImapProvider::new(servers, "hunter2").with_timeout(Duration::from_secs(5)).with_outgoing_password(Some("smtp-secret".into()))
}

#[test]
fn send_submits_over_smtp_appends_to_sent_and_reconciles() {
    use crate::smtp::fake::{serve_many, Behaviour};
    let smtp = serve_many(Behaviour::Normal);
    let server = serve(mailbox());
    let p = sending_provider(&server, &smtp);
    let acct = account();
    let (labels, _, cp) = synced(&server, &p);
    let sent_label = labels.iter().find(|l| l.kind == LabelKind::Sent).unwrap().id.clone();
    let d = draft(&acct, "Agenda");
    let context = SendContext { sent_label: Some(sent_label.clone()), in_reply_to: None, labels: labels.clone() };
    let receipt = block(p.send(&acct, &d, context)).unwrap();

    let rfc_id = crate::draft_message_id(&d);
    assert_eq!(receipt.rfc_message_id.as_deref(), Some(rfc_id.as_str()));
    assert_eq!(receipt.provider_id, pid("Sent", 1760400001, 2), "APPENDUID names the Sent copy");
    let m = &receipt.message;
    assert!(m.id.as_str().starts_with("imap:jason@example.com:"));
    assert!(m.is_outgoing && m.is_read);
    assert_eq!(m.label_ids.iter().collect::<Vec<_>>(), vec![&sent_label]);
    assert_eq!(m.subject, "Agenda");
    {
        let seen = smtp.wait_for_connections(1);
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].auth, Some(("jason@example.com".into(), "smtp-secret".into())), "the outgoing password is used");
        assert_eq!(seen[0].rcpt, vec!["RCPT TO:<maya@northstar.example>", "RCPT TO:<alex@studio.example>"]);
        let data = seen[0].data.clone().unwrap();
        assert!(String::from_utf8_lossy(&data).contains(&format!("Message-ID: {rfc_id}")));
        let s = server.lock();
        let copy = &s.folder("Sent").messages[&2];
        assert!(copy.has_flag("\\Seen"));
        assert_eq!(copy.header("Message-ID").as_deref(), Some(rfc_id.as_str()));
        assert!(data.starts_with(&copy.raw), "the same bytes went to SMTP and to Sent");
    }
    assert_eq!(block(p.reconcile_send(&acct, &d)).unwrap(), SendOutcome::Delivered(pid("Sent", 1760400001, 2)));

    // The next cycle sees the Sent copy as a header-only row with the receipt's provider id,
    // which the sync engine adopts for the stored message.
    let changes = block(p.fetch_changes(&acct, &cp)).unwrap();
    assert_eq!(changes.upserts.iter().map(|m| m.provider_id.clone()).collect::<Vec<_>>(), vec![Some(pid("Sent", 1760400001, 2))]);
    assert_eq!(changes.upserts[0].rfc_message_id.as_deref(), Some(rfc_id.as_str()));
}

#[test]
fn a_dropped_send_is_uncertain_until_the_sent_folder_or_the_user_decides() {
    use crate::smtp::fake::{serve_many, Behaviour};
    let smtp = serve_many(Behaviour::DropAfterData);
    let server = serve(mailbox());
    let p = sending_provider(&server, &smtp);
    let acct = account();
    let (labels, _, _) = synced(&server, &p);
    let d = draft(&acct, "Dropped");
    let context = || SendContext { sent_label: None, in_reply_to: None, labels: labels.clone() };
    assert_eq!(block(p.send(&acct, &d, context())).err(), Some(ProviderError::Uncertain));
    assert_eq!(server.lock().folder("Sent").messages.len(), 1, "no Sent copy was appended");
    assert_eq!(block(p.reconcile_send(&acct, &d)).unwrap(), SendOutcome::Unknown, "the bytes left this machine; not finding a copy proves nothing");

    // A failure before DATA is a plain Offline; a reconcile after it says not delivered.
    smtp.set(Behaviour::DropDuringGreeting);
    assert_eq!(block(p.send(&acct, &d, context())).err(), Some(ProviderError::Offline));
    assert_eq!(block(p.reconcile_send(&acct, &d)).unwrap(), SendOutcome::NotDelivered);

    // Another process (no phase on record) stays Unknown; a copy in Sent is Delivered.
    let fresh = sending_provider(&server, &smtp);
    assert_eq!(block(fresh.reconcile_send(&acct, &d)).unwrap(), SendOutcome::Unknown);
    smtp.set(Behaviour::Normal);
    let receipt = block(fresh.send(&acct, &d, context())).unwrap();
    assert_eq!(block(p.reconcile_send(&acct, &d)).unwrap(), SendOutcome::Delivered(receipt.provider_id.clone()));
    assert_eq!(receipt.message.label_ids.iter().map(|l| l.as_str()).collect::<Vec<_>>(), vec!["imap:jason@example.com:Sent"], "the Sent label comes from the folder list when the context has none");

    // Rejections and sign-in problems come from the SMTP transport unchanged.
    smtp.set(Behaviour::RejectRecipient);
    assert!(matches!(block(p.send(&acct, &draft(&acct, "Bad"), context())), Err(ProviderError::Rejected(_))));
    smtp.set(Behaviour::RejectAuth);
    assert_eq!(block(p.send(&acct, &draft(&acct, "Auth"), context())).err(), Some(ProviderError::SignInRequired));
}

#[test]
fn a_failed_sent_copy_still_counts_as_sent() {
    use crate::smtp::fake::{serve_many, Behaviour};
    let smtp = serve_many(Behaviour::Normal);
    let server = serve(mailbox());
    let p = sending_provider(&server, &smtp);
    let acct = account();
    let (labels, _, _) = synced(&server, &p);
    let context = || SendContext { sent_label: None, in_reply_to: None, labels: labels.clone() };

    // The server refuses the APPEND: the message left, the copy is local.
    server.lock().fail_next = Some(("APPEND".into(), "NO [OVERQUOTA] Quota exceeded".into()));
    let d = draft(&acct, "Over quota");
    let receipt = block(p.send(&acct, &d, context())).unwrap();
    assert!(is_local_placeholder(&receipt.provider_id), "{:?}", receipt.provider_id);
    assert!(MessageRef::parse(&receipt.provider_id).is_none(), "never mistaken for a server message");
    assert_eq!(receipt.message.label_ids.iter().map(|l| l.as_str()).collect::<Vec<_>>(), vec!["imap:jason@example.com:Sent"]);
    assert_eq!(smtp.wait_for_connections(1).len(), 1);

    // The APPEND reply is lost: the retry finds the copy instead of appending twice.
    server.lock().drop_after_next = Some("APPEND".into());
    let d = draft(&acct, "Lost reply");
    let receipt = block(p.send(&acct, &d, context())).unwrap();
    assert_eq!(receipt.provider_id, pid("Sent", 1760400001, 2));
    let s = server.lock();
    assert_eq!(s.folder("Sent").messages.len(), 2, "one copy");
    assert_eq!(s.commands("APPEND"), 2);
    assert_eq!(smtp.wait_for_connections(2).len(), 2, "SMTP is never retried on its own");

    // No Sent folder at all: local copy with the context's label.
    drop(s);
    let mut state = mailbox();
    state.folders.retain(|f| f.path != "Sent");
    let server = serve(state);
    let p = sending_provider(&server, &smtp);
    let (labels, _, _) = synced(&server, &p);
    let receipt = block(p.send(&acct, &draft(&acct, "No folder"), SendContext { sent_label: Some(LabelId::from("imap:jason@example.com:sent-local")), in_reply_to: None, labels })).unwrap();
    assert!(is_local_placeholder(&receipt.provider_id));
    assert_eq!(receipt.message.label_ids.iter().map(|l| l.as_str()).collect::<Vec<_>>(), vec!["imap:jason@example.com:sent-local"]);
    assert_eq!(block(p.reconcile_send(&acct, &draft(&acct, "x"))).unwrap(), SendOutcome::Unknown, "nothing to search");
}

#[test]
fn a_dead_idle_connection_is_reopened_once() {
    let server = serve(mailbox());
    let p = provider(&server);
    block(p.probe()).unwrap();
    server.state.lock().unwrap().drop_next_command = true;
    block(p.probe()).unwrap();
    let state = server.state.lock().unwrap();
    assert_eq!(state.connections, 2);
    assert_eq!(state.commands(" LOGIN "), 2);
    drop(state);
    p.disconnect();
    assert!(server.state.lock().unwrap().log.iter().any(|l| l.contains("LOGOUT")));
}

/// Round trip against a real server. Run with
/// `OVERLOOK_IMAP_TEST_HOST=imap.example.com OVERLOOK_IMAP_TEST_USER=… OVERLOOK_IMAP_TEST_PASSWORD=…
/// cargo test -p overlook-provider real_imap -- --ignored --nocapture`; optional
/// `OVERLOOK_IMAP_TEST_PORT` and `OVERLOOK_IMAP_TEST_SECURITY=tls|starttls|none`.
#[test]
#[ignore]
fn real_imap_server_round_trip() {
    let var = |n: &str| std::env::var(n).ok().filter(|v| !v.trim().is_empty());
    let (Some(host), Some(user), Some(password)) = (var("OVERLOOK_IMAP_TEST_HOST"), var("OVERLOOK_IMAP_TEST_USER"), var("OVERLOOK_IMAP_TEST_PASSWORD")) else {
        panic!("set OVERLOOK_IMAP_TEST_HOST, OVERLOOK_IMAP_TEST_USER and OVERLOOK_IMAP_TEST_PASSWORD");
    };
    let security = var("OVERLOOK_IMAP_TEST_SECURITY").and_then(|s| Security::from_key(&s)).unwrap_or(Security::Tls);
    let port = var("OVERLOOK_IMAP_TEST_PORT").and_then(|p| p.parse().ok()).unwrap_or(ServerProtocol::Imap.default_port(security));
    let servers = MailServers { incoming: ServerEndpoint::new(host, port, security, user.clone()), outgoing: ServerEndpoint::new("", 0, security, user.clone()), pop3: None, imap: None };
    let p = ImapProvider::new(servers, password).with_inbox_limit(20);
    let acct = Account { id: AccountId(format!("imap:{user}")), provider: ProviderKind::Imap, display_name: user.clone(), email: user, created_at: 0 };
    block(p.probe()).unwrap();
    eprintln!("capabilities: {:?}", p.capabilities());
    let snap = block(p.fetch_snapshot(&acct)).unwrap();
    for l in &snap.labels {
        eprintln!("label {:?} {} ({:?})", l.kind, l.name, l.provider_id);
    }
    eprintln!("{} messages queued", snap.message_ids.len());
    let batch: Vec<ProviderId> = snap.message_ids.iter().take(20).cloned().collect();
    let messages = block(p.fetch_messages(&acct, &batch, &snap.labels)).unwrap();
    assert!(!messages.is_empty() || batch.is_empty());
    for m in &messages {
        eprintln!("{} | {} | {} attachments | text {}B html {}B", m.date, m.subject, m.attachments.len(), m.body.text.as_deref().map(str::len).unwrap_or(0), m.body.html.as_deref().map(str::len).unwrap_or(0));
    }
    if let Some((m, a)) = messages.iter().find_map(|m| m.attachments.first().map(|a| (m, a))) {
        let bytes = block(p.fetch_attachment(&acct, m, a)).unwrap();
        eprintln!("attachment {} ({}): {} bytes fetched, {} declared", a.filename, a.mime_type, bytes.len(), a.size_bytes);
        assert!(!bytes.is_empty());
    }
    let changes = block(p.fetch_changes(&acct, snap.checkpoint.as_deref().unwrap())).unwrap();
    eprintln!("changes: {} upserts, {} deleted, {} bodies queued", changes.upserts.len(), changes.deleted_provider_ids.len(), changes.message_ids.len());
    p.disconnect();
}

// ------------------------------------------------------- M5: fast paths, search, IDLE

fn condstore_mailbox(qresync: bool) -> FakeState {
    let mut state = mailbox();
    state.capabilities = format!("IMAP4rev1 UIDPLUS MOVE IDLE SPECIAL-USE LITERAL+ ENABLE CONDSTORE{}", if qresync { " QRESYNC" } else { "" });
    state
}

#[test]
fn qresync_changes_come_from_changedsince_and_vanished() {
    let server = serve(condstore_mailbox(true));
    let p = provider(&server);
    let acct = account();
    let snap = block(p.fetch_snapshot(&acct)).unwrap();
    let caps = p.capabilities();
    assert!(caps.condstore && caps.qresync && caps.literal_plus, "{caps:?}");
    assert!(server.lock().log.iter().any(|l| l.contains("ENABLE QRESYNC")));
    let cp = Checkpoint::parse(snap.checkpoint.as_deref().unwrap()).unwrap();
    assert!(cp.folders["INBOX"].highest_modseq > 0, "the snapshot records HIGHESTMODSEQ");
    let searches_before = server.lock().commands("UID SEARCH ALL");

    // A quiet cycle costs one EXAMINE per folder and nothing else.
    let quiet = block(p.fetch_changes(&acct, &cp.to_json())).unwrap();
    assert!(quiet.upserts.is_empty() && quiet.deleted_provider_ids.is_empty() && quiet.message_ids.is_empty());
    {
        let state = server.lock();
        assert_eq!(state.commands("UID SEARCH ALL"), searches_before, "no UID SEARCH ALL with QRESYNC");
        assert_eq!(state.commands("CHANGEDSINCE"), 0, "an unchanged HIGHESTMODSEQ skips the fetch");
    }
    let cp = quiet.checkpoint.unwrap();

    {
        let mut s = server.lock();
        let inbox = s.folder_mut("INBOX");
        inbox.set_flags(2, &["\\Seen"]);
        inbox.remove(3);
        inbox.add(FakeMessage::new(&[], "15-Oct-2026 08:00:00 +0000", msg(&["From: new@example.com", "To: jason@example.com", "Subject: Fresh", "Message-ID: <fresh@example.com>", "Date: Thu, 15 Oct 2026 08:00:00 +0000"], "new body\r\n")));
        // Touched but not flipped: a STORE that set a flag the cursor already knew.
        inbox.set_flags(1, &["\\Seen"]);
        s.folder_mut("Work/Clients").set_flags(1, &["\\Flagged"]);
    }
    let changes = block(p.fetch_changes(&acct, &cp)).unwrap();
    assert_eq!(changes.deleted_provider_ids, vec![pid("INBOX", 1760400000, 3)], "VANISHED reported the expunge");
    assert_eq!(changes.message_ids, vec![pid("INBOX", 1760400000, 6)]);
    let subjects: Vec<(&str, bool, bool)> = changes.upserts.iter().map(|m| (m.subject.as_str(), m.is_read, m.is_starred)).collect();
    assert_eq!(subjects, vec![("Fresh", false, false), ("Re: Plain hello", true, false), ("Contract", false, true)], "new mail, then flipped flags, in every folder every cycle");
    {
        let state = server.lock();
        assert_eq!(state.commands("UID SEARCH ALL"), searches_before, "deletions came from VANISHED");
        assert!(state.log.iter().any(|l| l.contains("(CHANGEDSINCE") && l.contains("VANISHED)")), "{:?}", state.log);
        assert!(state.log.iter().any(|l| l.contains("UID FETCH 6:* (UID)")), "new mail is found above the cursor: {:?}", state.log);
    }
    let parsed = Checkpoint::parse(changes.checkpoint.as_deref().unwrap()).unwrap();
    let inbox = &parsed.folders["INBOX"];
    assert_eq!((inbox.known.as_str(), inbox.seen.as_str(), inbox.flagged.as_str(), inbox.highest_uid), ("1:2,4:6", "1:2", "", 6));
    assert_eq!(inbox.highest_modseq, server.lock().folder("INBOX").highest_modseq);
    assert_eq!(parsed.folders["Work/Clients"].flagged, "1");

    // Another quiet cycle after the changes: still nothing, still no search.
    let again = block(p.fetch_changes(&acct, &parsed.to_json())).unwrap();
    assert!(again.upserts.is_empty() && again.deleted_provider_ids.is_empty());
    assert_eq!(server.lock().commands("UID SEARCH ALL"), searches_before);
}

#[test]
fn condstore_without_qresync_keeps_the_uid_diff_for_deletions() {
    let server = serve(condstore_mailbox(false));
    let p = provider(&server);
    let acct = account();
    let snap = block(p.fetch_snapshot(&acct)).unwrap();
    let caps = p.capabilities();
    assert!(caps.condstore && !caps.qresync, "{caps:?}");
    assert!(!server.lock().log.iter().any(|l| l.contains("ENABLE")), "nothing to enable");
    let cp = snap.checkpoint.unwrap();
    let searches_before = server.lock().commands("UID SEARCH ALL");
    {
        let mut s = server.lock();
        let inbox = s.folder_mut("INBOX");
        inbox.set_flags(2, &["\\Seen"]);
        inbox.remove(3);
    }
    let changes = block(p.fetch_changes(&acct, &cp)).unwrap();
    assert_eq!(changes.deleted_provider_ids, vec![pid("INBOX", 1760400000, 3)]);
    assert_eq!(changes.upserts.iter().map(|m| m.subject.as_str()).collect::<Vec<_>>(), vec!["Re: Plain hello"]);
    let state = server.lock();
    assert_eq!(state.commands("UID SEARCH ALL"), searches_before + 1, "the changed folder still gets the diff that finds deletions; STATUS shows the others unchanged");
    assert!(state.log.iter().any(|l| l.contains("(CHANGEDSINCE") && !l.contains("VANISHED")), "{:?}", state.log);
    assert!(!state.log.iter().any(|l| l.contains("(UID FLAGS))")), "no full flag refresh: {:?}", state.log);
}

#[test]
fn a_checkpoint_without_modseq_takes_the_plain_path_once() {
    let server = serve(condstore_mailbox(true));
    let p = provider(&server);
    let acct = account();
    let snap = block(p.fetch_snapshot(&acct)).unwrap();
    let mut cp = Checkpoint::parse(snap.checkpoint.as_deref().unwrap()).unwrap();
    for cursor in cp.folders.values_mut() {
        cursor.highest_modseq = 0;
    }
    server.lock().folder_mut("INBOX").set_flags(2, &["\\Seen"]);
    let before = server.lock().commands("UID SEARCH ALL");
    let changes = block(p.fetch_changes(&acct, &cp.to_json())).unwrap();
    assert_eq!(changes.upserts.iter().map(|m| m.subject.as_str()).collect::<Vec<_>>(), vec!["Re: Plain hello"]);
    assert_eq!(server.lock().commands("UID SEARCH ALL"), before + 3, "an old checkpoint is read the slow way");
    let parsed = Checkpoint::parse(changes.checkpoint.as_deref().unwrap()).unwrap();
    assert!(parsed.folders.values().all(|c| c.highest_modseq > 0), "and records MODSEQ for the next cycle");
}

#[test]
fn server_search_runs_text_search_per_folder() {
    let mut state = mailbox();
    state.folder_mut("Trash").add(FakeMessage::new(&[], "01-Oct-2026 00:00:00 +0000", msg(&["From: x@example.com", "Subject: Contract draft", "Message-ID: <trash-1@example.com>"], "old contract\r\n")));
    let server = serve(state);
    let p = provider(&server);
    let acct = account();
    let hits = block(p.search(&acct, "contract", 60)).unwrap();
    assert_eq!(hits, vec![pid("Work/Clients", 1760400002, 2), pid("Work/Clients", 1760400002, 1)], "newest first within a folder; Trash is not searched");
    let hits = block(p.search(&acct, "plain HELLO", 60)).unwrap();
    assert_eq!(hits, vec![pid("INBOX", 1760400000, 2), pid("INBOX", 1760400000, 1), pid("Sent", 1760400001, 1)], "Inbox first; every word must match");
    assert_eq!(block(p.search(&acct, "plain hello", 2)).unwrap().len(), 2, "the limit holds across folders");
    assert_eq!(block(p.search(&acct, "Grégoire", 60)).unwrap(), vec![pid("INBOX", 1760400000, 2)], "non-ASCII words go as literals");
    assert!(block(p.search(&acct, "   ", 60)).unwrap().is_empty());
    let state = server.lock();
    assert!(state.log.iter().any(|l| l.contains("UID SEARCH TEXT \"plain\" TEXT \"HELLO\"")), "{:?}", state.log);
    assert!(state.log.iter().any(|l| l.contains("UID SEARCH CHARSET UTF-8 TEXT \"Grégoire\"")), "the literal arrived: {:?}", state.log);
    assert!(!state.log.iter().any(|l| l.contains("EXAMINE \"Trash\"")));
}

fn idle_events(server: &FakeServer) -> (idle::IdleWatcher, std::sync::mpsc::Receiver<idle::IdleEvent>) {
    let (tx, rx) = std::sync::mpsc::channel();
    let watcher = provider(server).idle_watcher(move |event| {
        let _ = tx.send(event);
    });
    (watcher, rx)
}

#[test]
fn idle_reports_inbox_changes_and_stops_at_once() {
    let server = serve(mailbox());
    let (mut watcher, rx) = idle_events(&server);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while server.lock().commands(" IDLE") == 0 {
        assert!(std::time::Instant::now() < deadline, "IDLE was never issued: {:?}", server.lock().log);
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(rx.recv_timeout(Duration::from_millis(500)).is_err(), "nothing reported while nothing changes");
    server.lock().folder_mut("INBOX").add(FakeMessage::new(&[], "15-Oct-2026 08:00:00 +0000", msg(&["From: new@example.com", "Subject: Delivered", "Message-ID: <idle-1@example.com>"], "body\r\n")));
    assert_eq!(rx.recv_timeout(Duration::from_secs(5)), Ok(idle::IdleEvent::Changed), "new mail is reported within seconds: {:?}", server.lock().log);
    // A flag change made elsewhere is reported too, and IDLE is re-entered in between.
    server.lock().folder_mut("INBOX").set_flags(1, &[]);
    assert_eq!(rx.recv_timeout(Duration::from_secs(5)), Ok(idle::IdleEvent::Changed));
    {
        let state = server.lock();
        assert!(state.commands(" IDLE") >= 2, "{:?}", state.log);
        assert!(state.log.iter().any(|l| l.contains("EXAMINE \"INBOX\"")));
        assert_eq!(state.connections, 1, "one extra connection, never the provider's");
    }
    let started = std::time::Instant::now();
    watcher.stop();
    assert!(started.elapsed() < Duration::from_secs(2), "stopping shuts the socket down instead of waiting for the IDLE timeout");
    assert!(!watcher.is_running());
}

#[test]
fn idle_reconnects_after_a_dropped_connection() {
    let server = serve(mailbox());
    let (mut watcher, rx) = idle_events(&server);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while server.lock().commands(" IDLE") == 0 {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(20));
    }
    server.lock().kill_idle = true;
    // Backoff after a working session is the first step (2 s); the second connection then IDLEs again.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while server.lock().connections < 2 || server.lock().commands(" IDLE") < 2 {
        assert!(std::time::Instant::now() < deadline, "no reconnect: {:?}", server.lock().log);
        std::thread::sleep(Duration::from_millis(50));
    }
    server.lock().folder_mut("INBOX").add(FakeMessage::new(&[], "15-Oct-2026 08:00:00 +0000", msg(&["From: new@example.com", "Subject: After reconnect", "Message-ID: <idle-2@example.com>"], "body\r\n")));
    assert_eq!(rx.recv_timeout(Duration::from_secs(5)), Ok(idle::IdleEvent::Changed));
    watcher.stop();
}

#[test]
fn idle_ends_on_servers_without_it_or_with_a_refused_login() {
    let server = serve(FakeState { capabilities: "IMAP4rev1 UIDPLUS MOVE SPECIAL-USE".into(), ..mailbox() });
    let (watcher, rx) = idle_events(&server);
    assert_eq!(rx.recv_timeout(Duration::from_secs(5)), Ok(idle::IdleEvent::Unsupported));
    std::thread::sleep(Duration::from_millis(100));
    assert!(!watcher.is_running(), "polling is the fallback; the thread is gone");
    assert!(server.lock().log.iter().any(|l| l.contains("LOGOUT")));

    let server = serve(FakeState { login: Login::AuthFailed, ..mailbox() });
    let (watcher, rx) = idle_events(&server);
    assert_eq!(rx.recv_timeout(Duration::from_secs(5)), Ok(idle::IdleEvent::SignInRequired));
    std::thread::sleep(Duration::from_millis(100));
    assert!(!watcher.is_running());
    assert_eq!(server.lock().connections, 1, "a refused password is not retried");
}

/// The write path against a real server, for the interoperability matrix. Same variables as
/// [`real_imap_server_round_trip`]; run with
/// `cargo test -p overlook-provider real_imap_server_write -- --ignored --nocapture`.
/// Everything happens in a folder of its own (`Overlook-Test-<pid>`), created and deleted
/// here: a message is appended to it with a raw session, then the provider lists the new
/// folder, downloads the message, stars it, marks it read, moves it to Trash and expunges
/// it there, and the folder is deleted. Nothing else in the account is touched.
#[test]
#[ignore]
fn real_imap_server_write_round_trip() {
    let var = |n: &str| std::env::var(n).ok().filter(|v| !v.trim().is_empty());
    let (Some(host), Some(user), Some(password)) = (var("OVERLOOK_IMAP_TEST_HOST"), var("OVERLOOK_IMAP_TEST_USER"), var("OVERLOOK_IMAP_TEST_PASSWORD")) else {
        panic!("set OVERLOOK_IMAP_TEST_HOST, OVERLOOK_IMAP_TEST_USER and OVERLOOK_IMAP_TEST_PASSWORD");
    };
    let security = var("OVERLOOK_IMAP_TEST_SECURITY").and_then(|s| Security::from_key(&s)).unwrap_or(Security::Tls);
    let port = var("OVERLOOK_IMAP_TEST_PORT").and_then(|p| p.parse().ok()).unwrap_or(ServerProtocol::Imap.default_port(security));
    let endpoint = ServerEndpoint::new(host.clone(), port, security, user.clone());
    let servers = MailServers { incoming: endpoint.clone(), outgoing: ServerEndpoint::new("", 0, security, user.clone()), pop3: None, imap: None };
    let p = ImapProvider::new(servers, password.clone()).with_inbox_limit(20);
    let acct = Account { id: AccountId(format!("imap:{user}")), provider: ProviderKind::Imap, display_name: user.clone(), email: user, created_at: 0 };
    let pid = std::process::id();
    let rfc_id = format!("<write-test-{pid}@overlook.lastrefuge.ai>");

    // A raw session plays "another client": it makes the scratch folder and a message in it.
    let mut other = connection::connect(&endpoint, &password, Duration::from_secs(30)).unwrap();
    let delimiter = other.list(Some(""), Some("")).ok().and_then(|names| names.iter().next().and_then(|n| n.delimiter().map(str::to_owned))).unwrap_or_else(|| "/".into());
    let prefix = if other.list(Some(""), Some("*")).map(|names| names.iter().all(|n| n.name() == "INBOX" || n.name().starts_with(&format!("INBOX{delimiter}")))).unwrap_or(false) { format!("INBOX{delimiter}") } else { String::new() };
    let scratch = format!("{prefix}Overlook-Test-{pid}");
    other.create(&scratch).unwrap();
    let raw = format!("From: Overlook test <test@overlook.lastrefuge.ai>\r\nTo: {}\r\nSubject: Overlook IMAP write test {pid}\r\nMessage-ID: {rfc_id}\r\nDate: Wed, 15 Oct 2026 12:00:00 +0000\r\nMIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nThis message exists only for Overlook's write-path test and is deleted by it.\r\n", acct.email);
    other.append(&scratch, raw.as_bytes()).finish().unwrap();
    eprintln!("created {scratch:?} with one message");

    let outcome = (|| -> Result<(), String> {
        block(p.probe()).map_err(|e| e.to_string())?;
        eprintln!("capabilities: {:?}", p.capabilities());
        let snap = block(p.fetch_snapshot(&acct)).map_err(|e| format!("snapshot: {e}"))?;
        let labels = snap.labels.clone();
        let label = labels.iter().find(|l| l.provider_id.as_ref().is_some_and(|x| x.0 == scratch)).ok_or("the scratch folder was not listed")?.clone();
        let wanted: Vec<ProviderId> = snap.message_ids.iter().filter(|id| MessageRef::parse(id).is_some_and(|r| r.folder == scratch)).cloned().collect();
        if wanted.len() != 1 {
            return Err(format!("expected one queued message in {scratch}, got {}", wanted.len()));
        }
        let mut messages = block(p.fetch_messages(&acct, &wanted, &labels)).map_err(|e| format!("fetch: {e}"))?;
        let mut message = messages.pop().ok_or("the message did not download")?;
        if message.rfc_message_id.as_deref() != Some(rfc_id.as_str()) || message.label_ids.iter().next() != Some(&label.id) {
            return Err(format!("unexpected message: {:?} in {:?}", message.rfc_message_id, message.label_ids));
        }
        message.id = MessageId(format!("{}:write-test", acct.id.0));
        let ops = |kind: OperationKind| PendingOperation { id: OperationId::from("op"), account_id: acct.id.clone(), seq: 1, kind, state: OperationState::Pending, attempts: 0, last_error: None, created_at: 0, updated_at: 0 };
        block(p.apply(&acct, &ops(OperationKind::SetStarred { message_ids: vec![message.id.clone()], starred: true }), std::slice::from_ref(&message), &labels)).map_err(|e| format!("star: {e}"))?;
        block(p.apply(&acct, &ops(OperationKind::SetRead { message_ids: vec![message.id.clone()], read: true }), std::slice::from_ref(&message), &labels)).map_err(|e| format!("read: {e}"))?;
        let changes = block(p.fetch_changes(&acct, snap.checkpoint.as_deref().unwrap())).map_err(|e| format!("changes: {e}"))?;
        let seen = changes.upserts.iter().find(|m| m.rfc_message_id.as_deref() == Some(rfc_id.as_str())).ok_or("the flag changes were not reported back")?;
        if !(seen.is_read && seen.is_starred) {
            return Err(format!("flags did not stick: read {} starred {}", seen.is_read, seen.is_starred));
        }
        eprintln!("flags: \\Flagged and \\Seen stored and read back");
        let applied = block(p.apply(&acct, &ops(OperationKind::Trash { message_ids: vec![message.id.clone()] }), std::slice::from_ref(&message), &labels)).map_err(|e| format!("trash: {e}"))?;
        let (_, new_pid, trash_label) = applied.provider_ids.first().ok_or("the move reported no new provider id")?;
        let moved = MessageRef::parse(new_pid).ok_or("bad provider id after the move")?;
        eprintln!("moved to {:?} (uid {}, label {}){}", moved.folder, moved.uid, trash_label.0, if applied.labels.is_empty() { "" } else { ", Trash folder created on demand" });
        // Clean up the server copy: flag \Deleted and expunge just that UID.
        let mut session = connection::connect(&endpoint, &password, Duration::from_secs(30)).unwrap();
        session.select(&moved.folder).map_err(|e| e.to_string())?;
        let found = session.uid_search(format!("HEADER Message-ID \"{rfc_id}\"")).map_err(|e| e.to_string())?;
        if !found.contains(&moved.uid) {
            return Err(format!("the moved message was not found at uid {} in {}", moved.uid, moved.folder));
        }
        session.uid_store(moved.uid.to_string(), "+FLAGS.SILENT (\\Deleted)").map_err(|e| e.to_string())?;
        if session.uid_expunge(moved.uid.to_string()).is_err() {
            let _ = session.expunge();
        }
        let _ = session.logout();
        Ok(())
    })();
    other.delete(&scratch).unwrap();
    let _ = other.logout();
    p.disconnect();
    eprintln!("deleted {scratch:?}");
    outcome.unwrap();
}

#[test]
fn delete_forever_expunges_by_the_carried_provider_ids() {
    let server = serve(mailbox());
    let p = provider(&server);
    let acct = account();
    let (labels, messages, _) = synced(&server, &p);
    let (a, b) = (by_pid(&messages, "INBOX", 1), by_pid(&messages, "INBOX", 2));
    // The local rows are already gone, so the operation alone names the messages; a stale
    // UIDVALIDITY is skipped rather than deleting whatever now has that UID.
    let stale = pid("INBOX", 1, 3);
    let kind = OperationKind::DeleteForever { message_ids: ids(&[a, b]), provider_ids: vec![a.provider_id.clone().unwrap(), b.provider_id.clone().unwrap(), stale], folder: None };
    let applied = block(p.apply(&acct, &op(&acct, kind), &[], &labels)).unwrap();
    assert!(applied.provider_ids.is_empty() && applied.labels.is_empty());
    let s = server.lock();
    let inbox = s.folder("INBOX");
    assert!(!inbox.messages.contains_key(&1) && !inbox.messages.contains_key(&2), "{:?}", s.log);
    assert!(inbox.messages.contains_key(&3), "a stale UIDVALIDITY must not delete UID 3");
    assert!(s.log.iter().any(|l| l.contains("UID STORE 1:2 +FLAGS.SILENT (\\Deleted)")), "{:?}", s.log);
}

#[test]
fn emptying_a_folder_expunges_messages_the_client_never_downloaded() {
    let mut state = mailbox();
    for i in 1..=3 {
        state.folder_mut("Trash").add(FakeMessage::new(&[], "15-Oct-2026 10:00:00 +0000", msg(&["From: bot@example.com", "To: jason@example.com", &format!("Subject: Old {i}"), &format!("Message-ID: <old-{i}@example.com>"), "Date: Thu, 15 Oct 2026 10:00:00 +0000"], "body\r\n")));
    }
    let server = serve(state);
    let p = provider(&server);
    let acct = account();
    let (labels, _, _) = synced(&server, &p);
    // Only UID 3 made it into the local cache; the operation still empties the whole folder.
    let kind = OperationKind::DeleteForever { message_ids: vec![MessageId::from("m3")], provider_ids: vec![pid("Trash", 1760400003, 3)], folder: Some(LabelKind::Trash) };
    block(p.apply(&acct, &op(&acct, kind), &[], &labels)).unwrap();
    let s = server.lock();
    assert!(s.folder("Trash").messages.is_empty(), "{:?}", s.log);
    assert_eq!(s.folder("INBOX").messages.len(), 5, "other folders are untouched");
}

#[test]
fn folders_are_created_renamed_and_deleted_on_the_server() {
    let server = serve(mailbox());
    let p = provider(&server);
    let acct = account();
    let (labels, messages, _) = synced(&server, &p);
    assert!(p.manages_folders());
    let work = labels.iter().find(|l| l.provider_id.as_ref().is_some_and(|p| p.0 == "Work")).unwrap().clone();

    // Top level, and inside a folder with a non-ASCII name sent as modified UTF-7.
    let receipts = block(p.create_folder(&acct, "Receipts", None, &labels)).unwrap();
    assert_eq!((receipts.id.clone(), receipts.name.as_str(), receipts.kind), (LabelId::from("imap:jason@example.com:Receipts"), "Receipts", LabelKind::User));
    let drafts = block(p.create_folder(&acct, "Entwürfe", Some(&work), &labels)).unwrap();
    assert_eq!(drafts.provider_id, Some(ProviderId("Work/Entw&APw-rfe".into())));
    assert_eq!(drafts.name, "Work / Entwürfe");
    assert!(p.relist_pending());
    {
        let s = server.lock();
        assert!(s.has_folder("Receipts") && s.has_folder("Work/Entw&APw-rfe"), "{:?}", s.log);
        assert!(s.subscribed.contains("Receipts"));
    }

    // Names that would nest or clash are refused before anything is sent.
    let clash = block(p.create_folder(&acct, "receipts", None, &labels)).unwrap_err();
    assert!(matches!(&clash, ProviderError::Rejected(m) if m.contains("already")), "{clash:?}");
    let nested = block(p.create_folder(&acct, "a/b", None, &labels)).unwrap_err();
    assert!(matches!(&nested, ProviderError::Rejected(m) if m.contains('/')), "{nested:?}");

    // Rename the folder that is open, with a child folder: the child moves too.
    let clients = by_pid(&messages, "Work/Clients", 2);
    block(p.fetch_raw(&acct, clients)).unwrap();
    let renamed = block(p.rename_folder(&acct, &work, "Jobs", &labels)).unwrap();
    assert_eq!((renamed[0].new.id.clone(), renamed[0].new.name.as_str()), (LabelId::from("imap:jason@example.com:Jobs"), "Jobs"));
    let moved: Vec<(&str, &str)> = renamed.iter().map(|r| (r.old.provider_id.as_ref().unwrap().0.as_str(), r.new.provider_id.as_ref().unwrap().0.as_str())).collect();
    assert_eq!(moved, vec![("Work", "Jobs"), ("Work/Clients", "Jobs/Clients")], "the folder inside is reported too");
    {
        let s = server.lock();
        assert!(s.has_folder("Jobs") && s.has_folder("Jobs/Clients") && !s.has_folder("Work"), "{:?}", s.log);
    }

    // System folders can't be renamed or deleted; a user folder can.
    let inbox = labels.iter().find(|l| l.kind == LabelKind::Inbox).unwrap();
    assert!(block(p.rename_folder(&acct, inbox, "Mail", &labels)).is_err());
    assert!(block(p.delete_folder(&acct, inbox)).is_err());
    block(p.delete_folder(&acct, &receipts)).unwrap();
    let s = server.lock();
    assert!(!s.has_folder("Receipts") && !s.subscribed.contains("Receipts"), "{:?}", s.log);
}

/// A message for a folder made in the middle of a test.
fn note(id: &str) -> FakeMessage {
    FakeMessage::new(&[], "15-Oct-2026 10:00:00 +0000", msg(&["From: shop@example.com", "To: jason@example.com", &format!("Subject: {id}"), &format!("Message-ID: <{id}@example.com>"), "Date: Thu, 15 Oct 2026 10:00:00 +0000"], "body\r\n"))
}

/// `EXAMINE`s since `from` in the server log, by folder.
fn examined(server: &FakeServer, from: usize) -> Vec<String> {
    server.lock().log[from..].iter().filter_map(|l| l.split_once(" EXAMINE ").map(|(_, p)| p.trim_matches('"').to_owned())).collect()
}

#[test]
fn list_status_finds_new_folders_and_opens_only_changed_ones() {
    let mut state = condstore_mailbox(true);
    state.capabilities.push_str(" LIST-STATUS");
    let server = serve(state);
    let p = provider(&server);
    let acct = account();
    let mut cp = block(p.fetch_snapshot(&acct)).unwrap().checkpoint.unwrap();
    assert!(p.capabilities().list_status);
    // The first cycle after the snapshot records each folder's UIDNEXT and count.
    cp = block(p.fetch_changes(&acct, &cp)).unwrap().checkpoint.unwrap();

    // Nothing changed: one LIST, no STATUS, and at most the folder left open is read.
    let mark = server.lock().log.len();
    let quiet = block(p.fetch_changes(&acct, &cp)).unwrap();
    assert!(quiet.labels.is_none() && quiet.message_ids.is_empty() && quiet.deleted_provider_ids.is_empty());
    assert!(examined(&server, mark).len() <= 1, "{:?}", examined(&server, mark));
    {
        let s = server.lock();
        assert_eq!(s.log[mark..].iter().filter(|l| l.contains("LIST \"\" \"*\" RETURN (SUBSCRIBED STATUS (MESSAGES UIDNEXT UIDVALIDITY HIGHESTMODSEQ))")).count(), 1, "{:?}", &s.log[mark..]);
        assert!(!s.log[mark..].iter().any(|l| l.contains(" STATUS \"")), "no per-folder STATUS");
    }
    cp = quiet.checkpoint.unwrap();

    // A folder made by another client, and new mail in Work: both show at the next sync,
    // and only those two folders are opened.
    {
        let mut s = server.lock();
        let mut receipts = FakeFolder::new("Receipts", &["\\HasNoChildren"], 1_900_000_000);
        receipts.add(note("receipt-1"));
        s.folders.push(receipts);
        s.folder_mut("Work").add(note("work-3"));
    }
    let mark = server.lock().log.len();
    let changes = block(p.fetch_changes(&acct, &cp)).unwrap();
    let names: Vec<String> = changes.labels.as_ref().expect("the label list changed").iter().map(|l| l.name.clone()).collect();
    assert!(names.contains(&"Receipts".to_owned()), "{names:?}");
    assert!(changes.message_ids.contains(&pid("Receipts", 1_900_000_000, 1)), "{:?}", changes.message_ids);
    assert!(changes.upserts.iter().any(|m| m.subject == "work-3"));
    let mut opened = examined(&server, mark);
    opened.sort();
    opened.dedup();
    assert_eq!(opened, vec!["Receipts".to_owned(), "Work".to_owned()]);

    // A folder removed elsewhere drops out (with its messages) at the next sync too.
    cp = changes.checkpoint.unwrap();
    server.lock().folders.retain(|f| f.path != "Receipts");
    let gone = block(p.fetch_changes(&acct, &cp)).unwrap();
    assert!(!gone.labels.unwrap().iter().any(|l| l.name == "Receipts"));
    assert_eq!(gone.deleted_provider_ids, vec![pid("Receipts", 1_900_000_000, 1)]);
}

#[test]
fn without_list_status_each_folder_gets_a_status_and_only_changed_ones_are_opened() {
    let server = serve(condstore_mailbox(false));
    let p = provider(&server);
    let acct = account();
    let mut cp = block(p.fetch_snapshot(&acct)).unwrap().checkpoint.unwrap();
    assert!(!p.capabilities().list_status);
    cp = block(p.fetch_changes(&acct, &cp)).unwrap().checkpoint.unwrap();

    server.lock().folder_mut("Sent").add(note("sent-2"));
    let mark = server.lock().log.len();
    let changes = block(p.fetch_changes(&acct, &cp)).unwrap();
    assert!(changes.upserts.iter().any(|m| m.subject == "sent-2"));
    let opened: Vec<String> = examined(&server, mark).into_iter().filter(|p| p != "Sent").collect();
    assert!(opened.len() <= 1, "only Sent, plus perhaps the folder left open: {opened:?}");
    let s = server.lock();
    let statuses = s.log[mark..].iter().filter(|l| l.contains(" STATUS \"")).count();
    assert!(statuses >= 3, "a STATUS per folder instead of opening it: {:?}", &s.log[mark..]);
    assert_eq!(s.log[mark..].iter().filter(|l| l.contains(" LIST ")).count(), 0, "the folder list only hourly without LIST-STATUS");
}

#[test]
fn folders_move_with_their_children_and_never_into_themselves() {
    let server = serve(mailbox());
    let p = provider(&server);
    let acct = account();
    let (labels, _, _) = synced(&server, &p);
    let work = labels.iter().find(|l| l.provider_id.as_ref().is_some_and(|p| p.0 == "Work")).unwrap().clone();
    let clients = labels.iter().find(|l| l.provider_id.as_ref().is_some_and(|p| p.0 == "Work/Clients")).unwrap().clone();
    let inbox = labels.iter().find(|l| l.kind == LabelKind::Inbox).unwrap().clone();

    // Into itself or its own child: refused before anything is sent.
    assert!(matches!(block(p.move_folder(&acct, &work, Some(&work), &labels)), Err(ProviderError::Rejected(m)) if m.contains("itself")));
    assert!(matches!(block(p.move_folder(&acct, &work, Some(&clients), &labels)), Err(ProviderError::Rejected(m)) if m.contains("itself")));

    // Into the Inbox: the child goes with it.
    let changed = block(p.move_folder(&acct, &work, Some(&inbox), &labels)).unwrap();
    let moved = changed[0].new.clone();
    assert_eq!((moved.provider_id.clone(), moved.name.as_str()), (Some(ProviderId("INBOX/Work".into())), "Inbox / Work"));
    assert_eq!(changed[1].new.provider_id, Some(ProviderId("INBOX/Work/Clients".into())));
    {
        let s = server.lock();
        assert!(s.has_folder("INBOX/Work") && s.has_folder("INBOX/Work/Clients") && !s.has_folder("Work"), "{:?}", s.log);
    }

    // Back to the top level.
    let back = block(p.move_folder(&acct, &moved, None, &labels)).unwrap();
    assert_eq!(back[0].new.provider_id, Some(ProviderId("Work".into())));
    assert!(server.lock().has_folder("Work/Clients"));
}

#[test]
fn a_verifying_cycle_opens_every_folder_and_lists_every_message() {
    let mut state = condstore_mailbox(true);
    state.capabilities.push_str(" LIST-STATUS");
    let server = serve(state);
    let p = provider(&server);
    let acct = account();
    let mut cp = block(p.fetch_snapshot(&acct)).unwrap().checkpoint.unwrap();
    cp = block(p.fetch_changes(&acct, &cp)).unwrap().checkpoint.unwrap();
    server.lock().folder_mut("Work/Clients").set_flags(1, &["\\Seen", "\\Flagged"]);

    let mark = server.lock().log.len();
    let checked = block(p.verify_changes(&acct, &cp)).unwrap();
    assert!(checked.labels.is_some(), "the label list is always reported");
    let mut opened = examined(&server, mark);
    opened.sort();
    opened.dedup();
    assert_eq!(opened, vec!["INBOX", "Sent", "Trash", "Work", "Work/Clients"], "every selectable mail folder, changed or not");
    {
        let s = server.lock();
        assert!(!s.log[mark..].iter().any(|l| l.contains("CHANGEDSINCE")), "flags come from a full fetch, not the CONDSTORE shortcut");
    }
    let complete = checked.complete.expect("a verifying cycle lists everything");
    let known: usize = Checkpoint::parse(checked.checkpoint.as_deref().unwrap()).unwrap().folders.values().map(|c| c.known_set().len()).sum();
    assert_eq!(complete.len(), known);
    let work = complete.iter().find(|s| s.provider_id == pid("Work/Clients", 1760400002, 1)).unwrap();
    assert!(work.read && work.starred, "{work:?}");

    // An ordinary cycle has no full list.
    let cp = checked.checkpoint.unwrap();
    assert!(block(p.fetch_changes(&acct, &cp)).unwrap().complete.is_none());
}

#[test]
fn a_renamed_folder_keeps_its_cursor_and_its_messages() {
    let server = serve(mailbox());
    let p = provider(&server);
    let acct = account();
    let (labels, _, cp) = synced(&server, &p);
    let cp = block(p.fetch_changes(&acct, &cp)).unwrap().checkpoint.unwrap();
    let work = labels.iter().find(|l| l.provider_id.as_ref().is_some_and(|p| p.0 == "Work")).unwrap().clone();
    let held = Checkpoint::parse(&cp).unwrap().folders["Work/Clients"].known_set();
    assert!(!held.is_empty());

    block(p.rename_folder(&acct, &work, "Jobs", &labels)).unwrap();
    let changes = block(p.fetch_changes(&acct, &cp)).unwrap();
    assert!(changes.deleted_provider_ids.is_empty(), "nothing was deleted: {:?}", changes.deleted_provider_ids);
    assert!(changes.message_ids.is_empty(), "nothing is downloaded again: {:?}", changes.message_ids);
    let after = Checkpoint::parse(changes.checkpoint.as_deref().unwrap()).unwrap();
    assert!(!after.folders.contains_key("Work/Clients"));
    assert_eq!(after.folders["Jobs/Clients"].known_set(), held, "the cursor moved with the folder");
}

#[test]
fn a_verifying_cycle_reports_mail_beyond_the_window_as_untracked() {
    let server = serve(mailbox());
    let p = provider(&server);
    let acct = account();
    let (_, _, cp) = synced(&server, &p);
    // Pretend INBOX 1 is older than the window: the server has it, the cursor doesn't.
    let mut parsed = Checkpoint::parse(&cp).unwrap();
    let inbox = parsed.folders.get_mut("INBOX").unwrap();
    let mut known = inbox.known_set();
    assert!(known.remove(&1));
    inbox.known = uidset::encode(&known);
    let checked = block(p.verify_changes(&acct, &parsed.to_json())).unwrap();
    let old = pid("INBOX", 1760400000, 1);
    assert!(!checked.complete.unwrap().iter().any(|s| s.provider_id == old));
    assert!(checked.untracked.contains(&old), "{:?}", checked.untracked);
}
