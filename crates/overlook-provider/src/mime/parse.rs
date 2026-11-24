//! Raw RFC 822 bytes to [`Message`], for IMAP and POP3.
//!
//! The mapping mirrors `gmail::mapping::message_from_json`: the first `text/plain` and
//! `text/html` parts become the body, every named or inline-binary part becomes an
//! [`Attachment`] (with its MIME section path as `provider_id`, so IMAP can fetch it on
//! demand), the first `text/calendar` part is parsed as an invitation, and `In-Reply-To`
//! and `References` are kept for threading. The conversation id is left empty: the sync
//! engine assigns it with `overlook_core::threading`.

use mail_parser::{MessageParser, MessagePart, MimeHeaders, PartType};
use overlook_core::sanitize::html_to_text;
use overlook_core::threading::normalize_message_id;
use overlook_core::time::Timestamp;
use overlook_core::*;
use std::collections::BTreeSet;

/// Everything about a message that is not inside its bytes: the provider's identity for
/// it, its folder, its flags, and when the server received it.
#[derive(Debug, Clone, Default)]
pub struct MessageFlags {
    pub provider_id: Option<ProviderId>,
    pub label_ids: BTreeSet<LabelId>,
    /// `\Seen` for IMAP; POP3 messages are unread until opened.
    pub is_read: bool,
    /// `\Flagged`.
    pub is_starred: bool,
    /// Server receive time (IMAP `INTERNALDATE`), used when the `Date` header is missing
    /// or unparseable.
    pub received_at: Option<Timestamp>,
}

/// Snippet length in characters, after whitespace is collapsed.
const SNIPPET_CHARS: usize = 160;

/// Cap on fallback raw bytes copied into `body.text` when a message cannot be parsed.
const MAX_FALLBACK_TEXT_BYTES: usize = 64 * 1024;

/// Parses a raw message. Never fails: unreadable input yields a message that says so, so a
/// broken message in a folder is shown rather than silently dropped.
pub fn message_from_rfc822(account: &Account, bytes: &[u8], local_id: MessageId, flags: &MessageFlags) -> Message {
    let parsed = MessageParser::default().parse(bytes);
    message_from_parsed(account, bytes, parsed.as_ref(), local_id, flags)
}

/// Builds a [`Message`] from an already-parsed `mail_parser::Message`, avoiding re-parsing
/// when the caller needs both the parsed AST and the mapped [`Message`].
pub(crate) fn message_from_parsed(account: &Account, bytes: &[u8], parsed: Option<&mail_parser::Message<'_>>, local_id: MessageId, flags: &MessageFlags) -> Message {
    let mut collected = Collected::default();
    let (from, to, cc, bcc, reply_to, subject, header_date, rfc_message_id, in_reply_to, references) = match parsed {
        Some(m) => {
            walk(&m.parts, 0, None, &mut collected, &local_id);
            (
                addresses(m.from()).into_iter().next().unwrap_or_default(),
                addresses(m.to()),
                addresses(m.cc()),
                addresses(m.bcc()),
                addresses(m.reply_to()).into_iter().next(),
                m.subject().map(|s| s.trim().to_owned()).unwrap_or_default(),
                m.date().filter(|d| d.year >= 1970).map(|d| d.to_timestamp() * 1000),
                m.message_id().map(normalize_message_id).filter(|s| !s.is_empty()),
                m.in_reply_to().as_text_list().and_then(|l| l.first()).map(|s| normalize_message_id(s)).filter(|s| !s.is_empty()),
                m.references().as_text_list().map(|l| l.iter().map(|s| normalize_message_id(s)).filter(|s| !s.is_empty()).collect()).unwrap_or_default(),
            )
        }
        None => Default::default(),
    };

    let Collected { mut text, html, calendar, disposition_notification, attachments } = collected;
    let plain_has_visible = text
        .as_deref()
        .is_some_and(|t| !snippet_of(t).is_empty());
    if text.is_none() || (!plain_has_visible && html.is_some()) {
        if let Some(rendered) = html.as_deref().map(html_to_text) {
            if text.is_none() || !snippet_of(&rendered).is_empty() {
                text = Some(rendered);
            }
        }
    }
    // Nothing recognisable at all: show a bounded prefix of the bytes rather than an empty message.
    let blank = |t: &Option<String>| t.as_deref().is_none_or(|t| t.trim().is_empty());
    let unreadable = parsed.is_none() || (subject.is_empty() && from.email.is_empty() && blank(&text) && blank(&html) && attachments.is_empty());
    if unreadable && blank(&text) && !bytes.is_empty() {
        let slice = &bytes[..bytes.len().min(MAX_FALLBACK_TEXT_BYTES)];
        text = Some(String::from_utf8_lossy(slice).into_owned());
    }
    let snippet = match text.as_deref().map(snippet_of) {
        Some(s) if !s.is_empty() => s,
        _ if unreadable => "This message could not be read.".to_owned(),
        _ => String::new(),
    };
    let invitation = calendar.as_deref().and_then(overlook_core::ics::parse_invitation);
    let read_receipt = disposition_notification.as_deref().and_then(overlook_core::read_receipt::parse_mdn);
    let is_outgoing = !from.email.is_empty() && from.email.eq_ignore_ascii_case(&account.email);

    Message {
        id: local_id,
        account_id: account.id.clone(),
        conversation_id: ConversationId(String::new()),
        provider_id: flags.provider_id.clone(),
        from,
        to,
        cc,
        bcc,
        reply_to,
        subject,
        snippet,
        body: Body { text, html },
        date: header_date.or(flags.received_at).unwrap_or(0),
        is_read: flags.is_read,
        is_starred: flags.is_starred,
        is_outgoing,
        label_ids: flags.label_ids.clone(),
        attachments,
        rfc_message_id,
        in_reply_to,
        references,
        category: None,
        is_important: false,
        focus_override: None,
        invitation,
        read_receipt,
    }
}

/// Only the headers of a raw message (IMAP `BODY.PEEK[HEADER]`, POP3 `TOP n 0`): the body
/// stays uncached so the sync engine downloads it later.
pub fn headers_from_rfc822(account: &Account, bytes: &[u8], local_id: MessageId, flags: &MessageFlags) -> Message {
    let mut m = message_from_rfc822(account, bytes, local_id, flags);
    m.body = Body::default();
    m.snippet = String::new();
    m.attachments.clear();
    m.read_receipt = None;
    m
}

/// The decoded bytes of the part at IMAP-style section `path` (`1`, `2.1`, …) of a raw
/// message, numbered as [`message_from_rfc822`] numbers attachments. A forwarded
/// `message/rfc822` part yields the nested message's bytes. `None` when there is no such
/// part. Lets POP3 (and anything holding the whole message) serve attachments from disk.
pub fn part_bytes(bytes: &[u8], path: &str) -> Option<Vec<u8>> {
    let parsed = MessageParser::default().parse(bytes)?;
    part_bytes_from_parsed(&parsed, path)
}

/// Extracts part bytes from an already-parsed `mail_parser::Message`.
pub(crate) fn part_bytes_from_parsed(parsed: &mail_parser::Message<'_>, path: &str) -> Option<Vec<u8>> {
    let mut id = 0u32;
    if path != "1" || matches!(parsed.parts.first().map(|p| &p.body), Some(PartType::Multipart(_))) {
        for index in path.split('.') {
            let n: usize = index.parse().ok()?;
            let PartType::Multipart(children) = &parsed.parts.get(id as usize)?.body else { return None };
            id = *children.get(n.checked_sub(1)?)?;
        }
    }
    Some(match &parsed.parts.get(id as usize)?.body {
        PartType::Binary(b) | PartType::InlineBinary(b) => b.to_vec(),
        PartType::Text(t) | PartType::Html(t) => t.as_bytes().to_vec(),
        PartType::Message(m) => m.raw_message.to_vec(),
        PartType::Multipart(_) => return None,
    })
}

fn addresses(list: Option<&mail_parser::Address<'_>>) -> Vec<Address> {
    let mut out = Vec::new();
    if let Some(list) = list {
        for addr in list.iter() {
            let Some(email) = addr.address.as_deref().map(str::trim).filter(|e| !e.is_empty()) else { continue };
            let name = addr.name.as_deref().map(|n| n.trim().trim_matches('"').to_owned()).filter(|n| !n.is_empty());
            out.push(Address { name, email: email.to_owned() });
        }
    }
    out
}

fn snippet_of(text: &str) -> String {
    overlook_core::clean_snippet(text, SNIPPET_CHARS)
}

#[derive(Default)]
struct Collected {
    text: Option<String>,
    html: Option<String>,
    calendar: Option<String>,
    disposition_notification: Option<String>,
    attachments: Vec<Attachment>,
}

/// Walks the part tree. `path` is the IMAP section number of the part (`1`, `2.1`, …);
/// `None` for the root, whose only body is section `1`.
fn walk(parts: &[MessagePart<'_>], id: u32, path: Option<&str>, out: &mut Collected, local_id: &MessageId) {
    let Some(part) = parts.get(id as usize) else { return };
    match &part.body {
        PartType::Multipart(children) => {
            for (i, child) in children.iter().enumerate() {
                let child_path = match path {
                    Some(p) => format!("{p}.{}", i + 1),
                    None => format!("{}", i + 1),
                };
                walk(parts, *child, Some(&child_path), out, local_id);
            }
        }
        PartType::Message(nested) => {
            let is_mdn = part.content_type().is_some_and(|ct| {
                ct.ctype().eq_ignore_ascii_case("message")
                    && ct.subtype().is_some_and(|s| s.eq_ignore_ascii_case("disposition-notification"))
            });
            if is_mdn {
                if out.disposition_notification.is_none() {
                    out.disposition_notification = Some(String::from_utf8_lossy(nested.raw_message.as_ref()).into_owned());
                }
                return;
            }
            // A forwarded message as an attachment. Its own parts are not expanded.
            let filename = part.attachment_name().map(str::to_owned).unwrap_or_else(|| {
                let subject = nested.subject().map(str::trim).filter(|s| !s.is_empty()).unwrap_or("forwarded message");
                format!("{subject}.eml")
            });
            push_attachment(out, local_id, filename, "message/rfc822".into(), nested.raw_message.len() as u64, path.unwrap_or("1"), None);
        }
        PartType::Text(_) | PartType::Html(_) | PartType::Binary(_) | PartType::InlineBinary(_) => leaf(part, path.unwrap_or("1"), out, local_id),
    }
}

fn leaf(part: &MessagePart<'_>, path: &str, out: &mut Collected, local_id: &MessageId) {
    let mime = part
        .content_type()
        .map(|ct| match ct.subtype() {
            Some(sub) => format!("{}/{}", ct.ctype(), sub),
            None => ct.ctype().to_owned(),
        })
        .unwrap_or_else(|| {
            match &part.body {
                PartType::Html(_) => "text/html",
                PartType::Text(_) => "text/plain",
                _ => "application/octet-stream",
            }
            .to_owned()
        })
        .to_ascii_lowercase();
    let filename = part.attachment_name().map(str::trim).filter(|f| !f.is_empty()).map(str::to_owned);
    let content_id = part.content_id().map(|v| v.trim().trim_start_matches('<').trim_end_matches('>').to_owned()).filter(|v| !v.is_empty());
    let disposition_attachment = part.content_disposition().is_some_and(|d| d.ctype().eq_ignore_ascii_case("attachment"));
    let (is_binary, size) = match &part.body {
        PartType::Text(t) | PartType::Html(t) => (false, t.len()),
        PartType::Binary(b) | PartType::InlineBinary(b) => (true, b.len()),
        _ => (false, 0),
    };

    if mime == "message/disposition-notification" {
        if out.disposition_notification.is_none() {
            out.disposition_notification = match &part.body {
                PartType::Text(t) | PartType::Html(t) => Some(t.to_string()),
                PartType::Binary(b) | PartType::InlineBinary(b) => Some(String::from_utf8_lossy(b).into_owned()),
                _ => None,
            };
        }
        return;
    }

    if mime.starts_with("text/calendar") && out.calendar.is_none() {
        out.calendar = match &part.body {
            PartType::Text(t) | PartType::Html(t) => Some(t.to_string()),
            PartType::Binary(b) | PartType::InlineBinary(b) => Some(String::from_utf8_lossy(b).into_owned()),
            _ => None,
        };
    }

    let is_attachment = filename.is_some() || disposition_attachment || is_binary;
    if is_attachment {
        let filename = filename.unwrap_or_else(|| {
            let ext = mime.split('/').nth(1).map(|e| e.split(';').next().unwrap_or(e)).unwrap_or("bin");
            format!("inline-{}.{}", out.attachments.len() + 1, ext)
        });
        push_attachment(out, local_id, filename, mime, size as u64, path, content_id);
        return;
    }
    match &part.body {
        PartType::Text(t) if (mime == "text/plain" || !mime.starts_with("text/")) && out.text.is_none() => out.text = Some(t.to_string()),
        PartType::Html(h) if out.html.is_none() => out.html = Some(h.to_string()),
        _ => {}
    }
}

fn push_attachment(out: &mut Collected, local_id: &MessageId, filename: String, mime_type: String, size_bytes: u64, path: &str, content_id: Option<String>) {
    out.attachments.push(Attachment {
        id: AttachmentId(format!("{}:att-{}", local_id.0, out.attachments.len())),
        message_id: local_id.clone(),
        filename,
        mime_type,
        size_bytes,
        local_path: None,
        provider_id: Some(ProviderId(path.to_owned())),
        content_id,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account() -> Account {
        Account { id: AccountId::from("imap:jason@example.com"), provider: ProviderKind::Imap, display_name: "Jason".into(), email: "jason@example.com".into(), created_at: 0 }
    }

    fn crlf(s: &str) -> Vec<u8> {
        s.replace('\n', "\r\n").into_bytes()
    }

    fn flags() -> MessageFlags {
        MessageFlags {
            provider_id: Some(ProviderId("INBOX\u{1f}7\u{1f}42".into())),
            label_ids: [LabelId::from("imap:jason@example.com:INBOX")].into_iter().collect(),
            is_read: false,
            is_starred: true,
            received_at: Some(1_760_000_000_000),
        }
    }

    const FULL: &str = "From: =?UTF-8?B?TWF5YSBDaGVu?= <maya@northstar.example>
To: Jason <jason@example.com>, alex@studio.example
Cc: =?utf-8?Q?Caf=C3=A9_Team?= <team@studio.example>
Subject: =?UTF-8?Q?Launch_cr=C3=A9ative?=
Date: Tue, 14 Oct 2026 09:15:00 -0700
Message-ID: <abc@northstar.example>
In-Reply-To: <root@northstar.example>
References: <grand@northstar.example>
 <root@northstar.example>
MIME-Version: 1.0
Content-Type: multipart/mixed; boundary=\"outer\"

--outer
Content-Type: multipart/alternative; boundary=\"inner\"

--inner
Content-Type: text/plain; charset=utf-8
Content-Transfer-Encoding: quoted-printable

Hi Jason & team =E2=80=94 see attached.
--inner
Content-Type: text/html; charset=utf-8

<p>Hi Jason &amp; team \u{2014} see attached.</p>
--inner--
--outer
Content-Type: application/pdf; name=\"brief.pdf\"
Content-Disposition: attachment; filename=\"brief.pdf\"
Content-Transfer-Encoding: base64

JVBERi0xLjQK
--outer
Content-Type: text/calendar; method=REQUEST; charset=utf-8
Content-Disposition: attachment; filename=\"invite.ics\"

BEGIN:VCALENDAR
METHOD:REQUEST
BEGIN:VEVENT
UID:ev1@google.com
SUMMARY:Launch review
DTSTART:20261014T160000Z
END:VEVENT
END:VCALENDAR
--outer--
";

    #[test]
    fn maps_full_message() {
        let m = message_from_rfc822(&account(), &crlf(FULL), MessageId::from("imap:jason@example.com:u1"), &flags());
        assert_eq!(m.id.as_str(), "imap:jason@example.com:u1");
        assert!(m.conversation_id.0.is_empty(), "threading is the sync engine's job");
        assert_eq!(m.provider_id.as_ref().unwrap().as_str(), "INBOX\u{1f}7\u{1f}42");
        assert_eq!(m.label_ids.len(), 1);
        assert!(!m.is_read && m.is_starred && !m.is_outgoing);
        assert_eq!(m.from, Address::named("Maya Chen", "maya@northstar.example"));
        assert_eq!(m.to.len(), 2);
        assert_eq!(m.to[0].name.as_deref(), Some("Jason"));
        assert_eq!(m.to[1], Address::new(None, "alex@studio.example"));
        assert_eq!(m.cc[0].name.as_deref(), Some("Café Team"));
        assert_eq!(m.subject, "Launch créative");
        assert_eq!(m.date, overlook_core::tz::parse_rfc3339("2026-10-14T09:15:00-07:00").unwrap());
        assert_eq!(m.rfc_message_id.as_deref(), Some("<abc@northstar.example>"));
        assert_eq!(m.in_reply_to.as_deref(), Some("<root@northstar.example>"));
        assert_eq!(m.references, vec!["<grand@northstar.example>".to_owned(), "<root@northstar.example>".to_owned()]);
        assert_eq!(m.body.text.as_deref(), Some("Hi Jason & team — see attached."));
        assert_eq!(m.body.html.as_deref(), Some("<p>Hi Jason &amp; team — see attached.</p>"));
        assert_eq!(m.snippet, "Hi Jason & team — see attached.");
        assert_eq!(m.attachments.len(), 2, "{:?}", m.attachments);
        let pdf = &m.attachments[0];
        assert_eq!((pdf.filename.as_str(), pdf.mime_type.as_str(), pdf.size_bytes), ("brief.pdf", "application/pdf", 9));
        assert_eq!(pdf.provider_id.as_ref().unwrap().as_str(), "2", "MIME section path");
        assert_eq!(pdf.id.as_str(), "imap:jason@example.com:u1:att-0");
        assert_eq!(m.attachments[1].provider_id.as_ref().unwrap().as_str(), "3");
        assert!(m.attachments[1].mime_type.starts_with("text/calendar"));
        let inv = m.invitation.as_ref().expect("invitation parsed from the text/calendar part");
        assert_eq!((inv.uid.as_str(), inv.method.as_str()), ("ev1@google.com", "REQUEST"));
        assert_eq!(inv.summary.as_deref(), Some("Launch review"));
    }

    #[test]
    fn inline_images_keep_their_content_id_and_section() {
        let raw = crlf(
            "From: a@b.example
Subject: Logo
Content-Type: multipart/related; boundary=\"rel\"

--rel
Content-Type: text/html; charset=us-ascii

<p>Look:</p><img src=\"cid:logo@x\">
--rel
Content-Type: image/png
Content-ID: <logo@x>
Content-Disposition: inline
Content-Transfer-Encoding: base64

iVBORw0KGgo=
--rel--
",
        );
        let m = message_from_rfc822(&account(), &raw, MessageId::from("m1"), &MessageFlags::default());
        assert_eq!(m.attachments.len(), 1);
        let img = &m.attachments[0];
        assert_eq!(img.content_id.as_deref(), Some("logo@x"));
        assert_eq!(img.filename, "inline-1.png");
        assert!(img.is_inline_image());
        assert_eq!(img.provider_id.as_ref().unwrap().as_str(), "2");
        assert_eq!(img.size_bytes, 8);
        assert!(m.body.html.as_deref().unwrap().contains("cid:logo@x"));
        assert_eq!(m.body.text.as_deref(), Some(html_to_text(m.body.html.as_deref().unwrap()).as_str()), "text derived from HTML when no text part exists");
        assert_eq!(m.snippet, "Look:");
    }

    #[test]
    fn minimal_message_uses_flags_and_sender() {
        let raw = crlf("From: Jason <jason@example.com>\nTo: maya@northstar.example\nSubject: Note to self\nMessage-ID: bare-id@example.com\n\nJust a line.\n");
        let f = MessageFlags { is_read: true, received_at: Some(123_000), ..Default::default() };
        let m = message_from_rfc822(&account(), &raw, MessageId::from("m2"), &f);
        assert!(m.is_outgoing, "from the account owner");
        assert!(m.is_read && !m.is_starred);
        assert_eq!(m.date, 123_000, "no Date header: the server's receive time");
        assert_eq!(m.rfc_message_id.as_deref(), Some("<bare-id@example.com>"), "ids are normalised with brackets");
        assert!(m.in_reply_to.is_none() && m.references.is_empty());
        assert_eq!(m.body.text.as_deref().map(str::trim), Some("Just a line."));
        assert!(m.body.html.is_none());
        assert!(m.attachments.is_empty() && m.invitation.is_none());
        let headers_only = headers_from_rfc822(&account(), &raw, MessageId::from("m2"), &f);
        assert!(!headers_only.body.is_cached() && headers_only.subject == "Note to self");
    }

    #[test]
    fn forwarded_message_and_rfc2231_names() {
        let raw = crlf(
            "From: a@b.example
Subject: Fwd
Content-Type: multipart/mixed; boundary=\"m\"

--m
Content-Type: text/plain

See below.
--m
Content-Type: message/rfc822

From: c@d.example
Subject: Original
Content-Type: text/plain

Original body.
--m
Content-Type: application/pdf
Content-Disposition: attachment; filename*=UTF-8''r%C3%A9sum%C3%A9.pdf

%PDF
--m--
",
        );
        let m = message_from_rfc822(&account(), &raw, MessageId::from("m3"), &MessageFlags::default());
        assert_eq!(m.body.text.as_deref().map(str::trim), Some("See below."));
        assert_eq!(m.attachments.len(), 2, "{:?}", m.attachments);
        assert_eq!((m.attachments[0].filename.as_str(), m.attachments[0].mime_type.as_str()), ("Original.eml", "message/rfc822"));
        assert_eq!(m.attachments[0].provider_id.as_ref().unwrap().as_str(), "2");
        assert_eq!(m.attachments[1].filename, "résumé.pdf");
        assert_eq!(m.attachments[1].provider_id.as_ref().unwrap().as_str(), "3");
    }

    #[test]
    fn malformed_messages_never_panic() {
        let acct = account();
        let f = MessageFlags { received_at: Some(5_000), ..Default::default() };

        let empty = message_from_rfc822(&acct, b"", MessageId::from("e"), &f);
        assert_eq!(empty.id.as_str(), "e");
        assert!(!empty.snippet.is_empty(), "something is shown for an unreadable message");
        assert_eq!(empty.date, 5_000);

        let garbage = message_from_rfc822(&acct, b"\xff\xfe\x00 not a message at all", MessageId::from("g"), &f);
        assert!(garbage.from.email.is_empty());
        assert!(garbage.body.text.as_deref().is_some_and(|t| t.contains("not a message")) || !garbage.snippet.is_empty());

        let truncated = message_from_rfc822(
            &acct,
            &crlf("From: a@b.example\nSubject: Cut off\nContent-Type: multipart/mixed; boundary=\"q\"\n\n--q\nContent-Type: text/plain\n\nStill readable"),
            MessageId::from("t"),
            &f,
        );
        assert_eq!(truncated.subject, "Cut off");
        assert!(truncated.body.text.as_deref().is_some_and(|t| t.contains("Still readable")), "{:?}", truncated.body);

        let bad_date = message_from_rfc822(&acct, &crlf("From: a@b.example\nDate: yesterday, sometime\nSubject: When\n\nx"), MessageId::from("d"), &f);
        assert_eq!(bad_date.date, 5_000, "unparseable Date falls back to the receive time");

        let bad_utf8 = message_from_rfc822(&acct, b"From: a@b.example\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nCaf\xe9 au lait\r\n", MessageId::from("u"), &f);
        assert!(bad_utf8.body.text.as_deref().is_some_and(|t| t.contains("Caf") && t.contains("au lait")));

        let hostile_html = include_str!("../../../overlook-core/tests/fixtures/formatting/unsafe_malformed.html");
        let raw = format!("From: a@b.example\r\nSubject: Hostile\r\nContent-Type: text/html; charset=utf-8\r\n\r\n{hostile_html}");
        let hostile = message_from_rfc822(&acct, raw.as_bytes(), MessageId::from("h"), &f);
        assert!(hostile.body.html.is_some());
        assert!(hostile.body.text.is_some(), "text derived even from malformed HTML");

        let no_body = message_from_rfc822(&acct, b"Subject: Only a subject\r\n\r\n", MessageId::from("s"), &f);
        assert_eq!(no_body.subject, "Only a subject");
        assert!(no_body.attachments.is_empty());

        // A header line without any line ending is not a header; the bytes are still shown.
        let unterminated = message_from_rfc822(&acct, b"Subject: Only a subject", MessageId::from("s2"), &f);
        assert!(unterminated.snippet.contains("Only a subject"), "{unterminated:?}");

        // Huge unreadable garbage is capped rather than copied whole into body.text.
        let huge_garbage = vec![b'X'; MAX_FALLBACK_TEXT_BYTES * 4];
        let huge = message_from_rfc822(&acct, &huge_garbage, MessageId::from("huge"), &f);
        assert_eq!(huge.body.text.as_deref().map(str::len), Some(MAX_FALLBACK_TEXT_BYTES));
    }

    #[test]
    fn part_bytes_follow_attachment_section_paths() {
        let account = account();
        let raw = b"From: a@example.com\r\nTo: b@example.com\r\nSubject: parts\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=\"outer\"\r\n\r\n--outer\r\nContent-Type: multipart/alternative; boundary=\"inner\"\r\n\r\n--inner\r\nContent-Type: text/plain\r\n\r\nText.\r\n--inner\r\nContent-Type: text/html\r\n\r\n<p>Html</p>\r\n--inner--\r\n--outer\r\nContent-Type: application/pdf; name=\"r.pdf\"\r\nContent-Disposition: attachment; filename=\"r.pdf\"\r\nContent-Transfer-Encoding: base64\r\n\r\nJVBERi0xLjQK\r\n--outer--\r\n";
        let m = message_from_rfc822(&account, raw, MessageId::from("m"), &MessageFlags::default());
        let section = m.attachments[0].provider_id.as_ref().unwrap().0.clone();
        assert_eq!(section, "2");
        assert_eq!(part_bytes(raw, &section).unwrap(), b"%PDF-1.4\n");
        assert_eq!(part_bytes(raw, "1.1").unwrap(), b"Text.", "mail-parser trims the boundary CRLF");
        assert!(part_bytes(raw, "1").is_none(), "a multipart container has no bytes of its own");
        assert!(part_bytes(raw, "7").is_none());
        let plain = b"From: a@example.com\r\nSubject: p\r\nContent-Type: text/plain\r\n\r\nHello\r\n";
        assert_eq!(part_bytes(plain, "1").unwrap(), b"Hello\r\n");
    }

    #[test]
    fn parses_incoming_rfc8098_mdn_receipt_message() {
        let raw = crlf(
            "From: Maya Chen <maya@northstar.example>
To: Jason <jason@example.com>
Subject: Read: Contract review
Date: Tue, 14 Oct 2026 10:02:00 -0700
Message-ID: <mdn-1@northstar.example>
MIME-Version: 1.0
Content-Type: multipart/report; report-type=disposition-notification; boundary=\"mdn-bnd\"

--mdn-bnd
Content-Type: text/plain; charset=utf-8

This is a Return Receipt for the mail that you sent to maya@northstar.example.
--mdn-bnd
Content-Type: message/disposition-notification

Reporting-UA: mail.northstar.example; Mozilla Thunderbird 128.0
Final-Recipient: rfc822; maya@northstar.example
Original-Message-ID: <optin-smtp@overlook.example>
Disposition: manual-action/MDN-sent-manually; displayed
--mdn-bnd
Content-Type: text/rfc822-headers

From: Jason <jason@example.com>
To: Maya Chen <maya@northstar.example>
Subject: Contract review
Message-ID: <optin-smtp@overlook.example>
Disposition-Notification-To: jason@example.com
--mdn-bnd--
",
        );

        let m = message_from_rfc822(&account(), &raw, MessageId::from("m-mdn"), &MessageFlags::default());
        assert_eq!(m.subject, "Read: Contract review");
        assert!(
            m.body.text.as_deref().unwrap_or("").contains("Return Receipt"),
            "human-readable report text is preserved: {:?}",
            m.body.text
        );
        assert!(
            !m.attachments.iter().any(|a| a.mime_type == "message/disposition-notification"),
            "MDN metadata part must not appear as a bogus attachment: {:?}",
            m.attachments
        );
        let receipt = m.read_receipt.as_ref().expect("RFC 8098 MDN receipt parsed");
        assert!(receipt.is_displayed());
        assert_eq!(receipt.original_message_id.as_deref(), Some("<optin-smtp@overlook.example>"));
        assert_eq!(receipt.final_recipient.as_deref(), Some("maya@northstar.example"));
        assert_eq!(receipt.disposition, "manual-action/MDN-sent-manually; displayed");
        assert_eq!(receipt.summary_banner(), "Read receipt from the recipient's mail client");
    }

    #[test]
    fn ignores_non_mdn_delivery_status_reports() {
        let raw = crlf(
            "From: Mail Delivery Subsystem <mailer-daemon@example.com>
To: jason@example.com
Subject: Delivery Status Notification (Failure)
MIME-Version: 1.0
Content-Type: multipart/report; report-type=delivery-status; boundary=\"dsn-bnd\"

--dsn-bnd
Content-Type: text/plain

Address not found.
--dsn-bnd
Content-Type: message/delivery-status

Reporting-MTA: dns; mx.example.com
Final-Recipient: rfc822; missing@example.com
Action: failed
Status: 5.1.1
--dsn-bnd--
",
        );
        let m = message_from_rfc822(&account(), &raw, MessageId::from("m-dsn"), &MessageFlags::default());
        assert!(m.read_receipt.is_none());
    }

    #[test]
    fn snippet_falls_back_to_html_when_plain_is_blank_and_strips_preheader_spacers() {
        // 1. Blank text/plain part alongside real HTML body.
        let raw_blank_plain = crlf(
            "From: Carl Friedrik <contact@carlfriedrik.com>
To: jason@example.com
Subject: Your order is on its way! (#A60412)
MIME-Version: 1.0
Content-Type: multipart/alternative; boundary=\"alt-bnd\"

--alt-bnd
Content-Type: text/plain; charset=utf-8

   
--alt-bnd
Content-Type: text/html; charset=utf-8

<div>&zwnj;&nbsp;&#847;&zwnj;&nbsp;&#847;&zwnj;&nbsp;&#847;</div><p>Your order is on the way</p><p>Hi Jason,</p>
--alt-bnd--
",
        );
        let m = message_from_rfc822(&account(), &raw_blank_plain, MessageId::from("m-blank-plain"), &MessageFlags::default());
        assert_eq!(m.snippet, "Your order is on the way Hi Jason,");

        // 2. HTML body padded with 200+ invisible preheader spacer characters before visible text.
        let spacer = "&zwnj;&nbsp;&#847;".repeat(120);
        let raw_padded_html = crlf(&format!(
            "From: Carl Friedrik <contact@carlfriedrik.com>
To: jason@example.com
Subject: Your order is on its way! (#A60412)
MIME-Version: 1.0
Content-Type: text/html; charset=utf-8

<html><body><div>{spacer}</div><h1>Your order is on the way</h1><p>Hi Jason,</p></body></html>
"
        ));
        let m2 = message_from_rfc822(&account(), &raw_padded_html, MessageId::from("m-padded-html"), &MessageFlags::default());
        assert_eq!(m2.snippet, "Your order is on the way Hi Jason,");
    }
}
