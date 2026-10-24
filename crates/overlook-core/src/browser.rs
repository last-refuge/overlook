//! Browser-only document preparation, independent of the native renderer.
//!
//! SECURITY: the embedding engine MUST disable scripts, intercept and consent-gate
//! ALL external requests (including CSS images/fonts), and isolate navigation,
//! downloads, popups and local resources. CSP is defense in depth, not consent or
//! a network sandbox. This function does not fetch resources or change stored mail.

use std::collections::{HashMap, HashSet};

use cssparser::{Parser, Token};
use html5ever::serialize::{serialize, SerializeOpts, TraversalScope};
use html5ever::tendril::TendrilSink;
use html5ever::{namespace_url, ns, parse_document, LocalName, ParseOpts, QualName};
use markup5ever_rcdom::{Handle, NodeData, RcDom, SerializableHandle};

/// An isolated browser document, not HTML for the native presentation pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserDocument {
    pub html: String,
    /// Number of external resource references in the prepared DOM (not unique
    /// URLs or requests). CSS `url()` and image `srcset` candidates are included;
    /// this is advisory, never a substitute for engine request interception.
    pub remote_resources: usize,
}

/// Maximum authored HTML size accepted for browser preparation (in bytes).
/// The original stored message is never modified by preparation.
pub const MAX_SOURCE_BYTES: usize = 4 * 1024 * 1024;
const MAX_TAG_STARTS: usize = 50_000;
const MAX_DOM_DEPTH: usize = 128;
const MAX_DOM_NODES: usize = 100_000;
const MAX_CSS_DEPTH: usize = 32;

const CSP: &str = "default-src 'none'; script-src 'none'; style-src 'unsafe-inline'; img-src data: https: http:; font-src data: https: http:; connect-src 'none'; frame-src 'none'; object-src 'none'; form-action 'none'; base-uri 'none'";

/// Prepare untrusted authored HTML without flattening layout or typography.
///
/// Full layout CSS is retained, with CSS imports and unsafe URL schemes removed.
/// The caller MUST enforce the isolation contract described in this module.
/// Relative resource URLs are rejected: a mail document has no trusted base URL.
/// Oversized or excessively complex documents become a fixed inert error page;
/// excessively nested CSS is discarded without affecting other styles.
pub fn prepare_document(source: &str) -> BrowserDocument {
    prepare_document_with_defaults(source, "body { background: white; color: black; }")
}

/// Apply display-only RGB defaults at zero CSS specificity so sender styles and
/// legacy body color attributes win. These colors never enter stored/sent mail.
pub fn prepare_document_with_colors(
    source: &str,
    background: u32,
    foreground: u32,
) -> BrowserDocument {
    let defaults = format!(
        ":where(body:not([bgcolor]):not([background])) {{ background-color: #{:06X}; }} :where(body:not([text])) {{ color: #{:06X}; }}",
        background & 0xFFFFFF, foreground & 0xFFFFFF,
    );
    prepare_document_with_defaults(source, &defaults)
}

fn prepare_document_with_defaults(source: &str, defaults: &str) -> BrowserDocument {
    // Count every '<', including comments/text, as a conservative allocation
    // bound before invoking the HTML parser. Do not copy or echo rejected input.
    if source.len() > MAX_SOURCE_BYTES
        || source
            .bytes()
            .filter(|&b| b == b'<')
            .take(MAX_TAG_STARTS + 1)
            .count()
            > MAX_TAG_STARTS
    {
        return preparation_error(defaults);
    }
    let dom = parse_document(RcDom::default(), ParseOpts::default()).one(source);
    if !dom_within_limits(&dom.document) {
        return preparation_error(defaults);
    }
    let mut styles = String::new();
    prepare_nodes(&dom.document, false, &mut styles);
    // Only the parser's actual document roots may supply root attributes. No
    // sender-controlled marker or nested element can impersonate these nodes.
    let html_root = element_child(&dom.document, "html").expect("HTML parser creates html");
    let body_root = element_child(&html_root, "body").expect("HTML parser creates body");
    let fragment = body_root
        .children
        .borrow()
        .iter()
        .map(serialize_node)
        .collect::<String>();
    let mut cleaner = ammonia::Builder::default();
    cleaner
        .tags(words("a abbr address article aside b bdi bdo blockquote br button caption center cite code col colgroup dd del details div dl dt em figcaption figure font footer h1 h2 h3 h4 h5 h6 header hr i img ins kbd li main mark nav ol p pre q s samp section small span strong sub summary sup table tbody td tfoot th thead time tr tt u ul var wbr style"))
        .clean_content_tags(words("script iframe object embed form input textarea select option meta base link svg math template noscript head title"))
        .generic_attributes(words("class id style dir lang title"))
        .tag_attributes(HashMap::from([
            ("div", words("bgcolor text background")),
            ("a", words("href name")),
            ("img", words("src srcset alt width height")),
            ("table", words("width height border cellpadding cellspacing align bgcolor background")),
            ("td", words("width height colspan rowspan align valign bgcolor background")),
            ("th", words("width height colspan rowspan align valign bgcolor background scope")),
            ("tr", words("align valign bgcolor background")),
            ("col", words("width span align valign")),
            ("colgroup", words("width span align valign")),
            ("font", words("color face size")),
            ("ol", words("start type reversed")),
            ("li", words("value")),
            ("button", words("type disabled")),
        ]))
        .url_schemes(words("http https data cid mailto tel"))
        .url_relative(ammonia::UrlRelative::Deny)
        .link_rel(None)
        .attribute_filter(|tag, attr, value| {
            if matches!(attr, "src" | "background") && !resource_url(value) {
                None
            } else if tag == "a" && attr == "href" && !navigation_url(value) {
                None
            } else {
                Some(value.into())
            }
        });
    let body = cleaner.clean(&fragment).to_string();
    // Head styles are processed separately so no sender head element can precede
    // or replace the policy. Use the same allowlist for their serialization.
    let styles = cleaner.clean(&styles).to_string();
    let html_attrs = clean_root_attributes(&html_root, &cleaner, false);
    let body_attrs = clean_root_attributes(&body_root, &cleaner, true);
    let html = format!("<!doctype html><html{html_attrs}><head><meta http-equiv=\"Content-Security-Policy\" content=\"{CSP}\"><style>body {{ margin: 0; font-family: Arial, sans-serif; font-size: 16px; }} {defaults}</style>{styles}</head><body{body_attrs}>{body}</body></html>");
    let counted = parse_document(RcDom::default(), ParseOpts::default()).one(html.as_str());
    if !dom_within_limits(&counted.document) {
        return preparation_error(defaults);
    }
    let remote_resources = count_resources(&counted.document);
    BrowserDocument {
        html,
        remote_resources,
    }
}

fn preparation_error(defaults: &str) -> BrowserDocument {
    BrowserDocument {
        html: format!("<!doctype html><html><head><meta http-equiv=\"Content-Security-Policy\" content=\"{CSP}\"><style>{defaults}</style></head><body><p>Message too large or complex to display safely.</p></body></html>"),
        remote_resources: 0,
    }
}

// Validate before any recursive preparation, serialization or resource counting.
// RcDom's Node::drop is iterative, so rejecting a deep tree is also safe.
fn dom_within_limits(root: &Handle) -> bool {
    let mut pending = vec![(root.clone(), 0)];
    let mut nodes = 1;
    while let Some((node, depth)) = pending.pop() {
        if depth > MAX_DOM_DEPTH {
            return false;
        }
        let children = node.children.borrow();
        let template = match &node.data {
            NodeData::Element {
                template_contents, ..
            } => template_contents.borrow().clone(),
            _ => None,
        };
        nodes += children.len() + usize::from(template.is_some());
        if nodes > MAX_DOM_NODES {
            return false;
        }
        pending.extend(children.iter().map(|child| (child.clone(), depth + 1)));
        if let Some(template) = template {
            pending.push((template, depth + 1));
        }
    }
    true
}

fn element_child(node: &Handle, tag: &str) -> Option<Handle> {
    node.children
        .borrow()
        .iter()
        .find(|child| {
            matches!(&child.data, NodeData::Element { name, .. }
            if name.ns == ns!(html) && name.local.as_ref() == tag)
        })
        .cloned()
}

fn clean_root_attributes(root: &Handle, cleaner: &ammonia::Builder<'_>, body: bool) -> String {
    let NodeData::Element { attrs, .. } = &root.data else {
        unreachable!("document root is an element");
    };
    // Sanitize an empty surrogate with the same allowlist as document content.
    // Its tag and empty contents are generated here, never copied from input.
    let surrogate = markup5ever_rcdom::Node::new(NodeData::Element {
        name: QualName::new(None, ns!(html), LocalName::from("div")),
        attrs: std::cell::RefCell::new(
            attrs
                .borrow()
                .iter()
                .filter(|attr| {
                    attr.name.ns == ns!()
                        && (matches!(
                            attr.name.local.as_ref(),
                            "class" | "id" | "style" | "lang" | "dir"
                        ) || (body
                            && matches!(
                                attr.name.local.as_ref(),
                                "bgcolor" | "text" | "background"
                            )))
                })
                .cloned()
                .collect(),
        ),
        template_contents: Default::default(),
        mathml_annotation_xml_integration_point: false,
    });
    let cleaned = cleaner.clean(&serialize_node(&surrogate)).to_string();
    cleaned
        .strip_prefix("<div")
        .and_then(|s| s.strip_suffix("></div>"))
        .expect("sanitized empty surrogate remains a div")
        .to_owned()
}

fn words(s: &'static str) -> HashSet<&'static str> {
    s.split_whitespace().collect()
}

fn remote_url(url: &str) -> bool {
    let url = url.trim().to_ascii_lowercase();
    url.starts_with("https://") || url.starts_with("http://") || url.starts_with("//")
}

fn resource_url(url: &str) -> bool {
    let url = url.trim().to_ascii_lowercase();
    !url.chars().any(char::is_control)
        && (remote_url(&url)
            || url.starts_with("data:")
            || url.starts_with("cid:")
            || url.starts_with('#'))
}

// Mail documents have no base URL. Only network-path references gain a scheme;
// ordinary relative paths remain subject to the existing deny policy.
fn normalize_protocol_relative(url: &str) -> std::borrow::Cow<'_, str> {
    if url.trim().starts_with("//") {
        format!("https:{}", url.trim()).into()
    } else {
        url.into()
    }
}

fn navigation_url(url: &str) -> bool {
    let url = url.trim().to_ascii_lowercase();
    !url.chars().any(char::is_control)
        && (remote_url(&url)
            || url.starts_with("mailto:")
            || url.starts_with("tel:")
            || url.starts_with('#'))
}

fn serialize_node(node: &Handle) -> String {
    let mut bytes = Vec::new();
    serialize(
        &mut bytes,
        &SerializableHandle::from(node.clone()),
        SerializeOpts {
            traversal_scope: if matches!(node.data, NodeData::Document) {
                TraversalScope::ChildrenOnly(None)
            } else {
                TraversalScope::IncludeNode
            },
            ..SerializeOpts::default()
        },
    )
    .expect("serializing HTML into memory");
    String::from_utf8(bytes).expect("HTML serializer emits UTF-8")
}

fn prepare_nodes(node: &Handle, in_head: bool, styles: &mut String) {
    let mut head = in_head;
    if let NodeData::Element { name, attrs, .. } = &node.data {
        head |= name.local.as_ref() == "head";
        for attr in attrs.borrow_mut().iter_mut() {
            match attr.name.local.as_ref() {
                "style" => attr.value = clean_css(&attr.value, &mut 0).into(),
                "srcset" => attr.value = clean_srcset(&attr.value).into(),
                "src" | "background" | "href" => {
                    attr.value = normalize_protocol_relative(&attr.value).as_ref().into();
                }
                _ => {}
            }
        }
        if name.local.as_ref() == "style" {
            for child in node.children.borrow().iter() {
                if let NodeData::Text { contents } = &child.data {
                    let css = clean_css(&contents.borrow(), &mut 0).replace('<', "\\3c ");
                    *contents.borrow_mut() = css.into();
                }
            }
            if head {
                styles.push_str(&serialize_node(node));
            }
        }
    }
    let mut children = node.children.borrow_mut();
    for child in children.iter_mut() {
        prepare_nodes(child, head, styles);
    }
    // Do this before Ammonia reparses the fragment: HTML fragment parsing can
    // relocate head styles into the body before its tag filter sees the head.
    children.retain(|child| {
        !matches!(&child.data,
        NodeData::Element { name, .. } if name.local.as_ref() == "head")
    });
}

// Insert the scheme into the authored URL token, retaining quotes, CSS escapes,
// function spelling and whitespace. Leading whitespace *inside* a quoted URL
// needs canonical serialization so it cannot precede the newly added scheme.
fn normalize_css_url(raw: &str, url: &str) -> String {
    if !url.trim().starts_with("//") {
        return raw.to_owned();
    }
    if !url.starts_with("//") {
        let mut quoted = String::new();
        cssparser::serialize_string(&normalize_protocol_relative(url), &mut quoted)
            .expect("serializing CSS into memory");
        return if raw.starts_with(['\'', '"']) {
            quoted
        } else {
            format!("url({quoted})")
        };
    }
    let offset = if raw.starts_with(['\'', '"']) {
        1
    } else {
        let opening = raw
            .find('(')
            .expect("URL token contains opening parenthesis")
            + 1;
        opening + raw[opening..].len() - raw[opening..].trim_start().len()
    };
    format!("{}https:{}", &raw[..offset], &raw[offset..])
}

// Tokenize CSS rather than regex-matching: CSS escapes and nested blocks must
// not conceal URL schemes. Leave ordinary tokens byte-for-byte unchanged.
fn clean_css(css: &str, count: &mut usize) -> String {
    let original_count = *count;
    let mut parser = Parser::new(css);
    parser.set_nested_block_limit(MAX_CSS_DEPTH as u8);
    match css_tokens(&mut parser, count, false, 0) {
        Some(css) => css,
        None => {
            *count = original_count;
            String::new()
        }
    }
}

fn css_tokens(
    parser: &mut Parser<'_>,
    count: &mut usize,
    image_set: bool,
    depth: usize,
) -> Option<String> {
    // Reject the entire stylesheet/attribute, not a partially rewritten rule.
    // cssparser skips unconsumed nested blocks iteratively on return, including
    // blocks inside rejected url()/expression() and @import constructs.
    if depth >= MAX_CSS_DEPTH {
        return None;
    }
    let mut out = String::new();
    let mut import = false;
    loop {
        let start = parser.position();
        let Ok(token) = parser.next_including_whitespace_and_comments().cloned() else {
            break;
        };
        let raw = parser.slice_from(start).to_owned();
        if matches!(&token, Token::AtKeyword(name) if name.eq_ignore_ascii_case("import")) {
            import = true;
            continue;
        }
        let rendered = match &token {
            Token::QuotedString(url) if image_set => {
                if resource_url(url) {
                    *count += usize::from(remote_url(url));
                    normalize_css_url(&raw, url)
                } else {
                    "\"\"".into()
                }
            }
            Token::UnquotedUrl(url) => {
                if resource_url(&url) {
                    *count += usize::from(remote_url(&url));
                    normalize_css_url(&raw, url)
                } else {
                    "url(\"\")".into()
                }
            }
            Token::Function(ref name) if name.eq_ignore_ascii_case("url") => {
                let url = parser
                    .parse_nested_block(|p| loop {
                        let token_start = p.position();
                        let token = p.next_including_whitespace_and_comments()?.clone();
                        match token {
                            Token::WhiteSpace(_) | Token::Comment(_) => continue,
                            Token::QuotedString(value) => {
                                let token_end = p.position();
                                p.expect_exhausted()?;
                                break Ok::<_, cssparser::ParseError<()>>((
                                    value.to_string(),
                                    token_start,
                                    token_end,
                                ));
                            }
                            _ => break Err(cssparser::ParseError::<()>::custom(())),
                        }
                    })
                    .ok();
                match url {
                    Some((url, token_start, token_end)) if resource_url(&url) => {
                        *count += usize::from(remote_url(&url));
                        format!(
                            "{}{}{}",
                            parser.slice(start..token_start),
                            normalize_css_url(parser.slice(token_start..token_end), &url),
                            parser.slice_from(token_end),
                        )
                    }
                    _ => "url(\"\")".into(),
                }
            }
            Token::Function(name) if name.eq_ignore_ascii_case("expression") => {
                let _ = parser.parse_nested_block(|p| {
                    while p.next_including_whitespace_and_comments().is_ok() {}
                    Ok::<_, cssparser::ParseError<()>>(())
                });
                String::new()
            }
            Token::Function(_)
            | Token::ParenthesisBlock
            | Token::SquareBracketBlock
            | Token::CurlyBracketBlock => {
                let nested_image_set = matches!(&token, Token::Function(name)
                    if name.eq_ignore_ascii_case("image-set") || name.eq_ignore_ascii_case("-webkit-image-set"));
                let closing = match token {
                    Token::SquareBracketBlock => "]",
                    Token::CurlyBracketBlock => "}",
                    _ => ")",
                };
                let inner = parser
                    .parse_nested_block(|p| {
                        css_tokens(p, count, nested_image_set, depth + 1)
                            .ok_or_else(|| cssparser::ParseError::<()>::custom(()))
                    })
                    .ok()?;
                format!("{raw}{inner}{closing}")
            }
            Token::BadUrl(_) | Token::BadString(_) => String::new(),
            _ => raw,
        };
        if !import {
            out.push_str(&rendered);
        }
        if matches!(token, Token::Semicolon | Token::CurlyBracketBlock) {
            import = false;
        }
    }
    Some(out)
}

// HTML srcset URLs end at whitespace; trailing commas delimit candidates.
// In particular, a comma inside a data URL is not a candidate separator.
fn srcset_candidates(value: &str) -> Vec<(&str, &str)> {
    let mut rest = value;
    let mut result = Vec::new();
    while !rest.is_empty() {
        rest = rest.trim_start_matches(|c: char| c.is_ascii_whitespace() || c == ',');
        if rest.is_empty() {
            break;
        }
        let end = rest
            .find(|c: char| c.is_ascii_whitespace())
            .unwrap_or(rest.len());
        let token = &rest[..end];
        rest = &rest[end..];
        if token.ends_with(',') {
            result.push((token.trim_end_matches(','), ""));
        } else {
            let end = rest.find(',').unwrap_or(rest.len());
            result.push((token, rest[..end].trim()));
            rest = rest[end..].trim_start_matches(',');
        }
    }
    result
}

fn clean_srcset(value: &str) -> String {
    srcset_candidates(value)
        .into_iter()
        .filter(|(url, _)| resource_url(url))
        .map(|(url, descriptor)| {
            format!("{} {descriptor}", normalize_protocol_relative(url))
                .trim()
                .to_owned()
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn count_resources(node: &Handle) -> usize {
    let mut count = 0;
    if let NodeData::Element { name, attrs, .. } = &node.data {
        for attr in attrs.borrow().iter() {
            match attr.name.local.as_ref() {
                "src" | "background" => count += usize::from(remote_url(&attr.value)),
                "srcset" => {
                    count += srcset_candidates(&attr.value)
                        .iter()
                        .filter(|(url, _)| remote_url(url))
                        .count()
                }
                "style" => {
                    clean_css(&attr.value, &mut count);
                }
                _ => {}
            }
        }
        if name.local.as_ref() == "style" {
            for child in node.children.borrow().iter() {
                if let NodeData::Text { contents } = &child.data {
                    clean_css(&contents.borrow(), &mut count);
                }
            }
        }
    }
    count
        + node
            .children
            .borrow()
            .iter()
            .map(count_resources)
            .sum::<usize>()
}
