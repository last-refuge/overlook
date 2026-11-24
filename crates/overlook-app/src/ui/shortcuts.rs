//! Live GPUI keybinding registration, shortcut hints, and the Keyboard settings page.

use super::settings::{section, setting_row, SettingsState};
use super::*;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::Disableable;
use overlook_core::shortcuts::{
    effective_context_for, find_conflicts, format_keystroke, keystrokes_conflict,
    normalize_keystroke, ShortcutAction, ShortcutCategory, ShortcutContext, ShortcutPrefs,
    ShortcutPreset, ShortcutSetError,
};
use std::collections::HashSet;

/// Currently applied keyboard shortcut preferences in GPUI's keymap.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct AppliedShortcuts(ShortcutPrefs);
impl Global for AppliedShortcuts {}

/// Tracks every `(keystroke, gpui_context)` pair Overlook has bound so preset switches
/// and override edits can unbind stale keys via `NoAction` without calling
/// `cx.clear_key_bindings()` (which would wipe `gpui_kit`'s internal Input/Dialog bindings).
#[derive(Default)]
struct AppliedBindings {
    keys: Vec<(String, &'static str)>,
}
impl Global for AppliedBindings {}

/// Interactive state for the Settings -> Keyboard page.
#[derive(Default)]
pub struct KeyboardSettingsState {
    /// Action currently capturing a keystroke, if any.
    pub recording: Option<ShortcutAction>,
    /// Inline validation or conflict error for a specific action.
    pub error: Option<(ShortcutAction, String)>,
}

/// Returns the shortcut preferences currently applied to GPUI's keymap.
pub(crate) fn current_shortcuts(cx: &App) -> ShortcutPrefs {
    cx.try_global::<AppliedShortcuts>()
        .map(|s| s.0.clone())
        .unwrap_or_default()
}

/// Maps an `overlook_core::ShortcutContext` to Overlook's GPUI key context string.
pub(crate) const fn gpui_context(ctx: ShortcutContext) -> &'static str {
    match ctx {
        ShortcutContext::Window => WINDOW_CONTEXT,
        ShortcutContext::MessageList => LIST_CONTEXT,
        ShortcutContext::Calendar => CALENDAR_CONTEXT,
    }
}

/// Expands a canonical keystroke into GPUI keystroke strings so physical shifted symbols
/// like `#` (`Shift+3`) and `?` (`Shift+/`) match reliably across keyboard layouts and
/// platforms. The primary display keystroke is always LAST so GPUI's `bindings_for_action`
/// (which prefers the last bound keystroke) displays the primary label in menus.
pub(crate) fn gpui_keystroke_variants(canonical: &str) -> Vec<&str> {
    match canonical {
        "#" => vec!["shift-3", "#"],
        "shift-3" => vec!["#", "shift-3"],
        "?" => vec!["shift-/", "?"],
        "shift-/" => vec!["?", "shift-/"],
        other => vec![other],
    }
}

fn make_key_binding(action: ShortcutAction, key: &str, ctx: &'static str) -> KeyBinding {
    let context = Some(ctx);
    match action {
        ShortcutAction::Compose => KeyBinding::new(key, Compose, context),
        ShortcutAction::Reply => KeyBinding::new(key, Reply, context),
        ShortcutAction::ReplyAll => KeyBinding::new(key, ReplyAll, context),
        ShortcutAction::Forward => KeyBinding::new(key, Forward, context),
        ShortcutAction::Archive => KeyBinding::new(key, ArchiveSelected, context),
        ShortcutAction::Trash => KeyBinding::new(key, TrashSelected, context),
        ShortcutAction::DeleteForever => KeyBinding::new(key, DeleteForeverSelected, context),
        ShortcutAction::ToggleRead => KeyBinding::new(key, ToggleRead, context),
        ShortcutAction::ToggleStar => KeyBinding::new(key, ToggleStar, context),
        ShortcutAction::MoveSelected => KeyBinding::new(key, MoveSelected, context),
        ShortcutAction::NextConversation => KeyBinding::new(key, NextConversation, context),
        ShortcutAction::PrevConversation => KeyBinding::new(key, PrevConversation, context),
        ShortcutAction::OpenSelected => KeyBinding::new(key, OpenSelected, context),
        ShortcutAction::SelectAllConversations => KeyBinding::new(key, SelectAllConversations, context),
        ShortcutAction::SendMessage => KeyBinding::new(key, SendMessage, context),
        ShortcutAction::PopOutComposer => KeyBinding::new(key, PopOutComposer, context),
        ShortcutAction::ToggleMessageHistory => KeyBinding::new(key, ToggleMessageHistory, context),
        ShortcutAction::FocusSearch => KeyBinding::new(key, FocusSearch, context),
        ShortcutAction::OpenCommandPalette => KeyBinding::new(key, OpenCommandPalette, context),
        ShortcutAction::Undo => KeyBinding::new(key, UndoLast, context),
        ShortcutAction::ShowHistory => KeyBinding::new(key, ShowHistory, context),
        ShortcutAction::SyncNow => KeyBinding::new(key, SyncNow, context),
        ShortcutAction::RenameFolder => KeyBinding::new(key, RenameFolder, context),
        ShortcutAction::SwitchToMail => KeyBinding::new(key, SwitchToMail, context),
        ShortcutAction::SwitchToCalendar => KeyBinding::new(key, SwitchToCalendar, context),
        ShortcutAction::SwitchToContacts => KeyBinding::new(key, SwitchToContacts, context),
        ShortcutAction::OpenSettings => KeyBinding::new(key, OpenSettings, context),
        ShortcutAction::Quit => KeyBinding::new(key, QuitOverlook, context),
        ShortcutAction::NewEvent => KeyBinding::new(key, NewEventAction, context),
        ShortcutAction::SaveCalendarEvent => KeyBinding::new(key, SaveCalendarEvent, context),
        ShortcutAction::CalendarToday => KeyBinding::new(key, CalendarToday, context),
        ShortcutAction::CalendarDay => KeyBinding::new(key, CalendarDay, context),
        ShortcutAction::CalendarWeek => KeyBinding::new(key, CalendarWeek, context),
        ShortcutAction::CalendarMonth => KeyBinding::new(key, CalendarMonth, context),
        ShortcutAction::CalendarAgenda => KeyBinding::new(key, CalendarAgenda, context),
        ShortcutAction::CalendarPrevious => KeyBinding::new(key, CalendarPrevious, context),
        ShortcutAction::CalendarNext => KeyBinding::new(key, CalendarNext, context),
        ShortcutAction::CalendarDeleteSelected => KeyBinding::new(key, CalendarDeleteSelected, context),
        ShortcutAction::CalendarEditSelected => KeyBinding::new(key, CalendarEditSelected, context),
    }
}

/// One resolved GPUI binding entry along with its canonical keystroke and context tag.
#[derive(Debug)]
pub(crate) struct ResolvedBinding {
    pub keystroke: String,
    pub context: &'static str,
    pub binding: KeyBinding,
}

/// Builds the ordered list of Overlook's GPUI keybindings for `prefs`.
///
/// Order matters in GPUI's keymap (later bindings take precedence at equal depth and are
/// returned first by `bindings_for_action`):
/// 1. Fixed non-customizable bindings (Escape, composer formatting, selection extend, settings search).
/// 2. Optional secondary aliases for non-overridden actions (only when unclaimed by primary bindings).
/// 3. Primary customizable bindings (with physical key variants ordered so the primary symbol is last).
fn push_binding(out: &mut Vec<ResolvedBinding>, key: &str, ctx: &'static str, binding: KeyBinding) {
    if Keystroke::parse(key).is_ok() {
        out.push(ResolvedBinding {
            keystroke: key.to_owned(),
            context: ctx,
            binding,
        });
    }
}

pub(crate) fn build_keybindings(prefs: &ShortcutPrefs) -> Vec<ResolvedBinding> {
    let mut out: Vec<ResolvedBinding> = Vec::with_capacity(80);

    // 1. Fixed non-customizable bindings.
    push_binding(&mut out, "escape", WINDOW_CONTEXT, KeyBinding::new("escape", Escape, Some(WINDOW_CONTEXT)));
    push_binding(&mut out, "ctrl-b", WINDOW_CONTEXT, KeyBinding::new("ctrl-b", FormatBold, Some(WINDOW_CONTEXT)));
    push_binding(&mut out, "ctrl-i", WINDOW_CONTEXT, KeyBinding::new("ctrl-i", FormatItalic, Some(WINDOW_CONTEXT)));
    push_binding(&mut out, "ctrl-shift-x", WINDOW_CONTEXT, KeyBinding::new("ctrl-shift-x", FormatStrike, Some(WINDOW_CONTEXT)));
    push_binding(&mut out, "ctrl-shift-c", WINDOW_CONTEXT, KeyBinding::new("ctrl-shift-c", FormatCode, Some(WINDOW_CONTEXT)));
    push_binding(&mut out, "ctrl-shift-8", WINDOW_CONTEXT, KeyBinding::new("ctrl-shift-8", FormatBullets, Some(WINDOW_CONTEXT)));
    push_binding(&mut out, "ctrl-shift-7", WINDOW_CONTEXT, KeyBinding::new("ctrl-shift-7", FormatNumbers, Some(WINDOW_CONTEXT)));
    push_binding(&mut out, "ctrl-shift-9", WINDOW_CONTEXT, KeyBinding::new("ctrl-shift-9", FormatQuote, Some(WINDOW_CONTEXT)));
    push_binding(&mut out, "shift-down", LIST_CONTEXT, KeyBinding::new("shift-down", ExtendSelectionNext, Some(LIST_CONTEXT)));
    push_binding(&mut out, "shift-up", LIST_CONTEXT, KeyBinding::new("shift-up", ExtendSelectionPrev, Some(LIST_CONTEXT)));
    push_binding(&mut out, "down", CALENDAR_CONTEXT, KeyBinding::new("down", CalendarSelectNext, Some(CALENDAR_CONTEXT)));
    push_binding(&mut out, "up", CALENDAR_CONTEXT, KeyBinding::new("up", CalendarSelectPrevious, Some(CALENDAR_CONTEXT)));
    push_binding(&mut out, "ctrl-f", SETTINGS_CONTEXT, KeyBinding::new("ctrl-f", FocusSettingsSearch, Some(SETTINGS_CONTEXT)));
    push_binding(&mut out, "f3", SETTINGS_CONTEXT, KeyBinding::new("f3", FocusSettingsSearch, Some(SETTINGS_CONTEXT)));

    // 2. Non-conflicting secondary aliases (bound BEFORE primaries so menus show the primary key).
    let mut secondary_palette_uses_ctrl_shift_p = false;
    for action in ShortcutAction::ALL {
        if prefs.is_overridden(action) {
            continue;
        }
        for &(sec_key, sec_ctx) in action.secondary_bindings(prefs.preset) {
            if prefs.is_claimed(sec_key, sec_ctx) {
                continue;
            }
            if action == ShortcutAction::OpenCommandPalette && sec_key == "ctrl-shift-p" {
                secondary_palette_uses_ctrl_shift_p = true;
            }
            let ctx = gpui_context(sec_ctx);
            for variant in gpui_keystroke_variants(sec_key) {
                push_binding(&mut out, variant, ctx, make_key_binding(action, variant, ctx));
            }
        }
    }

    // Preserve Overlook's existing TogglePreview propagation over OpenCommandPalette's
    // secondary Ctrl+Shift+P when composer is open.
    if secondary_palette_uses_ctrl_shift_p
        || !prefs.is_claimed("ctrl-shift-p", ShortcutContext::Window)
    {
        push_binding(
            &mut out,
            "ctrl-shift-p",
            WINDOW_CONTEXT,
            KeyBinding::new("ctrl-shift-p", TogglePreview, Some(WINDOW_CONTEXT)),
        );
    }

    // 3. Primary customizable bindings (bound LAST so they always take precedence and appear in menus).
    for action in ShortcutAction::ALL {
        let key = prefs.binding(action);
        let ctx = gpui_context(effective_context_for(action, &key));
        for variant in gpui_keystroke_variants(&key) {
            push_binding(&mut out, variant, ctx, make_key_binding(action, variant, ctx));
        }
    }

    out
}

/// Applies `prefs` to GPUI's live keymap immediately without restarting and without wiping
/// `gpui_kit`'s internal widget/text-input bindings.
pub(crate) fn apply_keybindings(prefs: &ShortcutPrefs, cx: &mut App) {
    let resolved = build_keybindings(prefs);
    let new_keys: HashSet<(String, &'static str)> = resolved
        .iter()
        .map(|b| (b.keystroke.clone(), b.context))
        .collect();

    let previous_keys: Vec<(String, &'static str)> = cx
        .try_global::<AppliedBindings>()
        .map(|b| b.keys.clone())
        .unwrap_or_default();

    let mut gpui_bindings: Vec<KeyBinding> =
        Vec::with_capacity(previous_keys.len() + resolved.len());

    // Unbind any previously registered Overlook (key, context) pair that is no longer active.
    for (old_key, old_ctx) in &previous_keys {
        if !new_keys.contains(&(old_key.clone(), *old_ctx)) {
            gpui_bindings.push(KeyBinding::new(old_key, NoAction, Some(old_ctx)));
        }
    }

    for entry in resolved {
        gpui_bindings.push(entry.binding);
    }

    cx.bind_keys(gpui_bindings);
    cx.set_global(AppliedBindings {
        keys: new_keys.into_iter().collect(),
    });
    cx.set_global(AppliedShortcuts(prefs.clone()));
}

/// Converts a live GPUI `Keystroke` captured from a `KeyDownEvent` into a normalized
/// Overlook keystroke string. Returns `None` for bare modifier presses (Shift, Ctrl, Alt, Cmd).
pub(crate) fn keystroke_from_gpui(stroke: &Keystroke) -> Option<String> {
    let raw_key = stroke.key.as_str();
    if matches!(
        raw_key,
        "shift" | "control" | "ctrl" | "alt" | "platform" | "cmd" | "super" | "meta" | "capslock"
    ) {
        return None;
    }

    let ctrl = stroke.modifiers.control;
    let alt = stroke.modifiers.alt;
    let cmd = stroke.modifiers.platform;
    let mut shift = stroke.modifiers.shift;

    // On shifted punctuation like `#` (Shift+3) or `?` (Shift+/), GPUI often reports
    // `key_char: Some("#")` alongside `shift: true`. Prefer the shifted symbol when no
    // non-shift modifiers are held so Gmail-style `#` and `?` capture naturally.
    let key_part = if !ctrl && !alt && !cmd && shift {
        if let Some(ch) = stroke.key_char.as_deref() {
            if matches!(
                ch,
                "#" | "?" | "!" | "@" | "$" | "%" | "^" | "&" | "*" | "(" | ")" | "_" | "+" | "{" | "}" | "|" | ":" | "\"" | "<" | ">" | "~"
            ) {
                shift = false;
                ch.to_owned()
            } else {
                raw_key.to_owned()
            }
        } else {
            raw_key.to_owned()
        }
    } else {
        raw_key.to_owned()
    };

    let mut parts: Vec<String> = Vec::with_capacity(5);
    if ctrl {
        parts.push("ctrl".to_owned());
    }
    if alt {
        parts.push("alt".to_owned());
    }
    if shift {
        parts.push("shift".to_owned());
    }
    if cmd {
        parts.push("cmd".to_owned());
    }
    parts.push(key_part);
    let candidate = parts.join("-");
    normalize_keystroke(&candidate).ok()
}

impl MailWindow {
    /// Human-readable shortcut hint for `action` reflecting the active preset and overrides.
    pub(crate) fn shortcut_hint(&self, action: ShortcutAction) -> String {
        self.prefs.shortcuts.hint(action)
    }

    /// Formats `"Label (Hint)"` for tooltips.
    pub(crate) fn tooltip_with_hint(&self, label: &str, action: ShortcutAction) -> String {
        let hint = self.shortcut_hint(action);
        if hint.is_empty() {
            label.to_owned()
        } else {
            format!("{label} ({hint})")
        }
    }

    /// Compact summary of key shortcuts shown on Settings -> About.
    pub(crate) fn keyboard_summary(&self) -> String {
        let h = |a| self.shortcut_hint(a);
        format!(
            "{} preset · {} new message · {} search · {} reply · {} reply all · {} forward · {} send · {} toggle read · {}/{} move · {} archive · {} delete · {} star · {} move to · {} undo · {} settings · {} quit",
            self.prefs.shortcuts.preset.label(),
            h(ShortcutAction::Compose),
            h(ShortcutAction::FocusSearch),
            h(ShortcutAction::Reply),
            h(ShortcutAction::ReplyAll),
            h(ShortcutAction::Forward),
            h(ShortcutAction::SendMessage),
            h(ShortcutAction::ToggleRead),
            h(ShortcutAction::NextConversation),
            h(ShortcutAction::PrevConversation),
            h(ShortcutAction::Archive),
            h(ShortcutAction::Trash),
            h(ShortcutAction::ToggleStar),
            h(ShortcutAction::MoveSelected),
            h(ShortcutAction::Undo),
            h(ShortcutAction::OpenSettings),
            h(ShortcutAction::Quit),
        )
    }

    pub(crate) fn set_shortcut_preset(&mut self, preset: ShortcutPreset, cx: &mut Context<Self>) {
        if self.prefs.shortcuts.preset == preset {
            return;
        }
        self.prefs.shortcuts.set_preset(preset);
        if let Some(state) = self.settings.as_mut() {
            state.keyboard.recording = None;
            state.keyboard.error = None;
        }
        self.save_prefs(cx);
        cx.notify();
    }

    pub(crate) fn start_recording_shortcut(
        &mut self,
        action: ShortcutAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(state) = self.settings.as_mut() {
            let toggle_off = state.keyboard.recording == Some(action);
            state.keyboard.recording = (!toggle_off).then_some(action);
            state.keyboard.error = None;
            state.focus.focus(window, cx);
        }
        cx.notify();
    }

    pub(crate) fn cancel_recording_shortcut(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(state) = self.settings.as_mut() else {
            return false;
        };
        if state.keyboard.recording.take().is_some() {
            state.keyboard.error = None;
            cx.notify();
            return true;
        }
        false
    }

    pub(crate) fn handle_shortcut_capture(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(action) = self
            .settings
            .as_ref()
            .and_then(|s| s.keyboard.recording)
        else {
            return false;
        };

        // Escape cancels recording without closing Settings.
        if event.keystroke.key == "escape"
            && !event.keystroke.modifiers.control
            && !event.keystroke.modifiers.alt
            && !event.keystroke.modifiers.platform
        {
            self.cancel_recording_shortcut(cx);
            window.prevent_default();
            cx.stop_propagation();
            return true;
        }

        // Ignore bare modifier presses until the main key is pressed.
        let Some(captured) = keystroke_from_gpui(&event.keystroke) else {
            // If it was a reserved key (e.g. Tab), show an error instead of letting Tab move focus.
            if matches!(event.keystroke.key.as_str(), "tab") {
                if let Some(state) = self.settings.as_mut() {
                    state.keyboard.error = Some((
                        action,
                        "Tab is reserved for navigation and cannot be reassigned.".to_owned(),
                    ));
                }
                window.prevent_default();
                cx.stop_propagation();
                cx.notify();
                return true;
            }
            return false;
        };

        window.prevent_default();
        cx.stop_propagation();

        match self.prefs.shortcuts.set_override_checked(action, &captured) {
            Ok(_) => {
                if let Some(state) = self.settings.as_mut() {
                    state.keyboard.recording = None;
                    state.keyboard.error = None;
                }
                self.save_prefs(cx);
                cx.notify();
            }
            Err(err) => {
                let message = match err {
                    ShortcutSetError::Invalid(e) => e.to_string(),
                    ShortcutSetError::Conflict {
                        keystroke,
                        conflicts_with,
                    } => {
                        let names: Vec<String> = conflicts_with
                            .iter()
                            .map(|a| {
                                format!(
                                    "{} ({})",
                                    a.label(),
                                    self.prefs.shortcuts.effective_context(*a).label()
                                )
                            })
                            .collect();
                        format!(
                            "{} conflicts with {}. Press another key or Esc to cancel.",
                            format_keystroke(&keystroke),
                            names.join(", ")
                        )
                    }
                };
                if let Some(state) = self.settings.as_mut() {
                    state.keyboard.error = Some((action, message));
                }
                cx.notify();
            }
        }
        true
    }

    pub(crate) fn reset_shortcut_action(&mut self, action: ShortcutAction, cx: &mut Context<Self>) {
        if self.prefs.shortcuts.reset_action(action) {
            if let Some(state) = self.settings.as_mut() {
                if state.keyboard.recording == Some(action) {
                    state.keyboard.recording = None;
                }
                if state.keyboard.error.as_ref().is_some_and(|(a, _)| *a == action) {
                    state.keyboard.error = None;
                }
            }
            self.save_prefs(cx);
            cx.notify();
        }
    }

    pub(crate) fn reset_all_shortcuts(&mut self, cx: &mut Context<Self>) {
        if self.prefs.shortcuts.reset_all() {
            if let Some(state) = self.settings.as_mut() {
                state.keyboard.recording = None;
                state.keyboard.error = None;
            }
            self.save_prefs(cx);
            cx.notify();
        }
    }

    pub(crate) fn render_settings_keyboard(
        &self,
        state: &SettingsState,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let p = *cx.palette();
        let shortcuts = &self.prefs.shortcuts;
        let preset = shortcuts.preset;
        let override_count = shortcuts.override_count();
        let conflicts = find_conflicts(shortcuts);

        let preset_control = segmented(&p)
            .children(ShortcutPreset::ALL.iter().map(|&opt| {
                segment(
                    SharedString::from(format!("shortcut-preset-{}", opt.label())),
                    opt.label(),
                    opt == preset,
                    &p,
                )
                .on_click(cx.listener(move |this, _, _w, cx| {
                    this.set_shortcut_preset(opt, cx);
                }))
            }))
            .into_any_element();

        let reset_label = if override_count == 0 {
            "Reset all to preset defaults".to_owned()
        } else if override_count == 1 {
            "Reset 1 custom shortcut".to_owned()
        } else {
            format!("Reset {override_count} custom shortcuts")
        };

        let mut root = v_flex()
            .gap(px(18.))
            .child(section(
                "Preset",
                Some("Choose a preset or click any shortcut below to customize it. Single-key mail shortcuts only fire while browsing the message list, never while typing."),
            ))
            .child(setting_row(
                "Keyboard preset",
                preset.description(),
                preset_control,
            ))
            .child(setting_row(
                "Reset keyboard shortcuts",
                "Restore every action to the active preset's defaults.",
                Button::new("shortcuts-reset-all")
                    .small()
                    .icon(icon("rotate-ccw").size(px(14.)))
                    .label(reset_label)
                    .disabled(override_count == 0)
                    .on_click(cx.listener(|this, _, _w, cx| this.reset_all_shortcuts(cx)))
                    .into_any_element(),
            ));

        if !conflicts.is_empty() {
            let summary: String = conflicts
                .iter()
                .map(|c| {
                    format!(
                        "{}: {} & {}",
                        format_keystroke(&c.keystroke),
                        c.action_a.label(),
                        c.action_b.label()
                    )
                })
                .collect::<Vec<_>>()
                .join(" · ");
            root = root.child(
                h_flex()
                    .items_center()
                    .gap(px(10.))
                    .px(px(12.))
                    .py(px(10.))
                    .rounded(px(8.))
                    .bg(p.error.opacity(0.10))
                    .border_1()
                    .border_color(p.error.opacity(0.35))
                    .child(icon("circle-alert").size(px(16.)).text_color(p.error))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap(px(2.))
                            .child(
                                div()
                                    .text_size(crate::ui::ui_px(12.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(p.error)
                                    .child(format!(
                                        "{} shortcut {} detected",
                                        conflicts.len(),
                                        if conflicts.len() == 1 { "conflict" } else { "conflicts" }
                                    )),
                            )
                            .child(
                                div()
                                    .text_size(crate::ui::ui_px(12.))
                                    .text_color(p.foreground)
                                    .child(summary),
                            ),
                    ),
            );
        }

        for category in ShortcutCategory::ALL {
            let actions: Vec<ShortcutAction> = ShortcutAction::ALL
                .iter()
                .copied()
                .filter(|a| a.category() == category)
                .collect();

            root = root.child(divider(&p)).child(section(category.label(), None));

            let mut list = v_flex().gap(px(6.));
            for action in actions {
                let binding = shortcuts.binding(action);
                let hint = format_keystroke(&binding);
                let default_hint = format_keystroke(action.default_binding(preset));
                let overridden = shortcuts.is_overridden(action);
                let ctx = shortcuts.effective_context(action);
                let recording = state.keyboard.recording == Some(action);
                let row_error = state
                    .keyboard
                    .error
                    .as_ref()
                    .filter(|(a, _)| *a == action)
                    .map(|(_, msg)| msg.clone());
                let conflicting_with: Vec<&'static str> = conflicts
                    .iter()
                    .filter_map(|c| {
                        if c.action_a == action {
                            Some(c.action_b.label())
                        } else if c.action_b == action {
                            Some(c.action_a.label())
                        } else {
                            None
                        }
                    })
                    .collect();

                let mut badges = h_flex().items_center().gap(px(6.));
                badges = badges.child(
                    div()
                        .text_size(crate::ui::ui_px(11.))
                        .text_color(p.muted)
                        .child(ctx.label()),
                );
                if overridden {
                    badges = badges.child(
                        div()
                            .px(px(6.))
                            .py(px(1.))
                            .rounded(px(4.))
                            .bg(p.selection)
                            .text_size(crate::ui::ui_px(10.))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(p.accent)
                            .child(format!("Custom · default {default_hint}")),
                    );
                }
                if !conflicting_with.is_empty() {
                    badges = badges.child(
                        div()
                            .px(px(6.))
                            .py(px(1.))
                            .rounded(px(4.))
                            .bg(p.error.opacity(0.14))
                            .text_size(crate::ui::ui_px(10.))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(p.error)
                            .child(format!("Conflicts with {}", conflicting_with.join(", "))),
                    );
                }

                let key_button = if recording {
                    Button::new(SharedString::from(format!("shortcut-bind-{action:?}")))
                        .primary()
                        .small()
                        .label("Press keys… (Esc)")
                        .tooltip("Press the new shortcut key combination, or Esc to cancel")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.start_recording_shortcut(action, window, cx);
                        }))
                } else {
                    Button::new(SharedString::from(format!("shortcut-bind-{action:?}")))
                        .small()
                        .label(hint.clone())
                        .tooltip(format!("Click to change shortcut for {}", action.label()))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.start_recording_shortcut(action, window, cx);
                        }))
                };

                let mut controls = h_flex().items_center().gap(px(6.)).child(key_button);
                if overridden {
                    controls = controls.child(
                        Button::new(SharedString::from(format!("shortcut-reset-{action:?}")))
                            .ghost()
                            .small()
                            .icon(icon("rotate-ccw").size(px(13.)))
                            .tooltip(format!("Reset to {default_hint}"))
                            .on_click(cx.listener(move |this, _, _w, cx| {
                                this.reset_shortcut_action(action, cx);
                            })),
                    );
                }

                let row = v_flex()
                    .gap(px(4.))
                    .px(px(10.))
                    .py(px(6.))
                    .rounded(px(7.))
                    .when(recording, |d| {
                        d.bg(p.selection.opacity(0.5))
                            .border_1()
                            .border_color(p.accent)
                    })
                    .when(!recording && !conflicting_with.is_empty(), |d| {
                        d.bg(p.error.opacity(0.06))
                            .border_1()
                            .border_color(p.error.opacity(0.3))
                    })
                    .child(
                        h_flex()
                            .items_center()
                            .gap(px(16.))
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .gap(px(2.))
                                    .child(
                                        div()
                                            .text_size(crate::ui::ui_px(13.))
                                            .font_weight(FontWeight::MEDIUM)
                                            .child(action.label()),
                                    )
                                    .child(badges),
                            )
                            .child(controls),
                    )
                    .when_some(row_error, |d, err| {
                        d.child(
                            div()
                                .text_size(crate::ui::ui_px(11.))
                                .text_color(p.error)
                                .child(err),
                        )
                    });

                list = list.child(row);
            }
            root = root.child(list);
        }

        // Guard against unused import warning on helper re-exports if needed.
        let _ = keystrokes_conflict;
        root
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::{KeyContext, Keymap, Keystroke, Modifiers};
    use std::prelude::v1::test;

    #[test]
    fn every_preset_binding_parses_in_gpui_and_single_key_mail_commands_stay_in_message_list() {
        for preset in ShortcutPreset::ALL {
            let prefs = ShortcutPrefs {
                preset,
                overrides: std::collections::BTreeMap::new(),
            };
            let built = build_keybindings(&prefs);
            assert!(!built.is_empty());

            for action in ShortcutAction::ALL {
                let key = prefs.binding(action);
                for variant in gpui_keystroke_variants(&key) {
                    assert!(
                        Keystroke::parse(variant).is_ok(),
                        "GPUI failed to parse {variant:?} for {action:?} in {preset:?}"
                    );
                }
                if overlook_core::shortcuts::is_single_key(&key) {
                    let ctx = gpui_context(effective_context_for(action, &key));
                    assert_ne!(
                        ctx, WINDOW_CONTEXT,
                        "Single-key binding {key:?} for {action:?} must NEVER bind in WINDOW_CONTEXT"
                    );
                }
            }
        }

        // Even when a user overrides a Window action to a single key (e.g. SyncNow -> "g"),
        // it must bind in MessageList and NEVER in MailWindow.
        let mut custom = ShortcutPrefs::default();
        custom
            .set_override_checked(ShortcutAction::SyncNow, "g")
            .unwrap();
        let built = build_keybindings(&custom);
        let sync_bindings: Vec<_> = built
            .iter()
            .filter(|b| b.keystroke == "g")
            .collect();
        assert_eq!(sync_bindings.len(), 1);
        assert_eq!(sync_bindings[0].context, LIST_CONTEXT);
    }

    #[test]
    fn keymap_rebind_switches_presets_and_preserves_gpui_kit_input_bindings() {
        // Simulate gpui_kit's Input bindings existing in the keymap BEFORE Overlook binds anything.
        let mut keymap = Keymap::default();
        keymap.add_bindings([
            KeyBinding::new("ctrl-c", gpui_kit::component::input::Copy, Some("Input")),
            KeyBinding::new("ctrl-v", gpui_kit::component::input::Paste, Some("Input")),
            KeyBinding::new("ctrl-a", gpui_kit::component::input::SelectAll, Some("Input")),
            KeyBinding::new("ctrl-z", gpui_kit::component::input::Undo, Some("Input")),
        ]);

        let apply_to_keymap = |keymap: &mut Keymap,
                               previous: &mut Vec<(String, &'static str)>,
                               prefs: &ShortcutPrefs| {
            let resolved = build_keybindings(prefs);
            let new_keys: HashSet<(String, &'static str)> = resolved
                .iter()
                .map(|b| (b.keystroke.clone(), b.context))
                .collect();
            let mut batch = Vec::new();
            for (old_k, old_ctx) in previous.iter() {
                if !new_keys.contains(&(old_k.clone(), *old_ctx)) {
                    batch.push(KeyBinding::new(old_k, NoAction, Some(old_ctx)));
                }
            }
            for entry in resolved {
                batch.push(entry.binding);
            }
            keymap.add_bindings(batch);
            *previous = new_keys.into_iter().collect();
        };

        let mut applied = Vec::new();
        let outlook = ShortcutPrefs::default();
        apply_to_keymap(&mut keymap, &mut applied, &outlook);

        let window_stack = vec![KeyContext::parse(WINDOW_CONTEXT).unwrap()];
        let list_stack = vec![
            KeyContext::parse(WINDOW_CONTEXT).unwrap(),
            KeyContext::parse(LIST_CONTEXT).unwrap(),
        ];
        let input_stack = vec![
            KeyContext::parse(WINDOW_CONTEXT).unwrap(),
            KeyContext::parse("Input").unwrap(),
        ];
        let settings_input_stack = vec![
            KeyContext::parse(WINDOW_CONTEXT).unwrap(),
            KeyContext::parse(SETTINGS_CONTEXT).unwrap(),
            KeyContext::parse("Input").unwrap(),
        ];

        // Outlook: Ctrl+N composes in MailWindow, `c` does NOT compose in MessageList.
        let (matched, _) =
            keymap.bindings_for_input(&[Keystroke::parse("ctrl-n").unwrap()], &window_stack);
        assert!(matched.iter().any(|b| b.action().name() == Compose.name()));
        let (matched, _) =
            keymap.bindings_for_input(&[Keystroke::parse("c").unwrap()], &list_stack);
        assert!(matched.is_empty());

        // Switch to Gmail preset!
        let gmail = ShortcutPrefs {
            preset: ShortcutPreset::Gmail,
            overrides: std::collections::BTreeMap::new(),
        };
        apply_to_keymap(&mut keymap, &mut applied, &gmail);

        // Old Outlook Ctrl+N is unbound (NoAction stops it), Gmail `c` fires in MessageList!
        let (matched, _) =
            keymap.bindings_for_input(&[Keystroke::parse("ctrl-n").unwrap()], &window_stack);
        assert!(
            matched.is_empty(),
            "Ctrl+N must stop firing after switching to Gmail preset"
        );
        let (matched, _) =
            keymap.bindings_for_input(&[Keystroke::parse("c").unwrap()], &list_stack);
        assert!(matched.iter().any(|b| b.action().name() == Compose.name()));

        // Gmail `/` focuses search in MessageList, `#` / `shift-3` trashes in MessageList,
        // `r`/`a`/`f`/`z` fire in MessageList.
        for (key, expected_name) in [
            ("/", FocusSearch.name()),
            ("#", TrashSelected.name()),
            ("shift-3", TrashSelected.name()),
            ("r", Reply.name()),
            ("a", ReplyAll.name()),
            ("f", Forward.name()),
            ("z", UndoLast.name()),
            ("j", NextConversation.name()),
            ("k", PrevConversation.name()),
            ("e", ArchiveSelected.name()),
            ("s", ToggleStar.name()),
            ("v", MoveSelected.name()),
            ("shift-u", ToggleRead.name()),
            ("?", OpenCommandPalette.name()),
            ("shift-/", OpenCommandPalette.name()),
        ] {
            let (matched, _) =
                keymap.bindings_for_input(&[Keystroke::parse(key).unwrap()], &list_stack);
            assert!(
                matched.iter().any(|b| b.action().name() == expected_name),
                "Gmail key {key:?} should trigger {expected_name} in MessageList"
            );
            // CRITICAL ACCEPTANCE CHECK: NONE of those single-key Gmail commands may fire
            // while typing in Search, Composer, Settings, or any Input!
            let (in_input, _) =
                keymap.bindings_for_input(&[Keystroke::parse(key).unwrap()], &input_stack);
            assert!(
                in_input.is_empty(),
                "Gmail key {key:?} must NEVER fire inside an Input! Matched: {in_input:?}"
            );
            let (in_settings, _) =
                keymap.bindings_for_input(&[Keystroke::parse(key).unwrap()], &settings_input_stack);
            assert!(
                in_settings.is_empty(),
                "Gmail key {key:?} must NEVER fire inside Settings Input!"
            );
        }

        // gpui_kit Input bindings (Ctrl+C, Ctrl+V, Ctrl+A, Ctrl+Z) MUST survive preset switches!
        let (copy_match, _) =
            keymap.bindings_for_input(&[Keystroke::parse("ctrl-c").unwrap()], &input_stack);
        assert!(
            copy_match
                .iter()
                .any(|b| b.action().name() == gpui_kit::component::input::Copy.name()),
            "Input Ctrl+C must be preserved"
        );
        let (undo_match, _) =
            keymap.bindings_for_input(&[Keystroke::parse("ctrl-z").unwrap()], &input_stack);
        assert!(
            undo_match
                .iter()
                .any(|b| b.action().name() == gpui_kit::component::input::Undo.name()),
            "Input Ctrl+Z must be preserved"
        );

        // Switch back to Outlook preset: `c` is unbound, `ctrl-n` is restored!
        apply_to_keymap(&mut keymap, &mut applied, &outlook);
        let (matched, _) =
            keymap.bindings_for_input(&[Keystroke::parse("c").unwrap()], &list_stack);
        assert!(matched.is_empty());
        let (matched, _) =
            keymap.bindings_for_input(&[Keystroke::parse("ctrl-n").unwrap()], &window_stack);
        assert!(matched.iter().any(|b| b.action().name() == Compose.name()));
    }

    #[test]
    fn keystroke_from_gpui_captures_chords_and_shifted_symbols() {
        let mut shift_mod = Modifiers::none();
        shift_mod.shift = true;
        let hash = Keystroke {
            modifiers: shift_mod,
            key: "3".to_owned(),
            key_char: Some("#".to_owned()),
        };
        assert_eq!(keystroke_from_gpui(&hash).as_deref(), Some("#"));

        let question = Keystroke {
            modifiers: shift_mod,
            key: "/".to_owned(),
            key_char: Some("?".to_owned()),
        };
        assert_eq!(keystroke_from_gpui(&question).as_deref(), Some("?"));

        let shift_u = Keystroke {
            modifiers: shift_mod,
            key: "u".to_owned(),
            key_char: Some("U".to_owned()),
        };
        assert_eq!(keystroke_from_gpui(&shift_u).as_deref(), Some("shift-u"));

        assert_eq!(
            keystroke_from_gpui(&Keystroke::parse("ctrl-shift-r").unwrap()).as_deref(),
            Some("ctrl-shift-r")
        );
        assert_eq!(
            keystroke_from_gpui(&Keystroke::parse("shift").unwrap()),
            None
        );
    }

    #[test]
    fn keyboard_icon_asset_loads() {
        let loaded = crate::assets::Assets.load("icons/keyboard.svg").unwrap();
        assert!(loaded.is_some(), "Lucide keyboard.svg icon should be bundled");
    }
}
