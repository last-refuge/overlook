//! Bounded sender presentation policy. Inline declarations only: stylesheets, selectors,
//! custom properties, CSS escapes/functions (except colors), and advanced layout are not supported.
//! Font sizes: 2–96px, 2–72pt, .25–6em/rem, 25–600%. Nonnegative margins:
//! at most 96px/72pt/6em/rem. Line heights: .5–6 or 2–144px/2–108pt/.5–6em/50–600%.
//! Font families are limited to eight ASCII names (48 bytes each, 200 bytes total).
use html5ever::serialize::{serialize, SerializeOpts, TraversalScope};
use markup5ever_rcdom::{Handle, NodeData, RcDom, SerializableHandle};

pub(super) fn document(html: &str) -> RcDom {
    crate::browser::parse_bounded_document(html).unwrap_or_default()
}

pub(crate) fn walk(node: &Handle, f: &mut impl FnMut(&Handle)) {
    let mut stack = vec![(node.clone(), 0usize)];
    while let Some((n, depth)) = stack.pop() {
        if depth > crate::browser::MAX_DOM_DEPTH {
            continue;
        }
        f(&n);
        let children = n.children.borrow();
        for child in children.iter().rev() {
            stack.push((child.clone(), depth + 1));
        }
    }
}

pub(crate) fn serialize_body(dom: &RcDom) -> String {
    let mut body = None;
    let mut roots = Vec::new();
    walk(&dom.document, &mut |n| {
        if let NodeData::Element { name, attrs, .. } = &n.data {
            if matches!(&*name.local, "html" | "body") {
                let mut wrapper = String::new();
                for a in attrs.borrow().iter() {
                    let value = match &*a.name.local {
                        "style" => safe_style(&a.value),
                        "dir" if matches!(&*a.value, "ltr" | "rtl" | "auto") => {
                            Some(a.value.to_string())
                        }
                        "lang"
                            if a.value.len() <= 64
                                && a.value
                                    .chars()
                                    .all(|c| c.is_ascii_alphanumeric() || c == '-') =>
                        {
                            Some(a.value.to_string())
                        }
                        _ => None,
                    };
                    if let Some(value) = value {
                        wrapper.push_str(&format!(
                            " {}=\"{}\"",
                            a.name.local,
                            crate::sanitize::escape_html(&value)
                        ));
                    }
                }
                if !wrapper.is_empty() {
                    roots.push(format!("<div{wrapper}>"));
                }
            }
            if &*name.local == "body" {
                body = Some(n.clone());
            }
        }
    });
    let mut buf = Vec::new();
    let opts = SerializeOpts {
        traversal_scope: TraversalScope::ChildrenOnly(None),
        ..Default::default()
    };
    let _ = serialize(
        &mut buf,
        &SerializableHandle::from(body.unwrap_or_else(|| dom.document.clone())),
        opts,
    );
    let content = String::from_utf8(buf).unwrap_or_default();
    format!(
        "{}{}{}",
        roots.join(""),
        content,
        "</div>".repeat(roots.len())
    )
}

/// Remove invisible/active subtrees without changing history, links, spacing or tables.
pub(super) fn visible_document(html: &str) -> RcDom {
    fn prune(root: &Handle) {
        let mut stack = vec![(root.clone(), 0usize)];
        while let Some((n, depth)) = stack.pop() {
            if depth >= crate::browser::MAX_DOM_DEPTH {
                n.children.borrow_mut().clear();
                continue;
            }
            n.children.borrow_mut().retain(|c| match &c.data {
                NodeData::Element { name, attrs, .. } => {
                    !matches!(
                        &*name.local,
                        "head"
                            | "script"
                            | "style"
                            | "title"
                            | "template"
                            | "noscript"
                            | "iframe"
                            | "object"
                            | "embed"
                            | "svg"
                            | "math"
                            | "audio"
                            | "video"
                    ) && !crate::prepass::is_hidden(&name.local, &attrs.borrow())
                }
                _ => true,
            });
            for c in n.children.borrow().iter() {
                stack.push((c.clone(), depth + 1));
            }
        }
    }
    let dom = document(html);
    prune(&dom.document);
    dom
}

fn length(value: &str, units: &[(&str, f32, f32)], allow_zero: bool) -> bool {
    if allow_zero && value == "0" {
        return true;
    }
    units.iter().any(|(unit, min, max)| {
        value.strip_suffix(unit).is_some_and(|n| {
            !n.is_empty()
                && n.chars().all(|c| c.is_ascii_digit() || c == '.')
                && n.parse::<f32>()
                    .is_ok_and(|n| n.is_finite() && n >= *min && n <= *max)
        })
    })
}

pub(super) fn font_family(value: &str) -> bool {
    value.len() <= 200
        && (1..=8).contains(&value.split(',').count())
        && value.split(',').all(|part| {
            let part = part.trim();
            let part = if part.len() >= 2
                && (part.starts_with('"') && part.ends_with('"')
                    || part.starts_with('\'') && part.ends_with('\''))
            {
                &part[1..part.len().saturating_sub(1)]
            } else {
                part
            };
            !part.is_empty()
                && part.len() <= 48
                && part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '-'))
        })
}

pub(super) fn color(value: &str) -> bool {
    let v = value.to_ascii_lowercase();
    if let Some(hex) = v.strip_prefix('#') {
        return matches!(hex.len(), 3 | 6) && hex.chars().all(|c| c.is_ascii_hexdigit());
    }
    if v == "windowtext" || cssparser::color::parse_named_color(&v).is_ok() {
        return true;
    }
    let (inner, alpha) =
        if let Some(inner) = v.strip_prefix("rgb(").and_then(|v| v.strip_suffix(')')) {
            (inner, false)
        } else if let Some(inner) = v.strip_prefix("rgba(").and_then(|v| v.strip_suffix(')')) {
            (inner, true)
        } else {
            return false;
        };
    let parts: Vec<_> = inner.split(',').collect();
    if parts.len() != if alpha { 4 } else { 3 } {
        return false;
    }
    if alpha {
        let a = parts[3].trim();
        if !a.chars().all(|c| c.is_ascii_digit() || c == '.')
            || !a.parse::<f32>().is_ok_and(|n| (0.0..=1.0).contains(&n))
        {
            return false;
        }
    }
    parts[..3].iter().all(|p| {
        let p = p.trim();
        let (n, max) = p.strip_suffix('%').map(|n| (n, 100.)).unwrap_or((p, 255.));
        !n.is_empty()
            && n.chars().all(|c| c.is_ascii_digit() || c == '.')
            && n.parse::<f32>()
                .is_ok_and(|n| n.is_finite() && n >= 0. && n <= max)
    })
}

/// Parse exactly the color forms admitted by the presentation policy. Keep alpha separate
/// so adapting foreground lightness never silently makes translucent text opaque.
pub(super) fn parse_color(value: &str) -> Option<(u8, u8, u8, Option<String>)> {
    let v = value.trim().to_ascii_lowercase();
    if !color(&v) {
        return None;
    }
    if v == "windowtext" {
        return Some((0, 0, 0, None));
    }
    if let Ok((r, g, b)) = cssparser::color::parse_named_color(&v) {
        return Some((r, g, b, None));
    }
    if v.starts_with('#') {
        let (r, g, b, _) = cssparser::color::parse_hash_color(v[1..].as_bytes()).ok()?;
        return Some((r, g, b, None));
    }
    let inner = v
        .strip_prefix("rgba(")
        .or_else(|| v.strip_prefix("rgb("))?
        .strip_suffix(')')?;
    let parts: Vec<_> = inner.split(',').map(str::trim).collect();
    let channel = |p: &str| -> Option<u8> {
        let (n, scale) = p
            .strip_suffix('%')
            .map(|n| (n, 255. / 100.))
            .unwrap_or((p, 1.));
        Some((n.parse::<f32>().ok()? * scale).round() as u8)
    };
    Some((
        channel(parts[0])?,
        channel(parts[1])?,
        channel(parts[2])?,
        parts.get(3).map(|a| a.to_string()),
    ))
}

/// Resolve inline priority before removing the priority annotation from safe output.
pub(crate) fn declaration_value(value: &str) -> (&str, bool) {
    if let Some((value, priority)) = value.rsplit_once('!') {
        if priority.trim().eq_ignore_ascii_case("important") {
            return (value.trim(), true);
        }
    }
    (value.trim(), false)
}

pub(super) fn safe_style(style: &str) -> Option<String> {
    let mut kept: Vec<(String, String, bool)> = Vec::new();
    for decl in style.split(';').take(128) {
        let Some((key, value)) = decl.split_once(':') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        let raw = value.trim();
        let lower = raw.to_ascii_lowercase();
        let (value, important) = declaration_value(&lower);
        let size = [
            ("px", 2., 96.),
            ("pt", 2., 72.),
            ("em", 0.25, 6.),
            ("rem", 0.25, 6.),
            ("%", 25., 600.),
        ];
        let spacing = [
            ("px", 0., 96.),
            ("pt", 0., 72.),
            ("em", 0., 6.),
            ("rem", 0., 6.),
        ];
        let valid = match key.as_str() {
            "color" => color(value),
            "font-size" => length(value, &size, false),
            "font-family" => font_family(value),
            "font-weight" => matches!(
                value,
                "normal"
                    | "bold"
                    | "100"
                    | "200"
                    | "300"
                    | "400"
                    | "500"
                    | "600"
                    | "700"
                    | "800"
                    | "900"
            ),
            "font-style" => matches!(value, "normal" | "italic" | "oblique"),
            "text-decoration" => matches!(value, "none" | "underline" | "line-through"),
            "text-align" => matches!(value, "left" | "right" | "center" | "justify"),
            "white-space" => matches!(value, "normal" | "pre" | "pre-wrap"),
            "line-height" => {
                value == "normal"
                    || length(
                        value,
                        &[
                            ("px", 2., 144.),
                            ("pt", 2., 108.),
                            ("em", 0.5, 6.),
                            ("%", 50., 600.),
                        ],
                        false,
                    )
                    || (value.chars().all(|c| c.is_ascii_digit() || c == '.')
                        && value.parse::<f32>().is_ok_and(|n| (0.5..=6.).contains(&n)))
            }
            "margin" => {
                (1..=4).contains(&value.split_whitespace().count())
                    && value.split_whitespace().all(|v| length(v, &spacing, true))
            }
            "margin-top" | "margin-bottom" | "margin-left" | "margin-right" => {
                length(value, &spacing, true)
            }
            _ => false,
        };
        if valid {
            // Expand spacing before resolving priority: shorthand and longhand
            // compete for the same components, not just identical property names.
            let declarations = if key == "margin" {
                let v: Vec<_> = value.split_whitespace().collect();
                let right = *v.get(1).unwrap_or(&v[0]);
                let bottom = *v.get(2).unwrap_or(&v[0]);
                let left = *v.get(3).unwrap_or(&right);
                vec![
                    ("margin-top".to_owned(), v[0]),
                    ("margin-right".to_owned(), right),
                    ("margin-bottom".to_owned(), bottom),
                    ("margin-left".to_owned(), left),
                ]
            } else {
                vec![(key, value)]
            };
            for (key, value) in declarations {
                if kept.iter().any(|(k, _, i)| k == &key && *i && !important) {
                    continue;
                }
                kept.retain(|(k, _, _)| k != &key);
                kept.push((key, value.to_owned(), important));
            }
        }
    }
    let margins: Option<Vec<_>> = ["margin-top", "margin-right", "margin-bottom", "margin-left"]
        .iter()
        .map(|key| {
            kept.iter()
                .find(|(k, _, _)| k.as_str() == *key)
                .map(|(_, value, _)| value.clone())
        })
        .collect();
    if let Some(v) = margins {
        let value = if v.iter().all(|s| s == &v[0]) {
            v[0].clone()
        } else if v[0] == v[2] && v[1] == v[3] {
            format!("{} {}", v[0], v[1])
        } else if v[1] == v[3] {
            format!("{} {} {}", v[0], v[1], v[2])
        } else {
            v.join(" ")
        };
        kept.retain(|(k, _, _)| !k.starts_with("margin-"));
        kept.push(("margin".to_owned(), value, false));
    }
    (!kept.is_empty()).then(|| {
        kept.into_iter()
            .map(|(k, v, _)| format!("{k}:{v}"))
            .collect::<Vec<_>>()
            .join(";")
    })
}
