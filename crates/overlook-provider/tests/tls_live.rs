//! Live TLS evidence (CASA 4.1.1 / 4.1.2): drives Overlook's real connection code against
//! badssl.com's deliberately broken servers and reports what the client negotiates with
//! howsmyssl.com. Needs the network, so it is ignored by default:
//!
//!     cargo test -p overlook-provider --test tls_live -- --ignored --nocapture --test-threads=1
//!
//! The rustls reason for each refusal is logged under `overlook::diag` and printed here.

use overlook_core::{Security, ServerEndpoint};
use overlook_provider::imap::connection as imap;
use overlook_provider::pop3::client::Client as Pop3;
use overlook_provider::smtp::Transport;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(20);

/// (host, port, what is wrong with it, should a correct client connect?)
const TARGETS: &[(&str, u16, &str, bool)] = &[
    ("badssl.com", 443, "valid certificate (control)", true),
    ("expired.badssl.com", 443, "expired certificate", false),
    ("wrong.host.badssl.com", 443, "certificate for another host name", false),
    ("self-signed.badssl.com", 443, "self-signed certificate", false),
    ("untrusted-root.badssl.com", 443, "certificate from an untrusted root", false),
    ("incomplete-chain.badssl.com", 443, "missing intermediate certificate", false),
    ("tls-v1-0.badssl.com", 1010, "TLS 1.0 only", false),
    ("tls-v1-1.badssl.com", 1011, "TLS 1.1 only", false),
    ("rc4.badssl.com", 443, "RC4 cipher only", false),
    ("3des.badssl.com", 443, "3DES cipher only", false),
    ("null.badssl.com", 443, "NULL cipher only", false),
    ("dh1024.badssl.com", 443, "1024-bit DH only", false),
    ("revoked.badssl.com", 443, "revoked certificate (no revocation checking; disclosed limitation)", true),
];

fn diag_logging() {
    let _ = tracing_subscriber::fmt().with_env_filter("overlook::diag=debug").with_target(false).without_time().try_init();
}

fn verdict(connected: bool, expect: bool) -> &'static str {
    match (connected, expect) {
        (true, true) => "connected (expected)",
        (false, false) => "REFUSED (expected)",
        (true, false) => "CONNECTED — UNEXPECTED",
        (false, true) => "refused — unexpected",
    }
}

/// The HTTPS stack used for Google sign-in, the Gmail/Calendar/People APIs, CalDAV,
/// autoconfiguration and updates: ureq 3 with its default rustls configuration, configured
/// exactly as `GmailClient::new` does.
fn https_agent() -> ureq::Agent {
    ureq::Agent::config_builder().http_status_as_error(false).https_only(true).max_redirects(0).timeout_global(Some(TIMEOUT)).build().into()
}

#[test]
#[ignore = "needs the network"]
fn https_client_refuses_bad_servers() {
    diag_logging();
    let agent = https_agent();
    let mut failures = Vec::new();
    println!("\nHTTPS stack (ureq + rustls)");
    for &(host, port, what, expect) in TARGETS {
        let result = agent.get(&format!("https://{host}:{port}/")).call();
        let connected = result.is_ok();
        let note = match &result {
            Ok(r) => format!("HTTP {}", r.status()),
            Err(e) => overlook_provider::secure_error::ureq_message(e).to_owned(),
        };
        println!("  {host}:{port:<5} {what:<70} {} — {note}", verdict(connected, expect));
        if connected != expect {
            failures.push(host);
        }
    }
    assert!(failures.is_empty(), "unexpected results: {failures:?}");
}

#[test]
#[ignore = "needs the network"]
fn mail_clients_refuse_bad_servers() {
    diag_logging();
    let mut failures = Vec::new();
    for (stack, connect) in [
        ("IMAP (rustls)", (|ep: &ServerEndpoint| imap::connect(ep, "unused", TIMEOUT).map(|_| ())) as fn(&ServerEndpoint) -> Result<(), overlook_provider::ProviderError>),
        ("POP3 (rustls)", |ep| Pop3::connect(ep, "unused", TIMEOUT).map(|_| ())),
        ("SMTP (lettre + rustls)", |ep| Transport::new(ep.clone(), "unused").with_timeout(TIMEOUT).probe()),
    ] {
        println!("\n{stack}, implicit TLS");
        for &(host, port, what, expect) in TARGETS {
            let ep = ServerEndpoint::new(host, port, Security::Tls, "nobody@example.com");
            let result = connect(&ep);
            // badssl.com speaks HTTP, not a mail protocol: getting past the TLS handshake shows up
            // as a protocol error after the handshake, which counts as "connected" here.
            let tls_ok = match &result {
                Ok(()) => true,
                Err(e) => {
                    let text = e.to_string();
                    !(text.contains(overlook_provider::secure_error::CERTIFICATE_NOT_TRUSTED) || text.contains(overlook_provider::secure_error::SECURE_CONNECTION_FAILED))
                }
            };
            let note = match &result {
                Ok(()) => "ok".to_owned(),
                Err(_) if tls_ok => "TLS accepted, then no mail greeting (badssl.com is a web server)".to_owned(),
                Err(e) => e.to_string(),
            };
            println!("  {host}:{port:<5} {what:<70} {} — {note}", verdict(tls_ok, expect));
            if tls_ok != expect {
                failures.push(format!("{stack} {host}"));
            }
        }
    }
    assert!(failures.is_empty(), "unexpected results: {failures:?}");
}

#[test]
#[ignore = "needs the network"]
fn negotiated_parameters_from_howsmyssl() {
    let mut response = https_agent().get("https://www.howsmyssl.com/a/check").call().expect("howsmyssl reachable");
    let report: serde_json::Value = response.body_mut().read_json().expect("json");
    println!("\nhowsmyssl.com report for the HTTPS stack:\n{}", serde_json::to_string_pretty(&report).unwrap());
    // "Bad" means an insecure suite, TLS compression or similar. "Improvable" is given only for
    // the absence of post-quantum key agreement, which rustls offers with aws-lc-rs, not ring.
    assert_ne!(report["rating"], "Bad");
    assert_eq!(report["tls_version"], "TLS 1.3");
    assert_eq!(report["insecure_cipher_suites"].as_object().map_or(0, |m| m.len()), 0);
    assert_eq!(report["tls_compression_supported"], false);
    assert_eq!(report["beast_vuln"], false);
}
