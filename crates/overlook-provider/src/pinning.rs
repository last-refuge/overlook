//! Trusting the certificate of a mail server on this computer.
//!
//! Proton Mail Bridge, and similar local bridges, encrypt their loopback connections with a
//! certificate they made themselves. No public authority vouches for it, so ordinary
//! verification refuses it, and Proton asks mail apps to have the person confirm it once.
//! Overlook does the same:
//!
//! - **The pin.** [`ServerEndpoint::pinned_certificate`] holds the SHA-256 fingerprint of
//!   the exact certificate the person trusted in the setup dialog, after
//!   [`fetch_certificate`] read it.
//! - **This computer only.** [`pin_for`] honours a pin only for an encrypted connection to a
//!   [`ServerEndpoint::is_local`] host, and the connection is refused unless the address it
//!   reached is a loopback one. Any other server is verified against the public roots as
//!   always, pin or not, so nothing here can weaken a connection that leaves the computer.
//! - **IMAP and POP3.** [`client_config`] gives rustls a verifier that accepts exactly the
//!   pinned certificate. It still checks the handshake signature with that certificate's
//!   key, so a server has to hold the private key, not just present the certificate.
//! - **SMTP.** lettre takes no custom verifier. For a pinned local server the transport lets
//!   lettre skip the chain and name checks (it still verifies the handshake signature), then
//!   compares the certificate it received with the pin before signing in (see
//!   `smtp::Transport`).
//!
//! A certificate that no longer matches its pin (Bridge reinstalled, or another program on
//! the port) is refused with [`crate::secure_error::CERTIFICATE_CHANGED`], before any
//! credentials are sent, and the setup dialog offers the new one for the person to trust or
//! not.

use crate::imap::connection::{starttls_by_hand, tcp_connect};
use crate::secure_error::{self, CERTIFICATE_CHANGED, CERTIFICATE_NOT_TRUSTED};
use crate::ProviderError;
use overlook_core::{Security, ServerEndpoint, ServerProtocol};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{verify_tls12_signature, verify_tls13_signature, CryptoProvider};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{CertificateError, DigitallySignedStruct, SignatureScheme};
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// A pinned connection reached something other than this computer.
const NOT_LOCAL: &str = "a certificate you trusted is only used for a server on this computer";

/// `AB:CD:…`: the SHA-256 of a certificate's DER, as `openssl x509 -fingerprint -sha256`
/// prints it.
pub fn fingerprint(der: &[u8]) -> String {
    crate::oauth::sha256(der).iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(":")
}

/// The 32 bytes of a fingerprint, in either case and with or without colons.
fn digest(fingerprint: &str) -> Option<[u8; 32]> {
    let hex: Vec<u8> = fingerprint.bytes().filter(|b| *b != b':').collect();
    if hex.len() != 64 || !hex.iter().all(u8::is_ascii_hexdigit) {
        return None;
    }
    let mut out = [0u8; 32];
    for (byte, pair) in out.iter_mut().zip(hex.chunks(2)) {
        *byte = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
    }
    Some(out)
}

/// Whether `der` is the certificate `pin` names.
pub fn matches(der: &[u8], pin: &str) -> bool {
    digest(pin).is_some_and(|expected| crate::oauth::sha256(der) == expected)
}

/// Whether two fingerprints name the same certificate, however they are written.
pub fn same_fingerprint(a: &str, b: &str) -> bool {
    matches!((digest(a), digest(b)), (Some(a), Some(b)) if a == b)
}

/// The pin to honour for `endpoint`: its own, and only for an encrypted connection to a
/// server on this computer.
pub fn pin_for(endpoint: &ServerEndpoint) -> Option<&str> {
    endpoint.pinned_certificate.as_deref().filter(|pin| endpoint.is_local() && endpoint.security != Security::None && digest(pin).is_some())
}

/// Whether a failure is a certificate the person could choose to trust: the server is on
/// this computer, and its certificate isn't publicly trusted or isn't the one trusted before.
/// Only the exact refusals the connections make count, so a server's own words can't pose
/// as one.
pub fn is_trust_question(endpoint: &ServerEndpoint, error: &ProviderError) -> bool {
    let ProviderError::Rejected(message) = error else { return false };
    let host = endpoint.host.trim();
    let refusal = |fixed: &str| message == fixed || *message == crate::imap::connection::handshake_failed(host, fixed);
    endpoint.is_local() && endpoint.security != Security::None && (refusal(CERTIFICATE_NOT_TRUSTED) || refusal(CERTIFICATE_CHANGED))
}

/// Refuses a pinned connection that reached anything but a loopback address, in case a
/// local-looking name resolves elsewhere.
pub(crate) fn require_loopback(tcp: &TcpStream) -> Result<(), ProviderError> {
    match tcp.peer_addr() {
        Ok(addr) if addr.ip().to_canonical().is_loopback() => Ok(()),
        Ok(_) => Err(ProviderError::Rejected(NOT_LOCAL.into())),
        Err(_) => Err(ProviderError::Offline),
    }
}

/// The loopback addresses an endpoint's host resolves to, in order, for clients that
/// connect by address; refused when it resolves to any other.
pub(crate) fn loopback_addresses(endpoint: &ServerEndpoint) -> Result<Vec<SocketAddr>, ProviderError> {
    let host = endpoint.host.trim().trim_start_matches('[').trim_end_matches(']');
    let addrs: Vec<SocketAddr> = (host, endpoint.port).to_socket_addrs().map_err(|_| ProviderError::Offline)?.collect();
    if addrs.is_empty() {
        return Err(ProviderError::Offline);
    }
    if !addrs.iter().all(|a| a.ip().to_canonical().is_loopback()) {
        return Err(ProviderError::Rejected(NOT_LOCAL.into()));
    }
    Ok(addrs)
}

/// Whether a handshake failed under [`client_config`] because the certificate wasn't the
/// pinned one. Only the pinned verifier reports this error.
pub(crate) fn is_mismatch_io(e: &std::io::Error) -> bool {
    e.get_ref().and_then(|inner| inner.downcast_ref::<rustls::Error>()).is_some_and(|e| matches!(e, rustls::Error::InvalidCertificate(CertificateError::ApplicationVerificationFailure)))
}

/// Accepts exactly the pinned certificate; the handshake signature is still verified.
#[derive(Debug)]
struct PinnedVerifier {
    pin: [u8; 32],
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for PinnedVerifier {
    fn verify_server_cert(&self, end_entity: &CertificateDer<'_>, _intermediates: &[CertificateDer<'_>], _server_name: &ServerName<'_>, _ocsp_response: &[u8], _now: UnixTime) -> Result<ServerCertVerified, rustls::Error> {
        if crate::oauth::sha256(end_entity) == self.pin {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::InvalidCertificate(CertificateError::ApplicationVerificationFailure))
        }
    }

    fn verify_tls12_signature(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn verify_tls13_signature(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

fn config_with(verifier: Arc<dyn ServerCertVerifier>, provider: Arc<CryptoProvider>) -> Result<Arc<rustls::ClientConfig>, ProviderError> {
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| {
            secure_error::log_detail("TLS setup", &e);
            ProviderError::Rejected(secure_error::rustls_message(&e).into())
        })?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    Ok(Arc::new(config))
}

/// A rustls configuration that trusts only the pinned certificate.
pub(crate) fn client_config(pin: &str) -> Result<Arc<rustls::ClientConfig>, ProviderError> {
    let pin = digest(pin).ok_or_else(|| ProviderError::Rejected(CERTIFICATE_CHANGED.into()))?;
    let provider = provider();
    config_with(Arc::new(PinnedVerifier { pin, provider: provider.clone() }), provider)
}

/// Accepts any certificate after checking the handshake signature, and keeps it: used only
/// to read a local server's certificate so the person can decide whether to trust it. No
/// credentials are ever sent over a connection it accepted.
#[derive(Debug)]
struct RecordingVerifier {
    seen: Mutex<Option<Vec<u8>>>,
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for RecordingVerifier {
    fn verify_server_cert(&self, end_entity: &CertificateDer<'_>, _intermediates: &[CertificateDer<'_>], _server_name: &ServerName<'_>, _ocsp_response: &[u8], _now: UnixTime) -> Result<ServerCertVerified, rustls::Error> {
        *self.seen.lock().unwrap_or_else(|p| p.into_inner()) = Some(end_entity.as_ref().to_vec());
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn verify_tls13_signature(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

/// Reads the certificate a server on this computer presents and returns its fingerprint,
/// without signing in: the connection ends after the handshake (for SMTP, after the
/// greeting and `EHLO`). Refused for any server that isn't on this computer.
pub fn fetch_certificate(endpoint: &ServerEndpoint, protocol: ServerProtocol, timeout: Duration) -> Result<String, ProviderError> {
    if !endpoint.is_local() {
        return Err(ProviderError::Rejected(NOT_LOCAL.into()));
    }
    if endpoint.security == Security::None {
        return Err(ProviderError::Rejected("this connection isn't encrypted, so there is no certificate to trust".into()));
    }
    if protocol == ServerProtocol::Smtp {
        return crate::smtp::fetch_certificate(endpoint, timeout);
    }
    let host = endpoint.host.trim();
    let tcp = tcp_connect(endpoint, timeout)?;
    require_loopback(&tcp)?;
    let mut tcp = match (endpoint.security, protocol) {
        (Security::StartTls, ServerProtocol::Pop3) => stls_by_hand(host, tcp)?,
        (Security::StartTls, _) => starttls_by_hand(host, tcp)?,
        _ => tcp,
    };
    let provider = provider();
    let verifier = Arc::new(RecordingVerifier { seen: Mutex::new(None), provider: provider.clone() });
    let config = config_with(verifier.clone(), provider)?;
    let name = ServerName::try_from(host.to_owned()).map_err(|_| ProviderError::Rejected(format!("“{host}”: {}", secure_error::INVALID_SERVER_NAME)))?;
    let mut conn = rustls::ClientConnection::new(config, name).map_err(|e| {
        secure_error::log_detail("TLS setup", &e);
        ProviderError::Rejected(secure_error::rustls_message(&e).into())
    })?;
    while conn.is_handshaking() {
        conn.complete_io(&mut tcp).map_err(|e| match e.kind() {
            std::io::ErrorKind::InvalidData => ProviderError::Rejected(crate::imap::connection::handshake_failed(host, secure_error::io("The TLS handshake", &e))),
            _ => ProviderError::Offline,
        })?;
    }
    let der = verifier.seen.lock().unwrap_or_else(|p| p.into_inner()).take().ok_or_else(|| ProviderError::Rejected(secure_error::SECURE_CONNECTION_FAILED.into()))?;
    conn.send_close_notify();
    let _ = conn.complete_io(&mut tcp);
    Ok(fingerprint(&der))
}

/// POP3's `STLS` (RFC 2595): the greeting, the command, and `+OK`.
fn stls_by_hand(host: &str, tcp: TcpStream) -> Result<TcpStream, ProviderError> {
    let mut reader = BufReader::new(tcp.try_clone().map_err(|_| ProviderError::Offline)?);
    let mut writer = tcp;
    let mut line = String::new();
    if reader.read_line(&mut line).map_err(|_| ProviderError::Offline)? == 0 {
        return Err(ProviderError::Offline);
    }
    if !line.starts_with("+OK") {
        return Err(ProviderError::Rejected(format!("{host} did not greet like a POP3 server")));
    }
    writer.write_all(b"STLS\r\n").and_then(|_| writer.flush()).map_err(|_| ProviderError::Offline)?;
    line.clear();
    if reader.read_line(&mut line).map_err(|_| ProviderError::Offline)? == 0 {
        return Err(ProviderError::Offline);
    }
    if !line.starts_with("+OK") {
        return Err(ProviderError::Rejected(format!("{host} refused STLS")));
    }
    Ok(writer)
}

/// Loopback mail servers with self-signed certificates, for tests here and in other crates.
#[cfg(any(test, feature = "fake-servers"))]
pub mod fake {
    use overlook_core::Security;
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::mpsc::{self, Receiver};
    use std::sync::Arc;
    use std::time::Duration;

    /// A self-signed P-256 certificate for 127.0.0.1 and localhost, valid until 2126, with
    /// its key. Test-only: nothing outside tests trusts it.
    pub const LOOPBACK_CERT: &[u8] = include_bytes!("../tests/fixtures/loopback-cert.der");
    pub const LOOPBACK_KEY: &[u8] = include_bytes!("../tests/fixtures/loopback-key.der");
    /// A second one, standing for a certificate that changed.
    pub const OTHER_CERT: &[u8] = include_bytes!("../tests/fixtures/other-loopback-cert.der");
    pub const OTHER_KEY: &[u8] = include_bytes!("../tests/fixtures/other-loopback-key.der");

    /// Presents one certificate with one key, without checking that they belong together,
    /// so a test can present a certificate with someone else's key.
    #[derive(Debug)]
    struct Presents(Arc<rustls::sign::CertifiedKey>);

    impl rustls::server::ResolvesServerCert for Presents {
        fn resolve(&self, _: rustls::server::ClientHello<'_>) -> Option<Arc<rustls::sign::CertifiedKey>> {
            Some(self.0.clone())
        }
    }

    /// A rustls server configuration presenting `cert` with `key`.
    pub fn server_config(cert: &[u8], key: &[u8]) -> Arc<rustls::ServerConfig> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let key = provider.key_provider.load_private_key(PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.to_vec()))).unwrap();
        let presents = Presents(Arc::new(rustls::sign::CertifiedKey::new(vec![CertificateDer::from(cert.to_vec())], key)));
        Arc::new(rustls::ServerConfig::builder_with_provider(provider).with_safe_default_protocol_versions().unwrap().with_no_client_auth().with_cert_resolver(Arc::new(presents)))
    }

    /// The protocol a fake server speaks.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Speaks {
        Imap,
        Pop3,
        Smtp,
    }

    /// A loopback mail server presenting `cert` with `key`, over implicit TLS
    /// ([`Security::Tls`]) or upgraded with STARTTLS (`STLS` for POP3). It accepts any
    /// sign-in and answers every command with success. When each connection ends, the lines
    /// it received over TLS are sent on the channel: a connection refused during the
    /// handshake sends none.
    pub fn serve(speaks: Speaks, security: Security, cert: &'static [u8], key: &'static [u8]) -> (u16, Receiver<Vec<String>>) {
        serve_on("127.0.0.1", speaks, security, cert, key)
    }

    /// [`serve`] on another loopback address, such as 127.0.0.2.
    pub fn serve_on(ip: &str, speaks: Speaks, security: Security, cert: &'static [u8], key: &'static [u8]) -> (u16, Receiver<Vec<String>>) {
        assert_ne!(security, Security::None, "the fake servers always encrypt");
        let listener = TcpListener::bind((ip, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for tcp in listener.incoming() {
                let Ok(tcp) = tcp else { break };
                let tx = tx.clone();
                std::thread::spawn(move || {
                    let _ = tcp.set_read_timeout(Some(Duration::from_secs(5)));
                    let _ = tx.send(session(speaks, security, cert, key, tcp));
                });
            }
        });
        (port, rx)
    }

    fn session(speaks: Speaks, security: Security, cert: &[u8], key: &[u8], mut tcp: TcpStream) -> Vec<String> {
        let mut lines = Vec::new();
        if security == Security::StartTls && !upgrade(speaks, &mut tcp) {
            return lines;
        }
        let Ok(conn) = rustls::ServerConnection::new(server_config(cert, key)) else { return lines };
        let mut tls = rustls::StreamOwned::new(conn, tcp);
        let greeting = match speaks {
            Speaks::Imap => "* OK ready",
            Speaks::Pop3 => "+OK ready",
            Speaks::Smtp => "220 local ESMTP",
        };
        if security == Security::Tls && !say(&mut tls, greeting) {
            return lines;
        }
        while let Some(line) = read_line(&mut tls) {
            lines.push(line.clone());
            let (replies, more) = answer(speaks, &line);
            for reply in replies {
                say(&mut tls, &reply);
            }
            if !more {
                break;
            }
        }
        lines
    }

    /// The plain part before TLS: greeting, capabilities and the upgrade command.
    fn upgrade(speaks: Speaks, tcp: &mut TcpStream) -> bool {
        match speaks {
            Speaks::Imap => {
                say(tcp, "* OK [CAPABILITY IMAP4rev1 STARTTLS] ready")
                    && read_line(tcp).is_some_and(|l| {
                        let tag = l.split(' ').next().unwrap_or("*").to_owned();
                        l.to_ascii_uppercase().ends_with(" STARTTLS") && say(tcp, &format!("{tag} OK begin TLS"))
                    })
            }
            Speaks::Pop3 => say(tcp, "+OK ready") && read_line(tcp).is_some_and(|l| l.eq_ignore_ascii_case("STLS")) && say(tcp, "+OK begin TLS"),
            Speaks::Smtp => {
                say(tcp, "220 local ESMTP")
                    && read_line(tcp).is_some_and(|l| l.to_ascii_uppercase().starts_with("EHLO"))
                    && say(tcp, "250-local")
                    && say(tcp, "250-STARTTLS")
                    && say(tcp, "250 AUTH PLAIN LOGIN")
                    && read_line(tcp).is_some_and(|l| l.eq_ignore_ascii_case("STARTTLS"))
                    && say(tcp, "220 go ahead")
            }
        }
    }

    /// The replies to one line over TLS, and whether the session goes on.
    fn answer(speaks: Speaks, line: &str) -> (Vec<String>, bool) {
        let upper = line.to_ascii_uppercase();
        match speaks {
            Speaks::Imap => {
                let tag = line.split(' ').next().unwrap_or("*");
                let command = upper.split(' ').nth(1).unwrap_or("");
                match command {
                    "CAPABILITY" => (vec!["* CAPABILITY IMAP4rev1 IDLE".into(), format!("{tag} OK done")], true),
                    "LOGOUT" => (vec!["* BYE".into(), format!("{tag} OK done")], false),
                    _ => (vec![format!("{tag} OK done")], true),
                }
            }
            Speaks::Pop3 => (vec![if upper == "QUIT" { "+OK bye".into() } else { "+OK".into() }], upper != "QUIT"),
            Speaks::Smtp if upper.starts_with("EHLO") => (vec!["250-local".into(), "250 AUTH PLAIN LOGIN".into()], true),
            Speaks::Smtp if upper.starts_with("AUTH") => (vec!["235 2.7.0 accepted".into()], true),
            Speaks::Smtp if upper == "QUIT" => (vec!["221 bye".into()], false),
            Speaks::Smtp => (vec!["250 ok".into()], true),
        }
    }

    /// One line from a stream, read a byte at a time so nothing past it is consumed.
    fn read_line(s: &mut impl Read) -> Option<String> {
        let mut buf = Vec::new();
        let mut byte = [0u8; 1];
        while let Ok(1) = s.read(&mut byte) {
            buf.push(byte[0]);
            if buf.ends_with(b"\r\n") {
                return Some(String::from_utf8_lossy(&buf).trim_end().to_owned());
            }
        }
        None
    }

    fn say(s: &mut impl Write, line: &str) -> bool {
        s.write_all(format!("{line}\r\n").as_bytes()).and_then(|_| s.flush()).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::fake::*;
    use super::*;

    const WAIT: Duration = Duration::from_secs(5);

    fn endpoint(port: u16, security: Security, pin: Option<&[u8]>) -> ServerEndpoint {
        ServerEndpoint { pinned_certificate: pin.map(fingerprint), ..ServerEndpoint::new("127.0.0.1", port, security, "jane@proton.me") }
    }

    fn refused_with<T: std::fmt::Debug>(result: Result<T, ProviderError>, message: &str) {
        match result {
            Err(ProviderError::Rejected(m)) if m.contains(message) => {}
            other => panic!("expected a refusal saying “{message}”, got {other:?}"),
        }
    }

    /// Whether any line sent a credential: SMTP or POP3 `AUTH`, POP3 `PASS`, IMAP `LOGIN` or
    /// `AUTHENTICATE`.
    fn signed_in(lines: &[String]) -> bool {
        lines.iter().any(|l| {
            let upper = l.to_ascii_uppercase();
            upper.starts_with("AUTH") || upper.starts_with("PASS") || matches!(upper.split(' ').nth(1), Some("LOGIN" | "AUTHENTICATE"))
        })
    }

    #[test]
    fn fingerprints_read_like_openssl() {
        let fp = fingerprint(LOOPBACK_CERT);
        assert_eq!(fp, "2E:9E:F8:BD:32:BA:74:07:C9:93:3D:90:A1:77:B7:81:63:8B:7D:7D:64:98:63:CE:27:A7:7F:20:F0:28:6E:F4");
        assert!(matches(LOOPBACK_CERT, &fp) && matches(LOOPBACK_CERT, &fp.replace(':', "").to_lowercase()));
        assert!(same_fingerprint(&fp, &fp.to_lowercase()) && !same_fingerprint(&fp, &fingerprint(OTHER_CERT)));
        assert!(!matches(OTHER_CERT, &fp) && !matches(LOOPBACK_CERT, "AB:CD") && !matches(LOOPBACK_CERT, &"é".repeat(32)));
        let signed = format!("+{}", &fp.replace(':', "")[1..]);
        assert!(!matches(LOOPBACK_CERT, &signed), "only hexadecimal digits");
    }

    #[test]
    fn pins_count_only_for_encrypted_servers_on_this_computer() {
        assert!(pin_for(&endpoint(1143, Security::StartTls, Some(LOOPBACK_CERT))).is_some());
        let localhost = ServerEndpoint { host: "localhost".into(), ..endpoint(1143, Security::StartTls, Some(LOOPBACK_CERT)) };
        assert!(pin_for(&localhost).is_some());
        let remote = ServerEndpoint { host: "imap.example.com".into(), ..endpoint(993, Security::Tls, Some(LOOPBACK_CERT)) };
        assert_eq!(pin_for(&remote), None, "a pin never relaxes verification of a remote server");
        assert_eq!(pin_for(&endpoint(1143, Security::None, Some(LOOPBACK_CERT))), None);
        assert_eq!(pin_for(&ServerEndpoint { pinned_certificate: Some("not a fingerprint".into()), ..endpoint(1143, Security::StartTls, None) }), None);
        refused_with(fetch_certificate(&remote, ServerProtocol::Imap, WAIT), NOT_LOCAL);
        refused_with(fetch_certificate(&ServerEndpoint { host: "smtp.example.com".into(), ..remote }, ServerProtocol::Smtp, WAIT), NOT_LOCAL);
        // An address elsewhere is refused even when asked for by address.
        refused_with(loopback_addresses(&ServerEndpoint::new("192.0.2.1", 1025, Security::StartTls, "x")), NOT_LOCAL);
        assert!(loopback_addresses(&ServerEndpoint::new("[::1]", 1025, Security::StartTls, "x")).is_ok());
    }

    #[test]
    fn a_pin_on_a_server_that_isnt_named_as_this_computer_is_ignored() {
        // 127.0.0.2 is a loopback address, but not one of the names a pin is honoured for:
        // the connection is verified against the public roots, which refuse the certificate.
        for (speaks, security) in [(Speaks::Imap, Security::StartTls), (Speaks::Smtp, Security::StartTls)] {
            let (port, seen) = serve_on("127.0.0.2", speaks, security, LOOPBACK_CERT, LOOPBACK_KEY);
            let elsewhere = ServerEndpoint { host: "127.0.0.2".into(), ..endpoint(port, security, Some(LOOPBACK_CERT)) };
            let result = match speaks {
                Speaks::Smtp => crate::smtp::Transport::new(elsewhere, "bridge-password").with_timeout(WAIT).probe(),
                _ => crate::imap::connection::connect(&elsewhere, &"bridge-password".into(), WAIT).map(|_| ()),
            };
            refused_with(result, CERTIFICATE_NOT_TRUSTED);
            assert!(!signed_in(&seen.recv_timeout(WAIT).unwrap()), "{speaks:?}");
        }
    }

    #[test]
    fn trust_questions_are_local_certificate_refusals() {
        let local = endpoint(1143, Security::StartTls, None);
        let remote = ServerEndpoint { host: "imap.example.com".into(), ..local.clone() };
        let untrusted = ProviderError::Rejected(format!("The TLS handshake to 127.0.0.1 failed: {CERTIFICATE_NOT_TRUSTED}"));
        assert!(is_trust_question(&local, &untrusted) && is_trust_question(&local, &ProviderError::Rejected(CERTIFICATE_CHANGED.into())));
        assert!(!is_trust_question(&remote, &untrusted), "remote certificates are never offered for trust");
        assert!(!is_trust_question(&local, &ProviderError::SignInRequired) && !is_trust_question(&local, &ProviderError::Offline));
        let posing = ProviderError::Rejected(format!("127.0.0.1 refused STARTTLS: {CERTIFICATE_NOT_TRUSTED}"));
        assert!(!is_trust_question(&local, &posing), "a server's own words don't count as a refusal");
    }

    #[test]
    fn local_certificates_are_read_without_signing_in() {
        let expected = Ok(fingerprint(LOOPBACK_CERT));
        for (speaks, protocol) in [(Speaks::Imap, ServerProtocol::Imap), (Speaks::Pop3, ServerProtocol::Pop3), (Speaks::Smtp, ServerProtocol::Smtp)] {
            for security in [Security::StartTls, Security::Tls] {
                let (port, seen) = serve(speaks, security, LOOPBACK_CERT, LOOPBACK_KEY);
                assert_eq!(fetch_certificate(&endpoint(port, security, None), protocol, WAIT), expected, "{speaks:?} {security:?}");
                let lines = seen.recv_timeout(WAIT).unwrap();
                assert!(!signed_in(&lines), "{speaks:?} {security:?}: {lines:?}");
            }
        }
        // A server whose key doesn't match its certificate fails the handshake signature
        // check, so its certificate is never offered.
        let (port, _) = serve(Speaks::Imap, Security::StartTls, LOOPBACK_CERT, OTHER_KEY);
        refused_with(fetch_certificate(&endpoint(port, Security::StartTls, None), ServerProtocol::Imap, WAIT), CERTIFICATE_NOT_TRUSTED);
    }

    /// For each protocol: not trusted yet, refused before signing in; trusted, signed in;
    /// another certificate where the trusted one was, refused as changed before signing in;
    /// the trusted certificate with someone else's key, refused before signing in.
    fn signs_in_only_to_the_pinned_certificate(speaks: Speaks, security: Security, sign_in: impl Fn(&ServerEndpoint) -> Result<(), ProviderError>) {
        let attempt = |cert, key, pin| {
            let (port, seen) = serve(speaks, security, cert, key);
            let result = sign_in(&endpoint(port, security, pin));
            (result, seen.recv_timeout(WAIT).unwrap())
        };
        let (result, lines) = attempt(LOOPBACK_CERT, LOOPBACK_KEY, None);
        refused_with(result, CERTIFICATE_NOT_TRUSTED);
        assert!(!signed_in(&lines), "{speaks:?}: {lines:?}");

        let (result, lines) = attempt(LOOPBACK_CERT, LOOPBACK_KEY, Some(LOOPBACK_CERT));
        assert_eq!(result, Ok(()), "{speaks:?}: the pinned certificate is accepted");
        assert!(signed_in(&lines), "{speaks:?}: {lines:?}");

        let (result, lines) = attempt(OTHER_CERT, OTHER_KEY, Some(LOOPBACK_CERT));
        refused_with(result, CERTIFICATE_CHANGED);
        assert!(!signed_in(&lines), "{speaks:?}: no credentials reach a server whose certificate changed: {lines:?}");

        let (result, lines) = attempt(LOOPBACK_CERT, OTHER_KEY, Some(LOOPBACK_CERT));
        refused_with(result, CERTIFICATE_NOT_TRUSTED);
        assert!(!signed_in(&lines), "{speaks:?}: the certificate alone is not enough: {lines:?}");
    }

    #[test]
    fn imap_signs_in_only_to_the_pinned_certificate() {
        for security in [Security::StartTls, Security::Tls] {
            signs_in_only_to_the_pinned_certificate(Speaks::Imap, security, |e| crate::imap::connection::connect(e, &"bridge-password".into(), WAIT).map(|mut s| drop(s.logout())));
        }
    }

    #[test]
    fn pop3_signs_in_only_to_the_pinned_certificate() {
        for security in [Security::StartTls, Security::Tls] {
            signs_in_only_to_the_pinned_certificate(Speaks::Pop3, security, |e| crate::pop3::client::Client::connect(e, &"bridge-password".into(), WAIT).and_then(|c| c.quit()));
        }
    }

    #[test]
    fn smtp_signs_in_only_to_the_pinned_certificate() {
        for security in [Security::StartTls, Security::Tls] {
            signs_in_only_to_the_pinned_certificate(Speaks::Smtp, security, |e| crate::smtp::Transport::new(e.clone(), "bridge-password").with_timeout(WAIT).probe());
        }
    }
}
