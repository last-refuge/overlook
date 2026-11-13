//! The loopback half of a browser sign-in (RFC 8252 §7.3), shared by Google and Microsoft.
//!
//! A listener on 127.0.0.1 waits on a background thread for the browser to follow the
//! redirect. It refuses anything that is not exactly that redirect (method, path, Host and the
//! request's `state`), answers the real one with a small branded page, and hands its query to
//! the sign-in for the token exchange.

use overlook_provider::oauth::{parse_callback, CallbackError};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(600);

/// What the browser's redirect must look like.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Redirect {
    /// The only `Host` header accepted: `127.0.0.1:PORT` (Google) or `localhost:PORT`
    /// (Microsoft).
    pub authority: String,
    /// The redirect's path: `/callback` (Google) or `/` (Microsoft).
    pub path: &'static str,
    /// The authorization request's `state`.
    pub state: String,
}

/// Binds the listener on an ephemeral port of 127.0.0.1. With `ipv6`, for a redirect to
/// `localhost`, the same port of `::1` is bound too: browsers try `::1` first, so no other
/// program may be listening there. A port whose `::1` side is taken is given up for another;
/// a machine without IPv6 needs only the first.
pub fn bind(ipv6: bool) -> Result<(Vec<TcpListener>, u16), String> {
    let failed = |e: std::io::Error| format!("could not open a local port for sign-in: {e}");
    for _ in 0..8 {
        let v4 = TcpListener::bind("127.0.0.1:0").map_err(failed)?;
        let port = v4.local_addr().map_err(|e| e.to_string())?.port();
        let mut listeners = vec![v4];
        if ipv6 {
            match TcpListener::bind(("::1", port)) {
                Ok(v6) => listeners.push(v6),
                Err(e)
                    if e.kind() == std::io::ErrorKind::AddrNotAvailable
                        || e.raw_os_error() == Some(libc::EAFNOSUPPORT) => {}
                Err(_) => continue,
            }
        }
        for listener in &listeners {
            listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        }
        return Ok((listeners, port));
    }
    Err("could not open a local port for sign-in: every port tried was taken".into())
}

/// Waits on a background thread for the redirect on any of `listeners` (one port, one or
/// both address families). The receiver gets its query string, or why the wait ended
/// (cancelled through `cancel`, timed out, or a listener failed).
pub fn wait_for_redirect(
    listeners: Vec<TcpListener>,
    redirect: Redirect,
    cancel: Arc<AtomicBool>,
) -> Result<Receiver<Result<String, String>>, String> {
    let (sender, receiver) = mpsc::channel();
    std::thread::Builder::new()
        .name("overlook-oauth-callback".into())
        .spawn(move || {
            let started = Instant::now();
            loop {
                if cancel.load(Ordering::SeqCst) {
                    let _ = sender.send(Err("Sign-in was cancelled.".into()));
                    return;
                }
                if started.elapsed() > SIGN_IN_TIMEOUT {
                    let _ = sender.send(Err(
                        "Sign-in timed out. Try again when you are ready.".into()
                    ));
                    return;
                }
                let mut idle = true;
                for listener in &listeners {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            idle = false;
                            if let Some(query) = handle_connection(stream, &redirect) {
                                let _ = sender.send(Ok(query));
                                return;
                            }
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                        Err(e) => {
                            let _ = sender.send(Err(format!("the sign-in listener failed: {e}")));
                            return;
                        }
                    }
                }
                if idle {
                    std::thread::sleep(Duration::from_millis(80));
                }
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(receiver)
}

/// Reads one HTTP request, answers with the completion page, and returns the query string
/// of the redirect. Anything else (a favicon, a forged request) gets a 400 and `None`.
fn handle_connection(mut stream: TcpStream, expected: &Redirect) -> Option<String> {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut buf = [0u8; 4096];
    let n = stream.read(&mut buf).ok()?;
    let request = String::from_utf8_lossy(&buf[..n]);
    let Some((query, denied)) = validated_callback(&request, expected) else {
        tracing::info!(target: "overlook::signin", "loopback request refused: method, path, host, or state did not match");
        let _ = stream.write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\nCache-Control: no-store\r\n\r\n");
        return None;
    };
    tracing::info!(target: "overlook::signin", state_matched = true, outcome = if denied { "consent declined" } else { "authorization code received" }, code = "[REDACTED]", "loopback callback accepted");
    let body = completion_page(denied);
    let _ = stream.write_all(
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nX-Content-Type-Options: nosniff\r\nContent-Security-Policy: default-src 'none'; style-src 'unsafe-inline'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'\r\n\r\n{}",
            body.len(),
            body
        )
        .as_bytes(),
    );
    let _ = stream.flush();
    Some(query.to_owned())
}

/// The redirect's query and whether it reports a declined consent, when `request` is exactly
/// the expected redirect: `GET <path>?<query> HTTP/1.1`, one `Host` header naming the
/// listener, and a query whose `state` matches.
fn validated_callback<'a>(request: &'a str, expected: &Redirect) -> Option<(&'a str, bool)> {
    let mut words = request.lines().next()?.split_whitespace();
    if words.next()? != "GET" {
        return None;
    }
    let (path, query) = words.next()?.split_once('?')?;
    if path != expected.path || words.next()? != "HTTP/1.1" || words.next().is_some() {
        return None;
    }
    let hosts: Vec<_> = request
        .lines()
        .skip(1)
        .take_while(|line| !line.is_empty())
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("host").then_some(value.trim())
        })
        .collect();
    if hosts != [expected.authority.as_str()] {
        return None;
    }
    match parse_callback(query, &expected.state) {
        Ok(_) => Some((query, false)),
        Err(CallbackError::Denied(_)) => Some((query, true)),
        Err(_) => None,
    }
}

fn completion_page(denied: bool) -> String {
    let (title, text) = if denied {
        (
            "Sign-in cancelled",
            "You can close this tab and return to Overlook.",
        )
    } else {
        (
            "You’re signed in",
            "You can close this tab and return to Overlook. Your mail is on its way.",
        )
    };
    format!(
        r##"<!doctype html><html lang="en"><head><meta charset="utf-8"><title>Overlook · {title}</title>
<style>body{{margin:0;min-height:100vh;display:grid;place-items:center;background:#101C24;color:#F0EBDD;font:16px/1.5 Inter,system-ui,sans-serif}}
main{{text-align:center;padding:40px}}h1{{font:400 40px/1.1 'Source Serif 4','Source Serif Pro',Georgia,serif;margin:18px 0 10px}}p{{color:#C6D5D5;margin:0}}
svg{{display:block;margin:0 auto}}small{{display:block;margin-top:34px;color:#A8B7BA;letter-spacing:.14em;font-size:12px}}</style></head>
<body><main><svg width="72" height="72" viewBox="0 0 128 128" aria-hidden="true"><circle cx="64" cy="64" r="49" fill="none" stroke="#F0EBDD" stroke-width="10"/><path d="M 21 79 L 48 59 L 72 77 L 92 64 L 109 78" fill="none" stroke="#F0EBDD" stroke-width="8" stroke-linejoin="round" stroke-linecap="round"/><circle cx="79" cy="42" r="5" fill="#FF9D32"/></svg>
<h1>{title}</h1><p>{text}</p><small>OVERLOOK BY LAST REFUGE</small></main></body></html>"##
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn localhost_redirects_are_answered_on_both_loopback_addresses() {
        use std::net::{SocketAddr, TcpStream};
        let (listeners, port) = bind(true).unwrap();
        let has_ipv6 = listeners.len() == 2;
        let redirect = Redirect {
            authority: format!("localhost:{port}"),
            path: "/",
            state: "synthetic-state".into(),
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let receiver = wait_for_redirect(listeners, redirect, cancel.clone()).unwrap();
        // A stray request is refused and the wait goes on; the redirect, on ::1 when this
        // machine has IPv6, ends it.
        let address: SocketAddr = if has_ipv6 {
            format!("[::1]:{port}")
        } else {
            format!("127.0.0.1:{port}")
        }
        .parse()
        .unwrap();
        let send = |request: &str| {
            let mut stream = TcpStream::connect(address).unwrap();
            stream.write_all(request.as_bytes()).unwrap();
            let mut reply = String::new();
            let _ = stream.read_to_string(&mut reply);
            reply
        };
        assert!(send(&format!(
            "GET /favicon.ico HTTP/1.1\r\nHost: localhost:{port}\r\n\r\n"
        ))
        .starts_with("HTTP/1.1 400"));
        assert!(send(&format!("GET /?code=synthetic-code&state=synthetic-state HTTP/1.1\r\nHost: localhost:{port}\r\n\r\n")).starts_with("HTTP/1.1 200"));
        assert_eq!(
            receiver.recv_timeout(Duration::from_secs(5)).unwrap(),
            Ok("code=synthetic-code&state=synthetic-state".to_owned())
        );
        let (single, _) = bind(false).unwrap();
        assert_eq!(
            single.len(),
            1,
            "Google's 127.0.0.1 redirect needs one listener"
        );
    }

    #[test]
    fn completion_page_mentions_overlook() {
        assert!(completion_page(false).contains("You’re signed in"));
        assert!(completion_page(true).contains("cancelled"));
    }

    #[test]
    fn callback_rejects_forgery_pollution_and_wrong_origin() {
        // Google's shape: 127.0.0.1 and /callback.
        let expected = Redirect {
            authority: "127.0.0.1:43123".into(),
            path: "/callback",
            state: "synthetic-state".into(),
        };
        let valid = "GET /callback?code=synthetic-code&state=synthetic-state HTTP/1.1\r\nHost: 127.0.0.1:43123\r\n\r\n";
        assert!(validated_callback(valid, &expected).is_some());
        for invalid in [
            valid.replace("GET", "POST"),
            valid.replace("Host: 127.0.0.1:43123", "Host: attacker.example"),
            valid.replace("state=synthetic-state", "state=wrong"),
            valid.replace("state=synthetic-state", "state=wrong&state=synthetic-state"),
            valid.replace("/callback?", "/other?"),
            valid.replace("\r\n\r\n", "\r\nHost: 127.0.0.1:43123\r\n\r\n"),
        ] {
            assert!(validated_callback(&invalid, &expected).is_none());
        }
        // Microsoft's shape: localhost and the root path, with its extra session_state.
        let expected = Redirect {
            authority: "localhost:43123".into(),
            path: "/",
            state: "synthetic-state".into(),
        };
        let valid = "GET /?code=synthetic-code&state=synthetic-state&session_state=s HTTP/1.1\r\nHost: localhost:43123\r\n\r\n";
        assert_eq!(
            validated_callback(valid, &expected),
            Some((
                "code=synthetic-code&state=synthetic-state&session_state=s",
                false
            ))
        );
        let declined = "GET /?error=access_denied&error_description=AADSTS65004&state=synthetic-state HTTP/1.1\r\nHost: localhost:43123\r\n\r\n";
        assert_eq!(
            validated_callback(declined, &expected).map(|(_, denied)| denied),
            Some(true)
        );
        for invalid in [
            valid.replace("GET /?", "GET /callback?"),
            valid.replace("Host: localhost:43123", "Host: 127.0.0.1:43123"),
            valid.replace("state=synthetic-state", "state=forged"),
        ] {
            assert!(
                validated_callback(&invalid, &expected).is_none(),
                "{invalid}"
            );
        }
    }
}
