//! Composer: new message, reply, and forward, with durable autosave to the local store.

use super::widgets::*;
use super::MailWindow;
use crate::assets::icon;
use crate::theme::ActivePalette;
use gpui_kit::component::button::{Button, ButtonVariants};
use super::mail_highlight::{list_marker_len, MailHighlighter, LANGUAGE};
use gpui_kit::component::input::{Editor, EditorState, Input, InputEvent, InputHighlighter, InputState};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::{Disableable, Sizable};
use overlook_core::compose::render_markdown;
use overlook_core::recipients::{RecipientField, RecipientQuery};
use std::rc::Rc;
use gpui_kit::prelude::*;
use gpui_kit::*;
use gpui_kit::base::{h_flex, v_flex};
use overlook_core::status::draft_state_label;
use overlook_core::time::{format_clock, now};
use overlook_core::*;
use std::time::{Duration, Instant};

/// A bounded opening reveal, not a sticky scroll-to-composer mode.
struct ComposerReveal {
    until: Instant,
    focus_to: bool,
}

/// Reveal the beginning of a field, even when the editor is taller than the pane.
fn reveal_offset(offset: Pixels, top: Pixels, height: Pixels, viewport: Pixels, max: Pixels) -> Pixels {
    let bottom = top + height.min(viewport);
    let delta = if top < px(0.) { -top } else if bottom > viewport { viewport - bottom } else { px(0.) };
    (offset + delta).min(px(0.)).max(-max)
}

pub struct Composer {
    pub draft: Draft,
    pub to: Entity<InputState>,
    pub cc: Entity<InputState>,
    pub bcc: Entity<InputState>,
    pub subject: Entity<InputState>,
    pub body: Entity<EditorState>,
    /// The message being replied to or forwarded, for the quoted block.
    pub source: Option<Message>,
    pub preview: bool,
    pub quote_shown: bool,
    /// The signature currently in the body, by id.
    pub signature_id: Option<String>,
    pub show_cc: bool,
    pub show_bcc: bool,
    pub dirty: bool,
    pub last_saved: Option<i64>,
    pub save_generation: u64,
    pub suggestions: Vec<Address>,
    recipient_query: Option<RecipientQuery>,
    pub error: Option<String>,
    pub _subscriptions: Vec<Subscription>,
    reveal: Option<ComposerReveal>,
}

impl Composer {
    fn recipient_input(&self, field: RecipientField) -> &Entity<InputState> {
        match field {
            RecipientField::To => &self.to,
            RecipientField::Cc => &self.cc,
            RecipientField::Bcc => &self.bcc,
        }
    }
}

impl MailWindow {
    /// Opens the composer for `draft`, creating the input entities and autosave wiring.
    pub(super) fn open_composer(&mut self, draft: Draft, window: &mut Window, cx: &mut Context<Self>) {
        crate::watchdog::mark("open composer");
        self.close_composer(window, cx);
        let join = |list: &[Address]| list.iter().map(Address::to_header).collect::<Vec<_>>().join(", ");
        let to = cx.new(|cx| InputState::new(window, cx).placeholder("Recipients").default_value(join(&draft.to)));
        let cc = cx.new(|cx| InputState::new(window, cx).placeholder("Cc").default_value(join(&draft.cc)));
        let bcc = cx.new(|cx| InputState::new(window, cx).placeholder("Bcc").default_value(join(&draft.bcc)));
        let subject = cx.new(|cx| InputState::new(window, cx).placeholder("Add a subject").default_value(draft.subject.clone()));
        let p = *cx.palette();
        let (accent, muted, code_bg) = (p.accent, p.muted, p.chip);
        let factory: gpui_kit::component::input::InputHighlighterFactory = Rc::new(move |language: &str| {
            (language == LANGUAGE).then(|| Box::new(MailHighlighter::new(accent, muted, code_bg)) as Box<dyn InputHighlighter>)
        });
        let body = cx.new(|cx| {
            let mut state = EditorState::new(window, cx)
                .language(LANGUAGE)
                .line_number(false)
                .soft_wrap(true)
                .placeholder("Write your message…")
                .default_value(draft.body_text.clone());
            state.set_highlighter_factory(factory, cx);
            state.set_indent_guides(false, window, cx);
            state
        });
        let source = draft.in_reply_to.as_ref().and_then(|id| self.store.message(id).ok().flatten());
        let signature_id = {
            let ap = self.account_prefs_for(&self.account.id);
            ap.default_signature(draft.kind != DraftKind::New).filter(|s| draft.body_text.contains(s.body.trim_end())).map(|s| s.id.clone())
        };

        let mut subs = Vec::new();
        for state in [&to, &cc, &bcc, &subject] {
            subs.push(cx.subscribe_in(state, window, |this, _state, event: &InputEvent, window, cx| match event {
                InputEvent::Change => this.composer_changed(window, cx),
                // Ctrl+Enter sends from any field; plain Enter in a header field moves on.
                InputEvent::PressEnter { secondary: true, .. } => this.send_composer(window, cx),
                InputEvent::PressEnter { .. } => this.focus_next_composer_field(window, cx),
                InputEvent::Focus => this.refresh_recipient_suggestions(window, cx),
                _ => {}
            }));
        }
        // Caret movement and selecting existing reply recipients do not emit
        // Change. Observe them too, without dirtying/autosaving the draft.
        for state in [&to, &cc, &bcc] {
            subs.push(cx.observe_in(state, window, |this, state, window, cx| {
                if state.read(cx).focus_handle(cx).is_focused(window) {
                    this.refresh_recipient_suggestions(window, cx);
                }
            }));
        }
        subs.push(cx.subscribe_in(&body, window, |this, _state, event: &InputEvent, window, cx| match event {
            InputEvent::Change => this.composer_changed(window, cx),
            InputEvent::PressEnter { secondary: true, .. } => this.send_composer(window, cx),
            InputEvent::PressEnter { .. } => this.continue_list(window, cx),
            InputEvent::Focus => this.refresh_recipient_suggestions(window, cx),
            _ => {}
        }));

        let focus_to = draft.to.is_empty();
        let reveal = (draft.kind != DraftKind::New && self.selected.is_some()).then(|| ComposerReveal {
            until: Instant::now() + Duration::from_secs(2),
            focus_to,
        });
        let show_cc = !draft.cc.is_empty();
        let show_bcc = !draft.bcc.is_empty();
        self.composer = Some(Composer {
            draft,
            to: to.clone(),
            cc,
            bcc,
            subject,
            body: body.clone(),
            source,
            preview: false,
            quote_shown: false,
            signature_id,
            show_cc,
            show_bcc,
            dirty: false,
            last_saved: None,
            save_generation: 0,
            suggestions: Vec::new(),
            recipient_query: None,
            error: None,
            _subscriptions: subs,
            reveal,
        });
        if focus_to {
            to.update(cx, |s, cx| s.focus(window, cx));
        } else {
            body.update(cx, |s, cx| s.focus(window, cx));
        }
        cx.notify();
    }

    pub(super) fn cancel_composer_reveal(&mut self) {
        if let Some(c) = self.composer.as_mut() { c.reveal = None; }
    }

    /// Measure the actual field in prepaint, then scroll only after the scroll
    /// handle has its final layout bounds. Re-measure briefly as browser height
    /// settles. Closing/replacing the draft or user scrolling cancels callbacks.
    fn composer_reveal_anchor(&self, focus_to: bool, cx: &Context<Self>) -> AnyElement {
        let Some(c) = self.composer.as_ref() else { return div().into_any_element() };
        if !c.reveal.as_ref().is_some_and(|r| r.focus_to == focus_to) {
            return div().into_any_element();
        }
        let body = c.body.entity_id();
        let view = cx.entity().downgrade();
        let scroll = self.reader_scroll.clone();
        canvas(move |bounds, window, _| {
            let measured_offset = scroll.offset().y;
            let view = view.clone();
            window.on_next_frame(move |_, cx| {
                let _ = view.update(cx, |this, cx| {
                    let Some(c) = this.composer.as_mut().filter(|c| c.body.entity_id() == body) else { return };
                    let Some(reveal) = c.reveal.as_ref() else { return };
                    if Instant::now() >= reveal.until {
                        c.reveal = None;
                        return;
                    }
                    let viewport = this.reader_scroll.bounds();
                    let offset = this.reader_scroll.offset();
                    if viewport.size.height > px(0.) {
                        let top = bounds.top() - viewport.top() + offset.y - measured_offset;
                        let next = reveal_offset(offset.y, top, bounds.size.height, viewport.size.height, this.reader_scroll.max_offset().y);
                        if next != offset.y {
                            this.reader_scroll.set_offset(point(offset.x, next));
                        }
                    }
                    // Drives only the bounded opening interval; never re-focuses
                    // after the user has chosen another field or control.
                    // notify schedules the next render. request_animation_frame
                    // is render-phase-only and would panic in this callback.
                    cx.notify();
                });
            });
        }, |_, _, _, _| {}).absolute().inset_0().into_any_element()
    }

    /// Enter in To/Cc/Bcc/Subject moves focus to the next field, ending in the body.
    fn focus_next_composer_field(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(c) = self.composer.as_ref() else { return };
        let order: Vec<Entity<InputState>> = {
            let mut v = vec![c.to.clone()];
            if c.show_cc { v.push(c.cc.clone()); }
            if c.show_bcc { v.push(c.bcc.clone()); }
            v.push(c.subject.clone());
            v
        };
        let focused = order.iter().position(|s| s.read(cx).focus_handle(cx).is_focused(window));
        match focused {
            Some(i) if i + 1 < order.len() => {
                let next = order[i + 1].clone();
                next.update(cx, |s, cx| s.focus(window, cx));
            }
            _ => {
                let body = c.body.clone();
                body.update(cx, |s, cx| s.focus(window, cx));
            }
        }
    }

    /// Reads the inputs back into the draft.
    pub(super) fn sync_composer_fields(&mut self, cx: &mut Context<Self>) {
        let Some(c) = self.composer.as_mut() else { return };
        c.draft.to = Address::parse_list(&c.to.read(cx).value());
        c.draft.cc = Address::parse_list(&c.cc.read(cx).value());
        c.draft.bcc = Address::parse_list(&c.bcc.read(cx).value());
        c.draft.subject = c.subject.read(cx).value().trim().to_owned();
        c.draft.body_text = c.body.read(cx).value().to_string();
        c.draft.body_html = (!c.draft.body_text.trim().is_empty()).then(|| render_markdown(&c.draft.body_text));
        c.draft.updated_at = now();
    }

    fn composer_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_composer_fields(cx);
        let Some(c) = self.composer.as_mut() else { return };
        c.dirty = true;
        c.error = None;
        c.save_generation += 1;
        let generation = c.save_generation;

        self.refresh_recipient_suggestions(window, cx);
        cx.notify();

        // Debounced durable autosave.
        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(700)).await;
            let _ = this.update_in(cx, |this, _window, cx| {
                if this.composer.as_ref().is_some_and(|c| c.save_generation == generation && c.dirty) {
                    this.autosave_composer(generation, cx);
                }
            });
        })
        .detach();
    }

    fn refresh_recipient_suggestions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(c) = self.composer.as_ref() else { return };
        let query = [RecipientField::To, RecipientField::Cc, RecipientField::Bcc]
            .into_iter()
            .filter(|field| match field {
                RecipientField::To => true,
                RecipientField::Cc => c.show_cc,
                RecipientField::Bcc => c.show_bcc,
            })
            .find_map(|field| {
                c.recipient_input(field).update(cx, |input, cx| {
                    (input.focus_handle(cx).is_focused(window) && input.marked_text_range(window, cx).is_none())
                        .then(|| RecipientQuery::new(field, &input.value(), input.selected_range()))
                        .flatten()
                })
            });
        // Input notifications include cursor blinking. Only search again when
        // the field, text or selection actually changes.
        if query == c.recipient_query { return; }
        let suggestions = if let Some(query) = &query {
            let token = query.text();
            // Address book first, then people written to before, without duplicates.
            let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
            // Both from memory: no database read per keystroke.
            let contacts = overlook_store::contacts_matching(&self.contacts, token, 5).into_iter().filter(|c| !c.email.is_empty()).map(|c| c.address());
            let known = self.mail_index.known_recipients(token, 5).into_iter();
            contacts
                .chain(known)
                .filter(|a| !a.email.eq_ignore_ascii_case(&self.account.email))
                .filter(|a| !token.eq_ignore_ascii_case(&a.email) && !token.eq_ignore_ascii_case(&a.to_header()))
                .filter(|a| seen.insert(a.email.to_ascii_lowercase()))
                .take(6)
                .collect()
        } else {
            Vec::new()
        };
        let c = self.composer.as_mut().unwrap();
        c.recipient_query = query;
        c.suggestions = suggestions;
        cx.notify();
    }

    /// The idle autosave: the write (a durable transaction that may wait for a sync's
    /// write) runs off the UI thread, so typing never hitches. "Saved" shows only after it
    /// commits, and only if nothing was typed meanwhile.
    fn autosave_composer(&mut self, generation: u64, cx: &mut Context<Self>) {
        self.sync_composer_fields(cx);
        let Some(c) = self.composer.as_ref() else { return };
        if c.draft.is_empty() && c.last_saved.is_none() {
            return;
        }
        let draft = c.draft.clone();
        let store = self.store.clone();
        cx.spawn(async move |this, cx| {
            let result = cx.background_spawn(async move { store.save_editing_draft(&draft).map(|_| ()) }).await;
            let _ = this.update(cx, |this, cx| {
                let Some(c) = this.composer.as_mut() else { return };
                match result {
                    Ok(()) if c.save_generation == generation => {
                        c.dirty = false;
                        c.last_saved = Some(now());
                    }
                    Ok(()) => {}
                    Err(e) => c.error = Some(format!("Couldn’t save on this device: {e}")),
                }
                this.reload_drafts();
                cx.notify();
            });
        })
        .detach();
    }

    /// Writes the draft to the store. Only after this succeeds does the UI say it is saved.
    pub(super) fn save_composer(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.sync_composer_fields(cx);
        let Some(c) = self.composer.as_mut() else { return false };
        if c.draft.is_empty() && c.last_saved.is_none() {
            return false;
        }
        match self.store.save_draft(&c.draft) {
            Ok(()) => {
                c.dirty = false;
                c.last_saved = Some(now());
                self.reload_drafts();
                cx.notify();
                true
            }
            Err(e) => {
                c.error = Some(format!("Couldn’t save on this device: {e}"));
                cx.notify();
                false
            }
        }
    }

    pub(super) fn close_composer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.composer.is_some() {
            self.save_composer(window, cx);
            if let Some(c) = self.composer.take() {
                if c.draft.is_empty() && c.last_saved.is_none() {
                    self.unwanted_drafts.insert(c.draft.id.clone());
                }
                if c.draft.is_empty() && c.last_saved.is_some() {
                    // Nothing was written; do not leave an empty draft behind.
                    let _ = self.store.delete_draft(&c.draft.account_id, &c.draft.id);
                    self.reload_drafts();
                }
            }
            self.list_focus.focus(window, cx);
            cx.notify();
        }
    }

    pub(super) fn discard_composer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(c) = self.composer.take() {
            if c.last_saved.is_none() {
                // Its first save may still be running; it removes the draft when it lands.
                self.unwanted_drafts.insert(c.draft.id.clone());
            }
            let _ = self.store.delete_draft(&c.draft.account_id, &c.draft.id);
            self.reload_drafts();
            crate::ui::toast(window, cx, crate::notify::Level::Info, "Draft discarded.");
            self.list_focus.focus(window, cx);
            cx.notify();
        }
    }

    pub(super) fn send_composer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        crate::watchdog::mark("send");
        self.sync_composer_fields(cx);
        let Some(c) = self.composer.as_mut() else { return };
        if let Err(e) = c.draft.validate_for_send() {
            c.error = Some(e.to_string());
            let to = c.to.clone();
            to.update(cx, |s, cx| s.focus(window, cx));
            cx.notify();
            return;
        }
        if c.draft.body_text.trim().is_empty() && c.draft.subject.trim().is_empty() {
            c.error = Some("Add a subject or a message before sending.".into());
            cx.notify();
            return;
        }
        let draft = c.draft.clone();
        match self.store.queue_send(&draft) {
            Ok(_) => {
                self.composer = None;
                self.reload(cx);
                let online = self.connected(&self.account.id);
                let delay = self.prefs.undo_send_secs;
                let text = match (online, delay) {
                    (false, _) => "Waiting for connection. We’ll send this when you’re back online.".to_owned(),
                    (true, 0) => "Sending…".to_owned(),
                    (true, s) => format!("Sending in {s} seconds."),
                };
                // The message is already safe in the Outbox; Undo takes it back to the composer
                // until the sync that sends it starts.
                let id = draft.id.clone();
                let account = draft.account_id.clone();
                self.toast_with_undo(text, move |this, window, cx| this.undo_send(account.clone(), id.clone(), window, cx), window, cx);
                self.list_focus.focus(window, cx);
                if online {
                    if delay == 0 {
                        self.sync_now(window, cx);
                    } else {
                        cx.spawn_in(window, async move |this, cx| {
                            cx.background_executor().timer(std::time::Duration::from_millis(u64::from(delay) * 1000 + 300)).await;
                            let _ = this.update_in(cx, |this, window, cx| this.sync_now(window, cx));
                        })
                        .detach();
                    }
                }
            }
            Err(e) => {
                if let Some(c) = self.composer.as_mut() {
                    c.error = Some(e.to_string());
                }
            }
        }
        cx.notify();
    }

    /// Takes a just-sent message back out of the Outbox into the composer.
    pub(super) fn undo_send(&mut self, account: AccountId, id: DraftId, window: &mut Window, cx: &mut Context<Self>) {
        match self.store.cancel_send(&account, &id) {
            Ok(_) => {
                self.reload(cx);
                self.open_draft(id, window, cx);
            }
            Err(e) => crate::ui::toast(window, cx, crate::notify::Level::Warning, format!("Too late to undo: {e}")),
        }
    }

    pub(super) fn attach_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(c) = self.composer.as_ref() else { return };
        let draft_id = c.draft.id.clone();
        let receiver = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: true, prompt: Some("Attach".into()) });
        let outgoing_dir = self.outgoing_dir.join(draft_id.as_str());
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = receiver.await else { return };
            let Ok(existing) = this.update(cx, |this, _| this.composer.as_ref().map(|c| c.draft.attachments.iter().map(|a| a.size_bytes).sum::<u64>()).unwrap_or(0)) else { return };
            // Reading, scanning and copying stay off the UI thread.
            let (added, refused) = cx.background_spawn(async move { prepare_attachments(paths, &outgoing_dir, existing) }).await;
            let _ = this.update_in(cx, |this, window, cx| {
                if let Some(first) = refused.first() {
                    let more = if refused.len() > 1 { format!(" ({} files not attached.)", refused.len()) } else { String::new() };
                    crate::ui::toast(window, cx, crate::notify::Level::Warning, format!("{first}{more}"));
                }
                if let Some(c) = this.composer.as_mut() {
                    if c.draft.id == draft_id {
                        c.draft.attachments.extend(added);
                        c.dirty = true;
                        this.save_composer(window, cx);
                    }
                }
            });
        })
        .detach();
    }

    pub(super) fn render_composer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = *cx.palette();
        let Some(c) = self.composer.as_ref() else { return div().into_any_element() };
        let title = match c.draft.kind {
            DraftKind::New => "New message",
            DraftKind::Reply => "Reply",
            DraftKind::ReplyAll => "Reply all",
            DraftKind::Forward => "Forward",
        };
        let inline = c.draft.kind != DraftKind::New;
        let saved_text = match (c.dirty, c.last_saved) {
            (true, _) => "Saving…".to_owned(),
            (false, Some(ts)) => {
                let (_, time, _) = overlook_core::time::civil(ts, self.utc_offset);
                format!("{} · {}", draft_state_label(&c.draft.state), format_clock(time))
            }
            (false, None) => "Not saved yet".to_owned(),
        };
        let owner = self.owner();

        let field_label = |text: &'static str| {
            div().w(px(56.)).flex_none().text_size(px(12.)).text_color(p.muted).child(text)
        };
        let field_row = |label: &'static str, input: AnyElement| {
            h_flex()
                .items_center()
                .gap(px(8.))
                .px(px(16.))
                .min_h(px(40.))
                .border_b_1()
                .border_color(p.divider)
                .child(field_label(label))
                .child(div().flex_1().min_w_0().child(input))
        };

        let mut suggestions: Option<Div> = c.recipient_query.as_ref().filter(|_| !c.suggestions.is_empty()).map(|query| {
            h_flex()
                .flex_wrap()
                .gap(px(6.))
                .px(px(80.))
                .py(px(6.))
                .border_b_1()
                .border_color(p.divider)
                .children(c.suggestions.iter().cloned().map(|a| {
                    let query = query.clone();
                    let draft_id = c.draft.id.clone();
                    Button::new(SharedString::from(format!("suggest-{}", a.email)))
                        .xsmall()
                        .label(if a.name.is_some() { format!("{} · {}", a.display_name(), a.email) } else { a.email.clone() })
                        .on_click(cx.listener(move |this, _, window, cx| this.accept_suggestion(&draft_id, &query, &a, window, cx)))
                }))
        });
        let suggestion_field = c.recipient_query.as_ref().map(|q| q.field);

        let attachments: Option<Div> = (!c.draft.attachments.is_empty()).then(|| {
            h_flex()
                .flex_wrap()
                .gap(px(8.))
                .px(px(16.))
                .py(px(8.))
                .children(c.draft.attachments.iter().map(|a| {
                    let id = a.id.clone();
                    h_flex()
                        .items_center()
                        .gap(px(8.))
                        .px(px(10.))
                        .py(px(6.))
                        .rounded(px(7.))
                        .border_1()
                        .border_color(p.divider)
                        .text_size(px(12.))
                        .child(icon("paperclip").size(px(13.)).text_color(p.muted))
                        .child(a.filename.clone())
                        .child(div().text_color(p.muted).child(overlook_core::human_size(a.size_bytes)))
                        .child(
                            Button::new(SharedString::from(format!("rm-att-{}", a.id)))
                                .ghost()
                                .xsmall()
                                .icon(icon("x").size(px(12.)))
                                .on_click(cx.listener(move |this, _, window, cx| this.remove_attachment(id.clone(), window, cx))),
                        )
                }))
        });

        let error = c.error.clone().map(|e| {
            h_flex()
                .items_center()
                .gap(px(8.))
                .px(px(16.))
                .py(px(8.))
                .text_size(px(12.))
                .text_color(p.error)
                .child(icon("circle-alert").size(px(14.)).text_color(p.error))
                .child(e)
        });

        v_flex()
            .when(inline, |d| d.mx(px(28.)).mt(px(18.)).mb(px(28.)).rounded(px(10.)).border_1().border_color(p.divider))
            .when(!inline, |d| d.size_full())
            .bg(p.message_surface())
            .overflow_hidden()
            .child(
                h_flex()
                    .items_center()
                    .px(px(16.))
                    .h(px(46.))
                    .border_b_1()
                    .border_color(p.divider)
                    .child(div().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).child(title))
                    .child(div().flex_1())
                    .child(
                        h_flex()
                            .items_center()
                            .gap(px(5.))
                            .text_size(px(11.))
                            .text_color(if c.dirty { p.muted } else { p.success })
                            .child(icon(if c.dirty { "loader-circle" } else { "check" }).size(px(13.)))
                            .child(saved_text),
                    )
                    .child(div().w(px(8.)))
                    .child(
                        Button::new("close-composer")
                            .ghost()
                            .small()
                            .icon(icon("x").size(px(16.)))
                            .tooltip("Save and close (Esc)")
                            .on_click(cx.listener(|this, _, window, cx| this.close_composer(window, cx))),
                    ),
            )
            .child(field_row(
                "From",
                div().text_size(px(13.)).child(format!("{} <{}>", owner.display_name(), owner.email)).into_any_element(),
            ))
            .child(
                h_flex()
                    .items_center()
                    .gap(px(8.))
                    .px(px(16.))
                    .min_h(px(40.))
                    .border_b_1()
                    .border_color(p.divider)
                    .child(field_label("To"))
                    .child(div().relative().flex_1().min_w_0().child(Input::new(&c.to).appearance(false))
                        .child(self.composer_reveal_anchor(true, cx)))
                    .when(!c.show_cc, |d| {
                        d.child(Button::new("show-cc").ghost().xsmall().label("Cc").on_click(cx.listener(|this, _, _w, cx| {
                            if let Some(c) = this.composer.as_mut() { c.show_cc = true; }
                            cx.notify();
                        })))
                    })
                    .when(!c.show_bcc, |d| {
                        d.child(Button::new("show-bcc").ghost().xsmall().label("Bcc").on_click(cx.listener(|this, _, _w, cx| {
                            if let Some(c) = this.composer.as_mut() { c.show_bcc = true; }
                            cx.notify();
                        })))
                    }),
            )
            .when(suggestion_field == Some(RecipientField::To), |d| d.children(suggestions.take()))
            .when(c.show_cc, |d| d.child(field_row("Cc", Input::new(&c.cc).appearance(false).into_any_element()))
                .when(suggestion_field == Some(RecipientField::Cc), |d| d.children(suggestions.take())))
            .when(c.show_bcc, |d| d.child(field_row("Bcc", Input::new(&c.bcc).appearance(false).into_any_element()))
                .when(suggestion_field == Some(RecipientField::Bcc), |d| d.children(suggestions.take())))
            .child(field_row("Subject", Input::new(&c.subject).appearance(false).into_any_element()))
            .child(self.render_format_toolbar(c, &p, cx))
            .child(if c.preview {
                div()
                    .id("composer-preview")
                    .flex_1()
                    .min_h(px(if inline { 180. } else { 240. }))
                    .overflow_y_scroll()
                    .px(px(16.))
                    .py(px(12.))
                    .text_size(px(15.))
                    .line_height(px(24.))
                    .child({
                        let html = overlook_core::compose::outgoing_bodies(&c.draft, None).html;
                        self.browser_message_body(
                            format!("preview-{}", c.draft.id), &html, None,
                            self.prefs.remote_images == RemoteImages::Allow, p.is_dark(), cx,
                        ).0
                    })
                    .into_any_element()
            } else {
                div()
                    .relative()
                    .flex_1()
                    .min_h(px(if inline { 180. } else { 240. }))
                    .px(px(10.))
                    .py(px(6.))
                    .child(Editor::new(&c.body).appearance(false).bordered(false).h_full().font_family("Inter").text_size(px(14.)).line_height(px(22.)))
                    .child(self.composer_reveal_anchor(false, cx))
                    .into_any_element()
            })
            .children(self.render_quote_block(c, &p, cx))
            .children(attachments)
            .children(error)
            .child(
                h_flex()
                    .items_center()
                    .gap(px(6.))
                    .px(px(12.))
                    .py(px(10.))
                    .border_t_1()
                    .border_color(p.divider)
                    .child(
                        Button::new("send")
                            .primary()
                            .label("Send")
                            .icon(icon("send").size(px(15.)))
                            .tooltip("Send (Ctrl Enter)")
                            .on_click(cx.listener(|this, _, window, cx| this.send_composer(window, cx))),
                    )
                    .child(
                        Button::new("attach")
                            .ghost()
                            .small()
                            .icon(icon("paperclip").size(px(16.)))
                            .tooltip("Attach files")
                            .on_click(cx.listener(|this, _, window, cx| this.attach_files(window, cx))),
                    )
                    .child(div().flex_1())
                    .child(kbd("Ctrl Enter", &p))
                    .child(
                        Button::new("discard")
                            .ghost()
                            .small()
                            .icon(icon("trash").size(px(16.)).text_color(p.error))
                            .tooltip("Discard draft")
                            .on_click(cx.listener(|this, _, window, cx| this.discard_composer(window, cx))),
                    ),
            )
            .into_any_element()
    }

    fn render_format_toolbar(&self, c: &Composer, p: &crate::theme::Palette, cx: &mut Context<Self>) -> Div {
        let preview = c.preview;
        let tool = |id: &'static str, icon_name: &'static str, tip: &'static str, cx: &mut Context<Self>, action: fn(&mut Self, &mut Window, &mut Context<Self>)| {
            Button::new(id)
                .ghost()
                .xsmall()
                .icon(icon(icon_name).size(px(15.)))
                .tooltip(tip)
                .disabled(preview)
                .on_click(cx.listener(move |this, _, window, cx| action(this, window, cx)))
        };
        h_flex()
            .items_center()
            .gap(px(2.))
            .px(px(10.))
            .h(px(36.))
            .border_b_1()
            .border_color(p.divider)
            .child(tool("fmt-bold", "bold", "Bold (Ctrl B)", cx, |t, w, cx| t.format_wrap("**", w, cx)))
            .child(tool("fmt-italic", "italic", "Italic (Ctrl I)", cx, |t, w, cx| t.format_wrap("*", w, cx)))
            .child(tool("fmt-strike", "strikethrough", "Strikethrough (Ctrl Shift X)", cx, |t, w, cx| t.format_wrap("~~", w, cx)))
            .child(tool("fmt-code", "code", "Code (Ctrl Shift C)", cx, |t, w, cx| t.format_wrap("`", w, cx)))
            .child(div().w(px(1.)).h(px(18.)).mx(px(4.)).bg(p.divider))
            .child(tool("fmt-link", "link", "Link (Ctrl K)", cx, |t, w, cx| t.format_link(w, cx)))
            .child(tool("fmt-bullets", "list", "Bulleted list (Ctrl Shift 8)", cx, |t, w, cx| t.format_lines(LinePrefix::Bullet, w, cx)))
            .child(tool("fmt-numbers", "list-ordered", "Numbered list (Ctrl Shift 7)", cx, |t, w, cx| t.format_lines(LinePrefix::Number, w, cx)))
            .child(tool("fmt-quote", "text-quote", "Quote (Ctrl Shift 9)", cx, |t, w, cx| t.format_lines(LinePrefix::Quote, w, cx)))
            .child(div().flex_1())
            .child({
                let ap = self.account_prefs_for(&self.account.id);
                let current = c.signature_id.clone();
                let choices: Vec<(Option<String>, String)> = std::iter::once((None, "None".to_string())).chain(ap.signatures.iter().map(|s| (Some(s.id.clone()), s.name.clone()))).collect();
                let label = current.as_deref().and_then(|id| ap.signature_by_id(id)).map(|s| s.name.clone()).unwrap_or_else(|| "No signature".into());
                let t = cx.entity().downgrade();
                Button::new("fmt-signature")
                    .ghost()
                    .xsmall()
                    .icon(icon("signature").size(px(15.)))
                    .label(label)
                    .dropdown_caret(true)
                    .tooltip("Signature")
                    .dropdown_menu(move |mut menu, _window, _cx| {
                        for (id, name) in &choices {
                            let t = t.clone();
                            let id = id.clone();
                            menu = menu.item(PopupMenuItem::new(name.clone()).checked(id == current).on_click(move |_, window, cx| {
                                let _ = t.update(cx, |this, cx| this.set_signature(id.clone(), window, cx));
                            }));
                        }
                        menu
                    })
            })
            .child(
                Button::new("fmt-preview")
                    .ghost()
                    .xsmall()
                    .icon(icon(if preview { "pencil" } else { "eye" }).size(px(15.)))
                    .label(if preview { "Edit" } else { "Preview" })
                    .tooltip("Show the message as recipients will see it (Ctrl Shift P)")
                    .on_click(cx.listener(|this, _, window, cx| this.toggle_preview(window, cx))),
            )
    }

    /// The collapsed original under the editor of a reply or forward.
    fn render_quote_block(&self, c: &Composer, p: &crate::theme::Palette, cx: &mut Context<Self>) -> Option<Div> {
        if !c.draft.include_quote {
            return None;
        }
        let source = c.source.as_ref()?;
        let who = source.from.display_name();
        let when = overlook_core::time::format_full(source.date, self.utc_offset);
        let label = match c.draft.kind {
            DraftKind::Forward => format!("Forwarding the message from {who} · {when}"),
            _ => format!("Quoting {who} · {when}"),
        };
        let shown = c.quote_shown;
        let html = shown.then(|| {
            let allow_remote = self.remote_allowed.contains(&source.id) || self.prefs.remote_images == RemoteImages::Allow;
            let safe = overlook_core::compose::quote_preview(&c.draft, source, overlook_core::sanitize::RemoteContent::Allowed);
            self.browser_message_body(format!("quote-{}", c.draft.id), &safe, Some(source), allow_remote, p.is_dark(), cx).0
        });
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
                        .child(icon("text-quote").size(px(14.)))
                        .child(div().flex_1().truncate().child(label))
                        .child(Button::new("quote-toggle").ghost().xsmall().label(if shown { "Hide" } else { "Show" }).on_click(cx.listener(|this, _, _w, cx| {
                            if let Some(c) = this.composer.as_mut() {
                                c.quote_shown = !c.quote_shown;
                            }
                            cx.notify();
                        })))
                        .child(Button::new("quote-remove").ghost().xsmall().icon(icon("x").size(px(13.))).tooltip("Send without the quoted message").on_click(cx.listener(|this, _, window, cx| {
                            if let Some(c) = this.composer.as_mut() {
                                c.draft.include_quote = false;
                                c.dirty = true;
                            }
                            this.save_composer(window, cx);
                        }))),
                )
                .when_some(html, |d, html| {
                    d.child(
                        div()
                            .pl(px(12.))
                            .border_l_2()
                            .border_color(p.divider)
                            .text_size(px(14.))
                            .line_height(px(22.))
                            .text_color(p.muted)
                            .child(html),
                    )
                }),
        )
    }

    /// Swaps the signature block at the end of the body for another one, or removes it.
    pub(super) fn set_signature(&mut self, id: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let ap = self.account_prefs_for(&self.account.id);
        let Some(c) = self.composer.as_ref() else { return };
        let old = c.signature_id.as_deref().and_then(|i| ap.signature_by_id(i)).map(|s| s.body.trim_end().to_owned());
        let new = id.as_deref().and_then(|i| ap.signature_by_id(i)).map(|s| s.body.trim_end().to_owned());
        let body = c.body.clone();
        body.update(cx, |s, cx| {
            let mut text = s.value().to_string();
            if let Some(old) = old.filter(|o| !o.is_empty()) {
                if let Some(pos) = text.rfind(&old) {
                    let before = text[..pos].trim_end_matches('\n').len();
                    text = format!("{}{}", &text[..before], &text[pos + old.len()..]);
                }
            }
            if let Some(new) = new.filter(|n| !n.is_empty()) {
                let trimmed = text.trim_end_matches('\n').len();
                text = format!("{}\n\n{}", &text[..trimmed], new);
            }
            s.set_value(text, window, cx);
        });
        if let Some(c) = self.composer.as_mut() {
            c.signature_id = id;
        }
        self.composer_changed(window, cx);
    }

    pub(super) fn composer_body_focused(&self, window: &Window, cx: &App) -> bool {
        self.composer.as_ref().is_some_and(|c| !c.preview && c.body.read(cx).focus_handle(cx).is_focused(window))
    }

    pub(super) fn toggle_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_composer_fields(cx);
        let Some(c) = self.composer.as_mut() else { return };
        c.preview = !c.preview;
        if !c.preview {
            let body = c.body.clone();
            body.update(cx, |s, cx| s.focus(window, cx));
        }
        cx.notify();
    }

    /// Wraps the selection in `mark` (bold, italic, strike, code), or unwraps it when it
    /// already is. With nothing selected the marks are inserted and the cursor sits between.
    pub(super) fn format_wrap(&mut self, mark: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(c) = self.composer.as_ref() else { return };
        let body = c.body.clone();
        let m = mark.len();
        body.update(cx, |s, cx| {
            let text = s.value().to_string();
            let range = s.selected_range();
            let (start, end) = (range.start.min(text.len()), range.end.min(text.len()));
            let sel = text[start..end].to_string();
            if start >= m && text[..start].ends_with(mark) && text[end..].starts_with(mark) {
                // Marks around the selection: remove them.
                s.set_selected_range(start - m..end + m, cx);
                s.replace(sel.clone(), window, cx);
                s.set_selected_range(start - m..start - m + sel.len(), cx);
            } else if sel.len() >= 2 * m && sel.starts_with(mark) && sel.ends_with(mark) {
                let inner = sel[m..sel.len() - m].to_string();
                s.replace(inner.clone(), window, cx);
                s.set_selected_range(start..start + inner.len(), cx);
            } else {
                s.replace(format!("{mark}{sel}{mark}"), window, cx);
                s.set_selected_range(start + m..start + m + sel.len(), cx);
            }
            s.focus(window, cx);
        });
        self.composer_changed(window, cx);
    }

    /// Inserts a Markdown link around the selection, leaving the URL selected for typing.
    pub(super) fn format_link(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(c) = self.composer.as_ref() else { return };
        let body = c.body.clone();
        body.update(cx, |s, cx| {
            let text = s.value().to_string();
            let range = s.selected_range();
            let (start, end) = (range.start.min(text.len()), range.end.min(text.len()));
            let sel = text[start..end].to_string();
            let is_url = sel.starts_with("http://") || sel.starts_with("https://") || sel.starts_with("www.");
            let (label, url) = if sel.is_empty() {
                ("text".to_string(), "https://".to_string())
            } else if is_url {
                (sel.clone(), sel.clone())
            } else {
                (sel.clone(), "https://".to_string())
            };
            s.replace(format!("[{label}]({url})"), window, cx);
            let url_start = start + 1 + label.len() + 2;
            s.set_selected_range(url_start..url_start + url.len(), cx);
            s.focus(window, cx);
        });
        self.composer_changed(window, cx);
    }

    /// Adds or removes a line prefix (bullets, numbers, quote) on every selected line.
    pub(super) fn format_lines(&mut self, prefix: LinePrefix, window: &mut Window, cx: &mut Context<Self>) {
        let Some(c) = self.composer.as_ref() else { return };
        let body = c.body.clone();
        body.update(cx, |s, cx| {
            let text = s.value().to_string();
            let range = s.selected_range();
            let (sel_start, sel_end) = (range.start.min(text.len()), range.end.min(text.len()));
            let start = text[..sel_start].rfind('\n').map(|i| i + 1).unwrap_or(0);
            let end = text[sel_end..].find('\n').map(|i| sel_end + i).unwrap_or(text.len());
            let block = &text[start..end];
            let lines: Vec<&str> = block.split('\n').collect();
            let has = |l: &str| match prefix {
                LinePrefix::Quote => l.trim_start().starts_with("> ") || l.trim_start() == ">",
                _ => list_marker_len(l.trim_start()).is_some(),
            };
            let all_have = lines.iter().all(|l| l.trim().is_empty() || has(l));
            let new_block = lines
                .iter()
                .enumerate()
                .map(|(i, l)| {
                    let indent_len = l.len() - l.trim_start().len();
                    let (indent, rest) = l.split_at(indent_len);
                    let stripped = match prefix {
                        LinePrefix::Quote => rest.strip_prefix("> ").or_else(|| rest.strip_prefix('>')).unwrap_or(rest),
                        _ => list_marker_len(rest).map(|n| &rest[n..]).unwrap_or(rest),
                    };
                    if all_have {
                        format!("{indent}{stripped}")
                    } else {
                        let mark = match prefix {
                            LinePrefix::Bullet => "- ".to_string(),
                            LinePrefix::Number => format!("{}. ", i + 1),
                            LinePrefix::Quote => "> ".to_string(),
                        };
                        format!("{indent}{mark}{stripped}")
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            s.set_selected_range(start..end, cx);
            s.replace(new_block.clone(), window, cx);
            s.set_selected_range(start..start + new_block.len(), cx);
            s.focus(window, cx);
        });
        self.composer_changed(window, cx);
    }

    /// After Enter inside a list, carries the marker to the new line; Enter on an empty
    /// item ends the list instead.
    fn continue_list(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(c) = self.composer.as_ref() else { return };
        let body = c.body.clone();
        let changed = body.update(cx, |s, cx| {
            let text = s.value().to_string();
            let cursor = s.cursor().min(text.len());
            // The newline is already inserted; the previous line is the one Enter was pressed on.
            let Some(nl) = text[..cursor].rfind('\n') else { return false };
            let prev_start = text[..nl].rfind('\n').map(|i| i + 1).unwrap_or(0);
            let prev = &text[prev_start..nl];
            let indent_len = prev.len() - prev.trim_start().len();
            let (indent, rest) = prev.split_at(indent_len);
            let Some(n) = list_marker_len(rest) else { return false };
            let marker = &rest[..n];
            if rest[n..].trim().is_empty() {
                // An empty item: Enter ends the list and removes the dangling marker.
                s.set_selected_range(prev_start..cursor, cx);
                s.replace("\n".to_string(), window, cx);
                return true;
            }
            let next = if let Some(dot) = marker.find(|c: char| c == '.' || c == ')') {
                let num: usize = marker[..dot].parse().unwrap_or(0);
                format!("{}{}", num + 1, &marker[dot..])
            } else {
                marker.to_string()
            };
            // Enter in the editor copied the indentation already; add only the marker.
            s.replace(format!("{}{}", if text[cursor..].starts_with(indent) { "" } else { indent }, next), window, cx);
            true
        });
        if changed {
            self.composer_changed(window, cx);
        }
    }

    fn accept_suggestion(&mut self, draft_id: &DraftId, query: &RecipientQuery, address: &Address, window: &mut Window, cx: &mut Context<Self>) {
        let Some(c) = self.composer.as_ref() else { return };
        if &c.draft.id != draft_id || c.recipient_query.as_ref() != Some(query) { return; }
        // Use the suggestion's field, not whichever control has focus after a
        // click or keyboard activation. Refuse stale text/selection snapshots.
        let input = c.recipient_input(query.field).clone();
        let state = input.read(cx);
        let Some((range, text)) = query.replacement(&state.value(), state.selected_range(), address) else { return };
        let applied = input.update(cx, |s, cx| {
            if s.marked_text_range(window, cx).is_some() { return false; }
            s.set_selected_range(range, cx);
            // Atomic replacement retains undo/redo history; set_value clears it.
            s.replace(text, window, cx);
            s.focus(window, cx);
            true
        });
        if !applied { return; }
        if let Some(c) = self.composer.as_mut() {
            c.recipient_query = None;
            c.suggestions.clear();
        }
        self.composer_changed(window, cx);
    }

    fn remove_attachment(&mut self, id: AttachmentId, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(c) = self.composer.as_mut() {
            if let Some(pos) = c.draft.attachments.iter().position(|a| a.id == id) {
                let att = c.draft.attachments.remove(pos);
                let _ = std::fs::remove_file(&att.local_path);
                c.dirty = true;
            }
        }
        self.save_composer(window, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::reveal_offset;
    use gpui_kit::px;
    // The parent's GPUI glob also exports a `test` proc macro.
    use std::prelude::v1::test;

    #[test]
    fn reveal_long_message_editor_precisely() {
        assert_eq!(reveal_offset(px(0.), px(100_000.), px(180.), px(600.), px(100_500.)), px(-99_580.));
    }

    #[test]
    fn reveal_is_minimal_and_clamped() {
        // Already visible: no scroll. Above: align the field's beginning.
        assert_eq!(reveal_offset(px(-500.), px(100.), px(180.), px(600.), px(1000.)), px(-500.));
        assert_eq!(reveal_offset(px(-500.), px(-40.), px(180.), px(600.), px(1000.)), px(-460.));
        // Tall editors reveal their beginning rather than their footer/quote.
        assert_eq!(reveal_offset(px(0.), px(800.), px(2000.), px(600.), px(3000.)), px(-800.));
        assert_eq!(reveal_offset(px(0.), px(800.), px(180.), px(600.), px(300.)), px(-300.));
    }

    #[test]
    fn reveal_tracks_height_settling_without_a_fixed_scroll_distance() {
        let first = reveal_offset(px(0.), px(10_000.), px(180.), px(600.), px(20_000.));
        // The browser body grows 4,000px after the initial reveal.
        let settled = reveal_offset(first, px(14_000.) + first, px(180.), px(600.), px(20_000.));
        assert_eq!(settled, px(-13_580.));
    }
}

/// A per-line formatting applied by the toolbar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinePrefix {
    Bullet,
    Number,
    Quote,
}

/// Checks and copies files chosen for a draft. Each accepted file is copied to its own
/// generated folder under the draft's outgoing directory, with a cleaned name and
/// owner-only, non-executable permissions (0600). Returns the attachments and the reasons
/// for any refusals. Blocking.
fn prepare_attachments(paths: Vec<std::path::PathBuf>, dir: &std::path::Path, mut total: u64) -> (Vec<DraftAttachment>, Vec<String>) {
    use overlook_core::attachment_policy::{check_outgoing, sanitize_filename, within_size_limit, TOO_LARGE};
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    let mut added = Vec::new();
    let mut refused = Vec::new();
    for path in paths {
        let Some(original) = path.file_name().map(|n| n.to_string_lossy().to_string()) else { continue };
        let name = sanitize_filename(&original);
        let Ok(content) = std::fs::read(&path) else {
            refused.push(format!("Couldn’t read “{name}”."));
            continue;
        };
        if !within_size_limit(total + content.len() as u64) {
            refused.push(TOO_LARGE.to_owned());
            continue;
        }
        // Programs, scripts and archives holding them are never attached.
        let mime_type = match check_outgoing(&name, &content) {
            Ok(mime) => mime,
            Err(refusal) => {
                tracing::info!(target: "overlook::attachments", reason = ?refusal, "outgoing attachment refused");
                refused.push(refusal.to_string());
                continue;
            }
        };
        if crate::malware_scan::scan(&path) == crate::malware_scan::Scan::Infected {
            refused.push(crate::malware_scan::INFECTED.to_owned());
            continue;
        }
        let id = AttachmentId::generate();
        let folder = dir.join(id.as_str());
        let dest = folder.join(&name);
        let written = std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&folder)
            .and_then(|_| std::fs::write(&dest, &content))
            .and_then(|_| std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o600)));
        if written.is_err() {
            refused.push(format!("Couldn’t attach “{name}”."));
            continue;
        }
        total += content.len() as u64;
        added.push(DraftAttachment { id, filename: name, mime_type: mime_type.into(), size_bytes: content.len() as u64, local_path: dest.to_string_lossy().to_string() });
    }
    (added, refused)
}

#[cfg(test)]
mod attachment_tests {
    use super::prepare_attachments;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn attachments_are_copied_privately_with_clean_names_and_limits() {
        let root = std::env::temp_dir().join(format!("overlook-attach-{}", std::process::id()));
        let source = root.join("source");
        std::fs::create_dir_all(&source).unwrap();
        let report = source.join("report\u{202e}fdp.txt");
        std::fs::write(&report, b"quarterly numbers").unwrap();
        std::fs::set_permissions(&report, std::fs::Permissions::from_mode(0o755)).unwrap();
        let program = source.join("holiday.jpg");
        std::fs::write(&program, b"\x7fELF\x02").unwrap();
        let (added, refused) = prepare_attachments(vec![report, program], &root.join("draft"), 0);
        assert_eq!(added.len(), 1);
        assert_eq!(added[0].filename, "reportfdp.txt");
        assert_eq!(added[0].mime_type, "text/plain");
        let copied = std::path::Path::new(&added[0].local_path);
        assert_eq!(std::fs::metadata(copied).unwrap().permissions().mode() & 0o777, 0o600, "no exec bit carried over");
        assert_eq!(copied.parent().unwrap().file_name().unwrap().to_str().unwrap(), added[0].id.as_str());
        assert_eq!(refused.len(), 1);
        // Already at the limit: nothing more fits.
        let small = source.join("small.txt");
        std::fs::write(&small, b"x").unwrap();
        let (added, refused) = prepare_attachments(vec![small], &root.join("draft"), 19 * 1024 * 1024);
        assert!(added.is_empty());
        assert_eq!(refused, vec![overlook_core::attachment_policy::TOO_LARGE.to_owned()]);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
