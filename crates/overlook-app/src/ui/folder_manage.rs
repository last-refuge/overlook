//! New, rename and delete folders from the sidebar's context menu.
//!
//! These run against the server straight away instead of being queued like message
//! changes: the server decides the new folder's identity (Gmail assigns a label id, an IMAP
//! rename changes the path every message is filed under). Offline, the dialog says so.

use super::*;
use gpui_kit::component::Disableable;
use overlook_provider::{ProviderError, RenamedFolder};

pub(crate) enum FolderPromptMode {
    /// A new folder at the top level, or inside `parent`.
    New { parent: Option<Label> },
    Rename { label: Label },
}

pub(crate) struct FolderPrompt {
    account: AccountId,
    mode: FolderPromptMode,
    input: Entity<InputState>,
    error: Option<String>,
    busy: bool,
    _enter: Subscription,
}

/// What the dialog shows for a failed folder change.
fn folder_error(e: &ProviderError) -> String {
    match e {
        ProviderError::Offline => "You’re offline. Folders can be changed only while connected.".into(),
        ProviderError::SignInRequired => "Sign in to this account again to change its folders.".into(),
        ProviderError::Rejected(reason) => reason.clone(),
        other => format!("Couldn’t reach the server: {other}"),
    }
}

impl MailWindow {
    /// Whether the account's folders can be changed from Overlook (Gmail and IMAP).
    pub(crate) fn manages_folders(&self, account: &AccountId) -> bool {
        self.providers.get(account).is_some_and(|p| p.mail.manages_folders())
    }

    /// A user folder of `account` by id, as the sidebar shows it.
    pub(crate) fn user_label(&self, account: &AccountId, id: &LabelId) -> Option<Label> {
        self.labels_of(account).iter().find(|l| &l.id == id && l.kind == LabelKind::User).cloned()
    }

    pub(crate) fn open_folder_prompt(&mut self, account: AccountId, mode: FolderPromptMode, window: &mut Window, cx: &mut Context<Self>) {
        let current = match &mode {
            // The last part of the name: IMAP shows nesting as " / ", Gmail as "/".
            FolderPromptMode::Rename { label } => label.name.rsplit(" / ").next().and_then(|n| n.rsplit('/').next()).unwrap_or(&label.name).to_owned(),
            FolderPromptMode::New { .. } => String::new(),
        };
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Folder name").default_value(current));
        let enter = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| {
            if let InputEvent::PressEnter { .. } = event {
                this.submit_folder_prompt(window, cx);
            }
        });
        input.update(cx, |state, cx| state.focus(window, cx));
        self.folder_prompt = Some(FolderPrompt { account, mode, input, error: None, busy: false, _enter: enter });
        cx.notify();
    }

    pub(crate) fn close_folder_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.folder_prompt.take().is_some() {
            self.focus_current_module(window, cx);
            cx.notify();
        }
    }

    fn submit_folder_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(prompt) = self.folder_prompt.as_mut() else { return };
        if prompt.busy {
            return;
        }
        let name = prompt.input.read(cx).value().trim().to_owned();
        if let Some(problem) = overlook_provider::folder_name_problem(&name, "") {
            prompt.error = Some(problem);
            cx.notify();
            return;
        }
        let account_id = prompt.account.clone();
        let (Some(account), Some(bundle)) = (self.accounts.iter().find(|a| a.id == account_id).cloned(), self.providers.get(&account_id).cloned()) else {
            prompt.error = Some("Sign in to this account again to change its folders.".into());
            cx.notify();
            return;
        };
        prompt.busy = true;
        prompt.error = None;
        let labels = self.store.labels(&account_id).unwrap_or_default();
        let mail = bundle.mail.clone();
        let (renaming, parent) = match &prompt.mode {
            FolderPromptMode::New { parent } => (None, parent.clone()),
            FolderPromptMode::Rename { label } => (Some(label.clone()), None),
        };
        // The dialog this request belongs to: it may be closed, and another opened, before
        // the server answers.
        let asked = prompt.input.entity_id();
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn({
                    let renaming = renaming.clone();
                    async move {
                        match &renaming {
                            Some(label) => mail.rename_folder(&account, label, &name, &labels).await.map(|changed| (changed.first().map(|c| c.new.clone()).unwrap_or_else(|| label.clone()), changed)),
                            None => mail.create_folder(&account, &name, parent.as_ref(), &labels).await.map(|label| (label, Vec::new())),
                        }
                    }
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                let current = this.folder_prompt.as_ref().is_some_and(|p| p.input.entity_id() == asked);
                match result {
                    Ok((label, changed)) => {
                        if renaming.is_some() {
                            this.record_renamed_folders(&account_id, &changed, cx);
                        } else if let Err(e) = this.store.upsert_label(&label) {
                            tracing::warn!("could not record the folder change: {e}");
                        }
                        if current {
                            this.close_folder_prompt(window, cx);
                        }
                        this.after_folder_change(&account_id, window, cx);
                        let what = if renaming.is_some() { "Renamed to" } else { "Created" };
                        crate::ui::toast(window, cx, crate::notify::Level::Success, format!("{what} “{}”.", label.name));
                    }
                    Err(e) => {
                        if current {
                            if let Some(prompt) = this.folder_prompt.as_mut() {
                                prompt.busy = false;
                                prompt.error = Some(folder_error(&e));
                            }
                            cx.notify();
                        } else {
                            crate::ui::toast(window, cx, crate::notify::Level::Error, folder_error(&e));
                        }
                    }
                }
            });
        })
        .detach();
    }

    /// Stores folders a rename or move changed (the folder and the ones inside it). For IMAP
    /// their messages' provider ids change with the path, so they are rewritten too and the
    /// next sync recognises the messages instead of dropping them.
    fn record_renamed_folders(&mut self, account: &AccountId, changed: &[RenamedFolder], cx: &mut Context<Self>) {
        if changed.is_empty() {
            return;
        }
        let imap = self.accounts.iter().any(|a| &a.id == account && a.provider == ProviderKind::Imap);
        let renames: Vec<(LabelId, Label)> = changed.iter().map(|c| (c.old.id.clone(), c.new.clone())).collect();
        let prefixes: Vec<(String, String)> = if imap {
            changed
                .iter()
                .filter_map(|c| Some((overlook_provider::imap::folder_id_prefix(&c.old.provider_id.as_ref()?.0), overlook_provider::imap::folder_id_prefix(&c.new.provider_id.as_ref()?.0))))
                .filter(|(old, new)| old != new)
                .collect()
        } else {
            Vec::new()
        };
        if let Err(e) = self.store.rename_folders(account, &renames, &prefixes) {
            tracing::warn!("could not record the folder change: {e}");
        }
        for c in changed {
            self.follow_renamed_folder(account, &c.old.id, &c.new.id, cx);
        }
    }

    /// Favorites and the open view follow a folder whose id changed with its name (IMAP).
    fn follow_renamed_folder(&mut self, account: &AccountId, old: &LabelId, new: &LabelId, cx: &mut Context<Self>) {
        if old == new {
            return;
        }
        let mut favorites = self.favorites();
        let mut changed = false;
        for f in favorites.iter_mut().filter(|f| &f.account == account && f.label.as_ref() == Some(old)) {
            f.label = Some(new.clone());
            changed = true;
        }
        if changed {
            self.prefs.favorites = Some(favorites);
            self.save_prefs(cx);
        }
        self.follow_view(account, old, new);
        self.follow_renamed_folder_in_other_windows(account, old, new, cx);
    }

    /// Moves this window's open view from the old id of a renamed folder to the new one.
    pub(super) fn follow_view(&mut self, account: &AccountId, old: &LabelId, new: &LabelId) {
        if &self.account.id == account && self.view == MailboxView::Label(old.clone()) {
            self.view = MailboxView::Label(new.clone());
        }
    }

    /// Re-reads labels here and in the other windows, then syncs so the folder list (and,
    /// after an IMAP rename, the messages' new places) come from the server.
    fn after_folder_change(&mut self, account: &AccountId, window: &mut Window, cx: &mut Context<Self>) {
        self.reload_account_labels();
        self.reload(cx);
        if self.folder_owner.is_some() {
            self.notify_owner(cx);
        } else {
            self.reload_folder_windows(cx);
        }
        self.sync_account(account.clone(), window, cx);
        cx.notify();
    }

    /// Moves a dropped folder into `parent` (`None`: the top level), after asking: a drag
    /// can land on the wrong row.
    pub(crate) fn move_folder(&mut self, drag: super::FolderDrag, parent: Option<Label>, window: &mut Window, cx: &mut Context<Self>) {
        if !drag.fits(&drag.account, parent.as_ref()) {
            return;
        }
        let (account_id, label) = (drag.account, drag.label);
        let Some(account) = self.accounts.iter().find(|a| a.id == account_id).cloned() else { return };
        let title = match &parent {
            Some(p) => format!("Move “{}” into “{}”?", label.name, p.name),
            None => format!("Move “{}” to the top level?", label.name),
        };
        let detail = if account.provider == ProviderKind::Imap { "Its messages and the folders inside it move with it." } else { "Its messages and the labels inside it move with it." };
        let answer = window.prompt(PromptLevel::Info, &title, Some(detail), &["Move", "Cancel"], cx);
        cx.spawn_in(window, async move |this, cx| {
            if answer.await != Ok(0) {
                return;
            }
            let Ok(Some((mail, labels))) = this.update(cx, |this, _| this.providers.get(&account_id).map(|p| (p.mail.clone(), this.store.labels(&account_id).unwrap_or_default()))) else {
                let _ = this.update_in(cx, |_, window, cx| crate::ui::toast(window, cx, crate::notify::Level::Error, "Sign in to this account again to change its folders."));
                return;
            };
            let result = cx
                .background_spawn({
                    let (label, parent) = (label.clone(), parent.clone());
                    async move { mail.move_folder(&account, &label, parent.as_ref(), &labels).await }
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| match result {
                Ok(changed) => {
                    this.record_renamed_folders(&account_id, &changed, cx);
                    this.after_folder_change(&account_id, window, cx);
                    let place = parent.as_ref().map(|p| format!("into “{}”", p.name)).unwrap_or_else(|| "to the top level".into());
                    crate::ui::toast(window, cx, crate::notify::Level::Success, format!("Moved “{}” {place}.", label.name));
                }
                Err(e) => crate::ui::toast(window, cx, crate::notify::Level::Error, format!("Couldn’t move “{}”: {}", label.name, folder_error(&e))),
            });
        })
        .detach();
    }

    /// Deletes a user folder after asking. IMAP deletes the mail in it too; Gmail keeps the
    /// messages and only removes the label.
    pub(crate) fn delete_folder(&mut self, account_id: AccountId, label: Label, window: &mut Window, cx: &mut Context<Self>) {
        let Some(account) = self.accounts.iter().find(|a| a.id == account_id).cloned() else { return };
        let imap = account.provider == ProviderKind::Imap;
        let detail = if imap {
            "The folder and every message in it are deleted from the server. This can’t be undone."
        } else {
            "The label is removed. Its messages stay in your mailbox."
        };
        let answer = window.prompt(PromptLevel::Warning, &format!("Delete the folder “{}”?", label.name), Some(detail), &["Delete", "Cancel"], cx);
        cx.spawn_in(window, async move |this, cx| {
            if answer.await != Ok(0) {
                return;
            }
            let Ok(Some(mail)) = this.update(cx, |this, _| this.providers.get(&account_id).map(|p| p.mail.clone())) else {
                let _ = this.update_in(cx, |_, window, cx| crate::ui::toast(window, cx, crate::notify::Level::Error, "Sign in to this account again to change its folders."));
                return;
            };
            let result = cx
                .background_spawn({
                    let label = label.clone();
                    async move { mail.delete_folder(&account, &label).await }
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| match result {
                Ok(()) => {
                    if let Err(e) = this.store.delete_label(&account_id, &label.id, imap) {
                        tracing::warn!("could not record the deleted folder: {e}");
                    }
                    this.unpin_favorite(&Favorite::label(account_id.clone(), label.id.clone()), cx);
                    if this.account.id == account_id && this.view == MailboxView::Label(label.id.clone()) {
                        this.select_view(MailboxView::Kind(LabelKind::Inbox), window, cx);
                    }
                    this.after_folder_change(&account_id, window, cx);
                    crate::ui::toast(window, cx, crate::notify::Level::Success, format!("Deleted “{}”.", label.name));
                }
                Err(e) => crate::ui::toast(window, cx, crate::notify::Level::Error, format!("Couldn’t delete “{}”: {}", label.name, folder_error(&e))),
            });
        })
        .detach();
    }

    pub(crate) fn render_folder_prompt(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = *cx.palette();
        let Some(prompt) = self.folder_prompt.as_ref() else { return div().into_any_element() };
        let (title, action) = match &prompt.mode {
            FolderPromptMode::New { parent: None } => ("New folder".to_owned(), "Create"),
            FolderPromptMode::New { parent: Some(parent) } => (format!("New folder in “{}”", parent.name), "Create"),
            FolderPromptMode::Rename { label } => (format!("Rename “{}”", label.name), "Rename"),
        };
        let account = self.account_name(&prompt.account);
        div()
            .id("folder-prompt-overlay")
            .absolute()
            .inset_0()
            .occlude()
            .bg(gpui_kit::black().opacity(if p.is_dark() { 0.55 } else { 0.3 }))
            .flex()
            .items_center()
            .justify_center()
            .on_click(cx.listener(|this, _, window, cx| this.close_folder_prompt(window, cx)))
            .child(
                v_flex()
                    .id("folder-prompt")
                    .w(px(420.))
                    .max_w(relative(0.94))
                    .rounded(px(12.))
                    .bg(p.surface)
                    .border_1()
                    .border_color(p.divider)
                    .shadow_lg()
                    .p(px(20.))
                    .gap(px(12.))
                    .on_click(|_, _, cx| cx.stop_propagation())
                    .child(
                        h_flex()
                            .items_center()
                            .child(div().flex_1().min_w_0().truncate().text_size(px(16.)).font_weight(FontWeight::SEMIBOLD).child(title))
                            .child(kbd("Esc", &p)),
                    )
                    .child(div().text_size(px(12.)).text_color(p.muted).child(account))
                    .child(Input::new(&prompt.input))
                    .children(prompt.error.clone().map(|e| div().text_size(px(12.)).text_color(p.error).child(e)))
                    .child(
                        h_flex()
                            .justify_end()
                            .gap(px(8.))
                            .child(Button::new("folder-prompt-cancel").ghost().small().label("Cancel").on_click(cx.listener(|this, _, window, cx| this.close_folder_prompt(window, cx))))
                            .child(Button::new("folder-prompt-ok").primary().small().label(action).loading(prompt.busy).disabled(prompt.busy).on_click(cx.listener(|this, _, window, cx| this.submit_folder_prompt(window, cx)))),
                    ),
            )
            .into_any_element()
    }
}
