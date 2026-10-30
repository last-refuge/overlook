//! Signatures in the app: how a message gets one, how the composer shows it, and the editor in
//! Settings (HTML source with a small toolbar, and a preview of what it looks like).
//!
//! A signature is HTML whose images are `cid:` references to images stored with it. A message
//! takes a copy of the signature when it is given one, with the images as inline attachments in
//! its own folder, so editing a signature later never changes a message already written.

use super::composer::{stage_attachment, Composer};
use super::settings::{SettingsPage, SettingsState};
use super::MailWindow;
use crate::assets::icon;
use crate::theme::Palette;
use base64::Engine as _;
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Textarea, TextareaState};
use gpui_kit::component::text::TextView;
use gpui_kit::component::Sizable;
use gpui_kit::prelude::*;
use gpui_kit::*;
use overlook_core::sanitize::{clean_only, escape_html, RemoteContent};
use overlook_core::signature as sig;
use overlook_core::*;

/// The editor's toolbar buttons that wrap or insert tags around the selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SignatureTool {
    Bold,
    Italic,
    Underline,
    Link,
    List,
    LineBreak,
}

/// The widest an inserted image is made to show, in CSS pixels. A logo exported for high-density
/// screens is twice as wide as it should look.
const DEFAULT_IMAGE_WIDTH: u32 = 240;

/// Takes the signature off `draft`: its HTML, and the images it brought along.
fn take_off_signature(draft: &mut Draft) {
    draft.attachments.retain(|a| {
        let ours = a.content_id.as_deref().is_some_and(sig::is_signature_content_id);
        if ours {
            if let Some(folder) = std::path::Path::new(&a.local_path).parent() {
                let _ = std::fs::remove_dir_all(folder);
            }
        }
        !ours
    });
    draft.signature_id = None;
    draft.signature_html = None;
}

/// Gives `draft` `signature`, or none: its HTML as it is now, and the images that HTML uses as
/// inline attachments in `folder`. Any signature the draft had comes off first. A blank
/// signature is no signature.
fn give_signature(draft: &mut Draft, signature: Option<&Signature>, folder: &std::path::Path) {
    take_off_signature(draft);
    let Some(signature) = signature.filter(|s| !s.html.trim().is_empty()) else { return };
    let mut total: u64 = draft.attachments.iter().map(|a| a.size_bytes).sum();
    for content_id in sig::referenced_content_ids(&signature.html) {
        let Some(image) = signature.images.iter().find(|i| i.content_id == content_id) else { continue };
        let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(&image.data) else { continue };
        // Scanned when it was added to the signature, not again for every message.
        match stage_attachment(&image.filename, &bytes, folder, total, Some(image.content_id.clone()), false) {
            Ok(attachment) => {
                total += attachment.size_bytes;
                draft.attachments.push(attachment);
            }
            Err(reason) => tracing::info!(target: "overlook::attachments", %reason, "signature image not attached"),
        }
    }
    draft.signature_id = Some(signature.id.clone());
    draft.signature_html = Some(signature.html.clone());
}

/// The images a draft carries inline, to draw HTML that refers to them with `cid:`.
fn inline_images(attachments: &[DraftAttachment]) -> Vec<SignatureImage> {
    attachments
        .iter()
        .filter_map(|a| {
            let content_id = a.content_id.clone()?;
            let bytes = std::fs::read(&a.local_path).ok()?;
            Some(SignatureImage { content_id, filename: a.filename.clone(), mime_type: a.mime_type.clone(), data: base64::engine::general_purpose::STANDARD.encode(bytes) })
        })
        .collect()
}

/// `html` with the draft's inline images swapped in for its `cid:` references, for drawing a
/// message that has not been sent where `cid:` means nothing.
pub(super) fn resolve_draft_images(html: &str, attachments: &[DraftAttachment]) -> String {
    if !html.to_ascii_lowercase().contains("cid:") {
        return html.to_owned();
    }
    sig::with_data_urls(html, &inline_images(attachments))
}

/// The draft's signature as the composer shows it: cleaned, with remote images held back, and
/// its own images drawn.
pub(super) fn signature_view(draft: &Draft) -> Option<SharedString> {
    let html = draft.signature_html.as_deref().filter(|h| !h.trim().is_empty())?;
    Some(resolve_draft_images(&clean_only(html, RemoteContent::Blocked), &draft.attachments).into())
}

/// Whether `html` shows a picture from the web, which the preview holds back.
fn has_remote_images(html: &str) -> bool {
    let lower = html.to_ascii_lowercase();
    ["src=\"http", "src='http", "src=http"].iter().any(|needle| lower.contains(needle))
}

impl MailWindow {
    /// The id of the signature an account uses by default for new messages, or for replies and
    /// forwards.
    pub(super) fn default_signature_id(&self, reply: bool) -> Option<String> {
        self.account_prefs_for(&self.account.id).default_signature(reply).map(|s| s.id.clone())
    }

    /// Gives `draft` the signature `id`, or none; see [`give_signature`].
    pub(super) fn apply_signature(&self, draft: &mut Draft, id: Option<&str>) {
        let prefs = self.account_prefs_for(&draft.account_id);
        let folder = self.outgoing_dir.join(draft.id.as_str());
        give_signature(draft, id.and_then(|i| prefs.signature_by_id(i)), &folder);
    }

    /// The signature under the editor of a message, with the two things to do with it.
    pub(super) fn render_signature_block(&self, c: &Composer, p: &Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        // The preview already shows the message as sent, signature included.
        if c.preview {
            return None;
        }
        let view = c.signature_view.clone()?;
        let id = c.draft.signature_id.clone().unwrap_or_default();
        let name = self.account_prefs_for(&c.draft.account_id).signature_by_id(&id).map(|s| s.name.clone()).filter(|n| !n.trim().is_empty()).unwrap_or_else(|| "Signature".into());
        let edit_id = id.clone();
        Some(
            v_flex()
                .px(px(16.))
                .py(px(8.))
                .gap(px(8.))
                .border_t_1()
                .border_color(p.divider)
                .child(
                    h_flex()
                        .items_center()
                        .gap(px(8.))
                        .text_size(px(12.))
                        .text_color(p.muted)
                        .child(icon("signature").size(px(14.)))
                        .child(div().flex_1().truncate().child(format!("Signature · {name}")))
                        .child(Button::new("signature-edit").ghost().xsmall().label("Edit").tooltip("Edit this signature in Settings. A message keeps the signature it was given.").on_click(cx.listener(move |this, _, window, cx| {
                            this.edit_signature_in_settings(edit_id.clone(), window, cx);
                        })))
                        .child(Button::new("signature-remove").ghost().xsmall().icon(icon("x").size(px(13.))).tooltip("Send without a signature").on_click(cx.listener(|this, _, window, cx| this.set_signature(None, window, cx)))),
                )
                .child(
                    div()
                        .pl(px(12.))
                        .border_l_2()
                        .border_color(p.divider)
                        .text_size(px(14.))
                        .line_height(px(22.))
                        .child(TextView::html(SharedString::from(format!("signature-{}-{id}", c.draft.id)), view)),
                )
                .into_any_element(),
        )
    }

    fn edit_signature_in_settings(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let account = self.account.id.clone();
        self.open_settings(SettingsPage::Accounts, window, cx);
        self.select_signature(&account, Some(id), window, cx);
    }

    // ------------------------------------------------------------------ the editor in Settings

    /// The editor for the signature being edited: a toolbar, the HTML, and how it looks.
    pub(super) fn render_signature_editor(&self, state: &SettingsState, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let prefs = self.account_prefs_for(&state.account);
        let signature = state.selected_signature.as_deref().and_then(|id| prefs.signature_by_id(id)).cloned().unwrap_or_default();
        let tool = |id: &'static str, name: &'static str, tip: &'static str, which: SignatureTool, cx: &mut Context<Self>| {
            Button::new(id).ghost().xsmall().icon(icon(name).size(px(15.))).tooltip(tip).on_click(cx.listener(move |this, _, window, cx| this.signature_tool(which, window, cx)))
        };
        let toolbar = h_flex()
            .items_center()
            .gap(px(2.))
            .child(tool("sig-bold", "bold", "Bold", SignatureTool::Bold, cx))
            .child(tool("sig-italic", "italic", "Italic", SignatureTool::Italic, cx))
            .child(tool("sig-underline", "underline", "Underline", SignatureTool::Underline, cx))
            .child(div().w(px(1.)).h(px(18.)).mx(px(4.)).bg(p.divider))
            .child(tool("sig-link", "link", "Link: select the text, or an address", SignatureTool::Link, cx))
            .child(tool("sig-list", "list", "Bulleted list from the selected lines", SignatureTool::List, cx))
            .child(tool("sig-break", "corner-down-left", "Line break", SignatureTool::LineBreak, cx))
            .child(div().w(px(1.)).h(px(18.)).mx(px(4.)).bg(p.divider))
            .child(Button::new("sig-image").ghost().xsmall().icon(icon("image-plus").size(px(15.))).label("Image").tooltip("Insert a picture (PNG, JPEG, GIF or WebP)").on_click(cx.listener(|this, _, window, cx| this.choose_signature_image(window, cx))));

        let preview: AnyElement = if signature.html.trim().is_empty() {
            div().text_size(px(12.)).text_color(p.muted).child("Nothing to show yet. Type or paste HTML above.").into_any_element()
        } else {
            let html = sig::with_data_urls(&clean_only(&signature.html, RemoteContent::Blocked), &signature.images);
            TextView::html(SharedString::from(format!("signature-preview-{}", signature.id)), html).into_any_element()
        };
        v_flex()
            .gap(px(8.))
            .child(toolbar)
            .child(div().rounded(px(8.)).border_1().border_color(p.divider).h(px(170.)).child(Textarea::new(&state.signature).h_full().font_family("monospace").text_size(px(12.))))
            .child(div().text_size(px(12.)).font_weight(FontWeight::MEDIUM).text_color(p.muted).child("Preview"))
            .child(div().rounded(px(8.)).border_1().border_color(p.divider).px(px(14.)).py(px(10.)).min_h(px(64.)).text_size(px(14.)).line_height(px(22.)).child(preview))
            .when(has_remote_images(&signature.html), |d| d.child(div().text_size(px(11.)).text_color(p.muted).child("Pictures from the web are not loaded in the preview. They are sent as links, and many mail programs hold them back until the reader allows them.")))
            .into_any_element()
    }

    /// Takes new HTML for the signature being edited: pictures pasted in as `data:` URLs become
    /// stored images, and the result is saved. The images are tidied when the signature is left,
    /// not here, so that undoing the removal of a picture still finds it.
    pub(super) fn edit_signature_html(&mut self, html: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(state) = self.settings.as_ref() else { return };
        let (Some(id), area, account) = (state.selected_signature.clone(), state.signature.clone(), state.account.clone()) else { return };
        let mut prefs = self.account_prefs_for(&account);
        let Some(signature) = prefs.signatures.iter_mut().find(|s| s.id == id) else { return };
        let already_had_data_images = signature.html.to_ascii_lowercase().contains("data:image/");
        let extracted = sig::extract_data_images(&html, &mut signature.images);
        signature.html = extracted.html.clone();
        self.save_account_prefs(&account, prefs);
        if extracted.html != html {
            // The pasted pictures are now references; show the source as it is stored.
            area.update(cx, |s, cx| s.set_value(extracted.html.clone(), window, cx));
        }
        if extracted.rejected > 0 && !already_had_data_images {
            crate::ui::toast(window, cx, crate::notify::Level::Warning, format!("{} pasted picture{} could not be kept: only PNG, JPEG, GIF and WebP up to 1 MB, 4 MB in all, are used in signatures.", extracted.rejected, if extracted.rejected == 1 { "" } else { "s" }));
        }
        cx.notify();
    }

    /// Drops the images the signatures no longer use, once they are not being edited.
    pub(super) fn prune_signature_images(&mut self, account: &AccountId) {
        let mut prefs = self.account_prefs_for(account);
        let before: usize = prefs.signatures.iter().map(|s| s.images.len()).sum();
        for s in &mut prefs.signatures {
            let html = s.html.clone();
            sig::prune_images(&html, &mut s.images);
        }
        if prefs.signatures.iter().map(|s| s.images.len()).sum::<usize>() != before {
            self.save_account_prefs(account, prefs);
        }
    }

    fn signature_tool(&mut self, tool: SignatureTool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(area) = self.settings.as_ref().map(|s| s.signature.clone()) else { return };
        let (text, selection) = {
            let area = area.read(cx);
            (area.value().to_string(), area.selected_range())
        };
        let edit = match tool {
            SignatureTool::Bold => sig::wrap(&text, selection, "<b>", "</b>"),
            SignatureTool::Italic => sig::wrap(&text, selection, "<i>", "</i>"),
            SignatureTool::Underline => sig::wrap(&text, selection, "<u>", "</u>"),
            SignatureTool::Link => sig::link(&text, selection),
            SignatureTool::List => sig::bullet_list(&text, selection),
            SignatureTool::LineBreak => sig::insert(&text, selection, "<br>\n"),
        };
        self.apply_signature_edit(&area, edit, window, cx);
    }

    /// Applies an edit to the source as a replacement, so undo still works, and saves it.
    fn apply_signature_edit(&mut self, area: &Entity<TextareaState>, edit: sig::Replacement, window: &mut Window, cx: &mut Context<Self>) {
        area.update(cx, |s, cx| {
            s.set_selected_range(edit.range.clone(), cx);
            s.replace(edit.text.clone(), window, cx);
            s.set_selected_range(edit.selection.clone(), cx);
            s.focus(window, cx);
        });
        let html = area.read(cx).value().to_string();
        self.edit_signature_html(html, window, cx);
    }

    /// Asks for a picture and puts it in the signature at the caret.
    fn choose_signature_image(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editing) = self.settings.as_ref().and_then(|s| s.selected_signature.clone()) else { return };
        let receiver = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: false, prompt: Some("Insert".into()) });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = receiver.await else { return };
            let Some(path) = paths.into_iter().next() else { return };
            // Reading and scanning stay off the UI thread.
            let read = cx
                .background_spawn(async move {
                    let bytes = std::fs::read(&path).map_err(|e| format!("Couldn’t read that file: {e}"))?;
                    if crate::malware_scan::scan(&path) == crate::malware_scan::Scan::Infected {
                        return Err(crate::malware_scan::INFECTED.to_owned());
                    }
                    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "image".into());
                    Ok((name, bytes))
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| match read {
                Ok((name, bytes)) => this.insert_signature_image(&editing, &name, &bytes, window, cx),
                Err(reason) => crate::ui::toast(window, cx, crate::notify::Level::Warning, reason),
            });
        })
        .detach();
    }

    fn insert_signature_image(&mut self, editing: &str, name: &str, bytes: &[u8], window: &mut Window, cx: &mut Context<Self>) {
        // The user may have chosen another signature, or closed Settings, while the file was read.
        let Some(state) = self.settings.as_ref().filter(|s| s.selected_signature.as_deref() == Some(editing)) else { return };
        let (area, account) = (state.signature.clone(), state.account.clone());
        let mut prefs = self.account_prefs_for(&account);
        let Some(signature) = prefs.signatures.iter_mut().find(|s| s.id == editing) else { return };
        // Make room by letting go of pictures the HTML no longer shows.
        let html = signature.html.clone();
        sig::prune_images(&html, &mut signature.images);
        let content_id = match sig::add_image(&mut signature.images, name, bytes) {
            Ok(content_id) => content_id,
            Err(reason) => {
                crate::ui::toast(window, cx, crate::notify::Level::Warning, reason);
                return;
            }
        };
        let width = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().ok().and_then(|r| r.into_dimensions().ok()).map(|(w, _)| w.min(DEFAULT_IMAGE_WIDTH));
        let stem = name.rsplit_once('.').map_or(name, |(stem, _)| stem);
        let snippet = format!("<img src=\"cid:{content_id}\" alt=\"{}\"{}>", escape_html(stem), width.map(|w| format!(" width=\"{w}\"")).unwrap_or_default());
        self.save_account_prefs(&account, prefs);
        let (text, selection) = {
            let area = area.read(cx);
            (area.value().to_string(), area.selected_range())
        };
        self.apply_signature_edit(&area, sig::insert(&text, selection, &snippet), window, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    // The parent's GPUI glob also exports a `test` proc macro.
    use std::prelude::v1::test;

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\nlogo";

    fn signature(html: &str, images: &[(&str, &[u8])]) -> Signature {
        Signature {
            id: "work".into(),
            name: "Work".into(),
            html: html.into(),
            images: images
                .iter()
                .map(|(id, bytes)| SignatureImage { content_id: format!("{id}@overlook"), filename: format!("{id}.png"), mime_type: "image/png".into(), data: base64::engine::general_purpose::STANDARD.encode(bytes) })
                .collect(),
            ..Default::default()
        }
    }

    fn draft() -> Draft {
        Draft::new(AccountId::from("a"), DraftKind::New, 0)
    }

    #[test]
    fn a_signature_brings_the_images_its_html_uses_as_inline_attachments() {
        let dir = tempfile::tempdir().unwrap();
        let signature = signature("<p>Jason</p><img src=\"cid:sig-logo@overlook\">", &[("sig-logo", PNG), ("sig-unused", b"\x89PNG\r\n\x1a\nother")]);
        let mut draft = draft();
        give_signature(&mut draft, Some(&signature), dir.path());

        assert_eq!((draft.signature_id.as_deref(), draft.signature_html.as_deref()), (Some("work"), Some(signature.html.as_str())));
        assert_eq!(draft.attachments.len(), 1, "the unused image is not sent");
        let logo = &draft.attachments[0];
        assert_eq!((logo.content_id.as_deref(), logo.mime_type.as_str(), logo.is_listed()), (Some("sig-logo@overlook"), "image/png", false));
        assert_eq!(std::fs::read(&logo.local_path).unwrap(), PNG);
        assert_eq!(std::fs::metadata(&logo.local_path).unwrap().permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn changing_or_removing_the_signature_takes_its_images_away_but_not_other_attachments() {
        let dir = tempfile::tempdir().unwrap();
        let mut draft = draft();
        // A file the user attached and an image a forwarded message brought along.
        let kept = stage_attachment("plan.pdf", b"%PDF-1.7", dir.path(), 0, None, false).unwrap();
        let forwarded = stage_attachment("photo.png", PNG, dir.path(), 0, Some("image001@01D9.5B3C".into()), false).unwrap();
        draft.attachments = vec![kept.clone(), forwarded.clone()];

        give_signature(&mut draft, Some(&signature("<img src=\"cid:sig-a@overlook\">", &[("sig-a", PNG)])), dir.path());
        assert_eq!(draft.attachments.len(), 3);
        let first = draft.attachments[2].local_path.clone();

        give_signature(&mut draft, Some(&signature("<img src=\"cid:sig-b@overlook\">", &[("sig-b", PNG)])), dir.path());
        assert_eq!(draft.attachments.iter().filter_map(|a| a.content_id.as_deref()).collect::<Vec<_>>(), vec!["image001@01D9.5B3C", "sig-b@overlook"]);
        assert!(!std::path::Path::new(&first).exists(), "the old signature's image is deleted");

        give_signature(&mut draft, None, dir.path());
        assert_eq!(draft.attachments.iter().map(|a| a.filename.as_str()).collect::<Vec<_>>(), vec!["plan.pdf", "photo.png"]);
        assert_eq!((draft.signature_id, draft.signature_html), (None, None));
        assert!(std::path::Path::new(&kept.local_path).exists() && std::path::Path::new(&forwarded.local_path).exists());
    }

    #[test]
    fn a_blank_signature_is_no_signature() {
        let dir = tempfile::tempdir().unwrap();
        let mut draft = draft();
        give_signature(&mut draft, Some(&signature("  \n", &[])), dir.path());
        assert_eq!((draft.signature_id, draft.signature_html, draft.attachments.len()), (None, None, 0));
    }

    #[test]
    fn an_image_that_is_not_there_is_skipped_rather_than_failing_the_message() {
        let dir = tempfile::tempdir().unwrap();
        let mut broken = signature("<img src=\"cid:sig-a@overlook\"><img src=\"cid:sig-b@overlook\">", &[("sig-a", PNG)]);
        broken.images.push(SignatureImage { content_id: "sig-b@overlook".into(), filename: "b.png".into(), mime_type: "image/png".into(), data: "!!not base64!!".into() });
        let mut draft = draft();
        give_signature(&mut draft, Some(&broken), dir.path());
        assert_eq!(draft.attachments.len(), 1);
        assert_eq!(draft.signature_id.as_deref(), Some("work"));
    }

    #[test]
    fn a_draft_is_drawn_with_its_inline_images_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let mut draft = draft();
        give_signature(&mut draft, Some(&signature("<p>Jason</p><img src=\"cid:sig-logo@overlook\">", &[("sig-logo", PNG)])), dir.path());
        let png64 = base64::engine::general_purpose::STANDARD.encode(PNG);
        let html = "<img src=\"cid:sig-logo@overlook\"><img src=\"cid:elsewhere@x\">";
        assert_eq!(resolve_draft_images(html, &draft.attachments), format!("<img src=\"data:image/png;base64,{png64}\"><img src=\"cid:elsewhere@x\">"));
        assert_eq!(resolve_draft_images("<p>no images</p>", &draft.attachments), "<p>no images</p>");
    }

    #[test]
    fn the_composer_shows_a_cleaned_signature_with_its_own_pictures_but_nothing_from_the_web() {
        let dir = tempfile::tempdir().unwrap();
        let mut draft = draft();
        let html = "<p onclick=\"x()\">Jason</p><script>alert(1)</script><img src=\"https://tracker.example/p.gif\"><img src=\"cid:sig-logo@overlook\">";
        give_signature(&mut draft, Some(&signature(html, &[("sig-logo", PNG)])), dir.path());
        let view = signature_view(&draft).unwrap();
        assert!(view.contains("Jason") && !view.contains("onclick") && !view.contains("script"), "{view}");
        assert!(!view.contains("tracker.example"), "remote pictures are held back: {view}");
        assert!(view.contains("data:image/png;base64,"), "its own picture is drawn: {view}");
        assert_eq!(signature_view(&self::draft()), None);
    }

    #[test]
    fn remote_pictures_are_noticed_however_they_are_quoted() {
        assert!(has_remote_images("<img src=\"https://x.example/a.png\">"));
        assert!(has_remote_images("<IMG SRC='http://x.example/a.png'>"));
        assert!(!has_remote_images("<img src=\"cid:a@overlook\"><a href=\"https://x.example\">x</a>"));
    }
}
