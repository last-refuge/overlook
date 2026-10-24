//! Linux private-pipe Chromium renderer. No UI work or mail access happens here.
//!
//! One sandboxed process per document (at most six live: open documents, a pre-render
//! and one warm spare), rather than a shared browser: this keeps failures and lifetime
//! isolation simple. The warm spare is a process started ahead of time and handed to
//! exactly one document; it is never reused for another. The last handle
//! signals cancellation; the worker kills/reaps Chromium and removes its 0700
//! profile. No screenshots or response bodies are persisted by the host.
//! Remote resources are fetched by a separate, cookie-less host agent, never by
//! Chromium. Redirects are bounded and every destination must be public.
#![allow(dead_code)] // UI integration is deliberately a separate change.

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use serde_json::{json, Value};
use std::{
    collections::VecDeque,
    fs,
    io::{Read, Write},
    net::IpAddr,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::{fs::DirBuilderExt, net::UnixStream, process::{CommandExt, ExitStatusExt}},
    },
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

const MAX_HTML: usize = 8 * 1024 * 1024;
const MAX_PACKET: usize = 32 * 1024 * 1024;
const MAX_RESOURCE: u64 = 8 * 1024 * 1024;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
static WORKERS: AtomicUsize = AtomicUsize::new(0);
/// Live Chromium processes: up to four open documents, one pre-render and one warm spare.
const MAX_WORKERS: usize = 6;
static SERIAL: AtomicUsize = AtomicUsize::new(0);
type Result<T> = std::result::Result<T, String>;

// Explicit opt-in on the synthetic test's thread only; never an environment
// switch that could enable logging for normal/private documents.
#[cfg(test)]
thread_local! {
    static SYNTHETIC_STDERR: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

struct WorkerSlot;
impl WorkerSlot {
    fn acquire() -> Option<Self> {
        let mut n = WORKERS.load(Ordering::Acquire);
        loop {
            if n >= MAX_WORKERS {
                return None;
            }
            match WORKERS.compare_exchange_weak(n, n + 1, Ordering::AcqRel, Ordering::Acquire) {
                Ok(_) => return Some(Self),
                Err(current) => n = current,
            }
        }
    }
}
impl Drop for WorkerSlot {
    fn drop(&mut self) {
        WORKERS.fetch_sub(1, Ordering::AcqRel);
        POOL_WAKE.notify_all();
    }
}

/// A Chromium already started and attached to a blank page, so opening a message skips
/// the launch (most of the time to first paint). It is handed to exactly one document and
/// never reused: each message still gets its own fresh process and profile.
struct Warm {
    actor: Actor,
    stop: Arc<AtomicBool>,
    slot: WorkerSlot,
}
static POOL: Mutex<Option<Warm>> = Mutex::new(None);
static POOL_WAKE: std::sync::Condvar = std::sync::Condvar::new();
static POOL_STARTED: std::sync::Once = std::sync::Once::new();
/// Tests that count live processes empty the pool and stop it refilling.
#[cfg(test)]
static POOL_PAUSED: AtomicBool = AtomicBool::new(false);

fn pool_paused() -> bool {
    #[cfg(test)]
    return POOL_PAUSED.load(Ordering::Acquire);
    #[cfg(not(test))]
    false
}

/// Starts keeping one spare Chromium ready. The launcher thread lives as long as the
/// app: Chromium's parent-death signal follows the thread that started it.
pub fn prewarm() {
    POOL_STARTED.call_once(|| {
        let _ = thread::Builder::new().name("browser-pool".into()).spawn(|| loop {
            let mut pool = POOL.lock().unwrap();
            while pool.is_some() || pool_paused() {
                pool = POOL_WAKE.wait_timeout(pool, Duration::from_millis(500)).unwrap().0;
            }
            drop(pool);
            let Some(slot) = WorkerSlot::acquire() else {
                // Every slot is busy with open documents; try again when one closes.
                let pool = POOL.lock().unwrap();
                let _ = POOL_WAKE.wait_timeout(pool, Duration::from_millis(500)).unwrap();
                continue;
            };
            let stop = Arc::new(AtomicBool::new(false));
            match Actor::start(stop.clone(), false) {
                Ok(actor) if !pool_paused() => *POOL.lock().unwrap() = Some(Warm { actor, stop, slot }),
                Ok(_) => {}
                // No Chromium (or it cannot start): documents report that themselves.
                Err(_) => {
                    drop(slot);
                    thread::sleep(Duration::from_secs(30));
                }
            }
        });
    });
}

fn take_warm() -> Option<Warm> {
    let warm = POOL.lock().unwrap().take();
    if warm.is_some() {
        POOL_WAKE.notify_all();
    }
    warm
}

#[derive(Debug)]
pub enum BrowserEvent {
    Layout { width: u32, height: u32 },
    Tile { png: Vec<u8>, width: u32, height: u32, top: u32, revision: u64 },
    TileInvalidate { revision: u64 },
    Reveal { top: u32, bottom: u32 },
    Frame {
        png: Vec<u8>,
        width: u32,
        height: u32,
    },
    Scrollability {
        up: bool,
        down: bool,
        left: bool,
        right: bool,
    },
    Link(String),
    Selection(String),
    Error(String),
}

#[derive(Clone)]
pub struct BrowserHandle(Arc<Inner>);
struct Inner {
    commands: Mutex<VecDeque<Input>>,
    events: Mutex<VecDeque<BrowserEvent>>,
    stop: Arc<AtomicBool>,
}
impl Drop for Inner {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}
#[derive(Debug)]
enum Input {
    VisibleRange(u32, u32),
    ForgetTile(u32, u64),
    Resize(u32, u32, f64),
    Mouse(String, f64, f64, String, u32, u32),
    Scroll(f64, f64, f64, f64),
    Key(String, String, String, u32, Option<String>),
    Copy,
    SelectAll,
}
fn chromium_executable() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("OVERLOOK_BROWSER_BIN") {
        let path = PathBuf::from(path);
        return path.is_file().then_some(path);
    }
    for name in [
        "chromium",
        "chromium-browser",
        "google-chrome",
        "google-chrome-stable",
    ] {
        if let Some(paths) = std::env::var_os("PATH") {
            for dir in std::env::split_paths(&paths) {
                let path = dir.join(name);
                if path.is_file() {
                    return Some(path);
                }
            }
        }
    }
    None
}

impl BrowserHandle {
    pub fn new(html: String, allow_remote: bool, dark: bool) -> Self {
        Self::new_sized(html, allow_remote, dark, None)
    }

    /// Like [`Self::new`], laid out from the start at `size` (CSS width, device scale) so
    /// the first tiles are not a throwaway render at a default width.
    pub fn new_sized(html: String, allow_remote: bool, dark: bool, size: Option<(u32, f64)>) -> Self {
        let warm = if html.len() > MAX_HTML { None } else { take_warm() };
        let inner = Arc::new(Inner {
            commands: Mutex::new(VecDeque::new()),
            events: Mutex::new(VecDeque::new()),
            stop: warm.as_ref().map(|w| w.stop.clone()).unwrap_or_else(|| Arc::new(AtomicBool::new(false))),
        });
        let handle = Self(inner.clone());
        if html.len() > MAX_HTML {
            push_event(
                &inner,
                BrowserEvent::Error("Browser HTML exceeds 8 MiB".into()),
            );
            return handle;
        }
        // The worker must NOT own Inner: otherwise last-handle drop cannot cancel it.
        let weak = Arc::downgrade(&inner);
        let stop = inner.stop.clone();
        #[cfg(test)]
        let synthetic_stderr = SYNTHETIC_STDERR.with(|p| p.borrow().clone());
        let spawn = thread::Builder::new()
            .name("browser-view".into())
            .spawn(move || {
                #[cfg(test)]
                SYNTHETIC_STDERR.with(|p| *p.borrow_mut() = synthetic_stderr);
                let emit = |event| {
                    if let Some(inner) = weak.upgrade() {
                        push_event(&inner, event);
                    }
                };
                // Replacing a document releases its old worker asynchronously.
                // Wait off the UI thread instead of permanently failing the new
                // document while the old process is being reaped.
                let deadline = Instant::now() + Duration::from_secs(10);
                let (warm_actor, slot) = match warm {
                    Some(Warm { actor, slot, .. }) => (Some(actor), slot),
                    None => loop {
                        if stop.load(Ordering::Acquire) {
                            return;
                        }
                        if let Some(slot) = WorkerSlot::acquire() {
                            break (None, slot);
                        }
                        if Instant::now() >= deadline {
                            emit(BrowserEvent::Error("Browser view limit reached".into()));
                            return;
                        }
                        thread::sleep(Duration::from_millis(25));
                    },
                };
                let _slot = slot;
                let result = (|| {
                    let mut actor = match warm_actor {
                        Some(mut actor) => {
                            actor.allow_remote = allow_remote;
                            actor
                        }
                        None => Actor::start(stop.clone(), allow_remote)?,
                    };
                    if let Some((width, scale)) = size {
                        actor.width = width.clamp(1, 2048);
                        actor.scale = if scale.is_finite() { scale.clamp(0.5, 2.0) } else { 1.0 };
                    }
                    actor.load(&html, dark)?;
                    actor.cdp.call("Page.stopScreencast", json!({}))?;
                    actor.refresh_layout(&emit)?;
                    let mut layout_dirty = false;
                    let mut next_measure = Instant::now();
                    while !stop.load(Ordering::Acquire) {
                        actor.cdp.poll()?;
                        if actor.pump()? {
                            layout_dirty = true;
                        }
                        // Failed optional images/fonts must not hide an otherwise
                        // valid document. Only engine/protocol failure is fatal.
                        if actor.resource_failed && !actor.resource_error_reported {
                            actor.resource_error_reported = true;
                        }
                        let commands = match weak.upgrade() {
                            Some(inner) => {
                                inner.commands.lock().unwrap().drain(..).collect::<Vec<_>>()
                            }
                            None => break,
                        };
                        for command in commands {
                            layout_dirty |= matches!(&command, Input::Resize(..) | Input::Key(..) | Input::SelectAll)
                                || matches!(&command, Input::Mouse(kind, _, _, _, buttons, _) if kind != "mouseMoved" || *buttons != 0);
                            actor.input(command, &emit)?;
                        }
                        if layout_dirty {
                            actor.refresh_layout(&emit)?;
                            layout_dirty = false;
                        } else if Instant::now() >= next_measure {
                            // Measure only: no periodic screenshots on native scroll.
                            if actor.measure_height()? != actor.document_height {
                                actor.refresh_layout(&emit)?;
                            }
                            next_measure = Instant::now() + Duration::from_millis(250);
                        }
                        actor.capture_next_tile(&emit)?;
                        thread::sleep(Duration::from_millis(2));
                    }
                    Ok(())
                })();
                if let Err(error) = result {
                    if !stop.load(Ordering::Acquire) {
                        emit(BrowserEvent::Error(error));
                    }
                }
            });
        if spawn.is_err() {
            push_event(
                &inner,
                BrowserEvent::Error("Cannot start browser worker".into()),
            );
        }
        handle
    }
    fn send(&self, input: Input) {
        let mut q = self.0.commands.lock().unwrap();
        if matches!(&input, Input::VisibleRange(..)) {
            q.retain(|i| !matches!(i, Input::VisibleRange(..)));
        }
        // Coalesce wheel bursts as well as motion/resize. Trackpads can produce
        // more events than the pipe can round-trip; do not replay stale deltas.
        if let (Input::Scroll(x, y, dx, dy), Some(Input::Scroll(old_x, old_y, old_dx, old_dy))) =
            (&input, q.back_mut())
        {
            *old_x = *x;
            *old_y = *y;
            *old_dx = (*old_dx + dx).clamp(-10000., 10000.);
            *old_dy = (*old_dy + dy).clamp(-10000., 10000.);
            return;
        }
        // Never reorder discrete input transitions.
        if matches!(
            (&input, q.back()),
            (Input::Resize(..), Some(Input::Resize(..)))
        ) || matches!((&input, q.back()), (Input::Mouse(k, ..), Some(Input::Mouse(old, ..))) if k == "mouseMoved" && old == k)
        {
            q.pop_back();
        }
        if q.len() >= 256 {
            drop(q);
            self.0.stop.store(true, Ordering::Release);
            push_event(
                &self.0,
                BrowserEvent::Error("Browser input queue overflow".into()),
            );
        } else {
            q.push_back(input);
        }
    }
    /// CSS width clamped to 1..=2048, DPR to 0.5..=2. CSS viewport height is always 900.
    /// max_height is retained for API compatibility; layout/tile dimensions are CSS pixels.
    pub fn resize(&self, width: u32, max_height: u32, scale: f64) {
        self.send(Input::Resize(width, max_height, scale));
    }
    /// Latest native visible range in document CSS coordinates.
    /// Frontend eviction acknowledgement keeps the captured-offset cache coherent.
    pub fn forget_tile(&self, top: u32, revision: u64) {
        self.send(Input::ForgetTile(top, revision));
    }

    pub fn visible_range(&self, top: u32, bottom: u32) {
        self.send(Input::VisibleRange(top, bottom));
    }
    /// CDP kinds: mousePressed/mouseReleased/mouseMoved. Document CSS coordinates.
    pub fn mouse(&self, kind: &str, x: f64, y: f64, button: &str, buttons: u32, click_count: u32) {
        self.send(Input::Mouse(
            kind.into(),
            x,
            y,
            button.into(),
            buttons,
            click_count,
        ));
    }
    pub fn scroll(&self, x: f64, y: f64, dx: f64, dy: f64) {
        self.send(Input::Scroll(x, y, dx, dy));
    }
    /// CDP kinds: keyDown/keyUp/rawKeyDown/char; modifiers: Alt=1, Ctrl=2, Meta=4, Shift=8.
    pub fn key(&self, kind: &str, key: &str, code: &str, modifiers: u32, text: Option<&str>) {
        if key.len() <= 128 && code.len() <= 128 && text.is_none_or(|s| s.len() <= 4096) {
            self.send(Input::Key(
                kind.into(),
                key.into(),
                code.into(),
                modifiers,
                text.map(str::to_owned),
            ));
        }
    }
    pub fn copy_selection(&self) {
        self.send(Input::Copy);
    }
    pub fn select_all(&self) {
        self.send(Input::SelectAll);
    }
    pub fn drain_events(&self) -> Vec<BrowserEvent> {
        self.0.events.lock().unwrap().drain(..).collect()
    }
}
fn push_event(inner: &Inner, event: BrowserEvent) {
    let mut q = inner.events.lock().unwrap();
    if let BrowserEvent::Tile { top, revision, .. } = &event {
        q.retain(|e| !matches!(e, BrowserEvent::Tile { top: t, revision: r, .. } if t == top && r == revision));
    }
    if let BrowserEvent::TileInvalidate { revision } = &event {
        q.retain(|e| !matches!(e, BrowserEvent::Tile { revision: r, .. } if r != revision)
            && !matches!(e, BrowserEvent::TileInvalidate { .. }));
    }
    if matches!(event, BrowserEvent::Layout { .. }) {
        q.retain(|e| !matches!(e, BrowserEvent::Layout { .. }));
    }
    if matches!(event, BrowserEvent::Frame { .. }) {
        q.retain(|e| !matches!(e, BrowserEvent::Frame { .. }));
    }
    if matches!(event, BrowserEvent::Scrollability { .. }) {
        q.retain(|e| !matches!(e, BrowserEvent::Scrollability { .. }));
    }
    if q.len() >= 64 {
        q.pop_front();
    }
    q.push_back(event);
}

/// Owns all OS resources, including partially initialized startup failures.
struct Cdp {
    child: Child,
    profile: PathBuf,
    read: UnixStream,
    write: UnixStream,
    buffer: Vec<u8>,
    events: VecDeque<Value>,
    id: u64,
    session: Option<String>,
    stop: Arc<AtomicBool>,
    memory_checked: Instant,
}
impl Drop for Cdp {
    fn drop(&mut self) {
        // Chromium is session leader; kill helpers too, then reap the direct child.
        unsafe {
            libc::kill(-(self.child.id() as i32), libc::SIGKILL);
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_dir_all(&self.profile);
    }
}
impl Cdp {
    fn start(stop: Arc<AtomicBool>) -> Result<Self> {
        // Do not put Chrome's font/shared-memory buffers on the user's /tmp:
        // tmpfs user quotas can be exhausted despite `df` reporting free space.
        // A short private runtime path also avoids Chromium's AF_UNIX socket
        // length abort when inherited TMPDIR points deep inside a project.
        let profile = PathBuf::from("/dev/shm").join(format!(
            "overlook-browser-{}-{}",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        ));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&profile)
            .map_err(|_| "Cannot create private browser profile")?;
        let result = (|| {
            let (write, child_read) = UnixStream::pair().map_err(|_| "Cannot create CDP pipe")?;
            let (child_write, read) = UnixStream::pair().map_err(|_| "Cannot create CDP pipe")?;
            // Move both source fds above 4 before dup2: no aliasing with fd3/4.
            fn high_fd(fd: i32) -> Result<OwnedFd> {
                let fd = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 10) };
                if fd < 0 {
                    Err("Cannot duplicate CDP pipe".into())
                } else {
                    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
                }
            }
            let input = high_fd(child_read.as_raw_fd())?;
            let output = high_fd(child_write.as_raw_fd())?;
            let parent = unsafe { libc::getpid() };
            let executable = chromium_executable()
                .ok_or("Chromium is not installed; configure OVERLOOK_BROWSER_BIN")?;
            let mut cmd = Command::new(executable);
            // Keep a 2x backing surface; CSS metrics/input remain independent.
            cmd.args(["--force-device-scale-factor=2", "--headless=new", "--remote-debugging-pipe", "--no-first-run", "--no-default-browser-check", "--no-startup-window",
                "--disable-background-networking", "--disable-component-update", "--disable-sync", "--disable-extensions",
                "--disable-default-apps", "--disable-breakpad", "--disable-crash-reporter",
                "--disable-application-cache", "--disk-cache-size=1", "--media-cache-size=1",
                "--disable-features=MediaRouter,OptimizationHints,AutofillServerCommunication,SpeculationRulesPrefetch",
                "--host-resolver-rules=MAP * ~NOTFOUND", "--proxy-server=http://127.0.0.1:9",
                "--js-flags=--max-old-space-size=128", "--renderer-process-limit=2"])
                .arg(format!("--user-data-dir={}", profile.display()))
                // Isolate distro-wrapper flags, crash metadata, NSS state and
                // caches from the user's ordinary browser configuration too.
                .env("TMPDIR", &profile)
                .env("HOME", &profile)
                .env("XDG_CONFIG_HOME", profile.join("config"))
                .env("XDG_CACHE_HOME", profile.join("cache"))
                .env("XDG_DATA_HOME", profile.join("data"))
                .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
            #[cfg(test)]
            SYNTHETIC_STDERR.with(|p| -> Result<()> {
                if let Some(dir) = p.borrow().as_ref() {
                    use std::os::unix::fs::OpenOptionsExt;
                    let path = dir.join(format!("chromium-{}.stderr", profile.file_name().unwrap().to_string_lossy()));
                    let file = fs::OpenOptions::new().write(true).create_new(true).mode(0o600)
                        .open(&path).map_err(|e| format!("Synthetic stderr: {e}"))?;
                    eprintln!("synthetic stderr {}", path.display());
                    cmd.stderr(Stdio::from(file));
                }
                Ok(())
            })?;
            unsafe {
                cmd.pre_exec(move || {
                    if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0
                        || libc::getppid() != parent
                    {
                        return Err(std::io::Error::last_os_error());
                    }
                    if libc::setsid() < 0
                        || libc::dup2(input.as_raw_fd(), 3) < 0
                        || libc::dup2(output.as_raw_fd(), 4) < 0
                    {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            write
                .set_nonblocking(true)
                .map_err(|_| "Cannot configure CDP pipe")?;
            read.set_nonblocking(true)
                .map_err(|_| "Cannot configure CDP pipe")?;
            let child = cmd.spawn().map_err(|_| "Cannot start sandboxed Chromium")?;
            Ok(Self {
                child,
                profile: profile.clone(),
                read,
                write,
                buffer: Vec::new(),
                events: VecDeque::new(),
                id: 0,
                session: None,
                stop,
                memory_checked: Instant::now() - Duration::from_secs(1),
            })
        })();
        if result.is_err() {
            let _ = fs::remove_dir_all(profile);
        }
        result
    }
    fn check(&mut self, deadline: Instant) -> Result<()> {
        if self.stop.load(Ordering::Acquire) {
            return Err("Browser stopped".into());
        }
        if Instant::now() >= deadline {
            return Err("Chromium command timed out".into());
        }
        if let Some(status) = self.child.try_wait().map_err(|_| "Cannot check Chromium")? {
            // Signal names are fixed diagnostics, never mail, URLs or core memory.
            let reason = match status.signal() {
                Some(libc::SIGILL) => "Sandboxed Chromium exited: SIGILL",
                Some(libc::SIGTRAP) => "Sandboxed Chromium exited: SIGTRAP",
                Some(libc::SIGBUS) => "Sandboxed Chromium exited: SIGBUS",
                Some(libc::SIGSEGV) => "Sandboxed Chromium exited: SIGSEGV",
                Some(libc::SIGABRT) => "Sandboxed Chromium exited: SIGABRT",
                Some(libc::SIGKILL) => "Sandboxed Chromium exited: SIGKILL",
                _ => "Sandboxed Chromium exited",
            };
            return Err(reason.into());
        }
        // Chromium reserves enormous virtual arenas, so RLIMIT_AS is not a
        // usable resident-memory limit. Enforce a process-tree RSS budget on
        // actor/protocol ticks instead (not a hard cgroup peak-memory limit).
        if self.memory_checked.elapsed() >= Duration::from_millis(250) {
            self.memory_checked = Instant::now();
            check_process_budget(self.child.id())?;
        }
        Ok(())
    }
    fn next(&mut self) -> Result<Option<Value>> {
        if let Some(end) = self.buffer.iter().position(|b| *b == 0) {
            let data: Vec<_> = self.buffer.drain(..=end).collect();
            return serde_json::from_slice(&data[..end])
                .map(Some)
                .map_err(|_| "Invalid Chromium protocol response".into());
        }
        let mut chunk = [0; 65536];
        match self.read.read(&mut chunk) {
            Ok(0) => Err("Chromium pipe closed".into()),
            Ok(n) => {
                if self.buffer.len() + n > MAX_PACKET {
                    return Err("Chromium protocol size limit exceeded".into());
                }
                self.buffer.extend_from_slice(&chunk[..n]);
                // Let the caller iterate, avoiding recursion on large screenshots.
                Ok(None)
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
            Err(_) => Err("Cannot read Chromium pipe".into()),
        }
    }
    fn queue(&mut self, message: Value) -> Result<()> {
        if self.events.len() >= 512 {
            return Err("Chromium event queue limit exceeded".into());
        }
        if message.get("method").is_some() {
            self.events.push_back(message);
        }
        Ok(())
    }
    fn poll(&mut self) -> Result<()> {
        self.check(Instant::now() + COMMAND_TIMEOUT)?;
        // Bounded work per tick; next() may read without yielding a complete packet.
        for _ in 0..128 {
            let before = self.buffer.len();
            match self.next()? {
                Some(v) => self.queue(v)?,
                None if self.buffer.len() == before => break,
                None => {}
            }
        }
        Ok(())
    }
    fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        self.id += 1;
        let id = self.id;
        let mut request = json!({"id":id,"method":method,"params":params});
        if let Some(session) = &self.session {
            request["sessionId"] = json!(session);
        }
        let mut bytes =
            serde_json::to_vec(&request).map_err(|_| "Cannot encode Chromium command")?;
        if bytes.len() >= MAX_PACKET {
            return Err("Chromium command size limit exceeded".into());
        }
        bytes.push(0);
        let deadline = Instant::now() + COMMAND_TIMEOUT;
        let mut written = 0;
        while written < bytes.len() {
            self.check(deadline)?;
            match self.write.write(&bytes[written..]) {
                Ok(0) => return Err("Chromium pipe closed".into()),
                Ok(n) => written += n,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(1))
                }
                Err(_) => return Err("Cannot write Chromium pipe".into()),
            }
        }
        loop {
            self.check(deadline)?;
            if let Some(response) = self.next()? {
                if response["id"].as_u64() == Some(id) {
                    if let Some(error) = response.get("error") {
                        // Responsive resources can be cancelled while an anonymous
                        // fetch is pending (resize/srcset/reflow). Resolving that
                        // obsolete interception is harmless, not a renderer failure.
                        if cancelled_resource_reply(method, error) {
                            return Ok(json!({}));
                        }
                        return Err(format!("Chromium rejected {method}"));
                    }
                    return Ok(response["result"].clone());
                }
                self.queue(response)?;
            } else {
                thread::sleep(Duration::from_millis(1));
            }
        }
    }
    fn eval(&mut self, expression: String) -> Result<Value> {
        let result = self.call(
            "Runtime.evaluate",
            json!({"expression":expression,"returnByValue":true,"timeout":2000}),
        )?;
        if result.get("exceptionDetails").is_some() {
            return Err("Trusted browser DOM operation failed".into());
        }
        Ok(result["result"]["value"].clone())
    }
}

fn cancelled_resource_reply(method: &str, error: &Value) -> bool {
    matches!(method, "Fetch.failRequest" | "Fetch.fulfillRequest")
        && error["code"].as_i64() == Some(-32602)
        && matches!(error["message"].as_str(), Some("Invalid InterceptionId." | "Invalid InterceptionId"))
}

/// Count only our child tree; never inspect process arguments or unrelated
/// application data. RSS double-counts shared pages, deliberately conservative.
fn check_process_budget(root: u32) -> Result<()> {
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) }.max(1) as u64;
    let mut pending = vec![root];
    let mut seen = Vec::new();
    let mut rss = 0u64;
    while let Some(pid) = pending.pop() {
        if seen.contains(&pid) {
            continue;
        }
        seen.push(pid);
        if seen.len() > 32 {
            return Err("Chromium process-tree limit exceeded".into());
        }
        if let Ok(statm) = fs::read_to_string(format!("/proc/{pid}/statm")) {
            rss = rss.saturating_add(
                statm
                    .split_whitespace()
                    .nth(1)
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(0)
                    .saturating_mul(page_size),
            );
        }
        if rss > 2 * 1024 * 1024 * 1024 {
            return Err(format!(
                "Chromium resident-memory budget exceeded ({rss} bytes across {} processes)",
                seen.len()
            ));
        }
        if let Ok(children) = fs::read_to_string(format!("/proc/{pid}/task/{pid}/children")) {
            pending.extend(
                children
                    .split_whitespace()
                    .filter_map(|s| s.parse::<u32>().ok()),
            );
        }
    }
    Ok(())
}

const MAX_PENDING_RESOURCES: usize = 4;
const MAX_RESOURCE_TOTAL: usize = 32 * 1024 * 1024;

struct ResourceJob {
    id: String,
    url: String,
    kind: String,
}
struct ResourceCompletion {
    id: String,
    response: Option<(String, Vec<u8>)>,
}
struct ResourceWorkers {
    jobs: std::sync::mpsc::SyncSender<ResourceJob>,
    results: std::sync::mpsc::Receiver<ResourceCompletion>,
    stop: Arc<AtomicBool>,
}
impl ResourceWorkers {
    fn new(agent: ureq::Agent, view_stop: Arc<AtomicBool>) -> Self {
        let (jobs, receiver) = std::sync::mpsc::sync_channel::<ResourceJob>(MAX_PENDING_RESOURCES);
        let receiver = Arc::new(Mutex::new(receiver));
        let (sender, results) = std::sync::mpsc::sync_channel(MAX_PENDING_RESOURCES);
        let stop = Arc::new(AtomicBool::new(false));
        for _ in 0..2 {
            let (receiver, sender, agent, stop, view_stop) = (
                receiver.clone(),
                sender.clone(),
                agent.clone(),
                stop.clone(),
                view_stop.clone(),
            );
            thread::spawn(move || loop {
                if stop.load(Ordering::Acquire) || view_stop.load(Ordering::Acquire) {
                    break;
                }
                let job = receiver
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_millis(50));
                let job = match job {
                    Ok(job) => job,
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(_) => break,
                };
                if stop.load(Ordering::Acquire) || view_stop.load(Ordering::Acquire) {
                    break;
                }
                let response = tracing::subscriber::with_default(
                    tracing::subscriber::NoSubscriber::default(),
                    || fetch_resource(&agent, &job.url, &job.kind).ok(),
                );
                if stop.load(Ordering::Acquire) || view_stop.load(Ordering::Acquire) {
                    break;
                }
                if sender
                    .try_send(ResourceCompletion {
                        id: job.id,
                        response,
                    })
                    .is_err()
                {
                    break;
                }
            });
        }
        Self {
            jobs,
            results,
            stop,
        }
    }
}
impl Drop for ResourceWorkers {
    fn drop(&mut self) {
        // Never join network threads on the actor/UI teardown path.
        self.stop.store(true, Ordering::Release);
    }
}

struct Actor {
    cdp: Cdp,
    allow_remote: bool,
    agent: ureq::Agent,
    resources: ResourceWorkers,
    resource_pending: usize,
    width: u32,
    cap: u32,
    height: u32,
    scale: f64,
    document_height: u32,
    revision: u64,
    visible: (u32, u32),
    tiles: VecDeque<u32>,
    pending_tiles: VecDeque<u32>,
    pressed: Option<(String, f64, f64, Instant)>,
    pointer: (f64, f64),
    requests: usize,
    resource_bytes: usize,
    resource_failed: bool,
    resource_error_reported: bool,
}
impl Actor {
    fn start(stop: Arc<AtomicBool>, allow_remote: bool) -> Result<Self> {
        let mut cdp = Cdp::start(stop.clone())?;
        let context = cdp.call(
            "Target.createBrowserContext",
            json!({"disposeOnDetach":true}),
        )?["browserContextId"]
            .as_str()
            .ok_or("Missing browser context")?
            .to_owned();
        cdp.call(
            "Browser.setDownloadBehavior",
            json!({"behavior":"deny","browserContextId":context}),
        )?;
        let target = cdp.call(
            "Target.createTarget",
            json!({"url":"about:blank","browserContextId":context}),
        )?["targetId"]
            .as_str()
            .ok_or("Missing browser target")?
            .to_owned();
        let session = cdp.call(
            "Target.attachToTarget",
            json!({"targetId":target,"flatten":true}),
        )?["sessionId"]
            .as_str()
            .ok_or("Missing browser session")?
            .to_owned();
        cdp.session = Some(session);
        cdp.call("Page.enable", json!({}))?;
        cdp.call(
            "Emulation.setScriptExecutionDisabled",
            json!({"value":true}),
        )?;
        cdp.call("Fetch.enable", json!({"patterns":[{"urlPattern":"*","requestStage":"Request"}],"handleAuthRequests":true}))?;
        cdp.call("Network.enable", json!({}))?;
        cdp.call("Network.setCacheDisabled", json!({"cacheDisabled":true}))?;
        cdp.call("Network.setBypassServiceWorker", json!({"bypass":true}))?;
        cdp.call("Page.setLifecycleEventsEnabled", json!({"enabled":true}))?;
        let config = ureq::Agent::config_builder()
            .proxy(None)
            .max_redirects(0)
            .max_idle_connections(0)
            .max_response_header_size(16384)
            .timeout_global(Some(Duration::from_secs(4)))
            .build();
        let agent = ureq::Agent::with_parts(
            config,
            ureq::unversioned::transport::DefaultConnector::default(),
            PublicResolver,
        );
        Ok(Self {
            cdp,
            allow_remote,
            resources: ResourceWorkers::new(agent.clone(), stop),
            resource_pending: 0,
            agent,
            width: 800,
            cap: 900,
            height: 900,
            scale: 1.0,
            document_height: 900,
            revision: 0,
            visible: (0, 900),
            tiles: VecDeque::new(),
            pending_tiles: VecDeque::new(),
            pressed: None,
            pointer: (1.0, 1.0),
            requests: 0,
            resource_bytes: 0,
            resource_failed: false,
            resource_error_reported: false,
        })
    }
    fn load(&mut self, html: &str, dark: bool) -> Result<()> {
        self.metrics()?;
        self.cdp.call(
            "Emulation.setAutoDarkModeOverride",
            json!({"enabled": dark}),
        )?;
        self.cdp.call("Emulation.setEmulatedMedia", json!({"features": [{"name":"prefers-color-scheme", "value": if dark { "dark" } else { "light" }}]}))?;
        let tree = self.cdp.call("Page.getFrameTree", json!({}))?;
        let frame = tree["frameTree"]["frame"]["id"]
            .as_str()
            .ok_or("Missing root frame")?;
        // Install policy before parsing ANY supplied markup. DOMParser creates an
        // inert tree; remove navigation/embedding surfaces before installing it.
        let policy = "default-src 'none'; script-src 'none'; style-src 'unsafe-inline' data: http: https:; img-src data: http: https:; font-src data: http: https:; frame-src 'none'; child-src 'none'; object-src 'none'; connect-src 'none'; form-action 'none'; base-uri 'none'; media-src 'none'";
        self.cdp.call("Page.setDocumentContent", json!({"frameId":frame,"html":format!("<!doctype html><html><head><meta http-equiv=\"Content-Security-Policy\" content=\"{policy}\"></head><body></body></html>")}))?;
        let args = serde_json::to_string(&json!([html, dark])).unwrap();
        self.cdp.eval(format!(r#"(([html,dark]) => {{
            const p = new DOMParser().parseFromString(html, 'text/html');
            p.querySelectorAll('script,iframe,frame,frameset,object,embed,applet,base,meta,portal,fencedframe,template,foreignObject,animate,animateMotion,animateTransform,set,mpath,discard').forEach(e=>e.remove());
            p.querySelectorAll('link').forEach(e=>{{if(e.rel.toLowerCase()!=='stylesheet')e.remove()}});
            p.querySelectorAll('form').forEach(e=>e.replaceWith(...e.childNodes));
            p.querySelectorAll('*').forEach(e=>{{
                for(const a of [...e.attributes]) {{
                    if(a.name.toLowerCase().startsWith('on') || ['srcdoc','action','formaction','target','download','ping','autofocus','contenteditable','data-overlook-link'].includes(a.name.toLowerCase())) e.removeAttribute(a.name);
                }}
                // No live navigation URL ever reaches the displayed DOM. Host
                // hit-testing below is the only way to emit an external link.
                if(e.localName==='a' || e.localName==='area') {{
                    const href=e.getAttribute('href') || e.getAttribute('xlink:href') || '';
                    e.removeAttribute('xlink:href');
                    e.setAttribute('data-overlook-link',href);
                    e.setAttribute('href','#');
                    if(!e.hasAttribute('tabindex'))e.setAttribute('tabindex','0');
                }} else if(!['link','image','use'].includes(e.localName)) {{
                    e.removeAttribute('href');e.removeAttribute('xlink:href');
                }}
            }});
            // Keep the original CSP node; imported markup cannot replace it.
            document.head.append(...[...p.head.childNodes].map(e=>document.importNode(e,true)));
            document.body.replaceChildren(...[...p.body.childNodes].map(e=>document.importNode(e,true)));
            for(const a of [...p.body.attributes])document.body.setAttribute(a.name,a.value);
            for(const a of [...p.documentElement.attributes])document.documentElement.setAttribute(a.name,a.value);
            const style=document.createElement('style');
            style.textContent='html{{overflow:auto!important;scrollbar-width:none!important}} html::-webkit-scrollbar{{display:none!important;width:0!important;height:0!important}} body{{overflow:visible!important;overflow-wrap:anywhere}} *,*::before,*::after{{animation:none!important;transition:none!important;caret-color:transparent!important}}';
            document.head.append(style);
            return true;
        }})({args})"#))?;
        self.pump()?;
        Ok(())
    }
    fn metrics(&mut self) -> Result<()> {
        self.cdp.call("Emulation.setDeviceMetricsOverride", json!({"width":self.width,"height":self.height,"deviceScaleFactor":self.scale,"mobile":false}))?;
        Ok(())
    }
    fn pump(&mut self) -> Result<bool> {
        let mut dirty = false;
        // Network work never runs on this actor. Fulfill only completed jobs here.
        while let Ok(result) = self.resources.results.try_recv() {
            self.resource_pending -= 1;
            self.complete_resource(&result.id, result.response)?;
            dirty = true;
        }
        for _ in 0..64 {
            let Some(event) = self.cdp.events.pop_front() else {
                break;
            };
            match event["method"].as_str().unwrap_or("") {
                "Fetch.requestPaused" => {
                    self.request(&event["params"])?;
                    dirty = true;
                    break;
                }
                "Fetch.authRequired" => {
                    self.cdp.call("Fetch.continueWithAuth", json!({"requestId":event["params"]["requestId"],"authChallengeResponse":{"response":"CancelAuth"}}))?;
                }
                "Page.loadEventFired" | "Page.lifecycleEvent" | "Network.loadingFinished" => {
                    dirty = true
                }
                "Inspector.targetCrashed" => return Err("Chromium renderer crashed".into()),
                _ => {}
            }
        }
        Ok(dirty)
    }
    fn request(&mut self, p: &Value) -> Result<()> {
        self.requests += 1;
        let kind = p["resourceType"].as_str().unwrap_or("");
        let url = p["request"]["url"].as_str().unwrap_or("");
        let eligible = self.allow_remote
            && self.requests <= 128
            && self.resource_pending < MAX_PENDING_RESOURCES
            && self.resource_bytes + (self.resource_pending + 1) * MAX_RESOURCE as usize
                <= MAX_RESOURCE_TOTAL
            && url.len() <= 8192
            && matches!(kind, "Image" | "Font" | "Stylesheet")
            && p["request"]["method"] == "GET";
        // Never continue a Chromium request. This also denies every Document,
        // including navigation and frames, irrespective of allow_remote.
        let id = p["requestId"]
            .as_str()
            .ok_or("Missing resource request id")?;
        if eligible
            && self
                .resources
                .jobs
                .try_send(ResourceJob {
                    id: id.to_owned(),
                    url: url.to_owned(),
                    kind: kind.to_owned(),
                })
                .is_ok()
        {
            self.resource_pending += 1;
            return Ok(()); // Fetch stays paused until the actor receives a result.
        }
        if self.allow_remote && matches!(kind, "Image" | "Font" | "Stylesheet") {
            self.resource_failed = true;
        }
        self.cdp.call(
            "Fetch.failRequest",
            json!({"requestId":id,"errorReason":"BlockedByClient"}),
        )?;
        Ok(())
    }
    fn complete_resource(&mut self, id: &str, response: Option<(String, Vec<u8>)>) -> Result<()> {
        if let Some((mime, bytes)) = response {
            self.resource_bytes += bytes.len();
            self.cdp.call("Fetch.fulfillRequest", json!({"requestId":id,"responseCode":200,
                "responseHeaders":[{"name":"Content-Type","value":mime},{"name":"Cache-Control","value":"no-store"},{"name":"X-Content-Type-Options","value":"nosniff"},{"name":"Access-Control-Allow-Origin","value":"*"}],"body":B64.encode(bytes)}))?;
        } else {
            self.resource_failed = true;
            self.cdp.call(
                "Fetch.failRequest",
                json!({"requestId":id,"errorReason":"BlockedByClient"}),
            )?;
        }
        Ok(())
    }
    fn scrollability(&mut self, x: f64, y: f64, emit: &impl Fn(BrowserEvent)) -> Result<()> {
        let args = json!([x, y]);
        let state = self.cdp.eval(format!(r#"(([x,y])=>{{
            const s=[false,false,false,false];
            const add=(e)=>{{if(!e)return;const c=getComputedStyle(e),root=e===document.scrollingElement;
                if(root||/auto|scroll/.test(c.overflowY)){{s[0] ||= e.scrollTop>1;s[1] ||= e.scrollTop+e.clientHeight<e.scrollHeight-1;}}
                if(root||/auto|scroll/.test(c.overflowX)){{s[2] ||= e.scrollLeft>1;s[3] ||= e.scrollLeft+e.clientWidth<e.scrollWidth-1;}}
            }};
            let e=document.elementFromPoint(x,y);for(let i=0;e&&i<256;i++,e=e.parentElement)add(e);
            add(document.scrollingElement);return s;
        }})({args})"#))?;
        emit(BrowserEvent::Scrollability {
            up: state[0].as_bool().unwrap_or(false),
            down: state[1].as_bool().unwrap_or(false),
            left: state[2].as_bool().unwrap_or(false),
            right: state[3].as_bool().unwrap_or(false),
        });
        Ok(())
    }

    fn anchor(&mut self, x: f64, y: f64) -> Result<Option<String>> {
        let args = json!([x, y]);
        let v = self.cdp.eval(format!("(([x,y])=>document.elementFromPoint(x,y)?.closest('a,area')?.getAttribute('data-overlook-link')||'')({args})"))?;
        Ok(v.as_str().filter(|s| safe_link(s)).map(str::to_owned))
    }
    fn input(&mut self, input: Input, emit: &impl Fn(BrowserEvent)) -> Result<()> {
        match input {
            Input::VisibleRange(top, bottom) => {
                let range = (top.min(100_000), bottom.max(top).min(100_000));
                if range != self.visible {
                    self.visible = range;
                    self.schedule_tiles();
                }
            }
            Input::ForgetTile(top, revision) => {
                if revision == self.revision {
                    self.tiles.retain(|t| *t != top);
                    self.schedule_tiles();
                }
            }
            Input::Resize(w, _h, s) => {
                self.width = w.clamp(1, 2048);
                self.cap = 900;
                self.scale = if s.is_finite() {
                    s.clamp(0.5, 2.0)
                } else {
                    1.0
                };
                self.height = 900;
                self.metrics()?;
            }
            Input::Mouse(kind, x, y, button, buttons, count) => {
                if !x.is_finite()
                    || !y.is_finite()
                    || !matches!(
                        kind.as_str(),
                        "mousePressed" | "mouseReleased" | "mouseMoved"
                    )
                    || !matches!(
                        button.as_str(),
                        "none" | "left" | "middle" | "right" | "back" | "forward"
                    )
                {
                    return Ok(());
                }
                // Blink needs the held button as well as its buttons mask for
                // drag selection. A hover-style `button:none` with buttons:1
                // silently produces no selection, even though presses work.
                let button = if kind == "mouseMoved" && button == "none" {
                    match buttons & 31 {
                        b if b & 1 != 0 => "left",
                        b if b & 2 != 0 => "right",
                        b if b & 4 != 0 => "middle",
                        b if b & 8 != 0 => "back",
                        b if b & 16 != 0 => "forward",
                        _ => "none",
                    }
                } else { button.as_str() };
                let viewport_y = self.interaction_y(y)?;
                self.pointer = (x, viewport_y);
                if kind == "mousePressed" && button == "left" {
                    self.pressed = self.anchor(x, viewport_y)?.map(|url| (url, x, y, Instant::now()));
                }
                if kind == "mouseReleased" && button == "left" {
                    if let Some((url, px, py, when)) = self.pressed.take() {
                        if when.elapsed() < Duration::from_secs(2)
                            && (x - px).abs() < 5.0
                            && (y - py).abs() < 5.0
                            && self.anchor(x, viewport_y)?.as_ref() == Some(&url)
                        {
                            emit(BrowserEvent::Link(url));
                        }
                    }
                }
                self.cdp.call("Input.dispatchMouseEvent", json!({"type":kind,"x":x,"y":viewport_y,"button":button,"buttons":buttons & 31,"clickCount":count.min(3)}))?;
                self.scrollability(x, viewport_y, emit)?;
            }
            // The native pane exclusively owns scrolling; never dispatch root wheels.
            Input::Scroll(..) => {}
            Input::Key(kind, key, code, modifiers, text) => {
                if !matches!(kind.as_str(), "keyDown" | "keyUp" | "rawKeyDown" | "char") {
                    return Ok(());
                }
                if key == "Enter" && matches!(kind.as_str(), "keyDown" | "rawKeyDown") {
                    let v = self.cdp.eval("document.activeElement?.closest('a,area')?.getAttribute('data-overlook-link')||''".into())?;
                    if let Some(url) = v.as_str().filter(|s| safe_link(s)) {
                        emit(BrowserEvent::Link(url.into()));
                        return Ok(());
                    }
                }
                // Clipboard shortcuts are host-only; never touch system clipboard.
                if modifiers & (2 | 4) != 0 && key.eq_ignore_ascii_case("c") {
                    if kind != "keyUp" {
                        self.selection(emit)?;
                    }
                    return Ok(());
                }
                if modifiers & (2 | 4) != 0 && key.eq_ignore_ascii_case("a") {
                    if kind != "keyUp" {
                        self.select_all()?;
                    }
                    return Ok(());
                }
                let virtual_key = match key.as_str() {
                    "Tab" => 9, "Enter" => 13, "Escape" => 27,
                    "PageUp" => 33, "PageDown" => 34, "End" => 35, "Home" => 36,
                    "ArrowLeft" => 37, "ArrowUp" => 38, "ArrowRight" => 39, "ArrowDown" => 40,
                    _ => 0,
                };
                let mut p = json!({"type":kind,"key":key,"code":code,"modifiers":modifiers & 15,"windowsVirtualKeyCode":virtual_key});
                if let Some(text) = text {
                    p["text"] = json!(text);
                }
                self.cdp.call("Input.dispatchKeyEvent", p)?;
                if kind != "keyUp" && (key == "Tab" || modifiers & 8 != 0) {
                    let selected = modifiers & 8 != 0 && key != "Tab";
                    let rect = self.cdp.eval(format!(r#"(()=>{{
                        let r; const s=getSelection();
                        if({selected} && s?.focusNode){{const p=document.createRange();p.setStart(s.focusNode,s.focusOffset);p.collapse(true);r=p.getBoundingClientRect();if(!r.height)r=(s.focusNode.nodeType===1?s.focusNode:s.focusNode.parentElement)?.getBoundingClientRect();}}
                        else {{const e=document.activeElement;if(e&&e!==document.body)r=e.getBoundingClientRect();}}
                        return r?[Math.max(0,Math.floor(r.top+scrollY)),Math.max(0,Math.ceil(r.bottom+scrollY))]:null;
                    }})()"#))?;
                    if let (Some(top), Some(bottom)) = (rect[0].as_u64(), rect[1].as_u64()) {
                        emit(BrowserEvent::Reveal { top: top.min(100_000) as u32, bottom: bottom.min(100_000) as u32 });
                    }
                }
            }
            Input::Copy => self.selection(emit)?,
            Input::SelectAll => self.select_all()?,
        }
        Ok(())
    }
    fn selection(&mut self, emit: &impl Fn(BrowserEvent)) -> Result<()> {
        let v = self
            .cdp
            .eval("String(getSelection()||'').slice(0,1048576)".into())?;
        emit(BrowserEvent::Selection(v.as_str().unwrap_or("").into()));
        Ok(())
    }
    fn select_all(&mut self) -> Result<()> {
        self.cdp.eval("(()=>{const r=document.createRange();r.selectNodeContents(document.body);const s=getSelection();s.removeAllRanges();s.addRange(r);return true})()".into())?;
        Ok(())
    }
    fn measure_height(&mut self) -> Result<u32> {
        // Root scrollHeight has a viewport floor. Body geometry includes margins
        // and overflowing descendants without making short mail 900px tall.
        let measured = self.cdp.eval(r#"(()=>{
            const b=document.body, r=b.getBoundingClientRect(), c=getComputedStyle(b);
            let h=Math.max(r.bottom+scrollY+parseFloat(c.marginBottom||0),
                r.top+scrollY+b.scrollHeight+parseFloat(c.marginBottom||0));
            if(document.documentElement.scrollHeight>innerHeight)
                h=Math.max(h,document.documentElement.scrollHeight);
            return Math.ceil(h);
        })()"#.into())?;
        Ok(measured.as_f64().unwrap_or(900.0).clamp(1.0, 100_000.0) as u32)
    }

    fn refresh_layout(&mut self, emit: &impl Fn(BrowserEvent)) -> Result<()> {
        self.document_height = self.measure_height()?;
        self.revision = self.revision.wrapping_add(1);
        self.tiles.clear();
        emit(BrowserEvent::TileInvalidate { revision: self.revision });
        emit(BrowserEvent::Layout { width: self.width, height: self.document_height });
        self.schedule_tiles();
        Ok(())
    }

    fn schedule_tiles(&mut self) {
        self.pending_tiles.clear();
        let (top, bottom) = self.visible;
        let end = bottom.saturating_add(512).min(self.document_height);
        let start = top.saturating_sub(512) / 512 * 512;
        // Prioritize the actual visible area, then its overscan.
        let mut wanted: Vec<_> = (start..end).step_by(512).collect();
        wanted.sort_by_key(|t| if *t + 512 > top && *t < bottom { 0 } else { 1 });
        for t in wanted {
            if let Some(i) = self.tiles.iter().position(|cached| *cached == t) {
                self.tiles.remove(i);
                self.tiles.push_back(t);
            } else {
                self.pending_tiles.push_back(t);
            }
        }
    }

    fn capture_next_tile(&mut self, emit: &impl Fn(BrowserEvent)) -> Result<()> {
        let Some(top) = self.pending_tiles.pop_front() else { return Ok(()); };
        let height = 512.min(self.document_height.saturating_sub(top));
        if height == 0 { return Ok(()); }
        // Chromium hangs when a clip rounds below one physical pixel (e.g.
        // a 1-CSS-pixel tail at DPR 0.5). Pad the raster, not document geometry;
        // the native document surface clips the tiny padded remainder.
        let height = height.max((1.0 / self.scale).ceil() as u32);
        let result = self.cdp.call("Page.captureScreenshot", json!({
            "format":"png", "optimizeForSpeed":true, "fromSurface":true,
            "captureBeyondViewport":true,
            "clip":{"x":0,"y":top,"width":self.width,"height":height,"scale":1}
        }))?;
        let encoded = result["data"].as_str().ok_or("Missing Chromium tile")?;
        if encoded.len() > 24 * 1024 * 1024 { return Err("Browser tile size limit exceeded".into()); }
        let png = B64.decode(encoded).map_err(|_| "Invalid Chromium tile")?;
        self.tiles.push_back(top);
        if self.tiles.len() > 12 { self.tiles.pop_front(); }
        emit(BrowserEvent::Tile { png, width: self.width, height, top, revision: self.revision });
        Ok(())
    }

    fn interaction_y(&mut self, doc_y: f64) -> Result<f64> {
        let y = doc_y.clamp(0.0, 100_000.0);
        let v = self.cdp.eval(format!(r#"(()=>{{
            const y={y};
            if(y<scrollY || y>=scrollY+innerHeight)
                scrollTo({{left:0,top:Math.max(0,y-innerHeight/2),behavior:'instant'}});
            return y-scrollY;
        }})()"#))?;
        Ok(v.as_f64().unwrap_or(y))
    }

    #[cfg(test)]
    fn frame(&mut self) -> Result<(Vec<u8>, u32, u32)> {
        let result = self.cdp.call(
            "Page.captureScreenshot",
            json!({"format":"png","fromSurface":true,"captureBeyondViewport":false}),
        )?;
        let encoded = result["data"]
            .as_str()
            .ok_or("Missing Chromium screenshot")?;
        if encoded.len() > 24 * 1024 * 1024 {
            return Err("Browser frame size limit exceeded".into());
        }
        let png = B64
            .decode(encoded)
            .map_err(|_| "Invalid Chromium screenshot")?;
        // Validate physical PNG pixels, but report CSS dimensions to GPUI.
        // Reporting physical dimensions double-scales HiDPI surfaces and clicks.
        if png.len() < 24 || &png[..8] != b"\x89PNG\r\n\x1a\n" {
            return Err("Invalid Chromium PNG".into());
        }
        let width = u32::from_be_bytes(png[16..20].try_into().unwrap());
        let height = u32::from_be_bytes(png[20..24].try_into().unwrap());
        if width > 4096 || height > 4096 {
            return Err("Browser pixel dimensions exceeded".into());
        }
        Ok((png, self.width, self.height))
    }
}

fn safe_link(s: &str) -> bool {
    if s.len() > 8192 || s.chars().any(|c| c.is_control()) {
        return false;
    }
    let lower = s.to_ascii_lowercase();
    lower.starts_with("https://") || lower.starts_with("http://") || lower.starts_with("mailto:")
}

#[derive(Debug)]
struct PublicResolver;
impl ureq::unversioned::resolver::Resolver for PublicResolver {
    fn resolve(
        &self,
        uri: &ureq::http::Uri,
        config: &ureq::config::Config,
        timeout: ureq::unversioned::transport::NextTimeout,
    ) -> std::result::Result<ureq::unversioned::resolver::ResolvedSocketAddrs, ureq::Error> {
        use ureq::unversioned::resolver::DefaultResolver;
        let addresses = DefaultResolver::default().resolve(uri, config, timeout)?;
        // Filter the actual connector addresses, not a separate preflight DNS
        // lookup (which would be vulnerable to DNS rebinding).
        if addresses.iter().any(|a| !public_ip(a.ip())) {
            return Err(ureq::Error::HostNotFound);
        }
        Ok(addresses)
    }
}
fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !(a == 0
                || a == 10
                || a == 127
                || a >= 224
                || (a == 100 && (64..=127).contains(&b))
                || (a == 169 && b == 254)
                || (a == 172 && (16..=31).contains(&b))
                || (a == 192 && b == 168)
                || (a == 192 && b == 0)
                || (a == 198 && (b == 18 || b == 19 || (b == 51 && c == 100)))
                || (a == 203 && b == 0 && c == 113)
                || (a == 192 && b == 2))
        }
        IpAddr::V6(ip) => {
            // Only global unicast, excluding documentation and transition ranges.
            let s = ip.segments();
            s[0] & 0xe000 == 0x2000
                && s[0] != 0x2002
                && !(s[0] == 0x2001 && s[1] < 0x0200)
                && !(s[0] == 0x2001 && s[1] == 0x0db8)
        }
    }
}
fn fetch_resource(agent: &ureq::Agent, url: &str, kind: &str) -> Result<(String, Vec<u8>)> {
    let mut url = url::Url::parse(url).map_err(|_| "Invalid resource URL")?;
    let deadline = Instant::now() + Duration::from_secs(4);
    let mut visited = std::collections::HashSet::new();
    let mut response = loop {
        url.set_fragment(None);
        if url.as_str().len() > 8192
            || url.scheme() != "https"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.port().is_some_and(|p| p != 80 && p != 443)
            || url.host_str().is_none()
            || matches!(url.host(), Some(url::Host::Ipv4(ip)) if !public_ip(ip.into()))
            || matches!(url.host(), Some(url::Host::Ipv6(ip)) if !public_ip(ip.into()))
        {
            return Err("Blocked resource URL".into());
        }
        if !visited.insert(url.as_str().to_owned()) {
            return Err("Resource redirect loop".into());
        }
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or("Resource timeout")?;
        // A fresh GET per hop: never forward cookies, auth, or referrer.
        // PublicResolver validates the actual connector addresses on every call.
        let response = agent
            .get(url.as_str())
            .config()
            .max_redirects(0)
            .timeout_global(Some(remaining))
            .build()
            .call()
            .map_err(|_| "Resource fetch failed")?;
        if !matches!(response.status().as_u16(), 301 | 302 | 303 | 307 | 308) {
            break response;
        }
        if visited.len() > 5 {
            return Err("Resource redirect limit".into());
        }
        let location = response
            .headers()
            .get("location")
            .and_then(|s| s.to_str().ok())
            .ok_or("Missing redirect location")?;
        let next = url
            .join(location)
            .map_err(|_| "Invalid redirect location")?;
        if url.scheme() == "https" && next.scheme() == "http" {
            return Err("Resource redirect downgrade".into());
        }
        url = next;
    };
    if response.status() != 200 {
        return Err("Resource status denied".into());
    }
    let mime = response
        .headers()
        .get("content-type")
        .and_then(|s| s.to_str().ok())
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let allowed = match kind {
        "Image" => matches!(
            mime.as_str(),
            "image/png"
                | "image/jpeg"
                | "image/gif"
                | "image/webp"
                | "image/avif"
                | "image/svg+xml"
        ),
        "Font" => matches!(
            mime.as_str(),
            "font/woff" | "font/woff2" | "font/ttf" | "font/otf" | "application/font-woff"
        ),
        "Stylesheet" => mime == "text/css",
        _ => false,
    };
    if !allowed {
        return Err("Resource content type denied".into());
    }
    let bytes = response
        .body_mut()
        .with_config()
        .limit(MAX_RESOURCE)
        .read_to_vec()
        .map_err(|_| "Resource exceeds limit or failed")?;
    Ok((mime, bytes))
}

#[cfg(test)]
mod cancelled_resource_tests {
    use super::*;

    #[test]
    fn browser_only_obsolete_resource_resolution_is_nonfatal() {
        let stale = json!({"code":-32602,"message":"Invalid InterceptionId."});
        assert!(cancelled_resource_reply("Fetch.failRequest", &stale));
        assert!(cancelled_resource_reply("Fetch.fulfillRequest", &stale));
        assert!(!cancelled_resource_reply("Page.captureScreenshot", &stale));
        assert!(!cancelled_resource_reply("Fetch.continueRequest", &stale));
        assert!(!cancelled_resource_reply("Fetch.fulfillRequest", &json!({"code":-32000,"message":"Target closed"})));
        assert!(!cancelled_resource_reply("Fetch.failRequest", &json!({"code":-32602,"message":"Other invalid parameter"})));
    }

    #[test]
    #[ignore = "runs real sandboxed Chromium"]
    fn browser_private_runtime_avoids_tmp_quota_and_long_socket_paths() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let mut actor = Actor::start(Arc::new(AtomicBool::new(false)), false).unwrap();
        actor.load("<p>Fictional private runtime</p>", false).unwrap();
        let profile = actor.cdp.profile.clone();
        assert_eq!(profile.parent(), Some(std::path::Path::new("/dev/shm")));
        let metadata = fs::metadata(&profile).unwrap();
        assert_eq!(metadata.uid(), unsafe { libc::getuid() });
        assert_eq!(metadata.permissions().mode() & 0o777, 0o700);
        let socket = fs::read_link(profile.join("SingletonSocket")).unwrap();
        assert!(socket.starts_with(&profile), "Chrome temporary socket must stay in its private runtime directory");
        assert!(socket.as_os_str().as_encoded_bytes().len() < 108);
        drop(actor);
        assert!(!profile.exists());
    }

    #[test]
    #[ignore = "runs real sandboxed Chromium"]
    fn browser_cancelled_resource_does_not_destroy_rendered_document() {
        let mut actor = Actor::start(Arc::new(AtomicBool::new(false)), false).unwrap();
        actor.load("<p>Fictional retained message</p>", false).unwrap();
        actor.cdp.call("Fetch.failRequest", json!({"requestId":"fictional-obsolete-request","errorReason":"BlockedByClient"})).unwrap();
        actor.cdp.call("Fetch.fulfillRequest", json!({"requestId":"fictional-obsolete-request","responseCode":200,"body":""})).unwrap();
        actor.refresh_layout(&|_| {}).unwrap();
        let received = AtomicBool::new(false);
        actor.capture_next_tile(&|event| { if matches!(event, BrowserEvent::Tile {..}) { received.store(true, Ordering::Relaxed); } }).unwrap();
        assert!(received.load(Ordering::Relaxed));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Synthetic TLS only: this key and trust anchor are never used by production.
    fn test_tls_config() -> ureq::tls::TlsConfig {
        ureq::tls::TlsConfig::builder()
            .root_certs(ureq::tls::RootCerts::Specific(vec![ureq::tls::Certificate::from_der(include_bytes!("../tests/fixtures/resource-test-cert.der"))].into()))
            .build()
    }

    fn tls_test_stream(socket: std::net::TcpStream) -> rustls::StreamOwned<rustls::ServerConnection, std::net::TcpStream> {
        let cert = rustls::pki_types::CertificateDer::from(include_bytes!("../tests/fixtures/resource-test-cert.der").to_vec());
        let key = rustls::pki_types::PrivatePkcs8KeyDer::from(include_bytes!("../tests/fixtures/resource-test-key.der").to_vec());
        let config = rustls::ServerConfig::builder().with_no_client_auth().with_single_cert(vec![cert], key.into()).unwrap();
        rustls::StreamOwned::new(rustls::ServerConnection::new(Arc::new(config)).unwrap(), socket)
    }

    const REPRO_HEIGHTS: &[u32] = &[1, 511, 512, 513, 514, 900, 1023, 1024, 1025];
    const REPRO_SCALES: &[f64] = &[1.25, 1.5, 0.5, 2.0];

    fn repro_fixed_document(height: u32) -> String {
        overlook_core::browser::prepare_document(&format!(
            "<div style='height:{height}px;background:#abcdef'></div>"
        )).html
    }

    fn repro_outlook_document() -> String {
        // Fictional authored mail only; substitute a synthetic CID as the mail
        // preparation layer does, before browser preparation. No mailbox access.
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(1, 1)
            .write_to(&mut png, image::ImageFormat::Png).unwrap();
        let mut jpeg = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(16, 16)
            .write_to(&mut jpeg, image::ImageFormat::Jpeg).unwrap();
        let source = format!(
            "<html><head><style>.MsoNormal{{margin:0;font:11pt Arial}} table{{border-collapse:collapse}}</style></head><body><p class=MsoNormal>Synthetic reply</p><table cellpadding=0 cellspacing=0><tr><td><img id=cid src='cid:synthetic-signature' width=1 height=1></td><td><b>Fictional Sender</b><br>Synthetic signature</td></tr></table><blockquote style='margin:16px;border-left:1px solid gray;padding-left:12px'>{}</blockquote></body></html>",
            "<p class=MsoNormal>Fictional quoted body, no personal or mailbox data.</p>".repeat(160)
        );
        let source = source.replace("cid:synthetic-signature", &format!("data:image/png;base64,{}", B64.encode(png.into_inner())));
        let source = source.replace("</td><td><b>", &format!("<img src='data:image/jpeg;base64,{}' width=16 height=16></td><td><b>", B64.encode(jpeg.into_inner())));
        let document = overlook_core::browser::prepare_document(&source);
        assert_eq!(document.remote_resources, 0);
        document.html
    }

    #[test]
    #[ignore = "synthetic-only real Chromium tile boundary/DPR diagnostic"]
    fn chromium_repro_tile_boundary_dimensions() {
        let mut failures = Vec::new();
        for &scale in REPRO_SCALES {
            for &height in REPRO_HEIGHTS {
                let mut actor = Actor::start(Arc::new(AtomicBool::new(false)), false).unwrap();
                actor.load(&repro_fixed_document(height), false).unwrap();
                actor.input(Input::Resize(760, 900, scale), &|_| {}).unwrap();
                actor.cdp.call("Page.stopScreencast", json!({})).unwrap();
                actor.refresh_layout(&|_| {}).unwrap();
                assert_eq!(actor.document_height, height);
                actor.input(Input::VisibleRange(0, height), &|_| {}).unwrap();
                let events = Mutex::new(Vec::new());
                while !actor.pending_tiles.is_empty() {
                    let top = *actor.pending_tiles.front().unwrap();
                    if let Err(error) = actor.capture_next_tile(&|e| events.lock().unwrap().push(e)) {
                        let detail = format!("doc={height} dpr={scale} top={top} CSS-height={} capture: {error}", 512.min(height - top));
                        eprintln!("synthetic CAPTURE ERROR {detail}");
                        failures.push(detail);
                    }
                }
                for event in events.into_inner().unwrap() {
                    if let BrowserEvent::Tile { png, width, height: css_height, top, .. } = event {
                        match image::load_from_memory(&png) {
                            Ok(pixels) => {
                                eprintln!("synthetic doc={height} dpr={scale} top={top} CSS={width}x{css_height} PNG={}x{}", pixels.width(), pixels.height());
                                if pixels.width() == 0 || pixels.height() == 0 {
                                    failures.push(format!("doc={height} dpr={scale} top={top}: zero decoded dimension"));
                                }
                            }
                            Err(error) => {
                                let ihdr = png.get(16..24).map(|b| (u32::from_be_bytes(b[..4].try_into().unwrap()), u32::from_be_bytes(b[4..].try_into().unwrap())));
                                let detail = format!("doc={height} dpr={scale} top={top} CSS={width}x{css_height} IHDR={ihdr:?}: {error}");
                                eprintln!("synthetic INVALID {detail}");
                                failures.push(detail);
                            }
                        }
                    }
                }
                // Exercise the actor's periodic trusted measurement after capture.
                let deadline = Instant::now() + Duration::from_secs(1);
                while Instant::now() < deadline {
                    let tick = (|| -> Result<()> {
                        actor.cdp.poll()?;
                        if actor.pump()? { actor.refresh_layout(&|_| {})?; }
                        actor.measure_height()?;
                        Ok(())
                    })();
                    if let Err(error) = tick {
                        failures.push(format!("doc={height} dpr={scale} post-tile: {error}"));
                        break;
                    }
                    thread::sleep(Duration::from_millis(25));
                }
            }
        }
        assert!(failures.is_empty(), "synthetic repro failures:\n{}", failures.join("\n"));
    }

    #[test]
    #[ignore = "Chromium timing diagnostic: cold launch vs warm pool"]
    fn browser_warm_pool_first_tile_timing() {
        fn first_tile(handle: &BrowserHandle) -> Duration {
            let started = Instant::now();
            loop {
                for event in handle.drain_events() {
                    match event {
                        BrowserEvent::Tile { .. } => return started.elapsed(),
                        BrowserEvent::Error(e) => panic!("{e}"),
                        _ => {}
                    }
                }
                assert!(started.elapsed() < Duration::from_secs(15), "no tile");
                thread::sleep(Duration::from_millis(2));
            }
        }
        let doc = repro_outlook_document();
        let cold = BrowserHandle::new_sized(doc.clone(), false, false, Some((760, 1.25)));
        let cold_time = first_tile(&cold);
        drop(cold);
        prewarm();
        let deadline = Instant::now() + Duration::from_secs(10);
        while POOL.lock().unwrap().is_none() {
            assert!(Instant::now() < deadline, "pool never filled");
            thread::sleep(Duration::from_millis(10));
        }
        let warm = BrowserHandle::new_sized(doc, false, false, Some((760, 1.25)));
        let warm_time = first_tile(&warm);
        eprintln!("first tile: cold {cold_time:?}, warm {warm_time:?}");
        // Leave no process behind for diagnostics that count them.
        drop(warm);
        POOL_PAUSED.store(true, Ordering::Release);
        let deadline = Instant::now() + Duration::from_secs(10);
        while WORKERS.load(Ordering::Acquire) != 0 {
            drop(take_warm());
            assert!(Instant::now() < deadline, "warm process did not retire");
            thread::sleep(Duration::from_millis(25));
        }
        assert!(warm_time < cold_time, "a warm process must paint sooner than a cold launch");
    }

    #[test]
    #[ignore = "synthetic-only production BrowserHandle post-render diagnostic"]
    fn chromium_repro_handle_post_render() {
        let mut failures = Vec::new();
        for &scale in REPRO_SCALES {
            let documents = REPRO_HEIGHTS.iter().map(|&h| (format!("height={h}"), repro_fixed_document(h)))
                .chain(std::iter::once(("Outlook-like CID/data signature + long quote".into(), repro_outlook_document())));
            for (label, html) in documents {
                let handle = BrowserHandle::new(html, false, false);
                handle.resize(760, 900, scale);
                handle.visible_range(0, 100_000);
                let started = Instant::now();
                // A 1px tail can hang for COMMAND_TIMEOUT after earlier tiles.
                // Keep draining beyond that timeout for the suspect cases;
                // all others still receive a full 1s post-first-tile window.
                let observation = if scale == 0.5 && matches!(label.as_str(), "height=513" | "height=1025") {
                    COMMAND_TIMEOUT + Duration::from_secs(1)
                } else { Duration::from_secs(1) };
                let mut first_tile = None;
                let mut tiles = 0;
                let mut layouts = Vec::new();
                loop {
                    // Use the public event API, never inspect worker mailboxes.
                    for event in handle.drain_events() {
                        match event {
                            BrowserEvent::Layout { height, .. } => layouts.push(height),
                            BrowserEvent::Tile { png, top, height, .. } => {
                                first_tile.get_or_insert_with(Instant::now);
                                tiles += 1;
                                if let Err(error) = image::load_from_memory(&png) {
                                    failures.push(format!("{label} dpr={scale} top={top} CSS-height={height}: {error}"));
                                }
                            }
                            BrowserEvent::Error(error) => failures.push(format!("{label} dpr={scale} Error {} Tile: {error}", if first_tile.is_some() { "after" } else { "before" })),
                            _ => {}
                        }
                    }
                    if first_tile.is_some_and(|t: Instant| t.elapsed() >= observation) { break; }
                    if started.elapsed() > Duration::from_secs(15) {
                        failures.push(format!("{label} dpr={scale}: no Tile within 15s"));
                        break;
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                eprintln!("synthetic handle {label} dpr={scale}: tiles={tiles} layouts={layouts:?}; observed {}", if first_tile.is_some() { "through post-Tile window (at least 1s)" } else { "15s without Tile" });
                drop(handle);
            }
        }
        assert!(failures.is_empty(), "synthetic handle failures:\n{}", failures.join("\n"));
    }

    #[test]
    #[ignore = "synthetic-only prepared Outlook-like inline-image/layout diagnostic"]
    fn chromium_repro_prepared_inline_image_layout() {
        for &scale in REPRO_SCALES {
            let mut actor = Actor::start(Arc::new(AtomicBool::new(false)), false).unwrap();
            actor.load(&repro_outlook_document(), false).unwrap();
            actor.input(Input::Resize(760, 900, scale), &|_| {}).unwrap();
            actor.refresh_layout(&|_| {}).unwrap();
            let deadline = Instant::now() + Duration::from_secs(1);
            let mut states = Vec::new();
            while Instant::now() < deadline {
                actor.cdp.poll().unwrap();
                if actor.pump().unwrap() { actor.refresh_layout(&|_| {}).unwrap(); }
                actor.capture_next_tile(&|event| {
                    if let BrowserEvent::Tile { png, .. } = event {
                        image::load_from_memory(&png).unwrap();
                    }
                }).unwrap();
                let height = actor.measure_height().unwrap();
                let image = actor.cdp.eval("(()=>{const i=document.getElementById('cid');return [i.complete,i.naturalWidth,i.naturalHeight]})()".into()).unwrap();
                let state = (height, image);
                if states.last() != Some(&state) { states.push(state); }
                thread::sleep(Duration::from_millis(10));
            }
            assert_eq!(states.last().unwrap().1, json!([true, 1, 1]));
            assert_eq!(actor.resource_pending, 0);
            eprintln!("synthetic prepared inline image dpr={scale} layout/image states={states:?}");
        }
    }

    // Read only process metadata for descendants of this synthetic test binary.
    fn synthetic_thread_counts() {
        let mut rows = Vec::new();
        for entry in fs::read_dir("/proc").unwrap().flatten() {
            let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else { continue };
            let Ok(status) = fs::read_to_string(entry.path().join("status")) else { continue };
            let number = |key: &str| status.lines().find_map(|l| l.strip_prefix(key)?.trim().parse::<u32>().ok()).unwrap_or(0);
            rows.push((pid, number("PPid:"), number("Threads:")));
        }
        let mut owned = vec![std::process::id()];
        loop {
            let next: Vec<_> = rows.iter().filter(|(pid, parent, _)| owned.contains(parent) && !owned.contains(pid)).map(|r| r.0).collect();
            if next.is_empty() { break; }
            owned.extend(next);
        }
        let counts: Vec<_> = rows.into_iter().filter(|r| owned.contains(&r.0)).collect();
        eprintln!("synthetic own tree (pid,ppid,threads)={counts:?}; total threads={}", counts.iter().map(|r| r.2).sum::<u32>());
    }

    #[test]
    #[ignore = "synthetic-only 4 live BrowserHandles, unique stderr files; run alone"]
    fn chromium_repro_concurrent_replace_stderr() {
        assert_eq!(WORKERS.load(Ordering::Acquire), 0, "run this diagnostic alone");
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/tmp");
        fs::create_dir_all(&root).unwrap();
        let dir = root.join(format!("synthetic-chromium-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        fs::DirBuilder::new().mode(0o700).create(&dir).unwrap();
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) { SYNTHETIC_STDERR.with(|p| *p.borrow_mut() = None); }
        }
        let _reset = Reset;
        SYNTHETIC_STDERR.with(|p| *p.borrow_mut() = Some(dir.clone()));
        eprintln!("synthetic artifacts={} TMPDIR={:?}", dir.display(), std::env::temp_dir());
        let mut failures = Vec::new();
        for simultaneous in [false, true] {
            let count = if simultaneous { 4 } else { 1 };
            let mut handles: Vec<BrowserHandle> = Vec::new();
            for round in 0..4 {
                // Drop all old handles then immediately replace, matching asynchronous GUI teardown.
                handles.clear();
                for i in 0..count {
                    let h = BrowserHandle::new(repro_outlook_document(), false, round % 2 == 1);
                    h.resize(if i % 2 == 0 { 760 } else { 1200 }, 900, if i % 2 == 0 { 1.25 } else { 2.0 });
                    h.visible_range(0, 1800);
                    handles.push(h);
                }
                let deadline = Instant::now() + Duration::from_secs(20);
                let mut tiles = vec![0; count];
                let mut decoded = 0;
                let mut tick = 0;
                while Instant::now() < deadline {
                    for (i, h) in handles.iter().enumerate() {
                        for event in h.drain_events() {
                            match event {
                                BrowserEvent::Tile { png, top, revision, .. } => {
                                    tiles[i] += 1;
                                    if let Err(e) = image::load_from_memory(&png) { failures.push(format!("invalid synthetic tile: {e}")); } else { decoded += 1; }
                                    h.forget_tile(top, revision);
                                }
                                BrowserEvent::Error(e) => failures.push(format!("simultaneous={simultaneous} round={round} actor={i}: {e}")),
                                _ => {}
                            }
                        }
                        if tick % 20 == 0 {
                            let alternate = (tick / 20 + i) % 2 == 0;
                            h.resize(if alternate { 760 } else { 1200 }, 900, if alternate { 1.25 } else { 2.0 });
                            let top = ((tick / 20) % 5 * 512) as u32;
                            h.visible_range(top, top + 1800);
                            h.mouse("mouseMoved", 90., top as f64 + 40., "none", 0, 0);
                        }
                    }
                    tick += 1;
                    if tick == 100 { synthetic_thread_counts(); }
                    if tick >= 300 && tiles.iter().all(|n| *n > 0) { break; }
                    thread::sleep(Duration::from_millis(20));
                }
                eprintln!("synthetic simultaneous={simultaneous} round={round} tiles={tiles:?} decoded={decoded}");
                if tiles.contains(&0) { failures.push(format!("simultaneous={simultaneous} round={round}: missing tiles {tiles:?}")); }
            }
            drop(handles);
            let deadline = Instant::now() + Duration::from_secs(15);
            while WORKERS.load(Ordering::Acquire) != 0 && Instant::now() < deadline { thread::sleep(Duration::from_millis(25)); }
            assert_eq!(WORKERS.load(Ordering::Acquire), 0, "synthetic actors did not retire");
        }
        for entry in fs::read_dir(&dir).unwrap().flatten() {
            let text = fs::read_to_string(entry.path()).unwrap();
            for line in text.lines().filter(|l| l.contains("FATAL") || l.contains("Check failed") || l.contains("pthread_create") || l.contains("zygote") || l.contains("sandbox") || l.contains("GPU")) {
                eprintln!("synthetic {}: {line}", entry.file_name().to_string_lossy());
            }
            if text.contains("FATAL") || text.contains("Check failed") { failures.push(format!("fatal/check in {}", entry.path().display())); }
        }
        assert!(failures.is_empty(), "synthetic failures:\n{}", failures.join("\n"));
    }

    #[test]
    #[ignore = "runs real sandboxed Chromium with fictional text only"]
    fn browser_drag_selection_survives_tile_capture() {
        for (button, top) in [("none", 0), ("left", 0), ("none", 1100), ("left", 1100)] {
            let mut actor = Actor::start(Arc::new(AtomicBool::new(false)), false).unwrap();
            actor.load(&format!("<style>body{{margin:0;font:20px monospace}}p{{margin:0}}</style><div style='height:{top}px'></div><p>Synthetic selectable text for copying café 日本語</p>"), false).unwrap();
            actor.input(Input::Resize(800, 900, 1.), &|_| {}).unwrap();
            actor.input(Input::VisibleRange(top, top + 512), &|_| {}).unwrap();
            let y = f64::from(top) + 12.;
            actor.input(Input::Mouse("mousePressed".into(), 2., y, "left".into(), 1, 1), &|_| {}).unwrap();
            for x in [40., 80., 120., 160., 200.] {
                actor.input(Input::Mouse("mouseMoved".into(), x, y, button.into(), 1, 0), &|_| {}).unwrap();
                actor.refresh_layout(&|_| {}).unwrap();
                actor.capture_next_tile(&|_| {}).unwrap();
            }
            actor.input(Input::Mouse("mouseReleased".into(), 200., y, "left".into(), 0, 1), &|_| {}).unwrap();
            let selected = actor.cdp.eval("String(getSelection())".into()).unwrap();
            assert!(selected.as_str().unwrap().contains("Synthetic"), "button={button}: {selected}");
        }
    }

    #[test]
    fn wheel_bursts_are_coalesced_without_reordering_buttons() {
        let handle = BrowserHandle(Arc::new(Inner {
            commands: Mutex::new(VecDeque::new()),
            events: Mutex::new(VecDeque::new()),
            stop: Arc::new(AtomicBool::new(false)),
        }));
        for _ in 0..100 {
            handle.scroll(12., 20., 0., 2.);
        }
        handle.mouse("mousePressed", 12., 20., "left", 1, 1);
        handle.scroll(12., 20., 0., 5.);
        let queue = handle.0.commands.lock().unwrap();
        assert_eq!(queue.len(), 3);
        assert!(matches!(queue[0], Input::Scroll(_, _, 0., 200.)));
        assert!(matches!(queue[1], Input::Mouse(..)));
        assert!(matches!(queue[2], Input::Scroll(_, _, 0., 5.)));
    }

    #[test]
    #[ignore = "fetches a public benign HTTPS image with real sandboxed Chromium"]
    fn chromium_remote_image_consent() {
        let url = "https://httpbin.org/image/png";
        let document =
            overlook_core::browser::prepare_document(&format!("<img id=probe src='{url}'>"));
        assert!(
            document.html.contains(url),
            "preparation stripped image URL"
        );
        for allowed in [false, true] {
            let mut actor = Actor::start(Arc::new(AtomicBool::new(false)), allowed).unwrap();
            if allowed {
                let (mime, bytes) = fetch_resource(&actor.agent, url, "Image").unwrap();
                assert_eq!(mime, "image/png");
                assert!(image::load_from_memory(&bytes).unwrap().width() > 0);
            }
            actor.load(&document.html, false).unwrap();
            let deadline = Instant::now() + Duration::from_secs(15);
            loop {
                actor.cdp.poll().unwrap();
                actor.pump().unwrap();
                let state = actor.cdp.eval("(()=>{const i=document.getElementById('probe');return [i.complete,i.naturalWidth]})()".into()).unwrap();
                if state[0] == true && actor.requests > 0 {
                    assert_eq!(
                        state[1].as_u64().unwrap() > 0,
                        allowed,
                        "image state {state}, allowed={allowed}, failed={}",
                        actor.resource_failed
                    );
                    assert_eq!(actor.resource_bytes > 0, allowed);
                    break;
                }
                assert!(Instant::now() < deadline, "image did not settle: {state}");
                thread::sleep(Duration::from_millis(10));
            }
        }
    }

    // The resolver override is test-only: no production path can map a public
    // hostname to loopback. This exercises the real HTTP/MIME/body handling.
    #[derive(Debug)]
    struct ControlledResolver(std::net::SocketAddr);
    impl ureq::unversioned::resolver::Resolver for ControlledResolver {
        fn resolve(
            &self,
            _: &ureq::http::Uri,
            _: &ureq::config::Config,
            _: ureq::unversioned::transport::NextTimeout,
        ) -> std::result::Result<ureq::unversioned::resolver::ResolvedSocketAddrs, ureq::Error>
        {
            let mut addresses = self.empty();
            addresses.push(self.0);
            Ok(addresses)
        }
    }

    #[test]
    #[ignore = "runs real sandboxed Chromium with a controlled anonymous resource server"]
    fn chromium_controlled_redirect_consent() {
        use std::net::TcpListener;
        let server = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = server.local_addr().unwrap();
        server.set_nonblocking(true).unwrap();
        let server_stop = Arc::new(AtomicBool::new(false));
        let serving_stop = server_stop.clone();
        let hits = Arc::new(AtomicUsize::new(0));
        let serving_hits = hits.clone();
        let serving = thread::spawn(move || {
            let mut png = std::io::Cursor::new(Vec::new());
            image::DynamicImage::new_rgb8(2, 2)
                .write_to(&mut png, image::ImageFormat::Png)
                .unwrap();
            while !serving_stop.load(Ordering::Acquire) {
                let (mut socket, _) = match server.accept() {
                    Ok(s) => s,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(e) => panic!("controlled accept: {e}"),
                };
                socket
                    .set_read_timeout(Some(Duration::from_secs(1)))
                    .unwrap();
                let mut socket = tls_test_stream(socket);
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    let mut byte = [0];
                    socket.read_exact(&mut byte).unwrap();
                    request.push(byte[0]);
                    assert!(request.len() < 16384);
                }
                let request = String::from_utf8(request).unwrap().to_ascii_lowercase();
                for header in [
                    "cookie:",
                    "authorization:",
                    "proxy-authorization:",
                    "referer:",
                ] {
                    assert!(!request.contains(header));
                }
                serving_hits.fetch_add(1, Ordering::Relaxed);
                let (status, extra, body) = if request.starts_with("get /public ") {
                    // A slow resource must not stall DOM/input work on the actor.
                    thread::sleep(Duration::from_millis(800));
                    (
                        "302 Found",
                        "Location: //cdn.synthetic.example/final\r\nSet-Cookie: ignored=1\r\n",
                        &b""[..],
                    )
                } else if request.starts_with("get /private ") {
                    (
                        "302 Found",
                        "Location: http://127.0.0.1/never\r\n",
                        &b""[..],
                    )
                } else {
                    assert!(
                        request.starts_with("get /final "),
                        "private target reached server"
                    );
                    ("200 OK", "", png.get_ref().as_slice())
                };
                write!(socket, "HTTP/1.1 {status}\r\nContent-Type: image/png\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
                socket.write_all(body).unwrap();
            }
        });
        for allowed in [false, true] {
            let stop = Arc::new(AtomicBool::new(false));
            let mut actor = Actor::start(stop.clone(), allowed).unwrap();
            let config = ureq::Agent::config_builder()
                .proxy(None)
                .tls_config(test_tls_config())
                .max_redirects(0)
                .max_idle_connections(0)
                .timeout_global(Some(Duration::from_secs(4)))
                .build();
            let agent = ureq::Agent::with_parts(
                config,
                ureq::unversioned::transport::DefaultConnector::default(),
                ControlledResolver(address),
            );
            actor.resources = ResourceWorkers::new(agent, stop);
            let started = Instant::now();
            actor.load("<img id=public src='https://synthetic.example/public'><img id=private src='https://synthetic.example/private'>", false).unwrap();
            actor.cdp.poll().unwrap();
            actor.pump().unwrap();
            assert_eq!(actor.cdp.eval("document.images.length".into()).unwrap(), 2);
            assert!(
                started.elapsed() < Duration::from_millis(600),
                "actor blocked on resource network"
            );
            assert!(actor.resource_pending <= MAX_PENDING_RESOURCES);
            if !allowed {
                assert_eq!(actor.resource_pending, 0);
            }
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                actor.cdp.poll().unwrap();
                let pending = actor.resource_pending;
                let dirty = actor.pump().unwrap();
                if actor.resource_pending < pending {
                    assert!(dirty);
                }
                let state = actor.cdp.eval("(()=>{const a=document.getElementById('public'),b=document.getElementById('private');return [a.complete,b.complete,a.naturalWidth,b.naturalWidth]})()".into()).unwrap();
                if state[0] == true && state[1] == true && actor.requests >= 2 {
                    assert_eq!(state[2].as_u64().unwrap(), if allowed { 2 } else { 0 });
                    assert_eq!(state[3], 0);
                    assert_eq!(hits.load(Ordering::Relaxed), if allowed { 3 } else { 0 });
                    break;
                }
                assert!(Instant::now() < deadline, "images did not settle: {state}");
                thread::sleep(Duration::from_millis(10));
            }
        }
        server_stop.store(true, Ordering::Release);
        serving.join().unwrap();
    }

    #[test]
    fn resource_fetch_is_anonymous_and_gated() {
        use std::net::TcpListener;
        let server = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = server.local_addr().unwrap();
        let serving = thread::spawn(move || {
            for (path, status, mime, extra) in [
                (
                    "/image?synthetic=1",
                    "200 OK",
                    "image/png",
                    "Set-Cookie: secret=never-send\r\n",
                ),
                (
                    "/image?synthetic=1",
                    "302 Found",
                    "image/png",
                    "Location: /relative\r\nSet-Cookie: secret=never-send\r\n",
                ),
                (
                    "/relative",
                    "307 Temporary Redirect",
                    "image/png",
                    "Location: //cdn.synthetic.example/final\r\n",
                ),
                ("/final", "200 OK", "image/png", ""),
                (
                    "/image?synthetic=1",
                    "302 Found",
                    "image/png",
                    "Location: http://127.0.0.1/private\r\n",
                ),
                (
                    "/image?synthetic=1",
                    "401 Unauthorized",
                    "image/png",
                    "WWW-Authenticate: Basic realm=probe\r\n",
                ),
                ("/image?synthetic=1", "200 OK", "text/html", ""),
                (
                    "/image?synthetic=1",
                    "302 Found",
                    "image/png",
                    "Location: /image?synthetic=1\r\n",
                ),
                (
                    "/image?synthetic=1",
                    "302 Found",
                    "image/png",
                    "Location: /hop1\r\n",
                ),
                ("/hop1", "302 Found", "image/png", "Location: /hop2\r\n"),
                ("/hop2", "302 Found", "image/png", "Location: /hop3\r\n"),
                ("/hop3", "302 Found", "image/png", "Location: /hop4\r\n"),
                ("/hop4", "302 Found", "image/png", "Location: /hop5\r\n"),
                ("/hop5", "302 Found", "image/png", "Location: /never\r\n"),
                (
                    "/image?synthetic=1",
                    "302 Found",
                    "image/png",
                    "Location: file:///tmp/never\r\n",
                ),
                (
                    "/image?synthetic=1",
                    "302 Found",
                    "image/png",
                    "Location: https://user:pass@synthetic.example/never\r\n",
                ),
                (
                    "/image?synthetic=1",
                    "302 Found",
                    "image/png",
                    "Location: https://synthetic.example:8080/never\r\n",
                ),
            ] {
                let (socket, _) = server.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(4)))
                    .unwrap();
                let mut socket = tls_test_stream(socket);
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    let mut byte = [0];
                    socket.read_exact(&mut byte).unwrap();
                    request.push(byte[0]);
                }
                let request = String::from_utf8(request).unwrap().to_ascii_lowercase();
                assert!(request.starts_with(&format!("get {path} http/1.1\r\n")));
                for header in [
                    "cookie:",
                    "authorization:",
                    "proxy-authorization:",
                    "referer:",
                ] {
                    assert!(!request.contains(header), "unexpected identifying header");
                }
                write!(socket, "HTTP/1.1 {status}\r\nContent-Type: {mime}\r\n{extra}Content-Length: 5\r\nConnection: close\r\n\r\nprobe").unwrap();
            }
        });
        let config = ureq::Agent::config_builder()
            .proxy(None)
                .tls_config(test_tls_config())
            .max_redirects(0)
            .max_idle_connections(0)
            .timeout_global(Some(Duration::from_secs(4)))
            .build();
        let agent = ureq::Agent::with_parts(
            config,
            ureq::unversioned::transport::DefaultConnector::default(),
            ControlledResolver(address),
        );
        let url = "https://synthetic.example/image?synthetic=1";
        for _ in 0..2 {
            assert_eq!(
                fetch_resource(&agent, url, "Image").unwrap(),
                ("image/png".into(), b"probe".to_vec())
            );
        }
        for _ in 0..8 {
            assert!(fetch_resource(&agent, url, "Image").is_err());
        }
        for url in [
            "file:///tmp/image",
            "http://synthetic.example/plaintext",
            "https://user:pass@synthetic.example/image",
            "https://synthetic.example:8080/image",
            "http://10.0.0.1/image",
            "http://[::1]/image",
        ] {
            assert!(fetch_resource(&agent, url, "Image").is_err());
        }
        serving.join().unwrap();
        let config = ureq::Agent::config_builder().proxy(None).build();
        let agent = ureq::Agent::with_parts(
            config,
            ureq::unversioned::transport::DefaultConnector::default(),
            PublicResolver,
        );
        assert!(fetch_resource(&agent, "http://127.0.0.1/image", "Image").is_err());
        assert!(fetch_resource(&agent, "http://[::1]/image", "Image").is_err());
    }

    #[test]
    fn tile_queue_preserves_offsets_and_revisions() {
        let inner = Inner {
            commands: Mutex::new(VecDeque::new()),
            events: Mutex::new(VecDeque::new()),
            stop: Arc::new(AtomicBool::new(false)),
        };
        for (top, revision) in [(0, 1), (512, 1), (0, 1)] {
            push_event(&inner, BrowserEvent::Tile { png: vec![], width: 800, height: 512, top, revision });
        }
        assert_eq!(inner.events.lock().unwrap().len(), 2);
        push_event(&inner, BrowserEvent::TileInvalidate { revision: 2 });
        assert_eq!(inner.events.lock().unwrap().len(), 1);
        let handle = BrowserHandle(Arc::new(inner));
        handle.visible_range(0, 900);
        handle.visible_range(4096, 4996);
        assert!(matches!(handle.0.commands.lock().unwrap().as_slices().0, [Input::VisibleRange(4096, 4996)]));
    }

    #[test]
    fn network_and_link_policy() {
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "169.254.169.254",
            "100.64.0.1",
            "::1",
            "::ffff:127.0.0.1",
            "fc00::1",
        ] {
            assert!(!public_ip(ip.parse().unwrap()));
        }
        assert!(public_ip("8.8.8.8".parse().unwrap()));
        assert!(!safe_link("javascript:alert(1)"));
        assert!(!safe_link("file:///tmp/mail"));
        assert!(safe_link("https://example.org/"));
    }
    /// Requires installed Chromium and working unprivileged sandbox support.
    #[test]
    #[ignore = "runs real sandboxed /usr/bin/chromium"]
    fn chromium_private_pipe_live_dom() {
        let stop = Arc::new(AtomicBool::new(false));
        let mut actor = Actor::start(stop, false).unwrap();
        let profile = actor.cdp.profile.clone();
        let pid = actor.cdp.child.id();
        // Prove host evaluation still works while an actual (not sanitized)
        // inline script, even without CSP, cannot execute.
        let tree = actor.cdp.call("Page.getFrameTree", json!({})).unwrap();
        actor
            .cdp
            .call(
                "Page.setDocumentContent",
                json!({
                    "frameId":tree["frameTree"]["frame"]["id"],
                    "html":"<script>globalThis.untrustedRan=true</script><p>probe</p>"
                }),
            )
            .unwrap();
        assert!(actor
            .cdp
            .eval("globalThis.untrustedRan || null".into())
            .unwrap()
            .is_null());
        actor.load(r#"<style>#proof{color:rgb(12, 34, 56);height:1200px}</style><div id=proof>synthetic selectable text</div><script>document.body.dataset.pwned='yes'</script><a href='https://example.org/'>link</a><iframe src='file:///etc/passwd'></iframe>"#, false).unwrap();
        assert_eq!(
            actor
                .cdp
                .eval("getComputedStyle(document.getElementById('proof')).color".into())
                .unwrap(),
            "rgb(12, 34, 56)"
        );
        assert!(actor
            .cdp
            .eval("document.body.dataset.pwned || null".into())
            .unwrap()
            .is_null());
        assert_eq!(
            actor
                .cdp
                .eval("document.querySelectorAll('iframe,script').length".into())
                .unwrap(),
            0
        );
        let (png, w, h) = actor.frame().unwrap();
        assert!(!png.is_empty());
        assert_eq!((w, h), (800, 900));
        actor.select_all().unwrap();
        let events = Mutex::new(Vec::new());
        actor
            .selection(&|e| events.lock().unwrap().push(e))
            .unwrap();
        assert!(
            matches!(&events.lock().unwrap()[0], BrowserEvent::Selection(s) if s.contains("synthetic selectable text"))
        );
        // Native wheels never move Chromium; host-gated input remains live.
        actor
            .input(Input::Scroll(30.0, 30.0, 0.0, 600.0), &|_| {})
            .unwrap();
        thread::sleep(Duration::from_millis(150));
        assert_eq!(actor.cdp.eval("scrollY".into()).unwrap(), 0);
        actor.load("<style>a{display:block;height:30px}</style><a href='https://example.org/'>synthetic link</a><svg><a href='https://example.org/'><set attributeName='href' to='file:///etc/passwd'/></a></svg><link rel=stylesheet href='https://example.invalid/probe.css'>", true).unwrap();
        actor.input(Input::Resize(320, 900, 2.0), &|_| {}).unwrap();
        let (_, w, h) = actor.frame().unwrap();
        assert_eq!(w, 320);
        assert_eq!(h, 900);
        assert_eq!(
            actor
                .cdp
                .eval("document.querySelectorAll('set').length".into())
                .unwrap(),
            0
        );
        let links = Mutex::new(Vec::new());
        let emit = |e| links.lock().unwrap().push(e);
        actor
            .input(
                Input::Mouse("mouseReleased".into(), 15.0, 15.0, "left".into(), 0, 1),
                &emit,
            )
            .unwrap();
        assert!(!links
            .lock()
            .unwrap()
            .iter()
            .any(|e| matches!(e, BrowserEvent::Link(_))));
        actor
            .input(
                Input::Mouse("mousePressed".into(), 15.0, 15.0, "left".into(), 1, 1),
                &emit,
            )
            .unwrap();
        actor
            .input(
                Input::Mouse("mouseReleased".into(), 15.0, 15.0, "left".into(), 0, 1),
                &emit,
            )
            .unwrap();
        assert!(links
            .lock()
            .unwrap()
            .iter()
            .any(|e| matches!(e, BrowserEvent::Link(s) if s == "https://example.org/")));
        for _ in 0..50 {
            actor.cdp.poll().unwrap();
            actor.pump().unwrap();
            if actor.requests > 0 {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert!(actor.requests > 0); // remote stylesheet was intercepted/denied
        drop(actor);
        assert!(!profile.exists());
        assert_eq!(unsafe { libc::kill(pid as i32, 0) }, -1);
    }

    #[test]
    #[ignore = "runs real sandboxed Chromium"]
    fn chromium_document_tiles_and_interaction() {
        let mut actor = Actor::start(Arc::new(AtomicBool::new(false)), false).unwrap();
        actor.load("<style>body{margin:0}html{scroll-behavior:smooth}</style><div style='height:1024px;background:red'>Unicode café 日本語 🦀</div><a style='display:block;height:1024px;background:blue' href='https://example.org/deep'>Deep link</a><div style='height:4000px;background:green'></div>", false).unwrap();
        actor.input(Input::Resize(760, 100, 2.0), &|_| {}).unwrap();
        let events = Mutex::new(Vec::new());
        let emit = |e| events.lock().unwrap().push(e);
        actor.refresh_layout(&emit).unwrap();
        assert_eq!(actor.height, 900);
        assert!(actor.document_height >= 6048);
        assert!(events.lock().unwrap().iter().any(|e| matches!(e, BrowserEvent::Layout { width: 760, height } if *height >= 6048)));
        assert_eq!(actor.cdp.eval("getComputedStyle(document.documentElement).scrollbarWidth".into()).unwrap(), "none");
        let revision = actor.revision;
        while !actor.pending_tiles.is_empty() { actor.capture_next_tile(&emit).unwrap(); }
        let first = events.lock().unwrap().iter().find_map(|e| match e {
            BrowserEvent::Tile { png, top: 0, width, height, .. } => {
                assert_eq!((*width, *height), (760, 512));
                let pixels = image::load_from_memory(png).unwrap().into_rgb8();
                assert_eq!((pixels.width(), pixels.height()), (1520, 1024));
                Some(pixels.get_pixel(1000, 500).0)
            }, _ => None
        }).unwrap();
        assert_eq!(first, [255, 0, 0]);
        actor.input(Input::VisibleRange(1024, 1536), &emit).unwrap();
        while !actor.pending_tiles.is_empty() { actor.capture_next_tile(&emit).unwrap(); }
        let second = events.lock().unwrap().iter().find_map(|e| match e {
            BrowserEvent::Tile { png, top: 1024, .. } => Some(image::load_from_memory(png).unwrap().into_rgb8().get_pixel(1000, 500).0),
            _ => None
        }).unwrap();
        assert_eq!(second, [0, 0, 255]);
        assert_eq!(actor.cdp.eval("scrollY".into()).unwrap(), 0);
        let count = events.lock().unwrap().len();
        actor.input(Input::VisibleRange(1024, 1536), &emit).unwrap();
        actor.capture_next_tile(&emit).unwrap();
        assert_eq!(events.lock().unwrap().len(), count);
        actor.input(Input::ForgetTile(1024, revision.wrapping_sub(1)), &emit).unwrap();
        assert!(actor.pending_tiles.is_empty());
        actor.input(Input::ForgetTile(1024, revision), &emit).unwrap();
        assert!(actor.pending_tiles.contains(&1024), "Frontend eviction must make the engine tile eligible again");
        while !actor.pending_tiles.is_empty() { actor.capture_next_tile(&emit).unwrap(); }
        assert!(events.lock().unwrap().len() > count);
        actor.input(Input::VisibleRange(4096, 4608), &emit).unwrap();
        assert_eq!(actor.pending_tiles.front(), Some(&4096));
        actor.capture_next_tile(&emit).unwrap();
        assert_eq!(actor.cdp.eval("scrollY".into()).unwrap(), 0);
        actor.cdp.eval("document.body.tabIndex=-1;document.body.focus({preventScroll:true});window.scrollTo({top:0,behavior:'instant'})".into()).unwrap();
        actor.input(Input::Key("keyDown".into(), "Tab".into(), "Tab".into(), 0, None), &emit).unwrap();
        actor.input(Input::Key("keyUp".into(), "Tab".into(), "Tab".into(), 0, None), &emit).unwrap();
        assert_eq!(actor.cdp.eval("document.activeElement.getAttribute('data-overlook-link')".into()).unwrap(), "https://example.org/deep");
        assert!(events.lock().unwrap().iter().any(|e| matches!(e, BrowserEvent::Reveal {top:1024, bottom:2048})), "Keyboard focus must reveal document coordinates through the native scroller");
        for kind in ["mousePressed", "mouseReleased"] {
            actor.input(Input::Mouse(kind.into(), 20., 1100., "left".into(), if kind == "mousePressed" {1} else {0}, 1), &emit).unwrap();
        }
        assert_eq!(events.lock().unwrap().iter().filter(|e| matches!(e, BrowserEvent::Link(url) if url == "https://example.org/deep")).count(), 1);
        actor.select_all().unwrap();
        actor.selection(&emit).unwrap();
        assert!(events.lock().unwrap().iter().any(|e| matches!(e, BrowserEvent::Selection(s) if s.contains("café 日本語 🦀"))));
        actor.input(Input::Resize(640, 10, 2.0), &emit).unwrap();
        actor.refresh_layout(&emit).unwrap();
        assert!(actor.revision > revision);
        actor.cdp.eval("document.body.lastElementChild.style.height='5000px'".into()).unwrap();
        actor.refresh_layout(&emit).unwrap();
        assert!(actor.document_height >= 7048);
        assert!(actor.tiles.is_empty());
        actor.input(Input::VisibleRange(0, 100_000), &emit).unwrap();
        while !actor.pending_tiles.is_empty() { actor.capture_next_tile(&emit).unwrap(); }
        assert!(actor.tiles.len() <= 12);
        actor.load("<style>body{margin:0}</style><div style='height:100px'></div>", false).unwrap();
        assert_eq!(actor.measure_height().unwrap(), 100);
    }

    #[test]
    #[ignore = "runs real sandboxed Chromium"]
    fn chromium_light_paper_defaults_preserve_authored_colors() {
        let palette = crate::theme::Palette::light();
        let mut actor = Actor::start(Arc::new(AtomicBool::new(false)), false).unwrap();
        for (source, paper, ink) in [
            ("<p>Synthetic unstyled message</p>", [225, 226, 220], "rgb(41, 51, 46)"),
            ("<style>body{background:#203040;color:#fafafa}</style><p>Authored stylesheet</p>", [32, 48, 64], "rgb(250, 250, 250)"),
            ("<body style='background:#203040;color:#fafafa'><p>Authored inline style</p></body>", [32, 48, 64], "rgb(250, 250, 250)"),
            ("<body bgcolor='#203040' text='#fafafa'><p>Authored legacy attributes</p></body>", [32, 48, 64], "rgb(250, 250, 250)"),
        ] {
            let document = palette.prepare_browser_document(source);
            actor.load(&document.html, false).unwrap();
            let (png, _, _) = actor.frame().unwrap();
            let pixels = image::load_from_memory(&png).unwrap().into_rgb8();
            assert_eq!(pixels.get_pixel(400, 50).0, paper, "{source}");
            assert_eq!(actor.cdp.eval("getComputedStyle(document.body).color".into()).unwrap(), ink, "{source}");
        }
        // A colorway switch replaces the prepared defaults, not the stored mail.
        let source = "<p>Synthetic unstyled message</p>";
        for theme in [overlook_core::ResolvedTheme::White, overlook_core::ResolvedTheme::Light] {
            let palette = crate::theme::Palette::for_theme(theme);
            let document = palette.prepare_browser_document(source);
            actor.load(&document.html, false).unwrap();
            let (png, _, _) = actor.frame().unwrap();
            let pixels = image::load_from_memory(&png).unwrap().into_rgb8();
            let expected = if theme == overlook_core::ResolvedTheme::Light { [225, 226, 220] } else { [255, 255, 255] };
            assert_eq!(pixels.get_pixel(400, 50).0, expected);
        }
    }

    #[test]
    #[ignore = "runs real sandboxed Chromium"]
    fn chromium_dark_paper_is_actually_painted_dark() {
        let document = overlook_core::browser::prepare_document(
            "<div style='height:100px;background:white;color:black'>Synthetic dark paper</div>",
        );
        let mut actor = Actor::start(Arc::new(AtomicBool::new(false)), false).unwrap();
        actor.load(&document.html, true).unwrap();
        let (png, _, _) = actor.frame().unwrap();
        let pixels = image::load_from_memory(&png).unwrap().into_rgb8();
        let paper = pixels.get_pixel(400, 50).0;
        assert!(
            paper.iter().all(|c| *c < 100),
            "Dark mode must paint dark paper, got {paper:?}"
        );
        assert_eq!(
            actor
                .cdp
                .eval("matchMedia('(prefers-color-scheme: dark)').matches".into())
                .unwrap(),
            true
        );
    }

    #[test]
    #[ignore = "runs real sandboxed Chromium"]
    fn chromium_nested_scroll_edges() {
        let mut actor = Actor::start(Arc::new(AtomicBool::new(false)), false).unwrap();
        let doc = overlook_core::browser::prepare_document("<div style='height:60px;overflow:auto'><div style='height:200px'>Synthetic nested panel</div></div>");
        actor.load(&doc.html, false).unwrap();
        actor.input(Input::Resize(320, 300, 1.0), &|_| {}).unwrap();
        actor.frame().unwrap();
        let events = Mutex::new(Vec::new());
        actor
            .scrollability(10., 10., &|e| events.lock().unwrap().push(e))
            .unwrap();
        assert!(matches!(
            events.lock().unwrap().last(),
            Some(BrowserEvent::Scrollability {
                up: false,
                down: true,
                ..
            })
        ));
        actor.cdp.eval("document.querySelector('body div').scrollTop=1000".into()).unwrap();
        thread::sleep(Duration::from_millis(150));
        actor
            .scrollability(10., 10., &|e| events.lock().unwrap().push(e))
            .unwrap();
        assert!(matches!(
            events.lock().unwrap().last(),
            Some(BrowserEvent::Scrollability {
                up: true,
                down: false,
                ..
            })
        ));
    }

    #[test]
    #[ignore = "runs real sandboxed Chromium"]
    fn chromium_authored_card_layout_and_hidpi() {
        let source =
            include_str!("../../overlook-core/tests/fixtures/formatting/browser-card.html");
        let document = overlook_core::browser::prepare_document(source);
        let mut actor = Actor::start(Arc::new(AtomicBool::new(false)), false).unwrap();
        actor.load(&document.html, false).unwrap();
        actor.input(Input::Resize(760, 600, 1.25), &|_| {}).unwrap();
        let styles = actor.cdp.eval(r#"JSON.stringify((()=>{
            const card=document.querySelector('.card'), button=document.querySelector('.button'), panel=document.querySelector('.panel'), title=document.querySelector('h1');
            return {width:card.getBoundingClientRect().width, radius:getComputedStyle(card).borderRadius, padding:getComputedStyle(card).padding,
                buttonRadius:getComputedStyle(button).borderRadius, buttonColor:getComputedStyle(button).backgroundColor,
                panelColor:getComputedStyle(panel).backgroundColor, titleSize:getComputedStyle(title).fontSize,
                titleWeight:getComputedStyle(title).fontWeight, bodyClass:document.body.className};
        })())"#.into()).unwrap();
        let styles: Value = serde_json::from_str(styles.as_str().unwrap()).unwrap();
        assert_eq!(styles["width"], 640);
        assert_eq!(styles["radius"], "24px");
        assert_eq!(styles["padding"], "32px");
        assert_eq!(styles["buttonRadius"], "999px");
        assert_eq!(styles["buttonColor"], "rgb(11, 87, 208)");
        assert_eq!(styles["panelColor"], "rgb(241, 243, 244)");
        assert_eq!(styles["titleSize"], "28px"); // 1.4rem uses actual 20px HTML root
        assert_eq!(styles["titleWeight"], "700");
        assert_eq!(styles["bodyClass"], "message");
        let (png, width, height) = actor.frame().unwrap();
        assert_eq!(width, 760); // CSS pixels, never the 950 physical PNG pixels
        assert_eq!(height, 900);
        assert_eq!(u32::from_be_bytes(png[16..20].try_into().unwrap()), 950);
        let pixels = image::load_from_memory(&png).unwrap().into_rgb8();
        let rects = actor.cdp.eval("['.card','.button','.panel'].map(s=>{const r=document.querySelector(s).getBoundingClientRect();return [r.x,r.y,r.width,r.height]})".into()).unwrap();
        let sample = |index: usize, dx: f64, dy: f64| {
            let x = (rects[index][0].as_f64().unwrap() + dx) * 1.25;
            let y = (rects[index][1].as_f64().unwrap() + dy) * 1.25;
            pixels.get_pixel(x.floor() as u32, y.floor() as u32).0
        };
        assert_eq!(sample(0, 2., 2.), [255, 255, 255]); // outside rounded card corner
        assert_eq!(
            sample(1, 12., rects[1][3].as_f64().unwrap() / 2.),
            [11, 87, 208]
        );
        assert_eq!(sample(2, 8., 40.), [241, 243, 244]); // actual painted gray panel
        assert_eq!(sample(2, 2., 2.), [255, 255, 255]); // rounded panel corner
                                                        // This opt-in output is ONLY the committed fictional fixture above.
        if let Some(path) = std::env::var_os("OVERLOOK_SYNTHETIC_CAPTURE") {
            fs::write(path, &png).unwrap();
        }
        actor.input(Input::Resize(360, 600, 1.0), &|_| {}).unwrap();
        assert_eq!(
            actor
                .cdp
                .eval("getComputedStyle(document.querySelector('.card')).padding".into())
                .unwrap(),
            "16px"
        );
        assert_eq!(
            actor
                .cdp
                .eval("document.querySelector('.card').getBoundingClientRect().width".into())
                .unwrap(),
            360
        );
    }

    #[test]
    #[ignore = "runs real sandboxed /usr/bin/chromium"]
    fn chromium_handle_last_clone_lifetime() {
        let handle = BrowserHandle::new("<p>Synthetic API selection</p>".into(), false, false);
        let clone = handle.clone();
        let stop = handle.0.stop.clone();
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let events = handle.drain_events();
            assert!(
                !events.iter().any(|e| matches!(e, BrowserEvent::Error(_))),
                "{events:?}"
            );
            if events
                .iter()
                .any(|e| matches!(e, BrowserEvent::Tile { .. }))
            {
                break;
            }
            assert!(Instant::now() < deadline, "no browser tile");
            thread::sleep(Duration::from_millis(10));
        }
        drop(handle);
        assert!(!stop.load(Ordering::Acquire));
        clone.select_all();
        clone.copy_selection();
        loop {
            if clone.drain_events().iter().any(|e| matches!(e, BrowserEvent::Selection(s) if s.contains("Synthetic API selection"))) { break; }
            assert!(Instant::now() < deadline, "no browser selection");
            thread::sleep(Duration::from_millis(10));
        }
        drop(clone);
        while WORKERS.load(Ordering::Acquire) != 0 {
            assert!(Instant::now() < deadline, "last clone did not reap worker");
            thread::sleep(Duration::from_millis(10));
        }
        assert!(stop.load(Ordering::Acquire));
    }
}
