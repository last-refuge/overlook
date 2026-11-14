//! Small presentational helpers shared by the mail views.

use crate::theme::Palette;
use gpui_kit::base::h_flex;
use gpui_kit::prelude::*;
use gpui_kit::*;
use overlook_core::Address;

/// GPUI clips children to a rectangle; cover the top notches after the pane paints.
pub fn mail_pane(body: impl IntoElement, backdrop: Hsla, top_right_radius: Pixels) -> Div {
    div().relative().size_full().overflow_hidden()
        .child(body)
        .child(gpui_kit::base::RoundedFrameCover::top(px(8.), top_right_radius).backdrop(backdrop))
}

/// Circular initials avatar. Each person keeps one of four quiet tints (slate, amber,
/// plum, pine), chosen from their address so they look the same in every list and
/// message; Alexander keeps its avatars gray.
pub fn avatar(address: &Address, size: f32, p: &Palette) -> Div {
    let (fill, ink) = p.avatar_tint(&address.email);
    div()
        .flex_none()
        .size(px(size))
        .rounded_full()
        .bg(fill)
        .text_color(ink)
        .flex()
        .items_center()
        .justify_center()
        .text_size(px((size * 0.36).round()))
        .font_weight(FontWeight::SEMIBOLD)
        .child(address.initials())
}

/// Avatar for the account owner, in the selection tint like the preview's "JA" badge.
pub fn owner_avatar(address: &Address, size: f32, p: &Palette) -> Div {
    avatar(address, size, p).bg(p.selection).text_color(p.selection_text)
}

/// Quiet rounded chip for labels, counts, and keyboard hints.
pub fn chip(text: impl Into<SharedString>, p: &Palette) -> Div {
    div()
        .flex_none()
        .px(px(7.))
        .py(px(1.))
        .rounded(px(5.))
        .bg(p.chip)
        .border_1()
        .border_color(p.divider)
        .text_size(px(11.))
        .text_color(p.muted)
        .child(text.into())
}

/// Keyboard hint, e.g. `Ctrl E`.
pub fn kbd(text: impl Into<SharedString>, p: &Palette) -> Div {
    div()
        .flex_none()
        .px(px(5.))
        .rounded(px(4.))
        .border_1()
        .border_color(p.divider)
        .bg(p.surface)
        .text_size(px(11.))
        .text_color(p.muted)
        .child(text.into())
}

/// Thin horizontal divider.
pub fn divider(p: &Palette) -> Div {
    div().w_full().h(px(1.)).bg(p.divider)
}

/// The track of a segmented control; fill it with [`segment`]s.
pub fn segmented(p: &Palette) -> Div {
    h_flex().items_center().gap(px(2.)).p(px(3.)).rounded(px(8.)).bg(p.sidebar).border_1().border_color(p.divider)
}

/// One option of a segmented control. The chosen segment is a raised card with full-strength,
/// semibold text; the others sit flat in muted text. Attach `.on_click` and an optional icon
/// child at the call site.
pub fn segment(id: impl Into<ElementId>, label: impl Into<SharedString>, selected: bool, p: &Palette) -> Stateful<Div> {
    segment_base(id, selected, p).child(label.into())
}

fn segment_base(id: impl Into<ElementId>, selected: bool, p: &Palette) -> Stateful<Div> {
    h_flex()
        .id(id)
        .items_center()
        .gap(px(6.))
        .px(px(10.))
        .py(px(4.))
        .rounded(px(6.))
        .text_size(px(13.))
        .cursor_pointer()
        .when(selected, |d| d.bg(p.surface).text_color(p.foreground).font_weight(FontWeight::SEMIBOLD).border_1().border_color(p.control_border).shadow_sm())
        .when(!selected, |d| d.text_color(p.muted).hover(|d| d.bg(p.hover).text_color(p.foreground)))
}
