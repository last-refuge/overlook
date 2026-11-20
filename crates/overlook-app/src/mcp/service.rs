//! Discovery IPC for the process that already owns the Overlook profile.
//!
//! The local UID is the filesystem trust boundary: a peer that can modify the
//! profile can also set its grants. Bearers limit SDK clients using the socket;
//! they are checked against the current grant on every request.

mod files;
mod wire;

use std::io;
use std::net::Shutdown;
use std::os::fd::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread;

use overlook_core::ProviderKind;
use overlook_store::Store;

use super::contract::{Account, Capabilities, ErrorCode, Failure, Reply, Request, Response};
use files::{Endpoint, Profile};

const SAMPLE_IDS: [&str; 2] = ["sample", "sample-secondary"];

/// Keep this handle alive for as long as the application owns the profile.
pub struct Server {
    stopped: Arc<AtomicBool>,
    wake: UnixStream,
    _endpoint: Endpoint,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        // A discovery read may be waiting on a Store lock. Let the worker
        // finish that read without delaying desktop shutdown.
        let _ = self.wake.shutdown(Shutdown::Both);
    }
}

pub fn start(profile: &Path, store: Arc<Store>) -> anyhow::Result<Server> {
    let profile = Profile::open(profile)?;
    let (endpoint, listener) = profile.listen()?;
    let key = profile.account_key()?;
    let stopped = Arc::new(AtomicBool::new(false));
    let stop_signal = Arc::clone(&stopped);
    let (wake, shutdown) = UnixStream::pair()?;
    thread::Builder::new()
        .name("overlook-mcp".into())
        .spawn(move || serve(listener, shutdown, profile, key, store, stop_signal))?;
    Ok(Server {
        stopped,
        wake,
        _endpoint: endpoint,
    })
}

pub fn call(profile: &Path, client: &str, token: &str, request: Request) -> Reply {
    let unavailable = || Failure::new(ErrorCode::ProfileUnavailable, "Profile unavailable");
    let socket = files::existing_socket(profile).map_err(|_| unavailable())?;
    let mut stream = wire::connect(&socket).map_err(|_| unavailable())?;
    if !files::valid_client(client) || !files::valid_token(token) {
        return Err(denied());
    }
    wire::call(&mut stream, client, token, request)
}

pub fn grant(profile: &Path, client: &str, account_ids: &[String]) -> anyhow::Result<String> {
    anyhow::ensure!(files::valid_client(client), "Invalid client name");
    // Grants can be provisioned before the profile owner is running. The owner
    // checks that each ID still resolves to a synthetic account at execution.
    anyhow::ensure!(
        !account_ids.is_empty()
            && account_ids
                .iter()
                .all(|id| SAMPLE_IDS.contains(&id.as_str())),
        "Only explicit sample account IDs can be granted"
    );
    Profile::open(profile)?.grant(client, account_ids)
}

pub fn revoke(profile: &Path, client: &str) -> anyhow::Result<()> {
    anyhow::ensure!(files::valid_client(client), "Invalid client name");
    Profile::open(profile)?.revoke(client)
}

fn serve(
    listener: UnixListener,
    shutdown: UnixStream,
    profile: Profile,
    key: [u8; 32],
    store: Arc<Store>,
    stopped: Arc<AtomicBool>,
) {
    while wait_for_request(&listener, &shutdown) && !stopped.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((mut stream, _)) => {
                if stream.set_nonblocking(true).is_ok() {
                    serve_request(&mut stream, &profile, &key, &store, &stopped);
                }
            }
            Err(err)
                if matches!(
                    err.kind(),
                    io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                ) =>
            {
                continue
            }
            Err(_) => break,
        }
    }
}

fn wait_for_request(listener: &UnixListener, shutdown: &UnixStream) -> bool {
    let mut ready = [listener.as_raw_fd(), shutdown.as_raw_fd()].map(|fd| libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    });
    loop {
        // SAFETY: ready is an initialized array of two valid pollfd entries.
        if unsafe { libc::poll(ready.as_mut_ptr(), ready.len() as libc::nfds_t, -1) } >= 0 {
            return ready[1].revents == 0 && ready[0].revents & libc::POLLIN != 0;
        }
        if io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
            return false;
        }
    }
}

fn serve_request(
    stream: &mut UnixStream,
    profile: &Profile,
    key: &[u8; 32],
    store: &Store,
    stopped: &AtomicBool,
) {
    wire::serve(stream, stopped, |client, token, request| {
        let grant = profile.authorize(client, token).ok_or_else(denied)?;
        let eligible = store
            .accounts()
            .map_err(|_| Failure::new(ErrorCode::Internal, "Account discovery failed"))?
            .into_iter()
            .filter(|account| {
                account.provider == ProviderKind::Sample
                    && SAMPLE_IDS.contains(&account.id.as_str())
                    && grant.contains(&account.id.0)
            })
            .map(|account| Account {
                account_id: files::public_id(key, account.id.as_str()),
                display_name: account.display_name,
                email: account.email,
                provider: account.provider.as_key().to_owned(),
            });

        match request {
            Request::AccountsList => Ok(Response::Accounts {
                accounts: eligible.collect(),
            }),
            Request::AccountCapabilities { account_id } => {
                let account = eligible
                    .into_iter()
                    .find(|account| account.account_id == account_id)
                    .ok_or_else(denied)?;
                // The Sample provider supplies fictional mail, calendars and contacts.
                Ok(Response::Capabilities(Capabilities {
                    account_id: account.account_id,
                    mail: true,
                    calendar: true,
                    contacts: true,
                }))
            }
        }
    });
}

fn denied() -> Failure {
    Failure::new(ErrorCode::AccessDenied, "Access denied")
}
