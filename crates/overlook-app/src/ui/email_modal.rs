//! Focused email modal opened by double-clicking an item in the message list.
//!
//! Actions taken inside the modal (Archive, Trash, Move, Star, Mark read/unread,
//! Reply, Forward) act on this email and keep the user in the modal until they
//! explicitly close it or send their message.

use super::*;
use crate::assets::{color_icon, icon, ColorIcon as C};

const MODAL_DEFAULT_WIDTH: f32 = 920.;
const MODAL_DEFAULT_HEIGHT: f32 = 760.;
const MODAL_MIN_WIDTH: f32 = 560.;
const MODAL_MIN_HEIGHT: f32 = 420.;
const MODAL_MAX_WIDTH: f32 = 0.94;
const MODAL_MAX_HEIGHT: f32 = 0.90;

/// The email modal's dimensions. Resizing persists for the session.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EmailModalSize {
    pub width: Pixels,
    pub height: Pixels,
}

impl Default for EmailModalSize {
    fn default() -> Self {
        Self {
            width: px(MODAL_DEFAULT_WIDTH),
            height: px(MODAL_DEFAULT_HEIGHT),
        }
    }
}

impl EmailModalSize {
    fn dragged_to(
        self,
        resize: EmailModalResize,
        pointer: Point<Pixels>,
        window: Size<Pixels>,
    ) -> Self {
        let span = |at: Pixels, whole: Pixels, least: f32, most: f32| {
            let whole = f32::from(whole);
            px((2. * (f32::from(at) - whole / 2.).abs())
                .max(least)
                .min(whole * most))
        };
        Self {
            width: if resize.width {
                span(pointer.x, window.width, MODAL_MIN_WIDTH, MODAL_MAX_WIDTH)
            } else {
                self.width
            },
            height: if resize.height {
                span(pointer.y, window.height, MODAL_MIN_HEIGHT, MODAL_MAX_HEIGHT)
            } else {
                self.height
            },
        }
    }
}

/// Active edge/corner resize drag on the email modal.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EmailModalResize {
    pub width: bool,
    pub height: bool,
    pub cursor: CursorStyle,
}

/// Clicking the backdrop closes the modal only when nothing is being composed,
/// so a stray click outside cannot interrupt an active reply or draft.
pub(super) fn backdrop_closes_modal(has_composer: bool) -> bool {
    !has_composer
}

/// While the email modal is open, list refreshes triggered by mailbox actions
/// (archive, trash, folder move, filter changes) must keep the open email
/// loaded as long as at least one message in its conversation still exists in
/// the mail index. Only permanent deletion empties it.
pub(super) fn keep_modal_selection_after_refresh(
    email_modal: bool,
    remaining_in_index: usize,
) -> bool {
    email_modal && remaining_in_index > 0
}

/// Subtle location pill shown in the modal header when the email has moved out
/// of the background list's folder (or sits in Trash/Junk), confirming the
/// result of actions taken while staying in the modal.
pub(super) fn modal_location_badge(
    view: &MailboxView,
    thread: &[Message],
    labels: &[Label],
) -> Option<(&'static str, String)> {
    if thread.is_empty() {
        return None;
    }
    let of_kind = |kind: LabelKind| labels.iter().find(|l| l.kind == kind).map(|l| &l.id);
    let trash = of_kind(LabelKind::Trash);
    let spam = of_kind(LabelKind::Spam);
    let inbox = of_kind(LabelKind::Inbox);
    let sent = of_kind(LabelKind::Sent);

    let all_in_trash = trash.is_some_and(|t| thread.iter().all(|m| m.has_label(t)));
    if all_in_trash {
        return Some(("trash", "In Trash".to_owned()));
    }
    let any_in_spam = spam.is_some_and(|s| thread.iter().any(|m| m.has_label(s)));
    if any_in_spam {
        return Some(("shield-alert", "In Junk".to_owned()));
    }
    let any_in_inbox = inbox.is_some_and(|i| thread.iter().any(|m| m.has_label(i)));
    if any_in_inbox {
        if matches!(
            view,
            MailboxView::Kind(LabelKind::Trash | LabelKind::Archive | LabelKind::Spam)
                | MailboxView::Label(_)
        ) {
            return Some(("inbox", "In Inbox".to_owned()));
        }
        return None;
    }
    if let Some(folder) = labels
        .iter()
        .find(|l| l.kind == LabelKind::User && thread.iter().any(|m| m.has_label(&l.id)))
    {
        if *view != MailboxView::Label(folder.id.clone()) {
            return Some((
                "folder",
                format!("In {}", overlook_core::sidebar::folder_title(&folder.name)),
            ));
        }
        return None;
    }
    let any_in_sent = sent.is_some_and(|s| thread.iter().any(|m| m.has_label(s)));
    if !any_in_sent || *view == MailboxView::Kind(LabelKind::Inbox) {
        return Some(("archive", "Archived".to_owned()));
    }
    None
}

impl MailWindow {
    /// Opens `id` in the focused email modal (selecting it first if needed).
    pub(super) fn open_conversation_modal(
        &mut self,
        id: ConversationId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let needs_select = self.selected.as_ref() != Some(&id)
            || self.outbox_selected.is_some()
            || self
                .composer
                .as_ref()
                .is_some_and(|c| c.draft.kind == DraftKind::New)
            || self.thread.is_empty();
        if needs_select {
            self.select_conversation(Some(id), window, cx);
        } else if !self.marked.is_empty() {
            self.marked.clear();
        }
        if self.selected.is_none() || self.thread.is_empty() {
            return;
        }
        self.email_modal = true;
        self.email_modal_next = None;
        self.email_modal_resize = None;
        self.email_modal_focus.focus(window, cx);
        cx.notify();
    }

    /// Opens the currently selected draft or outbox item in the email modal.
    pub(super) fn open_current_in_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.composer.is_none() && self.outbox_selected.is_none() && self.thread.is_empty() {
            return;
        }
        self.email_modal = true;
        self.email_modal_next = None;
        self.email_modal_resize = None;
        if self.composer.is_none() {
            self.email_modal_focus.focus(window, cx);
        }
        cx.notify();
    }

    /// Closes the email modal, saving any open draft first and advancing the
    /// background list if the conversation moved out of the current folder.
    /// Returns `false` if a draft save/attachment is still in flight.
    pub(super) fn close_email_modal(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.email_modal {
            return true;
        }
        if self
            .composer
            .as_ref()
            .is_some_and(|c| c.attaching || c.pending_saves > 0)
        {
            crate::ui::toast(
                window,
                cx,
                crate::notify::Level::Info,
                "Wait for the draft and attachments to finish saving.",
            );
            return false;
        }
        self.email_modal = false;
        self.email_modal_resize = None;
        if self.composer.is_some() {
            self.close_composer(window, cx);
        }
        let deferred_next = self.email_modal_next.take();
        if self.outbox_selected.is_none()
            && (self.selected.is_some() || deferred_next.is_some())
            && !self
                .conversations
                .iter()
                .any(|c| Some(&c.id) == self.selected.as_ref())
        {
            let next = deferred_next
                .filter(|id| self.conversations.iter().any(|c| &c.id == id))
                .or_else(|| self.conversations.first().map(|c| c.id.clone()));
            self.select_quietly(next, false, window, cx);
        }
        self.list_focus.focus(window, cx);
        cx.notify();
        true
    }

    pub(crate) fn render_email_modal(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = *cx.palette();
        let has_composer = self.composer.is_some();
        let reading_conversation = self.outbox_selected.is_none()
            && (self.composer.is_none() || self.composer_is_inline())
            && !self.thread.is_empty();
        let (in_trash, in_inbox) = self.thread_state();
        let any_unread = self.thread.iter().any(|m| !m.is_read);
        let starred = self.thread.iter().any(|m| m.is_starred);
        let newest = self.thread.iter().max_by_key(|m| m.date).cloned();
        let location = reading_conversation
            .then(|| modal_location_badge(&self.view, &self.thread, &self.labels))
            .flatten();

        let (archive_label, archive_icon, archive_tip) = if in_trash {
            ("Restore", C::Move, "Restore to Inbox")
        } else if !in_inbox {
            ("To Inbox", C::Move, "Move to Inbox")
        } else {
            ("Archive", C::Archive, "Archive")
        };

        let header: AnyElement = if reading_conversation {
            h_flex()
                .flex_none()
                .items_center()
                .gap(px(4.))
                .h(px(48.))
                .px(px(14.))
                .border_b_1()
                .border_color(p.divider)
                .bg(p.surface)
                .child(
                    Button::new("modal-reply")
                        .ghost()
                        .small()
                        .child(
                            h_flex()
                                .items_center()
                                .gap(px(6.))
                                .child(color_icon(C::Reply, 16., &p))
                                .child("Reply"),
                        )
                        .tooltip("Reply (Ctrl R)")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.reply(DraftKind::Reply, window, cx);
                        })),
                )
                .child(
                    Button::new("modal-reply-all")
                        .ghost()
                        .small()
                        .child(
                            h_flex()
                                .items_center()
                                .gap(px(6.))
                                .child(color_icon(C::ReplyAll, 16., &p))
                                .child("Reply all"),
                        )
                        .tooltip("Reply all (Ctrl Shift R)")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.reply(DraftKind::ReplyAll, window, cx);
                        })),
                )
                .child(
                    Button::new("modal-forward")
                        .ghost()
                        .small()
                        .child(
                            h_flex()
                                .items_center()
                                .gap(px(6.))
                                .child(color_icon(C::Forward, 16., &p))
                                .child("Forward"),
                        )
                        .tooltip("Forward (Ctrl Shift F)")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.reply(DraftKind::Forward, window, cx);
                        })),
                )
                .child(div().w(px(1.)).h(px(18.)).mx(px(4.)).bg(p.divider))
                .child(
                    Button::new("modal-archive")
                        .ghost()
                        .small()
                        .child(
                            h_flex()
                                .items_center()
                                .gap(px(6.))
                                .child(color_icon(archive_icon, 16., &p))
                                .child(archive_label),
                        )
                        .tooltip(archive_tip)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.on_archive(&ArchiveSelected, window, cx);
                        })),
                )
                .when(!in_trash, |bar| {
                    bar.child(
                        Button::new("modal-delete")
                            .ghost()
                            .small()
                            .child(
                                h_flex()
                                    .items_center()
                                    .gap(px(6.))
                                    .child(color_icon(C::Delete, 16., &p))
                                    .child("Delete"),
                            )
                            .tooltip("Move to Trash")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.on_trash(&TrashSelected, window, cx);
                            })),
                    )
                })
                .child(
                    Button::new("modal-move")
                        .ghost()
                        .small()
                        .child(
                            h_flex()
                                .items_center()
                                .gap(px(6.))
                                .child(color_icon(C::Move, 16., &p))
                                .child("Move"),
                        )
                        .tooltip("Move to folder… (Ctrl Shift V)")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.on_move_selected(&MoveSelected, window, cx);
                        })),
                )
                .child(
                    Button::new("modal-toggle-read")
                        .ghost()
                        .small()
                        .w(px(32.))
                        .h(px(30.))
                        .p(px(0.))
                        .child(color_icon(
                            if any_unread { C::Read } else { C::Mail },
                            16.,
                            &p,
                        ))
                        .accessibility_label(if any_unread {
                            "Mark read"
                        } else {
                            "Mark unread"
                        })
                        .tooltip(if any_unread {
                            "Mark read (Ctrl U)"
                        } else {
                            "Mark unread (Ctrl U)"
                        })
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.on_toggle_read(&ToggleRead, window, cx);
                        })),
                )
                .child(
                    Button::new("modal-toggle-star")
                        .ghost()
                        .small()
                        .w(px(32.))
                        .h(px(30.))
                        .p(px(0.))
                        .toggled(starred)
                        .child(color_icon(C::Star, 16., &p))
                        .accessibility_label(if starred { "Unstar" } else { "Star" })
                        .tooltip(if starred { "Unstar" } else { "Star" })
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.on_toggle_star(&ToggleStar, window, cx);
                        })),
                )
                .child(div().flex_1())
                .when_some(location, |bar, (glyph, label)| {
                    bar.child(
                        h_flex()
                            .flex_none()
                            .items_center()
                            .gap(px(5.))
                            .h(px(24.))
                            .px(px(9.))
                            .rounded(px(12.))
                            .bg(p.chip)
                            .text_size(px(11.5))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(if in_trash { p.warning } else { p.muted })
                            .child(
                                icon(glyph)
                                    .size(px(12.))
                                    .text_color(if in_trash { p.warning } else { p.muted }),
                            )
                            .child(label),
                    )
                })
                .when(any_unread, |bar| {
                    bar.child(
                        h_flex()
                            .flex_none()
                            .items_center()
                            .gap(px(5.))
                            .h(px(24.))
                            .px(px(9.))
                            .rounded(px(12.))
                            .bg(p.chip)
                            .text_size(px(11.5))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(p.accent)
                            .child(div().size(px(6.)).rounded_full().bg(p.accent))
                            .child("Unread"),
                    )
                })
                .children(newest.map(|m| {
                    Button::new("modal-print")
                        .ghost()
                        .small()
                        .w(px(32.))
                        .h(px(30.))
                        .p(px(0.))
                        .child(icon("printer").size(px(16.)).text_color(p.muted))
                        .accessibility_label("Print")
                        .tooltip("Print…")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.print_message(m.clone(), window, cx);
                        }))
                }))
                .child(
                    Button::new("modal-close")
                        .ghost()
                        .small()
                        .w(px(32.))
                        .h(px(30.))
                        .p(px(0.))
                        .child(icon("x").size(px(17.)).text_color(p.muted))
                        .accessibility_label("Close")
                        .tooltip("Close (Esc)")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.close_email_modal(window, cx);
                        })),
                )
                .into_any_element()
        } else {
            let title = if self.outbox_selected.is_some() {
                "Outbox"
            } else {
                "Draft"
            };
            h_flex()
                .flex_none()
                .items_center()
                .gap(px(8.))
                .h(px(44.))
                .px(px(16.))
                .border_b_1()
                .border_color(p.divider)
                .bg(p.surface)
                .child(
                    div()
                        .text_size(px(13.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(p.foreground)
                        .child(title),
                )
                .child(div().flex_1())
                .child(
                    Button::new("modal-close")
                        .ghost()
                        .small()
                        .w(px(30.))
                        .h(px(28.))
                        .p(px(0.))
                        .child(icon("x").size(px(16.)).text_color(p.muted))
                        .accessibility_label("Close")
                        .tooltip("Close (Esc)")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.close_email_modal(window, cx);
                        })),
                )
                .into_any_element()
        };

        let size = self.email_modal_size;
        let handle = |name: &'static str,
                      width: bool,
                      height: bool,
                      cursor: CursorStyle,
                      cx: &mut Context<Self>| {
            div()
                .id(name)
                .absolute()
                .occlude()
                .cursor(cursor)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, _, cx| {
                        this.email_modal_resize = Some(EmailModalResize {
                            width,
                            height,
                            cursor,
                        });
                        cx.notify();
                    }),
                )
        };
        let (out, thick, corner) = (px(-4.), px(6.), px(12.));

        div()
            .id("email-modal-overlay")
            .track_focus(&self.email_modal_focus)
            .absolute()
            .inset_0()
            .occlude()
            .bg(gpui_kit::black().opacity(if p.is_dark() { 0.55 } else { 0.32 }))
            .flex()
            .items_center()
            .justify_center()
            .on_click(cx.listener(move |this, _, window, cx| {
                if backdrop_closes_modal(has_composer) {
                    this.close_email_modal(window, cx);
                }
            }))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if this.composer.is_some() {
                    return;
                }
                let stroke = &event.keystroke;
                if stroke.modifiers.control
                    || stroke.modifiers.platform
                    || stroke.modifiers.alt
                    || stroke.modifiers.shift
                {
                    return;
                }
                let old = this.reader_scroll.offset();
                let page = this.reader_scroll.bounds().size.height * 0.9;
                let next = match stroke.key.as_str() {
                    "pageup" => old.y + page,
                    "pagedown" => old.y - page,
                    "home" => px(0.),
                    "end" => -this.reader_scroll.max_offset().y,
                    "up" => old.y + px(40.),
                    "down" => old.y - px(40.),
                    _ => return,
                };
                this.reader_scroll.set_offset(point(
                    old.x,
                    next.min(px(0.)).max(-this.reader_scroll.max_offset().y),
                ));
                window.prevent_default();
                cx.stop_propagation();
                cx.notify();
            }))
            .child(
                v_flex()
                    .id("email-modal")
                    .relative()
                    .w(size.width)
                    .max_w(relative(MODAL_MAX_WIDTH))
                    .h(size.height)
                    .max_h(relative(MODAL_MAX_HEIGHT))
                    .rounded(px(12.))
                    .occlude()
                    .bg(p.message_surface())
                    .border_1()
                    .border_color(p.divider)
                    .shadow_lg()
                    .on_click(|_, _, cx| cx.stop_propagation())
                    .child(
                        v_flex()
                            .size_full()
                            .min_h_0()
                            .rounded(px(12.))
                            .overflow_hidden()
                            .child(header)
                            .child(
                                div()
                                    .flex_1()
                                    .min_h_0()
                                    .overflow_hidden()
                                    .child(self.render_reader(cx)),
                            ),
                    )
                    // Resize handles outside the inner clipping container so they stay easy to grab.
                    .child(
                        handle(
                            "email-modal-resize-top",
                            false,
                            true,
                            CursorStyle::ResizeUpDown,
                            cx,
                        )
                        .top(out)
                        .left_0()
                        .right_0()
                        .h(thick),
                    )
                    .child(
                        handle(
                            "email-modal-resize-bottom",
                            false,
                            true,
                            CursorStyle::ResizeUpDown,
                            cx,
                        )
                        .bottom(out)
                        .left_0()
                        .right_0()
                        .h(thick),
                    )
                    .child(
                        handle(
                            "email-modal-resize-left",
                            true,
                            false,
                            CursorStyle::ResizeLeftRight,
                            cx,
                        )
                        .left(out)
                        .top_0()
                        .bottom_0()
                        .w(thick),
                    )
                    .child(
                        handle(
                            "email-modal-resize-right",
                            true,
                            false,
                            CursorStyle::ResizeLeftRight,
                            cx,
                        )
                        .right(out)
                        .top_0()
                        .bottom_0()
                        .w(thick),
                    )
                    .child(
                        handle(
                            "email-modal-resize-top-left",
                            true,
                            true,
                            CursorStyle::ResizeUpLeftDownRight,
                            cx,
                        )
                        .top(out)
                        .left(out)
                        .size(corner),
                    )
                    .child(
                        handle(
                            "email-modal-resize-top-right",
                            true,
                            true,
                            CursorStyle::ResizeUpRightDownLeft,
                            cx,
                        )
                        .top(out)
                        .right(out)
                        .size(corner),
                    )
                    .child(
                        handle(
                            "email-modal-resize-bottom-left",
                            true,
                            true,
                            CursorStyle::ResizeUpRightDownLeft,
                            cx,
                        )
                        .bottom(out)
                        .left(out)
                        .size(corner),
                    )
                    .child(
                        handle(
                            "email-modal-resize-bottom-right",
                            true,
                            true,
                            CursorStyle::ResizeUpLeftDownRight,
                            cx,
                        )
                        .bottom(out)
                        .right(out)
                        .size(corner),
                    ),
            )
            .when_some(self.email_modal_resize, |d, resize| {
                d.child(
                    div()
                        .id("email-modal-resizing")
                        .absolute()
                        .inset_0()
                        .occlude()
                        .cursor(resize.cursor)
                        .on_mouse_move(cx.listener(move |this, e: &MouseMoveEvent, window, cx| {
                            if e.dragging() {
                                this.email_modal_size = this.email_modal_size.dragged_to(
                                    resize,
                                    e.position,
                                    window.viewport_size(),
                                );
                            } else {
                                this.email_modal_resize = None;
                            }
                            cx.notify();
                        }))
                        .on_mouse_up(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                this.email_modal_resize = None;
                                cx.notify();
                            }),
                        ),
                )
            })
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::prelude::v1::test;

    fn label(kind: LabelKind, id: &str, name: &str) -> Label {
        Label {
            id: LabelId::from(id),
            account_id: AccountId::from("a"),
            kind,
            name: name.into(),
            provider_id: None,
            color: None,
            sort_order: 0,
        }
    }

    fn message_with_labels(ids: &[&str]) -> Message {
        Message {
            id: MessageId::from("m1"),
            account_id: AccountId::from("a"),
            conversation_id: ConversationId::from("c1"),
            provider_id: None,
            from: Address::named("Alex", "alex@example.com"),
            to: vec![],
            cc: vec![],
            bcc: vec![],
            reply_to: None,
            subject: "Hello".into(),
            snippet: String::new(),
            body: Body::default(),
            date: 1000,
            is_read: true,
            is_starred: false,
            is_outgoing: false,
            label_ids: ids.iter().map(|id| LabelId::from(*id)).collect(),
            attachments: vec![],
            invitation: None,
            rfc_message_id: None,
            in_reply_to: None,
            references: vec![],
            focus_override: None,
            category: None,
            is_important: false,
        }
    }

    #[test]
    fn backdrop_does_not_close_while_composing() {
        assert!(backdrop_closes_modal(false));
        assert!(!backdrop_closes_modal(true));
    }

    #[test]
    fn refresh_preserves_modal_selection_until_permanently_deleted() {
        assert!(keep_modal_selection_after_refresh(true, 1));
        assert!(keep_modal_selection_after_refresh(true, 3));
        assert!(!keep_modal_selection_after_refresh(true, 0));
        assert!(!keep_modal_selection_after_refresh(false, 1));
    }

    #[test]
    fn modal_size_clamps_to_minimums_and_viewport_fraction() {
        let resize = EmailModalResize {
            width: true,
            height: true,
            cursor: CursorStyle::ResizeUpLeftDownRight,
        };
        let window = size(px(1400.), px(900.));
        let small = EmailModalSize::default().dragged_to(resize, point(px(720.), px(460.)), window);
        assert_eq!(small.width, px(MODAL_MIN_WIDTH));
        assert_eq!(small.height, px(MODAL_MIN_HEIGHT));

        let large = EmailModalSize::default().dragged_to(resize, point(px(1390.), px(895.)), window);
        assert_eq!(large.width, px(1400. * MODAL_MAX_WIDTH));
        assert_eq!(large.height, px(900. * MODAL_MAX_HEIGHT));
    }

    #[test]
    fn location_badge_reflects_actions_taken_inside_modal() {
        let labels = vec![
            label(LabelKind::Inbox, "inbox", "Inbox"),
            label(LabelKind::Trash, "trash", "Trash"),
            label(LabelKind::Spam, "junk", "Junk"),
            label(LabelKind::Sent, "sent", "Sent"),
            label(LabelKind::User, "receipts", "Receipts"),
        ];
        let inbox_view = MailboxView::Kind(LabelKind::Inbox);

        assert_eq!(
            modal_location_badge(&inbox_view, &[message_with_labels(&["inbox"])], &labels),
            None
        );
        assert_eq!(
            modal_location_badge(&inbox_view, &[message_with_labels(&[])], &labels),
            Some(("archive", "Archived".into()))
        );
        assert_eq!(
            modal_location_badge(&inbox_view, &[message_with_labels(&["trash"])], &labels),
            Some(("trash", "In Trash".into()))
        );
        assert_eq!(
            modal_location_badge(&inbox_view, &[message_with_labels(&["junk"])], &labels),
            Some(("shield-alert", "In Junk".into()))
        );
        assert_eq!(
            modal_location_badge(&inbox_view, &[message_with_labels(&["receipts"])], &labels),
            Some(("folder", "In Receipts".into()))
        );
        assert_eq!(
            modal_location_badge(
                &MailboxView::Kind(LabelKind::Trash),
                &[message_with_labels(&["inbox"])],
                &labels
            ),
            Some(("inbox", "In Inbox".into()))
        );
    }
}
