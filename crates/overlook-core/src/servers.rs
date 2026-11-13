//! Server settings for IMAP, POP3 and SMTP accounts.
//!
//! These are not secrets and live in the store next to the account. Passwords and OAuth
//! tokens never appear here: they are kept in the system keyring under the account id, so a
//! `ServerEndpoint` can be logged or shown in the settings dialog without leaking anything.

use serde::{Deserialize, Serialize};

/// How the connection is protected.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Security {
    /// Implicit TLS from the first byte (IMAPS 993, POP3S 995, SMTPS 465).
    #[default]
    Tls,
    /// Plain connection upgraded with STARTTLS / STLS (IMAP 143, POP3 110, SMTP 587).
    StartTls,
    /// No encryption. Only acceptable on localhost, for tests and local bridges.
    None,
}

impl Security {
    pub fn label(self) -> &'static str {
        match self {
            Security::Tls => "SSL/TLS",
            Security::StartTls => "STARTTLS",
            Security::None => "None",
        }
    }

    pub fn as_key(self) -> &'static str {
        match self {
            Security::Tls => "tls",
            Security::StartTls => "starttls",
            Security::None => "none",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "tls" | "ssl" => Some(Security::Tls),
            "starttls" => Some(Security::StartTls),
            "none" | "plain" => Some(Security::None),
            _ => None,
        }
    }
}

/// How the client proves who it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum AuthMethod {
    /// `LOGIN` / `AUTH PLAIN` with the password from the keyring.
    #[default]
    Password,
    /// SASL `XOAUTH2` with an access token from the account's OAuth sign-in. Set on both
    /// endpoints of an account whose [`MailServers::oauth`] names the issuer.
    OAuth2,
}

/// Who issues the OAuth tokens of an account that signs in with [`AuthMethod::OAuth2`]
/// rather than a password.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OAuthIssuer {
    /// The Microsoft identity platform: Outlook.com, Hotmail, Live and MSN addresses, and
    /// Microsoft 365 (Exchange Online) work or school accounts.
    Microsoft,
}

impl OAuthIssuer {
    /// The name people know the sign-in by ("Sign in with Microsoft").
    pub fn label(self) -> &'static str {
        match self {
            OAuthIssuer::Microsoft => "Microsoft",
        }
    }
}

/// The protocol an endpoint speaks. Determines the default port for a security mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ServerProtocol {
    Imap,
    Pop3,
    Smtp,
}

impl ServerProtocol {
    /// The conventional port for this protocol and security mode.
    pub fn default_port(self, security: Security) -> u16 {
        match (self, security) {
            (ServerProtocol::Imap, Security::Tls) => 993,
            (ServerProtocol::Imap, _) => 143,
            (ServerProtocol::Pop3, Security::Tls) => 995,
            (ServerProtocol::Pop3, _) => 110,
            (ServerProtocol::Smtp, Security::Tls) => 465,
            (ServerProtocol::Smtp, Security::StartTls) => 587,
            (ServerProtocol::Smtp, Security::None) => 25,
        }
    }
}

/// One server to connect to. The password is not part of this type on purpose.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerEndpoint {
    pub host: String,
    pub port: u16,
    #[serde(default)]
    pub security: Security,
    /// The login name; usually the email address.
    pub username: String,
    #[serde(default)]
    pub auth: AuthMethod,
    /// SHA-256 fingerprint (`AB:CD:…`) of the certificate a server on this computer uses,
    /// which the person chose to trust: Proton Mail Bridge encrypts its local connections
    /// with a certificate it made itself, which no public authority vouches for. Honoured
    /// only for [`Self::is_local`] hosts; elsewhere certificates are verified as always.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned_certificate: Option<String>,
}

impl ServerEndpoint {
    pub fn new(host: impl Into<String>, port: u16, security: Security, username: impl Into<String>) -> Self {
        Self { host: host.into(), port, security, username: username.into(), auth: AuthMethod::Password, pinned_certificate: None }
    }

    /// `host:port` for connecting and for messages.
    pub fn address(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    /// Whether unencrypted connections are being asked for somewhere other than this machine.
    pub fn is_insecure_remote(&self) -> bool {
        self.security == Security::None && !is_local_host(&self.host)
    }

    /// Whether the server is on this computer (a loopback name or address).
    pub fn is_local(&self) -> bool {
        is_local_host(&self.host)
    }
}

fn is_local_host(host: &str) -> bool {
    let h = host.trim().trim_start_matches('[').trim_end_matches(']');
    h.eq_ignore_ascii_case("localhost") || h == "127.0.0.1" || h == "::1" || h.ends_with(".localhost")
}

/// What happens to mail on a POP3 server after it has been downloaded.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Pop3Policy {
    /// Keep messages on the server after downloading them.
    pub leave_on_server: bool,
    /// With `leave_on_server`, delete server copies this many days after they were first seen.
    pub delete_after_days: Option<u32>,
    /// Delete the server copy when the local message is moved to Trash.
    pub delete_on_server_when_trashed: bool,
}

impl Default for Pop3Policy {
    fn default() -> Self {
        Self { leave_on_server: true, delete_after_days: None, delete_on_server_when_trashed: false }
    }
}

/// Per-account IMAP folder choices that override SPECIAL-USE and name heuristics.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ImapPolicy {
    /// Folder path that the Archive operation moves mail to.
    pub archive_folder: Option<String>,
    /// Folder path that holds sent mail, when the server does not advertise one.
    pub sent_folder: Option<String>,
    /// Folder path that holds trashed mail, when the server does not advertise one.
    pub trash_folder: Option<String>,
    /// Without a Trash folder, flag `\Deleted` and expunge instead of moving.
    pub expunge_on_trash: bool,
}

/// Sync state of one IMAP folder, as mirrored into the store's `imap_folders` table from
/// the provider's checkpoint after each cycle. The checkpoint stays authoritative; this row
/// exists so the settings dialog and diagnostics can see folders without parsing it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ImapFolder {
    /// The raw folder path, as the server names it (also the label's `provider_id`).
    pub path: String,
    pub uidvalidity: Option<u32>,
    pub highest_uid: u32,
    pub highest_modseq: Option<u64>,
    pub delimiter: Option<String>,
    /// RFC 6154 attribute such as `\Sent`, when the server advertised one.
    pub special_use: Option<String>,
    pub subscribed: bool,
}

/// One row of the store's `pop3_seen` ledger, mirrored from the POP3 provider's checkpoint
/// after each cycle: every UIDL ever listed for the account, so a message that reappears on
/// the server or was deleted there is never downloaded twice or lost locally. The
/// checkpoint stays authoritative.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Pop3Seen {
    pub uidl: String,
    /// When the UIDL was first listed (ms since the epoch); retention counts from here.
    pub seen_at: i64,
    /// The server no longer lists it (deleted by Overlook's retention policy or elsewhere).
    pub deleted_on_server: bool,
}

/// Everything needed to reach an IMAP or POP3 account and its SMTP server, except passwords
/// and tokens.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MailServers {
    /// IMAP or POP3, depending on the account's provider kind.
    pub incoming: ServerEndpoint,
    /// SMTP.
    pub outgoing: ServerEndpoint,
    #[serde(default)]
    pub pop3: Option<Pop3Policy>,
    #[serde(default)]
    pub imap: Option<ImapPolicy>,
    /// Set when the account signs in through this OAuth issuer instead of with a password.
    /// This decides which credential the keyring holds for the account; password accounts
    /// leave it out, so their stored rows are unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth: Option<OAuthIssuer>,
}

impl MailServers {
    /// These servers, signing in through `issuer`: both endpoints use [`AuthMethod::OAuth2`].
    pub fn with_oauth(mut self, issuer: OAuthIssuer) -> Self {
        self.oauth = Some(issuer);
        self.incoming.auth = AuthMethod::OAuth2;
        self.outgoing.auth = AuthMethod::OAuth2;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_json_with_defaults() {
        let servers = MailServers {
            incoming: ServerEndpoint::new("imap.example.com", 993, Security::Tls, "jason@example.com"),
            outgoing: ServerEndpoint::new("smtp.example.com", 587, Security::StartTls, "jason@example.com"),
            pop3: None,
            imap: Some(ImapPolicy { archive_folder: Some("Archive".into()), ..Default::default() }),
            oauth: None,
        };
        let json = serde_json::to_string(&servers).unwrap();
        assert!(json.contains("\"security\":\"start-tls\""), "{json}");
        assert!(!json.contains("oauth"), "password accounts store the same JSON as before: {json}");
        assert_eq!(serde_json::from_str::<MailServers>(&json).unwrap(), servers);

        // Older rows without the optional fields still load.
        let minimal = r#"{"incoming":{"host":"pop.example.com","port":995,"username":"j"},"outgoing":{"host":"smtp.example.com","port":465,"username":"j"}}"#;
        let loaded: MailServers = serde_json::from_str(minimal).unwrap();
        assert_eq!(loaded.incoming.security, Security::Tls);
        assert_eq!(loaded.incoming.auth, AuthMethod::Password);
        assert!(loaded.pop3.is_none() && loaded.imap.is_none() && loaded.oauth.is_none());
        assert_eq!(Pop3Policy::default().leave_on_server, true);
    }

    #[test]
    fn oauth_accounts_round_trip() {
        let servers = MailServers {
            incoming: ServerEndpoint::new("outlook.office365.com", 993, Security::Tls, "jason@outlook.com"),
            outgoing: ServerEndpoint::new("smtp-mail.outlook.com", 587, Security::StartTls, "jason@outlook.com"),
            pop3: None,
            imap: None,
            oauth: None,
        }
        .with_oauth(OAuthIssuer::Microsoft);
        assert_eq!((servers.incoming.auth, servers.outgoing.auth), (AuthMethod::OAuth2, AuthMethod::OAuth2));
        let json = serde_json::to_string(&servers).unwrap();
        assert!(json.contains("\"oauth\":\"microsoft\"") && json.contains("\"auth\":\"o-auth2\""), "{json}");
        assert_eq!(serde_json::from_str::<MailServers>(&json).unwrap(), servers);
        assert_eq!(OAuthIssuer::Microsoft.label(), "Microsoft");
    }

    #[test]
    fn default_ports_and_local_hosts() {
        assert_eq!(ServerProtocol::Imap.default_port(Security::Tls), 993);
        assert_eq!(ServerProtocol::Smtp.default_port(Security::StartTls), 587);
        assert_eq!(ServerProtocol::Pop3.default_port(Security::None), 110);
        assert!(!ServerEndpoint::new("localhost", 1025, Security::None, "x").is_insecure_remote());
        assert!(ServerEndpoint::new("mail.example.com", 25, Security::None, "x").is_insecure_remote());
        assert!(ServerEndpoint::new("127.0.0.1", 1143, Security::StartTls, "x").is_local() && !ServerEndpoint::new("imap.example.com", 993, Security::Tls, "x").is_local());
        let pinned = ServerEndpoint { pinned_certificate: Some("AB:CD".into()), ..ServerEndpoint::new("127.0.0.1", 1143, Security::StartTls, "x") };
        let json = serde_json::to_string(&pinned).unwrap();
        assert!(json.contains("\"pinned_certificate\":\"AB:CD\""), "{json}");
        assert!(!serde_json::to_string(&ServerEndpoint::new("h", 1, Security::Tls, "x")).unwrap().contains("pinned"), "unpinned endpoints store what they always did");
        assert_eq!(Security::from_key("ssl"), Some(Security::Tls));
    }
}
