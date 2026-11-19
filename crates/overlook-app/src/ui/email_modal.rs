//! Standalone native message windows opened by double-clicking an email in the list.
//!
//! Like pop-out composers and folder windows, a message window is a detached `MailWindow`
//! that shares the store and providers with the main window and never syncs by itself.
//! Actions taken inside the window (Archive, Trash, Move, Star, Mark read/unread, Reply,
//! Forward) act on this email and keep the window on it until the user closes the window
//! or sends their message.

use super::composer_window::{MessageTarget, WindowOwner, WindowSetup};
use super::*;
use crate::assets::{color_icon, icon, ColorIcon as C};

const WINDOW_DEFAULT_WIDTH: f32 = 920.;
const WINDOW_DEFAULT_HEIGHT: f32 = 760.;
const WINDOW_MIN_WIDTH: f32 = 560.;
const WINDOW_MIN_HEIGHT: f32 = 420.;

pub(super) struct MessageWindow {
    pub(super) window: AnyWindowHandle,
    pub(super) editor: WeakEntity<MailWindow>,
    pub(super) account: AccountId,
    target: MessageTarget,
    _closed: Subscription,
}

/// While an email is open in a standalone message window, list refreshes triggered by
/// mailbox actions (archive, trash, folder move, filter changes) must keep the open email
/// loaded as long as at least one message in its conversation still exists in the mail
/// index. Only permanent deletion empties it.
pub(super) fn keep_modal_selection_after_refresh(
    is_message_window: bool,
    remaining_in_index: usize,
) -> bool {
    is_message_window && remaining_in_index > 0
}

/// Effective view used for permanent deletion inside a standalone message window.
///
/// Preserves the Trash boundary for mixed conversations (messages both in Trash and
/// outside Trash) by keeping the originating `view`, while still allowing permanent
/// deletion after an entire conversation is moved to Trash or restored from Trash while
/// staying open in this window.
pub(super) fn delete_forever_view(
    view: &MailboxView,
    thread: &[Message],
    labels: &[Label],
) -> MailboxView {
    if thread.is_empty() {
        return view.clone();
    }
    let trash = labels.iter().find(|l| l.kind == LabelKind::Trash).map(|l| &l.id);
    let all_in_trash = trash.is_some_and(|t| thread.iter().all(|m| m.has_label(t)));
    if all_in_trash {
        return MailboxView::Kind(LabelKind::Trash);
    }
    let none_in_trash = trash.is_none_or(|t| thread.iter().all(|m| !m.has_label(t)));
    if none_in_trash {
        return MailboxView::Kind(LabelKind::Inbox);
    }
    view.clone()
}

/// Title shown in the message window's native title bar.
pub(super) fn message_window_title(subject: Option<&str>, is_outbox: bool) -> String {
    let clean = subject.map(str::trim).filter(|s| !s.is_empty());
    match (clean, is_outbox) {
        (Some(subject), true) => format!("Outbox · {subject}"),
        (Some(subject), false) => subject.to_owned(),
        (None, true) => "Outbox".to_owned(),
        (None, false) => "Overlook · Message".to_owned(),
    }
}

/// Title reported to the OS window manager / macOS Window menu.
pub(super) fn os_window_title(title: &str) -> String {
    if title.starts_with("Overlook") {
        title.to_owned()
    } else {
        format!("{title} — Overlook")
    }
}

/// Subtle location pill shown in the message window's toolbar when the email has moved out
/// of the originating folder (or sits in Trash/Junk), confirming the result of actions
/// taken while staying in the window.
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
    pub(crate) fn is_message_window(&self) -> bool {
        self.message_owner.is_some()
    }

    /// Opens `id` in its own native window, or brings an already-open window forward.
    pub(super) fn open_conversation_window(
        &mut self,
        id: ConversationId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let account = self.account.id.clone();
        let view = self.view.clone();
        self.open_message_window(account, view, MessageTarget::Conversation(id), window, cx);
    }

    /// Opens an Outbox item in its own native window, or brings an already-open window forward.
    pub(super) fn open_outbox_window(
        &mut self,
        id: DraftId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let account = self.account.id.clone();
        let view = self.view.clone();
        self.open_message_window(account, view, MessageTarget::Outbox(id), window, cx);
    }

    /// Opens `target` of `account` in a standalone native window belonging to the main window.
    fn open_message_window(
        &mut self,
        account: AccountId,
        view: MailboxView,
        target: MessageTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(owner) = self.window_owner() {
            let parent = owner.parent.clone();
            let opened = owner
                .window
                .update(cx, |_, window, cx| {
                    parent.update(cx, |this, cx| {
                        this.open_message_window(
                            account.clone(),
                            view.clone(),
                            target.clone(),
                            window,
                            cx,
                        );
                    });
                })
                .is_ok();
            if !opened {
                crate::ui::toast(
                    window,
                    cx,
                    crate::notify::Level::Warning,
                    "Open the main Overlook window to open messages in new windows.",
                );
            }
            return;
        }

        self.message_windows.retain(|m| m.editor.upgrade().is_some());
        if let Some(open) = self
            .message_windows
            .iter()
            .find(|m| m.account == account && m.target == target)
        {
            if open
                .window
                .update(cx, |_, window, _| window.activate_window())
                .is_ok()
            {
                return;
            }
        }

        let exists = match &target {
            MessageTarget::Conversation(id) => self
                .store
                .messages_in_conversation(&account, id)
                .is_ok_and(|msgs| !msgs.is_empty()),
            MessageTarget::Outbox(id) => self.store.draft(id).ok().flatten().is_some(),
        };
        if !exists {
            return;
        }

        let store = self.store.clone();
        let sample = self.sample.clone();
        let accounts = self.accounts.clone();
        let prefs = self.prefs.clone();
        let account_prefs = self.account_prefs.clone();
        let provider_issues = self.provider_issues.clone();
        let remote_allowed = self.remote_allowed.clone();
        let setup = WindowSetup::Message {
            appearance: self.appearance,
            owner: WindowOwner {
                parent: cx.entity(),
                window: window.window_handle(),
            },
            providers: self.providers.clone(),
            account: account.clone(),
            view,
        };
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                None,
                size(px(WINDOW_DEFAULT_WIDTH), px(WINDOW_DEFAULT_HEIGHT)),
                cx,
            ))),
            window_min_size: Some(size(px(WINDOW_MIN_WIDTH), px(WINDOW_MIN_HEIGHT))),
            app_id: Some(crate::APP_ID.into()),
            ..TitleBar::window_options()
        };
        let open_target = target.clone();
        let open_account = account.clone();
        let opened = gpui_kit::open_window(options, cx, move |window, cx| {
            cx.new(|cx| {
                let mut this = MailWindow::new(
                    store,
                    sample,
                    accounts,
                    Some(open_account),
                    setup,
                    window,
                    cx,
                );
                // Settings saved moments ago may not be on disk yet; start from the main window's.
                this.prefs = prefs;
                this.account_prefs = account_prefs;
                this.provider_issues = provider_issues;
                this.remote_allowed = remote_allowed;
                this.open_message_target(open_target, window, cx);
                this
            })
        });
        match opened {
            Ok((handle, editor)) => {
                let parent = cx.entity().downgrade();
                let closed = cx.on_window_closed(move |cx, closed| {
                    if closed == handle.window_id() {
                        parent
                            .update(cx, |this, cx| {
                                this.message_windows
                                    .retain(|m| m.window.window_id() != closed);
                                this.reload(cx);
                            })
                            .ok();
                    }
                });
                self.message_windows.push(MessageWindow {
                    window: handle,
                    editor: editor.downgrade(),
                    account,
                    target,
                    _closed: closed,
                });
            }
            Err(error) => {
                tracing::warn!("could not open a message window: {error}");
                crate::ui::toast(
                    window,
                    cx,
                    crate::notify::Level::Error,
                    "Couldn’t open a message window.",
                );
            }
        }
    }

    /// Loads the target conversation or Outbox item inside a newly opened message window.
    fn open_message_target(
        &mut self,
        target: MessageTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.reload(cx);
        match target {
            MessageTarget::Conversation(id) => {
                self.select_conversation(Some(id), window, cx);
            }
            MessageTarget::Outbox(id) => {
                self.select_outbox_item(id, window, cx);
            }
        }
        window.set_window_title(&self.os_message_window_title());
        self.focus_handle.focus(window, cx);
    }

    fn current_message_window_title(&self) -> String {
        if let Some(id) = &self.outbox_selected {
            let subject = self
                .outbox
                .iter()
                .find(|d| &d.id == id)
                .map(|d| d.display_subject());
            return message_window_title(subject.as_deref(), true);
        }
        if self.thread.is_empty() {
            if let Some(c) = &self.composer {
                return message_window_title(Some(&c.draft.display_subject()), false);
            }
        }
        let subject = self.active_summary().map(|s| s.subject);
        message_window_title(subject.as_deref(), false)
    }

    pub(super) fn os_message_window_title(&self) -> String {
        os_window_title(&self.current_message_window_title())
    }

    pub(crate) fn render_message_window(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.prune_browser_bodies();
        let p = *cx.palette();
        let macos = cfg!(target_os = "macos");
        let reading_conversation = self.outbox_selected.is_none()
            && (self.composer.is_none() || self.composer_is_inline())
            && !self.thread.is_empty();
        let no_modal = self.palette.is_none() && self.quick_look.is_none();
        let menu_can_act = no_modal && self.composer.is_none();
        let message_selected = no_modal && reading_conversation;
        let (in_trash, in_inbox) = self.thread_state();
        let any_unread = self.thread.iter().any(|m| !m.is_read);
        let starred = self.thread.iter().any(|m| m.is_starred);
        let newest = self.thread.iter().max_by_key(|m| m.date).cloned();
        let location = reading_conversation
            .then(|| modal_location_badge(&self.view, &self.thread, &self.labels))
            .flatten();
        let title = self.current_message_window_title();

        let (archive_label, archive_icon, archive_tip) = if in_trash {
            ("Restore", C::Move, "Restore to Inbox")
        } else if !in_inbox {
            ("To Inbox", C::Move, "Move to Inbox")
        } else {
            ("Archive", C::Archive, "Archive")
        };

        let toolbar = reading_conversation.then(|| {
            h_flex()
                .flex_none()
                .items_center()
                .gap(px(4.))
                .h(px(44.))
                .px(px(14.))
                .border_b_1()
                .border_color(p.divider)
                .bg(p.surface)
                .child(
                    Button::new("msg-win-reply")
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
                    Button::new("msg-win-reply-all")
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
                    Button::new("msg-win-forward")
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
                    Button::new("msg-win-archive")
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
                        Button::new("msg-win-delete")
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
                    Button::new("msg-win-move")
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
                    Button::new("msg-win-toggle-read")
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
                    Button::new("msg-win-toggle-star")
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
                            .text_size(crate::ui::ui_px(11.5))
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
                            .text_size(crate::ui::ui_px(11.5))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(p.accent)
                            .child(div().size(px(6.)).rounded_full().bg(p.accent))
                            .child("Unread"),
                    )
                })
                .children(newest.map(|m| {
                    Button::new("msg-win-print")
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
        });

        let body: AnyElement = if self.thread.is_empty()
            && self.outbox_selected.is_none()
            && self.composer.is_none()
        {
            v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .gap_3()
                .text_color(p.muted)
                .child("This message is no longer available.")
                .child(
                    Button::new("close-gone-message")
                        .label("Close")
                        .on_click(|_, window, _| window.remove_window()),
                )
                .into_any_element()
        } else {
            div()
                .flex_1()
                .min_h_0()
                .overflow_hidden()
                .child(self.render_reader(cx))
                .into_any_element()
        };

        let root = v_flex()
            .id("message-window")
            .key_context(WINDOW_CONTEXT)
            .track_focus(&self.focus_handle);
        #[cfg(target_os = "macos")]
        let root = self.native_mail_menu_actions(
            self.native_window_menu_actions(root, cx),
            menu_can_act,
            message_selected,
            window,
            cx,
        );
        #[cfg(not(target_os = "macos"))]
        let _ = (window, menu_can_act);
        root.on_action(cx.listener(|_, _: &QuitOverlook, _window, cx| cx.defer(crate::lifecycle::quit)))
            .on_action(cx.listener(Self::on_compose))
            .on_action(cx.listener(Self::on_focus_search))
            .on_action(cx.listener(Self::on_command_palette))
            .on_action(cx.listener(Self::on_format_bold))
            .on_action(cx.listener(Self::on_format_italic))
            .on_action(cx.listener(Self::on_format_strike))
            .on_action(cx.listener(Self::on_format_code))
            .on_action(cx.listener(Self::on_format_bullets))
            .on_action(cx.listener(Self::on_format_numbers))
            .on_action(cx.listener(Self::on_format_quote))
            .on_action(cx.listener(Self::on_toggle_preview))
            .on_action(cx.listener(Self::on_escape))
            .on_action(cx.listener(Self::on_send))
            .on_action(cx.listener(Self::on_toggle_message_history))
            .on_action(cx.listener(Self::on_pop_out_composer))
            .on_action(cx.listener(Self::on_reply))
            .when(!macos || message_selected, |root| {
                root.on_action(cx.listener(Self::on_reply_all))
                    .on_action(cx.listener(Self::on_forward))
                    .on_action(cx.listener(Self::on_toggle_star))
                    .on_action(cx.listener(Self::on_toggle_read))
                    .on_action(cx.listener(Self::on_move_selected))
            })
            .on_action(cx.listener(Self::on_archive))
            .when(!macos || (message_selected && !in_trash), |root| {
                root.on_action(cx.listener(Self::on_trash))
            })
            .on_action(cx.listener(Self::on_delete_forever))
            .on_action(cx.listener(Self::on_sync_now))
            .on_action(cx.listener(Self::on_switch_mail))
            .on_action(cx.listener(Self::on_switch_calendar))
            .on_action(cx.listener(Self::on_switch_contacts))
            .on_action(cx.listener(Self::on_open_settings))
            .on_action(cx.listener(|this, _: &NewEventAction, _window, cx| {
                this.in_main_window(cx, |main, window, cx| {
                    main.on_new_event(&NewEventAction, window, cx);
                });
            }))
            .on_action(cx.listener(|this, _: &ShowHistory, _window, cx| {
                this.in_main_window(cx, |main, window, cx| main.open_history(window, cx));
            }))
            .on_action(cx.listener(|this, _: &UndoLast, window, cx| this.undo_last(window, cx)))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if this.composer.is_some() || this.palette.is_some() || this.quick_look.is_some() {
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
            .relative()
            .size_full()
            .bg(p.message_surface())
            .text_color(p.foreground)
            .font_family(SharedString::from(
                self.prefs.fonts.effective_ui_family().to_owned(),
            ))
            .text_size(ui_px(13.))
            .child(
                TitleBar::new()
                    .bg(p.chrome())
                    .on_close_window(cx.listener(|this, _, window, cx| {
                        if this.prepare_composer_window_close(window, cx) {
                            window.remove_window();
                        }
                    }))
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .items_center()
                            .px_3()
                            .gap(px(8.))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(ui_px(12.5))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(p.foreground)
                                    .child(title),
                            ),
                    ),
            )
            .children(toolbar)
            .children(self.render_banner(cx))
            .child(body)
            .when(self.quick_look.is_some(), |d| {
                d.child(self.render_quick_look(cx))
            })
            .when(self.palette.is_some(), |d| d.child(self.render_palette(cx)))
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
    fn refresh_preserves_message_window_selection_until_permanently_deleted() {
        assert!(keep_modal_selection_after_refresh(true, 1));
        assert!(keep_modal_selection_after_refresh(true, 3));
        assert!(!keep_modal_selection_after_refresh(true, 0));
        assert!(!keep_modal_selection_after_refresh(false, 1));
    }

    #[test]
    fn window_titles_format_subject_and_outbox_cleanly() {
        assert_eq!(message_window_title(Some("  Q3 Plan  "), false), "Q3 Plan");
        assert_eq!(message_window_title(Some("Invoice"), true), "Outbox · Invoice");
        assert_eq!(message_window_title(Some("   "), true), "Outbox");
        assert_eq!(message_window_title(None, false), "Overlook · Message");

        assert_eq!(os_window_title("Q3 Plan"), "Q3 Plan — Overlook");
        assert_eq!(os_window_title("Overlook · Message"), "Overlook · Message");
    }

    #[test]
    fn delete_forever_view_keeps_trash_boundary_for_mixed_conversations() {
        let labels = vec![
            label(LabelKind::Inbox, "inbox", "Inbox"),
            label(LabelKind::Trash, "trash", "Trash"),
        ];
        let mut trashed = message_with_labels(&["trash"]);
        trashed.id = MessageId::from("m-trash");
        let mut in_inbox = message_with_labels(&["inbox"]);
        in_inbox.id = MessageId::from("m-inbox");
        let mixed = vec![trashed.clone(), in_inbox.clone()];

        // Mixed conversation preserves the originating Trash boundary.
        assert_eq!(
            delete_forever_view(&MailboxView::Kind(LabelKind::Trash), &mixed, &labels),
            MailboxView::Kind(LabelKind::Trash)
        );
        assert_eq!(
            delete_forever_view(&MailboxView::Kind(LabelKind::Inbox), &mixed, &labels),
            MailboxView::Kind(LabelKind::Inbox)
        );
        // Conversation opened in Inbox then moved entirely to Trash can still be deleted forever.
        assert_eq!(
            delete_forever_view(&MailboxView::Kind(LabelKind::Inbox), &[trashed], &labels),
            MailboxView::Kind(LabelKind::Trash)
        );
        // Conversation opened in Trash then restored entirely out of Trash targets non-trash messages.
        assert_eq!(
            delete_forever_view(&MailboxView::Kind(LabelKind::Trash), &[in_inbox], &labels),
            MailboxView::Kind(LabelKind::Inbox)
        );
    }

    #[test]
    fn location_badge_reflects_actions_taken_inside_message_window() {
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
