//! HTML signatures: what the Settings editor and the composer do with them that is not drawing.
//!
//! A signature is HTML whose images are `cid:` references to images stored with it. Pasted
//! HTML often carries its images inline as `data:` URLs, which mail clients do not show, so
//! [`extract_data_images`] turns those into stored images. The editor's toolbar edits the
//! HTML source as text ([`wrap`], [`link`], [`bullet_list`], [`insert`]) and reports each
//! edit as a [`Replacement`], so the field can apply it without losing its undo history.

use crate::prefs::SignatureImage;
use base64::{
    alphabet,
    engine::{general_purpose::GeneralPurposeConfig, DecodePaddingMode, GeneralPurpose},
    Engine as _,
};
use std::ops::Range;

/// The largest image a signature may embed.
pub const MAX_IMAGE_BYTES: usize = 1024 * 1024;
/// How many images, and how many bytes of them in all, one signature may embed.
pub const MAX_IMAGES: usize = 20;
pub const MAX_TOTAL_IMAGE_BYTES: usize = 4 * 1024 * 1024;

/// Decodes padded or unpadded base64, which pasted HTML has both of.
const LENIENT: GeneralPurpose = GeneralPurpose::new(
    &alphabet::STANDARD,
    GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
);

/// A change to the source text: replace `range` of the old text with `text`, then select
/// `selection`, which is in the new text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Replacement {
    pub range: Range<usize>,
    pub text: String,
    pub selection: Range<usize>,
}

/// Snaps a selection reported in UTF-8 bytes to the text: inside it and on character
/// boundaries.
fn clamp(text: &str, selection: Range<usize>) -> Range<usize> {
    let mut start = selection.start.min(text.len());
    let mut end = selection.end.min(text.len()).max(start);
    let caret = start == end;
    while !text.is_char_boundary(start) {
        start -= 1;
    }
    // A caret stays a caret; a selection grows to take in the characters it cuts.
    while !caret && !text.is_char_boundary(end) {
        end += 1;
    }
    if caret {
        end = start;
    }
    start..end
}

/// Wraps the selection in `open` and `close`, keeping it selected. With nothing selected, the
/// pair is inserted with the caret between.
pub fn wrap(text: &str, selection: Range<usize>, open: &str, close: &str) -> Replacement {
    let sel = clamp(text, selection);
    let inner = &text[sel.clone()];
    let start = sel.start + open.len();
    Replacement { text: format!("{open}{inner}{close}"), selection: start..start + inner.len(), range: sel }
}

/// Replaces the selection with `snippet` and puts the caret after it.
pub fn insert(text: &str, selection: Range<usize>, snippet: &str) -> Replacement {
    let sel = clamp(text, selection);
    let caret = sel.start + snippet.len();
    Replacement { text: snippet.to_owned(), selection: caret..caret, range: sel }
}

fn looks_like_url(s: &str) -> bool {
    ["http://", "https://", "mailto:"].iter().any(|p| s.starts_with(p)) && !s.contains(|c: char| c.is_whitespace() || matches!(c, '"' | '<' | '>'))
}

/// Makes the selection a link. A selected address becomes the link's target; anything else
/// becomes its text, with the placeholder address selected so typing replaces it.
pub fn link(text: &str, selection: Range<usize>) -> Replacement {
    let sel = clamp(text, selection);
    let inner = &text[sel.clone()];
    if looks_like_url(inner.trim()) {
        return wrap(text, sel, &format!("<a href=\"{}\">", inner.trim()), "</a>");
    }
    const PLACEHOLDER: &str = "https://";
    let open = format!("<a href=\"{PLACEHOLDER}\">");
    let label = if inner.is_empty() { "link text" } else { inner };
    let start = sel.start + "<a href=\"".len();
    Replacement { text: format!("{open}{label}</a>"), selection: start..start + PLACEHOLDER.len(), range: sel }
}

/// Turns the lines the selection touches into a bulleted list, one item per non-empty line.
/// On an empty line it starts an empty list with the caret in the item.
pub fn bullet_list(text: &str, selection: Range<usize>) -> Replacement {
    let sel = clamp(text, selection);
    let start = text[..sel.start].rfind('\n').map_or(0, |i| i + 1);
    let end = text[sel.end..].find('\n').map_or(text.len(), |i| sel.end + i);
    let items: Vec<&str> = text[start..end].lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    if items.is_empty() {
        let caret = start + "<ul>\n<li>".len();
        return Replacement { range: start..end, text: "<ul>\n<li></li>\n</ul>".into(), selection: caret..caret };
    }
    let mut list = String::from("<ul>\n");
    for item in items {
        list.push_str(&format!("<li>{item}</li>\n"));
    }
    list.push_str("</ul>");
    let caret = start + list.len();
    Replacement { range: start..end, text: list, selection: caret..caret }
}

/// A `Content-ID` no other signature or message has.
pub fn new_content_id() -> String {
    format!("sig-{}@overlook", uuid::Uuid::new_v4().simple())
}

/// Whether a `Content-ID` was made by [`new_content_id`], that is, belongs to a signature's
/// image rather than to one a forwarded message brought along.
pub fn is_signature_content_id(content_id: &str) -> bool {
    content_id.starts_with("sig-") && content_id.ends_with("@overlook")
}

/// The kind of image a file is, from its first bytes rather than its name: the MIME type and
/// a file extension. Only formats mail clients show are accepted (not SVG).
pub fn image_type(bytes: &[u8]) -> Option<(&'static str, &'static str)> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(("image/png", "png"))
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some(("image/jpeg", "jpg"))
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some(("image/gif", "gif"))
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some(("image/webp", "webp"))
    } else {
        None
    }
}

pub const NOT_AN_IMAGE: &str = "That file is not a PNG, JPEG, GIF or WebP image.";

/// Why an image could not be added.
fn too_big() -> String {
    format!("Images in a signature can be at most {} KB each, {} MB and {} images in all.", MAX_IMAGE_BYTES / 1024, MAX_TOTAL_IMAGE_BYTES / (1024 * 1024), MAX_IMAGES)
}

/// Adds the image in `bytes` to `images` unless it is not an accepted image or would not fit,
/// and returns its `Content-ID`. An image that is already there is reused.
pub fn add_image(images: &mut Vec<SignatureImage>, filename: &str, bytes: &[u8]) -> Result<String, String> {
    let (mime, ext) = image_type(bytes).ok_or_else(|| NOT_AN_IMAGE.to_owned())?;
    let data = base64::engine::general_purpose::STANDARD.encode(bytes);
    if let Some(existing) = images.iter().find(|i| i.data == data) {
        return Ok(existing.content_id.clone());
    }
    let stored: usize = images.iter().map(|i| i.data.len() / 4 * 3).sum();
    if bytes.len() > MAX_IMAGE_BYTES || images.len() >= MAX_IMAGES || stored + bytes.len() > MAX_TOTAL_IMAGE_BYTES {
        return Err(too_big());
    }
    let stem = crate::attachment_policy::sanitize_filename(filename);
    let stem = stem.rsplit_once('.').map_or(stem.as_str(), |(s, _)| s);
    let content_id = new_content_id();
    images.push(SignatureImage { content_id: content_id.clone(), filename: format!("{stem}.{ext}"), mime_type: mime.to_owned(), data });
    Ok(content_id)
}

/// What [`extract_data_images`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Extracted {
    pub html: String,
    /// Images moved out of `data:` URLs into the signature's stored images.
    pub added: usize,
    /// `data:` images left alone: not a supported image, or no room for them.
    pub rejected: usize,
}

/// Replaces each `data:image/…;base64,…` URL in `html` with a `cid:` reference to a stored
/// image, adding the image to `images`. Mail clients do not show `data:` images, so a pasted
/// signature that carries its pictures that way would arrive without them.
pub fn extract_data_images(html: &str, images: &mut Vec<SignatureImage>) -> Extracted {
    const PREFIX: &str = "data:image/";
    // ASCII lowercasing keeps every byte offset, so offsets found here index `html`.
    let lower = html.to_ascii_lowercase();
    let (mut out, mut last, mut from) = (String::with_capacity(html.len()), 0, 0);
    let (mut added, mut rejected) = (0, 0);
    while let Some(found) = lower[from..].find(PREFIX) {
        let start = from + found;
        let end = start + html[start..].find(['"', '\'', '<', '>', ')']).unwrap_or(html.len() - start);
        from = end;
        let url = &html[start..end];
        let decoded = url.split_once(',').filter(|(header, _)| header.to_ascii_lowercase().ends_with(";base64")).and_then(|(_, payload)| {
            let payload: String = payload.chars().filter(|c| !c.is_whitespace()).collect();
            LENIENT.decode(payload).ok()
        });
        let before = images.len();
        match decoded.and_then(|bytes| add_image(images, "image", &bytes).ok()) {
            Some(content_id) => {
                out.push_str(&html[last..start]);
                out.push_str(&format!("cid:{content_id}"));
                last = end;
                added += usize::from(images.len() > before);
            }
            None => rejected += 1,
        }
    }
    out.push_str(&html[last..]);
    Extracted { html: out, added, rejected }
}

/// The `Content-ID`s `html` refers to with `cid:`, each once, in order.
pub fn referenced_content_ids(html: &str) -> Vec<String> {
    let lower = html.to_ascii_lowercase();
    let (mut ids, mut from) = (Vec::<String>::new(), 0);
    while let Some(found) = lower[from..].find("cid:") {
        let start = from + found + 4;
        let end = start + html[start..].find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | ')' | ',')).unwrap_or(html.len() - start);
        let id = &html[start..end];
        if !id.is_empty() && !ids.iter().any(|i| i == id) {
            ids.push(id.to_owned());
        }
        from = end.max(start);
    }
    ids
}

/// Drops the images `html` no longer refers to.
pub fn prune_images(html: &str, images: &mut Vec<SignatureImage>) {
    let used = referenced_content_ids(html);
    images.retain(|i| used.contains(&i.content_id));
}

/// `html` with each `cid:` reference to one of `images` replaced by the image as a `data:`
/// URL, for drawing it where `cid:` means nothing.
pub fn with_data_urls(html: &str, images: &[SignatureImage]) -> String {
    let mut out = html.to_owned();
    for image in images {
        let cid = format!("cid:{}", image.content_id);
        if out.contains(&cid) {
            out = out.replace(&cid, &format!("data:{};base64,{}", image.mime_type, image.data));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\nrest-of-png";

    fn apply(text: &str, r: &Replacement) -> String {
        let mut out = text.to_owned();
        out.replace_range(r.range.clone(), &r.text);
        out
    }

    fn selected<'a>(text: &'a str, r: &Replacement) -> &'a str {
        &text[r.selection.clone()]
    }

    #[test]
    fn wrapping_keeps_the_selected_text_selected() {
        let r = wrap("Jason Alexander", 0..5, "<b>", "</b>");
        let out = apply("Jason Alexander", &r);
        assert_eq!(out, "<b>Jason</b> Alexander");
        assert_eq!(selected(&out, &r), "Jason");
    }

    #[test]
    fn wrapping_nothing_leaves_the_caret_between_the_tags() {
        let r = wrap("ab", 1..1, "<i>", "</i>");
        let out = apply("ab", &r);
        assert_eq!(out, "a<i></i>b");
        assert_eq!((r.selection.start, r.selection.end), (4, 4));
    }

    #[test]
    fn a_selection_in_the_middle_of_a_character_snaps_to_whole_characters() {
        let text = "né café";
        // Byte 2 is inside the two-byte "é".
        let r = wrap(text, 2..2, "<b>", "</b>");
        assert_eq!(apply(text, &r), "n<b></b>é café");
        assert_eq!(apply(text, &wrap(text, 0..99, "<u>", "</u>")), "<u>né café</u>", "past the end is clamped");
        // A selection that cuts a character takes the whole character in.
        assert_eq!(apply(text, &wrap(text, 2..4, "<i>", "</i>")), "n<i>é </i>café");
    }

    #[test]
    fn a_selected_address_becomes_the_link_target() {
        let text = "see https://lastrefuge.ai now";
        let r = link(text, 4..25);
        let out = apply(text, &r);
        assert_eq!(out, "see <a href=\"https://lastrefuge.ai\">https://lastrefuge.ai</a> now");
        assert_eq!(selected(&out, &r), "https://lastrefuge.ai");
    }

    #[test]
    fn selected_words_become_link_text_with_the_address_ready_to_type_over() {
        let text = "Last Refuge";
        let r = link(text, 0..text.len());
        let out = apply(text, &r);
        assert_eq!(out, "<a href=\"https://\">Last Refuge</a>");
        assert_eq!(selected(&out, &r), "https://");
        let empty = link("", 0..0);
        assert_eq!(apply("", &empty), "<a href=\"https://\">link text</a>");
        // Something with a quote in it is never put inside the attribute.
        let tricky = "https://x.example/\"onmouseover=";
        assert!(apply(tricky, &link(tricky, 0..tricky.len())).starts_with("<a href=\"https://\">"));
    }

    #[test]
    fn the_lines_touched_become_one_bulleted_list() {
        let text = "Name\nPhone 555\n\nEmail x\nlast";
        let r = bullet_list(text, 6..20);
        assert_eq!(apply(text, &r), "Name\n<ul>\n<li>Phone 555</li>\n<li>Email x</li>\n</ul>\nlast");
        assert_eq!((r.selection.start, r.selection.start == r.selection.end), (r.range.start + r.text.len(), true));
        let single = bullet_list("one", 1..1);
        assert_eq!(apply("one", &single), "<ul>\n<li>one</li>\n</ul>");
    }

    #[test]
    fn a_list_on_an_empty_line_starts_with_the_caret_in_the_item() {
        let r = bullet_list("a\n\nb", 2..2);
        let out = apply("a\n\nb", &r);
        assert_eq!(out, "a\n<ul>\n<li></li>\n</ul>\nb");
        assert_eq!(&out[..r.selection.start], "a\n<ul>\n<li>");
    }

    #[test]
    fn insert_replaces_the_selection_and_puts_the_caret_after() {
        let r = insert("ab", 1..2, "<br>");
        assert_eq!(apply("ab", &r), "a<br>");
        assert_eq!(r.selection, 5..5);
    }

    #[test]
    fn a_signatures_content_ids_are_told_apart_from_a_forwarded_messages() {
        assert!(is_signature_content_id(&new_content_id()));
        assert!(!is_signature_content_id("image001.png@01D9F2A0.5B3C7E10"));
        assert!(!is_signature_content_id("logo@brand.example"));
    }

    #[test]
    fn images_are_recognised_by_their_bytes_not_their_names() {
        assert_eq!(image_type(PNG), Some(("image/png", "png")));
        assert_eq!(image_type(b"\xff\xd8\xff\xe0jfif"), Some(("image/jpeg", "jpg")));
        assert_eq!(image_type(b"GIF89a...."), Some(("image/gif", "gif")));
        assert_eq!(image_type(b"RIFF\x00\x00\x00\x00WEBPVP8 "), Some(("image/webp", "webp")));
        assert_eq!(image_type(b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>"), None, "SVG is not shown by mail clients");
        assert_eq!(image_type(b"MZ\x90\x00"), None, "a program renamed logo.png is not an image");
    }

    #[test]
    fn adding_an_image_names_it_by_what_it_is_and_reuses_one_it_already_has() {
        let mut images = Vec::new();
        let id = add_image(&mut images, "My Logo.JPEG", PNG).unwrap();
        assert!(id.starts_with("sig-") && id.ends_with("@overlook"), "{id}");
        assert_eq!((images[0].filename.as_str(), images[0].mime_type.as_str()), ("My Logo.png", "image/png"));
        assert_eq!(add_image(&mut images, "again.png", PNG).unwrap(), id);
        assert_eq!(images.len(), 1);
        assert_eq!(add_image(&mut images, "notes.png", b"just text"), Err(NOT_AN_IMAGE.to_owned()));
    }

    #[test]
    fn images_are_limited_in_size_count_and_total() {
        let mut big = PNG.to_vec();
        big.resize(MAX_IMAGE_BYTES + 1, 0);
        assert_eq!(add_image(&mut Vec::new(), "big.png", &big), Err(too_big()));

        let mut images = Vec::new();
        for i in 0..MAX_IMAGES {
            let mut bytes = PNG.to_vec();
            bytes.push(i as u8);
            add_image(&mut images, "x.png", &bytes).unwrap();
        }
        let mut one_more = PNG.to_vec();
        one_more.push(200);
        assert_eq!(add_image(&mut images, "x.png", &one_more), Err(too_big()));

        let mut images = Vec::new();
        for i in 0..4 {
            let mut bytes = PNG.to_vec();
            bytes.resize(MAX_IMAGE_BYTES - 1, i);
            add_image(&mut images, "x.png", &bytes).unwrap();
        }
        let mut over = PNG.to_vec();
        over.resize(4096, 9);
        assert_eq!(add_image(&mut images, "x.png", &over), Err(too_big()), "the total is capped too");
    }

    #[test]
    fn pasted_data_images_become_stored_images_and_cid_references() {
        let b64 = base64::engine::general_purpose::STANDARD.encode(PNG);
        let html = format!("<p>Jason</p><img src=\"data:image/png;base64,{b64}\" width=\"80\"><img src='DATA:image/PNG;BASE64,{b64}'>");
        let mut images = Vec::new();
        let out = extract_data_images(&html, &mut images);
        assert_eq!((out.added, out.rejected, images.len()), (1, 0, 1), "the same picture twice is stored once");
        let id = &images[0].content_id;
        assert_eq!(out.html, format!("<p>Jason</p><img src=\"cid:{id}\" width=\"80\"><img src='cid:{id}'>"));
        assert_eq!(images[0].mime_type, "image/png");
    }

    #[test]
    fn wrapped_and_unpadded_base64_still_decodes() {
        let b64 = base64::engine::general_purpose::STANDARD.encode(PNG);
        let wrapped = format!("{}\n  {}", &b64[..8], b64[8..].trim_end_matches('='));
        let mut images = Vec::new();
        let out = extract_data_images(&format!("<img src=\"data:image/png;base64,{wrapped}\">"), &mut images);
        assert_eq!((out.added, out.rejected), (1, 0), "{}", out.html);
    }

    #[test]
    fn data_images_that_cannot_be_stored_are_left_alone_and_counted() {
        let svg = base64::engine::general_purpose::STANDARD.encode(b"<svg/>");
        let html = format!("<img src=\"data:image/svg+xml;base64,{svg}\"><img src=\"data:image/png;base64,!!!not-base64!!!\"><img src=\"data:image/png,raw\">");
        let mut images = Vec::new();
        let out = extract_data_images(&html, &mut images);
        assert_eq!((out.added, out.rejected, out.html == html, images.len()), (0, 3, true, 0));
    }

    #[test]
    fn html_without_data_images_comes_back_unchanged() {
        let html = "<p>Café ☕ <a href=\"https://x.example\">x</a></p>";
        let mut images = Vec::new();
        assert_eq!(extract_data_images(html, &mut images), Extracted { html: html.into(), added: 0, rejected: 0 });
    }

    #[test]
    fn the_images_a_signature_still_uses_are_the_ones_it_refers_to() {
        let html = "<img src=\"cid:a@overlook\"><img src='CID:b@overlook' alt=x><img src=\"cid:a@overlook\"><p style=\"x:url(cid:c@overlook)\">";
        assert_eq!(referenced_content_ids(html), vec!["a@overlook", "b@overlook", "c@overlook"]);
        assert!(referenced_content_ids("<p>cid:</p>").is_empty());

        let image = |id: &str| SignatureImage { content_id: id.into(), filename: "x.png".into(), mime_type: "image/png".into(), data: "AAAA".into() };
        let mut images = vec![image("a@overlook"), image("gone@overlook")];
        prune_images("<img src=\"cid:a@overlook\">", &mut images);
        assert_eq!(images.iter().map(|i| i.content_id.as_str()).collect::<Vec<_>>(), vec!["a@overlook"]);
    }

    #[test]
    fn drawing_a_signature_swaps_cid_references_for_data_urls() {
        let image = SignatureImage { content_id: "a@overlook".into(), filename: "a.png".into(), mime_type: "image/png".into(), data: "QUJD".into() };
        let html = "<img src=\"cid:a@overlook\"><img src=\"cid:other@x\">";
        assert_eq!(with_data_urls(html, &[image]), "<img src=\"data:image/png;base64,QUJD\"><img src=\"cid:other@x\">");
    }
}
