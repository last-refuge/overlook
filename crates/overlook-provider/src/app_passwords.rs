//! Services that want an app-specific password from mail apps instead of the account's own
//! password, and what the setup dialog tells people about them: what the password is called,
//! where to create one, what it looks like, what a refused sign-in usually means, and the
//! service's server settings.
//!
//! iCloud Mail is the first. Yahoo, AOL, Fastmail and Zoho follow the same pattern (roadmap
//! priorities 3, 4 and 6), so each is one more [`AppPasswordGuide`] in [`GUIDES`].

use overlook_core::{MailServers, Security, ServerEndpoint};

/// How to set up one service that signs in with an app-specific password.
#[derive(Debug)]
pub struct AppPasswordGuide {
    /// The service's name: "iCloud Mail".
    pub provider: &'static str,
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
    /// Whether a password has the shape the service's app passwords have.
    pub looks_like_app_password: fn(&str) -> bool,
    /// The service's server settings for an address, its own or a custom domain's.
    pub servers: fn(&str) -> MailServers,
}

/// iCloud Mail, from Apple's "iCloud Mail server settings for other email client apps"
/// (support.apple.com/102525, February 2026) and "Sign in to apps with your Apple Account
/// using app-specific passwords" (support.apple.com/102654).
pub static ICLOUD: AppPasswordGuide = AppPasswordGuide {
    provider: "iCloud Mail",
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
    looks_like_app_password: looks_like_apple_app_password,
    servers: icloud_servers,
};

/// Every guided service.
pub static GUIDES: &[&AppPasswordGuide] = &[&ICLOUD];

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
fn icloud_servers(email: &str) -> MailServers {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icloud_servers_follow_apple() {
        let s = (ICLOUD.servers)(" Jane.Doe@iCloud.com ");
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
        let custom = (ICLOUD.servers)("jane@family.example");
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
        assert!(guide_for("jane@family.example", Some("imap.fastmail.com")).is_none());
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
    fn app_password_shape() {
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
