use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::thread;

use http_body_util::{BodyExt, Full};
use hyper::{body::Bytes, Request, StatusCode};
use ureq::config::Config;
use ureq::unversioned::{
    resolver::{ResolvedSocketAddrs, Resolver},
    transport::{DefaultConnector, NextTimeout},
};

use super::super::{tests::rpc, Bridge, MCP_PATH};
use super::*;

const RESOURCE: &str = "https://mcp.example/mcp";
const ISSUER: &str = "https://synthetic.example";
const GRANT: &str = "test-local-grant";

fn settings(endpoint: &str) -> Settings {
    Settings::TlsProxy {
        resource: RESOURCE.into(),
        issuer: ISSUER.into(),
        introspection_url: endpoint.into(),
        introspection_bearer: "fixture-introspection-credential".into(),
        subject: "fixture-user".into(),
    }
}

fn claims(audience: Value, scope: &str, expiration: u64) -> Value {
    json!({
        "active": true, "iss": ISSUER, "aud": audience, "sub": "fixture-user",
        "exp": expiration, "scope": scope, "token_type": "Bearer",
    })
}

#[test]
fn oauth_checks_resource_subject_issuer_time_and_scope() {
    assert!(Auth::new(settings("http://synthetic.example/introspect"), GRANT).is_err());
    let Auth::OAuth(oauth) =
        Auth::new(settings("https://synthetic.example/introspect"), GRANT).unwrap()
    else {
        unreachable!();
    };
    let valid = claims(json!(RESOURCE), ACCESS_SCOPE, 101);
    let authorize = |claims| oauth.authorize(serde_json::from_value(claims).unwrap(), 100);
    assert!(!authorize(valid.clone()).unwrap().may_write);
    assert!(
        authorize(claims(
            json!([RESOURCE]),
            "overlook:mcp overlook:mcp:write",
            101
        ))
        .unwrap()
        .may_write
    );
    for (field, value) in [
        ("active", json!(false)),
        ("iss", json!("https://other.example")),
        ("sub", json!("different-user")),
        ("aud", json!("https://other.example/mcp")),
        ("aud", json!([RESOURCE, "https://other.example/mcp"])),
        ("aud", Value::Null),
        ("exp", json!(100)),
        ("exp", Value::Null),
        ("nbf", json!(101)),
        ("token_type", json!("DPoP")),
        ("cnf", json!({"jkt": "proof-key"})),
    ] {
        let mut altered = valid.clone();
        altered[field] = value;
        assert!(
            matches!(authorize(altered), Err(Denial::Invalid)),
            "{field}"
        );
    }
    assert!(matches!(
        authorize(claims(
            json!(RESOURCE),
            "overlook:mcp:read overlook:mcp:write",
            101
        )),
        Err(Denial::Scope)
    ));
}

#[test]
fn http_oauth_introspects_over_tls_and_enforces_scopes_before_owner_ipc() {
    // The committed certificate and resolver are fixture-only. Every connection
    // made by this test goes to its own numeric loopback TLS listener.
    let cert = include_bytes!("../../../../tests/fixtures/resource-test-cert.der");
    let key = include_bytes!("../../../../tests/fixtures/resource-test-key.der");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let fixture = thread::spawn(move || {
        let tls = Arc::new(
            rustls::ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(
                    vec![rustls::pki_types::CertificateDer::from(cert.to_vec())],
                    rustls::pki_types::PrivatePkcs8KeyDer::from(key.to_vec()).into(),
                )
                .unwrap(),
        );
        for (token, result) in [
            (
                "wrong-audience",
                claims(
                    json!("https://elsewhere.example/mcp"),
                    ACCESS_SCOPE,
                    now + 60,
                ),
            ),
            ("no-scope", claims(json!(RESOURCE), "unrelated", now + 60)),
            ("read-only", claims(json!(RESOURCE), ACCESS_SCOPE, now + 60)),
            ("read-only", claims(json!(RESOURCE), ACCESS_SCOPE, now + 60)),
        ] {
            let (socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            socket
                .set_write_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let tls = rustls::StreamOwned::new(
                rustls::ServerConnection::new(tls.clone()).unwrap(),
                socket,
            );
            let mut io = BufReader::new(tls);
            let mut first_line = String::new();
            io.read_line(&mut first_line).unwrap();
            assert_eq!(first_line, "POST /introspect HTTP/1.1\r\n");
            let mut length = None;
            let mut auth = None;
            loop {
                let mut line = String::new();
                assert_ne!(io.read_line(&mut line).unwrap(), 0);
                if line == "\r\n" {
                    break;
                }
                let (name, value) = line.split_once(':').unwrap();
                match name.to_ascii_lowercase().as_str() {
                    "content-length" => length = Some(value.trim().parse().unwrap()),
                    "authorization" => auth = Some(value.trim().to_owned()),
                    _ => {}
                }
            }
            assert_eq!(
                auth.as_deref(),
                Some("Bearer fixture-introspection-credential")
            );
            let mut body = vec![0; length.unwrap()];
            io.read_exact(&mut body).unwrap();
            assert_eq!(
                body,
                format!("token={token}&token_type_hint=access_token").as_bytes()
            );
            let result = result.to_string();
            write!(io.get_mut(), "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{result}", result.len()).unwrap();
        }
    });
    let Auth::OAuth(mut oauth) = Auth::new(
        settings(&format!("{ISSUER}:{}/introspect", address.port())),
        GRANT,
    )
    .unwrap() else {
        unreachable!();
    };
    oauth.agent = ureq::Agent::with_parts(
        introspection_config()
            .tls_config(
                ureq::tls::TlsConfig::builder()
                    .root_certs(ureq::tls::RootCerts::Specific(
                        vec![ureq::tls::Certificate::from_der(cert)].into(),
                    ))
                    .build(),
            )
            .build(),
        DefaultConnector::default(),
        FixtureResolver(address),
    );
    let owner_calls = Arc::new(AtomicUsize::new(0));
    let count = owner_calls.clone();
    let bridge = Arc::new(Bridge {
        host: oauth.host.clone(),
        allowed_origins: Vec::new(),
        auth: Auth::OAuth(oauth),
        grant_token: GRANT.into(),
        call: Box::new(move |_| {
            count.fetch_add(1, Ordering::SeqCst);
            Ok(crate::mcp::contract::Response::Accounts { accounts: vec![] })
        }),
        active_calls: Arc::new(tokio::sync::Semaphore::new(1)),
    });
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let post = |token: Option<&str>, tool| {
        let mut request = Request::builder()
            .uri(MCP_PATH)
            .method("POST")
            .header("Host", "mcp.example")
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .header("MCP-Protocol-Version", "2025-11-25");
        if let Some(token) = token {
            request = request.header("Authorization", format!("Bearer {token}"));
        }
        let body = Full::new(Bytes::from(
            rpc(1, "tools/call", json!({"name": tool})).to_string(),
        ));
        rt.block_on(bridge.clone().handle(request.body(body).unwrap()))
    };

    let metadata_request = Request::builder()
        .method("GET")
        .uri(METADATA_PATH)
        .header("Host", "mcp.example")
        .body(Full::<Bytes>::default())
        .unwrap();
    let metadata = rt.block_on(bridge.clone().handle(metadata_request));
    assert_eq!(metadata.status(), StatusCode::OK);
    let bytes = rt
        .block_on(metadata.into_body().collect())
        .unwrap()
        .to_bytes();
    let metadata: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(metadata["authorization_servers"], json!([ISSUER]));
    assert_eq!(metadata["resource"], RESOURCE);

    let missing = post(None, "accounts_list");
    assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);
    assert!(missing.headers()["www-authenticate"]
        .to_str()
        .unwrap()
        .contains("https://mcp.example/.well-known/oauth-protected-resource/mcp"));
    assert_eq!(
        post(Some("wrong-audience"), "accounts_list").status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        post(Some("no-scope"), "accounts_list").status(),
        StatusCode::FORBIDDEN
    );
    let read_only = post(Some("read-only"), "mail_refresh");
    assert_eq!(read_only.status(), StatusCode::FORBIDDEN);
    let challenge = read_only.headers()["www-authenticate"].to_str().unwrap();
    assert!(challenge.contains("insufficient_scope") && challenge.contains(WRITE_SCOPE));
    assert_eq!(
        post(Some("read-only"), "accounts_list").status(),
        StatusCode::OK
    );
    assert_eq!(owner_calls.load(Ordering::SeqCst), 1);
    fixture.join().unwrap();
}

#[derive(Debug)]
struct FixtureResolver(SocketAddr);

impl Resolver for FixtureResolver {
    fn resolve(
        &self,
        _: &ureq::http::Uri,
        _: &Config,
        _: NextTimeout,
    ) -> Result<ResolvedSocketAddrs, ureq::Error> {
        let mut addresses = self.empty();
        addresses.push(self.0);
        Ok(addresses)
    }
}
