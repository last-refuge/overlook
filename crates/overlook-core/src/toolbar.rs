//! Profile-local toolbar order. Window overflow is a presentation detail, never a saved edit.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolbarModule {
    Mail,
    Calendar,
    Contacts,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolbarAction {
    NewMessage,
    Archive,
    DeleteMail,
    MoveMail,
    ToggleRead,
    ToggleStar,
    Reply,
    ReplyAll,
    Forward,
    JumpLatest,
    PreviousConversation,
    NextConversation,
    MoveFocus,
    AlwaysMoveFocus,
    NewEvent,
    Today,
    PreviousPeriod,
    NextPeriod,
    Day,
    Week,
    Month,
    Agenda,
    EditEvent,
    DeleteEvent,
    NewContact,
    ContactMessage,
    ContactMail,
    EditContact,
    DeleteContact,
    Sync,
    Appearance,
    Layout,
    #[serde(other)]
    Unknown,
}

/// Commands that belong together. The strip draws a separator where the group changes, so
/// the grouping follows any order the person saves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolbarGroup {
    /// New message, event or contact.
    Create,
    /// Archive, delete, move and edit: where an item goes.
    Organize,
    /// Read and starred state.
    Mark,
    /// Reply, reply all, forward, and writing to a contact.
    Respond,
    /// Moving through conversations or calendar periods.
    Navigate,
    /// The calendar's day, week, month and agenda views.
    View,
    /// Sync, appearance and layout.
    App,
}

impl ToolbarAction {
    pub fn group(self) -> ToolbarGroup {
        use ToolbarAction::*;
        match self {
            NewMessage | NewEvent | NewContact => ToolbarGroup::Create,
            Archive | DeleteMail | MoveMail | MoveFocus | AlwaysMoveFocus | EditEvent
            | DeleteEvent | EditContact | DeleteContact => ToolbarGroup::Organize,
            ToggleRead | ToggleStar => ToolbarGroup::Mark,
            Reply | ReplyAll | Forward | ContactMessage | ContactMail => ToolbarGroup::Respond,
            JumpLatest | PreviousConversation | NextConversation | Today | PreviousPeriod
            | NextPeriod => ToolbarGroup::Navigate,
            Day | Week | Month | Agenda => ToolbarGroup::View,
            Sync | Appearance | Layout | Unknown => ToolbarGroup::App,
        }
    }
}

/// How a run of strip commands is drawn: which commands get a separator before them, and
/// where the trailing navigation and app commands start, which sit at the far end.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StripPlan {
    /// One entry per command: true when a separator goes before it.
    pub separators: Vec<bool>,
    /// The first command of the trailing run of navigation and app commands. `None` when
    /// the strip doesn't end in one, or holds nothing else.
    pub trailing: Option<usize>,
}

impl StripPlan {
    pub fn of(actions: &[ToolbarAction]) -> Self {
        let tail = |a: &ToolbarAction| matches!(a.group(), ToolbarGroup::Navigate | ToolbarGroup::App);
        let run = actions.iter().rev().take_while(|a| tail(a)).count();
        let trailing = (run > 0 && run < actions.len()).then(|| actions.len() - run);
        let separators = (0..actions.len())
            .map(|i| i > 0 && Some(i) != trailing && actions[i].group() != actions[i - 1].group())
            .collect();
        Self { separators, trailing }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolbarArea {
    Strip,
    More,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ToolbarLayout {
    pub strip: Vec<ToolbarAction>,
    pub more: Vec<ToolbarAction>,
}

impl ToolbarModule {
    pub fn default_layout(self) -> ToolbarLayout {
        use ToolbarAction::*;
        let strip = match self {
            Self::Mail => vec![
                NewMessage,
                Archive,
                DeleteMail,
                MoveMail,
                ToggleRead,
                ToggleStar,
                Reply,
                ReplyAll,
                Forward,
                JumpLatest,
                PreviousConversation,
                NextConversation,
            ],
            Self::Calendar => vec![
                NewEvent,
                Today,
                PreviousPeriod,
                NextPeriod,
                Day,
                Week,
                Month,
                Agenda,
                EditEvent,
                DeleteEvent,
            ],
            Self::Contacts => vec![
                NewContact,
                ContactMessage,
                ContactMail,
                EditContact,
                DeleteContact,
            ],
        };
        ToolbarLayout {
            strip: strip
                .into_iter()
                .chain([Sync, Appearance, Layout])
                .collect(),
            more: if self == Self::Mail {
                vec![MoveFocus, AlwaysMoveFocus]
            } else {
                vec![]
            },
        }
    }
}

impl ToolbarLayout {
    pub fn actions(&self, area: ToolbarArea) -> &[ToolbarAction] {
        match area {
            ToolbarArea::Strip => &self.strip,
            ToolbarArea::More => &self.more,
        }
    }

    fn actions_mut(&mut self, area: ToolbarArea) -> &mut Vec<ToolbarAction> {
        match area {
            ToolbarArea::Strip => &mut self.strip,
            ToolbarArea::More => &mut self.more,
        }
    }

    /// Ignore removed/unknown commands and duplicates; new commands start in More.
    pub fn normalized(mut self, module: ToolbarModule) -> Self {
        let defaults = module.default_layout();
        let allowed: HashSet<_> = defaults
            .strip
            .iter()
            .chain(&defaults.more)
            .copied()
            .collect();
        let mut seen = HashSet::new();
        for actions in [&mut self.strip, &mut self.more] {
            actions.retain(|action| allowed.contains(action) && seen.insert(*action));
        }
        self.more.extend(
            defaults
                .strip
                .into_iter()
                .chain(defaults.more)
                .filter(|a| seen.insert(*a)),
        );
        self
    }

    /// Move before a stable command identity, or append. Indices can change during a drag.
    pub fn move_action(
        &mut self,
        action: ToolbarAction,
        area: ToolbarArea,
        before: Option<ToolbarAction>,
    ) -> bool {
        if before == Some(action) || !self.strip.contains(&action) && !self.more.contains(&action) {
            return false;
        }
        let old = self.clone();
        self.strip.retain(|a| *a != action);
        self.more.retain(|a| *a != action);
        let target = self.actions_mut(area);
        let index = before
            .and_then(|b| target.iter().position(|a| *a == b))
            .unwrap_or(target.len());
        target.insert(index, action);
        *self != old
    }

    pub fn nudge(&mut self, action: ToolbarAction, area: ToolbarArea, forward: bool) -> bool {
        let target = self.actions_mut(area);
        let Some(index) = target.iter().position(|a| *a == action) else {
            return false;
        };
        let other = if forward {
            index.checked_add(1).filter(|i| *i < target.len())
        } else {
            index.checked_sub(1)
        };
        let Some(other) = other else {
            return false;
        };
        target.swap(index, other);
        true
    }

    /// The largest prefix that fits, with `separator` counted wherever the group changes.
    /// Temporarily overflowed commands keep their saved position.
    pub fn visible_count(
        &self,
        available: f32,
        width: impl Fn(ToolbarAction) -> f32,
        separator: f32,
    ) -> usize {
        let mut used = 0.;
        let mut previous: Option<ToolbarGroup> = None;
        self.strip
            .iter()
            .take_while(|action| {
                if previous.is_some_and(|group| group != action.group()) {
                    used += separator;
                }
                previous = Some(action.group());
                used += width(**action);
                used <= available.max(0.)
            })
            .count()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ToolbarLayouts(pub BTreeMap<ToolbarModule, ToolbarLayout>);

impl ToolbarLayouts {
    pub fn layout(&self, module: ToolbarModule) -> ToolbarLayout {
        self.0
            .get(&module)
            .cloned()
            .map(|l| l.normalized(module))
            .unwrap_or_else(|| module.default_layout())
    }

    pub fn set(&mut self, module: ToolbarModule, layout: ToolbarLayout) {
        self.0.insert(module, layout.normalized(module));
    }

    pub fn reset(&mut self, module: ToolbarModule) {
        self.0.remove(&module);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ToolbarAction::*;

    #[test]
    fn moving_between_strip_and_more_preserves_every_command_once() {
        let mut layout = ToolbarModule::Mail.default_layout();
        let total = layout.strip.len() + layout.more.len();
        assert!(layout.move_action(Archive, ToolbarArea::More, Some(MoveFocus)));
        assert!(!layout.strip.contains(&Archive));
        assert_eq!(layout.more[0], Archive);
        assert!(layout.move_action(MoveFocus, ToolbarArea::Strip, Some(NewMessage)));
        assert_eq!(layout.strip[0], MoveFocus);
        assert_eq!(layout.strip.len() + layout.more.len(), total);
        assert_eq!(layout.clone().normalized(ToolbarModule::Mail), layout);
    }

    #[test]
    fn reorder_uses_identity_after_removal_in_both_directions() {
        let mut layout = ToolbarModule::Mail.default_layout();
        assert!(layout.move_action(NewMessage, ToolbarArea::Strip, Some(MoveMail)));
        assert_eq!(
            &layout.strip[..4],
            &[Archive, DeleteMail, NewMessage, MoveMail]
        );
        assert!(layout.move_action(MoveMail, ToolbarArea::Strip, Some(Archive)));
        assert_eq!(
            &layout.strip[..4],
            &[MoveMail, Archive, DeleteMail, NewMessage]
        );
        assert!(!layout.move_action(Archive, ToolbarArea::Strip, Some(Archive)));
        assert!(!layout.move_action(NewEvent, ToolbarArea::Strip, None));
    }

    #[test]
    fn keyboard_moves_and_empty_strip_are_supported() {
        let mut layout = ToolbarModule::Contacts.default_layout();
        assert!(!layout.nudge(NewContact, ToolbarArea::Strip, false));
        assert!(layout.nudge(NewContact, ToolbarArea::Strip, true));
        for action in layout.strip.clone() {
            layout.move_action(action, ToolbarArea::More, None);
        }
        assert!(layout.strip.is_empty());
        assert!(layout.move_action(NewContact, ToolbarArea::Strip, None));
        assert_eq!(layout.strip, vec![NewContact]);
    }

    #[test]
    fn loading_filters_unknown_cross_module_and_duplicate_actions() {
        let layout: ToolbarLayout = serde_json::from_str(r#"{"strip":["archive","archive","new_event","future_command"],"more":["archive","reply"]}"#).unwrap();
        let layout = layout.normalized(ToolbarModule::Mail);
        assert_eq!(layout.strip, vec![Archive]);
        assert_eq!(layout.more[0], Reply);
        let all: HashSet<_> = layout.strip.iter().chain(&layout.more).collect();
        assert_eq!(
            all.len(),
            ToolbarModule::Mail.default_layout().strip.len() + 2
        );
        assert!(!all.contains(&NewEvent));
        assert!(!all.contains(&Unknown));
    }

    #[test]
    fn preferences_round_trip_independent_module_layouts_and_reset() {
        let mut prefs = ToolbarLayouts::default();
        let mut mail = prefs.layout(ToolbarModule::Mail);
        mail.move_action(Sync, ToolbarArea::More, None);
        prefs.set(ToolbarModule::Mail, mail.clone());
        let saved = serde_json::to_string(&prefs).unwrap();
        let mut loaded: ToolbarLayouts = serde_json::from_str(&saved).unwrap();
        assert_eq!(loaded.layout(ToolbarModule::Mail), mail);
        assert_eq!(
            loaded.layout(ToolbarModule::Calendar),
            ToolbarModule::Calendar.default_layout()
        );
        loaded.reset(ToolbarModule::Mail);
        assert_eq!(
            loaded.layout(ToolbarModule::Mail),
            ToolbarModule::Mail.default_layout()
        );
    }

    #[test]
    fn overflow_is_reversible_and_never_edits_preferences() {
        let layout = ToolbarModule::Mail.default_layout();
        let original = layout.clone();
        assert_eq!(layout.visible_count(95., |_| 30., 0.), 3);
        assert_eq!(layout.visible_count(0., |_| 30., 0.), 0);
        assert_eq!(layout.visible_count(5000., |_| 30., 0.), layout.strip.len());
        // New message, then a separator before Archive and Delete.
        assert_eq!(layout.visible_count(95., |_| 30., 10.), 2);
        assert_eq!(layout, original);
    }

    #[test]
    fn groups_are_separated_and_navigation_and_app_commands_trail() {
        let mail = ToolbarModule::Mail.default_layout();
        let plan = StripPlan::of(&mail.strip);
        let before: Vec<_> = mail
            .strip
            .iter()
            .zip(&plan.separators)
            .filter(|(_, sep)| **sep)
            .map(|(a, _)| *a)
            .collect();
        // Navigation starts the trailing run, so the spacer stands in for its separator.
        assert_eq!(before, vec![Archive, ToggleRead, Reply, Sync]);
        assert_eq!(plan.trailing.map(|i| mail.strip[i]), Some(JumpLatest));

        let calendar = ToolbarModule::Calendar.default_layout();
        let plan = StripPlan::of(&calendar.strip);
        assert_eq!(plan.trailing.map(|i| calendar.strip[i]), Some(Sync));
        assert!(plan.separators[1], "Today starts its own group after New event");

        // Any saved order keeps working: groups split wherever they meet.
        let plan = StripPlan::of(&[Reply, Archive, Reply]);
        assert_eq!(plan.separators, vec![false, true, true]);
        assert_eq!(plan.trailing, None);
        // A strip of navigation and app commands only is left as it is.
        assert_eq!(StripPlan::of(&[Sync, NextConversation]).trailing, None);
        assert_eq!(StripPlan::of(&[]).trailing, None);
        assert!(StripPlan::of(&[]).separators.is_empty());
    }
}
