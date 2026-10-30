//! Outgoing message bodies.
//!
//! The composer edits Markdown-flavoured text: `**bold**`, `*italic*`, `~~strike~~`,
//! `` `code` ``, `[text](url)`, `- ` and `1. ` lists, `> ` quotes, `# ` headings, bare
//! URLs. [`render_markdown`] turns it into the HTML part of the message and
//! [`markdown_to_plain`] into the text part. [`outgoing_bodies`] appends the quoted original
//! of a reply or forward to both.

use crate::model::{forward_header, quote_header, quote_text, Draft, DraftKind, Message};
use crate::sanitize::{clean_only, escape_html, html_to_text, text_to_html, RemoteContent};

/// Renders the composer's text as HTML. Raw HTML typed by the user is escaped, not passed
/// through, so a message can never carry markup the author did not see.
pub fn render_markdown(src: &str) -> String {
    let mut options = markdown::Options::gfm();
    options.compile.allow_dangerous_html = false;
    options.compile.allow_dangerous_protocol = false;
    // A single newline inside a paragraph is a line break, the way mail is typed.
    options.parse.constructs.hard_break_trailing = true;
    let html = markdown::to_html_with_options(src, &options).unwrap_or_else(|_| text_to_html(src));
    soft_breaks_to_br(&html)
}

/// The markdown crate keeps a soft line break as a newline inside the paragraph; mail
/// readers collapse that to a space, so it becomes `<br>`.
fn soft_breaks_to_br(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_pre = false;
    let mut in_tag = false;
    for (i, c) in html.char_indices() {
        if c == '<' {
            in_tag = true;
            if html[i..].starts_with("<pre") {
                in_pre = true;
            } else if html[i..].starts_with("</pre") {
                in_pre = false;
            }
        } else if c == '>' {
            in_tag = false;
        }
        if c == '\n' && !in_tag && !in_pre {
            // A newline right after a block close or before a block open is layout, not a break.
            let before = html[..i].trim_end_matches(' ');
            let after = html[i + 1..].trim_start_matches(' ');
            if ends_at_block_boundary(before) || starts_at_block_boundary(after) {
                out.push('\n');
            } else {
                out.push_str("<br>\n");
            }
        } else {
            out.push(c);
        }
    }
    out
}

// Inline markup is not a block boundary: **Hello** followed by a newline must
// still produce a mail line break. Generated block separators and existing <br>
// elements must not acquire an extra break.
fn boundary_tag(tag: &str) -> bool {
    let name = tag.trim_start_matches(['<', '/']).split(['>', ' ', '\t', '\n']).next().unwrap_or_default();
    matches!(name, "p" | "div" | "br" | "hr" | "pre" | "blockquote" | "ul" | "ol" | "li" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "table" | "thead" | "tbody" | "tr" | "td" | "th")
}

fn ends_at_block_boundary(text: &str) -> bool {
    text.ends_with('>') && text.rfind('<').is_some_and(|start| boundary_tag(&text[start..]))
}

fn starts_at_block_boundary(text: &str) -> bool {
    text.starts_with('<') && boundary_tag(text)
}

/// The text part: the author's words with the formatting marks removed, links kept readable.
pub fn markdown_to_plain(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    for (i, line) in src.lines().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let trimmed = line.trim_start();
        let indent = &line[..line.len() - trimmed.len()];
        // Headings lose their hashes; everything else keeps its prefix (lists, quotes).
        let body = if let Some(rest) = trimmed.strip_prefix('#') {
            rest.trim_start_matches('#').trim_start()
        } else {
            trimmed
        };
        out.push_str(indent);
        out.push_str(&strip_inline(body));
    }
    out
}

fn strip_inline(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    let mut open_star = false;
    let mut open_under = false;
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix("**").or_else(|| rest.strip_prefix("__")).or_else(|| rest.strip_prefix("~~")) {
            rest = after;
            continue;
        }
        if rest.starts_with('`') {
            rest = &rest[1..];
            continue;
        }
        if rest.starts_with('[') {
            if let Some((text, url, len)) = parse_link(rest) {
                out.push_str(text);
                if !url.is_empty() && url != text {
                    out.push_str(" (");
                    out.push_str(url);
                    out.push(')');
                }
                rest = &rest[len..];
                continue;
            }
        }
        // Single `*` or `_` emphasis: a marker closes an open run, or opens one when a closer
        // follows on the line. `_` only counts at a word boundary, so snake_case stays intact.
        if rest.starts_with('*') || rest.starts_with('_') {
            let marker = rest.as_bytes()[0];
            let open = if marker == b'*' { &mut open_star } else { &mut open_under };
            let at_word_boundary = marker == b'*' || !out.chars().next_back().is_some_and(|c| c.is_alphanumeric());
            if *open {
                *open = false;
                rest = &rest[1..];
                continue;
            }
            if at_word_boundary && emphasis_closes(rest) {
                *open = true;
                rest = &rest[1..];
                continue;
            }
        }
        let ch = rest.chars().next().unwrap();
        out.push(ch);
        rest = &rest[ch.len_utf8()..];
    }
    out
}

fn emphasis_closes(rest: &str) -> bool {
    let marker = rest.as_bytes()[0];
    let inner = &rest[1..];
    if inner.starts_with(' ') {
        return false;
    }
    match inner.find(marker as char) {
        Some(0) | None => false,
        Some(i) => marker == b'*' || !inner[i + 1..].starts_with(|c: char| c.is_alphanumeric()),
    }
}

/// `[text](url)` at the start of `s`: (text, url, consumed length).
pub fn parse_link(s: &str) -> Option<(&str, &str, usize)> {
    let close = s.find("](")?;
    let text = &s[1..close];
    if text.contains('[') || text.contains('\n') {
        return None;
    }
    let url_start = close + 2;
    let url_end = s[url_start..].find(')')? + url_start;
    let url = &s[url_start..url_end];
    if url.contains(' ') {
        return None;
    }
    Some((text, url, url_end + 1))
}

/// The visible HTML of `source` as a reply or forward quotes it.
pub fn quoted_html(source: &Message) -> String {
    match (&source.body.html, &source.body.text) {
        (Some(h), _) => clean_only(h, RemoteContent::Allowed),
        (None, Some(t)) => text_to_html(t),
        (None, None) => text_to_html(&source.snippet),
    }
}

/// Whether `html` (as returned by [`quoted_html`] or [`outgoing_bodies`]) embeds the inline
/// part with this `Content-ID`, which is written without angle brackets.
pub fn embeds_content_id(html: &str, content_id: &str) -> bool {
    html.contains(&format!("cid:{content_id}\""))
}

/// Both parts of an outgoing message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutgoingBodies {
    pub text: String,
    pub html: String,
}

/// Builds the text and HTML parts of `draft`, appending the quoted `source` when the draft
/// asks for it. Replies get an attribution line and a blockquote; forwards get a header block
/// and the original in full.
pub fn outgoing_bodies(draft: &Draft, source: Option<&Message>) -> OutgoingBodies {
    let mut text = markdown_to_plain(&draft.body_text);
    let mut html = render_markdown(&draft.body_text);
    // The signature goes under what was written and above the quoted message. Its plain-text
    // form follows the "-- " line that mail readers know to fold away.
    if let Some(signature) = draft.signature_html.as_deref().filter(|s| !s.trim().is_empty()) {
        let signature = clean_only(signature, RemoteContent::Allowed);
        text = format!("{}\n\n-- \n{}", text.trim_end(), html_to_text(&signature));
        html = format!("{html}<br><div class=\"overlook_signature\">{signature}</div>");
    }
    let Some(source) = source.filter(|_| draft.include_quote) else {
        return OutgoingBodies { text, html };
    };
    let source_html = quoted_html(source);
    // Use the same visible source for both MIME alternatives. A provider-generated
    // text derivative may predate our visibility/security policy.
    let source_text = if source.body.html.is_some() {
        html_to_text(&source_html)
    } else {
        source.body.text.clone().unwrap_or_else(|| source.snippet.clone())
    };
    match draft.kind {
        DraftKind::Forward => {
            let header = forward_header(source);
            text = format!("{text}\n\n{header}\n\n{source_text}");
            let header_html = header
                .lines()
                .map(|l| match l.split_once(": ") {
                    Some((k, v)) if !l.starts_with('-') => format!("<b>{}:</b> {}", escape_html(k), escape_html(v)),
                    _ => escape_html(l),
                })
                .collect::<Vec<_>>()
                .join("<br>");
            html = format!("{html}<br><div class=\"overlook_forward\"><p>{header_html}</p>{source_html}</div>");
        }
        _ => {
            let header = quote_header(source);
            text = format!("{text}\n\n{header}\n{}", quote_text(&source_text));
            html = format!(
                "{html}<br><div class=\"overlook_quote\"><p>{}</p><blockquote style=\"margin:0 0 0 0.8ex;border-left:1px solid #ccc;padding-left:1ex\">{source_html}</blockquote></div>",
                escape_html(&header)
            );
        }
    }
    OutgoingBodies { text, html }
}

/// Local quote previews enforce resource consent independently of the outgoing
/// serialization. Keep the full visible history, rather than folding it for display.
pub fn quote_preview(draft: &Draft, source: &Message, policy: RemoteContent) -> String {
    outgoing_preview(&Draft { body_text: String::new(), signature_html: None, ..draft.clone() }, Some(source), policy)
}

/// Preview a draft (including Outbox items) without bypassing local image consent.
pub fn outgoing_preview(draft: &Draft, source: Option<&Message>, policy: RemoteContent) -> String {
    clean_only(&outgoing_bodies(draft, source).html, policy)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::*;
    use crate::model::{Address, Body};

    #[test]
    fn unicode_and_inline_soft_breaks_survive_rendering() {
        let html = render_markdown("**Café — 東京 👋**\nCrème brûlée\n你好");
        assert!(html.contains("<strong>Café — 東京 👋</strong><br>\nCrème brûlée<br>\n你好"), "{html}");
        assert_eq!(soft_breaks_to_br("<pre>é\n東京 👋</pre>\n<p>Fin</p>"), "<pre>é\n東京 👋</pre>\n<p>Fin</p>");
        assert_eq!(soft_breaks_to_br("<p>one<br>\ntwo</p>"), "<p>one<br>\ntwo</p>");
    }

    #[test]
    fn authored_image_previews_also_require_local_resource_consent() {
        let mut draft = Draft::new(AccountId::from("a"), DraftKind::New, 0);
        draft.body_text = "![Image](https://images.example/preview.png)".into();
        assert!(!outgoing_preview(&draft, None, RemoteContent::Blocked).contains("https://images.example/preview.png"));
        assert!(outgoing_preview(&draft, None, RemoteContent::Allowed).contains("https://images.example/preview.png"));
        assert!(outgoing_bodies(&draft, None).html.contains("https://images.example/preview.png"));
    }

    #[test]
    fn renders_mail_markdown() {
        let html = render_markdown("Hi **Maya**,\nsee *this* and ~~that~~ at https://ex.com/a\n\n- one\n- two\n\n> quoted");
        assert!(html.contains("<strong>Maya</strong>"), "{html}");
        assert!(html.contains("Hi <strong>Maya</strong>,<br>"), "{html}");
        assert!(html.contains("<em>this</em>") && html.contains("<del>that</del>"), "{html}");
        assert!(html.contains("<a href=\"https://ex.com/a\">https://ex.com/a</a>"), "{html}");
        assert!(html.contains("<ul>") && html.contains("<li>one</li>"), "{html}");
        assert!(html.contains("<blockquote>"), "{html}");
    }

    #[test]
    fn typed_html_is_escaped_not_rendered() {
        let html = render_markdown("<script>alert(1)</script> & <b>x</b>");
        assert!(!html.contains("<script>") && !html.contains("<b>x</b>"), "{html}");
        assert!(html.contains("&lt;b&gt;x&lt;/b&gt;"), "{html}");
    }

    #[test]
    fn plain_text_drops_marks_keeps_structure() {
        let plain = markdown_to_plain("# Title\n\nHi **Maya**, see [the brief](https://ex.com/b) and `cargo test`.\n- *one*\n> quoted_word_here");
        assert_eq!(plain, "Title\n\nHi Maya, see the brief (https://ex.com/b) and cargo test.\n- one\n> quoted_word_here");
    }

    fn message_with_body(html: &str) -> Message {
        let mut m = source();
        m.body.html = Some(html.into());
        m
    }

    fn source() -> Message {
        Message {
            id: MessageId::from("a:1"),
            account_id: AccountId::from("a"),
            conversation_id: ConversationId::from("a:t"),
            provider_id: None,
            from: Address::named("Maya Chen", "maya@northstar.example"),
            to: vec![Address::named("Jason", "jason@example.com")],
            cc: vec![],
            bcc: vec![],
            reply_to: None,
            subject: "Launch".into(),
            snippet: "Can we meet?".into(),
            body: Body { text: Some("Can we meet?\nThursday works.".into()), html: Some("<div>Can we meet?<br>Thursday works.</div>".into()) },
            date: 0,
            is_read: true,
            is_starred: false,
            is_outgoing: false,
            label_ids: Default::default(),
            attachments: vec![],
            rfc_message_id: None,
            in_reply_to: None,
            references: Vec::new(),
            category: None,
            is_important: false,
            focus_override: None,
            invitation: None,
        }
    }

    #[test]
    fn reply_quotes_below_the_body() {
        let mut draft = Draft::new(AccountId::from("a"), DraftKind::Reply, 0);
        draft.body_text = "Thursday it is.".into();
        draft.include_quote = true;
        let out = outgoing_bodies(&draft, Some(&source()));
        assert!(out.text.starts_with("Thursday it is.\n\nOn "), "{}", out.text);
        assert!(out.text.ends_with("wrote:\n> Can we meet?\n> Thursday works."), "{}", out.text);
        assert!(out.html.starts_with("<p>Thursday it is.</p>"), "{}", out.html);
        assert!(out.html.contains("<blockquote") && out.html.contains("Thursday works."), "{}", out.html);
        draft.include_quote = false;
        assert_eq!(outgoing_bodies(&draft, Some(&source())).text, "Thursday it is.");
    }

    #[test]
    fn quotes_share_visible_content_but_preview_requires_remote_consent() {
        let mut source = source();
        source.body.html = Some("<div style=\"display:none\">Private preheader</div><p style=\"margin:0;font-size:10pt\">Café 東京</p><img src=\"https://images.example/logo.png\" alt=\"Logo\"><blockquote>Earlier history</blockquote>".into());
        source.body.text = Some("Private preheader\nStale text derivative".into());
        let mut draft = Draft::new(AccountId::from("a"), DraftKind::Reply, 0);
        draft.include_quote = true;
        let outgoing = outgoing_bodies(&draft, Some(&source));
        assert!(!outgoing.html.contains("Private preheader"));
        assert!(!outgoing.text.contains("Private preheader") && !outgoing.text.contains("Stale text"));
        assert!(outgoing.text.contains("Café 東京") && outgoing.text.contains("Earlier history"));
        assert!(outgoing.html.contains("font-size:10pt") && outgoing.html.contains("margin:0"));
        assert!(outgoing.html.contains("https://images.example/logo.png"));
        let preview = quote_preview(&draft, &source, RemoteContent::Blocked);
        assert!(!preview.contains("https://images.example/logo.png"));
        assert!(preview.contains("Earlier history"));
        draft.body_text = "My **reply**".into();
        let outbox = outgoing_preview(&draft, Some(&source), RemoteContent::Blocked);
        assert!(outbox.contains("My <strong>reply</strong>"));
        assert!(!outbox.contains("https://images.example/logo.png"));
        assert!(quote_preview(&draft, &source, RemoteContent::Allowed).contains("https://images.example/logo.png"));
    }

    #[test]
    fn the_signature_sits_between_what_was_written_and_the_quote() {
        let mut draft = Draft::new(AccountId::from("a"), DraftKind::Forward, 0);
        draft.body_text = "FYI".into();
        draft.include_quote = true;
        draft.signature_html = Some("<p><b>Jason</b><br>Last Refuge</p><img src=\"cid:sig-1@overlook\">".into());
        let out = outgoing_bodies(&draft, Some(&message_with_body("<p>Original</p>")));
        let (body, signature, quote) = (out.html.find("FYI").unwrap(), out.html.find("Jason").unwrap(), out.html.find("Original").unwrap());
        assert!(body < signature && signature < quote, "{}", out.html);
        assert!(out.html.contains("<div class=\"overlook_signature\"><p><b>Jason</b>"), "{}", out.html);
        assert!(out.html.contains("cid:sig-1@overlook"), "images stay cid references");
        // Plain text: the "-- " line, then the signature as text, then the quote.
        assert!(out.text.starts_with("FYI\n\n-- \nJason"), "{:?}", out.text);
        assert!(out.text.find("Last Refuge").unwrap() < out.text.find("Original").unwrap(), "{:?}", out.text);
    }

    #[test]
    fn a_signature_is_cleaned_like_everything_else_that_is_sent() {
        let mut draft = Draft::new(AccountId::from("a"), DraftKind::New, 0);
        draft.signature_html = Some("<p onclick=\"x()\">Jason</p><script>alert(1)</script><img src=\"data:image/png;base64,AAAA\">".into());
        let out = outgoing_bodies(&draft, None);
        assert!(out.html.contains("Jason") && !out.html.contains("onclick") && !out.html.contains("script") && !out.html.contains("data:"), "{}", out.html);
    }

    #[test]
    fn no_signature_or_a_blank_one_changes_nothing() {
        let mut draft = Draft::new(AccountId::from("a"), DraftKind::New, 0);
        draft.body_text = "Hi".into();
        let plain = outgoing_bodies(&draft, None);
        draft.signature_html = Some("  \n".into());
        assert_eq!(outgoing_bodies(&draft, None), plain);
        assert!(!plain.html.contains("overlook_signature") && !plain.text.contains("-- "));
    }

    #[test]
    fn the_quote_preview_leaves_the_signature_out() {
        let mut draft = Draft::new(AccountId::from("a"), DraftKind::Reply, 0);
        draft.include_quote = true;
        draft.signature_html = Some("<p>Jason</p>".into());
        let preview = quote_preview(&draft, &message_with_body("<p>Original</p>"), RemoteContent::Allowed);
        assert!(preview.contains("Original") && !preview.contains("Jason"), "{preview}");
    }

    #[test]
    fn forward_carries_a_header_block() {
        let mut draft = Draft::new(AccountId::from("a"), DraftKind::Forward, 0);
        draft.body_text = "FYI".into();
        draft.include_quote = true;
        let out = outgoing_bodies(&draft, Some(&source()));
        assert!(out.text.contains("---------- Forwarded message ----------\nFrom: Maya Chen <maya@northstar.example>"), "{}", out.text);
        assert!(out.html.contains("<b>From:</b> Maya Chen &lt;maya@northstar.example&gt;"), "{}", out.html);
        assert!(out.html.contains("Thursday works."));
    }
}
