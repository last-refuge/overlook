//! Shared lossless MIME import for Gmail and IMAP drafts.
use crate::{mime, ProviderError};
use overlook_core::*;

pub const MAX_DRAFT_BYTES: usize = 40 * 1024 * 1024;

pub struct ImportedDraft {
    pub draft: Draft,
    /// Attachment content in the same order as `draft.attachments`. The sync engine
    /// stages these in its protected outgoing store before committing the import.
    pub files: Vec<Vec<u8>>,
}

pub fn import(
    account: &Account,
    remote: &RemoteDraft,
    raw: &[u8],
) -> Result<ImportedDraft, ProviderError> {
    if raw.len() > MAX_DRAFT_BYTES {
        return Err(ProviderError::Rejected(
            "This server draft exceeds the 40 MB message limit".into(),
        ));
    }
    let parsed = mail_parser::MessageParser::default()
        .parse(raw)
        .ok_or_else(|| ProviderError::Rejected("Could not read a server draft".into()))?;
    let message = mime::parse::message_from_parsed(
        account,
        raw,
        Some(&parsed),
        MessageId::generate(),
        &mime::MessageFlags::default(),
    );
    let mut draft = Draft::new(
        account.id.clone(),
        DraftKind::New,
        overlook_core::time::now(),
    );
    draft.to = recipients(parsed.to());
    draft.cc = recipients(parsed.cc());
    draft.bcc = recipients(parsed.bcc());
    draft.subject = message.subject;
    draft.body_text = parsed
        .text_part(0)
        .and_then(|part| match &part.body {
            mail_parser::PartType::Text(text) => Some(text.to_string()),
            mail_parser::PartType::Html(html) => Some(overlook_core::sanitize::html_to_text(html)),
            _ => None,
        })
        .or_else(|| {
            message
                .body
                .html
                .as_deref()
                .map(overlook_core::sanitize::html_to_text)
        })
        .unwrap_or_default();
    draft.body_html = message.body.html.clone();
    draft.imported = Some(DraftSource {
        text: draft.body_text.clone(),
        html: message.body.html,
        in_reply_to: message.in_reply_to,
        references: message.references,
    });
    draft.provider_id = Some(remote.id.clone());
    if account.provider == ProviderKind::Gmail {
        draft.conversation_id = remote
            .thread_id
            .as_ref()
            .map(|t| crate::gmail::mapping::local_conversation_id(&account.id, t));
    }
    let mut files = Vec::new();
    let mut total = 0u64;
    for att in message.attachments {
        let section = att
            .provider_id
            .as_ref()
            .ok_or_else(|| ProviderError::Rejected("A draft attachment has no MIME part".into()))?;
        let bytes = mime::parse::part_bytes_from_parsed(&parsed, &section.0)
            .ok_or_else(|| ProviderError::Rejected("Could not read a draft attachment".into()))?;
        total = total.saturating_add(bytes.len() as u64);
        if !overlook_core::attachment_policy::within_size_limit(total) {
            return Err(ProviderError::Rejected(
                overlook_core::attachment_policy::TOO_LARGE.into(),
            ));
        }
        draft.attachments.push(DraftAttachment {
            id: AttachmentId::generate(),
            filename: att.filename,
            mime_type: att.mime_type,
            size_bytes: bytes.len() as u64,
            local_path: String::new(),
            content_id: att.content_id,
        });
        files.push(bytes);
    }
    // Parsing a valid but blank message is allowed: server drafts need no subject/body.
    Ok(ImportedDraft { draft, files })
}

fn recipients(list: Option<&mail_parser::Address<'_>>) -> Vec<Address> {
    list.into_iter()
        .flat_map(|list| list.iter())
        .filter_map(|addr| {
            let name = addr
                .name
                .as_deref()
                .map(str::trim)
                .filter(|n| !n.is_empty())
                .map(str::to_owned);
            match addr
                .address
                .as_deref()
                .map(str::trim)
                .filter(|a| !a.is_empty())
            {
                Some(email) => Some(Address::new(name, email)),
                // Unlike received mail, unfinished addresses are meaningful in a draft.
                None => name.map(|unfinished| Address::new(None, unfinished)),
            }
        })
        .collect()
}

/// A remote editor can retain our Message-ID while changing the body. A retry may
/// adopt that id only when the decoded editable content still matches our upload.
pub(crate) fn same_content(left: &[u8], right: &[u8]) -> Result<bool, ProviderError> {
    let account = Account {
        id: AccountId::from("mime-comparison"),
        provider: ProviderKind::Imap,
        display_name: String::new(),
        email: String::new(),
        created_at: 0,
    };
    let remote = RemoteDraft {
        id: ProviderId::from("comparison"),
        revision: String::new(),
        thread_id: None,
    };
    let (a, b) = (
        import(&account, &remote, left)?,
        import(&account, &remote, right)?,
    );
    let normalize = |s: &str| s.replace("\r\n", "\n");
    let source = |d: &Draft| {
        d.imported.clone().map(|mut s| {
            s.text = normalize(&s.text);
            s.html = s.html.map(|h| normalize(&h));
            s
        })
    };
    let attachments = |d: &Draft| {
        d.attachments
            .iter()
            .map(|a| {
                (
                    a.filename.clone(),
                    a.mime_type.clone(),
                    a.content_id.clone(),
                )
            })
            .collect::<Vec<_>>()
    };
    Ok(a.draft.to == b.draft.to
        && a.draft.cc == b.draft.cc
        && a.draft.bcc == b.draft.bcc
        && a.draft.subject == b.draft.subject
        && source(&a.draft) == source(&b.draft)
        && attachments(&a.draft) == attachments(&b.draft)
        && a.files == b.files)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blank_server_drafts_and_unfinished_recipients_are_preserved() {
        let account = Account {
            id: AccountId::from("a"),
            provider: ProviderKind::Gmail,
            display_name: "Test".into(),
            email: "me@example.com".into(),
            created_at: 0,
        };
        let remote = RemoteDraft {
            id: ProviderId::from("d"),
            revision: "m".into(),
            thread_id: None,
        };
        let imported = import(
            &account,
            &remote,
            b"MIME-Version: 1.0\r\nContent-Type: text/plain\r\n\r\n",
        )
        .unwrap()
        .draft;
        assert!(imported.body_text.is_empty());
        assert!(
            !imported.is_empty(),
            "opening and closing an empty server draft must not discard it"
        );
        let imported = import(
            &account,
            &remote,
            b"To: friend@example.com, unfinished\r\nSubject: Test\r\n\r\nBody",
        )
        .unwrap()
        .draft;
        assert_eq!(
            imported.to,
            vec![
                Address::new(None, "friend@example.com"),
                Address::new(None, "unfinished")
            ]
        );
    }

    #[test]
    fn import_extracts_multiple_attachments_from_single_parse_and_compares_content() {
        let account = Account {
            id: AccountId::from("a"),
            provider: ProviderKind::Gmail,
            display_name: "Test".into(),
            email: "me@example.com".into(),
            created_at: 0,
        };
        let remote = RemoteDraft {
            id: ProviderId::from("d1"),
            revision: "r1".into(),
            thread_id: Some("t1".into()),
        };
        let raw = b"From: me@example.com\r\nTo: friend@example.com\r\nSubject: Multi\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=\"bnd\"\r\n\r\n--bnd\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nHello draft.\r\n--bnd\r\nContent-Type: application/pdf; name=\"a.pdf\"\r\nContent-Disposition: attachment; filename=\"a.pdf\"\r\nContent-Transfer-Encoding: base64\r\n\r\nJVBERi0xLjQK\r\n--bnd\r\nContent-Type: text/plain; name=\"b.txt\"\r\nContent-Disposition: attachment; filename=\"b.txt\"\r\n\r\nsecond file\r\n--bnd--\r\n";
        let imported = import(&account, &remote, raw).unwrap();
        assert_eq!(imported.draft.subject, "Multi");
        assert_eq!(imported.draft.body_text.trim(), "Hello draft.");
        assert_eq!(imported.draft.attachments.len(), 2);
        assert_eq!(imported.files.len(), 2);
        assert_eq!(imported.files[0], b"%PDF-1.4\n");
        assert_eq!(imported.files[1], b"second file");
        assert!(same_content(raw, raw).unwrap());
        let edited = raw.to_vec();
        let mut modified = String::from_utf8(edited).unwrap().replace("Hello draft.", "Changed.");
        assert!(!same_content(raw, modified.as_bytes()).unwrap());
        modified.clear();
    }
}
