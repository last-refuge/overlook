//! SMTP submission for IMAP and POP3 accounts.
//!
//! The transaction is driven command by command over lettre's blocking client so that
//! failures map truthfully onto [`ProviderError`]:
//!
//! - cannot connect, TLS fails, or the connection drops before `DATA` is accepted →
//!   [`ProviderError::Offline`]: nothing left this machine, the draft stays queued;
//! - a `4xx` reply → [`ProviderError::Temporary`]; a `5xx` reply → [`ProviderError::Rejected`]
//!   with the server's own words; a failed `AUTH` → [`ProviderError::SignInRequired`];
//! - the connection drops after the message body was written → [`ProviderError::Uncertain`]:
//!   the server may have queued it, so the Outbox must reconcile before any retry.
//!
//! Sign-in is `AUTH PLAIN`/`LOGIN` with a password, or `AUTH XOAUTH2` with an access token
//! for OAuth accounts. Credentials are passed in at construction and never logged.

use crate::credential::{Credential, Secret};
use crate::ProviderError;
use lettre::transport::smtp::authentication::{Credentials, Mechanism};
use lettre::transport::smtp::client::{SmtpConnection, TlsParameters};
use lettre::transport::smtp::commands::{Data, Mail, Rcpt};
use lettre::transport::smtp::extension::{ClientId, Extension, MailBodyParameter, MailParameter};
use lettre::transport::smtp::response::Severity;
use lettre::transport::smtp::Error as SmtpError;
use lettre::Address as SmtpAddress;
use overlook_core::{Security, ServerEndpoint};
use std::time::Duration;

/// Socket connect/read/write timeout. A hung server must not stall the sync thread.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// What Microsoft 365 means by `535 5.7.139 … SmtpClientAuthentication is disabled`: signing
/// in again cannot help.
pub const SMTP_AUTH_DISABLED: &str = "the organization has turned off sending from mail apps (SMTP AUTH) for this mailbox. An administrator can turn it on for the mailbox in Microsoft 365";

/// One SMTP server with the credentials to use it.
pub struct Transport {
    endpoint: ServerEndpoint,
    credential: Credential,
    timeout: Duration,
}

impl std::fmt::Debug for Transport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The credential is deliberately absent.
        f.debug_struct("Transport").field("endpoint", &self.endpoint).field("timeout", &self.timeout).finish()
    }
}

impl Transport {
    /// `credential` is a password, or an OAuth account's token source.
    pub fn new(endpoint: ServerEndpoint, credential: impl Into<Credential>) -> Self {
        Self { endpoint, credential: credential.into(), timeout: DEFAULT_TIMEOUT }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn endpoint(&self) -> &ServerEndpoint {
        &self.endpoint
    }

    /// Connects, authenticates and disconnects. Proves the settings and password work.
    pub fn probe(&self) -> Result<(), ProviderError> {
        let mut conn = self.connect()?;
        let _ = conn.quit();
        Ok(())
    }

    /// Submits `message` (complete RFC 822 bytes) from `from` to `recipients` (bare
    /// addresses: To, Cc and Bcc alike). Returns the server's acceptance line.
    pub fn send(&self, from: &str, recipients: &[String], message: &[u8]) -> Result<String, ProviderError> {
        let sender: SmtpAddress = from.trim().parse().map_err(|e| ProviderError::Rejected(format!("invalid sender address “{from}”: {e}")))?;
        let mut rcpts = Vec::with_capacity(recipients.len());
        for r in recipients {
            rcpts.push(r.trim().parse::<SmtpAddress>().map_err(|e| ProviderError::Rejected(format!("invalid recipient address “{r}”: {e}")))?);
        }
        if rcpts.is_empty() {
            return Err(ProviderError::Rejected("the message has no recipients".into()));
        }

        let mut conn = self.connect()?;
        let mut mail_parameters = Vec::new();
        if !message.is_ascii() && conn.server_info().supports_feature(Extension::EightBitMime) {
            mail_parameters.push(MailParameter::Body(MailBodyParameter::EightBitMime));
        }
        if (!from.is_ascii() || recipients.iter().any(|r| !r.is_ascii())) && conn.server_info().supports_feature(Extension::SmtpUtfEight) {
            mail_parameters.push(MailParameter::SmtpUtfEight);
        }
        conn.command(Mail::new(Some(sender), mail_parameters)).map_err(before_data)?;
        for rcpt in rcpts {
            conn.command(Rcpt::new(rcpt, Vec::new())).map_err(before_data)?;
        }
        conn.command(Data).map_err(before_data)?;
        // From here on the bytes are leaving this machine: a dropped connection no longer
        // proves the server did not queue the message.
        let accepted = conn.message(message).map_err(after_data)?;
        let _ = conn.quit();
        Ok(accepted.message().collect::<Vec<_>>().join(" "))
    }

    /// Connects and signs in. A refused access token is retried once on a new connection
    /// with a refreshed token.
    fn connect(&self) -> Result<SmtpConnection, ProviderError> {
        self.credential.sign_in(|secret| self.open(secret))
    }

    fn open(&self, secret: Secret<'_>) -> Result<SmtpConnection, ProviderError> {
        let endpoint = &self.endpoint;
        if endpoint.is_insecure_remote() {
            return Err(ProviderError::Rejected(format!("refusing to send to {} without encryption", endpoint.host)));
        }
        let client_id = ClientId::default();
        // A server on this computer whose certificate the person trusted is held to exactly
        // that certificate (`crate::pinning`); every other server is verified as always.
        let pin = crate::pinning::pin_for(endpoint);
        let tls = match (endpoint.security, pin) {
            (Security::None, _) => None,
            (_, Some(_)) => Some(local_tls(&endpoint.host)?),
            (_, None) => Some(TlsParameters::new(endpoint.host.clone()).map_err(connect_error)?),
        };
        let wrapper = match endpoint.security {
            Security::Tls => tls.as_ref(),
            _ => None,
        };
        // A pinned or unencrypted connection goes only to this computer's loopback addresses.
        let local = match (pin, endpoint.security) {
            (Some(_), _) => Some(crate::pinning::loopback_addresses(endpoint)?),
            (None, Security::None) => Some(crate::pinning::loopback_addresses(endpoint).map_err(|e| crate::imap::connection::unencrypted_refusal(endpoint, e))?),
            (None, _) => None,
        };
        let mut conn = match &local {
            Some(addrs) => SmtpConnection::connect(&addrs[..], Some(self.timeout), &client_id, wrapper, None),
            None => SmtpConnection::connect((endpoint.host.as_str(), endpoint.port), Some(self.timeout), &client_id, wrapper, None),
        }
        .map_err(connect_error)?;
        if endpoint.security == Security::StartTls {
            if let Some(params) = tls.as_ref() {
                conn.starttls(params, &client_id).map_err(connect_error)?;
            }
        }
        if let Some(pin) = pin {
            // Before any credentials: the certificate must be the one the person trusted.
            if !conn.peer_certificate().is_ok_and(|der| crate::pinning::matches(&der, pin)) {
                conn.abort();
                return Err(ProviderError::Rejected(crate::secure_error::CERTIFICATE_CHANGED.into()));
            }
        }
        if !endpoint.username.is_empty() {
            let (mechanisms, credentials): (&[Mechanism], Credentials) = match secret {
                Secret::Password(password) => {
                    tracing::info!(target: "overlook::signin", host = %endpoint.host, port = endpoint.port, mechanisms = "PLAIN LOGIN", username = "[account]", password = "[REDACTED]", "SMTP sign-in");
                    (&[Mechanism::Plain, Mechanism::Login], Credentials::new(endpoint.username.clone(), password.to_owned()))
                }
                Secret::AccessToken(token) => {
                    if !conn.server_info().supports_auth_mechanism(Mechanism::Xoauth2) {
                        return Err(ProviderError::Rejected(NO_SHARED_MECHANISM.into()));
                    }
                    tracing::info!(target: "overlook::signin", host = %endpoint.host, port = endpoint.port, mechanisms = "XOAUTH2", username = "[account]", access_token = "[REDACTED]", "SMTP sign-in");
                    (&[Mechanism::Xoauth2], Credentials::new(endpoint.username.clone(), token.to_owned()))
                }
            };
            let oauth = matches!(secret, Secret::AccessToken(_));
            conn.auth(mechanisms, &credentials).map_err(|e| {
                tracing::info!(target: "overlook::signin", host = %endpoint.host, outcome = "rejected or failed", "SMTP sign-in result");
                // A `334` error challenge to XOAUTH2 (RFC 7628 §3.2.2, as Gmail sends) is a
                // refused token; lettre cannot answer it and reports a client error.
                if oauth && e.status().is_none() && e.is_client() {
                    crate::secure_error::log_detail("SMTP XOAUTH2", &e);
                    return ProviderError::SignInRequired;
                }
                auth_error(e)
            })?;
            tracing::info!(target: "overlook::signin", host = %endpoint.host, outcome = "accepted", "SMTP sign-in result");
        }
        Ok(conn)
    }
}

/// TLS for a server on this computer whose certificate is compared with a pin after the
/// handshake, or read so the person can decide whether to trust it. lettre takes no custom
/// verifier, so it is told not to check the chain or the name; it still verifies the
/// handshake signature with the certificate's key. Never used for any other server.
fn local_tls(host: &str) -> Result<TlsParameters, ProviderError> {
    TlsParameters::builder(host.trim().to_owned()).dangerous_accept_invalid_certs(true).dangerous_accept_invalid_hostnames(true).build_rustls().map_err(connect_error)
}

/// Reads the certificate an SMTP server on this computer presents and returns its
/// fingerprint, without signing in. Callers check that the endpoint is local.
pub(crate) fn fetch_certificate(endpoint: &ServerEndpoint, timeout: Duration) -> Result<String, ProviderError> {
    let client_id = ClientId::default();
    let tls = local_tls(&endpoint.host)?;
    let wrapper = (endpoint.security == Security::Tls).then_some(&tls);
    let addrs = crate::pinning::loopback_addresses(endpoint)?;
    let mut conn = SmtpConnection::connect(&addrs[..], Some(timeout), &client_id, wrapper, None).map_err(connect_error)?;
    if endpoint.security == Security::StartTls {
        conn.starttls(&tls, &client_id).map_err(connect_error)?;
    }
    let der = conn.peer_certificate().map_err(|e| {
        crate::secure_error::log_detail("SMTP certificate", &e);
        ProviderError::Rejected(crate::secure_error::SECURE_CONNECTION_FAILED.into())
    });
    let _ = conn.quit();
    Ok(crate::pinning::fingerprint(&der?))
}

/// The server's words from a reply error, without lettre's "permanent error (550): " prefix.
fn reply_text(e: &SmtpError) -> String {
    let s = e.to_string();
    match s.split_once("): ") {
        Some((head, tail)) if head.contains("error (") => tail.trim().to_owned(),
        _ => s,
    }
}

fn by_status(e: &SmtpError) -> Option<ProviderError> {
    let code = e.status()?;
    Some(match code.severity {
        Severity::PermanentNegativeCompletion => ProviderError::Rejected(reply_text(e)),
        _ => ProviderError::Temporary(reply_text(e)),
    })
}

fn connect_error(e: SmtpError) -> ProviderError {
    if let Some(mapped) = by_status(&e) {
        return mapped;
    }
    crate::secure_error::log_detail("SMTP connection", &e);
    // A certificate or handshake failure will not succeed on retry; report it in fixed words.
    // With implicit TLS, lettre reports a failed handshake as a network error wrapping rustls's.
    if e.is_tls() {
        return ProviderError::Rejected(tls_message(&e).into());
    }
    if let Some(message) = rustls_cause(&e) {
        return ProviderError::Rejected(message.into());
    }
    // Network, timeouts, or a connection closed mid-reply (lettre reports the EOF as an
    // incomplete response): the server was never properly reached.
    if e.is_client() {
        ProviderError::Temporary(crate::secure_error::UNREADABLE_RESPONSE.into())
    } else {
        ProviderError::Offline
    }
}

/// The fixed message for a rustls failure anywhere in the error's causes, directly or inside
/// an I/O error.
fn rustls_cause(e: &SmtpError) -> Option<&'static str> {
    let mut source = std::error::Error::source(e);
    while let Some(inner) = source {
        let tls = inner.downcast_ref::<rustls::Error>().or_else(|| inner.downcast_ref::<std::io::Error>().and_then(|io| io.get_ref()).and_then(|i| i.downcast_ref::<rustls::Error>()));
        if let Some(tls) = tls {
            return Some(crate::secure_error::rustls_message(tls));
        }
        source = inner.source();
    }
    None
}

/// The fixed message for a lettre TLS error, from the rustls error underneath when present.
fn tls_message(e: &SmtpError) -> &'static str {
    let mut source = std::error::Error::source(e);
    while let Some(inner) = source {
        if let Some(tls) = inner.downcast_ref::<rustls::Error>() {
            return crate::secure_error::rustls_message(tls);
        }
        if let Some(io) = inner.downcast_ref::<std::io::Error>() {
            return crate::secure_error::io_message(io);
        }
        source = inner.source();
    }
    crate::secure_error::SECURE_CONNECTION_FAILED
}

/// The outgoing server and Overlook have no sign-in method in common.
const NO_SHARED_MECHANISM: &str = "the outgoing server offers no sign-in method Overlook supports";

fn auth_error(e: SmtpError) -> ProviderError {
    match e.status() {
        // Microsoft 365 refuses every sign-in this way while the organization has SMTP AUTH
        // turned off; asking for a new sign-in would only repeat the refusal. Its other
        // `5.7.139` replies (basic authentication disabled, security defaults) are ordinary
        // refusals that a new sign-in can fix.
        Some(code) if code.severity == Severity::PermanentNegativeCompletion && is_smtp_auth_disabled(&reply_text(&e)) => ProviderError::Rejected(SMTP_AUTH_DISABLED.into()),
        Some(code) if code.severity == Severity::PermanentNegativeCompletion => ProviderError::SignInRequired,
        Some(_) => ProviderError::Temporary(reply_text(&e)),
        // No shared mechanism is a settings problem, not a network one.
        None if e.is_client() => {
            crate::secure_error::log_detail("SMTP authentication", &e);
            ProviderError::Rejected(NO_SHARED_MECHANISM.into())
        }
        None => connect_error(e),
    }
}

/// Microsoft's wording for SMTP AUTH turned off for the organization or the mailbox.
fn is_smtp_auth_disabled(reply: &str) -> bool {
    reply.contains("5.7.139") && reply.to_ascii_lowercase().contains("smtpclientauthentication is disabled")
}

/// Before `DATA` is accepted nothing has left this machine, so a dropped connection is
/// plain Offline.
fn before_data(e: SmtpError) -> ProviderError {
    connect_error(e)
}

fn after_data(e: SmtpError) -> ProviderError {
    by_status(&e).unwrap_or(ProviderError::Uncertain)
}

/// Scripted SMTP servers for tests: one-shot (`serve`) and persistent (`serve_many`).
/// Plaintext, `AUTH PLAIN`, with faults armed through [`fake::Behaviour`].
#[cfg(any(test, feature = "fake-servers"))]
pub mod fake {
    use base64::Engine;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
    use std::sync::{mpsc, Arc, Condvar, Mutex};
    use std::time::Duration;

    /// How the scripted server misbehaves, if at all.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Behaviour {
        Normal,
        RejectAuth,
        /// Refuses every sign-in as Microsoft 365 does with SMTP AUTH turned off (`5.7.139`).
        AuthDisabled,
        /// Refuses password sign-ins as Microsoft does for basic authentication (`5.7.139`).
        BasicAuthDisabled,
        /// Answers `AUTH XOAUTH2` with Gmail's JSON error challenge (`334`) and then `535`.
        XoauthChallenge,
        TemporaryMail,
        RejectRecipient,
        DropAfterData,
        DropDuringGreeting,
    }

    /// What the server saw.
    #[derive(Default, Debug, Clone)]
    pub struct Seen {
        /// User name and password, or user name and access token for `XOAUTH2`.
        pub auth: Option<(String, String)>,
        /// `PLAIN` or `XOAUTH2`.
        pub mechanism: Option<String>,
        pub mail_from: Option<String>,
        pub rcpt: Vec<String>,
        pub data: Option<Vec<u8>>,
    }

    /// Serves exactly one connection and hands back what it saw.
    pub fn serve(behaviour: Behaviour) -> (SocketAddr, mpsc::Receiver<Seen>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let seen = script(stream, behaviour);
            let _ = tx.send(seen);
        });
        (addr, rx)
    }

    /// A server that keeps accepting connections. `behaviour` is read at the start of each
    /// connection, so a test can arm a fault for the next send; every finished connection
    /// is appended to `seen`.
    pub struct FakeSmtp {
        pub addr: SocketAddr,
        pub behaviour: Arc<Mutex<Behaviour>>,
        pub seen: Arc<Mutex<Vec<Seen>>>,
        completed: Arc<Condvar>,
    }

    impl FakeSmtp {
        /// The client can receive the QUIT reply before the worker publishes its
        /// transcript. Wait for that separate completion instead of racing it.
        pub fn wait_for_connections(&self, count: usize) -> Vec<Seen> {
            let seen = self.seen.lock().unwrap();
            let (seen, _) = self.completed.wait_timeout_while(
                seen, Duration::from_secs(10), |seen| seen.len() < count,
            ).unwrap();
            assert!(seen.len() >= count, "SMTP fixture timed out: expected {count} completed connections, got {}", seen.len());
            seen.clone()
        }

        pub fn set(&self, behaviour: Behaviour) {
            *self.behaviour.lock().unwrap() = behaviour;
        }

        pub fn messages(&self) -> Vec<Vec<u8>> {
            self.seen.lock().unwrap().iter().filter_map(|s| s.data.clone()).collect()
        }
    }

    pub fn serve_many(behaviour: Behaviour) -> FakeSmtp {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let behaviour = Arc::new(Mutex::new(behaviour));
        let seen: Arc<Mutex<Vec<Seen>>> = Arc::new(Mutex::new(Vec::new()));
        let completed = Arc::new(Condvar::new());
        let (b, s) = (behaviour.clone(), seen.clone());
        let ready = completed.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { break };
                let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                let behaviour = *b.lock().unwrap();
                let s = s.clone();
                let ready = ready.clone();
                std::thread::spawn(move || {
                    let result = script(stream, behaviour);
                    s.lock().unwrap().push(result);
                    ready.notify_all();
                });
            }
        });
        FakeSmtp { addr, behaviour, seen, completed }
    }

    fn script(mut stream: TcpStream, behaviour: Behaviour) -> Seen {
        let mut seen = Seen::default();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut say = |s: &str| {
            let _ = stream.write_all(format!("{s}\r\n").as_bytes());
            let _ = stream.flush();
        };
        if behaviour == Behaviour::DropDuringGreeting {
            let _ = stream.shutdown(Shutdown::Both);
            return seen;
        }
        say("220 fake.example ESMTP ready");
        let mut line = String::new();
        loop {
            line.clear();
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                break;
            }
            let cmd = line.trim_end();
            let upper = cmd.to_ascii_uppercase();
            if upper.starts_with("EHLO") {
                say("250-fake.example");
                say("250-AUTH PLAIN LOGIN XOAUTH2");
                say("250 8BITMIME");
            } else if upper.starts_with("AUTH PLAIN") || upper.starts_with("AUTH XOAUTH2") {
                let b64 = cmd.split_whitespace().nth(2).unwrap_or("");
                let raw = base64::engine::general_purpose::STANDARD.decode(b64).unwrap_or_default();
                if upper.starts_with("AUTH XOAUTH2") {
                    // `user=…^Aauth=Bearer …^A^A`
                    let text = String::from_utf8_lossy(&raw).into_owned();
                    let mut fields = text.split('\x01');
                    let user = fields.next().and_then(|f| f.strip_prefix("user=")).unwrap_or_default().to_owned();
                    let token = fields.next().and_then(|f| f.strip_prefix("auth=Bearer ")).unwrap_or_default().to_owned();
                    seen.auth = Some((user, token));
                    seen.mechanism = Some("XOAUTH2".into());
                } else {
                    let mut parts = raw.split(|b| *b == 0).skip(1).map(|p| String::from_utf8_lossy(p).into_owned());
                    seen.auth = Some((parts.next().unwrap_or_default(), parts.next().unwrap_or_default()));
                    seen.mechanism = Some("PLAIN".into());
                }
                match behaviour {
                    Behaviour::RejectAuth => say("535 5.7.8 Authentication credentials invalid"),
                    Behaviour::AuthDisabled => say("535 5.7.139 Authentication unsuccessful, SmtpClientAuthentication is disabled for the Tenant. Visit https://aka.ms/smtp_auth_disabled for more information."),
                    Behaviour::BasicAuthDisabled => say("535 5.7.139 Authentication unsuccessful, basic authentication is disabled. [SN4PR0601CA0015.namprd06.prod.outlook.com]"),
                    Behaviour::XoauthChallenge if upper.starts_with("AUTH XOAUTH2") => {
                        say("334 eyJzdGF0dXMiOiI0MDAiLCJzY2hlbWVzIjoiQmVhcmVyIiwic2NvcGUiOiJodHRwczovL21haWwuZ29vZ2xlLmNvbS8ifQ==");
                        line.clear();
                        if reader.read_line(&mut line).unwrap_or(0) == 0 {
                            break;
                        }
                        say("535 5.7.8 Username and Password not accepted");
                    }
                    _ => say("235 2.7.0 Authentication successful"),
                }
            } else if upper.starts_with("MAIL FROM") {
                seen.mail_from = Some(cmd.to_owned());
                if behaviour == Behaviour::TemporaryMail {
                    say("451 4.3.0 Mail system busy, try again later");
                } else {
                    say("250 2.1.0 Ok");
                }
            } else if upper.starts_with("RCPT TO") {
                seen.rcpt.push(cmd.to_owned());
                if behaviour == Behaviour::RejectRecipient {
                    say("550 5.1.1 mailbox unavailable: no such user here");
                } else {
                    say("250 2.1.5 Ok");
                }
            } else if upper == "DATA" {
                say("354 End data with <CR><LF>.<CR><LF>");
                let mut body = Vec::new();
                let mut buf = [0u8; 1024];
                loop {
                    let n = match reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => n,
                    };
                    body.extend_from_slice(&buf[..n]);
                    if body.ends_with(b"\r\n.\r\n") {
                        break;
                    }
                }
                seen.data = Some(body);
                if behaviour == Behaviour::DropAfterData {
                    let _ = stream.shutdown(Shutdown::Both);
                    break;
                }
                say("250 2.0.0 Ok: queued as 4F1D2");
            } else if upper == "QUIT" {
                say("221 2.0.0 Bye");
                break;
            } else {
                say("500 5.5.1 Command unrecognized");
            }
        }
        seen
    }

}

#[cfg(test)]
mod tests {
    use super::fake::*;
    use super::*;
    use std::net::{SocketAddr, TcpListener};

    fn transport(addr: SocketAddr) -> Transport {
        Transport::new(ServerEndpoint::new("127.0.0.1", addr.port(), Security::None, "jason@example.com"), "hunter2").with_timeout(Duration::from_secs(5))
    }

    /// An OAuth token source that hands out one fixed token.
    struct FixedToken(&'static str);
    impl crate::AccessTokenSource for FixedToken {
        fn access_token(&self, _rejected: Option<&str>) -> Result<String, ProviderError> {
            Ok(self.0.to_owned())
        }
    }

    #[test]
    fn oauth_accounts_send_with_xoauth2() {
        let (addr, rx) = serve(Behaviour::Normal);
        let oauth = Transport::new(ServerEndpoint::new("127.0.0.1", addr.port(), Security::None, "jason@outlook.com"), crate::Credential::OAuth(std::sync::Arc::new(FixedToken("synthetic-token")))).with_timeout(Duration::from_secs(5));
        oauth.send("jason@outlook.com", &["maya@northstar.example".into()], MESSAGE).unwrap();
        let seen = rx.recv().unwrap();
        assert_eq!(seen.mechanism.as_deref(), Some("XOAUTH2"));
        assert_eq!(seen.auth, Some(("jason@outlook.com".to_owned(), "synthetic-token".to_owned())));
        assert!(!format!("{oauth:?}").contains("synthetic-token"));
    }

    #[test]
    fn smtp_auth_turned_off_by_the_organization_is_not_a_password_problem() {
        let server = serve_many(Behaviour::AuthDisabled);
        let oauth = Transport::new(ServerEndpoint::new("127.0.0.1", server.addr.port(), Security::None, "jason@contoso.com"), crate::Credential::OAuth(std::sync::Arc::new(FixedToken("synthetic-token")))).with_timeout(Duration::from_secs(5));
        assert_eq!(oauth.probe(), Err(ProviderError::Rejected(SMTP_AUTH_DISABLED.into())));
        assert_eq!(server.wait_for_connections(1).len(), 1, "no second sign-in with a refreshed token");
        // Microsoft's other 5.7.139 replies stay sign-in problems: basic authentication being
        // off is what sends an Outlook.com password account to Microsoft sign-in.
        server.set(Behaviour::BasicAuthDisabled);
        assert_eq!(transport(server.addr).probe(), Err(ProviderError::SignInRequired));
    }

    #[test]
    fn an_xoauth2_error_challenge_is_a_refused_token() {
        let server = serve_many(Behaviour::XoauthChallenge);
        let oauth = Transport::new(ServerEndpoint::new("127.0.0.1", server.addr.port(), Security::None, "jason@example.com"), crate::Credential::OAuth(std::sync::Arc::new(FixedToken("synthetic-token")))).with_timeout(Duration::from_secs(5));
        assert_eq!(oauth.probe(), Err(ProviderError::SignInRequired));
        assert_eq!(server.wait_for_connections(2).len(), 2, "refused once, retried once with a refreshed token");
    }

    const MESSAGE: &[u8] = b"From: Jason <jason@example.com>\r\nTo: maya@northstar.example\r\nSubject: Agenda\r\nMessage-ID: <d1@overlook.lastrefuge.ai>\r\n\r\nA few things to cover.\r\n.leading dot line\r\n";

    #[test]
    fn sends_with_auth_plain_and_dot_stuffing() {
        let (addr, rx) = serve(Behaviour::Normal);
        let reply = transport(addr).send("jason@example.com", &["maya@northstar.example".into(), "alex@studio.example".into()], MESSAGE).unwrap();
        assert!(reply.contains("queued as 4F1D2"), "{reply}");
        let seen = rx.recv().unwrap();
        assert_eq!(seen.auth, Some(("jason@example.com".to_owned(), "hunter2".to_owned())));
        assert_eq!(seen.mail_from.as_deref(), Some("MAIL FROM:<jason@example.com>"));
        assert_eq!(seen.rcpt, vec!["RCPT TO:<maya@northstar.example>", "RCPT TO:<alex@studio.example>"]);
        let data = String::from_utf8(seen.data.unwrap()).unwrap();
        assert!(data.contains("Message-ID: <d1@overlook.lastrefuge.ai>"), "{data}");
        assert!(data.contains("\r\n..leading dot line\r\n"), "lines starting with a dot are stuffed: {data}");
        assert!(data.ends_with("\r\n.\r\n"));
    }

    #[test]
    fn probe_connects_and_authenticates() {
        let (addr, rx) = serve(Behaviour::Normal);
        transport(addr).probe().unwrap();
        assert!(rx.recv().unwrap().auth.is_some());
        let (addr, _rx) = serve(Behaviour::RejectAuth);
        assert_eq!(transport(addr).probe(), Err(ProviderError::SignInRequired));
    }

    #[test]
    fn wrong_password_is_sign_in_required() {
        let (addr, _rx) = serve(Behaviour::RejectAuth);
        assert_eq!(transport(addr).send("jason@example.com", &["maya@northstar.example".into()], MESSAGE), Err(ProviderError::SignInRequired));
    }

    #[test]
    fn rejected_recipient_carries_the_servers_words() {
        let (addr, rx) = serve(Behaviour::RejectRecipient);
        let err = transport(addr).send("jason@example.com", &["nobody@northstar.example".into()], MESSAGE).unwrap_err();
        match err {
            ProviderError::Rejected(text) => assert!(text.contains("mailbox unavailable: no such user here"), "{text}"),
            other => panic!("expected Rejected, got {other:?}"),
        }
        assert!(rx.recv().unwrap().data.is_none(), "nothing was sent");
    }

    #[test]
    fn transient_reply_is_temporary() {
        let (addr, _rx) = serve(Behaviour::TemporaryMail);
        match transport(addr).send("jason@example.com", &["maya@northstar.example".into()], MESSAGE) {
            Err(ProviderError::Temporary(text)) => assert!(text.contains("try again later"), "{text}"),
            other => panic!("expected Temporary, got {other:?}"),
        }
    }

    #[test]
    fn dropped_connection_after_data_is_uncertain() {
        let (addr, rx) = serve(Behaviour::DropAfterData);
        assert_eq!(transport(addr).send("jason@example.com", &["maya@northstar.example".into()], MESSAGE), Err(ProviderError::Uncertain));
        assert!(rx.recv().unwrap().data.is_some(), "the server had the whole message when it dropped");
    }

    #[test]
    fn unreachable_server_is_offline() {
        // A port that was just released: connection refused.
        let addr = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
        assert_eq!(transport(addr).send("jason@example.com", &["maya@northstar.example".into()], MESSAGE), Err(ProviderError::Offline));
        let (addr, _rx) = serve(Behaviour::DropDuringGreeting);
        assert_eq!(transport(addr).probe(), Err(ProviderError::Offline));
    }

    #[test]
    fn failed_implicit_tls_handshake_is_a_secure_connection_failure_not_offline() {
        // A server that answers the TLS ClientHello with plain text. lettre reports the rustls
        // failure as a network error; it must still read as a TLS failure, in fixed words.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                use std::io::{Read, Write};
                let mut hello = [0u8; 512];
                let _ = stream.read(&mut hello);
                let _ = stream.write_all(b"220 smtp.example ESMTP plain text, not TLS\r\n");
            }
        });
        let t = Transport::new(ServerEndpoint::new("127.0.0.1", addr.port(), Security::Tls, "jason@example.com"), "hunter2").with_timeout(Duration::from_secs(5));
        assert_eq!(t.probe(), Err(ProviderError::Rejected(crate::secure_error::SECURE_CONNECTION_FAILED.into())));
    }

    #[test]
    fn refuses_plaintext_to_remote_hosts_and_bad_addresses() {
        let t = Transport::new(ServerEndpoint::new("smtp.example.com", 25, Security::None, "j"), "pw");
        assert!(matches!(t.probe(), Err(ProviderError::Rejected(_))));
        let (addr, _rx) = serve(Behaviour::Normal);
        assert!(matches!(transport(addr).send("not an address", &["maya@northstar.example".into()], MESSAGE), Err(ProviderError::Rejected(_))));
        assert!(matches!(transport(addr).send("jason@example.com", &[], MESSAGE), Err(ProviderError::Rejected(_))));
    }
}

#[cfg(test)]
mod real_server {
    //! Against a real SMTP server, for the interoperability matrix. Run with
    //! `OVERLOOK_SMTP_TEST_HOST=smtp.example.com OVERLOOK_SMTP_TEST_USER=… OVERLOOK_SMTP_TEST_PASSWORD=…
    //! cargo test -p overlook-provider real_smtp -- --ignored --nocapture`; optional
    //! `OVERLOOK_SMTP_TEST_PORT`, `OVERLOOK_SMTP_TEST_SECURITY=tls|starttls` (default
    //! `starttls` on 587), `OVERLOOK_SMTP_TEST_FROM` (default: the user name) and
    //! `OVERLOOK_SMTP_TEST_TO`: without a recipient the test only connects, authenticates
    //! and quits; with one it also sends one short message there.
    use super::*;
    use overlook_core::ServerProtocol;

    #[test]
    #[ignore]
    fn real_smtp_server_round_trip() {
        let var = |n: &str| std::env::var(n).ok().filter(|v| !v.trim().is_empty());
        let (Some(host), Some(user), Some(password)) = (var("OVERLOOK_SMTP_TEST_HOST"), var("OVERLOOK_SMTP_TEST_USER"), var("OVERLOOK_SMTP_TEST_PASSWORD")) else {
            panic!("set OVERLOOK_SMTP_TEST_HOST, OVERLOOK_SMTP_TEST_USER and OVERLOOK_SMTP_TEST_PASSWORD");
        };
        let security = var("OVERLOOK_SMTP_TEST_SECURITY").and_then(|s| Security::from_key(&s)).unwrap_or(Security::StartTls);
        let port = var("OVERLOOK_SMTP_TEST_PORT").and_then(|p| p.parse().ok()).unwrap_or(ServerProtocol::Smtp.default_port(security));
        let from = var("OVERLOOK_SMTP_TEST_FROM").or_else(|| user.contains('@').then(|| user.clone())).expect("set OVERLOOK_SMTP_TEST_FROM when the user name is not an address");
        let transport = Transport::new(ServerEndpoint::new(host.clone(), port, security, user), password);
        transport.probe().unwrap();
        eprintln!("smtp: connected to {host}:{port} ({}) and authenticated", security.as_key());
        let Some(to) = var("OVERLOOK_SMTP_TEST_TO") else {
            eprintln!("smtp: OVERLOOK_SMTP_TEST_TO not set; nothing sent");
            return;
        };
        let id = format!("<interop-{}-{}@overlook.lastrefuge.ai>", std::process::id(), overlook_core::time::now());
        let message = lettre::Message::builder()
            .from(format!("Overlook interop test <{from}>").parse().unwrap())
            .to(to.parse().unwrap())
            .subject("Overlook SMTP interoperability test")
            .message_id(Some(id.clone()))
            .body(format!("Sent by Overlook's ignored real-server test over {} to {host}.\r\n.A line starting with a dot, to check dot-stuffing.\r\n", security.as_key()))
            .unwrap()
            .formatted();
        let reply = transport.send(&from, &[to.clone()], &message).unwrap();
        eprintln!("smtp: {host} accepted {id} for {to}: {reply}");
    }
}
