//! Calendar domain: calendars, events, attendees, and responses.

use crate::ids::*;
use crate::model::Address;
use crate::time::Timestamp;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Calendar {
    pub id: CalendarId,
    pub account_id: AccountId,
    pub provider_id: Option<ProviderId>,
    pub name: String,
    /// Hex color from the provider, e.g. `#9fe1e7`.
    pub color: Option<String>,
    pub is_primary: bool,
    pub can_edit: bool,
    /// Shown in the calendar views (user toggle).
    pub visible: bool,
    pub sort_order: i32,
    /// Default popup reminders in minutes before the start, from the provider.
    #[serde(default)]
    pub default_reminders: Vec<i32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ResponseStatus {
    #[default]
    NeedsAction,
    Accepted,
    Tentative,
    Declined,
}

impl ResponseStatus {
    pub fn as_key(self) -> &'static str {
        match self {
            ResponseStatus::NeedsAction => "needs-action",
            ResponseStatus::Accepted => "accepted",
            ResponseStatus::Tentative => "tentative",
            ResponseStatus::Declined => "declined",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ResponseStatus::NeedsAction => "Not responded",
            ResponseStatus::Accepted => "Accepted",
            ResponseStatus::Tentative => "Tentative",
            ResponseStatus::Declined => "Declined",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum EventStatus {
    #[default]
    Confirmed,
    Tentative,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attendee {
    pub email: String,
    pub name: Option<String>,
    pub response: ResponseStatus,
    pub is_self: bool,
    pub is_organizer: bool,
    pub optional: bool,
}

impl Attendee {
    pub fn display_name(&self) -> String {
        Address { name: self.name.clone(), email: self.email.clone() }.display_name()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    pub id: EventId,
    pub account_id: AccountId,
    pub calendar_id: CalendarId,
    pub provider_id: Option<ProviderId>,
    pub ical_uid: Option<String>,
    pub summary: String,
    pub description: Option<String>,
    pub location: Option<String>,
    /// Start, inclusive. For all-day events, local midnight of the first day.
    pub start: Timestamp,
    /// End, exclusive. For all-day events, local midnight after the last day.
    pub end: Timestamp,
    pub all_day: bool,
    pub status: EventStatus,
    pub organizer: Option<Address>,
    pub attendees: Vec<Attendee>,
    pub html_link: Option<String>,
    pub conference_url: Option<String>,
    pub recurring_event_id: Option<String>,
    pub updated: Timestamp,
    /// Provider event type: `default`, `working-location`, `out-of-office`, `focus-time`,
    /// `birthday`. Working-location entries are shown in the day header, not as events.
    pub event_type: Option<String>,
    /// Event-specific color (hex) when the provider has one; otherwise the calendar's.
    pub color: Option<String>,
    /// Popup reminders in minutes before the start. `None` means use the calendar default.
    pub reminders: Option<Vec<i32>>,
    /// For an instance of a recurring event: when it was originally scheduled, before any
    /// move of this one occurrence. Splitting a series happens at this point.
    #[serde(default)]
    pub original_start: Option<Timestamp>,
    /// Ask the provider to attach a video conference (Google Meet) when this is saved.
    #[serde(default)]
    pub wants_conference: bool,
    /// Set while a local change (RSVP, edit) is waiting to reach the provider.
    pub pending: bool,
}

impl Event {
    pub fn duration_minutes(&self) -> i64 {
        (self.end.saturating_sub(self.start) / 60_000).max(0)
    }

    pub fn my_attendance(&self) -> Option<&Attendee> {
        self.attendees.iter().find(|a| a.is_self)
    }

    pub fn my_response(&self) -> Option<ResponseStatus> {
        self.my_attendance().map(|a| a.response)
    }

    /// An invitation is an event someone else organized where we are an attendee.
    pub fn is_invitation(&self) -> bool {
        self.my_attendance().is_some_and(|a| !a.is_organizer) && self.organizer.as_ref().is_some_and(|_| true)
    }

    /// Effective reminder offsets (minutes before start) for this event.
    pub fn effective_reminders<'a>(&'a self, calendar_default: &'a [i32]) -> &'a [i32] {
        match &self.reminders {
            Some(r) => r.as_slice(),
            None => calendar_default,
        }
    }

    /// Whether the user may change this event (owner of the calendar and organizer, or
    /// a personal event without guests).
    pub fn editable_by(&self, owner_email: &str, calendar: &Calendar) -> bool {
        if !calendar.can_edit {
            return false;
        }
        let organizer_is_me = self.organizer.as_ref().is_none_or(|o| o.email.eq_ignore_ascii_case(owner_email));
        organizer_is_me || self.my_attendance().is_some_and(|a| a.is_organizer)
    }

    pub fn is_working_location(&self) -> bool {
        self.event_type.as_deref() == Some("working-location")
    }

    pub fn display_title(&self) -> String {
        if self.summary.trim().is_empty() { "(No title)".to_owned() } else { self.summary.clone() }
    }

    /// Whether the event overlaps the half-open range `[from, to)`.
    pub fn overlaps(&self, from: Timestamp, to: Timestamp) -> bool {
        self.start < to && self.end > from
    }
}

/// Generates a Google-compatible event id (base32hex, lowercase, 26 chars) so creation
/// is idempotent: a retried insert with the same id is rejected as a duplicate, not
/// created twice.
pub fn new_provider_event_id() -> String {
    const ALPHABET: &[u8; 32] = b"0123456789abcdefghijklmnopqrstuv";
    let bytes = uuid::Uuid::new_v4();
    let mut out = String::with_capacity(26);
    let mut acc: u32 = 0;
    let mut bits = 0;
    for b in bytes.as_bytes() {
        acc = (acc << 8) | u32::from(*b);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ALPHABET[((acc >> bits) & 31) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(ALPHABET[((acc << (5 - bits)) & 31) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_ids_are_base32hex() {
        let id = new_provider_event_id();
        assert_eq!(id.len(), 26);
        assert!(id.chars().all(|c| matches!(c, '0'..='9' | 'a'..='v')));
    }

    #[test]
    fn duration_minutes_saturates_without_overflow() {
        let mut ev = Event {
            id: EventId::from("e"),
            account_id: AccountId::from("a"),
            calendar_id: CalendarId::from("c"),
            provider_id: None,
            ical_uid: None,
            summary: String::new(),
            description: None,
            location: None,
            start: i64::MIN,
            end: i64::MAX,
            all_day: false,
            status: EventStatus::Confirmed,
            organizer: None,
            attendees: vec![],
            html_link: None,
            conference_url: None,
            recurring_event_id: None,
            updated: 0,
            event_type: None,
            color: None,
            reminders: None,
            original_start: None,
            wants_conference: false,
            pending: false,
        };
        assert_eq!(ev.duration_minutes(), i64::MAX / 60_000);
        ev.start = i64::MAX;
        ev.end = i64::MIN;
        assert_eq!(ev.duration_minutes(), 0);
    }
}

/// Which occurrences of a recurring event a change applies to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum RecurrenceScope {
    #[default]
    This,
    Following,
    All,
}

impl RecurrenceScope {
    pub fn label(self) -> &'static str {
        match self {
            RecurrenceScope::This => "This event",
            RecurrenceScope::Following => "This and following events",
            RecurrenceScope::All => "All events",
        }
    }
}

impl Event {
    /// Whether this is one occurrence of a repeating event.
    pub fn is_recurring_instance(&self) -> bool {
        self.recurring_event_id.is_some()
    }

    /// The moment a "this and following" split happens at.
    pub fn split_point(&self) -> Timestamp {
        self.original_start.unwrap_or(self.start)
    }
}
