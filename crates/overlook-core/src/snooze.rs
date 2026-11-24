//! Device-local email snooze domain types, presets, validation, formatting, and wake rules.

use crate::ids::{AccountId, ConversationId, LabelId, MessageId};
use crate::model::Message;
use crate::time::{civil_in_zone, days_from_civil, format_clock, Timestamp, MONTHS_SHORT, WEEKDAYS};
use crate::tz::{in_zone, local_datetime_in, parse_clock, parse_date, TimeZone};
use jiff::civil::{Date, Time, Weekday};
use jiff::ToSpan;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fmt;

/// A device-local snooze attached to one conversation in one account.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationSnooze {
    pub account_id: AccountId,
    pub conversation_id: ConversationId,
    /// When the conversation should return to the inbox (UTC Unix ms).
    pub wake_at: Timestamp,
    /// When the snooze was created or last rescheduled (UTC Unix ms).
    pub snoozed_at: Timestamp,
    /// Timestamp of the latest non-outgoing, non-trashed, non-spam message present when snoozed.
    pub baseline_latest_inbound: Option<Timestamp>,
    /// Local message IDs already in the conversation when snoozed.
    #[serde(default)]
    pub baseline_message_ids: BTreeSet<MessageId>,
    /// Normalized RFC 5322 Message-IDs already in the conversation when snoozed.
    #[serde(default)]
    pub baseline_rfc_ids: BTreeSet<String>,
}

/// Standard quick-pick snooze targets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SnoozePresetKind {
    LaterToday,
    Tomorrow,
    LaterThisWeek,
    ThisWeekend,
    NextWeek,
}

/// One concrete snooze preset resolved against the user's current time, zone, and work hour.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnoozePreset {
    pub kind: SnoozePresetKind,
    pub label: &'static str,
    pub wake_at: Timestamp,
    /// Short target time shown next to the label (e.g. `6:00 PM`, `Tue, 8:00 AM`).
    pub detail: String,
}

/// Wake-up grouping used in the Snoozed list (sorted soonest-waking first).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum SnoozeGroup {
    Today,
    Tomorrow,
    ThisWeek(String),
    Later,
}

impl SnoozeGroup {
    pub fn label(&self) -> &str {
        match self {
            SnoozeGroup::Today => "Today",
            SnoozeGroup::Tomorrow => "Tomorrow",
            SnoozeGroup::ThisWeek(day) => day,
            SnoozeGroup::Later => "Later",
        }
    }
}

/// Validation errors for custom snooze date/time inputs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SnoozeValidationError {
    InvalidDate,
    InvalidTime,
    InvalidDateTime,
    MustBeInFuture,
}

impl fmt::Display for SnoozeValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidDate => write!(f, "Enter a valid date as YYYY-MM-DD (for example, 2026-10-15)."),
            Self::InvalidTime => write!(f, "Enter a valid time (for example, 9:00 AM or 14:30)."),
            Self::InvalidDateTime => write!(f, "Could not resolve that date and time in your time zone."),
            Self::MustBeInFuture => write!(f, "Choose a date and time in the future."),
        }
    }
}

impl std::error::Error for SnoozeValidationError {}

/// Canonicalizes an RFC 5322 Message-ID for baseline comparison across reconnects/resyncs.
pub fn normalize_rfc_id(raw: &str) -> Option<String> {
    let cleaned = raw
        .trim()
        .trim_start_matches('<')
        .trim_end_matches('>')
        .trim()
        .to_ascii_lowercase();
    (!cleaned.is_empty()).then_some(cleaned)
}

/// Builds a baseline snapshot from the conversation's current messages.
pub fn build_snooze_baseline(
    messages: &[Message],
    trash_label: Option<&LabelId>,
    spam_label: Option<&LabelId>,
) -> (Option<Timestamp>, BTreeSet<MessageId>, BTreeSet<String>) {
    let mut latest_inbound: Option<Timestamp> = None;
    let mut message_ids = BTreeSet::new();
    let mut rfc_ids = BTreeSet::new();
    for m in messages {
        message_ids.insert(m.id.clone());
        if let Some(rfc) = m.rfc_message_id.as_deref().and_then(normalize_rfc_id) {
            rfc_ids.insert(rfc);
        }
        let in_trash = trash_label.is_some_and(|t| m.has_label(t));
        let in_spam = spam_label.is_some_and(|s| m.has_label(s));
        if !m.is_outgoing && !in_trash && !in_spam {
            latest_inbound = Some(match latest_inbound {
                Some(prev) => prev.max(m.date),
                None => m.date,
            });
        }
    }
    (latest_inbound, message_ids, rfc_ids)
}

/// Whether `message` is a genuinely new inbound reply that should wake `snooze`.
///
/// Rejects:
/// - messages belonging to a different account or conversation
/// - outgoing messages (`is_outgoing`)
/// - messages filed in Trash or Spam
/// - existing messages already present in `baseline_message_ids`
/// - re-downloaded copies matching `baseline_rfc_ids`
/// - older backfill messages at or before `baseline_latest_inbound` (or before `snoozed_at`
///   when no inbound message existed yet when snoozed)
pub fn is_new_inbound_reply(
    snooze: &ConversationSnooze,
    message: &Message,
    trash_label: Option<&LabelId>,
    spam_label: Option<&LabelId>,
) -> bool {
    if message.account_id != snooze.account_id || message.conversation_id != snooze.conversation_id {
        return false;
    }
    if message.is_outgoing {
        return false;
    }
    if trash_label.is_some_and(|t| message.has_label(t)) {
        return false;
    }
    if spam_label.is_some_and(|s| message.has_label(s)) {
        return false;
    }
    if snooze.baseline_message_ids.contains(&message.id) {
        return false;
    }
    if let Some(rfc) = message.rfc_message_id.as_deref().and_then(normalize_rfc_id) {
        if snooze.baseline_rfc_ids.contains(&rfc) {
            return false;
        }
    }
    match snooze.baseline_latest_inbound {
        Some(baseline) => message.date > baseline,
        None => message.date >= snooze.snoozed_at,
    }
}

fn local_at(date: Date, hour: i8, minute: i8, zone: &TimeZone) -> Option<Timestamp> {
    let time = Time::new(hour, minute, 0, 0).ok()?;
    local_datetime_in(date, time, zone)
}

/// Generates non-duplicate, strictly future snooze presets in `zone` respecting `work_start_hour`.
pub fn snooze_presets(now: Timestamp, zone: &TimeZone, work_start_hour: u8) -> Vec<SnoozePreset> {
    let start_hour = work_start_hour.min(23) as i8;
    let z = in_zone(now, zone);
    let today = z.date();
    let hour = z.hour();
    let weekday = z.weekday();

    let mut presets: Vec<SnoozePreset> = Vec::new();
    let mut push_preset = |kind: SnoozePresetKind, label: &'static str, wake_at: Option<Timestamp>| {
        let Some(wake_at) = wake_at else { return };
        if wake_at <= now {
            return;
        }
        if presets.iter().any(|p| p.wake_at == wake_at) {
            return;
        }
        presets.push(SnoozePreset {
            kind,
            label,
            wake_at,
            detail: format_snooze_preset_detail(wake_at, now, zone),
        });
    };

    // 1. Later today: if before work start (and at least 30 min away), target work start;
    //    else if before 17:00, target 18:00; else if before 20:00, target hour + 3.
    let later_today_hour: Option<i8> = if hour < start_hour && start_hour >= 6 {
        Some(start_hour)
    } else if hour < 17 {
        Some(18)
    } else if hour < 20 {
        Some((hour + 3).min(22))
    } else {
        None
    };
    if let Some(h) = later_today_hour {
        if let Some(ts) = local_at(today, h, 0, zone) {
            if ts >= now + 30 * 60_000 {
                push_preset(SnoozePresetKind::LaterToday, "Later today", Some(ts));
            }
        }
    }

    // 2. Tomorrow at work_start_hour.
    let tomorrow = today.checked_add(1.days()).ok();
    let tomorrow_ts = tomorrow.and_then(|d| local_at(d, start_hour, 0, zone));
    push_preset(SnoozePresetKind::Tomorrow, "Tomorrow", tomorrow_ts);

    // 3. Later this week (Mon..=Wed -> +2 days at work_start_hour).
    if matches!(weekday, Weekday::Monday | Weekday::Tuesday | Weekday::Wednesday) {
        let later_week_ts = today
            .checked_add(2.days())
            .ok()
            .and_then(|d| local_at(d, start_hour, 0, zone));
        push_preset(SnoozePresetKind::LaterThisWeek, "Later this week", later_week_ts);
    }

    // 4. This weekend (Mon..=Thu -> upcoming Saturday at work_start_hour; on Fri Saturday is Tomorrow).
    if matches!(
        weekday,
        Weekday::Monday | Weekday::Tuesday | Weekday::Wednesday | Weekday::Thursday
    ) {
        let days_to_saturday = 5 - weekday.to_monday_zero_offset();
        let weekend_ts = today
            .checked_add(i32::from(days_to_saturday).days())
            .ok()
            .and_then(|d| local_at(d, start_hour, 0, zone));
        push_preset(SnoozePresetKind::ThisWeekend, "This weekend", weekend_ts);
    }

    // 5. Next week (next Monday at work_start_hour; on Sunday Tomorrow is Monday so use +8 days).
    let days_to_next_monday = {
        let mon_zero = weekday.to_monday_zero_offset();
        let d = 7 - mon_zero;
        if d == 1 { 8 } else { d }
    };
    let next_week_ts = today
        .checked_add(i32::from(days_to_next_monday).days())
        .ok()
        .and_then(|d| local_at(d, start_hour, 0, zone));
    push_preset(SnoozePresetKind::NextWeek, "Next week", next_week_ts);

    presets
}

/// Parses and validates custom date (`YYYY-MM-DD`) and time (`9:00 AM`, `14:30`, `9`, etc.)
/// strings in `zone`. Requires the resulting instant to be strictly after `now`.
pub fn parse_custom_snooze(
    date_str: &str,
    time_str: &str,
    now: Timestamp,
    zone: &TimeZone,
) -> Result<Timestamp, SnoozeValidationError> {
    let date = parse_date(date_str.trim()).ok_or(SnoozeValidationError::InvalidDate)?;
    let time = parse_clock(time_str.trim()).ok_or(SnoozeValidationError::InvalidTime)?;
    let wake_at = local_datetime_in(date, time, zone).ok_or(SnoozeValidationError::InvalidDateTime)?;
    if wake_at <= now {
        return Err(SnoozeValidationError::MustBeInFuture);
    }
    Ok(wake_at)
}

/// Pre-fills `(YYYY-MM-DD, clock_string)` for the custom snooze picker.
pub fn default_custom_snooze_inputs(
    now: Timestamp,
    zone: &TimeZone,
    work_start_hour: u8,
    existing_wake_at: Option<Timestamp>,
) -> (String, String) {
    let target = existing_wake_at.filter(|&ts| ts > now).unwrap_or_else(|| {
        let start_hour = work_start_hour.min(23) as i8;
        let tomorrow = in_zone(now, zone)
            .date()
            .checked_add(1.days())
            .unwrap_or_else(|_| in_zone(now, zone).date());
        local_at(tomorrow, start_hour, 0, zone).unwrap_or(now + 86_400_000)
    });
    let (date, time, _) = civil_in_zone(target, zone);
    (
        format!("{:04}-{:02}-{:02}", date.year, date.month, date.day),
        format_clock(time),
    )
}

const WEEKDAYS_SHORT: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

/// Formats a snooze wake time for list chips and reader banners, e.g.
/// `Today, 6:00 PM`, `Tomorrow, 8:00 AM`, `Thu, 8:00 AM`, `Oct 24, 8:00 AM`.
pub fn format_snooze_until(wake_at: Timestamp, now: Timestamp, zone: &TimeZone) -> String {
    let (date, time, weekday) = civil_in_zone(wake_at, zone);
    let (today, _, _) = civil_in_zone(now, zone);
    let day_delta = days_from_civil(date.year, date.month, date.day)
        - days_from_civil(today.year, today.month, today.day);
    let clock = format_clock(time);
    if day_delta == 0 {
        format!("Today, {clock}")
    } else if day_delta == 1 {
        format!("Tomorrow, {clock}")
    } else if (2..=6).contains(&day_delta) {
        format!("{}, {clock}", WEEKDAYS_SHORT[weekday as usize])
    } else if date.year == today.year {
        format!("{} {}, {clock}", MONTHS_SHORT[(date.month - 1) as usize], date.day)
    } else {
        format!(
            "{} {}, {}, {clock}",
            MONTHS_SHORT[(date.month - 1) as usize],
            date.day,
            date.year
        )
    }
}

/// Compact target time for snooze preset menus, e.g. `6:00 PM`, `Tue, 8:00 AM`, `Oct 19, 8:00 AM`.
pub fn format_snooze_preset_detail(wake_at: Timestamp, now: Timestamp, zone: &TimeZone) -> String {
    let (date, time, weekday) = civil_in_zone(wake_at, zone);
    let (today, _, _) = civil_in_zone(now, zone);
    let day_delta = days_from_civil(date.year, date.month, date.day)
        - days_from_civil(today.year, today.month, today.day);
    let clock = format_clock(time);
    if day_delta == 0 {
        clock
    } else if (1..=6).contains(&day_delta) {
        format!("{}, {clock}", WEEKDAYS_SHORT[weekday as usize])
    } else if date.year == today.year {
        format!("{} {}, {clock}", MONTHS_SHORT[(date.month - 1) as usize], date.day)
    } else {
        format!(
            "{} {}, {}, {clock}",
            MONTHS_SHORT[(date.month - 1) as usize],
            date.day,
            date.year
        )
    }
}

/// Groups snoozed conversations by wake-up horizon for the Snoozed list.
pub fn snooze_group(wake_at: Timestamp, now: Timestamp, zone: &TimeZone) -> SnoozeGroup {
    let (date, _, weekday) = civil_in_zone(wake_at, zone);
    let (today, _, _) = civil_in_zone(now, zone);
    let day_delta = days_from_civil(date.year, date.month, date.day)
        - days_from_civil(today.year, today.month, today.day);
    match day_delta {
        i64::MIN..=0 => SnoozeGroup::Today,
        1 => SnoozeGroup::Tomorrow,
        2..=6 => SnoozeGroup::ThisWeek(WEEKDAYS[weekday as usize].to_owned()),
        _ => SnoozeGroup::Later,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Address, Body};

    fn ts(rfc3339: &str) -> Timestamp {
        crate::tz::parse_rfc3339(rfc3339).expect("valid rfc3339")
    }

    fn msg(id: &str, conv: &str, date: Timestamp, outgoing: bool, rfc: Option<&str>, labels: &[&str]) -> Message {
        Message {
            id: MessageId::from(id),
            account_id: AccountId::from("acct-a"),
            conversation_id: ConversationId::from(conv),
            provider_id: None,
            from: Address::new(None, "sender@example.com"),
            to: vec![Address::new(None, "me@example.com")],
            cc: vec![],
            bcc: vec![],
            reply_to: None,
            subject: "Hello".into(),
            snippet: "Hi".into(),
            body: Body::default(),
            date,
            is_read: false,
            is_starred: false,
            is_outgoing: outgoing,
            label_ids: labels.iter().map(|l| LabelId::from(*l)).collect(),
            attachments: vec![],
            rfc_message_id: rfc.map(str::to_owned),
            in_reply_to: None,
            references: vec![],
            category: None,
            is_important: false,
            focus_override: None,
            invitation: None,
            read_receipt: None,
        }
    }

    #[test]
    fn presets_respect_time_zone_work_start_hour_and_are_strictly_future_and_unique() {
        // Monday Oct 12, 2026 at 10:00 AM America/Toronto (EDT, UTC-4 -> 14:00Z)
        let toronto = TimeZone::get("America/Toronto").unwrap();
        let now = ts("2026-10-12T14:00:00Z");
        let presets = snooze_presets(now, &toronto, 9);

        let kinds: Vec<SnoozePresetKind> = presets.iter().map(|p| p.kind).collect();
        assert_eq!(
            kinds,
            vec![
                SnoozePresetKind::LaterToday,
                SnoozePresetKind::Tomorrow,
                SnoozePresetKind::LaterThisWeek,
                SnoozePresetKind::ThisWeekend,
                SnoozePresetKind::NextWeek,
            ]
        );
        for p in &presets {
            assert!(p.wake_at > now, "preset {:?} must be in the future", p.kind);
        }
        // Later today -> Mon Oct 12 at 18:00 EDT
        assert_eq!(civil_in_zone(presets[0].wake_at, &toronto).1.hour, 18);
        assert_eq!(civil_in_zone(presets[0].wake_at, &toronto).0.day, 12);
        // Tomorrow -> Tue Oct 13 at 09:00 EDT
        assert_eq!(civil_in_zone(presets[1].wake_at, &toronto).0.day, 13);
        assert_eq!(civil_in_zone(presets[1].wake_at, &toronto).1.hour, 9);
        // Later this week -> Wed Oct 14 at 09:00 EDT
        assert_eq!(civil_in_zone(presets[2].wake_at, &toronto).0.day, 14);
        assert_eq!(civil_in_zone(presets[2].wake_at, &toronto).1.hour, 9);
        // This weekend -> Sat Oct 17 at 09:00 EDT
        assert_eq!(civil_in_zone(presets[3].wake_at, &toronto).0.day, 17);
        assert_eq!(civil_in_zone(presets[3].wake_at, &toronto).1.hour, 9);
        // Next week -> Mon Oct 19 at 09:00 EDT
        assert_eq!(civil_in_zone(presets[4].wake_at, &toronto).0.day, 19);
        assert_eq!(civil_in_zone(presets[4].wake_at, &toronto).1.hour, 9);
    }

    #[test]
    fn late_evening_and_weekend_presets_omit_duplicates() {
        let utc = TimeZone::UTC;
        // Friday Oct 16, 2026 at 21:00 UTC -> no LaterToday, no ThisWeekend (Tomorrow IS Saturday)
        let fri_night = ts("2026-10-16T21:00:00Z");
        let fri_presets = snooze_presets(fri_night, &utc, 8);
        let fri_kinds: Vec<SnoozePresetKind> = fri_presets.iter().map(|p| p.kind).collect();
        assert_eq!(fri_kinds, vec![SnoozePresetKind::Tomorrow, SnoozePresetKind::NextWeek]);
        assert_eq!(civil_in_zone(fri_presets[0].wake_at, &utc).0.day, 17); // Sat
        assert_eq!(civil_in_zone(fri_presets[1].wake_at, &utc).0.day, 19); // Mon

        // Sunday Oct 18, 2026 at 10:00 UTC -> Tomorrow is Mon Oct 19, Next week is Mon Oct 26
        let sun = ts("2026-10-18T10:00:00Z");
        let sun_presets = snooze_presets(sun, &utc, 8);
        let times: BTreeSet<Timestamp> = sun_presets.iter().map(|p| p.wake_at).collect();
        assert_eq!(times.len(), sun_presets.len(), "no duplicate wake times");
        let next_week = sun_presets.iter().find(|p| p.kind == SnoozePresetKind::NextWeek).unwrap();
        assert_eq!(civil_in_zone(next_week.wake_at, &utc).0.day, 26);
    }

    #[test]
    fn custom_snooze_parses_and_rejects_past_or_invalid_inputs() {
        let toronto = TimeZone::get("America/Toronto").unwrap();
        let now = ts("2026-10-12T14:00:00Z"); // 10:00 AM EDT

        let ok = parse_custom_snooze("2026-10-12", "2:30 PM", now, &toronto).unwrap();
        assert_eq!(civil_in_zone(ok, &toronto).1.hour, 14);
        assert_eq!(civil_in_zone(ok, &toronto).1.minute, 30);

        assert_eq!(
            parse_custom_snooze("2026-10-12", "9:00 AM", now, &toronto),
            Err(SnoozeValidationError::MustBeInFuture)
        );
        assert_eq!(
            parse_custom_snooze("2026-10-12", "10:00 AM", now, &toronto),
            Err(SnoozeValidationError::MustBeInFuture)
        );
        assert_eq!(
            parse_custom_snooze("not-a-date", "10:00 AM", now, &toronto),
            Err(SnoozeValidationError::InvalidDate)
        );
        assert_eq!(
            parse_custom_snooze("2026-10-13", "whenever", now, &toronto),
            Err(SnoozeValidationError::InvalidTime)
        );
    }

    #[test]
    fn format_snooze_until_covers_today_tomorrow_week_and_dates() {
        let utc = TimeZone::UTC;
        let now = ts("2026-10-12T10:00:00Z"); // Mon Oct 12
        assert_eq!(format_snooze_until(ts("2026-10-12T18:00:00Z"), now, &utc), "Today, 6:00 PM");
        assert_eq!(format_snooze_until(ts("2026-10-13T08:00:00Z"), now, &utc), "Tomorrow, 8:00 AM");
        assert_eq!(format_snooze_until(ts("2026-10-15T08:00:00Z"), now, &utc), "Thu, 8:00 AM");
        assert_eq!(format_snooze_until(ts("2026-10-24T08:00:00Z"), now, &utc), "Oct 24, 8:00 AM");
        assert_eq!(format_snooze_until(ts("2027-01-05T08:00:00Z"), now, &utc), "Jan 5, 2027, 8:00 AM");
    }

    #[test]
    fn inbound_reply_predicate_filters_existing_backfill_outgoing_trash_and_spam() {
        let trash = LabelId::from("trash");
        let spam = LabelId::from("spam");
        let existing = msg("m1", "c1", 1000, false, Some("<M1@Example.com>"), &["inbox"]);
        let (latest, ids, rfcs) = build_snooze_baseline(&[existing.clone()], Some(&trash), Some(&spam));
        let snooze = ConversationSnooze {
            account_id: AccountId::from("acct-a"),
            conversation_id: ConversationId::from("c1"),
            wake_at: 5000,
            snoozed_at: 2000,
            baseline_latest_inbound: latest,
            baseline_message_ids: ids,
            baseline_rfc_ids: rfcs,
        };

        // Existing message re-synced
        assert!(!is_new_inbound_reply(&snooze, &existing, Some(&trash), Some(&spam)));
        // Same RFC Message-ID under a new local ID (reconnect/UIDVALIDITY)
        let rekeyed = msg("m1-rekeyed", "c1", 3000, false, Some("  m1@example.com  "), &["inbox"]);
        assert!(!is_new_inbound_reply(&snooze, &rekeyed, Some(&trash), Some(&spam)));
        // Older backfill message
        let backfill = msg("m0", "c1", 500, false, Some("<m0@example.com>"), &["inbox"]);
        assert!(!is_new_inbound_reply(&snooze, &backfill, Some(&trash), Some(&spam)));
        // Outgoing reply
        let outgoing = msg("m2", "c1", 3000, true, Some("<m2@example.com>"), &["sent"]);
        assert!(!is_new_inbound_reply(&snooze, &outgoing, Some(&trash), Some(&spam)));
        // Trashed or spam message
        let trashed = msg("m3", "c1", 3000, false, Some("<m3@example.com>"), &["trash"]);
        assert!(!is_new_inbound_reply(&snooze, &trashed, Some(&trash), Some(&spam)));
        let spammed = msg("m4", "c1", 3000, false, Some("<m4@example.com>"), &["spam"]);
        assert!(!is_new_inbound_reply(&snooze, &spammed, Some(&trash), Some(&spam)));
        // Different account or conversation
        let mut other_acct = msg("m5", "c1", 3000, false, Some("<m5@example.com>"), &["inbox"]);
        other_acct.account_id = AccountId::from("acct-b");
        assert!(!is_new_inbound_reply(&snooze, &other_acct, Some(&trash), Some(&spam)));
        // Genuine new inbound reply!
        let reply = msg("m6", "c1", 3000, false, Some("<m6@example.com>"), &["inbox"]);
        assert!(is_new_inbound_reply(&snooze, &reply, Some(&trash), Some(&spam)));
    }
}
