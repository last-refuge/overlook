//! Stateful loopback HTTPS fixture for cross-crate workflow tests. This is not a
//! reference CalDAV implementation: reports contain non-recurring test resources.
//! Compiled only with `fake-servers`; no real credentials, DNS or external traffic.
use super::*;
use overlook_core::CalDavRemote;
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

const CERT: &[u8] = include_bytes!("../../../overlook-app/tests/fixtures/resource-test-cert.der");
const KEY: &[u8] = include_bytes!("../../../overlook-app/tests/fixtures/resource-test-key.der");
const ORIGIN: &str = "https://synthetic.example/";

#[derive(Default)]
struct State {
    resources: BTreeMap<String, CalDavRemote>,
    revision: usize,
    writes: usize,
    offline: bool,
    denied: bool,
    lose_reply: bool,
    stop: bool,
    body_reports: usize,
    sync_reports: usize,
    expire_token: bool,
    partial_sync: bool,
    fail_reports: bool,
}

pub struct CalendarServer {
    address: SocketAddr,
    state: Arc<Mutex<State>>,
    worker: Option<JoinHandle<()>>,
}

impl CalendarServer {
    pub fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let state = Arc::new(Mutex::new(State::default()));
        let shared = state.clone();
        let worker = std::thread::spawn(move || {
            let config = Arc::new(
                rustls::ServerConfig::builder()
                    .with_no_client_auth()
                    .with_single_cert(
                        vec![rustls::pki_types::CertificateDer::from(CERT.to_vec())],
                        rustls::pki_types::PrivatePkcs8KeyDer::from(KEY.to_vec()).into(),
                    )
                    .unwrap(),
            );
            while !shared.lock().unwrap().stop {
                let socket = match listener.accept() {
                    Ok((socket, _)) => socket,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(e) => panic!("calendar fixture accept failed: {e}"),
                };
                // Accepted sockets can inherit the listener's nonblocking mode.
                socket.set_nonblocking(false).unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                socket
                    .set_write_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                if shared.lock().unwrap().offline {
                    continue;
                }
                let mut stream = rustls::StreamOwned::new(
                    rustls::ServerConnection::new(config.clone()).unwrap(),
                    socket,
                );
                let Some(request) = read_request(&mut stream) else {
                    continue;
                };
                let mut state = shared.lock().unwrap();
                let response = respond(&mut state, &request);
                drop(state);
                if let Some(response) = response {
                    let _ = stream.write_all(response.as_bytes());
                    let _ = stream.flush();
                }
            }
        });
        Self {
            address,
            state,
            worker: Some(worker),
        }
    }

    pub fn settings(&self) -> CalDavSettings {
        CalDavSettings {
            server_url: ORIGIN.into(),
            username: "fixture@example.test".into(),
            credential_source: CalendarCredentialSource::Separate,
        }
    }

    /// Trust only this public fixture certificate and resolve only to our loopback socket.
    pub fn client(&self) -> CalDavClient {
        reference_client(self.address)
    }

    pub fn set_offline(&self, offline: bool) {
        self.state.lock().unwrap().offline = offline;
    }
    pub fn deny_access(&self, denied: bool) {
        self.state.lock().unwrap().denied = denied;
    }
    pub fn lose_next_write_reply(&self) {
        self.state.lock().unwrap().lose_reply = true;
    }
    pub fn write_count(&self) -> usize {
        self.state.lock().unwrap().writes
    }
    pub fn resources(&self) -> BTreeMap<String, CalDavRemote> {
        self.state.lock().unwrap().resources.clone()
    }
    pub fn report_counts(&self) -> (usize, usize) {
        let state = self.state.lock().unwrap();
        (state.body_reports, state.sync_reports)
    }
    pub fn expire_next_token(&self) {
        self.state.lock().unwrap().expire_token = true;
    }
    pub fn partial_next_sync(&self) {
        self.state.lock().unwrap().partial_sync = true;
    }
    pub fn fail_body_reports(&self, fail: bool) {
        self.state.lock().unwrap().fail_reports = fail;
    }
    pub fn delete_remote(&self, path: &str) {
        let mut state = self.state.lock().unwrap();
        state.revision += 1;
        state.resources.remove(path);
    }

    /// Simulate another client; doesn't increment Overlook's observed write count.
    pub fn put_remote(&self, path: &str, body: &str) {
        assert!(path.starts_with('/') && path.ends_with(".ics"));
        let mut state = self.state.lock().unwrap();
        state.revision += 1;
        let etag = format!("\"revision-{}\"", state.revision);
        state.resources.insert(
            path.into(),
            CalDavRemote {
                etag,
                icalendar: body.into(),
            },
        );
    }
}

/// Qualification against an independent server with the same public TLS fixture.
/// Available only in test builds; cannot address a non-loopback service.
pub fn reference_client(address: SocketAddr) -> CalDavClient {
    assert!(address.ip().is_loopback());
    let settings = CalDavSettings {
        server_url: ORIGIN.into(),
        username: "fixture@example.test".into(),
        credential_source: CalendarCredentialSource::Separate,
    };
    let mut client = CalDavClient::new(&settings, "fixture-password".into()).unwrap();
    let tls = ureq::tls::TlsConfig::builder()
        .root_certs(ureq::tls::RootCerts::Specific(
            vec![ureq::tls::Certificate::from_der(CERT)].into(),
        ))
        .build();
    let config = ureq::Agent::config_builder()
        .https_only(true)
        .allow_non_standard_methods(true)
        .max_redirects(0)
        .http_status_as_error(false)
        .proxy(None)
        .timeout_global(Some(Duration::from_secs(3)))
        .tls_config(tls)
        .build();
    client.agent = ureq::Agent::with_parts(
        config,
        ureq::unversioned::transport::DefaultConnector::default(),
        Loopback(address),
    );
    client
}

impl Drop for CalendarServer {
    fn drop(&mut self) {
        self.state.lock().unwrap().stop = true;
        if let Some(worker) = self.worker.take() {
            let result = worker.join();
            if !std::thread::panicking() {
                result.unwrap();
            }
        }
    }
}

#[derive(Debug)]
struct Loopback(SocketAddr);
impl ureq::unversioned::resolver::Resolver for Loopback {
    fn resolve(
        &self,
        _: &ureq::http::Uri,
        _: &ureq::config::Config,
        _: ureq::unversioned::transport::NextTimeout,
    ) -> Result<ureq::unversioned::resolver::ResolvedSocketAddrs, ureq::Error> {
        let mut addresses = self.empty();
        addresses.push(self.0);
        Ok(addresses)
    }
}

fn read_request(stream: &mut impl Read) -> Option<String> {
    let mut bytes = Vec::new();
    let mut buffer = [0; 4096];
    loop {
        let read = stream.read(&mut buffer).ok()?;
        if read == 0 {
            return None;
        }
        bytes.extend_from_slice(&buffer[..read]);
        if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
            let headers = std::str::from_utf8(&bytes[..end]).ok()?;
            let length = header(headers, "content-length")
                .and_then(|n| n.parse::<usize>().ok())
                .unwrap_or(0);
            if bytes.len() >= end + 4 + length {
                return String::from_utf8(bytes).ok();
            }
        }
        assert!(bytes.len() <= 2 * 1024 * 1024);
    }
}
fn header<'a>(headers: &'a str, name: &str) -> Option<&'a str> {
    headers
        .lines()
        .filter_map(|line| line.split_once(':'))
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.trim())
}
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
fn prop(href: &str, value: &str) -> String {
    format!("<d:response><d:href>{}</d:href><d:propstat><d:prop>{value}</d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>", xml_escape(href))
}
fn multistatus(value: &str) -> String {
    format!("<d:multistatus xmlns:d=\"DAV:\" xmlns:c=\"urn:ietf:params:xml:ns:caldav\">{value}</d:multistatus>")
}
fn reply(status: u16, body: &str, etag: Option<&str>) -> String {
    let etag = etag.map(|e| format!("ETag: {e}\r\n")).unwrap_or_default();
    format!(
        "HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\nConnection: close\r\n{etag}\r\n{body}",
        body.len()
    )
}
fn respond(state: &mut State, request: &str) -> Option<String> {
    let (headers, body) = request.split_once("\r\n\r\n").unwrap();
    let mut line = headers.lines().next().unwrap().split_whitespace();
    let (method, path) = (line.next().unwrap(), line.next().unwrap());
    let auth = format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode("fixture@example.test:fixture-password")
    );
    if state.denied || header(headers, "authorization") != Some(auth.as_str()) {
        return Some(reply(401, "", None));
    }
    let response = match method {
        "PROPFIND" if path == "/" => reply(207, &multistatus(&prop("/", &format!("<d:resourcetype><d:collection/><c:calendar/></d:resourcetype><d:displayname>Simulated calendar</d:displayname><d:current-user-privilege-set><d:privilege><d:write/></d:privilege></d:current-user-privilege-set><d:supported-report-set><d:supported-report><d:report><d:sync-collection/></d:report></d:supported-report></d:supported-report-set><d:sync-token>https://synthetic.example/sync/{}</d:sync-token>", state.revision))), None),
        "REPORT" if path == "/" && body.contains("sync-collection") => {
            state.sync_reports += 1;
            if std::mem::take(&mut state.expire_token) { return Some(reply(403, "", None)); }
            let token = format!("https://synthetic.example/sync/{}", state.revision);
            let changes = if std::mem::take(&mut state.partial_sync) {
                "<d:response><d:href>/</d:href><d:status>HTTP/1.1 507 Incomplete</d:status></d:response>"
            } else if body.contains(&format!("<d:sync-token>{token}</d:sync-token>")) { "" } else {
                "<d:response><d:href>/changed.ics</d:href><d:status>HTTP/1.1 404 Gone</d:status></d:response>"
            };
            reply(207, &multistatus(&format!("{changes}<d:sync-token>{token}</d:sync-token>")), None)
        }
        "REPORT" if path == "/" => {
            state.body_reports += 1;
            if state.fail_reports { return Some(reply(503, "", None)); }
            let rows = state.resources.iter().map(|(path, r)| prop(path, &format!("<d:getetag>{}</d:getetag><c:calendar-data>{}</c:calendar-data>", xml_escape(&r.etag), xml_escape(&r.icalendar)))).collect::<String>();
            reply(207, &multistatus(&rows), None)
        }
        "GET" => match state.resources.get(path) {
            Some(r) => reply(200, &r.icalendar, Some(&r.etag)),
            None => reply(404, "", None),
        },
        "PUT" | "DELETE" => {
            let previous = state.resources.get(path);
            let matches = match previous {
                Some(r) => header(headers, "if-match") == Some(r.etag.as_str()),
                None => method == "PUT" && header(headers, "if-none-match") == Some("*"),
            };
            if !matches { return Some(reply(412, "", None)); }
            state.revision += 1;
            state.writes += 1;
            if method == "DELETE" { state.resources.remove(path); } else {
                state.resources.insert(path.into(), CalDavRemote { etag: format!("\"revision-{}\"", state.revision), icalendar: body.into() });
            }
            if std::mem::take(&mut state.lose_reply) { return None; }
            reply(204, "", None)
        }
        _ => reply(405, "", None),
    };
    Some(response)
}
