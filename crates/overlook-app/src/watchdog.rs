//! UI-thread watchdog. The window beats a heartbeat every 200 ms; a separate thread
//! checks it. When the UI thread misses beats for a second or more, the stall is logged
//! with how long it lasted, the last step the window recorded, and what the thread was
//! doing in the kernel meanwhile (running, or waiting on a lock, the disk, the GPU…).
//! The log goes to `overlook.log` in the data folder, so a stall can be traced after the
//! fact. Nothing here runs on the UI thread except `beat` and `mark`.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicI32, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

static BEAT_MS: AtomicU64 = AtomicU64::new(0);
static UI_TID: AtomicI32 = AtomicI32::new(0);
static MARK: Mutex<&'static str> = Mutex::new("startup");

fn epoch() -> &'static Instant {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    EPOCH.get_or_init(Instant::now)
}

fn now_ms() -> u64 {
    epoch().elapsed().as_millis() as u64
}

/// How long the UI thread may miss beats before it counts as stalled.
const STALL: Duration = Duration::from_millis(1000);

/// Called on the UI thread: records it and starts the watcher.
pub fn start() {
    // The main window can open again (after running in the background); one watcher is enough.
    static STARTED: std::sync::Once = std::sync::Once::new();
    STARTED.call_once(|| {
        UI_TID.store(unsafe { libc::gettid() }, Ordering::Relaxed);
        beat();
        let _ = std::thread::Builder::new().name("overlook-watchdog".into()).spawn(watch);
    });
}

/// The UI thread is alive.
pub fn beat() {
    BEAT_MS.store(now_ms(), Ordering::Relaxed);
}

/// What the UI thread is about to do, for the stall report.
pub fn mark(step: &'static str) {
    if let Ok(mut m) = MARK.try_lock() {
        *m = step;
    }
}

/// The UI thread's scheduler state and wait channel, e.g. ("S", "futex_wait_queue").
fn kernel_state(tid: i32) -> Option<String> {
    let stat = std::fs::read_to_string(format!("/proc/self/task/{tid}/stat")).ok()?;
    let state = stat.rsplit_once(')')?.1.split_whitespace().next()?.to_owned();
    let wchan = std::fs::read_to_string(format!("/proc/self/task/{tid}/wchan")).unwrap_or_default();
    Some(if state == "R" { "running".to_owned() } else { format!("{state}:{}", wchan.trim()) })
}

fn watch() {
    let mut stall: Option<(u64, &'static str, BTreeMap<String, u32>, bool)> = None;
    loop {
        std::thread::sleep(Duration::from_millis(100));
        let lag = now_ms().saturating_sub(BEAT_MS.load(Ordering::Relaxed));
        let tid = UI_TID.load(Ordering::Relaxed);
        if lag >= STALL.as_millis() as u64 {
            let s = stall.get_or_insert_with(|| (lag, *MARK.lock().unwrap(), BTreeMap::new(), false));
            s.0 = lag;
            if let Some(k) = kernel_state(tid) {
                *s.2.entry(k).or_default() += 1;
            }
            // Report a long stall while it lasts too, in case the window is closed over it.
            if lag >= 3000 && !s.3 {
                s.3 = true;
                tracing::warn!("UI thread stalled for {lag} ms so far; last step: {}; thread state: {}", s.1, summary(&s.2));
            }
        } else if let Some((lag, step, states, _)) = stall.take() {
            tracing::warn!("UI thread stalled for {lag} ms; last step: {step}; thread state: {}", summary(&states));
        }
    }
}

fn summary(states: &BTreeMap<String, u32>) -> String {
    let mut v: Vec<(&String, &u32)> = states.iter().collect();
    v.sort_by(|a, b| b.1.cmp(a.1));
    v.iter().take(4).map(|(k, n)| format!("{k}×{n}")).collect::<Vec<_>>().join(", ")
}
