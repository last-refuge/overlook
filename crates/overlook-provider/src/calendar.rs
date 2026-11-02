//! Calendar provider trait.

use crate::{BoxFuture, ProviderError};
use overlook_core::time::Timestamp;
use overlook_core::*;

#[derive(Debug, Clone, Default)]
pub struct EventChanges {
    pub upserts: Vec<Event>,
    pub deleted: Vec<EventId>,
    pub next_sync_token: Option<String>,
}

pub trait CalendarProvider: Send + Sync {
    /// Whether the stored credentials include calendar access.
    fn has_calendar_access(&self) -> bool;

    fn list_calendars(&self, account: &Account) -> BoxFuture<'_, Result<Vec<Calendar>, ProviderError>>;

    /// Events for a calendar. With a sync token only changes are returned; without one the
    /// provider returns the configured window. `CheckpointExpired` means: start over.
    fn fetch_events(&self, account: &Account, calendar: &Calendar, sync_token: Option<&str>) -> BoxFuture<'_, Result<EventChanges, ProviderError>>;

    fn respond(&self, account: &Account, calendar: &Calendar, event: &Event, response: ResponseStatus) -> BoxFuture<'_, Result<(), ProviderError>>;

    /// Creates the event; returns the provider's copy.
    fn create_event(&self, account: &Account, calendar: &Calendar, event: &Event) -> BoxFuture<'_, Result<Event, ProviderError>>;

    /// The zone in which date-only timestamps in this saved event were projected.
    /// Keep it stable while a request is in flight, even if the display zone changes.
    fn create_event_in_zone(&self, account: &Account, calendar: &Calendar, event: &Event, _zone: &tz::TimeZone) -> BoxFuture<'_, Result<Event, ProviderError>> {
        self.create_event(account, calendar, event)
    }

    fn update_event(&self, account: &Account, calendar: &Calendar, event: &Event) -> BoxFuture<'_, Result<Event, ProviderError>>;

    fn delete_event(&self, account: &Account, calendar: &Calendar, provider_id: &ProviderId) -> BoxFuture<'_, Result<(), ProviderError>>;

    /// Updates one occurrence, the rest of the series from it on, or the whole series.
    /// The default knows only single events.
    fn update_event_scoped(&self, account: &Account, calendar: &Calendar, event: &Event, _scope: RecurrenceScope) -> BoxFuture<'_, Result<Event, ProviderError>> {
        self.update_event(account, calendar, event)
    }

    fn update_event_scoped_in_zone(&self, account: &Account, calendar: &Calendar, event: &Event, scope: RecurrenceScope, _zone: &tz::TimeZone) -> BoxFuture<'_, Result<Event, ProviderError>> {
        self.update_event_scoped(account, calendar, event, scope)
    }

    /// Deletes with a scope; `recurring_event_id` and `split_at` describe the series when
    /// the scope is wider than one occurrence.
    fn delete_event_scoped(
        &self,
        account: &Account,
        calendar: &Calendar,
        provider_id: &ProviderId,
        _scope: RecurrenceScope,
        _recurring_event_id: Option<&str>,
        _split_at: Option<Timestamp>,
    ) -> BoxFuture<'_, Result<(), ProviderError>> {
        self.delete_event(account, calendar, provider_id)
    }

    /// Whether each address is busy between `start` and `end`; `None` when unknown.
    fn free_busy(&self, _account: &Account, _emails: &[String], _start: Timestamp, _end: Timestamp) -> BoxFuture<'_, Result<Vec<(String, Option<bool>)>, ProviderError>> {
        Box::pin(async { Ok(Vec::new()) })
    }
}
