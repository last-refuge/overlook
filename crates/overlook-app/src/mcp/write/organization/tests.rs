use super::*;
use overlook_core::{
    Account, AccountId, Address, ConversationId, LabelId, Message, MessageId, ProviderId,
};
use overlook_store::{McpWriteOutcome, Store};
use std::path::Path;

struct Fixture {
    store: Store,
    account: Account,
}

impl Fixture {
    fn new(provider: ProviderKind) -> Self {
        let store = Store::open_in_memory().unwrap();
        let account = Account {
            id: AccountId::from("sample"),
            provider,
            display_name: "Sample".into(),
            email: "owner@example.test".into(),
            created_at: 1,
        };
        store.upsert_account(&account).unwrap();
        for (sort_order, (id, kind)) in [
            ("inbox", LabelKind::Inbox),
            ("archive", LabelKind::Archive),
            ("trash", LabelKind::Trash),
            ("junk", LabelKind::Spam),
            ("work", LabelKind::User),
        ]
        .into_iter()
        .enumerate()
        {
            store
                .upsert_label(&Label {
                    id: LabelId::from(id),
                    kind,
                    account_id: account.id.clone(),
                    name: id.to_owned(),
                    color: None,
                    sort_order: sort_order as i32,
                    provider_id: (provider == ProviderKind::Imap).then(|| ProviderId::from(id)),
                })
                .unwrap();
        }
        Self { store, account }
    }

    fn context(&self) -> WriteContext<'_> {
        WriteContext {
            account: &self.account,
            account_id: "opaque-account",
            profile: Path::new("."),
            key: &[0; 32],
        }
    }

    fn put_message(&self, id: &str, folder: &str) {
        self.store
            .upsert_messages(&[Message {
                id: MessageId::from(id),
                account_id: self.account.id.clone(),
                conversation_id: ConversationId::from(id),
                provider_id: Some(ProviderId::from(id)),
                from: Address::named("Sender", "Sender@example.test"),
                to: Vec::new(),
                cc: Vec::new(),
                bcc: Vec::new(),
                reply_to: None,
                subject: "Test".into(),
                snippet: String::new(),
                body: Default::default(),
                date: 1,
                is_read: false,
                is_starred: false,
                is_outgoing: false,
                label_ids: [LabelId::from(folder)].into(),
                attachments: Vec::new(),
                rfc_message_id: None,
                in_reply_to: None,
                references: Vec::new(),
                category: None,
                is_important: false,
                focus_override: None,
                invitation: None,
                read_receipt: None,
            }])
            .unwrap();
    }

    fn message(&self, id: &str) -> Option<Message> {
        self.store.message(&MessageId::from(id)).unwrap()
    }

    fn call(&self, name: &str, key: &str, args: Value) -> Result<Value, Failure> {
        let tool = tools().into_iter().find(|tool| tool.name == name).unwrap();
        match self.context().execute(
            &self.store,
            "test-client",
            &tool,
            key,
            args.as_object().unwrap(),
        )? {
            McpWriteOutcome::Applied { data, .. } | McpWriteOutcome::Replayed(data) => Ok(data),
            McpWriteOutcome::KeyConflict => Err(conflict("test key conflict")),
        }
    }
}

#[test]
fn mixed_targets_report_individual_results_and_undo_restores_only_changed_messages() {
    let f = Fixture::new(ProviderKind::Sample);
    f.put_message("first", "inbox");
    f.put_message("second", "inbox");
    let other = Account {
        id: AccountId::from("other"),
        ..f.account.clone()
    };
    f.store.upsert_account(&other).unwrap();
    let mut foreign = f.message("first").unwrap();
    foreign.account_id = other.id;
    foreign.id = MessageId::from("foreign");
    foreign.conversation_id = ConversationId::from("foreign");
    foreign.label_ids.clear();
    f.store.upsert_messages(&[foreign]).unwrap();
    let ctx = f.context();
    let ids = [
        ctx.id(MESSAGE_ID, "first"),
        ctx.id(MESSAGE_ID, "absent"),
        ctx.id(MESSAGE_ID, "foreign"),
    ];
    let args = json!({"action":"star","message_ids":ids});
    let starred = f.call(ORGANIZE, "star", args.clone()).unwrap();
    assert_eq!(starred["items"][0]["status"], "updated");
    assert_eq!(starred["items"][1]["code"], "not_cached");
    assert_eq!(starred["items"][2]["code"], "not_cached");
    assert!(!f.message("foreign").unwrap().is_starred);
    let pending = f.store.pending_operations(&f.account.id).unwrap();
    assert_eq!(
        starred["operation_ids"],
        json!([ctx.id("operation", pending[0].id.as_str())])
    );
    assert_eq!(f.call(ORGANIZE, "star", args).unwrap(), starred);
    assert!(f.message("first").unwrap().is_starred);
    assert!(!f.message("second").unwrap().is_starred);

    let args = json!({"entry_id":starred["entry_id"]});
    f.call(UNDO, "undo", args.clone()).unwrap();
    assert!(!f.message("first").unwrap().is_starred);
    assert_eq!(
        f.call(UNDO, "undo-again", args).unwrap_err().code,
        ErrorCode::Conflict
    );
}

#[test]
fn imap_move_and_undo_each_queue_one_destination() {
    let f = Fixture::new(ProviderKind::Imap);
    f.put_message("first", "work");
    let id = f.context().id(MESSAGE_ID, "first");
    let moved = f
        .call(
            ORGANIZE,
            "junk",
            json!({"action":"junk","message_ids":[id]}),
        )
        .unwrap();
    assert_eq!(moved["operation_ids"].as_array().unwrap().len(), 1);
    assert_eq!(
        f.message("first").unwrap().label_ids,
        [LabelId::from("junk")].into()
    );
    let undone = f
        .call(UNDO, "undo", json!({"entry_id":moved["entry_id"]}))
        .unwrap();
    assert_eq!(undone["operation_ids"].as_array().unwrap().len(), 1);
    assert_eq!(
        f.message("first").unwrap().label_ids,
        [LabelId::from("work")].into()
    );
}

#[test]
fn delete_requires_confirmation_and_every_target_in_trash() {
    let f = Fixture::new(ProviderKind::Sample);
    f.put_message("first", "trash");
    f.put_message("second", "inbox");
    let first = f.context().id(MESSAGE_ID, "first");
    let second = f.context().id(MESSAGE_ID, "second");
    assert_eq!(
        f.call(
            DELETE,
            "unconfirmed",
            json!({"confirm":false,"message_ids":[first]})
        )
        .unwrap_err()
        .code,
        ErrorCode::InvalidRequest
    );
    assert_eq!(
        f.call(
            DELETE,
            "mixed",
            json!({"confirm":true,"message_ids":[first,second]})
        )
        .unwrap_err()
        .code,
        ErrorCode::Conflict
    );
    assert!(f.message("first").is_some());
    assert!(f
        .store
        .pending_operations(&f.account.id)
        .unwrap()
        .is_empty());

    let deleted = f
        .call(
            DELETE,
            "exact",
            json!({"confirm":true,"message_ids":[first]}),
        )
        .unwrap();
    assert_eq!(deleted["items"][0]["status"], "deleted");
    assert!(f.message("first").is_none());
    assert!(f.message("second").is_some());
    assert_eq!(
        f.call(UNDO, "undo-delete", json!({"entry_id":deleted["entry_id"]}))
            .unwrap_err()
            .code,
        ErrorCode::Unsupported
    );
}

#[test]
fn training_is_local_and_counts_each_sender_once() {
    let f = Fixture::new(ProviderKind::Sample);
    f.put_message("first", "inbox");
    f.put_message("second", "inbox");
    let ctx = f.context();
    let ids = [ctx.id(MESSAGE_ID, "first"), ctx.id(MESSAGE_ID, "second")];
    let set = f
        .call(
            FOCUS,
            "other",
            json!({"focus":"other","train_senders":true,"message_ids":ids}),
        )
        .unwrap();
    assert_eq!(set["sender_rules_changed"], 1);
    assert_eq!(set["overrides_changed"], 2);
    assert_eq!(f.message("first").unwrap().focus_override, Some(false));
    assert_eq!(
        f.store.focus_rules(&f.account.id).unwrap()[0].email,
        "sender@example.test"
    );
    let cleared = f
        .call(
            FOCUS,
            "clear",
            json!({"focus":"automatic","train_senders":true,"message_ids":ids}),
        )
        .unwrap();
    assert_eq!(cleared["sender_rules_changed"], 1);
    assert_eq!(f.message("first").unwrap().focus_override, None);
    assert!(f.store.focus_rules(&f.account.id).unwrap().is_empty());
    assert!(f
        .store
        .pending_operations(&f.account.id)
        .unwrap()
        .is_empty());
}

#[test]
fn pop3_folder_rename_preserves_its_message_membership() {
    let f = Fixture::new(ProviderKind::Pop3);
    let created = f
        .call(CREATE_FOLDER, "create", json!({"name":"Research"}))
        .unwrap();
    f.put_message("first", "inbox");
    let ctx = f.context();
    f.call(
        ORGANIZE,
        "move",
        json!({"action":"move","folder_id":created["folder_id"],"message_ids":[ctx.id(MESSAGE_ID, "first")]}),
    ).unwrap();
    let renamed = f
        .call(
            RENAME_FOLDER,
            "rename",
            json!({"folder_id":created["folder_id"],"name":"Client research"}),
        )
        .unwrap();
    assert_eq!(created["folder_id"], renamed["folder_id"]);
    let label = f
        .store
        .labels(&f.account.id)
        .unwrap()
        .into_iter()
        .find(|label| label.name == "Client research")
        .unwrap();
    assert_eq!(f.message("first").unwrap().label_ids, [label.id].into());
}

#[test]
fn local_folder_actions_have_reversible_membership_changes() {
    let f = Fixture::new(ProviderKind::Sample);
    f.put_message("first", "inbox");
    let id = f.context().id(MESSAGE_ID, "first");
    let original = f.message("first").unwrap().label_ids;
    for (action, expected) in [
        ("archive", Vec::new()),
        ("trash", vec![LabelId::from("trash")]),
        ("junk", vec![LabelId::from("junk")]),
    ] {
        let result = f
            .call(
                ORGANIZE,
                action,
                json!({"action":action,"message_ids":[id]}),
            )
            .unwrap();
        assert_eq!(
            f.message("first").unwrap().label_ids,
            expected.into_iter().collect()
        );
        f.call(
            UNDO,
            &format!("undo-{action}"),
            json!({"entry_id":result["entry_id"]}),
        )
        .unwrap();
        assert_eq!(f.message("first").unwrap().label_ids, original);
    }
}
