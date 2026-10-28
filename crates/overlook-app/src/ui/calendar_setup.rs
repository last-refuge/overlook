//! CalDAV account setup and independent calendar scheduling.
use super::{MailWindow, Module};
use crate::theme::ActivePalette;
use crate::{caldav_sync, credentials};
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{Disableable, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::*;
use overlook_core::time::now;
use overlook_core::*;
use overlook_provider::caldav::{fastmail_preset, CalDavClient};

pub struct CalendarSetup {
    request: CalendarConnectionId,
    previous: Option<CalendarConnection>,
    server: Entity<InputState>,
    username: Entity<InputState>,
    password: Entity<InputState>,
    reuse_mail: bool,
    busy: bool,
    note: Option<String>,
    calendars: Vec<Calendar>,
}

impl MailWindow {
    pub(crate) fn cancel_calendar_setup_dialog(&self) {
        if let Some(state) = self
            .settings
            .as_ref()
            .filter(|s| s.caldav.as_ref().is_some_and(|f| f.busy))
        {
            let _ = self.store.cancel_caldav_setup(&state.account);
        }
    }
    pub(crate) fn build_calendar_setup(
        &self,
        account: &Account,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<CalendarSetup> {
        if !matches!(account.provider, ProviderKind::Imap | ProviderKind::Pop3) {
            return None;
        }
        let previous = self.store.calendar_connection(&account.id).ok().flatten();
        let preset = previous.as_ref().map(|c| c.settings.clone()).or_else(|| {
            self.store
                .servers(&account.id)
                .ok()
                .flatten()
                .and_then(|s| fastmail_preset(&s))
        });
        let note = previous.as_ref().map(|c| {
            self.store
                .caldav_last_sync(&c.id)
                .ok()
                .flatten()
                .map(|at| {
                    format!(
                        "Calendar connection saved · Last downloaded {} at {}.",
                        tz::format_day(at),
                        tz::format_clock(at)
                    )
                })
                .unwrap_or_else(|| "Calendar settings saved. Sync to download events.".to_owned())
        });
        Some(CalendarSetup {
            request: CalendarConnectionId::generate(),
            server: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("Calendar server URL (https://…)")
                    .default_value(
                        preset
                            .as_ref()
                            .map(|s| s.server_url.clone())
                            .unwrap_or_default(),
                    )
            }),
            username: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("Calendar username")
                    .default_value(
                        preset
                            .as_ref()
                            .map(|s| s.username.clone())
                            .unwrap_or_else(|| account.email.clone()),
                    )
            }),
            password: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(if previous.is_some() {
                        "App password (leave blank to keep it)"
                    } else {
                        "App password with calendar access"
                    })
                    .masked(true)
            }),
            reuse_mail: previous.as_ref().is_some_and(|c| {
                c.settings.credential_source == CalendarCredentialSource::IncomingMail
            }),
            calendars: self.store.calendars(&account.id).unwrap_or_default(),
            previous,
            busy: false,
            note,
        })
    }

    pub(crate) fn render_calendar_setup(
        &self,
        form: &CalendarSetup,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = *cx.palette();
        let account = self
            .settings
            .as_ref()
            .map(|s| s.account.clone())
            .unwrap_or_else(|| self.account.id.clone());
        let prefs = self.account_prefs_for(&account);
        let default_name = prefs
            .preferred_calendar(&form.calendars)
            .map(|c| c.name.clone())
            .unwrap_or_else(|| "No writable calendar".into());
        let choices: Vec<_> = form
            .calendars
            .iter()
            .filter(|c| c.can_edit)
            .map(|c| {
                (
                    c.name.clone(),
                    c.provider_id
                        .as_ref()
                        .map(|id| id.0.clone())
                        .unwrap_or_else(|| c.id.0.clone()),
                )
            })
            .collect();
        let this = cx.entity().downgrade();
        v_flex().gap(px(10.)).p(px(16.)).rounded(px(10.)).border_1().border_color(p.divider)
            .child(div().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).child("Calendars"))
            .child(div().text_size(px(12.)).text_color(p.muted).child("Connect Fastmail or another CalDAV service. Personal events and offline changes sync in both directions. Invitations are not enabled yet."))
            .child(div().text_size(px(12.)).child("Calendar server URL"))
            .child(Input::new(&form.server).aria_label("Calendar server URL").disabled(form.busy))
            .child(div().text_size(px(12.)).child("Calendar username"))
            .child(Input::new(&form.username).aria_label("Calendar username").disabled(form.busy))
            .child(h_flex().gap(px(8.)).items_center()
                .child(Switch::new("caldav-reuse-mail").accessibility_label("Use this account’s saved mail password").checked(form.reuse_mail).disabled(form.busy).on_click(cx.listener(|this, checked: &bool, _, cx| {
                    if let Some(f) = this.settings.as_mut().and_then(|s| s.caldav.as_mut()) { f.reuse_mail = *checked; }
                    cx.notify();
                })))
                .child(div().text_size(px(12.)).child("Use this account’s saved mail password")))
            .when(!form.reuse_mail, |d| d.child(div().text_size(px(12.)).child("Calendar app password")).child(Input::new(&form.password).aria_label("Calendar app password").disabled(form.busy)))
            .child(div().text_size(px(12.)).text_color(p.muted).child("Fastmail needs an app password with calendar access. Passwords stay in your system keyring. To update a separate password while preserving pending changes, enter it here and reconnect with the same server and username."))
            .child(h_flex().gap(px(8.)).flex_wrap()
                .child(Button::new("caldav-connect").primary().small().disabled(form.busy)
                    .label(if form.busy { "Connecting…" } else if form.previous.is_some() { "Reconnect calendars" } else { "Connect calendars" })
                    .on_click(cx.listener(|this, _, window, cx| this.connect_calendar(window, cx))))
                .when(form.previous.is_some(), |d| d.child(Button::new("caldav-disconnect").small().disabled(form.busy).label("Disconnect calendars")
                    .on_click(cx.listener(|this, _, window, cx| this.disconnect_calendar(window, cx))))))
            .children(form.note.as_ref().map(|note| div().id("caldav-setup-status").text_size(px(12.)).text_color(p.muted).child(note.clone())))
            .when(!choices.is_empty(), |d| d
                .child(div().text_size(px(12.)).child("Default calendar for new events"))
                .child(Button::new("caldav-default-calendar").small().disabled(form.busy).label(default_name).dropdown_caret(true)
                    .dropdown_menu(move |mut menu, _, _| {
                        for (name, key) in &choices {
                            let this = this.clone();
                            let account = account.clone();
                            let key = key.clone();
                            menu = menu.item(PopupMenuItem::new(name.clone()).on_click(move |_, _, cx| {
                                let _ = this.update(cx, |this, cx| {
                                    let mut prefs = this.account_prefs_for(&account);
                                    prefs.default_calendar = Some(key.clone());
                                    this.save_account_prefs(&account, prefs);
                                    cx.notify();
                                });
                            }));
                        }
                        menu
                    })))
            .when(!form.calendars.is_empty(), |d| d.child(div().text_size(px(12.)).font_weight(FontWeight::MEDIUM).child("Show calendars")))
            .children(form.calendars.iter().map(|calendar| {
                let id = calendar.id.clone();
                let account_id = calendar.account_id.clone();
                h_flex().gap(px(8.)).items_center()
                    .child(Switch::new(SharedString::from(format!("caldav-visible-{id}"))).accessibility_label(format!("Show {}", calendar.name)).checked(calendar.visible).disabled(form.busy).on_click(cx.listener(move |this, visible: &bool, _, cx| {
                        if this.store.set_calendar_visible(&account_id, &id, *visible).is_ok() {
                            if let Some(form) = this.settings.as_mut().and_then(|s| s.caldav.as_mut()) {
                                if let Some(c) = form.calendars.iter_mut().find(|c| c.id == id) { c.visible = *visible; }
                            }
                            this.reload_calendar();
                        }
                        cx.notify();
                    })))
                    .child(div().text_size(px(12.)).child(calendar.name.clone()))
                    .when(!calendar.can_edit, |d| d.child(div().text_size(px(11.)).text_color(p.muted).child("Read only")))
            }))
            .into_any_element()
    }

    fn connect_calendar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(state) = self.settings.as_mut() else {
            return;
        };
        let account_id = state.account.clone();
        if self.removing_accounts.contains(&account_id) {
            return;
        }
        let Some(account) = self.accounts.iter().find(|a| a.id == account_id).cloned() else {
            return;
        };
        let Some(form) = state.caldav.as_mut().filter(|f| !f.busy) else {
            return;
        };
        let settings = CalDavSettings {
            server_url: form.server.read(cx).value().trim().to_owned(),
            username: form.username.read(cx).value().trim().to_owned(),
            credential_source: if form.reuse_mail {
                CalendarCredentialSource::IncomingMail
            } else {
                CalendarCredentialSource::Separate
            },
        };
        if let Err(error) = settings.validate() {
            form.note = Some(error.into());
            cx.notify();
            return;
        }
        let typed = form.password.read(cx).value().to_string();
        let previous = form.previous.clone();
        // A credential-only repair must preserve the connection identity and its
        // durable outbox. Re-discovery replaces that identity and is blocked while
        // changes are pending.
        if let Some(old) = previous.as_ref().filter(|old| {
            old.settings == settings
                && settings.credential_source == CalendarCredentialSource::Separate
                && !typed.is_empty()
        }) {
            self.update_calendar_password(old.clone(), typed, window, cx);
            return;
        }
        let request = form.request.clone();
        let connection = CalendarConnection {
            id: CalendarConnectionId::generate(),
            account_id: account_id.clone(),
            settings,
            enabled: true,
            generation: 1,
        };
        if let Err(error) = self.store.reserve_caldav_setup(&account_id, &connection.id) {
            form.note = Some(error.to_string());
            cx.notify();
            return;
        }
        form.busy = true;
        form.note = Some("Checking access and downloading calendars… Mail keeps working.".into());
        form.password
            .update(cx, |s, cx| s.set_value("", window, cx));
        let store = self.store.clone();
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx.background_spawn(async move {
                let password = if connection.settings.credential_source == CalendarCredentialSource::IncomingMail {
                    caldav_sync::stored_password(&connection)?
                } else if !typed.is_empty() { typed } else if let Some(old) = &previous {
                    if !connection.settings.can_keep_separate_password(&old.settings) {
                        return Err("Enter a separate calendar app password when changing the server, username, or password choice.".to_owned());
                    }
                    caldav_sync::stored_password(old)?
                } else { return Err("Enter an app password with calendar access.".to_owned()); };
                let client = CalDavClient::new(&connection.settings, password.clone()).map_err(|e| e.to_string())?;
                let mut snapshot = client.snapshot(&account, &connection.id, tz::add_days(tz::day_start(now()), -60), tz::add_days(tz::day_start(now()), 366), || {
                    store.calendar_connection(&account_id).ok().as_ref() == Some(&previous)
                        && store.caldav_setup_current(&connection).unwrap_or(false)
                }).map_err(|e| e.to_string())?;
                let old_calendars = store.calendars(&account_id).map_err(|e| e.to_string())?;
                for entry in &mut snapshot.calendars {
                    if let Some(old) = old_calendars.iter().find(|old| old.provider_id == entry.calendar.provider_id) {
                        entry.calendar.visible = old.visible;
                    }
                }
                let owns_secret = connection.settings.credential_source == CalendarCredentialSource::Separate;
                if !store.caldav_setup_current(&connection).unwrap_or(false) { return Err("Calendar setup was cancelled or superseded.".into()); }
                if owns_secret { credentials::save_password(&account_id, &connection.secret_kind(), &password).map_err(|e| e.to_string())?; }
                if let Err(error) = store.install_caldav_connection(&connection, previous.as_ref(), &snapshot) {
                    if owns_secret && credentials::delete_secret(&account_id, &connection.secret_kind()).is_err() {
                        return Err(format!("{error} The unused calendar password could not be removed from the keyring."));
                    }
                    return Err(error.to_string());
                }
                let mut note = "Connected. Personal events can be edited here and offline when the calendar grants write access.".to_owned();
                if let Some(old) = previous.filter(|c| c.settings.credential_source == CalendarCredentialSource::Separate) {
                    if credentials::delete_secret(&account_id, &old.secret_kind()).is_err() { note.push_str(" The old calendar password could not be removed from the keyring."); }
                }
                Ok::<_, String>((connection, snapshot.calendars.into_iter().map(|c| c.calendar).collect::<Vec<_>>(), note))
            }).await;
            let _ = this.update_in(cx, |this, _, cx| {
                if let Some(form) = this.settings.as_mut().and_then(|s| s.caldav.as_mut()).filter(|f| f.request == request) {
                    form.busy = false;
                    match result {
                        Ok((connection, calendars, note)) => { form.previous = Some(connection); form.calendars = calendars; form.note = Some(note); }
                        Err(error) => form.note = Some(format!("Couldn’t connect: {error}")),
                    }
                }
                this.reload_calendar();
                cx.notify();
            });
        }).detach();
    }

    fn update_calendar_password(
        &mut self,
        connection: CalendarConnection,
        password: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(form) = self.settings.as_mut().and_then(|s| s.caldav.as_mut()) else {
            return;
        };
        let request = form.request.clone();
        form.busy = true;
        form.note = Some("Saving calendar password… Your pending changes will be kept.".into());
        form.password
            .update(cx, |s, cx| s.set_value("", window, cx));
        let store = self.store.clone();
        let account_id = connection.account_id.clone();
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx.background_spawn(async move {
                if !store.caldav_connection_current(&connection).map_err(|e| e.to_string())? {
                    return Err("Calendar settings changed. Reopen settings before trying again.".to_owned());
                }
                credentials::save_password(&connection.account_id, &connection.secret_kind(), &password)
                    .map_err(|e| e.to_string())?;
                if !store.caldav_connection_current(&connection).map_err(|e| e.to_string())? {
                    credentials::delete_secret(&connection.account_id, &connection.secret_kind())
                        .map_err(|_| "Calendar settings changed and the unused password could not be removed from the keyring.".to_owned())?;
                    return Err("Calendar settings changed while saving the password.".to_owned());
                }
                Ok(())
            }).await;
            let _ = this.update_in(cx, |this, _, cx| {
                let saved = result.is_ok();
                if let Some(form) = this.settings.as_mut().and_then(|s| s.caldav.as_mut()).filter(|f| f.request == request) {
                    form.busy = false;
                    form.note = Some(match result {
                        Ok(()) => "Calendar password saved. Retrying synchronization with your pending changes.".into(),
                        Err(error) => error,
                    });
                }
                if saved { this.sync_caldav_account(account_id, true, cx); }
                cx.notify();
            });
        }).detach();
    }

    fn disconnect_calendar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(form) = self
            .settings
            .as_mut()
            .and_then(|s| s.caldav.as_mut())
            .filter(|f| !f.busy)
        else {
            return;
        };
        let Some(connection) = form.previous.clone() else {
            return;
        };
        let request = form.request.clone();
        form.busy = true;
        form.note = Some("Disconnecting calendars on this device…".into());
        let store = self.store.clone();
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx.background_spawn(async move {
                store.disconnect_caldav(&connection).map_err(|e| e.to_string())?;
                let cleanup_failed = connection.settings.credential_source == CalendarCredentialSource::Separate
                    && credentials::delete_secret(&connection.account_id, &connection.secret_kind()).is_err();
                Ok::<_, String>(if cleanup_failed { "Disconnected. Remove the old calendar password from your system keyring when it becomes available." } else { "Disconnected. Your calendars remain at the provider and mail keeps working." })
            }).await;
            let _ = this.update_in(cx, |this, _, cx| {
                if let Some(form) = this.settings.as_mut().and_then(|s| s.caldav.as_mut()).filter(|f| f.request == request) {
                    form.busy = false;
                    match result { Ok(note) => { form.previous = None; form.calendars.clear(); form.note = Some(note.into()); }, Err(error) => form.note = Some(error) }
                }
                this.reload_calendar(); cx.notify();
            });
        }).detach();
    }

    pub(crate) fn ensure_caldav_range(&mut self, cx: &mut Context<Self>) {
        if !self.caldav_cached {
            self.sync_caldav_account(self.account.id.clone(), false, cx);
        }
    }

    pub(crate) fn sync_caldav_account(
        &mut self,
        id: AccountId,
        force: bool,
        cx: &mut Context<Self>,
    ) {
        if self.removing_accounts.contains(&id) {
            return;
        }
        let Some(account) = self.accounts.iter().find(|a| a.id == id).cloned() else {
            return;
        };
        let Ok(Some(connection)) = self.store.calendar_connection(&id) else {
            return;
        };
        if !connection.enabled {
            return;
        }
        if !force
            && self
                .caldav_next_retry
                .get(&id)
                .is_some_and(|time| *time > now())
        {
            return;
        }
        if self.caldav_syncing.contains_key(&id) {
            self.caldav_requested.insert(id);
            return;
        }
        let (mut from, mut to) = (
            tz::add_days(tz::day_start(now()), -60),
            tz::add_days(tz::day_start(now()), 366),
        );
        if self.module == Module::Calendar && self.account.id == id {
            let (view_from, view_to) = self.cal_range();
            if view_from < from || view_to > to {
                from = tz::add_days(view_from, -7);
                to = tz::add_days(view_to, 7);
            }
        }
        self.caldav_syncing
            .insert(id.clone(), connection.id.clone());
        self.caldav_requested.remove(&id);
        if self.account.id == id {
            self.cal_status = SyncState::Syncing;
        }
        let store = self.store.clone();
        let worker_connection = connection.clone();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let delay = cx
                .background_spawn(async move {
                    caldav_sync::synchronize(&store, &account, &worker_connection, from, to)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.caldav_syncing.get(&id) != Some(&connection.id) {
                    return;
                }
                this.caldav_syncing.remove(&id);
                if !this
                    .store
                    .caldav_connection_current(&connection)
                    .unwrap_or(false)
                {
                    this.reload_calendar();
                    cx.notify();
                    return;
                }
                if delay == 0 {
                    this.caldav_next_retry.remove(&id);
                } else {
                    this.caldav_next_retry
                        .insert(id.clone(), now() + delay as i64 * 1000);
                }
                this.reload_calendar();
                if this.caldav_requested.remove(&id)
                    && delay == 0
                    && this.account.id == id
                    && !this.caldav_cached
                {
                    this.sync_caldav_account(id, false, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }
}
