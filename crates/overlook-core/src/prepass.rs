//! Structural clean-up of received HTML, run before sanitizing.
//!
//! Mail HTML carries things a browser hides or a client is expected to tidy: preheader text
//! hidden with `display:none`, tracking pixels, `<title>` text, `<center>` wrappers, and the
//! quoted history of a reply. The native HTML view supports only a bounded presentation
//! subset, so visibility is resolved on the DOM here. Explicit blank blocks and hard breaks
//! are sender content, not disposable spacers. The result is still untrusted HTML;
//! `sanitize_html` cleans it afterwards.

use html5ever::serialize::{serialize, SerializeOpts, TraversalScope};
use html5ever::{namespace_url, ns, Attribute, LocalName, QualName};
use markup5ever_rcdom::{Handle, Node, NodeData, RcDom, SerializableHandle};
use std::cell::RefCell;
use std::rc::Rc;

/// The message split into what the reader shows and the quoted history, if any.
pub struct Prepared {
    pub html: String,
    pub quoted: Option<String>,
}

pub(crate) struct PreparedDom {
    pub doc: RcDom,
    pub quoted: Option<RcDom>,
}

/// Elements removed with their content.
const REMOVE: &[&str] = &[
    "head", "title", "style", "script", "meta", "link", "noscript", "template", "iframe", "object",
    "embed", "svg", "math", "base",
];

pub(crate) fn prepare_dom(html: &str) -> Option<PreparedDom> {
    let dom = crate::browser::parse_bounded_document(html)?;
    let doc = dom.document.clone();
    let cleaned = clean_children(&doc, &Flags::default(), 0);
    set_children(&doc, cleaned);
    let quoted = split_quote(&doc, 0).map(|nodes| {
        let qdom = RcDom::default();
        set_children(&qdom.document, nodes);
        qdom
    });
    Some(PreparedDom { doc: dom, quoted })
}

pub fn prepare(html: &str) -> Prepared {
    let Some(prepared) = prepare_dom(html) else {
        return Prepared {
            html: String::new(),
            quoted: None,
        };
    };
    let quoted = prepared
        .quoted
        .as_ref()
        .map(|q| serialize_children(&q.document));
    Prepared {
        html: serialize_children(&prepared.doc.document),
        quoted,
    }
}

#[derive(Clone, Copy, Default)]
struct Flags {
    in_pre: bool,
    no_links: bool,
}

fn clean_children(node: &Handle, flags: &Flags, depth: usize) -> Vec<Handle> {
    if depth > crate::browser::MAX_DOM_DEPTH {
        return Vec::new();
    }
    let mut out = Vec::new();
    for child in node.children.borrow().iter() {
        match &child.data {
            NodeData::Text { contents } => {
                let text = contents.borrow().to_string();
                if flags.in_pre {
                    out.extend(preformatted_nodes(&text));
                } else if flags.no_links {
                    out.push(child.clone());
                } else {
                    out.extend(linkified_nodes(&text, child));
                }
            }
            NodeData::Element { name, attrs, .. } => {
                let local = name.local.to_string();
                let mut attrs_vec = attrs.borrow().clone();
                if REMOVE.contains(&local.as_str()) || is_hidden(&local, &attrs_vec) {
                    continue;
                }
                // The view has no baseline shift: digits and signs become the Unicode
                // super/subscript glyphs (m², H₂O, footnote³); other text stays inline.
                if local == "sup" || local == "sub" {
                    // Resolve descendant visibility before converting glyphs; reading
                    // raw subtree text can resurrect a hidden footnote or tracker.
                    let visible = clean_children(child, flags, depth + 1);
                    // Detach before `child` is dropped so rcdom's Drop does not clear reused nodes.
                    child.children.borrow_mut().clear();
                    let mut text = String::new();
                    for v in &visible {
                        collect_subtree_text(v, &mut text, 64, depth + 1);
                    }
                    match script_text(&text, local == "sup") {
                        Some(converted) => out.push(text_node(&converted)),
                        None => out.extend(visible),
                    }
                    continue;
                }
                let mut inner = *flags;
                inner.in_pre |= local == "pre";
                inner.no_links |= matches!(local.as_str(), "a" | "pre" | "code");
                let children = clean_children(child, &inner, depth + 1);
                let (new_name, bold) = match local.as_str() {
                    "center" => ("div", false),
                    "dt" => ("div", true),
                    "dd" => ("div", false),
                    _ => (local.as_str(), false),
                };
                if local == "center" && !attrs_vec.iter().any(|a| &*a.name.local == "align") {
                    attrs_vec.push(Attribute {
                        name: QualName::new(None, ns!(), LocalName::from("align")),
                        value: "center".into(),
                    });
                }
                // Each embedded message (Outlook's From:/Sent: block, Gmail's "On … wrote:"
                // or "Forwarded message" line) is set off with a rule, so a forwarded chain
                // reads as separate messages. Not when the author already put one there.
                if starts_embedded_message(child, &local, &attrs_vec)
                    && !out.last().is_some_and(|n| is_named(n, &["hr"]))
                {
                    out.push(element("hr", vec![]));
                }
                if new_name != local || bold {
                    // rcdom's Drop empties the children of every node beneath a dropped
                    // node, including ones that now live under `fresh`; detach them first.
                    child.children.borrow_mut().clear();
                    let fresh = element(new_name, attrs_vec);
                    let children = if bold {
                        let b = element("b", vec![]);
                        set_children(&b, children);
                        vec![b]
                    } else {
                        children
                    };
                    set_children(&fresh, children);
                    out.push(fresh);
                } else {
                    set_children(child, children);
                    out.push(child.clone());
                }
            }
            NodeData::Document => {}
            // Comments (including Outlook's conditional comments), doctypes, PIs.
            _ => {}
        }
    }
    out
}

fn is_named(node: &Handle, names: &[&str]) -> bool {
    matches!(&node.data, NodeData::Element { name, .. } if names.contains(&&*name.local))
}

pub(super) fn element(name: &str, attrs: Vec<Attribute>) -> Handle {
    Node::new(NodeData::Element {
        name: QualName::new(None, ns!(html), LocalName::from(name)),
        attrs: RefCell::new(attrs),
        template_contents: RefCell::new(None),
        mathml_annotation_xml_integration_point: false,
    })
}

pub(super) fn text_node(text: &str) -> Handle {
    Node::new(NodeData::Text {
        contents: RefCell::new(text.into()),
    })
}

pub(super) fn set_children(parent: &Handle, children: Vec<Handle>) {
    for child in &children {
        child.parent.set(Some(Rc::downgrade(parent)));
    }
    *parent.children.borrow_mut() = children;
}

fn attr<'a>(attrs: &'a [Attribute], name: &str) -> Option<&'a str> {
    attrs
        .iter()
        .find(|a| &*a.name.local == name)
        .map(|a| &*a.value)
}

/// Whether an element would be invisible in a browser, or is a tracking pixel.
pub(crate) fn is_hidden(local: &str, attrs: &[Attribute]) -> bool {
    if attr(attrs, "hidden").is_some() {
        return true;
    }
    if let Some(class) = attr(attrs, "class") {
        let class = class.to_ascii_lowercase();
        if class.contains("preheader") || class.contains("preview-text") {
            return true;
        }
    }
    if local == "img" {
        let dim = |n: &str| {
            attr(attrs, n).and_then(|v| v.trim().trim_end_matches("px").parse::<f32>().ok())
        };
        if let (Some(w), Some(h)) = (dim("width"), dim("height")) {
            if w <= 1.0 && h <= 1.0 {
                return true;
            }
        }
    }
    let Some(style) = attr(attrs, "style") else {
        return false;
    };
    let mut resolved: Vec<(String, String, bool)> = Vec::new();
    let zero = |v: &str| {
        let number = v.trim_end_matches(|c: char| c.is_ascii_alphabetic() || c == '%');
        number.parse::<f32>().is_ok_and(|n| n == 0.)
    };
    for decl in style.split(';').take(128) {
        let Some((key, value)) = decl.split_once(':') else {
            continue;
        };
        let mut key = key.trim().to_ascii_lowercase();
        let value = value.trim().to_ascii_lowercase();
        let (value, important) = crate::sanitize::presentation::declaration_value(&value);
        // Invalid declarations must not override an earlier valid hiding declaration.
        let valid = match key.as_str() {
            "display" => matches!(
                value,
                "none"
                    | "block"
                    | "inline"
                    | "inline-block"
                    | "flex"
                    | "inline-flex"
                    | "grid"
                    | "inline-grid"
                    | "table"
                    | "table-row"
                    | "table-cell"
                    | "list-item"
                    | "contents"
                    | "flow-root"
                    | "initial"
                    | "inherit"
                    | "unset"
                    | "revert"
            ),
            "visibility" => matches!(
                value,
                "visible" | "hidden" | "collapse" | "initial" | "inherit" | "unset" | "revert"
            ),
            "overflow" | "overflow-y" => {
                matches!(value, "visible" | "hidden" | "clip" | "scroll" | "auto")
            }
            "opacity" => value
                .parse::<f32>()
                .is_ok_and(|n| n.is_finite() && (0.0..=1.0).contains(&n)),
            "mso-hide" => matches!(value, "all" | "none"),
            "font-size" | "max-height" | "height" | "width" => {
                value == "auto"
                    || value == "none"
                    || value
                        .trim_end_matches(|c: char| c.is_ascii_alphabetic() || c == '%')
                        .parse::<f32>()
                        .is_ok_and(|n| n.is_finite() && n >= 0.)
            }
            _ => false,
        };
        // For this visibility check only the vertical component matters. Treat the
        // supported one-value shorthand as the same property before resolving priority.
        if key == "overflow" {
            key = "overflow-y".to_owned();
        }
        if !valid
            || resolved
                .iter()
                .any(|(k, _, i)| k == &key && *i && !important)
        {
            continue;
        }
        resolved.retain(|(k, _, _)| k != &key);
        resolved.push((key, value.to_owned(), important));
    }
    let clipped = resolved
        .iter()
        .rev()
        .find(|(k, _, _)| k == "overflow" || k == "overflow-y")
        .is_some_and(|(_, v, _)| matches!(v.as_str(), "hidden" | "clip"));
    for (key, value, _) in &resolved {
        let value = value.as_str();
        let hidden = match key.as_str() {
            "display" => value == "none",
            "visibility" => matches!(value, "hidden" | "collapse"),
            "opacity" => zero(value),
            "mso-hide" => value == "all",
            "max-height" | "height" if local != "img" => clipped && zero(value),
            "font-size" => zero(value) || matches!(value, "1px" | "1pt"),
            "width" | "height" if local == "img" => zero(value),
            _ => false,
        };
        if hidden {
            return true;
        }
    }
    false
}

/// `text` in superscript or subscript glyphs when every character has one.
fn script_text(text: &str, superscript: bool) -> Option<String> {
    let text = text.trim();
    if text.is_empty() || text.chars().count() > 6 {
        return None;
    }
    text.chars()
        .map(|c| {
            let sup = ['⁰', '¹', '²', '³', '⁴', '⁵', '⁶', '⁷', '⁸', '⁹'];
            let sub = ['₀', '₁', '₂', '₃', '₄', '₅', '₆', '₇', '₈', '₉'];
            match c {
                '0'..='9' => Some(if superscript {
                    sup[c as usize - '0' as usize]
                } else {
                    sub[c as usize - '0' as usize]
                }),
                '+' => Some(if superscript { '⁺' } else { '₊' }),
                '-' => Some(if superscript { '⁻' } else { '₋' }),
                '(' => Some(if superscript { '⁽' } else { '₍' }),
                ')' => Some(if superscript { '⁾' } else { '₎' }),
                'n' if superscript => Some('ⁿ'),
                'i' if superscript => Some('ⁱ'),
                _ => None,
            }
        })
        .collect()
}

/// Whitespace and invisible padding, used only for quote detection (not block deletion).
fn is_blank_text(s: &str) -> bool {
    s.chars().all(|c| {
        c.is_whitespace()
            || matches!(
                c,
                '\u{a0}'
                    | '\u{200b}'
                    | '\u{200c}'
                    | '\u{200d}'
                    | '\u{034f}'
                    | '\u{feff}'
                    | '\u{2007}'
                    | '\u{202f}'
            )
    })
}

/// Preformatted text as nodes that survive HTML whitespace collapsing: `<br>` for newlines
/// and non-breaking spaces for indentation. The HTML view turns them back into a code block.
fn preformatted_nodes(text: &str) -> Vec<Handle> {
    let mut out = Vec::new();
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            out.push(element("br", vec![]));
        }
        let indent = line.len() - line.trim_start_matches(' ').len();
        let mut s = String::with_capacity(line.len());
        for _ in 0..indent {
            s.push('\u{a0}');
        }
        s.push_str(&line[indent..]);
        if !s.is_empty() {
            out.push(text_node(&s));
        }
    }
    out
}

/// Splits a text node around bare URLs, turning each into a link.
fn linkified_nodes(text: &str, original: &Handle) -> Vec<Handle> {
    let mut out = Vec::new();
    let mut rest = text;
    let mut any = false;
    while let Some((start, len)) = find_url(rest) {
        any = true;
        if start > 0 {
            out.push(text_node(&rest[..start]));
        }
        let url = &rest[start..start + len];
        let href = if url.starts_with("www.") {
            format!("https://{url}")
        } else {
            url.to_owned()
        };
        let a = element(
            "a",
            vec![Attribute {
                name: QualName::new(None, ns!(), LocalName::from("href")),
                value: href.into(),
            }],
        );
        set_children(&a, vec![text_node(url)]);
        out.push(a);
        rest = &rest[start + len..];
    }
    if !any {
        return vec![original.clone()];
    }
    if !rest.is_empty() {
        out.push(text_node(rest));
    }
    out
}

/// Position and length of the first bare URL in `s`.
pub fn find_url(s: &str) -> Option<(usize, usize)> {
    let mut search_from = 0;
    while search_from < s.len() {
        let mut best: Option<(usize, usize)> = None;
        for needle in ["https://", "http://", "www."] {
            let mut offset = search_from;
            while let Some(rel) = s[offset..].find(needle) {
                let i = offset + rel;
                if best.is_some_and(|(b, _)| i >= b) {
                    break;
                }
                // A word boundary before it; "www." must not be part of a longer token.
                let boundary = i == 0
                    || !s[..i]
                        .chars()
                        .next_back()
                        .is_some_and(|c| c.is_alphanumeric() || c == '/' || c == '.' || c == '@');
                if boundary {
                    best = Some((i, needle.len()));
                    break;
                }
                offset = i + needle.len();
            }
        }
        let (start, needle_len) = best?;
        let tail = &s[start..];
        let mut end = tail
            .find(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '"' | '\'' | '\u{a0}'))
            .unwrap_or(tail.len());
        let mut open_parens = 0usize;
        let mut close_parens = 0usize;
        for b in tail[..end].bytes() {
            if b == b'(' {
                open_parens += 1;
            } else if b == b')' {
                close_parens += 1;
            }
        }
        // Trailing punctuation belongs to the sentence, not the link.
        while end > 0 {
            let c = tail[..end].chars().next_back().unwrap();
            let unbalanced_paren = c == ')' && open_parens < close_parens;
            if matches!(c, '.' | ',' | ';' | ':' | '!' | '?' | '\'' | '"' | ']') || unbalanced_paren
            {
                if c == ')' {
                    close_parens -= 1;
                }
                end -= c.len_utf8();
            } else {
                break;
            }
        }
        let url = &tail[..end];
        let min = if url.starts_with("www.") { 5 } else { 9 };
        if end >= min && url.contains('.') {
            return Some((start, end));
        }
        search_from = start + end.max(needle_len);
    }
    None
}

// ------------------------------------------------------------------ quoted history

/// Detaches the quoted history (and whatever follows it) from the message body.
fn split_quote(node: &Handle, depth: usize) -> Option<Vec<Handle>> {
    if depth > crate::browser::MAX_DOM_DEPTH {
        return None;
    }
    let children: Vec<Handle> = node.children.borrow().clone();
    for (i, child) in children.iter().enumerate() {
        if is_quote_start(child, depth + 1) {
            let (keep, quoted) = children.split_at(i);
            let quoted = quoted.to_vec();
            set_children(node, keep.to_vec());
            return Some(quoted);
        }
        if matches!(child.data, NodeData::Element { .. }) {
            if let Some(mut found) = split_quote(child, depth + 1) {
                // Everything after the quote in document order goes with it, at every
                // level: siblings that follow the ancestor holding the quote start too.
                // Keep the ancestor's sender attributes on its split-off portion too.
                if let NodeData::Element { name, attrs, .. } = &child.data {
                    let wrapper = element(&name.local, attrs.borrow().clone());
                    set_children(&wrapper, found);
                    found = vec![wrapper];
                }
                found.extend(children[i + 1..].iter().cloned());
                set_children(node, children[..=i].to_vec());
                return Some(found);
            }
        }
    }
    None
}

/// Whether this element begins a message embedded in the body: an Outlook header block, a
/// Gmail attribution line, or a Gmail quote container.
fn starts_embedded_message(node: &Handle, local: &str, attrs: &[Attribute]) -> bool {
    if local != "div" && local != "p" {
        return false;
    }
    let class = attr(attrs, "class").unwrap_or("").to_ascii_lowercase();
    if class.split_whitespace().any(|c| c == "gmail_attr") {
        return true;
    }
    let style = attr(attrs, "style").unwrap_or("").to_ascii_lowercase();
    if style.contains("border-top") && subtree_text(node, 200).trim_start().starts_with("From:") {
        return true;
    }
    // Plain-text style markers count only when they are this element's own first text,
    // not when they sit in a child that will get the rule itself.
    let own = node.children.borrow().iter().find_map(|c| match &c.data {
        NodeData::Text { contents } => {
            let t = contents.borrow().to_string();
            (!is_blank_text(&t)).then_some(Some(t))
        }
        NodeData::Element { .. } => Some(None),
        _ => None,
    });
    match own {
        Some(Some(t)) => {
            let t = t.trim_start();
            t.starts_with("-----Original Message-----")
                || t.starts_with("---------- Forwarded message")
        }
        _ => false,
    }
}

fn is_quote_start(node: &Handle, depth: usize) -> bool {
    let NodeData::Element { name, attrs, .. } = &node.data else {
        return false;
    };
    let attrs = attrs.borrow();
    let local = name.local.to_string();
    let class = attr(&attrs, "class").unwrap_or("").to_ascii_lowercase();
    let id = attr(&attrs, "id").unwrap_or("").to_ascii_lowercase();
    match local.as_str() {
        "blockquote" => attr(&attrs, "type").is_some_and(|t| t.eq_ignore_ascii_case("cite")),
        "div" => {
            if class
                .split_whitespace()
                .any(|c| c == "gmail_quote" || c == "gmail_quote_container")
            {
                // A forward is content, not history.
                return !first_text_matching(node, "gmail_attr", depth)
                    .is_some_and(|t| t.contains("Forwarded message"));
            }
            // Outlook's "From: / Sent: / To:" header block is deliberately not a quote
            // marker: people annotate inside the text that follows it, and the header is
            // the context for those notes. It stays inline, as Gmail shows it.
            class.contains("yahoo_quoted")
                || class.contains("moz-cite-prefix")
                || id == "divrplyfwdmsg"
                || id == "appendonsend"
        }
        _ => false,
    }
}

fn first_text_matching(node: &Handle, class: &str, depth: usize) -> Option<String> {
    if depth > crate::browser::MAX_DOM_DEPTH {
        return None;
    }
    for child in node.children.borrow().iter() {
        if let NodeData::Element { attrs, .. } = &child.data {
            if attr(&attrs.borrow(), "class")
                .is_some_and(|c| c.split_whitespace().any(|x| x == class))
            {
                return Some(subtree_text(child, 400));
            }
        }
        if let Some(found) = first_text_matching(child, class, depth + 1) {
            return Some(found);
        }
    }
    None
}

fn collect_subtree_text(node: &Handle, out: &mut String, limit: usize, depth: usize) {
    if out.len() >= limit || depth > crate::browser::MAX_DOM_DEPTH {
        return;
    }
    match &node.data {
        NodeData::Text { contents } => out.push_str(&contents.borrow()),
        _ => {
            for child in node.children.borrow().iter() {
                collect_subtree_text(child, out, limit, depth + 1);
            }
        }
    }
}

fn subtree_text(node: &Handle, limit: usize) -> String {
    let mut out = String::new();
    collect_subtree_text(node, &mut out, limit, 0);
    out
}

fn serialize_children(node: &Handle) -> String {
    let mut buf = Vec::new();
    let opts = SerializeOpts {
        traversal_scope: TraversalScope::ChildrenOnly(None),
        ..Default::default()
    };
    let _ = serialize(&mut buf, &SerializableHandle::from(node.clone()), opts);
    String::from_utf8(buf).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hides_what_a_browser_hides() {
        let p = prepare(
            r#"<html><head><title>Title text</title></head><body><div style="display:none">pre</div><span style="font-size:0px">zero</span><div style="visibility:hidden">vis</div><div class="preheader">ph</div><img src="https://t/x.gif" width="1" height="1"><p>Visible</p></body></html>"#,
        );
        assert!(
            !p.html.contains("Title text")
                && !p.html.contains("pre<")
                && !p.html.contains("zero")
                && !p.html.contains("vis<")
                && !p.html.contains("ph<")
        );
        assert!(!p.html.contains("<img"));
        assert!(p.html.contains("<p>Visible</p>"));
    }

    #[test]
    fn preserves_spacer_blocks_and_converts_center() {
        let p = prepare(
            r#"<div>a</div><div><br></div><p>&nbsp;</p><p><o:p>&nbsp;</o:p></p><center><h1>Head</h1></center><dl><dt>Term</dt><dd>Def</dd></dl>"#,
        );
        assert_eq!(p.html, "<html><body><div>a</div><div><br></div><p>&nbsp;</p><p><o:p>&nbsp;</o:p></p><div align=\"center\"><h1>Head</h1></div><dl><div><b>Term</b></div><div>Def</div></dl></body></html>");
    }

    #[test]
    fn links_bare_urls_outside_anchors() {
        let p = prepare(
            r#"<p>See https://example.com/a?b=1. Also www.example.org, and <a href="https://x.y">https://x.y</a></p>"#,
        );
        assert!(
            p.html
                .contains(r#"<a href="https://example.com/a?b=1">https://example.com/a?b=1</a>."#),
            "{}",
            p.html
        );
        assert!(
            p.html
                .contains(r#"<a href="https://www.example.org">www.example.org</a>,"#),
            "{}",
            p.html
        );
        assert_eq!(p.html.matches("<a ").count(), 3);
    }

    #[test]
    fn preformatted_lines_survive() {
        let p = prepare("<pre>fn main() {\n    body\n}</pre>");
        assert_eq!(
            p.html,
            "<html><body><pre>fn main() {<br>&nbsp;&nbsp;&nbsp;&nbsp;body<br>}</pre></body></html>"
        );
    }

    #[test]
    fn splits_gmail_quote_but_not_forwards() {
        let p = prepare(
            r#"<div dir="ltr">New text<br><div class="gmail_quote"><div class="gmail_attr">On Mon, X wrote:</div><blockquote class="gmail_quote">old</blockquote></div></div>"#,
        );
        assert!(p.html.contains("New text") && !p.html.contains("old"));
        assert!(p
            .quoted
            .as_deref()
            .is_some_and(|q| q.contains("old") && q.contains("wrote:")));
        let f = prepare(
            r#"<div dir="ltr">FYI<div class="gmail_quote"><div class="gmail_attr">---------- Forwarded message ---------<br>From: A</div><div>body</div></div></div>"#,
        );
        assert!(f.quoted.is_none() && f.html.contains("body"));
    }

    #[test]
    fn splits_cite_blockquotes_and_outlook_headers() {
        let p = prepare(r#"<div>Reply</div><blockquote type="cite"><div>older</div></blockquote>"#);
        assert!(
            p.quoted.as_deref().is_some_and(|q| q.contains("older")) && !p.html.contains("older")
        );
        // Outlook's header block and what follows stay inline.
        let o = prepare(
            r#"<div class="WordSection1"><p>Thanks</p><div style="border:none;border-top:solid #E1E1E1 1.0pt;padding:3.0pt 0in 0in 0in"><p><b>From:</b> A<br><b>Sent:</b> B</p></div><p>original</p></div>"#,
        );
        assert!(
            o.quoted.is_none() && o.html.contains("From:") && o.html.contains("original"),
            "{}",
            o.html
        );
        // A Gmail quote nested one level down takes the nodes after its ancestor with it.
        let n = prepare(
            r#"<div><div>New<div class="gmail_quote"><blockquote class="gmail_quote">old</blockquote></div></div><p>trailing signature</p></div>"#,
        );
        assert!(
            n.html.contains("New") && !n.html.contains("old") && !n.html.contains("trailing"),
            "{}",
            n.html
        );
        assert!(n
            .quoted
            .as_deref()
            .is_some_and(|q| q.contains("old") && q.contains("trailing")));
    }

    #[test]
    fn preserves_intentional_breaks() {
        let p = prepare("<div>FYI<br><br><div>next</div><br></div><div><br>lead<br>line<br></div>");
        assert_eq!(p.html, "<html><body><div>FYI<br><br><div>next</div><br></div><div><br>lead<br>line<br></div></body></html>");
    }

    #[test]
    fn embedded_message_headers_get_a_rule() {
        let o = prepare(
            r#"<div><p>Thanks</p><div style="border:none;border-top:solid #E1E1E1 1.0pt"><p><b>From:</b> A</p></div><p>original</p></div>"#,
        );
        assert!(o.html.contains("<hr><div"), "{}", o.html);
        let f = prepare(
            r#"<div dir="ltr">FYI<div class="gmail_quote"><div class="gmail_attr">---------- Forwarded message ---------<br>From: A</div><div>body</div></div></div>"#,
        );
        assert_eq!(f.html.matches("<hr>").count(), 1, "{}", f.html);
        // Not doubled when the author already drew one.
        let d = prepare(
            r#"<div><hr><div style="border-top:1px solid #ccc"><p>From: A</p></div></div>"#,
        );
        assert_eq!(d.html.matches("<hr>").count(), 1, "{}", d.html);
    }

    #[test]
    fn superscripts_never_resurrect_hidden_descendants() {
        let p = prepare("<p>A<sup><span hidden>2</span></sup>B<sub><span style='display:none'>3</span></sub>C</p>");
        assert!(p.html.contains("<p>ABC</p>"), "{}", p.html);
        assert!(!p.html.contains('²') && !p.html.contains('₃'));
    }

    #[test]
    fn sup_and_sub_become_glyphs_when_possible() {
        let p = prepare(
            "<p>E=mc<sup>2</sup>, H<sub>2</sub>O, see note<sup>12</sup> and x<sup>th</sup></p>",
        );
        assert!(
            p.html.contains("E=mc²") && p.html.contains("H₂O") && p.html.contains("note¹²"),
            "{}",
            p.html
        );
        assert!(
            p.html.contains("xth") && !p.html.contains("<sup>"),
            "{}",
            p.html
        );
    }

    #[test]
    fn url_boundaries() {
        assert_eq!(find_url("go to https://a.b/c, then"), Some((6, 13)));
        assert_eq!(find_url("(see www.x.org)"), Some((5, 9)));
        assert_eq!(find_url("mail@www.example.com"), None);
        assert_eq!(find_url("https://x"), None);
        // Invalid prefix matches must not hide a valid URL later in the string.
        assert_eq!(
            find_url("mail@www.example.com and www.good.org"),
            Some((25, 12))
        );
        assert_eq!(
            find_url("foohttps://bad.test then https://good.example/a"),
            Some((25, 22))
        );
        assert_eq!(
            find_url("https://x then https://good.example/b"),
            Some((15, 22))
        );
        // Balanced vs unbalanced trailing parentheses in O(n).
        assert_eq!(
            find_url("(see https://en.wikipedia.org/wiki/Rust_(lang)))"),
            Some((5, 41))
        );
        let many_parens = format!("https://example.org/a{}", ")".repeat(2000));
        assert_eq!(find_url(&many_parens), Some((0, 21)));
    }

    #[test]
    fn sup_fallback_does_not_double_clean_or_drop_children() {
        let p = prepare("<p>A<sup><span>note</span></sup> B<sup><dt>term</dt></sup></p>");
        assert!(p.html.contains("<span>note</span>"), "{}", p.html);
        assert_eq!(p.html.matches("<b>").count(), 1, "{}", p.html);
        assert!(p.html.contains("<div><b>term</b></div>"), "{}", p.html);
    }

    #[test]
    fn deeply_nested_html_is_bounded_without_stack_overflow() {
        let deep = format!("{}hi{}", "<div>".repeat(1000), "</div>".repeat(1000));
        let p = prepare(&deep);
        assert!(p.html.is_empty());
        assert!(p.quoted.is_none());
    }
}
