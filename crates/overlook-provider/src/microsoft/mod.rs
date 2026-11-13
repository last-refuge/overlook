//! Microsoft accounts (Outlook.com, Hotmail, Live and MSN addresses, and Microsoft 365 work
//! or school accounts) over IMAP or POP3 and SMTP, signed in through the Microsoft identity
//! platform with SASL `XOAUTH2`. Microsoft no longer accepts passwords from mail apps for
//! these protocols, so this is the only way in.
//!
//! - `oauth`: the app registration, the authorization request (PKCE, loopback redirect),
//!   token exchange and refresh, and the account details in the ID token.
//! - `session`: the account's token source for the mail protocols.
//!
//! Mail itself goes through the ordinary IMAP, POP3 and SMTP providers: a Microsoft account
//! is an IMAP (or POP3) account whose [`overlook_core::MailServers::oauth`] says Microsoft.

pub mod oauth;
pub mod session;

pub use oauth::{AuthorizationRequest, ClientConfig, IdClaims, SignedIn};
pub use session::MicrosoftSession;

use overlook_core::{MailServers, OAuthIssuer, Security, ServerEndpoint, ServerProtocol};

/// The directory Microsoft issues personal accounts' tokens from (`tid` in their ID tokens).
pub const CONSUMER_TENANT_ID: &str = "9188040d-6c67-4c5b-b112-36a304b66dad";

/// Where every Microsoft mailbox is read, over IMAP (993) or POP3 (995).
pub const INCOMING_HOST: &str = "outlook.office365.com";
/// Where personal accounts send.
pub const PERSONAL_SMTP_HOST: &str = "smtp-mail.outlook.com";
/// Where work or school accounts send.
pub const WORK_SMTP_HOST: &str = "smtp.office365.com";

/// Microsoft's own domains for personal accounts. Addresses at other domains can be personal
/// accounts too (Microsoft 365 Family custom domains) or work accounts; the ID token's
/// tenant decides once the person has signed in.
pub const CONSUMER_DOMAINS: &[&str] = &[
    "outlook.com",
    "hotmail.com",
    "live.com",
    "msn.com",
    "passport.com",
    "windowslive.com",
    "outlook.at",
    "outlook.be",
    "outlook.cl",
    "outlook.co.id",
    "outlook.co.il",
    "outlook.co.nz",
    "outlook.co.th",
    "outlook.com.ar",
    "outlook.com.au",
    "outlook.com.br",
    "outlook.com.gr",
    "outlook.com.pe",
    "outlook.com.tr",
    "outlook.com.vn",
    "outlook.cz",
    "outlook.de",
    "outlook.dk",
    "outlook.es",
    "outlook.fr",
    "outlook.hu",
    "outlook.ie",
    "outlook.in",
    "outlook.it",
    "outlook.jp",
    "outlook.kr",
    "outlook.lv",
    "outlook.my",
    "outlook.ph",
    "outlook.pt",
    "outlook.sa",
    "outlook.sg",
    "outlook.sk",
    "hotmail.be",
    "hotmail.ca",
    "hotmail.ch",
    "hotmail.cl",
    "hotmail.co.il",
    "hotmail.co.jp",
    "hotmail.co.kr",
    "hotmail.co.nz",
    "hotmail.co.th",
    "hotmail.co.uk",
    "hotmail.co.za",
    "hotmail.com.ar",
    "hotmail.com.au",
    "hotmail.com.br",
    "hotmail.com.hk",
    "hotmail.com.tr",
    "hotmail.com.tw",
    "hotmail.com.vn",
    "hotmail.cz",
    "hotmail.de",
    "hotmail.dk",
    "hotmail.es",
    "hotmail.fi",
    "hotmail.fr",
    "hotmail.gr",
    "hotmail.hu",
    "hotmail.it",
    "hotmail.lt",
    "hotmail.lv",
    "hotmail.my",
    "hotmail.nl",
    "hotmail.no",
    "hotmail.ph",
    "hotmail.rs",
    "hotmail.se",
    "hotmail.sg",
    "hotmail.sk",
    "live.at",
    "live.be",
    "live.ca",
    "live.cl",
    "live.cn",
    "live.co.kr",
    "live.co.uk",
    "live.co.za",
    "live.com.ar",
    "live.com.au",
    "live.com.mx",
    "live.com.my",
    "live.com.ph",
    "live.com.pt",
    "live.com.sg",
    "live.de",
    "live.dk",
    "live.fi",
    "live.fr",
    "live.hk",
    "live.ie",
    "live.in",
    "live.it",
    "live.jp",
    "live.nl",
    "live.no",
    "live.ru",
    "live.se",
];

/// Whether `email` is at one of Microsoft's personal-account domains.
pub fn is_consumer_address(email: &str) -> bool {
    let domain = email
        .trim()
        .rsplit('@')
        .next()
        .unwrap_or("")
        .trim_end_matches('.')
        .to_ascii_lowercase();
    CONSUMER_DOMAINS.contains(&domain.as_str())
}

/// Whether `host` is one of Microsoft's mail servers, under the names Microsoft documents now
/// or documented before.
pub fn is_microsoft_host(host: &str) -> bool {
    const HOSTS: &[&str] = &[
        INCOMING_HOST,
        PERSONAL_SMTP_HOST,
        WORK_SMTP_HOST,
        "outlook.office.com",
        "imap-mail.outlook.com",
        "pop-mail.outlook.com",
        "imap.outlook.com",
        "pop3.live.com",
        "smtp.live.com",
    ];
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    HOSTS.contains(&host.as_str())
}

/// Whether the signed-in account is a personal one: from the ID token's tenant when there is
/// one, otherwise from the address.
pub fn is_personal(claims: Option<&IdClaims>, email: &str) -> bool {
    claims
        .and_then(IdClaims::is_personal)
        .unwrap_or_else(|| is_consumer_address(email))
}

/// Microsoft's documented server settings for a mailbox, signing in as `email` through
/// Microsoft: `outlook.office365.com` for reading (IMAP 993 or POP3 995, TLS), and SMTP with
/// STARTTLS on 587 at `smtp-mail.outlook.com` for personal accounts or `smtp.office365.com`
/// for work or school accounts.
pub fn servers(incoming: ServerProtocol, email: &str, personal: bool) -> MailServers {
    let email = email.trim().to_lowercase();
    let port = if incoming == ServerProtocol::Pop3 {
        995
    } else {
        993
    };
    let smtp_host = if personal {
        PERSONAL_SMTP_HOST
    } else {
        WORK_SMTP_HOST
    };
    MailServers {
        incoming: ServerEndpoint::new(INCOMING_HOST, port, Security::Tls, email.clone()),
        outgoing: ServerEndpoint::new(smtp_host, 587, Security::StartTls, email),
        pop3: None,
        imap: None,
        oauth: None,
    }
    .with_oauth(OAuthIssuer::Microsoft)
}

#[cfg(test)]
mod tests {
    use super::*;
    use overlook_core::AuthMethod;

    #[test]
    fn server_settings_follow_the_account_type() {
        let personal = servers(ServerProtocol::Imap, " Jason@Outlook.com ", true);
        assert_eq!(
            (
                personal.incoming.address(),
                personal.incoming.security,
                personal.incoming.username.as_str()
            ),
            (
                "outlook.office365.com:993".to_owned(),
                Security::Tls,
                "jason@outlook.com"
            )
        );
        assert_eq!(
            (personal.outgoing.address(), personal.outgoing.security),
            ("smtp-mail.outlook.com:587".to_owned(), Security::StartTls)
        );
        assert_eq!(
            (
                personal.oauth,
                personal.incoming.auth,
                personal.outgoing.auth
            ),
            (
                Some(OAuthIssuer::Microsoft),
                AuthMethod::OAuth2,
                AuthMethod::OAuth2
            )
        );
        let work = servers(ServerProtocol::Pop3, "jason@contoso.com", false);
        assert_eq!(
            (work.incoming.address(), work.outgoing.address()),
            (
                "outlook.office365.com:995".to_owned(),
                "smtp.office365.com:587".to_owned()
            )
        );
    }

    #[test]
    fn personal_accounts_are_recognised_by_tenant_then_domain() {
        assert!(
            is_consumer_address("someone@hotmail.co.uk")
                && is_consumer_address("x@LIVE.com")
                && is_consumer_address("x@outlook.com.")
        );
        assert!(
            !is_consumer_address("jason@contoso.com") && !is_consumer_address("live@example.com")
        );
        let work = IdClaims {
            tenant_id: Some("72f988bf-86f1-41af-91ab-2d7cd011db47".into()),
            ..Default::default()
        };
        let family = IdClaims {
            tenant_id: Some(CONSUMER_TENANT_ID.into()),
            ..Default::default()
        };
        assert!(
            !is_personal(Some(&work), "j@outlook.com"),
            "the tenant outranks the domain"
        );
        assert!(
            is_personal(Some(&family), "j@family-domain.example"),
            "a personal account at its own domain"
        );
        assert!(is_personal(None, "j@outlook.com") && !is_personal(None, "j@contoso.com"));
        assert!(
            is_microsoft_host("IMAP-MAIL.outlook.com.")
                && is_microsoft_host("outlook.office365.com")
                && !is_microsoft_host("imap.fastmail.com")
        );
    }
}
