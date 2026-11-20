use std::fs;
use std::os::unix::fs::symlink;

use overlook_core::{Account, AccountId, Attachment, AttachmentId, MessageId, ProviderKind};
use overlook_provider::sample::sample_mailbox;
use overlook_store::Store;

use super::seed_sample;

fn account(id: &str) -> Account {
    Account {
        id: AccountId::from(id),
        provider: ProviderKind::Sample,
        email: "owner@example.com".into(),
        display_name: "Owner".into(),
        created_at: 0,
    }
}

#[test]
fn cache_search_scopes_hits_dates_and_body_coverage() {
    let store = Store::open_in_memory().unwrap();
    let one = account("one");
    let two = account("two");
    for account in [&one, &two] {
        store.upsert_account(account).unwrap();
        let mut mailbox = sample_mailbox(account, 1_785_254_400_000);
        for label in &mailbox.labels {
            store.upsert_label(label).unwrap();
        }
        mailbox.messages[0].body = Default::default();
        store.upsert_messages(&mailbox.messages).unwrap();
    }
    let first = store
        .message(&MessageId::from("one:msg-1"))
        .unwrap()
        .unwrap();
    let folder = &overlook_core::LabelId::from("one:inbox");
    let result = store
        .mcp_mail_cache_search(
            &one.id,
            Some(folder),
            Some(first.date),
            Some(first.date + 1),
            Some("launch creative"),
        )
        .unwrap();
    assert_eq!(result.messages.len(), 1);
    assert_eq!(result.messages[0].id, first.id);
    assert_eq!(result.messages[0].is_read, first.is_read);
    assert_eq!(result.coverage.messages_indexed, 1);
    assert_eq!(result.coverage.bodies_cached, 0);
    assert_eq!(result.oldest, Some(first.date));
    let gaps = store
        .mcp_mail_cache_search(
            &one.id,
            Some(folder),
            Some(first.date),
            Some(first.date + 1),
            Some("nonexistentkeyword"),
        )
        .unwrap();
    assert!(gaps.messages.is_empty());
    assert_eq!(gaps.coverage.messages_indexed, 1);
    assert_eq!(gaps.coverage.bodies_cached, 0);
    let provider_matches = store
        .mcp_mail_provider_headers(&two.id, &[first.provider_id.unwrap()])
        .unwrap();
    assert_eq!(provider_matches.len(), 1);
    assert_eq!(provider_matches[0].account_id, two.id);
}

#[test]
fn fixture_has_stable_small_bytes_and_preserves_mail_state() {
    let store = Store::open_in_memory().unwrap();
    let account = account("sample");
    store.upsert_account(&account).unwrap();
    let mut mailbox = sample_mailbox(&account, 1_785_254_400_000);
    mailbox.messages[0].is_read = false;
    for label in &mailbox.labels {
        store.upsert_label(label).unwrap();
    }
    store.upsert_messages(&mailbox.messages).unwrap();
    seed_sample(&store, &account).unwrap();
    seed_sample(&store, &account).unwrap();
    let message = store.message(&mailbox.messages[0].id).unwrap().unwrap();
    assert!(!message.is_read);
    assert_eq!(message.attachments.len(), 1);
    let content = super::attachment::bytes(
        &account,
        &message.attachments[0],
        std::path::Path::new("/irrelevant"),
    )
    .unwrap();
    assert!(content.starts_with(b"Overlook sample mailbox attachment."));
}

#[test]
fn cache_attachment_confines_reads_and_enforces_actual_size() {
    let profile = tempfile::tempdir().unwrap();
    let account = account("sample");
    let file = profile
        .path()
        .join("attachments/sample/msg_1/att/notes.txt");
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, b"cached content").unwrap();
    let mut attachment = Attachment {
        id: AttachmentId::from("att"),
        message_id: MessageId::from("msg:1"),
        filename: "notes.txt".into(),
        mime_type: "text/plain".into(),
        size_bytes: 14,
        local_path: Some(file.to_string_lossy().into_owned()),
        provider_id: None,
        content_id: None,
    };
    let read = |a: &Attachment| super::attachment::bytes(&account, a, profile.path());
    assert_eq!(read(&attachment).unwrap(), b"cached content");
    fs::write(&file, vec![0u8; 24 * 1024 + 1]).unwrap();
    assert_eq!(
        read(&attachment).unwrap_err().code,
        super::ErrorCode::Unsupported
    );
    fs::write(&file, b"#!/bin/sh\n").unwrap();
    assert_eq!(
        read(&attachment).unwrap_err().code,
        super::ErrorCode::Unsupported
    );

    let outside = profile.path().join("other.txt");
    fs::write(&outside, b"secret").unwrap();
    fs::remove_file(&file).unwrap();
    symlink(&outside, &file).unwrap();
    assert_eq!(
        read(&attachment).unwrap_err().code,
        super::ErrorCode::NotCached
    );
    attachment.local_path = Some(outside.to_string_lossy().into_owned());
    assert_eq!(
        read(&attachment).unwrap_err().code,
        super::ErrorCode::NotCached
    );
}
