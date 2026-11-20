//! Discovery IPC for the process that already owns the Overlook profile.
//!
//! The local UID is the filesystem trust boundary: a peer that can modify the
//! profile can also set its grants. Bearers limit SDK clients using the socket;
//! they are checked against the current grant on every request.

mod files;
mod wire;

use std::collections::BTreeSet;
use std::io;
use std::net::Shutdown;
use std::os::fd::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc, Mutex,
};
use std::thread;

use overlook_core::Account as LocalAccount;
use overlook_provider::sample::SampleProvider;
use overlook_store::{McpWriteOutcome, Store};
use serde_json::{json, Map, Value};
use tokio::runtime::{Builder, Runtime};

use super::contract::{Account, ErrorCode, Failure, Reply, Request, Response};
use super::read::{self, ReadContext};
use super::write::{self, Domain, WriteContext, WritePermission};
use files::{Endpoint, Profile};

const READ_WORKERS: usize = 3;
const WAITING_CALLS: usize = 8;

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

pub fn start(profile: &Path, store: Arc<Store>, sample: SampleProvider) -> anyhow::Result<Server> {
    let profile = Profile::open(profile)?;
    let (endpoint, listener) = profile.listen()?;
    let key = profile.account_key()?;
    let stopped = Arc::new(AtomicBool::new(false));
    let (wake, shutdown) = UnixStream::pair()?;
    let owner = Arc::new(Owner {
        profile,
        key,
        store,
        sample,
        runtime: Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()?,
        stopped: stopped.clone(),
    });
    let (queue, pending) = mpsc::sync_channel(WAITING_CALLS);
    let pending = Arc::new(Mutex::new(pending));
    for index in 0..READ_WORKERS {
        let (owner, pending) = (owner.clone(), pending.clone());
        thread::Builder::new()
            .name(format!("overlook-mcp-read-{index}"))
            .spawn(move || loop {
                let next = pending.lock().unwrap().recv();
                let Ok(mut stream) = next else { break };
                if owner.stopped.load(Ordering::Acquire) {
                    break;
                }
                wire::serve(&mut stream, &owner.stopped, |client, token, request| {
                    owner.call(client, token, request)
                });
            })?;
    }
    let stop_signal = stopped.clone();
    thread::Builder::new()
        .name("overlook-mcp".into())
        .spawn(move || serve(listener, shutdown, queue, stop_signal))?;
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

pub fn grant(
    profile: &Path,
    client: &str,
    account_ids: &[String],
    permissions: BTreeSet<WritePermission>,
) -> anyhow::Result<String> {
    anyhow::ensure!(files::valid_client(client), "Invalid client name");
    // Grants can be provisioned before startup. The owner checks current
    // account identity, connection, and data eligibility at execution.
    anyhow::ensure!(
        !account_ids.is_empty()
            && account_ids.len() <= 64
            && account_ids.iter().all(|id| {
                !id.is_empty()
                    && id.len() <= 256
                    && !id.chars().any(char::is_control)
                    && (matches!(id.as_str(), "sample" | "sample-secondary")
                        || id.starts_with("imap:")
                        || id.starts_with("pop3:"))
            }),
        "Use explicit sample, IMAP, or POP3 account IDs"
    );
    Profile::open(profile)?.grant(client, account_ids, permissions)
}

pub fn sample_database(profile: &Path) -> anyhow::Result<std::path::PathBuf> {
    Profile::open(profile)?.sample_database()
}

pub fn revoke(profile: &Path, client: &str) -> anyhow::Result<()> {
    anyhow::ensure!(files::valid_client(client), "Invalid client name");
    Profile::open(profile)?.revoke(client)
}

fn serve(
    listener: UnixListener,
    shutdown: UnixStream,
    queue: mpsc::SyncSender<UnixStream>,
    stopped: Arc<AtomicBool>,
) {
    while wait_for_request(&listener, &shutdown) && !stopped.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((stream, _)) => {
                if stream.set_nonblocking(true).is_ok() {
                    // Dropping a connection when saturated lets its bridge
                    // report unavailability without an unbounded queue.
                    let _ = queue.try_send(stream);
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

struct Owner {
    profile: Profile,
    key: [u8; 32],
    store: Arc<Store>,
    sample: SampleProvider,
    runtime: Runtime,
    stopped: Arc<AtomicBool>,
}

impl Owner {
    fn call(&self, client: &str, token: &str, request: Request) -> Reply {
        let grant = self.profile.authorize(client, token).ok_or_else(denied)?;
        let mut eligible = Vec::new();
        for account in self
            .store
            .accounts()
            .map_err(|_| Failure::new(ErrorCode::Internal, "Account discovery failed"))?
        {
            if grant.account_ids.contains(&account.id.0)
                && read::eligible_account(&self.store, &account)?
            {
                eligible.push((files::public_id(&self.key, account.id.as_str()), account));
            }
        }
        let lookup = |account_id: &str| {
            eligible
                .iter()
                .find(|(id, _)| id == account_id)
                .map(|(_, account)| account)
                .ok_or_else(denied)
        };

        match request {
            Request::AccountsList => Ok(Response::Accounts {
                accounts: eligible
                    .into_iter()
                    .map(|(account_id, account)| Account {
                        account_id,
                        display_name: account.display_name,
                        email: account.email,
                        provider: account.provider.as_key().to_owned(),
                    })
                    .collect(),
            }),
            Request::AccountCapabilities { account_id } => {
                let mut capabilities = self
                    .context(lookup(&account_id)?, &account_id)
                    .capabilities()?;
                for (key, domain) in [
                    ("mail", Domain::Mail),
                    ("calendar", Domain::Calendar),
                    ("contacts", Domain::Contacts),
                ] {
                    let scopes = grant
                        .permissions
                        .iter()
                        .filter(|scope| scope.domain() == domain)
                        .collect::<Vec<_>>();
                    capabilities.details[key]["write_permissions"] = json!(scopes);
                }
                Ok(Response::Capabilities(capabilities))
            }
            Request::Read {
                name,
                account_ids,
                batch,
                args,
            } => {
                let tool = read::tools()
                    .find(|tool| tool.name == name)
                    .ok_or_else(|| read::invalid("Unknown read tool"))?;
                if account_ids.is_empty()
                    || account_ids.len() > read::MAX_ACCOUNTS
                    || (!batch && account_ids.len() != 1)
                    || (batch && (!read::batch_tool(&name) || args.contains_key("cursor")))
                    || args.contains_key("account_id")
                    || args.contains_key("account_ids")
                    || account_ids.iter().collect::<BTreeSet<_>>().len() != account_ids.len()
                {
                    return Err(read::invalid(
                        "Invalid account selector or cross-account cursor",
                    ));
                }
                // Authorize the complete batch before invoking any operation.
                let mut contexts = account_ids
                    .iter()
                    .map(|id| Ok(self.context(lookup(id)?, id)))
                    .collect::<Result<Vec<_>, Failure>>()?;
                if !batch {
                    return contexts.remove(0).execute(tool, &args).map(Response::Read);
                }
                self.batch(contexts, tool, &args).map(Response::Read)
            }
            Request::Write {
                name,
                account_id,
                idempotency_key,
                args,
            } => {
                let account = lookup(&account_id)?;
                let tool = write::tools()
                    .find(|tool| tool.name == name)
                    .ok_or_else(|| read::invalid("Unknown write tool"))?;
                if !grant.permissions.contains(&tool.permission) {
                    return Err(denied());
                }
                let domain = tool.permission.domain();
                let read = self.context(account, &account_id);
                if (domain == Domain::Calendar && !read.calendar_enabled()?)
                    || (domain == Domain::Contacts && !read.contacts_enabled()?)
                {
                    return Err(Failure::new(
                        ErrorCode::Unsupported,
                        "This account has no eligible connection for this write",
                    ));
                }
                let ctx = WriteContext {
                    account,
                    account_id: &account_id,
                    profile: self.profile.path(),
                    key: &self.key,
                };
                match ctx.execute(&self.store, client, tool, &idempotency_key, &args)? {
                    McpWriteOutcome::Applied { data, queued } => {
                        super::note_write(&account.id, domain, queued);
                        Ok(Response::Read(data))
                    }
                    McpWriteOutcome::Replayed(data) => Ok(Response::Read(data)),
                    McpWriteOutcome::KeyConflict => Err(Failure::new(
                        ErrorCode::Conflict,
                        "This idempotency key was committed for a different request",
                    )),
                }
            }
        }
    }

    fn context<'a>(&'a self, account: &'a LocalAccount, account_id: &'a str) -> ReadContext<'a> {
        ReadContext {
            store: &self.store,
            account,
            account_id,
            sample: &self.sample,
            runtime: &self.runtime,
            key: &self.key,
        }
    }

    fn batch(
        &self,
        contexts: Vec<ReadContext<'_>>,
        tool: &read::ReadTool,
        args: &Map<String, Value>,
    ) -> Result<Value, Failure> {
        let mut partial = false;
        let mut results = Vec::with_capacity(contexts.len());
        for ctx in contexts {
            let result = match ctx.execute(tool, args) {
                Ok(result) => {
                    partial |= result.get("next_cursor").is_some_and(Value::is_string)
                        || result["coverage"]["partial"] == true
                        || result["partial"] == true;
                    result
                }
                Err(error) if error.code == ErrorCode::InvalidRequest => return Err(error),
                Err(error) => {
                    partial = true;
                    json!({"account_id":ctx.account_id,"error":error})
                }
            };
            results.push(result);
        }
        Ok(json!({"results":results,"partial":partial}))
    }
}

fn denied() -> Failure {
    Failure::new(ErrorCode::AccessDenied, "Access denied")
}
