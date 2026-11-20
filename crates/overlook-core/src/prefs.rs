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

/// Sentinel stored in font preferences for the operating system's UI font.
pub const SYSTEM_UI_FONT: &str = ".SystemUIFont";
/// Bundled default interface font family.
pub const DEFAULT_UI_FONT: &str = "Inter";

/// Font family and size applied to outgoing HTML email bodies when requested.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct OutgoingFont {
    pub family: Option<String>,
    pub size_px: u8,
}

impl OutgoingFont {
    pub fn css_family(&self) -> String {
        css_sans_font_stack(self.family.as_deref())
    }

    pub fn clamped_size(&self) -> u8 {
        self.size_px.clamp(FontPrefs::MIN_EMAIL_SIZE, FontPrefs::MAX_EMAIL_SIZE)
    }
}

/// Font and text size preferences across the interface, message list, email reader,
/// composer, and monospace code/HTML views.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default)]
pub struct FontPrefs {
    /// Interface font family (`None` = bundled Inter; `Some(".SystemUIFont")` = OS UI font).
    pub ui_family: Option<String>,
    /// Base interface text size in px (`14` default, clamped `11..=20`).
    pub ui_size: u8,
    /// Monospace font family (`None` = platform default).
    pub mono_family: Option<String>,
    /// Monospace text size in px (`13` default, clamped `10..=20`).
    pub mono_size: u8,
    /// Message list font family (`None` = follow interface font).
    pub list_family: Option<String>,
    /// Message list text size in px (`None` = follow interface size, else `11..=20`).
    pub list_size: Option<u8>,
    /// Email reading font family (`None` = default; `Some(".SystemUIFont")` or installed font).
    pub reading_family: Option<String>,
    /// Email reading text size in px (`16` default, clamped `12..=28`).
    pub reading_size: u8,
    /// When true, overrides sender-specified fonts and forces the reading size in HTML emails.
    pub override_email_fonts: bool,
    /// Composer editor font family (`None` = follow interface font).
    pub compose_family: Option<String>,
    /// Composer editor text size in px (`14` default, clamped `11..=28`).
    pub compose_size: u8,
    /// When true, includes the compose font family and size in outgoing HTML emails.
    pub style_outgoing_mail: bool,
}

impl FontPrefs {
    pub const DEFAULT_UI_SIZE: u8 = 14;
    pub const MIN_UI_SIZE: u8 = 11;
    pub const MAX_UI_SIZE: u8 = 20;

    pub const DEFAULT_MONO_SIZE: u8 = 13;
    pub const MIN_MONO_SIZE: u8 = 10;
    pub const MAX_MONO_SIZE: u8 = 20;

    pub const DEFAULT_LIST_SIZE: u8 = 13;
    pub const MIN_LIST_SIZE: u8 = 11;
    pub const MAX_LIST_SIZE: u8 = 20;

    pub const DEFAULT_READING_SIZE: u8 = 16;
    pub const DEFAULT_COMPOSE_SIZE: u8 = 14;
    pub const MIN_EMAIL_SIZE: u8 = 11;
    pub const MAX_EMAIL_SIZE: u8 = 28;

    pub fn effective_ui_family(&self) -> &str {
        clean_family(self.ui_family.as_deref()).unwrap_or(DEFAULT_UI_FONT)
    }

    pub fn clamped_ui_size(&self) -> u8 {
        self.ui_size.clamp(Self::MIN_UI_SIZE, Self::MAX_UI_SIZE)
    }

    pub fn effective_mono_family(&self) -> Option<&str> {
        clean_family(self.mono_family.as_deref())
    }

    pub fn clamped_mono_size(&self) -> u8 {
        self.mono_size.clamp(Self::MIN_MONO_SIZE, Self::MAX_MONO_SIZE)
    }

    pub fn effective_list_family(&self) -> &str {
        clean_family(self.list_family.as_deref()).unwrap_or_else(|| self.effective_ui_family())
    }

    /// Scale multiplier for message-list typography relative to the 13 px baseline.
    pub fn list_scale(&self) -> f32 {
        match self.list_size {
            Some(size) => {
                f32::from(size.clamp(Self::MIN_LIST_SIZE, Self::MAX_LIST_SIZE))
                    / f32::from(Self::DEFAULT_LIST_SIZE)
            }
            None => f32::from(self.clamped_ui_size()) / f32::from(Self::DEFAULT_UI_SIZE),
        }
    }

    /// Effective reading font family for native plain-text email bodies.
    pub fn effective_native_reading_family(&self) -> &str {
        clean_family(self.reading_family.as_deref()).unwrap_or_else(|| self.effective_ui_family())
    }

    /// Explicit reading font family for Chromium HTML email rendering, if configured.
    /// Returns `None` when left at default so HTML emails keep Chromium's `Arial, sans-serif` default.
    pub fn browser_reading_family(&self) -> Option<String> {
        clean_family(self.reading_family.as_deref()).map(str::to_owned)
    }

    pub fn clamped_reading_size(&self) -> u8 {
        self.reading_size.clamp(12, Self::MAX_EMAIL_SIZE)
    }

    /// Plain-text emails rendered natively default to 14 px when reading_size is the 16 px HTML default,
    /// scaling smoothly when the user adjusts reading_size.
    pub fn native_reading_size(&self) -> f32 {
        let reading = f32::from(self.clamped_reading_size());
        (reading - 2.).clamp(11., f32::from(Self::MAX_EMAIL_SIZE))
    }

    pub fn effective_compose_family(&self) -> &str {
        clean_family(self.compose_family.as_deref()).unwrap_or_else(|| self.effective_ui_family())
    }

    pub fn clamped_compose_size(&self) -> u8 {
        self.compose_size.clamp(Self::MIN_EMAIL_SIZE, Self::MAX_EMAIL_SIZE)
    }

    /// Outgoing email font when "Include font in sent emails" is enabled.
    pub fn outgoing_font(&self) -> Option<OutgoingFont> {
        self.style_outgoing_mail.then(|| OutgoingFont {
            family: Some(self.effective_compose_family().to_owned()),
            size_px: self.clamped_compose_size(),
        })
    }
}

impl Default for FontPrefs {
    fn default() -> Self {
        Self {
            ui_family: None,
            ui_size: Self::DEFAULT_UI_SIZE,
            mono_family: None,
            mono_size: Self::DEFAULT_MONO_SIZE,
            list_family: None,
            list_size: None,
            reading_family: None,
            reading_size: Self::DEFAULT_READING_SIZE,
            override_email_fonts: false,
            compose_family: None,
            compose_size: Self::DEFAULT_COMPOSE_SIZE,
            style_outgoing_mail: false,
        }
    }
}

fn clean_family(family: Option<&str>) -> Option<&str> {
    family.map(str::trim).filter(|s| !s.is_empty())
}

/// Sanitizes a single font family name for safe inclusion inside a CSS string literal.
/// Strips control characters and CSS/HTML delimiters so untrusted preferences can never
/// break out of a `<style>` block or inline `style="..."` attribute.
pub fn sanitize_css_font_name(name: &str) -> Option<String> {
    let cleaned: String = name
        .chars()
        .filter(|&c| {
            !c.is_control()
                && !matches!(
                    c,
                    '\'' | '"' | '\\' | ';' | '{' | '}' | '<' | '>' | '(' | ')' | '/'
                )
        })
        .collect();
    let trimmed = cleaned.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// Builds a safe CSS `font-family` stack for body / reading / outgoing mail text.
pub fn css_sans_font_stack(family: Option<&str>) -> String {
    match clean_family(family) {
        None => "Arial, sans-serif".to_owned(),
        Some(SYSTEM_UI_FONT) => {
            "-apple-system, BlinkMacSystemFont, 'Segoe UI', system-ui, Arial, sans-serif".to_owned()
        }
        Some(name) => match sanitize_css_font_name(name) {
            Some(safe) => {
                format!("'{safe}', -apple-system, BlinkMacSystemFont, 'Segoe UI', Arial, sans-serif")
            }
            None => "Arial, sans-serif".to_owned(),
        },
    }
}

/// Builds a safe CSS `font-family` stack for monospace code blocks.
pub fn css_mono_font_stack(family: Option<&str>) -> Option<String> {
    let safe = sanitize_css_font_name(clean_family(family)?)?;
    Some(format!("'{safe}', ui-monospace, Menlo, Consolas, monospace"))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    // General
    pub startup_module: StartupModule,
    pub clock_24h: bool,
    /// None follows the system with confirmation when it changes. An override is
    /// normally an IANA name; a kept anonymous system zone retains its TZif rules.
    pub time_zone: Option<crate::tz::ZoneDefinition>,
    pub week_starts_monday: bool,
    pub new_mail_notifications: NewMailNotifications,
    /// On Omarchy, also show Overlook's messages (sent, errors, warnings) as desktop notifications.
    pub omarchy_notifications: bool,
    pub event_reminders: bool,
    pub sync_interval_secs: u32,
    /// Closing the last window leaves Overlook running without one, so sync, reminders and
    /// notifications carry on; Quit ends it.
    pub keep_running: bool,
    // Mail
    pub show_avatars: bool,
    pub show_previews: bool,
    /// Use dark message bodies when the app's active theme is dark.
    pub dark_message_body: bool,
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
    // Sidebar
    pub sidebar_mode: crate::SidebarMode,
    /// Account ids in the order the sidebar lists them; see `sidebar::ordered_accounts`.
    pub account_order: Vec<crate::AccountId>,
    /// Pinned folders; `None` until the user changes them, meaning every account's Inbox.
    pub favorites: Option<Vec<crate::Favorite>>,
    // Typography
    pub fonts: FontPrefs,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            startup_module: StartupModule::Mail,
            clock_24h: false,
            time_zone: None,
            week_starts_monday: true,
            new_mail_notifications: NewMailNotifications::FocusedOnly,
            omarchy_notifications: true,
            event_reminders: true,
            sync_interval_secs: 30,
            keep_running: false,
            show_avatars: true,
            show_previews: true,
            dark_message_body: false,
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
            sidebar_mode: crate::SidebarMode::AllAccounts,
            account_order: Vec::new(),
            favorites: None,
            fonts: FontPrefs::default(),
        }
    }
}

/// One named signature: HTML, with the images it embeds and optional typography overrides.
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
    /// Optional font family for this signature (`None` inherits the compose/outgoing font).
    pub font_family: Option<String>,
    /// Optional font size in CSS px (`None` inherits the compose/outgoing size).
    pub font_size: Option<u8>,
}

impl Signature {
    /// Effective UI/preview font family for this signature, falling back to the compose font.
    pub fn effective_font_family<'a>(&'a self, fonts: &'a FontPrefs) -> &'a str {
        clean_family(self.font_family.as_deref()).unwrap_or_else(|| fonts.effective_compose_family())
    }

    /// Effective UI/preview font size in px for this signature, falling back to the compose size.
    pub fn effective_font_size(&self, fonts: &FontPrefs) -> u8 {
        self.font_size
            .map(|s| s.clamp(FontPrefs::MIN_EMAIL_SIZE, FontPrefs::MAX_EMAIL_SIZE))
            .unwrap_or_else(|| fonts.clamped_compose_size())
    }

    /// Signature HTML wrapped with this signature's explicit font family/size when configured.
    /// Returns raw `self.html` unchanged when neither override is set, preserving defaults and
    /// allowing outgoing-font inheritance in [`crate::compose::outgoing_bodies_with_font`].
    pub fn styled_html(&self) -> String {
        if self.html.trim().is_empty() {
            return String::new();
        }
        let family = clean_family(self.font_family.as_deref());
        let size = self
            .font_size
            .map(|s| s.clamp(FontPrefs::MIN_EMAIL_SIZE, FontPrefs::MAX_EMAIL_SIZE));
        let mut decls = Vec::new();
        if let Some(fam) = family {
            decls.push(format!("font-family: {};", css_sans_font_stack(Some(fam))));
        }
        if let Some(px) = size {
            decls.push(format!("font-size: {px}px;"));
        }
        if decls.is_empty() {
            self.html.clone()
        } else {
            format!("<div style=\"{}\">{}</div>", decls.join(" "), self.html)
        }
    }
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
    /// What the account is called in the sidebar ("Work"); empty shows the address.
    pub name: String,
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
        assert_eq!(p.time_zone, None);
        assert!(!p.dark_message_body);
        let custom = Prefs {
            time_zone: Some(crate::tz::ZoneDefinition::Named("America/Toronto".into())),
            dark_message_body: true,
            ..p
        };
        let restored: Prefs = serde_json::from_str(&serde_json::to_string(&custom).unwrap()).unwrap();
        assert_eq!(restored.time_zone, custom.time_zone);
        assert!(restored.dark_message_body);
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

    #[test]
    fn font_prefs_defaults_and_sanitization() {
        let p: Prefs = serde_json::from_str("{}").unwrap();
        assert_eq!(p.fonts, FontPrefs::default());
        assert_eq!(p.fonts.effective_ui_family(), DEFAULT_UI_FONT);
        assert_eq!(p.fonts.clamped_ui_size(), 14);
        assert_eq!(p.fonts.clamped_reading_size(), 16);
        assert_eq!(p.fonts.list_scale(), 1.0);
        assert!(p.fonts.outgoing_font().is_none());

        assert_eq!(css_sans_font_stack(None), "Arial, sans-serif");
        assert!(css_sans_font_stack(Some(SYSTEM_UI_FONT)).contains("system-ui"));
        assert!(css_sans_font_stack(Some("Georgia")).starts_with("'Georgia',"));
        // Reject CSS/HTML breakout characters in untrusted font names.
        let escaped = css_sans_font_stack(Some("Georgia'; }</style><script>alert(1)</script>"));
        assert!(!escaped.contains("</style>"));
        assert!(!escaped.contains(';'));
        assert!(!escaped.contains('{'));
        assert!(!escaped.contains('}'));

        let custom = FontPrefs {
            ui_size: 255,
            reading_size: 2,
            list_size: Some(16),
            compose_family: Some("Georgia".into()),
            compose_size: 15,
            style_outgoing_mail: true,
            ..Default::default()
        };
        assert_eq!(custom.clamped_ui_size(), FontPrefs::MAX_UI_SIZE);
        assert_eq!(custom.clamped_reading_size(), 12);
        assert!((custom.list_scale() - (16.0 / 13.0)).abs() < 0.001);
        let out = custom.outgoing_font().expect("outgoing font enabled");
        assert_eq!(out.family.as_deref(), Some("Georgia"));
        assert_eq!(out.clamped_size(), 15);

        let default_outgoing = FontPrefs {
            style_outgoing_mail: true,
            ..Default::default()
        }
        .outgoing_font()
        .expect("default outgoing font enabled");
        assert_eq!(default_outgoing.family.as_deref(), Some(DEFAULT_UI_FONT));
        assert!(default_outgoing.css_family().starts_with("'Inter',"));
    }

    #[test]
    fn signature_font_defaults_inheritance_and_styling() {
        // Older saved signatures without font fields deserialize cleanly.
        let prefs: AccountPrefs =
            serde_json::from_str(r#"{"signatures":[{"id":"s1","name":"Work","html":"<p>Jason</p>"}]}"#).unwrap();
        let sig = &prefs.signatures[0];
        assert_eq!((sig.font_family.as_deref(), sig.font_size), (None, None));
        assert_eq!(sig.styled_html(), "<p>Jason</p>");

        let fonts = FontPrefs {
            compose_family: Some("Georgia".into()),
            compose_size: 16,
            ..Default::default()
        };
        assert_eq!(sig.effective_font_family(&fonts), "Georgia");
        assert_eq!(sig.effective_font_size(&fonts), 16);

        let styled = Signature {
            id: "s2".into(),
            name: "Personal".into(),
            html: "<p>Best,<br>Jason</p>".into(),
            font_family: Some("Palatino".into()),
            font_size: Some(13),
            ..Default::default()
        };
        assert_eq!(styled.effective_font_family(&fonts), "Palatino");
        assert_eq!(styled.effective_font_size(&fonts), 13);
        let wrapped = styled.styled_html();
        assert!(wrapped.starts_with("<div style=\"font-family: 'Palatino',"));
        assert!(wrapped.contains("font-size: 13px;\"><p>Best,<br>Jason</p></div>"));

        // Family-only or size-only overrides emit only that declaration.
        let family_only = Signature {
            html: "<p>Hi</p>".into(),
            font_family: Some("Verdana".into()),
            ..Default::default()
        };
        assert!(family_only.styled_html().contains("font-family: 'Verdana',"));
        assert!(!family_only.styled_html().contains("font-size:"));

        let size_only = Signature {
            html: "<p>Hi</p>".into(),
            font_size: Some(99),
            ..Default::default()
        };
        assert_eq!(
            size_only.styled_html(),
            format!("<div style=\"font-size: {}px;\"><p>Hi</p></div>", FontPrefs::MAX_EMAIL_SIZE)
        );
    }
}
