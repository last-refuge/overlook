//! Separate native composer windows. Each editor owns one draft and its input subscriptions.

use super::*;

/// The main window a detached window (a composer or a folder window) belongs to. Sync,
/// IDLE and the update checker run only there; detached windows hand that work back.
#[derive(Clone)]
pub(crate) struct WindowOwner {
    pub(super) parent: Entity<MailWindow>,
    pub(super) window: AnyWindowHandle,
}

pub(crate) enum WindowSetup {
    Mail(AppearancePreference),
    Composer {
        appearance: AppearancePreference,
        owner: WindowOwner,
        providers: HashMap<AccountId, providers::AccountProviders>,
    },
    /// "Open in New Window": one folder of one account, beside the main window.
    Folder {
        appearance: AppearancePreference,
        owner: WindowOwner,
        providers: HashMap<AccountId, providers::AccountProviders>,
        account: AccountId,
        view: MailboxView,
    },
}

pub(super) struct DetachedComposer {
    window: AnyWindowHandle,
    editor: WeakEntity<MailWindow>,
    _closed: Subscription,
}

impl MailWindow {
    pub(super) fn activate_detached_draft(&mut self, id: &DraftId, cx: &mut Context<Self>) -> bool {
        let Some(detached) = self.detached_composers.get(id) else {
            return false;
        };
        detached
            .window
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
    }

    pub(super) fn activate_detached_reply(
        &mut self,
        kind: DraftKind,
        source: &Message,
        cx: &mut Context<Self>,
    ) -> bool {
        let matching = self.detached_composers.values().find(|d| {
            d.editor.upgrade().is_some_and(|e| {
                e.read(cx).composer.as_ref().is_some_and(|c| {
                    c.draft.in_reply_to.as_ref() == Some(&source.id)
                        && (c.draft.kind == kind
                            || (matches!(c.draft.kind, DraftKind::Reply | DraftKind::ReplyAll)
                                && matches!(kind, DraftKind::Reply | DraftKind::ReplyAll)))
                })
            })
        });
        let Some(detached) = matching else {
            return false;
        };
        let editor = detached.editor.clone();
        detached
            .window
            .update(cx, |_, window, cx| {
                editor
                    .update(cx, |this, cx| {
                        this.reuse_reply(kind, source, window, cx);
                    })
                    .ok();
                window.activate_window();
            })
            .is_ok()
    }

    pub(super) fn on_pop_out_composer(
        &mut self,
        _: &PopOutComposer,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.pop_out_composer(window, cx);
    }

    pub(super) fn on_toggle_message_history(
        &mut self,
        _: &ToggleMessageHistory,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_message_history(cx);
    }

    pub(super) fn pop_out_composer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.composer_owner.is_some()
            || self
                .composer
                .as_ref()
                .is_none_or(|c| c.attaching || c.pending_saves > 0)
        {
            return;
        }
        self.sync_composer_fields(cx);
        let Some(c) = &self.composer else { return };
        if (!c.draft.is_empty() || c.last_saved.is_some()) && !self.save_composer(window, cx) {
            return;
        }
        let transfer = self.composer_transfer(cx);
        let id = transfer.draft.id.clone();
        let store = self.store.clone();
        let sample = self.sample.clone();
        let accounts = self.accounts.clone();
        let active = self.account.id.clone();
        let prefs = self.prefs.clone();
        let account_prefs = self.account_prefs.clone();
        let remote_allowed = self.remote_allowed.clone();
        let setup = WindowSetup::Composer {
            appearance: self.appearance,
            owner: WindowOwner {
                parent: cx.entity(),
                window: window.window_handle(),
            },
            providers: self.providers.clone(),
        };
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                None,
                size(px(820.), px(760.)),
                cx,
            ))),
            window_min_size: Some(size(px(540.), px(420.))),
            app_id: Some(crate::APP_ID.into()),
            ..TitleBar::window_options()
        };
        match gpui_kit::open_window(options, cx, move |window, cx| {
            let editor = cx.new(|cx| {
                let mut editor =
                    MailWindow::new(store, sample, accounts, Some(active), setup, window, cx);
                editor.prefs = prefs;
                editor.account_prefs = account_prefs;
                editor.remote_allowed = remote_allowed;
                editor.restore_composer_transfer(transfer, window, cx);
                editor
            });
            let weak = editor.downgrade();
            window.on_window_should_close(cx, move |window, cx| {
                weak.update(cx, |this, cx| {
                    this.prepare_composer_window_close(window, cx)
                })
                .unwrap_or(true)
            });
            editor
        }) {
            Ok((handle, editor)) => {
                let parent = cx.entity().downgrade();
                let closed_id = id.clone();
                let closed = cx.on_window_closed(move |cx, closed| {
                    if closed == handle.window_id() {
                        parent
                            .update(cx, |this, cx| {
                                this.detached_composers.remove(&closed_id);
                                this.reload(cx);
                            })
                            .ok();
                    }
                });
                self.detached_composers.insert(
                    id,
                    DetachedComposer {
                        window: handle,
                        editor: editor.downgrade(),
                        _closed: closed,
                    },
                );
                self.composer = None;
                self.list_focus.focus(window, cx);
                cx.notify();
            }
            Err(_) => {
                if let Some(c) = &mut self.composer {
                    c.error =
                        Some("Couldn’t open a compose window. Your draft is still here.".into());
                }
                cx.notify();
            }
        }
    }

    /// Veto native close on a failed save. The input entities and edit guard stay alive.
    pub(super) fn prepare_composer_window_close(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self
            .composer
            .as_ref()
            .is_some_and(|c| c.attaching || c.pending_saves > 0)
        {
            return false;
        }
        self.sync_composer_fields(cx);
        if let Some(c) = &self.composer {
            if (!c.draft.is_empty() || c.last_saved.is_some()) && !self.save_composer(window, cx) {
                return false;
            }
        }
        if let Some(c) = self.composer.take() {
            if c.draft.is_empty() {
                if c.last_saved.is_none() {
                    self.unwanted_drafts.insert(c.draft.id.clone());
                }
                if let Err(e) = self.store.delete_draft(&c.draft.account_id, &c.draft.id) {
                    self.composer = Some(c);
                    if let Some(c) = &mut self.composer {
                        c.error = Some(format!("Couldn’t close the empty draft: {e}"));
                    }
                    cx.notify();
                    return false;
                }
                self.forget_staged_files(&c.draft.id);
            }
        }
        self.schedule_draft_sync(window, cx);
        true
    }

    /// All windows retain the same mail controller. If its original window is
    /// closed, use this window for feedback/timers while retaining the controller's
    /// single-flight sync state; never start a second mail sender.
    pub(super) fn finish_queued_send(
        &mut self,
        draft: &Draft,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(owner) = self.composer_owner.clone() else {
            self.announce_queued_send(draft, window, cx);
            return;
        };
        let routed = owner
            .window
            .update(cx, |_, window, cx| {
                owner
                    .parent
                    .update(cx, |this, cx| this.announce_queued_send(draft, window, cx));
            })
            .is_ok();
        if routed {
            window.remove_window();
        } else {
            // Keep Undo attached to the visible editor when the main window
            // has closed; its send still goes through the shared controller.
            self.announce_queued_send(draft, window, cx);
        }
    }

    pub(super) fn sync_through_owner(
        &self,
        account: &AccountId,
        delay: Duration,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(owner) = self.window_owner() else {
            return false;
        };
        let schedule = |parent: &Entity<MailWindow>, window: &mut Window, cx: &mut App| {
            parent.update(cx, |this, cx| {
                if delay.is_zero() {
                    this.sync_account(account.clone(), window, cx);
                    return;
                }
                let account = account.clone();
                cx.spawn_in(window, async move |this, cx| {
                    cx.background_executor().timer(delay).await;
                    this.update_in(cx, |this, window, cx| {
                        this.sync_account(account, window, cx)
                    })
                    .ok();
                })
                .detach();
            });
        };
        if owner
            .window
            .update(cx, |_, window, cx| schedule(&owner.parent, window, cx))
            .is_err()
        {
            schedule(&owner.parent, window, cx);
        }
        true
    }

    /// Tells the main window (and through it, the other folder windows) that this
    /// detached window changed the store.
    pub(super) fn notify_owner(&self, cx: &mut Context<Self>) {
        if let Some(owner) = self.window_owner() {
            let parent = owner.parent.clone();
            // This window's account changed in the store; the main window recounts it.
            let changed = self.account.id.clone();
            cx.defer(move |cx| {
                parent.update(cx, |this, cx| {
                    this.mark_counts_stale([changed.clone()]);
                    this.reload(cx);
                    this.reload_folder_windows([changed], cx);
                });
            });
        }
    }

    pub(super) fn render_composer_window(&self, cx: &mut Context<Self>) -> impl IntoElement {
        self.prune_browser_bodies();
        let p = *cx.palette();
        v_flex()
            .id("composer-window")
            .key_context(WINDOW_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::on_escape))
            .on_action(cx.listener(Self::on_send))
            .on_action(cx.listener(Self::on_command_palette))
            .on_action(cx.listener(Self::on_format_bold))
            .on_action(cx.listener(Self::on_format_italic))
            .on_action(cx.listener(Self::on_format_strike))
            .on_action(cx.listener(Self::on_format_code))
            .on_action(cx.listener(Self::on_format_bullets))
            .on_action(cx.listener(Self::on_format_numbers))
            .on_action(cx.listener(Self::on_format_quote))
            .on_action(cx.listener(Self::on_toggle_preview))
            .on_action(cx.listener(Self::on_toggle_message_history))
            .size_full()
            .bg(p.message_surface())
            .text_color(p.foreground)
            .font_family("Inter")
            .text_size(px(13.))
            .child(TitleBar::new().child(div().px_3().child("Overlook · Compose")))
            .child(if self.composer.is_some() {
                div()
                    .flex_1()
                    .min_h_0()
                    .child(self.render_composer(cx))
                    .into_any_element()
            } else {
                // If the main window was closed first, keep this window alive
                // for the queued send, its timer and Undo rather than exiting.
                v_flex()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .gap_3()
                    .child("Your message has been queued for sending.")
                    .child(
                        Button::new("close-sent-composer")
                            .label("Close")
                            .on_click(|_, window, _| window.remove_window()),
                    )
                    .into_any_element()
            })
    }
}
