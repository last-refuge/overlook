//! One running Overlook per profile. A second launch with a `mailto:` link hands the link to
//! the running instance over a Unix socket and exits; the running instance drains the queue
//! from its UI loop. `overlook --reload-theme` (Omarchy's theme-set hook) tells every running
//! profile to re-read the desktop theme.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::Mutex;

static PENDING: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// The line that asks a running instance to re-read the desktop theme.
pub const RELOAD_THEME: &str = "overlook:reload-theme";

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
    let Ok(mut stream) = UnixStream::connect(socket_path()) else { return false };
    stream.write_all(format!("{line}\n").as_bytes()).is_ok()
}

/// Queues a line for the UI to pick up.
pub fn push(line: String) {
    PENDING.lock().unwrap().push(line);
}

/// Everything queued since the last call.
pub fn take() -> Vec<String> {
    std::mem::take(&mut *PENDING.lock().unwrap())
}

/// Starts listening for lines from later launches. A stale socket from a crashed instance
/// is replaced.
pub fn listen() {
    let path = socket_path();
    if UnixStream::connect(&path).is_ok() {
        return;
    }
    let _ = std::fs::remove_file(&path);
    let Ok(listener) = UnixListener::bind(&path) else {
        tracing::warn!("could not listen on {}", path.display());
        return;
    };
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
        })
        .ok();
}
