//! A hand-written RFC 1939 client: `STLS` (RFC 2595), `USER`/`PASS`, `STAT`, `LIST`, `UIDL`,
//! `TOP`, `RETR`, `DELE`, `NOOP`, `QUIT`, over the same TLS / STARTTLS / plaintext-to-localhost
//! stream handling the IMAP provider uses. One session is one maildrop lock: callers open a
//! session per unit of work and `QUIT`, which is also what commits deletions.
//!
//! Error mapping, in the IMAP provider's spirit: a refused connection, a lost connection or
//! a timeout is [`ProviderError::Offline`]; `-ERR` to `USER`/`PASS` is
//! [`ProviderError::SignInRequired`] unless the RFC 2449 code says `[IN-USE]`,
//! `[LOGIN-DELAY]` or `[SYS/TEMP]` ([`ProviderError::Temporary`]); any other `-ERR` is
//! [`ProviderError::Rejected`] with the server's words, except the temporary codes.

use crate::imap::connection::{tcp_connect, wrap_tls, Stream};
use crate::ProviderError;
use overlook_core::{Security, ServerEndpoint};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::time::Duration;

pub struct Client {
    stream: BufReader<Stream>,
    host: String,
    /// Commands sent, for diagnostics (passwords are never recorded).
    pub commands: Vec<String>,
}

/// What a `+OK` line said after the status word.
pub type OkText = String;

impl Client {
    /// Connects, secures the connection as the endpoint asks, and logs in.
    pub fn connect(endpoint: &ServerEndpoint, password: &str, timeout: Duration) -> Result<Self, ProviderError> {
        if endpoint.is_insecure_remote() {
            return Err(ProviderError::Rejected(format!("refusing to connect to {} without encryption", endpoint.host)));
        }
        let host = endpoint.host.trim().to_owned();
        let tcp = tcp_connect(endpoint, timeout)?;
        let mut client = match endpoint.security {
            Security::Tls => {
                let mut c = Self { stream: BufReader::new(wrap_tls(&host, tcp)?), host: host.clone(), commands: Vec::new() };
                c.greeting()?;
                c
            }
            Security::None => {
                let mut c = Self { stream: BufReader::new(Stream::Plain(tcp)), host: host.clone(), commands: Vec::new() };
                c.greeting()?;
                c
            }
            Security::StartTls => {
                let mut plain = Self { stream: BufReader::new(Stream::Plain(tcp)), host: host.clone(), commands: Vec::new() };
                plain.greeting()?;
                match plain.command("STLS") {
                    Ok(_) => {}
                    Err(ProviderError::Rejected(words)) => return Err(ProviderError::Rejected(format!("{host} refused STLS: {words}"))),
                    Err(e) => return Err(e),
                }
                let Stream::Plain(tcp) = plain.stream.into_inner() else { unreachable!("plaintext before STLS") };
                Self { stream: BufReader::new(wrap_tls(&host, tcp)?), host: host.clone(), commands: plain.commands }
            }
        };
        if endpoint.username.trim().is_empty() {
            return Err(ProviderError::SignInRequired);
        }
        client.login(endpoint.username.trim(), password)?;
        Ok(client)
    }

    fn greeting(&mut self) -> Result<(), ProviderError> {
        let line = self.read_line()?;
        if line.starts_with("+OK") {
            Ok(())
        } else {
            Err(ProviderError::Rejected(format!("{} did not greet like a POP3 server: {}", self.host, line.trim_end())))
        }
    }

    fn login(&mut self, user: &str, password: &str) -> Result<(), ProviderError> {
        let sign_in = |e: ProviderError| match e {
            ProviderError::Rejected(words) => match code_of(&words).as_deref() {
                Some("IN-USE") | Some("LOGIN-DELAY") | Some("SYS/TEMP") => ProviderError::Temporary(strip_code(&words)),
                _ => ProviderError::SignInRequired,
            },
            other => other,
        };
        tracing::info!(target: "overlook::signin", host = %self.host, mechanism = "USER/PASS", username = "[account]", password = "[REDACTED]", "POP3 sign-in");
        self.send(&format!("USER {user}"), "USER ****").and_then(|_| self.ok()).map_err(sign_in)?;
        self.send(&format!("PASS {password}"), "PASS ****").and_then(|_| self.ok()).map_err(sign_in)?;
        Ok(())
    }

    fn read_line(&mut self) -> Result<String, ProviderError> {
        let mut buf = Vec::new();
        match self.stream.read_until(b'\n', &mut buf) {
            Ok(0) => Err(ProviderError::Offline),
            Ok(_) => Ok(String::from_utf8_lossy(&buf).into_owned()),
            Err(_) => Err(ProviderError::Offline),
        }
    }

    fn send(&mut self, line: &str, logged: &str) -> Result<(), ProviderError> {
        self.commands.push(logged.to_owned());
        let stream = self.stream.get_mut();
        stream.write_all(line.as_bytes()).and_then(|_| stream.write_all(b"\r\n")).and_then(|_| stream.flush()).map_err(|_| ProviderError::Offline)
    }

    /// Reads a status line: the text after `+OK`, or the mapped `-ERR`.
    fn ok(&mut self) -> Result<OkText, ProviderError> {
        let line = self.read_line()?;
        let line = line.trim_end();
        if let Some(rest) = line.strip_prefix("+OK") {
            return Ok(rest.trim().to_owned());
        }
        let words = line.strip_prefix("-ERR").map(str::trim).unwrap_or(line).to_owned();
        Err(match code_of(&words).as_deref() {
            Some("IN-USE") | Some("LOGIN-DELAY") | Some("SYS/TEMP") => ProviderError::Temporary(strip_code(&words)),
            Some("AUTH") => ProviderError::SignInRequired,
            _ => ProviderError::Rejected(strip_code(&words)),
        })
    }

    /// Sends a single-line command.
    pub fn command(&mut self, line: &str) -> Result<OkText, ProviderError> {
        self.send(line, line)?;
        self.ok()
    }

    /// The lines of a multi-line response, terminated by a lone `.`, with byte-stuffing
    /// undone. CRLF line endings are kept.
    fn multiline(&mut self) -> Result<Vec<u8>, ProviderError> {
        let mut out = Vec::new();
        loop {
            let mut buf = Vec::new();
            match self.stream.read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => return Err(ProviderError::Offline),
                Ok(_) => {}
            }
            if buf == b".\r\n" || buf == b".\n" {
                return Ok(out);
            }
            if buf.starts_with(b"..") {
                out.extend_from_slice(&buf[1..]);
            } else {
                out.extend_from_slice(&buf);
            }
        }
    }

    pub fn noop(&mut self) -> Result<(), ProviderError> {
        self.command("NOOP").map(|_| ())
    }

    /// `(count, size)` of the maildrop.
    pub fn stat(&mut self) -> Result<(u32, u64), ProviderError> {
        let text = self.command("STAT")?;
        let mut parts = text.split_whitespace();
        let count = parts.next().and_then(|n| n.parse().ok()).ok_or_else(|| ProviderError::Temporary(format!("unreadable STAT reply: {text}")))?;
        let size = parts.next().and_then(|n| n.parse().ok()).unwrap_or(0);
        Ok((count, size))
    }

    /// Message number → UIDL, in maildrop order.
    pub fn uidl(&mut self) -> Result<Vec<(u32, String)>, ProviderError> {
        self.command("UIDL")?;
        let body = self.multiline()?;
        let mut out = Vec::new();
        for line in String::from_utf8_lossy(&body).lines() {
            let mut parts = line.split_whitespace();
            if let (Some(n), Some(uidl)) = (parts.next().and_then(|n| n.parse::<u32>().ok()), parts.next()) {
                out.push((n, uidl.to_owned()));
            }
        }
        Ok(out)
    }

    /// Message number → size in octets.
    pub fn list(&mut self) -> Result<BTreeMap<u32, u64>, ProviderError> {
        self.command("LIST")?;
        let body = self.multiline()?;
        let mut out = BTreeMap::new();
        for line in String::from_utf8_lossy(&body).lines() {
            let mut parts = line.split_whitespace();
            if let (Some(n), Some(size)) = (parts.next().and_then(|n| n.parse::<u32>().ok()), parts.next().and_then(|s| s.parse::<u64>().ok())) {
                out.insert(n, size);
            }
        }
        Ok(out)
    }

    /// The headers and the first `lines` body lines of a message.
    pub fn top(&mut self, n: u32, lines: u32) -> Result<Vec<u8>, ProviderError> {
        self.command(&format!("TOP {n} {lines}"))?;
        self.multiline()
    }

    /// The whole message.
    pub fn retr(&mut self, n: u32) -> Result<Vec<u8>, ProviderError> {
        self.command(&format!("RETR {n}"))?;
        self.multiline()
    }

    /// Marks a message deleted; `QUIT` commits.
    pub fn dele(&mut self, n: u32) -> Result<(), ProviderError> {
        self.command(&format!("DELE {n}")).map(|_| ())
    }

    /// Ends the session, committing deletions. A server that drops the connection without
    /// answering is reported as `Offline`: its deletions may not have happened.
    pub fn quit(mut self) -> Result<(), ProviderError> {
        self.command("QUIT").map(|_| ())
    }
}

/// The RFC 2449 response code at the start of an `-ERR` text, upper-cased.
fn code_of(words: &str) -> Option<String> {
    let rest = words.trim_start().strip_prefix('[')?;
    let end = rest.find(']')?;
    Some(rest[..end].trim().to_ascii_uppercase())
}

fn strip_code(words: &str) -> String {
    let t = words.trim();
    match (t.starts_with('['), t.find(']')) {
        (true, Some(end)) => t[end + 1..].trim().to_owned(),
        _ => t.to_owned(),
    }
    .trim_end_matches('.')
    .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_and_words() {
        assert_eq!(code_of("[IN-USE] Do you have another POP session running?").as_deref(), Some("IN-USE"));
        assert_eq!(code_of("no such message"), None);
        assert_eq!(strip_code("[AUTH] Authentication failed."), "Authentication failed");
        assert_eq!(strip_code("no such message"), "no such message");
    }

    #[test]
    fn refuses_plaintext_to_remote_hosts_and_maps_refused_connections() {
        let e = Client::connect(&ServerEndpoint::new("pop.example.com", 110, Security::None, "j"), "pw", Duration::from_secs(1)).err();
        assert!(matches!(e, Some(ProviderError::Rejected(_))), "{e:?}");
        let unreachable = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
        assert_eq!(Client::connect(&ServerEndpoint::new("127.0.0.1", unreachable.port(), Security::None, "j"), "pw", Duration::from_secs(1)).err(), Some(ProviderError::Offline));
    }

    /// A one-shot server that plays a script: reads a line, answers with the next reply.
    fn scripted(replies: Vec<&'static str>) -> std::net::SocketAddr {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut replies = replies.into_iter();
            stream.write_all(replies.next().unwrap().as_bytes()).unwrap();
            let mut line = String::new();
            while reader.read_line(&mut line).unwrap_or(0) > 0 {
                line.clear();
                let Some(reply) = replies.next() else { break };
                stream.write_all(reply.as_bytes()).unwrap();
            }
        });
        addr
    }

    #[test]
    fn unstuffs_multiline_replies_and_maps_errors() {
        let addr = scripted(vec!["+OK hello\r\n", "+OK\r\n", "+OK\r\n", "+OK 2 messages:\r\n1 a\r\n2 b\r\n.\r\n", "+OK 12 octets\r\nSubject: x\r\n\r\n..dot line\r\n.\r\n", "-ERR no such message\r\n", "-ERR [SYS/TEMP] try later\r\n", "+OK bye\r\n"]);
        let mut c = Client::connect(&ServerEndpoint::new("127.0.0.1", addr.port(), Security::None, "j"), "pw", Duration::from_secs(2)).unwrap();
        assert_eq!(c.uidl().unwrap(), vec![(1, "a".to_owned()), (2, "b".to_owned())]);
        assert_eq!(c.retr(1).unwrap(), b"Subject: x\r\n\r\n.dot line\r\n");
        assert_eq!(c.retr(9).err(), Some(ProviderError::Rejected("no such message".into())));
        assert_eq!(c.dele(1).err(), Some(ProviderError::Temporary("try later".into())));
        assert_eq!(c.commands, vec!["USER ****", "PASS ****", "UIDL", "RETR 1", "RETR 9", "DELE 1"]);
        c.quit().unwrap();
    }

    #[test]
    fn login_failures_are_sign_in_required_unless_temporary() {
        let addr = scripted(vec!["+OK\r\n", "+OK\r\n", "-ERR [AUTH] bad password\r\n"]);
        assert_eq!(Client::connect(&ServerEndpoint::new("127.0.0.1", addr.port(), Security::None, "j"), "pw", Duration::from_secs(2)).err(), Some(ProviderError::SignInRequired));
        let addr = scripted(vec!["+OK\r\n", "+OK\r\n", "-ERR [IN-USE] maildrop locked\r\n"]);
        assert_eq!(Client::connect(&ServerEndpoint::new("127.0.0.1", addr.port(), Security::None, "j"), "pw", Duration::from_secs(2)).err(), Some(ProviderError::Temporary("maildrop locked".into())));
        let addr = scripted(vec!["* OK I am IMAP\r\n"]);
        assert!(matches!(Client::connect(&ServerEndpoint::new("127.0.0.1", addr.port(), Security::None, "j"), "pw", Duration::from_secs(2)).err(), Some(ProviderError::Rejected(_))));
    }
}
