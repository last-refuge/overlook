//! One running Overlook per profile. A later launch hands its request to the running
//! instance over a Unix socket and exits: a `mailto:` link to open, `overlook --quit`, or
//! otherwise a request to bring Overlook forward, with the launch's display variables and
//! activation token so a window hidden in the background can come back on that desktop. The
//! listener thread queues each line and wakes the app's task (`lifecycle::serve_launches`).
//! `overlook --reload-theme` (Omarchy's theme-set hook) tells every running profile to re-read
//! the desktop theme.

use sha2::{Digest, Sha256};
use std::collections::hash_map::DefaultHasher;
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::hash::{Hash, Hasher};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::Shutdown;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Mutex, OnceLock,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

static PENDING: Mutex<Vec<String>> = Mutex::new(Vec::new());
static WAKE: OnceLock<UnboundedSender<()>> = OnceLock::new();
static DESKTOP_STOPPING: AtomicBool = AtomicBool::new(false);

const DESKTOP_LAUNCH: &str = "overlook:desktop\t";
const HANDOFF: &str = "overlook:headless-handoff";
const LAUNCH_TIMEOUT: Duration = Duration::from_secs(30);
const IO_TIMEOUT: Duration = Duration::from_millis(500);
const MAX_LINE: u64 = 16 * 1024;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Desktop,
    Headless,
    Sample,
}

/// The existing launch arbitration now also covers the lifetime of the Store.
/// The lock inode is permanent and independent of the launch's environment.
pub struct Owner {
    profile: PathBuf,
    path: PathBuf,
    shutdown: UnixStream,
    listener: Option<JoinHandle<()>>,
    _lock: File,
}

impl Owner {
    pub fn profile(&self) -> &Path {
        &self.profile
    }

    pub fn close_listener(&mut self) {
        if let Some(listener) = self.listener.take() {
            let _ = self.shutdown.shutdown(Shutdown::Both);
            let _ = listener.join();
            let _ = fs::remove_file(&self.path);
        }
    }
}

impl Drop for Owner {
    fn drop(&mut self) {
        self.close_listener();
    }
}

/// The line that asks a running instance to re-read the desktop theme.
pub const RELOAD_THEME: &str = "overlook:reload-theme";
/// The line a plain launch sends: show the running Overlook instead of starting another.
/// Tab-separated `NAME=value` pairs from `LaunchEnv` may follow.
pub const SHOW: &str = "overlook:show";
/// The line `overlook --quit` sends.
pub const QUIT: &str = "overlook:quit";
/// Check for new mail now (the tray icon's menu).
pub const SYNC: &str = "overlook:sync";

/// The variables that name a launch's desktop, passed along with a show request.
const LAUNCH_VARIABLES: [&str; 4] = [
    "WAYLAND_DISPLAY",
    "DISPLAY",
    "XDG_RUNTIME_DIR",
    "XDG_ACTIVATION_TOKEN",
];

/// The desktop a launch came from. Only the names in `LAUNCH_VARIABLES` are kept.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct LaunchEnv(Vec<(String, String)>);

impl LaunchEnv {
    /// This process's own, read before GPUI takes the activation token out of it.
    pub fn current() -> Self {
        Self(
            LAUNCH_VARIABLES
                .iter()
                .filter_map(|name| {
                    Some((
                        name.to_string(),
                        std::env::var(name).ok().filter(|v| !v.is_empty())?,
                    ))
                })
                .collect(),
        )
    }

    #[cfg(any(target_os = "linux", test))]
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    /// The show line carrying this environment.
    pub fn show_line(&self) -> String {
        let mut line = SHOW.to_owned();
        for (name, value) in &self.0 {
            // A value with a tab or line break would split the line; it can't name a display.
            if !value.contains(['\t', '\n', '\r']) {
                line.push('\t');
                line.push_str(&format!("{name}={value}"));
            }
        }
        line
    }

    fn parse(fields: &str) -> Self {
        Self(
            fields
                .split('\t')
                .filter_map(|pair| pair.split_once('='))
                .filter(|(name, value)| LAUNCH_VARIABLES.contains(name) && !value.is_empty())
                .map(|(name, value)| (name.to_owned(), value.to_owned()))
                .collect(),
        )
    }
}

/// What a line from a later launch asks for.
#[derive(Debug, PartialEq, Eq)]
pub enum Request {
    Show(LaunchEnv),
    Quit,
    SyncNow,
    ReloadTheme,
    Mailto(String),
}

impl Request {
    /// `None` for a line this version doesn't understand (a newer launch, say).
    pub fn parse(line: &str) -> Option<Self> {
        let line = line.trim_end_matches(['\r', '\n']);
        let (head, fields) = line.split_once('\t').unwrap_or((line, ""));
        match head.trim() {
            SHOW => Some(Self::Show(LaunchEnv::parse(fields))),
            QUIT => Some(Self::Quit),
            SYNC => Some(Self::SyncNow),
            RELOAD_THEME => Some(Self::ReloadTheme),
            head if head.to_ascii_lowercase().starts_with("mailto:") => {
                Some(Self::Mailto(line.trim().to_owned()))
            }
            _ => None,
        }
    }

    /// The kind alone, for the log: a mailto link names people.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Show(_) => "show",
            Self::Quit => "quit",
            Self::SyncNow => "check for new mail",
            Self::ReloadTheme => "reload theme",
            Self::Mailto(_) => "mailto",
        }
    }
}

fn uid() -> u32 {
    // SAFETY: geteuid has no arguments or preconditions.
    unsafe { libc::geteuid() }
}

fn socket_dir() -> PathBuf {
    // A short, stable path works across GUI, terminal, and service environments.
    PathBuf::from(format!("/tmp/overlook-launch-{}", uid()))
}

fn check_socket_dir(dir: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(dir)?;
    if !meta.is_dir() || meta.uid() != uid() || meta.mode() & 0o077 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "insecure launch directory",
        ));
    }
    Ok(())
}

fn socket_path(profile: &Path) -> PathBuf {
    let digest = Sha256::digest(profile.as_os_str().as_bytes());
    socket_dir().join(format!("{digest:x}.sock"))
}

fn check_legacy_owner(profile: &Path) -> io::Result<()> {
    // Before the lifetime lock, Overlook used the launcher's runtime directory.
    // Recognize a running copy on upgrade before opening the Store again.
    let default = crate::default_data_dir().canonicalize().ok();
    let identity = if default.as_deref() == Some(profile) {
        Path::new("")
    } else {
        profile
    };
    let mut digest = DefaultHasher::new();
    match identity.to_str() {
        Some(path) => path.hash(&mut digest),
        None => identity.hash(&mut digest),
    }
    let name = format!("overlook-{:08x}.sock", digest.finish() as u32);
    let directories = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .into_iter()
        .chain([std::env::temp_dir(), PathBuf::from("/tmp")]);
    for dir in directories {
        let socket = dir.join(&name);
        if fs::symlink_metadata(&socket)
            .is_ok_and(|meta| meta.file_type().is_socket() && meta.uid() == uid())
            && connect(&socket).is_ok()
        {
            return Err(io::Error::new(io::ErrorKind::AlreadyExists, "an earlier version of Overlook is running for this profile. Quit it before starting this version"));
        }
    }
    Ok(())
}

/// Hands `line` to every running instance, whatever its profile. Stale sockets are skipped.
pub fn broadcast(line: &str) {
    let dir = socket_dir();
    if check_socket_dir(&dir).is_err() {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.path().extension().is_some_and(|ext| ext == "sock") {
            send_to(&entry.path(), line);
        }
    }
}

/// Hands `line` to a running instance. `false` when none is listening.
pub fn send(line: &str) -> bool {
    let Ok(profile) = crate::data_dir().canonicalize() else {
        return false;
    };
    check_socket_dir(&socket_dir()).is_ok() && send_to(&socket_path(&profile), line)
}

fn send_to(path: &Path, line: &str) -> bool {
    let Ok(mut stream) = connect(path) else {
        return false;
    };
    writeln!(stream, "{line}").is_ok()
        && read_line(&mut stream).is_ok_and(|reply| reply == "accepted")
}

fn connect(path: &Path) -> io::Result<UnixStream> {
    let stream = crate::mcp::connect_until(path, Instant::now() + IO_TIMEOUT)?;
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    Ok(stream)
}

/// Queues a line for the UI to pick up, and wakes it.
pub fn push(line: String) {
    PENDING.lock().unwrap().push(line);
    wake();
}

pub(crate) fn wake() {
    if let Some(wake) = WAKE.get() {
        let _ = wake.send(());
    }
}

/// A wake-up for each queued line. Call once; lines queued before it are found by the
/// first `take`.
pub fn wakeups() -> UnboundedReceiver<()> {
    let (tx, rx) = unbounded_channel();
    if WAKE.set(tx).is_err() {
        tracing::warn!("single_instance::wakeups called twice");
    }
    rx
}

/// Everything queued since the last call.
pub fn take() -> Vec<String> {
    std::mem::take(&mut *PENDING.lock().unwrap())
}

/// The GUI may still have background jobs on quit or update. Keep its ownership
/// until process exit and have new launches wait for the kernel to release it.
pub fn desktop_stopping() {
    DESKTOP_STOPPING.store(true, Ordering::Release);
}

pub fn listen(profile: &Path, role: Role) -> io::Result<Option<Owner>> {
    let profile = profile.canonicalize()?;
    check_legacy_owner(&profile)?;
    listen_at(&profile, &socket_path(&profile), role)
}

fn listen_at(profile: &Path, path: &Path, role: Role) -> io::Result<Option<Owner>> {
    let dir = path.parent().unwrap();
    match DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    check_socket_dir(dir)?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(profile.join(".overlook-launch.lock"))?;
    let meta = lock.metadata()?;
    if !meta.is_file() || meta.uid() != uid() || meta.mode() & 0o077 != 0 || meta.nlink() != 1 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "insecure profile launch lock",
        ));
    }
    // SAFETY: the owned descriptor is valid and stays open until after the listener closes.
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        let error = io::Error::last_os_error();
        return if error.kind() == io::ErrorKind::WouldBlock {
            Ok(None)
        } else {
            Err(error)
        };
    }
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_socket() && meta.uid() == uid() => fs::remove_file(path)?,
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "unexpected launch socket entry",
            ))
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => return Err(err),
    }
    let (shutdown, stop) = UnixStream::pair()?;
    let listener = UnixListener::bind(path)?;
    let mut owner = Owner {
        profile: profile.into(),
        path: path.into(),
        shutdown,
        listener: None,
        _lock: lock,
    };
    match thread::Builder::new()
        .name("overlook-instance".into())
        .spawn(move || {
            while listener_ready(&listener, &stop) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let _ = receive(stream, role);
                    }
                    Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
        }) {
        Ok(listener) => owner.listener = Some(listener),
        Err(err) => {
            let _ = fs::remove_file(path);
            return Err(err);
        }
    }
    Ok(Some(owner))
}

pub(crate) fn listener_ready(listener: &UnixListener, stop: &UnixStream) -> bool {
    let mut fds = [listener.as_raw_fd(), stop.as_raw_fd()].map(|fd| libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    });
    loop {
        // SAFETY: Both descriptors are alive and fds has room for both results.
        if unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, -1) } >= 0 {
            return fds[1].revents == 0 && fds[0].revents & libc::POLLIN != 0;
        }
        if io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
            return false;
        }
    }
}

fn receive(mut stream: UnixStream, role: Role) -> io::Result<()> {
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    let line = read_line(&mut stream)?;
    if let Some(launch) = line.strip_prefix(DESKTOP_LAUNCH) {
        if !launch.is_empty()
            && !matches!(
                Request::parse(launch),
                Some(Request::Show(_) | Request::Mailto(_))
            )
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid desktop launch",
            ));
        }
        let reply = match role {
            Role::Desktop if DESKTOP_STOPPING.load(Ordering::Acquire) => "wait",
            Role::Desktop => {
                if !launch.is_empty() {
                    push(launch.to_owned());
                }
                "forwarded"
            }
            Role::Headless if !launch.is_empty() => {
                push(HANDOFF.into());
                "wait"
            }
            Role::Headless => "forwarded", // Background startup leaves the owner alone.
            Role::Sample => "sample",
        };
        writeln!(stream, "{reply}")?;
    } else if !line.is_empty() && line != HANDOFF {
        push(line);
        writeln!(stream, "accepted")?;
    }
    Ok(())
}

fn read_line(stream: &mut UnixStream) -> io::Result<String> {
    let mut line = String::new();
    BufReader::new(stream.take(MAX_LINE)).read_line(&mut line)?;
    if !line.ends_with('\n') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "incomplete launch request",
        ));
    }
    line.truncate(line.trim_end_matches(['\r', '\n']).len());
    Ok(line)
}

pub fn headless_stop_requested(line: &str) -> bool {
    line == HANDOFF || matches!(Request::parse(line), Some(Request::Quit))
}

pub fn launch_desktop(profile: &Path, line: Option<&str>) -> io::Result<Option<Owner>> {
    let profile = profile.canonicalize()?;
    check_legacy_owner(&profile)?;
    let path = socket_path(&profile);
    launch_at(&profile, &path, line, LAUNCH_TIMEOUT)
}

fn launch_at(
    profile: &Path,
    path: &Path,
    line: Option<&str>,
    timeout: Duration,
) -> io::Result<Option<Owner>> {
    let deadline = Instant::now() + timeout;
    let command = format!("{DESKTOP_LAUNCH}{}", line.unwrap_or_default());
    if command.contains(['\r', '\n']) || command.len() as u64 >= MAX_LINE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid desktop launch request",
        ));
    }
    loop {
        if let Some(owner) = listen_at(profile, path, Role::Desktop)? {
            return Ok(Some(owner));
        }
        let reply = connect(path).and_then(|mut stream| {
            writeln!(stream, "{command}")?;
            read_line(&mut stream)
        });
        match reply.as_deref() {
            Ok("forwarded") => return Ok(None),
            Ok("sample") => {
                return Err(io::Error::other(
                    "this directory is reserved for the MCP sample profile",
                ))
            }
            _ => {}
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "the current profile owner did not finish shutting down",
            ));
        }
        thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrent_launches_share_one_owner_and_hold_the_lock_after_unlink() {
        let dir = tempfile::tempdir_in("/tmp").unwrap();
        let path = dir.path().join("run/test.sock");
        let barrier = std::sync::Barrier::new(4);
        let mut owners = thread::scope(|scope| {
            let attempts: Vec<_> = (0..4)
                .map(|_| {
                    let (path, barrier, profile) = (&path, &barrier, dir.path());
                    scope.spawn(move || {
                        barrier.wait();
                        listen_at(profile, path, Role::Headless).unwrap()
                    })
                })
                .collect();
            attempts
                .into_iter()
                .filter_map(|attempt| attempt.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert_eq!(owners.len(), 1);
        let mut owner = owners.pop().unwrap();
        owner.close_listener();
        assert!(listen_at(dir.path(), &path, Role::Desktop)
            .unwrap()
            .is_none());
        drop(owner);
        assert!(listen_at(dir.path(), &path, Role::Desktop)
            .unwrap()
            .is_some());
    }

    #[test]
    fn desktop_requests_release_only_from_the_headless_owner() {
        let dir = tempfile::tempdir_in("/tmp").unwrap();
        let profile = dir.path();
        let path = profile.join("run/test.sock");
        let mut headless = listen_at(profile, &path, Role::Headless).unwrap().unwrap();

        assert!(launch_at(profile, &path, None, Duration::ZERO)
            .unwrap()
            .is_none());
        assert!(
            take().is_empty(),
            "background startup must not displace headless"
        );

        let err = launch_at(profile, &path, Some(SHOW), Duration::ZERO)
            .err()
            .unwrap();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
        assert_eq!(take(), [HANDOFF]);

        // The listener has closed, but native work would still be allowed to
        // finish. A second attempt must time out until the lease is dropped.
        headless.close_listener();
        let err = launch_at(profile, &path, Some(SHOW), Duration::ZERO)
            .err()
            .unwrap();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
        assert!(listen_at(profile, &path, Role::Headless).unwrap().is_none());
        drop(headless);

        let desktop = launch_at(profile, &path, Some(SHOW), Duration::ZERO)
            .unwrap()
            .unwrap();
        let mailto = "mailto:fictional@example.com";
        assert!(launch_at(profile, &path, Some(mailto), Duration::ZERO)
            .unwrap()
            .is_none());
        assert_eq!(take(), [mailto]);
        assert!(listen_at(profile, &path, Role::Headless).unwrap().is_none());
        drop(desktop);
    }

    #[test]
    fn launch_lines_parse_into_requests() {
        assert_eq!(
            Request::parse("overlook:show"),
            Some(Request::Show(LaunchEnv::default()))
        );
        assert_eq!(Request::parse("overlook:quit"), Some(Request::Quit));
        assert_eq!(Request::parse("overlook:sync"), Some(Request::SyncNow));
        assert_eq!(
            Request::parse("overlook:reload-theme\r"),
            Some(Request::ReloadTheme)
        );
        assert_eq!(
            Request::parse("MAILTO:maya@example.com"),
            Some(Request::Mailto("MAILTO:maya@example.com".into()))
        );
        assert_eq!(Request::parse("overlook:something-newer"), None);
        assert_eq!(Request::parse("https://example.com"), None);
    }

    #[test]
    fn a_show_request_carries_the_launch_desktop() {
        let env = LaunchEnv(vec![
            ("WAYLAND_DISPLAY".into(), "wayland-1".into()),
            ("XDG_ACTIVATION_TOKEN".into(), "token=with=equals".into()),
            ("DISPLAY".into(), "bad\tvalue".into()),
        ]);
        let line = env.show_line();
        let Some(Request::Show(back)) = Request::parse(&line) else {
            panic!("{line}")
        };
        assert_eq!(back.get("WAYLAND_DISPLAY"), Some("wayland-1"));
        assert_eq!(back.get("XDG_ACTIVATION_TOKEN"), Some("token=with=equals"));
        assert_eq!(
            back.get("DISPLAY"),
            None,
            "a value that would split the line is left out"
        );
        // Only the desktop's own variables are taken from a line.
        let Some(Request::Show(other)) =
            Request::parse("overlook:show\tPATH=/tmp\tWAYLAND_DISPLAY=")
        else {
            panic!()
        };
        assert_eq!(other, LaunchEnv::default());
    }
}
