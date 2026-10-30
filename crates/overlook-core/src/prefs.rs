//! User preferences. One `Prefs` per profile plus `AccountPrefs` per account, persisted as
//! JSON in the settings table. Every field here drives real behaviour; defaults follow
//! Outlook's conventions where Overlook has an equivalent.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum StartupModule {
    #[default]
    Mail,
    Calendar,
    LastUsed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum NewMailNotifications {
    Off,
    #[default]
    FocusedOnly,
    All,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum MarkAsRead {
    #[default]
    WhenSelected,
    AfterDelay,
    Never,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum RemoteImages {
    #[default]
    Block,
    Allow,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    // General
    pub startup_module: StartupModule,
    pub clock_24h: bool,
    pub week_starts_monday: bool,
    pub new_mail_notifications: NewMailNotifications,
    /// On Omarchy, also show Overlook's messages (sent, errors, warnings) as desktop notifications.
    pub omarchy_notifications: bool,
    pub event_reminders: bool,
    pub sync_interval_secs: u32,
    // Mail
    pub show_avatars: bool,
    pub show_previews: bool,
    pub mark_as_read: MarkAsRead,
    pub mark_as_read_delay_secs: u32,
    pub reply_all_by_default: bool,
    /// Seconds a sent message waits in the Outbox so it can be undone; 0 sends at once.
    pub undo_send_secs: u32,
    pub quote_on_reply: bool,
    pub remote_images: RemoteImages,
    pub attachments_dir: Option<String>,
    /// Newest Inbox messages fetched on first download (Sent/archive/etc. scale with it).
    pub inbox_messages: u32,
    // Calendar
    pub work_start_hour: u8,
    pub work_end_hour: u8,
    pub default_event_minutes: u32,
    pub default_reminder_minutes: u32,
    pub show_declined_events: bool,
    // Updates
    pub check_for_updates: bool,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            startup_module: StartupModule::Mail,
            clock_24h: false,
            week_starts_monday: true,
            new_mail_notifications: NewMailNotifications::FocusedOnly,
            omarchy_notifications: true,
            event_reminders: true,
            sync_interval_secs: 30,
            show_avatars: true,
            show_previews: true,
            mark_as_read: MarkAsRead::WhenSelected,
            mark_as_read_delay_secs: 3,
            reply_all_by_default: false,
            undo_send_secs: 5,
            quote_on_reply: true,
            remote_images: RemoteImages::Block,
            attachments_dir: None,
            inbox_messages: 100,
            work_start_hour: 8,
            work_end_hour: 18,
            default_event_minutes: 30,
            default_reminder_minutes: 10,
            show_declined_events: false,
            check_for_updates: true,
        }
    }
}

/// One named signature: HTML, with the images it embeds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Signature {
    pub id: String,
    pub name: String,
    /// The Markdown-flavoured text of older versions. `normalized` turns it into `html` and
    /// empties it, so this is only ever set on preferences written before HTML signatures.
    pub body: String,
    /// The signature as HTML. Its images are `cid:` references to `images`.
    pub html: String,
    pub images: Vec<SignatureImage>,
}

/// An image a signature embeds. It travels in the message as an inline part whose
/// `Content-ID` is `content_id`, which the signature's HTML refers to as `cid:<content_id>`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct SignatureImage {
    pub content_id: String,
    pub filename: String,
    pub mime_type: String,
    /// The file, base64-encoded.
    pub data: String,
}

/// Per-account preferences.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AccountPrefs {
    /// The single signature of older versions; migrated into `signatures` by `normalized`.
    pub signature: String,
    pub signatures: Vec<Signature>,
    /// Signature ids used by default; `None` means no signature.
    pub default_new: Option<String>,
    pub default_reply: Option<String>,
    pub sign_new_messages: bool,
    pub sign_replies: bool,
    /// Provider URL/id survives reconnecting a CalDAV account with a fresh local id.
    pub default_calendar: Option<String>,
}

impl AccountPrefs {
    pub fn preferred_calendar<'a>(&self, calendars: &'a [crate::Calendar]) -> Option<&'a crate::Calendar> {
        calendars.iter().filter(|c| c.can_edit).find(|c| {
            self.default_calendar.as_deref() == Some(c.provider_id.as_ref().map(|id| id.0.as_str()).unwrap_or(&c.id.0))
        }).or_else(|| calendars.iter().find(|c| c.can_edit && c.is_primary))
            .or_else(|| calendars.iter().find(|c| c.can_edit))
    }

    /// Brings older stored preferences up to date: the single signature becomes the first
    /// named one, the two "include" switches become the defaults, and signatures written as
    /// Markdown become HTML.
    pub fn normalized(mut self) -> Self {
        if self.signatures.is_empty() && !self.signature.trim().is_empty() {
            self.signatures.push(Signature { id: "default".into(), name: "Default".into(), body: std::mem::take(&mut self.signature), ..Default::default() });
            self.default_new = self.sign_new_messages.then(|| "default".to_string());
            self.default_reply = self.sign_replies.then(|| "default".to_string());
        }
        for s in &mut self.signatures {
            if s.html.trim().is_empty() && !s.body.trim().is_empty() {
                s.html = crate::compose::render_markdown(s.body.trim_end());
            }
            s.body.clear();
        }
        self.sign_new_messages = true;
        self.sign_replies = true;
        let known = |id: &Option<String>| id.as_ref().is_some_and(|i| self.signatures.iter().any(|s| &s.id == i));
        if !known(&self.default_new) {
            self.default_new = None;
        }
        if !known(&self.default_reply) {
            self.default_reply = None;
        }
        self
    }

    pub fn signature_by_id(&self, id: &str) -> Option<&Signature> {
        self.signatures.iter().find(|s| s.id == id)
    }

    pub fn default_signature(&self, reply: bool) -> Option<&Signature> {
        let id = if reply { self.default_reply.as_deref() } else { self.default_new.as_deref() };
        id.and_then(|i| self.signature_by_id(i)).filter(|s| !s.html.trim().is_empty())
    }

    pub fn new_signature_id() -> String {
        format!("sig-{}", uuid::Uuid::new_v4().simple())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_calendar_survives_reconnect_and_ignores_unwritable_or_missing_choices() {
        use crate::*;
        let calendar = |id: &str, url: &str, primary| Calendar {
            id: CalendarId::from(id), account_id: AccountId::from("imap:test"),
            provider_id: Some(ProviderId::from(url)), name: id.into(), color: None,
            is_primary: primary, can_edit: true, visible: true, sort_order: 0, default_reminders: vec![],
        };
        let mut calendars = vec![calendar("first", "https://example.test/first/", true), calendar("second", "https://example.test/second/", false)];
        let prefs: AccountPrefs = serde_json::from_str(r#"{"default_calendar":"https://example.test/second/"}"#).unwrap();
        let reopened: AccountPrefs = serde_json::from_str(&serde_json::to_string(&prefs).unwrap()).unwrap();
        calendars[1].id = CalendarId::from("reconnected-second");
        assert_eq!(reopened.preferred_calendar(&calendars).unwrap().id.0, "reconnected-second");
        calendars[1].can_edit = false;
        assert_eq!(reopened.preferred_calendar(&calendars).unwrap().id.0, "first");
        calendars.remove(1);
        assert_eq!(reopened.preferred_calendar(&calendars).unwrap().id.0, "first");
        calendars[0].can_edit = false;
        assert!(reopened.preferred_calendar(&calendars).is_none());
        assert!(serde_json::from_str::<AccountPrefs>("{}").unwrap().default_calendar.is_none());
    }

    #[test]
    fn defaults_survive_partial_json() {
        let p: Prefs = serde_json::from_str(r#"{"clock_24h":true}"#).unwrap();
        assert!(p.clock_24h);
        assert_eq!(p.sync_interval_secs, 30);
        assert_eq!(p.new_mail_notifications, NewMailNotifications::FocusedOnly);
    }

    #[test]
    fn older_signatures_become_html_and_defaults_follow_the_toggles() {
        let a = AccountPrefs { signature: "Jason\nLast Refuge  ".into(), sign_new_messages: true, sign_replies: false, ..Default::default() }.normalized();
        assert_eq!(a.signatures.len(), 1);
        let html = &a.signatures[0].html;
        assert!(html.starts_with("<p>Jason<br>") && html.contains("Last Refuge") && !html.contains("  "), "{html}");
        assert_eq!(a.signatures[0].body, "", "the Markdown is gone once it is HTML");
        assert_eq!(a.default_signature(false).map(|s| s.id.as_str()), Some("default"));
        assert!(a.default_signature(true).is_none());
        let mut b = a.clone();
        b.signatures.push(Signature { id: "short".into(), name: "Short".into(), html: "<p>J.</p>".into(), ..Default::default() });
        b.default_reply = Some("short".into());
        assert_eq!(b.default_signature(true).map(|s| s.html.as_str()), Some("<p>J.</p>"));
    }

    #[test]
    fn normalizing_twice_does_not_bring_back_a_signature_that_was_emptied() {
        let mut a = AccountPrefs { signature: "Jason".into(), ..Default::default() }.normalized();
        a.signatures[0].html.clear();
        let a = a.normalized();
        assert_eq!(a.signatures[0].html, "");
        assert!(a.default_signature(false).is_none(), "an empty signature is not used");
    }

    #[test]
    fn preferences_from_before_images_load_with_none() {
        let prefs: AccountPrefs = serde_json::from_str(r#"{"signatures":[{"id":"a","name":"A","body":"Hi"}]}"#).unwrap();
        let prefs = prefs.normalized();
        assert_eq!((prefs.signatures[0].images.len(), prefs.signatures[0].html.contains("Hi")), (0, true));
    }
}
