//! A standalone, non-modal window for calendar reminders.

use crate::assets::icon;
use crate::reminders::{Reminder, MINUTE};
use crate::theme::{self, ActivePalette, Palette};
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::{Sizable, TitleBar};
use gpui_kit::prelude::*;
use gpui_kit::*;
use overlook_core::{time::now, tz, EventId};

const SNOOZE_DURATIONS: [(i64, &str); 8] = [
    (5, "5 minutes"),
    (10, "10 minutes"),
    (15, "15 minutes"),
    (30, "30 minutes"),
    (60, "1 hour"),
    (120, "2 hours"),
    (240, "4 hours"),
    (1_440, "1 day"),
];

pub(crate) struct ReminderWindow {
    focus: FocusHandle,
    scroll: ScrollHandle,
    due: Vec<Reminder>,
    selected: Option<EventId>,
    snooze: usize,
}

impl ReminderWindow {
    pub fn open(
        due: Vec<Reminder>,
        cx: &mut App,
    ) -> anyhow::Result<(AnyWindowHandle, Entity<Self>)> {
        let bounds = Bounds::centered(None, size(px(650.), px(444.)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(580.), px(372.))),
            app_id: Some(crate::APP_ID.into()),
            kind: WindowKind::Floating,
            ..TitleBar::window_options()
        };
        gpui_kit::open_window(options, cx, move |window, cx| {
            window.set_window_title("Overlook Calendar Reminders");
            cx.new(|cx| {
                let view = Self {
                    focus: cx.focus_handle(),
                    scroll: ScrollHandle::new(),
                    selected: due.first().map(|r| r.event.id.clone()),
                    due,
                    snooze: 0,
                };
                view.focus(window, cx);
                view
            })
        })
    }

    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus.focus(window, cx);
    }

    pub fn update_due(&mut self, due: Vec<Reminder>, cx: &mut Context<Self>) {
        let previous_index = self.selected_index().unwrap_or(0);
        self.due = due;
        if self.selected_index().is_none() {
            self.select(previous_index.min(self.due.len().saturating_sub(1)), cx);
        } else {
            // Refresh the time remaining even when the set of reminders is the same.
            cx.notify();
        }
    }

    fn selected_index(&self) -> Option<usize> {
        let id = self.selected.as_ref()?;
        self.due.iter().position(|r| &r.event.id == id)
    }

    fn select(&mut self, index: usize, cx: &mut Context<Self>) {
        self.selected = self.due.get(index).map(|r| r.event.id.clone());
        if self.selected.is_some() {
            self.scroll.scroll_to_item(index);
        }
        cx.notify();
    }

    fn open_event(&self, cx: &mut Context<Self>) {
        if let Some(id) = self.selected.clone() {
            cx.defer(move |cx| crate::lifecycle::open_reminder_event(id, cx));
        }
    }

    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let stroke = &event.keystroke;
        if stroke.key == "escape" {
            window.remove_window();
        } else if stroke.key == "q" && stroke.modifiers.control {
            cx.defer(crate::lifecycle::quit);
        } else if self.focus.is_focused(window) && !self.due.is_empty() {
            let index = self.selected_index().unwrap_or(0);
            match stroke.key.as_str() {
                "up" => self.select(index.saturating_sub(1), cx),
                "down" => self.select((index + 1).min(self.due.len() - 1), cx),
                "enter" => self.open_event(cx),
                _ => return,
            }
        } else {
            return;
        }
        window.prevent_default();
        cx.stop_propagation();
    }

    fn list(
        &self,
        now: i64,
        p: Palette,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let focused = self.focus.is_focused(window);
        v_flex()
            .flex_1()
            .min_h_0()
            .overflow_hidden()
            .rounded(px(8.))
            .border_1()
            .border_color(if focused { p.focus } else { p.divider })
            .bg(p.message_surface())
            .child(
                h_flex()
                    .flex_none()
                    .justify_between()
                    .px_3()
                    .h(px(32.))
                    .items_center()
                    .border_b_1()
                    .border_color(p.divider)
                    .text_size(super::ui_px(12.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(p.muted)
                    .child("Event")
                    .child("Starts"),
            )
            .child(
                v_flex()
                    .id("reminder-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .track_focus(&self.focus)
                    .tab_stop(true)
                    .children(self.due.iter().enumerate().map(|(index, reminder)| {
                        let event = &reminder.event;
                        let selected = self.selected.as_ref() == Some(&event.id);
                        h_flex()
                            .id(SharedString::from(event.id.0.clone()))
                            .w_full()
                            .flex_none()
                            .min_h(px(39.))
                            .px_3()
                            .gap(px(12.))
                            .items_center()
                            .cursor_pointer()
                            .border_b_1()
                            .border_color(p.divider)
                            .when(selected, |row| {
                                row.bg(p.selection)
                                    .text_color(p.selection_text)
                                    .font_weight(FontWeight::MEDIUM)
                            })
                            .when(!selected, |row| row.hover(|row| row.bg(p.hover)))
                            .child(icon("calendar").size(px(15.)).text_color(if selected {
                                p.selection_text
                            } else {
                                p.accent
                            }))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .child(event.display_title()),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .text_size(super::ui_px(12.))
                                    .text_color(if selected { p.selection_text } else { p.muted })
                                    .child(relative_start(event.start, now)),
                            )
                            .on_click(cx.listener(move |this, click, window, cx| {
                                this.focus(window, cx);
                                this.select(index, cx);
                                if super::calendar::is_double_click(click) {
                                    this.open_event(cx);
                                }
                            }))
                    })),
            )
    }

    fn selected_details(
        &self,
        reminder: &Reminder,
        p: Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let event = &reminder.event;
        let start = tz::zoned(event.start).strftime("%A, %B %-d, %Y");
        let when = format!("{start} · {}", tz::format_clock(event.start));
        let context = event
            .location
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .map_or_else(
                || reminder.calendar.clone(),
                |location| format!("{} · {location}", reminder.calendar),
            );
        let meeting = event
            .conference_url
            .as_ref()
            .filter(|link| {
                url::Url::parse(link).is_ok_and(|url| matches!(url.scheme(), "http" | "https"))
            })
            .cloned();
        h_flex()
            .w_full()
            .flex_none()
            .items_center()
            .gap(px(16.))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(3.))
                    .child(
                        div()
                            .truncate()
                            .text_size(super::ui_px(18.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(event.display_title()),
                    )
                    .child(div().truncate().text_size(super::ui_px(13.)).child(when))
                    .child(
                        div()
                            .truncate()
                            .text_size(super::ui_px(12.))
                            .text_color(p.muted)
                            .child(context),
                    ),
            )
            .child(
                h_flex()
                    .flex_none()
                    .gap(px(6.))
                    .child(
                        Button::new("open-event")
                            .outline()
                            .small()
                            .label("Open event")
                            .on_click(cx.listener(|this, _, _, cx| this.open_event(cx))),
                    )
                    .children(meeting.map(|url| {
                        Button::new("join-meeting")
                            .outline()
                            .small()
                            .label("Join meeting")
                            .icon(icon("video").size(px(14.)))
                            .on_click(move |_, _, cx| cx.open_url(&url))
                    })),
            )
    }

    fn actions(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.weak_entity();
        let snooze = self.snooze;
        h_flex()
            .w_full()
            .flex_none()
            .gap(px(8.))
            .justify_between()
            .items_center()
            .flex_wrap()
            .child(
                h_flex()
                    .gap(px(6.))
                    .child(
                        Button::new("snooze-duration")
                            .outline()
                            .small()
                            .label(SNOOZE_DURATIONS[snooze].1)
                            .icon(icon("chevron-down").size(px(13.)))
                            .tooltip("Snooze duration")
                            .dropdown_menu(move |menu, _, _| {
                                SNOOZE_DURATIONS.iter().enumerate().fold(
                                    menu,
                                    |menu, (index, (_, label))| {
                                        let view = view.clone();
                                        menu.item(
                                            PopupMenuItem::new(*label)
                                                .checked(snooze == index)
                                                .on_click(move |_, _, cx| {
                                                    let _ = view.update(cx, |this, cx| {
                                                        this.snooze = index;
                                                        cx.notify();
                                                    });
                                                }),
                                        )
                                    },
                                )
                            }),
                    )
                    .child(
                        Button::new("snooze")
                            .primary()
                            .small()
                            .label("Snooze")
                            .on_click(cx.listener(|this, _, _, cx| {
                                if let Some(id) = this.selected.clone() {
                                    let duration = SNOOZE_DURATIONS[this.snooze].0;
                                    cx.defer(move |cx| {
                                        crate::lifecycle::snooze_reminder(id, duration, cx)
                                    });
                                }
                            })),
                    ),
            )
            .child(
                h_flex()
                    .gap(px(6.))
                    .child(
                        Button::new("dismiss")
                            .outline()
                            .small()
                            .label("Dismiss")
                            .on_click(cx.listener(|this, _, _, cx| {
                                if let Some(id) = this.selected.clone() {
                                    cx.defer(move |cx| {
                                        crate::lifecycle::dismiss_reminders(Some(id), cx)
                                    });
                                }
                            })),
                    )
                    .child(
                        Button::new("dismiss-all")
                            .outline()
                            .small()
                            .label("Dismiss all")
                            .on_click(|_, _, cx| {
                                cx.defer(|cx| crate::lifecycle::dismiss_reminders(None, cx))
                            }),
                    ),
            )
    }
}

impl Render for ReminderWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = *cx.palette();
        let fonts = theme::current_fonts(cx);
        let count = self.due.len();
        let title = if count == 0 {
            "Calendar reminders".into()
        } else if count == 1 {
            "1 calendar reminder".into()
        } else {
            format!("{count} calendar reminders")
        };
        let selected = self.selected_index().and_then(|index| self.due.get(index));
        v_flex()
            .id("calendar-reminders")
            .size_full()
            .bg(p.surface)
            .text_color(p.foreground)
            .font_family(SharedString::from(fonts.effective_ui_family().to_owned()))
            .text_size(super::ui_px(13.))
            .on_key_down(cx.listener(Self::key_down))
            .child(
                TitleBar::new().bg(p.chrome()).child(
                    h_flex()
                        .px_3()
                        .gap(px(8.))
                        .items_center()
                        .child(icon("bell").size(px(16.)).text_color(p.accent))
                        .child(div().font_weight(FontWeight::SEMIBOLD).child(title)),
                ),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .gap(px(16.))
                    .p(px(16.))
                    .when_some(selected, |body, reminder| {
                        body.child(self.list(now(), p, window, cx))
                            .child(self.selected_details(reminder, p, cx))
                            .child(self.actions(cx))
                    })
                    .when(selected.is_none(), |body| {
                        body.child(
                            v_flex()
                                .flex_1()
                                .justify_center()
                                .items_center()
                                .track_focus(&self.focus)
                                .tab_stop(true)
                                .gap(px(8.))
                                .text_center()
                                .child(icon("bell").size(px(26.)).text_color(p.muted))
                                .child(
                                    div()
                                        .text_size(super::ui_px(16.))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child("No reminders due"),
                                )
                                .child(
                                    div()
                                        .text_color(p.muted)
                                        .child("Snoozed reminders return here when they’re due."),
                                ),
                        )
                    }),
            )
    }
}

fn relative_start(start: i64, now: i64) -> String {
    let difference = start.saturating_sub(now);
    let minutes = difference.unsigned_abs() / MINUTE as u64;
    if minutes == 0 {
        return "Now".into();
    }
    let duration = match minutes {
        1..=59 => format!("{minutes} min"),
        60..=1439 => format!("{} hr", minutes / 60),
        _ => format!("{} d", minutes / 1440),
    };
    if difference > 0 {
        format!("In {duration}")
    } else {
        format!("{duration} overdue")
    }
}
