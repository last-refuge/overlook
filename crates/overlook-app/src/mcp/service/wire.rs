//! One versioned JSON exchange per connection, prefixed by a big-endian u32 length.

use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::{ffi::OsStrExt, net::UnixStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::super::contract::{ErrorCode, Failure, Reply, Request, VERSION};

const MAX_REQUEST: usize = 16 * 1024;
const MAX_REPLY: usize = 256 * 1024;
const FRAME_TIMEOUT: Duration = Duration::from_secs(2);
const CLIENT_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Serialize)]
struct RequestFrame<'a> {
    version: u32,
    client: &'a str,
    token: &'a str,
    request: Request,
}

#[derive(Deserialize)]
struct Invocation {
    client: String,
    token: String,
    request: Request,
}

#[derive(Serialize, Deserialize)]
struct ReplyFrame {
    version: u32,
    reply: Reply,
}

pub(super) fn call(stream: &mut UnixStream, client: &str, token: &str, request: Request) -> Reply {
    if let Request::AccountCapabilities { account_id } = &request {
        if account_id.len() > MAX_REQUEST / 2 {
            return Err(super::denied());
        }
    }
    let deadline = Instant::now() + CLIENT_TIMEOUT;
    let request = serde_json::to_vec(&RequestFrame {
        version: VERSION,
        client,
        token,
        request,
    })
    .map_err(|_| invalid())?;
    if request.len() > MAX_REQUEST {
        return Err(super::denied());
    }
    write_frame(stream, &request, deadline).map_err(|_| unavailable())?;
    let reply = read_frame(stream, MAX_REPLY, deadline).map_err(|_| unavailable())?;
    let reply: serde_json::Value = serde_json::from_slice(&reply).map_err(|_| invalid())?;
    if reply["version"].as_u64() != Some(u64::from(VERSION)) {
        return Err(unsupported());
    }
    let reply: ReplyFrame = serde_json::from_value(reply).map_err(|_| invalid())?;
    reply.reply
}

pub(super) fn serve(
    stream: &mut UnixStream,
    stopped: &AtomicBool,
    dispatch: impl FnOnce(&str, &str, Request) -> Reply,
) {
    let deadline = Instant::now() + FRAME_TIMEOUT;
    let reply = read_frame(stream, MAX_REQUEST, deadline)
        .map_err(|_| invalid())
        .and_then(parse)
        .and_then(|invocation| {
            if stopped.load(Ordering::Acquire) {
                return Err(unavailable());
            }
            dispatch(&invocation.client, &invocation.token, invocation.request)
        });
    // A request that was dispatched before shutdown may have committed a
    // write. Return its reply while the foreground owner drains this worker.
    let response = ReplyFrame {
        version: VERSION,
        reply,
    };
    let Ok(mut bytes) = serde_json::to_vec(&response) else {
        return;
    };
    if bytes.len() > MAX_REPLY {
        let failure = Failure::new(
            ErrorCode::Internal,
            "The tool reply exceeded the profile service size limit",
        );
        let Ok(fallback) = serde_json::to_vec(&ReplyFrame {
            version: VERSION,
            reply: Err(failure),
        }) else {
            return;
        };
        bytes = fallback;
    }
    let _ = write_frame(stream, &bytes, Instant::now() + FRAME_TIMEOUT);
}

fn parse(bytes: Vec<u8>) -> Result<Invocation, Failure> {
    let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
    let version = value
        .get("version")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(invalid)?;
    if version != u64::from(VERSION) {
        return Err(unsupported());
    }
    serde_json::from_value(value).map_err(|_| invalid())
}

fn read_frame(stream: &mut UnixStream, max: usize, deadline: Instant) -> io::Result<Vec<u8>> {
    let mut header = [0; 4];
    read_exact(stream, &mut header, deadline)?;
    let len = u32::from_be_bytes(header) as usize;
    if len == 0 || len > max {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Invalid frame length",
        ));
    }
    let mut bytes = vec![0; len];
    read_exact(stream, &mut bytes, deadline)?;
    Ok(bytes)
}

fn read_exact(stream: &mut UnixStream, mut bytes: &mut [u8], deadline: Instant) -> io::Result<()> {
    while !bytes.is_empty() {
        remaining(deadline)?;
        match stream.read(bytes) {
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(len) => bytes = &mut bytes[len..],
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                wait_ready(stream, libc::POLLIN, deadline)?;
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        }
    }
    Ok(())
}

fn write_frame(stream: &mut UnixStream, bytes: &[u8], deadline: Instant) -> io::Result<()> {
    let header = (bytes.len() as u32).to_be_bytes();
    for mut chunk in [header.as_slice(), bytes] {
        while !chunk.is_empty() {
            remaining(deadline)?;
            match stream.write(chunk) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(len) => chunk = &chunk[len..],
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                    wait_ready(stream, libc::POLLOUT, deadline)?;
                }
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(err) => return Err(err),
            }
        }
    }
    Ok(())
}

fn remaining(deadline: Instant) -> io::Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|duration| !duration.is_zero())
        .ok_or_else(|| io::ErrorKind::TimedOut.into())
}

// Use a single absolute deadline even if the peer trickles bytes or never reads.
fn wait_ready(stream: &UnixStream, events: libc::c_short, deadline: Instant) -> io::Result<()> {
    let mut ready = libc::pollfd {
        fd: stream.as_raw_fd(),
        events,
        revents: 0,
    };
    loop {
        let timeout = remaining(deadline)?.as_nanos().div_ceil(1_000_000) as libc::c_int;
        // SAFETY: ready points to one initialized pollfd for a valid stream.
        match unsafe { libc::poll(&mut ready, 1, timeout) } {
            0 => return Err(io::ErrorKind::TimedOut.into()),
            -1 => {
                let err = io::Error::last_os_error();
                if err.kind() != io::ErrorKind::Interrupted {
                    return Err(err);
                }
            }
            _ => return Ok(()),
        }
    }
}

pub(super) fn connect(path: &Path) -> io::Result<UnixStream> {
    connect_until(path, Instant::now() + CLIENT_TIMEOUT)
}

pub(crate) fn connect_until(path: &Path, deadline: Instant) -> io::Result<UnixStream> {
    // UnixStream::connect can block when the owner's listen queue fills.
    // SAFETY: zero is a valid initial representation for sockaddr_un.
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    let bytes = path.as_os_str().as_bytes();
    if bytes.len() >= address.sun_path.len() || bytes.contains(&0) {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (dest, src) in address.sun_path.iter_mut().zip(bytes) {
        *dest = *src as libc::c_char;
    }
    let len =
        (std::mem::offset_of!(libc::sockaddr_un, sun_path) + bytes.len() + 1) as libc::socklen_t;
    #[cfg(target_os = "macos")]
    {
        address.sun_len = len as u8;
    }

    let stream = client_socket()?;
    stream.set_nonblocking(true)?;
    // SAFETY: address is initialized and len covers only its used portion.
    let result = unsafe {
        libc::connect(
            stream.as_raw_fd(),
            (&address as *const libc::sockaddr_un).cast(),
            len,
        )
    };
    if result != 0 {
        let err = io::Error::last_os_error();
        if err.raw_os_error() != Some(libc::EINPROGRESS) {
            return Err(err);
        }
        wait_ready(&stream, libc::POLLOUT, deadline)?;
        if let Some(err) = stream.take_error()? {
            return Err(err);
        }
    }
    Ok(stream)
}

fn client_socket() -> io::Result<UnixStream> {
    #[cfg(target_os = "linux")]
    let kind = libc::SOCK_STREAM | libc::SOCK_CLOEXEC;
    #[cfg(target_os = "macos")]
    let kind = libc::SOCK_STREAM;
    // SAFETY: these arguments specify a Unix stream socket with the default protocol.
    let fd = unsafe { libc::socket(libc::AF_UNIX, kind, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a successful socket call returns a new owned file descriptor.
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    #[cfg(target_os = "macos")]
    {
        let enabled: libc::c_int = 1;
        // SAFETY: fd is valid, and enabled contains the option's integer value.
        if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } < 0
            || unsafe {
                libc::setsockopt(
                    fd.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_NOSIGPIPE,
                    (&enabled as *const libc::c_int).cast(),
                    std::mem::size_of_val(&enabled) as libc::socklen_t,
                )
            } < 0
        {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(UnixStream::from(fd))
}

fn invalid() -> Failure {
    Failure::new(ErrorCode::InvalidRequest, "Invalid profile service request")
}

fn unsupported() -> Failure {
    Failure::new(
        ErrorCode::UnsupportedVersion,
        "Unsupported profile service version",
    )
}

fn unavailable() -> Failure {
    Failure::new(ErrorCode::ProfileUnavailable, "Profile unavailable")
}
