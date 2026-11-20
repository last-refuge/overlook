//! Partial download of large messages through `BODYSTRUCTURE`.
//!
//! Messages over [`LARGE_MESSAGE_BYTES`] are not fetched whole. The structure is read, the
//! text parts (and a calendar part) are fetched by section, and a small synthetic message
//! is assembled for the shared MIME parser; attachments are listed from the structure with
//! their section numbers so they can be fetched on click, like every other attachment.

use imap_proto::types::{BodyStructure, ContentDisposition, ContentType};

/// Messages at or above this size take the structure path.
pub const LARGE_MESSAGE_BYTES: u32 = 2 * 1024 * 1024;

/// How much of each text part is fetched (`<0.N>` partial fetch).
pub const TEXT_PART_LIMIT: u32 = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartRef {
    pub section: String,
    pub filename: Option<String>,
    pub mime: String,
    pub size: u64,
    pub content_id: Option<String>,
}

/// Which sections to fetch and which to list.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Plan {
    pub text: Option<String>,
    pub html: Option<String>,
    pub calendar: Option<String>,
    pub attachments: Vec<PartRef>,
    /// The message has no MIME multipart: section `1` is the whole body.
    pub single_part: bool,
}

impl Plan {
    /// Sections to fetch, in order, without duplicates.
    pub fn text_sections(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for s in [&self.text, &self.html, &self.calendar].into_iter().flatten() {
            if !out.contains(s) {
                out.push(s.clone());
            }
        }
        out
    }
}

pub fn plan(structure: &BodyStructure<'_>) -> Plan {
    let mut plan = Plan { single_part: !matches!(structure, BodyStructure::Multipart { .. }), ..Default::default() };
    walk(structure, &[], &mut plan);
    plan
}

fn section_of(path: &[u32]) -> String {
    if path.is_empty() {
        "1".to_owned()
    } else {
        path.iter().map(|n| n.to_string()).collect::<Vec<_>>().join(".")
    }
}

fn param<'a>(params: &'a Option<Vec<(std::borrow::Cow<'a, str>, std::borrow::Cow<'a, str>)>>, name: &str) -> Option<String> {
    params.as_ref()?.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.trim().to_owned()).filter(|v| !v.is_empty())
}

fn mime_of(ty: &ContentType<'_>) -> String {
    format!("{}/{}", ty.ty, ty.subtype).to_ascii_lowercase()
}

fn filename_of(ty: &ContentType<'_>, disposition: Option<&ContentDisposition<'_>>) -> Option<String> {
    disposition.and_then(|d| param(&d.params, "filename")).or_else(|| param(&ty.params, "name")).map(|n| crate::mime::decode_rfc2047(&n))
}

fn walk(bs: &BodyStructure<'_>, path: &[u32], plan: &mut Plan) {
    match bs {
        BodyStructure::Multipart { bodies, .. } => {
            for (i, child) in bodies.iter().enumerate() {
                let mut p = path.to_vec();
                p.push(i as u32 + 1);
                walk(child, &p, plan);
            }
        }
        BodyStructure::Text { common, other, .. } => {
            let mime = mime_of(&common.ty);
            let filename = filename_of(&common.ty, common.disposition.as_ref());
            let disposition_attachment = common.disposition.as_ref().is_some_and(|d| d.ty.eq_ignore_ascii_case("attachment"));
            let section = section_of(path);
            if mime == "text/calendar" && plan.calendar.is_none() {
                plan.calendar = Some(section.clone());
            }
            if filename.is_some() || disposition_attachment {
                plan.attachments.push(PartRef { section, filename, mime, size: other.octets as u64, content_id: strip_id(other.id.as_deref()) });
                return;
            }
            match mime.as_str() {
                "text/plain" if plan.text.is_none() => plan.text = Some(section),
                "text/html" if plan.html.is_none() => plan.html = Some(section),
                _ => {}
            }
        }
        BodyStructure::Basic { common, other, .. } => {
            let mime = mime_of(&common.ty);
            let filename = filename_of(&common.ty, common.disposition.as_ref()).unwrap_or_else(|| {
                let ext = mime.split('/').nth(1).unwrap_or("bin");
                format!("inline-{}.{}", plan.attachments.len() + 1, ext)
            });
            plan.attachments.push(PartRef { section: section_of(path), filename: Some(filename), mime, size: other.octets as u64, content_id: strip_id(other.id.as_deref()) });
        }
        BodyStructure::Message { common, other, envelope, .. } => {
            let filename = filename_of(&common.ty, common.disposition.as_ref()).unwrap_or_else(|| {
                let subject = envelope.subject.as_deref().map(|s| crate::mime::decode_rfc2047(&String::from_utf8_lossy(s))).filter(|s| !s.trim().is_empty()).unwrap_or_else(|| "forwarded message".into());
                format!("{subject}.eml")
            });
            plan.attachments.push(PartRef { section: section_of(path), filename: Some(filename), mime: "message/rfc822".into(), size: other.octets as u64, content_id: None });
        }
    }
}

fn strip_id(id: Option<&str>) -> Option<String> {
    id.map(|v| v.trim().trim_start_matches('<').trim_end_matches('>').to_owned()).filter(|v| !v.is_empty())
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && haystack.windows(needle.len()).any(|w| w == needle)
}

fn unique_boundary(parts: &[(Vec<u8>, Vec<u8>)]) -> String {
    loop {
        let bytes: [u8; 16] = rand::random();
        let mut candidate = String::with_capacity("overlook-partial-".len() + 32);
        candidate.push_str("overlook-partial-");
        for b in bytes {
            use std::fmt::Write;
            let _ = write!(&mut candidate, "{b:02x}");
        }
        let needle = candidate.as_bytes();
        if !parts.iter().any(|(h, b)| contains_bytes(h, needle) || contains_bytes(b, needle)) {
            return candidate;
        }
    }
}

/// Builds the message the MIME parser sees: the original headers minus their body framing,
/// plus one synthetic multipart holding the fetched text sections with their own MIME
/// headers. `parts` is `(section MIME headers, section body)`.
pub fn synthetic_message(header: &[u8], parts: &[(Vec<u8>, Vec<u8>)], single_part: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(header.len() + parts.iter().map(|(h, b)| h.len() + b.len() + 64).sum::<usize>());
    if single_part {
        out.extend_from_slice(header.trim_ascii_end());
        out.extend_from_slice(b"\r\n\r\n");
        if let Some((_, body)) = parts.first() {
            out.extend_from_slice(body);
        }
        return out;
    }
    let boundary = unique_boundary(parts);
    out.extend_from_slice(&strip_framing_headers(header));
    out.extend_from_slice(format!("MIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=\"{boundary}\"\r\n\r\n").as_bytes());
    for (mime, body) in parts {
        out.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        out.extend_from_slice(mime.trim_ascii_end());
        out.extend_from_slice(b"\r\n\r\n");
        out.extend_from_slice(body);
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    out
}

/// The header block without `Content-Type`, `Content-Transfer-Encoding` and `MIME-Version`
/// (folded continuation lines included), ending with one CRLF.
fn strip_framing_headers(header: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(header);
    let mut out = String::new();
    let mut skipping = false;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if line.starts_with([' ', '\t']) {
            if !skipping {
                out.push_str(trimmed);
                out.push_str("\r\n");
            }
            continue;
        }
        let name = trimmed.split(':').next().unwrap_or("").trim().to_ascii_lowercase();
        skipping = matches!(name.as_str(), "content-type" | "content-transfer-encoding" | "mime-version" | "content-disposition");
        if !skipping {
            out.push_str(trimmed);
            out.push_str("\r\n");
        }
    }
    out.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use overlook_core::*;

    fn parse_structure(line: &str) -> BodyStructure<'static> {
        let raw = format!("* 1 FETCH (BODYSTRUCTURE {line})\r\n");
        let bytes: &'static [u8] = Box::leak(raw.into_bytes().into_boxed_slice());
        match imap_proto::parser::parse_response(bytes).expect("parses").1 {
            imap_proto::Response::Fetch(_, attrs) => attrs
                .into_iter()
                .find_map(|a| match a {
                    imap_proto::AttributeValue::BodyStructure(bs) => Some(bs),
                    _ => None,
                })
                .expect("bodystructure"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn plans_text_parts_and_attachments() {
        // multipart/mixed: multipart/alternative(text/plain, text/html), image/png inline, application/pdf attachment, text/calendar
        let bs = parse_structure(
            r#"((("TEXT" "PLAIN" ("CHARSET" "utf-8") NIL NIL "7BIT" 12 1 NIL NIL NIL NIL)("TEXT" "HTML" ("CHARSET" "utf-8") NIL NIL "7BIT" 40 1 NIL NIL NIL NIL) "ALTERNATIVE" ("BOUNDARY" "inner") NIL NIL NIL)("IMAGE" "PNG" ("NAME" "logo.png") "<logo>" NIL "BASE64" 12 NIL ("INLINE" ("FILENAME" "logo.png")) NIL NIL)("APPLICATION" "PDF" ("NAME" "report.pdf") NIL NIL "BASE64" 3000000 NIL ("ATTACHMENT" ("FILENAME" "report.pdf")) NIL NIL)("TEXT" "CALENDAR" ("METHOD" "REQUEST") NIL NIL "7BIT" 90 4 NIL NIL NIL NIL) "MIXED" ("BOUNDARY" "outer") NIL NIL NIL)"#,
        );
        let p = plan(&bs);
        assert!(!p.single_part);
        assert_eq!(p.text.as_deref(), Some("1.1"));
        assert_eq!(p.html.as_deref(), Some("1.2"));
        assert_eq!(p.calendar.as_deref(), Some("4"));
        assert_eq!(p.text_sections(), vec!["1.1", "1.2", "4"]);
        assert_eq!(p.attachments.len(), 2, "{:?}", p.attachments);
        assert_eq!(p.attachments[0], PartRef { section: "2".into(), filename: Some("logo.png".into()), mime: "image/png".into(), size: 12, content_id: Some("logo".into()) });
        assert_eq!(p.attachments[1].section, "3");
        assert_eq!(p.attachments[1].filename.as_deref(), Some("report.pdf"));
        assert_eq!(p.attachments[1].size, 3_000_000);
    }

    #[test]
    fn single_part_and_forwarded_message() {
        let bs = parse_structure(r#"("TEXT" "PLAIN" ("CHARSET" "us-ascii") NIL NIL "7BIT" 5000000 100000 NIL NIL NIL NIL)"#);
        let p = plan(&bs);
        assert!(p.single_part);
        assert_eq!(p.text.as_deref(), Some("1"));
        let bs = parse_structure(
            r#"(("TEXT" "PLAIN" ("CHARSET" "us-ascii") NIL NIL "7BIT" 10 1 NIL NIL NIL NIL)("MESSAGE" "RFC822" NIL NIL NIL "7BIT" 500 ("Mon, 12 Oct 2026 14:30:00 +0000" "Original" NIL NIL NIL NIL NIL NIL NIL NIL) ("TEXT" "PLAIN" NIL NIL NIL "7BIT" 20 1 NIL NIL NIL NIL) 10 NIL NIL NIL NIL) "MIXED" ("BOUNDARY" "m") NIL NIL NIL)"#,
        );
        let p = plan(&bs);
        assert_eq!(p.attachments.len(), 1);
        assert_eq!((p.attachments[0].section.as_str(), p.attachments[0].filename.as_deref(), p.attachments[0].mime.as_str()), ("2", Some("Original.eml"), "message/rfc822"));
    }

    #[test]
    fn synthetic_message_parses_with_the_shared_parser() {
        let header = b"From: a@b.example\r\nSubject: Big\r\nMessage-ID: <big@x>\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed;\r\n boundary=\"outer\"\r\n\r\n";
        let parts = vec![
            (b"Content-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: quoted-printable\r\n\r\n".to_vec(), b"Caf=C3=A9".to_vec()),
            (b"Content-Type: text/html; charset=utf-8\r\n".to_vec(), b"<p>Caf\xc3\xa9</p>".to_vec()),
        ];
        let bytes = synthetic_message(header, &parts, false);
        let text = String::from_utf8_lossy(&bytes);
        assert!(!text.contains("boundary=\"outer\""), "{text}");
        assert!(text.contains("Subject: Big\r\n"));
        let account = Account { id: AccountId::from("imap:j"), provider: ProviderKind::Imap, display_name: "J".into(), email: "j@example.com".into(), created_at: 0 };
        let m = crate::mime::message_from_rfc822(&account, &bytes, MessageId::from("m"), &crate::mime::MessageFlags::default());
        assert_eq!(m.subject, "Big");
        assert_eq!(m.rfc_message_id.as_deref(), Some("<big@x>"));
        assert_eq!(m.body.text.as_deref(), Some("Café"));
        assert_eq!(m.body.html.as_deref(), Some("<p>Café</p>"));
        assert!(m.attachments.is_empty());

        let single = synthetic_message(b"From: a@b.example\r\nSubject: Plain\r\nContent-Type: text/plain\r\n\r\n", &[(Vec::new(), b"hello".to_vec())], true);
        let m = crate::mime::message_from_rfc822(&account, &single, MessageId::from("m"), &crate::mime::MessageFlags::default());
        assert_eq!(m.body.text.as_deref().map(str::trim), Some("hello"));
    }

    #[test]
    fn synthetic_message_uses_unique_random_boundary_per_call_and_resists_spoofing() {
        let header = b"From: a@b.example\r\nSubject: Spoof\r\n\r\n";
        let spoofed = b"Before\r\n--overlook-partial-fetch\r\nContent-Type: text/html\r\n\r\n<script>evil</script>".to_vec();
        let parts = vec![(b"Content-Type: text/plain; charset=utf-8\r\n".to_vec(), spoofed.clone())];
        let a = synthetic_message(header, &parts, false);
        let b = synthetic_message(header, &parts, false);
        let a_text = String::from_utf8_lossy(&a);
        let b_text = String::from_utf8_lossy(&b);
        let extract_boundary = |s: &str| {
            s.split("boundary=\"").nth(1).and_then(|rest| rest.split('"').next()).unwrap().to_owned()
        };
        let ba = extract_boundary(&a_text);
        let bb = extract_boundary(&b_text);
        assert_ne!(ba, bb, "each call generates a distinct random boundary");
        assert_ne!(ba, "overlook-partial-fetch");
        let account = Account { id: AccountId::from("imap:j"), provider: ProviderKind::Imap, display_name: "J".into(), email: "j@example.com".into(), created_at: 0 };
        let m = crate::mime::message_from_rfc822(&account, &a, MessageId::from("m"), &crate::mime::MessageFlags::default());
        assert!(m.body.html.is_none(), "spoofed boundary in body must not inject an HTML part");
        assert!(m.body.text.as_deref().unwrap().contains("--overlook-partial-fetch"));
    }
}
