//! Builds RFC 822 messages for sending: the Gmail API's `users.messages.send` takes the same
//! bytes SMTP does, so every provider shares this builder.

use lettre::message::header::ContentType;
use lettre::message::{Attachment as MimeAttachment, Mailbox, MultiPart};
use lettre::Message as MimeMessage;
use overlook_core::*;

fn mailbox(address: &Address) -> Result<Mailbox, String> {
    let email = address.email.parse().map_err(|e| format!("invalid address “{}”: {e}", address.email))?;
    Ok(Mailbox::new(address.name.clone().filter(|n| !n.trim().is_empty()), email))
}

/// Returns the raw message bytes. `rfc_id` is the Message-ID (with angle brackets).
pub fn build_raw(account: &Account, draft: &Draft, in_reply_to: Option<&Message>, rfc_id: &str) -> Result<Vec<u8>, String> {
    let mut builder = MimeMessage::builder()
        .from(mailbox(&Address::named(&account.display_name, &account.email))?)
        .subject(draft.subject.clone())
        .message_id(Some(rfc_id.to_owned()))
        .user_agent("Overlook".to_owned());
    for a in &draft.to {
        builder = builder.to(mailbox(a)?);
    }
    for a in &draft.cc {
        builder = builder.cc(mailbox(a)?);
    }
    for a in &draft.bcc {
        builder = builder.bcc(mailbox(a)?);
    }
    if let Some(source) = in_reply_to {
        if let Some(id) = &source.rfc_message_id {
            builder = builder.in_reply_to(id.clone());
            let references = match &source.in_reply_to {
                Some(parent) => format!("{parent} {id}"),
                None => id.clone(),
            };
            builder = builder.references(references);
        }
    }
    let bodies = overlook_core::compose::outgoing_bodies(draft, in_reply_to);
    let text = if bodies.text.is_empty() { "\n".to_owned() } else { bodies.text };
    let html = format!("<!DOCTYPE html><html><body>{}</body></html>", bodies.html);
    let alternative = MultiPart::alternative_plain_html(text, html);
    let message = if draft.attachments.is_empty() {
        builder.multipart(alternative)
    } else {
        let mut multipart = MultiPart::mixed().multipart(alternative);
        for att in &draft.attachments {
            let bytes = std::fs::read(&att.local_path).map_err(|e| format!("could not read attachment {}: {e}", att.filename))?;
            // Re-checked at send time: covers drafts restored from storage and forwards.
            overlook_core::attachment_policy::check_outgoing(&att.filename, &bytes).map_err(|refusal| format!("{}: {refusal}", att.filename))?;
            let content_type = ContentType::parse(&att.mime_type).unwrap_or(ContentType::parse("application/octet-stream").unwrap());
            multipart = multipart.singlepart(MimeAttachment::new(att.filename.clone()).body(bytes, content_type));
        }
        builder.multipart(multipart)
    }
    .map_err(|e| e.to_string())?;
    Ok(message.formatted())
}

/// Rebuilds an RFC 822 message from what is stored locally. Used when the provider cannot
/// hand back the original bytes.
pub fn message_to_raw(message: &Message) -> Vec<u8> {
    let mailbox = |a: &Address| -> Option<Mailbox> { a.email.parse().ok().map(|e| Mailbox::new(a.name.clone(), e)) };
    let mut builder = MimeMessage::builder().subject(message.subject.clone()).date(std::time::UNIX_EPOCH + std::time::Duration::from_millis(message.date.max(0) as u64));
    if let Some(from) = mailbox(&message.from) {
        builder = builder.from(from);
    }
    for a in &message.to {
        if let Some(m) = mailbox(a) {
            builder = builder.to(m);
        }
    }
    for a in &message.cc {
        if let Some(m) = mailbox(a) {
            builder = builder.cc(m);
        }
    }
    if let Some(id) = &message.rfc_message_id {
        builder = builder.message_id(Some(id.clone()));
    }
    let text = message.body.text.clone().unwrap_or_else(|| message.snippet.clone());
    let built = match &message.body.html {
        Some(html) => builder.multipart(MultiPart::alternative_plain_html(text, html.clone())),
        None => builder.header(ContentType::TEXT_PLAIN).body(text),
    };
    built.map(|m| m.formatted()).unwrap_or_else(|_| format!("Subject: {}\n\n{}", message.subject, message.body.text.clone().unwrap_or_default()).into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decoded_part(raw: &str, mime: &str) -> String {
        let start = raw.find(&format!("Content-Type: {mime}")).expect("MIME alternative");
        let (headers, body) = raw[start..].split_once("\r\n\r\n").expect("part headers");
        let body = body.split("\r\n--").next().unwrap();
        let bytes = if headers.contains("base64") {
            use base64::Engine;
            base64::engine::general_purpose::STANDARD.decode(body.replace(['\r', '\n'], "")).unwrap()
        } else if headers.contains("quoted-printable") {
            let bytes = body.as_bytes();
            let mut decoded = Vec::new();
            let mut i = 0;
            while i < bytes.len() {
                if bytes[i..].starts_with(b"=\r\n") { i += 3; }
                else if bytes[i] == b'=' && i + 2 < bytes.len() {
                    decoded.push(u8::from_str_radix(&body[i + 1..i + 3], 16).unwrap());
                    i += 3;
                } else { decoded.push(bytes[i]); i += 1; }
            }
            decoded
        } else { body.as_bytes().to_vec() };
        String::from_utf8(bytes).unwrap()
    }

    #[test]
    fn send_time_check_refuses_a_renamed_executable_attachment() {
        let account = Account { id: AccountId::from("g1"), provider: ProviderKind::Gmail, display_name: "Synthetic".into(), email: "synthetic@example.com".into(), created_at: 0 };
        let dir = std::env::temp_dir().join(format!("overlook-attachment-policy-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let attach = |name: &str, bytes: &[u8]| {
            let path = dir.join(name);
            std::fs::write(&path, bytes).unwrap();
            DraftAttachment { id: AttachmentId::generate(), filename: name.into(), mime_type: "image/jpeg".into(), size_bytes: bytes.len() as u64, local_path: path.to_string_lossy().into() }
        };
        let mut draft = Draft::new(account.id.clone(), DraftKind::New, 0);
        draft.to = vec![Address::named("Recipient", "recipient@example.com")];
        draft.attachments = vec![attach("photo.jpg", b"\xff\xd8\xff\xe0")];
        assert!(build_raw(&account, &draft, None, "<synthetic@example.com>").is_ok());
        draft.attachments.push(attach("holiday.jpg", b"\x7fELF\x02\x01"));
        let error = build_raw(&account, &draft, None, "<synthetic@example.com>").unwrap_err();
        assert!(error.starts_with("holiday.jpg: Overlook doesn’t send programs"), "{error}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn builds_plain_message_with_threading() {
        let account = Account {
            id: AccountId::from("g1"),
            provider: ProviderKind::Gmail,
            display_name: "Jason Alexander".into(),
            email: "jason@example.com".into(),
            created_at: 0,
        };
        let mut draft = Draft::new(account.id.clone(), DraftKind::Reply, 0);
        draft.to = vec![Address::named("Maya Chen", "maya@northstar.example")];
        draft.subject = "Re: Launch créative".into();
        draft.body_text = "Looks great — thanks!".into();
        let mut source = Message {
            id: MessageId::from("g1:1"),
            account_id: account.id.clone(),
            conversation_id: ConversationId::from("g1:t"),
            provider_id: None,
            from: Address::named("Maya Chen", "maya@northstar.example"),
            to: vec![],
            cc: vec![],
            bcc: vec![],
            reply_to: None,
            subject: "Launch créative".into(),
            snippet: String::new(),
            body: Body::default(),
            date: 0,
            is_read: true,
            is_starred: false,
            is_outgoing: false,
            label_ids: Default::default(),
            attachments: vec![],
            rfc_message_id: Some("<abc@northstar.example>".into()),
            in_reply_to: None,
            references: Vec::new(),
            category: None,
            is_important: false,
            focus_override: None,
            invitation: None,
        };
        source.in_reply_to = Some("<root@northstar.example>".into());
        let raw = String::from_utf8(build_raw(&account, &draft, Some(&source), "<d1@overlook.lastrefuge.ai>").unwrap()).unwrap();
        assert!(raw.contains("From: \"Jason Alexander\" <jason@example.com>"), "{raw}");
        assert!(raw.contains("To: \"Maya Chen\" <maya@northstar.example>"));
        assert!(raw.contains("Message-ID: <d1@overlook.lastrefuge.ai>"));
        assert!(raw.contains("In-Reply-To: <abc@northstar.example>"));
        assert!(raw.contains("References: <root@northstar.example> <abc@northstar.example>"));
        assert!(raw.contains("Subject: Re: Launch =?utf-8?b?"), "{raw}");
        assert!(raw.contains("Content-Type: multipart/alternative"), "{raw}");
        assert!(raw.contains("Content-Type: text/plain") && raw.contains("Content-Type: text/html"), "{raw}");
        assert!(raw.contains("<p>Looks great"), "{raw}");
        assert!(decoded_part(&raw, "text/plain").contains("Looks great — thanks!"));
        assert!(decoded_part(&raw, "text/html").contains("Looks great — thanks!"));

        draft.body_text = "**Café — 東京 👋**\nCrème brûlée".into();
        let raw = String::from_utf8(build_raw(&account, &draft, None, "<unicode@overlook.example>").unwrap()).unwrap();
        assert!(decoded_part(&raw, "text/plain").contains("Café — 東京 👋"));
        let html = decoded_part(&raw, "text/html");
        assert!(html.contains("<strong>Café — 東京 👋</strong><br>"), "{html}");
        assert!(html.contains("Crème brûlée"), "{html}");
    }
}
