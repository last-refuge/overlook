//! The main mail window: header, sidebar, message list, reader, composer, status bar.

mod account_setup;
mod calendar;
mod calendar_setup;
mod calendar_changes;
mod composer;
mod contacts;
mod mail_highlight;
mod palette;
mod list;
mod settings;
mod reader;
mod sidebar;
mod widgets;

use overlook_provider::gmail::api::SessionExpiry;
use crate::assets::icon;
use crate::google_signin::{self, PendingSignIn, SignInResult};
use crate::updater::{self, InstallKind, PacmanOutcome, Progress, ReleaseInfo};
use crate::{credentials, providers};
use crate::sync::{run_sync, SyncOutcome};
use crate::theme::{self, ActivePalette, Palette};
use crate::SETTING_APPEARANCE;
pub use account_setup::{AccountSetup, SetupMode};
pub use calendar::{CalView, Module, NewEvent};
pub use composer::Composer;
pub use settings::{SettingsPage, SettingsState};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{
    h_resizable, resizable_panel, v_resizable, ResizableState, Sizable, TitleBar, WindowExt,
};
use gpui_kit::prelude::*;
use gpui_kit::*;
use gpui_kit::base::{h_flex, v_flex};
use overlook_core::time::{format_clock, now};
use overlook_core::*;
use overlook_provider::sample::SampleProvider;
use overlook_store::{LocalChange, ListFilter, MailIndex, MailboxView, Store};
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use widgets::*;

actions!(
    overlook,
    [
        Compose,
        FocusSearch,
        OpenCommandPalette,
        Escape,
        SendMessage,
        Reply,
        ReplyAll,
        Forward,
        NextConversation,
        PrevConversation,
        SwitchToMail,
        SwitchToCalendar,
        SwitchToContacts,
        CalendarPrevious,
        CalendarNext,
        CalendarToday,
        CalendarDay,
        CalendarWeek,
        CalendarAgenda,
        CalendarMonth,
        NewEventAction,
        SaveCalendarEvent,
        OpenSettings,
        ArchiveSelected,
        TrashSelected,
        ToggleStar,
        ToggleRead,
        OpenSelected,
        SyncNow,
        CalendarDeleteSelected,
        CalendarEditSelected,
        CalendarSelectNext,
        CalendarSelectPrevious,
        FormatBold,
        FormatItalic,
        FormatStrike,
        FormatCode,
        FormatBullets,
        FormatNumbers,
        FormatQuote,
        TogglePreview,
    ]
);

const WINDOW_CONTEXT: &str = "MailWindow";
const LIST_CONTEXT: &str = "MessageList";
const CALENDAR_CONTEXT: &str = "CalendarView";

/// Incoming, unread Inbox messages in `next` that `previous` did not have, newest first,
/// filtered by the new-mail preference (Focused only follows the Focused Inbox split).
pub(crate) fn new_inbox_mail(
    previous: &MailIndex,
    next: &MailIndex,
    owner: &str,
    pref: NewMailNotifications,
    focused_enabled: bool,
    since: i64,
) -> Vec<Message> {
    if pref == NewMailNotifications::Off {
        return Vec::new();
    }
    let inbox = MailboxView::Kind(LabelKind::Inbox);
    let focus = (pref == NewMailNotifications::FocusedOnly && focused_enabled).then_some(FocusTab::Focused);
    let mut out: Vec<Message> = next
        .conversations(owner, &inbox, ListFilter::Unread, focus)
        .iter()
        .flat_map(|c| next.message_ids_in(&c.id))
        .filter(|id| previous.message(id).is_none())
        .filter_map(|id| next.message(&id).cloned())
        .filter(|m| !m.is_read && !m.is_outgoing && m.date >= since)
        .collect();
    out.sort_by_key(|m| std::cmp::Reverse(m.date));
    out
}

/// Shows an in-app toast and, when the Omarchy option is on, the same message as a
/// desktop notification. Plain information stays in the app.
pub(crate) fn toast(window: &mut Window, cx: &mut App, level: crate::notify::Level, text: impl Into<SharedString>) {
    use crate::notify::Level;
    let text: SharedString = text.into();
    crate::notify::mirror(level, &text);
    let note = match level {
        Level::Success => Notification::success(text),
        Level::Warning => Notification::warning(text),
        Level::Error => Notification::error(text),
        Level::Info | Level::Mail | Level::Reminder => Notification::info(text),
    };
    window.push_notification(note, cx);
}

/// Stored appearance preference, readable from window-level observers.
pub struct AppearanceGlobal(pub AppearancePreference);
impl Global for AppearanceGlobal {}

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("ctrl-n", Compose, Some(WINDOW_CONTEXT)),
        KeyBinding::new("ctrl-e", FocusSearch, Some(WINDOW_CONTEXT)),
        KeyBinding::new("ctrl-k", OpenCommandPalette, Some(WINDOW_CONTEXT)),
        KeyBinding::new("ctrl-shift-p", OpenCommandPalette, Some(WINDOW_CONTEXT)),
        KeyBinding::new("escape", Escape, Some(WINDOW_CONTEXT)),
        KeyBinding::new("ctrl-enter", SendMessage, Some(WINDOW_CONTEXT)),
        KeyBinding::new("ctrl-r", Reply, Some(WINDOW_CONTEXT)),
        KeyBinding::new("ctrl-shift-r", ReplyAll, Some(WINDOW_CONTEXT)),
        KeyBinding::new("ctrl-f", Forward, Some(WINDOW_CONTEXT)),
        KeyBinding::new("ctrl-u", ToggleRead, Some(WINDOW_CONTEXT)),
        KeyBinding::new("f5", SyncNow, Some(WINDOW_CONTEXT)),
        KeyBinding::new("ctrl-1", SwitchToMail, Some(WINDOW_CONTEXT)),
        KeyBinding::new("ctrl-2", SwitchToCalendar, Some(WINDOW_CONTEXT)),
        KeyBinding::new("ctrl-3", SwitchToContacts, Some(WINDOW_CONTEXT)),
        KeyBinding::new("ctrl-shift-a", NewEventAction, Some(WINDOW_CONTEXT)),
        KeyBinding::new("ctrl-s", SaveCalendarEvent, Some(WINDOW_CONTEXT)),
        KeyBinding::new("ctrl-,", OpenSettings, Some(WINDOW_CONTEXT)),
        KeyBinding::new("ctrl-b", FormatBold, Some(WINDOW_CONTEXT)),
        KeyBinding::new("ctrl-i", FormatItalic, Some(WINDOW_CONTEXT)),
        KeyBinding::new("ctrl-shift-x", FormatStrike, Some(WINDOW_CONTEXT)),
        KeyBinding::new("ctrl-shift-c", FormatCode, Some(WINDOW_CONTEXT)),
        KeyBinding::new("ctrl-shift-8", FormatBullets, Some(WINDOW_CONTEXT)),
        KeyBinding::new("ctrl-shift-7", FormatNumbers, Some(WINDOW_CONTEXT)),
        KeyBinding::new("ctrl-shift-9", FormatQuote, Some(WINDOW_CONTEXT)),
        KeyBinding::new("ctrl-shift-p", TogglePreview, Some(WINDOW_CONTEXT)),
        KeyBinding::new("left", CalendarPrevious, Some(CALENDAR_CONTEXT)),
        KeyBinding::new("right", CalendarNext, Some(CALENDAR_CONTEXT)),
        KeyBinding::new("t", CalendarToday, Some(CALENDAR_CONTEXT)),
        KeyBinding::new("d", CalendarDay, Some(CALENDAR_CONTEXT)),
        KeyBinding::new("w", CalendarWeek, Some(CALENDAR_CONTEXT)),
        KeyBinding::new("a", CalendarAgenda, Some(CALENDAR_CONTEXT)),
        KeyBinding::new("m", CalendarMonth, Some(CALENDAR_CONTEXT)),
        KeyBinding::new("delete", CalendarDeleteSelected, Some(CALENDAR_CONTEXT)),
        KeyBinding::new("enter", CalendarEditSelected, Some(CALENDAR_CONTEXT)),
        KeyBinding::new("down", CalendarSelectNext, Some(CALENDAR_CONTEXT)),
        KeyBinding::new("up", CalendarSelectPrevious, Some(CALENDAR_CONTEXT)),
        KeyBinding::new("j", NextConversation, Some(LIST_CONTEXT)),
        KeyBinding::new("down", NextConversation, Some(LIST_CONTEXT)),
        KeyBinding::new("k", PrevConversation, Some(LIST_CONTEXT)),
        KeyBinding::new("up", PrevConversation, Some(LIST_CONTEXT)),
        KeyBinding::new("e", ArchiveSelected, Some(LIST_CONTEXT)),
        KeyBinding::new("backspace", ArchiveSelected, Some(LIST_CONTEXT)),
        KeyBinding::new("delete", TrashSelected, Some(LIST_CONTEXT)),
        KeyBinding::new("s", ToggleStar, Some(LIST_CONTEXT)),
        KeyBinding::new("u", ToggleRead, Some(LIST_CONTEXT)),
        KeyBinding::new("enter", OpenSelected, Some(LIST_CONTEXT)),
    ]);
}

pub struct Nav {
    pub label: SharedString,
    pub icon: &'static str,
    pub view: MailboxView,
    pub color: Option<Hsla>,
}

impl Nav {
    pub fn view(label: &str, icon: &'static str, view: MailboxView) -> Self {
        Self { label: label.to_owned().into(), icon, view, color: None }
    }
}

pub enum AfterDownload {
    Open,
    SaveTo(PathBuf),
    /// An inline image: the reader picks it up on the next render.
    Inline,
    /// Show the image in the quick look overlay.
    QuickLook,
}

/// What the quick look overlay shows: one image, or the rendered pages of a PDF.
#[derive(Clone, Debug)]
pub struct QuickLook {
    pub attachment: Attachment,
    pub pages: Vec<PathBuf>,
    pub rendering: bool,
    pub error: Option<String>,
}

/// A local write the window already shows but the store may not have yet.
#[derive(Clone)]
enum LocalWrite {
    Change(LocalChange),
    FocusOverride { ids: Vec<MessageId>, focused: Option<bool> },
    FocusRule { email: String, focused: Option<bool> },
}

/// The server-side part of a search.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OnlineSearch {
    Idle,
    Searching,
    Done { added: usize },
    Offline,
    Failed(String),
}

/// A conversation being dragged from the list onto the sidebar.
#[derive(Clone, Debug)]
pub struct ConversationDrag {
    pub message_ids: Vec<MessageId>,
    pub title: String,
    pub count: usize,
}

/// The small card that follows the pointer during a drag.
pub struct DragPreview {
    pub label: String,
}

impl Render for DragPreview {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = *cx.palette();
        h_flex()
            .items_center()
            .gap(px(8.))
            .px(px(12.))
            .py(px(7.))
            .rounded(px(8.))
            .bg(p.surface)
            .border_1()
            .border_color(p.accent)
            .shadow_md()
            .text_size(px(13.))
            .text_color(p.foreground)
            .child(icon("mail").size(px(14.)).text_color(p.accent))
            .child(self.label.clone())
    }
}

#[derive(Clone, PartialEq)]
pub enum ListRow {
    Group(SharedString, usize),
    Conversation(ConversationSummary),
    Draft(Draft),
}

/// Hands a received attachment to the desktop, except programs and scripts: those are
/// never launched from Overlook (saving them is still possible). Downloaded files are
/// written without the executable bit.
pub(super) fn open_received_attachment(path: &std::path::Path, filename: &str, window: &mut Window, cx: &mut App) {
    use overlook_core::attachment_policy::{blocked_extension, is_executable_content};
    let head = std::fs::File::open(path).and_then(|mut f| { use std::io::Read; let mut buf = vec![0; 512]; let n = f.read(&mut buf)?; buf.truncate(n); Ok(buf) }).unwrap_or_default();
    if blocked_extension(filename).is_some() || is_executable_content(&head) {
        tracing::info!(target: "overlook::attachments", "refused to open a received program or script");
        crate::ui::toast(window, cx, crate::notify::Level::Warning, "This attachment is a program or script, so Overlook won’t open it. Use Save as… only if you trust the sender.");
        return;
    }
    cx.open_with_system(path);
}

const SIGNED_OUT_WORDING: &str = "You signed out of this account. Sign in again to continue syncing. Downloaded mail stays on this device.";
const SESSION_IDLE_WORDING: &str = "Signed out after 90 days without use. Sign in again to continue syncing.";
const SESSION_AGE_WORDING: &str = "Signed out because this sign-in is 180 days old. Sign in again to continue syncing.";

/// Ends an account's session everywhere it exists: revokes the Google grant (through the
/// shared client, so cloned providers stop too) and deletes every stored credential.
/// Shared by Sign out and Remove account. Blocking; run in the background.
fn end_session(account: &Account, id: &AccountId, gmail: Option<Arc<overlook_provider::gmail::GmailClient>>) -> Result<(), String> {
    if account.provider == ProviderKind::Gmail {
        if let Some(client) = gmail {
            client.disconnect().map_err(|e| e.to_string())?;
        } else if let Some(tokens) = credentials::load_tokens(id).map_err(|e| e.to_string())? {
            overlook_provider::gmail::oauth::revoke(&tokens).map_err(|e| e.to_string())?;
        }
    }
    if account.provider != ProviderKind::Sample {
        credentials::delete_all_secrets(id).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// "Sign in again" for Google accounts, "Update password" for password accounts.
pub(crate) fn sign_in_label_for(provider: ProviderKind) -> &'static str {
    match provider {
        ProviderKind::Imap | ProviderKind::Pop3 => "Update password",
        _ => "Sign in again",
    }
}

/// A Google sign-in in progress (the browser has been opened).
pub struct SignIn {
    cancel: Arc<AtomicBool>,
    /// The address this sign-in was started for ("Sign in again"), if any.
    expected: Option<String>,
}

/// The warning shown when Google returns a different account than the one being signed in
/// again: the sign-in still counts for the account actually chosen, but the original stays
/// signed out.
fn sign_in_mismatch(expected: Option<&str>, actual: &str, added: bool) -> Option<String> {
    let expected = expected?.trim().to_lowercase();
    if expected.is_empty() || expected == actual {
        return None;
    }
    Some(if added {
        format!("You signed in as {actual}, not {expected}, so {actual} was added. {expected} still needs you to sign in again.")
    } else {
        format!("You signed in as {actual}, not {expected}. {expected} still needs you to sign in again.")
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UpdatePhase {
    Idle,
    Checking,
    Downloading,
    /// The new version is in place; restart to run it.
    ReadyToRestart,
    /// A terminal window is finishing the pacman step.
    TerminalOpened,
    Failed(String),
}

pub struct UpdateState {
    pub install: InstallKind,
    pub latest: Option<ReleaseInfo>,
    pub phase: UpdatePhase,
    pub progress: Option<Arc<Progress>>,
    pub last_check: i64,
    pub last_result: Option<String>,
    pub dismissed: Option<String>,
}

pub struct MailWindow {
    focus_handle: FocusHandle,
    list_focus: FocusHandle,
    store: Arc<Store>,
    sample: SampleProvider,
    accounts: Vec<Account>,
    providers: HashMap<AccountId, providers::AccountProviders>,
    /// Why an account has no provider (shown as “Sign-in required”).
    provider_issues: HashMap<AccountId, String>,
    removing_accounts: HashSet<AccountId>,
    /// The `IDLE` thread per IMAP account (see `overlook_provider::imap::idle`); each one
    /// reports Inbox changes through `idle_events`, which `watch_idle_events` turns into a
    /// sync of that account. Rebuilt by `refresh_idle_watchers` whenever `providers` change.
    idle_watchers: HashMap<AccountId, overlook_provider::imap::idle::IdleWatcher>,
    idle_events: tokio::sync::mpsc::UnboundedSender<AccountId>,
    signin: Option<SignIn>,
    /// The account whose mail the window shows.
    account: Account,
    utc_offset: i32,
    outgoing_dir: PathBuf,

    view: MailboxView,
    filter: ListFilter,
    search_input: Entity<InputState>,
    search_query: String,
    search_coverage: String,
    /// The server-side half of a search: bumped per query so stale results are dropped.
    search_generation: u64,
    online_search: OnlineSearch,

    labels: Vec<Label>,
    conversations: Vec<ConversationSummary>,
    counts: HashMap<MailboxView, usize>,
    /// Headers of every message of the current account, for building the list and the
    /// counts without the database. A local change lands here first so the list updates
    /// at once; the store write and a fresh copy follow on a background thread.
    mail_index: MailIndex,
    mail_index_account: Option<AccountId>,
    /// Indexes of the other accounts, so switching back shows them at once.
    other_indexes: HashMap<AccountId, MailIndex>,
    /// Bumped whenever the index changes; a background rebuild started earlier is dropped.
    mail_index_generation: u64,
    /// Local changes waiting to be written, in order, and the batch being written now.
    local_writes: VecDeque<(AccountId, LocalWrite)>,
    local_in_flight: Vec<(AccountId, LocalWrite)>,
    /// Off-screen renders of the conversations next to the open one; replaced (and so
    /// cancelled) whenever the selection moves.
    prerender: Option<Task<()>>,
    /// Background download of attachments the reader would fetch on open.
    prefetching: bool,
    first_frame_logged: bool,
    palette: Option<palette::CommandPalette>,
    /// When this window opened; mail older than this is never announced as new.
    session_started: i64,
    /// Reply drafts discarded or closed empty while their first save was still running;
    /// the save removes them again when it lands.
    unwanted_drafts: HashSet<DraftId>,
    /// The message list draws only the rows in view; it keeps the rows it was last given
    /// so a change re-measures just the rows that differ.
    list_state: ListState,
    shown_rows: std::rc::Rc<std::cell::RefCell<Vec<ListRow>>>,
    drafts: Vec<Draft>,
    outbox: Vec<Draft>,
    status: AccountStatus,

    selected: Option<ConversationId>,
    outbox_selected: Option<DraftId>,
    thread: Vec<Message>,
    expanded: BTreeSet<MessageId>,
    remote_allowed: BTreeSet<MessageId>,
    /// The message whose sender's contact card is open.
    pub(crate) contact_card: Option<MessageId>,
    // Contacts module state.
    pub(crate) contacts: Vec<Contact>,
    pub(crate) contact_selected: Option<String>,
    pub(crate) contact_search: Entity<InputState>,
    pub(crate) contact_query: String,
    pub(crate) contact_filter: contacts::ContactFilter,
    pub(crate) contact_form: Option<contacts::ContactForm>,
    /// An image or PDF attachment shown over the window.
    pub(crate) quick_look: Option<QuickLook>,
    /// First-page thumbnails of PDF attachments, rendered in the background.
    pub(crate) pdf_thumbs: HashMap<AttachmentId, PathBuf>,
    pub(crate) pdf_thumbs_pending: HashSet<AttachmentId>,
    /// Inline images already encoded as `data:` URLs, by attachment.
    pub(crate) inline_images: std::cell::RefCell<HashMap<AttachmentId, Option<String>>>,
    /// Live isolated message documents; released when their reader surface closes.
    pub(crate) browser_bodies: std::cell::RefCell<HashMap<String, (u64, usize, Entity<crate::browser_body::BrowserBody>)>>,

    composer: Option<Composer>,
    appearance: AppearancePreference,
    density: Density,
    reading_pane: ReadingPane,
    /// Sidebar sections the user collapsed ("favorites", "labels"); persisted.
    collapsed_sections: HashSet<String>,
    /// Focused Inbox: enabled per profile, with the selected tab.
    focused_enabled: bool,
    inbox_tab: FocusTab,
    /// Attachments being fetched right now.
    downloading: HashSet<AttachmentId>,
    /// "Back up mail…" is copying the profile.
    pub(crate) backing_up: bool,
    attachments_dir: PathBuf,

    // Calendar module state.
    pub(crate) module: Module,
    pub(crate) cal_view: CalView,
    pub(crate) cal_anchor: i64,
    pub(crate) cal_events: Vec<Event>,
    pub(crate) calendars: Vec<Calendar>,
    pub(crate) cal_selected: Option<EventId>,
    pub(crate) cal_status: SyncState,
    pub(crate) caldav_connection: Option<CalendarConnection>,
    pub(crate) caldav_cached: bool,
    pub(crate) caldav_last_sync: Option<i64>,
    pub(crate) caldav_changes: Vec<CalDavChange>,
    pub(crate) caldav_previews: HashMap<OperationId, String>,
    pub(crate) show_caldav_changes: bool,
    caldav_syncing: HashMap<AccountId, CalendarConnectionId>,
    caldav_next_retry: HashMap<AccountId, i64>,
    caldav_requested: HashSet<AccountId>,
    pub(crate) reader_scroll: ScrollHandle,
    pub(crate) cal_scroll: ScrollHandle,
    pub(crate) cal_scrolled_for: std::cell::Cell<Option<(CalView, i64)>>,
    pub(crate) cal_focus: FocusHandle,
    pub(crate) new_event: Option<NewEvent>,
    /// "This event / this and following / all" question pending for a repeating event.
    pub(crate) cal_scope_prompt: Option<calendar::ScopePrompt>,
    /// An event being dragged in the time grid.
    pub(crate) cal_drag: Option<calendar::CalDrag>,
    pub(crate) reminders_enabled: bool,
    /// Reminders already shown this session: (event, minutes before).
    pub(crate) notified: HashSet<(EventId, i32)>,
    /// Reminders put off from the toast: event → when to show it again.
    pub(crate) snoozed_reminders: HashMap<EventId, i64>,
    pub(crate) prefs: Prefs,
    pub(crate) account_prefs: HashMap<AccountId, AccountPrefs>,
    pub(crate) settings: Option<SettingsState>,
    /// The IMAP/POP3 account dialog, when open.
    pub(crate) account_setup: Option<AccountSetup>,
    pub(crate) update: UpdateState,
    /// Generation counter for delayed mark-as-read timers.
    mark_read_generation: u64,
    last_sync_tick: i64,
    sidebar_width: f32,
    list_width: f32,
    list_height: f32,
    panes: Entity<ResizableState>,
    content_panes_h: Entity<ResizableState>,
    content_panes_v: Entity<ResizableState>,

    syncing: HashSet<AccountId>,
    sync_requested: HashSet<AccountId>,
    /// Earliest time (unix ms) a background sync may start again, per account, after a
    /// temporary failure or rate limit. Manual Sync now ignores it.
    next_retry: HashMap<AccountId, i64>,
    retry_attempts: HashMap<AccountId, u32>,
    _subscriptions: Vec<Subscription>,
}

impl MailWindow {
    pub fn new(
        store: Arc<Store>,
        sample: SampleProvider,
        accounts: Vec<Account>,
        active: AccountId,
        appearance: AppearancePreference,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        tracing::debug!("startup: window opening after {} ms", crate::since_start_ms());
        let account = accounts.iter().find(|a| a.id == active).cloned().unwrap_or_else(|| accounts[0].clone());
        let mut providers: HashMap<AccountId, providers::AccountProviders> = HashMap::new();
        let mut provider_issues = HashMap::new();
        for a in &accounts {
            match providers::providers_for(a, &sample, &store) {
                Ok(p) => {
                    providers.insert(a.id.clone(), p);
                }
                Err(providers::ProviderIssue::SignInRequired(reason)) => {
                    provider_issues.insert(a.id.clone(), reason);
                }
            }
        }
        cx.set_global(AppearanceGlobal(appearance));
        let search_input = cx.new(|cx| InputState::new(window, cx).placeholder("Search mail"));
        let contact_search = cx.new(|cx| InputState::new(window, cx).placeholder("Search contacts"));
        let mut subs = Vec::new();
        tracing::debug!("startup: accounts and sign-ins loaded after {} ms", crate::since_start_ms());
        let (idle_tx, idle_rx) = tokio::sync::mpsc::unbounded_channel::<AccountId>();
        subs.push(cx.subscribe_in(&contact_search, window, |this, state, event: &InputEvent, _window, cx| {
            if matches!(event, InputEvent::Change) {
                this.contact_query = state.read(cx).value().to_string();
                cx.notify();
            }
        }));
        subs.push(cx.subscribe_in(&search_input, window, |this, state, event: &InputEvent, window, cx| match event {
            InputEvent::Change => {
                let value = state.read(cx).value().to_string();
                this.set_search(value, window, cx);
            }
            InputEvent::PressEnter { .. } => this.list_focus.focus(window, cx),
            _ => {}
        }));
        subs.push(window.observe_window_appearance(|window, cx| {
            let pref = cx.global::<AppearanceGlobal>().0;
            theme::apply_preference(pref, theme::system_appearance(window.appearance()), cx);
        }));

        let density = store.setting::<Density>("density").ok().flatten().unwrap_or_default();
        let reading_pane = store.setting::<ReadingPane>("reading_pane").ok().flatten().unwrap_or_default();
        let sidebar_width = store.setting::<f32>("panes.sidebar").ok().flatten().unwrap_or(232.);
        let collapsed_sections: HashSet<String> = store.setting::<Vec<String>>("sidebar.collapsed").ok().flatten().unwrap_or_default().into_iter().collect();
        let focused_enabled = store.setting::<bool>("focused_inbox").ok().flatten().unwrap_or(true);
        let prefs: Prefs = store.setting::<Prefs>("prefs").ok().flatten().unwrap_or_default();
        crate::notify::set_mirror(prefs.omarchy_notifications);
        let reminders_enabled = prefs.event_reminders;
        overlook_core::tz::set_clock_24h(prefs.clock_24h);
        let update_last_check = store.setting::<i64>("update.last_check").ok().flatten().unwrap_or(0);
        let update_dismissed = store.setting::<String>("update.dismissed").ok().flatten();
        let account_prefs: HashMap<AccountId, AccountPrefs> = accounts
            .iter()
            .map(|a| (a.id.clone(), store.setting::<AccountPrefs>(&format!("account_prefs:{}", a.id.0)).ok().flatten().unwrap_or_default().normalized()))
            .collect();
        let startup_module = match prefs.startup_module {
            StartupModule::Mail => Module::Mail,
            StartupModule::Calendar => Module::Calendar,
            StartupModule::LastUsed => match store.setting::<String>("last_module").ok().flatten().as_deref() {
                Some("calendar") => Module::Calendar,
                Some("contacts") => Module::Contacts,
                _ => Module::Mail,
            },
        };
        let attachments_dir = crate::data_dir().join("attachments");
        let list_width = store.setting::<f32>("panes.list").ok().flatten().unwrap_or(340.);
        let list_height = store.setting::<f32>("panes.list_height").ok().flatten().unwrap_or(300.);
        let outgoing_dir = crate::data_dir().join("outgoing");

        let mut this = Self {
            focus_handle: cx.focus_handle(),
            list_focus: cx.focus_handle(),
            store,
            sample,
            accounts,
            providers,
            idle_watchers: HashMap::new(),
            idle_events: idle_tx,
            provider_issues,
            removing_accounts: HashSet::new(),
            signin: None,
            account,
            utc_offset: overlook_core::time::local_utc_offset_minutes(),
            outgoing_dir,
            view: MailboxView::Kind(LabelKind::Inbox),
            filter: ListFilter::All,
            search_input,
            search_query: String::new(),
            search_coverage: String::new(),
            search_generation: 0,
            online_search: OnlineSearch::Idle,
            labels: Vec::new(),
            conversations: Vec::new(),
            counts: HashMap::new(),
            mail_index: MailIndex::default(),
            mail_index_account: None,
            other_indexes: HashMap::new(),
            mail_index_generation: 0,
            local_writes: VecDeque::new(),
            local_in_flight: Vec::new(),
            prerender: None,
            prefetching: false,
            first_frame_logged: false,
            palette: None,
            session_started: now(),
            unwanted_drafts: HashSet::new(),
            list_state: ListState::new(0, ListAlignment::Top, px(600.)),
            shown_rows: Default::default(),
            drafts: Vec::new(),
            outbox: Vec::new(),
            status: AccountStatus::default(),
            selected: None,
            outbox_selected: None,
            thread: Vec::new(),
            expanded: BTreeSet::new(),
            remote_allowed: BTreeSet::new(),
            quick_look: None,
            contact_card: None,
            contacts: Vec::new(),
            contact_selected: None,
            contact_search,
            contact_query: String::new(),
            contact_filter: contacts::ContactFilter::All,
            contact_form: None,
            pdf_thumbs: HashMap::new(),
            pdf_thumbs_pending: HashSet::new(),
            inline_images: std::cell::RefCell::new(HashMap::new()),
            browser_bodies: std::cell::RefCell::new(HashMap::new()),
            composer: None,
            appearance,
            density,
            reading_pane,
            collapsed_sections,
            focused_enabled,
            inbox_tab: FocusTab::Focused,
            downloading: HashSet::new(),
            backing_up: false,
            attachments_dir,
            module: startup_module,
            cal_view: CalView::Week,
            cal_anchor: overlook_core::tz::day_start(now()),
            cal_events: Vec::new(),
            calendars: Vec::new(),
            cal_selected: None,
            cal_status: SyncState::Offline,
            caldav_connection: None,
            caldav_cached: false,
            caldav_last_sync: None,
            caldav_changes: Vec::new(),
            caldav_previews: HashMap::new(),
            show_caldav_changes: false,
            caldav_syncing: HashMap::new(),
            caldav_next_retry: HashMap::new(),
            caldav_requested: HashSet::new(),
            reader_scroll: ScrollHandle::new(),
            cal_scroll: ScrollHandle::new(),
            cal_scrolled_for: std::cell::Cell::new(None),
            cal_focus: cx.focus_handle(),
            new_event: None,
            cal_scope_prompt: None,
            cal_drag: None,
            reminders_enabled,
            notified: HashSet::new(),
            snoozed_reminders: HashMap::new(),
            update: UpdateState {
                install: InstallKind::detect(),
                latest: None,
                phase: UpdatePhase::Idle,
                progress: None,
                last_check: update_last_check,
                last_result: None,
                dismissed: update_dismissed,
            },
            prefs,
            account_prefs,
            settings: None,
            account_setup: None,
            mark_read_generation: 0,
            last_sync_tick: now(),
            sidebar_width,
            list_width,
            list_height,
            panes: cx.new(|_| ResizableState::default()),
            content_panes_h: cx.new(|_| ResizableState::default()),
            content_panes_v: cx.new(|_| ResizableState::default()),
            syncing: HashSet::new(),
            sync_requested: HashSet::new(),
            next_retry: HashMap::new(),
            retry_attempts: HashMap::new(),
            _subscriptions: subs,
        };
        // The watchdog reports any stall of this thread (see `watchdog.rs`).
        crate::watchdog::start();
        cx.spawn(async move |_, cx| loop {
            cx.background_executor().timer(Duration::from_millis(200)).await;
            crate::watchdog::beat();
        })
        .detach();
        // Keep a Chromium started so the first message opens without its launch.
        crate::browser_engine::prewarm();
        // A fresh process has not reached any provider yet: say so until a probe succeeds.
        for a in this.accounts.clone() {
            if this.provider_issues.contains_key(&a.id) {
                let _ = this.store.set_sync_state(&a.id, &SyncState::SignInRequired, None);
            } else if !this.connected(&a.id) {
                let _ = this.store.set_sync_state(&a.id, &SyncState::Offline, None);
            }
        }
        this.reload(cx);
        if this.selected.is_none() {
            if let Some(first) = this.conversations.first().map(|c| c.id.clone()) {
                this.selected = Some(first);
                this.load_thread();
            }
        }
        this.focus_current_module(window, cx);
        // Synchronize on startup for every reachable account; queued work from a previous
        // session is replayed before anything else happens.
        this.sync_all(window, cx);
        // IMAP IDLE: one thread per account asks for a sync the moment the Inbox changes.
        this.watch_idle_events(idle_rx, window, cx);
        this.refresh_idle_watchers();
        // `OVERLOOK_SIGN_IN=gmail` opens the Google sign-in immediately (first-run helper).
        match std::env::var("OVERLOOK_SIGN_IN").as_deref() {
            Ok("gmail") => this.start_google_sign_in(None, window, cx),
            Ok("imap") => this.open_account_setup(ProviderKind::Imap, SetupMode::New, window, cx),
            Ok("pop3") => this.open_account_setup(ProviderKind::Pop3, SetupMode::New, window, cx),
            _ => {}
        }

        // First update check shortly after launch, then once a day from the periodic loop.
        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(20)).await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.prefs.check_for_updates && now() - this.update.last_check > 6 * 3_600_000 {
                    this.check_for_updates(false, window, cx);
                }
            });
        })
        .detach();

        // mailto: links handed over by later launches (or this one) open the composer.
        cx.spawn_in(window, async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_millis(700)).await;
            let alive = this.update_in(cx, |this, window, cx| {
                for link in crate::single_instance::take() {
                    if let Some(m) = overlook_core::mailto::parse(&link) {
                        this.compose_mailto(m, window, cx);
                        window.activate_window();
                    }
                }
            });
            if alive.is_err() {
                break;
            }
        })
        .detach();

        // Periodic synchronization while accounts are reachable, and reminder checks.
        cx.spawn_in(window, async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_secs(15)).await;
            let alive = this.update_in(cx, |this, window, cx| {
                // Reminders are checked every tick; mail/calendar sync follows the preference.
                this.check_reminders(window, cx);
                if this.prefs.check_for_updates && now() - this.update.last_check > 24 * 3_600_000 {
                    this.check_for_updates(false, window, cx);
                }
                let interval = u64::from(this.prefs.sync_interval_secs.max(15));
                let due = now() - this.last_sync_tick >= (interval as i64) * 1000;
                if due {
                    this.last_sync_tick = now();
                    this.sync_all(window, cx);
                }
            });
            if alive.is_err() {
                break;
            }
        })
        .detach();
        this
    }

    pub(crate) fn owner(&self) -> Address {
        Address::named(&self.account.display_name, &self.account.email)
    }

    pub(crate) fn is_sample(&self, account: &AccountId) -> bool {
        self.accounts.iter().any(|a| &a.id == account && a.provider == ProviderKind::Sample)
    }

    /// Whether a sync attempt is worth starting for the account right now.
    pub(crate) fn connected(&self, account: &AccountId) -> bool {
        if self.is_sample(account) {
            self.sample.is_online()
        } else {
            self.providers.contains_key(account)
        }
    }

    pub(crate) fn active_issue(&self) -> Option<&String> {
        self.provider_issues.get(&self.account.id)
    }

    /// What the "fix the credentials" action is called for the active account.
    pub(crate) fn sign_in_again_label(&self) -> &'static str {
        sign_in_label_for(self.account.provider)
    }

    /// Reopens the credentials flow for the active account: the Google sign-in, or the
    /// server dialog with the password field focused for IMAP/POP3 accounts.
    pub(crate) fn sign_in_again(&mut self, hint: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.account.id.clone();
        self.sign_in_again_for(&id, hint, window, cx);
    }

    pub(crate) fn sign_in_again_for(&mut self, account: &AccountId, hint: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        match self.accounts.iter().find(|a| &a.id == account).map(|a| a.provider) {
            Some(ProviderKind::Imap) | Some(ProviderKind::Pop3) => {
                let kind = self.accounts.iter().find(|a| &a.id == account).map(|a| a.provider).unwrap_or(ProviderKind::Imap);
                self.open_account_setup(kind, SetupMode::UpdatePassword(account.clone()), window, cx);
            }
            _ => self.start_google_sign_in(hint, window, cx),
        }
    }

    /// The provider's name in status lines ("Searching Gmail…").
    pub(crate) fn provider_name(&self, account: &AccountId) -> String {
        match self.accounts.iter().find(|a| &a.id == account).map(|a| a.provider) {
            Some(ProviderKind::Sample) => "the sample server".into(),
            Some(ProviderKind::Gmail) => "Gmail".into(),
            Some(ProviderKind::Imap) => match self.providers.get(account).and_then(|p| p.imap.as_ref()) {
                Some(imap) => imap.servers().incoming.host.clone(),
                None => "the IMAP server".into(),
            },
            _ => "the mail server".into(),
        }
    }

    pub(crate) fn is_syncing_active(&self) -> bool {
        self.syncing.contains(&self.account.id)
    }

    // ---------------------------------------------------------------- accounts

    pub(crate) fn switch_account(&mut self, id: AccountId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(account) = self.accounts.iter().find(|a| a.id == id).cloned() else { return };
        if self.composer.is_some() {
            self.close_composer(window, cx);
        }
        self.account = account;
        self.save_setting("active_account", &id);
        self.view = MailboxView::Kind(LabelKind::Inbox);
        self.filter = ListFilter::All;
        self.selected = None;
        self.outbox_selected = None;
        self.thread.clear();
        self.expanded.clear();
        if !self.search_query.is_empty() {
            self.search_query.clear();
            let input = self.search_input.clone();
            input.update(cx, |s, cx| s.set_value("", window, cx));
        }
        self.reload(cx);
        self.selected = self.conversations.first().map(|c| c.id.clone());
        self.load_thread();
        self.focus_current_module(window, cx);
        if self.module == Module::Calendar { self.ensure_caldav_range(cx); }
        cx.notify();
    }

    pub(crate) fn remove_account(&mut self, id: AccountId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(account) = self.accounts.iter().find(|a| a.id == id).cloned() else { return };
        if !self.removing_accounts.insert(id.clone()) { return; }
        if let Err(error) = self.store.cancel_caldav_setup(&id) {
            self.removing_accounts.remove(&id);
            crate::ui::toast(window, cx, crate::notify::Level::Error, format!("Couldn’t stop calendar setup: {error}"));
            return;
        }
        let gmail = self.providers.get(&id).and_then(|p| p.gmail.clone());
        let background_id = id.clone();
        crate::ui::toast(window, cx, crate::notify::Level::Info, "Removing account and disconnecting access…");
        cx.spawn_in(window, async move |this, cx| {
            let result = cx.background_spawn(async move { end_session(&account, &background_id, gmail) }).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.removing_accounts.remove(&id);
                match result {
                    Ok(()) => this.finish_remove_account(id, window, cx),
                    Err(e) => crate::ui::toast(window, cx, crate::notify::Level::Error, format!("Account removal is incomplete: {e}. Reconnect and try again.")),
                }
                cx.notify();
            });
        }).detach();
    }

    /// Signs out of an account without removing it: the Google grant is revoked, every
    /// stored credential is deleted, and downloaded mail stays on this device. The account
    /// then waits in "Sign in again". `expired` is set for the automatic sign-out when a
    /// session ends on its own (see [`overlook_provider::gmail::api::SessionExpiry`]).
    pub(crate) fn sign_out_account(&mut self, id: AccountId, expiry: Option<SessionExpiry>, window: &mut Window, cx: &mut Context<Self>) {
        let expired = expiry.is_some();
        let Some(account) = self.accounts.iter().find(|a| a.id == id).cloned() else { return };
        if account.provider == ProviderKind::Sample || !self.removing_accounts.insert(id.clone()) { return; }
        let gmail = self.providers.get(&id).and_then(|p| p.gmail.clone());
        let background_id = id.clone();
        if !expired {
            crate::ui::toast(window, cx, crate::notify::Level::Info, "Signing out…");
        }
        let reason = match expiry {
            Some(SessionExpiry::Idle) => "session idle limit (90 days) reached",
            Some(SessionExpiry::MaxAge) => "session age limit (180 days) reached",
            None => "user signed out",
        };
        tracing::info!(target: "overlook::signin", account = %id.0, reason, "sign-out started");
        cx.spawn_in(window, async move |this, cx| {
            let result = cx.background_spawn(async move { end_session(&account, &background_id, gmail) }).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.removing_accounts.remove(&id);
                match result {
                    Ok(()) => {
                        tracing::info!(target: "overlook::signin", account = %id.0, "sign-out complete: grant revoked, stored credentials deleted");
                        this.providers.remove(&id);
                        this.syncing.remove(&id);
                        this.next_retry.remove(&id);
                        this.refresh_idle_watchers();
                        let wording = match expiry {
                            Some(SessionExpiry::Idle) => SESSION_IDLE_WORDING,
                            Some(SessionExpiry::MaxAge) => SESSION_AGE_WORDING,
                            None => SIGNED_OUT_WORDING,
                        };
                        this.provider_issues.insert(id.clone(), wording.into());
                        let _ = this.store.set_sync_state(&id, &SyncState::SignInRequired, None);
                        if let Some(account) = this.accounts.iter().find(|a| a.id == id) {
                            let email = account.email.clone();
                            let text = match expiry {
                                Some(SessionExpiry::Idle) => format!("Signed out of {email} after 90 days without use. Sign in again to continue."),
                                Some(SessionExpiry::MaxAge) => format!("Your sign-in for {email} is 180 days old. Sign in again to continue."),
                                None => format!("Signed out of {email}. Downloaded mail stays on this device."),
                            };
                            crate::ui::toast(window, cx, crate::notify::Level::Info, text);
                        }
                        this.reload(cx);
                    }
                    // An automatic sign-out retries on the next sync pass, quietly.
                    Err(e) if expired => tracing::warn!("idle sign-out incomplete: {e}"),
                    Err(e) => crate::ui::toast(window, cx, crate::notify::Level::Error, format!("Sign-out is incomplete: {e}. Reconnect and try again.")),
                }
                cx.notify();
            });
        }).detach();
    }

    /// Signs out connected Gmail sessions that are idle past the limit or past their maximum
    /// age.
    fn expire_idle_sessions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let now_ms = now();
        let expired: Vec<(AccountId, SessionExpiry)> = self.providers.iter().filter_map(|(id, p)| Some((id.clone(), p.gmail.as_ref()?.session_expiry(now_ms)?))).collect();
        for (id, expiry) in expired {
            self.sign_out_account(id, Some(expiry), window, cx);
        }
    }

    fn finish_remove_account(&mut self, id: AccountId, window: &mut Window, cx: &mut Context<Self>) {
        self.other_indexes.remove(&id);
        if self.composer.is_some() {
            self.close_composer(window, cx);
        }
        let Some(account) = self.accounts.iter().find(|a| a.id == id).cloned() else { return };
        if let Err(e) = self.store.delete_account(&id) {
            crate::ui::toast(window, cx, crate::notify::Level::Error, format!("Couldn’t remove the account: {e}"));
            return;
        }
        self.providers.remove(&id);
        self.caldav_syncing.remove(&id);
        self.caldav_next_retry.remove(&id);
        self.caldav_requested.remove(&id);
        self.provider_issues.remove(&id);
        self.syncing.remove(&id);
        self.refresh_idle_watchers();
        self.accounts = self.store.accounts().unwrap_or_default();
        if self.accounts.is_empty() {
            if let Ok(sample) = crate::seed_sample_account(&self.store) {
                self.providers.insert(sample.id.clone(), providers::AccountProviders::new(Arc::new(self.sample.clone()), Some(Arc::new(self.sample.clone()))));
                self.accounts.push(sample);
            }
        }
        let next = self.accounts[0].id.clone();
        crate::ui::toast(window, cx, crate::notify::Level::Info, format!("Removed {} from this device.", account.email));
        self.switch_account(next, window, cx);
    }

    // ---------------------------------------------------------------- updates

    pub(crate) fn check_for_updates(&mut self, manual: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.update.phase == UpdatePhase::Checking || self.update.phase == UpdatePhase::Downloading {
            return;
        }
        self.update.phase = UpdatePhase::Checking;
        self.update.last_check = now();
        self.save_setting("update.last_check", &self.update.last_check);
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx.background_spawn(async move { updater::check_latest() }).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.update.phase = UpdatePhase::Idle;
                match result {
                    Ok(release) if updater::is_newer(&release.version, updater::CURRENT_VERSION) => {
                        this.update.last_result = Some(format!("Overlook {} is available.", release.version));
                        this.update.latest = Some(release);
                        if manual {
                            this.update.dismissed = None;
                        }
                    }
                    Ok(release) => {
                        this.update.latest = None;
                        this.update.last_result = Some(format!("You have the latest version ({}).", release.version));
                        if manual {
                            crate::ui::toast(window, cx, crate::notify::Level::Success, format!("You’re up to date. Overlook {} is the latest version.", updater::CURRENT_VERSION));
                        }
                    }
                    Err(e) => {
                        this.update.last_result = Some(format!("Couldn’t check for updates: {e}"));
                        if manual {
                            crate::ui::toast(window, cx, crate::notify::Level::Warning, format!("Couldn’t check for updates: {e}"));
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn dismiss_update(&mut self, cx: &mut Context<Self>) {
        if let Some(r) = &self.update.latest {
            self.update.dismissed = Some(r.version.clone());
            self.save_setting("update.dismissed", &r.version);
        }
        cx.notify();
    }

    pub(crate) fn update_banner_visible(&self) -> bool {
        match &self.update.latest {
            Some(r) => self.update.dismissed.as_deref() != Some(r.version.as_str()) || self.update.phase != UpdatePhase::Idle,
            None => false,
        }
    }

    /// Downloads, verifies, and applies the latest release for this installation.
    pub(crate) fn start_update(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(release) = self.update.latest.clone() else { return };
        let install = self.update.install.clone();
        let asset = match &install {
            InstallKind::Pacman => release.package().cloned(),
            InstallKind::Prefix { .. } => release.tarball().cloned(),
            InstallKind::Development | InstallKind::Unknown(_) => None,
        };
        let Some(asset) = asset else {
            self.update.phase = UpdatePhase::Failed(match &install {
                InstallKind::Development => "This is a development build: pull the repository and rebuild.".into(),
                InstallKind::Unknown(p) => format!("Overlook at {} cannot update itself. Run the installer again.", p.display()),
                _ => "This release has no package for your installation. Run the installer again.".into(),
            });
            cx.notify();
            return;
        };
        let progress = Progress::new();
        self.update.progress = Some(progress.clone());
        self.update.phase = UpdatePhase::Downloading;
        cx.notify();
        let dir = updater::cache_dir();
        cx.spawn_in(window, async move |this, cx| {
            // Repaint while the download runs.
            let ticker = {
                let this = this.clone();
                let cx = cx.clone();
                cx.spawn(async move |cx| loop {
                    cx.background_executor().timer(Duration::from_millis(250)).await;
                    if this.update(cx, |this, cx| {
                        if this.update.phase == UpdatePhase::Downloading {
                            cx.notify();
                            true
                        } else {
                            false
                        }
                    }).unwrap_or(false) == false
                    {
                        break;
                    }
                })
            };
            let result = cx
                .background_spawn(async move {
                    let file = updater::download(&asset, &dir, &progress)?;
                    let expected = updater::expected_sha256(&release, &asset.name)?;
                    let actual = updater::sha256_file(&file)?;
                    if expected != actual {
                        let _ = std::fs::remove_file(&file);
                        return Err("The download did not match its checksum. Nothing was installed.".to_owned());
                    }
                    match &install {
                        InstallKind::Pacman => updater::apply_pacman_update(&file).map(|o| match o {
                            PacmanOutcome::Installed => UpdatePhase::ReadyToRestart,
                            PacmanOutcome::TerminalOpened => UpdatePhase::TerminalOpened,
                        }),
                        InstallKind::Prefix { bin, prefix } => updater::apply_prefix_update(&file, bin, prefix).map(|_| UpdatePhase::ReadyToRestart),
                        _ => Err("unsupported installation".to_owned()),
                    }
                })
                .await;
            drop(ticker);
            let _ = this.update_in(cx, |this, window, cx| {
                this.update.progress = None;
                this.update.phase = match result {
                    Ok(phase) => phase,
                    Err(e) => UpdatePhase::Failed(e),
                };
                if this.update.phase == UpdatePhase::ReadyToRestart {
                    crate::ui::toast(window, cx, crate::notify::Level::Success, "Update installed. Restart Overlook to finish.");
                }
                cx.notify();
            });
        })
        .detach();
    }

    // ------------------------------------------------------------ Google sign-in

    pub(crate) fn start_google_sign_in(&mut self, hint: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        if self.signin.is_some() {
            crate::ui::toast(window, cx, crate::notify::Level::Info, "Finish the sign-in that is already open in your browser, or cancel it first.");
            return;
        }
        let config = match google_signin::load_client_config() {
            Ok(c) => c,
            Err(e) => {
                crate::ui::toast(window, cx, crate::notify::Level::Error, e);
                return;
            }
        };
        let pending = match PendingSignIn::start(config, hint.clone()) {
            Ok(p) => p,
            Err(e) => {
                crate::ui::toast(window, cx, crate::notify::Level::Error, e);
                return;
            }
        };
        let url = pending.url.clone();
        self.signin = Some(SignIn { cancel: pending.cancel_handle(), expected: hint });
        cx.open_url(&url);
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx.background_spawn(async move { pending.wait() }).await;
            let _ = this.update_in(cx, |this, window, cx| this.finish_google_sign_in(result, window, cx));
        })
        .detach();
    }

    pub(crate) fn cancel_sign_in(&mut self, cx: &mut Context<Self>) {
        if let Some(s) = self.signin.take() {
            s.cancel.store(true, Ordering::SeqCst);
        }
        cx.notify();
    }

    fn finish_google_sign_in(&mut self, result: Result<SignInResult, String>, window: &mut Window, cx: &mut Context<Self>) {
        let was_cancelled = self.signin.is_none();
        let expected = self.signin.take().and_then(|s| s.expected);
        let result = match result {
            Ok(r) => r,
            Err(e) => {
                if !was_cancelled {
                    crate::ui::toast(window, cx, crate::notify::Level::Warning, e);
                }
                cx.notify();
                return;
            }
        };
        let email = result.user.email.to_lowercase();
        let mut added = false;
        let account = match self.store.account_by_email(ProviderKind::Gmail, &email) {
            Ok(Some(existing)) => existing,
            _ => {
                added = true;
                let account = Account {
                    id: AccountId(format!("gmail:{email}")),
                    provider: ProviderKind::Gmail,
                    display_name: result.user.name.clone().filter(|n| !n.trim().is_empty()).unwrap_or_else(|| {
                        let local = email.split('@').next().unwrap_or(&email);
                        let mut chars = local.chars();
                        chars.next().map(|c| c.to_uppercase().collect::<String>() + chars.as_str()).unwrap_or_else(|| local.to_owned())
                    }),
                    email: email.clone(),
                    created_at: now(),
                };
                if let Err(e) = self.store.upsert_account(&account) {
                    crate::ui::toast(window, cx, crate::notify::Level::Error, format!("Couldn’t save the account: {e}"));
                    return;
                }
                account
            }
        };
        if let Err(e) = credentials::save_tokens(&account.id, &result.tokens) {
            crate::ui::toast(window, cx, crate::notify::Level::Error, format!("{e} Overlook cannot keep you signed in without it."));
            return;
        }
        match providers::gmail_providers(&account, result.tokens) {
            Ok(p) => {
                self.providers.insert(account.id.clone(), p);
                self.provider_issues.remove(&account.id);
            }
            Err(e) => {
                crate::ui::toast(window, cx, crate::notify::Level::Error, e);
                return;
            }
        }
        let _ = self.store.set_sync_state(&account.id, &SyncState::Syncing, None);
        self.accounts = self.store.accounts().unwrap_or_default();
        if let Some(warning) = sign_in_mismatch(expected.as_deref(), &email, added) {
            tracing::info!(target: "overlook::signin", account = %account.id.0, "signed in to a different Google account than the one being signed in again");
            crate::ui::toast(window, cx, crate::notify::Level::Warning, warning);
        } else {
            crate::ui::toast(window, cx, crate::notify::Level::Success, format!("Signed in as {}. Downloading recent mail…", account.email));
        }
        self.switch_account(account.id.clone(), window, cx);
        self.sync_account(account.id, window, cx);
    }

    fn view_title(&self) -> String {
        match &self.view {
            MailboxView::Kind(kind) => match kind {
                LabelKind::Inbox => "Inbox",
                LabelKind::Starred => "Starred",
                LabelKind::Drafts => "Drafts",
                LabelKind::Sent => "Sent",
                LabelKind::Archive => "Archive",
                LabelKind::Trash => "Trash",
                LabelKind::Spam => "Junk",
                LabelKind::Outbox => "Outbox",
                LabelKind::User => "Label",
            }
            .to_owned(),
            MailboxView::Label(id) => self.labels.iter().find(|l| &l.id == id).map(|l| l.name.clone()).unwrap_or_default(),
        }
    }

    // ------------------------------------------------------------------ loading

    /// Re-reads everything the window shows from the local store. The conversation list
    /// and counts come from the in-memory index at once; a fresh index is read on a
    /// background thread and shown when it arrives.
    pub(crate) fn reload(&mut self, cx: &mut Context<Self>) {
        crate::watchdog::mark("reload");
        let acct = self.account.id.clone();
        if self.mail_index_account.as_ref() != Some(&acct) {
            // Another account (or the first load): show its last index if there is one and
            // refresh it in the background; read it here only the first time.
            let previous = std::mem::take(&mut self.mail_index);
            if let Some(old) = self.mail_index_account.take() {
                self.other_indexes.insert(old, previous);
            }
            match self.other_indexes.remove(&acct) {
                Some(index) => {
                    self.mail_index = index;
                    self.mail_index_account = Some(acct.clone());
                    self.rebuild_mail_index(cx);
                }
                None => {
                    self.mail_index = self.store.mail_index(&acct).unwrap_or_default();
                    self.mail_index_account = Some(acct.clone());
                    self.mail_index_generation += 1;
                }
            }
        } else {
            self.rebuild_mail_index(cx);
        }
        self.refresh_mail();
        self.reload_drafts();
        self.status = self.store.account_status(&acct).unwrap_or_default();
        self.load_thread();
        self.reload_calendar();
        self.reload_contacts();
        cx.notify();
    }

    /// Rebuilds the conversation list, counts and labels from the in-memory index.
    fn refresh_mail(&mut self) {
        let acct = self.account.id.clone();
        let owner = self.account.email.clone();
        self.labels = self.mail_index.labels().to_vec();
        let focus = self.inbox_focus();
        self.conversations = if self.search_query.trim().is_empty() {
            self.mail_index.conversations(&owner, &self.view, self.filter, focus)
        } else {
            self.search_results()
        };
        // Search coverage counts cached bodies, a full scan: only worth it while searching.
        self.search_coverage = if self.search_query.trim().is_empty() { String::new() } else { self.search_coverage_line(&acct) };
        let mut counts = HashMap::new();
        for view in [MailboxView::Kind(LabelKind::Inbox), MailboxView::Kind(LabelKind::Starred)]
            .into_iter()
            .chain(self.labels.iter().filter(|l| l.kind == LabelKind::User).map(|l| MailboxView::Label(l.id.clone())))
        {
            let focus = if view == MailboxView::Kind(LabelKind::Inbox) && self.focused_enabled { Some(FocusTab::Focused) } else { None };
            let n = self.mail_index.view_count(&owner, &view, focus);
            counts.insert(view, n);
        }
        self.counts = counts;
        if let Some(sel) = &self.selected {
            if !self.conversations.iter().any(|c| &c.id == sel) && self.search_query.is_empty() {
                // The selected conversation left this view (archived, trashed, filtered).
                self.selected = None;
                self.thread.clear();
            }
        }
    }

    /// Saves a setting on the background writer: the UI never waits for the disk.
    pub(crate) fn save_setting<T: serde::Serialize>(&self, key: &str, value: &T) {
        let value = match serde_json::to_value(value) {
            Ok(v) => v,
            Err(e) => return tracing::warn!("could not encode setting {key}: {e}"),
        };
        let (store, key) = (self.store.clone(), key.to_owned());
        crate::persist::later(move || {
            if let Err(e) = store.set_setting(&key, &value) {
                tracing::warn!("could not save setting {key}: {e}");
            }
        });
    }

    /// Takes a freshly read index and announces mail that arrived since the last one.
    fn replace_mail_index(&mut self, index: MailIndex) {
        let previous = std::mem::replace(&mut self.mail_index, index);
        // The first index of a session has nothing to compare against.
        if previous.labels().is_empty() {
            return;
        }
        let owner = self.account.email.clone();
        let fresh = new_inbox_mail(&previous, &self.mail_index, &owner, self.prefs.new_mail_notifications, self.focused_enabled, self.session_started);
        match fresh.as_slice() {
            [] => {}
            [one] => crate::notify::desktop(crate::notify::Level::Mail, one.from.display_name(), one.subject.clone()),
            many => {
                let senders: Vec<String> = many.iter().map(|m| m.from.display_name()).take(3).collect();
                crate::notify::desktop(crate::notify::Level::Mail, format!("{} new messages", many.len()), format!("From {}", senders.join(", ")));
            }
        }
    }

    /// Reads a fresh index on a background thread and shows it, unless a local change
    /// arrived meanwhile (the write that follows it rebuilds the index again).
    fn rebuild_mail_index(&mut self, cx: &mut Context<Self>) {
        self.mail_index_generation += 1;
        let generation = self.mail_index_generation;
        let store = self.store.clone();
        let acct = self.account.id.clone();
        cx.spawn(async move |this, cx| {
            let index = cx.background_spawn({
                let acct = acct.clone();
                async move { store.mail_index(&acct) }
            })
            .await;
            let _ = this.update(cx, |this, cx| {
                let current = this.mail_index_generation == generation && this.account.id == acct && this.local_writes.is_empty() && this.local_in_flight.is_empty();
                if let (true, Ok(index)) = (current, index) {
                    this.replace_mail_index(index);
                    this.refresh_mail();
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn reload_drafts(&mut self) {
        let acct = self.account.id.clone();
        self.drafts = self.store.drafts(&acct, false).unwrap_or_default();
        self.outbox = self.store.drafts(&acct, true).unwrap_or_default();
        if let Some(sel) = &self.outbox_selected {
            if !self.outbox.iter().any(|d| &d.id == sel) {
                self.outbox_selected = None;
            }
        }
    }

    fn load_thread(&mut self) {
        self.thread = match &self.selected {
            Some(id) => self.store.messages_in_conversation(&self.account.id, id).unwrap_or_default(),
            None => Vec::new(),
        };
        let unwritten = self.unwritten_changes();
        self.apply_to_thread(&unwritten);
    }

    /// Downloads the inline images of the open conversation that are not cached yet.
    fn fetch_inline_images(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        const INLINE_LIMIT: u64 = 4 * 1024 * 1024;
        // Inline images the body references, and attached images for their thumbnails.
        let wanted: Vec<Attachment> = self
            .thread
            .iter()
            .flat_map(|m| {
                let has_cid = m.body.html.as_deref().is_some_and(|h| h.contains("cid:"));
                m.attachments.iter().filter(move |a| (has_cid && a.is_inline_image()) || a.is_image() || a.is_pdf()).cloned()
            })
            .filter(|a| a.size_bytes <= if a.is_pdf() { 2 * INLINE_LIMIT } else { INLINE_LIMIT } && a.provider_id.is_some())
            .filter(|a| !a.local_path.as_deref().map(PathBuf::from).is_some_and(|p| p.exists()))
            .collect();
        for att in wanted {
            self.download_attachment(att, AfterDownload::Inline, window, cx);
        }
        self.prepare_pdf_thumbnails(window, cx);
    }

    fn search_results(&self) -> Vec<ConversationSummary> {
        let hits = self.store.search(&self.account.id, &self.search_query).unwrap_or_default();
        let hit_set: BTreeSet<MessageId> = hits.into_iter().collect();
        self.mail_index.conversations_matching(&hit_set, &self.account.email, self.filter)
    }

    /// The Focused/Other split applies only to the Inbox when the feature is on.
    pub(crate) fn inbox_focus(&self) -> Option<FocusTab> {
        (self.focused_enabled && self.view == MailboxView::Kind(LabelKind::Inbox)).then_some(self.inbox_tab)
    }

    pub(crate) fn set_inbox_tab(&mut self, tab: FocusTab, window: &mut Window, cx: &mut Context<Self>) {
        self.inbox_tab = tab;
        self.reload(cx);
        if !self.conversations.iter().any(|c| Some(&c.id) == self.selected.as_ref()) {
            self.selected = self.conversations.first().map(|c| c.id.clone());
            self.expanded.clear();
            self.load_thread();
        }
        self.list_focus.focus(window, cx);
        cx.notify();
    }

    pub(crate) fn toggle_focused_inbox(&mut self, cx: &mut Context<Self>) {
        self.focused_enabled = !self.focused_enabled;
        self.save_setting("focused_inbox", &self.focused_enabled);
        self.inbox_tab = FocusTab::Focused;
        self.reload(cx);
    }

    /// Moves the selected conversation to Focused or Other. With `always`, future mail from
    /// the sender follows too.
    pub(crate) fn move_focus(&mut self, focused: bool, always: bool, window: &mut Window, cx: &mut Context<Self>) {
        let ids: Vec<MessageId> = self.thread.iter().map(|m| m.id.clone()).collect();
        if ids.is_empty() {
            return;
        }
        let sender = self.thread.iter().filter(|m| !m.is_outgoing).max_by_key(|m| m.date).map(|m| m.from.clone());
        self.mail_index.set_focus_override(&ids, Some(focused));
        self.local_writes.push_back((self.account.id.clone(), LocalWrite::FocusOverride { ids, focused: Some(focused) }));
        if always {
            if let Some(sender) = &sender {
                self.mail_index.set_focus_rule(&sender.email, Some(focused));
                self.local_writes.push_back((self.account.id.clone(), LocalWrite::FocusRule { email: sender.email.clone(), focused: Some(focused) }));
            }
        }
        self.mail_index_generation += 1;
        self.write_local_changes(window, cx);
        let target = if focused { "Focused" } else { "Other" };
        let text = match (&sender, always) {
            (Some(s), true) => format!("Moved to {target}. Messages from {} will go to {target}.", s.display_name()),
            _ => format!("Moved to {target}."),
        };
        let next = self.neighbour_after_removal();
        self.refresh_mail();
        self.select_after_removal(next, window, cx);
        crate::ui::toast(window, cx, crate::notify::Level::Info, text);
    }

    // -------------------------------------------------------------- attachments

    pub(crate) fn open_attachment(&mut self, attachment: Attachment, window: &mut Window, cx: &mut Context<Self>) {
        let downloaded = attachment.local_path.as_deref().map(PathBuf::from).filter(|p| p.exists());
        if attachment.is_image() || (attachment.is_pdf() && crate::pdf_preview::available()) {
            match downloaded {
                Some(_) => self.show_quick_look(attachment, window, cx),
                None => self.download_attachment(attachment, AfterDownload::QuickLook, window, cx),
            }
            return;
        }
        if attachment.is_pdf() && downloaded.is_some() {
            crate::ui::toast(window, cx, crate::notify::Level::Info, "Install poppler (pdftoppm) to preview PDFs inside Overlook.");
        }
        if let Some(path) = downloaded {
            open_received_attachment(&path, &attachment.filename, window, cx);
            return;
        }
        self.download_attachment(attachment, AfterDownload::Open, window, cx);
    }

    /// Opens the quick look for a downloaded image or PDF; PDF pages render in the background.
    fn show_quick_look(&mut self, attachment: Attachment, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = attachment.local_path.as_deref().map(PathBuf::from) else { return };
        if attachment.is_image() {
            self.quick_look = Some(QuickLook { attachment, pages: vec![path], rendering: false, error: None });
            cx.notify();
            return;
        }
        let id = attachment.id.clone();
        self.quick_look = Some(QuickLook { attachment, pages: Vec::new(), rendering: true, error: None });
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx.background_spawn(async move { crate::pdf_preview::render_pages(&path) }).await;
            let _ = this.update(cx, |this, cx| {
                if let Some(ql) = this.quick_look.as_mut().filter(|q| q.attachment.id == id) {
                    ql.rendering = false;
                    match result {
                        Ok(pages) if !pages.is_empty() => ql.pages = pages,
                        Ok(_) => ql.error = Some("This PDF has no pages that could be rendered.".into()),
                        Err(e) => ql.error = Some(format!("Couldn’t render the PDF: {e}")),
                    }
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Renders first-page thumbnails for downloaded PDFs in the open conversation.
    pub(crate) fn prepare_pdf_thumbnails(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !crate::pdf_preview::available() {
            return;
        }
        let wanted: Vec<(AttachmentId, PathBuf)> = self
            .thread
            .iter()
            .flat_map(|m| m.attachments.iter())
            .filter(|a| a.is_pdf() && !self.pdf_thumbs.contains_key(&a.id) && !self.pdf_thumbs_pending.contains(&a.id))
            .filter_map(|a| a.local_path.as_deref().map(PathBuf::from).filter(|p| p.exists()).map(|p| (a.id.clone(), p)))
            .collect();
        for (id, path) in wanted {
            self.pdf_thumbs_pending.insert(id.clone());
            cx.spawn_in(window, async move |this, cx| {
                let result = cx.background_spawn(async move { crate::pdf_preview::render_thumbnail(&path) }).await;
                let _ = this.update(cx, |this, cx| {
                    this.pdf_thumbs_pending.remove(&id);
                    if let Ok(thumb) = result {
                        this.pdf_thumbs.insert(id, thumb);
                        cx.notify();
                    }
                });
            })
            .detach();
        }
    }

    /// The original message bytes, from the provider when it has them.
    fn with_raw_message(&mut self, message: Message, window: &mut Window, cx: &mut Context<Self>, then: impl FnOnce(&mut Self, Vec<u8>, &mut Window, &mut Context<Self>) + 'static) {
        let provider = self.providers.get(&self.account.id).map(|p| p.mail.clone());
        let account = self.account.clone();
        cx.spawn_in(window, async move |this, cx| {
            let fallback = overlook_provider::message_to_raw(&message);
            let bytes = match provider {
                Some(p) => cx.background_spawn(async move { p.fetch_raw(&account, &message).await }).await.unwrap_or(fallback),
                None => fallback,
            };
            let _ = this.update_in(cx, |this, window, cx| then(this, bytes, window, cx));
        })
        .detach();
    }

    fn safe_file_stem(subject: &str) -> String {
        let stem: String = subject.chars().map(|c| if c.is_alphanumeric() || c == ' ' || c == '-' || c == '_' { c } else { ' ' }).collect();
        let stem = stem.split_whitespace().collect::<Vec<_>>().join(" ");
        if stem.is_empty() { "message".to_owned() } else { stem.chars().take(80).collect() }
    }

    pub(crate) fn save_message_eml(&mut self, message: Message, window: &mut Window, cx: &mut Context<Self>) {
        let downloads = directories::UserDirs::new().and_then(|u| u.download_dir().map(|p| p.to_path_buf())).unwrap_or_else(|| PathBuf::from("."));
        let name = format!("{}.eml", Self::safe_file_stem(&message.subject));
        let receiver = cx.prompt_for_new_path(&downloads, Some(&name));
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(target))) = receiver.await else { return };
            let _ = this.update_in(cx, |this, window, cx| {
                this.with_raw_message(message, window, cx, move |_this, bytes, window, cx| match std::fs::write(&target, &bytes) {
                    Ok(()) => crate::ui::toast(window, cx, crate::notify::Level::Success, format!("Saved to {}", target.display())),
                    Err(e) => crate::ui::toast(window, cx, crate::notify::Level::Error, format!("Couldn’t save the message: {e}")),
                });
            });
        })
        .detach();
    }

    pub(crate) fn view_message_source(&mut self, message: Message, window: &mut Window, cx: &mut Context<Self>) {
        let dir = crate::updater::cache_dir().join("source");
        let path = dir.join(format!("{}.eml", message.id.as_str().replace([':', '/'], "_")));
        self.with_raw_message(message, window, cx, move |_this, bytes, window, cx| {
            if let Err(e) = std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&path, &bytes)) {
                crate::ui::toast(window, cx, crate::notify::Level::Error, format!("Couldn’t write the source: {e}"));
                return;
            }
            cx.open_with_system(&path);
        });
    }

    /// Writes a printable page for the message and opens it in the browser, which prints.
    pub(crate) fn print_message(&mut self, message: Message, window: &mut Window, cx: &mut Context<Self>) {
        use overlook_core::sanitize::{clean_only, escape_html, text_to_html, RemoteContent};
        let allow_remote = self.remote_allowed.contains(&message.id) || self.prefs.remote_images == RemoteImages::Allow;
        let policy = if allow_remote { RemoteContent::Allowed } else { RemoteContent::Blocked };
        let body = match (&message.body.html, &message.body.text) {
            // Printing includes all visible history, not just the unfolded reader body.
            (Some(h), _) => clean_only(h, policy),
            (None, Some(t)) => text_to_html(t),
            (None, None) => String::new(),
        };
        let body = self.resolve_inline_images(&message, body);
        let to = message.to.iter().map(Address::to_header).collect::<Vec<_>>().join(", ");
        let page = format!(
            "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>{subject}</title><style>body{{font:15px/1.5 -apple-system,Segoe UI,Inter,sans-serif;color:#111;max-width:760px;margin:40px auto;padding:0 24px}}.h{{border-bottom:1px solid #ccc;padding-bottom:12px;margin-bottom:20px;font-size:13px;color:#444}}.h b{{color:#111}}h1{{font-size:22px;margin:0 0 12px}}blockquote{{margin:0 0 0 .8ex;border-left:2px solid #ccc;padding-left:1ex;color:#555}}table{{border-collapse:collapse}}td,th{{border:1px solid #ddd;padding:4px 8px}}@media print{{body{{margin:0}}}}</style></head><body><h1>{subject}</h1><div class=\"h\"><div><b>From:</b> {from}</div><div><b>To:</b> {to}</div><div><b>Date:</b> {date}</div></div>{body}<script>window.print()</script></body></html>",
            subject = escape_html(&message.subject),
            from = escape_html(&message.from.to_header()),
            to = escape_html(&to),
            date = escape_html(&overlook_core::time::format_full(message.date, self.utc_offset)),
        );
        let dir = crate::updater::cache_dir().join("print");
        let path = dir.join(format!("{}.html", message.id.as_str().replace([':', '/'], "_")));
        match std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&path, page)) {
            Ok(()) => cx.open_with_system(&path),
            Err(e) => crate::ui::toast(window, cx, crate::notify::Level::Error, format!("Couldn’t prepare the page: {e}")),
        }
    }

    pub(crate) fn save_attachment(&mut self, attachment: Attachment, window: &mut Window, cx: &mut Context<Self>) {
        let downloads = self
            .prefs
            .attachments_dir
            .as_deref()
            .map(|d| PathBuf::from(shellexpand_home(d)))
            .filter(|d| d.is_dir())
            .or_else(|| directories::UserDirs::new().and_then(|u| u.download_dir().map(|p| p.to_path_buf())))
            .unwrap_or_else(|| PathBuf::from("."));
        let receiver = cx.prompt_for_new_path(&downloads, Some(&attachment.filename));
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(target))) = receiver.await else { return };
            let _ = this.update_in(cx, |this, window, cx| {
                match attachment.local_path.as_deref().map(PathBuf::from).filter(|p| p.exists()) {
                    Some(local) => this.finish_save(&local, &target, window, cx),
                    None => this.download_attachment(attachment.clone(), AfterDownload::SaveTo(target), window, cx),
                }
            });
        })
        .detach();
    }

    fn finish_save(&mut self, local: &std::path::Path, target: &std::path::Path, window: &mut Window, cx: &mut Context<Self>) {
        match std::fs::copy(local, target) {
            Ok(_) => crate::ui::toast(window, cx, crate::notify::Level::Success, format!("Saved to {}", target.display())),
            Err(e) => crate::ui::toast(window, cx, crate::notify::Level::Error, format!("Couldn’t save the file: {e}")),
        }
    }

    fn download_attachment(&mut self, attachment: Attachment, after: AfterDownload, window: &mut Window, cx: &mut Context<Self>) {
        if self.downloading.contains(&attachment.id) {
            return;
        }
        let Some(message) = self.store.message(&attachment.message_id).ok().flatten() else { return };
        let Some(provider) = self.providers.get(&self.account.id).map(|p| p.mail.clone()) else {
            crate::ui::toast(window, cx, crate::notify::Level::Warning, "Sign in again to download attachments.");
            return;
        };
        let account = self.account.clone();
        let (dir, dest) = self.attachment_path(&account.id, &attachment);
        self.downloading.insert(attachment.id.clone());
        cx.notify();
        let att = attachment.clone();
        let store = self.store.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    let bytes = provider.fetch_attachment(&account, &message, &att).await?;
                    std::fs::create_dir_all(&dir).map_err(|e| overlook_provider::ProviderError::Rejected(e.to_string()))?;
                    std::fs::write(&dest, &bytes).map_err(|e| overlook_provider::ProviderError::Rejected(e.to_string()))?;
                    // Recorded here, off the UI thread, before the reader reloads the thread.
                    let _ = store.set_attachment_local_path(&att.id, Some(&dest.to_string_lossy()));
                    Ok::<PathBuf, overlook_provider::ProviderError>(dest)
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.downloading.remove(&attachment.id);
                match result {
                    Ok(path) => {
                        this.load_thread();
                        match after {
                            AfterDownload::Open => open_received_attachment(&path, &attachment.filename, window, cx),
                            AfterDownload::SaveTo(target) => this.finish_save(&path, &target, window, cx),
                            AfterDownload::QuickLook => {
                                let mut a = attachment.clone();
                                a.local_path = Some(path.to_string_lossy().into_owned());
                                this.show_quick_look(a, window, cx);
                            }
                            AfterDownload::Inline => this.prepare_pdf_thumbnails(window, cx),
                        }
                    }
                    Err(e) => crate::ui::toast(window, cx, crate::notify::Level::Error, format!("Couldn’t download {}: {e}", attachment.filename)),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Where a downloaded attachment lives: one folder per account and message.
    fn attachment_path(&self, account: &AccountId, attachment: &Attachment) -> (PathBuf, PathBuf) {
        let safe_name: String = attachment.filename.chars().map(|c| if matches!(c, '/' | '\\' | '\0') { '_' } else { c }).collect();
        let dir = self.attachments_dir.join(account.as_str().replace([':', '/'], "_")).join(attachment.message_id.as_str().replace([':', '/'], "_"));
        let dest = dir.join(if safe_name.trim().is_empty() || safe_name.starts_with('.') { format!("attachment{}", safe_name) } else { safe_name });
        (dir, dest)
    }

    /// Downloads, one at a time in list order, the images and PDFs the reader would fetch
    /// when a message opens: Inbox conversations first, then unread mail elsewhere. They
    /// come from the user's own mail provider; nothing else is contacted.
    fn prefetch_attachments(&mut self, cx: &mut Context<Self>) {
        const INLINE_LIMIT: u64 = 4 * 1024 * 1024;
        const MESSAGES: usize = 80;
        if self.prefetching {
            return;
        }
        let Some(provider) = self.providers.get(&self.account.id).map(|p| p.mail.clone()) else { return };
        let owner = self.account.email.clone();
        let mut convs = self.mail_index.conversations(&owner, &MailboxView::Kind(LabelKind::Inbox), ListFilter::All, None);
        let unread = self.mail_index.conversations(&owner, &MailboxView::Kind(LabelKind::Archive), ListFilter::Unread, None);
        convs.extend(unread);
        let mut wanted: Vec<(Attachment, PathBuf, PathBuf)> = Vec::new();
        for c in convs.iter().take(MESSAGES) {
            for id in self.mail_index.message_ids_in(&c.id) {
                let Some(m) = self.mail_index.message(&id) else { continue };
                for a in &m.attachments {
                    let wanted_kind = a.is_inline_image() || a.is_image() || a.is_pdf();
                    let small = a.size_bytes <= if a.is_pdf() { 2 * INLINE_LIMIT } else { INLINE_LIMIT };
                    let missing = !a.local_path.as_deref().map(PathBuf::from).is_some_and(|p| p.exists());
                    if wanted_kind && small && missing && a.provider_id.is_some() && !self.downloading.contains(&a.id) {
                        let (dir, dest) = self.attachment_path(&self.account.id, a);
                        wanted.push((a.clone(), dir, dest));
                    }
                }
            }
        }
        if wanted.is_empty() {
            return;
        }
        self.prefetching = true;
        let store = self.store.clone();
        let account = self.account.clone();
        cx.spawn(async move |this, cx| {
            let fetched = cx
                .background_spawn(async move {
                    let mut fetched = 0;
                    for (att, dir, dest) in wanted {
                        let Some(message) = store.message(&att.message_id).ok().flatten() else { continue };
                        let Ok(bytes) = provider.fetch_attachment(&account, &message, &att).await else {
                            // Offline or rate limited: stop and try again after the next sync.
                            break;
                        };
                        if std::fs::create_dir_all(&dir).is_ok() && std::fs::write(&dest, &bytes).is_ok() {
                            let _ = store.set_attachment_local_path(&att.id, Some(&dest.to_string_lossy()));
                            fetched += 1;
                        }
                    }
                    fetched
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.prefetching = false;
                if fetched > 0 {
                    // The open conversation may have gained images; the index sees new paths.
                    this.load_thread();
                    this.rebuild_mail_index(cx);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn selected_summary(&self) -> Option<&ConversationSummary> {
        let id = self.selected.as_ref()?;
        self.conversations.iter().find(|c| &c.id == id)
    }

    // ------------------------------------------------------------ navigation

    fn select_view(&mut self, view: MailboxView, window: &mut Window, cx: &mut Context<Self>) {
        if !self.search_query.is_empty() {
            self.search_query.clear();
            let input = self.search_input.clone();
            input.update(cx, |s, cx| s.set_value("", window, cx));
        }
        if self.composer.as_ref().is_some_and(|c| c.draft.kind != DraftKind::New) {
            self.close_composer(window, cx);
        }
        self.view = view;
        self.outbox_selected = None;
        self.reload(cx);
        if self.selected.is_none() || !self.conversations.iter().any(|c| Some(&c.id) == self.selected.as_ref()) {
            self.selected = self.conversations.first().map(|c| c.id.clone());
            self.expanded.clear();
            self.load_thread();
        }
        self.list_focus.focus(window, cx);
        cx.notify();
    }

    fn set_filter(&mut self, filter: ListFilter, window: &mut Window, cx: &mut Context<Self>) {
        self.filter = filter;
        self.reload(cx);
        self.list_focus.focus(window, cx);
    }

    fn set_search(&mut self, query: String, window: &mut Window, cx: &mut Context<Self>) {
        self.search_query = query;
        self.search_generation += 1;
        self.online_search = OnlineSearch::Idle;
        self.reload(cx);
        if !self.search_query.is_empty() && self.selected_summary().is_none() {
            self.selected = self.conversations.first().map(|c| c.id.clone());
            self.load_thread();
        }
        self.start_online_search(window, cx);
        cx.notify();
    }

    /// What the search covered, for the line above the results.
    fn search_coverage_line(&self, acct: &AccountId) -> String {
        let local = self.store.search_coverage(acct).map(|c| c.sentence()).unwrap_or_default();
        let provider = self.provider_name(acct);
        let online = match &self.online_search {
            OnlineSearch::Idle => String::new(),
            OnlineSearch::Searching => format!(" · Searching {provider}…"),
            OnlineSearch::Done { added: 0 } => format!(" · {provider} has nothing more."),
            OnlineSearch::Done { added } => format!(" · {added} more from {provider}."),
            OnlineSearch::Offline => format!(" · Offline, so {provider} was not searched."),
            OnlineSearch::Failed(e) => format!(" · {provider} search failed: {e}"),
        };
        format!("{local}{online}")
    }

    /// Asks the provider for matches beyond what is on this device and downloads the ones
    /// that are missing, after a short pause so every keystroke does not become a request.
    fn start_online_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let query = self.search_query.trim().to_owned();
        if query.chars().count() < 2 {
            return;
        }
        let generation = self.search_generation;
        let acct = self.account.id.clone();
        if !self.connected(&acct) {
            self.online_search = OnlineSearch::Offline;
            self.search_coverage = self.search_coverage_line(&acct);
            return;
        }
        let Some(provider) = self.providers.get(&acct).map(|p| p.mail.clone()) else { return };
        let account = self.account.clone();
        let store = self.store.clone();
        let labels = self.labels.clone();
        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(450)).await;
            let still_current = this.update(cx, |this, cx| {
                if this.search_generation != generation {
                    return false;
                }
                this.online_search = OnlineSearch::Searching;
                this.search_coverage = this.search_coverage_line(&this.account.id.clone());
                cx.notify();
                true
            });
            if !matches!(still_current, Ok(true)) {
                return;
            }
            let result = cx
                .background_spawn(async move {
                    const FETCH_LIMIT: usize = 25;
                    let ids = provider.search(&account, &query, 60).await?;
                    let missing: Vec<ProviderId> = ids
                        .into_iter()
                        .filter(|pid| store.message_id_for_provider_id(&account.id, pid).ok().flatten().is_none())
                        .take(FETCH_LIMIT)
                        .collect();
                    if missing.is_empty() {
                        return Ok::<usize, overlook_provider::ProviderError>(0);
                    }
                    let mut messages = provider.fetch_messages(&account, &missing, &labels).await?;
                    // IMAP rows arrive without a conversation; thread them like a sync would.
                    crate::sync::reconcile_incoming(&store, &account.id, &mut messages, &mut Vec::new());
                    let protected = store.pending_message_ids(&account.id).unwrap_or_default();
                    store.merge_remote_messages(&messages, &protected).map_err(|e| overlook_provider::ProviderError::Temporary(e.to_string()))?;
                    Ok(messages.len())
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.search_generation != generation {
                    return;
                }
                this.online_search = match result {
                    Ok(added) => OnlineSearch::Done { added },
                    Err(overlook_provider::ProviderError::Offline) => OnlineSearch::Offline,
                    Err(e) => OnlineSearch::Failed(e.to_string()),
                };
                this.reload(cx);
            });
        })
        .detach();
    }

    fn select_conversation(&mut self, id: Option<ConversationId>, window: &mut Window, cx: &mut Context<Self>) {
        crate::watchdog::mark("open conversation");
        if self.composer.is_some() {
            self.close_composer(window, cx);
        }
        self.outbox_selected = None;
        self.expanded.clear();
        self.reader_scroll.set_offset(point(px(0.), px(0.)));
        self.selected = id;
        self.load_thread();
        self.fetch_inline_images(window, cx);
        // Reading marks the conversation read: a local change with its pending operation.
        let unread: Vec<MessageId> = self.thread.iter().filter(|m| !m.is_read).map(|m| m.id.clone()).collect();
        if !unread.is_empty() {
            match self.prefs.mark_as_read {
                MarkAsRead::WhenSelected => self.apply_local(overlook_store::LocalChange::SetRead { message_ids: unread, read: true }, window, cx),
                MarkAsRead::AfterDelay => {
                    self.mark_read_generation += 1;
                    let generation = self.mark_read_generation;
                    let selected = self.selected.clone();
                    let delay = u64::from(self.prefs.mark_as_read_delay_secs.max(1));
                    cx.spawn_in(window, async move |this, cx| {
                        cx.background_executor().timer(Duration::from_secs(delay)).await;
                        let _ = this.update_in(cx, |this, window, cx| {
                            if this.mark_read_generation == generation && this.selected == selected {
                                this.apply_local(overlook_store::LocalChange::SetRead { message_ids: unread.clone(), read: true }, window, cx);
                            }
                        });
                    })
                    .detach();
                }
                MarkAsRead::Never => {}
            }
        }
        self.list_focus.focus(window, cx);
        self.schedule_prerender(cx);
        cx.notify();
    }

    /// Renders the latest message of the conversations above and below the open one
    /// off-screen, so moving to them (j/k, or archive/delete landing on the next) shows
    /// them at once. Messages whose remote content would load are skipped: nothing is
    /// fetched for mail the user has not opened.
    fn schedule_prerender(&mut self, cx: &mut Context<Self>) {
        self.prerender = None;
        let Some(size) = crate::browser_body::reader_size() else { return };
        let Some(i) = self.selected.as_ref().and_then(|id| self.conversations.iter().position(|c| &c.id == id)) else { return };
        let ids: Vec<MessageId> = [Some(i + 1), i.checked_sub(1)]
            .into_iter()
            .flatten()
            .filter_map(|j| self.conversations.get(j))
            .map(|c| c.latest_message_id.clone())
            .collect();
        let store = self.store.clone();
        self.prerender = Some(cx.spawn(async move |this, cx| {
            // Let the open message paint and quick j/k runs settle before starting.
            cx.background_executor().timer(Duration::from_millis(250)).await;
            let messages = cx.background_spawn(async move { ids.into_iter().filter_map(|id| store.message(&id).ok().flatten()).collect::<Vec<_>>() }).await;
            for m in messages {
                let job = this
                    .update(cx, |this, cx| {
                        if m.body.html.is_none() {
                            return None; // Plain text is drawn natively.
                        }
                        if this.remote_allowed.contains(&m.id) || this.prefs.remote_images == RemoteImages::Allow {
                            return None;
                        }
                        let dark = cx.palette().is_dark();
                        let (html, _) = this.browser_document(&Self::message_source(&m), Some(&m), cx.palette());
                        if crate::browser_body::is_rendered(&html, false, dark, size) {
                            return None;
                        }
                        Some(crate::browser_body::prerender(html, false, dark, size, cx))
                    })
                    .ok()
                    .flatten();
                if let Some(job) = job {
                    job.await;
                }
            }
        }));
    }

    fn move_selection(&mut self, delta: i32, window: &mut Window, cx: &mut Context<Self>) {
        if self.conversations.is_empty() {
            return;
        }
        let current = self.selected.as_ref().and_then(|id| self.conversations.iter().position(|c| &c.id == id));
        let next = match current {
            Some(i) => (i as i32 + delta).clamp(0, self.conversations.len() as i32 - 1) as usize,
            None => 0,
        };
        let id = self.conversations[next].id.clone();
        if Some(&id) != self.selected.as_ref() {
            self.select_conversation(Some(id.clone()), window, cx);
        }
        // Keep the selected row in view in the virtual list.
        let ix = self.shown_rows.borrow().iter().position(|r| matches!(r, ListRow::Conversation(c) if c.id == id));
        if let Some(ix) = ix {
            self.list_state.scroll_to_reveal_item(ix);
        }
    }

    fn select_outbox_item(&mut self, id: DraftId, window: &mut Window, cx: &mut Context<Self>) {
        self.close_composer(window, cx);
        self.selected = None;
        self.thread.clear();
        self.outbox_selected = Some(id);
        cx.notify();
    }

    // --------------------------------------------------------- local changes

    /// Applies a mailbox change: the list, counts and open conversation update at once in
    /// memory, then the store write (with its pending operation) and the sync to the
    /// provider run in the background, in the order the changes were made.
    fn apply_local(&mut self, change: LocalChange, window: &mut Window, cx: &mut Context<Self>) {
        self.mail_index.apply(&change);
        self.mail_index_generation += 1;
        self.apply_to_thread(std::slice::from_ref(&change));
        self.refresh_mail();
        self.local_writes.push_back((self.account.id.clone(), LocalWrite::Change(change)));
        self.write_local_changes(window, cx);
        cx.notify();
    }

    /// Shows changes that are not in the store yet on the open conversation.
    fn apply_to_thread(&mut self, changes: &[LocalChange]) {
        let inbox = self.labels.iter().find(|l| l.kind == LabelKind::Inbox).map(|l| l.id.clone());
        let trash = self.labels.iter().find(|l| l.kind == LabelKind::Trash).map(|l| l.id.clone());
        for change in changes {
            let ids = change.message_ids();
            for m in self.thread.iter_mut().filter(|m| ids.contains(&m.id)) {
                overlook_store::apply_to_message(m, change, inbox.as_ref(), trash.as_ref());
            }
        }
    }

    /// Mailbox changes made in the window that the store does not have yet.
    fn unwritten_changes(&self) -> Vec<LocalChange> {
        self.local_in_flight
            .iter()
            .chain(self.local_writes.iter())
            .filter_map(|(_, w)| match w {
                LocalWrite::Change(c) => Some(c.clone()),
                _ => None,
            })
            .collect()
    }

    /// Writes queued local changes on a background thread, then reads a fresh index and
    /// syncs. Only one write runs at a time, so changes reach the store in order.
    fn write_local_changes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.local_in_flight.is_empty() || self.local_writes.is_empty() {
            return;
        }
        self.local_in_flight = self.local_writes.drain(..).collect();
        let writes = self.local_in_flight.clone();
        let store = self.store.clone();
        let accounts: BTreeSet<AccountId> = writes.iter().map(|(a, _)| a.clone()).collect();
        let acct = self.account.id.clone();
        cx.spawn_in(window, async move |this, cx| {
            let (errors, index) = cx
                .background_spawn({
                    let acct = acct.clone();
                    async move {
                        let errors: Vec<String> = writes
                            .into_iter()
                            .filter_map(|(acct, w)| {
                                match w {
                                    LocalWrite::Change(c) => store.apply_change(&acct, c).map(|_| ()),
                                    LocalWrite::FocusOverride { ids, focused } => store.set_focus_override(&acct, &ids, focused),
                                    LocalWrite::FocusRule { email, focused } => store.set_focus_rule(&acct, &email, focused),
                                }
                                .err()
                                .map(|e| e.to_string())
                            })
                            .collect();
                        (errors, store.mail_index(&acct))
                    }
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.local_in_flight.clear();
                for e in &errors {
                    crate::ui::toast(window, cx, crate::notify::Level::Error, format!("Couldn’t save that change: {e}"));
                }
                if !this.local_writes.is_empty() {
                    // More changes arrived while writing; this index is already behind them.
                    this.write_local_changes(window, cx);
                    return;
                }
                if this.account.id == acct {
                    if let Ok(index) = index {
                        this.replace_mail_index(index);
                        this.mail_index_generation += 1;
                        this.refresh_mail();
                    }
                }
                for acct in accounts {
                    this.next_retry.remove(&acct);
                    if this.connected(&acct) {
                        this.sync_account(acct, window, cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn selected_message_ids(&self) -> Vec<MessageId> {
        self.thread.iter().map(|m| m.id.clone()).collect()
    }

    /// Shows a toast with an Undo button that runs `undo` on this window.
    fn toast_with_undo(&self, text: impl Into<SharedString>, undo: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static, window: &mut Window, cx: &mut Context<Self>) {
        let this = cx.entity().downgrade();
        let undo = std::rc::Rc::new(undo);
        window.push_notification(
            Notification::info(text)
                .action(move |_note, _window, cx| {
                    let this = this.clone();
                    let undo = undo.clone();
                    let note = cx.entity();
                    Button::new("undo").xsmall().label("Undo").on_click(move |_, window, cx| {
                        let _ = this.update(cx, |t, cx| undo(t, window, cx));
                        note.update(cx, |n, cx| n.dismiss(window, cx));
                    })
                })
                .autohide(true),
            cx,
        );
    }

    /// Whether every message is in Trash / has the Inbox label, for toolbar wording.
    pub(crate) fn thread_state(&self) -> (bool, bool) {
        let trash = self.labels.iter().find(|l| l.kind == LabelKind::Trash).map(|l| l.id.clone());
        let inbox = self.labels.iter().find(|l| l.kind == LabelKind::Inbox).map(|l| l.id.clone());
        let in_trash = !self.thread.is_empty() && trash.as_ref().is_some_and(|t| self.thread.iter().all(|m| m.has_label(t)));
        let in_inbox = inbox.as_ref().is_some_and(|i| self.thread.iter().any(|m| m.has_label(i)));
        (in_trash, in_inbox)
    }

    pub(crate) fn archive(&mut self, ids: Vec<MessageId>, window: &mut Window, cx: &mut Context<Self>) {
        if ids.is_empty() {
            return;
        }
        let next = self.neighbour_after_removal();
        self.apply_local(overlook_store::LocalChange::Archive { message_ids: ids.clone() }, window, cx);
        self.select_after_removal(next, window, cx);
        self.toast_with_undo("Archived", move |this, window, cx| this.move_to_inbox(ids.clone(), window, cx), window, cx);
    }

    pub(crate) fn trash(&mut self, ids: Vec<MessageId>, window: &mut Window, cx: &mut Context<Self>) {
        if ids.is_empty() {
            return;
        }
        let next = self.neighbour_after_removal();
        self.apply_local(overlook_store::LocalChange::Trash { message_ids: ids.clone() }, window, cx);
        self.select_after_removal(next, window, cx);
        self.toast_with_undo("Moved to Trash", move |this, window, cx| this.restore(ids.clone(), window, cx), window, cx);
    }

    /// Puts archived (or trashed) messages back in the Inbox.
    pub(crate) fn move_to_inbox(&mut self, ids: Vec<MessageId>, window: &mut Window, cx: &mut Context<Self>) {
        if ids.is_empty() {
            return;
        }
        let trash = self.labels.iter().find(|l| l.kind == LabelKind::Trash).map(|l| l.id.clone());
        let trashed: Vec<MessageId> = ids.iter().filter(|id| self.mail_index.message(id).is_some_and(|m| trash.as_ref().is_some_and(|t| m.has_label(t)))).cloned().collect();
        let others: Vec<MessageId> = ids.iter().filter(|id| !trashed.contains(id)).cloned().collect();
        let next = self.neighbour_after_removal();
        if !trashed.is_empty() {
            self.apply_local(overlook_store::LocalChange::Untrash { message_ids: trashed }, window, cx);
        }
        if let (Some(inbox), false) = (self.labels.iter().find(|l| l.kind == LabelKind::Inbox).map(|l| l.id.clone()), others.is_empty()) {
            self.apply_local(overlook_store::LocalChange::AddLabel { message_ids: others, label_id: inbox }, window, cx);
        }
        self.select_after_removal(next, window, cx);
        self.toast_with_undo("Moved to Inbox", move |this, window, cx| this.archive(ids.clone(), window, cx), window, cx);
    }

    /// Restores trashed messages to the Inbox.
    pub(crate) fn restore(&mut self, ids: Vec<MessageId>, window: &mut Window, cx: &mut Context<Self>) {
        if ids.is_empty() {
            return;
        }
        let next = self.neighbour_after_removal();
        self.apply_local(overlook_store::LocalChange::Untrash { message_ids: ids.clone() }, window, cx);
        self.select_after_removal(next, window, cx);
        self.toast_with_undo("Restored to Inbox", move |this, window, cx| this.trash(ids.clone(), window, cx), window, cx);
    }

    /// Applies a user label and, like moving to a folder, takes the messages out of the Inbox.
    pub(crate) fn move_to_label(&mut self, ids: Vec<MessageId>, label_id: LabelId, window: &mut Window, cx: &mut Context<Self>) {
        if ids.is_empty() {
            return;
        }
        let name = self.labels.iter().find(|l| l.id == label_id).map(|l| l.name.clone()).unwrap_or_default();
        let inbox = self.labels.iter().find(|l| l.kind == LabelKind::Inbox).map(|l| l.id.clone());
        let was_in_inbox: Vec<MessageId> = ids.iter().filter(|id| self.mail_index.message(id).is_some_and(|m| inbox.as_ref().is_some_and(|i| m.has_label(i)))).cloned().collect();
        let next = self.neighbour_after_removal();
        self.apply_local(overlook_store::LocalChange::AddLabel { message_ids: ids.clone(), label_id: label_id.clone() }, window, cx);
        if !was_in_inbox.is_empty() {
            self.apply_local(overlook_store::LocalChange::Archive { message_ids: was_in_inbox.clone() }, window, cx);
        }
        self.select_after_removal(next, window, cx);
        let label_for_undo = label_id.clone();
        self.toast_with_undo(format!("Moved to {name}"), move |this, window, cx| {
            this.apply_local(overlook_store::LocalChange::RemoveLabel { message_ids: ids.clone(), label_id: label_for_undo.clone() }, window, cx);
            if !was_in_inbox.is_empty() {
                this.move_to_inbox(was_in_inbox.clone(), window, cx);
            }
        }, window, cx);
    }

    /// Handles a conversation dropped onto a sidebar destination.
    pub(crate) fn drop_on_view(&mut self, drag: ConversationDrag, view: MailboxView, window: &mut Window, cx: &mut Context<Self>) {
        let ids = drag.message_ids;
        match view {
            MailboxView::Kind(LabelKind::Inbox) => self.move_to_inbox(ids, window, cx),
            MailboxView::Kind(LabelKind::Archive) => self.archive(ids, window, cx),
            MailboxView::Kind(LabelKind::Trash) => self.trash(ids, window, cx),
            MailboxView::Kind(LabelKind::Starred) => self.set_starred(vec![ids.last().cloned().unwrap_or_else(|| ids[0].clone())], true, window, cx),
            MailboxView::Label(label_id) => self.move_to_label(ids, label_id, window, cx),
            _ => crate::ui::toast(window, cx, crate::notify::Level::Info, "Messages can be dropped on Inbox, Archive, Trash, Starred, or a label."),
        }
    }

    fn neighbour_after_removal(&self) -> Option<ConversationId> {
        let i = self.selected.as_ref().and_then(|id| self.conversations.iter().position(|c| &c.id == id))?;
        self.conversations
            .get(i + 1)
            .or_else(|| if i > 0 { self.conversations.get(i - 1) } else { None })
            .map(|c| c.id.clone())
    }

    fn select_after_removal(&mut self, next: Option<ConversationId>, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected.is_none() || !self.conversations.iter().any(|c| Some(&c.id) == self.selected.as_ref()) {
            let next = next.filter(|id| self.conversations.iter().any(|c| &c.id == id));
            self.select_conversation(next, window, cx);
        }
    }

    pub(crate) fn set_read(&mut self, ids: Vec<MessageId>, read: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !ids.is_empty() {
            self.apply_local(overlook_store::LocalChange::SetRead { message_ids: ids, read }, window, cx);
        }
    }

    pub(crate) fn set_starred(&mut self, ids: Vec<MessageId>, starred: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !ids.is_empty() {
            self.apply_local(overlook_store::LocalChange::SetStarred { message_ids: ids, starred }, window, cx);
        }
    }

    // --------------------------------------------------------------- compose

    /// Opens the composer from a `mailto:` link.
    pub(crate) fn compose_mailto(&mut self, m: overlook_core::mailto::Mailto, window: &mut Window, cx: &mut Context<Self>) {
        let mut draft = Draft::new(self.account.id.clone(), DraftKind::New, now());
        draft.to = m.to;
        draft.cc = m.cc;
        draft.bcc = m.bcc;
        draft.subject = m.subject;
        draft.body_text = format!("{}{}", m.body, self.account_prefs_for(&self.account.id).signature_block(false));
        self.set_module(Module::Mail, window, cx);
        self.outbox_selected = None;
        self.open_composer(draft, window, cx);
    }

    pub(crate) fn compose_new(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut draft = Draft::new(self.account.id.clone(), DraftKind::New, now());
        draft.body_text = self.account_prefs_for(&self.account.id).signature_block(false);
        self.outbox_selected = None;
        self.open_composer(draft, window, cx);
    }

    /// Reply or reply-all according to the "default reply" preference.
    pub(crate) fn reply_default(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let kind = if self.prefs.reply_all_by_default { DraftKind::ReplyAll } else { DraftKind::Reply };
        self.reply(kind, window, cx);
    }

    pub(crate) fn reply(&mut self, kind: DraftKind, window: &mut Window, cx: &mut Context<Self>) {
        let Some(source) = self.thread.iter().max_by_key(|m| m.date).cloned() else { return };
        self.reply_to(kind, source, window, cx);
    }

    /// Replies to or forwards one particular message of the conversation.
    pub(crate) fn reply_to(&mut self, kind: DraftKind, source: Message, window: &mut Window, cx: &mut Context<Self>) {
        let mut draft = Draft::in_response_to_with(self.account.id.clone(), kind, &source, &self.owner(), now(), self.prefs.quote_on_reply);
        // The quoted original is appended at send time and shown collapsed under the editor;
        // the body starts with two blank lines for the reply and the signature below them.
        draft.body_text = self.account_prefs_for(&self.account.id).signature_block(true);
        // Open at once; the new draft is written in the background. Until it commits, the
        // composer is not marked saved (nothing has been typed yet, so nothing can be lost).
        let store = self.store.clone();
        let to_save = draft.clone();
        self.open_composer(draft, window, cx);
        let draft_id = to_save.id.clone();
        let draft_account = to_save.account_id.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx.background_spawn(async move { store.save_editing_draft(&to_save) }).await;
            let _ = this.update_in(cx, |this, window, cx| {
                match result {
                    Ok(_) if this.unwanted_drafts.remove(&draft_id) => {
                        let _ = this.store.delete_draft(&draft_account, &draft_id);
                        this.reload_drafts();
                    }
                    Ok(_) => {
                        if let Some(c) = this.composer.as_mut().filter(|c| c.draft.id == draft_id && c.last_saved.is_none()) {
                            c.last_saved = Some(now());
                        }
                        this.reload_drafts();
                    }
                    Err(e) => crate::ui::toast(window, cx, crate::notify::Level::Error, format!("Couldn’t start the draft: {e}")),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn open_draft(&mut self, id: DraftId, window: &mut Window, cx: &mut Context<Self>) {
        if let Ok(Some(draft)) = self.store.draft(&id) {
            if let Some(conv) = draft.conversation_id.clone() {
                // Show the conversation being replied to, if it is in this view.
                if self.conversations.iter().any(|c| c.id == conv) {
                    self.selected = Some(conv);
                    self.load_thread();
                }
            }
            self.open_composer(draft, window, cx);
            if let Some(c) = self.composer.as_mut() {
                c.last_saved = Some(now());
            }
        }
    }

    pub(crate) fn cancel_send(&mut self, id: DraftId, window: &mut Window, cx: &mut Context<Self>) {
        match self.store.cancel_send(&self.account.id, &id) {
            Ok(draft) => {
                self.outbox_selected = None;
                self.view = MailboxView::Kind(LabelKind::Drafts);
                self.reload(cx);
                self.open_composer(draft, window, cx);
                if let Some(c) = self.composer.as_mut() {
                    c.last_saved = Some(now());
                }
            }
            Err(e) => crate::ui::toast(window, cx, crate::notify::Level::Warning, e.to_string()),
        }
    }

    /// "Send again" on an uncertain or failed Outbox item: a fresh attempt, now.
    pub(crate) fn resend_draft(&mut self, id: DraftId, window: &mut Window, cx: &mut Context<Self>) {
        match self.store.resend(&self.account.id, &id) {
            Ok(draft) => {
                self.reload(cx);
                self.sync_account(draft.account_id, window, cx);
            }
            Err(e) => crate::ui::toast(window, cx, crate::notify::Level::Warning, e.to_string()),
        }
    }

    /// "Discard" on an uncertain or failed Outbox item: the draft and its send are dropped.
    pub(crate) fn discard_send(&mut self, id: DraftId, window: &mut Window, cx: &mut Context<Self>) {
        match self.store.discard_send(&self.account.id, &id) {
            Ok(()) => {
                self.outbox_selected = None;
                self.reload(cx);
            }
            Err(e) => crate::ui::toast(window, cx, crate::notify::Level::Warning, e.to_string()),
        }
    }

    // ------------------------------------------------------------- settings

    fn set_appearance(&mut self, pref: AppearancePreference, window: &mut Window, cx: &mut Context<Self>) {
        self.appearance = pref;
        cx.set_global(AppearanceGlobal(pref));
        self.save_setting(SETTING_APPEARANCE, &pref);
        theme::apply_preference(pref, theme::system_appearance(window.appearance()), cx);
        cx.notify();
    }

    pub(crate) fn toggle_section(&mut self, key: &str, cx: &mut Context<Self>) {
        if !self.collapsed_sections.remove(key) {
            self.collapsed_sections.insert(key.to_owned());
        }
        let list: Vec<&String> = self.collapsed_sections.iter().collect();
        self.save_setting("sidebar.collapsed", &list);
        cx.notify();
    }

    fn toggle_density(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.density = match self.density {
            Density::Comfortable => Density::Compact,
            Density::Compact => Density::Comfortable,
        };
        self.save_setting("density", &self.density);
        cx.notify();
    }

    fn set_reading_pane(&mut self, placement: ReadingPane, cx: &mut Context<Self>) {
        self.reading_pane = placement;
        self.save_setting("reading_pane", &placement);
        cx.notify();
    }

    fn set_online(&mut self, online: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.sample.set_online(online);
        let sample_ids: Vec<AccountId> = self.accounts.iter().filter(|a| a.provider == ProviderKind::Sample).map(|a| a.id.clone()).collect();
        for id in sample_ids {
            if online {
                self.sync_account(id, window, cx);
            } else {
                let _ = self.store.set_sync_state(&id, &SyncState::Offline, None);
            }
        }
        self.reload(cx);
    }

    // ------------------------------------------------------------------ sync

    /// Syncs the active account (manual: ignores the retry pause).

    pub(crate) fn sync_now(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.account.id.clone();
        self.sync_caldav_account(id.clone(), true, cx);
        self.next_retry.remove(&id);
        self.sync_account(id, window, cx);
    }

    /// Syncs every reachable account that is not in a retry pause. One account never
    /// blocks another.
    pub(crate) fn sync_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.expire_idle_sessions(window, cx);
        for id in self.accounts.iter().map(|a| a.id.clone()).collect::<Vec<_>>() {
            self.sync_caldav_account(id, false, cx);
        }
        let now_ms = now();
        let ids: Vec<AccountId> = self
            .accounts
            .iter()
            .map(|a| a.id.clone())
            .filter(|id| self.connected(id) && self.next_retry.get(id).is_none_or(|t| *t <= now_ms))
            .collect();
        for id in ids {
            self.sync_account(id, window, cx);
        }
    }

    pub(crate) fn sync_account(&mut self, id: AccountId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(account) = self.accounts.iter().find(|a| a.id == id).cloned() else { return };
        let Some(bundle) = self.providers.get(&id).cloned() else {
            let _ = self.store.set_sync_state(&id, &SyncState::SignInRequired, None);
            self.reload(cx);
            return;
        };
        if self.syncing.contains(&id) {
            self.sync_requested.insert(id);
            return;
        }
        self.syncing.insert(id.clone());
        self.sync_requested.remove(&id);
        let store = self.store.clone();
        let first_download = store.message_count(&id).unwrap_or(0) == 0;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let outcome: SyncOutcome = cx.background_spawn(run_sync(store, bundle.mail, bundle.calendar, account.clone())).await;
            let _ = this.update_in(cx, |this, window, cx| {
                crate::watchdog::mark("sync finished");
                this.syncing.remove(&account.id);
                match &outcome.state {
                    Some(SyncState::UpToDate) => {
                        this.retry_attempts.remove(&account.id);
                        this.next_retry.remove(&account.id);
                    }
                    Some(SyncState::SyncFailed(_)) | Some(SyncState::Paused(_)) => {
                        let attempts = this.retry_attempts.entry(account.id.clone()).or_insert(0);
                        *attempts += 1;
                        let base = overlook_provider::backoff_seconds(*attempts).max(if outcome.rate_limited { 60 } else { 15 });
                        this.next_retry.insert(account.id.clone(), now() + (base as i64) * 1000);
                    }
                    _ => {}
                }
                if matches!(outcome.state, Some(SyncState::SignInRequired)) {
                    let wording = match account.provider {
                        ProviderKind::Imap | ProviderKind::Pop3 => providers::PASSWORD_NEEDED_WORDING,
                        _ => "Sign in again to continue syncing. Your local drafts are safe.",
                    };
                    this.provider_issues.insert(account.id.clone(), wording.into());
                    // Rejected credentials are never retried automatically: repeated failed
                    // logins would look like a brute-force attempt to the provider and can
                    // lock the account. Syncing resumes only after the user signs in again.
                    if account.provider != ProviderKind::Sample && this.providers.remove(&account.id).is_some() {
                        tracing::info!(target: "overlook::signin", account = %account.id.0, "credentials rejected; automatic sync stopped until the user signs in again");
                        this.refresh_idle_watchers();
                    }
                }
                this.reload(cx);
                if matches!(outcome.state, Some(SyncState::UpToDate)) && account.id == this.account.id {
                    this.prefetch_attachments(cx);
                }
                if first_download && outcome.downloaded > 0 {
                    crate::ui::toast(window, cx, crate::notify::Level::Success, format!("Downloaded {} recent messages for {}.", outcome.downloaded, account.email));
                }
                for subject in &outcome.sent {
                    crate::ui::toast(window, cx, crate::notify::Level::Success, format!("Sent: {subject}"));
                }
                for subject in &outcome.uncertain {
                    crate::ui::toast(
                        window,
                        cx,
                        crate::notify::Level::Warning,
                        format!("The connection dropped while sending “{subject}”. Overlook will check whether it arrived before trying again."),
                    );
                }
                for what in &outcome.failed {
                    crate::ui::toast(window, cx, crate::notify::Level::Error, format!("Couldn’t sync: {what}"));
                }
                let more_pending = this.store.pending_count(&account.id).unwrap_or(0) > 0;
                let paused = this.next_retry.get(&account.id).is_some_and(|t| *t > now());
                if !paused && (this.sync_requested.contains(&account.id) || (more_pending && !outcome.stopped_offline && outcome.applied > 0)) {
                    this.sync_account(account.id.clone(), window, cx);
                }
            });
        })
        .detach();
    }

    // ---------------------------------------------------------------- actions

    fn on_compose(&mut self, _: &Compose, window: &mut Window, cx: &mut Context<Self>) {
        // In Contacts, Ctrl+N starts a new contact rather than a message.
        if self.module == Module::Contacts {
            self.start_new_contact(None, window, cx);
            return;
        }
        self.compose_new(window, cx);
    }
    fn on_focus_search(&mut self, _: &FocusSearch, window: &mut Window, cx: &mut Context<Self>) {
        // Ctrl+K in the composer body inserts a link, as in Gmail and Outlook.
        if self.composer_body_focused(window, cx) {
            self.format_link(window, cx);
            return;
        }
        let input = self.search_input.clone();
        input.update(cx, |s, cx| {
            s.focus(window, cx);
            s.select_all(window, cx);
        });
    }
    fn on_command_palette(&mut self, _: &OpenCommandPalette, window: &mut Window, cx: &mut Context<Self>) {
        // Ctrl+K in the composer body inserts a link, as in Gmail and Outlook.
        if self.composer_body_focused(window, cx) {
            self.format_link(window, cx);
            return;
        }
        if self.palette.is_some() {
            self.close_palette(window, cx);
        } else {
            self.open_palette(window, cx);
        }
    }
    fn on_escape(&mut self, _: &Escape, window: &mut Window, cx: &mut Context<Self>) {
        if self.palette.is_some() {
            self.close_palette(window, cx);
        } else if self.account_setup.is_some() {
            self.close_account_setup(window, cx);
        } else if self.quick_look.is_some() {
            self.quick_look = None;
            cx.notify();
        } else if self.settings.is_some() {
            self.close_settings(window, cx);
        } else if self.composer.is_some() {
            self.close_composer(window, cx);
        } else if self.contact_form.is_some() {
            self.cancel_contact_form(cx);
        } else if self.new_event.is_some() {
            self.cancel_new_event(window, cx);
        } else if self.cal_scope_prompt.is_some() {
            self.cal_scope_prompt = None;
            cx.notify();
        } else if !self.search_query.is_empty() {
            let input = self.search_input.clone();
            input.update(cx, |s, cx| s.set_value("", window, cx));
            self.set_search(String::new(), window, cx);
            self.list_focus.focus(window, cx);
        } else {
            self.focus_current_module(window, cx);
        }
    }
    fn on_format_bold(&mut self, _: &FormatBold, window: &mut Window, cx: &mut Context<Self>) {
        if self.composer_body_focused(window, cx) { self.format_wrap("**", window, cx) } else { cx.propagate() }
    }
    fn on_format_italic(&mut self, _: &FormatItalic, window: &mut Window, cx: &mut Context<Self>) {
        if self.composer_body_focused(window, cx) { self.format_wrap("*", window, cx) } else { cx.propagate() }
    }
    fn on_format_strike(&mut self, _: &FormatStrike, window: &mut Window, cx: &mut Context<Self>) {
        if self.composer_body_focused(window, cx) { self.format_wrap("~~", window, cx) } else { cx.propagate() }
    }
    fn on_format_code(&mut self, _: &FormatCode, window: &mut Window, cx: &mut Context<Self>) {
        if self.composer_body_focused(window, cx) { self.format_wrap("`", window, cx) } else { cx.propagate() }
    }
    fn on_format_bullets(&mut self, _: &FormatBullets, window: &mut Window, cx: &mut Context<Self>) {
        if self.composer_body_focused(window, cx) { self.format_lines(composer::LinePrefix::Bullet, window, cx) } else { cx.propagate() }
    }
    fn on_format_numbers(&mut self, _: &FormatNumbers, window: &mut Window, cx: &mut Context<Self>) {
        if self.composer_body_focused(window, cx) { self.format_lines(composer::LinePrefix::Number, window, cx) } else { cx.propagate() }
    }
    fn on_format_quote(&mut self, _: &FormatQuote, window: &mut Window, cx: &mut Context<Self>) {
        if self.composer_body_focused(window, cx) { self.format_lines(composer::LinePrefix::Quote, window, cx) } else { cx.propagate() }
    }
    fn on_toggle_preview(&mut self, _: &TogglePreview, window: &mut Window, cx: &mut Context<Self>) {
        if self.composer.is_some() { self.toggle_preview(window, cx) } else { cx.propagate() }
    }
    fn on_send(&mut self, _: &SendMessage, window: &mut Window, cx: &mut Context<Self>) {
        if self.composer.is_some() {
            self.send_composer(window, cx);
        }
    }
    fn on_reply(&mut self, _: &Reply, window: &mut Window, cx: &mut Context<Self>) {
        self.reply_default(window, cx);
    }
    fn on_open_settings(&mut self, _: &OpenSettings, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings.is_some() {
            self.close_settings(window, cx);
        } else {
            self.open_settings(SettingsPage::General, window, cx);
        }
    }

    /// Rebuilds Gmail providers so a new download size applies, and re-lists the mailboxes.
    pub(crate) fn rebuild_providers_for_download_size(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        crate::set_inbox_limit(self.prefs.inbox_messages as usize);
        let accounts = self.accounts.clone();
        for a in accounts.iter().filter(|a| matches!(a.provider, ProviderKind::Gmail | ProviderKind::Imap)) {
            if let Ok(bundle) = providers::providers_for(a, &self.sample, &self.store) {
                self.providers.insert(a.id.clone(), bundle);
            }
            // Drop the coverage marker so the next sync lists the mailboxes again.
            let _ = self.store.set_setting(&format!("download_coverage:{}", a.id.0), &0u32);
        }
        self.refresh_idle_watchers();
        self.sync_all(window, cx);
    }

    /// Starts an `IDLE` thread for every IMAP account that has a provider and stops the
    /// ones whose account or provider went away. A provider rebuilt with new settings gets a
    /// fresh thread (the old one is dropped, which shuts its socket). Idempotent.
    pub(crate) fn refresh_idle_watchers(&mut self) {
        let wanted: HashMap<AccountId, Arc<overlook_provider::imap::ImapProvider>> = self.providers.iter().filter_map(|(id, p)| p.imap.clone().map(|imap| (id.clone(), imap))).collect();
        self.idle_watchers.retain(|id, _| wanted.contains_key(id));
        for (id, provider) in wanted {
            let tag = Arc::as_ptr(&provider) as usize;
            let stale = self.idle_watchers.get(&id).is_some_and(|w| w.tag() != tag);
            if stale {
                self.idle_watchers.remove(&id);
            }
            if self.idle_watchers.contains_key(&id) {
                continue;
            }
            let events = self.idle_events.clone();
            let account = id.clone();
            let watcher = provider.idle_watcher(move |event| {
                if event == overlook_provider::imap::idle::IdleEvent::Changed {
                    let _ = events.send(account.clone());
                }
            });
            self.idle_watchers.insert(id, watcher.with_tag(tag));
        }
    }

    /// Turns `IDLE` reports into syncs of the account that changed. Runs for the window's
    /// lifetime; the channel wakes the task, so nothing polls.
    fn watch_idle_events(&mut self, mut events: tokio::sync::mpsc::UnboundedReceiver<AccountId>, window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |this, cx| {
            while let Some(id) = events.recv().await {
                let alive = this.update_in(cx, |this, window, cx| {
                    if this.connected(&id) {
                        this.next_retry.remove(&id);
                        this.sync_account(id, window, cx);
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();
    }
    fn on_reply_all(&mut self, _: &ReplyAll, window: &mut Window, cx: &mut Context<Self>) {
        self.reply(DraftKind::ReplyAll, window, cx);
    }
    fn on_forward(&mut self, _: &Forward, window: &mut Window, cx: &mut Context<Self>) {
        self.reply(DraftKind::Forward, window, cx);
    }
    fn on_next(&mut self, _: &NextConversation, window: &mut Window, cx: &mut Context<Self>) {
        self.move_selection(1, window, cx);
    }
    fn on_prev(&mut self, _: &PrevConversation, window: &mut Window, cx: &mut Context<Self>) {
        self.move_selection(-1, window, cx);
    }
    fn on_archive(&mut self, _: &ArchiveSelected, window: &mut Window, cx: &mut Context<Self>) {
        let ids = self.selected_message_ids();
        let (in_trash, in_inbox) = self.thread_state();
        if in_trash || !in_inbox {
            self.move_to_inbox(ids, window, cx);
        } else {
            self.archive(ids, window, cx);
        }
    }
    fn on_trash(&mut self, _: &TrashSelected, window: &mut Window, cx: &mut Context<Self>) {
        let ids = self.selected_message_ids();
        self.trash(ids, window, cx);
    }
    fn on_toggle_star(&mut self, _: &ToggleStar, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(summary) = self.selected_summary().cloned() {
            self.set_starred(vec![summary.latest_message_id], !summary.starred, window, cx);
        }
    }
    fn on_toggle_read(&mut self, _: &ToggleRead, window: &mut Window, cx: &mut Context<Self>) {
        let unread_any = self.thread.iter().any(|m| !m.is_read);
        let ids = self.selected_message_ids();
        self.set_read(ids, unread_any, window, cx);
    }
    fn on_open_selected(&mut self, _: &OpenSelected, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(latest) = self.thread.iter().max_by_key(|m| m.date) {
            let id = latest.id.clone();
            self.expanded.insert(id);
            cx.notify();
        } else if self.selected.is_none() {
            self.move_selection(1, window, cx);
        }
    }
    fn on_sync_now(&mut self, _: &SyncNow, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_now(window, cx);
    }
    fn on_switch_mail(&mut self, _: &SwitchToMail, window: &mut Window, cx: &mut Context<Self>) {
        self.set_module(Module::Mail, window, cx);
    }
    fn on_switch_contacts(&mut self, _: &SwitchToContacts, window: &mut Window, cx: &mut Context<Self>) {
        self.set_module(Module::Contacts, window, cx);
    }
    fn on_switch_calendar(&mut self, _: &SwitchToCalendar, window: &mut Window, cx: &mut Context<Self>) {
        self.set_module(Module::Calendar, window, cx);
    }
    /// The single-letter calendar shortcuts (T, D, W, M, A, arrows) must not fire while the
    /// user is typing in the event editor. Propagating lets the keystroke reach the text field.
    fn typing_in_event_editor(&self, window: &Window, cx: &App) -> bool {
        let Some(editor) = &self.new_event else { return false };
        editor.title.read(cx).focus_handle(cx).is_focused(window)
            || editor.date.read(cx).focus_handle(cx).is_focused(window)
            || editor.end_date.read(cx).focus_handle(cx).is_focused(window)
            || editor.start.read(cx).focus_handle(cx).is_focused(window)
            || editor.end.read(cx).focus_handle(cx).is_focused(window)
            || editor.location.read(cx).focus_handle(cx).is_focused(window)
            || editor.guests.read(cx).focus_handle(cx).is_focused(window)
            || editor.description.read(cx).focus_handle(cx).is_focused(window)
    }
    fn on_cal_prev(&mut self, _: &CalendarPrevious, window: &mut Window, cx: &mut Context<Self>) {
        if self.typing_in_event_editor(window, cx) {
            return cx.propagate();
        }
        self.cal_step(-1, cx);
    }
    fn on_cal_next(&mut self, _: &CalendarNext, window: &mut Window, cx: &mut Context<Self>) {
        if self.typing_in_event_editor(window, cx) {
            return cx.propagate();
        }
        self.cal_step(1, cx);
    }
    fn on_cal_today(&mut self, _: &CalendarToday, window: &mut Window, cx: &mut Context<Self>) {
        if self.typing_in_event_editor(window, cx) {
            return cx.propagate();
        }
        self.cal_today(cx);
    }
    fn on_cal_day(&mut self, _: &CalendarDay, window: &mut Window, cx: &mut Context<Self>) {
        if self.typing_in_event_editor(window, cx) {
            return cx.propagate();
        }
        self.set_cal_view(CalView::Day, cx);
    }
    fn on_cal_week(&mut self, _: &CalendarWeek, window: &mut Window, cx: &mut Context<Self>) {
        if self.typing_in_event_editor(window, cx) {
            return cx.propagate();
        }
        self.set_cal_view(CalView::Week, cx);
    }
    fn on_cal_agenda(&mut self, _: &CalendarAgenda, window: &mut Window, cx: &mut Context<Self>) {
        if self.typing_in_event_editor(window, cx) {
            return cx.propagate();
        }
        self.set_cal_view(CalView::Agenda, cx);
    }
    fn on_cal_delete_selected(&mut self, _: &CalendarDeleteSelected, window: &mut Window, cx: &mut Context<Self>) {
        if !self.cal_focus.is_focused(window) || self.typing_in_event_editor(window, cx) || self.module != Module::Calendar {
            return cx.propagate();
        }
        if let Some(id) = self.cal_selected.clone() {
            self.request_delete_event(id, window, cx);
        }
    }
    fn on_cal_edit_selected(&mut self, _: &CalendarEditSelected, window: &mut Window, cx: &mut Context<Self>) {
        if !self.cal_focus.is_focused(window) || self.typing_in_event_editor(window, cx) || self.module != Module::Calendar {
            return cx.propagate();
        }
        if let Some(id) = self.cal_selected.clone() {
            self.request_edit_event(id, window, cx);
        }
    }

    fn select_calendar_event(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.new_event.is_some() || !self.cal_focus.is_focused(window) {
            return cx.propagate();
        }
        let len = self.cal_events.len();
        if len == 0 { return; }
        let current = self.cal_selected.as_ref().and_then(|id| self.cal_events.iter().position(|e| &e.id == id));
        let next = match (current, forward) {
            (Some(i), true) => (i + 1).min(len - 1),
            (Some(i), false) => i.saturating_sub(1),
            (None, true) => 0,
            (None, false) => len - 1,
        };
        self.cal_selected = Some(self.cal_events[next].id.clone());
        cx.notify();
    }
    fn on_cal_select_next(&mut self, _: &CalendarSelectNext, window: &mut Window, cx: &mut Context<Self>) {
        self.select_calendar_event(true, window, cx);
    }
    fn on_cal_select_previous(&mut self, _: &CalendarSelectPrevious, window: &mut Window, cx: &mut Context<Self>) {
        self.select_calendar_event(false, window, cx);
    }
    fn on_cal_month(&mut self, _: &CalendarMonth, window: &mut Window, cx: &mut Context<Self>) {
        if self.typing_in_event_editor(window, cx) {
            return cx.propagate();
        }
        self.set_cal_view(CalView::Month, cx);
    }
    fn on_new_event(&mut self, _: &NewEventAction, window: &mut Window, cx: &mut Context<Self>) {
        self.set_module(Module::Calendar, window, cx);
        self.start_new_event(window, cx);
    }

    fn on_save_calendar_event(&mut self, _: &SaveCalendarEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.module == Module::Calendar && self.new_event.is_some() && self.settings.is_none() {
            self.save_new_event(window, cx);
        } else {
            cx.propagate();
        }
    }

    // ---------------------------------------------------------------- header

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = *cx.palette();
        let owner = self.owner();
        let appearance = self.appearance;
        let reading_pane = self.reading_pane;
        let density = self.density;
        let focused_enabled = self.focused_enabled;
        let reminders_enabled = self.reminders_enabled;
        let this = cx.entity().downgrade();
        let this2 = cx.entity().downgrade();

        let appearance_icon = if p.is_dark() { "moon" } else { "sun" };

        TitleBar::new()
            .h(px(64.))
            .bg(p.surface)
            .border_b_1()
            .border_color(p.divider)
            .child(
                h_flex()
                    .size_full()
                    .items_center()
                    .gap(px(20.))
                    .pl(px(10.))
                    .pr(px(8.))
                    .child(
                        h_flex()
                            .items_center()
                            .gap(px(10.))
                            .w(px(214.))
                            .flex_none()
                            .child(brand_tile(30.))
                            .child(
                                div()
                                    .font_family("Source Serif 4 Display")
                                    .text_size(px(22.))
                                    .line_height(px(30.))
                                    .text_color(p.foreground)
                                    .child("Overlook"),
                            ),
                    )
                    .child(
                        h_flex()
                            .items_center()
                            .gap(px(2.))
                            .p(px(3.))
                            .rounded(px(8.))
                            .bg(p.sidebar)
                            .border_1()
                            .border_color(p.divider)
                            .child(
                                segment_with_icon("module-mail", icon("mail").size(px(15.)), "Mail", self.module == Module::Mail, &p)
                                    .tooltip(|window, cx| Tooltip::new("Mail (Ctrl 1)").build(window, cx))
                                    .on_click(cx.listener(|this, _, window, cx| this.set_module(Module::Mail, window, cx))),
                            )
                            .child(
                                segment_with_icon("module-calendar", icon("calendar").size(px(15.)), "Calendar", self.module == Module::Calendar, &p)
                                    .tooltip(|window, cx| Tooltip::new("Calendar (Ctrl 2)").build(window, cx))
                                    .on_click(cx.listener(|this, _, window, cx| this.set_module(Module::Calendar, window, cx))),
                            )
                            .child(
                                segment_with_icon("module-contacts", icon("users").size(px(15.)), "Contacts", self.module == Module::Contacts, &p)
                                    .tooltip(|window, cx| Tooltip::new("Contacts (Ctrl 3)").build(window, cx))
                                    .on_click(cx.listener(|this, _, window, cx| this.set_module(Module::Contacts, window, cx))),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .max_w(px(480.))
                            .h(px(37.))
                            .child(
                                Input::new(&self.search_input)
                                    .prefix(icon("search").size(px(16.)).text_color(p.muted))
                                    .suffix(kbd("Ctrl E", &p))
                                    .cleanable(true),
                            ),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("appearance-menu")
                            .ghost()
                            .icon(icon(appearance_icon).size(px(17.)))
                            .tooltip(format!("Appearance: {}", appearance.label()))
                            .dropdown_menu(move |menu, window, _cx| {
                                let system = theme::system_appearance(window.appearance());
                                let mut menu = menu.label("Appearance");
                                for pref in AppearancePreference::ALL {
                                    let this = this.clone();
                                    let label = if pref == AppearancePreference::FollowSystem {
                                        match system {
                                            SystemAppearance::Dark => "Follow system (currently Dark)".to_owned(),
                                            SystemAppearance::Light => "Follow system (currently Light)".to_owned(),
                                            SystemAppearance::Unknown => "Follow system".to_owned(),
                                        }
                                    } else {
                                        pref.label().to_owned()
                                    };
                                    menu = menu.item(PopupMenuItem::new(label).checked(pref == appearance).on_click(
                                        move |_, window, cx| {
                                            let _ = this.update(cx, |this, cx| this.set_appearance(pref, window, cx));
                                        },
                                    ));
                                }
                                menu
                            }),
                    )
                    .child(
                        Button::new("view-menu")
                            .ghost()
                            .icon(icon("panels-top-left").size(px(17.)))
                            .tooltip("Layout")
                            .dropdown_menu(move |menu, _window, _cx| {
                                let a = this2.clone();
                                let b = this2.clone();
                                let c = this2.clone();
                                let d = this2.clone();
                                let r = this2.clone();
                                menu.label("Reading pane")
                                    .item(PopupMenuItem::new("On the right").checked(reading_pane == ReadingPane::Right).on_click(move |_, _w, cx| {
                                        let _ = a.update(cx, |this, cx| this.set_reading_pane(ReadingPane::Right, cx));
                                    }))
                                    .item(PopupMenuItem::new("Below the list").checked(reading_pane == ReadingPane::Bottom).on_click(move |_, _w, cx| {
                                        let _ = b.update(cx, |this, cx| this.set_reading_pane(ReadingPane::Bottom, cx));
                                    }))
                                    .separator()
                                    .label("Density")
                                    .item(PopupMenuItem::new(if density == Density::Compact { "Comfortable" } else { "Compact" }).on_click(move |_, window, cx| {
                                        let _ = c.update(cx, |this, cx| this.toggle_density(window, cx));
                                    }))
                                    .separator()
                                    .label("Inbox")
                                    .item(PopupMenuItem::new("Focused Inbox").checked(focused_enabled).on_click(move |_, _w, cx| {
                                        let _ = d.update(cx, |this, cx| this.toggle_focused_inbox(cx));
                                    }))
                                    .separator()
                                    .label("Calendar")
                                    .item(PopupMenuItem::new("Event reminders").checked(reminders_enabled).on_click(move |_, _w, cx| {
                                        let _ = r.update(cx, |this, cx| {
                                            this.reminders_enabled = !this.reminders_enabled;
                                            this.save_setting("reminders", &this.reminders_enabled);
                                            cx.notify();
                                        });
                                    }))
                            }),
                    )
                    .child(
                        Button::new("open-settings")
                            .ghost()
                            .icon(icon("settings").size(px(17.)))
                            .tooltip("Settings (Ctrl ,)")
                            .on_click(cx.listener(|this, _, window, cx| this.open_settings(SettingsPage::General, window, cx))),
                    )
                    .child(owner_avatar(&owner, 29., &p).ml(px(6.)).mr(px(8.))),
            )
    }

    // ------------------------------------------------------------ status bar

    fn render_status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = *cx.palette();
        let status = &self.status;
        let is_sample = self.is_sample(&self.account.id);
        let online = self.sample.is_online();
        let (dot, state_text) = match &status.state {
            SyncState::UpToDate => (p.success, "Up to date".to_owned()),
            SyncState::Syncing => (p.accent, "Syncing your changes…".to_owned()),
            SyncState::Paused(reason) => (p.accent, reason.clone()),
            SyncState::Offline => (p.muted, status.state.sentence(status.pending_operations)),
            SyncState::SignInRequired => (p.warning, status.state.sentence(0)),
            SyncState::SyncFailed(reason) => (p.warning, format!("Sync failed: {reason}")),
        };
        let last = status.last_successful_sync.map(|ts| {
            let (_, t, _) = overlook_core::time::civil(ts, self.utc_offset);
            format!("Last synced {}", format_clock(t))
        });
        let unread: usize = self.conversations.iter().filter(|c| c.is_unread()).count();
        let center = if self.view == MailboxView::Kind(LabelKind::Drafts) {
            format!("{} draft{}", self.drafts.len(), if self.drafts.len() == 1 { "" } else { "s" })
        } else if self.view == MailboxView::Kind(LabelKind::Outbox) {
            format!("{} waiting to send", self.outbox.len())
        } else {
            format!(
                "{} conversation{} · {} unread",
                self.conversations.len(),
                if self.conversations.len() == 1 { "" } else { "s" },
                unread
            )
        };
        let this = cx.entity().downgrade();

        h_flex()
            .items_center()
            .h(px(32.))
            .px(px(14.))
            .gap(px(12.))
            .bg(p.background)
            .border_t_1()
            .border_color(p.divider)
            .text_size(px(12.))
            .text_color(p.muted)
            .child(
                h_flex()
                    .items_center()
                    .gap(px(7.))
                    .min_w(px(260.))
                    .child(div().size(px(7.)).rounded_full().bg(dot))
                    .child(state_text)
                    .when(status.pending_operations > 0 && status.state != SyncState::Offline, |d| {
                        d.child(chip(format!("{} pending", status.pending_operations), &p))
                    })
                    .children(last.map(|l| div().text_color(p.muted).child(format!("· {l}")))),
            )
            .child(div().flex_1())
            .child(center)
            .child(div().flex_1())
            .child(
                h_flex()
                    .items_center()
                    .gap(px(10.))
                    .child(format!("{} · {}", self.account.provider.label(), self.account.email))
                    .when(is_sample, |row| {
                        row.child(
                            Switch::new("connection")
                                .checked(online)
                                .label(if online { "Connected" } else { "Disconnected" })
                                .tooltip("Simulated connection for the sample account")
                                .on_click(cx.listener(|this, checked: &bool, window, cx| this.set_online(*checked, window, cx))),
                        )
                    })
                    .when(is_sample, |row| row.child(
                        Button::new("dev-menu")
                            .ghost()
                            .xsmall()
                            .icon(icon("ellipsis").size(px(15.)))
                            .tooltip("Sample account tools")
                            .dropdown_menu(move |menu, _window, _cx| {
                                let a = this.clone();
                                let b = this.clone();
                                menu.label("Sample account")
                                    .item(PopupMenuItem::new("Sync now").on_click(move |_, window, cx| {
                                        let _ = a.update(cx, |this, cx| this.sync_now(window, cx));
                                    }))
                                    .item(PopupMenuItem::new("Drop the connection during the next send").on_click(move |_, window, cx| {
                                        let _ = b.update(cx, |this, cx| {
                                            this.sample.drop_next_send();
                                            crate::ui::toast(window, cx, crate::notify::Level::Info, "The next send will lose its connection after the server accepts it.");
                                        });
                                    }))
                            }),
                    )),
            )
    }

    /// Banner for an available update, its download, and the restart step.
    fn render_update_banner(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        if !self.update_banner_visible() {
            return None;
        }
        let p = *cx.palette();
        let release = self.update.latest.clone()?;
        let version = release.version.clone();
        let url = release.html_url.clone();
        let (text, color): (String, Hsla) = match &self.update.phase {
            UpdatePhase::Idle | UpdatePhase::Checking => (format!("Overlook {version} is available."), p.accent),
            UpdatePhase::Downloading => {
                let pct = self.update.progress.as_ref().and_then(|p| p.fraction()).map(|f| format!(" {:.0}%", f * 100.)).unwrap_or_default();
                (format!("Downloading Overlook {version}…{pct}"), p.accent)
            }
            UpdatePhase::ReadyToRestart => (format!("Overlook {version} is installed. Restart to start using it."), p.success),
            UpdatePhase::TerminalOpened => ("Finish the update in the terminal window, then restart Overlook.".into(), p.accent),
            UpdatePhase::Failed(e) => (e.clone(), p.warning),
        };
        let phase = self.update.phase.clone();
        Some(
            h_flex()
                .items_center()
                .gap(px(10.))
                .px(px(16.))
                .h(px(40.))
                .bg(p.chip)
                .border_b_1()
                .border_color(p.divider)
                .text_size(px(13.))
                .child(icon("arrow-down-to-line").size(px(16.)).text_color(color))
                .child(div().flex_1().truncate().child(text))
                .child(Button::new("update-notes").ghost().small().label("What’s new").on_click(move |_, _, cx| cx.open_url(&url)))
                .when(matches!(phase, UpdatePhase::Idle | UpdatePhase::Failed(_)), |row| {
                    row.child(Button::new("update-now").primary().small().label(if matches!(phase, UpdatePhase::Failed(_)) { "Try again" } else { "Update now" }).on_click(cx.listener(|this, _, window, cx| this.start_update(window, cx))))
                        .child(Button::new("update-later").ghost().small().label("Later").on_click(cx.listener(|this, _, _w, cx| this.dismiss_update(cx))))
                })
                .when(matches!(phase, UpdatePhase::ReadyToRestart | UpdatePhase::TerminalOpened), |row| {
                    row.child(Button::new("update-restart").primary().small().label("Restart Overlook").on_click(|_, _, _| updater::restart()))
                }),
        )
    }

    /// Banner under the header for a sign-in in progress or an account that needs sign-in.
    fn render_banner(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let p = *cx.palette();
        let (icon_name, text, action, color): (&str, String, Option<(&str, bool)>, Hsla) = if self.signin.is_some() {
            ("globe", "Finish signing in to Google in your browser. Overlook is waiting for the sign-in to complete.".into(), Some(("Cancel", false)), p.accent)
        } else if let Some(reason) = self.active_issue() {
            ("key-round", reason.clone(), Some((self.sign_in_again_label(), true)), p.warning)
        } else {
            return None;
        };
        let email = self.account.email.clone();
        Some(
            h_flex()
                .items_center()
                .gap(px(10.))
                .px(px(16.))
                .h(px(40.))
                .bg(p.chip)
                .border_b_1()
                .border_color(p.divider)
                .text_size(px(13.))
                .child(icon(icon_name).size(px(16.)).text_color(color))
                .child(div().flex_1().truncate().child(text))
                .children(action.map(|(label, sign_in)| {
                    Button::new("banner-action").small().label(label).on_click(cx.listener(move |this, _, window, cx| {
                        if sign_in {
                            this.sign_in_again(Some(email.clone()), window, cx);
                        } else {
                            this.cancel_sign_in(cx);
                        }
                    }))
                })),
        )
    }

    // ------------------------------------------------------------------ body

    fn render_body(&self, cx: &mut Context<Self>) -> AnyElement {
        if matches!(self.module, Module::Contacts | Module::Calendar) {
            self.browser_bodies.borrow_mut().clear();
        }
        if self.module == Module::Contacts {
            return h_resizable("main-contacts")
                .with_state(&self.panes)
                .child(resizable_panel().size(px(self.sidebar_width)).size_range(px(180.)..px(360.)).child(self.render_contacts_sidebar(cx).into_any_element()))
                .child(resizable_panel().child(self.render_contacts_main(cx).into_any_element()))
                .into_any_element();
        }
        if self.module == Module::Calendar {
            return h_resizable("main-calendar")
                .with_state(&self.panes)
                .child(resizable_panel().size(px(self.sidebar_width)).size_range(px(180.)..px(360.)).child(self.render_calendar_sidebar(cx).into_any_element()))
                .child(resizable_panel().child(self.render_calendar_main(cx).into_any_element()))
                .into_any_element();
        }
        let sidebar = self.render_sidebar(cx).into_any_element();
        let list = self.render_list(cx).into_any_element();
        let reader = self.render_reader(cx).into_any_element();

        let content = match self.reading_pane {
            ReadingPane::Right => h_resizable("content-h")
                .with_state(&self.content_panes_h)
                .child(resizable_panel().size(px(self.list_width)).size_range(px(260.)..px(640.)).child(list))
                .child(resizable_panel().child(reader))
                .on_resize(cx.listener(|this, state: &Entity<ResizableState>, _window, cx| {
                    if let Some(w) = state.read(cx).sizes().first() {
                        this.list_width = f32::from(*w);
                        this.save_setting("panes.list", &this.list_width);
                    }
                })),
            ReadingPane::Bottom => v_resizable("content-v")
                .with_state(&self.content_panes_v)
                .child(resizable_panel().size(px(self.list_height)).size_range(px(160.)..px(900.)).child(list))
                .child(resizable_panel().child(reader))
                .on_resize(cx.listener(|this, state: &Entity<ResizableState>, _window, cx| {
                    if let Some(h) = state.read(cx).sizes().first() {
                        this.list_height = f32::from(*h);
                        this.save_setting("panes.list_height", &this.list_height);
                    }
                })),
        };

        h_resizable("main")
            .with_state(&self.panes)
            .child(resizable_panel().size(px(self.sidebar_width)).size_range(px(180.)..px(360.)).child(sidebar))
            .child(resizable_panel().child(content))
            .on_resize(cx.listener(|this, state: &Entity<ResizableState>, _window, cx| {
                if let Some(w) = state.read(cx).sizes().first() {
                    this.sidebar_width = f32::from(*w);
                    this.save_setting("panes.sidebar", &this.sidebar_width);
                }
            }))
            .into_any_element()
    }
}

/// Expands a leading `~` to the home directory.
fn shellexpand_home(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return format!("{}/{rest}", home.to_string_lossy());
        }
    }
    path.to_owned()
}

/// The app icon drawn from vectors: midnight rounded tile, ivory horizon mark, amber beacon.
/// Crisp at any scale, unlike a downscaled bitmap.
fn brand_tile(size: f32) -> impl IntoElement {
    let midnight: Hsla = Rgba::from(gpui_kit::rgb(0x101C24)).into();
    let ivory: Hsla = Rgba::from(gpui_kit::rgb(0xF0EBDD)).into();
    let amber: Hsla = Rgba::from(gpui_kit::rgb(0xFF9D32)).into();
    // Geometry from branding/assets/overlook-app-icon.svg (128-unit canvas): the mark is
    // inset 10 units and scaled .84375; the beacon sits at (79, 42) with radius 5.
    let inset = size * 10. / 128.;
    let mark = size * 108. / 128.;
    let dot = size * 10. * 0.84375 / 128.;
    let dot_cx = inset + mark * 79. / 128.;
    let dot_cy = inset + mark * 42. / 128.;
    div()
        .relative()
        .flex_none()
        .size(px(size))
        .rounded(px(size * 28. / 128.))
        .bg(midnight)
        .child(svg().path("overlook/symbolic.svg").absolute().left(px(inset)).top(px(inset)).size(px(mark)).text_color(ivory))
        .child(div().absolute().left(px(dot_cx - dot / 2.)).top(px(dot_cy - dot / 2.)).size(px(dot)).rounded_full().bg(amber))
}

impl Focusable for MailWindow {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for MailWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::watchdog::mark("render");
        if !self.first_frame_logged {
            self.first_frame_logged = true;
            tracing::info!("startup: first frame after {} ms", crate::since_start_ms());
        }
        let p: Palette = *cx.palette();
        v_flex()
            .id("mail-window")
            .key_context(WINDOW_CONTEXT)
            .track_focus(&self.focus_handle)
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
            .on_action(cx.listener(Self::on_reply))
            .on_action(cx.listener(Self::on_reply_all))
            .on_action(cx.listener(Self::on_forward))
            .on_action(cx.listener(Self::on_next))
            .on_action(cx.listener(Self::on_prev))
            .on_action(cx.listener(Self::on_archive))
            .on_action(cx.listener(Self::on_trash))
            .on_action(cx.listener(Self::on_toggle_star))
            .on_action(cx.listener(Self::on_toggle_read))
            .on_action(cx.listener(Self::on_open_selected))
            .on_action(cx.listener(Self::on_sync_now))
            .on_action(cx.listener(Self::on_switch_mail))
            .on_action(cx.listener(Self::on_switch_calendar))
            .on_action(cx.listener(Self::on_switch_contacts))
            .on_action(cx.listener(Self::on_cal_prev))
            .on_action(cx.listener(Self::on_cal_next))
            .on_action(cx.listener(Self::on_cal_today))
            .on_action(cx.listener(Self::on_cal_day))
            .on_action(cx.listener(Self::on_cal_week))
            .on_action(cx.listener(Self::on_cal_agenda))
            .on_action(cx.listener(Self::on_cal_month))
            .on_action(cx.listener(Self::on_cal_delete_selected))
            .on_action(cx.listener(Self::on_cal_edit_selected))
            .on_action(cx.listener(Self::on_new_event))
            .on_action(cx.listener(Self::on_cal_select_next))
            .on_action(cx.listener(Self::on_cal_select_previous))
            .on_action(cx.listener(Self::on_save_calendar_event))
            .on_action(cx.listener(Self::on_open_settings))
            .size_full()
            .bg(p.background)
            .text_color(p.foreground)
            .font_family("Inter")
            .text_size(px(13.))
            .relative()
            .child(self.render_header(cx))
            .children(self.render_update_banner(cx))
            .children(self.render_banner(cx))
            .child(div().flex_1().min_h_0().child(self.render_body(cx)))
            .child(self.render_status_bar(cx))
            .when(self.settings.is_some(), |d| d.child(self.render_settings(cx)))
            .when(self.account_setup.is_some(), |d| d.child(self.render_account_setup(cx)))
            .when(self.quick_look.is_some(), |d| d.child(self.render_quick_look(cx)))
            .when(self.palette.is_some(), |d| d.child(self.render_palette(cx)))
    }
}

#[cfg(test)]
mod sign_in_tests {
    use super::sign_in_mismatch;

    #[test]
    fn signing_in_as_another_google_account_is_called_out() {
        assert_eq!(sign_in_mismatch(None, "b@example.com", true), None);
        assert_eq!(sign_in_mismatch(Some("A@Example.com"), "a@example.com", false), None);
        let added = sign_in_mismatch(Some("a@example.com"), "b@example.com", true).unwrap();
        assert!(added.contains("b@example.com was added") && added.contains("a@example.com still needs"), "{added}");
        let existing = sign_in_mismatch(Some("a@example.com"), "b@example.com", false).unwrap();
        assert!(!existing.contains("added") && existing.contains("not a@example.com"), "{existing}");
    }
}

#[cfg(test)]
mod new_mail_tests {
    use super::new_inbox_mail;
    use overlook_core::prefs::NewMailNotifications;
    use overlook_core::*;
    use overlook_store::Store;

    #[test]
    fn only_mail_that_arrived_since_the_last_index_is_announced() {
        let store = Store::open_in_memory().unwrap();
        let account = crate::seed_sample_account(&store).unwrap();
        let before = store.mail_index(&account.id).unwrap();
        let inbox = before.labels().iter().find(|l| l.kind == LabelKind::Inbox).unwrap().id.clone();
        let mut fresh = before
            .message(&before.message_ids_in(&before.conversations(&account.email, &overlook_store::MailboxView::Kind(LabelKind::Inbox), overlook_store::ListFilter::All, None)[0].id)[0])
            .unwrap()
            .clone();
        fresh.id = MessageId::from("brand-new");
        fresh.conversation_id = ConversationId::from("brand-new-conv");
        fresh.provider_id = Some(ProviderId("brand-new".into()));
        fresh.attachments.clear();
        fresh.is_read = false;
        fresh.is_outgoing = false;
        fresh.date = 5_000_000_000_000;
        fresh.label_ids = [inbox].into_iter().collect();
        fresh.body = Body { text: Some("Hi".into()), html: None };
        store.upsert_messages(&[fresh]).unwrap();
        let after = store.mail_index(&account.id).unwrap();

        let found = new_inbox_mail(&before, &after, &account.email, NewMailNotifications::All, false, 0);
        assert_eq!(found.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), vec!["brand-new"]);
        assert!(new_inbox_mail(&before, &after, &account.email, NewMailNotifications::Off, false, 0).is_empty());
        assert!(new_inbox_mail(&after, &after, &account.email, NewMailNotifications::All, false, 0).is_empty(), "nothing new");
        assert!(new_inbox_mail(&before, &after, &account.email, NewMailNotifications::All, false, 6_000_000_000_000).is_empty(), "older than the session");
    }
}
