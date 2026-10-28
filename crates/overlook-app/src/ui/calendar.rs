//! Calendar module: week, day, and agenda views, event details with responses, and a
//! simple new-event form. Everything reads from the local store; changes are written with
//! their pending operation and reach Google through the sync engine.

use super::widgets::*;
use super::MailWindow;
use crate::assets::icon;
use crate::theme::ActivePalette;
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputState, Textarea, TextareaState};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::text::TextView;
use gpui_kit::component::{Disableable, Sizable, WindowExt};
use gpui_kit::prelude::*;
use gpui_kit::*;
use overlook_core::sanitize::{sanitize_html, text_to_html, RemoteContent};
use overlook_core::time::now;
use overlook_core::tz;
use overlook_core::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Module {
    Mail,
    Calendar,
    Contacts,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CalView {
    Day,
    Week,
    Month,
    Agenda,
}

const HOUR_PX: f32 = 52.;
const GUTTER_PX: f32 = 56.;
const AGENDA_DAYS: i32 = 14;

pub struct NewEvent {
    pub title: Entity<InputState>,
    pub date: Entity<InputState>,
    pub end_date: Entity<InputState>,
    pub start: Entity<InputState>,
    pub end: Entity<InputState>,
    pub location: Entity<InputState>,
    pub description: Entity<TextareaState>,
    /// Guest addresses, comma separated.
    pub guests: Entity<InputState>,
    pub add_meet: bool,
    /// Free/busy of each guest for the chosen time, after "Check availability".
    pub availability: Vec<(String, Option<bool>)>,
    pub checking: bool,
    pub calendar: Option<CalendarId>,
    pub all_day: bool,
    pub error: Option<String>,
    /// `Some` when editing an existing event.
    pub editing: Option<EventId>,
    /// Preserve the version the editor opened, so a refresh cannot silently rebase it.
    pub original: Option<Event>,
    /// For a repeating event: which occurrences the edit applies to.
    pub scope: RecurrenceScope,
}

/// The question asked before editing or deleting one occurrence of a repeating event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScopePrompt {
    pub event_id: EventId,
    pub action: ScopeAction,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScopeAction {
    Edit,
    Delete,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DragMode {
    Move,
    Resize,
}

/// An event block being dragged in the day or week grid.
#[derive(Clone, Debug)]
pub struct CalDrag {
    pub event_id: EventId,
    pub mode: DragMode,
    pub origin_y: f32,
    pub origin_day: i32,
    pub day_offset: i32,
    pub dy: f32,
    pub start: i64,
    pub end: i64,
    pub moved: bool,
}

impl CalDrag {
    /// The times the drag currently proposes, snapped to 15 minutes.
    pub fn proposed(&self) -> (i64, i64) {
        let minutes = ((self.dy / HOUR_PX * 60.) / 15.).round() as i64 * 15;
        let day_shift = if self.day_offset == 0 { 0 } else {
            let date = tz::local_date(tz::add_days(self.start, self.day_offset));
            tz::local_datetime(date, tz::zoned(self.start).time()) - self.start
        };
        let shift = minutes * 60_000 + day_shift;
        match self.mode {
            DragMode::Move => (self.start + shift, self.end + shift),
            DragMode::Resize => (self.start, (self.end + minutes * 60_000).max(self.start + 15 * 60_000)),
        }
    }
}

fn clock_text(ts: i64) -> String {
    tz::format_clock(ts)
}

fn hex_to_hsla(hex: &str) -> Option<Hsla> {
    let v = u32::from_str_radix(hex.trim_start_matches('#'), 16).ok()?;
    Some(Rgba::from(gpui_kit::rgb(v)).into())
}

impl MailWindow {
    // ------------------------------------------------------------------ state

    pub(crate) fn cal_range(&self) -> (i64, i64) {
        match self.cal_view {
            CalView::Day => (self.cal_anchor, tz::add_days(self.cal_anchor, 1)),
            CalView::Week => {
                let start = tz::week_start(self.cal_anchor, self.prefs.week_starts_monday);
                (start, tz::add_days(start, 7))
            }
            CalView::Month => {
                let start = tz::week_start(tz::month_start(self.cal_anchor), self.prefs.week_starts_monday);
                (start, tz::add_days(start, 42))
            }
            CalView::Agenda => (self.cal_anchor, tz::add_days(self.cal_anchor, AGENDA_DAYS)),
        }
    }

    pub(crate) fn reload_calendar(&mut self) {
        self.calendars = self.store.calendars(&self.account.id).unwrap_or_default();
        let (from, to) = self.cal_range();
        self.cal_events = self.store.events_between(&self.account.id, from, to).unwrap_or_default();
        if !self.prefs.show_declined_events {
            self.cal_events.retain(|e| e.my_response() != Some(ResponseStatus::Declined));
        }
        self.cal_status = crate::sync::calendar_status(&self.store, &self.account.id);
        self.caldav_connection = self.store.calendar_connection(&self.account.id).ok().flatten();
        self.caldav_cached = self.caldav_connection.as_ref().is_some_and(|c| self.store.caldav_range_cached(&c.id, from, to, &format!("{:?}", tz::system_zone())).unwrap_or(false));
        self.caldav_last_sync = self.caldav_connection.as_ref().and_then(|c| self.store.caldav_last_sync(&c.id).ok().flatten());
        self.caldav_changes = self.store.caldav_changes(&self.account.id).unwrap_or_default();
        self.caldav_previews = self.caldav_changes.iter().filter_map(|change| {
            change.remote.as_ref().map(|remote| (change.id.clone(), overlook_provider::caldav::resource_preview(&remote.icalendar)))
        }).collect();
        // Date-only and floating projections depend on the local zone. Preserve them on
        // disk, but don't present an old-zone projection as the current calendar.
        if self.caldav_connection.as_ref().is_some_and(|c| !self.store.caldav_timezone_matches(&c.id, &format!("{:?}", tz::system_zone())).unwrap_or(false)) {
            self.cal_events.clear();
        }
        if let Some(sel) = &self.cal_selected {
            if self.store.event(sel).ok().flatten().is_none() {
                self.cal_selected = None;
            }
        }
    }

    pub(crate) fn set_module(&mut self, module: Module, window: &mut Window, cx: &mut Context<Self>) {
        if self.module == module {
            self.focus_current_module(window, cx);
            return;
        }
        self.module = module;
        self.save_setting("last_module", &match module { Module::Calendar => "calendar", Module::Contacts => "contacts", Module::Mail => "mail" });
        match module {
            Module::Calendar => {
                self.reload_calendar();
                self.ensure_caldav_range(cx);
                self.cal_focus.focus(window, cx);
            }
            Module::Contacts => {
                self.reload_contacts();
                let input = self.contact_search.clone();
                input.update(cx, |s, cx| s.focus(window, cx));
            }
            Module::Mail => self.list_focus.focus(window, cx),
        }
        cx.notify();
    }

    pub(crate) fn focus_current_module(&self, window: &mut Window, cx: &mut Context<Self>) {
        match self.module {
            Module::Calendar => self.cal_focus.focus(window, cx),
            Module::Contacts => self.contact_search.update(cx, |s, cx| s.focus(window, cx)),
            Module::Mail => self.list_focus.focus(window, cx),
        }
    }

    pub(crate) fn set_cal_view(&mut self, view: CalView, cx: &mut Context<Self>) {
        self.cal_view = view;
        self.reload_calendar();
        self.ensure_caldav_range(cx);
        cx.notify();
    }

    pub(crate) fn cal_step(&mut self, direction: i32, cx: &mut Context<Self>) {
        self.cal_anchor = match self.cal_view {
            CalView::Day => tz::add_days(self.cal_anchor, direction),
            CalView::Week => tz::add_days(self.cal_anchor, direction * 7),
            CalView::Month => tz::add_months(tz::month_start(self.cal_anchor), direction),
            CalView::Agenda => tz::add_days(self.cal_anchor, direction * AGENDA_DAYS),
        };
        self.reload_calendar();
        self.ensure_caldav_range(cx);
        cx.notify();
    }

    pub(crate) fn cal_today(&mut self, cx: &mut Context<Self>) {
        self.cal_anchor = tz::day_start(now());
        self.reload_calendar();
        self.ensure_caldav_range(cx);
        cx.notify();
    }

    pub(crate) fn toggle_calendar_visible(&mut self, id: CalendarId, cx: &mut Context<Self>) {
        if let Some(c) = self.calendars.iter().find(|c| c.id == id) {
            let _ = self.store.set_calendar_visible(&c.account_id, &id, !c.visible);
        }
        self.reload_calendar();
        cx.notify();
    }

    fn selected_event(&self) -> Option<&Event> {
        let id = self.cal_selected.as_ref()?;
        self.cal_events.iter().find(|e| &e.id == id)
    }

    fn calendar_of(&self, event: &Event) -> Option<&Calendar> {
        self.calendars.iter().find(|c| c.id == event.calendar_id)
    }

    fn can_edit_calendar_event(&self, event: &Event) -> bool {
        self.calendar_of(event).is_some_and(|c| event.editable_by(&self.account.email,c))
            && (!event.calendar_id.0.starts_with("caldav:") || event.attendees.is_empty())
    }

    /// Switches to the calendar, moves to the event's day, and selects it.
    pub(crate) fn show_event_in_calendar(&mut self, id: EventId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(event) = self.store.event(&id).ok().flatten() else { return };
        self.set_module(Module::Calendar, window, cx);
        self.cal_anchor = tz::day_start(event.start);
        if self.cal_view == CalView::Agenda {
            self.cal_view = CalView::Week;
        }
        self.new_event = None;
        self.reload_calendar();
        self.ensure_caldav_range(cx);
        self.cal_selected = Some(id);
        cx.notify();
    }

    /// Fires due reminders: a desktop notification plus an in-app toast, once per event
    /// and offset per session.
    pub(crate) fn check_reminders(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.reminders_enabled {
            return;
        }
        let now_ms = now();
        let horizon = now_ms + 2 * 3_600_000;
        let accounts = self.accounts.clone();
        for account in accounts {
            if self.store.calendar_connection(&account.id).ok().flatten().is_some_and(|c| !self.store.caldav_timezone_matches(&c.id, &format!("{:?}", tz::system_zone())).unwrap_or(false)) { continue; }
            let calendars = self.store.calendars(&account.id).unwrap_or_default();
            let Ok(events) = self.store.events_starting_between(&account.id, now_ms - 60_000, horizon) else { continue };
            for event in events {
                if event.my_response() == Some(ResponseStatus::Declined) || event.is_working_location() {
                    continue;
                }
                // A snoozed reminder comes back once, when its time is up.
                if let Some(until) = self.snoozed_reminders.get(&event.id).copied() {
                    if now_ms < until {
                        continue;
                    }
                    self.snoozed_reminders.remove(&event.id);
                    if event.start > now_ms - 60_000 {
                        self.fire_reminder(&event, now_ms, window, cx);
                    }
                    continue;
                }
                // Google reports no default for some calendars; 10 minutes is Google's own
                // default and close to Outlook's 15, and reminders can be switched off.
                let fallback = self.prefs.default_reminder_minutes as i32;
                let defaults: Vec<i32> = calendars.iter().find(|c| c.id == event.calendar_id).map(|c| c.default_reminders.clone()).filter(|d| !d.is_empty()).unwrap_or_else(|| if fallback > 0 { vec![fallback] } else { Vec::new() });
                let offsets: Vec<i32> = event.effective_reminders(&defaults).to_vec();
                for minutes in offsets {
                    let fire_at = event.start - i64::from(minutes) * 60_000;
                    let key = (event.id.clone(), minutes);
                    if fire_at <= now_ms && event.start > now_ms - 60_000 && !self.notified.contains(&key) {
                        self.notified.insert(key);
                        self.fire_reminder(&event, now_ms, window, cx);
                    }
                }
            }
        }
    }

    /// Shows one reminder: a desktop notification and an in-app toast with Snooze.
    fn fire_reminder(&mut self, event: &Event, now_ms: i64, window: &mut Window, cx: &mut Context<Self>) {
        let remaining = ((event.start - now_ms) / 60_000).max(0);
        let when = if remaining == 0 { "now".to_owned() } else { format!("in {remaining} min") };
        let title = event.display_title();
        let mut body = format!("{} · {} – {}", when, tz::format_clock(event.start), tz::format_clock(event.end));
        if let Some(l) = &event.location {
            body.push_str(&format!("\n{l}"));
        }
        crate::notify::desktop(crate::notify::Level::Reminder, title.clone(), body);
        let t = cx.entity().downgrade();
        let id = event.id.clone();
        window.push_notification(
            Notification::info(format!("{title} · {when}")).action(move |_, _window, cx| {
                let t = t.clone();
                let id = id.clone();
                let toast = cx.entity().downgrade();
                Button::new("snooze-reminder").small().label("Snooze 5 min").on_click(move |_, window, cx| {
                    let _ = t.update(cx, |this, _cx| {
                        this.snoozed_reminders.insert(id.clone(), now() + 5 * 60_000);
                    });
                    let _ = toast.update(cx, |n, cx| n.dismiss(window, cx));
                })
            }),
            cx,
        );
    }

    pub(crate) fn start_edit_event(&mut self, id: EventId, window: &mut Window, cx: &mut Context<Self>) {
        self.start_edit_event_scoped(id, RecurrenceScope::This, window, cx);
    }

    /// Edit or delete on a repeating event asks for the scope first.
    pub(crate) fn request_edit_event(&mut self, id: EventId, window: &mut Window, cx: &mut Context<Self>) {
        if self.store.event(&id).ok().flatten().is_some_and(|e| !self.can_edit_calendar_event(&e)) { return; }
        match self.store.event(&id).ok().flatten() {
            Some(e) if e.is_recurring_instance() => {
                self.cal_scope_prompt = Some(ScopePrompt { event_id: id, action: ScopeAction::Edit });
                cx.notify();
            }
            _ => self.start_edit_event(id, window, cx),
        }
    }

    pub(crate) fn request_delete_event(&mut self, id: EventId, window: &mut Window, cx: &mut Context<Self>) {
        if self.store.event(&id).ok().flatten().is_some_and(|e| !self.can_edit_calendar_event(&e)) { return; }
        match self.store.event(&id).ok().flatten() {
            Some(e) if e.is_recurring_instance() => {
                self.cal_scope_prompt = Some(ScopePrompt { event_id: id, action: ScopeAction::Delete });
                cx.notify();
            }
            _ => self.delete_event_scoped(id, RecurrenceScope::This, window, cx),
        }
    }

    pub(crate) fn answer_scope_prompt(&mut self, scope: RecurrenceScope, window: &mut Window, cx: &mut Context<Self>) {
        let Some(prompt) = self.cal_scope_prompt.take() else { return };
        match prompt.action {
            ScopeAction::Edit => self.start_edit_event_scoped(prompt.event_id, scope, window, cx),
            ScopeAction::Delete => self.delete_event_scoped(prompt.event_id, scope, window, cx),
        }
    }

    pub(crate) fn start_edit_event_scoped(&mut self, id: EventId, scope: RecurrenceScope, window: &mut Window, cx: &mut Context<Self>) {
        let Some(event) = self.store.event(&id).ok().flatten() else { return };
        if !self.can_edit_calendar_event(&event) { return; }
        let guest_text = event.attendees.iter().filter(|a| !a.is_self).map(|a| a.email.clone()).collect::<Vec<_>>().join(", ");
        let guests = cx.new(|cx| InputState::new(window, cx).placeholder("Guests, comma separated").default_value(guest_text));
        let add_meet = event.conference_url.is_some() || event.wants_conference;
        let title = cx.new(|cx| InputState::new(window, cx).placeholder("Add a title").default_value(event.summary.clone()));
        let date = cx.new(|cx| InputState::new(window, cx).placeholder("YYYY-MM-DD").default_value(tz::local_date(event.start).to_string()));
        let end_date = cx.new(|cx| InputState::new(window,cx).placeholder("YYYY-MM-DD").default_value(tz::local_date(if event.all_day {tz::add_days(event.end,-1)} else {event.end}).to_string()));
        let start = cx.new(|cx| InputState::new(window, cx).placeholder("9:00 AM").default_value(clock_text(event.start)));
        let end = cx.new(|cx| InputState::new(window, cx).placeholder("10:00 AM").default_value(clock_text(if event.all_day { event.start + 3_600_000 } else { event.end })));
        let location = cx.new(|cx| InputState::new(window, cx).placeholder("Add a location").default_value(event.location.clone().unwrap_or_default()));
        let description = cx.new(|cx| TextareaState::new(window, cx).placeholder("Add a description").default_value(event.description.clone().unwrap_or_default()));
        title.update(cx, |s, cx| s.focus(window, cx));
        self.new_event = Some(NewEvent { title, date, end_date, start, end, location, description, guests, add_meet, availability: Vec::new(), checking: false, calendar: Some(event.calendar_id.clone()), all_day: event.all_day, error: None, editing: Some(id), original: Some(event), scope });
        cx.notify();
    }

    /// Asks the provider whether each guest is free at the time in the form.
    pub(crate) fn check_availability(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(form) = self.new_event.as_ref() else { return };
        let emails: Vec<String> = Address::parse_list(&form.guests.read(cx).value()).into_iter().map(|a| a.email).collect();
        let Some((start, end)) = self.form_times(form, cx) else {
            if let Some(f) = self.new_event.as_mut() {
                f.error = Some("Enter the date and times first.".into());
            }
            cx.notify();
            return;
        };
        let Some(provider) = self.providers.get(&self.account.id).and_then(|p| p.calendar.clone()) else { return };
        if emails.is_empty() {
            return;
        }
        if let Some(f) = self.new_event.as_mut() {
            f.checking = true;
            f.error = None;
        }
        cx.notify();
        let account = self.account.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx.background_spawn(async move { provider.free_busy(&account, &emails, start, end).await }).await;
            let _ = this.update(cx, |this, cx| {
                if let Some(f) = this.new_event.as_mut() {
                    f.checking = false;
                    match result {
                        Ok(list) => f.availability = list,
                        Err(e) => f.error = Some(format!("Couldn’t check availability: {e}")),
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Start and end from the form's date and clock fields.
    fn form_times(&self, form: &NewEvent, cx: &App) -> Option<(i64, i64)> {
        let date = tz::parse_date(form.date.read(cx).value().trim())?;
        let end_date = tz::parse_date(form.end_date.read(cx).value().trim())?;
        if form.all_day {
            let s = tz::date_start(date);
            let end = tz::add_days(tz::date_start(end_date),1);
            return (end>s).then_some((s,end));
        }
        let st = tz::parse_clock(form.start.read(cx).value().trim())?;
        let et = tz::parse_clock(form.end.read(cx).value().trim())?;
        let (s, e) = (tz::local_datetime(date, st), tz::local_datetime(end_date, et));
        (e > s).then_some((s, e))
    }

    pub(crate) fn respond_to_event(&mut self, id: EventId, response: ResponseStatus, window: &mut Window, cx: &mut Context<Self>) {
        let Some(mut event) = self.store.event(&id).ok().flatten() else { return };
        if event.id.0.starts_with("caldav:") { return; }
        let mut found = false;
        for a in &mut event.attendees {
            if a.is_self {
                a.response = response;
                found = true;
            }
        }
        if !found {
            event.attendees.push(Attendee { email: self.account.email.clone(), name: Some(self.account.display_name.clone()), response, is_self: true, is_organizer: false, optional: false });
        }
        match self.store.apply_event_change(&self.account.id, &event, OperationKind::RespondToEvent { event_id: id, response }) {
            Ok(_) => {
                self.reload_calendar();
                cx.notify();
                if self.connected(&self.account.id) {
                    self.sync_now(window, cx);
                }
            }
            Err(e) => crate::ui::toast(window, cx, crate::notify::Level::Error, format!("Couldn’t save your response: {e}")),
        }
    }

    pub(crate) fn delete_event_scoped(&mut self, id: EventId, scope: RecurrenceScope, window: &mut Window, cx: &mut Context<Self>) {
        let Some(event) = self.store.event(&id).ok().flatten() else { return };
        if event.calendar_id.0.starts_with("caldav:") {
            match self.save_caldav_edit(Some(event),None,scope,cx) {
                Ok(_) => { self.cal_selected=None; crate::ui::toast(window,cx,crate::notify::Level::Info,"Deletion saved on this device. It will sync when connected."); },
                Err(e) => crate::ui::toast(window,cx,crate::notify::Level::Error,e),
            }
            cx.notify(); return;
        }
        match self.store.delete_event_locally_scoped(&self.account.id, &event, scope) {
            Ok(_) => {
                self.cal_selected = None;
                self.reload_calendar();
                cx.notify();
                let what = match scope {
                    RecurrenceScope::This => format!("Deleted “{}”.", event.display_title()),
                    RecurrenceScope::Following => format!("Deleted “{}” and the following occurrences.", event.display_title()),
                    RecurrenceScope::All => format!("Deleted every occurrence of “{}”.", event.display_title()),
                };
                crate::ui::toast(window, cx, crate::notify::Level::Info, what);
                if self.connected(&self.account.id) {
                    self.sync_now(window, cx);
                }
            }
            Err(e) => crate::ui::toast(window, cx, crate::notify::Level::Error, format!("Couldn’t delete the event: {e}")),
        }
    }

    pub(crate) fn start_new_event(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let editable: Vec<&Calendar> = self.calendars.iter().filter(|c| c.can_edit).collect();
        if editable.is_empty() {
            crate::ui::toast(window, cx, crate::notify::Level::Warning, "No calendar you can write to is connected yet.");
            return;
        }
        let calendar = self.account_prefs_for(&self.account.id).preferred_calendar(&self.calendars).map(|c| c.id.clone());
        let day = if self.cal_view == CalView::Week { tz::day_start(now()).max(self.cal_range().0).min(tz::add_days(self.cal_range().1, -1)) } else { self.cal_anchor };
        let date_text = tz::local_date(day).to_string();
        let next_hour = {
            let z = tz::zoned(now());
            let h = (z.hour() as i32 + 1).min(23);
            format!("{}:00 {}", if h % 12 == 0 { 12 } else { h % 12 }, if h < 12 { "AM" } else { "PM" })
        };
        let end_hour = {
            let z = tz::zoned(now());
            let start_minutes = ((z.hour() as i32 + 1).min(23)) * 60;
            let end_minutes = (start_minutes + self.prefs.default_event_minutes as i32).min(23 * 60 + 59);
            let (h, m) = (end_minutes / 60, end_minutes % 60);
            format!("{}:{:02} {}", if h % 12 == 0 { 12 } else { h % 12 }, m, if h < 12 { "AM" } else { "PM" })
        };
        let title = cx.new(|cx| InputState::new(window, cx).placeholder("Add a title"));
        let date = cx.new(|cx| InputState::new(window, cx).placeholder("YYYY-MM-DD").default_value(date_text.clone()));
        let end_date = cx.new(|cx| InputState::new(window,cx).placeholder("YYYY-MM-DD").default_value(date_text));
        let start = cx.new(|cx| InputState::new(window, cx).placeholder("9:00 AM").default_value(next_hour));
        let end = cx.new(|cx| InputState::new(window, cx).placeholder("10:00 AM").default_value(end_hour));
        let location = cx.new(|cx| InputState::new(window, cx).placeholder("Add a location"));
        let description = cx.new(|cx| TextareaState::new(window, cx).placeholder("Add a description"));
        let guests = cx.new(|cx| InputState::new(window, cx).placeholder("Guests, comma separated"));
        title.update(cx, |s, cx| s.focus(window, cx));
        self.cal_selected = None;
        self.cal_scope_prompt = None;
        self.new_event = Some(NewEvent { title, date, end_date, start, end, location, description, guests, add_meet: false, availability: Vec::new(), checking: false, calendar, all_day: false, error: None, editing: None, original: None, scope: RecurrenceScope::This });
        cx.notify();
    }

    pub(crate) fn cancel_new_event(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.new_event = None;
        self.cal_focus.focus(window, cx);
        cx.notify();
    }

    pub(crate) fn save_new_event(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(form) = self.new_event.as_ref() else { return };
        let title = form.title.read(cx).value().trim().to_owned();
        let date_text = form.date.read(cx).value().trim().to_owned();
        let end_date_text = form.end_date.read(cx).value().trim().to_owned();
        let start_text = form.start.read(cx).value().trim().to_owned();
        let end_text = form.end.read(cx).value().trim().to_owned();
        let location = form.location.read(cx).value().trim().to_owned();
        let description = form.description.read(cx).value().trim().to_owned();
        let guest_addresses = Address::parse_list(&form.guests.read(cx).value());
        let add_meet = form.add_meet;
        let scope = form.scope;
        let all_day = form.all_day;
        let editing = form.editing.clone();
        let original = form.original.clone();
        let calendar = form.calendar.clone().and_then(|id| self.calendars.iter().find(|c| c.id == id).cloned());
        let fail = |msg: &str, this: &mut Self, cx: &mut Context<Self>| {
            if let Some(f) = this.new_event.as_mut() {
                f.error = Some(msg.to_owned());
            }
            cx.notify();
        };
        let Some(calendar) = calendar else { return fail("Choose a calendar.", self, cx) };
        if !calendar.can_edit { return fail("This calendar is available for viewing only.", self, cx); }
        let Some(date) = tz::parse_date(&date_text) else { return fail("Enter the date as YYYY-MM-DD.", self, cx) };
        let Some(end_date) = tz::parse_date(&end_date_text) else {return fail("Enter the end date as YYYY-MM-DD.",self,cx)};
        let (start, end) = if all_day {
            let s = tz::date_start(date);
            let end = tz::add_days(tz::date_start(end_date),1);
            if end<=s {return fail("The end date must not precede the start date.",self,cx);}
            (s,end)
        } else {
            let Some(st) = tz::parse_clock(&start_text) else { return fail("Enter a start time like 9:30 AM.", self, cx) };
            let Some(et) = tz::parse_clock(&end_text) else { return fail("Enter an end time like 10:00 AM.", self, cx) };
            let s = tz::local_datetime(date, st);
            let e = tz::local_datetime(end_date, et);
            if e <= s {
                return fail("The end time must be after the start time.", self, cx);
            }
            (s, e)
        };
        if let Some(existing_id) = editing {
            let Some(mut existing) = original.clone().or_else(||self.store.event(&existing_id).ok().flatten()) else { return fail("This event no longer exists.", self, cx) };
            existing.summary = if title.is_empty() { "(No title)".into() } else { title };
            existing.location = if location.is_empty() { None } else { Some(location) };
            existing.description = if description.is_empty() { None } else { Some(description) };
            existing.start = start;
            existing.end = end;
            existing.all_day = all_day;
            if !calendar.id.0.starts_with("caldav:") {
                existing.attendees = merge_guests(&existing.attendees, &guest_addresses, &self.owner());
                existing.wants_conference = add_meet;
            }
            existing.updated = now();
            if calendar.id.0.starts_with("caldav:") {
                match self.save_caldav_edit(original,Some(existing),scope,cx) {
                    Ok(id)=>{self.new_event=None;self.cal_anchor=tz::day_start(start);self.reload_calendar();self.cal_selected=Some(id);self.cal_focus.focus(window,cx);cx.notify();crate::ui::toast(window,cx,crate::notify::Level::Info,"Saved on this device. Changes will sync when connected.");},
                    Err(e)=>fail(&e,self,cx),
                }
                return;
            }
            match self.store.apply_series_change(&self.account.id, &existing, scope) {
                Ok(_) => {
                    self.new_event = None;
                    self.cal_anchor = tz::day_start(start);
                    self.reload_calendar();
                    self.cal_selected = Some(existing_id);
                    self.cal_focus.focus(window, cx);
                    cx.notify();
                    crate::ui::toast(window, cx, crate::notify::Level::Info, "Saved on this device. Syncing to your calendar…");
                    if self.connected(&self.account.id) {
                        self.sync_now(window, cx);
                    }
                }
                Err(e) => fail(&format!("Couldn’t save the event: {e}"), self, cx),
            }
            return;
        }
        let pid = new_provider_event_id();
        let event = Event {
            id: EventId(format!("{}:{pid}", calendar.id.0)),
            account_id: self.account.id.clone(),
            calendar_id: calendar.id.clone(),
            provider_id: Some(ProviderId(pid)),
            ical_uid: None,
            summary: if title.is_empty() { "(No title)".into() } else { title },
            description: if description.is_empty() { None } else { Some(description) },
            location: if location.is_empty() { None } else { Some(location) },
            start,
            end,
            all_day,
            status: EventStatus::Confirmed,
            organizer: Some(self.owner()),
            attendees: merge_guests(&[], &guest_addresses, &self.owner()),
            html_link: None,
            conference_url: None,
            recurring_event_id: None,
            updated: now(),
            event_type: None,
            color: None,
            reminders: None,
            original_start: None,
            wants_conference: add_meet,
            pending: true,
        };
        if calendar.id.0.starts_with("caldav:") {
            match self.save_caldav_edit(None,Some(event),scope,cx) {
                Ok(id)=>{self.new_event=None;self.cal_anchor=tz::day_start(start);self.reload_calendar();self.cal_selected=Some(id);self.cal_focus.focus(window,cx);cx.notify();crate::ui::toast(window,cx,crate::notify::Level::Info,"Saved on this device. Changes will sync when connected.");},
                Err(e)=>fail(&e,self,cx),
            }
            return;
        }
        match self.store.apply_event_change(&self.account.id, &event, OperationKind::CreateEvent { event_id: event.id.clone() }) {
            Ok(_) => {
                self.new_event = None;
                self.cal_anchor = tz::day_start(start);
                self.reload_calendar();
                self.cal_selected = Some(event.id.clone());
                self.cal_focus.focus(window, cx);
                cx.notify();
                crate::ui::toast(window, cx, crate::notify::Level::Info, "Saved on this device. Syncing to your calendar…");
                if self.connected(&self.account.id) {
                    self.sync_now(window, cx);
                }
            }
            Err(e) => fail(&format!("Couldn’t save the event: {e}"), self, cx),
        }
    }



    // --------------------------------------------------------------- sidebar

    pub(crate) fn render_calendar_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = cx.palette().navigation_palette();
        let (dot, text) = match &self.cal_status {
            SyncState::UpToDate => (p.success, "Up to date".to_owned()),
            SyncState::Syncing => (p.accent, "Syncing…".to_owned()),
            SyncState::Paused(_) => (p.accent, "Paused".to_owned()),
            SyncState::Offline => (p.muted, "Offline".to_owned()),
            SyncState::SignInRequired => (p.warning, "Sign-in required".to_owned()),
            SyncState::SyncFailed(r) => (p.warning, format!("Sync failed: {r}")),
        };
        let mut list = v_flex().gap(px(2.));
        for c in &self.calendars {
            let id = c.id.clone();
            let color = c.color.as_deref().and_then(hex_to_hsla).unwrap_or(p.accent);
            list = list.child(
                h_flex()
                    .id(SharedString::from(format!("cal-{}", c.id)))
                    .items_center()
                    .gap(px(10.))
                    .min_h(px(32.))
                    .px(px(11.))
                    .rounded(px(7.))
                    .cursor_pointer()
                    .text_size(px(13.))
                    .text_color(if c.visible { p.foreground } else { p.muted })
                    .hover(|d| d.bg(p.hover))
                    .child(
                        div()
                            .flex_none()
                            .size(px(14.))
                            .rounded(px(4.))
                            .border_2()
                            .border_color(color)
                            .when(c.visible, |d| d.bg(color))
                            .flex()
                            .items_center()
                            .justify_center()
                            .when(c.visible, |d| d.child(icon("check").size(px(10.)).text_color(gpui_kit::white()))),
                    )
                    .child(div().flex_1().truncate().child(c.name.clone()))
                    .on_click(cx.listener(move |this, _, _w, cx| this.toggle_calendar_visible(id.clone(), cx))),
            );
        }
        let owner = self.owner();
        v_flex()
            .size_full()
            .bg(p.sidebar)
            .text_color(p.foreground)
            .border_r_1()
            .border_color(p.divider)
            .pt(px(18.))
            .px(px(11.))
            .pb(px(14.))
            .child(
                Button::new("new-event")
                    .disabled(!self.calendars.iter().any(|c| c.can_edit))
                    .primary()
                    .icon(icon("calendar-plus").size(px(16.)))
                    .label("New event")
                    .tooltip("New event (Ctrl Shift A)")
                    .w_full()
                    .h(px(38.))
                    .on_click(cx.listener(|this, _, window, cx| this.start_new_event(window, cx))),
            )
            .child(div().mt(px(18.)).mb(px(6.)).px(px(11.)).text_size(px(11.)).font_weight(FontWeight::MEDIUM).text_color(p.muted).child("My calendars"))
            .child(v_flex().id("calendar-list").flex_1().min_h_0().child(list).overflow_y_scrollbar())
            .child(
                h_flex()
                    .items_center()
                    .gap(px(9.))
                    .px(px(6.))
                    .pt(px(16.))
                    .child(owner_avatar(&owner, 30., &p))
                    .child(
                        v_flex()
                            .min_w_0()
                            .child(div().text_size(px(13.)).font_weight(FontWeight::MEDIUM).truncate().child(owner.display_name()))
                            .child(
                                h_flex()
                                    .items_center()
                                    .gap(px(5.))
                                    .text_size(px(11.))
                                    .text_color(p.muted)
                                    .child(div().size(px(6.)).rounded_full().bg(dot))
                                    .child(format!("Calendar · {text}")),
                            ),
                    ),
            )
    }

    // ------------------------------------------------------------------ main

    pub(crate) fn render_calendar_main(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = *cx.palette();
        let (from, to) = self.cal_range();
        let title = match self.cal_view {
            CalView::Day => tz::zoned(self.cal_anchor).strftime("%A, %B %-d, %Y").to_string(),
            CalView::Week => tz::format_week_range(from),
            CalView::Month => tz::format_month_year(self.cal_anchor),
            CalView::Agenda => format!("{} – {}", tz::format_day(from), tz::format_day(tz::add_days(to, -1))),
        };
        let view_button = |label: &'static str, view: CalView, cx: &mut Context<Self>| {
            segment(SharedString::from(format!("view-{label}")), label, self.cal_view == view, &p)
                .on_click(cx.listener(move |this, _, _w, cx| this.set_cal_view(view, cx)))
        };
        let toolbar = h_flex()
            .items_center()
            .gap(px(6.))
            .h(px(52.))
            .px(px(16.))
            .border_b_1()
            .border_color(p.divider)
            .child(Button::new("cal-today").small().label("Today").on_click(cx.listener(|this, _, _w, cx| this.cal_today(cx))))
            .child(Button::new("cal-prev").ghost().small().icon(icon("chevron-left").size(px(16.))).tooltip("Previous (←)").on_click(cx.listener(|this, _, _w, cx| this.cal_step(-1, cx))))
            .child(Button::new("cal-next").ghost().small().icon(icon("chevron-right").size(px(16.))).tooltip("Next (→)").on_click(cx.listener(|this, _, _w, cx| this.cal_step(1, cx))))
            .child(div().ml(px(8.)).text_size(px(18.)).font_weight(FontWeight::SEMIBOLD).child(title))
            .child(div().flex_1())
            .child(
                segmented(&p)
                    .child(view_button("Day", CalView::Day, cx))
                    .child(view_button("Week", CalView::Week, cx))
                    .child(view_button("Month", CalView::Month, cx))
                    .child(view_button("Agenda", CalView::Agenda, cx)),
            );

        let banner = match &self.cal_status {
            SyncState::SignInRequired if self.account.provider == ProviderKind::Gmail => {
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
                        .child(icon("key-round").size(px(16.)).text_color(p.warning))
                        .child(div().flex_1().child(crate::sync::CALENDAR_SIGNIN_WORDING))
                        .child(Button::new("cal-signin").small().label("Sign in again").on_click(cx.listener(move |this, _, window, cx| this.start_google_sign_in(Some(email.clone()), window, cx)))),
                )
            }
            _ if matches!(self.account.provider, ProviderKind::Imap | ProviderKind::Pop3)
                && (self.caldav_connection.is_none()
                    || !self.caldav_cached
                    || !self.caldav_changes.is_empty()
                    || !matches!(self.cal_status, SyncState::UpToDate | SyncState::Syncing)) => Some(
                h_flex()
                    .items_center()
                    .gap(px(10.))
                    .px(px(16.))
                    .min_h(px(40.))
                    .py(px(8.))
                    .bg(p.chip)
                    .border_b_1()
                    .border_color(p.divider)
                    .text_size(px(13.))
                    .text_color(p.muted)
                    .child(icon("info").size(px(16.)).text_color(p.muted))
                    .child(div().flex_1().child(if self.caldav_connection.is_none() {
                        "Connect Fastmail or another calendar service to see your events here.".to_owned()
                    } else {
                        match &self.cal_status {
                            SyncState::Syncing => "Syncing calendar changes… Saved events remain available.".to_owned(),
                            SyncState::SignInRequired => "Calendar sign-in needs attention. Check the app password and calendar permission in settings.".to_owned(),
                            SyncState::SyncFailed(error) | SyncState::Paused(error) => error.clone(),
                            _ if !self.caldav_cached => "This date range has not been downloaded for your current time zone. Connect to sync it; saved events may be incomplete.".to_owned(),
                            SyncState::Offline => "Offline · Changes are saved on this device and will sync when you reconnect.".to_owned(),
                            _ => "Changes are saved on this device and waiting to sync.".to_owned(),
                        }
                    }))
                    .when(!self.caldav_changes.is_empty(), |d| d.child(Button::new("caldav-review").small().label(format!("Review {} {}",self.caldav_changes.len(),if self.caldav_changes.len()==1 {"change"} else {"changes"})).on_click(cx.listener(|this,_,_,cx|{this.show_caldav_changes = !this.show_caldav_changes;cx.notify();}))))
                    .child(Button::new("caldav-settings").small().label(if self.caldav_connection.is_some() { "Calendar settings" } else { "Connect calendars" })
                        .on_click(cx.listener(|this, _, window, cx| this.open_settings(super::settings::SettingsPage::Accounts, window, cx))))
                    .when(self.caldav_connection.is_some(), |d| d.child(Button::new("caldav-refresh").small().ghost().label("Sync now")
                        .on_click(cx.listener(|this, _, _, cx| this.sync_caldav_account(this.account.id.clone(), true, cx))))),
            ),
            _ if self.account.provider == ProviderKind::Sample && self.cal_status != SyncState::UpToDate => Some(
                h_flex()
                    .items_center()
                    .gap(px(10.))
                    .px(px(16.))
                    .h(px(40.))
                    .bg(p.chip)
                    .border_b_1()
                    .border_color(p.divider)
                    .text_size(px(13.))
                    .text_color(p.muted)
                    .child(icon("info").size(px(16.)).text_color(p.muted))
                    .child("Sample calendar with fictional events. Connect the sample account (status bar switch) to load them."),
            ),
            _ => None,
        };

        let grid: AnyElement = match self.cal_view {
            CalView::Day => self.render_time_grid(from, 1, cx).into_any_element(),
            CalView::Week => self.render_time_grid(from, 7, cx).into_any_element(),
            CalView::Month => self.render_month_grid(from, cx).into_any_element(),
            CalView::Agenda => self.render_agenda(from, cx).into_any_element(),
        };

        let detail: Option<AnyElement> = if self.new_event.is_some() {
            Some(self.render_new_event(cx).into_any_element())
        } else {
            self.selected_event().map(|e| self.render_event_detail(e, cx).into_any_element())
        };

        v_flex()
            .id("calendar-main")
            .key_context(super::CALENDAR_CONTEXT)
            .track_focus(&self.cal_focus)
            .size_full()
            .bg(p.surface)
            .child(toolbar)
            .children(banner)
            .when(self.show_caldav_changes, |d|d.child(self.render_caldav_changes(cx)))
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .child(div().flex_1().min_w_0().h_full().child(grid))
                    .children(detail.map(|d| div().w(px(360.)).flex_none().h_full().border_l_1().border_color(p.divider).child(d))),
            )
    }

    fn render_time_grid(&self, from: i64, days: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let p = *cx.palette();
        let today = tz::day_start(now());
        let day_starts: Vec<i64> = (0..days as i32).map(|d| tz::add_days(from, d)).collect();

        // Scroll to the working hours the first time a view/period is shown.
        let key = (self.cal_view, from);
        if self.cal_scrolled_for.get() != Some(key) {
            self.cal_scroll.set_offset(point(px(0.), px(-7.5 * HOUR_PX)));
            self.cal_scrolled_for.set(Some(key));
        }

        let header = h_flex()
            .pl(px(GUTTER_PX))
            .border_b_1()
            .border_color(p.divider)
            .children(day_starts.iter().map(|&d| {
                let z = tz::zoned(d);
                let is_today = d == today;
                let next = tz::add_days(d, 1);
                let location: Option<String> = self.cal_events.iter().find(|e| e.is_working_location() && e.overlaps(d, next)).map(|e| e.display_title());
                v_flex()
                    .flex_1()
                    .items_center()
                    .py(px(8.))
                    .gap(px(2.))
                    .child(div().text_size(px(11.)).text_color(if is_today { p.accent } else { p.muted }).child(z.strftime("%a").to_string().to_uppercase()))
                    .child(
                        div()
                            .size(px(28.))
                            .rounded_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_size(px(15.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .when(is_today, |d| d.bg(p.action).text_color(p.on_action))
                            .child(z.day().to_string()),
                    )
                    .children(location.map(|l| {
                        h_flex()
                            .items_center()
                            .gap(px(3.))
                            .text_size(px(10.))
                            .text_color(p.muted)
                            .child(icon("map-pin").size(px(10.)).text_color(p.muted))
                            .child(l)
                    }))
            }));

        // All-day and multi-day events.
        let all_day: Vec<&Event> = self.cal_events.iter().filter(|e| !e.is_working_location() && (e.all_day || e.end - e.start >= 86_400_000)).collect();
        let all_day_row = (!all_day.is_empty()).then(|| {
            h_flex()
                .pl(px(GUTTER_PX))
                .min_h(px(28.))
                .border_b_1()
                .border_color(p.divider)
                .children(day_starts.iter().map(|&d| {
                    let next = tz::add_days(d, 1);
                    let here: Vec<&Event> = all_day.iter().copied().filter(|e| e.overlaps(d, next)).collect();
                    v_flex().flex_1().min_w_0().gap(px(2.)).p(px(2.)).children(here.into_iter().map(|e| self.event_chip(e, cx)))
                }))
        });

        let hours = v_flex().w(px(GUTTER_PX)).flex_none().relative().h(px(24. * HOUR_PX)).children((1..24).map(|h| {
            div()
                .absolute()
                .top(px(h as f32 * HOUR_PX - 7.))
                .right(px(8.))
                .text_size(px(10.))
                .text_color(p.muted)
                .child(format!("{} {}", if h % 12 == 0 { 12 } else { h % 12 }, if h < 12 { "AM" } else { "PM" }))
        }));

        let now_ms = now();
        // While a block is dragged, every column sees it at the proposed time.
        let previewed: Vec<Event> = self
            .cal_events
            .iter()
            .cloned()
            .map(|mut e| {
                if let Some(d) = self.cal_drag.as_ref().filter(|d| d.event_id == e.id && d.moved) {
                    let (s, en) = d.proposed();
                    e.start = s;
                    e.end = en;
                }
                e
            })
            .collect();
        let columns = day_starts.iter().enumerate().map(|(ci, &d)| {
            let next = tz::add_days(d, 1);
            let timed: Vec<&Event> = previewed.iter().filter(|e| !e.all_day && e.end - e.start < 86_400_000 && e.overlaps(d, next)).collect();
            let lanes = assign_lanes(&timed);
            let mut col = div()
                .id(SharedString::from(format!("cal-col-{ci}")))
                .flex_1()
                .min_w_0()
                .relative()
                .h(px(24. * HOUR_PX))
                .border_l_1()
                .border_color(p.divider)
                .on_mouse_move(cx.listener(move |this, e: &MouseMoveEvent, _w, cx| {
                    if let Some(d) = this.cal_drag.as_mut() {
                        if e.dragging() {
                            d.dy = f32::from(e.position.y) - d.origin_y;
                            d.day_offset = ci as i32 - d.origin_day;
                            d.moved = true;
                            cx.notify();
                        }
                    }
                }));
            let (ws, we) = (self.prefs.work_start_hour.min(24) as f32, self.prefs.work_end_hour.min(24) as f32);
            if ws > 0. {
                col = col.child(div().absolute().top_0().left_0().right_0().h(px(ws * HOUR_PX)).bg(p.background));
            }
            if we < 24. {
                col = col.child(div().absolute().top(px(we * HOUR_PX)).left_0().right_0().h(px((24. - we) * HOUR_PX)).bg(p.background));
            }
            for h in 0..24 {
                col = col.child(div().absolute().top(px(h as f32 * HOUR_PX)).left_0().right_0().h(px(1.)).bg(p.divider));
            }
            for (e, (lane, lanes_total)) in timed.iter().zip(lanes) {
                let start_min = if e.start < d { 0 } else { tz::minutes_into_day(e.start) } as f32;
                let end_min = if e.end > next { 24. * 60. } else { tz::minutes_into_day(e.end) as f32 }.max(start_min + 20.);
                let top = start_min / 60. * HOUR_PX;
                let height = (end_min - start_min) / 60. * HOUR_PX;
                let width = 1.0 / lanes_total as f32;
                col = col.child(
                    div()
                        .absolute()
                        .top(px(top))
                        .h(px(height - 2.))
                        .left(relative(lane as f32 * width))
                        .w(relative(width))
                        .pr(px(4.))
                        .child(self.event_block(e, height, ci as i32, cx)),
                );
            }
            if d == today {
                let m = tz::minutes_into_day(now_ms) as f32;
                col = col.child(div().absolute().top(px(m / 60. * HOUR_PX - 1.)).left_0().right_0().h(px(2.)).bg(p.brand_accent));
                col = col.child(div().absolute().top(px(m / 60. * HOUR_PX - 4.)).left(px(-4.)).size(px(8.)).rounded_full().bg(p.brand_accent));
            }
            col
        }).collect::<Vec<_>>();

        v_flex()
            .size_full()
            .child(header)
            .children(all_day_row)
            .child(
                div()
                    .id("cal-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.cal_scroll)
                    .on_mouse_up(MouseButton::Left, cx.listener(|this, _, window, cx| this.finish_cal_drag(window, cx)))
                    .on_mouse_up_out(MouseButton::Left, cx.listener(|this, _, window, cx| this.finish_cal_drag(window, cx)))
                    .child(h_flex().items_start().child(hours).children(columns)),
            )
    }

    /// Ends a drag: a real move or resize is saved; a click that did not move is ignored.
    pub(crate) fn finish_cal_drag(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(drag) = self.cal_drag.take() else { return };
        cx.notify();
        if !drag.moved {
            return;
        }
        let (start, end) = drag.proposed();
        if start == drag.start && end == drag.end {
            return;
        }
        let Some(mut event) = self.store.event(&drag.event_id).ok().flatten() else { return };
        let original = event.clone();
        event.start = start;
        event.end = end;
        event.updated = now();
        if event.calendar_id.0.starts_with("caldav:") {
            if original.start!=drag.start || original.end!=drag.end {crate::ui::toast(window,cx,crate::notify::Level::Warning,"The event changed during the drag. Try again.");return;}
            if let Err(e)=self.save_caldav_edit(Some(original),Some(event),RecurrenceScope::This,cx){crate::ui::toast(window,cx,crate::notify::Level::Error,e);}
            cx.notify();return;
        }
        match self.store.apply_event_change(&self.account.id, &event, OperationKind::UpdateEvent { event_id: event.id.clone(), scope: RecurrenceScope::This }) {
            Ok(_) => {
                self.reload_calendar();
                self.cal_selected = Some(event.id.clone());
                let what = if drag.mode == DragMode::Resize {
                    format!("“{}” now ends at {}.", event.display_title(), tz::format_clock(end))
                } else {
                    format!("Moved “{}” to {} {}.", event.display_title(), tz::format_day(start), tz::format_clock(start))
                };
                crate::ui::toast(window, cx, crate::notify::Level::Info, what);
                if self.connected(&self.account.id) {
                    self.sync_now(window, cx);
                }
            }
            Err(e) => crate::ui::toast(window, cx, crate::notify::Level::Error, format!("Couldn’t move the event: {e}")),
        }
    }

    fn event_colors(&self, e: &Event, p: &crate::theme::Palette) -> (Hsla, Hsla) {
        let base = e
            .color
            .as_deref()
            .and_then(hex_to_hsla)
            .or_else(|| self.calendar_of(e).and_then(|c| c.color.as_deref()).and_then(hex_to_hsla))
            .unwrap_or(p.accent);
        let mut fill = base;
        fill.a = if p.is_dark() { 0.28 } else { 0.18 };
        let mut edge = base;
        if p.is_dark() {
            edge.l = edge.l.max(0.6);
        } else {
            edge.l = edge.l.min(0.42);
        }
        (fill, edge)
    }

    fn event_block(&self, e: &Event, height: f32, day_index: i32, cx: &mut Context<Self>) -> impl IntoElement {
        let p = *cx.palette();
        let (fill, edge) = self.event_colors(e, &p);
        let selected = self.cal_selected.as_ref() == Some(&e.id);
        let dragging = self.cal_drag.as_ref().is_some_and(|d| d.event_id == e.id && d.moved);
        let declined = e.my_response() == Some(ResponseStatus::Declined);
        let tentative = matches!(e.my_response(), Some(ResponseStatus::NeedsAction) | Some(ResponseStatus::Tentative)) && e.is_invitation();
        let can_edit = self.can_edit_calendar_event(e);
        let id = e.id.clone();
        let (start_ms, end_ms) = (e.start, e.end);
        let begin_drag = move |mode: DragMode, id: EventId, y: f32| CalDrag { event_id: id, mode, origin_y: y, origin_day: day_index, day_offset: 0, dy: 0., start: start_ms, end: end_ms, moved: false };
        let move_id = id.clone();
        let resize_id = id.clone();
        div()
            .id(SharedString::from(format!("ev-{}", e.id)))
            .relative()
            .size_full()
            .rounded(px(6.))
            .when(dragging, |d| d.shadow_md().opacity(0.9))
            .when(can_edit, |d| {
                d.on_mouse_down(MouseButton::Left, cx.listener(move |this, ev: &MouseDownEvent, _w, _cx| {
                    this.cal_drag = Some(begin_drag(DragMode::Move, move_id.clone(), f32::from(ev.position.y)));
                }))
                .child(
                    div()
                        .id(SharedString::from(format!("ev-resize-{}", e.id)))
                        .absolute()
                        .bottom_0()
                        .left_0()
                        .right_0()
                        .h(px(7.))
                        .cursor_ns_resize()
                        .on_mouse_down(MouseButton::Left, cx.listener(move |this, ev: &MouseDownEvent, _w, cx| {
                            cx.stop_propagation();
                            this.cal_drag = Some(begin_drag(DragMode::Resize, resize_id.clone(), f32::from(ev.position.y)));
                        })),
                )
            })
            .bg(fill)
            .border_l_2()
            .border_color(edge)
            .when(selected, |d| d.border_1().border_color(edge).bg(fill.opacity(1.0)))
            .when(tentative, |d| d.border_dashed())
            .when(declined, |d| d.opacity(0.5))
            .px(px(6.))
            .py(px(3.))
            .overflow_hidden()
            .cursor_pointer()
            .text_size(px(12.))
            .text_color(p.foreground)
            .child(div().font_weight(FontWeight::SEMIBOLD).truncate().child(e.display_title()))
            .when(height >= 36., |d| d.child(div().text_size(px(11.)).text_color(p.muted).truncate().child(format!("{} – {}", tz::format_clock(e.start), tz::format_clock(e.end)))))
            .when(height >= 54. && e.location.is_some(), |d| d.child(div().text_size(px(11.)).text_color(p.muted).truncate().child(e.location.clone().unwrap_or_default())))
            .on_click(cx.listener(move |this, _, _w, cx| {
                this.new_event = None;
                this.cal_selected = Some(id.clone());
                cx.notify();
            }))
    }

    fn event_chip(&self, e: &Event, cx: &mut Context<Self>) -> impl IntoElement {
        let p = *cx.palette();
        let (fill, edge) = self.event_colors(e, &p);
        let id = e.id.clone();
        div()
            .id(SharedString::from(format!("chip-{}", e.id)))
            .px(px(6.))
            .py(px(2.))
            .rounded(px(5.))
            .bg(fill)
            .border_l_2()
            .border_color(edge)
            .text_size(px(11.))
            .truncate()
            .cursor_pointer()
            .child(e.display_title())
            .on_click(cx.listener(move |this, _, _w, cx| {
                this.new_event = None;
                this.cal_selected = Some(id.clone());
                cx.notify();
            }))
    }

    fn render_month_grid(&self, from: i64, cx: &mut Context<Self>) -> impl IntoElement {
        let p = *cx.palette();
        let today = tz::day_start(now());
        let month = tz::local_date(self.cal_anchor).month();
        let names: [&str; 7] = if self.prefs.week_starts_monday { ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"] } else { ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"] };
        let header = h_flex().border_b_1().border_color(p.divider).children(names.iter().map(|d| {
            div().flex_1().py(px(6.)).text_size(px(11.)).font_weight(FontWeight::MEDIUM).text_color(p.muted).child(div().px(px(8.)).child(d.to_uppercase()))
        }));
        let mut rows = v_flex().flex_1().min_h_0();
        for week in 0..6 {
            let mut row = h_flex().flex_1().min_h_0().border_b_1().border_color(p.divider);
            for dow in 0..7 {
                let day = tz::add_days(from, week * 7 + dow);
                let next = tz::add_days(day, 1);
                let z = tz::zoned(day);
                let in_month = z.month() == month;
                let is_today = day == today;
                let mut events: Vec<&Event> = self.cal_events.iter().filter(|e| !e.is_working_location() && e.overlaps(day, next)).collect();
                events.sort_by_key(|e| (!e.all_day, e.start));
                let shown = events.iter().take(3).copied().collect::<Vec<_>>();
                let more = events.len().saturating_sub(shown.len());
                let day_for_click = day;
                row = row.child(
                    v_flex()
                        .id(SharedString::from(format!("month-{day}")))
                        .flex_1()
                        .min_w_0()
                        .min_h(px(96.))
                        .p(px(4.))
                        .gap(px(2.))
                        .border_l_1()
                        .border_color(p.divider)
                        .when(!in_month, |d| d.bg(p.background))
                        .child(
                            h_flex().justify_end().child(
                                div()
                                    .id(SharedString::from(format!("month-day-{day}")))
                                    .size(px(24.))
                                    .rounded_full()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .text_size(px(12.))
                                    .cursor_pointer()
                                    .text_color(if in_month { p.foreground } else { p.muted })
                                    .when(is_today, |d| d.bg(p.action).text_color(p.on_action).font_weight(FontWeight::SEMIBOLD))
                                    .hover(|d| d.bg(p.hover))
                                    .child(z.day().to_string())
                                    .on_click(cx.listener(move |this, _, _w, cx| {
                                        this.cal_anchor = day_for_click;
                                        this.set_cal_view(CalView::Day, cx);
                                    })),
                            ),
                        )
                        .children(shown.into_iter().map(|e| {
                            let (fill, edge) = self.event_colors(e, &p);
                            let id = e.id.clone();
                            let label = if e.all_day { e.display_title() } else { format!("{} {}", tz::format_clock(e.start), e.display_title()) };
                            div()
                                .id(SharedString::from(format!("mev-{}-{day}", e.id)))
                                .px(px(5.))
                                .py(px(1.))
                                .rounded(px(4.))
                                .bg(fill)
                                .border_l_2()
                                .border_color(edge)
                                .text_size(px(11.))
                                .truncate()
                                .cursor_pointer()
                                .when(self.cal_selected.as_ref() == Some(&e.id), |d| d.bg(fill.opacity(1.0)))
                                .child(label)
                                .on_click(cx.listener(move |this, _, _w, cx| {
                                    this.new_event = None;
                                    this.cal_selected = Some(id.clone());
                                    cx.notify();
                                }))
                        }))
                        .when(more > 0, |d| {
                            d.child(
                                div()
                                    .id(SharedString::from(format!("more-{day}")))
                                    .px(px(5.))
                                    .text_size(px(11.))
                                    .text_color(p.muted)
                                    .cursor_pointer()
                                    .child(format!("+{more} more"))
                                    .on_click(cx.listener(move |this, _, _w, cx| {
                                        this.cal_anchor = day_for_click;
                                        this.set_cal_view(CalView::Day, cx);
                                    })),
                            )
                        }),
                );
            }
            rows = rows.child(row);
        }
        v_flex().id("month-grid").size_full().child(header).child(rows).overflow_y_scrollbar()
    }

    fn render_agenda(&self, from: i64, cx: &mut Context<Self>) -> impl IntoElement {
        let p = *cx.palette();
        let today = tz::day_start(now());
        let mut list = v_flex().id("agenda").px(px(20.)).py(px(12.)).gap(px(4.));
        let mut any = false;
        for d in 0..AGENDA_DAYS {
            let day = tz::add_days(from, d);
            let next = tz::add_days(day, 1);
            let events: Vec<&Event> = self.cal_events.iter().filter(|e| !e.is_working_location() && e.overlaps(day, next)).collect();
            if events.is_empty() {
                continue;
            }
            any = true;
            list = list.child(
                h_flex()
                    .items_center()
                    .gap(px(8.))
                    .pt(px(14.))
                    .pb(px(4.))
                    .text_size(px(12.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(if day == today { p.accent } else { p.muted })
                    .child(tz::zoned(day).strftime("%A, %B %-d").to_string())
                    .when(day == today, |r| r.child(chip("Today", &p))),
            );
            for e in events {
                let (fill, edge) = self.event_colors(e, &p);
                let id = e.id.clone();
                let selected = self.cal_selected.as_ref() == Some(&e.id);
                let time = if e.all_day { "All day".to_owned() } else { format!("{} – {}", tz::format_clock(e.start), tz::format_clock(e.end)) };
                list = list.child(
                    h_flex()
                        .id(SharedString::from(format!("ag-{}", e.id)))
                        .items_center()
                        .gap(px(12.))
                        .px(px(10.))
                        .py(px(8.))
                        .rounded(px(7.))
                        .cursor_pointer()
                        .when(selected, |r| r.bg(p.selection))
                        .when(!selected, |r| r.hover(|r| r.bg(p.hover)))
                        .child(div().w(px(4.)).h(px(32.)).rounded(px(2.)).bg(edge))
                        .child(div().w(px(130.)).text_size(px(12.)).text_color(p.muted).child(time))
                        .child(v_flex().flex_1().min_w_0().child(div().text_size(px(13.)).font_weight(FontWeight::MEDIUM).truncate().child(e.display_title())).children(e.location.clone().map(|l| div().text_size(px(11.)).text_color(p.muted).truncate().child(l))))
                        .when(e.pending, |r| r.child(icon("cloud-off").size(px(13.)).text_color(p.warning)))
                        .child(div().size(px(10.)).rounded_full().bg(fill.opacity(1.0)))
                        .on_click(cx.listener(move |this, _, _w, cx| {
                            this.new_event = None;
                            this.cal_selected = Some(id.clone());
                            cx.notify();
                        })),
                );
            }
        }
        if !any {
            list = list.child(v_flex().items_center().pt(px(72.)).gap(px(6.)).text_color(p.muted).child(icon("calendar").size(px(28.)).text_color(p.divider)).child(div().text_size(px(14.)).font_weight(FontWeight::MEDIUM).text_color(p.foreground).child("Nothing scheduled")).child(div().text_size(px(12.)).child("The next two weeks are clear.")));
        }
        list.overflow_y_scrollbar()
    }

    fn render_event_detail(&self, e: &Event, cx: &mut Context<Self>) -> impl IntoElement {
        let p = *cx.palette();
        let (fill, edge) = self.event_colors(e, &p);
        let calendar = self.calendar_of(e);
        let when = if e.all_day {
            let single_day = tz::local_date(e.end) <= tz::local_date(tz::add_days(e.start, 1));
            if single_day { format!("{} · All day", tz::format_day(e.start)) } else { format!("{} – {} · All day", tz::format_day(e.start), tz::format_day(tz::add_days(e.end, -1))) }
        } else if tz::day_start(e.start) == tz::day_start(e.end) || e.end == tz::add_days(tz::day_start(e.start), 1) {
            format!("{} · {} – {}", tz::zoned(e.start).strftime("%A, %B %-d"), tz::format_clock(e.start), tz::format_clock(e.end))
        } else {
            format!("{} {} – {} {}", tz::format_day(e.start), tz::format_clock(e.start), tz::format_day(e.end), tz::format_clock(e.end))
        };
        let my = e.my_response();
        let id = e.id.clone();
        let can_edit = self.can_edit_calendar_event(e);
        let can_delete = can_edit;
        let fallback = self.prefs.default_reminder_minutes as i32;
        let defaults: Vec<i32> = calendar.map(|c| c.default_reminders.clone()).filter(|d| !d.is_empty()).unwrap_or_else(|| if fallback > 0 { vec![fallback] } else { Vec::new() });
        let reminders: Vec<i32> = e.effective_reminders(&defaults).to_vec();
        let reminder_text = if reminders.is_empty() || e.all_day {
            None
        } else {
            Some(reminders.iter().map(|m| match *m {
                0 => "at start".to_owned(),
                m if m % 1440 == 0 => format!("{} day{} before", m / 1440, if m == 1440 { "" } else { "s" }),
                m if m % 60 == 0 => format!("{} hour{} before", m / 60, if m == 60 { "" } else { "s" }),
                m => format!("{m} minutes before"),
            }).collect::<Vec<_>>().join(", "))
        };
        let response_button = |label: &'static str, response: ResponseStatus, icon_name: &'static str, cx: &mut Context<Self>| {
            let id = id.clone();
            Button::new(SharedString::from(format!("rsvp-{label}")))
                .small()
                .icon(icon(icon_name).size(px(14.)))
                .label(label)
                .toggled(my == Some(response))
                .when(my == Some(response), |b| b.primary())
                .on_click(cx.listener(move |this, _, window, cx| this.respond_to_event(id.clone(), response, window, cx)))
        };
        let description_html = e.description.as_deref().map(|d| if d.contains('<') { sanitize_html(d, RemoteContent::Blocked).html } else { text_to_html(d) });

        let prompt = self.cal_scope_prompt.clone().filter(|p| p.event_id == e.id);
        let scope_card = prompt.map(|prompt| {
            let verb = match prompt.action { ScopeAction::Edit => "Edit", ScopeAction::Delete => "Delete" };
            let choice = |label: &'static str, scope: RecurrenceScope, cx: &mut Context<Self>| {
                Button::new(SharedString::from(format!("scope-{label}")))
                    .small()
                    .label(label)
                    .when(scope == RecurrenceScope::This, |b| b.primary())
                    .on_click(cx.listener(move |this, _, window, cx| this.answer_scope_prompt(scope, window, cx)))
            };
            v_flex()
                .gap(px(8.))
                .p(px(12.))
                .rounded(px(8.))
                .bg(p.chip)
                .border_1()
                .border_color(p.divider)
                .child(div().text_size(px(13.)).font_weight(FontWeight::MEDIUM).child(format!("This event repeats. {verb}:")))
                .child(
                    h_flex()
                        .flex_wrap()
                        .gap(px(6.))
                        .child(choice("This event", RecurrenceScope::This, cx))
                        .when(!e.calendar_id.0.starts_with("caldav:"),|d|d.child(choice("This and following", RecurrenceScope::Following, cx)))
                        .when(!e.calendar_id.0.starts_with("caldav:") || matches!(prompt.action,ScopeAction::Delete),|d|d.child(choice("All events", RecurrenceScope::All, cx)))
                        .child(Button::new("scope-cancel").ghost().small().label("Cancel").on_click(cx.listener(|this, _, _w, cx| {
                            this.cal_scope_prompt = None;
                            cx.notify();
                        }))),
                )
        });
        v_flex()
            .id("event-detail")
            .size_full()
            .p(px(20.))
            .children(scope_card)
            .gap(px(14.))
            .child(
                h_flex()
                    .items_start()
                    .gap(px(10.))
                    .child(div().mt(px(5.)).size(px(12.)).rounded(px(3.)).bg(fill.opacity(1.0)).border_1().border_color(edge))
                    .child(div().flex_1().text_size(px(20.)).line_height(px(26.)).font_weight(FontWeight::SEMIBOLD).child(e.display_title()))
                    .child(Button::new("close-detail").ghost().xsmall().icon(icon("x").size(px(14.))).on_click(cx.listener(|this, _, _w, cx| {
                        this.cal_selected = None;
                        cx.notify();
                    }))),
            )
            .child(v_flex().gap(px(6.)).text_size(px(13.))
                .child(h_flex().gap(px(8.)).items_center().child(icon("clock").size(px(15.)).text_color(p.muted)).child(when))
                .children(e.location.clone().map(|l| h_flex().gap(px(8.)).items_center().child(icon("map-pin").size(px(15.)).text_color(p.muted)).child(div().flex_1().child(l))))
                .children(calendar.map(|c| h_flex().gap(px(8.)).items_center().child(icon("calendar").size(px(15.)).text_color(p.muted)).child(c.name.clone())))
                .children(reminder_text.map(|t| h_flex().gap(px(8.)).items_center().child(icon("bell").size(px(15.)).text_color(p.muted)).child(format!("Reminder {t}"))))
                .when(e.pending, |d| d.child(h_flex().gap(px(8.)).items_center().text_color(p.warning).child(icon("cloud-off").size(px(15.)).text_color(p.warning)).child("Saved on this device, waiting to sync"))))
            .when(e.is_invitation() && !e.id.0.starts_with("caldav:"), |d| {
                d.child(
                    v_flex()
                        .gap(px(8.))
                        .child(div().text_size(px(12.)).font_weight(FontWeight::MEDIUM).text_color(p.muted).child(match my {
                            Some(ResponseStatus::NeedsAction) | None => "Respond".to_owned(),
                            Some(r) => format!("Your response: {}", r.label()),
                        }))
                        .child(
                            h_flex()
                                .gap(px(6.))
                                .child(response_button("Accept", ResponseStatus::Accepted, "check", cx))
                                .child(response_button("Tentative", ResponseStatus::Tentative, "circle-question-mark", cx))
                                .child(response_button("Decline", ResponseStatus::Declined, "x", cx)),
                        ),
                )
            })
            .when(e.conference_url.is_some() || e.html_link.is_some(), |d| {
                let conf = e.conference_url.clone();
                let link = e.html_link.clone();
                d.child(
                    h_flex()
                        .gap(px(6.))
                        .children(conf.map(|url| Button::new("join").small().icon(icon("video").size(px(14.))).label("Join").on_click(move |_, _w, cx| cx.open_url(&url))))
                        .children(link.map(|url| Button::new("open-gcal").ghost().small().icon(icon("external-link").size(px(14.))).label("Open in Google Calendar").on_click(move |_, _w, cx| cx.open_url(&url)))),
                )
            })
            .when(!e.attendees.is_empty(), |d| {
                d.child(
                    v_flex()
                        .gap(px(6.))
                        .child(div().text_size(px(12.)).font_weight(FontWeight::MEDIUM).text_color(p.muted).child(format!("{} guests", e.attendees.len())))
                        .children(e.attendees.iter().take(12).map(|a| {
                            let (ic, color) = match a.response {
                                ResponseStatus::Accepted => ("circle-check", p.success),
                                ResponseStatus::Declined => ("circle-x", p.error),
                                ResponseStatus::Tentative => ("circle-question-mark", p.warning),
                                ResponseStatus::NeedsAction => ("circle", p.muted),
                            };
                            h_flex()
                                .items_center()
                                .gap(px(8.))
                                .text_size(px(12.))
                                .child(icon(ic).size(px(14.)).text_color(color))
                                .child(div().flex_1().truncate().child(format!("{}{}", a.display_name(), if a.is_organizer { " · Organizer" } else if a.optional { " · Optional" } else { "" })))
                        }))
                        .when(e.attendees.len() > 12, |d| d.child(div().text_size(px(11.)).text_color(p.muted).child(format!("and {} more", e.attendees.len() - 12)))),
                )
            })
            .children(description_html.map(|html| div().text_size(px(13.)).line_height(px(20.)).child(TextView::html(SharedString::from(format!("evdesc-{}", e.id)), html).selectable(true))))
            .child(div().flex_1())
            .when(can_edit || can_delete, |d| {
                let edit_id = e.id.clone();
                let del_id = e.id.clone();
                d.child(
                    h_flex()
                        .gap(px(6.))
                        .when(can_edit, |r| r.child(Button::new("edit-event").small().icon(icon("pencil").size(px(14.))).label("Edit").on_click(cx.listener(move |this, _, window, cx| this.request_edit_event(edit_id.clone(), window, cx)))))
                        .when(can_delete, |r| r.child(Button::new("delete-event").ghost().small().icon(icon("trash").size(px(14.)).text_color(p.error)).label("Delete").on_click(cx.listener(move |this, _, window, cx| this.request_delete_event(del_id.clone(), window, cx))))),
                )
            })
    }

    fn render_new_event(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = *cx.palette();
        let Some(form) = self.new_event.as_ref() else { return div().into_any_element() };
        let caldav = form.calendar.as_ref().is_some_and(|id|id.0.starts_with("caldav:"));
        let calendar_name = form.calendar.as_ref().and_then(|id| self.calendars.iter().find(|c| &c.id == id)).map(|c| c.name.clone()).unwrap_or_else(|| "Choose…".into());
        let editable: Vec<(CalendarId, String)> = self.calendars.iter().filter(|c| c.can_edit).map(|c| (c.id.clone(), c.name.clone())).collect();
        let this = cx.entity().downgrade();
        let field = |label: &'static str, input: AnyElement| {
            v_flex().gap(px(4.)).child(div().text_size(px(11.)).font_weight(FontWeight::MEDIUM).text_color(p.muted).child(label)).child(input)
        };
        v_flex()
            .id("new-event")
            .size_full()
            .p(px(20.))
            .gap(px(12.))
            .child(h_flex().items_center().child(div().flex_1().text_size(px(16.)).font_weight(FontWeight::SEMIBOLD).child(if form.editing.is_some() { "Edit event" } else { "New event" })).child(Button::new("cancel-new-event").ghost().xsmall().icon(icon("x").size(px(14.))).on_click(cx.listener(|this, _, window, cx| this.cancel_new_event(window, cx)))))
            .child(field("Title", Input::new(&form.title).aria_label("Event title").into_any_element()))
            .child(h_flex().gap(px(8.)).child(div().flex_1().min_w_0().child(field("Starts on", Input::new(&form.date).aria_label("Event start date").into_any_element())))
                .child(div().flex_1().min_w_0().child(field("Ends on",Input::new(&form.end_date).aria_label("Event end date").into_any_element()))))
            .child(h_flex().items_center().gap(px(10.)).child(Switch::new("all-day").checked(form.all_day).label("All day").on_click(cx.listener(|this, checked: &bool, _w, cx| {
                if let Some(f) = this.new_event.as_mut() {
                    f.all_day = *checked;
                }
                cx.notify();
            }))))
            .when(!form.all_day, |d| d.child(h_flex().gap(px(10.)).child(div().flex_1().child(field("Start", Input::new(&form.start).aria_label("Event start time").into_any_element()))).child(div().flex_1().child(field("End", Input::new(&form.end).aria_label("Event end time").into_any_element())))))
            .child(field("Location", Input::new(&form.location).aria_label("Event location").into_any_element()))
            .when(!caldav,|d|d.child(field(
                "Guests",
                v_flex()
                    .gap(px(6.))
                    .child(Input::new(&form.guests))
                    .child(
                        h_flex()
                            .flex_wrap()
                            .items_center()
                            .gap(px(6.))
                            .children(form.availability.iter().map(|(email, busy)| {
                                let (label, color, ic) = match busy {
                                    Some(true) => ("busy", p.error, "circle-x"),
                                    Some(false) => ("free", p.success, "circle-check"),
                                    None => ("unknown", p.muted, "circle"),
                                };
                                h_flex().items_center().gap(px(4.)).px(px(7.)).py(px(2.)).rounded(px(5.)).bg(p.chip).text_size(px(11.)).child(icon(ic).size(px(12.)).text_color(color)).child(format!("{email} · {label}"))
                            }))
                            .child(
                                Button::new("check-availability")
                                    .ghost()
                                    .xsmall()
                                    .icon(icon("calendar-search").size(px(13.)))
                                    .label(if form.checking { "Checking…" } else { "Check availability" })
                                    .on_click(cx.listener(|this, _, window, cx| this.check_availability(window, cx))),
                            ),
                    )
                    .into_any_element(),
            )))
            .when(!caldav,|d|d.child(h_flex().items_center().gap(px(10.)).child(Switch::new("add-meet").checked(form.add_meet).label("Add a Google Meet link").on_click(cx.listener(|this, checked: &bool, _w, cx| {
                if let Some(f) = this.new_event.as_mut() {
                    f.add_meet = *checked;
                }
                cx.notify();
            })))))
            .when(form.editing.is_some() && form.scope != RecurrenceScope::This, |d| {
                d.child(h_flex().items_center().gap(px(6.)).text_size(px(12.)).text_color(p.muted).child(icon("repeat").size(px(13.))).child(format!("Applies to: {}", form.scope.label().to_lowercase())))
            })
            .child(field("Description", div().h(px(120.)).child(Textarea::new(&form.description).h_full()).into_any_element()))
            .when(form.editing.is_none(), |d| d.child(field(
                "Calendar",
                Button::new("pick-calendar")
                    .small()
                    .label(calendar_name)
                    .dropdown_caret(true)
                    .dropdown_menu(move |mut menu, _window, _cx| {
                        for (id, name) in &editable {
                            let t = this.clone();
                            let id = id.clone();
                            menu = menu.item(PopupMenuItem::new(name.clone()).on_click(move |_, _w, cx| {
                                let _ = t.update(cx, |this, cx| {
                                    if let Some(f) = this.new_event.as_mut() {
                                        f.calendar = Some(id.clone());
                                    }
                                    cx.notify();
                                });
                            }));
                        }
                        menu
                    })
                    .into_any_element(),
            )))
            .children(form.error.clone().map(|e| h_flex().gap(px(6.)).items_center().text_size(px(12.)).text_color(p.error).child(icon("circle-alert").size(px(14.)).text_color(p.error)).child(e)))
            .child(div().flex_1())
            .child(h_flex().gap(px(8.)).child(Button::new("save-event").primary().label("Save").tooltip("Save event (Ctrl S)").on_click(cx.listener(|this, _, window, cx| this.save_new_event(window, cx)))).child(Button::new("discard-event").ghost().label("Cancel").on_click(cx.listener(|this, _, window, cx| this.cancel_new_event(window, cx)))))
            .into_any_element()
    }
}

/// Greedy lane assignment for overlapping timed events (sorted by start). Returns
/// `(lane, lanes_in_cluster)` per event.
fn assign_lanes(events: &[&Event]) -> Vec<(usize, usize)> {
    let mut result = vec![(0usize, 1usize); events.len()];
    let mut i = 0;
    while i < events.len() {
        // Find the cluster of transitively overlapping events starting at i.
        let mut cluster_end = events[i].end;
        let mut j = i + 1;
        while j < events.len() && events[j].start < cluster_end {
            cluster_end = cluster_end.max(events[j].end);
            j += 1;
        }
        let mut lane_ends: Vec<i64> = Vec::new();
        for k in i..j {
            let e = events[k];
            let lane = match lane_ends.iter().position(|&end| end <= e.start) {
                Some(l) => {
                    lane_ends[l] = e.end;
                    l
                }
                None => {
                    lane_ends.push(e.end);
                    lane_ends.len() - 1
                }
            };
            result[k].0 = lane;
        }
        for k in i..j {
            result[k].1 = lane_ends.len();
        }
        i = j;
    }
    result
}

/// The attendee list after editing the guest field: known guests keep their responses, new
/// ones start as "needs action", and the owner is listed as organizer when there are guests.
fn merge_guests(existing: &[Attendee], guests: &[Address], owner: &Address) -> Vec<Attendee> {
    let mut out: Vec<Attendee> = Vec::new();
    for g in guests {
        if g.email.eq_ignore_ascii_case(&owner.email) {
            continue;
        }
        match existing.iter().find(|a| a.email.eq_ignore_ascii_case(&g.email)) {
            Some(a) => out.push(a.clone()),
            None => out.push(Attendee { email: g.email.clone(), name: g.name.clone(), response: ResponseStatus::NeedsAction, is_self: false, is_organizer: false, optional: false }),
        }
    }
    if out.is_empty() {
        return existing.iter().filter(|a| a.is_self).cloned().collect();
    }
    let me = existing.iter().find(|a| a.is_self).cloned().unwrap_or(Attendee { email: owner.email.clone(), name: owner.name.clone(), response: ResponseStatus::Accepted, is_self: true, is_organizer: true, optional: false });
    out.insert(0, me);
    out
}

#[cfg(test)]
mod calendar_drag_tests {
    use super::{CalDrag, DragMode};
    use overlook_core::{tz, EventId};

    #[test]
    fn dragging_between_days_preserves_wall_clock_time_across_dst() {
        // Also run in a separate TZ=Europe/Berlin process to exercise both transitions.
        for date in ["2026-03-28", "2026-10-24"] {
            let start = tz::date_start(tz::parse_date(date).unwrap()) + 9 * 3_600_000;
            let mut drag = CalDrag { event_id: EventId::from("drag-test"), mode: DragMode::Move,
                origin_y: 0., origin_day: 0, day_offset: 1, dy: 0., start,
                end: start + 3_600_000, moved: true };
            let (moved, end) = drag.proposed();
            assert_eq!(tz::minutes_into_day(moved), tz::minutes_into_day(start));
            assert_eq!(tz::local_date(moved), tz::local_date(tz::add_days(start, 1)));
            assert_eq!(end - moved, 3_600_000);
            drag.start = moved;
            drag.end = end;
            drag.day_offset = -1;
            assert_eq!(drag.proposed(), (start, start + 3_600_000));
        }
    }
}
