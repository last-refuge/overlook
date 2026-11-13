//! Services that want an app-specific password from mail apps instead of the account's own
//! password, and what the setup dialog tells people about them: what the password is called,
//! where to create one, what it looks like, what a refused sign-in usually means, and the
//! service's server settings.
//!
//! iCloud Mail, Yahoo Mail, AOL Mail and Fastmail are guided. Zoho and the providers of
//! roadmap priority 7 follow the same pattern, so each is one more [`AppPasswordGuide`] in
//! [`GUIDES`]; the app builds its "Add … account" entries from that list.

use overlook_core::{MailServers, Security, ServerEndpoint, ServerProtocol};

/// How to set up one service that signs in with an app-specific password.
#[derive(Debug)]
pub struct AppPasswordGuide {
    /// The service's name: "iCloud Mail".
    pub provider: &'static str,
    /// Its short name, for "Add iCloud account…" and `OVERLOOK_SIGN_IN=icloud`.
    pub short_name: &'static str,
    /// Address domains that belong to the service.
    pub domains: &'static [&'static str],
    /// Server host names that belong to it, matched with their subdomains, for accounts at
    /// other domains.
    pub hosts: &'static [&'static str],
    /// The dialog's opening paragraph.
    pub explanation: &'static str,
    /// What the password field asks for.
    pub password_label: &'static str,
    /// The shape of one, shown in the empty field.
    pub password_placeholder: &'static str,
    /// The button that opens the page where one is made.
    pub create_label: &'static str,
    pub create_url: &'static str,
    /// Added when the service refuses the sign-in.
    pub refused: &'static str,
    /// Shown when an address at another domain is set up with this service.
    pub custom_domain: &'static str,
    /// Whether the service offers POP3.
    pub offers_pop3: bool,
    /// Whether the incoming user name may be the address's name part or the whole address:
    /// the connection test tries the other form when the first is refused.
    pub alternate_user_names: bool,
    /// Whether spaces are taken out of a typed password: the service shows its app passwords
    /// in groups, but the spaces are not part of them.
    pub strip_spaces: bool,
    /// Whether a password has the shape the service's app passwords have.
    pub looks_like_app_password: fn(&str) -> bool,
    /// The service's server settings for an address (its own or a custom domain's), reading
    /// mail over IMAP or, where offered, POP3.
    pub servers: fn(ServerProtocol, &str) -> MailServers,
}

/// iCloud Mail, from Apple's "iCloud Mail server settings for other email client apps"
/// (support.apple.com/102525, February 2026) and "Sign in to apps with your Apple Account
/// using app-specific passwords" (support.apple.com/102654).
pub static ICLOUD: AppPasswordGuide = AppPasswordGuide {
    provider: "iCloud Mail",
    short_name: "iCloud",
    domains: &["icloud.com", "me.com", "mac.com"],
    hosts: &["mail.me.com"],
    explanation: "iCloud Mail needs an app-specific password for mail apps outside Apple’s own, not your Apple Account password. Create one in your Apple Account under Sign-In and Security → App-Specific Passwords (it needs two-factor authentication), then paste it here. Overlook keeps it in the system keyring.",
    password_label: "App-specific password",
    password_placeholder: "xxxx-xxxx-xxxx-xxxx",
    create_label: "Create an app-specific password",
    create_url: "https://account.apple.com",
    refused: "iCloud accepts only an app-specific password here, not your Apple Account password. Create one at account.apple.com under Sign-In and Security → App-Specific Passwords; one you revoked there no longer works.",
    custom_domain: "For an address at your own domain, iCloud signs you in with your iCloud Mail address: use its name (before the @) as the incoming user name and the whole @icloud.com address as the outgoing one.",
    offers_pop3: false,
    alternate_user_names: true,
    strip_spaces: false,
    looks_like_app_password: looks_like_apple_app_password,
    servers: icloud_servers,
};

/// Yahoo Mail, from Yahoo's "IMAP server settings for Yahoo Mail" (help.yahoo.com SLN4075)
/// and "Generate and manage 3rd-party app passwords" (SLN15241), November 2026. Yahoo! JAPAN
/// (yahoo.co.jp) is a separate service and is not included.
pub static YAHOO: AppPasswordGuide = AppPasswordGuide {
    provider: "Yahoo Mail",
    short_name: "Yahoo",
    domains: &[
        "yahoo.com", "ymail.com", "rocketmail.com", "yahoo.co.uk", "yahoo.ca", "yahoo.de", "yahoo.fr", "yahoo.es", "yahoo.it",
        "yahoo.com.au", "yahoo.co.nz", "yahoo.co.in", "yahoo.in", "yahoo.ie", "yahoo.com.br", "yahoo.com.mx", "yahoo.com.ar",
        "yahoo.com.sg", "yahoo.com.ph", "yahoo.co.id",
    ],
    hosts: &["mail.yahoo.com"],
    explanation: "Yahoo Mail needs an app password for mail apps, not your Yahoo account password. Create one in your Yahoo account under Account Security → External connections → Create app password, then paste it here. Yahoo offers app passwords only in a browser you’ve used to sign in to Yahoo for several days (not a private window). Overlook keeps it in the system keyring.",
    password_label: "App password",
    password_placeholder: "The app password Yahoo generated",
    create_label: "Create an app password",
    create_url: "https://login.yahoo.com/account/security",
    refused: "Yahoo accepts only an app password here, not your Yahoo account password. Create one at login.yahoo.com/account/security under External connections; one you deleted there no longer works.",
    custom_domain: "This address isn’t at a Yahoo Mail domain. These are Yahoo Mail’s own servers; check them against the settings your mail provider gives you.",
    offers_pop3: true,
    alternate_user_names: false,
    strip_spaces: true,
    looks_like_app_password: looks_like_yahoo_app_password,
    servers: yahoo_servers,
};

/// AOL Mail, which runs on Yahoo's platform. AOL's help pages do not render without a
/// browser; the settings and steps here agree across the third-party guides checked in
/// November 2026, which also say AOL wants two-step verification before it offers app
/// passwords. Verizon addresses, also on AOL's servers, are left to separate qualification.
pub static AOL: AppPasswordGuide = AppPasswordGuide {
    provider: "AOL Mail",
    short_name: "AOL",
    domains: &["aol.com", "aim.com", "aol.co.uk", "aol.fr", "aol.de", "love.com", "ygm.com", "games.com", "wow.com"],
    hosts: &["aol.com"],
    explanation: "AOL Mail needs an app password for mail apps, not your AOL account password. Create one in your AOL account under Account Security → Generate app password (AOL asks you to turn on two-step verification first), then paste it here. Overlook keeps it in the system keyring.",
    password_label: "App password",
    password_placeholder: "The app password AOL generated",
    create_label: "Create an app password",
    create_url: "https://login.aol.com/account/security",
    refused: "AOL accepts only an app password here, not your AOL account password. Create one at login.aol.com/account/security; one you deleted there no longer works.",
    custom_domain: "This address isn’t at an AOL Mail domain. These are AOL Mail’s own servers; check them against the settings your mail provider gives you.",
    offers_pop3: true,
    alternate_user_names: false,
    strip_spaces: true,
    looks_like_app_password: looks_like_yahoo_app_password,
    servers: aol_servers,
};

/// Fastmail, from its help pages "App passwords" (fastmail.help 360058752854), "Server names
/// and ports" (1500000278342) and "Choosing your data residency" (16796454162063), November
/// 2026. US and EU accounts alike use the standard server names; the optional regional ones
/// (`eu-imap.fastmail.com`, `us-imap.fastmail.com`) are recognised by host.
pub static FASTMAIL: AppPasswordGuide = AppPasswordGuide {
    provider: "Fastmail",
    short_name: "Fastmail",
    domains: &["fastmail.com", "fastmail.fm", "messagingengine.com", "fastmail.us", "fastmail.uk", "fastmail.de"],
    hosts: &["fastmail.com", "messagingengine.com"],
    explanation: "Fastmail needs its own app password for each mail app, not your Fastmail password. Create one under Settings → Privacy & Security → Manage app passwords and access → New app password; its default access, Mail, Contacts & Calendars, also lets Overlook connect your calendars. Fastmail’s Basic plan doesn’t include mail apps. Overlook keeps the password in the system keyring.",
    password_label: "App password",
    password_placeholder: "16 letters and digits from Fastmail",
    create_label: "Create an app password",
    create_url: "https://app.fastmail.com/settings/security",
    refused: "Fastmail accepts only an app password here, not your Fastmail password, and its Basic plan doesn’t allow mail apps at all. Create one under Settings → Privacy & Security → Manage app passwords and access; one you removed or disabled there no longer works.",
    custom_domain: "Fastmail signs you in with the address you log in to Fastmail with. If that isn’t this one, enter it as both user names below.",
    offers_pop3: true,
    alternate_user_names: false,
    strip_spaces: true,
    looks_like_app_password: looks_like_fastmail_app_password,
    servers: fastmail_servers,
};

/// Every guided service, in the order the "Add … account" entries list them.
pub static GUIDES: &[&AppPasswordGuide] = &[&ICLOUD, &YAHOO, &AOL, &FASTMAIL];

/// The guide for an address's service, or, for an address at another domain, for the
/// service its incoming server belongs to.
pub fn guide_for(email: &str, incoming_host: Option<&str>) -> Option<&'static AppPasswordGuide> {
    GUIDES
        .iter()
        .copied()
        .find(|g| g.owns_address(email) || incoming_host.is_some_and(|h| g.owns_host(h)))
}

fn domain_of(email: &str) -> String {
    email
        .trim()
        .rsplit('@')
        .next()
        .unwrap_or("")
        .trim()
        .trim_end_matches('.')
        .to_ascii_lowercase()
}

impl AppPasswordGuide {
    /// Whether `email` is at one of the service's own domains.
    pub fn owns_address(&self, email: &str) -> bool {
        email.contains('@') && self.domains.contains(&domain_of(email).as_str())
    }

    /// Whether `host` is one of the service's servers.
    pub fn owns_host(&self, host: &str) -> bool {
        let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
        self.hosts
            .iter()
            .any(|h| host == *h || host.ends_with(&format!(".{h}")))
    }

    /// The incoming user names a connection test tries, `configured` first: with
    /// [`Self::alternate_user_names`], also the other form (the name part of an address, or
    /// the whole address for a bare name).
    pub fn user_names(&self, configured: &str, email: &str) -> Vec<String> {
        let configured = configured.trim().to_owned();
        let mut names = vec![configured.clone()];
        if self.alternate_user_names {
            let email = email.trim().to_lowercase();
            let other = match configured.split_once('@') {
                Some((name, _)) => name.to_owned(),
                // The address itself when it is the service's own; otherwise the name at the
                // service's main domain (a custom domain's address is not the sign-in name).
                None if self.owns_address(&email)
                    && email.split('@').next() == Some(configured.to_lowercase().as_str()) =>
                {
                    email
                }
                None => format!("{configured}@{}", self.domains[0]),
            };
            if !other.is_empty() && other != configured {
                names.push(other);
            }
        }
        names
    }
}

/// Yahoo's and AOL's app passwords are 16 letters, sometimes shown in groups with spaces.
fn looks_like_yahoo_app_password(password: &str) -> bool {
    let letters: Vec<char> = password.chars().filter(|c| !c.is_whitespace()).collect();
    letters.len() == 16 && letters.iter().all(char::is_ascii_alphabetic)
}

/// Fastmail's app passwords are 16 letters and digits (without 0, 1, O or I); spaces and
/// capitalisation don't matter to Fastmail.
fn looks_like_fastmail_app_password(password: &str) -> bool {
    let characters: Vec<char> = password.chars().filter(|c| !c.is_whitespace()).collect();
    characters.len() == 16 && characters.iter().all(char::is_ascii_alphanumeric)
}

/// Apple's app-specific passwords are 16 letters, shown in four groups of four with hyphens.
fn looks_like_apple_app_password(password: &str) -> bool {
    let password = password.trim();
    let letters: Vec<char> = password.chars().filter(|c| *c != '-').collect();
    let grouped = password.len() == 19
        && password.char_indices().all(|(i, c)| {
            if matches!(i, 4 | 9 | 14) {
                c == '-'
            } else {
                c != '-'
            }
        });
    letters.len() == 16
        && letters.iter().all(char::is_ascii_alphabetic)
        && (grouped || !password.contains('-'))
}

/// `imap.mail.me.com` on 993 with TLS and `smtp.mail.me.com` on 587 with STARTTLS. Apple: the
/// incoming user name "is usually the name of your iCloud Mail email address (for example,
/// johnappleseed, not johnappleseed@icloud.com)"; the outgoing one is the full address. For
/// an address at a custom domain the iCloud name is unknown here, so both start as the
/// address and [`AppPasswordGuide::custom_domain`] explains what to enter.
fn icloud_servers(_incoming: ServerProtocol, email: &str) -> MailServers {
    let email = email.trim().to_lowercase();
    let incoming = if ICLOUD.owns_address(&email) {
        email.split('@').next().unwrap_or(&email).to_owned()
    } else {
        email.clone()
    };
    MailServers {
        incoming: ServerEndpoint::new("imap.mail.me.com", 993, Security::Tls, incoming),
        outgoing: ServerEndpoint::new("smtp.mail.me.com", 587, Security::StartTls, email),
        pop3: None,
        imap: None,
        oauth: None,
    }
}

/// The full address signs in everywhere, with implicit TLS: IMAP 993 or POP3 995, SMTP 465.
fn full_address_servers(
    incoming: ServerProtocol,
    email: &str,
    imap: &str,
    pop3: &str,
    smtp: &str,
) -> MailServers {
    let email = email.trim().to_lowercase();
    let (host, port) = if incoming == ServerProtocol::Pop3 {
        (pop3, 995)
    } else {
        (imap, 993)
    };
    MailServers {
        incoming: ServerEndpoint::new(host, port, Security::Tls, email.clone()),
        outgoing: ServerEndpoint::new(smtp, 465, Security::Tls, email),
        pop3: None,
        imap: None,
        oauth: None,
    }
}

/// Yahoo: `imap.mail.yahoo.com` 993 or `pop.mail.yahoo.com` 995, `smtp.mail.yahoo.com` 465,
/// all with SSL, the full address as user name.
fn yahoo_servers(incoming: ServerProtocol, email: &str) -> MailServers {
    full_address_servers(
        incoming,
        email,
        "imap.mail.yahoo.com",
        "pop.mail.yahoo.com",
        "smtp.mail.yahoo.com",
    )
}

/// Fastmail: `imap.fastmail.com` 993 or `pop.fastmail.com` 995, `smtp.fastmail.com` 465, all
/// with SSL (not STARTTLS on 993 and 995), the sign-in address as user name.
fn fastmail_servers(incoming: ServerProtocol, email: &str) -> MailServers {
    full_address_servers(
        incoming,
        email,
        "imap.fastmail.com",
        "pop.fastmail.com",
        "smtp.fastmail.com",
    )
}

/// AOL: `imap.aol.com` 993 or `pop.aol.com` 995, `smtp.aol.com` 465, all with SSL, the full
/// address as user name.
fn aol_servers(incoming: ServerProtocol, email: &str) -> MailServers {
    full_address_servers(
        incoming,
        email,
        "imap.aol.com",
        "pop.aol.com",
        "smtp.aol.com",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icloud_servers_follow_apple() {
        let s = (ICLOUD.servers)(ServerProtocol::Imap, " Jane.Doe@iCloud.com ");
        assert_eq!(
            (
                s.incoming.address(),
                s.incoming.security,
                s.incoming.username.as_str()
            ),
            ("imap.mail.me.com:993".to_owned(), Security::Tls, "jane.doe")
        );
        assert_eq!(
            (
                s.outgoing.address(),
                s.outgoing.security,
                s.outgoing.username.as_str()
            ),
            (
                "smtp.mail.me.com:587".to_owned(),
                Security::StartTls,
                "jane.doe@icloud.com"
            )
        );
        let custom = (ICLOUD.servers)(ServerProtocol::Imap, "jane@family.example");
        assert_eq!(
            (
                custom.incoming.username.as_str(),
                custom.outgoing.username.as_str()
            ),
            ("jane@family.example", "jane@family.example"),
            "a custom domain's iCloud name is not guessed"
        );
    }

    #[test]
    fn guides_are_found_by_address_or_server() {
        assert!(guide_for("x@me.com", None).is_some_and(|g| std::ptr::eq(g, &ICLOUD)));
        assert!(guide_for("x@MAC.com", None).is_some());
        assert!(
            guide_for("jane@family.example", Some("imap.mail.me.com")).is_some(),
            "a custom domain on iCloud's servers"
        );
        assert!(guide_for("jane@family.example", Some("p42-imap.mail.me.com.")).is_some());
        assert!(guide_for("jane@family.example", Some("imap.example.net")).is_none());
        assert!(guide_for("x@notme.com", Some("mail.me.com.evil.example")).is_none());
    }

    #[test]
    fn the_other_user_name_form_is_tried() {
        assert_eq!(
            ICLOUD.user_names("jane", "jane@icloud.com"),
            ["jane", "jane@icloud.com"]
        );
        assert_eq!(
            ICLOUD.user_names("jane@me.com", "jane@me.com"),
            ["jane@me.com", "jane"]
        );
        assert_eq!(
            ICLOUD.user_names("jane", "jane@family.example"),
            ["jane", "jane@icloud.com"]
        );
    }

    #[test]
    fn yahoo_and_aol_servers_take_the_full_address_and_offer_pop3() {
        let imap = (YAHOO.servers)(ServerProtocol::Imap, "Jane@Yahoo.co.uk");
        assert_eq!(
            (
                imap.incoming.address(),
                imap.incoming.security,
                imap.incoming.username.as_str()
            ),
            (
                "imap.mail.yahoo.com:993".to_owned(),
                Security::Tls,
                "jane@yahoo.co.uk"
            )
        );
        assert_eq!(
            (
                imap.outgoing.address(),
                imap.outgoing.security,
                imap.outgoing.username.as_str()
            ),
            (
                "smtp.mail.yahoo.com:465".to_owned(),
                Security::Tls,
                "jane@yahoo.co.uk"
            )
        );
        assert_eq!(
            (YAHOO.servers)(ServerProtocol::Pop3, "jane@yahoo.com")
                .incoming
                .address(),
            "pop.mail.yahoo.com:995"
        );
        let aol = (AOL.servers)(ServerProtocol::Pop3, "jane@aol.com");
        assert_eq!(
            (
                aol.incoming.address(),
                aol.outgoing.address(),
                aol.incoming.username.as_str()
            ),
            (
                "pop.aol.com:995".to_owned(),
                "smtp.aol.com:465".to_owned(),
                "jane@aol.com"
            )
        );
        assert_eq!(
            (AOL.servers)(ServerProtocol::Imap, "jane@aim.com")
                .incoming
                .address(),
            "imap.aol.com:993"
        );
        assert!(YAHOO.offers_pop3 && AOL.offers_pop3 && !ICLOUD.offers_pop3);
    }

    #[test]
    fn yahoo_and_aol_are_found_by_address_or_server_but_not_yahoo_japan() {
        assert!(guide_for("x@ymail.com", None).is_some_and(|g| std::ptr::eq(g, &YAHOO)));
        assert!(guide_for("x@love.com", None).is_some_and(|g| std::ptr::eq(g, &AOL)));
        assert!(
            guide_for("x@yahoo.co.jp", None).is_none(),
            "Yahoo! JAPAN is a separate service"
        );
        assert!(
            guide_for("x@verizon.net", None).is_none(),
            "Verizon is qualified separately"
        );
        assert!(guide_for("me@family.example", Some("imap.mail.yahoo.com"))
            .is_some_and(|g| std::ptr::eq(g, &YAHOO)));
        assert!(guide_for("me@family.example", Some("imap.aol.com"))
            .is_some_and(|g| std::ptr::eq(g, &AOL)));
        assert_eq!(
            GUIDES.iter().map(|g| g.short_name).collect::<Vec<_>>(),
            ["iCloud", "Yahoo", "AOL", "Fastmail"]
        );
        assert_eq!(
            YAHOO.user_names("jane@yahoo.com", "jane@yahoo.com"),
            ["jane@yahoo.com"],
            "Yahoo has one user name form"
        );
    }

    #[test]
    fn fastmail_follows_its_help_pages() {
        let imap = (FASTMAIL.servers)(ServerProtocol::Imap, "Jane@Fastmail.com");
        assert_eq!(
            (
                imap.incoming.address(),
                imap.incoming.security,
                imap.incoming.username.as_str()
            ),
            (
                "imap.fastmail.com:993".to_owned(),
                Security::Tls,
                "jane@fastmail.com"
            )
        );
        assert_eq!(
            (imap.outgoing.address(), imap.outgoing.security),
            ("smtp.fastmail.com:465".to_owned(), Security::Tls)
        );
        assert_eq!(
            (FASTMAIL.servers)(ServerProtocol::Pop3, "jane@fastmail.fm")
                .incoming
                .address(),
            "pop.fastmail.com:995"
        );
        assert!(guide_for("jane@fastmail.fm", None).is_some_and(|g| std::ptr::eq(g, &FASTMAIL)));
        for host in [
            "imap.fastmail.com",
            "eu-imap.fastmail.com",
            "mail.messagingengine.com",
        ] {
            assert!(
                guide_for("me@own.example", Some(host)).is_some_and(|g| std::ptr::eq(g, &FASTMAIL)),
                "{host}"
            );
        }
        let looks = FASTMAIL.looks_like_app_password;
        assert!(
            looks("abcd2345efgh6789")
                && looks("ABCD 2345 efgh 6789")
                && !looks("hunter2")
                && !looks("abcd-2345-efgh-6789")
        );
        assert!(FASTMAIL.strip_spaces && FASTMAIL.offers_pop3);
    }

    #[test]
    fn app_password_shape() {
        let yahoo = YAHOO.looks_like_app_password;
        assert!(
            yahoo("abcdefghijklmnop")
                && yahoo("abcd efgh ijkl mnop")
                && !yahoo("hunter2")
                && !yahoo("abcd-efgh-ijkl-mnop")
        );
        assert!(YAHOO.strip_spaces && AOL.strip_spaces && !ICLOUD.strip_spaces);
        let looks = ICLOUD.looks_like_app_password;
        assert!(
            looks("abcd-efgh-ijkl-mnop")
                && looks(" ABCD-efgh-ijkl-mnop ")
                && looks("abcdefghijklmnop")
        );
        assert!(
            !looks("hunter2")
                && !looks("abcd-efgh-ijkl-mno1")
                && !looks("abc-defgh-ijkl-mnop")
                && !looks("abcd-efgh-ijkl-mnop-qrst")
        );
    }
}
