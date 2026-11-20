//! Reversible, display-only folding of history already present in the thread.
//! Original messages, outgoing quotes, and the browser's security policy stay intact.

use crate::MessageId;
use html5ever::serialize::{serialize, SerializeOpts};
use html5ever::{namespace_url, ns, Attribute, LocalName, QualName};
use markup5ever_rcdom::{Handle, Node, NodeData, SerializableHandle};
use std::collections::{HashMap, VecDeque};

pub const MAX_MESSAGES: usize = 64;
pub const MAX_THREAD_BYTES: usize = 8 * 1024 * 1024;
const MAX_SIGNATURE_BYTES: usize = 128 * 1024;
const MAX_SIGNATURES_BYTES: usize = 2 * 1024 * 1024;

pub enum HistoryBody {
    Html(String),
    Text(String),
}

pub struct HistoryMessage {
    pub id: MessageId,
    pub subject: String,
    pub body: HistoryBody,
}

/// Input is chronological and bounded by MAX_MESSAGES/MAX_THREAD_BYTES at the
/// caller. Results include only changed HTML. A generation change cancels work
/// between documents and candidate boundaries; no partial result is published.
pub fn prepare(
    messages: &[HistoryMessage],
    current: impl Fn() -> bool,
) -> HashMap<MessageId, String> {
    if messages.len() > MAX_MESSAGES
        || messages
            .iter()
            .map(|m| match &m.body {
                HistoryBody::Html(s) | HistoryBody::Text(s) => s.len(),
            })
            .sum::<usize>()
            > MAX_THREAD_BYTES
    {
        return HashMap::new();
    }
    let mut previous = VecDeque::new();
    let mut result = HashMap::new();
    let mut bytes = 0;
    for message in messages {
        if !current() {
            return HashMap::new();
        }
        let signature = match &message.body {
            HistoryBody::Html(source) => {
                let Some(dom) = crate::browser::parse_bounded_document(source) else {
                    continue;
                };
                let Some(body) = find_body(&dom.document) else {
                    continue;
                };
                let signature = signature(std::slice::from_ref(&body));
                let subject = message.subject.trim_start().to_ascii_lowercase();
                if !subject.starts_with("fw:")
                    && !subject.starts_with("fwd:")
                    && fold(&body, &previous, &mut 64, &current)
                {
                    let mut html = Vec::new();
                    if serialize(
                        &mut html,
                        &SerializableHandle::from(dom.document.clone()),
                        SerializeOpts::default(),
                    )
                    .is_ok()
                    {
                        if let Ok(html) = String::from_utf8(html) {
                            result.insert(message.id.clone(), html);
                        }
                    }
                }
                signature
            }
            HistoryBody::Text(text) if text.len() <= MAX_SIGNATURE_BYTES => Some(Signature {
                text: normalized(text),
                resources: Vec::new(),
            }),
            HistoryBody::Text(_) => None,
        };
        if let Some(signature) =
            signature.filter(|s| s.text.len() >= 40 && s.text.len() <= MAX_SIGNATURE_BYTES)
        {
            bytes += signature.text.len();
            previous.push_back(signature);
            while bytes > MAX_SIGNATURES_BYTES || previous.len() > MAX_MESSAGES {
                bytes -= previous
                    .pop_front()
                    .expect("the queue exceeded its bound")
                    .text
                    .len();
            }
        }
    }
    if current() {
        result
    } else {
        HashMap::new()
    }
}

#[derive(PartialEq, Eq)]
struct Signature {
    text: String,
    resources: Vec<(String, String)>,
}

fn normalized(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

const MAX_HEADER_SCAN_BYTES: usize = 1024;
const MAX_HEADER_SCAN_NODES: usize = 128;
const MAX_FORWARD_SCAN_BYTES: usize = 4096;
const MAX_FORWARD_SCAN_NODES: usize = 256;

fn with_attr<R>(node: &Handle, key: &str, f: impl FnOnce(&str) -> R) -> Option<R> {
    let NodeData::Element { attrs, .. } = &node.data else {
        return None;
    };
    attrs
        .borrow()
        .iter()
        .find(|a| a.name.local.as_ref() == key)
        .map(|a| f(&a.value))
}

fn has_attr(node: &Handle, key: &str) -> bool {
    with_attr(node, key, |_| ()).is_some()
}

fn attr(node: &Handle, key: &str) -> Option<String> {
    with_attr(node, key, str::to_owned)
}

fn contains_ignore_ascii_case(haystack: &str, needle: &str) -> bool {
    haystack
        .as_bytes()
        .windows(needle.len())
        .any(|w| w.eq_ignore_ascii_case(needle.as_bytes()))
}

fn is_hidden_style(style: &str) -> bool {
    for decl in style.split(';').take(128) {
        let Some((k, v)) = decl.split_once(':') else {
            continue;
        };
        let k = k.trim();
        let v = v.trim();
        if (k.eq_ignore_ascii_case("display")
            && v.get(..4).is_some_and(|p| p.eq_ignore_ascii_case("none")))
            || (k.eq_ignore_ascii_case("visibility")
                && v.get(..6).is_some_and(|p| p.eq_ignore_ascii_case("hidden")))
        {
            return true;
        }
    }
    false
}

fn class(node: &Handle, value: &str) -> bool {
    with_attr(node, "class", |classes| {
        classes
            .split_whitespace()
            .any(|c| c.eq_ignore_ascii_case(value))
    })
    .unwrap_or(false)
}

fn tag(node: &Handle) -> &str {
    if let NodeData::Element { name, .. } = &node.data {
        name.local.as_ref()
    } else {
        ""
    }
}

fn find_body(node: &Handle) -> Option<Handle> {
    if tag(node) == "body" {
        return Some(node.clone());
    }
    node.children.borrow().iter().find_map(find_body)
}

fn attribution(node: &Handle) -> bool {
    if class(node, "gmail_attr") {
        return true;
    }
    if !matches!(tag(node), "div" | "p") {
        return false;
    }
    fn text(node: &Handle, out: &mut String) -> Option<()> {
        if out.len() > 512 {
            return None;
        }
        match &node.data {
            NodeData::Text { contents } => out.push_str(&contents.borrow()),
            _ => {
                for child in node.children.borrow().iter() {
                    text(child, out)?;
                }
            }
        }
        Some(())
    }
    let mut line = String::new();
    if text(node, &mut line).is_none() {
        return false;
    }
    let line = normalized(&line);
    // Outlook drops Gmail's classes when copying an earlier message. Recognize
    // its standalone attribution too; never omit a paragraph containing body text.
    line.starts_with("On ") && line.ends_with("wrote:") && line.contains('@')
}

fn forward_marker(node: &Handle) -> bool {
    fn visit(node: &Handle, nodes_left: &mut usize, bytes_left: &mut usize) -> bool {
        if *nodes_left == 0 || *bytes_left == 0 {
            return false;
        }
        *nodes_left -= 1;
        if let NodeData::Text { contents } = &node.data {
            let text = contents.borrow();
            let check = if text.len() > *bytes_left {
                let mut end = *bytes_left;
                while end > 0 && !text.is_char_boundary(end) {
                    end -= 1;
                }
                *bytes_left = 0;
                &text[..end]
            } else {
                *bytes_left -= text.len();
                &text[..]
            };
            if check.contains("Forwarded message") || check.contains("Begin forwarded message") {
                return true;
            }
        }
        node.children
            .borrow()
            .iter()
            .any(|child| visit(child, nodes_left, bytes_left))
    }
    let mut nodes_left = MAX_FORWARD_SCAN_NODES;
    let mut bytes_left = MAX_FORWARD_SCAN_BYTES;
    visit(node, &mut nodes_left, &mut bytes_left)
}

fn signature(nodes: &[Handle]) -> Option<Signature> {
    fn visit(
        node: &Handle,
        text: &mut String,
        resources: &mut Vec<(String, String)>,
    ) -> Option<()> {
        if text.len() > MAX_SIGNATURE_BYTES || resources.len() > 1024 {
            return None;
        }
        if let NodeData::Text { contents } = &node.data {
            text.push_str(&contents.borrow());
            return Some(());
        }
        let local = tag(node);
        if matches!(local, "head" | "style" | "script" | "template") || attribution(node) {
            return Some(());
        }
        if has_attr(node, "hidden") || with_attr(node, "style", is_hidden_style).unwrap_or(false) {
            return Some(());
        }
        if matches!(local, "video" | "audio" | "object" | "svg" | "iframe") {
            return None;
        }
        if local == "img" {
            // Do not conceal image-only annotations or substituted link targets.
            resources.push((
                local.into(),
                format!(
                    "{:?}",
                    (attr(node, "src"), attr(node, "srcset"), attr(node, "alt"))
                ),
            ));
        }
        if local == "a" {
            resources.push(("a".into(), attr(node, "href").unwrap_or_default()));
        }
        let block = matches!(
            local,
            "div"
                | "p"
                | "br"
                | "table"
                | "tr"
                | "td"
                | "li"
                | "blockquote"
                | "hr"
                | "h1"
                | "h2"
                | "h3"
                | "pre"
        );
        if block {
            text.push(' ');
        }
        for child in node.children.borrow().iter() {
            visit(child, text, resources)?;
        }
        if block {
            text.push(' ');
        }
        Some(())
    }
    let mut text = String::new();
    let mut resources = Vec::new();
    for node in nodes {
        visit(node, &mut text, &mut resources)?;
    }
    Some(Signature {
        text: normalized(&text),
        resources,
    })
}

/// Bounded scan for an Outlook-style `border-top` header block (`From: ... Sent:/To: ...`),
/// avoiding full 128 KB `signature()` traversals on ordinary bordered divs.
fn is_outlook_header_block(node: &Handle) -> bool {
    fn visit(node: &Handle, out: &mut String, nodes_left: &mut usize) -> bool {
        if *nodes_left == 0 || out.len() >= MAX_HEADER_SCAN_BYTES {
            return true;
        }
        *nodes_left -= 1;
        if let NodeData::Text { contents } = &node.data {
            let text = contents.borrow();
            let remaining = MAX_HEADER_SCAN_BYTES.saturating_sub(out.len());
            if text.len() <= remaining {
                out.push_str(&text);
            } else {
                let mut end = remaining;
                while end > 0 && !text.is_char_boundary(end) {
                    end -= 1;
                }
                out.push_str(&text[..end]);
            }
            let trimmed = out.trim_start();
            if trimmed.len() >= 5 && !trimmed.starts_with("From:") {
                return false;
            }
            return true;
        }
        let local = tag(node);
        if matches!(local, "head" | "style" | "script" | "template")
            || has_attr(node, "hidden")
            || with_attr(node, "style", is_hidden_style).unwrap_or(false)
        {
            return true;
        }
        let block = matches!(
            local,
            "div" | "p" | "br" | "table" | "tr" | "td" | "li" | "blockquote" | "hr"
        );
        if block {
            out.push(' ');
        }
        for child in node.children.borrow().iter() {
            if !visit(child, out, nodes_left) {
                return false;
            }
        }
        if block {
            out.push(' ');
        }
        true
    }
    let mut raw = String::new();
    let mut nodes_left = MAX_HEADER_SCAN_NODES;
    if !visit(node, &mut raw, &mut nodes_left) {
        return false;
    }
    let text = normalized(&raw);
    text.starts_with("From:") && (text.contains("Sent:") || text.contains("To:"))
}

#[derive(Clone, Copy)]
enum Boundary {
    Quote,
    Header,
}

fn boundary(node: &Handle, remaining: &mut usize) -> Option<(Boundary, bool)> {
    let local = tag(node);
    if with_attr(node, "id", |id| id.eq_ignore_ascii_case("divrplyfwdmsg")).unwrap_or(false)
        || class(node, "moz-cite-prefix")
    {
        return Some((Boundary::Header, false));
    }
    if (local == "blockquote"
        && with_attr(node, "type", |t| t.eq_ignore_ascii_case("cite")).unwrap_or(false))
        || class(node, "gmail_quote")
        || class(node, "gmail_quote_container")
        || class(node, "yahoo_quoted")
    {
        return Some((Boundary::Quote, false));
    }
    if matches!(local, "div" | "p")
        && *remaining > 0
        && with_attr(node, "style", |s| contains_ignore_ascii_case(s, "border-top"))
            .unwrap_or(false)
    {
        *remaining -= 1;
        if is_outlook_header_block(node) {
            return Some((Boundary::Header, true));
        }
    }
    None
}

fn fold(
    node: &Handle,
    previous: &VecDeque<Signature>,
    remaining: &mut usize,
    current: &impl Fn() -> bool,
) -> bool {
    if !current() || previous.is_empty() || *remaining == 0 {
        return false;
    }
    let children = node.children.borrow().clone();
    for (index, child) in children.iter().enumerate() {
        if !current() || *remaining == 0 {
            return false;
        }
        if let Some((boundary, counted)) = boundary(child, remaining) {
            if forward_marker(child) {
                continue;
            }
            if !counted {
                *remaining -= 1;
            }
            let content = match boundary {
                Boundary::Header => &children[index + 1..],
                Boundary::Quote => &children[index..],
            };
            if let Some(candidate) = signature(content) {
                // Forwarded messages remain content, even if the original is in the thread.
                if !candidate.text.contains("Forwarded message")
                    && !candidate.text.contains("Begin forwarded message")
                    && candidate.text.len() >= 40
                    && previous.iter().any(|old| old == &candidate)
                    && matches!(tag(node), "body" | "div" | "section" | "td" | "blockquote")
                {
                    let details = element("details", vec![("class", "overlook-message-history")]);
                    let summary = element("summary", vec![("title", "Quoted messages already available above"), ("style", "cursor:pointer;font:12px/22px system-ui,sans-serif;padding:4px 8px;margin:10px 0;border:1px solid #d1d5db;border-radius:4px;width:fit-content")]);
                    set_children(
                        &summary,
                        vec![Node::new(NodeData::Text {
                            contents: std::cell::RefCell::new("Message history".into()),
                        })],
                    );
                    let mut history = vec![summary];
                    history.extend_from_slice(&children[index..]);
                    set_children(&details, history);
                    let mut visible = children[..index].to_vec();
                    visible.push(details);
                    set_children(node, visible);
                    return true;
                }
            }
        }
        if fold(child, previous, remaining, current) {
            return true;
        }
    }
    false
}

fn element(local: &str, attributes: Vec<(&str, &str)>) -> Handle {
    Node::new(NodeData::Element {
        name: QualName::new(None, ns!(html), LocalName::from(local)),
        attrs: std::cell::RefCell::new(
            attributes
                .into_iter()
                .map(|(name, value)| Attribute {
                    name: QualName::new(None, ns!(), LocalName::from(name)),
                    value: value.into(),
                })
                .collect(),
        ),
        template_contents: std::cell::RefCell::new(None),
        mathml_annotation_xml_integration_point: false,
    })
}

fn set_children(parent: &Handle, children: Vec<Handle>) {
    for child in &children {
        child.parent.set(Some(std::rc::Rc::downgrade(parent)));
    }
    *parent.children.borrow_mut() = children;
}

#[cfg(test)]
mod tests {
    use super::*;
    const OLD: &str = "<p>Hello team, the webhook is ready. Please run the agreed tests and send your feedback.</p>";
    const HEADER: &str = "<div style='border-top:1px solid #ccc'><b>From:</b> A<br><b>Sent:</b> Monday<br><b>To:</b> Team</div>";
    fn prepared(old: &str, reply: &str, subject: &str) -> Option<String> {
        let messages = [
            HistoryMessage {
                id: "old".into(),
                subject: "Test".into(),
                body: HistoryBody::Html(old.into()),
            },
            HistoryMessage {
                id: "reply".into(),
                subject: subject.into(),
                body: HistoryBody::Html(reply.into()),
            },
        ];
        prepare(&messages, || true).remove(&MessageId::from("reply"))
    }
    #[test]
    fn outlook_history_folds_without_losing_styles_or_current_content() {
        let source = format!("<html><head><style>.note{{color:blue}}</style></head><body><div class=WordSection1><p class=note>Thanks, we will test it.</p>{HEADER}{OLD}</div></body></html>");
        let folded = prepared(OLD, &source, "Re: Test").unwrap();
        assert!(folded.contains(".note{color:blue}"));
        assert!(
            folded.find("Thanks, we will test it.").unwrap() < folded.find("<details").unwrap()
        );
        assert!(folded.contains("Message history") && folded.contains("the webhook is ready"));
        assert!(!folded.contains("<details open"));
        let sanitized = crate::browser::prepare_document(&folded).html;
        assert!(sanitized.contains("<details") && sanitized.contains("<summary"));
    }
    #[test]
    fn gmail_and_cite_quotes_fold_only_with_matching_earlier_content() {
        let gmail = format!("<p>Thanks.</p><div class=gmail_quote><div class=gmail_attr>On Monday, A wrote:</div><blockquote>{OLD}</blockquote></div>");
        assert!(prepared(OLD, &gmail, "Re: Test").is_some());
        let cite = format!("<p>Thanks.</p><blockquote type=cite>{OLD}</blockquote>");
        assert!(prepared(OLD, &cite, "Re: Test").is_some());
        assert!(prepared(
            "<p>Different message, with enough unique text to establish a clear comparison.</p>",
            &cite,
            "Re: Test"
        )
        .is_none());
    }
    #[test]
    fn annotations_and_unknown_history_remain_visible() {
        let inline = OLD.replace("Please run", "MY NOTE: use staging first. Please run");
        assert!(prepared(
            OLD,
            &format!("<p>See my notes.</p>{HEADER}{inline}"),
            "Re: Test"
        )
        .is_none());
        assert!(prepared(
            OLD,
            &format!("<p>Thanks.</p>{HEADER}{OLD}<p>One more thing: do not deploy yet.</p>"),
            "Re: Test"
        )
        .is_none());
        assert!(prepared(
            OLD,
            &format!("{HEADER}{OLD}<img src=cid:new-annotation>"),
            "Re: Test"
        )
        .is_none());
        let edited_link = OLD.replace(
            "agreed tests",
            "<a href='https://other.example'>agreed tests</a>",
        );
        assert!(prepared(OLD, &format!("{HEADER}{edited_link}"), "Re: Test").is_none());
    }
    #[test]
    fn forwards_and_short_ambiguous_quotes_are_not_hidden() {
        assert!(prepared(OLD, &format!("<p>FYI</p>{HEADER}{OLD}"), "FW: Test").is_none());
        let forwarded = format!("<div class=gmail_quote><div class=gmail_attr>---------- Forwarded message ---------</div>{OLD}</div>");
        // The attribution is excluded from comparison, so subject is an additional safeguard.
        assert!(prepared(OLD, &forwarded, "Fwd: Test").is_none());
        assert!(prepared(
            "<p>Thanks!</p>",
            &format!("{HEADER}<p>Thanks!</p>"),
            "Re: Test"
        )
        .is_none());
    }
    #[test]
    fn cancellation_discards_all_prepared_results() {
        let messages = [HistoryMessage {
            id: "old".into(),
            subject: "Test".into(),
            body: HistoryBody::Html(OLD.into()),
        }];
        assert!(prepare(&messages, || false).is_empty());
    }

    #[test]
    fn copied_gmail_attributions_match_after_outlook_removes_the_class() {
        let attribution = "On Monday, Alice &lt;alice@example.test&gt; wrote:";
        let old = format!("<p>The update is ready for your team to review.</p><div class=gmail_quote><div class=gmail_attr>{attribution}</div><blockquote>{OLD}</blockquote></div>");
        let copied = old.replace(" class=gmail_attr", "");
        assert!(prepared(
            &old,
            &format!("<p>Thanks, we will test.</p>{HEADER}{copied}"),
            "Re: Test"
        )
        .is_some());
        let annotated = copied.replace(
            "The update is ready",
            "MY NOTE: wait for approval. The update is ready",
        );
        assert!(prepared(
            &old,
            &format!("<p>Thanks.</p>{HEADER}{annotated}"),
            "Re: Test"
        )
        .is_none());
    }

    #[test]
    fn a_forward_inside_a_reply_keeps_its_content() {
        let source = format!("<p>FYI</p><div class=gmail_quote><div class=gmail_attr>---------- Forwarded message ---------</div>{OLD}</div>");
        assert!(prepared(OLD, &source, "Re: Test").is_none());
        let old_image = format!("{OLD}<img src=cid:chart srcset='https://example.test/old 2x'>");
        let changed_image = old_image.replace("/old", "/new");
        assert!(prepared(&old_image, &format!("{HEADER}{changed_image}"), "Re: Test").is_none());
    }

    #[test]
    fn bordered_div_scans_are_bounded_and_still_fold_real_outlook_headers() {
        let dividers = "<div style='border-top:1px solid #eee'>Section text</div>".repeat(10);
        let reply = format!("<div>{dividers}{HEADER}{OLD}</div>");
        assert!(prepared(OLD, &reply, "Re: Test").is_some());

        // Excessive bordered divs stay within the candidate budget without hanging.
        let many_dividers = "<div style='border-top:1px solid #eee'>Card content</div>".repeat(200);
        let big_reply = format!("<div>{many_dividers}{HEADER}{OLD}</div>");
        assert!(prepared(OLD, &big_reply, "Re: Test").is_none());
    }
}
