//! A scripted POP3 server for the provider tests, shaped like Dovecot's replies.
//!
//! It serves `CAPA`, `USER`/`PASS`, `STAT`, `LIST`, `UIDL`, `TOP`, `RETR`, `DELE`, `RSET`,
//! `NOOP` and `QUIT` from a mutable maildrop the test can change between sessions, and
//! keeps POP3's transaction semantics: `DELE` marks, `QUIT` commits, a dropped connection
//! discards the marks. Faults: refuse the login or report the maildrop in use, drop the
//! connection after executing the next command containing a word (`drop_after_next`), or
//! answer the next such command with a scripted `-ERR` (`fail_next`). `STLS` is refused.
//!
//! Compiled for this crate's tests and, with the `fake-servers` feature, for other crates'
//! tests that drive the real provider through the sync engine.

use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Login {
    Ok,
    AuthFailed,
    InUse,
}

#[derive(Clone, Debug)]
pub struct FakeMessage {
    pub uidl: String,
    pub raw: Vec<u8>,
    /// Reported `LIST` size when it should differ from the bytes (to force the `TOP` path).
    pub size_override: Option<u64>,
}

impl FakeMessage {
    pub fn new(uidl: &str, raw: impl Into<Vec<u8>>) -> Self {
        Self { uidl: uidl.into(), raw: raw.into(), size_override: None }
    }
}

#[derive(Debug)]
pub struct FakeState {
    pub user: String,
    /// What `PASS` must present, and the bearer token `AUTH XOAUTH2` must present.
    pub password: String,
    /// Answer a refused `XOAUTH2` token with a JSON error challenge before the `-ERR`.
    pub xoauth2_error_challenge: bool,
    /// Every bearer token presented with `AUTH XOAUTH2`, accepted or not.
    pub tokens_seen: Vec<String>,
    pub login: Login,
    /// The maildrop in message-number order (1-based on the wire).
    pub messages: Vec<FakeMessage>,
    /// UIDLs whose deletion a `QUIT` committed.
    pub deleted: Vec<String>,
    /// Every command line received, with passwords masked.
    pub log: Vec<String>,
    pub connections: usize,
    /// Sessions ended by `QUIT`.
    pub quits: usize,
    /// Execute the next command containing this word, then close without answering.
    pub drop_after_next: Option<String>,
    /// Answer the next command containing the word with this `-ERR` text instead.
    pub fail_next: Option<(String, String)>,
    /// Refuse `TOP` (an RFC 1939 server need not offer it).
    pub no_top: bool,
}

impl Default for FakeState {
    fn default() -> Self {
        Self { user: "jason@example.com".into(), password: "hunter2".into(), xoauth2_error_challenge: false, tokens_seen: Vec::new(), login: Login::Ok, messages: Vec::new(), deleted: Vec::new(), log: Vec::new(), connections: 0, quits: 0, drop_after_next: None, fail_next: None, no_top: false }
    }
}

impl FakeState {
    pub fn add(&mut self, m: FakeMessage) {
        self.messages.push(m);
    }

    pub fn uidls(&self) -> Vec<String> {
        self.messages.iter().map(|m| m.uidl.clone()).collect()
    }

    pub fn commands(&self, prefix: &str) -> usize {
        self.log.iter().filter(|l| l.starts_with(prefix)).count()
    }
}

pub struct FakeServer {
    pub addr: SocketAddr,
    pub state: Arc<Mutex<FakeState>>,
}

impl FakeServer {
    pub fn lock(&self) -> std::sync::MutexGuard<'_, FakeState> {
        self.state.lock().unwrap()
    }
}

pub fn serve(state: FakeState) -> FakeServer {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let state = Arc::new(Mutex::new(state));
    let shared = state.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { break };
            let state = shared.clone();
            std::thread::spawn(move || {
                let _ = Session { state, stream: stream.try_clone().unwrap(), reader: BufReader::new(stream), authed: false, marked: BTreeSet::new(), muted: false }.run();
            });
        }
    });
    FakeServer { addr, state }
}

struct Session {
    state: Arc<Mutex<FakeState>>,
    stream: TcpStream,
    reader: BufReader<TcpStream>,
    authed: bool,
    /// Message numbers marked by `DELE` in this session.
    marked: BTreeSet<usize>,
    muted: bool,
}

impl Session {
    fn say(&mut self, line: &str) -> std::io::Result<()> {
        if self.muted {
            return Ok(());
        }
        self.stream.write_all(line.as_bytes())?;
        self.stream.write_all(b"\r\n")
    }

    /// A multi-line body with byte stuffing and the terminating `.`.
    fn body(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        if self.muted {
            return Ok(());
        }
        let mut out = Vec::with_capacity(bytes.len() + 16);
        for line in bytes.split_inclusive(|b| *b == b'\n') {
            if line.starts_with(b".") {
                out.push(b'.');
            }
            out.extend_from_slice(line);
        }
        if !out.ends_with(b"\n") {
            out.extend_from_slice(b"\r\n");
        }
        out.extend_from_slice(b".\r\n");
        self.stream.write_all(&out)
    }

    fn run(&mut self) -> std::io::Result<()> {
        {
            let mut s = self.state.lock().unwrap();
            s.connections += 1;
        }
        self.say("+OK Fake POP3 ready.")?;
        let mut user: Option<String> = None;
        let mut line = String::new();
        loop {
            line.clear();
            if self.reader.read_line(&mut line)? == 0 {
                return Ok(());
            }
            let text = line.trim_end_matches(['\r', '\n']).to_owned();
            let (cmd, args) = text.split_once(' ').unwrap_or((&text, ""));
            let cmd = cmd.to_ascii_uppercase();
            let upper = text.to_ascii_uppercase();
            {
                let mut s = self.state.lock().unwrap();
                s.log.push(if cmd == "PASS" { "PASS ****".into() } else { text.clone() });
                if s.drop_after_next.as_ref().is_some_and(|w| upper.contains(w.as_str())) {
                    s.drop_after_next = None;
                    self.muted = true;
                }
                if let Some((word, reply)) = s.fail_next.clone() {
                    if upper.contains(word.as_str()) {
                        s.fail_next = None;
                        drop(s);
                        self.say(&format!("-ERR {reply}"))?;
                        continue;
                    }
                }
            }
            if !self.authed && !matches!(cmd.as_str(), "USER" | "PASS" | "AUTH" | "CAPA" | "QUIT" | "STLS") {
                self.say("-ERR [AUTH] Unauthenticated")?;
                continue;
            }
            match cmd.as_str() {
                "CAPA" => {
                    self.say("+OK")?;
                    self.body(b"TOP\r\nUIDL\r\nUSER\r\nSASL XOAUTH2\r\nPIPELINING\r\n")?;
                }
                "AUTH" if args.eq_ignore_ascii_case("XOAUTH2") => {
                    use base64::Engine;
                    self.say("+ ")?;
                    let mut response = String::new();
                    if self.reader.read_line(&mut response)? == 0 {
                        return Ok(());
                    }
                    let raw = base64::engine::general_purpose::STANDARD.decode(response.trim()).unwrap_or_default();
                    let text = String::from_utf8_lossy(&raw).into_owned();
                    let mut fields = text.split('\x01');
                    let u = fields.next().and_then(|f| f.strip_prefix("user=")).unwrap_or_default().to_owned();
                    let token = fields.next().and_then(|f| f.strip_prefix("auth=Bearer ")).unwrap_or_default().to_owned();
                    let (login, accepted, challenge) = {
                        let mut s = self.state.lock().unwrap();
                        s.log.push("XOAUTH2 ****".into());
                        s.tokens_seen.push(token.clone());
                        (s.login, u == s.user && token == s.password, s.xoauth2_error_challenge)
                    };
                    match login {
                        Login::InUse => self.say("-ERR [IN-USE] Do you have another POP session running?")?,
                        Login::Ok if accepted => {
                            self.authed = true;
                            self.say("+OK User successfully authenticated.")?;
                        }
                        _ => {
                            if challenge {
                                let error = base64::engine::general_purpose::STANDARD.encode(r#"{"status":"401","schemes":"bearer"}"#);
                                self.say(&format!("+ {error}"))?;
                                let mut cancel = String::new();
                                self.reader.read_line(&mut cancel)?;
                                self.state.lock().unwrap().log.push(cancel.trim().to_owned());
                            }
                            self.say("-ERR [AUTH] Authentication failure: unknown user name or bad password.")?;
                        }
                    }
                }
                "STLS" => self.say("-ERR STLS is not offered by the fake server")?,
                "USER" => {
                    user = Some(args.to_owned());
                    self.say("+OK")?;
                }
                "PASS" => {
                    let (login, u, p) = {
                        let s = self.state.lock().unwrap();
                        (s.login, s.user.clone(), s.password.clone())
                    };
                    match login {
                        Login::InUse => self.say("-ERR [IN-USE] Do you have another POP session running?")?,
                        Login::AuthFailed => self.say("-ERR [AUTH] Authentication failed.")?,
                        Login::Ok if user.as_deref() == Some(u.as_str()) && args == p => {
                            self.authed = true;
                            let n = self.state.lock().unwrap().messages.len();
                            self.say(&format!("+OK Logged in. {n} messages"))?;
                        }
                        Login::Ok => self.say("-ERR [AUTH] Authentication failed.")?,
                    }
                }
                "NOOP" => self.say("+OK")?,
                "STAT" => {
                    let s = self.state.lock().unwrap();
                    let live: Vec<&FakeMessage> = s.messages.iter().enumerate().filter(|(i, _)| !self.marked.contains(&(i + 1))).map(|(_, m)| m).collect();
                    let size: u64 = live.iter().map(|m| m.size_override.unwrap_or(m.raw.len() as u64)).sum();
                    let reply = format!("+OK {} {size}", live.len());
                    drop(s);
                    self.say(&reply)?;
                }
                "LIST" | "UIDL" => {
                    let s = self.state.lock().unwrap();
                    let mut lines = Vec::new();
                    for (i, m) in s.messages.iter().enumerate() {
                        if self.marked.contains(&(i + 1)) {
                            continue;
                        }
                        lines.push(if cmd == "LIST" { format!("{} {}", i + 1, m.size_override.unwrap_or(m.raw.len() as u64)) } else { format!("{} {}", i + 1, m.uidl) });
                    }
                    drop(s);
                    if let Some(n) = args.split_whitespace().next() {
                        match lines.iter().find(|l| l.starts_with(&format!("{n} "))) {
                            Some(l) => self.say(&format!("+OK {l}"))?,
                            None => self.say("-ERR no such message")?,
                        }
                        continue;
                    }
                    self.say(&format!("+OK {} messages", lines.len()))?;
                    self.body(format!("{}\r\n", lines.join("\r\n")).trim_start().as_bytes())?;
                }
                "RETR" | "TOP" => {
                    let mut parts = args.split_whitespace();
                    let n: usize = parts.next().and_then(|n| n.parse().ok()).unwrap_or(0);
                    let lines: usize = parts.next().and_then(|n| n.parse().ok()).unwrap_or(0);
                    let s = self.state.lock().unwrap();
                    if cmd == "TOP" && s.no_top {
                        drop(s);
                        self.say("-ERR TOP not supported")?;
                        continue;
                    }
                    let Some(m) = (n > 0 && !self.marked.contains(&n)).then(|| s.messages.get(n - 1)).flatten() else {
                        drop(s);
                        self.say("-ERR no such message")?;
                        continue;
                    };
                    let raw = m.raw.clone();
                    drop(s);
                    let bytes = if cmd == "RETR" { raw } else { top_bytes(&raw, lines) };
                    self.say(&format!("+OK {} octets", bytes.len()))?;
                    self.body(&bytes)?;
                }
                "DELE" => {
                    let n: usize = args.trim().parse().unwrap_or(0);
                    let exists = { n > 0 && n <= self.state.lock().unwrap().messages.len() };
                    if !exists || self.marked.contains(&n) {
                        self.say("-ERR no such message")?;
                    } else {
                        self.marked.insert(n);
                        self.say(&format!("+OK message {n} deleted"))?;
                    }
                }
                "RSET" => {
                    self.marked.clear();
                    self.say("+OK")?;
                }
                "QUIT" => {
                    let mut s = self.state.lock().unwrap();
                    if self.authed {
                        let marked: Vec<usize> = self.marked.iter().copied().collect();
                        for n in marked.iter().rev() {
                            if *n <= s.messages.len() {
                                let removed = s.messages.remove(n - 1);
                                s.deleted.push(removed.uidl);
                            }
                        }
                        s.quits += 1;
                    }
                    drop(s);
                    self.say("+OK Logging out.")?;
                    return Ok(());
                }
                _ => self.say("-ERR Unknown command")?,
            }
            if self.muted {
                return Ok(());
            }
        }
    }
}

/// The headers and the first `lines` body lines, as `TOP` serves them.
fn top_bytes(raw: &[u8], lines: usize) -> Vec<u8> {
    let (head, body) = match raw.windows(4).position(|w| w == b"\r\n\r\n") {
        Some(i) => (&raw[..i + 4], &raw[i + 4..]),
        None => (raw, &b""[..]),
    };
    let mut out = head.to_vec();
    for line in body.split_inclusive(|b| *b == b'\n').take(lines) {
        out.extend_from_slice(line);
    }
    out
}
