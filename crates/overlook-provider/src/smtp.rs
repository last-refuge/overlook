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
//! Passwords are passed in at construction and never logged.

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

/// One SMTP server with the credentials to use it.
pub struct Transport {
    endpoint: ServerEndpoint,
    password: String,
    timeout: Duration,
}

impl std::fmt::Debug for Transport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The password is deliberately absent.
        f.debug_struct("Transport").field("endpoint", &self.endpoint).field("timeout", &self.timeout).finish()
    }
}

impl Transport {
    pub fn new(endpoint: ServerEndpoint, password: impl Into<String>) -> Self {
        Self { endpoint, password: password.into(), timeout: DEFAULT_TIMEOUT }
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

    fn connect(&self) -> Result<SmtpConnection, ProviderError> {
        let endpoint = &self.endpoint;
        if endpoint.is_insecure_remote() {
            return Err(ProviderError::Rejected(format!("refusing to send to {} without encryption", endpoint.host)));
        }
        let client_id = ClientId::default();
        let tls = match endpoint.security {
            Security::None => None,
            Security::Tls | Security::StartTls => Some(TlsParameters::new(endpoint.host.clone()).map_err(connect_error)?),
        };
        let wrapper = match endpoint.security {
            Security::Tls => tls.as_ref(),
            _ => None,
        };
        let mut conn = SmtpConnection::connect((endpoint.host.as_str(), endpoint.port), Some(self.timeout), &client_id, wrapper, None).map_err(connect_error)?;
        if endpoint.security == Security::StartTls {
            if let Some(params) = tls.as_ref() {
                conn.starttls(params, &client_id).map_err(connect_error)?;
            }
        }
        if !endpoint.username.is_empty() {
            conn.auth(&[Mechanism::Plain, Mechanism::Login], &Credentials::new(endpoint.username.clone(), self.password.clone())).map_err(auth_error)?;
        }
        Ok(conn)
    }
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
    if e.is_tls() {
        return ProviderError::Rejected(tls_message(&e).into());
    }
    // Network, timeouts, or a connection closed mid-reply (lettre reports the EOF as an
    // incomplete response): the server was never properly reached.
    if e.is_client() {
        ProviderError::Temporary(crate::secure_error::UNREADABLE_RESPONSE.into())
    } else {
        ProviderError::Offline
    }
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

fn auth_error(e: SmtpError) -> ProviderError {
    match e.status() {
        Some(code) if code.severity == Severity::PermanentNegativeCompletion => ProviderError::SignInRequired,
        Some(_) => ProviderError::Temporary(reply_text(&e)),
        // No shared mechanism is a settings problem, not a network one.
        None if e.is_client() => {
            crate::secure_error::log_detail("SMTP authentication", &e);
            ProviderError::Rejected("the outgoing server offers no sign-in method Overlook supports".into())
        }
        None => connect_error(e),
    }
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
        TemporaryMail,
        RejectRecipient,
        DropAfterData,
        DropDuringGreeting,
    }

    /// What the server saw.
    #[derive(Default, Debug, Clone)]
    pub struct Seen {
        pub auth: Option<(String, String)>,
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
                say("250-AUTH PLAIN LOGIN");
                say("250 8BITMIME");
            } else if upper.starts_with("AUTH PLAIN") {
                let b64 = cmd.split_whitespace().nth(2).unwrap_or("");
                let raw = base64::engine::general_purpose::STANDARD.decode(b64).unwrap_or_default();
                let mut parts = raw.split(|b| *b == 0).skip(1).map(|p| String::from_utf8_lossy(p).into_owned());
                seen.auth = Some((parts.next().unwrap_or_default(), parts.next().unwrap_or_default()));
                if behaviour == Behaviour::RejectAuth {
                    say("535 5.7.8 Authentication credentials invalid");
                } else {
                    say("235 2.7.0 Authentication successful");
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
