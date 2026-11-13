//! Server settings for an email address: Microsoft's own domains, then a small preset table
//! (with the guided app-password services of [`crate::app_passwords`]), then Thunderbird's
//! ISPDB, then the common `mail.<domain>` guess. Services that offer no IMAP, POP3 or SMTP
//! access at all are named by [`unsupported`] instead.

use overlook_core::{MailServers, Security, ServerEndpoint, ServerProtocol};
use std::time::Duration;

/// Where the settings came from, for the dialog's explanation line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// Thunderbird's autoconfiguration database.
    Ispdb,
    /// Overlook's built-in table of common providers.
    Preset,
    /// The `mail.<domain>` convention many hosting panels follow; unverified.
    Guess,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    pub servers: MailServers,
    pub source: Source,
    /// The provider's name, when known.
    pub provider: Option<String>,
}

pub const ISPDB_BASE: &str = "https://autoconfig.thunderbird.net/v1.1/";

/// Looks the domain up for an IMAP account, online then offline. Never fails: the worst
/// case is the guess.
pub fn lookup(email: &str) -> Suggestion {
    lookup_for(ServerProtocol::Imap, email)
}

/// [`lookup`] for the given incoming protocol (IMAP or POP3). Outlook.com, Hotmail, Live and
/// MSN addresses get Microsoft's settings with [`MailServers::oauth`] set: Microsoft accepts
/// only its own sign-in from mail apps.
pub fn lookup_for(protocol: ServerProtocol, email: &str) -> Suggestion {
    let email = email.trim();
    let domain = email.rsplit('@').next().unwrap_or("").trim().to_ascii_lowercase();
    if crate::microsoft::is_consumer_address(email) {
        return Suggestion { servers: crate::microsoft::servers(protocol, email, true), source: Source::Preset, provider: Some("Outlook.com".into()) };
    }
    let preset = match protocol {
        ServerProtocol::Pop3 => preset_pop3(&domain, email),
        _ => preset(&domain, email),
    };
    if let Some(s) = preset {
        return s;
    }
    if let Some(s) = ispdb(protocol, &domain, email) {
        return s;
    }
    guess_for(protocol, &domain, email)
}

/// POP3 settings for the providers that still offer POP3; most of the IMAP table do not.
pub fn preset_pop3(domain: &str, email: &str) -> Option<Suggestion> {
    const TABLE: &[(&[&str], &str, &str, u16, &str, u16, Security)] = &[
        (&["fastmail.com", "fastmail.fm", "messagingengine.com", "fastmail.us", "fastmail.uk", "fastmail.de"], "Fastmail", "pop.fastmail.com", 995, "smtp.fastmail.com", 465, Security::Tls),
        (&["yahoo.com", "ymail.com", "rocketmail.com", "yahoo.co.uk", "yahoo.ca", "yahoo.de", "yahoo.fr"], "Yahoo Mail", "pop.mail.yahoo.com", 995, "smtp.mail.yahoo.com", 465, Security::Tls),
        (&["aol.com"], "AOL Mail", "pop.aol.com", 995, "smtp.aol.com", 465, Security::Tls),
        (&["zoho.com", "zohomail.com", "zoho.eu", "zohomail.eu"], "Zoho Mail", "poppro.zoho.com", 995, "smtp.zoho.com", 465, Security::Tls),
        (&["posteo.de", "posteo.net", "posteo.eu", "posteo.org", "posteo.ch", "posteo.at"], "Posteo", "posteo.de", 995, "posteo.de", 465, Security::Tls),
        (&["mailbox.org"], "mailbox.org", "pop3.mailbox.org", 995, "smtp.mailbox.org", 465, Security::Tls),
        (&["gmx.de", "gmx.net", "gmx.at", "gmx.ch", "gmx.com", "gmx.us"], "GMX", "pop.gmx.net", 995, "mail.gmx.net", 465, Security::Tls),
        (&["web.de"], "WEB.DE", "pop3.web.de", 995, "smtp.web.de", 465, Security::Tls),
        (&["gmail.com", "googlemail.com"], "Gmail over POP3 (needs an app password and POP enabled in Gmail)", "pop.gmail.com", 995, "smtp.gmail.com", 465, Security::Tls),
    ];
    let row = TABLE.iter().find(|(domains, ..)| domains.iter().any(|d| *d == domain))?;
    let (_, provider, ih, ip, sh, sp, ssec) = *row;
    Some(Suggestion { servers: servers(ih, ip, Security::Tls, sh, sp, ssec, email), source: Source::Preset, provider: Some(provider.to_owned()) })
}

/// The built-in table alone (no network), for tests and the offline path.
pub fn preset(domain: &str, email: &str) -> Option<Suggestion> {
    if let Some(guide) = crate::app_passwords::GUIDES.iter().find(|g| g.domains.contains(&domain)) {
        return Some(Suggestion { servers: (guide.servers)(email), source: Source::Preset, provider: Some(guide.provider.to_owned()) });
    }
    // (domains, provider, imap host, imap port, imap security, smtp host, smtp port, smtp security)
    const TABLE: &[(&[&str], &str, &str, u16, Security, &str, u16, Security)] = &[
        (&["fastmail.com", "fastmail.fm", "messagingengine.com", "fastmail.us", "fastmail.uk", "fastmail.de"], "Fastmail", "imap.fastmail.com", 993, Security::Tls, "smtp.fastmail.com", 465, Security::Tls),
        (&["yahoo.com", "ymail.com", "rocketmail.com", "yahoo.co.uk", "yahoo.ca", "yahoo.de", "yahoo.fr"], "Yahoo Mail", "imap.mail.yahoo.com", 993, Security::Tls, "smtp.mail.yahoo.com", 465, Security::Tls),
        (&["aol.com"], "AOL Mail", "imap.aol.com", 993, Security::Tls, "smtp.aol.com", 465, Security::Tls),
        (&["zoho.com", "zohomail.com", "zoho.eu", "zohomail.eu"], "Zoho Mail", "imap.zoho.com", 993, Security::Tls, "smtp.zoho.com", 465, Security::Tls),
        (&["proton.me", "protonmail.com", "protonmail.ch", "pm.me"], "Proton Mail Bridge", "127.0.0.1", 1143, Security::StartTls, "127.0.0.1", 1025, Security::StartTls),
        (&["posteo.de", "posteo.net", "posteo.eu", "posteo.org", "posteo.ch", "posteo.at"], "Posteo", "posteo.de", 993, Security::Tls, "posteo.de", 465, Security::Tls),
        (&["mailbox.org"], "mailbox.org", "imap.mailbox.org", 993, Security::Tls, "smtp.mailbox.org", 465, Security::Tls),
        (&["gmx.de", "gmx.net", "gmx.at", "gmx.ch", "gmx.com", "gmx.us"], "GMX", "imap.gmx.net", 993, Security::Tls, "mail.gmx.net", 465, Security::Tls),
        (&["web.de"], "WEB.DE", "imap.web.de", 993, Security::Tls, "smtp.web.de", 465, Security::Tls),
        (&["gmail.com", "googlemail.com"], "Gmail over IMAP (needs an app password)", "imap.gmail.com", 993, Security::Tls, "smtp.gmail.com", 465, Security::Tls),
    ];
    let row = TABLE.iter().find(|(domains, ..)| domains.iter().any(|d| *d == domain))?;
    let (_, provider, ih, ip, isec, sh, sp, ssec) = *row;
    Some(Suggestion { servers: servers(ih, ip, isec, sh, sp, ssec, email), source: Source::Preset, provider: Some(provider.to_owned()) })
}

/// Why an address's service cannot be added as an IMAP or POP3 account, for services that
/// do not let other mail apps in at all. Checked before any lookup.
pub fn unsupported(email: &str) -> Option<&'static str> {
    let domain = email.trim().rsplit('@').next().unwrap_or("").trim().trim_end_matches('.').to_ascii_lowercase();
    match domain.as_str() {
        "hey.com" => Some("HEY doesn’t let other mail apps read or send its mail, so it can’t be added to Overlook. Use HEY’s own apps."),
        "tuta.com" | "tuta.io" | "tutanota.com" | "tutanota.de" | "tutamail.com" | "keemail.me" => Some("Tuta doesn’t offer IMAP, POP3 or SMTP, so other mail apps can’t connect to it. Use Tuta’s own apps."),
        _ => None,
    }
}

pub fn guess(domain: &str, email: &str) -> Suggestion {
    guess_for(ServerProtocol::Imap, domain, email)
}

/// The `mail.<domain>` convention with the protocol's implicit-TLS port.
pub fn guess_for(protocol: ServerProtocol, domain: &str, email: &str) -> Suggestion {
    let host = if domain.is_empty() { String::new() } else { format!("mail.{domain}") };
    let port = if protocol == ServerProtocol::Pop3 { 995 } else { 993 };
    Suggestion { servers: servers(&host, port, Security::Tls, &host, 587, Security::StartTls, email), source: Source::Guess, provider: None }
}

fn servers(ih: &str, ip: u16, isec: Security, sh: &str, sp: u16, ssec: Security, email: &str) -> MailServers {
    MailServers { incoming: ServerEndpoint::new(ih, ip, isec, email), outgoing: ServerEndpoint::new(sh, sp, ssec, email), pop3: None, imap: None, oauth: None }
}

fn ispdb(protocol: ServerProtocol, domain: &str, email: &str) -> Option<Suggestion> {
    if domain.is_empty() || !domain.contains('.') {
        return None;
    }
    let agent = ureq::Agent::config_builder().https_only(true).timeout_global(Some(Duration::from_secs(8))).build().new_agent();
    let mut response = agent.get(format!("{ISPDB_BASE}{domain}")).call().ok()?;
    if response.status() != 200 {
        return None;
    }
    let body = response.body_mut().read_to_string().ok()?;
    parse_ispdb_for(protocol, &body, email)
}

/// Reads the ISPDB XML for an IMAP account. The format is small and regular, so a tag
/// scanner is enough: the first `incomingServer type="imap"` and the first
/// `outgoingServer` win.
pub fn parse_ispdb(xml: &str, email: &str) -> Option<Suggestion> {
    parse_ispdb_for(ServerProtocol::Imap, xml, email)
}

/// [`parse_ispdb`] for the given incoming protocol (`type="imap"` or `type="pop3"`).
pub fn parse_ispdb_for(protocol: ServerProtocol, xml: &str, email: &str) -> Option<Suggestion> {
    let wanted = if protocol == ServerProtocol::Pop3 { "type=\"pop3\"" } else { "type=\"imap\"" };
    let provider = text_in(xml, "displayName").map(|s| s.trim().to_owned()).filter(|s| !s.is_empty());
    let incoming_xml = elements(xml, "incomingServer").into_iter().find(|(attrs, _)| attrs.contains(wanted))?.1;
    let outgoing_xml = elements(xml, "outgoingServer").into_iter().next()?.1;
    let endpoint = |block: &str, default_port: u16| -> Option<ServerEndpoint> {
        let host = text_in(block, "hostname")?.trim().to_owned();
        let port = text_in(block, "port").and_then(|p| p.trim().parse().ok()).unwrap_or(default_port);
        let security = match text_in(block, "socketType").map(|s| s.trim().to_ascii_uppercase()).as_deref() {
            Some("SSL") => Security::Tls,
            Some("STARTTLS") => Security::StartTls,
            Some("PLAIN") => Security::None,
            _ => Security::Tls,
        };
        let username = match text_in(block, "username").as_deref().map(str::trim) {
            Some("%EMAILLOCALPART%") => email.split('@').next().unwrap_or(email).to_owned(),
            Some(other) if !other.is_empty() && !other.starts_with('%') => other.to_owned(),
            _ => email.to_owned(),
        };
        Some(ServerEndpoint::new(host, port, security, username))
    };
    let incoming = endpoint(&incoming_xml, if protocol == ServerProtocol::Pop3 { 995 } else { 993 })?;
    let outgoing = endpoint(&outgoing_xml, 587)?;
    if incoming.is_insecure_remote() || outgoing.is_insecure_remote() {
        return None;
    }
    Some(Suggestion { servers: MailServers { incoming, outgoing, pop3: None, imap: None, oauth: None }, source: Source::Ispdb, provider })
}

/// `(attributes, inner text)` of every `<tag …>…</tag>` element, in document order.
fn elements(xml: &str, tag: &str) -> Vec<(String, String)> {
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(start) = rest.find(&open) {
        let after = &rest[start + open.len()..];
        let Some(gt) = after.find('>') else { break };
        let attrs = after[..gt].to_owned();
        let inner_start = gt + 1;
        let Some(end) = after[inner_start..].find(&close) else { break };
        out.push((attrs, after[inner_start..inner_start + end].to_owned()));
        rest = &after[inner_start + end + close.len()..];
    }
    out
}

fn text_in(xml: &str, tag: &str) -> Option<String> {
    elements(xml, tag).into_iter().next().map(|(_, inner)| unescape(inner.trim()))
}

fn unescape(s: &str) -> String {
    s.replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_and_guesses() {
        let s = preset("fastmail.com", "jason@fastmail.com").unwrap();
        assert_eq!(s.source, Source::Preset);
        assert_eq!(s.provider.as_deref(), Some("Fastmail"));
        assert_eq!(s.servers.incoming.host, "imap.fastmail.com");
        assert_eq!(s.servers.incoming.username, "jason@fastmail.com");
        assert_eq!((s.servers.outgoing.port, s.servers.outgoing.security), (465, Security::Tls));
        assert!(preset("example.com", "x@example.com").is_none());
        let g = guess("example.com", "x@example.com");
        assert_eq!((g.source, g.servers.incoming.host.as_str(), g.servers.outgoing.port), (Source::Guess, "mail.example.com", 587));
        assert_eq!(preset("proton.me", "x@proton.me").unwrap().servers.incoming.host, "127.0.0.1");
    }

    #[test]
    fn microsoft_addresses_sign_in_with_microsoft() {
        let s = lookup_for(ServerProtocol::Imap, "Jason@Hotmail.co.uk");
        assert_eq!((s.source, s.provider.as_deref(), s.servers.oauth), (Source::Preset, Some("Outlook.com"), Some(overlook_core::OAuthIssuer::Microsoft)));
        assert_eq!((s.servers.incoming.address(), s.servers.outgoing.address()), ("outlook.office365.com:993".to_owned(), "smtp-mail.outlook.com:587".to_owned()));
        assert_eq!(lookup_for(ServerProtocol::Pop3, "x@outlook.com").servers.incoming.port, 995);
    }

    #[test]
    fn services_without_mail_app_access_are_explained_not_guessed() {
        assert!(unsupported("x@hey.com").unwrap().contains("HEY"));
        assert!(unsupported("x@Tuta.io").unwrap().contains("Tuta"));
        assert!(unsupported("x@fastmail.com").is_none());
        assert!(preset("hey.com", "x@hey.com").is_none() && preset("tuta.com", "x@tuta.com").is_none(), "no invented endpoints");
    }

    #[test]
    fn parses_ispdb_xml() {
        let xml = r#"<?xml version="1.0"?>
<clientConfig version="1.1">
  <emailProvider id="example.com">
    <domain>example.com</domain>
    <displayName>Example &amp; Co</displayName>
    <incomingServer type="pop3"><hostname>pop.example.com</hostname><port>995</port><socketType>SSL</socketType><username>%EMAILADDRESS%</username></incomingServer>
    <incomingServer type="imap">
      <hostname>imap.example.com</hostname>
      <port>143</port>
      <socketType>STARTTLS</socketType>
      <authentication>password-cleartext</authentication>
      <username>%EMAILLOCALPART%</username>
    </incomingServer>
    <outgoingServer type="smtp">
      <hostname>smtp.example.com</hostname>
      <port>465</port>
      <socketType>SSL</socketType>
      <username>%EMAILADDRESS%</username>
    </outgoingServer>
  </emailProvider>
</clientConfig>"#;
        let s = parse_ispdb(xml, "jason@example.com").unwrap();
        assert_eq!(s.source, Source::Ispdb);
        assert_eq!(s.provider.as_deref(), Some("Example & Co"));
        assert_eq!((s.servers.incoming.host.as_str(), s.servers.incoming.port, s.servers.incoming.security, s.servers.incoming.username.as_str()), ("imap.example.com", 143, Security::StartTls, "jason"));
        assert_eq!((s.servers.outgoing.host.as_str(), s.servers.outgoing.port, s.servers.outgoing.security, s.servers.outgoing.username.as_str()), ("smtp.example.com", 465, Security::Tls, "jason@example.com"));
        let pop = parse_ispdb_for(ServerProtocol::Pop3, xml, "jason@example.com").unwrap();
        assert_eq!((pop.servers.incoming.host.as_str(), pop.servers.incoming.port, pop.servers.incoming.security, pop.servers.incoming.username.as_str()), ("pop.example.com", 995, Security::Tls, "jason@example.com"));
        assert_eq!(pop.servers.outgoing.host, "smtp.example.com");
        assert!(parse_ispdb_for(ServerProtocol::Pop3, &xml.replace("type=\"pop3\"", "type=\"exchange\""), "j@example.com").is_none());
        assert_eq!(preset_pop3("gmx.net", "x@gmx.net").unwrap().servers.incoming.host, "pop.gmx.net");
        assert!(preset_pop3("icloud.com", "x@icloud.com").is_none(), "iCloud has no POP3");
        let icloud = preset("icloud.com", "jane@icloud.com").unwrap();
        assert_eq!((icloud.provider.as_deref(), icloud.servers.incoming.username.as_str(), icloud.servers.outgoing.username.as_str()), (Some("iCloud Mail"), "jane", "jane@icloud.com"), "Apple's user names: the name for IMAP, the address for SMTP");
        assert_eq!(guess_for(ServerProtocol::Pop3, "example.com", "x@example.com").servers.incoming.port, 995);
        assert!(parse_ispdb("<clientConfig/>", "x@y.z").is_none());
        let plain = xml.replace("<socketType>STARTTLS</socketType>", "<socketType>plain</socketType>");
        assert!(parse_ispdb(&plain, "jason@example.com").is_none(), "plaintext to a remote host is never suggested");
    }
}
