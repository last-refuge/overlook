//! One running Overlook per profile. A later launch hands its request to the running
//! instance over a Unix socket and exits: a `mailto:` link to open, `overlook --quit`, or
//! otherwise a request to bring Overlook forward, with the launch's display variables and
//! activation token so a window hidden in the background can come back on that desktop. The
//! listener thread queues each line and wakes the app's task (`lifecycle::serve_launches`).
//! `overlook --reload-theme` (Omarchy's theme-set hook) tells every running profile to re-read
//! the desktop theme.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

static PENDING: Mutex<Vec<String>> = Mutex::new(Vec::new());
static WAKE: OnceLock<UnboundedSender<()>> = OnceLock::new();
/// The socket this process bound, so it can remove it again (and only that one).
static BOUND: Mutex<Option<PathBuf>> = Mutex::new(None);

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
const LAUNCH_VARIABLES: [&str; 4] = ["WAYLAND_DISPLAY", "DISPLAY", "XDG_RUNTIME_DIR", "XDG_ACTIVATION_TOKEN"];

/// The desktop a launch came from. Only the names in `LAUNCH_VARIABLES` are kept.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct LaunchEnv(Vec<(String, String)>);

impl LaunchEnv {
    /// This process's own, read before GPUI takes the activation token out of it.
    pub fn current() -> Self {
        Self(
            LAUNCH_VARIABLES
                .iter()
                .filter_map(|name| Some((name.to_string(), std::env::var(name).ok().filter(|v| !v.is_empty())?)))
                .collect(),
        )
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.0.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
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
            head if head.to_ascii_lowercase().starts_with("mailto:") => Some(Self::Mailto(line.trim().to_owned())),
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

fn socket_dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir)
}

fn socket_path() -> PathBuf {
    let profile = std::env::var("OVERLOOK_DATA_DIR").unwrap_or_default();
    let mut h = DefaultHasher::new();
    profile.hash(&mut h);
    socket_dir().join(format!("overlook-{:08x}.sock", h.finish() as u32))
}

/// Hands `line` to every running instance, whatever its profile. Stale sockets are skipped.
pub fn broadcast(line: &str) {
    let Ok(entries) = std::fs::read_dir(socket_dir()) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("overlook-") && name.ends_with(".sock") {
            if let Ok(mut stream) = UnixStream::connect(entry.path()) {
                let _ = stream.write_all(format!("{line}\n").as_bytes());
            }
        }
    }
}

/// Hands `line` to a running instance. `false` when none is listening.
pub fn send(line: &str) -> bool {
    send_to(&socket_path(), line)
}

/// Whether an instance is listening for this profile.
pub fn running() -> bool {
    UnixStream::connect(socket_path()).is_ok()
}

fn send_to(path: &Path, line: &str) -> bool {
    let Ok(mut stream) = UnixStream::connect(path) else { return false };
    stream.write_all(format!("{line}\n").as_bytes()).is_ok()
}

/// Queues a line for the UI to pick up, and wakes it.
pub fn push(line: String) {
    PENDING.lock().unwrap().push(line);
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

/// Claims the profile and starts listening. `false` means another instance already owns
/// it. Called before opening the store, so concurrent launches cannot start two controllers.
pub fn listen() -> std::io::Result<bool> {
    listen_at(socket_path())
}

fn listen_at(path: PathBuf) -> std::io::Result<bool> {
    // Serialize the stale-socket check, removal and bind. Keep this lock file in place:
    // unlinking it would let another launch lock a different inode for the same profile.
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path.with_extension("lock"))?;
    // SAFETY: `lock` owns a valid descriptor for this call and until it is dropped on
    // return. Closing the descriptor releases the startup lock after the socket is bound.
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    if UnixStream::connect(&path).is_ok() {
        return Ok(false);
    }
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path)?;
    std::thread::Builder::new()
        .name("overlook-instance".into())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                let reader = BufReader::new(stream);
                for line in reader.lines().map_while(Result::ok) {
                    if !line.trim().is_empty() {
                        push(line);
                    }
                }
            }
        })?;
    *BOUND.lock().unwrap() = Some(path);
    Ok(true)
}

/// Stops later launches from finding this instance, so the copy that replaces it (after an
/// update) starts on its own instead of handing its launch back to the one that is quitting.
pub fn release() {
    if let Some(path) = BOUND.lock().unwrap().take() {
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_released_instance_no_longer_takes_launches() {
        let dir = std::env::temp_dir().join(format!("overlook-si-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("overlook-test.sock");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(4));
        let attempts: Vec<_> = (0..4).map(|_| {
            let (path, barrier) = (path.clone(), barrier.clone());
            std::thread::spawn(move || {
                barrier.wait();
                listen_at(path).unwrap()
            })
        }).collect();
        let owners = attempts.into_iter().map(|attempt| attempt.join().unwrap()).filter(|owned| *owned).count();
        assert_eq!(owners, 1, "concurrent launches must share one listener");
        assert!(send_to(&path, SHOW), "the listening instance takes the launch");
        assert!(UnixStream::connect(&path).is_ok());
        release();
        assert!(!send_to(&path, SHOW), "after release a launch starts its own copy");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn launch_lines_parse_into_requests() {
        assert_eq!(Request::parse("overlook:show"), Some(Request::Show(LaunchEnv::default())));
        assert_eq!(Request::parse("overlook:quit"), Some(Request::Quit));
        assert_eq!(Request::parse("overlook:sync"), Some(Request::SyncNow));
        assert_eq!(Request::parse("overlook:reload-theme\r"), Some(Request::ReloadTheme));
        assert_eq!(Request::parse("MAILTO:maya@example.com"), Some(Request::Mailto("MAILTO:maya@example.com".into())));
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
        let Some(Request::Show(back)) = Request::parse(&line) else { panic!("{line}") };
        assert_eq!(back.get("WAYLAND_DISPLAY"), Some("wayland-1"));
        assert_eq!(back.get("XDG_ACTIVATION_TOKEN"), Some("token=with=equals"));
        assert_eq!(back.get("DISPLAY"), None, "a value that would split the line is left out");
        // Only the desktop's own variables are taken from a line.
        let Some(Request::Show(other)) = Request::parse("overlook:show\tPATH=/tmp\tWAYLAND_DISPLAY=") else { panic!() };
        assert_eq!(other, LaunchEnv::default());
    }
}
