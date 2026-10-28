//! Opening an authenticated IMAP session and mapping its failures onto [`ProviderError`].
//!
//! The `imap` crate is used without its TLS features (they pin an older rustls), so the
//! TLS stream is built here with the rustls `ureq` already uses and handed to
//! `imap::Client::new`, which is generic over `Read + Write`. STARTTLS is negotiated by hand
//! on the raw socket before the client exists, exactly as the M0 spike did.
//!
//! Error mapping, from the plan: a lost connection, a `BYE`, a socket timeout or a refused
//! connection is [`ProviderError::Offline`]; `[AUTHENTICATIONFAILED]` (or any `NO` to
//! `LOGIN`) is [`ProviderError::SignInRequired`]; `[UNAVAILABLE]`, `[INUSE]` and friends are
//! [`ProviderError::Temporary`]; any other `NO` or `BAD` is [`ProviderError::Rejected`] with
//! the server's own words. A failed TLS handshake is `Rejected` too: it will not pass on
//! retry without a settings change.

use crate::ProviderError;
use imap_proto::types::ResponseCode;
use overlook_core::{Security, ServerEndpoint};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;

/// A TCP stream that may be wrapped in TLS. One `Session<Stream>` type serves every
/// security mode.
pub enum Stream {
    Plain(TcpStream),
    Tls(Box<rustls::StreamOwned<rustls::ClientConnection, TcpStream>>),
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Stream::Plain(s) => s.read(buf),
            Stream::Tls(s) => s.read(buf),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Stream::Plain(s) => s.write(buf),
            Stream::Tls(s) => s.write(buf),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Stream::Plain(s) => s.flush(),
            Stream::Tls(s) => s.flush(),
        }
    }
}

pub type Session = imap::Session<Stream>;

fn tls_config() -> Arc<rustls::ClientConfig> {
    static CONFIG: std::sync::OnceLock<Arc<rustls::ClientConfig>> = std::sync::OnceLock::new();
    CONFIG
        .get_or_init(|| {
            let mut roots = rustls::RootCertStore::empty();
            roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            Arc::new(rustls::ClientConfig::builder().with_root_certificates(roots).with_no_client_auth())
        })
        .clone()
}

fn io_error(host: &str, what: &str, e: &std::io::Error) -> ProviderError {
    use std::io::ErrorKind::*;
    match e.kind() {
        // rustls reports certificate and protocol failures as InvalidData.
        InvalidData => ProviderError::Rejected(format!("{what} to {host} failed: {}", crate::secure_error::io(what, e))),
        _ => ProviderError::Offline,
    }
}

pub(crate) fn tcp_connect(endpoint: &ServerEndpoint, timeout: Duration) -> Result<TcpStream, ProviderError> {
    let host = endpoint.host.trim();
    if host.is_empty() {
        return Err(ProviderError::Rejected("no incoming server is set for this account".into()));
    }
    let mut addrs = (host, endpoint.port).to_socket_addrs().map_err(|_| ProviderError::Offline)?;
    let addr = addrs.next().ok_or(ProviderError::Offline)?;
    let tcp = TcpStream::connect_timeout(&addr, timeout).map_err(|_| ProviderError::Offline)?;
    tcp.set_read_timeout(Some(timeout)).map_err(|_| ProviderError::Offline)?;
    tcp.set_write_timeout(Some(timeout)).map_err(|_| ProviderError::Offline)?;
    let _ = tcp.set_nodelay(true);
    Ok(tcp)
}

/// Completes the TLS handshake before the stream is handed to the IMAP client, so a
/// certificate problem is reported as such rather than as a lost connection.
pub(crate) fn wrap_tls(host: &str, mut tcp: TcpStream) -> Result<Stream, ProviderError> {
    let name = rustls::pki_types::ServerName::try_from(host.to_owned()).map_err(|e| { crate::secure_error::log_detail("TLS server name", e); ProviderError::Rejected(format!("“{host}”: {}", crate::secure_error::INVALID_SERVER_NAME)) })?;
    let mut conn = rustls::ClientConnection::new(tls_config(), name).map_err(|e| { crate::secure_error::log_detail("TLS setup", &e); ProviderError::Rejected(crate::secure_error::rustls_message(&e).into()) })?;
    while conn.is_handshaking() {
        conn.complete_io(&mut tcp).map_err(|e| io_error(host, "The TLS handshake", &e))?;
    }
    Ok(Stream::Tls(Box::new(rustls::StreamOwned::new(conn, tcp))))
}

/// Reads the greeting, sends `STARTTLS`, and returns the socket once the server says OK.
fn starttls_by_hand(host: &str, tcp: TcpStream) -> Result<TcpStream, ProviderError> {
    let mut reader = BufReader::new(tcp.try_clone().map_err(|_| ProviderError::Offline)?);
    let mut writer = tcp;
    let mut line = String::new();
    if reader.read_line(&mut line).map_err(|_| ProviderError::Offline)? == 0 {
        return Err(ProviderError::Offline);
    }
    if !line.starts_with("* OK") && !line.starts_with("* PREAUTH") {
        return Err(ProviderError::Rejected(format!("{host} did not greet like an IMAP server: {}", line.trim_end())));
    }
    writer.write_all(b"s1 STARTTLS\r\n").and_then(|_| writer.flush()).map_err(|_| ProviderError::Offline)?;
    loop {
        line.clear();
        if reader.read_line(&mut line).map_err(|_| ProviderError::Offline)? == 0 {
            return Err(ProviderError::Offline);
        }
        if line.starts_with("s1 OK") {
            return Ok(writer);
        }
        if line.starts_with("s1 ") {
            return Err(ProviderError::Rejected(format!("{host} refused STARTTLS: {}", line.trim_end().trim_start_matches("s1 "))));
        }
    }
}

/// The IDLE thread's read timeout is set through the `imap` crate's trait; the TLS stream
/// exposes its socket, so one implementation serves every security mode.
impl imap::extensions::idle::SetReadTimeout for Stream {
    fn set_read_timeout(&mut self, timeout: Option<Duration>) -> imap::error::Result<()> {
        match self {
            Stream::Plain(s) => TcpStream::set_read_timeout(s, timeout),
            Stream::Tls(s) => TcpStream::set_read_timeout(&s.sock, timeout),
        }
        .map_err(imap::Error::Io)
    }
}

/// Connects, secures the connection as the endpoint asks, and logs in.
pub fn connect(endpoint: &ServerEndpoint, password: &str, timeout: Duration) -> Result<Session, ProviderError> {
    connect_with_socket(endpoint, password, timeout).map(|(session, _)| session)
}

/// Like [`connect`], also returning a handle on the underlying socket. Shutting that handle
/// down from another thread unblocks a read in progress, which is how the IDLE watcher is
/// stopped promptly.
pub fn connect_with_socket(endpoint: &ServerEndpoint, password: &str, timeout: Duration) -> Result<(Session, TcpStream), ProviderError> {
    if endpoint.is_insecure_remote() {
        return Err(ProviderError::Rejected(format!("refusing to connect to {} without encryption", endpoint.host)));
    }
    let host = endpoint.host.trim();
    let tcp = tcp_connect(endpoint, timeout)?;
    let socket = tcp.try_clone().map_err(|_| ProviderError::Offline)?;
    let mut client = match endpoint.security {
        Security::Tls => {
            let mut c = imap::Client::new(wrap_tls(host, tcp)?);
            c.read_greeting().map_err(|e| map_error(&e))?;
            c
        }
        Security::None => {
            let mut c = imap::Client::new(Stream::Plain(tcp));
            c.read_greeting().map_err(|e| map_error(&e))?;
            c
        }
        Security::StartTls => {
            let tcp = starttls_by_hand(host, tcp)?;
            let mut c = imap::Client::new(wrap_tls(host, tcp)?);
            // No new greeting after STARTTLS; mark it read so LOGIN does not wait for one.
            c.greeting_read = true;
            c
        }
    };
    client.debug = false;
    if endpoint.username.trim().is_empty() {
        return Err(ProviderError::SignInRequired);
    }
    let session = client.login(endpoint.username.trim(), password).map_err(|(e, _)| login_error(&e))?;
    Ok((session, socket))
}

/// Which family of problem a response text's bracketed code announces.
fn code_word(information: &str) -> Option<String> {
    let t = information.trim_start();
    let rest = t.strip_prefix('[')?;
    let end = rest.find(']')?;
    Some(rest[..end].split_whitespace().next().unwrap_or("").to_ascii_uppercase())
}

fn no_response(information: &str, code: &Option<ResponseCode<'static>>) -> ProviderError {
    let word = code_word(information).unwrap_or_default();
    match word.as_str() {
        "AUTHENTICATIONFAILED" | "AUTHORIZATIONFAILED" | "EXPIRED" | "PRIVACYREQUIRED" => ProviderError::SignInRequired,
        "UNAVAILABLE" | "INUSE" | "SERVERBUG" | "LIMIT" | "CONTACTADMIN" => ProviderError::Temporary(strip_code(information)),
        _ => match code {
            Some(ResponseCode::TryCreate) => ProviderError::Rejected("that folder no longer exists".into()),
            _ => ProviderError::Rejected(strip_code(information)),
        },
    }
}

/// The server's words without the leading `[CODE]`.
fn strip_code(information: &str) -> String {
    let t = information.trim();
    match (t.starts_with('['), t.find(']')) {
        (true, Some(end)) => t[end + 1..].trim().to_owned(),
        _ => t.to_owned(),
    }
    .trim_end_matches('.')
    .to_owned()
}

/// Maps a command failure. Use [`login_error`] for `LOGIN`, where any `NO` means the
/// credentials were not accepted.
pub fn map_error(e: &imap::Error) -> ProviderError {
    match e {
        imap::Error::Io(_) | imap::Error::ConnectionLost | imap::Error::Bye(_) => ProviderError::Offline,
        imap::Error::No(no) => no_response(&no.information, &no.code),
        imap::Error::Bad(bad) => ProviderError::Rejected(format!("the server rejected a command: {}", strip_code(&bad.information))),
        imap::Error::Parse(p) => ProviderError::Temporary(format!("the server's reply could not be read: {p}")),
        imap::Error::Validate(v) => ProviderError::Rejected(v.to_string()),
        other => ProviderError::Temporary(other.to_string()),
    }
}

pub fn login_error(e: &imap::Error) -> ProviderError {
    match e {
        imap::Error::No(no) => match no_response(&no.information, &no.code) {
            ProviderError::Temporary(t) => ProviderError::Temporary(t),
            _ => ProviderError::SignInRequired,
        },
        other => map_error(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_no_responses_by_code() {
        // `imap::error::No` cannot be built outside its crate; the fake-server tests drive
        // the real path. This covers the classification itself.
        assert_eq!(no_response("[AUTHENTICATIONFAILED] Authentication failed.", &None), ProviderError::SignInRequired);
        assert_eq!(no_response("[UNAVAILABLE] Temporary authentication failure.", &None), ProviderError::Temporary("Temporary authentication failure".into()));
        assert_eq!(no_response("[INUSE] Mailbox is in use", &None), ProviderError::Temporary("Mailbox is in use".into()));
        assert_eq!(no_response("Mailbox doesn't exist: Old", &Some(ResponseCode::TryCreate)), ProviderError::Rejected("that folder no longer exists".into()));
        assert_eq!(no_response("Mailbox doesn't exist: Old", &None), ProviderError::Rejected("Mailbox doesn't exist: Old".into()));
        assert_eq!(map_error(&imap::Error::ConnectionLost), ProviderError::Offline);
        assert_eq!(map_error(&imap::Error::Io(std::io::Error::new(std::io::ErrorKind::TimedOut, "slow"))), ProviderError::Offline);
        assert_eq!(code_word("  [ALERT] x"), Some("ALERT".into()));
        assert_eq!(code_word("plain"), None);
    }

    #[test]
    fn refuses_plaintext_to_remote_hosts() {
        let e = connect(&ServerEndpoint::new("imap.example.com", 143, Security::None, "j"), "pw", Duration::from_secs(1)).err();
        assert!(matches!(e, Some(ProviderError::Rejected(_))), "{e:?}");
        let unreachable = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
        assert_eq!(connect(&ServerEndpoint::new("127.0.0.1", unreachable.port(), Security::None, "j"), "pw", Duration::from_secs(1)).err(), Some(ProviderError::Offline));
    }
}
