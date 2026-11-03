//! Settings dialog: Accounts (with signatures), General, Mail, Calendar, About.
//!
//! Rendered as an overlay in the main window. Every control writes its preference
//! immediately and the window applies it; nothing here is decorative.

use super::widgets::*;
use super::MailWindow;
use crate::assets::icon;
use crate::theme::ActivePalette;
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::combobox::{Combobox, ComboboxEvent, ComboboxState};
use gpui_kit::component::searchable_list::{SearchableListItem, SearchableVec};
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{Disableable, Sizable, IndexPath};
use gpui_kit::prelude::*;
use gpui_kit::*;
use overlook_core::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsPage {
    Accounts,
    General,
    Mail,
    Calendar,
    About,
}

impl SettingsPage {
    pub(crate) const ALL: [SettingsPage; 5] = [SettingsPage::Accounts, SettingsPage::General, SettingsPage::Mail, SettingsPage::Calendar, SettingsPage::About];

    pub(crate) fn label(self) -> &'static str {
        match self {
            SettingsPage::Accounts => "Accounts",
            SettingsPage::General => "General",
            SettingsPage::Mail => "Mail",
            SettingsPage::Calendar => "Calendar",
            SettingsPage::About => "About",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            SettingsPage::Accounts => "user-round",
            SettingsPage::General => "settings-2",
            SettingsPage::Mail => "mail",
            SettingsPage::Calendar => "calendar",
            SettingsPage::About => "info",
        }
    }
}

pub struct SettingsState {
    pub page: SettingsPage,
    pub focus: FocusHandle,
    pub scroll: ScrollHandle,
    /// Account shown on the Accounts page.
    pub account: AccountId,
    pub name_input: Entity<InputState>,
    /// HTML source of the signature being edited.
    pub signature: Entity<TextareaState>,
    pub signature_name: Entity<InputState>,
    pub selected_signature: Option<String>,
    /// Automatic replies, once loaded from the provider.
    pub vacation: Option<VacationSettings>,
    pub vacation_loading: bool,
    pub vacation_note: Option<String>,
    pub vacation_subject: Entity<InputState>,
    pub vacation_body: Entity<TextareaState>,
    pub vacation_start: Entity<InputState>,
    pub vacation_end: Entity<InputState>,
    pub attachments_dir: Entity<InputState>,
    pub work_start: Entity<InputState>,
    pub work_end: Entity<InputState>,
    pub time_zone: Entity<ComboboxState<SearchableVec<ZoneChoice>>>,
    pub caldav: Option<super::calendar_setup::CalendarSetup>,
    /// Filters every page's settings by name; non-empty shows results instead of a page.
    pub search: Entity<InputState>,
    /// The setting a search result jumped to: scrolled into view and briefly highlighted.
    pub scroll_target: Option<ScrollTarget>,
    pub _subscriptions: Vec<Subscription>,
}

/// Where a search result should land. `key` is the row or section text to scroll to;
/// `fallback` (the entry's section title) is used when that row is not on screen, such
/// as automatic replies on an account without them.
#[derive(Clone)]
pub struct ScrollTarget {
    key: &'static str,
    fallback: &'static str,
    anchor: ScrollAnchor,
    fallback_anchor: ScrollAnchor,
    found: std::rc::Rc<std::cell::Cell<bool>>,
}

thread_local! {
    /// The scroll target and highlight colour while the settings page renders. The row
    /// helpers (`setting_row`, `section`, `scroll_target`) are free functions called from
    /// every page, so they read this instead of taking the target as an argument.
    static RENDER_TARGET: std::cell::RefCell<Option<(ScrollTarget, Hsla)>> = const { std::cell::RefCell::new(None) };
}

/// Marks `element` as the place the current search result lands, when `key` names it.
fn scroll_target(key: &str, element: impl IntoElement) -> AnyElement {
    RENDER_TARGET.with_borrow(|target| {
        let Some((t, highlight)) = target.as_ref() else { return element.into_any_element() };
        let anchor = if key == t.key {
            t.found.set(true);
            t.anchor.clone()
        } else if key == t.fallback {
            t.fallback_anchor.clone()
        } else {
            return element.into_any_element();
        };
        div()
            .id("settings-scroll-target")
            .anchor_scroll(Some(anchor))
            .when(key == t.key, |d| d.mx(px(-10.)).px(px(10.)).my(px(-6.)).py(px(6.)).rounded(px(8.)).bg(*highlight))
            .child(element)
            .into_any_element()
    })
}

#[derive(Clone)]
pub struct ZoneChoice { definition: Option<overlook_core::tz::ZoneDefinition>, title: SharedString }
impl SearchableListItem for ZoneChoice {
    type Value = Option<overlook_core::tz::ZoneDefinition>;
    fn title(&self) -> SharedString { self.title.clone() }
    fn value(&self) -> &Self::Value { &self.definition }
}

pub(super) fn time_zone_choices(selected: &Option<overlook_core::tz::ZoneDefinition>) -> Vec<ZoneChoice> {
    let mut definitions = vec![None];
    definitions.extend(overlook_core::tz::available_zone_names().iter().map(|name| Some(overlook_core::tz::ZoneDefinition::Named(name.clone()))));
    if selected.is_some() && !definitions.contains(selected) { definitions.push(selected.clone()); }
    definitions.into_iter().map(|definition| {
        let title = match &definition {
            None => "Follow system".into(),
            Some(overlook_core::tz::ZoneDefinition::Named(name)) => name.replace('_', " ").into(),
            Some(definition) => format!("{} (kept)", definition.resolve().map(|zone| overlook_core::tz::zone_label(&zone)).unwrap_or_else(|_| "Local time".into())).into(),
        };
        ZoneChoice { definition, title }
    }).collect()
}

impl MailWindow {
    pub(crate) fn save_prefs(&mut self, cx: &mut Context<Self>) {
        let (store, prefs) = (self.store.clone(), self.prefs.clone());
        crate::persist::later(move || {
            if let Err(error) = store.save_preferences(&prefs) { tracing::warn!("could not save preferences: {error}"); }
        });
        overlook_core::tz::set_clock_24h(self.prefs.clock_24h);
        crate::notify::set_mirror(self.prefs.omarchy_notifications);
        self.reminders_enabled = self.prefs.event_reminders;
        self.reload(cx);
    }

    pub(crate) fn account_prefs_for(&self, id: &AccountId) -> AccountPrefs {
        self.account_prefs.get(id).cloned().unwrap_or_default()
    }

    fn update_selected_signature(&mut self, account: &AccountId, change: impl FnOnce(&mut Signature)) {
        let Some(id) = self.settings.as_ref().and_then(|s| s.selected_signature.clone()) else { return };
        let mut p = self.account_prefs_for(account);
        if let Some(s) = p.signatures.iter_mut().find(|s| s.id == id) {
            change(s);
        }
        self.save_account_prefs(account, p);
    }

    pub(super) fn select_signature(&mut self, account: &AccountId, id: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        // Leaving a signature: let go of the pictures it no longer shows.
        self.prune_signature_images(account);
        let p = self.account_prefs_for(account);
        let sig = id.as_deref().and_then(|i| p.signature_by_id(i)).cloned().unwrap_or_default();
        if let Some(state) = self.settings.as_mut() {
            state.selected_signature = id;
            let (name, source) = (state.signature_name.clone(), state.signature.clone());
            name.update(cx, |s, cx| s.set_value(sig.name.clone(), window, cx));
            source.update(cx, |s, cx| s.set_value(sig.html.clone(), window, cx));
        }
        cx.notify();
    }

    fn add_signature(&mut self, account: &AccountId, window: &mut Window, cx: &mut Context<Self>) {
        let mut p = self.account_prefs_for(account);
        let id = AccountPrefs::new_signature_id();
        p.signatures.push(Signature { id: id.clone(), name: format!("Signature {}", p.signatures.len() + 1), ..Default::default() });
        if p.default_new.is_none() {
            p.default_new = Some(id.clone());
        }
        if p.default_reply.is_none() {
            p.default_reply = Some(id.clone());
        }
        self.save_account_prefs(account, p);
        self.select_signature(account, Some(id), window, cx);
        if let Some(state) = self.settings.as_ref() {
            let name = state.signature_name.clone();
            name.update(cx, |s, cx| {
                s.focus(window, cx);
                s.select_all(window, cx);
            });
        }
    }

    fn delete_signature(&mut self, account: &AccountId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.settings.as_ref().and_then(|s| s.selected_signature.clone()) else { return };
        let mut p = self.account_prefs_for(account);
        p.signatures.retain(|s| s.id != id);
        let p = p.normalized();
        let next = p.signatures.first().map(|s| s.id.clone());
        self.save_account_prefs(account, p);
        self.select_signature(account, next, window, cx);
    }

    fn set_default_signature(&mut self, account: &AccountId, reply: bool, id: Option<String>, cx: &mut Context<Self>) {
        let mut p = self.account_prefs_for(account);
        if reply {
            p.default_reply = id;
        } else {
            p.default_new = id;
        }
        self.save_account_prefs(account, p);
        cx.notify();
    }

    fn load_vacation(&mut self, account: AccountId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(provider) = self.providers.get(&account).map(|p| p.mail.clone()).filter(|p| p.has_settings_access()) else { return };
        let Some(acct) = self.accounts.iter().find(|a| a.id == account).cloned() else { return };
        if let Some(state) = self.settings.as_mut() {
            state.vacation_loading = true;
        }
        cx.spawn_in(window, async move |this, cx| {
            let result = cx.background_spawn(async move { provider.fetch_vacation(&acct).await }).await;
            let _ = this.update_in(cx, |this, window, cx| {
                let Some(state) = this.settings.as_mut().filter(|s| s.account == account) else { return };
                state.vacation_loading = false;
                match result {
                    Ok(Some(v)) => {
                        let (subject, body, start, end) = (state.vacation_subject.clone(), state.vacation_body.clone(), state.vacation_start.clone(), state.vacation_end.clone());
                        subject.update(cx, |s, cx| s.set_value(v.subject.clone(), window, cx));
                        body.update(cx, |s, cx| s.set_value(v.body.clone(), window, cx));
                        start.update(cx, |s, cx| s.set_value(v.start.map(|t| overlook_core::tz::local_date(t).to_string()).unwrap_or_default(), window, cx));
                        end.update(cx, |s, cx| s.set_value(v.end.map(|t| overlook_core::tz::local_date(t).to_string()).unwrap_or_default(), window, cx));
                        state.vacation = Some(v);
                    }
                    Ok(None) => state.vacation = None,
                    Err(e) => state.vacation_note = Some(format!("Couldn’t load automatic replies: {e}")),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn save_vacation(&mut self, account: AccountId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(state) = self.settings.as_ref() else { return };
        let Some(mut v) = state.vacation.clone() else { return };
        v.subject = state.vacation_subject.read(cx).value().trim().to_owned();
        v.body = state.vacation_body.read(cx).value().trim().to_owned();
        let parse = |s: &str| -> Result<Option<i64>, ()> {
            let s = s.trim();
            if s.is_empty() {
                return Ok(None);
            }
            overlook_core::tz::parse_date(s).map(|d| Some(overlook_core::tz::date_start(d))).ok_or(())
        };
        let (Ok(start), Ok(end)) = (parse(&state.vacation_start.read(cx).value()), parse(&state.vacation_end.read(cx).value())) else {
            if let Some(s) = self.settings.as_mut() {
                s.vacation_note = Some("Enter dates as YYYY-MM-DD.".into());
            }
            cx.notify();
            return;
        };
        v.start = start;
        // Google treats the end as exclusive; the last day runs through midnight.
        v.end = end.map(|e| overlook_core::tz::add_days(e, 1));
        if v.enabled && v.body.is_empty() {
            if let Some(s) = self.settings.as_mut() {
                s.vacation_note = Some("Write the reply before turning it on.".into());
            }
            cx.notify();
            return;
        }
        let Some(provider) = self.providers.get(&account).map(|p| p.mail.clone()) else { return };
        let Some(acct) = self.accounts.iter().find(|a| a.id == account).cloned() else { return };
        if let Some(s) = self.settings.as_mut() {
            s.vacation_note = Some("Saving…".into());
        }
        cx.notify();
        let saved = v.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx.background_spawn(async move { provider.set_vacation(&acct, &saved).await }).await;
            let _ = this.update_in(cx, |this, window, cx| {
                if let Some(s) = this.settings.as_mut().filter(|s| s.account == account) {
                    match result {
                        Ok(()) => {
                            s.vacation = Some(v.clone());
                            s.vacation_note = Some(if v.enabled { "Automatic replies are on.".into() } else { "Automatic replies are off.".into() });
                        }
                        Err(e) => s.vacation_note = Some(format!("Couldn’t save: {e}")),
                    }
                }
                let _ = window;
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn save_account_prefs(&mut self, id: &AccountId, prefs: AccountPrefs) {
        self.save_setting(&format!("account_prefs:{}", id.0), &prefs);
        self.account_prefs.insert(id.clone(), prefs);
    }

    pub(crate) fn open_settings(&mut self, page: SettingsPage, window: &mut Window, cx: &mut Context<Self>) {
        let account = self.account.id.clone();
        self.build_settings(page, account, window, cx);
    }

    fn build_settings(&mut self, page: SettingsPage, account: AccountId, window: &mut Window, cx: &mut Context<Self>) {
        self.cancel_calendar_setup_dialog();
        let acct = self.accounts.iter().find(|a| a.id == account).cloned().unwrap_or_else(|| self.account.clone());
        let ap = self.account_prefs_for(&acct.id);
        let name_input = cx.new(|cx| InputState::new(window, cx).placeholder("Your name").default_value(acct.display_name.clone()));
        let selected_signature = ap.default_new.clone().or_else(|| ap.signatures.first().map(|s| s.id.clone()));
        let selected = selected_signature.as_deref().and_then(|id| ap.signature_by_id(id)).cloned().unwrap_or_default();
        let signature = cx.new(|cx| TextareaState::new(window, cx).placeholder("<p><b>Jason Alexander</b><br>Last Refuge</p>").default_value(selected.html.clone()));
        let signature_name = cx.new(|cx| InputState::new(window, cx).placeholder("Signature name").default_value(selected.name.clone()));
        let vacation_subject = cx.new(|cx| InputState::new(window, cx).placeholder("Out of office"));
        let vacation_body = cx.new(|cx| TextareaState::new(window, cx).placeholder("I’m away until Monday and will reply when I’m back."));
        let vacation_start = cx.new(|cx| InputState::new(window, cx).placeholder("First day, YYYY-MM-DD (optional)"));
        let vacation_end = cx.new(|cx| InputState::new(window, cx).placeholder("Last day, YYYY-MM-DD (optional)"));
        let attachments_dir = cx.new(|cx| InputState::new(window, cx).placeholder("~/Downloads").default_value(self.prefs.attachments_dir.clone().unwrap_or_default()));
        let work_start = cx.new(|cx| InputState::new(window, cx).placeholder("8:00 AM").default_value(hour_text(self.prefs.work_start_hour)));
        let work_end = cx.new(|cx| InputState::new(window, cx).placeholder("6:00 PM").default_value(hour_text(self.prefs.work_end_hour)));
        let zones = time_zone_choices(&self.prefs.time_zone);
        let selected = zones.iter().position(|choice| choice.definition == self.prefs.time_zone).unwrap_or(0);
        let time_zone = cx.new(|cx| ComboboxState::new(SearchableVec::new(zones), vec![IndexPath::default().row(selected)], window, cx).searchable(true));

        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search settings"));

        let mut subs = Vec::new();
        subs.push(cx.subscribe_in(&search, window, |this, state, event: &InputEvent, window, cx| match event {
            InputEvent::Change => {
                if let Some(s) = this.settings.as_ref() { s.scroll.set_offset(point(px(0.), px(0.))); }
                cx.notify();
            }
            InputEvent::PressEnter { .. } => {
                let query = state.read(cx).value().to_string();
                if let Some(&first) = search_settings(&query).first() { this.open_settings_result(first.page, Some(first), window, cx); }
            }
            _ => {}
        }));
        subs.push(cx.subscribe_in(&time_zone, window, |this, _, event: &ComboboxEvent<SearchableVec<ZoneChoice>>, window, cx| {
            if let ComboboxEvent::Confirm(values) = event {
                if let Some(name) = values.first().filter(|name| **name != this.prefs.time_zone) { this.change_time_zone(name.clone(), window, cx); }
            }
        }));
        let for_account = acct.id.clone();
        subs.push(cx.subscribe_in(&name_input, window, move |this, state, event: &InputEvent, _window, cx| {
            if matches!(event, InputEvent::Change) {
                let name = state.read(cx).value().trim().to_owned();
                if name.is_empty() {
                    return;
                }
                if let Some(a) = this.accounts.iter_mut().find(|a| a.id == for_account) {
                    a.display_name = name.clone();
                    let _ = this.store.upsert_account(a);
                }
                if this.account.id == for_account {
                    this.account.display_name = name;
                }
                cx.notify();
            }
        }));
        subs.push(cx.subscribe_in(&signature, window, move |this, state, event: &InputEvent, window, cx| {
            if matches!(event, InputEvent::Change) {
                let html = state.read(cx).value().to_string();
                this.edit_signature_html(html, window, cx);
            }
        }));
        let for_account = acct.id.clone();
        subs.push(cx.subscribe_in(&signature_name, window, move |this, state, event: &InputEvent, _window, cx| {
            if matches!(event, InputEvent::Change) {
                let name = state.read(cx).value().trim().to_owned();
                this.update_selected_signature(&for_account, |s| s.name = name);
                cx.notify();
            }
        }));
        subs.push(cx.subscribe_in(&attachments_dir, window, |this, state, event: &InputEvent, _window, cx| {
            if matches!(event, InputEvent::Change) {
                let v = state.read(cx).value().trim().to_owned();
                this.prefs.attachments_dir = if v.is_empty() { None } else { Some(v) };
                this.save_prefs(cx);
            }
        }));
        subs.push(cx.subscribe_in(&work_start, window, |this, state, event: &InputEvent, _window, cx| {
            if matches!(event, InputEvent::Change) {
                if let Some(t) = overlook_core::tz::parse_clock(&state.read(cx).value()) {
                    this.prefs.work_start_hour = t.hour() as u8;
                    this.save_prefs(cx);
                }
            }
        }));
        subs.push(cx.subscribe_in(&work_end, window, |this, state, event: &InputEvent, _window, cx| {
            if matches!(event, InputEvent::Change) {
                if let Some(t) = overlook_core::tz::parse_clock(&state.read(cx).value()) {
                    this.prefs.work_end_hour = t.hour() as u8;
                    this.save_prefs(cx);
                }
            }
        }));

        let caldav = self.build_calendar_setup(&acct, window, cx);
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        self.settings = Some(SettingsState {
            page,
            focus,
            scroll: ScrollHandle::new(),
            account: acct.id.clone(),
            name_input,
            signature,
            signature_name,
            selected_signature,
            vacation: None,
            vacation_loading: false,
            vacation_note: None,
            vacation_subject,
            vacation_body,
            vacation_start,
            vacation_end,
            attachments_dir,
            work_start,
            work_end,
            time_zone,
            caldav,
            search,
            scroll_target: None,
            _subscriptions: subs,
        });
        self.load_vacation(acct.id, window, cx);
        cx.notify();
    }

    pub(crate) fn close_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.cancel_calendar_setup_dialog();
        if let Some(account) = self.settings.as_ref().map(|s| s.account.clone()) {
            self.prune_signature_images(&account);
        }
        self.settings = None;
        self.focus_current_module(window, cx);
        cx.notify();
    }

    fn set_settings_page(&mut self, page: SettingsPage, cx: &mut Context<Self>) {
        if let Some(s) = self.settings.as_mut() {
            s.page = page;
            s.scroll_target = None;
            s.scroll.set_offset(point(px(0.), px(0.)));
        }
        cx.notify();
    }

    /// The settings search query, trimmed; empty when nothing is typed.
    pub(crate) fn settings_query(&self, cx: &App) -> String {
        self.settings.as_ref().map(|s| s.search.read(cx).value().trim().to_owned()).unwrap_or_default()
    }

    /// Clears the settings search. Returns whether there was anything to clear, so Escape
    /// empties the search before it closes the dialog.
    pub(crate) fn clear_settings_search(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.settings_query(cx).is_empty() { return false; }
        if let Some(search) = self.settings.as_ref().map(|s| s.search.clone()) {
            search.update(cx, |s, cx| s.set_value("", window, cx));
        }
        cx.notify();
        true
    }

    /// Ctrl F / F3: put the cursor in the search box with any earlier query selected.
    fn focus_settings_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(search) = self.settings.as_ref().map(|s| s.search.clone()) else { return };
        search.update(cx, |s, cx| {
            s.focus(window, cx);
            s.select_all(window, cx);
        });
    }

    /// Leaves search for `page`; with an `entry`, scrolls to that setting and highlights it.
    fn open_settings_result(&mut self, page: SettingsPage, entry: Option<&'static SettingEntry>, window: &mut Window, cx: &mut Context<Self>) {
        self.clear_settings_search(window, cx);
        if let Some(focus) = self.settings.as_ref().map(|s| s.focus.clone()) { focus.focus(window, cx); }
        self.set_settings_page(page, cx);
        let (Some(entry), Some(state)) = (entry, self.settings.as_mut()) else { return };
        let target = ScrollTarget {
            key: entry.anchor(),
            fallback: entry.section,
            anchor: ScrollAnchor::for_handle(state.scroll.clone()),
            fallback_anchor: ScrollAnchor::for_handle(state.scroll.clone()),
            found: Default::default(),
        };
        state.scroll_target = Some(target.clone());
        let found = target.found.clone();
        // The anchors learn their position when the new page is laid out. Wait a frame past
        // that layout before choosing the row or its section, then draw at the new offset.
        window.on_next_frame(move |window, _| {
            window.on_next_frame(move |window, cx| {
                if target.found.get() { target.anchor.scroll_to(window, cx) } else { target.fallback_anchor.scroll_to(window, cx) }
                window.on_next_frame(|window, _| window.refresh());
            });
        });
        // The highlight fades once the eye has found it, unless another jump replaced it.
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(std::time::Duration::from_millis(1800)).await;
            let _ = this.update(cx, |this, cx| {
                if let Some(s) = this.settings.as_mut().filter(|s| s.scroll_target.as_ref().is_some_and(|t| std::rc::Rc::ptr_eq(&t.found, &found))) {
                    s.scroll_target = None;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn set_settings_account(&mut self, id: AccountId, window: &mut Window, cx: &mut Context<Self>) {
        self.build_settings(SettingsPage::Accounts, id, window, cx);
    }

    // ---------------------------------------------------------------- render

    pub(crate) fn render_settings(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = *cx.palette();
        let Some(state) = self.settings.as_ref() else { return div().into_any_element() };
        let page = state.page;
        let query = self.settings_query(cx);
        let results = search_settings(&query);
        let searching = !query.is_empty();

        let nav = {
            let p = cx.palette().navigation_palette();
            v_flex()
            .w(px(200.))
            .flex_none()
            .h_full()
            .bg(p.sidebar)
            .text_color(p.foreground)
            .border_r_1()
            .border_color(p.divider)
            .p(px(12.))
            .gap(px(2.))
            .child(div().px(px(10.)).pt(px(6.)).pb(px(10.)).text_size(px(18.)).font_weight(FontWeight::SEMIBOLD).child("Settings"))
            .child(div().pb(px(10.)).child(Input::new(&state.search).small().prefix(icon("search").size(px(14.)).text_color(p.muted)).cleanable(true)))
            .children(SettingsPage::ALL.iter().map(|&pg| {
                let active = pg == page && !searching;
                let matches = results.iter().filter(|r| r.page == pg).count();
                h_flex()
                    .id(SharedString::from(format!("settings-nav-{}", pg.label())))
                    .focusable()
                    .tab_stop(true)
                    .role(gpui::Role::Button)
                    .aria_label(pg.label())
                    .items_center()
                    .gap(px(10.))
                    .px(px(10.))
                    .h(px(34.))
                    .rounded(px(7.))
                    .cursor_pointer()
                    .text_size(px(13.))
                    .text_color(if active { p.accent } else { p.foreground })
                    .font_weight(if active { FontWeight::SEMIBOLD } else { FontWeight::NORMAL })
                    .when(active, |d| d.bg(p.selection))
                    .when(!active, |d| d.hover(|d| d.bg(p.hover)))
                    .focus_visible(|d| d.bg(p.selection).border_1().border_color(p.accent))
                    .child(icon(pg.icon()).size(px(16.)).text_color(if active { p.accent } else { p.muted }))
                    .child(div().flex_1().child(pg.label()))
                    .when(searching, |d| d.when(matches == 0, |d| d.opacity(0.45)).when(matches > 0, |d| d.child(div().text_size(px(11.)).text_color(p.muted).child(matches.to_string()))))
                    .on_click(cx.listener(move |this, _, window, cx| this.open_settings_result(pg, None, window, cx)))
                    .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                        if matches!(event.keystroke.key.as_str(), "enter" | "space") && !event.keystroke.modifiers.control && !event.keystroke.modifiers.alt && !event.keystroke.modifiers.platform {
                            this.open_settings_result(pg, None, window, cx);
                            cx.stop_propagation();
                        }
                    }))
            }))
        };

        RENDER_TARGET.set(state.scroll_target.clone().filter(|_| !searching).map(|t| (t, p.selection)));
        let content: AnyElement = if searching { self.render_settings_results(&query, &results, cx) } else { match page {
            SettingsPage::Accounts => self.render_settings_accounts(state, cx).into_any_element(),
            SettingsPage::General => self.render_settings_general(cx).into_any_element(),
            SettingsPage::Mail => self.render_settings_mail(state, cx).into_any_element(),
            SettingsPage::Calendar => self.render_settings_calendar(state, cx).into_any_element(),
            SettingsPage::About => self.render_settings_about(cx).into_any_element(),
        } };
        RENDER_TARGET.set(None);
        let title: SharedString = if searching { format!("Results for “{query}”").into() } else { page.label().into() };

        div()
            .id("settings-overlay")
            .absolute()
            .inset_0()
            // Nothing underneath (a message body) may see the clicks meant for this.
            .occlude()
            .bg(gpui_kit::black().opacity(if p.is_dark() { 0.55 } else { 0.3 }))
            .flex()
            .items_center()
            .justify_center()
            .on_click(cx.listener(|this, _, window, cx| this.close_settings(window, cx)))
            .child(
                h_flex()
                    .id("settings-dialog")
                    .key_context(super::SETTINGS_CONTEXT)
                    .track_focus(&state.focus)
                    .on_action(cx.listener(|this, _: &super::FocusSettingsSearch, window, cx| this.focus_settings_search(window, cx)))
                    .tab_group()
                    .role(gpui::Role::Dialog)
                    .aria_label("Settings")
                    .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                        if event.keystroke.modifiers.control || event.keystroke.modifiers.alt || event.keystroke.modifiers.platform { return; }
                        let Some(state) = this.settings.as_ref() else { return };
                        let old = state.scroll.offset();
                        let page = state.scroll.bounds().size.height * 0.85;
                        let next = match event.keystroke.key.as_str() {
                            "pageup" => old.y + page,
                            "pagedown" => old.y - page,
                            _ => return,
                        };
                        state.scroll.set_offset(point(old.x, next.min(px(0.)).max(-state.scroll.max_offset().y)));
                        window.prevent_default();
                        cx.stop_propagation();
                        cx.notify();
                    }))
                    .w(px(960.))
                    .h(px(640.))
                    .max_w(relative(0.94))
                    .max_h(relative(0.92))
                    .rounded(px(12.))
                    .overflow_hidden()
                    .bg(p.surface)
                    .border_1()
                    .border_color(p.divider)
                    .shadow_lg()
                    .on_click(|_, _, cx| cx.stop_propagation())
                    .child(nav)
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .child(
                                h_flex()
                                    .items_center()
                                    .h(px(52.))
                                    .px(px(24.))
                                    .border_b_1()
                                    .border_color(p.divider)
                                    .child(div().flex_1().text_size(px(16.)).font_weight(FontWeight::SEMIBOLD).truncate().child(title))
                                    .child(kbd("Esc", &p))
                                    .child(Button::new("settings-close").ghost().small().icon(icon("x").size(px(16.))).ml(px(8.)).on_click(cx.listener(|this, _, window, cx| this.close_settings(window, cx)))),
                            )
                            .child(v_flex().id("settings-content").flex_1().min_h_0().p(px(24.)).gap(px(22.))
                                .child(div().flex_none().child(content)).overflow_y_scroll().track_scroll(&state.scroll).vertical_scrollbar(&state.scroll)),
                    ),
            )
            .into_any_element()
    }

    fn render_settings_results(&self, query: &str, results: &[&'static SettingEntry], cx: &mut Context<Self>) -> AnyElement {
        let p = *cx.palette();
        if results.is_empty() {
            return v_flex()
                .gap(px(4.))
                .child(div().text_size(px(13.)).font_weight(FontWeight::MEDIUM).child(format!("No settings match “{query}”.")))
                .child(div().text_size(px(12.)).text_color(p.muted).child("Try a shorter word, such as “theme”, “signature” or “reminder”."))
                .into_any_element();
        }
        v_flex()
            .gap(px(4.))
            .children(results.iter().enumerate().map(|(i, &entry)| {
                let pg = entry.page;
                v_flex()
                    .id(SharedString::from(format!("settings-result-{i}")))
                    .focusable()
                    .tab_stop(true)
                    .role(gpui::Role::Button)
                    .aria_label(entry.label)
                    .gap(px(2.))
                    .px(px(12.))
                    .py(px(9.))
                    .rounded(px(8.))
                    .cursor_pointer()
                    .hover(|d| d.bg(p.hover))
                    .focus_visible(|d| d.bg(p.selection).border_1().border_color(p.accent))
                    .child(
                        h_flex()
                            .items_center()
                            .gap(px(8.))
                            .child(icon(pg.icon()).size(px(14.)).text_color(p.muted))
                            .child(div().flex_1().min_w_0().truncate().text_size(px(13.)).font_weight(FontWeight::MEDIUM).child(entry.label))
                            .child(div().flex_none().text_size(px(11.)).text_color(p.muted).child(format!("{} › {}", pg.label(), entry.section))),
                    )
                    .when(!entry.description.is_empty(), |d| d.child(div().pl(px(22.)).text_size(px(12.)).text_color(p.muted).truncate().child(entry.description)))
                    .on_click(cx.listener(move |this, _, window, cx| this.open_settings_result(pg, Some(entry), window, cx)))
                    .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                        if matches!(event.keystroke.key.as_str(), "enter" | "space") && !event.keystroke.modifiers.control && !event.keystroke.modifiers.alt && !event.keystroke.modifiers.platform {
                            this.open_settings_result(pg, Some(entry), window, cx);
                            cx.stop_propagation();
                        }
                    }))
            }))
            .into_any_element()
    }

    fn render_settings_accounts(&self, state: &SettingsState, cx: &mut Context<Self>) -> impl IntoElement {
        let p = *cx.palette();
        let selected = state.account.clone();
        let account = self.accounts.iter().find(|a| a.id == selected).cloned().unwrap_or_else(|| self.account.clone());
        let ap = self.account_prefs_for(&account.id);
        let status = self.store.account_status(&account.id).unwrap_or_default();
        let issue = self.provider_issues.get(&account.id).cloned();
        let email = account.email.clone();
        let is_gmail = account.provider == ProviderKind::Gmail;
        let is_password_account = matches!(account.provider, ProviderKind::Imap | ProviderKind::Pop3);
        let servers = if is_password_account { self.store.servers(&account.id).ok().flatten() } else { None };
        let connection_line = match (account.provider, &servers) {
            (ProviderKind::Gmail, _) => "Connected through the Gmail and Google Calendar APIs.".to_owned(),
            (ProviderKind::Sample, _) => "Sample account with fictional mail; nothing leaves this device.".to_owned(),
            (ProviderKind::Imap, Some(s)) => format!("Mail is read over IMAP from {} and sent through {}. Password kept in the system keyring.", s.incoming.address(), s.outgoing.address()),
            (ProviderKind::Pop3, Some(s)) => format!("Mail is downloaded over POP3 from {} and sent through {}. This device holds the only copy.", s.incoming.address(), s.outgoing.address()),
            (_, None) => "Server settings are not set up yet.".to_owned(),
        };
        let sign_in_label = super::sign_in_label_for(account.provider);

        let account_list = v_flex().gap(px(4.)).children(self.accounts.iter().map(|a| {
            let id = a.id.clone();
            let active = a.id == selected;
            h_flex()
                .id(SharedString::from(format!("settings-account-{}", a.id)))
                .items_center()
                .gap(px(10.))
                .px(px(10.))
                .py(px(8.))
                .rounded(px(8.))
                .cursor_pointer()
                .border_1()
                .border_color(if active { p.accent } else { p.divider })
                .when(active, |d| d.bg(p.selection))
                .when(!active, |d| d.hover(|d| d.bg(p.hover)))
                .child(owner_avatar(&Address::named(&a.display_name, &a.email), 28., &p))
                .child(v_flex().flex_1().min_w_0().child(div().text_size(px(13.)).font_weight(FontWeight::MEDIUM).truncate().child(a.display_name.clone())).child(div().text_size(px(11.)).text_color(p.muted).truncate().child(format!("{} · {}", a.email, a.provider.label()))))
                .on_click(cx.listener(move |this, _, window, cx| this.set_settings_account(id.clone(), window, cx)))
        }));

        let remove_id = account.id.clone();
        v_flex()
            .gap(px(22.))
            .child(section("Your accounts", Some("Mail and calendar accounts on this device.")))
            .child(account_list)
            .child(self.add_account_menu(cx))
            .child(section(&format!("{}", account.email), Some(&connection_line)))
            .child(setting_row("Your name", "Shown as the sender on messages you write.", Input::new(&state.name_input).w(px(280.)).into_any_element()))
            .child(setting_row(
                "Status",
                &status.state.sentence(status.pending_operations),
                h_flex()
                    .gap(px(6.))
                    .when(issue.is_some(), |r| {
                        let e = email.clone();
                        let id = account.id.clone();
                        r.child(Button::new("settings-signin").small().label(sign_in_label).on_click(cx.listener(move |this, _, window, cx| this.sign_in_again_for(&id, Some(e.clone()), window, cx))))
                    })
                    .when(is_password_account, |r| {
                        let id = account.id.clone();
                        let kind = account.provider;
                        r.child(Button::new("settings-servers").small().icon(icon("server").size(px(14.))).label("Server settings…").on_click(cx.listener(move |this, _, window, cx| this.open_account_setup(kind, super::SetupMode::Edit(id.clone()), window, cx))))
                    })
                    .child(Button::new("settings-sync").ghost().small().icon(icon("refresh-cw").size(px(14.))).label("Sync now").on_click(cx.listener({
                        let id = account.id.clone();
                        move |this, _, window, cx| {
                            this.next_retry.remove(&id);
                            this.sync_caldav_account(id.clone(), true, cx);
                            this.sync_account(id.clone(), window, cx);
                        }
                    })))
                    .into_any_element(),
            ))
            .children(state.caldav.as_ref().map(|form| scroll_target("Calendar server", self.render_calendar_setup(form, cx))))
            .child(section("Signatures", Some("Added below your message, above anything you are replying to. Write HTML below, use the buttons for the basics, or paste HTML from elsewhere. Pictures you insert or paste are sent with the message.")))
            .child({
                let acct_id = account.id.clone();
                let selected = state.selected_signature.clone();
                h_flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(6.))
                    .children(ap.signatures.iter().map(|s| {
                        let id = s.id.clone();
                        let acct_id = acct_id.clone();
                        segment(SharedString::from(format!("sig-{}", s.id)), if s.name.trim().is_empty() { "Untitled".to_string() } else { s.name.clone() }, selected.as_deref() == Some(&s.id), &p)
                            .on_click(cx.listener(move |this, _, window, cx| this.select_signature(&acct_id, Some(id.clone()), window, cx)))
                    }))
                    .child(Button::new("sig-add").ghost().xsmall().icon(icon("plus").size(px(13.))).label("Add").on_click(cx.listener({
                        let acct_id = acct_id.clone();
                        move |this, _, window, cx| this.add_signature(&acct_id, window, cx)
                    })))
                    .when(selected.is_some(), |r| {
                        r.child(Button::new("sig-delete").ghost().xsmall().icon(icon("trash").size(px(13.)).text_color(p.error)).label("Delete").on_click(cx.listener({
                            let acct_id = acct_id.clone();
                            move |this, _, window, cx| this.delete_signature(&acct_id, window, cx)
                        })))
                    })
            })
            .when(state.selected_signature.is_some(), |d| {
                d.child(setting_row("Name", "How this signature appears in the composer’s picker.", Input::new(&state.signature_name).w(px(280.)).into_any_element()))
                    .child(self.render_signature_editor(state, &p, cx))
            })
            .child(setting_row("Use for new messages", "", self.signature_picker("default-new", &account.id, false, cx)))
            .child(setting_row("Use for replies and forwards", "", self.signature_picker("default-reply", &account.id, true, cx)))
            .child(section("Automatic replies", Some("An out-of-office reply sent for you while it is on. Stored at your mail service, so it works even when Overlook is closed.")))
            .children(self.render_vacation(state, &account, cx))
            .child(section("Contacts", Some(if is_gmail { "Your Google Contacts and the people you have written to, for recipient suggestions and sender cards." } else { "The people you have written to, for recipient suggestions and sender cards." })))
            .child({
                let has_access = self.providers.get(&account.id).is_some_and(|p| p.mail.has_contacts_access());
                let count = self.store.contact_count(&account.id).unwrap_or(0);
                let acct_id = account.id.clone();
                let e = email.clone();
                setting_row(
                    &format!("{count} contact{}", if count == 1 { "" } else { "s" }),
                    if has_access { "Refreshed once a day." } else if is_gmail { "Sign in again to let Overlook read your contacts." } else { "" },
                    h_flex()
                        .gap(px(6.))
                        .when(!has_access && is_gmail, |r| r.child(Button::new("contacts-signin").small().label("Sign in again").on_click(cx.listener(move |this, _, window, cx| this.start_google_sign_in(Some(e.clone()), window, cx)))))
                        .when(has_access, |r| {
                            r.child(Button::new("contacts-sync").ghost().small().icon(icon("refresh-cw").size(px(14.))).label("Sync contacts now").on_click(cx.listener(move |this, _, window, cx| {
                                let _ = this.store.set_setting(&crate::sync::contacts_synced_key(&acct_id), &0i64);
                                this.sync_now(window, cx);
                            })))
                        })
                        .into_any_element(),
                )
            })
            .when(account.provider != ProviderKind::Sample && issue.is_none(), |d| {
                let sign_out_id = account.id.clone();
                d.child(section("Sign out", None)).child(setting_row(
                    "Sign out of this account",
                    if is_gmail { "Revokes Overlook’s access at Google and deletes the stored sign-in. Downloaded mail stays on this device until you sign in again. Requires internet." } else { "Deletes the stored password from the system keyring. Downloaded mail stays on this device until you sign in again." },
                    Button::new("settings-sign-out").small().label("Sign out").on_click(cx.listener(move |this, _, window, cx| {
                        this.close_settings(window, cx);
                        this.sign_out_account(sign_out_id.clone(), None, window, cx);
                    })).into_any_element(),
                ))
            })
            .child(section("Remove", None))
            .child(setting_row(
                "Remove this account from this device",
                if is_gmail { "Removes local mail and sign-in, and revokes this Google account’s access for this app’s Google project on all devices. Requires internet; mail at Google is kept." } else { "Deletes downloaded mail, events, drafts, and stored sign-in for this account. Nothing is deleted at the provider." },
                Button::new("settings-remove").danger().small().label("Remove account").on_click(cx.listener(move |this, _, window, cx| {
                    this.remove_account(remove_id.clone(), window, cx);
                })).into_any_element(),
            ))
    }

    fn render_settings_general(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let prefs = self.prefs.clone();
        let appearance = self.appearance;
        let system_zone = self.time_zone_prompt.as_ref().map(|zone| zone.zone().clone()).unwrap_or_else(overlook_core::tz::system_zone);
        let time_zone_hint = format!("System: {}. {}", overlook_core::tz::zone_label(&system_zone), if prefs.time_zone.is_some() { "Your override stays active until you select Follow system." } else { "Checks every minute and asks before changing." });
        v_flex()
            .gap(px(22.))
            .child(section("Appearance", None))
            .child(
                v_flex()
                    .gap(px(8.))
                    .child(section("Theme", Some("Follow system picks Light or Dark from your desktop setting.")))
                    .child(choice(
                        &AppearancePreference::ALL.map(|pref| (pref.label(), pref)),
                        appearance,
                        cx,
                        |this, v, window, cx| this.set_appearance(v, window, cx),
                    )),
            )
            .child(setting_row(
                "Density",
                "Compact fits more messages on screen.",
                choice(&[("Comfortable", Density::Comfortable), ("Compact", Density::Compact)], self.density, cx, |this, v, _w, cx| {
                    this.density = v;
                    this.save_setting("density", &v);
                    cx.notify();
                }),
            ))
            .child(setting_row(
                "Reading pane",
                "",
                choice(&[("On the right", ReadingPane::Right), ("Below the list", ReadingPane::Bottom)], self.reading_pane, cx, |this, v, _w, cx| this.set_reading_pane(v, cx)),
            ))
            .child(section("Start-up and time", None))
            .child(setting_row(
                "Time zone",
                &time_zone_hint,
                Combobox::new(&self.settings.as_ref().expect("settings is open").time_zone).small().w(px(260.)).disabled(self.time_zone_pending).placeholder("Time zone").menu_width(px(320.)).menu_max_h(px(280.)).search_placeholder("Search city or time zone").into_any_element(),
            ))
            .child(setting_row(
                "Open Overlook in",
                "",
                choice(&[("Mail", StartupModule::Mail), ("Calendar", StartupModule::Calendar), ("Last used", StartupModule::LastUsed)], prefs.startup_module, cx, |this, v, _w, cx| {
                    this.prefs.startup_module = v;
                    this.save_prefs(cx);
                }),
            ))
            .child(setting_row(
                "Time format",
                "",
                choice(&[("12-hour", false), ("24-hour", true)], prefs.clock_24h, cx, |this, v, _w, cx| {
                    this.prefs.clock_24h = v;
                    this.save_prefs(cx);
                }),
            ))
            .child(setting_row(
                "Week starts on",
                "",
                choice(&[("Monday", true), ("Sunday", false)], prefs.week_starts_monday, cx, |this, v, _w, cx| {
                    this.prefs.week_starts_monday = v;
                    this.save_prefs(cx);
                }),
            ))
            .child(section("Notifications", None))
            .when(crate::notify::omarchy_available(), |d| {
                d.child(setting_row(
                    "Show in Omarchy notifications",
                    "Messages such as Sent, sync errors and warnings also appear in Omarchy’s notification center; clicking one brings Overlook forward.",
                    Switch::new("pref-omarchy-notify").checked(prefs.omarchy_notifications).on_click(cx.listener(|this, checked: &bool, _w, cx| {
                        this.prefs.omarchy_notifications = *checked;
                        this.save_prefs(cx);
                    })).into_any_element(),
                ))
            })
            .child(setting_row(
                "New mail",
                "Desktop notification when new mail arrives.",
                choice(
                    &[("Off", NewMailNotifications::Off), ("Focused only", NewMailNotifications::FocusedOnly), ("All mail", NewMailNotifications::All)],
                    prefs.new_mail_notifications,
                    cx,
                    |this, v, _w, cx| {
                        this.prefs.new_mail_notifications = v;
                        this.save_prefs(cx);
                    },
                ),
            ))
            .child(setting_row(
                "Event reminders",
                "Desktop notification before events start.",
                Switch::new("pref-reminders").checked(prefs.event_reminders).on_click(cx.listener(|this, checked: &bool, _w, cx| {
                    this.prefs.event_reminders = *checked;
                    this.save_prefs(cx);
                })).into_any_element(),
            ))
            .child(section("Synchronization", None))
            .child(setting_row(
                "Check for changes every",
                "Changes you make are sent immediately; this is how often Overlook looks for new mail and events.",
                choice(&[("30 seconds", 30u32), ("1 minute", 60), ("5 minutes", 300), ("15 minutes", 900)], prefs.sync_interval_secs, cx, |this, v, _w, cx| {
                    this.prefs.sync_interval_secs = v;
                    this.save_prefs(cx);
                }),
            ))
    }

    fn render_settings_mail(&self, state: &SettingsState, cx: &mut Context<Self>) -> impl IntoElement {
        let prefs = self.prefs.clone();
        v_flex()
            .gap(px(22.))
            .child(section("Layout", None))
            .child(setting_row(
                "Focused Inbox",
                "Sort the Inbox into Focused and Other.",
                Switch::new("pref-focused").checked(self.focused_enabled).on_click(cx.listener(|this, _checked: &bool, _w, cx| this.toggle_focused_inbox(cx))).into_any_element(),
            ))
            .child(setting_row(
                "Sender pictures",
                "Show initials next to each conversation.",
                Switch::new("pref-avatars").checked(prefs.show_avatars).on_click(cx.listener(|this, checked: &bool, _w, cx| {
                    this.prefs.show_avatars = *checked;
                    this.save_prefs(cx);
                })).into_any_element(),
            ))
            .child(setting_row(
                "Message preview text",
                "Show the first line of each conversation in the list.",
                Switch::new("pref-previews").checked(prefs.show_previews).on_click(cx.listener(|this, checked: &bool, _w, cx| {
                    this.prefs.show_previews = *checked;
                    this.save_prefs(cx);
                })).into_any_element(),
            ))
            .child(section("Reading", None))
            .child(setting_row(
                "Mark as read",
                "When a conversation is opened in the reading pane.",
                choice(&[("When selected", MarkAsRead::WhenSelected), ("After 3 seconds", MarkAsRead::AfterDelay), ("Never", MarkAsRead::Never)], prefs.mark_as_read, cx, |this, v, _w, cx| {
                    this.prefs.mark_as_read = v;
                    this.save_prefs(cx);
                }),
            ))
            .child(setting_row(
                "Remote content",
                "Remote images and fonts can reveal when you open a message. Browser resources use the same consent decision.",
                choice(&[("Block until I choose", RemoteImages::Block), ("Always show", RemoteImages::Allow)], prefs.remote_images, cx, |this, v, _w, cx| {
                    this.prefs.remote_images = v;
                    this.save_prefs(cx);
                }),
            ))
            .child(section("Compose and reply", None))
            .child(setting_row(
                "Undo send",
                "A sent message waits this long in the Outbox, with an Undo button, before it leaves.",
                choice(&[("Off", 0u32), ("5 seconds", 5), ("10 seconds", 10), ("20 seconds", 20), ("30 seconds", 30)], prefs.undo_send_secs, cx, |this, v, _w, cx| {
                    this.prefs.undo_send_secs = v;
                    this.save_prefs(cx);
                }),
            ))
            .child(setting_row(
                "Default reply",
                "What the reply box and Ctrl R do.",
                choice(&[("Reply", false), ("Reply all", true)], prefs.reply_all_by_default, cx, |this, v, _w, cx| {
                    this.prefs.reply_all_by_default = v;
                    this.save_prefs(cx);
                }),
            ))
            .child(setting_row(
                "Quote the original message in replies",
                "",
                Switch::new("pref-quote").checked(prefs.quote_on_reply).on_click(cx.listener(|this, checked: &bool, _w, cx| {
                    this.prefs.quote_on_reply = *checked;
                    this.save_prefs(cx);
                })).into_any_element(),
            ))
            .child(section("Attachments", None))
            .child(setting_row("Save attachments to", "Folder offered first when you save an attachment. Leave empty for Downloads.", Input::new(&state.attachments_dir).w(px(320.)).into_any_element()))
            .child(section("Download", None))
            .child(setting_row(
                "Inbox messages to download",
                "Newest messages fetched for each account; Sent, Archive, and other mailboxes scale with it. Larger numbers take longer on first sync.",
                choice(&[("100", 100u32), ("300", 300), ("1,000", 1000)], prefs.inbox_messages, cx, |this, v, window, cx| {
                    this.prefs.inbox_messages = v;
                    this.save_prefs(cx);
                    this.rebuild_providers_for_download_size(window, cx);
                }),
            ))
    }

    fn render_settings_calendar(&self, state: &SettingsState, cx: &mut Context<Self>) -> impl IntoElement {
        let prefs = self.prefs.clone();
        v_flex()
            .gap(px(22.))
            .child(section("Working hours", Some("Hours outside this range are shaded in Day and Week views.")))
            .child(setting_row("Start", "", Input::new(&state.work_start).w(px(140.)).into_any_element()))
            .child(setting_row("End", "", Input::new(&state.work_end).w(px(140.)).into_any_element()))
            .child(section("Events", None))
            .child(setting_row(
                "Default event length",
                "",
                choice(&[("15 min", 15u32), ("30 min", 30), ("45 min", 45), ("1 hour", 60)], prefs.default_event_minutes, cx, |this, v, _w, cx| {
                    this.prefs.default_event_minutes = v;
                    this.save_prefs(cx);
                }),
            ))
            .child(setting_row(
                "Default reminder",
                "Used when an event or calendar sets no reminder of its own.",
                choice(&[("None", 0u32), ("5 min", 5), ("10 min", 10), ("15 min", 15), ("30 min", 30)], prefs.default_reminder_minutes, cx, |this, v, _w, cx| {
                    this.prefs.default_reminder_minutes = v;
                    this.save_prefs(cx);
                }),
            ))
            .child(setting_row(
                "Show declined events",
                "",
                Switch::new("pref-declined").checked(prefs.show_declined_events).on_click(cx.listener(|this, checked: &bool, _w, cx| {
                    this.prefs.show_declined_events = *checked;
                    this.save_prefs(cx);
                })).into_any_element(),
            ))
            .child(setting_row(
                "Event reminders",
                "Desktop notification before events start.",
                Switch::new("pref-reminders-cal").checked(prefs.event_reminders).on_click(cx.listener(|this, checked: &bool, _w, cx| {
                    this.prefs.event_reminders = *checked;
                    this.save_prefs(cx);
                })).into_any_element(),
            ))
    }

    fn render_settings_about(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = *cx.palette();
        let data_dir = crate::data_dir();
        let data_dir_text = data_dir.display().to_string();
        let config = crate::google_signin::client_config_path().display().to_string();
        v_flex()
            .gap(px(22.))
            .child(
                h_flex()
                    .items_center()
                    .gap(px(14.))
                    .child(super::brand_tile(44.))
                    .child(
                        v_flex()
                            .child(div().font_family("Source Serif 4 Display").text_size(px(26.)).child("Overlook"))
                            .child(div().text_size(px(13.)).text_color(p.muted).child("A clear view of what matters. Thoughtfully made for Linux."))
                            .child(div().text_size(px(12.)).text_color(p.muted).child(format!("Version {} · by Last Refuge", env!("CARGO_PKG_VERSION")))),
                    ),
            )
            .child(section("Updates", None))
            .child(setting_row(
                &format!("Overlook {} · {}", crate::updater::CURRENT_VERSION, self.update.install.describe()),
                self.update.last_result.as_deref().unwrap_or("Not checked yet."),
                h_flex()
                    .gap(px(6.))
                    .child(Button::new("check-updates").small().icon(icon("refresh-cw").size(px(14.))).label("Check for updates").loading(self.update.phase == super::UpdatePhase::Checking).on_click(cx.listener(|this, _, window, cx| this.check_for_updates(true, window, cx))))
                    .when(self.update.latest.is_some() && matches!(self.update.phase, super::UpdatePhase::Idle | super::UpdatePhase::Failed(_)), |r| {
                        r.child(Button::new("install-update").primary().small().label("Update now").on_click(cx.listener(|this, _, window, cx| {
                            this.close_settings(window, cx);
                            this.start_update(window, cx);
                        })))
                    })
                    .into_any_element(),
            ))
            .child(setting_row(
                "Check for updates automatically",
                "Once a day, from the project’s GitHub releases. Nothing about you is sent.",
                Switch::new("pref-updates").checked(self.prefs.check_for_updates).on_click(cx.listener(|this, checked: &bool, _w, cx| {
                    this.prefs.check_for_updates = *checked;
                    this.save_prefs(cx);
                })).into_any_element(),
            ))
            .child(section("Links", None))
            .child(setting_row("Last Refuge", "", Button::new("about-site").ghost().small().icon(icon("external-link").size(px(14.))).label("lastrefuge.ai").on_click(|_, _, cx| cx.open_url("https://lastrefuge.ai/")).into_any_element()))
            .child(setting_row("Source", "", Button::new("about-repo").ghost().small().icon(icon("external-link").size(px(14.))).label("github.com/last-refuge/overlook").on_click(|_, _, cx| cx.open_url("https://github.com/last-refuge/overlook")).into_any_element()))
            .child(section("On this device", None))
            .child(setting_row("Mail, events, drafts, and attachments", &data_dir_text, Button::new("about-open-data").ghost().small().icon(icon("folder-open").size(px(14.))).label("Open folder").on_click(move |_, _, cx| cx.reveal_path(&data_dir)).into_any_element()))
            .child(setting_row(
                "Back up mail",
                if self.accounts.iter().any(|a| a.provider == ProviderKind::Pop3) {
                    "Copies the database, downloaded POP3 messages and attachments to a folder you choose. POP3 mail exists only on this device: back it up regularly."
                } else {
                    "Copies the database, downloaded messages and attachments to a folder you choose. To restore, stop Overlook and replace the data folder with the copy."
                },
                Button::new("about-backup").small().icon(icon("hard-drive-download").size(px(14.))).label("Back up mail…").loading(self.backing_up).disabled(self.backing_up).on_click(cx.listener(|this, _, window, cx| this.back_up_mail(window, cx))).into_any_element(),
            ))
            .child(setting_row("Google OAuth client", &config, div().into_any_element()))
            .child(setting_row("Sign-in tokens", "Stored in the system keyring (Secret Service), never in files.", div().into_any_element()))
            .child(section("Keyboard", None))
            .child(div().text_size(px(12.)).text_color(p.muted).line_height(px(20.)).child(
                "Ctrl N new message · Ctrl E search · Ctrl R reply · Ctrl Shift R reply all · Ctrl F forward · Ctrl Enter send · Ctrl U toggle read · J/K move · E archive or move to Inbox · Delete · S star · Ctrl 1 Mail · Ctrl 2 Calendar · Ctrl Shift A new event · Ctrl , settings · in settings: Ctrl F or F3 search · in the calendar: ← → T D W M A",
            ))
    }
}

fn hour_text(hour: u8) -> String {
    let (h12, suffix) = match hour {
        0 => (12, "AM"),
        1..=11 => (hour, "AM"),
        12 => (12, "PM"),
        _ => (hour - 12, "PM"),
    };
    format!("{h12}:00 {suffix}")
}

impl MailWindow {
    /// Settings → About → "Back up mail…": asks for a folder, then copies the profile there
    /// off the UI thread.
    fn back_up_mail(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.backing_up {
            return;
        }
        let receiver = cx.prompt_for_paths(PathPromptOptions { files: false, directories: true, multiple: false, prompt: Some("Back up here".into()) });
        let store = self.store.clone();
        let data_dir = crate::data_dir();
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = receiver.await else { return };
            let Some(target) = paths.into_iter().next() else { return };
            let _ = this.update_in(cx, |this, _window, cx| {
                this.backing_up = true;
                cx.notify();
            });
            let shown = target.clone();
            let result = cx.background_spawn(async move { crate::backup::write_backup(&store, &data_dir, &target) }).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.backing_up = false;
                match result {
                    Ok(files) => crate::ui::toast(window, cx, crate::notify::Level::Success, format!("Backed up {files} files to {}.", shown.display())),
                    Err(e) => crate::ui::toast(window, cx, crate::notify::Level::Error, format!("The backup did not finish: {e}")),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// "Add account" with one entry per account type.
    fn add_account_menu(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = cx.entity().downgrade();
        Button::new("settings-add-account")
            .small()
            .icon(icon("plus").size(px(14.)))
            .label("Add account")
            .dropdown_caret(true)
            .dropdown_menu(move |mut menu, _window, _cx| {
                let g = t.clone();
                menu = menu.item(PopupMenuItem::new("Gmail…").on_click(move |_, window, cx| {
                    let _ = g.update(cx, |this, cx| this.start_google_sign_in(None, window, cx));
                }));
                let i = t.clone();
                menu = menu.item(PopupMenuItem::new("IMAP…").on_click(move |_, window, cx| {
                    let _ = i.update(cx, |this, cx| this.open_account_setup(ProviderKind::Imap, super::SetupMode::New, window, cx));
                }));
                let p = t.clone();
                menu.item(PopupMenuItem::new("POP3…").on_click(move |_, window, cx| {
                    let _ = p.update(cx, |this, cx| this.open_account_setup(ProviderKind::Pop3, super::SetupMode::New, window, cx));
                }))
            })
            .into_any_element()
    }

    fn signature_picker(&self, id: &'static str, account: &AccountId, reply: bool, cx: &mut Context<Self>) -> AnyElement {
        let ap = self.account_prefs_for(account);
        let current = if reply { ap.default_reply.clone() } else { ap.default_new.clone() };
        let label = current.as_deref().and_then(|i| ap.signature_by_id(i)).map(|s| s.name.clone()).unwrap_or_else(|| "None".into());
        let choices: Vec<(Option<String>, String)> = std::iter::once((None, "None".to_string())).chain(ap.signatures.iter().map(|s| (Some(s.id.clone()), s.name.clone()))).collect();
        let t = cx.entity().downgrade();
        let account = account.clone();
        Button::new(id)
            .small()
            .label(label)
            .dropdown_caret(true)
            .dropdown_menu(move |mut menu, _window, _cx| {
                for (sid, name) in &choices {
                    let t = t.clone();
                    let sid = sid.clone();
                    let account = account.clone();
                    menu = menu.item(PopupMenuItem::new(name.clone()).checked(sid == current).on_click(move |_, _w, cx| {
                        let _ = t.update(cx, |this, cx| this.set_default_signature(&account, reply, sid.clone(), cx));
                    }));
                }
                menu
            })
            .into_any_element()
    }

    fn render_vacation(&self, state: &SettingsState, account: &Account, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let p = *cx.palette();
        let has_access = self.providers.get(&account.id).is_some_and(|p| p.mail.has_settings_access());
        if !has_access {
            let e = account.email.clone();
            let is_gmail = account.provider == ProviderKind::Gmail;
            return vec![setting_row(
                "Not available yet",
                if is_gmail { "Sign in again to let Overlook manage automatic replies for this account." } else { "This account has no automatic replies." },
                if is_gmail {
                    Button::new("vacation-signin").small().label("Sign in again").on_click(cx.listener(move |this, _, window, cx| this.start_google_sign_in(Some(e.clone()), window, cx))).into_any_element()
                } else {
                    div().into_any_element()
                },
            )
            .into_any_element()];
        }
        if state.vacation_loading {
            return vec![div().text_size(px(12.)).text_color(p.muted).child("Loading…").into_any_element()];
        }
        let Some(v) = state.vacation.clone() else {
            return vec![div().text_size(px(12.)).text_color(p.muted).child(state.vacation_note.clone().unwrap_or_else(|| "Automatic replies could not be loaded.".into())).into_any_element()];
        };
        let acct_id = account.id.clone();
        vec![
            setting_row(
                "Send automatic replies",
                "",
                Switch::new("vacation-on").checked(v.enabled).on_click(cx.listener(|this, checked: &bool, _w, cx| {
                    if let Some(v) = this.settings.as_mut().and_then(|s| s.vacation.as_mut()) {
                        v.enabled = *checked;
                    }
                    cx.notify();
                })).into_any_element(),
            )
            .into_any_element(),
            setting_row("Subject", "", Input::new(&state.vacation_subject).w(px(320.)).into_any_element()).into_any_element(),
            div().rounded(px(8.)).border_1().border_color(p.divider).h(px(120.)).child(Textarea::new(&state.vacation_body).h_full()).into_any_element(),
            h_flex()
                .gap(px(10.))
                .child(div().flex_1().child(Input::new(&state.vacation_start)))
                .child(div().flex_1().child(Input::new(&state.vacation_end)))
                .into_any_element(),
            setting_row(
                "Only reply to people in my contacts",
                "",
                Switch::new("vacation-contacts").checked(v.contacts_only).on_click(cx.listener(|this, checked: &bool, _w, cx| {
                    if let Some(v) = this.settings.as_mut().and_then(|s| s.vacation.as_mut()) {
                        v.contacts_only = *checked;
                    }
                    cx.notify();
                })).into_any_element(),
            )
            .into_any_element(),
            h_flex()
                .items_center()
                .gap(px(10.))
                .child(Button::new("vacation-save").primary().small().label("Save automatic replies").on_click(cx.listener(move |this, _, window, cx| this.save_vacation(acct_id.clone(), window, cx))))
                .children(state.vacation_note.clone().map(|n| div().text_size(px(12.)).text_color(p.muted).child(n)))
                .into_any_element(),
        ]
    }
}

fn section(title: &str, description: Option<&str>) -> Div {
    div().child(scroll_target(title, v_flex()
        .gap(px(2.))
        .pt(px(6.))
        .child(div().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).child(title.to_owned()))
        .children(description.map(|d| div().text_size(px(12.)).child(d.to_owned()).opacity(0.7)))))
}

fn setting_row(label: &str, description: &str, control: AnyElement) -> Div {
    div().child(scroll_target(label, h_flex()
        .items_center()
        .gap(px(24.))
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap(px(2.))
                .child(div().text_size(px(13.)).font_weight(FontWeight::MEDIUM).child(label.to_owned()))
                .when(!description.is_empty(), |d| d.child(div().text_size(px(12.)).opacity(0.7).child(description.to_owned()))),
        )
        .child(div().flex_none().child(control))))
}

/// Segmented choice control.
fn choice<T: Copy + PartialEq + 'static>(
    options: &[(&'static str, T)],
    current: T,
    cx: &mut Context<MailWindow>,
    on_select: impl Fn(&mut MailWindow, T, &mut Window, &mut Context<MailWindow>) + 'static,
) -> AnyElement {
    let p = *cx.palette();
    let on_select = std::rc::Rc::new(on_select);
    segmented(&p)
        .flex_wrap()
        .max_w_full()
        .children(options.iter().map(|(label, value)| {
            let value = *value;
            let on_select = on_select.clone();
            segment(SharedString::from(format!("choice-{label}")), *label, value == current, &p)
                .on_click(cx.listener(move |this, _, window, cx| on_select(this, value, window, cx)))
        }))
        .into_any_element()
}

// ---------------------------------------------------------------- search

/// One searchable setting. Keep in step with the render functions above: a row added
/// there should get an entry here so search can find it.
pub(crate) struct SettingEntry {
    pub page: SettingsPage,
    pub section: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    /// Extra words people might search for that the label does not contain.
    pub keywords: &'static str,
}

impl SettingEntry {
    /// The row or section text this entry scrolls to. Usually the label; a few entries
    /// name a setting whose row reads differently, or that has no row of its own.
    fn anchor(&self) -> &'static str {
        match self.label {
            "Add account" => "Your accounts",
            "Check for updates" => "Updates",
            "Keyboard shortcuts" => "Keyboard",
            label => label,
        }
    }
}

const fn entry(page: SettingsPage, section: &'static str, label: &'static str, description: &'static str, keywords: &'static str) -> SettingEntry {
    SettingEntry { page, section, label, description, keywords }
}

use SettingsPage::{About as AB, Accounts as AC, Calendar as CA, General as GE, Mail as MA};

pub(crate) static SETTINGS_INDEX: &[SettingEntry] = &[
    entry(AC, "Your accounts", "Add account", "Mail and calendar accounts on this device.", "gmail imap pop3 new"),
    entry(AC, "Account", "Your name", "Shown as the sender on messages you write.", "display name from sender"),
    entry(AC, "Account", "Status", "Sync state, sign-in and server settings.", "sync now sign in server settings connection password"),
    entry(AC, "Account", "Calendar server", "CalDAV calendars for this account.", "caldav calendar"),
    entry(AC, "Signatures", "Signatures", "Added below your message, above anything you are replying to.", "signature html picture image"),
    entry(AC, "Signatures", "Use for new messages", "Default signature for new messages.", "signature default"),
    entry(AC, "Signatures", "Use for replies and forwards", "Default signature for replies and forwards.", "signature default reply forward"),
    entry(AC, "Automatic replies", "Send automatic replies", "An out-of-office reply sent for you while it is on.", "vacation out of office away autoresponder"),
    entry(AC, "Automatic replies", "Only reply to people in my contacts", "", "vacation out of office"),
    entry(AC, "Contacts", "Contacts", "For recipient suggestions and sender cards.", "address book sync contacts google"),
    entry(AC, "Sign out", "Sign out of this account", "Deletes the stored sign-in.", "log out logout revoke keyring"),
    entry(AC, "Remove", "Remove this account from this device", "Deletes downloaded mail, events, drafts, and stored sign-in.", "delete account"),
    entry(GE, "Appearance", "Theme", "Follow system picks Light or Dark from your desktop setting.", "dark light mode appearance colors omarchy"),
    entry(GE, "Appearance", "Density", "Compact fits more messages on screen.", "compact comfortable spacing"),
    entry(GE, "Appearance", "Reading pane", "On the right or below the list.", "layout preview pane bottom right"),
    entry(GE, "Start-up and time", "Time zone", "Follow the system or choose a zone.", "timezone tz city"),
    entry(GE, "Start-up and time", "Open Overlook in", "Mail, Calendar, or the last used module.", "startup launch start"),
    entry(GE, "Start-up and time", "Time format", "12-hour or 24-hour clock.", "clock 24h 12h am pm"),
    entry(GE, "Start-up and time", "Week starts on", "Monday or Sunday.", "first day week"),
    entry(GE, "Notifications", "Show in Omarchy notifications", "Messages also appear in Omarchy’s notification center.", "omarchy notification center"),
    entry(GE, "Notifications", "New mail", "Desktop notification when new mail arrives.", "notifications alert focused"),
    entry(GE, "Notifications", "Event reminders", "Desktop notification before events start.", "notifications alert reminder"),
    entry(GE, "Synchronization", "Check for changes every", "How often Overlook looks for new mail and events.", "sync interval refresh poll frequency"),
    entry(MA, "Layout", "Focused Inbox", "Sort the Inbox into Focused and Other.", "other inbox sort"),
    entry(MA, "Layout", "Sender pictures", "Show initials next to each conversation.", "avatars initials photos"),
    entry(MA, "Layout", "Message preview text", "Show the first line of each conversation in the list.", "snippet preview"),
    entry(MA, "Reading", "Mark as read", "When a conversation is opened in the reading pane.", "unread read"),
    entry(MA, "Reading", "Remote content", "Remote images and fonts can reveal when you open a message.", "images privacy tracking block load"),
    entry(MA, "Compose and reply", "Undo send", "A sent message waits this long in the Outbox before it leaves.", "delay outbox cancel"),
    entry(MA, "Compose and reply", "Default reply", "What the reply box and Ctrl R do.", "reply all"),
    entry(MA, "Compose and reply", "Quote the original message in replies", "", "quote quoting reply"),
    entry(MA, "Attachments", "Save attachments to", "Folder offered first when you save an attachment.", "downloads folder directory files"),
    entry(MA, "Download", "Inbox messages to download", "Newest messages fetched for each account.", "sync history count offline"),
    entry(CA, "Working hours", "Working hours", "Hours outside this range are shaded in Day and Week views.", "work start end business"),
    entry(CA, "Events", "Default event length", "", "duration meeting minutes"),
    entry(CA, "Events", "Default reminder", "Used when an event or calendar sets no reminder of its own.", "alert notification alarm"),
    entry(CA, "Events", "Show declined events", "", "declined rsvp hide"),
    entry(CA, "Events", "Event reminders", "Desktop notification before events start.", "notifications alert"),
    entry(AB, "Updates", "Check for updates", "From the project’s GitHub releases.", "update version upgrade automatic"),
    entry(AB, "Links", "Source", "github.com/last-refuge/overlook", "github repository code website"),
    entry(AB, "On this device", "Mail, events, drafts, and attachments", "Open the data folder.", "data folder storage location"),
    entry(AB, "On this device", "Back up mail", "Copies the database and messages to a folder you choose.", "backup export restore"),
    entry(AB, "On this device", "Google OAuth client", "", "oauth client config google"),
    entry(AB, "On this device", "Sign-in tokens", "Stored in the system keyring.", "keyring secret service tokens"),
    entry(AB, "Keyboard", "Keyboard shortcuts", "", "keys shortcuts hotkeys keybindings"),
];

/// Settings whose label, section, page, description or keywords contain every word of
/// `query` (case-insensitive). Label matches come first; otherwise index order is kept.
pub(crate) fn search_settings(query: &str) -> Vec<&'static SettingEntry> {
    let terms: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    if terms.is_empty() { return Vec::new(); }
    let mut hits: Vec<(bool, &'static SettingEntry)> = SETTINGS_INDEX
        .iter()
        .filter_map(|e| {
            let haystack = format!("{} {} {} {} {}", e.label, e.section, e.page.label(), e.description, e.keywords).to_lowercase();
            terms.iter().all(|t| haystack.contains(t.as_str())).then(|| {
                let label = e.label.to_lowercase();
                (terms.iter().all(|t| label.contains(t.as_str())), e)
            })
        })
        .collect();
    hits.sort_by_key(|(in_label, _)| !*in_label);
    hits.into_iter().map(|(_, e)| e).collect()
}

#[cfg(test)]
mod search_tests {
    use super::{search_settings, SettingsPage};

    #[test]
    fn empty_query_finds_nothing() {
        assert!(search_settings("").is_empty());
        assert!(search_settings("   ").is_empty());
    }

    #[test]
    fn matches_label_case_insensitively() {
        let hits = search_settings("THEME");
        assert_eq!(hits.first().map(|e| e.label), Some("Theme"));
    }

    #[test]
    fn matches_keywords() {
        let hits = search_settings("vacation");
        assert!(hits.iter().any(|e| e.label == "Send automatic replies"));
        assert!(search_settings("dark").iter().any(|e| e.label == "Theme"));
    }

    #[test]
    fn every_word_must_match() {
        assert!(search_settings("signature reply").iter().all(|e| e.page == SettingsPage::Accounts));
        assert!(search_settings("theme zzzz").is_empty());
    }

    #[test]
    fn label_matches_rank_first() {
        let hits = search_settings("reminder");
        assert!(hits[0].label.to_lowercase().contains("reminder"));
    }
}
