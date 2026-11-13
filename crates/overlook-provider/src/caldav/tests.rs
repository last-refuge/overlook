use super::*;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

fn multistatus(responses: &str) -> String {
    format!(
        r#"<d:multistatus xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav" xmlns:a="http://apple.com/ns/ical/">{responses}</d:multistatus>"#
    )
}

fn properties(href: &str, props: &str) -> String {
    format!("<d:response><d:href>{href}</d:href><d:propstat><d:prop>{props}</d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>")
}

fn principal() -> String {
    multistatus(&properties(
        "/",
        "<d:current-user-principal><d:href>/principals/me/</d:href></d:current-user-principal>",
    ))
}

fn homes() -> String {
    multistatus(&properties(
        "/principals/me/",
        "<c:calendar-home-set><d:href>/calendars/me/</d:href></c:calendar-home-set>",
    ))
}

fn collection_props(name: &str) -> String {
    format!("<d:resourcetype><d:collection/><c:calendar/></d:resourcetype><d:displayname>{name}</d:displayname>")
}

fn reply(code: u16, body: &str, extra_headers: &str) -> String {
    format!("HTTP/1.1 {code} Test\r\nContent-Length: {}\r\nContent-Type: application/xml\r\nConnection: close\r\n{extra_headers}\r\n{body}", body.len())
}

struct Server {
    url: Url,
    requests: Arc<Mutex<Vec<String>>>,
    handle: Option<JoinHandle<()>>,
}

impl Server {
    fn new(responses: Vec<String>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let observed = requests.clone();
        let handle = std::thread::spawn(move || {
            for response in responses {
                let until = Instant::now() + Duration::from_secs(5);
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(e)
                            if e.kind() == std::io::ErrorKind::WouldBlock
                                && Instant::now() < until =>
                        {
                            std::thread::sleep(Duration::from_millis(2))
                        }
                        Err(e) => panic!("fixture expected a request: {e}"),
                    }
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                observed.lock().unwrap().push(read_request(&mut stream));
                // Oversized-response tests may deliberately close before consuming the body.
                let _ = stream.write_all(response.as_bytes());
            }
        });
        Self {
            url,
            requests,
            handle: Some(handle),
        }
    }

    fn client(&self) -> CalDavClient {
        // Plaintext transport is available only inside unit tests, never public constructors.
        CalDavClient {
            endpoint: self.url.clone(),
            username: "me@example.test".into(),
            password: "test-app-password".into(),
            agent: agent(false),
        }
    }

    fn finish(mut self) -> Vec<String> {
        self.handle.take().unwrap().join().unwrap();
        self.requests.lock().unwrap().clone()
    }
}

fn read_request(stream: &mut impl Read) -> String {
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 4096];
    loop {
        let read = stream.read(&mut buffer).unwrap();
        assert!(read > 0);
        bytes.extend_from_slice(&buffer[..read]);
        if let Some(header_end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
            let headers = std::str::from_utf8(&bytes[..header_end]).unwrap();
            let length = headers
                .lines()
                .find_map(|line| {
                    let (key, value) = line.split_once(':')?;
                    key.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().unwrap())
                })
                .unwrap_or(0);
            if bytes.len() >= header_end + 4 + length {
                return String::from_utf8(bytes).unwrap();
            }
        }
        assert!(bytes.len() < 32_768);
    }
}

#[test]
fn discovers_principal_home_and_event_calendars_over_http() {
    let editable = format!("{}<d:current-user-privilege-set><d:privilege><d:write/></d:privilege></d:current-user-privilege-set><a:calendar-color>#12ab34ff</a:calendar-color><d:supported-report-set><d:supported-report><d:report><d:sync-collection/></d:report></d:supported-report></d:supported-report-set>", collection_props("Work &amp; family"));
    let tasks = format!("{}<c:supported-calendar-component-set><c:comp name=\"VTODO\"/></c:supported-calendar-component-set>", collection_props("Tasks"));
    let listing = multistatus(&format!(
        "{}{}{}{}",
        properties(
            "/calendars/me/",
            "<d:resourcetype><d:collection/></d:resourcetype>"
        ),
        properties("work%20calendar/", &editable),
        properties("shared/", &collection_props("Shared")),
        properties("tasks/", &tasks)
    ));
    let server = Server::new(vec![
        reply(207, &principal(), ""),
        reply(207, &homes(), ""),
        reply(207, &listing, ""),
    ]);
    let found = server.client().discover().unwrap();
    assert_eq!(
        found.principal_url,
        Some(server.url.join("principals/me/").unwrap().to_string())
    );
    assert_eq!(found.calendars.len(), 2);
    assert_eq!(found.calendars[0].name, "Work & family");
    assert_eq!(found.calendars[0].color.as_deref(), Some("#12ab34ff"));
    assert!(found.calendars[0]
        .url
        .ends_with("/calendars/me/work%20calendar/"));
    assert_eq!(
        found.calendars[0].permissions,
        CalendarPermissions {
            create: true,
            update: true,
            delete: true
        }
    );
    assert!(found.calendars[0].supports_collection_sync);
    assert_eq!(
        found.calendars[1].permissions,
        CalendarPermissions::default()
    );
    let requests = server.finish();
    assert_eq!(requests.len(), 3);
    assert!(requests[0].starts_with("PROPFIND / HTTP/1.1"));
    assert!(requests[1].starts_with("PROPFIND /principals/me/ HTTP/1.1"));
    assert!(requests[2].to_ascii_lowercase().contains("depth: 1\r\n"));
    let expected =
        base64::engine::general_purpose::STANDARD.encode("me@example.test:test-app-password");
    for request in requests {
        assert!(request.contains(&format!("Basic {expected}")));
        assert!(request.ends_with(xml::DISCOVERY_PROPERTIES));
    }
}

#[test]
fn follows_same_origin_redirect_without_changing_propfind() {
    let collection = multistatus(&properties("/calendar/", &collection_props("Personal")));
    let server = Server::new(vec![
        reply(302, "", "Location: /calendar/\r\n"),
        reply(207, &collection, ""),
    ]);
    assert_eq!(server.client().discover().unwrap().calendars.len(), 1);
    let requests = server.finish();
    assert!(requests[1].starts_with("PROPFIND /calendar/ HTTP/1.1"));
    assert!(requests[1].ends_with(xml::DISCOVERY_PROPERTIES));
}

#[test]
fn missing_origin_discovers_calendars_through_well_known_redirect() {
    for status in [404, 405] {
        let service = multistatus(&properties(
            "/dav/calendars",
            "<d:current-user-principal><d:href>/principals/me/</d:href></d:current-user-principal>",
        ));
        let listing = multistatus(&format!(
            "{}{}",
            properties(
                "/calendars/me/",
                "<d:resourcetype><d:collection/></d:resourcetype>"
            ),
            properties("personal/", &collection_props("Personal")),
        ));
        let server = Server::new(vec![
            reply(status, "Not a DAV resource", ""),
            reply(301, "", "Location: /dav/calendars\r\n"),
            reply(207, &service, ""),
            reply(207, &homes(), ""),
            reply(207, &listing, ""),
        ]);
        let found = server.client().discover().unwrap();
        assert_eq!(found.calendars.len(), 1);
        assert_eq!(found.calendars[0].name, "Personal");
        assert_eq!(
            found.calendars[0].url,
            server.url.join("calendars/me/personal/").unwrap().as_str()
        );
        let requests = server.finish();
        assert_eq!(requests.len(), 5);
        assert!(requests[1].starts_with("PROPFIND /.well-known/caldav HTTP/1.1"));
        assert!(requests[2].starts_with("PROPFIND /dav/calendars HTTP/1.1"));
        for request in requests {
            assert!(request.ends_with(xml::DISCOVERY_PROPERTIES));
        }
    }
}

#[test]
fn missing_explicit_endpoint_does_not_discover_a_different_calendar() {
    for path in ["/missing/calendar/", "/.well-known/caldav"] {
        let server = Server::new(vec![reply(404, "", "")]);
        let mut client = server.client();
        client.endpoint = server.url.join(path).unwrap();
        assert_eq!(
            client.discover().unwrap_err(),
            CalDavError::UnexpectedStatus(404)
        );
        assert_eq!(server.finish().len(), 1);
    }
}

#[test]
fn missing_principal_or_home_does_not_restart_discovery() {
    for replies in [
        vec![reply(207, &principal(), ""), reply(404, "", "")],
        vec![
            reply(207, &principal(), ""),
            reply(207, &homes(), ""),
            reply(404, "", ""),
        ],
    ] {
        let count = replies.len();
        let server = Server::new(replies);
        assert_eq!(
            server.client().discover().unwrap_err(),
            CalDavError::UnexpectedStatus(404)
        );
        assert_eq!(server.finish().len(), count);
    }
}

#[test]
fn well_known_failure_stops_and_does_not_expand_credential_origin() {
    for (response, expected) in [
        (
            reply(404, "private response", ""),
            CalDavError::UnexpectedStatus(404),
        ),
        (
            reply(401, "private response", ""),
            CalDavError::AuthenticationRequired,
        ),
        (
            reply(301, "", "Location: https://unapproved.invalid/private\r\n"),
            CalDavError::UnapprovedOrigin("https://unapproved.invalid".into()),
        ),
    ] {
        let server = Server::new(vec![reply(404, "", ""), response]);
        let error = server.client().discover().unwrap_err();
        assert_eq!(error, expected);
        assert!(!error.to_string().contains("private"));
        assert_eq!(server.finish().len(), 2);
    }
}

#[test]
fn cross_origin_redirect_or_discovered_principal_never_receives_credentials() {
    for response in [
        reply(302, "", "Location: https://unapproved.invalid/private\r\n"),
        reply(
            207,
            &multistatus(&properties("/", "<d:current-user-principal><d:href>https://unapproved.invalid/private</d:href></d:current-user-principal>")),
            "",
        ),
    ] {
        let server = Server::new(vec![response]);
        let error = server.client().discover().unwrap_err();
        assert_eq!(error, CalDavError::UnapprovedOrigin("https://unapproved.invalid".into()));
        assert!(!error.to_string().contains("private"));
        assert_eq!(server.finish().len(), 1);
    }
}

#[test]
fn classifies_auth_permission_rate_limit_and_server_errors_without_echoing_body() {
    for (status, expected) in [
        (401, CalDavError::AuthenticationRequired),
        (403, CalDavError::Forbidden),
        (429, CalDavError::RateLimited),
        (503, CalDavError::ServiceUnavailable),
        (200, CalDavError::UnexpectedStatus(200)),
    ] {
        let server = Server::new(vec![reply(status, "sensitive server response", "")]);
        let error = server.client().discover().unwrap_err();
        assert_eq!(error, expected);
        assert!(!error.to_string().contains("sensitive"));
        server.finish();
    }
}

#[test]
fn manually_supplied_home_and_empty_calendar_list_are_valid() {
    let root = multistatus(&properties(
        "/",
        "<d:resourcetype><d:collection/></d:resourcetype>",
    ));
    let server = Server::new(vec![reply(207, &root, ""), reply(207, &root, "")]);
    let found = server.client().discover().unwrap();
    assert!(found.calendars.is_empty());
    assert_eq!(found.home_urls, vec![server.url.to_string()]);
    assert!(server.finish()[1]
        .to_ascii_lowercase()
        .contains("depth: 1\r\n"));
}

#[test]
fn redirect_cycles_stop_and_oversized_responses_fail() {
    let server = Server::new(vec![reply(301, "", "Location: /\r\n")]);
    assert_eq!(
        server.client().discover().unwrap_err(),
        CalDavError::RedirectLimit
    );
    assert_eq!(server.finish().len(), 1);

    let server = Server::new(vec![reply(
        207,
        &"x".repeat(MAX_RESPONSE_BYTES as usize + 1),
        "",
    )]);
    assert!(matches!(
        server.client().discover(),
        Err(CalDavError::InvalidResponse(_))
    ));
    server.finish();
}

#[test]
fn collection_listing_failure_is_not_returned_as_partial_success() {
    let root = multistatus(&properties(
        "/",
        "<c:calendar-home-set><d:href>/one/</d:href><d:href>/two/</d:href></c:calendar-home-set>",
    ));
    let first = multistatus(
        &(properties("/one/", "<d:resourcetype><d:collection/></d:resourcetype>")
            + &properties("/one/calendar/", &collection_props("Personal"))),
    );
    let server = Server::new(vec![
        reply(207, &root, ""),
        reply(207, &first, ""),
        reply(503, "", ""),
    ]);
    assert_eq!(
        server.client().discover().unwrap_err(),
        CalDavError::ServiceUnavailable
    );
    assert_eq!(server.finish().len(), 3);
}

#[test]
fn fastmail_preset_uses_incoming_host_and_requires_explicit_credential_choice() {
    use overlook_core::{Security, ServerEndpoint};
    let mut servers = MailServers {
        incoming: ServerEndpoint::new(
            "IMAP.FASTMAIL.COM.",
            993,
            Security::Tls,
            "owner@custom.example",
        ),
        outgoing: ServerEndpoint::new(
            "smtp.fastmail.com",
            465,
            Security::Tls,
            "owner@custom.example",
        ),
        pop3: None,
        imap: None,
        oauth: None,
    };
    let preset = fastmail_preset(&servers).unwrap();
    assert_eq!(preset.username, "owner@custom.example");
    assert_eq!(preset.server_url, FASTMAIL_CALDAV_URL);
    assert_eq!(preset.credential_source, CalendarCredentialSource::Separate);
    servers.incoming.host = "imap.fastmail.com.attacker.example".into();
    assert!(fastmail_preset(&servers).is_none());
}

#[test]
fn public_client_requires_https_and_does_not_log_secret_or_username() {
    let mut settings = CalDavSettings {
        server_url: FASTMAIL_CALDAV_URL.into(),
        username: "private@example.com".into(),
        credential_source: CalendarCredentialSource::Separate,
    };
    let client = CalDavClient::new(&settings, "private-app-password".into()).unwrap();
    let debug = format!("{client:?}");
    assert!(!debug.contains("private"));
    settings.server_url = "http://127.0.0.1/".into();
    assert!(matches!(
        CalDavClient::new(&settings, "test".into()),
        Err(CalDavError::InvalidConfiguration(_))
    ));
}

#[test]
fn xml_uses_namespace_uris_and_only_successful_properties() {
    let response = r#"<multistatus xmlns="DAV:" xmlns:cal="urn:ietf:params:xml:ns:caldav" xmlns:fake="urn:fake">
    <response><href>/calendar/</href>
      <propstat><prop><resourcetype><cal:calendar/></resourcetype><displayname>Read only</displayname>
        <current-user-privilege-set><privilege><fake:write/></privilege></current-user-privilege-set></prop><status>HTTP/1.1 200 OK</status></propstat>
      <propstat><prop><current-user-privilege-set><privilege><write/></privilege></current-user-privilege-set></prop><status>HTTP/1.1 404 Not Found</status></propstat>
    </response></multistatus>"#;
    let parsed = xml::parse_multistatus(response).unwrap();
    assert!(parsed[0].is_calendar);
    assert_eq!(parsed[0].permissions, CalendarPermissions::default());
}

#[test]
fn xml_rejects_dtd_spoofed_namespaces_partial_failures_and_excessive_nesting() {
    for xml in [
        "<!DOCTYPE d [<!ENTITY x SYSTEM 'file:///etc/passwd'>]><d:multistatus xmlns:d='DAV:'>&x;</d:multistatus>".into(),
        "<multistatus xmlns='urn:fake'/>".into(),
        multistatus("<d:response><d:href>/failed/</d:href><d:status>HTTP/1.1 403 Forbidden</d:status></d:response>"),
        multistatus(&properties("/", "").replace("HTTP/1.1 200 OK", "HTTP/1.1 500 Broken")),
        multistatus(&format!("{}{}", "<d:nested>".repeat(65), "</d:nested>".repeat(65))),
    ] {
        assert!(xml::parse_multistatus(&xml).is_err());
    }
}

#[test]
fn discovery_rejects_unrelated_or_duplicate_root_properties() {
    for body in [
        multistatus(&properties("/unrelated/", &collection_props("Wrong"))),
        multistatus(
            &(properties("/", &collection_props("One"))
                + &properties("/", &collection_props("Two"))),
        ),
    ] {
        let server = Server::new(vec![reply(207, &body, "")]);
        assert!(matches!(
            server.client().discover(),
            Err(CalDavError::InvalidResponse(_))
        ));
        server.finish();
    }
}

#[derive(Debug)]
struct LoopbackResolver(std::net::SocketAddr);

impl ureq::unversioned::resolver::Resolver for LoopbackResolver {
    fn resolve(
        &self,
        _uri: &ureq::http::Uri,
        _config: &ureq::config::Config,
        _timeout: ureq::unversioned::transport::NextTimeout,
    ) -> Result<ureq::unversioned::resolver::ResolvedSocketAddrs, ureq::Error> {
        let mut addresses = self.empty();
        addresses.push(self.0);
        Ok(addresses)
    }
}

#[test]
fn actual_tls_discovery_checks_trust_and_hostname_before_sending_credentials() {
    // Public disposable workspace fixture, documented in overlook-app/tests/fixtures/README.md.
    // Its key and the test trust override are never compiled into the production client.
    const CERT: &[u8] =
        include_bytes!("../../../overlook-app/tests/fixtures/resource-test-cert.der");
    const KEY: &[u8] = include_bytes!("../../../overlook-app/tests/fixtures/resource-test-key.der");
    for (host, trust_fixture, succeeds) in [
        ("synthetic.example", true, true),
        ("synthetic.example", false, false),
        ("wrong.example", true, false),
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let until = Instant::now() + Duration::from_secs(5);
            let socket = loop {
                match listener.accept() {
                    Ok((socket, _)) => break socket,
                    Err(e)
                        if e.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < until =>
                    {
                        std::thread::sleep(Duration::from_millis(2))
                    }
                    Err(e) => panic!("TLS fixture expected a connection: {e}"),
                }
            };
            socket
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let certificate = rustls::pki_types::CertificateDer::from(CERT.to_vec());
            let key = rustls::pki_types::PrivatePkcs8KeyDer::from(KEY.to_vec());
            let config = rustls::ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(vec![certificate], key.into())
                .unwrap();
            let mut stream = rustls::StreamOwned::new(
                rustls::ServerConnection::new(Arc::new(config)).unwrap(),
                socket,
            );
            if succeeds {
                let request = read_request(&mut stream);
                assert!(request
                    .to_ascii_lowercase()
                    .contains("authorization: basic "));
                let body = multistatus(&properties("/", &collection_props("TLS test")));
                stream.write_all(reply(207, &body, "").as_bytes()).unwrap();
                stream.flush().unwrap();
            } else {
                let mut first_application_byte = [0u8; 1];
                assert!(
                    stream.read(&mut first_application_byte).is_err(),
                    "no HTTP credentials may be sent after TLS verification fails"
                );
            }
        });
        let settings = CalDavSettings {
            server_url: format!("https://{host}/"),
            username: "me@example.test".into(),
            credential_source: CalendarCredentialSource::Separate,
        };
        let mut client = CalDavClient::new(&settings, "test-password".into()).unwrap();
        let tls = if trust_fixture {
            ureq::tls::TlsConfig::builder()
                .root_certs(ureq::tls::RootCerts::Specific(
                    vec![ureq::tls::Certificate::from_der(CERT)].into(),
                ))
                .build()
        } else {
            ureq::tls::TlsConfig::default()
        };
        let config = ureq::Agent::config_builder()
            .https_only(true)
            .allow_non_standard_methods(true)
            .max_redirects(0)
            .http_status_as_error(false)
            .proxy(None)
            .tls_config(tls)
            .build();
        client.agent = ureq::Agent::with_parts(
            config,
            ureq::unversioned::transport::DefaultConnector::default(),
            LoopbackResolver(address),
        );
        let result = client.discover();
        if succeeds {
            assert_eq!(result.unwrap().calendars[0].name, "TLS test");
        } else {
            assert_eq!(result.unwrap_err(), CalDavError::Network);
        }
        server.join().unwrap();
    }
}

fn report(href: &str, etag: &str, data: &str) -> String {
    let escape = |s: &str| {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    };
    multistatus(&properties(
        href,
        &format!(
            "<d:getetag>{}</d:getetag><c:calendar-data>{}</c:calendar-data>",
            escape(etag),
            escape(data)
        ),
    ))
}

fn snapshot_account() -> overlook_core::Account {
    use overlook_core::*;
    Account {
        id: AccountId::from("imap:fixture"),
        provider: ProviderKind::Imap,
        display_name: "Fixture".into(),
        email: "me@example.test".into(),
        created_at: 0,
    }
}

fn snapshot_times() -> (i64, i64) {
    use overlook_core::tz;
    (
        tz::parse_rfc3339("2026-10-01T00:00:00Z").unwrap(),
        tz::parse_rfc3339("2026-11-01T00:00:00Z").unwrap(),
    )
}

const EXPANDED: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:repeating\r\nDTSTART:20261022T160000Z\r\nDTEND:20261022T170000Z\r\nSUMMARY:First\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:repeating\r\nRECURRENCE-ID:20261023T160000Z\r\nDTSTART:20261023T180000Z\r\nDTEND:20261023T190000Z\r\nSUMMARY:Moved occurrence\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

#[test]
fn snapshot_retains_original_resource_and_maps_expanded_occurrences() {
    let original = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:repeating\r\nDTSTART:20261022T160000Z\r\nRRULE:FREQ=DAILY;COUNT=2\r\nX-PRIVATE:Keep me & my unknown fields\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    let server = Server::new(vec![
        reply(
            207,
            &multistatus(&properties("/", &collection_props("Personal"))),
            "",
        ),
        reply(207, &report("/event.ics", "\"v1\"", original), ""),
        reply(207, &report("/event.ics", "\"v1\"", EXPANDED), ""),
    ]);
    let (from, to) = snapshot_times();
    let snapshot = server
        .client()
        .snapshot(
            &snapshot_account(),
            &overlook_core::CalendarConnectionId::from("fixture"),
            from,
            to,
            || true,
        )
        .unwrap();
    let collection = &snapshot.calendars[0];
    assert!(!collection.calendar.can_edit);
    assert_eq!(
        collection.resources[0].icalendar,
        original.replace("\r\n", "\n")
    ); // XML line-ending normalization.
    let events = &collection.resources[0].events;
    assert_eq!(events.len(), 2);
    assert_ne!(events[0].id, events[1].id);
    assert_eq!(events[1].summary, "Moved occurrence");
    let requests = server.finish();
    assert!(requests[1].starts_with("REPORT / HTTP/1.1"));
    assert!(!requests[1].contains("<c:expand"));
    assert!(requests[2].contains("<c:expand"));
    assert!(requests[2]
        .to_ascii_lowercase()
        .contains("authorization: basic"));
}

#[test]
fn snapshot_rejects_etag_races_missing_resources_and_cross_origin_hrefs() {
    for second in [
        report("/event.ics", "\"v2\"", EXPANDED),
        multistatus(""),
        report("https://unapproved.invalid/event.ics", "\"v1\"", EXPANDED),
    ] {
        let server = Server::new(vec![
            reply(
                207,
                &multistatus(&properties("/", &collection_props("Personal"))),
                "",
            ),
            reply(207, &report("/event.ics", "\"v1\"", EXPANDED), ""),
            reply(207, &second, ""),
        ]);
        let (from, to) = snapshot_times();
        assert!(server
            .client()
            .snapshot(
                &snapshot_account(),
                &overlook_core::CalendarConnectionId::from("fixture"),
                from,
                to,
                || true
            )
            .is_err());
        assert_eq!(server.finish().len(), 3);
    }
}

#[test]
fn snapshot_accepts_complete_empty_reports_and_stops_after_cancellation() {
    let discovery = reply(
        207,
        &multistatus(&properties("/", &collection_props("Personal"))),
        "",
    );
    let server = Server::new(vec![
        discovery.clone(),
        reply(207, &multistatus(""), ""),
        reply(207, &multistatus(""), ""),
    ]);
    let (from, to) = snapshot_times();
    let connection = overlook_core::CalendarConnectionId::from("fixture");
    let found = server
        .client()
        .snapshot(&snapshot_account(), &connection, from, to, || true)
        .unwrap();
    assert!(found.calendars[0].resources.is_empty());
    server.finish();
    let server = Server::new(vec![discovery]);
    assert!(server
        .client()
        .snapshot(&snapshot_account(), &connection, from, to, || false)
        .is_err());
    assert_eq!(server.finish().len(), 1);
}

#[test]
fn discovery_requires_a_complete_home_listing_before_cache_replacement() {
    let home = multistatus(&properties(
        "/",
        "<d:resourcetype><d:collection/></d:resourcetype>",
    ));
    for listing in [
        multistatus(""),
        multistatus(&properties(
            "/",
            "<d:displayname>Unknown type</d:displayname>",
        )),
    ] {
        let server = Server::new(vec![reply(207, &home, ""), reply(207, &listing, "")]);
        assert!(server.client().discover().is_err());
        server.finish();
    }
}

fn write_event() -> overlook_core::Event {
    use overlook_core::*;
    Event {
        id: EventId::from("caldav:test:event"),
        account_id: AccountId::from("imap:test"),
        calendar_id: CalendarId::from("caldav:test"),
        provider_id: None,
        ical_uid: Some("event-1@example.test".into()),
        summary: "Original".into(),
        description: None,
        location: None,
        start: tz::parse_date("2026-10-22").map(tz::date_start).unwrap() + 12 * 3_600_000,
        end: tz::parse_date("2026-10-22").map(tz::date_start).unwrap() + 13 * 3_600_000,
        all_day: false,
        status: EventStatus::Confirmed,
        organizer: None,
        attendees: vec![],
        html_link: None,
        conference_url: None,
        recurring_event_id: None,
        updated: 0,
        event_type: None,
        color: None,
        reminders: Some(vec![]),
        original_start: None,
        wants_conference: false,
        pending: false,
    }
}
fn write_body() -> String {
    prepare_resource(
        None,
        &[overlook_core::CalDavEdit {
            before: None,
            after: Some(write_event()),
            scope: overlook_core::RecurrenceScope::This,
        }],
        "me@example.test",
    )
    .unwrap()
    .unwrap()
}
fn write_change(
    url: String,
    base: Option<overlook_core::CalDavRemote>,
    desired: Option<String>,
) -> overlook_core::CalDavChange {
    use overlook_core::*;
    CalDavChange {
        id: OperationId::generate(),
        connection_id: CalendarConnectionId::generate(),
        generation: 1,
        account_id: AccountId::from("imap:test"),
        calendar_id: CalendarId::from("caldav:test"),
        href: url,
        revision: 1,
        state: CalDavChangeState::Checking,
        base,
        desired,
        edits: vec![],
        remote: None,
        note: None,
        timezone: "test".into(),
    }
}

#[test]
fn edits_retain_unknown_properties_alarms_timezones_and_fold_utf8_safely() {
    use overlook_core::*;
    let before = write_event();
    let mut after = before.clone();
    after.summary = "Ridge 🌄, planning; notes\nNext line".repeat(12);
    let raw=write_body().replace("END:VEVENT", "X-PRIVATE;P=\"a:b\":keep this\r\n folded value\r\nBEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER:-PT10M\r\nDESCRIPTION:Reminder\r\nEND:VALARM\r\nEND:VEVENT").replace("END:VCALENDAR", "BEGIN:VTIMEZONE\r\nTZID:Custom/Zone\r\nX-CUSTOM:keep\r\nEND:VTIMEZONE\r\nEND:VCALENDAR");
    let result = prepare_resource(
        Some(&raw),
        &[CalDavEdit {
            before: Some(before),
            after: Some(after.clone()),
            scope: RecurrenceScope::This,
        }],
        "me@example.test",
    )
    .unwrap()
    .unwrap();
    assert!(result.contains("X-PRIVATE;P=\"a:b\":keep this\r\n folded value"));
    assert!(result.contains("BEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER:-PT10M"));
    assert!(result.contains("TZID:Custom/Zone\r\nX-CUSTOM:keep"));
    assert!(result.lines().all(|l| l.len() <= 75));
    let parsed = ical::IcalParser::new(result.as_bytes())
        .next()
        .unwrap()
        .unwrap();
    let title = parsed.events[0]
        .properties
        .iter()
        .find(|p| p.name == "SUMMARY")
        .unwrap()
        .value
        .as_ref()
        .unwrap();
    assert!(title.contains("🌄\\, planning\\; notes\\nNext line"));
    assert!(!result.contains("ATTENDEE"));
}

#[test]
fn recurring_occurrence_edit_and_cancel_preserve_master_and_other_exceptions() {
    use overlook_core::*;
    let before = write_event();
    let mut after = before.clone();
    after.summary = "Moved occurrence".into();
    after.start += 3_600_000;
    after.end += 3_600_000;
    let raw = write_body().replace(
        "END:VEVENT",
        "RRULE:FREQ=DAILY;COUNT=10\r\nX-SERIES:keep\r\nEND:VEVENT",
    );
    let result = prepare_resource(
        Some(&raw),
        &[CalDavEdit {
            before: Some(before.clone()),
            after: Some(after),
            scope: RecurrenceScope::This,
        }],
        "me@example.test",
    )
    .unwrap()
    .unwrap();
    assert_eq!(result.matches("RRULE:").count(), 1);
    assert_eq!(result.matches("BEGIN:VEVENT").count(), 2);
    assert!(result.contains(&format!("RECURRENCE-ID:{}", tz::utc_basic(before.start))));
    let cancelled = prepare_resource(
        Some(&result),
        &[CalDavEdit {
            before: Some(before.clone()),
            after: None,
            scope: RecurrenceScope::This,
        }],
        "me@example.test",
    )
    .unwrap()
    .unwrap();
    assert_eq!(cancelled.matches("BEGIN:VEVENT").count(), 2);
    assert!(cancelled.contains("STATUS:CANCELLED"));
    assert!(prepare_resource(
        Some(&result),
        &[CalDavEdit {
            before: Some(before),
            after: None,
            scope: RecurrenceScope::All
        }],
        "me@example.test"
    )
    .unwrap()
    .is_none());
}

#[test]
fn conflict_rebase_changes_only_locally_edited_fields_and_rejects_meetings() {
    use overlook_core::*;
    let before = write_event();
    let mut after = before.clone();
    after.summary = "Local title".into();
    let remote = write_body().replace(
        "END:VEVENT",
        "LOCATION:Changed remotely\r\nX-REMOTE:keep\r\nEND:VEVENT",
    );
    let edit = CalDavEdit {
        before: Some(before.clone()),
        after: Some(after),
        scope: RecurrenceScope::This,
    };
    let merged = prepare_resource(
        Some(&remote),
        std::slice::from_ref(&edit),
        "me@example.test",
    )
    .unwrap()
    .unwrap();
    assert!(merged.contains("SUMMARY:Local title"));
    assert!(merged.contains("LOCATION:Changed remotely"));
    assert!(merged.contains("X-REMOTE:keep"));
    let meeting = remote.replace(
        "END:VEVENT",
        "ATTENDEE:mailto:other@example.test\r\nEND:VEVENT",
    );
    assert!(prepare_resource(Some(&meeting), &[edit], "me@example.test").is_err());
    let mut all_day = before;
    all_day.all_day = true;
    all_day.start = tz::date_start(tz::parse_date("2026-10-22").unwrap());
    all_day.end = tz::add_days(all_day.start, 3);
    let date = prepare_resource(
        None,
        &[CalDavEdit {
            before: None,
            after: Some(all_day),
            scope: RecurrenceScope::This,
        }],
        "me@example.test",
    )
    .unwrap()
    .unwrap();
    assert!(date.contains("DTSTART;VALUE=DATE:20261022"));
    assert!(date.contains("DTEND;VALUE=DATE:20261025"));
}

#[test]
fn conditional_create_then_lost_response_reconciliation_never_duplicates() {
    let body = write_body();
    let server = Server::new(vec![
        reply(404, "", ""),
        reply(201, "", "ETag: \"new\"\r\n"),
        reply(200, &body, "ETag: \"new\"\r\n"),
        reply(200, &body, "ETag: \"new\"\r\n"),
    ]);
    let change = write_change(
        server.url.join("event.ics").unwrap().to_string(),
        None,
        Some(body.clone()),
    );
    assert!(matches!(
        server.client().reconcile_change(&change, || true).unwrap(),
        CalDavWriteResult::Applied(Some(_))
    ));
    // Same durable attempt replayed after a simulated crash before local acknowledgment.
    assert!(matches!(
        server.client().reconcile_change(&change, || true).unwrap(),
        CalDavWriteResult::Applied(Some(_))
    ));
    let requests = server.finish();
    assert_eq!(requests.iter().filter(|r| r.starts_with("PUT ")).count(), 1);
    assert!(requests[1].to_lowercase().contains("if-none-match: *"));
    assert!(requests[1]
        .to_lowercase()
        .contains("content-type: text/calendar"));
}

#[test]
fn conditional_update_delete_races_and_missing_resources_are_explicit() {
    use overlook_core::*;
    let body = write_body();
    let changed = body.replace("SUMMARY:Original", "SUMMARY:Local");
    let server = Server::new(vec![
        reply(200, &body, "ETag: \"one\"\r\n"),
        reply(412, "", ""),
        reply(
            200,
            &body.replace("Original", "Remote"),
            "ETag: \"two\"\r\n",
        ),
    ]);
    let change = write_change(
        server.url.join("event.ics").unwrap().to_string(),
        Some(CalDavRemote {
            etag: "\"one\"".into(),
            icalendar: body.clone(),
        }),
        Some(changed),
    );
    assert!(matches!(
        server.client().reconcile_change(&change, || true).unwrap(),
        CalDavWriteResult::Conflict(Some(_))
    ));
    let requests = server.finish();
    assert!(requests[1].to_lowercase().contains("if-match: \"one\""));
    let server = Server::new(vec![
        reply(200, &body, "ETag: \"one\"\r\n"),
        reply(204, "", ""),
        reply(404, "", ""),
    ]);
    let change = write_change(
        server.url.join("event.ics").unwrap().to_string(),
        Some(CalDavRemote {
            etag: "\"one\"".into(),
            icalendar: body,
        }),
        None,
    );
    assert_eq!(
        server.client().reconcile_change(&change, || true).unwrap(),
        CalDavWriteResult::Applied(None)
    );
    assert_eq!(
        server.client().reconcile_change(&change, || true).unwrap(),
        CalDavWriteResult::Applied(None)
    );
    let requests = server.finish();
    assert_eq!(
        requests.iter().filter(|r| r.starts_with("DELETE ")).count(),
        1
    );
}

#[test]
fn writes_reject_weak_etags_redirects_cross_origin_and_cancelled_work() {
    let body = write_body();
    for response in [
        reply(200, &body, "ETag: W/\"weak\"\r\n"),
        reply(302, "", "Location: https://elsewhere.test/private\r\n"),
    ] {
        let server = Server::new(vec![response]);
        let change = write_change(
            server.url.join("event.ics").unwrap().to_string(),
            None,
            Some(body.clone()),
        );
        assert!(server.client().reconcile_change(&change, || true).is_err());
        assert_eq!(server.finish().len(), 1);
    }
    let server = Server::new(vec![]);
    let change = write_change("https://other.example/event.ics".into(), None, Some(body));
    assert!(server.client().reconcile_change(&change, || true).is_err());
    assert!(server.client().reconcile_change(&change, || false).is_err());
    assert!(server.finish().is_empty());
}

#[test]
fn failed_preflight_allows_offline_coalescing_but_lost_mutation_response_is_uncertain() {
    let body = write_body();
    let server = Server::new(vec![String::new()]);
    let change = write_change(
        server.url.join("event.ics").unwrap().to_string(),
        None,
        Some(body.clone()),
    );
    let failure = server
        .client()
        .reconcile_change(&change, || true)
        .unwrap_err();
    assert!(!failure.may_have_written);
    assert_eq!(server.finish().len(), 1);

    let server = Server::new(vec![
        reply(404, "", ""),
        String::new(),
        reply(200, &body, "ETag: \"saved\"\r\n"),
    ]);
    let change = write_change(
        server.url.join("event.ics").unwrap().to_string(),
        None,
        Some(body),
    );
    let failure = server
        .client()
        .reconcile_change(&change, || true)
        .unwrap_err();
    assert!(failure.may_have_written);
    assert!(matches!(
        server.client().reconcile_change(&change, || true).unwrap(),
        CalDavWriteResult::Applied(Some(_))
    ));
    assert_eq!(
        server
            .finish()
            .iter()
            .filter(|r| r.starts_with("PUT "))
            .count(),
        1
    );
}

#[test]
fn preflight_detects_remote_changes_or_deletion_without_mutating() {
    use overlook_core::*;
    let body = write_body();
    for response in [
        reply(
            200,
            &body.replace("Original", "Remote"),
            "ETag: \"two\"\r\n",
        ),
        reply(404, "", ""),
    ] {
        let server = Server::new(vec![response]);
        let change = write_change(
            server.url.join("event.ics").unwrap().to_string(),
            Some(CalDavRemote {
                etag: "\"one\"".into(),
                icalendar: body.clone(),
            }),
            Some(body.replace("Original", "Local")),
        );
        assert!(matches!(
            server.client().reconcile_change(&change, || true).unwrap(),
            CalDavWriteResult::Conflict(_)
        ));
        let requests = server.finish();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].starts_with("GET "));
    }
}

#[test]
fn title_only_edit_targets_later_occurrence_and_changed_series_schedule_needs_refresh() {
    use overlook_core::*;
    let raw = write_body().replace("END:VEVENT", "RRULE:FREQ=DAILY;COUNT=10\r\nEND:VEVENT");
    let mut before = write_event();
    before.start += 2 * 86_400_000;
    before.end += 2 * 86_400_000;
    before.original_start = Some(before.start);
    let mut after = before.clone();
    after.summary = "Later occurrence".into();
    let edit = CalDavEdit {
        before: Some(before.clone()),
        after: Some(after),
        scope: RecurrenceScope::This,
    };
    let result = prepare_resource(Some(&raw), std::slice::from_ref(&edit), "me@example.test")
        .unwrap()
        .unwrap();
    assert!(result.contains(&format!("DTSTART:{}", tz::utc_basic(before.start))));
    assert!(result.contains(&format!("DTEND:{}", tz::utc_basic(before.end))));
    assert!(result.contains(&format!("RECURRENCE-ID:{}", tz::utc_basic(before.start))));
    let remote = raw.replace("COUNT=10", "COUNT=9");
    assert!(prepare_conflict_resource(
        Some(&raw),
        &remote,
        std::slice::from_ref(&edit),
        "me@example.test"
    )
    .is_err());
    let remote = raw.replace("SUMMARY:Original", "SUMMARY:Remote");
    assert!(prepare_conflict_resource(Some(&raw), &remote, &[edit], "me@example.test").is_ok());
}
