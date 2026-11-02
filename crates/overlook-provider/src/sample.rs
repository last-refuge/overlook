//! Sample provider.
//!
//! A deterministic, in-process "server" with fictional mail. It exists so the design
//! prototype and automated tests can exercise the real local-first path: offline queueing,
//! restart durability, ordered replay, provider acceptance before "Sent", and uncertain
//! delivery. Its connection state is controlled by the app (a visible switch) and never
//! pretends to be a real account: the account is labelled "Sample account".

use crate::{AppliedChanges, BoxFuture, CalendarProvider, EventChanges, MailProvider, MailboxSnapshot, ProviderError, RemoteChanges, SendContext, SendOutcome, SentReceipt};
use overlook_core::time::Timestamp;
use overlook_core::*;
use std::collections::{BTreeSet, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Clone)]
pub struct SampleProvider {
    inner: Arc<Inner>,
}

struct Inner {
    online: AtomicBool,
    /// When set, the next send drops the connection after the server accepted the message.
    drop_next_send: AtomicBool,
    latency: Duration,
    counter: AtomicU64,
    /// Message-IDs the "server" has accepted, for reconciliation.
    accepted: Mutex<HashSet<String>>,
    applied_ops: Mutex<Vec<OperationId>>,
    /// Calendar events created or changed through the provider in this session.
    event_overrides: Mutex<Vec<Event>>,
    deleted_events: Mutex<HashSet<String>>,
    vacation: Mutex<VacationSettings>,
    contacts: Mutex<Vec<Contact>>,
}

impl Default for SampleProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl SampleProvider {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                online: AtomicBool::new(false),
                drop_next_send: AtomicBool::new(false),
                latency: Duration::from_millis(350),
                counter: AtomicU64::new(1),
                accepted: Mutex::new(HashSet::new()),
                applied_ops: Mutex::new(Vec::new()),
                event_overrides: Mutex::new(Vec::new()),
                deleted_events: Mutex::new(HashSet::new()),
                vacation: Mutex::new(VacationSettings::default()),
                contacts: Mutex::new(Vec::new()),
            }),
        }
    }

    pub fn with_latency(mut self, latency: Duration) -> Self {
        Arc::get_mut(&mut self.inner).expect("unshared").latency = latency;
        self
    }

    pub fn is_online(&self) -> bool {
        self.inner.online.load(Ordering::SeqCst)
    }

    pub fn set_online(&self, online: bool) {
        self.inner.online.store(online, Ordering::SeqCst);
    }

    /// Arms a simulated connection drop during the next send.
    pub fn drop_next_send(&self) {
        self.inner.drop_next_send.store(true, Ordering::SeqCst);
    }

    pub fn applied_operations(&self) -> Vec<OperationId> {
        self.inner.applied_ops.lock().unwrap().clone()
    }

    fn require_online(&self) -> Result<(), ProviderError> {
        if self.is_online() { Ok(()) } else { Err(ProviderError::Offline) }
    }

    fn next_id(&self, prefix: &str) -> ProviderId {
        ProviderId(format!("{prefix}-{}", self.inner.counter.fetch_add(1, Ordering::SeqCst)))
    }

    async fn sleep(&self) {
        // The provider does not own an executor; a blocking sleep on the caller's
        // background thread keeps this crate runtime-agnostic.
        let latency = self.inner.latency;
        if !latency.is_zero() {
            std::thread::sleep(latency);
        }
    }
}

impl MailProvider for SampleProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Sample
    }

    fn probe(&self) -> BoxFuture<'_, Result<(), ProviderError>> {
        Box::pin(async move { self.require_online() })
    }

    fn fetch_snapshot(&self, account: &Account) -> BoxFuture<'_, Result<MailboxSnapshot, ProviderError>> {
        let account = account.clone();
        Box::pin(async move {
            self.require_online()?;
            self.sleep().await;
            Ok(sample_mailbox(&account, overlook_core::time::now()))
        })
    }

    fn has_contacts_access(&self) -> bool {
        true
    }

    fn has_contacts_write_access(&self) -> bool {
        true
    }

    fn fetch_contacts(&self, account: &Account) -> BoxFuture<'_, Result<Option<Vec<Contact>>, ProviderError>> {
        let account = account.clone();
        Box::pin(async move {
            self.require_online()?;
            let mut book = self.inner.contacts.lock().unwrap();
            if book.is_empty() {
                let now = overlook_core::time::now();
                let make = |id: &str, name: &str, email: &str, org: Option<&str>, in_book: bool| Contact {
                    id: id.into(),
                    account_id: account.id.clone(),
                    email: email.into(),
                    emails: vec![email.into()],
                    name: Some(name.into()),
                    given_name: name.split(' ').next().map(str::to_owned),
                    family_name: name.split(' ').nth(1).map(str::to_owned),
                    organization: org.map(str::to_owned),
                    in_address_book: in_book,
                    updated: now,
                    ..Default::default()
                };
                book.extend([
                    make("people/c1", "Maya Chen", "maya@northstar.example", Some("Northstar Studio"), true),
                    make("people/c2", "Alex Rivera", "alex@northstar.example", Some("Northstar Studio"), true),
                    make("people/c3", "Daniel Okafor", "daniel@example.com", None, true),
                    make("people/c4", "Sophie Lindqvist", "sophie@example.com", None, true),
                    make("otherContacts/c5", "Priya Natarajan", "priya@fieldandvine.example", None, false),
                ]);
            }
            Ok(Some(book.clone()))
        })
    }

    fn create_contact(&self, _account: &Account, contact: &Contact) -> BoxFuture<'_, Result<Contact, ProviderError>> {
        let mut c = contact.clone();
        Box::pin(async move {
            self.require_online()?;
            self.sleep().await;
            c.id = format!("people/{}", self.next_id("c").0);
            c.pending = false;
            c.in_address_book = true;
            c.etag = Some("v1".into());
            self.inner.contacts.lock().unwrap().push(c.clone());
            Ok(c)
        })
    }

    fn update_contact(&self, _account: &Account, contact: &Contact) -> BoxFuture<'_, Result<Contact, ProviderError>> {
        let mut c = contact.clone();
        Box::pin(async move {
            self.require_online()?;
            self.sleep().await;
            c.pending = false;
            let mut book = self.inner.contacts.lock().unwrap();
            book.retain(|x| x.id != c.id);
            book.push(c.clone());
            Ok(c)
        })
    }

    fn delete_contact(&self, _account: &Account, resource_name: &str) -> BoxFuture<'_, Result<(), ProviderError>> {
        let id = resource_name.to_owned();
        Box::pin(async move {
            self.require_online()?;
            self.inner.contacts.lock().unwrap().retain(|x| x.id != id);
            Ok(())
        })
    }

    fn has_settings_access(&self) -> bool {
        true
    }

    fn fetch_vacation(&self, _account: &Account) -> BoxFuture<'_, Result<Option<VacationSettings>, ProviderError>> {
        Box::pin(async move {
            self.require_online()?;
            Ok(Some(self.inner.vacation.lock().unwrap().clone()))
        })
    }

    fn set_vacation(&self, _account: &Account, settings: &VacationSettings) -> BoxFuture<'_, Result<(), ProviderError>> {
        let settings = settings.clone();
        Box::pin(async move {
            self.require_online()?;
            *self.inner.vacation.lock().unwrap() = settings;
            Ok(())
        })
    }

    fn search(&self, account: &Account, query: &str, limit: usize) -> BoxFuture<'_, Result<Vec<ProviderId>, ProviderError>> {
        let account = account.clone();
        let needle = query.to_ascii_lowercase();
        Box::pin(async move {
            self.require_online()?;
            self.sleep().await;
            let snapshot = sample_mailbox(&account, overlook_core::time::now());
            let mut hits: Vec<&Message> = snapshot
                .messages
                .iter()
                .filter(|m| {
                    let hay = format!("{} {} {} {}", m.subject, m.from.email, m.from.name.clone().unwrap_or_default(), m.body.text.clone().unwrap_or_default()).to_ascii_lowercase();
                    needle.split_whitespace().all(|w| hay.contains(w))
                })
                .collect();
            hits.sort_by_key(|m| std::cmp::Reverse(m.date));
            Ok(hits.into_iter().filter_map(|m| m.provider_id.clone()).take(limit).collect())
        })
    }

    fn fetch_changes(&self, _account: &Account, checkpoint: &str) -> BoxFuture<'_, Result<RemoteChanges, ProviderError>> {
        let checkpoint = checkpoint.to_owned();
        Box::pin(async move {
            self.require_online()?;
            Ok(RemoteChanges { checkpoint: Some(checkpoint), ..Default::default() })
        })
    }

    fn apply(&self, _account: &Account, op: &PendingOperation, _messages: &[Message], _labels: &[Label]) -> BoxFuture<'_, Result<AppliedChanges, ProviderError>> {
        let op = op.clone();
        Box::pin(async move {
            self.require_online()?;
            self.sleep().await;
            self.inner.applied_ops.lock().unwrap().push(op.id.clone());
            Ok(AppliedChanges::default())
        })
    }

    fn send(&self, account: &Account, draft: &Draft, context: SendContext) -> BoxFuture<'_, Result<SentReceipt, ProviderError>> {
        let account = account.clone();
        let draft = draft.clone();
        let sent_label = context.sent_label;
        let in_reply_to = context.in_reply_to;
        Box::pin(async move {
            self.require_online()?;
            self.sleep().await;
            let rfc_id = format!("<{}@overlook.sample>", draft.id);
            // The server accepts the message before the response is lost.
            self.inner.accepted.lock().unwrap().insert(rfc_id.clone());
            if self.inner.drop_next_send.swap(false, Ordering::SeqCst) {
                return Err(ProviderError::Uncertain);
            }
            let provider_id = self.next_id("sent");
            let message = sent_message(&account, &draft, in_reply_to.as_ref(), &provider_id, &rfc_id, sent_label.as_ref(), overlook_core::time::now());
            Ok(SentReceipt { provider_id, rfc_message_id: Some(rfc_id), message })
        })
    }

    fn reconcile_send(&self, _account: &Account, draft: &Draft) -> BoxFuture<'_, Result<SendOutcome, ProviderError>> {
        let draft = draft.clone();
        Box::pin(async move {
            self.require_online()?;
            self.sleep().await;
            let rfc_id = format!("<{}@overlook.sample>", draft.id);
            if self.inner.accepted.lock().unwrap().contains(&rfc_id) {
                Ok(SendOutcome::Delivered(self.next_id("sent")))
            } else {
                Ok(SendOutcome::NotDelivered)
            }
        })
    }
}

pub const SAMPLE_CALENDAR_ID: &str = "sample-calendar";

fn sample_calendar(account: &Account) -> Calendar {
    Calendar {
        id: CalendarId(format!("{}:cal:{SAMPLE_CALENDAR_ID}", account.id.0)),
        account_id: account.id.clone(),
        provider_id: Some(ProviderId(SAMPLE_CALENDAR_ID.into())),
        name: "Calendar".into(),
        color: Some("#365F73".into()),
        is_primary: true,
        can_edit: true,
        visible: true,
        sort_order: 0,
        default_reminders: vec![10],
    }
}

/// Fictional events for the current week and the next, anchored to `now`.
pub fn sample_events(account: &Account, now: Timestamp) -> Vec<Event> {
    use overlook_core::tz;
    let calendar = sample_calendar(account);
    let week = tz::week_start(now, true);
    let me = Address::named(&account.display_name, &account.email);
    let maya = Address::named("Maya Chen", "maya@northstar.example");
    let alex = Address::named("Alex Rivera", "alex@studio.example");
    let daniel = Address::named("Daniel Park", "daniel@studio.example");
    let sophie = Address::named("Sophie Williams", "sophie@example.com");
    let att = |a: &Address, response: ResponseStatus, organizer: bool| Attendee { email: a.email.clone(), name: a.name.clone(), response, is_self: a.email == account.email, is_organizer: organizer, optional: false };
    let mut n = 0;
    let mut ev = |day: i32, hour: f32, minutes: i64, summary: &str, location: Option<&str>, organizer: &Address, attendees: Vec<Attendee>, all_day: bool, event_type: Option<&str>| {
        n += 1;
        let start = tz::add_days(week, day) + (hour * 3_600_000.) as i64;
        let (start, end) = if all_day { (tz::add_days(week, day), tz::add_days(week, day + 1)) } else { (start, start + minutes * 60_000) };
        Event {
            id: EventId(format!("{}:ev-{n}", calendar.id.0)),
            account_id: account.id.clone(),
            calendar_id: calendar.id.clone(),
            provider_id: Some(ProviderId(format!("ev-{n}"))),
            ical_uid: Some(format!("ev-{n}@overlook.sample")),
            summary: summary.into(),
            description: None,
            location: location.map(str::to_owned),
            start,
            end,
            all_day,
            status: EventStatus::Confirmed,
            organizer: Some(organizer.clone()),
            attendees,
            html_link: None,
            conference_url: None,
            recurring_event_id: None,
            updated: now,
            event_type: event_type.map(str::to_owned),
            color: None,
            reminders: None,
            original_start: None,
            wants_conference: false,
            pending: false,
        }
    };
    let mut events = Vec::new();
    for day in 0..5 {
        events.push(ev(day, 0., 0, if day == 2 { "Studio" } else { "Home" }, None, &me, vec![], true, Some("working-location")));
    }
    events.push(ev(0, 9., 60, "Weekly build leaders", Some("Room 4"), &me, vec![att(&me, ResponseStatus::Accepted, true), att(&alex, ResponseStatus::Accepted, false), att(&daniel, ResponseStatus::Tentative, false)], false, None));
    events.push(ev(0, 10.5, 30, "Launch creative review", Some("Northstar Studio"), &maya, vec![att(&maya, ResponseStatus::Accepted, true), att(&me, ResponseStatus::NeedsAction, false)], false, None));
    events.push(ev(0, 14., 55, "Haircut", None, &me, vec![], false, None));
    events.push(ev(1, 9., 60, "Executive meeting", Some("Boardroom"), &me, vec![att(&me, ResponseStatus::Accepted, true), att(&daniel, ResponseStatus::Accepted, false)], false, None));
    events.push(ev(1, 11., 30, "Daniel / Jason 1:1", None, &daniel, vec![att(&daniel, ResponseStatus::Accepted, true), att(&me, ResponseStatus::Accepted, false)], false, None));
    events.push(ev(1, 13.5, 45, "Homepage copy review", None, &alex, vec![att(&alex, ResponseStatus::Accepted, true), att(&me, ResponseStatus::Accepted, false), att(&maya, ResponseStatus::Accepted, false)], false, None));
    events.push(ev(2, 10., 90, "Campaign working session", Some("Northstar Studio"), &maya, vec![att(&maya, ResponseStatus::Accepted, true), att(&me, ResponseStatus::Accepted, false)], false, None));
    events.push(ev(2, 12.5, 60, "Lunch with Sophie", Some("Field & Vine"), &sophie, vec![att(&sophie, ResponseStatus::Accepted, true), att(&me, ResponseStatus::Accepted, false)], false, None));
    events.push(ev(3, 9.5, 30, "Sprint planning", None, &me, vec![att(&me, ResponseStatus::Accepted, true), att(&alex, ResponseStatus::Accepted, false), att(&daniel, ResponseStatus::Accepted, false)], false, None));
    events.push(ev(3, 15., 60, "Quarterly review prep", None, &me, vec![], false, None));
    events.push(ev(4, 0., 0, "Offsite planning day", Some("The Overlook, Pine Ridge"), &me, vec![], true, None));
    events.push(ev(5, 8., 180, "Trail walk", Some("Ridge trailhead"), &sophie, vec![att(&sophie, ResponseStatus::Accepted, true), att(&me, ResponseStatus::Accepted, false)], false, None));
    events.push(ev(7, 9., 60, "Weekly build leaders", Some("Room 4"), &me, vec![att(&me, ResponseStatus::Accepted, true), att(&alex, ResponseStatus::Accepted, false)], false, None));
    events.push(ev(8, 14., 60, "Launch day go/no-go", Some("Boardroom"), &maya, vec![att(&maya, ResponseStatus::Accepted, true), att(&me, ResponseStatus::NeedsAction, false)], false, None));
    events
}

impl CalendarProvider for SampleProvider {
    fn has_calendar_access(&self) -> bool {
        true
    }

    fn list_calendars(&self, account: &Account) -> BoxFuture<'_, Result<Vec<Calendar>, ProviderError>> {
        let account = account.clone();
        Box::pin(async move {
            self.require_online()?;
            Ok(vec![sample_calendar(&account)])
        })
    }

    fn fetch_events(&self, account: &Account, _calendar: &Calendar, sync_token: Option<&str>) -> BoxFuture<'_, Result<EventChanges, ProviderError>> {
        let account = account.clone();
        let incremental = sync_token.is_some();
        Box::pin(async move {
            self.require_online()?;
            self.sleep().await;
            let mut upserts = if incremental { Vec::new() } else { sample_events(&account, overlook_core::time::now()) };
            let deleted = self.inner.deleted_events.lock().unwrap().clone();
            upserts.retain(|e| !deleted.contains(&e.id.0));
            for o in self.inner.event_overrides.lock().unwrap().iter() {
                upserts.retain(|e| e.id != o.id);
                upserts.push(o.clone());
            }
            Ok(EventChanges { upserts, deleted: Vec::new(), next_sync_token: Some("sample-cal-1".into()) })
        })
    }

    fn respond(&self, _account: &Account, _calendar: &Calendar, event: &Event, response: ResponseStatus) -> BoxFuture<'_, Result<(), ProviderError>> {
        let mut event = event.clone();
        Box::pin(async move {
            self.require_online()?;
            for a in &mut event.attendees {
                if a.is_self {
                    a.response = response;
                }
            }
            event.pending = false;
            let mut overrides = self.inner.event_overrides.lock().unwrap();
            overrides.retain(|e| e.id != event.id);
            overrides.push(event);
            Ok(())
        })
    }

    fn create_event(&self, _account: &Account, _calendar: &Calendar, event: &Event) -> BoxFuture<'_, Result<Event, ProviderError>> {
        let mut event = event.clone();
        Box::pin(async move {
            self.require_online()?;
            event.pending = false;
            self.inner.event_overrides.lock().unwrap().push(event.clone());
            Ok(event)
        })
    }

    fn update_event(&self, account: &Account, calendar: &Calendar, event: &Event) -> BoxFuture<'_, Result<Event, ProviderError>> {
        self.create_event(account, calendar, event)
    }

    fn free_busy(&self, _account: &Account, emails: &[String], _start: Timestamp, _end: Timestamp) -> BoxFuture<'_, Result<Vec<(String, Option<bool>)>, ProviderError>> {
        let emails = emails.to_vec();
        Box::pin(async move {
            self.require_online()?;
            self.sleep().await;
            Ok(emails.into_iter().map(|e| { let busy = e.contains("alex") || e.contains("daniel"); (e, Some(busy)) }).collect())
        })
    }

    fn delete_event(&self, _account: &Account, calendar: &Calendar, provider_id: &ProviderId) -> BoxFuture<'_, Result<(), ProviderError>> {
        let id = format!("{}:{}", calendar.id.0, provider_id.0);
        Box::pin(async move {
            self.require_online()?;
            self.inner.deleted_events.lock().unwrap().insert(id.clone());
            self.inner.event_overrides.lock().unwrap().retain(|e| e.id.0 != id);
            Ok(())
        })
    }
}

/// Builds the Sent copy of a draft once the provider accepted it.
pub fn sent_message(
    account: &Account,
    draft: &Draft,
    in_reply_to: Option<&Message>,
    provider_id: &ProviderId,
    rfc_id: &str,
    sent_label: Option<&LabelId>,
    now: Timestamp,
) -> Message {
    let bodies = overlook_core::compose::outgoing_bodies(draft, in_reply_to);
    let mut label_ids = BTreeSet::new();
    if let Some(l) = sent_label {
        label_ids.insert(l.clone());
    }
    Message {
        id: MessageId::generate(),
        account_id: account.id.clone(),
        conversation_id: draft.conversation_id.clone().unwrap_or_else(ConversationId::generate),
        provider_id: Some(provider_id.clone()),
        from: Address::named(&account.display_name, &account.email),
        to: draft.to.clone(),
        cc: draft.cc.clone(),
        bcc: draft.bcc.clone(),
        reply_to: None,
        subject: draft.subject.clone(),
        snippet: bodies.text.lines().find(|l| !l.trim().is_empty()).unwrap_or("").chars().take(100).collect(),
        body: Body { text: Some(bodies.text), html: Some(bodies.html) },
        date: now,
        is_read: true,
        is_starred: false,
        is_outgoing: true,
        label_ids,
        attachments: Vec::new(),
        rfc_message_id: Some(rfc_id.to_owned()),
        in_reply_to: None,
        references: Vec::new(),
        category: None,
        is_important: false,
        focus_override: None,
        invitation: None,
    }
}

/// The fictional mailbox from the design preview, anchored to `now` so that day groups
/// read correctly whenever the app runs.
pub fn sample_mailbox(account: &Account, now: Timestamp) -> MailboxSnapshot {
    let acct = &account.id;
    let label = |key: &str, kind: LabelKind, name: &str, color: Option<&str>, order: i32| Label {
        id: LabelId(format!("{}:{key}", acct.0)),
        account_id: acct.clone(),
        kind,
        name: name.into(),
        provider_id: Some(ProviderId(key.to_uppercase())),
        color: color.map(str::to_owned),
        sort_order: order,
    };
    let labels = vec![
        label("inbox", LabelKind::Inbox, "Inbox", None, 0),
        label("sent", LabelKind::Sent, "Sent", None, 1),
        label("trash", LabelKind::Trash, "Trash", None, 2),
        label("spam", LabelKind::Spam, "Spam", None, 3),
        label("projects", LabelKind::User, "Projects", Some("slate"), 10),
        label("personal", LabelKind::User, "Personal", Some("plum"), 11),
        label("reading", LabelKind::User, "Reading", Some("amber"), 12),
    ];
    let lid = |key: &str| LabelId(format!("{}:{key}", acct.0));

    // Anchor "today" at the local calendar day of `now` and place messages at fixed clock
    // times, like the design preview.
    let at = |days_ago: i64, hour: i64, minute: i64| {
        let day = tz::local_date(tz::add_days(now, -(days_ago as i32)));
        let clock = tz::parse_clock(&format!("{hour:02}:{minute:02}")).expect("sample clock");
        tz::local_datetime(day, clock)
    };

    let me = Address::named(&account.display_name, &account.email);
    let maya = Address::named("Maya Chen", "maya@northstar.example");
    let alex = Address::named("Alex Rivera", "alex@studio.example");
    let daniel = Address::named("Daniel Park", "daniel@studio.example");
    let sophie = Address::named("Sophie Williams", "sophie@example.com");

    let mut counter = 0u32;
    let mut mk = |conv: &str, from: &Address, to: Vec<Address>, subject: &str, snippet: &str, html: &str, date: Timestamp, read: bool, starred: bool, labels: &[&str], attachments: Vec<(&str, &str, u64)>| {
        counter += 1;
        let id = MessageId(format!("{}:msg-{counter}", acct.0));
        let outgoing = from.email == account.email;
        Message {
            id: id.clone(),
            account_id: acct.clone(),
            conversation_id: ConversationId(format!("{}:{conv}", acct.0)),
            provider_id: Some(ProviderId(format!("sample-{counter}"))),
            from: from.clone(),
            to,
            cc: vec![],
            bcc: vec![],
            reply_to: None,
            subject: subject.into(),
            snippet: snippet.into(),
            body: Body { text: Some(overlook_core::sanitize::html_to_text(html)), html: Some(html.into()) },
            date,
            is_read: read || outgoing,
            is_starred: starred,
            is_outgoing: outgoing,
            label_ids: labels.iter().map(|l| lid(l)).collect(),
            attachments: attachments
                .into_iter()
                .map(|(name, mime, size)| Attachment {
                    id: AttachmentId(format!("{}:att-{}", id.0, name)),
                    message_id: id.clone(),
                    filename: name.into(),
                    mime_type: mime.into(),
                    size_bytes: size,
                    local_path: None,
                    provider_id: None,
                    content_id: None,
                })
                .collect(),
            rfc_message_id: None,
            in_reply_to: None,
            references: Vec::new(),
            category: None,
            is_important: false,
            focus_override: None,
            invitation: None,
        }
    };

    let messages = vec![
        // Conversation 1: three messages, latest with attachments.
        mk("launch", &maya, vec![me.clone()], "Launch creative — final review",
           "Sharing the first round of launch concepts.",
           "<p>Hi Jason,</p><p>Sharing the first round of launch concepts. We’ve put together two directions for the homepage and campaign. I’d love your thoughts before we refine the final route.</p><p>Maya</p>",
           at(1, 14, 5), true, false, &["inbox", "projects"], vec![]),
        mk("launch", &me, vec![maya.clone()], "Re: Launch creative — final review",
           "The second direction looks great. A few notes…",
           "<p>The second direction looks great. Let’s move forward with that one.</p><p>Can we simplify the headline, give the product a bit more room, and bring the launch checklist into the same file?</p>",
           at(0, 9, 15), true, false, &["sent", "projects"], vec![]),
        mk("launch", &maya, vec![me.clone()], "Re: Launch creative — final review",
           "The updated launch materials are ready.",
           "<p>Hi Jason,</p><p>The updated launch materials are ready for a final look. We’ve worked your feedback into the homepage and campaign.</p><ul><li><strong>Homepage:</strong> refined layout and updated headline</li><li><strong>Campaign:</strong> final assets in both formats</li><li><strong>Launch brief:</strong> checklist and handoff notes</li></ul><p>Could you take a look by tomorrow afternoon? Once we have your sign-off, we’re ready to go.</p><p>Thanks,<br>Maya<br><small>Design Director · Northstar Studio</small></p>",
           at(0, 10, 42), true, true, &["inbox", "projects"],
           vec![("Launch-brief.pdf", "application/pdf", 2_516_582), ("Homepage-v3.fig", "application/octet-stream", 8_493_465)]),
        mk("website", &alex, vec![me.clone()], "Website updates are ready",
           "The latest build is ready for your review.",
           "<p>Morning Jason,</p><p>The latest build is ready for your review. The navigation and smaller screens are looking good, and all of the copy updates are in.</p><p>I’ve also addressed the feedback on the checkout flow. Let me know if you spot anything else before we share this with the team.</p><p>Alex</p>",
           at(0, 10, 18), false, false, &["inbox", "projects"], vec![]),
        mk("figma", &Address::named("Figma", "updates@figma.example"), vec![me.clone()], "Your team’s weekly activity",
           "3 files updated in the Northstar workspace.",
           "<p>Here’s what your team has been working on.</p><ul><li>Homepage exploration — updated by Maya</li><li>Component library — updated by Alex</li><li>Campaign concepts — updated by Daniel</li></ul><p>You’re all caught up with this week’s activity.</p><p><img src=\"https://cdn.figma.example/footer.png\" alt=\"Figma\"></p>",
           at(0, 9, 36), false, false, &["inbox", "projects"], vec![]),
        mk("saturday", &sophie, vec![me.clone()], "Saturday plans?",
           "Coffee first, then a trail with a view?",
           "<p>Hey Jason,</p><p>The weather looks perfect for Saturday. Want to grab coffee and head out for a walk?</p><p>You pick the trail. I’ll bring the snacks.</p><p>Sophie</p>",
           at(0, 8, 51), false, true, &["inbox", "personal"], vec![]),
        mk("notes", &daniel, vec![me.clone()], "Notes from our conversation",
           "Thanks again for making the time today.",
           "<p>Hi Jason,</p><p>Thanks again for making the time today. It was helpful to walk through the direction together.</p><p>I’ll collect the remaining feedback and send it over tomorrow. We’re aligned on the timeline and ready for the next stage.</p><p>Daniel</p>",
           at(1, 16, 24), true, false, &["inbox", "projects"], vec![]),
        mk("linear", &Address::named("Linear", "digest@linear.example"), vec![me.clone()], "Your project update",
           "12 issues completed. Here’s what’s next.",
           "<p>A productive week.</p><p>Your team completed 12 issues across design and development. The next milestone is the creative handoff, followed by final testing.</p><table><tr><th>Area</th><th>Done</th></tr><tr><td>Design</td><td>5</td></tr><tr><td>Development</td><td>7</td></tr></table><p>You can review the remaining tasks with your team.</p>",
           at(1, 14, 10), false, false, &["inbox", "projects"], vec![]),
        mk("discovery", &Address::named("Dense Discovery", "read@discovery.example"), vec![me.clone()], "Ideas worth your attention",
           "Design, technology, and a different perspective.",
           "<p>This week’s reading list.</p><p>A thoughtful essay on independent software, a new typeface, and a small studio doing remarkable things with ordinary materials.</p><blockquote>“Good tools disappear. You notice the work, not the instrument.”</blockquote><p>A few things to save for a quiet moment.</p>",
           at(1, 11, 3), true, false, &["inbox", "reading"], vec![]),
        mk("timeline", &me, vec![daniel.clone()], "Re: Project timeline",
           "The timeline works for me. Let’s move ahead.",
           "<p>Hi Daniel,</p><p>The timeline works for me. Let’s move ahead with the plan we discussed.</p><p>Jason</p>",
           at(3, 15, 2), true, false, &["sent", "projects"], vec![]),
        mk("concepts", &alex, vec![me.clone()], "First round of concepts",
           "Some early thinking to get us started.",
           "<p>Hi Jason,</p><p>Some early thinking to get us started. We’ve explored two directions and would love your feedback.</p><p>Alex</p>",
           at(4, 9, 40), true, false, &["projects"], vec![]),
        mk("receipt", &Address::named("Northstar Supply", "orders@northstar-supply.example"), vec![me.clone()], "Your order has shipped",
           "Order #48213 is on its way.",
           "<table width=\"100%\"><tr><td><h2>Your order has shipped</h2><p>Order <strong>#48213</strong> is on its way and should arrive Thursday.</p></td></tr><tr><td><table><tr><td>Field notebook ×2</td><td align=\"right\">$24.00</td></tr><tr><td>Brass pencil</td><td align=\"right\">$18.00</td></tr><tr><td><strong>Total</strong></td><td align=\"right\"><strong>$42.00</strong></td></tr></table></td></tr></table><p><a href=\"https://northstar-supply.example/track/48213\">Track your package</a></p>",
           at(2, 7, 12), true, false, &["inbox"], vec![]),
    ];

    MailboxSnapshot { labels, messages, message_ids: Vec::new(), checkpoint: Some("sample-1".into()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account() -> Account {
        Account {
            id: AccountId::from("sample"),
            provider: ProviderKind::Sample,
            display_name: "Jason".into(),
            email: "jason@example.com".into(),
            created_at: 0,
        }
    }

    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};
        fn noop(_: *const ()) {}
        fn clone(p: *const ()) -> RawWaker {
            RawWaker::new(p, &VTABLE)
        }
        static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, noop, noop, noop);
        let waker = unsafe { Waker::from_raw(RawWaker::new(std::ptr::null(), &VTABLE)) };
        let mut cx = Context::from_waker(&waker);
        let mut f = Box::pin(f);
        loop {
            if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
                return v;
            }
        }
    }

    #[test]
    fn offline_provider_refuses_everything() {
        let p = SampleProvider::new().with_latency(Duration::ZERO);
        assert_eq!(block_on(p.probe()), Err(ProviderError::Offline));
        assert!(matches!(block_on(p.fetch_snapshot(&account())), Err(ProviderError::Offline)));
    }

    #[test]
    fn snapshot_is_consistent() {
        let snap = sample_mailbox(&account(), 1_760_000_000_000);
        assert_eq!(snap.labels.len(), 7);
        assert!(snap.messages.len() >= 10);
        let label_ids: BTreeSet<_> = snap.labels.iter().map(|l| l.id.clone()).collect();
        for m in &snap.messages {
            for l in &m.label_ids {
                assert!(label_ids.contains(l), "message references unknown label {l}");
            }
            assert!(m.body.is_cached());
        }
    }

    #[test]
    fn sample_calendar_has_a_week_of_events() {
        let events = sample_events(&account(), overlook_core::time::now());
        assert!(events.len() >= 15);
        assert!(events.iter().any(|e| e.is_invitation() && e.my_response() == Some(ResponseStatus::NeedsAction)));
        assert!(events.iter().any(|e| e.is_working_location()));
        let p = SampleProvider::new().with_latency(Duration::ZERO);
        p.set_online(true);
        let cal = sample_calendar(&account());
        let first = block_on(p.fetch_events(&account(), &cal, None)).unwrap();
        assert!(first.next_sync_token.is_some());
        let again = block_on(p.fetch_events(&account(), &cal, first.next_sync_token.as_deref())).unwrap();
        assert!(again.upserts.is_empty());
    }

    #[test]
    fn uncertain_send_reconciles_as_delivered() {
        let p = SampleProvider::new().with_latency(Duration::ZERO);
        p.set_online(true);
        let mut draft = Draft::new(AccountId::from("sample"), DraftKind::New, 0);
        draft.to = vec![Address::named("Maya Chen", "maya@northstar.example")];
        p.drop_next_send();
        assert_eq!(block_on(p.send(&account(), &draft, SendContext::default())).err(), Some(ProviderError::Uncertain));
        assert!(matches!(block_on(p.reconcile_send(&account(), &draft)), Ok(SendOutcome::Delivered(_))));
        let other = Draft::new(AccountId::from("sample"), DraftKind::New, 0);
        assert_eq!(block_on(p.reconcile_send(&account(), &other)), Ok(SendOutcome::NotDelivered));
    }
}
