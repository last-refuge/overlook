//! Sanitization of received HTML.
//!
//! Received mail is untrusted. The sanitizer removes scripts, stylesheets, forms, frames, event
//! handlers, and (by default) every remote resource. The output is safe to hand to the
//! native HTML text view, which executes nothing, but the policy is enforced here so that
//! no renderer ever sees the raw markup.

use markup5ever_rcdom::{Handle, NodeData, RcDom};
use std::collections::HashSet;
use std::sync::LazyLock;
#[path = "presentation.rs"]
pub(crate) mod presentation;

/// Remote-content policy for a particular rendering.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum RemoteContent {
    /// Block remote images and other resources (default).
    #[default]
    Blocked,
    /// The user chose to load remote content for this message.
    Allowed,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Sanitized {
    pub html: String,
    /// The quoted history of a reply, split off so the reader can collapse it.
    pub quoted: Option<String>,
    /// Number of remote resources removed.
    pub blocked_remote: usize,
    /// Plain-text rendering for previews and search (body and quoted history).
    pub text: String,
}

static BUILDER_BLOCKED: LazyLock<ammonia::Builder<'static>> =
    LazyLock::new(|| build_cleaner(RemoteContent::Blocked));
static BUILDER_ALLOWED: LazyLock<ammonia::Builder<'static>> =
    LazyLock::new(|| build_cleaner(RemoteContent::Allowed));

/// Sanitizes `html` for display. Never fails: malformed input yields best-effort output.
/// The ammonia policy shared by the reader and by outgoing quotes.
fn builder(policy: RemoteContent) -> &'static ammonia::Builder<'static> {
    match policy {
        RemoteContent::Blocked => &BUILDER_BLOCKED,
        RemoteContent::Allowed => &BUILDER_ALLOWED,
    }
}

fn build_cleaner(policy: RemoteContent) -> ammonia::Builder<'static> {
    let mut builder = ammonia::Builder::default();
    let tags: HashSet<&str> = [
        "a",
        "abbr",
        "b",
        "blockquote",
        "br",
        "caption",
        "center",
        "code",
        "dd",
        "del",
        "div",
        "dl",
        "dt",
        "em",
        "font",
        "h1",
        "h2",
        "h3",
        "h4",
        "h5",
        "h6",
        "hr",
        "i",
        "img",
        "ins",
        "kbd",
        "li",
        "ol",
        "p",
        "pre",
        "q",
        "s",
        "small",
        "span",
        "strike",
        "strong",
        "sub",
        "sup",
        "table",
        "tbody",
        "td",
        "tfoot",
        "th",
        "thead",
        "tr",
        "u",
        "ul",
    ]
    .into_iter()
    .collect();
    builder.tags(tags);
    builder.rm_tags([
        "script", "style", "iframe", "object", "embed", "form", "input", "button", "link", "meta",
        "svg", "math", "video", "audio", "base",
    ]);
    builder.clean_content_tags(
        [
            "script", "style", "iframe", "object", "embed", "svg", "math", "video", "audio",
            "noscript", "template",
        ]
        .into_iter()
        .collect(),
    );
    builder.generic_attributes(
        ["title", "dir", "lang", "style", "align"]
            .into_iter()
            .collect(),
    );
    builder.add_tag_attributes("a", ["href"]);
    builder.add_tag_attributes("img", ["src", "alt", "width", "height"]);
    builder.add_tag_attributes("td", ["colspan", "rowspan"]);
    builder.add_tag_attributes("th", ["colspan", "rowspan"]);
    builder.add_tag_attributes("ol", ["start"]);
    // Inherited presentation belongs on blocks as well as inline wrappers.
    builder.add_tag_attributes("font", ["color", "face", "size"]);
    builder.link_rel(Some("noopener noreferrer"));
    builder.url_schemes(["http", "https", "mailto", "cid"].into_iter().collect());
    builder.strip_comments(true);
    let blocked = policy == RemoteContent::Blocked;
    builder.attribute_filter(move |element, attribute, value| {
        if attribute == "style" {
            return presentation::safe_style(value).map(Into::into);
        }
        if attribute == "align" {
            let value = value.trim().to_ascii_lowercase();
            return matches!(value.as_str(), "left" | "right" | "center" | "justify")
                .then(|| value.into());
        }
        if matches!(element, "td" | "th") && matches!(attribute, "colspan" | "rowspan") {
            // Spans are presentation hints, not permission to allocate unbounded cells.
            let span = value.trim().parse::<u32>().ok().filter(|n| *n > 0)?;
            return Some(span.min(64).to_string().into());
        }
        if element == "font" {
            let safe = match attribute {
                "color" => presentation::color(value),
                "face" => presentation::font_family(value),
                "size" => matches!(
                    value.trim(),
                    "1" | "2"
                        | "3"
                        | "4"
                        | "5"
                        | "6"
                        | "7"
                        | "+1"
                        | "+2"
                        | "+3"
                        | "-1"
                        | "-2"
                        | "-3"
                ),
                _ => true,
            };
            if !safe {
                return None;
            }
        }
        if blocked
            && element == "img"
            && attribute == "src"
            && !value.to_ascii_lowercase().starts_with("cid:")
        {
            return None;
        }
        Some(value.into())
    });
    builder
}

/// Cleans HTML without the structural pre-pass: scripts, handlers, and disallowed tags are
/// removed along with hidden content, but quoted history and layout stay intact. Used when an
/// original message is quoted in a reply or forward, where the recipient should get the whole
/// thing.
pub fn clean_only(html: &str, policy: RemoteContent) -> String {
    let dom = presentation::visible_document(html);
    let clean = builder(policy)
        .clean(&presentation::serialize_body(&dom))
        .to_string();
    // Canonical serialization keeps equivalent entity spellings stable across passes.
    presentation::serialize_body(&presentation::document(&clean))
}

pub fn sanitize_html(html: &str, policy: RemoteContent) -> Sanitized {
    let Some(prepared) = crate::prepass::prepare_dom(html) else {
        return Sanitized::default();
    };
    let blocked_remote = if policy == RemoteContent::Blocked {
        count_remote_in_dom(&prepared.doc.document)
            + prepared
                .quoted
                .as_ref()
                .map(|q| count_remote_in_dom(&q.document))
                .unwrap_or(0)
    } else {
        0
    };

    let builder = builder(policy);
    let root_preserved = presentation::serialize_body(&prepared.doc);
    let cleaned = builder.clean(&root_preserved).to_string();
    let (mut clean, clean_dom) = flatten_tables_with_dom(&cleaned);
    let mut quoted_and_text = prepared.quoted.as_ref().map(|q| {
        let q_root = presentation::serialize_body(q);
        let q_cleaned = builder.clean(&q_root).to_string();
        let (q_html, q_dom) = flatten_tables_with_dom(&q_cleaned);
        let q_text = render_html_to_text(&q_dom.document);
        (q_html, q_text)
    });
    let mut text = render_html_to_text(&clean_dom.document);
    let mut quoted = None;
    if let Some((q, quoted_text)) = quoted_and_text.take() {
        if text.trim().is_empty() {
            // Nothing but history: show it inline rather than behind a toggle.
            clean.push_str(&q);
            text = quoted_text;
        } else if !quoted_text.trim().is_empty() {
            text.push_str("\n\n");
            text.push_str(&quoted_text);
            quoted = Some(q);
        }
    }
    Sanitized {
        html: clean,
        quoted,
        blocked_remote,
        text,
    }
}

fn contains_ignore_ascii_case(haystack: &str, needle: &str) -> bool {
    haystack
        .as_bytes()
        .windows(needle.len())
        .any(|w| w.eq_ignore_ascii_case(needle.as_bytes()))
}

/// Rewrites tables into block flow: each row becomes a block, cells are separated by a
/// wide gap and cells retain their sender attributes on block wrappers. Email relies on
/// nested layout tables that the native HTML view cannot lay
/// out yet; without this step adjacent cells run together. Column alignment of real
/// data tables is lost, which is a known foundation gap.
pub fn flatten_tables(html: &str) -> String {
    flatten_tables_with_dom(html).0
}

fn flatten_tables_with_dom(html: &str) -> (String, RcDom) {
    fn render(n: &Handle, out: &mut String, flatten: bool, depth: usize, changed: &mut bool) {
        if depth > crate::browser::MAX_DOM_DEPTH {
            return;
        }
        match &n.data {
            NodeData::Text { contents } => out.push_str(&escape_html(&contents.borrow())),
            NodeData::Element { name, attrs, .. } => {
                let local = &*name.local;
                let flatten = if local == "table" {
                    let should_flatten = !is_data_table(n);
                    if should_flatten {
                        *changed = true;
                    }
                    should_flatten
                } else {
                    flatten
                };
                let replacement = if flatten {
                    match local {
                        "table" | "tbody" | "thead" | "tfoot" | "tr" | "td" | "th" => "div",
                        "caption" => "p",
                        _ => local,
                    }
                } else {
                    local
                };
                out.push('<');
                out.push_str(replacement);
                for a in attrs.borrow().iter() {
                    // Table-only attributes have no meaning on a flattened div/p.
                    // Carry only presentation that is valid on the replacement node.
                    if replacement != local
                        && !matches!(&*a.name.local, "title" | "dir" | "lang" | "style" | "align")
                    {
                        continue;
                    }
                    out.push(' ');
                    out.push_str(&a.name.local);
                    out.push_str("=\"");
                    out.push_str(&escape_html(&a.value));
                    out.push('"');
                }
                out.push('>');
                let mut cells = 0;
                for c in n.children.borrow().iter() {
                    if flatten
                        && local == "tr"
                        && matches!(&c.data, NodeData::Element { name, .. } if matches!(&*name.local, "td" | "th"))
                    {
                        if cells > 0 {
                            out.push_str("\u{a0}\u{a0}\u{a0}\u{a0}");
                        }
                        cells += 1;
                    }
                    render(c, out, flatten, depth + 1, changed);
                }
                if !matches!(local, "br" | "hr" | "img") {
                    out.push_str("</");
                    out.push_str(replacement);
                    out.push('>');
                }
            }
            NodeData::Document => {
                for c in n.children.borrow().iter() {
                    render(c, out, flatten, depth + 1, changed);
                }
            }
            _ => {}
        }
    }
    let dom = presentation::document(html);
    if !contains_ignore_ascii_case(html, "<table") {
        let serialized = presentation::serialize_body(&dom);
        return (serialized, dom);
    }
    let mut out = String::with_capacity(html.len());
    let mut changed = false;
    presentation::walk(&dom.document, &mut |n| {
        if matches!(&n.data, NodeData::Element { name, .. } if &*name.local == "body") {
            for c in n.children.borrow().iter() {
                render(c, &mut out, false, 0, &mut changed);
            }
        }
    });
    if !changed {
        let serialized = presentation::serialize_body(&dom);
        return (serialized, dom);
    }
    // Transformations can change HTML's adoption-agency/foster-parenting behavior
    // (notably anchors around table layouts). Normalize the resulting tree now.
    let normalized = presentation::document(&out);
    let serialized = presentation::serialize_body(&normalized);
    (serialized, normalized)
}

/// Counts characters in the trimmed text of `node` without allocating a `String`.
fn trimmed_char_count(node: &Handle) -> usize {
    let mut stack = vec![(node.clone(), 0usize)];
    let mut seen_non_ws = false;
    let mut total_chars = 0usize;
    let mut trailing_ws = 0usize;
    while let Some((n, depth)) = stack.pop() {
        if depth > crate::browser::MAX_DOM_DEPTH {
            continue;
        }
        match &n.data {
            NodeData::Text { contents } => {
                for c in contents.borrow().chars() {
                    if c.is_whitespace() {
                        if seen_non_ws {
                            trailing_ws += 1;
                        }
                    } else {
                        seen_non_ws = true;
                        total_chars += trailing_ws + 1;
                        trailing_ws = 0;
                    }
                }
            }
            _ => {
                let children = n.children.borrow();
                for child in children.iter().rev() {
                    stack.push((child.clone(), depth + 1));
                }
            }
        }
    }
    total_chars
}

/// Classify actual cells, not tag-like strings or the tail of an entire row.
fn is_data_table(table: &Handle) -> bool {
    let mut rows = Vec::new();
    let mut stack = Vec::new();
    for child in table.children.borrow().iter().rev() {
        stack.push((child.clone(), 1usize));
    }
    while let Some((n, depth)) = stack.pop() {
        if depth > crate::browser::MAX_DOM_DEPTH {
            continue;
        }
        if let NodeData::Element { name, .. } = &n.data {
            let local = &*name.local;
            if local == "img" || local == "table" {
                return false;
            }
            if local == "tr" {
                rows.push(n.clone());
            }
        }
        let children = n.children.borrow();
        for child in children.iter().rev() {
            stack.push((child.clone(), depth + 1));
        }
    }
    if rows.len() < 2 {
        return false;
    }
    let mut first_count: Option<usize> = None;
    let (mut cells, mut total, mut longest, mut header) = (0usize, 0usize, 0usize, false);
    for row in rows {
        let mut count = 0usize;
        for cell in row.children.borrow().iter() {
            if let NodeData::Element { name, attrs, .. } = &cell.data {
                if !matches!(&*name.local, "td" | "th") {
                    continue;
                }
                header |= &*name.local == "th";
                let span = attrs
                    .borrow()
                    .iter()
                    .find(|a| &*a.name.local == "colspan")
                    .and_then(|a| a.value.parse::<usize>().ok())
                    .unwrap_or(1)
                    .clamp(1, 1000);
                count = count.saturating_add(span);
                let len = trimmed_char_count(cell);
                longest = longest.max(len);
                total += len;
                cells += 1;
            }
        }
        match first_count {
            None => {
                if !(2..=8).contains(&count) {
                    return false;
                }
                first_count = Some(count);
            }
            Some(first) if count != first => return false,
            _ => {}
        }
    }
    header || (cells > 0 && longest <= 160 && total / cells <= 60)
}

/// Adapt only actual color attributes/declarations; sender text and URLs are untouched.
pub fn adapt_colors(html: &str, dark: bool) -> String {
    let dom = presentation::document(html);
    let adapt = |value: &str| {
        presentation::parse_color(value).map(|(r, g, b, alpha)| {
            let (r, g, b) = adapt_rgb(r, g, b, dark);
            match alpha {
                Some(a) => format!("rgba({r},{g},{b},{a})"),
                None => format!("#{r:02x}{g:02x}{b:02x}"),
            }
        })
    };
    presentation::walk(&dom.document, &mut |n| {
        if let NodeData::Element { attrs, .. } = &n.data {
            for a in attrs.borrow_mut().iter_mut() {
                if &*a.name.local == "color" {
                    if let Some(v) = adapt(&a.value) {
                        a.value = v.into();
                    }
                } else if &*a.name.local == "style" {
                    let style = a
                        .value
                        .split(';')
                        .map(|decl| {
                            if let Some((key, value)) = decl.split_once(':') {
                                if key.trim().eq_ignore_ascii_case("color") {
                                    let (value, important) = presentation::declaration_value(value);
                                    if let Some(v) = adapt(value) {
                                        let priority = if important { " !important" } else { "" };
                                        return format!("{key}:{v}{priority}");
                                    }
                                }
                            }
                            decl.to_owned()
                        })
                        .collect::<Vec<_>>()
                        .join(";");
                    a.value = style.into();
                }
            }
        }
    });
    presentation::serialize_body(&dom)
}

/// Adjusts lightness only, in HSL, so the hue the author chose is kept.
fn adapt_rgb(r: u8, g: u8, b: u8, dark: bool) -> (u8, u8, u8) {
    let (rf, gf, bf) = (r as f32 / 255., g as f32 / 255., b as f32 / 255.);
    let max = rf.max(gf).max(bf);
    let min = rf.min(gf).min(bf);
    let l = (max + min) / 2.;
    let d = max - min;
    let s = if d == 0. {
        0.
    } else {
        d / (1. - (2. * l - 1.).abs())
    };
    let h = if d == 0. {
        0.
    } else if max == rf {
        60. * (((gf - bf) / d) % 6.)
    } else if max == gf {
        60. * ((bf - rf) / d + 2.)
    } else {
        60. * ((rf - gf) / d + 4.)
    };
    let h = if h < 0. { h + 360. } else { h };
    let new_l: f32 = if dark && l < 0.45 {
        if s < 0.15 {
            0.82
        } else {
            0.68
        }
    } else if !dark && l > 0.8 {
        if s < 0.15 {
            0.25
        } else {
            0.38
        }
    } else {
        return (r, g, b);
    };
    // HSL back to RGB.
    let c = (1. - (2. * new_l - 1.).abs()) * s;
    let x = c * (1. - ((h / 60.) % 2. - 1.).abs());
    let m = new_l - c / 2.;
    let (r1, g1, b1) = match h as u32 {
        0..=59 => (c, x, 0.),
        60..=119 => (x, c, 0.),
        120..=179 => (0., c, x),
        180..=239 => (0., x, c),
        240..=299 => (x, 0., c),
        _ => (c, 0., x),
    };
    (
        ((r1 + m) * 255.).round() as u8,
        ((g1 + m) * 255.).round() as u8,
        ((b1 + m) * 255.).round() as u8,
    )
}

fn count_remote_in_dom(root: &Handle) -> usize {
    let mut count = 0;
    presentation::walk(root, &mut |n| {
        if let NodeData::Element { name, attrs, .. } = &n.data {
            if &*name.local == "img"
                && attrs.borrow().iter().any(|a| {
                    &*a.name.local == "src" && {
                        let src = a.value.trim();
                        src.get(..7).is_some_and(|p| p.eq_ignore_ascii_case("http://"))
                            || src.get(..8).is_some_and(|p| p.eq_ignore_ascii_case("https://"))
                            || src.starts_with("//")
                    }
                })
            {
                count += 1;
            }
        }
    });
    count
}

/// Parsed HTML-to-text conversion. Active and invisible subtrees are never indexed.
pub fn html_to_text(html: &str) -> String {
    let dom = presentation::visible_document(html);
    render_html_to_text(&dom.document)
}

fn render_html_to_text(root: &Handle) -> String {
    fn boundary(out: &mut String) {
        if !out.is_empty() && !out.ends_with("\n\n") {
            if out.ends_with('\n') {
                out.push('\n');
            } else {
                out.push_str("\n\n");
            }
        }
    }
    fn render(n: &Handle, out: &mut String, pre: bool, depth: usize) {
        if depth > crate::browser::MAX_DOM_DEPTH {
            return;
        }
        match &n.data {
            NodeData::Text { contents } => {
                for c in contents.borrow().chars() {
                    if pre {
                        out.push(c);
                    } else if c.is_whitespace() {
                        if !out.is_empty() && !out.ends_with([' ', '\n']) {
                            out.push(' ');
                        }
                    } else {
                        out.push(c);
                    }
                }
            }
            NodeData::Element { name, attrs, .. } => {
                let local = &*name.local;
                if local == "br" {
                    out.push('\n');
                    return;
                }
                let block = matches!(
                    local,
                    "p" | "div"
                        | "tr"
                        | "h1"
                        | "h2"
                        | "h3"
                        | "h4"
                        | "h5"
                        | "h6"
                        | "blockquote"
                        | "pre"
                        | "table"
                        | "ul"
                        | "ol"
                );
                if block {
                    boundary(out);
                }
                if local == "li" {
                    if !out.is_empty() && !out.ends_with('\n') {
                        out.push('\n');
                    }
                    out.push_str("• ");
                }
                let pre = pre
                    || local == "pre"
                    || attrs.borrow().iter().any(|a| {
                        &*a.name.local == "style"
                            && presentation::safe_style(&a.value).is_some_and(|s| {
                                s.split(';').any(|d| {
                                    matches!(d, "white-space:pre" | "white-space:pre-wrap")
                                })
                            })
                    });
                for c in n.children.borrow().iter() {
                    render(c, out, pre, depth + 1);
                }
                if block {
                    boundary(out);
                }
                if matches!(local, "td" | "th") {
                    out.push('\t');
                }
            }
            NodeData::Document => {
                for c in n.children.borrow().iter() {
                    render(c, out, pre, depth + 1);
                }
            }
            _ => {}
        }
    }
    let mut out = String::new();
    render(root, &mut out, false, 0);
    out.trim().to_owned()
}

/// Converts plain text to safe HTML for display in the HTML view.
///
/// Line breaks, blank runs, and indentation are preserved with wrapping. Lines quoted with `>`
/// are grouped into (nested) blockquotes with the markers removed, the way a mail client
/// shows a plain-text reply. Bare URLs become links.
pub fn text_to_html(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    // Plain text owns its line spacing and indentation. Do not reinterpret blank
    // runs as generic HTML paragraphs (which acquire renderer-specific margins).
    let mut out = String::from("<div style=\"white-space:pre-wrap\">");
    let mut depth = 0usize;
    for (index, raw) in text.split('\n').enumerate() {
        let (line_depth, body) = strip_quote_markers(raw.strip_suffix('\r').unwrap_or(raw));
        if line_depth == depth && index > 0 {
            out.push_str("<br>");
        }
        while depth > line_depth {
            out.push_str("</blockquote>");
            depth -= 1;
        }
        while depth < line_depth {
            out.push_str("<blockquote>");
            depth += 1;
        }
        let marker = body.trim_start();
        if line_depth == 0
            && (marker.starts_with("---------- Forwarded message")
                || marker.starts_with("-----Original Message-----"))
        {
            out.push_str("<hr>");
        }
        out.push_str(&linkify_escaped(body));
    }
    while depth > 0 {
        out.push_str("</blockquote>");
        depth -= 1;
    }
    out.push_str("</div>");
    out
}

const MAX_QUOTE_DEPTH: usize = 32;

/// Counts the leading `>` markers of a quoted line and returns the text after them.
fn strip_quote_markers(line: &str) -> (usize, &str) {
    let mut depth = 0;
    let mut rest = line.trim_start();
    while let Some(after) = rest.strip_prefix('>') {
        depth += 1;
        rest = after.strip_prefix(' ').unwrap_or(after);
    }
    (depth.min(MAX_QUOTE_DEPTH), if depth == 0 { line } else { rest })
}

/// Escapes text for HTML, turning bare URLs into links.
fn linkify_escaped(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some((start, len)) = crate::prepass::find_url(rest) {
        out.push_str(&escape_html(&rest[..start]));
        let url = &rest[start..start + len];
        let href = if url.starts_with("www.") {
            format!("https://{url}")
        } else {
            url.to_owned()
        };
        out.push_str(&format!(
            "<a href=\"{}\">{}</a>",
            escape_html(&href),
            escape_html(url)
        ));
        rest = &rest[start + len..];
    }
    out.push_str(&escape_html(rest));
    out
}

pub fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sender_presentation_is_bounded_and_inherited() {
        let input = r##"<blockquote style="color:#123456;font-size:1.2rem;font-family:'Example Sans', serif;font-weight:700;font-style:oblique;text-decoration:underline;text-align:justify;line-height:150%;margin:0 2em 12pt 4px;margin-top:1rem;margin-bottom:0;white-space:pre-wrap;position:fixed;background:url(https://bad.invalid/x);--x:red"><p>Child</p></blockquote><font face="Example Sans,serif" size="+2" color="navy">Legacy</font>"##;
        let s = sanitize_html(input, RemoteContent::Blocked);
        for expected in [
            "font-size:1.2rem",
            "font-family:'example sans', serif",
            "font-weight:700",
            "font-style:oblique",
            "text-decoration:underline",
            "text-align:justify",
            "line-height:150%",
            "margin:1rem 2em 0 4px",
            "white-space:pre-wrap",
            "<p>Child</p>",
            "size=\"+2\"",
        ] {
            assert!(s.html.contains(expected), "missing {expected}: {}", s.html);
        }
        assert!(
            !s.html.contains("position")
                && !s.html.contains("bad.invalid")
                && !s.html.contains("--x")
        );
        assert_eq!(sanitize_html(&s.html, RemoteContent::Blocked).html, s.html);
    }

    #[test]
    fn rejects_active_and_unbounded_style_values() {
        for style in [
            "font-size:999999px",
            "font-size:-1em",
            "margin:-2px",
            "margin:999rem",
            "line-height:NaN",
            "font-family:url(x)",
            "color:expression(alert(1))",
            "color:#12",
            "font-weight:950",
            "white-space:nowrap",
            "font-size:var(--size)",
            "font-family:&#x5c;65vil",
            "color:rgb(999,0,0)",
            "font-family:'",
        ] {
            let html = clean_only(
                &format!("<p style=\"{style}\">safe</p>"),
                RemoteContent::Blocked,
            );
            assert!(!html.contains("style="), "{style}: {html}");
        }
        let safe = clean_only(
            r#"<font face="url(x)" size="999" color="url(x)">safe</font>"#,
            RemoteContent::Blocked,
        );
        assert_eq!(safe, "<font>safe</font>");
    }

    #[test]
    fn full_history_quote_removes_hidden_without_restructuring() {
        let input = r#"<div style="display:none">secret</div><table><tr><td>new</td></tr></table><div class="gmail_quote"><blockquote type="cite">old</blockquote></div><p hidden>also secret</p><img width="1" height="1" src="https://t.invalid/pixel">"#;
        let html = clean_only(input, RemoteContent::Allowed);
        assert!(html.contains("<table>") && html.contains("new") && html.contains("old"));
        assert!(!html.contains("secret") && !html.contains("pixel"));
        assert_eq!(clean_only(&html, RemoteContent::Allowed), html);
    }

    #[test]
    fn text_extraction_never_exposes_active_or_hidden_subtrees() {
        let text = html_to_text(
            r#"<style>secret-css</style><script>secret-js <p>x</script><p>A &lt;script&gt; literal</p><div hidden>secret-hidden</div><svg><text>secret-svg</text></svg><template>secret-template</template><p>B"#,
        );
        assert_eq!(text, "A <script> literal\n\nB");
        assert!(!html_to_text("<script>unterminated").contains("unterminated"));
        assert_eq!(
            html_to_text("<p>a<br><span><br><br></span>b</p>"),
            "a\n\n\nb"
        );
        assert_eq!(html_to_text("<pre>a  b\n  c</pre>"), "a  b\n  c");
    }

    #[test]
    fn root_presentation_is_safe_and_idempotent() {
        let input = r#"<html dir="rtl" lang="ar" style="color:navy;position:fixed" onclick="bad()"><body lang="en" style="font-size:12pt;background:url(https://bad.invalid)"><p>Hello</p></body></html>"#;
        let expected = r#"<div dir="rtl" lang="ar" style="color:navy"><div lang="en" style="font-size:12pt"><p>Hello</p></div></div>"#;
        for clean in [
            clean_only(input, RemoteContent::Blocked),
            sanitize_html(input, RemoteContent::Blocked).html,
        ] {
            assert_eq!(clean, expected);
            assert_eq!(clean_only(&clean, RemoteContent::Blocked), expected);
            assert_eq!(sanitize_html(&clean, RemoteContent::Blocked).html, expected);
        }
        assert_eq!(
            clean_only("<html><body><p>x</p></body></html>", RemoteContent::Blocked),
            "<p>x</p>"
        );
        assert_eq!(
            adapt_colors(input, true),
            r#"<div dir="rtl" lang="ar" style="color:#5c5cff"><div lang="en" style="font-size:12pt"><p>Hello</p></div></div>"#
        );
    }

    #[test]
    fn split_history_preserves_root_inheritance_exactly() {
        let s = sanitize_html(
            r#"<html dir="rtl" style="color:navy"><body lang="ar"><p>new</p><blockquote type="cite"><p>old</p></blockquote></body></html>"#,
            RemoteContent::Blocked,
        );
        assert_eq!(
            s.html,
            r#"<div dir="rtl" style="color:navy"><div lang="ar"><p>new</p></div></div>"#
        );
        let quoted = s.quoted.unwrap();
        assert_eq!(
            quoted,
            r#"<div dir="rtl" style="color:navy"><div lang="ar"><blockquote><p>old</p></blockquote></div></div>"#
        );
        assert_eq!(clean_only(&quoted, RemoteContent::Blocked), quoted);
    }

    #[test]
    fn safe_side_margins_have_the_same_bounds_as_other_margins() {
        assert_eq!(
            clean_only(
                r#"<p style="margin-left:8px;margin-right:8px">x</p>"#,
                RemoteContent::Blocked
            ),
            r#"<p style="margin-left:8px;margin-right:8px">x</p>"#
        );
        assert_eq!(
            clean_only(
                r#"<p style="margin-left:-1px;margin-right:999px">x</p>"#,
                RemoteContent::Blocked
            ),
            "<p>x</p>"
        );
    }

    #[test]
    fn inline_priority_is_resolved_before_stripping_important() {
        let input = r#"<p style="color:red !important;color:blue;font-size:12pt ! important;font-size:18pt;color:green !important">x</p>"#;
        let expected = r#"<p style="font-size:12pt;color:green">x</p>"#;
        assert_eq!(clean_only(input, RemoteContent::Blocked), expected);
        assert_eq!(sanitize_html(input, RemoteContent::Blocked).html, expected);
        assert_eq!(clean_only(expected, RemoteContent::Blocked), expected);
    }

    #[test]
    fn visibility_uses_winning_declarations_and_requires_clipping() {
        for style in [
            "display:none;display:block",
            "visibility:hidden;visibility:visible",
            "opacity:0;opacity:1",
            "font-size:0;font-size:12pt",
            "max-height:0",
            "max-height:0;overflow:visible",
            "height:0;overflow:hidden;overflow:visible",
            "display:block !important;display:none",
            "height:0;overflow-y:visible !important;overflow:hidden",
        ] {
            let input = format!("<div style=\"{style}\">Visible</div>");
            assert_eq!(html_to_text(&input), "Visible", "{style}");
            assert_eq!(
                sanitize_html(&input, RemoteContent::Blocked).text,
                "Visible",
                "{style}"
            );
            assert!(
                clean_only(&input, RemoteContent::Blocked).contains(">Visible</div>"),
                "{style}"
            );
        }
        for style in [
            "display:none !important;display:block",
            "display:none;display:bogus",
            "max-height:0;overflow:hidden",
            "height:0;overflow-y:clip",
            "height:0;overflow:hidden !important;overflow-y:visible",
            "opacity:0 ! important;opacity:1",
        ] {
            let input = format!("<div style=\"{style}\">Secret</div><p>Visible</p>");
            assert_eq!(html_to_text(&input), "Visible", "{style}");
            assert_eq!(
                clean_only(&input, RemoteContent::Blocked),
                "<p>Visible</p>",
                "{style}"
            );
            assert_eq!(
                sanitize_html(&input, RemoteContent::Blocked).html,
                "<p>Visible</p>",
                "{style}"
            );
        }
    }

    #[test]
    fn color_policy_and_adaptation_agree_and_preserve_alpha() {
        for (name, (r, g, b)) in cssparser::color::all_named_colors() {
            assert!(presentation::color(name), "{name}");
            assert_eq!(
                presentation::parse_color(name),
                Some((r, g, b, None)),
                "{name}"
            );
        }
        assert_eq!(presentation::parse_color("maroon"), Some((128, 0, 0, None)));
        let input = r#"<p style="color:rgba(0,0,0,0.25)">x</p><font color="rgba(100%,100%,100%,0)">y</font><span style="color:darkslategray">z</span>"#;
        let clean = clean_only(input, RemoteContent::Blocked);
        assert_eq!(clean, input);
        let dark = adapt_colors(&clean, true);
        assert_eq!(
            dark,
            r#"<p style="color:rgba(209,209,209,0.25)">x</p><font color="rgba(255,255,255,0)">y</font><span style="color:#99c2c2">z</span>"#
        );
        assert_eq!(adapt_colors(&dark, true), dark);
        assert_eq!(
            adapt_colors(r#"<p style="color:rgba(255,255,255,.5)">x</p>"#, false),
            r#"<p style="color:rgba(64,64,64,.5)">x</p>"#
        );
    }

    #[test]
    fn color_adaptation_does_not_rewrite_text_or_link_values() {
        let input = r##"<p>color:#000000; color="#ffffff" <a href="https://example.invalid/?color:#000000" title="color:#000000">color:#000000</a><span style="font-size:12pt;color:#000000">x</span><font color='#000000'>y</font></p>"##;
        let html = adapt_colors(input, true);
        assert!(html.contains("color:#000000; color=\"#ffffff\""));
        assert!(html.contains("href=\"https://example.invalid/?color:#000000\""));
        assert!(html.contains("title=\"color:#000000\""));
        assert!(html.contains("font-size:12pt;color:#d1d1d1"));
        assert!(html.contains("<font color=\"#d1d1d1\">"));
    }

    #[test]
    fn flattened_layout_attributes_and_anchor_structure_are_stable() {
        let input = "<a href='https://example.test'><table align='center' width='600'><tr><td align='left' colspan='2'><p>Newsletter copy with enough text to represent a layout cell and not a numeric data grid.</p></td></tr></table></a>";
        let first = sanitize_html(input, RemoteContent::Blocked);
        assert!(first.html.contains("align=\"center\"") && first.html.contains("align=\"left\""));
        assert!(!first.html.contains("colspan="));
        assert_eq!(
            sanitize_html(&first.html, RemoteContent::Blocked).html,
            first.html
        );
        assert_eq!(
            clean_only(&first.html, RemoteContent::Blocked),
            clean_only(
                &clean_only(&first.html, RemoteContent::Blocked),
                RemoteContent::Blocked
            )
        );
    }

    #[test]
    fn margin_shorthand_and_longhand_priority_is_resolved_per_side() {
        for (input, expected) in [
            ("margin-top:0!important;margin:40px", "margin:0 40px 40px"),
            ("margin:5px!important;margin-top:40px", "margin:5px"),
            (
                "margin:5px;margin-top:10px!important;margin:20px!important",
                "margin:20px",
            ),
        ] {
            let html = clean_only(&format!("<p style='{input}'>X</p>"), RemoteContent::Blocked);
            assert!(html.contains(expected), "{input}: {html}");
            assert_eq!(clean_only(&html, RemoteContent::Blocked), html);
        }
    }

    #[test]
    fn table_spans_cannot_request_unbounded_layout() {
        let clean = clean_only(
            "<table><tr><td colspan='999999999' rowspan='0'>cell</td></tr></table>",
            RemoteContent::Blocked,
        );
        assert!(clean.contains("colspan=\"64\""));
        assert!(!clean.contains("rowspan="));
        assert_eq!(clean_only(&clean, RemoteContent::Blocked), clean);
    }

    #[test]
    fn counts_real_remote_images_not_text_or_attributes() {
        let s = sanitize_html(
            r#"<p>src=&quot;https://fake.invalid&quot;</p><a href="https://example.invalid/src=http">link</a><img SRC = 'HTTPS://example.invalid/a'><img src=//example.invalid/b><img src=cid:local><img width=1 height=1 src=https://example.invalid/pixel>"#,
            RemoteContent::Blocked,
        );
        assert_eq!(s.blocked_remote, 2);
    }

    #[test]
    fn tables_use_real_cells_and_nested_row_state() {
        let one_column = "<table><tr><td>a</td></tr><tr><td>b</td></tr></table>";
        assert!(!flatten_tables(one_column).contains("<table"));
        let nested =
            "<table><tr><td><table><tr><td>inner</td></tr></table></td><td>outer</td></tr></table>";
        let flat = flatten_tables(nested);
        assert!(
            flat.contains("&nbsp;&nbsp;&nbsp;&nbsp;<div>outer"),
            "{flat}"
        );
        assert_eq!(flatten_tables(&flat), flat);
        let malformed = "<table><tr><td title='>'>a<td>b<tr><td>c<td>d</table>";
        let html = sanitize_html(malformed, RemoteContent::Blocked).html;
        assert!(html.contains("<table") && html.contains("<td"), "{html}");
        assert_eq!(sanitize_html(&html, RemoteContent::Blocked).html, html);
    }

    #[test]
    fn blank_blocks_and_inline_wrapped_hard_breaks_survive() {
        let input = "<p><br><span><br></span>x<br></p><p></p><div><span><br></span></div><p>&nbsp;</p><div><br>tail<br></div>";
        let html = sanitize_html(input, RemoteContent::Blocked).html;
        assert_eq!(html, input);
        assert_eq!(sanitize_html(&html, RemoteContent::Blocked).html, html);
    }

    #[test]
    fn split_history_keeps_inherited_sender_presentation() {
        let s = sanitize_html(
            r#"<div style="color:red;font-size:12pt"><p>new</p><blockquote type="cite"><p>old</p></blockquote></div>"#,
            RemoteContent::Blocked,
        );
        assert!(s.html.contains("color:red;font-size:12pt"));
        assert!(s
            .quoted
            .as_deref()
            .unwrap()
            .contains("color:red;font-size:12pt"));
    }

    #[test]
    fn strips_scripts_and_handlers() {
        let s = sanitize_html(
            r#"<p onclick="x()">Hi<script>alert(1)</script></p><iframe src="https://evil"></iframe><a href="javascript:alert(1)">x</a>"#,
            RemoteContent::Blocked,
        );
        assert!(!s.html.contains("script"));
        assert!(!s.html.contains("onclick"));
        assert!(!s.html.contains("iframe"));
        assert!(!s.html.contains("javascript:"));
        assert!(s.html.contains("<p>Hi</p>"));
    }

    #[test]
    fn blocks_remote_images_by_default() {
        let html = r#"<img src="https://tracker.example/pixel.gif"><img src="cid:logo">"#;
        let blocked = sanitize_html(html, RemoteContent::Blocked);
        assert_eq!(blocked.blocked_remote, 1);
        assert!(!blocked.html.contains("tracker.example"));
        assert!(blocked.html.contains("cid:logo"));
        let allowed = sanitize_html(html, RemoteContent::Allowed);
        assert!(allowed.html.contains("tracker.example"));
    }

    #[test]
    fn text_extraction_keeps_structure() {
        let text = html_to_text(
            "<p>Hi&nbsp;Jason,</p><ul><li>One</li><li>Two &amp; three</li></ul><p>Bye</p>",
        );
        assert_eq!(text, "Hi Jason,\n\n• One\n• Two & three\n\nBye");
    }

    #[test]
    fn tables_become_rows_with_gaps() {
        // A header row makes it a data table, kept for the view's grid layout.
        let s = sanitize_html(
            "<table><tr><th>Area</th><th>Done</th></tr><tr><td>Design</td><td>5</td></tr></table>",
            RemoteContent::Blocked,
        );
        assert!(
            s.html.starts_with("<table>") && s.html.contains("<th>Area</th>"),
            "{}",
            s.html
        );
        // Layout tables (nested, long prose) become block flow with gaps between cells.
        let nested = sanitize_html("<table><tr><td><p>Hello there, this is a long paragraph of newsletter copy that runs on well past the length of any data cell and keeps going.</p><table><tr><td><p>Another long paragraph of copy inside the nested layout table that is also far too long for a data cell.</p></td><td>b</td></tr></table></td></tr></table>", RemoteContent::Blocked);
        assert!(!nested.html.contains("<table"), "{}", nested.html);
        assert!(
            nested.html.contains("<p>Hello there")
                && nested.html.contains("&nbsp;&nbsp;&nbsp;&nbsp;<div>b"),
            "{}",
            nested.html
        );
    }

    #[test]
    fn text_colour_survives_and_adapts() {
        let s = sanitize_html(
            r##"<p>Topic <span style="color:red; font-size:11pt; mso-fareast-language:EN-US">careful</span> and <font color="#1F497D" face="Calibri">navy</font></p>"##,
            RemoteContent::Blocked,
        );
        assert!(
            s.html
                .contains(r#"<span style="color:red;font-size:11pt">careful</span>"#),
            "{}",
            s.html
        );
        assert!(
            s.html
                .contains(r##"<font color="#1F497D" face="Calibri">navy</font>"##),
            "{}",
            s.html
        );
        let dark = adapt_colors(&s.html, true);
        assert!(
            dark.contains("color:#ff0000") || dark.contains("color:red"),
            "{dark}"
        );
        assert!(
            !dark.contains("#1F497D") && !dark.contains("#1f497d"),
            "{dark}"
        );
        let light = adapt_colors(r##"<span style="color:#ffffff">x</span>"##, false);
        assert!(!light.contains("#ffffff"), "{light}");
    }

    #[test]
    fn data_tables_are_kept_and_layout_tables_flattened() {
        let invoice = r#"<table border="1"><thead><tr><th>Item</th><th>Qty</th></tr></thead><tbody><tr><td>Lamp</td><td>2</td></tr><tr><td>Cable</td><td>1</td></tr></tbody></table>"#;
        let s = sanitize_html(invoice, RemoteContent::Blocked);
        assert!(
            s.html.contains("<table") && s.html.contains("<th>Item</th>"),
            "{}",
            s.html
        );
        let layout = r#"<table width="600"><tr><td><p>A long paragraph of newsletter copy that goes on and on and on and on and on and on, well past the length any data cell would ever have in a real table of figures.</p><table><tr><td>inner</td></tr></table></td></tr></table>"#;
        let s = sanitize_html(layout, RemoteContent::Blocked);
        assert!(!s.html.contains("<table"), "{}", s.html);
        assert!(s.html.contains("inner"));
    }

    #[test]
    fn breaks_are_kept_for_the_view() {
        // The HTML view (vendored gpui-base) renders <br> as a hard line break, so the
        // sanitizer leaves it alone; the text extraction keeps it as a newline.
        let s = sanitize_html(
            "<p>Thanks,<br>Maya<br><small>Design Director</small></p><div>a<br>b</div>",
            RemoteContent::Blocked,
        );
        assert_eq!(
            s.html,
            "<p>Thanks,<br>Maya<br><small>Design Director</small></p><div>a<br>b</div>"
        );
        assert_eq!(s.text, "Thanks,\nMaya\nDesign Director\n\na\nb");
    }

    #[test]
    fn gmail_div_bodies_keep_their_lines() {
        // Gmail composes mail as nested divs with <br> between lines; every break survives.
        let s = sanitize_html("<div dir=\"ltr\"><div dir=\"auto\">Hey,<br><br>Step 1<br>- one<br>- two<br><br>Bye</div></div>", RemoteContent::Blocked);
        assert_eq!(s.html, "<div dir=\"ltr\"><div dir=\"auto\">Hey,<br><br>Step 1<br>- one<br>- two<br><br>Bye</div></div>");
    }

    #[test]
    fn plain_text_preserves_blank_runs_indentation_and_terminal_breaks() {
        let html = text_to_html("\n  Café\t東京\n\n\nlast\n");
        assert_eq!(
            html,
            "<div style=\"white-space:pre-wrap\"><br>  Café\t東京<br><br><br>last<br></div>"
        );
        let clean = sanitize_html(&html, RemoteContent::Blocked);
        assert_eq!(clean.html.matches("<br>").count(), 5);
        assert!(clean.html.contains("  Café\t東京"));
        assert_eq!(
            text_to_html("one\r\ntwo"),
            "<div style=\"white-space:pre-wrap\">one<br>two</div>"
        );
    }

    #[test]
    fn text_round_trips_to_html() {
        let html = text_to_html("Hello <you>\nsecond line\n\n> quoted one\n> quoted two\n>\n> > nested\n\nAfter https://ex.com/a.");
        assert_eq!(
            html,
            "<div style=\"white-space:pre-wrap\">Hello &lt;you&gt;<br>second line<br><blockquote>quoted one<br>quoted two<br><blockquote>nested</blockquote></blockquote><br>After <a href=\"https://ex.com/a\">https://ex.com/a</a>.</div>"
        );
    }

    #[test]
    fn quote_depth_and_deep_html_are_bounded() {
        let deep_quotes = format!("{} hello", ">".repeat(500));
        let html = text_to_html(&deep_quotes);
        assert_eq!(html.matches("<blockquote>").count(), MAX_QUOTE_DEPTH);
        assert_eq!(html.matches("</blockquote>").count(), MAX_QUOTE_DEPTH);
        let sanitized = sanitize_html(&html, RemoteContent::Blocked);
        assert!(sanitized.text.contains("hello"));

        let deep_html = format!("{}payload{}", "<div>".repeat(1000), "</div>".repeat(1000));
        assert!(sanitize_html(&deep_html, RemoteContent::Blocked).html.is_empty());
        assert!(clean_only(&deep_html, RemoteContent::Blocked).is_empty());
        assert!(html_to_text(&deep_html).is_empty());
        assert!(flatten_tables(&deep_html).is_empty());
        assert!(adapt_colors(&deep_html, true).is_empty());
    }
}
