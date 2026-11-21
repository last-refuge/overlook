use super::*;
use crate::mcp::read::{self, ReadContext};
use overlook_core::{
    Account, AccountId, ContactCard, ContactsConnection, ContactsCredential, OperationKind,
};
use overlook_provider::{microsoft::contacts::GRAPH_CONTACTS_URL, sample::SampleProvider};
use overlook_store::{McpWriteKey, McpWriteOutcome, Store};
use std::path::Path;
use tokio::runtime::Runtime;

struct Fixture {
    store: Store,
    account: Account,
    sample: SampleProvider,
    runtime: Runtime,
}

impl Fixture {
    fn new(provider: ProviderKind) -> Self {
        let store = Store::open_in_memory().unwrap();
        let account = Account {
            id: AccountId::from("fixture"),
            display_name: "Contacts fixture".into(),
            email: "owner@example.test".into(),
            created_at: 1,
            provider,
        };
        store.upsert_account(&account).unwrap();
        Self {
            store,
            account,
            sample: SampleProvider::new(),
            runtime: Runtime::new().unwrap(),
        }
    }

    fn context(&self) -> WriteContext<'_> {
        WriteContext {
            account: &self.account,
            account_id: self.account.id.as_str(),
            key: &[0; 32],
            profile: Path::new("."),
        }
    }

    fn read(&self, name: &str, args: Value) -> Value {
        let ctx = ReadContext {
            store: &self.store,
            account: &self.account,
            account_id: self.account.id.as_str(),
            key: &[0; 32],
            sample: &self.sample,
            runtime: &self.runtime,
        };
        let tool = read::tools().find(|tool| tool.name == name).unwrap();
        ctx.execute(tool, args.as_object().unwrap()).unwrap()
    }

    fn call(&self, name: &str, key: u8, args: Value) -> Result<Value, Failure> {
        let outcome = self.store.mcp_write(
            McpWriteKey {
                account: &self.account.id,
                client: "contacts-test",
                key_digest: &[key; 32],
                request_digest: &[key; 32],
                action: name,
            },
            |tx, _| execute(&self.context(), tx, name, args.as_object().unwrap()),
        )?;
        match outcome {
            McpWriteOutcome::Applied { data, queued } => {
                assert_eq!(queued, data["status"] == "queued");
                Ok(data)
            }
            McpWriteOutcome::Replayed(data) => Ok(data),
            McpWriteOutcome::KeyConflict => panic!("fixture key conflict"),
        }
    }

    fn connect(&self, credential: ContactsCredential, default: Option<&str>) -> ContactsConnection {
        self.store
            .save_contacts_connection(&ContactsConnection {
                account_id: self.account.id.clone(),
                credential,
                default_book: default.map(str::to_owned),
                server_url: "https://contacts.example.test/".into(),
                username: "fixture".into(),
                connected_at: 0,
            })
            .unwrap();
        self.store
            .contacts_connection(&self.account.id)
            .unwrap()
            .unwrap()
    }
}

#[test]
fn local_crud_round_trips_read_ids_and_revisions_without_an_outbox() {
    for provider in [ProviderKind::Sample, ProviderKind::Imap] {
        let fixture = Fixture::new(provider);
        let list = fixture.read("contacts_address_books_list", json!({}));
        let book = &list["items"][0]["address_book_id"];
        let create =
            json!({"address_book_id":book, "given_name":"Maya", "email":"Maya@Example.test"});
        let saved = fixture.call(CREATE, 1, create.clone()).unwrap();
        assert_eq!(fixture.call(CREATE, 1, create).unwrap(), saved);
        assert_eq!(saved["status"], "saved");
        assert_eq!(saved["operation_id"], Value::Null);
        let id = &saved["contact_id"];
        let details = fixture.read("contacts_get", json!({"contact_id": id}));
        assert_eq!(details["contact"]["revision"], saved["revision"]);
        assert_eq!(details["contact"]["address_book_id"], *book);
        assert_eq!(details["contact"]["email"], "maya@example.test");
        assert_eq!(details["contact"]["pending"], false);

        let updated = fixture.call(UPDATE, 2, json!({
            "contact_id":id, "if_revision":saved["revision"], "organization":"Studio", "email":null
        })).unwrap();
        let details = fixture.read("contacts_get", json!({"contact_id": id}));
        assert_eq!(details["contact"]["display_name"], "Maya");
        assert_eq!(details["contact"]["organization"], "Studio");
        assert_eq!(details["contact"]["email"], Value::Null);
        assert_eq!(details["contact"]["revision"], updated["revision"]);
        for action in [UPDATE, DELETE] {
            assert_eq!(
                fixture
                    .call(
                        action,
                        3,
                        json!({
                            "contact_id":id, "if_revision":saved["revision"]
                        })
                    )
                    .unwrap_err()
                    .code,
                ErrorCode::Conflict
            );
        }
        let deleted = fixture
            .call(
                DELETE,
                4,
                json!({
                    "contact_id":id, "if_revision":updated["revision"]
                }),
            )
            .unwrap();
        assert_eq!(deleted["status"], "deleted");
        assert_eq!(fixture.store.contact_count(&fixture.account.id).unwrap(), 0);
        assert!(fixture
            .store
            .pending_operations(&fixture.account.id)
            .unwrap()
            .is_empty());
    }
}

#[test]
fn rejects_inferred_foreign_and_google_contacts() {
    let fixture = Fixture::new(ProviderKind::Sample);
    let inferred = Contact {
        id: "inferred".into(),
        account_id: fixture.account.id.clone(),
        email: "inferred@example.test".into(),
        ..Default::default()
    };
    fixture
        .store
        .replace_contacts(&fixture.account.id, &[inferred.clone()])
        .unwrap();
    let ctx = fixture.context();
    for (id, code) in [
        (ctx.id("contact", &inferred.id), ErrorCode::Unsupported),
        (
            crate::mcp::read::record_id(ctx.key, "another-account", "contact", &inferred.id),
            ErrorCode::AccessDenied,
        ),
        (inferred.id.clone(), ErrorCode::AccessDenied),
    ] {
        assert_eq!(
            fixture
                .call(
                    DELETE,
                    1,
                    json!({
                        "contact_id":id, "if_revision":ctx.contact_revision(&inferred)
                    })
                )
                .unwrap_err()
                .code,
            code
        );
    }
    assert_eq!(fixture.store.contact_count(&fixture.account.id).unwrap(), 1);
    let google = Fixture::new(ProviderKind::Gmail);
    assert_eq!(
        google.call(CREATE, 1, json!({})).unwrap_err().code,
        ErrorCode::Unsupported
    );
}

#[test]
fn queues_crud_for_cached_carddav_and_graph_default_books() {
    for (credential, book, prefix) in [
        (
            ContactsCredential::Separate,
            "https://contacts.example.test/default/",
            "carddav",
        ),
        (ContactsCredential::Microsoft, GRAPH_CONTACTS_URL, "graph"),
    ] {
        let fixture = Fixture::new(ProviderKind::Imap);
        let connection = fixture.connect(credential, (prefix == "carddav").then_some(book));
        let ctx = fixture.context();
        let list = fixture.read("contacts_address_books_list", json!({}));
        let book_id = &list["items"][0]["address_book_id"];
        let created = fixture
            .call(
                CREATE,
                1,
                json!({
                    "address_book_id":book_id, "given_name":"New", "email":"new@example.test"
                }),
            )
            .unwrap();
        let details = fixture.read("contacts_get", json!({"contact_id":created["contact_id"]}));
        assert_eq!(details["contact"]["address_book_id"], *book_id);
        assert_eq!(details["contact"]["pending"], true);
        assert_eq!(created["status"], "queued");
        assert!(!created.to_string().contains(book));

        let contact_card = |book: &str| {
            let url = format!("{}/card", book.trim_end_matches('/'));
            let id = format!("{prefix}:{url}");
            ContactCard {
                contact_id: id.clone(),
                address_book: book.into(),
                url,
                etag: "etag".into(),
                base: Contact {
                    id,
                    account_id: fixture.account.id.clone(),
                    given_name: Some("Maya".into()),
                    email: "maya@example.test".into(),
                    emails: vec!["maya@example.test".into(), "studio@example.test".into()],
                    in_address_book: true,
                    ..Default::default()
                },
            }
        };
        let card = contact_card(book);
        let shared = contact_card("https://contacts.example.test/shared");
        fixture
            .store
            .apply_contact_download(&connection, &[card.clone(), shared.clone()])
            .unwrap();
        assert_eq!(fixture.call(DELETE, 2, json!({
            "contact_id":ctx.id("contact", &shared.contact_id), "if_revision":ctx.contact_revision(&shared.base)
        })).unwrap_err().code, ErrorCode::Unsupported);
        assert_eq!(fixture.call(CREATE, 2, json!({
            "address_book_id":ctx.id("address_book", &format!("{prefix}:{}", shared.address_book)),
            "given_name":"Wrong book"
        })).unwrap_err().code, ErrorCode::AccessDenied);
        let contact_id = ctx.id("contact", &card.contact_id);
        let updated = fixture.call(UPDATE, 2, json!({
            "contact_id":contact_id, "if_revision":ctx.contact_revision(&card.base), "email":"other@example.test"
        })).unwrap();
        let details = fixture.read("contacts_get", json!({"contact_id":contact_id}));
        assert_eq!(
            details["contact"]["emails"],
            json!(["other@example.test", "studio@example.test"])
        );
        let deleted = fixture
            .call(
                DELETE,
                3,
                json!({
                    "contact_id":contact_id, "if_revision":updated["revision"]
                }),
            )
            .unwrap();

        // The prior update can complete after this deletion has committed.
        fixture
            .store
            .save_synced_contact(&connection, &card.contact_id, 1, &card)
            .unwrap();
        assert!(fixture
            .store
            .contact_by_id(&fixture.account.id, &card.contact_id)
            .unwrap()
            .is_none());

        let ops = fixture
            .store
            .pending_operations(&fixture.account.id)
            .unwrap();
        assert_eq!(ops.len(), 3);
        assert!(ops
            .iter()
            .any(|op| matches!(op.kind, OperationKind::CreateContact { .. })
                && created["operation_id"] == ctx.id("operation", op.id.as_str())));
        assert!(ops.iter().any(|op| op.kind
            == OperationKind::UpdateContact {
                contact_id: card.contact_id.clone()
            }
            && updated["operation_id"] == ctx.id("operation", op.id.as_str())));
        assert!(ops.iter().any(|op| op.kind
            == OperationKind::DeleteContact {
                contact_id: card.contact_id.clone(),
                resource_name: Some(card.contact_id.clone())
            }
            && deleted["operation_id"] == ctx.id("operation", op.id.as_str())));
        assert_eq!(fixture.store.contact_count(&fixture.account.id).unwrap(), 2);

        fixture
            .store
            .remove_contacts_connection(&fixture.account.id)
            .unwrap();
        assert!(fixture
            .store
            .pending_operations(&fixture.account.id)
            .unwrap()
            .is_empty());
        let rows = fixture.store.contacts(&fixture.account.id).unwrap();
        assert_eq!(rows.len(), 1);
        assert!(!rows[0].pending);
    }
}
