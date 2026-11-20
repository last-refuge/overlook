use super::*;
use overlook_core::{AccountId, ContactCard, ContactsConnection};
use overlook_provider::sample::SampleProvider;
use tokio::runtime::Runtime;

fn account(id: &str, provider: ProviderKind, store: &Store) -> Account {
    let account = Account {
        id: AccountId::from(id),
        provider,
        display_name: "Fixture".into(),
        email: "fixture@example.test".into(),
        created_at: 1,
    };
    store.upsert_account(&account).unwrap();
    account
}

fn context<'a>(
    store: &'a Store,
    account: &'a Account,
    sample: &'a SampleProvider,
    runtime: &'a Runtime,
) -> ReadContext<'a> {
    ReadContext {
        store,
        account,
        account_id: account.id.as_str(),
        sample,
        runtime,
        key: &[0; 32],
    }
}

fn call(ctx: &ReadContext<'_>, name: &str, args: Value) -> Result<Value, Failure> {
    execute(ctx, name, args.as_object().unwrap())
}

#[test]
fn search_and_get_are_scoped_to_the_selected_account_and_book() {
    let store = Store::open_in_memory().unwrap();
    let primary = account("sample", ProviderKind::Sample, &store);
    let secondary = account("sample-secondary", ProviderKind::Sample, &store);
    seed_sample(&store, &primary).unwrap();
    seed_sample(&store, &secondary).unwrap();
    let sample = SampleProvider::new();
    let runtime = Runtime::new().unwrap();
    let a = context(&store, &primary, &sample, &runtime);
    let b = context(&store, &secondary, &sample, &runtime);
    let query = json!({"query": "alex.production NORTHSTAR"});
    assert_eq!(call(&a, SEARCH, query.clone()).unwrap()["items"], json!([]));
    let matches = call(&b, SEARCH, query).unwrap();
    let alex = &matches["items"][0];
    assert_eq!(alex["display_name"], "Alex Rivera");
    assert_eq!(alex["email"], "alex@studio.example");
    let book = &alex["address_book_id"];
    assert_eq!(
        call(
            &a,
            SEARCH,
            json!({"query": "Alex", "address_book_id": book})
        )
        .unwrap_err()
        .code,
        ErrorCode::AccessDenied
    );
    assert_eq!(
        call(
            &b,
            SEARCH,
            json!({"query": "Alex", "address_book_id": book})
        )
        .unwrap()["items"][0],
        *alex
    );

    let id = json!({"contact_id": alex["contact_id"]});
    assert_eq!(
        call(&a, GET, id.clone()).unwrap_err().code,
        ErrorCode::AccessDenied
    );
    let details = call(&b, GET, id).unwrap();
    assert_eq!(details["contact"]["title"], "Creative Producer");
    assert_eq!(
        details["contact"]["emails"],
        json!(["alex@studio.example", "alex.production@studio.example"])
    );
    assert_eq!(
        call(&b, GET, json!({"contact_id": "sample-contact:1"}))
            .unwrap_err()
            .code,
        ErrorCode::AccessDenied
    );

    let page = call(&a, SEARCH, json!({"query": "Northstar", "limit": 1})).unwrap();
    assert_eq!(page["items"][0]["display_name"], "Daniel Park");
    let next = call(
        &a,
        SEARCH,
        json!({"query": "Northstar", "limit": 1, "cursor": page["next_cursor"]}),
    )
    .unwrap();
    assert_eq!(next["items"][0]["display_name"], "Maya Chen");
    assert_eq!(next["next_cursor"], Value::Null);

    let page = call(&a, LIST, json!({"limit": 1})).unwrap();
    let next = call(&a, LIST, json!({"cursor": page["next_cursor"]})).unwrap();
    let inferred = &next["items"][0];
    assert_eq!(inferred["source"], "inferred");
    assert_eq!(inferred["read_only"], true);
    let scoped = call(
        &a,
        SEARCH,
        json!({"query": "Field and Vine", "address_book_id": inferred["address_book_id"]}),
    )
    .unwrap();
    assert_eq!(scoped["items"][0]["display_name"], "Priya Natarajan");
    assert_eq!(scoped["items"][0]["read_only"], true);
}

#[test]
fn carddav_lists_visible_books_without_disclosing_server_metadata() {
    let store = Store::open_in_memory().unwrap();
    let account = account("imap:fixture", ProviderKind::Imap, &store);
    let book = "https://contact-cache.example.test/shared/book/";
    let mut connection = ContactsConnection {
        account_id: account.id.clone(),
        server_url: "https://contact-cache.example.test/".into(),
        username: "private-sign-in".into(),
        default_book: Some("https://contact-cache.example.test/default/book/".into()),
        credential: ContactsCredential::Separate,
        connected_at: 0,
    };
    store.save_contacts_connection(&connection).unwrap();
    connection = store.contacts_connection(&account.id).unwrap().unwrap();
    let url = format!("{book}opaque.vcf");
    let contact = Contact {
        id: format!("carddav:{url}"),
        account_id: account.id.clone(),
        name: Some("Renée Silva".into()),
        email: "renee@example.test".into(),
        notes: Some("界".repeat(NOTES_LIMIT)),
        photo_url: Some("https://contact-cache.example.test/photo/".into()),
        in_address_book: true,
        ..Default::default()
    };
    store
        .apply_contact_download(
            &connection,
            &[ContactCard {
                contact_id: contact.id.clone(),
                address_book: book.into(),
                url,
                etag: "private-etag".into(),
                base: contact.clone(),
            }],
        )
        .unwrap();
    let sample = SampleProvider::new();
    let runtime = Runtime::new().unwrap();
    let ctx = context(&store, &account, &sample, &runtime);
    let books = call(&ctx, LIST, json!({})).unwrap();
    let books_list = books["items"].as_array().unwrap();
    assert_eq!(books_list.len(), 2);
    assert_eq!(books_list[0]["cached_contacts"], 0);
    assert_eq!(books_list[1]["source"], "carddav");
    assert_eq!(books_list[1]["read_only"], Value::Null);
    let found = call(
        &ctx,
        SEARCH,
        json!({"query": "RENÉE", "address_book_id": books_list[1]["address_book_id"]}),
    )
    .unwrap();
    assert_eq!(found["items"][0]["display_name"], "Renée Silva");
    let id = &found["items"][0]["contact_id"];
    let details = call(&ctx, GET, json!({"contact_id": id})).unwrap();
    assert!(details["contact"]["notes"].as_str().unwrap().len() <= NOTES_LIMIT);
    for result in [&books, &found, &details] {
        let text = result.to_string();
        for internal in [
            "contact-cache.example.test",
            "opaque.vcf",
            "private-sign-in",
            "private-etag",
        ] {
            assert!(!text.contains(internal));
        }
    }

    store.delete_contact_locally(&account.id, &contact).unwrap();
    assert_eq!(
        call(&ctx, LIST, json!({})).unwrap()["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        call(&ctx, GET, json!({"contact_id": id})).unwrap_err().code,
        ErrorCode::AccessDenied
    );
}
