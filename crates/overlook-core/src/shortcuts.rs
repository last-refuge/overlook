//! Keyboard shortcut presets (Outlook and Gmail), per-action overrides,
//! normalization, display formatting, conflict detection, and reset helpers.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;

/// Built-in keyboard shortcut presets.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ShortcutPreset {
    #[default]
    Outlook,
    Gmail,
}

impl ShortcutPreset {
    pub const ALL: [ShortcutPreset; 2] = [ShortcutPreset::Outlook, ShortcutPreset::Gmail];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Outlook => "Outlook",
            Self::Gmail => "Gmail",
        }
    }

    pub const fn description(self) -> &'static str {
        match self {
            Self::Outlook => "Default Ctrl-based shortcuts with J, K, E, S, U, and V in the message list.",
            Self::Gmail => "Gmail-style single-key mail shortcuts (C, R, A, F, #, /, ?, Z, Shift U) in the message list.",
        }
    }
}

/// Scope in which a keyboard shortcut fires.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ShortcutContext {
    /// Fires anywhere in the Overlook window (only used for modified or function keys;
    /// single-key bindings are automatically scoped to `MessageList` so they never
    /// hijack typing in search, composer, event editor, or settings).
    Window,
    /// Fires only when browsing the mail message list.
    MessageList,
    /// Fires only when browsing the calendar view (and not typing in the event editor).
    Calendar,
}

impl ShortcutContext {
    /// Two contexts overlap when they can be active at the same time.
    /// `Window` overlaps with every context, whereas `MessageList` and `Calendar`
    /// are disjoint and never conflict with each other.
    pub const fn overlaps(self, other: Self) -> bool {
        matches!(
            (self, other),
            (Self::Window, _)
                | (_, Self::Window)
                | (Self::MessageList, Self::MessageList)
                | (Self::Calendar, Self::Calendar)
        )
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Window => "Window",
            Self::MessageList => "Message list",
            Self::Calendar => "Calendar",
        }
    }
}

/// Logical grouping for displaying actions in Settings -> Keyboard.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ShortcutCategory {
    Mail,
    Compose,
    Navigation,
    Calendar,
}

impl ShortcutCategory {
    pub const ALL: [ShortcutCategory; 4] = [
        Self::Mail,
        Self::Compose,
        Self::Navigation,
        Self::Calendar,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Mail => "Mail",
            Self::Compose => "Compose",
            Self::Navigation => "Navigation & App",
            Self::Calendar => "Calendar",
        }
    }
}

/// Every customizable keyboard action in Overlook.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ShortcutAction {
    // Mail
    Compose,
    Reply,
    ReplyAll,
    Forward,
    Archive,
    Trash,
    DeleteForever,
    ToggleRead,
    ToggleStar,
    MoveSelected,
    NextConversation,
    PrevConversation,
    OpenSelected,
    SelectAllConversations,
    // Compose
    SendMessage,
    PopOutComposer,
    ToggleMessageHistory,
    // Navigation & App
    FocusSearch,
    OpenCommandPalette,
    Undo,
    ShowHistory,
    SyncNow,
    RenameFolder,
    SwitchToMail,
    SwitchToCalendar,
    SwitchToContacts,
    OpenSettings,
    Quit,
    // Calendar
    NewEvent,
    SaveCalendarEvent,
    CalendarToday,
    CalendarDay,
    CalendarWeek,
    CalendarMonth,
    CalendarAgenda,
    CalendarPrevious,
    CalendarNext,
    CalendarDeleteSelected,
    CalendarEditSelected,
}

impl ShortcutAction {
    pub const ALL: [ShortcutAction; 39] = [
        Self::Compose,
        Self::Reply,
        Self::ReplyAll,
        Self::Forward,
        Self::Archive,
        Self::Trash,
        Self::DeleteForever,
        Self::ToggleRead,
        Self::ToggleStar,
        Self::MoveSelected,
        Self::NextConversation,
        Self::PrevConversation,
        Self::OpenSelected,
        Self::SelectAllConversations,
        Self::SendMessage,
        Self::PopOutComposer,
        Self::ToggleMessageHistory,
        Self::FocusSearch,
        Self::OpenCommandPalette,
        Self::Undo,
        Self::ShowHistory,
        Self::SyncNow,
        Self::RenameFolder,
        Self::SwitchToMail,
        Self::SwitchToCalendar,
        Self::SwitchToContacts,
        Self::OpenSettings,
        Self::Quit,
        Self::NewEvent,
        Self::SaveCalendarEvent,
        Self::CalendarToday,
        Self::CalendarDay,
        Self::CalendarWeek,
        Self::CalendarMonth,
        Self::CalendarAgenda,
        Self::CalendarPrevious,
        Self::CalendarNext,
        Self::CalendarDeleteSelected,
        Self::CalendarEditSelected,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Compose => "New message",
            Self::Reply => "Reply",
            Self::ReplyAll => "Reply all",
            Self::Forward => "Forward",
            Self::Archive => "Archive / Move to Inbox",
            Self::Trash => "Delete (Move to Trash)",
            Self::DeleteForever => "Delete permanently",
            Self::ToggleRead => "Mark read or unread",
            Self::ToggleStar => "Star or unstar",
            Self::MoveSelected => "Move to folder",
            Self::NextConversation => "Next conversation",
            Self::PrevConversation => "Previous conversation",
            Self::OpenSelected => "Open conversation",
            Self::SelectAllConversations => "Select all conversations",
            Self::SendMessage => "Send message",
            Self::PopOutComposer => "Pop out composer",
            Self::ToggleMessageHistory => "Show or hide message history",
            Self::FocusSearch => "Search",
            Self::OpenCommandPalette => "Command palette",
            Self::Undo => "Undo",
            Self::ShowHistory => "Change history",
            Self::SyncNow => "Sync now",
            Self::RenameFolder => "Rename folder",
            Self::SwitchToMail => "Go to Mail",
            Self::SwitchToCalendar => "Go to Calendar",
            Self::SwitchToContacts => "Go to Contacts",
            Self::OpenSettings => "Settings",
            Self::Quit => "Quit Overlook",
            Self::NewEvent => "New event",
            Self::SaveCalendarEvent => "Save event",
            Self::CalendarToday => "Jump to Today",
            Self::CalendarDay => "Day view",
            Self::CalendarWeek => "Week view",
            Self::CalendarMonth => "Month view",
            Self::CalendarAgenda => "Agenda view",
            Self::CalendarPrevious => "Previous period",
            Self::CalendarNext => "Next period",
            Self::CalendarDeleteSelected => "Delete selected event",
            Self::CalendarEditSelected => "Edit selected event",
        }
    }

    pub const fn category(self) -> ShortcutCategory {
        match self {
            Self::Compose
            | Self::Reply
            | Self::ReplyAll
            | Self::Forward
            | Self::Archive
            | Self::Trash
            | Self::DeleteForever
            | Self::ToggleRead
            | Self::ToggleStar
            | Self::MoveSelected
            | Self::NextConversation
            | Self::PrevConversation
            | Self::OpenSelected
            | Self::SelectAllConversations => ShortcutCategory::Mail,
            Self::SendMessage | Self::PopOutComposer | Self::ToggleMessageHistory => {
                ShortcutCategory::Compose
            }
            Self::FocusSearch
            | Self::OpenCommandPalette
            | Self::Undo
            | Self::ShowHistory
            | Self::SyncNow
            | Self::RenameFolder
            | Self::SwitchToMail
            | Self::SwitchToCalendar
            | Self::SwitchToContacts
            | Self::OpenSettings
            | Self::Quit => ShortcutCategory::Navigation,
            Self::NewEvent
            | Self::SaveCalendarEvent
            | Self::CalendarToday
            | Self::CalendarDay
            | Self::CalendarWeek
            | Self::CalendarMonth
            | Self::CalendarAgenda
            | Self::CalendarPrevious
            | Self::CalendarNext
            | Self::CalendarDeleteSelected
            | Self::CalendarEditSelected => ShortcutCategory::Calendar,
        }
    }

    /// Natural context of the action when bound to a non-single-key chord.
    /// Single-key bindings for `Window` actions are automatically scoped to
    /// `MessageList` via [`effective_context_for`].
    pub const fn base_context(self) -> ShortcutContext {
        match self {
            Self::Archive
            | Self::Trash
            | Self::DeleteForever
            | Self::ToggleStar
            | Self::NextConversation
            | Self::PrevConversation
            | Self::OpenSelected
            | Self::SelectAllConversations => ShortcutContext::MessageList,
            Self::CalendarToday
            | Self::CalendarDay
            | Self::CalendarWeek
            | Self::CalendarMonth
            | Self::CalendarAgenda
            | Self::CalendarPrevious
            | Self::CalendarNext
            | Self::CalendarDeleteSelected
            | Self::CalendarEditSelected => ShortcutContext::Calendar,
            Self::Compose
            | Self::Reply
            | Self::ReplyAll
            | Self::Forward
            | Self::ToggleRead
            | Self::MoveSelected
            | Self::SendMessage
            | Self::PopOutComposer
            | Self::ToggleMessageHistory
            | Self::FocusSearch
            | Self::OpenCommandPalette
            | Self::Undo
            | Self::ShowHistory
            | Self::SyncNow
            | Self::RenameFolder
            | Self::SwitchToMail
            | Self::SwitchToCalendar
            | Self::SwitchToContacts
            | Self::OpenSettings
            | Self::Quit
            | Self::NewEvent
            | Self::SaveCalendarEvent => ShortcutContext::Window,
        }
    }

    /// Primary default keystroke for `preset`.
    pub const fn default_binding(self, preset: ShortcutPreset) -> &'static str {
        match (preset, self) {
            // Mail — Outlook defaults preserve every existing Overlook binding verbatim.
            (ShortcutPreset::Outlook, Self::Compose) => "ctrl-n",
            (ShortcutPreset::Gmail, Self::Compose) => "c",

            (ShortcutPreset::Outlook, Self::Reply) => "ctrl-r",
            (ShortcutPreset::Gmail, Self::Reply) => "r",

            (ShortcutPreset::Outlook, Self::ReplyAll) => "ctrl-shift-r",
            (ShortcutPreset::Gmail, Self::ReplyAll) => "a",

            (ShortcutPreset::Outlook, Self::Forward) => "ctrl-f",
            (ShortcutPreset::Gmail, Self::Forward) => "f",

            (_, Self::Archive) => "e",

            (ShortcutPreset::Outlook, Self::Trash) => "delete",
            (ShortcutPreset::Gmail, Self::Trash) => "#",

            (_, Self::DeleteForever) => "shift-delete",

            (ShortcutPreset::Outlook, Self::ToggleRead) => "ctrl-u",
            (ShortcutPreset::Gmail, Self::ToggleRead) => "shift-u",

            (_, Self::ToggleStar) => "s",
            (_, Self::MoveSelected) => "v",
            (_, Self::NextConversation) => "j",
            (_, Self::PrevConversation) => "k",
            (_, Self::OpenSelected) => "enter",
            (_, Self::SelectAllConversations) => "ctrl-a",

            // Compose
            (_, Self::SendMessage) => "ctrl-enter",
            (_, Self::PopOutComposer) => "ctrl-shift-o",
            (_, Self::ToggleMessageHistory) => "ctrl-shift-h",

            // Navigation & App
            (ShortcutPreset::Outlook, Self::FocusSearch) => "ctrl-e",
            (ShortcutPreset::Gmail, Self::FocusSearch) => "/",

            (ShortcutPreset::Outlook, Self::OpenCommandPalette) => "ctrl-k",
            (ShortcutPreset::Gmail, Self::OpenCommandPalette) => "?",

            (ShortcutPreset::Outlook, Self::Undo) => "ctrl-z",
            (ShortcutPreset::Gmail, Self::Undo) => "z",

            (_, Self::ShowHistory) => "ctrl-h",
            (_, Self::SyncNow) => "f5",
            (_, Self::RenameFolder) => "f2",
            (_, Self::SwitchToMail) => "ctrl-1",
            (_, Self::SwitchToCalendar) => "ctrl-2",
            (_, Self::SwitchToContacts) => "ctrl-3",
            (_, Self::OpenSettings) => "ctrl-,",
            (_, Self::Quit) => "ctrl-q",

            // Calendar
            (_, Self::NewEvent) => "ctrl-shift-a",
            (_, Self::SaveCalendarEvent) => "ctrl-s",
            (_, Self::CalendarToday) => "t",
            (_, Self::CalendarDay) => "d",
            (_, Self::CalendarWeek) => "w",
            (_, Self::CalendarMonth) => "m",
            (_, Self::CalendarAgenda) => "a",
            (_, Self::CalendarPrevious) => "left",
            (_, Self::CalendarNext) => "right",
            (_, Self::CalendarDeleteSelected) => "delete",
            (_, Self::CalendarEditSelected) => "enter",
        }
    }

    /// Optional non-conflicting secondary aliases for `preset` when this action
    /// is NOT overridden by the user. Each entry is `(keystroke, context)`.
    /// Secondary aliases are only registered if no primary binding claims the same
    /// keystroke in an overlapping context.
    pub const fn secondary_bindings(self, preset: ShortcutPreset) -> &'static [(&'static str, ShortcutContext)] {
        match (preset, self) {
            (ShortcutPreset::Outlook, Self::Archive) => &[("backspace", ShortcutContext::MessageList)],
            (ShortcutPreset::Outlook, Self::ToggleRead) => &[("u", ShortcutContext::MessageList)],
            (ShortcutPreset::Gmail, Self::ToggleRead) => &[("shift-i", ShortcutContext::MessageList)],
            (ShortcutPreset::Outlook, Self::MoveSelected) => &[("ctrl-shift-v", ShortcutContext::Window)],
            (_, Self::NextConversation) => &[("down", ShortcutContext::MessageList)],
            (_, Self::PrevConversation) => &[("up", ShortcutContext::MessageList)],
            (ShortcutPreset::Gmail, Self::OpenSelected) => &[("o", ShortcutContext::MessageList)],
            (ShortcutPreset::Gmail, Self::Trash) => &[("delete", ShortcutContext::MessageList)],
            (ShortcutPreset::Outlook, Self::OpenCommandPalette) => &[("ctrl-shift-p", ShortcutContext::Window)],
            (ShortcutPreset::Gmail, Self::OpenCommandPalette) => &[("ctrl-k", ShortcutContext::Window)],
            _ => &[],
        }
    }
}

/// Errors produced when parsing or validating a keystroke string.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShortcutError {
    Empty,
    MissingKey,
    UnknownKey(String),
    Reserved(String),
}

impl fmt::Display for ShortcutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "Enter a shortcut key."),
            Self::MissingKey => write!(f, "Add a key after the modifier (for example, Ctrl N)."),
            Self::UnknownKey(token) => write!(f, "Unrecognized key “{token}”."),
            Self::Reserved(key) => {
                write!(f, "{} is reserved for navigation and cannot be reassigned.", format_keystroke(key))
            }
        }
    }
}

impl std::error::Error for ShortcutError {}

/// A conflict between two actions bound to equivalent keystrokes in overlapping contexts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShortcutConflict {
    pub action_a: ShortcutAction,
    pub action_b: ShortcutAction,
    pub keystroke: String,
    pub context_a: ShortcutContext,
    pub context_b: ShortcutContext,
}

/// Error returned when setting an override fails validation or conflicts with another action.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShortcutSetError {
    Invalid(ShortcutError),
    Conflict {
        keystroke: String,
        conflicts_with: Vec<ShortcutAction>,
    },
}

impl fmt::Display for ShortcutSetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(e) => write!(f, "{e}"),
            Self::Conflict { keystroke, conflicts_with } => {
                let names: Vec<&str> = conflicts_with.iter().map(|a| a.label()).collect();
                write!(
                    f,
                    "{} is already used by {}.",
                    format_keystroke(keystroke),
                    names.join(", ")
                )
            }
        }
    }
}

impl std::error::Error for ShortcutSetError {}

/// User keyboard shortcut preferences stored in [`crate::Prefs`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ShortcutPrefs {
    pub preset: ShortcutPreset,
    /// Per-action overrides stored as normalized keystrokes (e.g. `"ctrl-n"`, `"c"`, `"#"`).
    /// Actions not present here use the active preset's default.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub overrides: BTreeMap<ShortcutAction, String>,
}

impl ShortcutPrefs {
    /// Returns the active normalized keystroke for `action`.
    pub fn binding(&self, action: ShortcutAction) -> String {
        if let Some(raw) = self.overrides.get(&action) {
            if let Ok(norm) = normalize_keystroke(raw) {
                return norm;
            }
        }
        action.default_binding(self.preset).to_owned()
    }

    /// Returns whether `action` currently has a custom override differing from the active preset default.
    pub fn is_overridden(&self, action: ShortcutAction) -> bool {
        self.overrides
            .get(&action)
            .and_then(|raw| normalize_keystroke(raw).ok())
            .is_some_and(|norm| norm != action.default_binding(self.preset))
    }

    /// Human-readable shortcut hint for tooltips, buttons, palette, and menus (e.g. `"Ctrl N"`, `"/"`, `"#"`).
    pub fn hint(&self, action: ShortcutAction) -> String {
        format_keystroke(&self.binding(action))
    }

    /// Effective context in which `action` fires for its current binding.
    pub fn effective_context(&self, action: ShortcutAction) -> ShortcutContext {
        effective_context_for(action, &self.binding(action))
    }

    /// Switches the active preset and drops any override that happens to match the new preset's default.
    pub fn set_preset(&mut self, preset: ShortcutPreset) {
        self.preset = preset;
        self.overrides.retain(|action, raw| {
            normalize_keystroke(raw)
                .ok()
                .is_some_and(|norm| norm != action.default_binding(preset))
        });
    }

    /// Number of active custom overrides differing from the active preset's defaults.
    pub fn override_count(&self) -> usize {
        ShortcutAction::ALL
            .iter()
            .filter(|&&a| self.is_overridden(a))
            .count()
    }

    /// Returns every OTHER action whose current binding conflicts with assigning `raw_keystroke` to `action`.
    pub fn conflict_for(&self, action: ShortcutAction, raw_keystroke: &str) -> Vec<ShortcutAction> {
        let Ok(candidate) = normalize_keystroke(raw_keystroke) else {
            return Vec::new();
        };
        let candidate_ctx = effective_context_for(action, &candidate);
        let mut out = Vec::new();
        for other in ShortcutAction::ALL {
            if other == action {
                continue;
            }
            let other_key = self.binding(other);
            let other_ctx = effective_context_for(other, &other_key);
            if candidate_ctx.overlaps(other_ctx) && keystrokes_conflict(&candidate, &other_key) {
                out.push(other);
            }
        }
        out
    }

    /// Finds all conflicting pairs of primary bindings in the current configuration.
    pub fn find_conflicts(&self) -> Vec<ShortcutConflict> {
        let resolved: Vec<(ShortcutAction, String, ShortcutContext)> = ShortcutAction::ALL
            .iter()
            .map(|&a| {
                let key = self.binding(a);
                let ctx = effective_context_for(a, &key);
                (a, key, ctx)
            })
            .collect();
        let mut conflicts = Vec::new();
        for i in 0..resolved.len() {
            for j in (i + 1)..resolved.len() {
                let (a, ref ka, ca) = resolved[i];
                let (b, ref kb, cb) = resolved[j];
                if ca.overlaps(cb) && keystrokes_conflict(ka, kb) {
                    conflicts.push(ShortcutConflict {
                        action_a: a,
                        action_b: b,
                        keystroke: ka.clone(),
                        context_a: ca,
                        context_b: cb,
                    });
                }
            }
        }
        conflicts
    }

    /// Sets or clears a per-action override without blocking on conflicts.
    /// If the normalized keystroke matches the active preset's default, the override is removed.
    pub fn set_override(&mut self, action: ShortcutAction, raw_keystroke: &str) -> Result<String, ShortcutError> {
        let norm = normalize_keystroke(raw_keystroke)?;
        if norm == action.default_binding(self.preset) {
            self.overrides.remove(&action);
        } else {
            self.overrides.insert(action, norm.clone());
        }
        Ok(norm)
    }

    /// Validates `raw_keystroke`, checks for conflicts in overlapping contexts, and stores the override only when conflict-free.
    pub fn set_override_checked(
        &mut self,
        action: ShortcutAction,
        raw_keystroke: &str,
    ) -> Result<String, ShortcutSetError> {
        let norm = normalize_keystroke(raw_keystroke).map_err(ShortcutSetError::Invalid)?;
        let conflicts = self.conflict_for(action, &norm);
        if !conflicts.is_empty() {
            return Err(ShortcutSetError::Conflict {
                keystroke: norm,
                conflicts_with: conflicts,
            });
        }
        if norm == action.default_binding(self.preset) {
            self.overrides.remove(&action);
        } else {
            self.overrides.insert(action, norm.clone());
        }
        Ok(norm)
    }

    /// Resets one action to the active preset's default. Returns `true` if an override was removed.
    pub fn reset_action(&mut self, action: ShortcutAction) -> bool {
        self.overrides.remove(&action).is_some()
    }

    /// Resets all actions to the active preset's defaults. Returns `true` if any overrides were cleared.
    pub fn reset_all(&mut self) -> bool {
        if self.overrides.is_empty() {
            return false;
        }
        self.overrides.clear();
        true
    }

    /// Returns whether `keystroke` in `ctx` is already claimed by any primary binding in an overlapping context.
    /// Used when deciding whether to register optional secondary aliases.
    pub fn is_claimed(&self, keystroke: &str, ctx: ShortcutContext) -> bool {
        let Ok(candidate) = normalize_keystroke(keystroke) else {
            return false;
        };
        ShortcutAction::ALL.iter().any(|&action| {
            let other_key = self.binding(action);
            let other_ctx = effective_context_for(action, &other_key);
            ctx.overlaps(other_ctx) && keystrokes_conflict(&candidate, &other_key)
        })
    }
}

/// Top-level convenience helper: returns all conflicts in `prefs`.
pub fn find_conflicts(prefs: &ShortcutPrefs) -> Vec<ShortcutConflict> {
    prefs.find_conflicts()
}

/// Top-level convenience helper: returns actions conflicting with binding `action` to `keystroke`.
pub fn conflict_for(prefs: &ShortcutPrefs, action: ShortcutAction, keystroke: &str) -> Vec<ShortcutAction> {
    prefs.conflict_for(action, keystroke)
}

/// Top-level convenience helper: resets one action to the active preset default.
pub fn reset_action(prefs: &mut ShortcutPrefs, action: ShortcutAction) -> bool {
    prefs.reset_action(action)
}

/// Top-level convenience helper: resets all overrides to the active preset defaults.
pub fn reset_all(prefs: &mut ShortcutPrefs) -> bool {
    prefs.reset_all()
}

/// Parsed components of a normalized keystroke.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedKeystroke {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub cmd: bool,
    pub key: String,
}

impl ParsedKeystroke {
    pub fn canonical_string(&self) -> String {
        let mut parts: Vec<&str> = Vec::with_capacity(5);
        if self.ctrl {
            parts.push("ctrl");
        }
        if self.alt {
            parts.push("alt");
        }
        if self.shift {
            parts.push("shift");
        }
        if self.cmd {
            parts.push("cmd");
        }
        parts.push(&self.key);
        parts.join("-")
    }
}

/// Returns `true` when `keystroke` has no `ctrl`/`alt`/`cmd` modifier and is not a function key
/// (`f1`..`f12`). Such keystrokes must NEVER be bound in the window-wide context because they
/// would hijack typing in search, composer, event editor, settings, or other text inputs.
pub fn is_single_key(keystroke: &str) -> bool {
    let Ok(parsed) = parse_keystroke(keystroke) else {
        return true;
    };
    if parsed.ctrl || parsed.alt || parsed.cmd {
        return false;
    }
    !is_function_key(&parsed.key) && parsed.key != "escape"
}

/// Computes the effective key context for `action` when bound to `keystroke`.
/// Single-key bindings on `Window` actions are automatically scoped to `MessageList`
/// so they only fire while browsing mail and never hijack text inputs.
pub fn effective_context_for(action: ShortcutAction, keystroke: &str) -> ShortcutContext {
    match action.base_context() {
        ShortcutContext::Window if is_single_key(keystroke) => ShortcutContext::MessageList,
        other => other,
    }
}

/// Normalizes a user-supplied or serialized keystroke into Overlook's canonical GPUI format
/// (e.g. `"Ctrl+Shift+R"` -> `"ctrl-shift-r"`, `"Del"` -> `"delete"`, `"#"` -> `"#"`).
pub fn normalize_keystroke(raw: &str) -> Result<String, ShortcutError> {
    let parsed = parse_keystroke(raw)?;
    let canonical = parsed.canonical_string();
    if matches!(canonical.as_str(), "escape" | "tab" | "shift-tab") {
        return Err(ShortcutError::Reserved(canonical));
    }
    Ok(canonical)
}

/// Parses a keystroke string into modifiers and canonical key name.
pub fn parse_keystroke(raw: &str) -> Result<ParsedKeystroke, ShortcutError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(ShortcutError::Empty);
    }

    // Direct single-character symbols that also act as delimiters when combined with modifiers.
    if trimmed == "-" || trimmed == "+" {
        return Ok(ParsedKeystroke {
            ctrl: false,
            alt: false,
            shift: false,
            cmd: false,
            key: trimmed.to_owned(),
        });
    }

    // Tokenize on whitespace, `-`, and `+`, while preserving a trailing literal `-` or `+`
    // (such as `"ctrl--"` or `"ctrl-+"` or `"Ctrl + +"`).
    let tokens = split_keystroke_tokens(trimmed);
    if tokens.is_empty() {
        return Err(ShortcutError::Empty);
    }

    let mut ctrl = false;
    let mut alt = false;
    let mut shift = false;
    let mut cmd = false;
    let mut key: Option<String> = None;

    for (ix, token) in tokens.iter().enumerate() {
        let lower = token.to_ascii_lowercase();
        let is_last = ix + 1 == tokens.len();
        if !is_last {
            match lower.as_str() {
                "ctrl" | "control" => {
                    ctrl = true;
                    continue;
                }
                "alt" | "option" | "opt" => {
                    alt = true;
                    continue;
                }
                "shift" => {
                    shift = true;
                    continue;
                }
                "cmd" | "command" | "meta" | "super" | "platform" => {
                    cmd = true;
                    continue;
                }
                _ => return Err(ShortcutError::UnknownKey(token.clone())),
            }
        }
        // Final token must be a key, not a standalone modifier.
        if matches!(
            lower.as_str(),
            "ctrl" | "control" | "alt" | "option" | "opt" | "shift" | "cmd" | "command" | "meta" | "super" | "platform"
        ) {
            return Err(ShortcutError::MissingKey);
        }
        let normalized = normalize_key_name(&lower, token)?;
        key = Some(normalized);
    }

    let Some(mut key) = key else {
        return Err(ShortcutError::MissingKey);
    };

    // Shifted punctuation symbols like `shift-#` or `shift-?` carry shift implicitly.
    if shift && matches!(key.as_str(), "#" | "?" | "!" | "@" | "$" | "%" | "^" | "&" | "*" | "(" | ")" | "_" | "+" | "{" | "}" | "|" | ":" | "\"" | "<" | ">" | "~") {
        shift = false;
    }

    // Lowercase ASCII letters always normalize to lowercase (with `shift` flag capturing Shift).
    if key.len() == 1 && key.as_bytes()[0].is_ascii_uppercase() {
        key.make_ascii_lowercase();
    }

    Ok(ParsedKeystroke {
        ctrl,
        alt,
        shift,
        cmd,
        key,
    })
}

fn split_keystroke_tokens(input: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        // Skip whitespace.
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        if i >= chars.len() {
            break;
        }
        // If we are at `-` or `+`, check if it is the trailing literal key or a separator.
        if chars[i] == '-' || chars[i] == '+' {
            let next_non_ws = ((i + 1)..chars.len()).find(|&j| !chars[j].is_whitespace());
            if next_non_ws.is_none() {
                tokens.push(chars[i].to_string());
                break;
            }
            // Two consecutive separators (e.g. `ctrl--` or `ctrl-+`): the second is the key.
            i += 1;
            continue;
        }
        let start = i;
        while i < chars.len() && !chars[i].is_whitespace() && chars[i] != '-' && chars[i] != '+' {
            i += 1;
        }
        tokens.push(chars[start..i].iter().collect());
    }
    tokens
}

fn normalize_key_name(lower: &str, original: &str) -> Result<String, ShortcutError> {
    let named = match lower {
        "del" | "delete" => Some("delete"),
        "bs" | "back" | "backspace" => Some("backspace"),
        "ret" | "return" | "enter" => Some("enter"),
        "esc" | "escape" => Some("escape"),
        "tab" => Some("tab"),
        "space" | "spacebar" => Some("space"),
        "left" | "arrowleft" | "←" => Some("left"),
        "right" | "arrowright" | "→" => Some("right"),
        "up" | "arrowup" | "↑" => Some("up"),
        "down" | "arrowdown" | "↓" => Some("down"),
        "home" => Some("home"),
        "end" => Some("end"),
        "pageup" | "pgup" => Some("pageup"),
        "pagedown" | "pgdn" | "pgdown" => Some("pagedown"),
        "comma" => Some(","),
        "period" | "dot" => Some("."),
        "slash" => Some("/"),
        "backslash" => Some("\\"),
        "semicolon" => Some(";"),
        "quote" | "apostrophe" => Some("'"),
        "minus" => Some("-"),
        "plus" => Some("+"),
        "equal" | "equals" => Some("="),
        "hash" => Some("#"),
        "question" => Some("?"),
        _ if is_function_key(lower) => Some(lower),
        _ => None,
    };
    if let Some(n) = named {
        return Ok(n.to_owned());
    }
    let mut chars = original.chars();
    if let (Some(ch), None) = (chars.next(), chars.next()) {
        if ch.is_ascii_graphic() {
            return Ok(ch.to_ascii_lowercase().to_string());
        }
    }
    Err(ShortcutError::UnknownKey(original.to_owned()))
}

fn is_function_key(key: &str) -> bool {
    matches!(
        key,
        "f1" | "f2" | "f3" | "f4" | "f5" | "f6" | "f7" | "f8" | "f9" | "f10" | "f11" | "f12"
    )
}

/// Equivalent keystroke canonicalization for conflict detection (e.g., `#` and `shift-3`,
/// `?` and `shift-/` represent the same physical shortcut on standard layouts).
fn conflict_equivalence_key(normalized: &str) -> &str {
    match normalized {
        "shift-3" => "#",
        "shift-/" => "?",
        other => other,
    }
}

/// Returns `true` when two normalized keystrokes represent the same shortcut.
pub fn keystrokes_conflict(a: &str, b: &str) -> bool {
    conflict_equivalence_key(a) == conflict_equivalence_key(b)
}

/// Formats a normalized (or raw) keystroke into Overlook's human-readable hint string.
///
/// Examples:
/// - `"ctrl-n"` -> `"Ctrl N"`
/// - `"ctrl-shift-r"` -> `"Ctrl Shift R"`
/// - `"delete"` -> `"Del"`
/// - `"shift-3"` -> `"Shift 3"`
/// - `"#"` -> `"#"`
/// - `"/"` -> `"/"`
/// - `"?"` -> `"?"`
/// - `"left"` -> `"←"`
pub fn format_keystroke(raw: &str) -> String {
    let Ok(parsed) = parse_keystroke(raw) else {
        return raw.trim().to_owned();
    };
    let mut parts: Vec<String> = Vec::with_capacity(5);
    if parsed.ctrl {
        parts.push("Ctrl".to_owned());
    }
    if parsed.alt {
        parts.push("Alt".to_owned());
    }
    if parsed.shift {
        parts.push("Shift".to_owned());
    }
    if parsed.cmd {
        parts.push("Cmd".to_owned());
    }
    let key_label = match parsed.key.as_str() {
        "delete" => "Del".to_owned(),
        "backspace" => "Backspace".to_owned(),
        "enter" => "Enter".to_owned(),
        "escape" => "Esc".to_owned(),
        "tab" => "Tab".to_owned(),
        "space" => "Space".to_owned(),
        "left" => "←".to_owned(),
        "right" => "→".to_owned(),
        "up" => "↑".to_owned(),
        "down" => "↓".to_owned(),
        "home" => "Home".to_owned(),
        "end" => "End".to_owned(),
        "pageup" => "PgUp".to_owned(),
        "pagedown" => "PgDn".to_owned(),
        k if is_function_key(k) => k.to_ascii_uppercase(),
        k if k.len() == 1 && k.as_bytes()[0].is_ascii_alphabetic() => k.to_ascii_uppercase(),
        k => k.to_owned(),
    };
    parts.push(key_label);
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outlook_and_gmail_presets_have_zero_conflicts() {
        let outlook = ShortcutPrefs {
            preset: ShortcutPreset::Outlook,
            overrides: BTreeMap::new(),
        };
        assert!(
            find_conflicts(&outlook).is_empty(),
            "Outlook preset should have no conflicts: {:?}",
            find_conflicts(&outlook)
        );

        let gmail = ShortcutPrefs {
            preset: ShortcutPreset::Gmail,
            overrides: BTreeMap::new(),
        };
        assert!(
            find_conflicts(&gmail).is_empty(),
            "Gmail preset should have no conflicts: {:?}",
            find_conflicts(&gmail)
        );
    }

    #[test]
    fn outlook_defaults_preserve_existing_shortcuts_verbatim() {
        let prefs = ShortcutPrefs::default();
        assert_eq!(prefs.preset, ShortcutPreset::Outlook);
        assert_eq!(prefs.binding(ShortcutAction::Compose), "ctrl-n");
        assert_eq!(prefs.binding(ShortcutAction::FocusSearch), "ctrl-e");
        assert_eq!(prefs.binding(ShortcutAction::OpenCommandPalette), "ctrl-k");
        assert_eq!(prefs.binding(ShortcutAction::Reply), "ctrl-r");
        assert_eq!(prefs.binding(ShortcutAction::ReplyAll), "ctrl-shift-r");
        assert_eq!(prefs.binding(ShortcutAction::Forward), "ctrl-f");
        assert_eq!(prefs.binding(ShortcutAction::ToggleRead), "ctrl-u");
        assert_eq!(prefs.binding(ShortcutAction::Undo), "ctrl-z");
        assert_eq!(prefs.binding(ShortcutAction::ShowHistory), "ctrl-h");
        assert_eq!(prefs.binding(ShortcutAction::SyncNow), "f5");
        assert_eq!(prefs.binding(ShortcutAction::RenameFolder), "f2");
        assert_eq!(prefs.binding(ShortcutAction::Archive), "e");
        assert_eq!(prefs.binding(ShortcutAction::Trash), "delete");
        assert_eq!(prefs.binding(ShortcutAction::DeleteForever), "shift-delete");
        assert_eq!(prefs.binding(ShortcutAction::ToggleStar), "s");
        assert_eq!(prefs.binding(ShortcutAction::MoveSelected), "v");
        assert_eq!(prefs.binding(ShortcutAction::NextConversation), "j");
        assert_eq!(prefs.binding(ShortcutAction::PrevConversation), "k");
        assert_eq!(prefs.binding(ShortcutAction::CalendarToday), "t");
        assert_eq!(prefs.binding(ShortcutAction::CalendarAgenda), "a");
    }

    #[test]
    fn gmail_preset_aligns_with_gmail_conventions_and_reuses_shared_keys() {
        let prefs = ShortcutPrefs {
            preset: ShortcutPreset::Gmail,
            overrides: BTreeMap::new(),
        };
        // Reused Overlook/Gmail keys
        assert_eq!(prefs.binding(ShortcutAction::NextConversation), "j");
        assert_eq!(prefs.binding(ShortcutAction::PrevConversation), "k");
        assert_eq!(prefs.binding(ShortcutAction::Archive), "e");
        assert_eq!(prefs.binding(ShortcutAction::ToggleStar), "s");
        assert_eq!(prefs.binding(ShortcutAction::MoveSelected), "v");
        // Gmail-specific mail shortcuts
        assert_eq!(prefs.binding(ShortcutAction::Compose), "c");
        assert_eq!(prefs.binding(ShortcutAction::Reply), "r");
        assert_eq!(prefs.binding(ShortcutAction::ReplyAll), "a");
        assert_eq!(prefs.binding(ShortcutAction::Forward), "f");
        assert_eq!(prefs.binding(ShortcutAction::Trash), "#");
        assert_eq!(prefs.binding(ShortcutAction::ToggleRead), "shift-u");
        assert_eq!(prefs.binding(ShortcutAction::FocusSearch), "/");
        assert_eq!(prefs.binding(ShortcutAction::OpenCommandPalette), "?");
        assert_eq!(prefs.binding(ShortcutAction::Undo), "z");
        // Every single-key Gmail mail shortcut must resolve to MessageList context, never Window!
        for action in [
            ShortcutAction::Compose,
            ShortcutAction::Reply,
            ShortcutAction::ReplyAll,
            ShortcutAction::Forward,
            ShortcutAction::Trash,
            ShortcutAction::ToggleRead,
            ShortcutAction::FocusSearch,
            ShortcutAction::OpenCommandPalette,
            ShortcutAction::Undo,
            ShortcutAction::Archive,
            ShortcutAction::ToggleStar,
            ShortcutAction::MoveSelected,
            ShortcutAction::NextConversation,
            ShortcutAction::PrevConversation,
        ] {
            assert_eq!(
                prefs.effective_context(action),
                ShortcutContext::MessageList,
                "{action:?} in Gmail preset must be scoped to MessageList"
            );
        }
    }

    #[test]
    fn normalization_and_display_formatting() {
        assert_eq!(normalize_keystroke("Ctrl+N").unwrap(), "ctrl-n");
        assert_eq!(normalize_keystroke("shift+ctrl+r").unwrap(), "ctrl-shift-r");
        assert_eq!(normalize_keystroke("Del").unwrap(), "delete");
        assert_eq!(normalize_keystroke("Shift+Del").unwrap(), "shift-delete");
        assert_eq!(normalize_keystroke("shift-3").unwrap(), "shift-3");
        assert_eq!(normalize_keystroke("#").unwrap(), "#");
        assert_eq!(normalize_keystroke("shift-#").unwrap(), "#");
        assert_eq!(normalize_keystroke("/").unwrap(), "/");
        assert_eq!(normalize_keystroke("?").unwrap(), "?");
        assert_eq!(normalize_keystroke("ctrl--").unwrap(), "ctrl--");
        assert_eq!(normalize_keystroke("ctrl-+").unwrap(), "ctrl-+");
        assert_eq!(normalize_keystroke("Escape"), Err(ShortcutError::Reserved("escape".into())));
        assert_eq!(normalize_keystroke("Tab"), Err(ShortcutError::Reserved("tab".into())));
        assert_eq!(normalize_keystroke(""), Err(ShortcutError::Empty));
        assert_eq!(normalize_keystroke("ctrl"), Err(ShortcutError::MissingKey));

        assert_eq!(format_keystroke("ctrl-n"), "Ctrl N");
        assert_eq!(format_keystroke("ctrl-shift-r"), "Ctrl Shift R");
        assert_eq!(format_keystroke("delete"), "Del");
        assert_eq!(format_keystroke("shift-delete"), "Shift Del");
        assert_eq!(format_keystroke("shift-3"), "Shift 3");
        assert_eq!(format_keystroke("#"), "#");
        assert_eq!(format_keystroke("/"), "/");
        assert_eq!(format_keystroke("?"), "?");
        assert_eq!(format_keystroke("left"), "←");
        assert_eq!(format_keystroke("right"), "→");
        assert_eq!(format_keystroke("f5"), "F5");
        assert_eq!(format_keystroke("e"), "E");
    }

    #[test]
    fn conflict_detection_respects_disjoint_vs_overlapping_contexts() {
        let prefs = ShortcutPrefs {
            preset: ShortcutPreset::Gmail,
            overrides: BTreeMap::new(),
        };
        // Disjoint: `a` is ReplyAll in MessageList and CalendarAgenda in Calendar — no conflict!
        assert!(conflict_for(&prefs, ShortcutAction::CalendarAgenda, "a").is_empty());
        assert!(conflict_for(&prefs, ShortcutAction::CalendarToday, "e").is_empty());

        // Same context: MessageList + MessageList -> conflict!
        assert_eq!(
            conflict_for(&prefs, ShortcutAction::Compose, "e"),
            vec![ShortcutAction::Archive]
        );
        // Equivalent keystrokes: `shift-3` conflicts with `#` (Trash) in MessageList!
        assert_eq!(
            conflict_for(&prefs, ShortcutAction::Archive, "shift-3"),
            vec![ShortcutAction::Trash]
        );
        // Equivalent keystrokes: `shift-/` conflicts with `?` (OpenCommandPalette) in MessageList!
        assert_eq!(
            conflict_for(&prefs, ShortcutAction::FocusSearch, "shift-/"),
            vec![ShortcutAction::OpenCommandPalette]
        );

        // Overlapping contexts: Window + MessageList -> conflict!
        let outlook = ShortcutPrefs::default();
        assert_eq!(
            conflict_for(&outlook, ShortcutAction::Archive, "ctrl-n"),
            vec![ShortcutAction::Compose]
        );
        // Overlapping contexts: Window + Calendar -> conflict!
        assert_eq!(
            conflict_for(&outlook, ShortcutAction::CalendarToday, "ctrl-n"),
            vec![ShortcutAction::Compose]
        );
        // Re-binding an action to its own current keystroke is never a self-conflict.
        assert!(conflict_for(&outlook, ShortcutAction::Compose, "ctrl-n").is_empty());
    }

    #[test]
    fn overrides_checked_persistence_and_reset() {
        let mut prefs = ShortcutPrefs::default();
        // Conflicting override is rejected cleanly.
        let err = prefs
            .set_override_checked(ShortcutAction::Forward, "ctrl-r")
            .unwrap_err();
        assert!(matches!(err, ShortcutSetError::Conflict { .. }));
        assert_eq!(prefs.binding(ShortcutAction::Forward), "ctrl-f");

        // Non-conflicting override succeeds.
        assert_eq!(
            prefs
                .set_override_checked(ShortcutAction::Compose, "ctrl-m")
                .unwrap(),
            "ctrl-m"
        );
        assert!(prefs.is_overridden(ShortcutAction::Compose));
        assert_eq!(prefs.hint(ShortcutAction::Compose), "Ctrl M");
        assert_eq!(prefs.override_count(), 1);

        // JSON round-trip preserves preset and overrides.
        let json = serde_json::to_string(&prefs).unwrap();
        let decoded: ShortcutPrefs = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, prefs);

        // Single-action reset restores preset default.
        assert!(reset_action(&mut prefs, ShortcutAction::Compose));
        assert!(!reset_action(&mut prefs, ShortcutAction::Compose));
        assert!(!prefs.is_overridden(ShortcutAction::Compose));
        assert_eq!(prefs.binding(ShortcutAction::Compose), "ctrl-n");

        // Multiple overrides and reset_all.
        prefs.set_override_checked(ShortcutAction::Compose, "ctrl-m").unwrap();
        prefs.set_override_checked(ShortcutAction::SyncNow, "f6").unwrap();
        assert_eq!(prefs.override_count(), 2);
        assert!(reset_all(&mut prefs));
        assert!(!reset_all(&mut prefs));
        assert_eq!(prefs.override_count(), 0);
    }

    #[test]
    fn switching_preset_detects_conflicts_with_existing_overrides() {
        let mut prefs = ShortcutPrefs::default();
        // `c` is free in Outlook preset.
        prefs.set_override_checked(ShortcutAction::SyncNow, "c").unwrap();
        assert!(find_conflicts(&prefs).is_empty());

        // Switching to Gmail makes Compose = `c`, conflicting with SyncNow = `c`.
        prefs.set_preset(ShortcutPreset::Gmail);
        let conflicts = find_conflicts(&prefs);
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].action_a, ShortcutAction::Compose);
        assert_eq!(conflicts[0].action_b, ShortcutAction::SyncNow);
    }

    #[test]
    fn prefs_deserialization_upgrades_existing_profiles_cleanly() {
        // Older serialized Prefs JSON without `shortcuts` must deserialize to Outlook default.
        let legacy_json = r#"{"clock_24h":true}"#;
        let prefs: crate::Prefs = serde_json::from_str(legacy_json).unwrap();
        assert_eq!(prefs.shortcuts, ShortcutPrefs::default());
        assert_eq!(prefs.shortcuts.preset, ShortcutPreset::Outlook);
    }
}
