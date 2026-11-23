//! The local reminder inbox, independent of the windows displaying it.

use overlook_core::{tz, Calendar, CalendarId, Event, EventId, EventStatus, ResponseStatus};
use overlook_store::Store;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

pub(crate) const MINUTE: i64 = 60_000;
const DAY: i64 = 24 * 60 * MINUTE;
const SETTING: &str = "calendar.reminders";

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct Preferences {
    pub enabled: bool,
    pub default_minutes: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Reminder {
    pub event: Event,
    pub calendar: String,
}

pub(crate) struct Candidate {
    reminder: Reminder,
    fire_at: Option<i64>,
    visible: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Status {
    Due,
    SnoozedUntil(i64),
    Dismissed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Record {
    start: i64,
    status: Status,
}

#[derive(Default)]
pub(crate) struct Inbox {
    records: HashMap<EventId, Record>,
    due: Vec<Reminder>,
}

impl Inbox {
    pub fn load(store: &Store) -> Self {
        match store.setting(SETTING) {
            Ok(records) => Self {
                records: records.unwrap_or_default(),
                due: Vec::new(),
            },
            Err(error) => {
                tracing::warn!(%error, "could not load saved calendar reminders");
                Self::default()
            }
        }
    }

    pub fn save(&self, store: Arc<Store>) {
        let records = self.records.clone();
        crate::persist::later(move || {
            if let Err(error) = store.set_setting(SETTING, &records) {
                tracing::warn!(%error, "could not save calendar reminders");
            }
        });
    }

    pub fn due(&self) -> &[Reminder] {
        &self.due
    }

    pub fn tracked(&self) -> Vec<EventId> {
        self.records.keys().cloned().collect()
    }

    /// Snoozes that have already expired are handled by the normal refresh, including
    /// reminders on hidden calendars that can only return once visible again.
    pub fn next_snooze_after(&self, now: i64) -> Option<i64> {
        self.records
            .values()
            .filter_map(|record| match record.status {
                Status::SnoozedUntil(until) if until > now => Some(until),
                _ => None,
            })
            .min()
    }

    /// Refresh the inbox from one successful query. Return whether to persist and which
    /// occurrences have newly become due. A moved occurrence gets a fresh schedule.
    pub fn refresh(
        &mut self,
        candidates: Vec<Candidate>,
        now: i64,
        enabled: bool,
    ) -> (bool, Vec<Reminder>) {
        let before = self.records.clone();
        let shown: HashSet<_> = self
            .due
            .iter()
            .map(|r| (&r.event.id, r.event.start))
            .collect();
        let mut next = Vec::new();
        let mut newly_due = Vec::new();
        self.records.clear();
        for Candidate {
            reminder,
            fire_at,
            visible,
        } in candidates
        {
            let event = &reminder.event;
            let status = before
                .get(&event.id)
                .filter(|r| r.start == event.start)
                .map(|r| r.status);
            let status = match status {
                Some(Status::Dismissed) if event.end <= now => continue,
                Some(Status::SnoozedUntil(until)) if enabled && visible && now >= until => {
                    Status::Due
                }
                Some(status) => status,
                None if enabled && event.end > now && fire_at.is_some_and(|at| at <= now) => {
                    Status::Due
                }
                None => continue,
            };
            self.records.insert(
                event.id.clone(),
                Record {
                    start: event.start,
                    status,
                },
            );
            if visible && status == Status::Due {
                if enabled && !shown.contains(&(&event.id, event.start)) {
                    newly_due.push(reminder.clone());
                }
                next.push(reminder);
            }
        }
        next.sort_by(|a, b| (a.event.start, &a.event.id).cmp(&(b.event.start, &b.event.id)));
        self.due = next;
        (before != self.records, newly_due)
    }

    pub fn snooze(&mut self, id: &EventId, now: i64, minutes: i64) -> bool {
        let Some(record) = self.records.get(id) else {
            return false;
        };
        let until = now.saturating_add(minutes.saturating_mul(MINUTE));
        // For an upcoming event, bring the reminder back by the start time.
        let until = if now < record.start {
            until.min(record.start)
        } else {
            until
        };
        self.change_due(Some(id), Status::SnoozedUntil(until))
    }

    pub fn dismiss(&mut self, id: Option<&EventId>) -> bool {
        self.change_due(id, Status::Dismissed)
    }

    fn change_due(&mut self, id: Option<&EventId>, status: Status) -> bool {
        let mut changed = false;
        self.due.retain(|reminder| {
            let event_id = &reminder.event.id;
            if id.is_some_and(|id| id != event_id) {
                return true;
            }
            if let Some(record) = self.records.get_mut(event_id) {
                record.status = status;
            }
            changed = true;
            false
        });
        changed
    }
}

/// Read reminder candidates across all accounts, including tracked items that are now
/// outside the scheduling range. Cancelled, hidden and removed meetings leave the inbox.
pub(crate) fn candidates(
    store: &Store,
    tracked: Vec<EventId>,
    now: i64,
    prefs: Preferences,
) -> anyhow::Result<Vec<Candidate>> {
    let tz_key = tz::cache_key();
    let mut events = HashMap::new();
    let mut calendars = HashMap::new();
    for account in store.accounts()? {
        let account_calendars = store.calendars(&account.id)?;
        calendars.extend(account_calendars.into_iter().map(|c| (c.id.clone(), c)));
        if !prefs.enabled
            || store
                .calendar_connection(&account.id)?
                .is_some_and(|connection| {
                    !store
                        .caldav_timezone_matches(&connection.id, &tz_key)
                        .unwrap_or(false)
                })
        {
            continue;
        }
        for event in store.events_starting_between(&account.id, now - DAY, now + 28 * DAY + 1)? {
            if let Some(candidate) = candidate(event, &calendars, Some(prefs.default_minutes)) {
                events.insert(candidate.reminder.event.id.clone(), candidate);
            }
        }
    }
    for id in tracked {
        if let std::collections::hash_map::Entry::Vacant(entry) = events.entry(id.clone()) {
            if let Some(candidate) = store
                .event(&id)?
                .and_then(|event| candidate(event, &calendars, None))
            {
                entry.insert(candidate);
            }
        }
    }
    Ok(events.into_values().collect())
}

fn candidate(
    event: Event,
    calendars: &HashMap<CalendarId, Calendar>,
    default_minutes: Option<u32>,
) -> Option<Candidate> {
    if event.status == EventStatus::Cancelled
        || event.all_day
        || event.is_working_location()
        || event.my_response() == Some(ResponseStatus::Declined)
        || event.reminders.as_ref().is_some_and(Vec::is_empty)
    {
        return None;
    }
    let calendar = calendars.get(&event.calendar_id)?;
    let fire_at = default_minutes.and_then(|minutes| {
        let fallback = [i32::try_from(minutes).unwrap_or(i32::MAX)];
        let defaults = if !calendar.default_reminders.is_empty() {
            &calendar.default_reminders
        } else if minutes > 0 {
            &fallback[..]
        } else {
            &[]
        };
        event
            .effective_reminders(defaults)
            .iter()
            .max()
            .map(|minutes| event.start.saturating_sub(i64::from(*minutes) * MINUTE))
    });
    Some(Candidate {
        reminder: Reminder {
            event,
            calendar: calendar.name.clone(),
        },
        fire_at,
        visible: calendar.visible,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use overlook_core::{Account, AccountId, ProviderKind};

    const NOW: i64 = 2_000 * DAY;
    const PREFS: Preferences = Preferences {
        enabled: true,
        default_minutes: 10,
    };

    fn fixture() -> (Arc<Store>, Event) {
        let store = Arc::new(Store::open_in_memory().unwrap());
        let account = Account {
            id: AccountId::from("reminders-account"),
            email: "alex@example.test".into(),
            display_name: "Alex".into(),
            provider: ProviderKind::Sample,
            created_at: NOW,
        };
        store.upsert_account(&account).unwrap();
        let calendar = Calendar {
            id: CalendarId::from("reminders-calendar"),
            account_id: account.id.clone(),
            name: "Work".into(),
            provider_id: None,
            color: None,
            is_primary: true,
            can_edit: true,
            visible: true,
            sort_order: 0,
            default_reminders: vec![30, 5],
        };
        store
            .replace_calendars(&account.id, &[calendar.clone()])
            .unwrap();
        let event = Event {
            id: EventId::from("reminders-event"),
            account_id: account.id,
            calendar_id: calendar.id,
            summary: "Design review".into(),
            start: NOW + 20 * MINUTE,
            end: NOW + 50 * MINUTE,
            provider_id: None,
            ical_uid: None,
            description: None,
            location: None,
            all_day: false,
            status: EventStatus::Confirmed,
            organizer: None,
            attendees: Vec::new(),
            html_link: None,
            conference_url: None,
            recurring_event_id: None,
            updated: NOW,
            event_type: None,
            color: None,
            reminders: None,
            original_start: None,
            wants_conference: false,
            pending: false,
        };
        store.upsert_events(&[event.clone()]).unwrap();
        (store, event)
    }

    fn refresh(inbox: &mut Inbox, store: &Store, now: i64) -> Vec<Reminder> {
        inbox
            .refresh(
                candidates(store, inbox.tracked(), now, PREFS).unwrap(),
                now,
                true,
            )
            .1
    }

    #[test]
    fn snoozed_reminders_return_after_a_restart_and_the_end_of_the_event() {
        let (store, event) = fixture();
        let mut inbox = Inbox::default();
        assert_eq!(refresh(&mut inbox, &store, NOW).len(), 1);
        let already_started = event.start + MINUTE;
        assert!(refresh(&mut inbox, &store, already_started).is_empty());
        assert_eq!(inbox.due().len(), 1);

        let until = already_started + 60 * MINUTE;
        assert!(inbox.snooze(&event.id, already_started, 60));
        inbox.save(store.clone());
        crate::persist::flush();
        let mut reopened = Inbox::load(&store);
        assert!(refresh(&mut reopened, &store, until - 1).is_empty());
        assert!(reopened.due().is_empty());
        assert_eq!(refresh(&mut reopened, &store, until)[0].event.id, event.id);
        assert!(refresh(&mut reopened, &store, until + MINUTE).is_empty());
    }

    #[test]
    fn snoozing_an_upcoming_event_caps_the_deadline_at_its_start() {
        for (minutes_before, minutes_to_snooze, minutes_before_return) in [(3, 5, 0), (10, 5, 5)] {
            let (store, event) = fixture();
            let mut inbox = Inbox::default();
            let now = event.start - minutes_before * MINUTE;
            assert_eq!(refresh(&mut inbox, &store, now).len(), 1);
            assert!(inbox.snooze(&event.id, now, minutes_to_snooze));
            inbox.save(store.clone());
            crate::persist::flush();
            let mut inbox = Inbox::load(&store);

            let deadline = event.start - minutes_before_return * MINUTE;
            assert_eq!(inbox.next_snooze_after(now), Some(deadline));
            assert!(refresh(&mut inbox, &store, deadline - 1).is_empty());
            assert_eq!(refresh(&mut inbox, &store, deadline)[0].event.id, event.id);
            assert_eq!(inbox.next_snooze_after(deadline), None);
        }
    }

    #[test]
    fn dismiss_all_suppresses_each_due_occurrence_but_a_moved_event_can_remind_again() {
        let (store, mut first) = fixture();
        let mut second = first.clone();
        second.id = EventId::from("reminders-second");
        store.upsert_events(&[second]).unwrap();
        let mut inbox = Inbox::default();
        assert_eq!(refresh(&mut inbox, &store, NOW).len(), 2);
        assert!(inbox.dismiss(None));
        inbox.save(store.clone());
        crate::persist::flush();
        let mut reopened = Inbox::load(&store);
        assert!(refresh(&mut reopened, &store, NOW + 15 * MINUTE).is_empty());
        assert!(reopened.due().is_empty());
        store
            .set_calendar_visible(&first.account_id, &first.calendar_id, false)
            .unwrap();
        assert!(refresh(&mut reopened, &store, NOW + 15 * MINUTE).is_empty());
        store
            .set_calendar_visible(&first.account_id, &first.calendar_id, true)
            .unwrap();
        assert!(refresh(&mut reopened, &store, NOW + 15 * MINUTE).is_empty());

        first.start += DAY;
        first.end += DAY;
        store.upsert_events(&[first.clone()]).unwrap();
        assert_eq!(
            refresh(&mut reopened, &store, NOW + DAY)[0].event.id,
            first.id
        );
        assert_eq!(reopened.due().len(), 1);
    }

    #[test]
    fn cancelled_and_hidden_events_leave_the_inbox_and_explicit_offsets_take_precedence() {
        let (store, mut event) = fixture();
        event.reminders = Some(vec![5]);
        store.upsert_events(&[event.clone()]).unwrap();
        let mut inbox = Inbox::default();
        assert!(refresh(&mut inbox, &store, NOW).is_empty());
        let when = NOW + 15 * MINUTE;
        assert_eq!(refresh(&mut inbox, &store, when).len(), 1);

        store
            .set_calendar_visible(&event.account_id, &event.calendar_id, false)
            .unwrap();
        assert!(refresh(&mut inbox, &store, when).is_empty());
        assert!(inbox.due().is_empty());
        store
            .set_calendar_visible(&event.account_id, &event.calendar_id, true)
            .unwrap();
        assert_eq!(refresh(&mut inbox, &store, when).len(), 1);

        event.status = EventStatus::Cancelled;
        store.upsert_events(&[event]).unwrap();
        assert!(refresh(&mut inbox, &store, when).is_empty());
        assert!(inbox.due().is_empty());
    }
}
