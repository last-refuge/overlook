//! Contacts for IMAP and POP3 accounts, in Settings → Accounts: connecting the account's own
//! service (Fastmail, iCloud, Yahoo, AOL) with its mail password, Microsoft's through a
//! sign-in that allows contacts, or any CardDAV server with its own password. Every
//! connection is checked against its server before it is saved, so a wrong address or
//! password shows here, not later as a failed sync.

use super::contacts::ContactsSource;
use super::settings::setting_row;
use super::MailWindow;
use crate::assets::icon;
use crate::microsoft_signin::{self, PendingSignIn, SignInResult};
use crate::theme::ActivePalette;
use crate::{carddav_sync, credentials, graph_contacts_sync};
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{Disableable, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::*;
use overlook_core::*;
use overlook_provider::caldav::carddav::{self, CardDavError};
use overlook_provider::microsoft::contacts as graph;
use overlook_provider::microsoft::IdClaims;

/// Connecting contacts, while Settings shows an IMAP or POP3 account.
#[derive(Default)]
pub struct ContactsSetup {
    /// The form for a server of the person's choosing, while it's open.
    pub form: Option<ContactsForm>,
    /// A connection is being checked against its server.
    pub busy: bool,
    /// Why the last check failed.
    pub note: Option<String>,
    /// A Microsoft sign-in for an account other than this one, and its address, waiting for
    /// the person to say whether its contacts belong here.
    pub microsoft_other: Option<(SignInResult, String)>,
}

pub struct ContactsForm {
    server: Entity<InputState>,
    username: Entity<InputState>,
    password: Entity<InputState>,
    /// Sign in with the account's mail password rather than one of the server's own.
    reuse_mail: bool,
}

impl MailWindow {
    /// The Contacts rows of an IMAP or POP3 account's settings.
    pub(super) fn render_contacts_connection(&self, account: &Account, servers: Option<&MailServers>, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let p = *cx.palette();
        let setup = self.settings.as_ref().map(|s| &s.contacts);
        let busy = setup.is_some_and(|s| s.busy);
        let mut rows: Vec<AnyElement> = Vec::new();
        let note = setup.and_then(|s| s.note.clone()).map(|note| div().id("contacts-setup-note").text_size(px(12.)).text_color(p.warning).whitespace_normal().child(note).into_any_element());
        if let ContactsSource::Server(name) = self.contacts_source_of(account) {
            rows.push(self.render_contacts_status(account, name, busy, cx));
            // Signing in again can come back as someone else, or fail: both are shown here too.
            if setup.is_some_and(|s| s.microsoft_other.is_some()) {
                rows.push(self.render_microsoft_contacts(account, busy, cx));
            }
            rows.extend(note);
            return rows;
        }
        let microsoft = servers.is_some_and(|s| s.oauth == Some(OAuthIssuer::Microsoft));
        if microsoft {
            rows.push(self.render_microsoft_contacts(account, busy, cx));
        }
        // The account's own service, with its mail password: one click.
        if let Some((service, servers)) = servers.and_then(|s| carddav_sync::contacts_service(s).map(|service| (service, s))) {
            let connection = ContactsConnection {
                account_id: account.id.clone(),
                server_url: service.server_url.into(),
                username: carddav_sync::user_name(service, servers),
                default_book: None,
                credential: ContactsCredential::IncomingMail,
                connected_at: 0,
            };
            let owner = account.clone();
            rows.push(
                setting_row(
                    &format!("{} contacts", service.name),
                    &format!("See and edit your {} contacts here; changes go back to {}. {}", service.name, service.name, service.password_note),
                    Button::new("contacts-connect")
                        .small()
                        .disabled(busy)
                        .label(if busy { "Checking…" } else { "Connect contacts" })
                        .on_click(cx.listener(move |this, _, window, cx| this.connect_contacts(owner.clone(), connection.clone(), None, window, cx)))
                        .into_any_element(),
                )
                .into_any_element(),
            );
        }
        match setup.and_then(|s| s.form.as_ref()) {
            Some(form) => rows.push(self.render_contacts_form(account, form, busy, !microsoft, cx)),
            None => {
                let owner = account.clone();
                rows.push(
                    setting_row(
                        "Another contacts server",
                        "Any CardDAV server, such as Nextcloud or your mail host’s.",
                        Button::new("contacts-other").small().ghost().disabled(busy).label("Connect a server…").on_click(cx.listener(move |this, _, window, cx| this.open_contacts_form(&owner, window, cx))).into_any_element(),
                    )
                    .into_any_element(),
                );
            }
        }
        rows.extend(note);
        rows
    }

    /// Microsoft's contacts for an account that signs in with Microsoft: a sign-in of their
    /// own, in the browser, or the question of whose they are when it came back as someone else.
    fn render_microsoft_contacts(&self, account: &Account, busy: bool, cx: &mut Context<Self>) -> AnyElement {
        let other = self.settings.as_ref().and_then(|s| s.contacts.microsoft_other.as_ref());
        let owner = account.clone();
        if let Some((_, address)) = other {
            let keep = account.clone();
            return setting_row(
                &format!("Microsoft signed you in as {address}"),
                &format!("Not {}. Connect these contacts here only if that’s you, such as another name for the same account.", account.email),
                h_flex()
                    .gap(px(6.))
                    .child(Button::new("contacts-microsoft-use").small().label("Use these contacts").on_click(cx.listener(move |this, _, window, cx| {
                        if let Some((signed_in, _)) = this.settings.as_mut().and_then(|s| s.contacts.microsoft_other.take()) {
                            this.save_microsoft_contacts(keep.clone(), signed_in, window, cx);
                        }
                    })))
                    .child(Button::new("contacts-microsoft-again").small().ghost().label("Sign in again").on_click(cx.listener(move |this, _, window, cx| this.connect_microsoft_contacts(owner.clone(), window, cx))))
                    .into_any_element(),
            )
            .into_any_element();
        }
        setting_row(
            "Microsoft contacts",
            "See and edit your Outlook contacts here; changes go back to Microsoft. Your browser opens to allow contacts. Mail keeps its own sign-in.",
            Button::new("contacts-microsoft")
                .small()
                .disabled(busy)
                .label(if busy { "Waiting for the browser…" } else { "Connect contacts" })
                .on_click(cx.listener(move |this, _, window, cx| this.connect_microsoft_contacts(owner.clone(), window, cx)))
                .into_any_element(),
        )
        .into_any_element()
    }

    /// A connected account: how many contacts, when they were last checked or why not.
    fn render_contacts_status(&self, account: &Account, name: Option<&'static str>, busy: bool, cx: &mut Context<Self>) -> AnyElement {
        let count = self.store.contact_count(&account.id).unwrap_or(0);
        let counted = format!("{count} contact{}", if count == 1 { "" } else { "s" });
        let status = carddav_sync::status(&self.store, &account.id);
        let note = match &status {
            Some(carddav_sync::ContactsStatus { error: Some(error), .. }) => error.clone(),
            Some(status) if status.at > 0 => format!("Last checked at {}. Checked every 15 minutes.", tz::format_clock(status.at)),
            _ => "Checked every 15 minutes.".to_owned(),
        };
        // A Microsoft sign-in that stopped working is renewed in the browser; the contacts stay.
        let renew = status.as_ref().is_some_and(|s| s.error.is_some())
            && self.store.contacts_connection(&account.id).ok().flatten().is_some_and(|c| c.credential == ContactsCredential::Microsoft);
        let renew_for = account.clone();
        let (sync_id, off_id) = (account.id.clone(), account.id.clone());
        let place = name.unwrap_or("your contacts server");
        setting_row(
            &counted,
            &note,
            h_flex()
                .gap(px(6.))
                .when(renew, |d| d.child(Button::new("contacts-renew").small().disabled(busy).label(if busy { "Waiting for the browser…" } else { "Sign in again" }).on_click(cx.listener(move |this, _, window, cx| this.connect_microsoft_contacts(renew_for.clone(), window, cx)))))
                .child(Button::new("contacts-sync").ghost().small().icon(icon("refresh-cw").size(px(14.))).label("Sync contacts now").on_click(cx.listener(move |this, _, window, cx| {
                    let _ = this.store.set_setting(&crate::sync::contacts_synced_key(&sync_id), &0i64);
                    this.sync_account(sync_id.clone(), window, cx);
                })))
                .child(Button::new("contacts-disconnect").ghost().small().label("Disconnect").on_click(cx.listener(move |this, _, window, cx| {
                    match carddav_sync::disconnect(&this.store, &off_id) {
                        Ok(()) => crate::ui::toast(window, cx, crate::notify::Level::Info, format!("Disconnected contacts. They stay at {place}.")),
                        Err(error) => crate::ui::toast(window, cx, crate::notify::Level::Error, format!("Couldn’t disconnect contacts: {error}")),
                    }
                    this.reload_contacts();
                    cx.notify();
                })))
                .into_any_element(),
        )
        .into_any_element()
    }

    /// `has_password`: the account signs in to mail with a password, which may be offered here.
    fn render_contacts_form(&self, account: &Account, form: &ContactsForm, busy: bool, has_password: bool, cx: &mut Context<Self>) -> AnyElement {
        let p = *cx.palette();
        let label = |text: &'static str| div().text_size(px(12.)).child(text);
        let owner = account.clone();
        v_flex()
            .gap(px(8.))
            .p(px(14.))
            .rounded(px(10.))
            .border_1()
            .border_color(p.divider)
            .child(div().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).child("Another contacts server"))
            .child(div().text_size(px(12.)).text_color(p.muted).whitespace_normal().child("Overlook finds the address books at this address and keeps them in step both ways. Requests go only to this server, over a verified connection."))
            .child(label("Server address"))
            .child(Input::new(&form.server).aria_label("Contacts server address").disabled(busy))
            .child(label("User name"))
            .child(Input::new(&form.username).aria_label("Contacts server user name").disabled(busy))
            .when(has_password, |d| d.child(
                h_flex()
                    .gap(px(8.))
                    .items_center()
                    .child(Switch::new("contacts-reuse-mail").accessibility_label("Use this account’s mail password").checked(form.reuse_mail).disabled(busy).on_click(cx.listener(|this, checked: &bool, _, cx| {
                        if let Some(form) = this.settings.as_mut().and_then(|s| s.contacts.form.as_mut()) {
                            form.reuse_mail = *checked;
                        }
                        cx.notify();
                    })))
                    .child(div().text_size(px(12.)).child("Use this account’s mail password")),
            ))
            .when(!form.reuse_mail, |d| d.child(label("Password")).child(Input::new(&form.password).aria_label("Contacts server password").disabled(busy)))
            .child(div().text_size(px(12.)).text_color(p.muted).whitespace_normal().child("Passwords stay in your system keyring."))
            .child(
                h_flex()
                    .gap(px(8.))
                    .child(Button::new("contacts-form-connect").primary().small().disabled(busy).label(if busy { "Checking…" } else { "Connect contacts" }).on_click(cx.listener(move |this, _, window, cx| this.submit_contacts_form(owner.clone(), window, cx))))
                    .child(Button::new("contacts-form-cancel").small().ghost().disabled(busy).label("Cancel").on_click(cx.listener(|this, _, _, cx| {
                        if let Some(state) = this.settings.as_mut() {
                            state.contacts.form = None;
                            state.contacts.note = None;
                        }
                        cx.notify();
                    }))),
            )
            .into_any_element()
    }

    fn open_contacts_form(&mut self, account: &Account, window: &mut Window, cx: &mut Context<Self>) {
        // The user name is a guess the person can change; the address is theirs to give, and
        // the mail password goes to it only once they switch that on.
        let user = match self.store.servers(&account.id).ok().flatten() {
            Some(servers) => match carddav_sync::contacts_service(&servers) {
                Some(service) => carddav_sync::user_name(service, &servers),
                None => servers.incoming.username.clone(),
            },
            None => account.email.clone(),
        };
        let form = ContactsForm {
            server: cx.new(|cx| InputState::new(window, cx).placeholder("https://dav.example.com/")),
            username: cx.new(|cx| InputState::new(window, cx).placeholder("User name").default_value(user)),
            password: cx.new(|cx| InputState::new(window, cx).placeholder("The server’s password or app password").masked(true)),
            reuse_mail: false,
        };
        if let Some(state) = self.settings.as_mut() {
            state.contacts.form = Some(form);
            state.contacts.note = None;
        }
        cx.notify();
    }

    fn submit_contacts_form(&mut self, account: Account, window: &mut Window, cx: &mut Context<Self>) {
        let Some(state) = self.settings.as_mut() else { return };
        let Some(form) = state.contacts.form.as_ref() else { return };
        let server_url = form.server.read(cx).value().trim().to_owned();
        let username = form.username.read(cx).value().trim().to_owned();
        let password = form.password.read(cx).value().to_string();
        let reuse_mail = form.reuse_mail;
        let problem = if !server_url.to_ascii_lowercase().starts_with("https://") {
            Some("Enter the server’s address, starting with https://.")
        } else if username.is_empty() {
            Some("Enter the user name for the contacts server.")
        } else if !reuse_mail && password.is_empty() {
            Some("Enter the contacts server’s password, or use this account’s mail password.")
        } else {
            None
        };
        if let Some(problem) = problem {
            state.contacts.note = Some(problem.into());
            cx.notify();
            return;
        }
        let connection = ContactsConnection {
            account_id: account.id.clone(),
            server_url,
            username,
            default_book: None,
            credential: if reuse_mail { ContactsCredential::IncomingMail } else { ContactsCredential::Separate },
            connected_at: 0,
        };
        self.connect_contacts(account, connection, (!reuse_mail).then_some(password), window, cx);
    }

    /// Checks the connection against its server off the UI thread, then saves it and starts
    /// the first download. Nothing is saved when the check fails.
    fn connect_contacts(&mut self, account: Account, connection: ContactsConnection, separate_password: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(state) = self.settings.as_mut() else { return };
        if state.contacts.busy {
            return;
        }
        state.contacts.busy = true;
        state.contacts.note = None;
        cx.notify();
        let (checked, password) = (connection.clone(), separate_password.clone());
        let check = cx.background_spawn(async move {
            let password = password.or_else(|| carddav_sync::password(&checked)).ok_or(CardDavError::AuthenticationRequired)?;
            carddav_sync::verify(&checked.server_url, &checked.username, password)
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = check.await;
            let _ = this.update_in(cx, |this, window, cx| {
                let shown = this.settings.as_ref().is_some_and(|s| s.account == account.id);
                let outcome = result.map_err(|error| error.to_string()).and_then(|_| carddav_sync::save(&this.store, &account, connection.clone(), separate_password.as_deref()));
                // Settings opened afresh, or for another account, start with their own state.
                if let Some(state) = this.settings.as_mut().filter(|_| shown) {
                    state.contacts.busy = false;
                    match &outcome {
                        Ok(_) => {
                            state.contacts.form = None;
                            state.contacts.note = None;
                        }
                        Err(error) => state.contacts.note = Some(error.clone()),
                    }
                }
                if outcome.is_ok() {
                    let name = carddav::service_for_url(&connection.server_url).map(|s| format!("your {} contacts", s.name)).unwrap_or_else(|| "your contacts".into());
                    crate::ui::toast(window, cx, crate::notify::Level::Success, format!("Connected {name}. Downloading them…"));
                    this.reload_contacts();
                    this.sync_account(account.id.clone(), window, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }
    /// "Connect contacts" for an account that signs in with Microsoft: the browser asks to
    /// allow contacts, the new sign-in is checked against the contacts, then saved apart from
    /// the mail sign-in. Also renews a contacts sign-in that stopped working.
    fn connect_microsoft_contacts(&mut self, account: Account, window: &mut Window, cx: &mut Context<Self>) {
        if self.signin.is_some() {
            crate::ui::toast(window, cx, crate::notify::Level::Info, "Finish the sign-in that is already open in your browser, or cancel it first.");
            return;
        }
        // The registration the mail sign-in came from, so contacts are allowed for the same app.
        let config = match credentials::load_microsoft_tokens(&account.id) {
            Ok(Some((_, config))) => Ok(config),
            _ => microsoft_signin::load_client_config(),
        };
        let pending = match config.and_then(|config| PendingSignIn::start_with_scopes(config, graph::contacts_scopes(), Some(account.email.clone()))) {
            Ok(pending) => pending,
            Err(error) => {
                if let Some(state) = self.settings.as_mut() {
                    state.contacts.note = Some(error);
                }
                cx.notify();
                return;
            }
        };
        self.signin = Some(super::SignIn { cancel: pending.cancel_handle(), expected: Some(account.email.clone()), issuer: OAuthIssuer::Microsoft.label() });
        if let Some(state) = self.settings.as_mut() {
            state.contacts.busy = true;
            state.contacts.note = None;
            state.contacts.microsoft_other = None;
        }
        cx.open_url(&pending.url);
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    let signed_in = pending.wait()?;
                    // An organization can allow the sign-in and still refuse contacts.
                    graph_contacts_sync::verify(&signed_in.tokens.access_token).map_err(|e| e.to_string())?;
                    Ok::<_, String>(signed_in)
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| this.finish_microsoft_contacts(account, result, window, cx));
        })
        .detach();
    }

    fn finish_microsoft_contacts(&mut self, account: Account, result: Result<SignInResult, String>, window: &mut Window, cx: &mut Context<Self>) {
        let was_cancelled = self.signin.is_none();
        self.signin = None;
        let shown = self.settings.as_ref().is_some_and(|s| s.account == account.id);
        if let Some(state) = self.settings.as_mut().filter(|_| shown) {
            state.contacts.busy = false;
        }
        let signed_in = match result {
            Ok(signed_in) => signed_in,
            Err(error) => {
                if !was_cancelled {
                    match self.settings.as_mut().filter(|_| shown) {
                        Some(state) => state.contacts.note = Some(error),
                        None => crate::ui::toast(window, cx, crate::notify::Level::Warning, format!("Couldn’t connect Microsoft contacts: {error}")),
                    }
                }
                cx.notify();
                return;
            }
        };
        // Another account's contacts are never connected without the person seeing it first.
        let incoming = self.store.servers(&account.id).ok().flatten().map(|s| s.incoming.username).unwrap_or_default();
        if signed_in.claims.as_ref().and_then(|c| c.names_any(&[&account.email, &incoming])) == Some(false) {
            let address = signed_in.claims.as_ref().and_then(IdClaims::address).unwrap_or_else(|| "another account".into());
            match self.settings.as_mut().filter(|_| shown) {
                Some(state) => state.contacts.microsoft_other = Some((signed_in, address)),
                None => crate::ui::toast(window, cx, crate::notify::Level::Warning, format!("Microsoft signed you in as {address}, not {}. Nothing was connected.", account.email)),
            }
            cx.notify();
            return;
        }
        self.save_microsoft_contacts(account, signed_in, window, cx);
    }

    fn save_microsoft_contacts(&mut self, account: Account, signed_in: SignInResult, window: &mut Window, cx: &mut Context<Self>) {
        let shown = self.settings.as_ref().is_some_and(|s| s.account == account.id);
        match graph_contacts_sync::save(&self.store, &account, &signed_in) {
            Ok(_) => {
                if let Some(state) = self.settings.as_mut().filter(|_| shown) {
                    state.contacts.note = None;
                    state.contacts.form = None;
                }
                crate::ui::toast(window, cx, crate::notify::Level::Success, "Connected your Microsoft contacts. Downloading them…");
                self.reload_contacts();
                self.sync_account(account.id.clone(), window, cx);
            }
            Err(error) => match self.settings.as_mut().filter(|_| shown) {
                Some(state) => state.contacts.note = Some(error),
                None => crate::ui::toast(window, cx, crate::notify::Level::Error, format!("Couldn’t connect Microsoft contacts: {error}")),
            },
        }
        cx.notify();
    }
}
