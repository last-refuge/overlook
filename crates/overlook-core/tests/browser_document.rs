//! Synthetic browser fixtures only; no real mail or store access.
use overlook_core::browser::MAX_SOURCE_BYTES;
use overlook_core::{prepare_document, BrowserDocument};

fn assert_inert_error(prepared: &BrowserDocument) {
    assert!(prepared.html.len() < 1024);
    assert!(prepared
        .html
        .starts_with("<!doctype html><html><head><meta http-equiv=\"Content-Security-Policy\""));
    assert!(prepared
        .html
        .contains("default-src 'none'; script-src 'none'"));
    assert!(prepared
        .html
        .contains("Message too large or complex to display safely."));
    assert!(!prepared.html.contains("sender-marker"));
    assert_eq!(prepared.remote_resources, 0);
}

#[test]
fn oversized_source_is_rejected_without_echoing_or_changing_it() {
    let source = format!("sender-marker{}", "x".repeat(MAX_SOURCE_BYTES));
    let original = source.clone();
    assert_inert_error(&prepare_document(&source));
    assert_eq!(
        source, original,
        "preparation must not rewrite original HTML"
    );
}

#[test]
fn excessive_tag_starts_are_rejected_before_dom_preparation() {
    let source = format!("sender-marker{}", "<br>".repeat(50_001));
    assert!(source.len() < MAX_SOURCE_BYTES);
    assert_inert_error(&prepare_document(&source));
}

#[test]
fn deeply_nested_dom_is_rejected_without_recursive_traversal() {
    for tag in ["div", "template"] {
        let source = format!(
            "{}sender-marker{}",
            format!("<{tag}>").repeat(1000),
            format!("</{tag}>").repeat(1000)
        );
        assert_inert_error(&prepare_document(&source));
    }
}

#[test]
fn deeply_nested_css_is_dropped_without_affecting_regular_styles() {
    let deep = format!("{}1{}", "f(".repeat(1000), ")".repeat(1000));
    // Exercise ordinary nested parsing and the special discarded-block paths.
    for css in [
        format!(".sender-marker {{ width: {deep}; }}"),
        format!("@import {deep};"),
        format!(".sender-marker {{ width: expression({deep}); }}"),
        format!(".sender-marker {{ background: url({deep}); }}"),
    ] {
        let source = format!(
            "<style>{css}</style><style>.safe {{ color: navy; }}</style><div style=\"width:{deep}\">Safe</div>"
        );
        let prepared = prepare_document(&source);
        assert!(prepared.html.len() < 2048);
        assert!(!prepared.html.contains("f(f("));
        assert!(!prepared.html.contains("expression("));
        assert!(prepared.html.contains(".safe { color: navy; }"));
        assert!(prepared.html.contains(">Safe</div>"));
        assert!(prepared.html.contains("Content-Security-Policy"));
        assert_eq!(prepared.remote_resources, 0);
    }
}

#[test]
fn actual_document_roots_keep_authored_attributes_without_wrappers() {
    let prepared = prepare_document(
        r##"<html id="root" style="font-size:20px" lang="en"><head><style>html#root { color: navy; } body.some-class#message { padding: 2rem; font-family: Georgia; background: pink; }</style></head><body class="some-class" id="message" style="font-size:18px" dir="rtl" bgcolor="#eeeeee" text="#123456" background="https://example.test/bg"><p>Root geometry</p></body></html>"##,
    );
    let html = &prepared.html;
    assert!(html.starts_with("<!doctype html><html id=\"root\" style=\"font-size:20px\" lang=\"en\"><head><meta http-equiv=\"Content-Security-Policy\""), "{html}");
    assert!(html.contains("<body class=\"some-class\" id=\"message\" style=\"font-size:18px\" dir=\"rtl\" bgcolor=\"#eeeeee\" text=\"#123456\" background=\"https://example.test/bg\"><p>Root geometry</p></body>"), "{html}");
    assert!(html.contains("html#root { color: navy; } body.some-class#message { padding: 2rem; font-family: Georgia; background: pink; }"));
    assert!(
        html.find("font-family: Arial, sans-serif; font-size: 16px")
            .unwrap()
            < html.find("html#root").unwrap()
    );
    assert_eq!(html.matches("<html").count(), 1);
    assert_eq!(html.matches("<body").count(), 1);
    assert!(!html.contains("<div"));
    assert_eq!(prepared.remote_resources, 1, "body background is counted");
}

#[test]
fn malicious_root_attributes_and_spoofed_wrappers_cannot_replace_policy() {
    let prepared = prepare_document(
        r#"<html class="root" onclick="root_attack()" data-browser-root="html" style="font-size:20px; background:url(javascript:root_attack)"><head><meta http-equiv="Content-Security-Policy" content="default-src *"></head><body id="message" onload="body_attack()" background="file:///tmp/private" style="padding:12px; background:url('f\69le:///tmp/private')"><div data-browser-root="html" data-browser-policy="true" id="spoof" style="font-size:99px"><meta http-equiv="Content-Security-Policy" content="default-src *">Safe</div></body></html>"#,
    );
    let html = &prepared.html;
    assert!(
        html.starts_with(
            "<!doctype html><html class=\"root\" style=\"font-size:20px; background:url("
        ),
        "{html}"
    );
    assert!(
        html.contains("<body id=\"message\" style=\"padding:12px; background:url("),
        "{html}"
    );
    assert!(html.contains("<head><meta http-equiv=\"Content-Security-Policy\""));
    assert_eq!(html.matches("<meta").count(), 1);
    for forbidden in [
        "onclick",
        "onload",
        "root_attack",
        "body_attack",
        "file:",
        "f\\69le",
        "data-browser",
        "default-src *",
    ] {
        assert!(!html.contains(forbidden), "retained {forbidden}: {html}");
    }
    assert!(html.contains("<div id=\"spoof\" style=\"font-size:99px\">Safe</div>"));
    assert_eq!(prepared.remote_resources, 0);
}

#[test]
fn cid_image_sources_are_normalized_and_not_remote_resources() {
    let prepared = prepare_document(
        "<IMG SRC='cid:part-one@example.test'><img src=cid:part-two><img srcset='cid:part-three 1x, cid:part-four 2x'>",
    );
    assert!(prepared
        .html
        .contains("<img src=\"cid:part-one@example.test\">"));
    assert!(prepared.html.contains("<img src=\"cid:part-two\">"));
    assert!(prepared
        .html
        .contains("srcset=\"cid:part-three 1x, cid:part-four 2x\""));
    assert_eq!(prepared.remote_resources, 0);
    assert!(prepared.html.contains("img-src data: https: http:;"));
    assert!(!prepared.html.contains("img-src cid:"));
}

#[test]
fn authored_card_layout_and_typography_survive() {
    let source = r##"<!doctype html><html style="background:#fafafa" class="root"><head>
<style>.card { border: 1px solid #ddd; width: 600px; } .notes { background: #eee; padding: 24px; } .cta { background: blue; border-radius: 8px; padding: 12px 20px; color: white; } h2 { font-weight: bold; }</style>
</head><body style="padding:32px; font-family:Georgia; font-size:18px" dir="rtl" lang="en">
<table id="card" class="card" width="600" cellpadding="0" cellspacing="0" border="1"><tr><td bgcolor="#eeeeee" width="560" style="padding:20px; border:1px solid gray">
<h2>Bold heading</h2><div class="notes">Padded gray notes</div><p>&nbsp;</p><br><p></p>
<a class="cta" href="https://example.test/go">Blue rounded CTA</a><button style="border-radius:8px; background:blue">Button</button>
</td></tr></table></body></html>"##;
    let BrowserDocument {
        html,
        remote_resources,
    } = prepare_document(source);
    for expected in [
        ".card { border: 1px solid #ddd; width: 600px; }",
        "background: #eee; padding: 24px",
        "border-radius: 8px",
        "font-weight: bold",
        "<table",
        "<tr>",
        "<td",
        "width=\"600\"",
        "bgcolor=\"#eeeeee\"",
        "cellpadding=\"0\"",
        "id=\"card\"",
        "class=\"card\"",
        "class=\"notes\"",
        "class=\"cta\"",
        "<h2>Bold heading</h2>",
        "<button",
        "<p></p>",
        "<br>",
        "font-family:Georgia",
        "font-size:18px",
        "dir=\"rtl\"",
        "lang=\"en\"",
        "background:#fafafa",
        "class=\"root\"",
    ] {
        assert!(html.contains(expected), "missing {expected}: {html}");
    }
    assert_eq!(remote_resources, 0, "navigation links are not resources");
    assert!(!html.contains("line-height"));
    assert!(html.contains("font-family: Arial, sans-serif; font-size: 16px"));
}

#[test]
fn policy_is_first_and_sender_active_content_is_removed() {
    let source = r#"<html><head><meta http-equiv="Content-Security-Policy" content="default-src *"><meta http-equiv="refresh" content="0;url=https://evil.test"><base href="file:///tmp/"><link rel="stylesheet" href="https://evil.test/style"><title>Discard title</title><script>head_attack()</script><style>.safe {padding:12px}</style></head><body onload="attack()"><script>body_attack()</script><iframe src="https://evil.test">frame payload</iframe><object data="file:///etc/passwd">object payload</object><form action="https://evil.test"><input autofocus onfocus="attack()"><button>submit payload</button></form><svg onload="attack()"></svg><a href="javascript:attack()" onclick="attack()">Safe text &amp; &lt;escaped&gt;</a><img src="file:///etc/passwd"><img src="javascript:attack()"><p style="background:url(f\69le:///tmp/x); padding:17px">Notes</p></body></html>"#;
    let prepared = prepare_document(source);
    let html = prepared.html;
    assert!(
        html.starts_with("<!doctype html><html><head><meta http-equiv=\"Content-Security-Policy\"")
    );
    assert_eq!(html.matches("<meta").count(), 1);
    for forbidden in [
        "head_attack",
        "body_attack",
        "attack()",
        "<iframe",
        "<object",
        "<form",
        "<input",
        "<svg",
        "<script",
        "<link",
        "<base",
        "refresh",
        "default-src *",
        "file:",
        "f\\69le",
        "frame payload",
        "submit payload",
        "Discard title",
        "onload",
        "onclick",
    ] {
        assert!(!html.contains(forbidden), "retained {forbidden}: {html}");
    }
    for directive in [
        "default-src 'none'",
        "script-src 'none'",
        "style-src 'unsafe-inline'",
        "img-src data: https: http:",
        "font-src data: https: http:",
        "connect-src 'none'",
        "frame-src 'none'",
        "object-src 'none'",
        "form-action 'none'",
        "base-uri 'none'",
    ] {
        assert!(html.contains(directive), "missing {directive}");
    }
    assert!(html.contains("<style>.safe {padding:12px}</style>"));
    assert!(html.contains("Safe text &amp; &lt;escaped&gt;"));
    assert!(html.contains("padding:17px"));
    assert_eq!(prepared.remote_resources, 0);
}

#[test]
fn counts_retained_dom_resources_not_links_or_removed_payloads() {
    let prepared = prepare_document(
        r#"<head><style>@import "https://example.test/external.css"; .panel { background: url(https://example.test/bg); } @font-face { font-family: Custom; src: url("https://example.test/font.woff2"); }</style></head><body><img src="https://example.test/a?x=1&amp;y=2" srcset="https://example.test/b 1x, https://example.test/c 2x, file:///tmp/x 3x"><table background="http://example.test/table"><tr><td style="background: u\72l('https://example.test/cell')">X</td></tr></table><a href="https://example.test/link">Link</a><iframe src="https://example.test/frame"></iframe><form><img src="https://example.test/form"></form><img src="data:image/png;base64,AAAA"></body>"#,
    );
    assert_eq!(prepared.remote_resources, 7, "{}", prepared.html);
    assert!(!prepared.html.contains("external.css"));
    assert!(!prepared.html.contains("file:"));
    assert!(prepared.html.contains("data:image/png;base64,AAAA"));
    assert!(prepared.html.contains("?x=1&amp;y=2"));
}

#[test]
fn css_tokenization_rejects_escaped_schemes_and_external_imports() {
    let source = r#"<style>@\69mport url(https://example.test/forbidden.css); .panel { padding: 11px; background: image-set("file:///tmp/x" 1x, "https://example.test/a" 2x); border-image: url('j\61vascript:alert(1)'); width: expression(alert(1)); } @media (max-width: 600px) { .panel { width: 100%; background: url("https://example.test/b"); } }</style><div style="background:url('f\69le:///etc/passwd'); display:grid; gap:12px">Grid</div>"#;
    let prepared = prepare_document(source);
    for forbidden in [
        "forbidden.css",
        "file:",
        "f\\69le",
        "j\\61vascript",
        "expression",
        "alert(1)",
    ] {
        assert!(
            !prepared.html.contains(forbidden),
            "{forbidden}: {}",
            prepared.html
        );
    }
    for retained in [
        "padding: 11px",
        "@media (max-width: 600px)",
        "width: 100%",
        "display:grid; gap:12px",
    ] {
        assert!(
            prepared.html.contains(retained),
            "{retained}: {}",
            prepared.html
        );
    }
    assert_eq!(prepared.remote_resources, 2);
}

#[test]
fn protocol_relative_image_source_is_retained_as_https() {
    let prepared = prepare_document(r#"<img src="//cdn.example/image.png">"#);
    assert!(prepared.remote_resources > 0, "{}", prepared.html);
    assert!(prepared
        .html
        .contains("src=\"https://cdn.example/image.png\""));
    assert_eq!(prepared.remote_resources, 1);
}

#[test]
fn protocol_relative_resources_and_navigation_are_normalized() {
    let prepared = prepare_document(
        r#"<head><style>.card { background: u\72l( //cdn.example/bg ); border-image: url( '//cdn.example/border' ); mask-image: image-set("//cdn.example/set" 1x, url(//cdn.example/set2) 2x); } .escaped { background: url(\2f\2f cdn.example/escaped); }</style></head><body background="//cdn.example/body"><table background="//cdn.example/table"><tr><td background="//cdn.example/cell">X</td></tr></table><img srcset="//cdn.example/small 320w, //cdn.example/large 640w, data:image/png;base64,AAAA 960w, cid:part 1280w"><div style="background: -webkit-image-set('//cdn.example/inline' 1x); padding:12px"></div><a href="//cdn.example/go">Go</a></body>"#,
    );
    for path in [
        "bg", "border", "set", "set2", "body", "table", "cell", "small", "large", "inline", "go",
    ] {
        assert!(
            prepared
                .html
                .contains(&format!("https://cdn.example/{path}")),
            "{path}: {}",
            prepared.html
        );
    }
    assert!(prepared.html.contains("https:\\2f\\2f cdn.example/escaped"));
    assert!(prepared.html.contains(
        "320w, https://cdn.example/large 640w, data:image/png;base64,AAAA 960w, cid:part 1280w"
    ));
    assert!(prepared.html.contains("padding:12px"));
    assert_eq!(prepared.remote_resources, 11, "{}", prepared.html);
}

#[test]
fn css_normalization_preserves_quotes_escapes_and_surrounding_syntax() {
    let css = r#".x { background: /* before */ u\72l( '//cdn.example/a' /* after */ ); mask: image-set("\2f\2f cdn.example/b" 1x, 'data:image/png;base64,AAAA' 2x, 'cid:part' 3x); border-image: url( //cdn.example/c ); color: navy; }"#;
    let prepared = prepare_document(&format!("<style>{css}</style>"));
    let expected = css
        .replace("//cdn.example/", "https://cdn.example/")
        .replace(r"\2f\2f cdn.example/", r"https:\2f\2f cdn.example/");
    assert!(
        prepared
            .html
            .contains(&format!("<style>{expected}</style>")),
        "{}",
        prepared.html
    );
    assert_eq!(prepared.remote_resources, 3);
}

#[test]
fn normalization_does_not_enable_relative_paths_or_change_absolute_urls() {
    let prepared = prepare_document(
        r#"<base href="https://cdn.example/"><img src="relative.png"><img src="/root.png"><img src="https://cdn.example/absolute"><img src="http://cdn.example/plain"><img src="data:image/png;base64,AAAA"><img src="cid:part"><img srcset="relative.png 1x, /root.png 2x, https://cdn.example/set 3x"><a href="/go">Go</a><style>.x { background: url(relative.png); mask: image-set('/root.png' 1x); border-image: url('https://cdn.example/border'); }</style>"#,
    );
    for forbidden in ["<base", "relative.png", "/root.png", "href=\"/go\""] {
        assert!(!prepared.html.contains(forbidden), "{}", prepared.html);
    }
    for retained in [
        "src=\"https://cdn.example/absolute\"",
        "src=\"http://cdn.example/plain\"",
        "src=\"data:image/png;base64,AAAA\"",
        "src=\"cid:part\"",
        "https://cdn.example/set 3x",
        "url('https://cdn.example/border')",
    ] {
        assert!(
            prepared.html.contains(retained),
            "{retained}: {}",
            prepared.html
        );
    }
    assert_eq!(prepared.remote_resources, 4);
}

#[test]
fn body_styles_cannot_escape_into_markup() {
    let prepared = prepare_document(
        r#"<div style="content: '&quot;&gt;&lt;script&gt;attack()&lt;/script&gt;'; padding: 9px" title="&quot;&lt;img src=x onerror=attack()&gt;">&lt;script&gt;literal&lt;/script&gt;</div><style>.x::before { content: '<img src=x onerror=attack()>'; }</style>"#,
    );
    assert!(!prepared.html.contains("<script>"));
    assert!(!prepared.html.contains("<img src=x"));
    assert!(prepared
        .html
        .contains("&lt;script&gt;literal&lt;/script&gt;"));
    assert!(prepared.html.contains("padding: 9px"));
}

#[test]
fn frameset_document_without_body_returns_inert_error_instead_of_panicking() {
    let prepared = prepare_document(
        "<html><frameset cols=\"50%,50%\"><frame src=\"https://example.test/a\"><frame src=\"https://example.test/b\"></frameset></html>",
    );
    assert_inert_error(&prepared);
}
