//! Account setup for IMAP and POP3 accounts: name, address and password first,
//! then server settings found through autoconfiguration, with an Advanced page that is
//! always reachable, a "Test connection" that probes both servers off the UI thread, and a
//! save path that mirrors the Google sign-in's tail (account row, server settings, keyring,
//! providers, first sync).
//!
//! The same dialog reopens for "Server settings…" and "Update password" on an existing
//! account.

use super::widgets::*;
use super::MailWindow;
use crate::assets::icon;
use crate::sync::pollster_block;
use crate::theme::ActivePalette;
use crate::{caldav_sync, credentials, providers};
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{Disableable, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::*;
use overlook_core::time::now;
use overlook_core::*;
use overlook_provider::autoconfig::{self, Source, Suggestion};
use overlook_provider::caldav::fastmail_preset_for_login;
use overlook_provider::imap::ImapProvider;
use overlook_provider::pop3::Pop3Provider;
use overlook_provider::{smtp, MailProvider, ProviderError};
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SetupMode {
    New,
    /// Reopened from "Server settings…": everything editable, the password optional.
    Edit(AccountId),
    /// Reopened from "Update password": the password is required and focused.
    UpdatePassword(AccountId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetupStep {
    Basics,
    Servers,
}

pub struct AccountSetup {
    pub kind: ProviderKind,
    pub mode: SetupMode,
    pub step: SetupStep,
    pub name: Entity<InputState>,
    pub email: Entity<InputState>,
    pub password: Entity<InputState>,
    pub in_host: Entity<InputState>,
    pub in_port: Entity<InputState>,
    pub in_user: Entity<InputState>,
    pub in_security: Security,
    pub out_host: Entity<InputState>,
    pub out_port: Entity<InputState>,
    pub out_user: Entity<InputState>,
    pub out_security: Security,
    pub same_password: bool,
    pub out_password: Entity<InputState>,
    /// POP3 retention: keep messages on the server after downloading them.
    pub leave_on_server: bool,
    /// POP3 retention: with `leave_on_server`, delete server copies after this many days
    /// (empty: never).
    pub delete_after_days: Entity<InputState>,
    /// POP3 retention: delete the server copy when a message is moved to Trash.
    pub delete_when_trashed: bool,
    /// IMAP folder overrides (`ImapPolicy`): empty means "found automatically".
    pub archive_folder: Entity<InputState>,
    pub sent_folder: Entity<InputState>,
    pub trash_folder: Entity<InputState>,
    /// IMAP: without a Trash folder, flag `\Deleted` and expunge instead of making one.
    pub expunge_on_trash: bool,
    /// What the background task is doing, while it runs.
    pub busy: Option<&'static str>,
    /// Where the server settings came from.
    pub note: Option<String>,
    pub error: Option<String>,
    /// Outcome of "Test connection": one line per server.
    pub test_result: Vec<(bool, String)>,
    /// The last probe: the [`connection_key`] it ran with and whether both servers
    /// answered. Saving settings that match a failed probe is the explicit "anyway"; any
    /// edit to a connection field clears it.
    pub checked: Option<(u64, bool)>,
    /// Bumped per background task so a stale result is dropped.
    pub generation: u64,
    /// Explicit consent to reuse this new Fastmail account's incoming credential.
    pub connect_calendars: bool,
    _subscriptions: Vec<Subscription>,
}

impl AccountSetup {
    fn account_id(&self) -> Option<&AccountId> {
        match &self.mode {
            SetupMode::New => None,
            SetupMode::Edit(id) | SetupMode::UpdatePassword(id) => Some(id),
        }
    }

    fn incoming_protocol(&self) -> ServerProtocol {
        match self.kind {
            ProviderKind::Pop3 => ServerProtocol::Pop3,
            _ => ServerProtocol::Imap,
        }
    }

    fn incoming_name(&self) -> &'static str {
        match self.kind {
            ProviderKind::Pop3 => "POP3",
            _ => "IMAP",
        }
    }
}

const PROBE_TIMEOUT: Duration = Duration::from_secs(20);

fn capitalised_local_part(email: &str) -> String {
    let local = email.split('@').next().unwrap_or(email);
    let mut chars = local.chars();
    chars.next().map(|c| c.to_uppercase().collect::<String>() + chars.as_str()).unwrap_or_else(|| local.to_owned())
}

/// Identifies what a probe tested: both endpoints and the passwords, hashed so the setup
/// keeps no extra copy of them.
fn connection_key(servers: &MailServers, password: &str, out_password: Option<&str>) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    format!("{:?}{:?}", servers.incoming, servers.outgoing).hash(&mut h);
    password.hash(&mut h);
    out_password.hash(&mut h);
    h.finish()
}

/// Signs in to both servers (blocking; run it off the UI thread). One line per server.
fn probe_servers(kind: ProviderKind, servers: &MailServers, password: String, out_password: Option<String>) -> Vec<(bool, String)> {
    let (what, incoming) = if kind == ProviderKind::Pop3 {
        let pop3 = Pop3Provider::new(servers.clone(), password.clone(), std::env::temp_dir()).with_timeout(PROBE_TIMEOUT);
        ("Incoming (POP3)", pollster_block(pop3.probe()))
    } else {
        let imap = ImapProvider::new(servers.clone(), password.clone()).with_timeout(PROBE_TIMEOUT);
        let result = pollster_block(imap.probe());
        imap.disconnect();
        ("Incoming (IMAP)", result)
    };
    let outgoing = smtp::Transport::new(servers.outgoing.clone(), out_password.unwrap_or(password)).with_timeout(PROBE_TIMEOUT).probe();
    vec![probe_line(what, &servers.incoming, incoming), probe_line("Outgoing (SMTP)", &servers.outgoing, outgoing)]
}

/// One line of "Test connection" output.
fn probe_line(what: &str, endpoint: &ServerEndpoint, result: Result<(), ProviderError>) -> (bool, String) {
    match result {
        Ok(()) => (true, format!("{what}: connected to {} and signed in.", endpoint.address())),
        Err(ProviderError::SignInRequired) => (false, format!("{what}: {} did not accept the user name or password.", endpoint.host)),
        Err(ProviderError::Offline) => (false, format!("{what}: could not connect to {}. Check the host, port and security setting.", endpoint.address())),
        Err(ProviderError::Temporary(t)) | Err(ProviderError::Rejected(t)) | Err(ProviderError::RateLimited(t)) => (false, format!("{what}: {t}")),
        Err(e) => (false, format!("{what}: {e}")),
    }
}

impl MailWindow {
    pub(crate) fn open_account_setup(&mut self, kind: ProviderKind, mode: SetupMode, window: &mut Window, cx: &mut Context<Self>) {
        let existing = mode.clone();
        let protocol = if kind == ProviderKind::Pop3 { ServerProtocol::Pop3 } else { ServerProtocol::Imap };
        let (account, servers) = match &existing {
            SetupMode::New => (None, None),
            SetupMode::Edit(id) | SetupMode::UpdatePassword(id) => (self.accounts.iter().find(|a| &a.id == id).cloned(), self.store.servers(id).ok().flatten()),
        };
        let email_value = account.as_ref().map(|a| a.email.clone()).unwrap_or_default();
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("Your name").default_value(account.as_ref().map(|a| a.display_name.clone()).unwrap_or_default()));
        let email = cx.new(|cx| InputState::new(window, cx).placeholder("you@example.com").default_value(email_value.clone()));
        let password = cx.new(|cx| InputState::new(window, cx).placeholder(if matches!(existing, SetupMode::Edit(_)) { "Leave empty to keep the saved password" } else { "Password or app password" }).masked(true));
        let mut field = |cx: &mut Context<Self>, placeholder: &str, value: String| cx.new(|cx| InputState::new(window, cx).placeholder(placeholder.to_owned()).default_value(value));
        let incoming = servers.as_ref().map(|s| s.incoming.clone());
        let outgoing = servers.as_ref().map(|s| s.outgoing.clone());
        let in_security = incoming.as_ref().map(|e| e.security).unwrap_or(Security::Tls);
        let out_security = outgoing.as_ref().map(|e| e.security).unwrap_or(Security::StartTls);
        let in_host = field(cx, if protocol == ServerProtocol::Pop3 { "pop.example.com" } else { "imap.example.com" }, incoming.as_ref().map(|e| e.host.clone()).unwrap_or_default());
        let in_port = field(cx, if protocol == ServerProtocol::Pop3 { "995" } else { "993" }, incoming.as_ref().map(|e| e.port.to_string()).unwrap_or_else(|| protocol.default_port(in_security).to_string()));
        let in_user = field(cx, "Usually your email address", incoming.as_ref().map(|e| e.username.clone()).unwrap_or_else(|| email_value.clone()));
        let out_host = field(cx, "smtp.example.com", outgoing.as_ref().map(|e| e.host.clone()).unwrap_or_default());
        let out_port = field(cx, "587", outgoing.as_ref().map(|e| e.port.to_string()).unwrap_or_else(|| ServerProtocol::Smtp.default_port(out_security).to_string()));
        let out_user = field(cx, "Usually your email address", outgoing.as_ref().map(|e| e.username.clone()).unwrap_or_else(|| email_value.clone()));
        let policy = servers.as_ref().and_then(|s| s.pop3.clone()).unwrap_or_default();
        let delete_after_days = field(cx, "never", policy.delete_after_days.map(|d| d.to_string()).unwrap_or_default());
        let imap_policy = servers.as_ref().and_then(|s| s.imap.clone()).unwrap_or_default();
        let archive_folder = field(cx, "Found automatically (or created as Archive)", imap_policy.archive_folder.clone().unwrap_or_default());
        let sent_folder = field(cx, "Found automatically", imap_policy.sent_folder.clone().unwrap_or_default());
        let trash_folder = field(cx, "Found automatically (or created as Trash)", imap_policy.trash_folder.clone().unwrap_or_default());
        let out_password = cx.new(|cx| InputState::new(window, cx).placeholder("Outgoing server password").masked(true));
        let same_password = match account.as_ref() {
            Some(a) => !matches!(credentials::load_password(&a.id, credentials::KIND_PASSWORD_OUTGOING), Ok(Some(_))),
            None => true,
        };
        let step = if matches!(existing, SetupMode::New) { SetupStep::Basics } else { SetupStep::Servers };
        let focus = match step {
            SetupStep::Basics if !email_value.is_empty() => password.clone(),
            SetupStep::Basics => name.clone(),
            SetupStep::Servers if matches!(existing, SetupMode::UpdatePassword(_)) => password.clone(),
            SetupStep::Servers => in_host.clone(),
        };
        let mut subscriptions: Vec<Subscription> = [&in_host, &in_user].into_iter().map(|input| {
            cx.subscribe(input, |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    if let Some(setup) = this.account_setup.as_mut() {
                        // A changed login must not carry an earlier credential choice.
                        setup.connect_calendars = false;
                    }
                    cx.notify();
                }
            })
        }).collect();
        // Any change to what the probe tests asks for a fresh probe before saving.
        subscriptions.extend([&email, &password, &in_host, &in_port, &in_user, &out_host, &out_port, &out_user, &out_password].into_iter().map(|input| {
            cx.subscribe(input, |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    if let Some(setup) = this.account_setup.as_mut().filter(|s| s.checked.is_some()) {
                        setup.checked = None;
                        cx.notify();
                    }
                }
            })
        }));
        self.account_setup = Some(AccountSetup {
            kind,
            mode: existing,
            step,
            name,
            email,
            password,
            in_host,
            in_port,
            in_user,
            in_security,
            out_host,
            out_port,
            out_user,
            out_security,
            same_password,
            out_password,
            leave_on_server: policy.leave_on_server,
            delete_after_days,
            delete_when_trashed: policy.delete_on_server_when_trashed,
            archive_folder,
            sent_folder,
            trash_folder,
            expunge_on_trash: imap_policy.expunge_on_trash,
            busy: None,
            note: None,
            error: None,
            test_result: Vec::new(),
            checked: None,
            generation: 0,
            connect_calendars: false,
            _subscriptions: subscriptions,
        });
        focus.update(cx, |s, cx| s.focus(window, cx));
        cx.notify();
    }

    pub(crate) fn close_account_setup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.account_setup = None;
        self.focus_current_module(window, cx);
        cx.notify();
    }

    fn setup_value(&self, cx: &App, pick: impl Fn(&AccountSetup) -> &Entity<InputState>) -> String {
        self.account_setup.as_ref().map(|s| pick(s).read(cx).value().trim().to_owned()).unwrap_or_default()
    }

    /// Step 1 → step 2: look the domain up in the background, then show the servers.
    fn setup_continue(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let email = self.setup_value(cx, |s| &s.email).to_lowercase();
        let password = self.setup_value(cx, |s| &s.password);
        let Some(setup) = self.account_setup.as_mut() else { return };
        if !Address::new(None, email.clone()).looks_valid() {
            setup.error = Some("Enter the full email address, like you@example.com.".into());
            cx.notify();
            return;
        }
        if password.is_empty() {
            setup.error = Some("Enter the account’s password. Many providers want an app password here.".into());
            cx.notify();
            return;
        }
        setup.error = None;
        setup.busy = Some("Looking up the server settings…");
        setup.generation += 1;
        let generation = setup.generation;
        cx.notify();
        let protocol = setup.incoming_protocol();
        cx.spawn_in(window, async move |this, cx| {
            let suggestion = cx.background_spawn(async move { autoconfig::lookup_for(protocol, &email) }).await;
            let _ = this.update_in(cx, |this, window, cx| {
                let Some(setup) = this.account_setup.as_mut() else { return };
                if setup.generation != generation {
                    return;
                }
                setup.busy = None;
                this.apply_suggestion(suggestion, window, cx);
            });
        })
        .detach();
    }

    /// Step 1 → step 2 without the lookup.
    fn setup_manual(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let email = self.setup_value(cx, |s| &s.email).to_lowercase();
        let domain = email.rsplit('@').next().unwrap_or("").to_owned();
        let protocol = self.account_setup.as_ref().map(|s| s.incoming_protocol()).unwrap_or(ServerProtocol::Imap);
        let suggestion = autoconfig::guess_for(protocol, &domain, &email);
        self.apply_suggestion(suggestion, window, cx);
    }

    fn apply_suggestion(&mut self, suggestion: Suggestion, window: &mut Window, cx: &mut Context<Self>) {
        let Some(setup) = self.account_setup.as_mut() else { return };
        let s = &suggestion.servers;
        let provider = suggestion.provider.clone().unwrap_or_else(|| "this provider".into());
        setup.note = Some(match suggestion.source {
            Source::Ispdb => format!("Settings for {provider} from Thunderbird’s provider database. Check them, then save."),
            Source::Preset => format!("Common settings for {provider}. Check them, then save."),
            Source::Guess => "No published settings for this domain; these follow the usual mail.<domain> convention. Check them before saving.".into(),
        });
        setup.in_security = s.incoming.security;
        setup.out_security = s.outgoing.security;
        setup.step = SetupStep::Servers;
        setup.error = None;
        setup.test_result.clear();
        setup.checked = None;
        let fields = [
            (setup.in_host.clone(), s.incoming.host.clone()),
            (setup.in_port.clone(), s.incoming.port.to_string()),
            (setup.in_user.clone(), s.incoming.username.clone()),
            (setup.out_host.clone(), s.outgoing.host.clone()),
            (setup.out_port.clone(), s.outgoing.port.to_string()),
            (setup.out_user.clone(), s.outgoing.username.clone()),
        ];
        let focus = setup.in_host.clone();
        for (input, value) in fields {
            input.update(cx, |i, cx| i.set_value(value, window, cx));
        }
        focus.update(cx, |i, cx| i.focus(window, cx));
        cx.notify();
    }

    /// Step 2 → step 1 for a new account. Continuing again looks the servers up afresh.
    fn setup_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(setup) = self.account_setup.as_mut() else { return };
        setup.step = SetupStep::Basics;
        setup.error = None;
        setup.test_result.clear();
        setup.checked = None;
        setup.generation += 1;
        let focus = setup.email.clone();
        focus.update(cx, |i, cx| i.focus(window, cx));
        cx.notify();
    }

    fn setup_set_security(&mut self, incoming: bool, security: Security, window: &mut Window, cx: &mut Context<Self>) {
        let Some(setup) = self.account_setup.as_mut() else { return };
        setup.checked = None;
        let incoming_protocol = setup.incoming_protocol();
        let (protocol, port_input, current) = if incoming { (incoming_protocol, setup.in_port.clone(), &mut setup.in_security) } else { (ServerProtocol::Smtp, setup.out_port.clone(), &mut setup.out_security) };
        let previous_default = protocol.default_port(*current).to_string();
        *current = security;
        let next_default = protocol.default_port(security).to_string();
        // Follow the conventional port unless the user typed their own.
        let typed = port_input.read(cx).value().trim().to_owned();
        if typed.is_empty() || typed == previous_default {
            port_input.update(cx, |i, cx| i.set_value(next_default, window, cx));
        }
        cx.notify();
    }

    fn setup_toggle_same_password(&mut self, same: bool, cx: &mut Context<Self>) {
        if let Some(setup) = self.account_setup.as_mut() {
            setup.same_password = same;
            setup.checked = None;
        }
        cx.notify();
    }

    fn setup_toggle_leave_on_server(&mut self, leave: bool, cx: &mut Context<Self>) {
        if let Some(setup) = self.account_setup.as_mut() {
            setup.leave_on_server = leave;
        }
        cx.notify();
    }

    fn setup_toggle_delete_when_trashed(&mut self, delete: bool, cx: &mut Context<Self>) {
        if let Some(setup) = self.account_setup.as_mut() {
            setup.delete_when_trashed = delete;
        }
        cx.notify();
    }

    fn setup_toggle_expunge_on_trash(&mut self, expunge: bool, cx: &mut Context<Self>) {
        if let Some(setup) = self.account_setup.as_mut() {
            setup.expunge_on_trash = expunge;
        }
        cx.notify();
    }

    /// The server settings as typed, validated. The password is `None` when it may stay as
    /// saved (edit mode with the field left empty).
    fn setup_servers(&self, cx: &App) -> Result<(MailServers, Option<String>, Option<String>), String> {
        let setup = self.account_setup.as_ref().ok_or("no setup in progress")?;
        let value = |e: &Entity<InputState>| e.read(cx).value().trim().to_owned();
        let email = value(&setup.email).to_lowercase();
        if !Address::new(None, email.clone()).looks_valid() {
            return Err("Enter the full email address, like you@example.com.".into());
        }
        let port = |e: &Entity<InputState>, what: &str| value(e).parse::<u16>().map_err(|_| format!("The {what} port must be a number between 1 and 65535."));
        let host = |e: &Entity<InputState>, what: &str| {
            let h = value(e);
            if h.is_empty() {
                Err(format!("Enter the {what} server’s host name."))
            } else {
                Ok(h)
            }
        };
        let user_or = |e: &Entity<InputState>| {
            let u = value(e);
            if u.is_empty() {
                email.clone()
            } else {
                u
            }
        };
        let incoming = ServerEndpoint::new(host(&setup.in_host, "incoming")?, port(&setup.in_port, "incoming")?, setup.in_security, user_or(&setup.in_user));
        let outgoing = ServerEndpoint::new(host(&setup.out_host, "outgoing")?, port(&setup.out_port, "outgoing")?, setup.out_security, user_or(&setup.out_user));
        if incoming.is_insecure_remote() || outgoing.is_insecure_remote() {
            return Err("Overlook will not send your password unencrypted. Choose SSL/TLS or STARTTLS for servers other than this machine.".into());
        }
        let password = value(&setup.password);
        let password = if password.is_empty() {
            match setup.mode {
                SetupMode::Edit(_) => None,
                _ => return Err("Enter the account’s password.".into()),
            }
        } else {
            Some(password)
        };
        let out_password = if setup.same_password { None } else { Some(value(&setup.out_password)).filter(|p| !p.is_empty()) };
        if !setup.same_password && out_password.is_none() && !matches!(setup.mode, SetupMode::Edit(_)) {
            return Err("Enter the outgoing server’s password, or use the same password for both.".into());
        }
        let (pop3, imap) = match setup.kind {
            ProviderKind::Pop3 => {
                let days = value(&setup.delete_after_days);
                let delete_after_days = if days.is_empty() || days.eq_ignore_ascii_case("never") {
                    None
                } else {
                    Some(days.parse::<u32>().map_err(|_| "“Delete from the server after” must be a number of days, or empty for never.".to_owned())?)
                };
                (Some(Pop3Policy { leave_on_server: setup.leave_on_server, delete_after_days: if setup.leave_on_server { delete_after_days } else { None }, delete_on_server_when_trashed: setup.delete_when_trashed }), None)
            }
            _ => {
                let folder = |e: &Entity<InputState>| Some(value(e)).filter(|v| !v.is_empty());
                let policy = ImapPolicy { archive_folder: folder(&setup.archive_folder), sent_folder: folder(&setup.sent_folder), trash_folder: folder(&setup.trash_folder), expunge_on_trash: setup.expunge_on_trash };
                (None, Some(policy).filter(|p| *p != ImapPolicy::default()))
            }
        };
        Ok((MailServers { incoming, outgoing, pop3, imap }, password, out_password))
    }

    /// The passwords to use: typed ones, or the saved ones in edit mode.
    fn setup_passwords(&self, servers_password: Option<String>, out_password: Option<String>) -> Result<(String, Option<String>), String> {
        let setup = self.account_setup.as_ref().ok_or("no setup in progress")?;
        let incoming = match servers_password {
            Some(p) => p,
            None => {
                let id = setup.account_id().ok_or("Enter the account’s password.")?;
                credentials::load_password(id, credentials::KIND_PASSWORD_INCOMING).map_err(|e| e.to_string())?.ok_or_else(|| "No saved password: enter it.".to_owned())?
            }
        };
        let outgoing = if setup.same_password {
            None
        } else {
            match out_password {
                Some(p) => Some(p),
                None => setup.account_id().and_then(|id| credentials::load_password(id, credentials::KIND_PASSWORD_OUTGOING).ok().flatten()),
            }
        };
        Ok((incoming, outgoing))
    }

    /// The typed settings with the passwords to use, and the key a probe of them records.
    fn setup_prepared(&self, cx: &App) -> Result<(MailServers, String, Option<String>, u64), String> {
        let (servers, pw, out_pw) = self.setup_servers(cx)?;
        let (password, out_password) = self.setup_passwords(pw, out_pw)?;
        let key = connection_key(&servers, &password, out_password.as_deref());
        Ok((servers, password, out_password, key))
    }

    fn setup_test(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.setup_probe(false, window, cx);
    }

    /// "Add account" / "Save": settings that already passed a probe, or that failed one the
    /// user has seen (the button then reads "anyway"), are saved at once; anything else is
    /// probed first and saved only when both servers answer.
    fn setup_save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let prepared = self.setup_prepared(cx);
        let Some(setup) = self.account_setup.as_mut() else { return };
        match prepared {
            Ok((.., key)) if setup.checked.is_some_and(|(k, _)| k == key) => self.setup_commit(window, cx),
            Ok(_) => self.setup_probe(true, window, cx),
            Err(e) => {
                setup.error = Some(e);
                cx.notify();
            }
        }
    }

    /// Signs in to both servers off the UI thread and shows the outcome; with `then_save`,
    /// a clean result saves the account and a failure asks the user to fix it or save anyway.
    fn setup_probe(&mut self, then_save: bool, window: &mut Window, cx: &mut Context<Self>) {
        let prepared = self.setup_prepared(cx);
        let Some(setup) = self.account_setup.as_mut() else { return };
        let (servers, password, out_password, key) = match prepared {
            Ok(v) => v,
            Err(e) => {
                setup.error = Some(e);
                cx.notify();
                return;
            }
        };
        setup.error = None;
        setup.test_result.clear();
        setup.checked = None;
        setup.busy = Some(if then_save { "Checking the connection before saving…" } else { "Testing the connection…" });
        setup.generation += 1;
        let generation = setup.generation;
        let kind = setup.kind;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let lines = cx.background_spawn(async move { probe_servers(kind, &servers, password, out_password) }).await;
            let _ = this.update_in(cx, |this, window, cx| {
                let Some(setup) = this.account_setup.as_mut().filter(|s| s.generation == generation) else { return };
                let ok = lines.iter().all(|(ok, _)| *ok);
                setup.busy = None;
                setup.test_result = lines;
                setup.checked = Some((key, ok));
                if then_save && ok {
                    this.setup_commit(window, cx);
                    return;
                }
                if then_save {
                    let anyway = if matches!(setup.mode, SetupMode::New) { "Add anyway" } else { "Save anyway" };
                    setup.error = Some(format!("These settings didn’t work (details above), so the account won’t sync. Fix them, or choose {anyway}."));
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Saves the account: the same tail as finishing a Google sign-in.
    fn setup_commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let prepared = self.setup_servers(cx).and_then(|(servers, pw, out_pw)| self.setup_passwords(pw, out_pw).map(|p| (servers, p)));
        let name = self.setup_value(cx, |s| &s.name);
        let Some(setup) = self.account_setup.as_ref() else { return };
        let (servers, (password, out_password)) = match prepared {
            Ok(v) => v,
            Err(e) => {
                if let Some(s) = self.account_setup.as_mut() {
                    s.error = Some(e);
                }
                cx.notify();
                return;
            }
        };
        let email = setup.email.read(cx).value().trim().to_lowercase();
        let kind = setup.kind;
        let mode = setup.mode.clone();
        let is_new = matches!(mode, SetupMode::New);
        let connect_calendars = is_new && setup.connect_calendars;
        let account = match &mode {
            SetupMode::Edit(id) | SetupMode::UpdatePassword(id) => match self.accounts.iter().find(|a| &a.id == id).cloned() {
                Some(mut a) => {
                    if !name.is_empty() {
                        a.display_name = name.clone();
                    }
                    a
                }
                None => {
                    self.close_account_setup(window, cx);
                    return;
                }
            },
            SetupMode::New => {
                let id = AccountId(format!("{}:{email}", kind.as_key()));
                if self.accounts.iter().any(|a| a.id == id) {
                    if let Some(s) = self.account_setup.as_mut() {
                        s.error = Some(format!("{email} is already set up on this device. Use Server settings… on that account instead."));
                    }
                    cx.notify();
                    return;
                }
                Account { id, provider: kind, display_name: if name.is_empty() { capitalised_local_part(&email) } else { name.clone() }, email: email.clone(), created_at: now() }
            }
        };
        if let Err(e) = self.store.upsert_account(&account).and_then(|_| self.store.set_servers(&account.id, &servers)) {
            crate::ui::toast(window, cx, crate::notify::Level::Error, format!("Couldn’t save the account: {e}"));
            return;
        }
        if let Err(e) = credentials::save_password(&account.id, credentials::KIND_PASSWORD_INCOMING, &password) {
            crate::ui::toast(window, cx, crate::notify::Level::Error, format!("{e} Overlook cannot sync this account without it."));
            return;
        }
        let outgoing_result = match &out_password {
            Some(p) => credentials::save_password(&account.id, credentials::KIND_PASSWORD_OUTGOING, p),
            None => match credentials::delete_secret(&account.id, credentials::KIND_PASSWORD_OUTGOING) {
                // Nothing stored under that kind is the normal case.
                Ok(()) | Err(_) => Ok(()),
            },
        };
        if let Err(e) = outgoing_result {
            crate::ui::toast(window, cx, crate::notify::Level::Warning, format!("The outgoing password could not be saved: {e}"));
        }
        // Calendar configuration is durable before starting its worker. A failure
        // is reported independently and never rolls back a working mail account.
        let calendar_result = caldav_sync::add_fastmail_calendars(&self.store, &account, &servers, connect_calendars);
        // A fresh provider drops any connection made with the old settings.
        let bundle = match kind {
            ProviderKind::Pop3 => providers::pop3_providers(&account.id, servers, password, out_password.clone()),
            _ => providers::imap_providers(servers, password, out_password.clone()),
        };
        self.providers.insert(account.id.clone(), bundle);
        self.provider_issues.remove(&account.id);
        // The IDLE watcher follows the provider: started for a new account, restarted when
        // the settings change.
        self.refresh_idle_watchers();
        let _ = self.store.set_sync_state(&account.id, &SyncState::Syncing, None);
        self.accounts = self.store.accounts().unwrap_or_default();
        if self.account.id == account.id {
            self.account = account.clone();
        }
        self.account_setup = None;
        if is_new {
            crate::ui::toast(window, cx, crate::notify::Level::Success, format!("Added {}. Downloading recent mail…", account.email));
            self.settings = None;
            self.switch_account(account.id.clone(), window, cx);
        } else {
            crate::ui::toast(window, cx, crate::notify::Level::Success, format!("Saved the settings for {}.", account.email));
            if self.settings.is_some() {
                self.open_settings(super::SettingsPage::Accounts, window, cx);
            }
        }
        match calendar_result {
            Ok(Some(_)) => self.sync_caldav_account(account.id.clone(), true, cx),
            Ok(None) => {},
            Err(error) => crate::ui::toast(window, cx, crate::notify::Level::Warning,
                format!("Mail is ready, but calendars could not be added. {error}")),
        }
        self.sync_account(account.id, window, cx);
    }

    // ---------------------------------------------------------------- render

    fn security_picker(&self, id: &'static str, incoming: bool, current: Security, cx: &mut Context<Self>) -> AnyElement {
        let t = cx.entity().downgrade();
        Button::new(id)
            .small()
            .label(current.label())
            .dropdown_caret(true)
            .dropdown_menu(move |mut menu, _window, _cx| {
                for s in [Security::Tls, Security::StartTls, Security::None] {
                    let t = t.clone();
                    menu = menu.item(PopupMenuItem::new(s.label()).checked(s == current).on_click(move |_, window, cx| {
                        let _ = t.update(cx, |this, cx| this.setup_set_security(incoming, s, window, cx));
                    }));
                }
                menu
            })
            .into_any_element()
    }

    pub(crate) fn render_account_setup(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = *cx.palette();
        let Some(setup) = self.account_setup.as_ref() else { return div().into_any_element() };
        let busy = setup.busy;
        let field = |label: &str, input: AnyElement| v_flex().gap(px(4.)).child(div().text_size(px(11.)).font_weight(FontWeight::MEDIUM).text_color(p.muted).child(label.to_owned())).child(input);
        let is_pop3 = setup.kind == ProviderKind::Pop3;
        let offer_calendars = setup.mode == SetupMode::New && setup.kind == ProviderKind::Imap
            && fastmail_preset_for_login(setup.in_host.read(cx).value().trim(), setup.in_user.read(cx).value().trim()).is_some();
        let title = match (&setup.mode, setup.step) {
            (SetupMode::New, SetupStep::Basics) => format!("Add {} account", if is_pop3 { "a POP3" } else { "an IMAP" }),
            (SetupMode::New, SetupStep::Servers) => "Server settings".to_owned(),
            (SetupMode::Edit(_), _) => "Server settings".to_owned(),
            (SetupMode::UpdatePassword(_), _) => "Update password".to_owned(),
        };

        let body: AnyElement = match setup.step {
            SetupStep::Basics => v_flex()
                .gap(px(12.))
                .child(div().text_size(px(13.)).text_color(p.muted).child(if is_pop3 {
                    "Overlook downloads mail over POP3 and sends it over SMTP. Downloaded messages are kept on this device as the only copy, so back them up from Settings → About. Your password is kept in the system keyring and never written to disk by Overlook."
                } else {
                    "Overlook reads mail over IMAP and sends it over SMTP. Your password is kept in the system keyring and never written to disk by Overlook."
                }))
                .child(field("Your name", Input::new(&setup.name).into_any_element()))
                .child(field("Email address", Input::new(&setup.email).into_any_element()))
                .child(field("Password", Input::new(&setup.password).into_any_element()))
                .into_any_element(),
            SetupStep::Servers => v_flex()
                .gap(px(12.))
                // Always shown: the address or password may need correcting after a failed
                // test, without going Back (which looks the servers up again).
                .child(h_flex().gap(px(10.)).child(div().flex_1().child(field("Your name", Input::new(&setup.name).into_any_element()))).child(div().flex_1().child(field("Email address", Input::new(&setup.email).into_any_element()))))
                .child(field("Password", Input::new(&setup.password).into_any_element()))
                .child(divider(&p))
                .children(setup.note.clone().map(|n| div().text_size(px(12.)).text_color(p.muted).child(n)))
                .child(div().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).child(format!("Incoming ({})", setup.incoming_name())))
                .child(h_flex().gap(px(10.)).child(div().flex_1().child(field("Host", Input::new(&setup.in_host).into_any_element()))).child(div().w(px(90.)).child(field("Port", Input::new(&setup.in_port).into_any_element()))).child(field("Security", self.security_picker("setup-in-security", true, setup.in_security, cx))))
                .child(field("User name", Input::new(&setup.in_user).into_any_element()))
                .child(div().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).pt(px(6.)).child("Outgoing (SMTP)"))
                .child(h_flex().gap(px(10.)).child(div().flex_1().child(field("Host", Input::new(&setup.out_host).into_any_element()))).child(div().w(px(90.)).child(field("Port", Input::new(&setup.out_port).into_any_element()))).child(field("Security", self.security_picker("setup-out-security", false, setup.out_security, cx))))
                .child(field("User name", Input::new(&setup.out_user).into_any_element()))
                .child(h_flex().items_center().gap(px(10.)).child(Switch::new("setup-same-password").checked(setup.same_password).label("Use the same password for sending").on_click(cx.listener(|this, checked: &bool, _w, cx| this.setup_toggle_same_password(*checked, cx)))))
                .when(!setup.same_password, |d| d.child(field("Outgoing password", Input::new(&setup.out_password).into_any_element())))
                .when(offer_calendars, |d| d
                    .child(divider(&p))
                    .child(div().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).child("Fastmail calendars"))
                    .child(Switch::new("setup-fastmail-calendars")
                        .checked(setup.connect_calendars).disabled(busy.is_some())
                        .label("Connect calendars using this mail password")
                        .accessibility_label("Connect Fastmail calendars using this mail password")
                        .on_click(cx.listener(|this, checked: &bool, _, cx| {
                            if let Some(setup) = this.account_setup.as_mut() { setup.connect_calendars = *checked; }
                            cx.notify();
                        })))
                    .child(div().text_size(px(12.)).text_color(p.muted).child("Use a Fastmail app password with calendar access. Calendars connect in the background after you add the account. If access is denied, mail keeps working and you can enter a separate calendar password in Settings → Accounts → Calendars.")))
                .when(!is_pop3, |d| {
                    d.child(div().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).pt(px(6.)).child("Folders on the server"))
                        .child(div().text_size(px(12.)).text_color(p.muted).child("Sent, Trash and Archive are found from the server’s folder list. Name a folder here only when the server gets it wrong (paths as the server names them, like INBOX/Archive)."))
                        .child(field("Archive folder", Input::new(&setup.archive_folder).into_any_element()))
                        .child(field("Sent folder", Input::new(&setup.sent_folder).into_any_element()))
                        .child(field("Trash folder", Input::new(&setup.trash_folder).into_any_element()))
                        .child(h_flex().items_center().gap(px(10.)).child(Switch::new("setup-expunge-on-trash").checked(setup.expunge_on_trash).label("Delete permanently when the server has no Trash folder").on_click(cx.listener(|this, checked: &bool, _w, cx| this.setup_toggle_expunge_on_trash(*checked, cx)))))
                        .when(setup.expunge_on_trash, |d| d.child(div().text_size(px(12.)).text_color(p.muted).child("Without a Trash folder, deleting a message removes it from the server at once instead of creating one.")))
                })
                .when(is_pop3, |d| {
                    d.child(div().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).pt(px(6.)).child("On the server"))
                        .child(h_flex().items_center().gap(px(10.)).child(Switch::new("setup-leave-on-server").checked(setup.leave_on_server).label("Leave messages on the server after downloading").on_click(cx.listener(|this, checked: &bool, _w, cx| this.setup_toggle_leave_on_server(*checked, cx)))))
                        .when(setup.leave_on_server, |d| {
                            d.child(
                                h_flex()
                                    .items_center()
                                    .gap(px(10.))
                                    .child(div().text_size(px(13.)).child("Delete from the server after"))
                                    .child(div().w(px(90.)).child(Input::new(&setup.delete_after_days).into_any_element()))
                                    .child(div().text_size(px(13.)).text_color(p.muted).child("days (empty: never)")),
                            )
                        })
                        .when(!setup.leave_on_server, |d| d.child(div().text_size(px(12.)).text_color(p.muted).child("Each message is deleted from the server once its download is saved here.")))
                        .child(h_flex().items_center().gap(px(10.)).child(Switch::new("setup-delete-when-trashed").checked(setup.delete_when_trashed).label("Also delete from the server when a message is moved to Trash").on_click(cx.listener(|this, checked: &bool, _w, cx| this.setup_toggle_delete_when_trashed(*checked, cx)))))
                })
                .children(setup.test_result.iter().map(|(ok, line)| {
                    h_flex()
                        .items_start()
                        .gap(px(8.))
                        .text_size(px(12.))
                        .child(icon(if *ok { "circle-check" } else { "circle-x" }).size(px(14.)).text_color(if *ok { p.success } else { p.error }))
                        .child(div().flex_1().min_w_0().child(line.clone()))
                }))
                .into_any_element(),
        };

        // The message gets its own wrapping line so it never pushes the buttons out of
        // the dialog.
        let message = setup.error.clone().map(|e| (e, p.error)).or_else(|| busy.map(|b| (b.to_owned(), p.muted)));
        let buttons = h_flex()
            .items_center()
            .justify_end()
            .flex_wrap()
            .gap(px(8.))
            .child(Button::new("setup-cancel").ghost().small().label("Cancel").on_click(cx.listener(|this, _, window, cx| this.close_account_setup(window, cx))))
            .map(|d| match setup.step {
                SetupStep::Basics => d
                    .child(Button::new("setup-manual").small().label("Set up servers manually").disabled(busy.is_some()).on_click(cx.listener(|this, _, window, cx| this.setup_manual(window, cx))))
                    .child(Button::new("setup-continue").primary().small().label("Continue").loading(busy.is_some()).on_click(cx.listener(|this, _, window, cx| this.setup_continue(window, cx)))),
                SetupStep::Servers => {
                    let is_new = matches!(setup.mode, SetupMode::New);
                    let failed = matches!(setup.checked, Some((_, false)));
                    let save_label = match (is_new, failed) {
                        (true, false) => "Add account",
                        (true, true) => "Add anyway",
                        (false, false) => "Save",
                        (false, true) => "Save anyway",
                    };
                    d.when(is_new, |d| d.child(Button::new("setup-back").ghost().small().label("Back").disabled(busy.is_some()).on_click(cx.listener(|this, _, window, cx| this.setup_back(window, cx)))))
                        .child(Button::new("setup-test").small().label("Test connection").disabled(busy.is_some()).on_click(cx.listener(|this, _, window, cx| this.setup_test(window, cx))))
                        .child(Button::new("setup-save").small().map(|b| if failed { b.danger() } else { b.primary() }).label(save_label).loading(busy.is_some()).disabled(busy.is_some()).on_click(cx.listener(|this, _, window, cx| this.setup_save(window, cx))))
                }
            });
        let footer = v_flex()
            .gap(px(8.))
            .pt(px(6.))
            .children(message.map(|(text, color)| div().w_full().text_size(px(12.)).text_color(color).child(text)))
            .child(buttons);

        div()
            .id("account-setup-overlay")
            .absolute()
            .inset_0()
            // Nothing underneath (a message body) may see the clicks meant for this.
            .occlude()
            .bg(gpui_kit::black().opacity(if p.is_dark() { 0.55 } else { 0.3 }))
            .flex()
            .items_center()
            .justify_center()
            .on_click(cx.listener(|this, _, window, cx| this.close_account_setup(window, cx)))
            .child(
                v_flex()
                    .id("account-setup-dialog")
                    .w(px(560.))
                    // A flex child with `min_h_0` needs a definite height to fill; without
                    // this the body and footer collapse to nothing.
                    .h(px(if setup.step == SetupStep::Basics { 400. } else { 1060. }))
                    .max_w(relative(0.94))
                    .max_h(relative(0.92))
                    .rounded(px(12.))
                    .overflow_hidden()
                    .bg(p.surface)
                    .border_1()
                    .border_color(p.divider)
                    .shadow_lg()
                    .on_click(|_, _, cx| cx.stop_propagation())
                    .child(
                        h_flex()
                            .items_center()
                            .h(px(52.))
                            .px(px(20.))
                            .border_b_1()
                            .border_color(p.divider)
                            .child(div().flex_1().text_size(px(16.)).font_weight(FontWeight::SEMIBOLD).child(title))
                            .child(kbd("Esc", &p))
                            .child(Button::new("setup-close").ghost().small().icon(icon("x").size(px(16.))).ml(px(8.)).on_click(cx.listener(|this, _, window, cx| this.close_account_setup(window, cx)))),
                    )
                    .child(v_flex().id("account-setup-content").flex_1().min_h_0().p(px(20.)).gap(px(14.)).child(body).child(footer).overflow_y_scrollbar()),
            )
            .into_any_element()
    }
}
