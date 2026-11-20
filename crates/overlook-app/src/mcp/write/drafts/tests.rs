use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;

use base64::{engine::general_purpose::STANDARD, Engine};
use overlook_core::{
    prefs::Prefs, Account, Attachment, AttachmentId, DraftId, DraftSource, Message,
};
use overlook_provider::sample::sample_mailbox;
use overlook_store::{McpWriteKey, McpWriteOutcome, Store};

use super::*;

struct Fixture {
    account: Account,
    store: Arc<Store>,
    dir: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let store = Arc::new(Store::open_in_memory().unwrap());
        let account = crate::seed_sample_account(&store).unwrap();
        Self {
            account,
            store,
            dir: tempfile::tempdir().unwrap(),
        }
    }

    fn ctx(&self) -> WriteContext<'_> {
        WriteContext {
            account: &self.account,
            account_id: "test-account",
            profile: self.dir.path(),
            key: &[5; 32],
        }
    }

    fn run(&self, tool: &str, key: &str, args: Value) -> Result<McpWriteOutcome, Failure> {
        let tool = super::tools().into_iter().find(|t| t.name == tool).unwrap();
        self.ctx().execute(
            &self.store,
            "fixture",
            &tool,
            key,
            args.as_object().unwrap(),
        )
    }

    fn call(&self, tool: &str, key: &str, args: Value) -> Value {
        match self.run(tool, key, args).unwrap() {
            McpWriteOutcome::Applied {
                data,
                queued: false,
            } => data,
            _ => panic!("expected a local draft write"),
        }
    }

    fn has_error(&self, code: ErrorCode, tool: &str, key: &str, args: Value) {
        assert_eq!(self.run(tool, key, args).err().unwrap().code, code);
    }

    fn draft(&self, value: &Value) -> Draft {
        self.store
            .drafts_for_account(&self.account.id)
            .unwrap()
            .into_iter()
            .find(|d| value["draft_id"] == self.ctx().id("draft", d.id.as_str()))
            .unwrap()
    }

    fn source(&self) -> Message {
        sample_mailbox(&self.account, now()).messages.remove(0)
    }
}

fn target(value: &Value) -> Value {
    json!({"draft_id": value["draft_id"], "if_revision": value["revision"]})
}

fn upload(value: &Value, title: &str, bytes: &[u8]) -> Value {
    let mut args = target(value);
    args["filename"] = json!(title);
    args["mime_type"] = json!("text/plain");
    args["content_base64"] = json!(STANDARD.encode(bytes));
    args
}

#[test]
fn schemas_require_write_envelope_and_edit_preconditions() {
    let expected: [(&str, &[&str]); 8] = [
        (CREATE, &[]),
        (UPDATE, &["draft_id", "if_revision"]),
        (REPLY, &["message_id"]),
        (REPLY_ALL, &["message_id"]),
        (FORWARD, &["message_id"]),
        (DISCARD, &["draft_id", "if_revision"]),
        (
            ADD,
            &[
                "draft_id",
                "if_revision",
                "filename",
                "mime_type",
                "content_base64",
            ],
        ),
        (REMOVE, &["draft_id", "if_revision", "attachment_id"]),
    ];
    let tools = tools();
    assert_eq!(tools.len(), expected.len());
    for (name, required) in expected {
        let tool = tools.iter().find(|t| t.name == name).unwrap();
        assert_eq!(tool.permission, WritePermission::Modify);
        let fields = required
            .iter()
            .copied()
            .chain(["account_id", "idempotency_key"])
            .collect::<Vec<_>>();
        assert_eq!(tool.schema["required"], json!(fields));
        assert_eq!(tool.schema["additionalProperties"], false);
    }
}

#[test]
fn create_update_and_discard_are_revision_checked_and_replayable() {
    let f = Fixture::new();
    let create = json!({"subject":"Fixture", "body_text":"**Original**", "to":[{"email":"test@example.com"}]});
    let original = f.call(CREATE, "create", create.clone());
    assert_eq!(original["status"], "saved");
    assert!(
        matches!(f.run(CREATE, "create", create).unwrap(), McpWriteOutcome::Replayed(value) if value == original)
    );
    let mut update = target(&original);
    update["body_text"] = json!("Modified");
    update["cc"] = json!([{"name":"QA", "email":"qa@example.com"}]);
    let updated = f.call(UPDATE, "update", update.clone());
    assert_ne!(updated["revision"], original["revision"]);
    assert_eq!(updated["draft_id"], original["draft_id"]);
    f.has_error(ErrorCode::Conflict, UPDATE, "stale", update);
    f.has_error(
        ErrorCode::Conflict,
        DISCARD,
        "stale-discard",
        target(&original),
    );
    let draft = f.draft(&updated);
    assert_eq!(draft.subject, "Fixture");
    assert_eq!(draft.to[0].email, "test@example.com");
    assert_eq!(draft.cc[0].email, "qa@example.com");
    assert_eq!(draft.body_text, "Modified");
    assert_eq!(f.ctx().draft_revision(&draft), updated["revision"]);

    let discarded = f.call(DISCARD, "discard", target(&updated));
    assert_eq!(discarded["status"], "discarded");
    assert_eq!(discarded["revision"], updated["revision"]);
    assert!(f.store.draft(&draft.id).unwrap().is_none());
    assert!(
        matches!(f.run(DISCARD, "discard", target(&updated)).unwrap(), McpWriteOutcome::Replayed(value) if value == discarded)
    );
}

#[test]
fn edits_protect_open_and_outbox_rows_and_preserve_imported_metadata() {
    let f = Fixture::new();
    let created = f.call(
        CREATE,
        "create",
        json!({"to":[{"email":"test@example.com"}]}),
    );
    let mut draft = f.draft(&created);
    draft.imported = Some(DraftSource {
        text: draft.body_text.clone(),
        html: Some("<b>original</b>".into()),
        in_reply_to: Some("<original@example.com>".into()),
        references: vec![],
    });
    draft.body_html = draft.imported.as_ref().unwrap().html.clone();
    draft.signature_html = Some("<p>Signature</p>".into());
    f.store.save_draft(&draft).unwrap();
    let current = result(&f.ctx(), &draft, "saved");
    let mut update = target(&current);
    update["subject"] = json!("Edited subject");
    let _guard = f.store.edit_draft(&draft.id);
    f.has_error(ErrorCode::Conflict, UPDATE, "open-edit", update.clone());
    f.has_error(
        ErrorCode::Conflict,
        DISCARD,
        "open-discard",
        target(&current),
    );
    drop(_guard);
    let changed = f.call(UPDATE, "changed", update.clone());
    let draft = f.draft(&changed);
    assert_eq!(
        draft.imported.unwrap().in_reply_to.as_deref(),
        Some("<original@example.com>")
    );
    assert_eq!(draft.body_html.as_deref(), Some("<b>original</b>"));
    assert_eq!(draft.signature_html.as_deref(), Some("<p>Signature</p>"));

    let mut draft = f.draft(&changed);
    for state in [
        DraftState::Queued,
        DraftState::Sending,
        DraftState::Uncertain,
        DraftState::Failed(String::new()),
    ] {
        draft.state = state;
        f.store.save_draft(&draft).unwrap();
        update["if_revision"] = json!(f.ctx().draft_revision(&draft));
        f.has_error(ErrorCode::Conflict, UPDATE, "blocked", update.clone());
        f.has_error(
            ErrorCode::Conflict,
            ADD,
            "blocked-upload",
            upload(&result(&f.ctx(), &draft, "saved"), "notes.txt", b"notes"),
        );
    }
}

#[test]
fn replies_use_threading_recipients_and_quote_preferences_and_forward_cached_files() {
    let f = Fixture::new();
    let mut source = f.source();
    source.from = Address::named("Sender", "sender@example.com");
    source.reply_to = Some(Address::named("Reply", "reply@example.com"));
    source.to = vec![
        Address::new(None, f.account.email.clone()),
        Address::new(None, "other@example.com"),
    ];
    source.cc = vec![
        Address::new(None, "OTHER@example.com"),
        Address::new(None, "cc@example.com"),
    ];
    source.bcc = vec![Address::new(None, "hidden@example.com")];
    source.is_outgoing = false;
    source.subject = "Re: Fwd: Test".into();
    let file = f
        .dir
        .path()
        .join("attachments/sample/sample_msg-1/fixture/notes.txt");
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, b"forwarded bytes").unwrap();
    let cached = Attachment {
        id: AttachmentId::from("fixture"),
        message_id: source.id.clone(),
        filename: "notes.txt".into(),
        mime_type: "text/plain".into(),
        size_bytes: 15,
        local_path: Some(file.to_string_lossy().into_owned()),
        provider_id: None,
        content_id: None,
    };
    source.attachments = vec![
        cached.clone(),
        Attachment {
            id: AttachmentId::from("uncached"),
            local_path: None,
            ..cached
        },
    ];
    f.store.upsert_messages(&[source.clone()]).unwrap();
    f.store
        .set_setting(
            "prefs",
            &Prefs {
                quote_on_reply: false,
                ..Prefs::default()
            },
        )
        .unwrap();
    let args = json!({"message_id":f.ctx().id("mail_message", source.id.as_str())});
    let reply = f.draft(&f.call(REPLY, "reply", args.clone()));
    assert_eq!(reply.to, vec![source.reply_to.unwrap()]);
    assert!(reply.cc.is_empty());
    assert!(!reply.include_quote);
    assert_eq!(reply.subject, "Re: Test");
    assert_eq!(
        reply.conversation_id.as_ref(),
        Some(&source.conversation_id)
    );
    assert_eq!(reply.in_reply_to.as_ref(), Some(&source.id));
    let mut reply_all = args.clone();
    reply_all["include_quote"] = json!(true);
    let reply_all = f.draft(&f.call(REPLY_ALL, "reply-all", reply_all));
    assert!(reply_all.include_quote);
    assert!(reply_all.bcc.is_empty());
    assert_eq!(
        reply_all
            .cc
            .into_iter()
            .map(|a| a.email)
            .collect::<Vec<_>>(),
        ["other@example.com", "cc@example.com"]
    );
    let forward = f.call(FORWARD, "forward", args);
    assert_eq!(forward["attachments_skipped"], 1);
    let draft = f.draft(&forward);
    assert!(draft.to.is_empty());
    assert!(draft.include_quote);
    assert_eq!(draft.subject, "Fwd: Test");
    assert_eq!(draft.attachments.len(), 1);
    assert_eq!(
        fs::read(&draft.attachments[0].local_path).unwrap(),
        b"forwarded bytes"
    );
    let path = Path::new(&draft.attachments[0].local_path);
    assert!(path.exists());
    f.call(DISCARD, "discard-forward", target(&forward));
    assert!(!path.exists());
    assert!(file.exists());
}

#[test]
fn attachments_are_bounded_private_and_rolled_back_with_failed_receipts() {
    let f = Fixture::new();
    let created = f.call(CREATE, "create", json!({}));
    let draft = f.draft(&created);
    let add = upload(&created, "plan\u{202e}.txt", b"notes");
    let failure = f.store.mcp_write::<Failure>(
        McpWriteKey {
            account: &f.account.id,
            client: "fixture",
            key_digest: &[1; 32],
            request_digest: &[2; 32],
            action: ADD,
        },
        |tx, _| {
            execute(&f.ctx(), tx, ADD, add.as_object().unwrap())?;
            Err(invalid("simulated failure after staging"))
        },
    );
    assert_eq!(failure.err().unwrap().code, ErrorCode::InvalidRequest);
    let staged = f.dir.path().join("outgoing").join(draft.id.as_str());
    assert_eq!(staged.read_dir().unwrap().count(), 0);
    assert!(f.draft(&created).attachments.is_empty());

    for (filename, bytes, code) in [
        (
            "../notes.txt",
            b"notes".as_slice(),
            ErrorCode::InvalidRequest,
        ),
        ("notes.txt", b"#!/bin/sh".as_slice(), ErrorCode::Unsupported),
        ("notes.EXE", b"notes".as_slice(), ErrorCode::Unsupported),
    ] {
        f.has_error(
            code,
            ADD,
            "invalid-upload",
            upload(&created, filename, bytes),
        );
    }
    f.has_error(
        ErrorCode::InvalidRequest,
        ADD,
        "too-large",
        upload(&created, "notes.txt", &vec![0; 8193]),
    );
    let attached = f.call(ADD, "add", add.clone());
    assert_eq!(attached["filename"], "plan.txt");
    let draft = f.draft(&attached);
    let item = &draft.attachments[0];
    let path = Path::new(&item.local_path);
    assert!(path.starts_with(&staged));
    assert_eq!(fs::read(path).unwrap(), b"notes");
    assert_eq!(
        fs::metadata(path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(!attached.to_string().contains(&item.local_path));
    assert_ne!(attached["attachment_id"], item.id.as_str());
    assert!(
        matches!(f.run(ADD, "add", add).unwrap(), McpWriteOutcome::Replayed(value) if value == attached)
    );
    assert_eq!(staged.read_dir().unwrap().count(), 1);

    let mut remove = target(&created);
    remove["attachment_id"] = attached["attachment_id"].clone();
    f.has_error(ErrorCode::Conflict, REMOVE, "stale-remove", remove.clone());
    assert!(path.exists());
    remove["if_revision"] = attached["revision"].clone();
    let removed = f.call(REMOVE, "remove", remove);
    assert_eq!(removed["attachment_id"], attached["attachment_id"]);
    assert!(!path.exists());
    assert!(f.draft(&removed).attachments.is_empty());
    assert_eq!(f.draft(&removed).id, DraftId::from(draft.id.as_str()));
}
