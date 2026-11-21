//! Scheduling on the Sample calendar through the native event operation queue.

use super::{parse, storage, ErrorCode, Failure, WriteContext, WritePermission, WriteTool};
use overlook_core::{
    time::now, Address, Attendee, Event, EventStatus, ProviderKind, ResponseStatus,
};
use overlook_provider::sample::SAMPLE_CALENDAR_ID;
use overlook_store::{McpWriteTxn, StoreError};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::collections::HashSet;

const RESPOND: &str = "calendar_invitation_respond";
const SET_GUESTS: &str = "calendar_event_guests_set";
const MAX_GUESTS: usize = 24;

pub(super) fn tools() -> Vec<WriteTool> {
    vec![
        WriteTool::new(
            RESPOND,
            "Queue a response to a cached Sample calendar invitation as its attendee.",
            json!({"type":"object","properties":{
                "event_id":{"type":"string"},
                "if_revision":{"type":"string"},
                "response":{"enum":["accepted","tentative","declined"]},
                "confirmed":{"type":"boolean","const":true}
            },"required":["event_id","if_revision","response","confirmed"]}),
            WritePermission::Invite,
        ),
        WriteTool::new(
            SET_GUESTS,
            "Replace the guest list on one Sample event you organize. Sample sync simulates scheduling.",
            json!({"type":"object","properties":{
                "event_id":{"type":"string"},
                "if_revision":{"type":"string"},
                "guest_emails":{"type":"array","maxItems":MAX_GUESTS,"items":{"type":"string","maxLength":254},
                    "description":"Full list of guest email addresses, excluding the organizer. An empty list removes all guests."},
                "confirmed":{"type":"boolean","const":true}
            },"required":["event_id","if_revision","guest_emails","confirmed"]}),
            WritePermission::Invite,
        ),
    ]
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RespondArgs {
    event_id: String,
    if_revision: String,
    response: ResponseStatus,
    confirmed: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GuestsArgs {
    event_id: String,
    if_revision: String,
    guest_emails: Vec<String>,
    confirmed: bool,
}

enum Change {
    Respond(ResponseStatus),
    SetGuests(Vec<String>),
}

pub(super) fn execute(
    ctx: &WriteContext<'_>,
    tx: &mut McpWriteTxn<'_, '_>,
    name: &str,
    args: &Map<String, Value>,
) -> Result<Value, Failure> {
    let (event_id, if_revision, confirmed, change) = match name {
        RESPOND => {
            let input: RespondArgs = parse(args)?;
            if input.response == ResponseStatus::NeedsAction {
                return Err(invalid("Choose accepted, tentative, or declined"));
            }
            (
                input.event_id,
                input.if_revision,
                input.confirmed,
                Change::Respond(input.response),
            )
        }
        SET_GUESTS => {
            let input: GuestsArgs = parse(args)?;
            (
                input.event_id,
                input.if_revision,
                input.confirmed,
                Change::SetGuests(input.guest_emails),
            )
        }
        _ => return Err(invalid("Unknown calendar scheduling tool")),
    };
    if !confirmed {
        return Err(invalid(
            "Calendar scheduling requires explicit confirmation",
        ));
    }
    if event_id.is_empty()
        || event_id.len() > 128
        || if_revision.is_empty()
        || if_revision.len() > 128
    {
        return Err(invalid("Provide a valid event ID and revision"));
    }
    let mut event = tx
        .find_event(|id| ctx.id("event", id) == event_id)
        .map_err(storage)?
        .ok_or_else(|| {
            Failure::new(
                ErrorCode::AccessDenied,
                "Event is unavailable for this account",
            )
        })?;
    if event.calendar_id.0.starts_with("caldav:") {
        return Err(Failure::new(
            ErrorCode::Unsupported,
            "CalDAV invitation scheduling is unsupported",
        ));
    }
    if ctx.account.provider != ProviderKind::Sample
        || event.calendar_id.0 != format!("{}:cal:{SAMPLE_CALENDAR_ID}", ctx.account.id.0)
        || event.provider_id.as_ref().is_none_or(|id| id.0.is_empty())
        || event.is_working_location()
    {
        return Err(Failure::new(
            ErrorCode::Unsupported,
            "This event has no supported invitation scheduling path",
        ));
    }
    if ctx.event_revision(&event) != if_revision {
        return Err(Failure::new(
            ErrorCode::Conflict,
            "The event changed. Read it again before scheduling",
        ));
    }
    if event.status == EventStatus::Cancelled {
        return Err(Failure::new(
            ErrorCode::Conflict,
            "Cancelled events cannot be scheduled",
        ));
    }
    let organizer = event
        .organizer
        .as_ref()
        .filter(|address| valid_address(address))
        .ok_or_else(|| Failure::new(ErrorCode::AccessDenied, "Event organizer is unavailable"))?;
    let email = &ctx.account.email;
    let is_organizer = organizer.email.eq_ignore_ascii_case(email);
    let response = match change {
        Change::Respond(response) => {
            if is_organizer {
                return Err(Failure::new(
                    ErrorCode::AccessDenied,
                    "Respond as an invited attendee",
                ));
            }
            respond(&mut event, email, response)?;
            Some(response)
        }
        Change::SetGuests(guests) => {
            if !is_organizer {
                return Err(Failure::new(
                    ErrorCode::AccessDenied,
                    "Only the organizer can set guests",
                ));
            }
            set_guests(&mut event, email, guests)?;
            None
        }
    };
    event.updated = now();
    let operation = tx
        .queue_invitation_change(&event, response)
        .map_err(|error| match error {
            StoreError::NotOwned(_) => Failure::new(
                ErrorCode::AccessDenied,
                "A writable scheduling calendar is required",
            ),
            StoreError::Invalid(_) => Failure::new(
                ErrorCode::Unsupported,
                "CalDAV invitation scheduling is unsupported",
            ),
            other => storage(other),
        })?
        .ok_or_else(|| {
            Failure::new(
                ErrorCode::Conflict,
                "An event change is already waiting to sync",
            )
        })?;
    let mut result = json!({
        "event_id": event_id,
        "revision": ctx.event_revision(&event),
        "operation_id": ctx.id("operation", operation.id.as_str()),
        "status": "queued",
        "delivery": "sample_simulated",
    });
    if let Some(response) = response {
        result["response"] = json!(response);
    }
    Ok(result)
}

fn respond(event: &mut Event, email: &str, response: ResponseStatus) -> Result<(), Failure> {
    let mut matching = event
        .attendees
        .iter_mut()
        .filter(|a| a.is_self || a.email.eq_ignore_ascii_case(email));
    let attendee = matching.next().filter(|attendee| {
        attendee.is_self && !attendee.is_organizer && attendee.email.eq_ignore_ascii_case(email)
    });
    match (attendee, matching.next()) {
        (Some(attendee), None) => {
            attendee.response = response;
            Ok(())
        }
        _ => Err(Failure::new(
            ErrorCode::AccessDenied,
            "The account has no unambiguous attendee identity for this invitation",
        )),
    }
}

fn set_guests(event: &mut Event, organizer: &str, guests: Vec<String>) -> Result<(), Failure> {
    if guests.len() > MAX_GUESTS {
        return Err(invalid("A guest list can contain up to 24 addresses"));
    }
    let mut seen = HashSet::new();
    let mut attendees = Vec::with_capacity(guests.len() + 1);
    for attendee in event
        .attendees
        .iter()
        .filter(|a| a.is_self || a.is_organizer || a.email.eq_ignore_ascii_case(organizer))
    {
        if !attendees.is_empty()
            || !attendee.is_self
            || !attendee.is_organizer
            || !attendee.email.eq_ignore_ascii_case(organizer)
        {
            return Err(Failure::new(
                ErrorCode::AccessDenied,
                "Event organizer identity is ambiguous",
            ));
        }
        attendees.push(attendee.clone());
    }
    for email in guests {
        let address = Address::new(None, email);
        if !valid_address(&address)
            || address.email.eq_ignore_ascii_case(organizer)
            || !seen.insert(address.email.to_ascii_lowercase())
        {
            return Err(invalid(
                "Provide unique guest email addresses and omit the organizer",
            ));
        }
        let attendee = event
            .attendees
            .iter()
            .find(|a| a.email.eq_ignore_ascii_case(&address.email));
        attendees.push(attendee.cloned().unwrap_or(Attendee {
            email: address.email,
            name: None,
            response: ResponseStatus::NeedsAction,
            is_self: false,
            is_organizer: false,
            optional: false,
        }));
    }
    event.attendees = attendees;
    Ok(())
}

fn invalid(message: &'static str) -> Failure {
    Failure::new(ErrorCode::InvalidRequest, message)
}

fn valid_address(address: &Address) -> bool {
    address.looks_valid()
        && address.email.len() <= 254
        && !address.email.chars().any(char::is_control)
        && !address
            .email
            .contains([',', ';', '<', '>', '(', ')', '"', ':', '\\'])
}
