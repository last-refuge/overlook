//! Constant wording for transport, TLS and certificate failures.
//!
//! Library error text (rustls, ureq, lettre, the IMAP client) can describe handshake state,
//! certificate contents, or internal paths. It never reaches returned errors or the UI:
//! callers get one of the fixed messages below, and the detail is logged only at debug level
//! under `overlook::diag`, which release builds do not emit.

use std::fmt::Display;

/// The TLS handshake or session failed for a reason other than certificate trust.
pub const SECURE_CONNECTION_FAILED: &str = "couldn't make a secure connection to the server";
/// The server's certificate chain, validity, or hostname was rejected.
pub const CERTIFICATE_NOT_TRUSTED: &str = "the server's security certificate isn't trusted";
/// The server could not be reached at all (DNS, refused, timeout, dropped).
pub const NETWORK_FAILED: &str = "couldn't reach the server";
/// The host name cannot be used for TLS server-name verification.
pub const INVALID_SERVER_NAME: &str = "the server name isn't valid for a secure connection";
/// A reply arrived but could not be decoded.
pub const UNREADABLE_RESPONSE: &str = "the server's reply couldn't be read";

/// Logs library detail for local troubleshooting, at debug level only.
pub fn log_detail(context: &str, detail: impl Display) {
    tracing::debug!(target: "overlook::diag", context, "{detail}");
}

/// The fixed message for a rustls error.
pub fn rustls_message(e: &rustls::Error) -> &'static str {
    match e {
        rustls::Error::InvalidCertificate(_) | rustls::Error::NoCertificatesPresented | rustls::Error::InvalidCertRevocationList(_) => CERTIFICATE_NOT_TRUSTED,
        _ => SECURE_CONNECTION_FAILED,
    }
}

/// The fixed message for an I/O error. rustls reports handshake and certificate failures
/// as `InvalidData` wrapping a `rustls::Error`.
pub fn io_message(e: &std::io::Error) -> &'static str {
    if let Some(tls) = e.get_ref().and_then(|inner| inner.downcast_ref::<rustls::Error>()) {
        return rustls_message(tls);
    }
    match e.kind() {
        std::io::ErrorKind::InvalidData => SECURE_CONNECTION_FAILED,
        _ => NETWORK_FAILED,
    }
}

/// The fixed message for a ureq (HTTPS) error.
pub fn ureq_message(e: &ureq::Error) -> &'static str {
    match e {
        ureq::Error::Rustls(tls) => rustls_message(tls),
        ureq::Error::Tls(_) | ureq::Error::RequireHttpsOnly(_) => SECURE_CONNECTION_FAILED,
        ureq::Error::Io(io) => io_message(io),
        ureq::Error::Json(_) | ureq::Error::BodyExceedsLimit(_) | ureq::Error::Protocol(_) => UNREADABLE_RESPONSE,
        _ => NETWORK_FAILED,
    }
}

/// Maps a ureq error to its fixed message and logs the detail.
pub fn ureq(context: &str, e: ureq::Error) -> &'static str {
    let message = ureq_message(&e);
    log_detail(context, &e);
    message
}

/// Maps an I/O error to its fixed message and logs the detail.
pub fn io(context: &str, e: &std::io::Error) -> &'static str {
    log_detail(context, e);
    io_message(e)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustls::{CertificateError, Error as TlsError};

    #[test]
    fn certificate_failures_map_to_one_constant() {
        for e in [
            TlsError::InvalidCertificate(CertificateError::Expired),
            TlsError::InvalidCertificate(CertificateError::UnknownIssuer),
            TlsError::InvalidCertificate(CertificateError::NotValidForName),
            TlsError::NoCertificatesPresented,
        ] {
            assert_eq!(rustls_message(&e), CERTIFICATE_NOT_TRUSTED, "{e:?}");
            let wrapped = std::io::Error::new(std::io::ErrorKind::InvalidData, e);
            assert_eq!(io_message(&wrapped), CERTIFICATE_NOT_TRUSTED);
            assert_eq!(ureq_message(&ureq::Error::Io(wrapped)), CERTIFICATE_NOT_TRUSTED);
        }
    }

    #[test]
    fn handshake_and_network_failures_map_to_constants() {
        let alert = TlsError::AlertReceived(rustls::AlertDescription::HandshakeFailure);
        assert_eq!(rustls_message(&alert), SECURE_CONNECTION_FAILED);
        assert_eq!(ureq_message(&ureq::Error::Rustls(alert)), SECURE_CONNECTION_FAILED);
        assert_eq!(io_message(&std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "synthetic-detail")), NETWORK_FAILED);
        assert_eq!(ureq_message(&ureq::Error::HostNotFound), NETWORK_FAILED);
    }

    #[test]
    fn constants_carry_no_library_detail() {
        for message in [SECURE_CONNECTION_FAILED, CERTIFICATE_NOT_TRUSTED, NETWORK_FAILED, INVALID_SERVER_NAME, UNREADABLE_RESPONSE] {
            for word in ["rustls", "ureq", "lettre", "webpki", "alert", "{"] {
                assert!(!message.contains(word), "{message}");
            }
        }
    }
}
