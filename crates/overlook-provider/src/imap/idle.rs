//! IMAP `IDLE` on the Inbox (plan milestone M5): a dedicated thread holding one extra
//! connection per account that reports when the server announces a change, so the app can
//! run its ordinary sync at once instead of waiting for the next poll.
//!
//! The thread connects, `EXAMINE`s the Inbox and sits in `IDLE`. An untagged `EXISTS`,
//! `EXPUNGE` or `FETCH` ends the wait and is reported as [`IdleEvent::Changed`]; the
//! `IDLE` is re-issued every [`IDLE_REFRESH`] so quiet connections are not dropped by
//! NATs and proxies long before RFC 2177's 29 minutes. A lost connection is reopened with
//! the shared backoff schedule; a server without `IDLE`, or one that refuses the
//! credentials, ends the thread (polling stays the fallback, and the sync path reports the
//! sign-in problem). Nothing here touches the UI thread or the provider's own connection.
//!
//! [`IdleWatcher::stop`] (also on drop) sets a flag and shuts the socket down, which
//! unblocks the read in progress, so stopping is immediate.

use super::connection::{self, map_error};
use crate::{backoff_seconds, Credential, ProviderError};
use imap::extensions::idle::WaitOutcome;
use imap::types::UnsolicitedResponse;
use overlook_core::ServerEndpoint;
use std::net::{Shutdown, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;
use tracing::{debug, info, warn};

/// How long one `IDLE` command runs before it is ended and re-issued.
pub const IDLE_REFRESH: Duration = Duration::from_secs(9 * 60);

/// Pause after the first change notice before reporting, so a delivery that produces several
/// untagged lines becomes one sync request.
const SETTLE: Duration = Duration::from_millis(300);

/// Reconnect attempts after which a thread that keeps failing gives up until the watcher is
/// restarted (the provider is rebuilt on every settings change).
const MAX_ATTEMPTS: u32 = 1_000;

/// What the watcher reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdleEvent {
    /// The Inbox changed on the server (new mail, an expunge or a flag change): sync now.
    Changed,
    /// The server does not offer `IDLE`; polling is the only path. Reported once, then the
    /// thread ends.
    Unsupported,
    /// The server refused the credentials. The thread ends; the sync path shows the problem.
    SignInRequired,
}

/// A running IDLE thread. Dropping it stops the thread.
pub struct IdleWatcher {
    stop: Arc<AtomicBool>,
    socket: Arc<Mutex<Option<TcpStream>>>,
    thread: Option<JoinHandle<()>>,
    tag: usize,
}

impl IdleWatcher {
    /// Attaches an opaque tag (the app uses the provider's address) so a caller can tell
    /// whether a running watcher belongs to the current provider.
    pub fn with_tag(mut self, tag: usize) -> Self {
        self.tag = tag;
        self
    }

    pub fn tag(&self) -> usize {
        self.tag
    }

    /// Ends the thread: the flag is set and the open socket is shut down so a blocked read
    /// returns at once. Waits for the thread to finish.
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Ok(guard) = self.socket.lock() {
            if let Some(sock) = guard.as_ref() {
                let _ = sock.shutdown(Shutdown::Both);
            }
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }

    /// Whether the thread is still alive (it ends by itself on an unsupported server or a
    /// refused sign-in).
    pub fn is_running(&self) -> bool {
        self.thread.as_ref().is_some_and(|t| !t.is_finished())
    }
}

impl Drop for IdleWatcher {
    fn drop(&mut self) {
        self.stop();
    }
}

enum Ended {
    Stopped,
    Unsupported,
    SignInRequired,
    /// The connection or the server failed; `worked` says whether IDLE had been running.
    Lost { error: ProviderError, worked: bool },
}

/// Starts the IDLE thread for `endpoint`'s Inbox. `on_event` runs on that thread; keep it
/// quick (send on a channel) and never block in it.
pub fn watch(endpoint: ServerEndpoint, credential: Credential, timeout: Duration, on_event: impl Fn(IdleEvent) + Send + 'static) -> IdleWatcher {
    let stop = Arc::new(AtomicBool::new(false));
    let socket: Arc<Mutex<Option<TcpStream>>> = Arc::new(Mutex::new(None));
    let (stop_flag, socket_slot) = (stop.clone(), socket.clone());
    let host = endpoint.host.clone();
    let thread = std::thread::Builder::new()
        .name(format!("imap-idle {host}"))
        .spawn(move || run(endpoint, credential, timeout, stop_flag, socket_slot, on_event))
        .ok();
    if thread.is_none() {
        warn!("imap idle: could not start a thread for {host}; polling only");
    }
    IdleWatcher { stop, socket, thread, tag: 0 }
}

fn run(endpoint: ServerEndpoint, credential: Credential, timeout: Duration, stop: Arc<AtomicBool>, socket: Arc<Mutex<Option<TcpStream>>>, on_event: impl Fn(IdleEvent)) {
    let host = endpoint.host.clone();
    let mut attempt: u32 = 0;
    while !stop.load(Ordering::SeqCst) && attempt < MAX_ATTEMPTS {
        let (mut session, sock) = match connection::connect_with_socket(&endpoint, &credential, timeout) {
            Ok(connected) => connected,
            Err(ProviderError::SignInRequired) => {
                warn!("imap idle: {host} refused the credentials; stopping until the account signs in again");
                on_event(IdleEvent::SignInRequired);
                return;
            }
            Err(ProviderError::Rejected(reason)) => {
                warn!("imap idle: {host}: {reason}; not retrying");
                return;
            }
            Err(e) => {
                attempt += 1;
                let wait = backoff_seconds(attempt);
                debug!("imap idle: {host} unreachable ({e}); retrying in {wait}s");
                pause(&stop, Duration::from_secs(wait));
                continue;
            }
        };
        let sock_for_timeouts = sock.try_clone().ok();
        if let Ok(mut slot) = socket.lock() {
            *slot = Some(sock);
        }
        let ended = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| serve(&mut session, sock_for_timeouts.as_ref(), timeout, &stop, &on_event))) {
            Ok(ended) => ended,
            Err(_) => Ended::Lost { error: ProviderError::Temporary("the IDLE reply could not be read".into()), worked: false },
        };
        if !matches!(ended, Ended::Stopped) {
            let _ = session.logout();
        }
        drop(session);
        if let Ok(mut slot) = socket.lock() {
            *slot = None;
        }
        match ended {
            Ended::Stopped => return,
            Ended::Unsupported => {
                info!("imap idle: {host} does not offer IDLE; polling only");
                on_event(IdleEvent::Unsupported);
                return;
            }
            Ended::SignInRequired => {
                on_event(IdleEvent::SignInRequired);
                return;
            }
            Ended::Lost { error, worked } => {
                if stop.load(Ordering::SeqCst) {
                    return;
                }
                attempt = if worked { 1 } else { attempt + 1 };
                let wait = backoff_seconds(attempt);
                debug!("imap idle: {host} connection ended ({error}); reconnecting in {wait}s");
                pause(&stop, Duration::from_secs(wait));
            }
        }
    }
}

/// Runs IDLE on one connection until it is stopped or fails.
fn serve(session: &mut connection::Session, sock: Option<&TcpStream>, timeout: Duration, stop: &AtomicBool, on_event: &impl Fn(IdleEvent)) -> Ended {
    let lost = |error: ProviderError, worked: bool| if stop.load(Ordering::SeqCst) { Ended::Stopped } else { Ended::Lost { error, worked } };
    let caps = match session.capabilities() {
        Ok(caps) => caps,
        Err(e) => return lost(map_error(&e), false),
    };
    if !caps.has_str("IDLE") {
        return Ended::Unsupported;
    }
    if let Err(e) = session.examine("INBOX") {
        return match map_error(&e) {
            ProviderError::SignInRequired => Ended::SignInRequired,
            other => lost(other, false),
        };
    }
    let mut worked = false;
    loop {
        if stop.load(Ordering::SeqCst) {
            return Ended::Stopped;
        }
        let outcome = {
            let mut handle = session.idle();
            handle.timeout(IDLE_REFRESH).keepalive(false);
            let outcome = handle.wait_while(|response| !matches!(response, UnsolicitedResponse::Exists(_) | UnsolicitedResponse::Expunge(_) | UnsolicitedResponse::Fetch { .. }));
            // `wait_while` removes the read timeout; put it back before the handle's drop
            // sends DONE and waits for the tagged reply, so a dead server cannot hang here.
            if let Some(sock) = sock {
                let _ = sock.set_read_timeout(Some(timeout));
            }
            outcome
        };
        match outcome {
            Ok(WaitOutcome::MailboxChanged) => {
                worked = true;
                std::thread::sleep(SETTLE);
                // Changes the server queued meanwhile are already parsed; the sync reads
                // the folder anyway.
                let _ = session.take_all_unsolicited();
                if stop.load(Ordering::SeqCst) {
                    return Ended::Stopped;
                }
                debug!("imap idle: the Inbox changed; asking for a sync");
                on_event(IdleEvent::Changed);
            }
            Ok(WaitOutcome::TimedOut) => {
                worked = true;
                let _ = session.take_all_unsolicited();
            }
            Err(e) => return lost(map_error(&e), worked),
        }
    }
}

/// Sleeps for `total`, waking early when the watcher is stopped.
fn pause(stop: &AtomicBool, total: Duration) {
    let step = Duration::from_millis(250);
    let mut slept = Duration::ZERO;
    while slept < total && !stop.load(Ordering::SeqCst) {
        std::thread::sleep(step);
        slept += step;
    }
}
