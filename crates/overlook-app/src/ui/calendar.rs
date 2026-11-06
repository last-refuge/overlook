//! Calendar module: week, day, and agenda views, event details with responses, and a
//! simple new-event form. Everything reads from the local store; changes are written with
//! their pending operation and reach Google through the sync engine.

use super::widgets::*;
use super::MailWindow;
use crate::assets::icon;
use crate::theme::ActivePalette;
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::component::menu::{ContextMenuExt, DropdownMenu, PopupMenu, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::text::TextView;
use gpui_kit::component::{Sizable, WindowExt};
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
    /// Keep typed event times in their labeled zone while the system/display zone changes.
    pub time_zone: tz::TimeZone,
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
    /// The event this form copies, when it was opened with Duplicate.
    pub duplicate_of: Option<Event>,
    /// The time dragged out in the grid, when the form was opened that way. It then shows as
    /// a modal over the grid instead of beside it, with the time still marked behind it.
    pub dragged: Option<(i64, i64)>,
    /// Enter in the title saves a dragged-out event.
    pub _title_enter: Option<Subscription>,
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

/// Empty time being dragged out in the day or week grid, to become a new event. Positions are
/// from the top of the day, so scrolling during the drag doesn't move the anchor.
#[derive(Clone, Copy, Debug)]
pub struct CalRangeDrag {
    pub day: i64,
    pub anchor_y: f32,
    pub y: f32,
}

impl CalRangeDrag {
    /// Whether the pointer went far enough to be a drag rather than a click.
    pub fn moved(&self) -> bool {
        (self.y - self.anchor_y).abs() >= 4.
    }

    /// Minutes into the day of the time covered, out to the 15-minute marks around it.
    fn minutes(&self) -> (i32, i32) {
        let minutes = |y: f32| (y / HOUR_PX * 60.).clamp(0., 24. * 60.);
        let (a, b) = (minutes(self.anchor_y), minutes(self.y));
        let start = ((a.min(b) / 15.).floor() as i32 * 15).min(24 * 60 - 15);
        let end = ((a.max(b) / 15.).ceil() as i32 * 15).max(start + 15).min(24 * 60);
        (start, end)
    }

    pub fn times(&self) -> (i64, i64) {
        let (start, end) = self.minutes();
        (tz::at_minutes(self.day, start), tz::at_minutes(self.day, end))
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
        self.caldav_cached = self.caldav_connection.as_ref().is_some_and(|c| self.store.caldav_range_cached(&c.id, from, to, &tz::cache_key()).unwrap_or(false));
        self.caldav_last_sync = self.caldav_connection.as_ref().and_then(|c| self.store.caldav_last_sync(&c.id).ok().flatten());
        self.caldav_changes = self.store.caldav_changes(&self.account.id).unwrap_or_default();
        self.caldav_previews = self.caldav_changes.iter().filter_map(|change| {
            change.remote.as_ref().map(|remote| (change.id.clone(), overlook_provider::caldav::resource_preview(&remote.icalendar)))
        }).collect();
        // Date-only and floating projections depend on the local zone. Preserve them on
        // disk, but don't present an old-zone projection as the current calendar.
        if self.caldav_connection.as_ref().is_some_and(|c| !self.store.caldav_timezone_matches(&c.id, &tz::cache_key()).unwrap_or(false)) {
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
        self.toolbar_more_open = false;
        self.toolbar_editing = false;
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

    /// A click on an event selects it, and a second click on the same event opens its details
    /// in a modal. Opening the side panel narrows the grid, which would move the event out from
    /// under that second click, so a panel that isn't already open waits to see if one comes.
    fn click_event(&mut self, id: EventId, click: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        let second_click = is_double_click(click) && self.cal_selected.as_ref() == Some(&id);
        let panel_open = self.new_event.is_some() || (self.selected_event().is_some() && self.cal_panel_wait.is_none() && self.event_in_modal().is_none());
        self.new_event = None;
        self.cal_selected = Some(id.clone());
        if second_click {
            self.cal_panel_wait = None;
            self.cal_event_modal = Some(id);
            self.cal_modal_focus.focus(window, cx);
        } else {
            self.cal_event_modal = None;
            if !panel_open {
                self.cal_panel_wait = Some(cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(PANEL_WAIT).await;
                    this.update(cx, |this, cx| {
                        this.cal_panel_wait = None;
                        cx.notify();
                    })
                    .ok();
                }));
            }
        }
        cx.notify();
    }

    /// The event whose details are showing in a modal.
    pub(super) fn event_in_modal(&self) -> Option<&Event> {
        let open = self.module == Module::Calendar && self.new_event.is_none() && self.cal_event_modal.is_some() && self.cal_event_modal == self.cal_selected;
        if open { self.selected_event() } else { None }
    }

    /// Closes the modal for one of the event's actions, which keeps the event selected.
    fn leave_event_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.cal_event_modal.take().is_some() {
            self.cal_focus.focus(window, cx);
        }
    }

    /// Closes the event's details, wherever they are showing.
    pub(crate) fn close_event_details(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.leave_event_modal(window, cx);
        self.cal_selected = None;
        cx.notify();
    }

    fn calendar_of(&self, event: &Event) -> Option<&Calendar> {
        self.calendars.iter().find(|c| c.id == event.calendar_id)
    }

    pub(super) fn can_edit_calendar_event(&self, event: &Event) -> bool {
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
        self.cal_event_modal = None;
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
            if self.store.calendar_connection(&account.id).ok().flatten().is_some_and(|c| !self.store.caldav_timezone_matches(&c.id, &tz::cache_key()).unwrap_or(false)) { continue; }
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
        self.leave_event_modal(window, cx);
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
        self.leave_event_modal(window, cx);
        match self.store.event(&id).ok().flatten() {
            Some(e) if e.is_recurring_instance() => {
                self.cal_scope_prompt = Some(ScopePrompt { event_id: id, action: ScopeAction::Delete });
                cx.notify();
            }
            _ => self.confirm_delete_event(id, RecurrenceScope::This, window, cx),
        }
    }

    /// Deletes an event, first asking if that will tell guests: their cancellation can't be
    /// taken back, and a deleted event has no Undo.
    fn confirm_delete_event(&mut self, id: EventId, scope: RecurrenceScope, window: &mut Window, cx: &mut Context<Self>) {
        let Some(event) = self.store.event(&id).ok().flatten() else { return };
        let guests = event.attendees.iter().filter(|a| !a.is_self).count();
        if guests == 0 {
            return self.delete_event_scoped(id, scope, window, cx);
        }
        let (message, detail) = delete_question(&event.display_title(), scope, guests);
        let answer = window.prompt(PromptLevel::Warning, &message, Some(&detail), &["Delete", "Cancel"], cx);
        let account = self.account.id.clone();
        cx.spawn_in(window, async move |this, cx| {
            if answer.await != Ok(0) {
                return;
            }
            let _ = this.update_in(cx, |this, window, cx| {
                // The prompt was open a while: only delete in the account it asked about.
                if this.account.id == account {
                    this.delete_event_scoped(id, scope, window, cx);
                }
            });
        })
        .detach();
    }

    pub(crate) fn answer_scope_prompt(&mut self, scope: RecurrenceScope, window: &mut Window, cx: &mut Context<Self>) {
        let Some(prompt) = self.cal_scope_prompt.take() else { return };
        match prompt.action {
            ScopeAction::Edit => self.start_edit_event_scoped(prompt.event_id, scope, window, cx),
            ScopeAction::Delete => self.confirm_delete_event(prompt.event_id, scope, window, cx),
        }
    }

    pub(crate) fn start_edit_event_scoped(&mut self, id: EventId, scope: RecurrenceScope, window: &mut Window, cx: &mut Context<Self>) {
        let Some(event) = self.store.event(&id).ok().flatten() else { return };
        if !self.can_edit_calendar_event(&event) { return; }
        let form = Self::form_from_event(&event, guest_text(&event.attendees), window, cx);
        let add_meet = event.conference_url.is_some() || event.wants_conference;
        self.new_event = Some(NewEvent { add_meet, editing: Some(id), original: Some(event), scope, ..form });
        cx.notify();
    }

    /// Opens the event form filled in from `id`. Nothing is created, and no guest hears of it,
    /// until Save. Only your own events bring their guests and Meet link: a copy of an invitation
    /// should not invite everyone on it.
    pub(crate) fn duplicate_event(&mut self, id: EventId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(event) = self.store.event(&id).ok().flatten() else { return };
        if !self.calendars.iter().any(|c| c.can_edit) {
            crate::ui::toast(window, cx, crate::notify::Level::Warning, "No calendar you can write to is connected yet.");
            return;
        }
        let own = self.can_edit_calendar_event(&event);
        let calendar = match self.calendar_of(&event).filter(|c| c.can_edit) {
            Some(c) => Some(c.id.clone()),
            None => self.account_prefs_for(&self.account.id).preferred_calendar(&self.calendars).map(|c| c.id.clone()),
        };
        let form = Self::form_from_event(&event, if own { guest_text(&event.attendees) } else { String::new() }, window, cx);
        let add_meet = own && (event.conference_url.is_some() || event.wants_conference);
        self.leave_event_modal(window, cx);
        self.cal_selected = None;
        self.cal_scope_prompt = None;
        self.new_event = Some(NewEvent { add_meet, calendar, duplicate_of: Some(event), ..form });
        cx.notify();
    }

    /// The event form with `event`'s fields in it, its title focused.
    fn form_from_event(event: &Event, guests: String, window: &mut Window, cx: &mut Context<Self>) -> NewEvent {
        let guests = cx.new(|cx| InputState::new(window, cx).placeholder("Guests, comma separated").default_value(guests));
        let title = cx.new(|cx| InputState::new(window, cx).placeholder("Add a title").default_value(event.summary.clone()));
        let date = cx.new(|cx| InputState::new(window, cx).placeholder("YYYY-MM-DD").default_value(tz::local_date(event.start).to_string()));
        let end_date = cx.new(|cx| InputState::new(window,cx).placeholder("YYYY-MM-DD").default_value(tz::local_date(if event.all_day {tz::add_days(event.end,-1)} else {event.end}).to_string()));
        let start = cx.new(|cx| InputState::new(window, cx).placeholder("9:00 AM").default_value(clock_text(event.start)));
        let end = cx.new(|cx| InputState::new(window, cx).placeholder("10:00 AM").default_value(clock_text(if event.all_day { event.start + 3_600_000 } else { event.end })));
        let location = cx.new(|cx| InputState::new(window, cx).placeholder("Add a location").default_value(event.location.clone().unwrap_or_default()));
        let description = cx.new(|cx| TextareaState::new(window, cx).placeholder("Add a description").default_value(event.description.clone().unwrap_or_default()));
        title.update(cx, |s, cx| s.focus(window, cx));
        NewEvent { time_zone: tz::current_zone(), title, date, end_date, start, end, location, description, guests, add_meet: false, availability: Vec::new(), checking: false, calendar: Some(event.calendar_id.clone()), all_day: event.all_day, error: None, editing: None, original: None, scope: RecurrenceScope::This, duplicate_of: None, dragged: None, _title_enter: None }
    }

    /// The right-click menu of an event anywhere in the calendar. Edit and Delete ask the same
    /// questions as the toolbar, and need the event open: that is where a repeating event asks
    /// which occurrences are meant.
    fn event_menu(&self, e: &Event, cx: &mut Context<Self>) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
        let me = cx.entity().downgrade();
        let id = e.id.clone();
        let can_edit = self.can_edit_calendar_event(e);
        move |menu, _window, _cx| {
            let (t, event) = (me.clone(), id.clone());
            let menu = menu.item(PopupMenuItem::new("Edit…").disabled(!can_edit).on_click(move |_, window, cx| {
                t.update(cx, |this, cx| {
                    this.new_event = None;
                    this.cal_selected = Some(event.clone());
                    this.request_edit_event(event.clone(), window, cx);
                })
                .ok();
            }));
            let (t, event) = (me.clone(), id.clone());
            let menu = menu.item(PopupMenuItem::new("Duplicate…").on_click(move |_, window, cx| {
                t.update(cx, |this, cx| this.duplicate_event(event.clone(), window, cx)).ok();
            }));
            let (t, event) = (me.clone(), id.clone());
            menu.separator().item(PopupMenuItem::new("Delete").disabled(!can_edit).on_click(move |_, window, cx| {
                t.update(cx, |this, cx| {
                    this.new_event = None;
                    this.cal_selected = Some(event.clone());
                    this.request_delete_event(event.clone(), window, cx);
                })
                .ok();
            }))
        }
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
        let (Some(s), Some(e)) = (tz::local_datetime_in(date, st, &form.time_zone), tz::local_datetime_in(end_date, et, &form.time_zone)) else { return None };
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
        let form = Self::empty_form(date_text.clone(), date_text, next_hour, end_hour, calendar, window, cx);
        self.cal_selected = None;
        self.cal_scope_prompt = None;
        self.new_event = Some(form);
        cx.notify();
    }

    /// Opens the new-event modal for the time dragged out in the grid.
    fn start_dragged_event(&mut self, start: i64, end: i64, window: &mut Window, cx: &mut Context<Self>) {
        let calendar = self.account_prefs_for(&self.account.id).preferred_calendar(&self.calendars).map(|c| c.id.clone());
        let form = Self::empty_form(tz::local_date(start).to_string(), tz::local_date(end).to_string(), clock_text(start), clock_text(end), calendar, window, cx);
        let enter = cx.subscribe_in(&form.title, window, |this, _state, event: &InputEvent, window, cx| {
            if let InputEvent::PressEnter { .. } = event {
                this.save_new_event(window, cx);
            }
        });
        self.cal_selected = None;
        self.cal_scope_prompt = None;
        self.new_event = Some(NewEvent { dragged: Some((start, end)), _title_enter: Some(enter), ..form });
        cx.notify();
    }

    /// The event form with only the date and times filled in, its title focused.
    fn empty_form(date: String, end_date: String, start: String, end: String, calendar: Option<CalendarId>, window: &mut Window, cx: &mut Context<Self>) -> NewEvent {
        let title = cx.new(|cx| InputState::new(window, cx).placeholder("Add a title"));
        let date = cx.new(|cx| InputState::new(window, cx).placeholder("YYYY-MM-DD").default_value(date));
        let end_date = cx.new(|cx| InputState::new(window,cx).placeholder("YYYY-MM-DD").default_value(end_date));
        let start = cx.new(|cx| InputState::new(window, cx).placeholder("9:00 AM").default_value(start));
        let end = cx.new(|cx| InputState::new(window, cx).placeholder("10:00 AM").default_value(end));
        let location = cx.new(|cx| InputState::new(window, cx).placeholder("Add a location"));
        let description = cx.new(|cx| TextareaState::new(window, cx).placeholder("Add a description"));
        let guests = cx.new(|cx| InputState::new(window, cx).placeholder("Guests, comma separated"));
        title.update(cx, |s, cx| s.focus(window, cx));
        NewEvent { time_zone: tz::current_zone(), title, date, end_date, start, end, location, description, guests, add_meet: false, availability: Vec::new(), checking: false, calendar, all_day: false, error: None, editing: None, original: None, scope: RecurrenceScope::This, duplicate_of: None, dragged: None, _title_enter: None }
    }

    /// A window position as a distance from the top of the day in the time grid.
    fn grid_y(&self, position: Point<Pixels>) -> f32 {
        f32::from(position.y - self.cal_scroll.bounds().top() - self.cal_scroll.offset().y)
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
        let reminders = form.duplicate_of.as_ref().and_then(|e| e.reminders.clone());
        let time_zone = form.time_zone.clone();
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
            let Some(s) = tz::local_datetime_in(date, st, &time_zone) else { return fail("Choose a valid start time in the event’s time zone.", self, cx) };
            let Some(e) = tz::local_datetime_in(end_date, et, &time_zone) else { return fail("Choose a valid end time in the event’s time zone.", self, cx) };
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
        // The form hides guests and Meet for these calendars, so a copy must not bring them.
        let caldav = calendar.id.0.starts_with("caldav:");
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
            attendees: if caldav { Vec::new() } else { merge_guests(&[], &guest_addresses, &self.owner()) },
            html_link: None,
            conference_url: None,
            recurring_event_id: None,
            updated: now(),
            event_type: None,
            color: None,
            reminders,
            original_start: None,
            wants_conference: add_meet && !caldav,
            pending: true,
        };
        if caldav {
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
        let heading = h_flex().items_center().gap(px(12.)).min_h(px(52.)).px(px(20.))
            .border_b_1().border_color(p.divider)
            .child(div().flex_1().text_size(px(18.)).font_weight(FontWeight::SEMIBOLD).child(title))
            .child(div().text_size(px(11.)).text_color(p.muted).child(tz::zone_label(&self.time_zone)));

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

        let detail: Option<AnyElement> = match &self.new_event {
            Some(form) if form.dragged.is_some() => None,
            Some(_) => Some(self.render_new_event(cx).into_any_element()),
            None if self.cal_panel_wait.is_some() || self.event_in_modal().is_some() => None,
            None => self.selected_event().map(|e| self.render_event_detail(e, cx).size_full().into_any_element()),
        };

        v_flex()
            .id("calendar-main")
            .key_context(super::CALENDAR_CONTEXT)
            .track_focus(&self.cal_focus)
            .size_full()
            .bg(p.surface)
            .child(heading)
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
        // The time being dragged out, or the one the open modal was started from.
        let marked = self.cal_range_drag.filter(CalRangeDrag::moved).map(|r| r.times()).or_else(|| self.new_event.as_ref().and_then(|f| f.dragged));
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
                // Pressing on an event moves or opens that event; empty time starts a new one.
                .on_mouse_down(MouseButton::Left, cx.listener(move |this, e: &MouseDownEvent, _w, _cx| {
                    if std::mem::take(&mut this.cal_press_on_event) || this.new_event.is_some() || !this.calendars.iter().any(|c| c.can_edit) {
                        return;
                    }
                    let y = this.grid_y(e.position);
                    this.cal_range_drag = Some(CalRangeDrag { day: d, anchor_y: y, y });
                }))
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
            if let Some((start, end)) = marked.filter(|(start, _)| tz::day_start(*start) == d) {
                let start_min = tz::minutes_into_day(start) as f32;
                let end_min = if end >= next { 24. * 60. } else { tz::minutes_into_day(end) as f32 };
                let height = (end_min - start_min) / 60. * HOUR_PX;
                col = col.child(
                    div()
                        .absolute()
                        .top(px(start_min / 60. * HOUR_PX))
                        .h(px(height - 2.))
                        .left_0()
                        .right(px(4.))
                        .px(px(6.))
                        .py(px(3.))
                        .rounded(px(6.))
                        .overflow_hidden()
                        .bg(p.accent.opacity(0.2))
                        .border_1()
                        .border_color(p.accent)
                        .text_size(px(11.))
                        .text_color(p.foreground)
                        .when(height >= 36., |d| d.child(div().text_size(px(12.)).font_weight(FontWeight::SEMIBOLD).truncate().child("New event")))
                        .when(height >= 20., |d| d.child(div().truncate().child(format!("{} – {}", tz::format_clock(start), tz::format_clock(end))))),
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
                    .on_mouse_move(cx.listener(|this, e: &MouseMoveEvent, _w, cx| {
                        if this.cal_range_drag.is_none() {
                            return;
                        }
                        // The button came up outside the window: the drag is over.
                        if !e.dragging() {
                            this.cal_range_drag = None;
                        }
                        let y = this.grid_y(e.position);
                        if let Some(range) = this.cal_range_drag.as_mut() {
                            range.y = y;
                        }
                        cx.notify();
                    }))
                    .on_mouse_up(MouseButton::Left, cx.listener(|this, _, window, cx| this.finish_cal_drag(window, cx)))
                    .on_mouse_up_out(MouseButton::Left, cx.listener(|this, _, window, cx| this.finish_cal_drag(window, cx)))
                    .child(h_flex().items_start().child(hours).children(columns)),
            )
    }

    /// Ends a drag: a real move or resize is saved, time dragged out opens the new-event modal,
    /// and a click that did not move is ignored.
    pub(crate) fn finish_cal_drag(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(range) = self.cal_range_drag.take() {
            cx.notify();
            if range.moved() {
                let (start, end) = range.times();
                self.start_dragged_event(start, end, window, cx);
            }
            return;
        }
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
            // Not stopped here: the press must still reach the window's focus handling. The
            // column under the event reads this instead.
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _w, _cx| this.cal_press_on_event = true))
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
            .on_click(cx.listener(move |this, click: &ClickEvent, window, cx| this.click_event(id.clone(), click, window, cx)))
            .context_menu(self.event_menu(e, cx))
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
            .on_click(cx.listener(move |this, click: &ClickEvent, window, cx| this.click_event(id.clone(), click, window, cx)))
            .context_menu(self.event_menu(e, cx))
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
                                .on_click(cx.listener(move |this, click: &ClickEvent, window, cx| this.click_event(id.clone(), click, window, cx)))
                                .context_menu(self.event_menu(e, cx))
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
                        .on_click(cx.listener(move |this, click: &ClickEvent, window, cx| this.click_event(id.clone(), click, window, cx)))
                        .context_menu(self.event_menu(e, cx)),
                );
            }
        }
        if !any {
            list = list.child(v_flex().items_center().pt(px(72.)).gap(px(6.)).text_color(p.muted).child(icon("calendar").size(px(28.)).text_color(p.divider)).child(div().text_size(px(14.)).font_weight(FontWeight::MEDIUM).text_color(p.foreground).child("Nothing scheduled")).child(div().text_size(px(12.)).child("The next two weeks are clear.")));
        }
        list.overflow_y_scrollbar()
    }

    fn render_event_detail(&self, e: &Event, cx: &mut Context<Self>) -> Div {
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
        let body = v_flex()
            .flex_none()
            .w_full()
            .p(px(20.))
            .gap(px(14.))
            .child(
                h_flex()
                    .items_start()
                    .gap(px(10.))
                    .child(div().mt(px(5.)).size(px(12.)).rounded(px(3.)).bg(fill.opacity(1.0)).border_1().border_color(edge))
                    .child(div().flex_1().text_size(px(20.)).line_height(px(26.)).font_weight(FontWeight::SEMIBOLD).child(e.display_title()))
                    .child(Button::new("close-detail").ghost().xsmall().icon(icon("x").size(px(14.))).on_click(cx.listener(|this, _, window, cx| this.close_event_details(window, cx)))),
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
                let total = e.attendees.len();
                let expanded = self.cal_guests_expanded.as_ref() == Some(&e.id);
                let shown = visible_guests(total, expanded);
                let event_id = e.id.clone();
                d.child(
                    v_flex()
                        .gap(px(6.))
                        .child(div().text_size(px(12.)).font_weight(FontWeight::MEDIUM).text_color(p.muted).child(format!("{total} guests")))
                        .children(e.attendees.iter().take(shown).map(|a| {
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
                        .when(total > GUESTS_SHOWN + 1, |d| {
                            let text = if expanded { "show fewer".to_owned() } else { format!("+{} more", total - shown) };
                            d.child(div().id("guests-more").w_full().cursor_pointer().text_size(px(12.)).text_color(p.accent).child(text).on_click(cx.listener(move |this, _, _w, cx| {
                                this.cal_guests_expanded = (!expanded).then(|| event_id.clone());
                                cx.notify();
                            })))
                        }),
                )
            })
            .children(description_html.map(|html| div().text_size(px(13.)).line_height(px(20.)).child(TextView::html(SharedString::from(format!("evdesc-{}", e.id)), html).selectable(true))));
        // The scope question stays above the scrolling details, so it is seen from any scroll
        // position. The scroller's id is per event, so opening another event starts at its top.
        v_flex()
            .children(scope_card.map(|card| div().flex_none().px(px(20.)).pt(px(20.)).child(card)))
            .child(v_flex().flex_1().min_h_0().child(body).overflow_y_scrollbar().id(SharedString::from(format!("event-detail-{}", e.id))))
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
        let heading = h_flex().flex_none().items_center().px(px(20.)).pt(px(20.)).pb(px(12.)).child(div().flex_1().text_size(px(16.)).font_weight(FontWeight::SEMIBOLD).child(if form.editing.is_some() { "Edit event" } else if form.duplicate_of.is_some() { "Duplicate event" } else { "New event" })).child(Button::new("cancel-new-event").ghost().xsmall().icon(icon("x").size(px(14.))).on_click(cx.listener(|this, _, window, cx| this.cancel_new_event(window, cx))));
        let fields = v_flex()
            .id("new-event")
            .flex_1()
            .min_h_0()
            .px(px(20.))
            .pb(px(16.))
            .gap(px(12.))
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
            .when(!form.all_day, |d| d.child(div().text_size(px(12.)).text_color(p.muted).child(format!("Times in {}", tz::zone_label(&form.time_zone)))))
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
            .when(form.duplicate_of.as_ref().is_some_and(|e| e.is_recurring_instance()), |d| {
                d.child(h_flex().items_center().gap(px(6.)).text_size(px(12.)).text_color(p.muted).child(icon("repeat").size(px(13.))).child("Copies this occurrence. The copy won’t repeat."))
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
            .overflow_y_scrollbar();
        // Outside the scrolling part, so Save and a save error are always in view.
        let footer = v_flex()
            .flex_none()
            .gap(px(10.))
            .px(px(20.))
            .py(px(14.))
            .border_t_1()
            .border_color(p.divider)
            .children(form.error.clone().map(|e| h_flex().gap(px(6.)).items_center().text_size(px(12.)).text_color(p.error).child(icon("circle-alert").size(px(14.)).text_color(p.error)).child(e)))
            .child(h_flex().gap(px(8.)).child(Button::new("save-event").primary().label("Save").tooltip("Save event (Ctrl S)").on_click(cx.listener(|this, _, window, cx| this.save_new_event(window, cx)))).child(Button::new("discard-event").ghost().label("Cancel").on_click(cx.listener(|this, _, window, cx| this.cancel_new_event(window, cx)))));
        v_flex().size_full().child(heading).child(fields).child(footer).into_any_element()
    }

    /// The event form as a modal, for an event dragged out in the grid. A click outside does
    /// nothing, so a stray click can't lose what was typed; Esc, Cancel and the close button do.
    pub(crate) fn render_event_modal(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = *cx.palette();
        div()
            .id("new-event-overlay")
            .absolute()
            .inset_0()
            .occlude()
            .bg(gpui_kit::black().opacity(if p.is_dark() { 0.55 } else { 0.3 }))
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .id("new-event-modal")
                    .w(px(480.))
                    .max_w(relative(0.94))
                    .h(px(760.))
                    .max_h(relative(0.92))
                    .rounded(px(12.))
                    .overflow_hidden()
                    .bg(p.surface)
                    .border_1()
                    .border_color(p.divider)
                    .shadow_lg()
                    .child(self.render_new_event(cx)),
            )
            .into_any_element()
    }

    /// An event's details as a modal, opened by double-clicking it. It covers the toolbar, so it
    /// carries the event's actions itself. Nothing can be lost here, so a click outside closes it.
    pub(crate) fn render_event_details_modal(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = *cx.palette();
        let Some(e) = self.event_in_modal() else { return div().into_any_element() };
        let can_edit = self.can_edit_calendar_event(e);
        let action = |name: &'static str, label: &'static str, run: fn(&mut MailWindow, EventId, &mut Window, &mut Context<MailWindow>), cx: &mut Context<Self>| {
            let id = e.id.clone();
            Button::new(name).small().ghost().label(label).on_click(cx.listener(move |this, _, window, cx| run(this, id.clone(), window, cx)))
        };
        let footer = h_flex()
            .flex_none()
            .justify_end()
            .gap(px(6.))
            .px(px(20.))
            .py(px(12.))
            .border_t_1()
            .border_color(p.divider)
            .when(can_edit, |d| d.child(action("modal-delete-event", "Delete", Self::request_delete_event, cx)))
            .child(action("modal-duplicate-event", "Duplicate…", Self::duplicate_event, cx))
            .when(can_edit, |d| d.child(action("modal-edit-event", "Edit…", Self::request_edit_event, cx)));
        div()
            .id("event-details-overlay")
            .track_focus(&self.cal_modal_focus)
            .absolute()
            .inset_0()
            .occlude()
            .bg(gpui_kit::black().opacity(if p.is_dark() { 0.55 } else { 0.3 }))
            .flex()
            .items_center()
            .justify_center()
            .on_click(cx.listener(|this, _, window, cx| this.close_event_details(window, cx)))
            .child(
                v_flex()
                    .id("event-details-modal")
                    .w(px(480.))
                    .max_w(relative(0.94))
                    .max_h(relative(0.88))
                    .rounded(px(12.))
                    .overflow_hidden()
                    .occlude()
                    .bg(p.surface)
                    .border_1()
                    .border_color(p.divider)
                    .shadow_lg()
                    .child(self.render_event_detail(e, cx).flex_1().overflow_hidden())
                    .child(footer),
            )
            .into_any_element()
    }
}

/// How long a first click holds the side panel back. Linux counts a second press within 400 ms of
/// the first as a double click, and this runs from the release of the first.
const PANEL_WAIT: std::time::Duration = std::time::Duration::from_millis(350);

/// A second click in place. Pressing a second time and dragging is a move, not a double click.
fn is_double_click(click: &ClickEvent) -> bool {
    let in_place = match click {
        ClickEvent::Mouse(m) => {
            let moved = m.up.position - m.down.position;
            f32::from(moved.x).abs() <= 4. && f32::from(moved.y).abs() <= 4.
        }
        _ => true,
    };
    click.click_count() >= 2 && in_place
}

/// What to ask before deleting an event that has guests.
fn delete_question(title: &str, scope: RecurrenceScope, guests: usize) -> (String, String) {
    let message = match scope {
        RecurrenceScope::This => format!("Delete “{title}”?"),
        RecurrenceScope::Following => format!("Delete “{title}” and the following occurrences?"),
        RecurrenceScope::All => format!("Delete every occurrence of “{title}”?"),
    };
    let told = if guests == 1 { "Its guest will be emailed a cancellation.".to_owned() } else { format!("Its {guests} guests will be emailed a cancellation.") };
    (message, format!("{told} This can’t be undone."))
}

/// An event's guests as the form's guest field shows them, without you.
/// How many guests the event details list before "+N more".
const GUESTS_SHOWN: usize = 12;

/// How many of `total` guests to list. One more than the limit is listed rather than replaced
/// by "+1 more", which would take the same room.
fn visible_guests(total: usize, expanded: bool) -> usize {
    if expanded || total <= GUESTS_SHOWN + 1 {
        total
    } else {
        GUESTS_SHOWN
    }
}

fn guest_text(attendees: &[Attendee]) -> String {
    attendees.iter().filter(|a| !a.is_self).map(|a| a.email.clone()).collect::<Vec<_>>().join(", ")
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
    use super::{delete_question, guest_text, is_double_click, visible_guests, CalDrag, CalRangeDrag, DragMode, HOUR_PX};
    use gpui_kit::{point, px, ClickEvent, MouseClickEvent, MouseDownEvent, MouseUpEvent};
    use overlook_core::{tz, Attendee, EventId, RecurrenceScope, ResponseStatus};

    fn click(count: usize, moved: f32) -> ClickEvent {
        ClickEvent::Mouse(MouseClickEvent {
            down: MouseDownEvent { position: point(px(100.), px(100.)), click_count: count, ..Default::default() },
            up: MouseUpEvent { position: point(px(100.), px(100. + moved)), click_count: count, ..Default::default() },
        })
    }

    #[test]
    fn only_a_second_click_in_place_is_a_double_click() {
        assert!(!is_double_click(&click(1, 0.)));
        assert!(is_double_click(&click(2, 0.)));
        assert!(is_double_click(&click(2, 3.)), "a hand isn't perfectly still");
        assert!(!is_double_click(&click(2, 30.)), "a second press that drags moves the event");
    }

    #[test]
    fn long_guest_lists_stop_at_twelve_unless_expanded() {
        assert_eq!(visible_guests(5, false), 5);
        assert_eq!(visible_guests(13, false), 13, "one more is listed rather than '+1 more'");
        assert_eq!(visible_guests(14, false), 12);
        assert_eq!(visible_guests(20, false), 12);
        assert_eq!(visible_guests(20, true), 20);
    }

    #[test]
    fn deleting_with_guests_says_what_will_happen() {
        let (message, detail) = delete_question("Standup", RecurrenceScope::This, 1);
        assert_eq!(message, "Delete “Standup”?");
        assert_eq!(detail, "Its guest will be emailed a cancellation. This can’t be undone.");
        let (message, detail) = delete_question("Standup", RecurrenceScope::All, 3);
        assert_eq!(message, "Delete every occurrence of “Standup”?");
        assert!(detail.starts_with("Its 3 guests"));
        assert_eq!(delete_question("Standup", RecurrenceScope::Following, 2).0, "Delete “Standup” and the following occurrences?");
    }

    #[test]
    fn dragging_out_time_covers_the_quarter_hours_it_touches() {
        let at = |minutes: f32| minutes / 60. * HOUR_PX;
        let range = |from: f32, to: f32| CalRangeDrag { day: 0, anchor_y: at(from), y: at(to) }.minutes();
        assert_eq!(range(545., 610.), (540, 615));
        assert_eq!(range(610., 545.), (540, 615), "upward drags too");
        assert_eq!(range(540., 600.), (540, 600), "exact marks stay");
        assert_eq!(range(541., 542.), (540, 555), "at least one quarter hour");
        assert_eq!(range(1430., 2000.), (1425, 1440), "stops at the end of the day");
        assert_eq!(range(1440., 1440.), (1425, 1440));
        assert_eq!(range(-50., 20.), (0, 30), "stops at the start of the day");
    }

    #[test]
    fn a_press_without_movement_is_not_a_drag() {
        assert!(!CalRangeDrag { day: 0, anchor_y: 100., y: 102. }.moved());
        assert!(CalRangeDrag { day: 0, anchor_y: 100., y: 96. }.moved());
    }

    #[test]
    fn the_guest_field_leaves_you_out() {
        let attendee = |email: &str, is_self| Attendee { email: email.into(), name: None, response: ResponseStatus::Accepted, is_self, is_organizer: is_self, optional: false };
        assert_eq!(guest_text(&[attendee("me@example.com", true), attendee("a@example.com", false), attendee("b@example.com", false)]), "a@example.com, b@example.com");
        assert_eq!(guest_text(&[attendee("me@example.com", true)]), "");
    }

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
