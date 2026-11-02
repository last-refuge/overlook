//! Tiled Chromium document. The native reader scrolls this full-height surface;
//! Chromium only supplies visible tiles and handles live DOM input.
use crate::browser_engine::{BrowserEvent, BrowserHandle};
use gpui_kit::{prelude::*, *};
use gpui_kit::component::button::Button;
use std::{
    cell::Cell,
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    rc::Rc,
    sync::Arc,
    time::Duration,
};

const VIEWPORT_HEIGHT: u32 = 900;
const MAX_DOCUMENT_HEIGHT: u32 = 100_000;
const MIN_WIDTH: u32 = 160;
const OVERSCAN: u32 = 512;
const TILE_LIMIT: usize = 12;

struct CachedTile<T> {
    top: u32,
    height: u32,
    used: u64,
    image: T,
}

/// Both encoded and decoded storage are bounded. Invalidation leaves the active
/// decode running, but its generation can never publish into the new document.
struct Tiles<T> {
    generation: u64,
    revision: Option<u64>,
    awaiting_revision: bool,
    in_flight: bool,
    pending: Vec<EncodedTile>,
    cached: Vec<CachedTile<T>>,
    visible: (u32, u32),
    clock: u64,
}

impl<T> Tiles<T> {
    fn new() -> Self {
        Self {
            generation: 0,
            revision: None,
            awaiting_revision: false,
            in_flight: false,
            pending: Vec::new(),
            cached: Vec::new(),
            visible: (0, 0),
            clock: 0,
        }
    }

    fn freeze(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.pending.clear();
    }

    fn invalidate(&mut self) {
        self.freeze();
        self.cached.clear();
    }

    fn reset(&mut self) {
        self.invalidate();
        self.revision = None;
        self.awaiting_revision = false;
    }

    fn revision(&mut self, revision: u64) -> bool {
        if self.revision.is_some_and(|old| revision < old)
            || (self.awaiting_revision && self.revision == Some(revision))
        {
            return false;
        }
        if self.revision != Some(revision) || self.awaiting_revision {
            // Reflow changes the live DOM, not the message identity. Keep the
            // previous paint until replacement tiles arrive; never flash blank.
            self.freeze();
            self.revision = Some(revision);
            self.awaiting_revision = false;
        }
        true
    }

    fn visible(&mut self, range: (u32, u32)) {
        self.visible = range;
        // Refresh cached overscan too: the worker skips already-captured offsets
        // on range requests, so evicting those ahead of older offscreen tiles
        // could leave a hole when the native viewport reaches them.
        let requested = request_range(range, MAX_DOCUMENT_HEIGHT);
        let warm = if requested.0 == requested.1 {
            (0, 0)
        } else {
            (
                requested.0.saturating_sub(OVERSCAN) / OVERSCAN * OVERSCAN,
                requested
                    .1
                    .saturating_add(OVERSCAN)
                    .min(MAX_DOCUMENT_HEIGHT),
            )
        };
        self.cached
            .sort_by_key(|tile| (!overlaps(tile.top, tile.height, requested), tile.top));
        for tile in &mut self.cached {
            if overlaps(tile.top, tile.height, warm) {
                self.clock += 1;
                tile.used = self.clock;
            }
        }
    }

    fn offer(&mut self, tile: EncodedTile) -> Option<u32> {
        if self.awaiting_revision || !self.revision(tile.revision) {
            return None;
        }
        // Coalesce by offset within the current revision, not across offsets.
        self.pending.retain(|old| old.top != tile.top);
        self.pending.push(tile);
        if self.pending.len() > TILE_LIMIT {
            let victim = self
                .pending
                .iter()
                .position(|tile| !overlaps(tile.top, tile.height, self.visible))
                .unwrap_or(0);
            return Some(self.pending.remove(victim).top);
        }
        None
    }

    fn start(&mut self) -> Option<(u64, EncodedTile)> {
        if self.in_flight || self.pending.is_empty() {
            return None;
        }
        let next = self
            .pending
            .iter()
            .position(|tile| overlaps(tile.top, tile.height, self.visible))
            .unwrap_or(0);
        self.in_flight = true;
        Some((self.generation, self.pending.remove(next)))
    }

    fn finish(&mut self, generation: u64, top: u32, revision: u64) -> bool {
        self.in_flight = false;
        generation == self.generation
            && self.revision == Some(revision)
            && !self.awaiting_revision
            && !self.pending.iter().any(|tile| tile.top == top)
    }

    fn insert(&mut self, top: u32, height: u32, image: T) -> Option<u32> {
        self.cached.retain(|tile| tile.top != top);
        self.clock += 1;
        self.cached.push(CachedTile {
            top,
            height,
            used: self.clock,
            image,
        });
        if self.cached.len() > TILE_LIMIT {
            // Visible tiles are pinned before all overscan/offscreen entries.
            let victim = self
                .cached
                .iter()
                .enumerate()
                .min_by_key(|(_, tile)| (overlaps(tile.top, tile.height, self.visible), tile.used))
                .map(|(index, _)| index)
                .unwrap();
            return Some(self.cached.remove(victim).top);
        }
        None
    }
}

/// A finished render, kept so a message shows instantly when opened again or after it
/// was pre-rendered. Keyed by document, CSS width and device scale.
#[derive(Clone)]
struct FinishedRender {
    key: (u64, u32, u64),
    height: u32,
    tiles: Vec<(u32, u32, Arc<RenderImage>)>,
}

/// How many finished renders to keep. Each holds a few decoded tiles of one message.
const FINISHED_LIMIT: usize = 8;

thread_local! {
    static FINISHED: std::cell::RefCell<std::collections::VecDeque<FinishedRender>> = Default::default();
    /// The width and scale of the last message body laid out in the reader, so new ones
    /// (and pre-renders) start at the real pane width.
    static READER_SIZE: Cell<Option<(u32, f64)>> = const { Cell::new(None) };
}

fn render_key(document_hash: u64, size: (u32, f64)) -> (u64, u32, u64) {
    (document_hash, size.0, size.1.to_bits())
}

/// The size message bodies are laid out at, once the reader has shown one.
pub fn reader_size() -> Option<(u32, f64)> {
    READER_SIZE.with(Cell::get)
}

fn finished_render(key: (u64, u32, u64)) -> Option<FinishedRender> {
    FINISHED.with(|f| {
        let mut f = f.borrow_mut();
        let i = f.iter().position(|r| r.key == key)?;
        let render = f.remove(i)?;
        f.push_back(render.clone());
        Some(render)
    })
}

fn remember_render(render: FinishedRender) {
    FINISHED.with(|f| {
        let mut f = f.borrow_mut();
        f.retain(|r| r.key != render.key);
        f.push_back(render);
        while f.len() > FINISHED_LIMIT {
            f.pop_front();
        }
    });
}

/// Whether a document is already rendered at this size (pre-rendering skips it).
pub fn is_rendered(document: &str, allow_remote: bool, dark: bool, size: (u32, f64)) -> bool {
    let key = render_key(document_hash(document, allow_remote, dark), size);
    FINISHED.with(|f| f.borrow().iter().any(|r| r.key == key))
}

/// Renders a document off-screen at `size` and keeps the first screenful, so opening
/// it later shows it at once. The Chromium process is released when done.
pub fn prerender(document: String, allow_remote: bool, dark: bool, size: (u32, f64), cx: &mut App) -> Task<()> {
    let key = render_key(document_hash(&document, allow_remote, dark), size);
    let browser = BrowserHandle::new_sized(document, allow_remote, dark, Some(size));
    browser.visible_range(0, VIEWPORT_HEIGHT);
    cx.spawn(async move |cx| {
        let started = std::time::Instant::now();
        let mut height = None;
        let mut encoded: Vec<EncodedTile> = Vec::new();
        let mut revision = None;
        loop {
            cx.background_executor().timer(Duration::from_millis(16)).await;
            for event in browser.drain_events() {
                match event {
                    BrowserEvent::Layout { width, height: h } if width == size.0 => height = Some(h.clamp(1, MAX_DOCUMENT_HEIGHT)),
                    BrowserEvent::Tile { png, width, height, top, revision: r } if width == size.0 && height > 0 && height <= OVERSCAN => {
                        // A newer layout replaces older tiles.
                        if revision.is_some_and(|old| r > old) {
                            encoded.clear();
                        }
                        if revision.is_none_or(|old| r >= old) {
                            revision = Some(r);
                            encoded.retain(|t| t.top != top);
                            encoded.push(EncodedTile { bytes: png, width, height, top, revision: r });
                        }
                    }
                    BrowserEvent::Error(_) => return,
                    _ => {}
                }
            }
            if let Some(h) = height {
                let want = h.min(VIEWPORT_HEIGHT);
                let covered = (0..want).step_by(16).all(|y| encoded.iter().any(|t| t.top <= y && y < t.top + t.height));
                if covered {
                    break;
                }
            }
            if started.elapsed() > Duration::from_secs(10) {
                return;
            }
        }
        drop(browser);
        let Some(height) = height else { return };
        let decoded = cx
            .background_executor()
            .spawn(async move { encoded.into_iter().filter_map(|t| decode_frame(&t.bytes).ok().map(|img| (t.top, t.height, img))).collect::<Vec<_>>() })
            .await;
        let _ = cx.update(|_| remember_render(FinishedRender { key, height, tiles: decoded }));
    })
}

fn freeze_failure<T>(tiles: &mut Tiles<T>, height: &mut u32) {
    tiles.freeze();
    if tiles.cached.is_empty() { *height = 160; }
}

// Exact allowlist only: never display or log a worker diagnostic (even a
// familiar prefix may have a private URL or document text appended).
fn classify_error(raw: &str, startup: bool) -> &'static str {
    match raw {
        "Chromium is not installed; configure OVERLOOK_BROWSER_BIN" if startup => "missingBinary",
        "Chromium pipe closed" | "Cannot read Chromium pipe" | "Cannot write Chromium pipe" => "pipeClosed",
        "Chromium command timed out" => "timeout",
        "Sandboxed Chromium exited" | "Chromium renderer crashed" => "engineExited",
        "Sandboxed Chromium exited: SIGILL" => "engineSIGILL",
        "Sandboxed Chromium exited: SIGTRAP" => "engineSIGTRAP",
        "Sandboxed Chromium exited: SIGBUS" => "engineSIGBUS",
        "Sandboxed Chromium exited: SIGSEGV" => "engineSIGSEGV",
        "Sandboxed Chromium exited: SIGABRT" => "engineSIGABRT",
        "Sandboxed Chromium exited: SIGKILL" => "engineSIGKILL",
        "Cannot create private browser profile" => "profileFailed",
        "Cannot start sandboxed Chromium" => "launchFailed",
        raw if raw.starts_with("Chromium resident-memory budget exceeded (") => "memoryLimit",
        "Trusted browser DOM operation failed" => "DOMUpdate",
        "Chromium rejected Fetch.fulfillRequest" | "Chromium rejected Fetch.failRequest" => "resourceCompletion",
        "Chromium rejected Page.captureScreenshot" | "Missing Chromium tile" | "Missing Chromium screenshot" => "captureFailed",
        "Invalid Chromium tile" | "Invalid Chromium screenshot" | "Invalid Chromium PNG" => "decodeFailed",
        "Browser HTML exceeds 8 MiB" | "Browser view limit reached" |
        "Browser input queue overflow" | "Chromium protocol size limit exceeded" |
        "Chromium event queue limit exceeded" | "Chromium command size limit exceeded" |
        "Chromium process-tree limit exceeded" | "Browser tile size limit exceeded" |
        "Browser frame size limit exceeded" | "Browser pixel dimensions exceeded" => "limit",
        _ => "engineStopped",
    }
}

fn failure_message(code: &str, retained: bool) -> &'static str {
    if retained {
        "Browser stopped updating. Last rendered content retained. Content is frozen; live interactions are unavailable."
    } else if code == "missingBinary" {
        "The isolated browser could not start. Install Chromium, then retry."
    } else {
        "The isolated browser could not render this message. Retry to try again."
    }
}

fn overlaps(top: u32, height: u32, visible: (u32, u32)) -> bool {
    top < visible.1 && top.saturating_add(height) > visible.0
}

struct EncodedTile {
    bytes: Vec<u8>,
    width: u32,
    height: u32,
    top: u32,
    revision: u64,
}

fn visible_range(bounds: Bounds<Pixels>, mask: Bounds<Pixels>, height: u32) -> (u32, u32) {
    let top = f32::from(mask.top() - bounds.top())
        .max(0.)
        .min(height as f32);
    let bottom = f32::from(mask.bottom() - bounds.top())
        .max(0.)
        .min(height as f32);
    if mask.right() <= bounds.left() || mask.left() >= bounds.right() || bottom <= top {
        return (0, 0);
    }
    (top.floor() as u32, bottom.ceil() as u32)
}

fn request_range(visible: (u32, u32), height: u32) -> (u32, u32) {
    if visible.0 == visible.1 {
        return (0, 0);
    }
    (
        visible.0.saturating_sub(OVERSCAN),
        visible.1.saturating_add(OVERSCAN).min(height),
    )
}

fn decode_frame(bytes: &[u8]) -> image::ImageResult<Arc<RenderImage>> {
    // The compatibility `png` event field can carry either PNG or CDP JPEG.
    let mut pixels = image::load_from_memory(bytes)?.into_rgba8();
    for pixel in pixels.pixels_mut() {
        pixel.0.swap(0, 2);
    }
    Ok(Arc::new(RenderImage::new(vec![image::Frame::new(pixels)])))
}

struct RetryDocument {
    html: Arc<str>,
    allow_remote: bool,
    dark: bool,
}

impl RetryDocument {
    fn new(html: String, allow_remote: bool, dark: bool) -> Self {
        Self { html: Arc::from(html), allow_remote, dark }
    }

    fn start(&self) -> BrowserHandle {
        BrowserHandle::new(self.html.to_string(), self.allow_remote, self.dark)
    }
}

pub struct BrowserBody {
    browser: BrowserHandle,
    prepared: RetryDocument,
    document_hash: u64,
    focus: FocusHandle,
    bounds: Rc<Cell<Bounds<Pixels>>>,
    scale: Rc<Cell<f64>>,
    last_resize: Option<(u32, f64)>,
    tiles: Tiles<Arc<RenderImage>>,
    visible: Rc<Cell<(u32, u32)>>,
    requested: Rc<Cell<Option<(u32, u32)>>>,
    height: u32,
    error: Option<&'static str>,
    has_rendered: bool,
    layout_ready: bool,
    cache_initial_layout: bool,
    buttons: u32,
    native_scroll: Option<ScrollHandle>,
    /// The message as plain text, shown natively until the first tiles arrive.
    placeholder: Option<SharedString>,
}

impl BrowserBody {
    pub fn new(document: String, allow_remote: bool, dark: bool, cx: &mut Context<Self>) -> Self {
        Self::new_sized(document, allow_remote, dark, None, None, cx)
    }

    /// Starts the document at `size` (the reader's width and scale) and shows a finished
    /// render of it at once when one is cached. `placeholder` is shown until then.
    pub fn new_sized(
        document: String,
        allow_remote: bool,
        dark: bool,
        size: Option<(u32, f64)>,
        placeholder: Option<SharedString>,
        cx: &mut Context<Self>,
    ) -> Self {
        // Only the weak entity is retained across awaits. Dropping the view drops
        // its last browser handle and cancels/reaps the engine worker.
        cx.spawn(async move |this, cx| loop {
            cx.background_executor()
                .timer(Duration::from_millis(16))
                .await;
            if this.update(cx, |view, cx| view.poll(cx)).is_err() {
                break;
            }
        })
        .detach();
        let document_hash = document_hash(&document, allow_remote, dark);
        let prepared = RetryDocument::new(document, allow_remote, dark);
        let mut tiles = Tiles::new();
        let mut height = 300;
        let finished = size.and_then(|size| finished_render(render_key(document_hash, size)));
        if let Some(render) = &finished {
            height = render.height;
            for (top, h, image) in &render.tiles {
                tiles.insert(*top, *h, image.clone());
            }
        }
        Self {
            document_hash,
            browser: BrowserHandle::new_sized(prepared.html.to_string(), allow_remote, dark, size),
            prepared,
            focus: cx.focus_handle(),
            bounds: Rc::new(Cell::new(Bounds::default())),
            scale: Rc::new(Cell::new(1.0)),
            // Started at `size`: no resize (and no throwaway layout) unless the pane differs.
            last_resize: size,
            tiles,
            visible: Rc::new(Cell::new((0, 0))),
            requested: Rc::new(Cell::new(None)),
            height,
            error: None,
            has_rendered: finished.is_some(),
            layout_ready: finished.is_some(),
            cache_initial_layout: true,
            buttons: 0,
            native_scroll: None,
            placeholder,
        }
    }

    /// Keeps the current paint as this document's finished render.
    fn remember(&self) {
        // A user's open <details> or nested-scroll position is not the initial
        // state of this HTML. Reopening must not paint it over a fresh closed DOM.
        if !self.cache_initial_layout { return; }
        if let Some(size) = self.last_resize {
            remember_render(FinishedRender {
                key: render_key(self.document_hash, size),
                height: self.height,
                // The opening screenful is what makes a revisit instant; the rest renders live.
                tiles: self.tiles.cached.iter().filter(|t| t.top < VIEWPORT_HEIGHT + OVERSCAN).map(|t| (t.top, t.height, t.image.clone())).collect(),
            });
        }
    }

    pub fn attach_native_scroll(&mut self, scroll: ScrollHandle) {
        self.native_scroll = Some(scroll);
    }

    pub fn layout_is_ready(&self) -> bool { self.layout_ready || self.error.is_some() }

    pub fn set_document(
        &mut self,
        document: String,
        allow_remote: bool,
        dark: bool,
        cx: &mut Context<Self>,
    ) {
        let hash = document_hash(&document, allow_remote, dark);
        if hash == self.document_hash {
            return;
        }
        self.document_hash = hash;
        self.layout_ready = false;
        self.cache_initial_layout = true;
        let retain_paint = allow_remote == self.prepared.allow_remote && dark == self.prepared.dark;
        self.prepared = RetryDocument::new(document, allow_remote, dark);
        if retain_paint {
            // CID completion/preview edits replace the same entity's document.
            // Consent or theme changes deliberately use the clearing path below.
            self.tiles.freeze();
            self.tiles.revision = None;
            self.tiles.awaiting_revision = false;
            self.requested.set(None);
            self.browser = self.prepared.start();
            self.error = None;
            self.buttons = 0;
            self.last_resize = None;
            cx.notify();
        } else {
            self.retry(cx);
        }
    }

    fn retry(&mut self, cx: &mut Context<Self>) {
        self.tiles.reset();
        self.requested.set(None);
        self.browser = self.prepared.start();
        self.height = 300;
        self.error = None;
        self.has_rendered = false;
        self.layout_ready = false;
        self.cache_initial_layout = true;
        self.buttons = 0;
        self.last_resize = None;
        cx.notify();
    }

    fn fail(&mut self, code: &'static str, cx: &mut Context<Self>) {
        freeze_failure(&mut self.tiles, &mut self.height);
        self.error = Some(code);
        self.buttons = 0;
        cx.notify();
    }

    fn poll(&mut self, cx: &mut Context<Self>) {
        crate::watchdog::mark("message body update");
        if self.error.is_some() { return; }
        // Wait for real layout rather than starting a throwaway 160px render.
        if self.bounds.get().size.width <= px(0.) { return; }
        let width = (f32::from(self.bounds.get().size.width).round() as u32).max(MIN_WIDTH);
        let size = (width.min(2048), self.scale.get());
        if self.native_scroll.is_some() {
            READER_SIZE.with(|s| s.set(Some(size)));
        }
        let events = self.browser.drain_events();
        // Terminal errors win over queued geometry, preserving the decoded
        // surface without allocating a second tile cache.
        if let Some(raw) = events.iter().find_map(|event| match event {
            BrowserEvent::Error(raw) => Some(raw.as_str()),
            _ => None,
        }) {
            self.fail(classify_error(raw, !self.has_rendered), cx);
            return;
        }
        let resized = self.last_resize != Some(size);
        if resized {
            self.layout_ready = false;
            // Establish a revision floor from everything already queued before
            // resize. None of those geometry events may repopulate the surface.
            for event in &events {
                if let BrowserEvent::TileInvalidate { revision }
                | BrowserEvent::Tile { revision, .. } = event
                {
                    self.tiles.revision = Some(self.tiles.revision.unwrap_or(0).max(*revision));
                }
            }
            self.last_resize = Some(size);
            self.tiles.freeze();
            self.tiles.awaiting_revision = true;
            self.requested.set(None);
            self.browser.resize(size.0, VIEWPORT_HEIGHT, size.1);
            cx.notify();
        }
        if self.tiles.visible != self.visible.get() {
            self.tiles.visible(self.visible.get());
        }
        for event in events {
            if resized
                && matches!(
                    event,
                    BrowserEvent::Layout { .. }
                        | BrowserEvent::Tile { .. }
                        | BrowserEvent::TileInvalidate { .. }
                )
            {
                continue;
            }
            match event {
                BrowserEvent::Layout { width, height } => {
                    if width != size.0 {
                        continue;
                    }
                    let height = height.clamp(1, MAX_DOCUMENT_HEIGHT);
                    self.layout_ready = true;
                    if self.error.is_none() && self.height != height {
                        self.height = height;
                        self.requested.set(None);
                        cx.notify();
                    }
                }
                BrowserEvent::TileInvalidate { revision } => {
                    let generation = self.tiles.generation;
                    if self.tiles.revision(revision) && generation != self.tiles.generation {
                        self.requested.set(None);
                        cx.notify();
                    }
                }
                BrowserEvent::Tile {
                    png,
                    width,
                    height,
                    top,
                    revision,
                } => {
                    if self.error.is_some()
                        || width != self.last_resize.map_or(width, |size| size.0)
                        || height == 0
                        || height > OVERSCAN
                        || top >= MAX_DOCUMENT_HEIGHT
                    {
                        continue;
                    }
                    let generation = self.tiles.generation;
                    if let Some(dropped) = self.tiles.offer(EncodedTile {
                        bytes: png,
                        width,
                        height,
                        top,
                        revision,
                    }) {
                        self.browser.forget_tile(dropped, revision);
                    }
                    if generation != self.tiles.generation {
                        cx.notify();
                    }
                }
                // Legacy events deliberately do not drive the production surface.
                BrowserEvent::Frame { .. } | BrowserEvent::Scrollability { .. } => {}
                BrowserEvent::Reveal { top, bottom } => {
                    if let Some(scroll) = &self.native_scroll {
                        let viewport = scroll.bounds();
                        let bounds = self.bounds.get();
                        let top = bounds.top() + px(top as f32);
                        let bottom = bounds.top() + px(bottom as f32);
                        let delta = if top < viewport.top() { viewport.top() - top }
                            else if bottom - top > viewport.size.height {
                                if top >= viewport.bottom() { viewport.top() - top } else { px(0.) }
                            }
                            else if bottom > viewport.bottom() { viewport.bottom() - bottom }
                            else { px(0.) };
                        if delta != px(0.) {
                            let old = scroll.offset();
                            scroll.set_offset(point(old.x, (old.y + delta).min(px(0.)).max(-scroll.max_offset().y)));
                            cx.notify();
                        }
                    }
                }
                BrowserEvent::Selection(text) => {
                    if !text.is_empty() {
                        cx.write_to_clipboard(ClipboardItem::new_string(text));
                    }
                }
                BrowserEvent::Link(url) => {
                    if allowed_url(&url) {
                        cx.open_url(&url);
                    }
                }
                BrowserEvent::Error(_) => unreachable!("errors handled before geometry"),
            }
        }
        // Coalesce the entire drained batch before scheduling any image work.
        self.start_decode(cx);
    }

    fn start_decode(&mut self, cx: &mut Context<Self>) {
        let Some((generation, encoded)) = self.tiles.start() else {
            return;
        };
        let decode = cx.background_executor().spawn(async move {
            let result = decode_frame(&encoded.bytes);
            (
                result,
                encoded.top,
                encoded.height,
                encoded.width,
                encoded.revision,
            )
        });
        // Neither background work nor delivery retains the view/browser.
        cx.spawn(async move |this, cx| {
            let (result, top, height, width, revision) = decode.await;
            let _ = this.update(cx, |view, cx| {
                if view.tiles.finish(generation, top, revision)
                    && view.error.is_none()
                    && view.last_resize.is_some_and(|size| size.0 == width)
                {
                    match result {
                        Ok(frame) => {
                            view.has_rendered = true;
                            if let Some(evicted) = view.tiles.insert(top, height, frame) {
                                view.browser.forget_tile(evicted, revision);
                            }
                            view.remember();
                        }
                        Err(_) => {
                            view.fail("decodeFailed", cx);
                        }
                    }
                    cx.notify();
                }
                view.start_decode(cx);
            });
        })
        .detach();
    }

    fn position(&self, position: Point<Pixels>) -> (f64, f64) {
        local_position(position, self.bounds.get())
    }

    fn keyboard(&mut self, stroke: &Keystroke, down: bool) {
        if down { self.cache_initial_layout = false; }
        if self.error.is_some() { return; }
        if down && (stroke.modifiers.control || stroke.modifiers.platform) {
            match stroke.key.as_str() {
                "c" => {
                    self.browser.copy_selection();
                    return;
                }
                "a" => {
                    self.browser.select_all();
                    return;
                }
                _ => {}
            }
        }
        let (key, code) = browser_key(&stroke.key);
        let text = if down
            && !stroke.modifiers.control
            && !stroke.modifiers.platform
            && !stroke.modifiers.alt
        {
            stroke
                .key_char
                .as_deref()
                .or_else(|| (stroke.key.chars().count() == 1).then_some(stroke.key.as_str()))
        } else {
            None
        };
        let key = if stroke.key.chars().count() == 1 { text.unwrap_or(&key) } else { &key };
        let m = &stroke.modifiers;
        let modifiers = u32::from(m.alt)
            | (u32::from(m.control) << 1)
            | (u32::from(m.platform) << 2)
            | (u32::from(m.shift) << 3);
        self.browser.key(
            if down { "keyDown" } else { "keyUp" },
            key,
            &code,
            modifiers,
            text,
        );
    }
}

impl Focusable for BrowserBody {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for BrowserBody {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let bounds = self.bounds.clone();
        let scale = self.scale.clone();
        let current_scale = f64::from(window.scale_factor());
        let visible = self.visible.clone();
        let requested = self.requested.clone();
        let browser = self.browser.clone();
        let height = self.height;
        let live = self.error.is_none();
        let warning = self.error.map(|code| {
            div().p_4().child(failure_message(code, !self.tiles.cached.is_empty()))
                .child(format!(" ({code}) "))
                .child(Button::new("browser-retry").label("Retry")
                    .on_click(cx.listener(|this, _, _, cx| this.retry(cx))))
        });
        // Until Chromium's first tiles arrive, show the message as plain text rather
        // than a blank surface.
        let placeholder = (!self.has_rendered && self.error.is_none() && self.tiles.cached.is_empty())
            .then(|| self.placeholder.clone())
            .flatten()
            .map(|text| div().absolute().inset_0().px(px(16.)).py(px(12.)).text_size(px(14.)).line_height(px(21.)).child(text));
        let content = {
            div()
                .size_full()
                .children(placeholder)
                .children(self.tiles.cached.iter().map(|tile| {
                    img(tile.image.clone())
                        .absolute()
                        .top(px(tile.top as f32))
                        .left_0()
                        .w_full()
                        .h(px(tile.height as f32))
                }))
                .into_any_element()
        };
        let mut surface = div()
            .id("browser-body")
            .relative()
            .w_full()
            .min_w(px(MIN_WIDTH as f32))
            .max_w(px(2048.))
            .h(px(self.height as f32))
            .overflow_hidden()
            .flex_shrink_0()
            .track_focus(&self.focus)
            .child(content)
            .child(
                canvas(
                    move |actual, window, _| {
                        bounds.set(actual);
                        scale.set(current_scale);
                        let range = visible_range(actual, window.content_mask().bounds, height);
                        visible.set(range);
                        let range = request_range(range, height);
                        if live && requested.get() != Some(range) {
                            requested.set(Some(range));
                            // Native scroll prepaint sends directly; no polling latency
                            // and no browser wheel/root scrolling or UI image decode.
                            browser.visible_range(range.0, range.1);
                        }
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .inset_0(),
            )
            .on_any_mouse_down(cx.listener(|this, event: &MouseDownEvent, window, cx| {
                if this.error.is_some() { return; }
                this.cache_initial_layout = false;
                this.focus.focus(window, cx);
                let (button, mask) = mouse_button(event.button);
                this.buttons |= mask;
                let (x, y) = this.position(event.position);
                this.browser.mouse(
                    "mousePressed",
                    x,
                    y,
                    button,
                    this.buttons,
                    event.click_count as u32,
                );
                window.prevent_default();
                cx.stop_propagation();
            }))
            .capture_any_mouse_up(cx.listener(|this, event: &MouseUpEvent, _, cx| {
                if this.error.is_some() { return; }
                let (button, mask) = mouse_button(event.button);
                // A scrollbar or another native control owns a drag unless
                // this surface received its corresponding press.
                if this.buttons & mask == 0 { return; }
                this.buttons &= !mask;
                let (x, y) = this.position(event.position);
                this.browser.mouse(
                    "mouseReleased",
                    x,
                    y,
                    button,
                    this.buttons,
                    event.click_count as u32,
                );
                cx.stop_propagation();
            }))
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, _| {
                if this.error.is_some() { return; }
                let (x, y) = this.position(event.position);
                if event.pressed_button.is_some() && this.buttons == 0 { return; }
                this.browser
                    .mouse("mouseMoved", x, y, "none", this.buttons, 0);
            }))
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if this.error.is_some() || !this.focus.contains_focused(window, cx)
                    || !browser_handles_key(&event.keystroke)
                {
                    return;
                }
                this.keyboard(&event.keystroke, true);
                window.prevent_default();
                cx.stop_propagation();
            }))
            .capture_key_up(cx.listener(|this, event: &KeyUpEvent, window, cx| {
                if this.error.is_some() || !this.focus.contains_focused(window, cx)
                    || !browser_handles_key(&event.keystroke)
                {
                    return;
                }
                this.keyboard(&event.keystroke, false);
                window.prevent_default();
                cx.stop_propagation();
            }));
        // Selection drags can finish outside the surface. Always release the
        // browser's pressed button instead of leaving a stuck DOM drag.
        for button in MouseButton::all() {
            surface = surface.on_mouse_up_out(
                button,
                cx.listener(|this, event: &MouseUpEvent, _, cx| {
                    if this.error.is_some() { return; }
                    let (button, mask) = mouse_button(event.button);
                    if this.buttons & mask == 0 {
                        return;
                    }
                    this.buttons &= !mask;
                    let (x, y) = this.position(event.position);
                    this.browser.mouse(
                        "mouseReleased",
                        x,
                        y,
                        button,
                        this.buttons,
                        event.click_count as u32,
                    );
                    cx.stop_propagation();
                }),
            );
        }
        div().w_full().flex_shrink_0().children(warning).child(surface)
    }
}

/// A mail surface is read-only. Keep application shortcuts such as Ctrl+R
/// outside Chromium (where they could reload the document or open browser UI).
fn browser_handles_key(stroke: &Keystroke) -> bool {
    if stroke.modifiers.control || stroke.modifiers.platform {
        return matches!(stroke.key.as_str(), "a" | "c");
    }
    matches!(stroke.key.as_str(), "tab" | "enter" | "escape")
        || (stroke.modifiers.shift
            && matches!(
                stroke.key.as_str(),
                "left" | "right" | "up" | "down" | "home" | "end" | "pageup" | "pagedown"
            ))
}

fn document_hash(document: &str, allow_remote: bool, dark: bool) -> u64 {
    let mut hasher = DefaultHasher::new();
    (document, allow_remote, dark).hash(&mut hasher);
    hasher.finish()
}
fn local_position(position: Point<Pixels>, bounds: Bounds<Pixels>) -> (f64, f64) {
    (
        f64::from(f32::from(position.x - bounds.origin.x)),
        f64::from(f32::from(position.y - bounds.origin.y)),
    )
}
fn mouse_button(button: MouseButton) -> (&'static str, u32) {
    match button {
        MouseButton::Left => ("left", 1),
        MouseButton::Right => ("right", 2),
        MouseButton::Middle => ("middle", 4),
        _ => ("none", 0),
    }
}
pub(crate) fn allowed_url(url: &str) -> bool {
    if url.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return false;
    }
    let Some((scheme, rest)) = url.split_once(':') else {
        return false;
    };
    match scheme.to_ascii_lowercase().as_str() {
        "http" | "https" => rest
            .strip_prefix("//")
            .is_some_and(|host| !host.is_empty() && !host.starts_with('/')),
        "mailto" => !rest.is_empty(),
        _ => false,
    }
}
fn browser_key(key: &str) -> (String, String) {
    let mapped = match key {
        "tab" => ("Tab", "Tab"),
        "enter" => ("Enter", "Enter"),
        "escape" => ("Escape", "Escape"),
        "left" => ("ArrowLeft", "ArrowLeft"),
        "right" => ("ArrowRight", "ArrowRight"),
        "up" => ("ArrowUp", "ArrowUp"),
        "down" => ("ArrowDown", "ArrowDown"),
        "backspace" => ("Backspace", "Backspace"),
        "delete" => ("Delete", "Delete"),
        "home" => ("Home", "Home"),
        "end" => ("End", "End"),
        "pageup" => ("PageUp", "PageUp"),
        "pagedown" => ("PageDown", "PageDown"),
        "space" => (" ", "Space"),
        _ => {
            let code = if key.len() == 1 && key.as_bytes()[0].is_ascii_alphabetic() {
                format!("Key{}", key.to_ascii_uppercase())
            } else if key.len() == 1 && key.as_bytes()[0].is_ascii_digit() {
                format!("Digit{key}")
            } else {
                String::new()
            };
            return (key.to_owned(), code);
        }
    };
    (mapped.0.into(), mapped.1.into())
}

#[cfg(test)]
mod tests {
    use super::{allowed_url, browser_handles_key, browser_key, document_hash, local_position};
    use gpui_kit::{point, px, size, Bounds, Keystroke};

    fn tiny_frame() -> std::sync::Arc<gpui_kit::RenderImage> {
        let mut png = Vec::new();
        image::RgbaImage::new(1, 1).write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
        super::decode_frame(&png).unwrap()
    }

    #[test]
    fn finished_renders_are_found_by_document_and_size_and_bounded() {
        let size = (760, 1.25);
        assert!(!super::is_rendered("<p>a</p>", false, false, size));
        let key = super::render_key(document_hash("<p>a</p>", false, false), size);
        super::remember_render(super::FinishedRender { key, height: 640, tiles: vec![(0, 512, tiny_frame())] });
        assert!(super::is_rendered("<p>a</p>", false, false, size));
        // Another width, theme or consent is another render.
        assert!(!super::is_rendered("<p>a</p>", false, false, (900, 1.25)));
        assert!(!super::is_rendered("<p>a</p>", false, true, size));
        assert!(!super::is_rendered("<p>a</p>", true, false, size));
        assert_eq!(super::finished_render(key).map(|r| r.height), Some(640));
        for i in 0..super::FINISHED_LIMIT {
            let doc = format!("<p>{i}</p>");
            let key = super::render_key(document_hash(&doc, false, false), size);
            super::remember_render(super::FinishedRender { key, height: 1, tiles: Vec::new() });
        }
        assert!(!super::is_rendered("<p>a</p>", false, false, size), "oldest render is evicted");
    }

    fn tile(top: u32, revision: u64, value: u8) -> super::EncodedTile {
        super::EncodedTile {
            bytes: vec![value],
            width: 400,
            height: 512,
            top,
            revision,
        }
    }

    // Exercise the real worker and frontend ownership without a GUI or BGRA decode.
    fn chromium_cache_fixture() -> super::BrowserHandle {
        let sections = (0..32)
            .map(|i| format!("<section style='height:512px;background:rgb({},{},{})'></section>",
                i * 7, i * 3, i * 5))
            .collect::<String>();
        let handle = super::BrowserHandle::new(
            format!("<html><head><style>html,body{{margin:0;padding:0}}</style></head><body>{sections}</body></html>"),
            false,
            false,
        );
        handle.resize(400, 900, 1.0);
        // The worker adds one overscan tile: exactly twelve offsets, 0..=5632.
        handle.visible_range(0, 11 * 512);
        handle
    }

    fn drain_frontend_tiles(
        handle: &super::BrowserHandle,
        tiles: &mut super::Tiles<Vec<u8>>,
    ) -> (usize, Vec<(u32, u64)>) {
        let mut count = 0;
        let mut evicted = Vec::new();
        for event in handle.drain_events() {
            match event {
                super::BrowserEvent::Error(error) => panic!("Chromium: {}", super::classify_error(&error, tiles.cached.is_empty())),
                super::BrowserEvent::TileInvalidate { revision } => {
                    tiles.revision(revision);
                }
                super::BrowserEvent::Tile { png, width, height, top, revision } => {
                    count += 1;
                    assert!(!png.is_empty());
                    if let Some(top) = tiles.offer(super::EncodedTile {
                        bytes: png, width, height, top, revision,
                    }) {
                        evicted.push((top, revision));
                    }
                    while let Some((generation, tile)) = tiles.start() {
                        assert!(tiles.finish(generation, tile.top, tile.revision));
                        if let Some(top) = tiles.insert(tile.top, tile.height, tile.bytes) {
                            evicted.push((top, tile.revision));
                        }
                    }
                }
                _ => {}
            }
        }
        (count, evicted)
    }

    fn populate_chromium_frontend(
        handle: &super::BrowserHandle,
        tiles: &mut super::Tiles<Vec<u8>>,
    ) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while tiles.cached.len() < super::TILE_LIMIT {
            let (_, evicted) = drain_frontend_tiles(handle, tiles);
            for (top, revision) in evicted {
                handle.forget_tile(top, revision);
            }
            assert!(std::time::Instant::now() < deadline, "initial twelve tiles timed out");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let mut offsets = tiles.cached.iter().map(|tile| tile.top).collect::<Vec<_>>();
        offsets.sort_unstable();
        assert_eq!(offsets, (0..12).map(|i| i * 512).collect::<Vec<_>>());
    }

    #[test]
    #[ignore = "runs real sandboxed Chromium; no GUI or private data"]
    fn browser_body_frontend_eviction_ack_restores_backend_cached_tile() {
        let handle = chromium_cache_fixture();
        let mut tiles = super::Tiles::<Vec<u8>>::new();
        populate_chromium_frontend(&handle, &mut tiles);
        let original_tiles = tiles.cached.iter()
            .map(|tile| (tile.top, tile.image.clone()))
            .collect::<std::collections::HashMap<_, _>>();
        // Pin the oldest backend offset in the frontend, making their LRU victims differ.
        tiles.visible((0, 1));
        handle.visible_range(11 * 512, 12 * 512);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let (victim, revision) = loop {
            let (_, evicted) = drain_frontend_tiles(&handle, &mut tiles);
            if !evicted.is_empty() {
                assert_eq!(evicted.len(), 1);
                break evicted[0];
            }
            assert!(std::time::Instant::now() < deadline, "thirteenth tile timed out");
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        assert_ne!(victim, 0, "frontend must not choose the backend's oldest offset");
        assert!(tiles.cached.iter().any(|tile| tile.top == 0));
        assert!(!tiles.cached.iter().any(|tile| tile.top == victim));
        tiles.visible((victim, victim + 1));
        handle.visible_range(victim, victim + 1);
        // Deliberately defer the eviction acknowledgement: backend still owns this offset.
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(300);
        while std::time::Instant::now() < deadline {
            let (count, evicted) = drain_frontend_tiles(&handle, &mut tiles);
            assert_eq!(count, 0, "backend should still consider the frontend victim cached");
            assert!(evicted.is_empty());
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        handle.forget_tile(victim, revision);
        // The range is unchanged: ForgetTile itself must reschedule the missing demand.
        handle.visible_range(victim, victim + 1);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let mut captures = 0;
        while !tiles.cached.iter().any(|tile| tile.top == victim) {
            let (count, evicted) = drain_frontend_tiles(&handle, &mut tiles);
            captures += count;
            for (top, revision) in evicted {
                handle.forget_tile(top, revision);
            }
            assert!(std::time::Instant::now() < deadline, "acknowledged offset not redelivered within 2s");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(captures, 1);
        assert_eq!(tiles.revision, Some(revision));
        assert_eq!(
            &tiles.cached.iter().find(|tile| tile.top == victim).unwrap().image,
            &original_tiles[&victim],
            "redelivered tile must contain the original deterministic section",
        );
    }

    #[test]
    #[ignore = "runs real sandboxed Chromium; no GUI or private data"]
    fn browser_body_unchanged_visible_range_does_not_capture_populated_tiles() {
        let handle = chromium_cache_fixture();
        let mut tiles = super::Tiles::<Vec<u8>>::new();
        populate_chromium_frontend(&handle, &mut tiles);
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(600);
        let mut captures = 0;
        while std::time::Instant::now() < deadline {
            handle.visible_range(0, 11 * 512);
            let (count, evicted) = drain_frontend_tiles(&handle, &mut tiles);
            captures += count;
            assert!(evicted.is_empty());
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(captures, 0, "unchanged populated range must not emit new captures");
        assert_eq!(tiles.cached.len(), super::TILE_LIMIT);
    }

    #[test]
    fn browser_body_failure_classification_is_private_and_exact() {
        for (raw, code) in [
            ("Chromium pipe closed", "pipeClosed"),
            ("Chromium command timed out", "timeout"),
            ("Trusted browser DOM operation failed", "DOMUpdate"),
            ("Chromium rejected Page.captureScreenshot", "captureFailed"),
            ("Chromium rejected Fetch.fulfillRequest", "resourceCompletion"),
            ("Browser tile size limit exceeded", "limit"),
            ("Invalid Chromium tile", "decodeFailed"),
        ] {
            assert_eq!(super::classify_error(raw, false), code);
            let private = format!("{raw} https://private.example/token?secret=mail");
            assert_eq!(super::classify_error(&private, true), "engineStopped");
        }
        let missing = "Chromium is not installed; configure OVERLOOK_BROWSER_BIN";
        assert_eq!(super::classify_error(missing, true), "missingBinary");
        assert_eq!(super::classify_error(missing, false), "engineStopped");
        assert!(super::failure_message("missingBinary", false).contains("Install Chromium"));
        for raw in ["Cannot start sandboxed Chromium", "Cannot start browser worker", "https://private.example/secret"] {
            let code = super::classify_error(raw, true);
            assert!(!super::failure_message(code, false).contains("Install"));
            assert!(!super::failure_message(code, false).contains("private"));
        }
    }

    #[test]
    fn browser_body_failure_retains_tiles_height_and_rejects_active_decode() {
        let mut tiles = super::Tiles::new();
        tiles.offer(tile(0, 1, 0));
        let (generation, active) = tiles.start().unwrap();
        tiles.insert(512, 512, 42);
        let mut height = 4096;
        super::freeze_failure(&mut tiles, &mut height);
        assert_eq!(height, 4096);
        assert_eq!(tiles.cached.len(), 1);
        assert_eq!(tiles.cached[0].image, 42);
        assert!(!tiles.finish(generation, active.top, active.revision));
        assert!(tiles.pending.is_empty());
        assert!(super::failure_message("decodeFailed", true).contains("Content is frozen"));
        let mut empty = super::Tiles::<u32>::new();
        super::freeze_failure(&mut empty, &mut height);
        assert_eq!(height, 160);
    }

    #[test]
    fn browser_body_retry_resets_generation_and_preserves_prepared_consent() {
        for allow_remote in [false, true] {
            for dark in [false, true] {
                let prepared = super::RetryDocument::new("<p>original</p>".into(), allow_remote, dark);
                let html = prepared.html.clone();
                let mut tiles = super::Tiles::<u32>::new();
                tiles.offer(tile(0, 1, 0));
                let (generation, _) = tiles.start().unwrap();
                tiles.reset();
                tiles.offer(tile(0, 1, 1));
                assert!(!tiles.finish(generation, 0, 1));
                let (new_generation, active) = tiles.start().unwrap();
                assert!(tiles.finish(new_generation, active.top, active.revision));
                assert!(std::sync::Arc::ptr_eq(&html, &prepared.html));
                assert_eq!(&*prepared.html, "<p>original</p>");
                assert_eq!(prepared.allow_remote, allow_remote);
                assert_eq!(prepared.dark, dark);
            }
        }
    }

    #[test]
    fn pending_tiles_coalesce_by_offset_not_entire_frame() {
        let mut tiles = super::Tiles::<u32>::new();
        tiles.offer(tile(0, 1, 1));
        let (generation, active) = tiles.start().unwrap();
        tiles.offer(tile(512, 1, 2));
        tiles.offer(tile(1024, 1, 3));
        tiles.offer(tile(512, 1, 4));
        assert!(tiles.start().is_none());
        assert!(tiles.finish(generation, active.top, active.revision));
        assert_eq!(tiles.pending.len(), 2);
        assert_eq!(
            tiles.pending.iter().find(|t| t.top == 512).unwrap().bytes,
            [4]
        );
        for top in 0..100 {
            tiles.offer(tile(top * 512, 1, 0));
        }
        assert_eq!(tiles.pending.len(), super::TILE_LIMIT);
    }

    #[test]
    fn reflow_keeps_paint_until_new_revision_replaces_it() {
        let mut tiles = super::Tiles::<u32>::new();
        tiles.offer(tile(0, 1, 1));
        let (old_generation, _) = tiles.start().unwrap();
        tiles.insert(0, 512, 1);
        assert!(tiles.revision(2));
        assert_eq!(tiles.cached[0].image, 1, "Do not blank the visible body during layout updates");
        assert!(!tiles.finish(old_generation, 0, 1));
        tiles.offer(tile(0, 2, 2));
        let (generation, new_tile) = tiles.start().unwrap();
        assert!(tiles.finish(generation, new_tile.top, new_tile.revision));
        tiles.insert(0, 512, 2);
        assert_eq!(tiles.cached.len(), 1);
        assert_eq!(tiles.cached[0].image, 2);
        tiles.offer(tile(0, 1, 3));
        assert!(tiles.pending.is_empty(), "A late old revision cannot overwrite new paint");
        tiles.reset();
        assert!(tiles.cached.is_empty(), "Explicit document/consent reset must discard previous pixels");
    }

    #[test]
    fn invalidation_rejects_old_decode_without_parallel_work() {
        let mut tiles = super::Tiles::<u32>::new();
        tiles.offer(tile(0, 1, 1));
        let (old_generation, _) = tiles.start().unwrap();
        tiles.insert(0, 512, 1);
        tiles.invalidate(); // resize: wait for a newer backend revision
        tiles.awaiting_revision = true;
        tiles.offer(tile(512, 1, 2));
        assert!(tiles.pending.is_empty());
        assert!(tiles.cached.is_empty());
        assert!(!tiles.revision(1));
        assert!(tiles.revision(2));
        tiles.offer(tile(512, 2, 3));
        assert!(tiles.start().is_none());
        assert!(!tiles.finish(old_generation, 0, 1));
        let (generation, active) = tiles.start().unwrap();
        assert_ne!(generation, old_generation);
        assert!(tiles.finish(generation, active.top, active.revision));
        tiles.offer(tile(0, 1, 0));
        assert!(tiles.pending.is_empty());
    }

    #[test]
    fn replaced_active_offset_and_error_cannot_publish() {
        let mut tiles = super::Tiles::<u32>::new();
        tiles.offer(tile(0, 1, 1));
        let (generation, _) = tiles.start().unwrap();
        tiles.offer(tile(0, 1, 2));
        tiles.offer(tile(512, 1, 3));
        assert!(!tiles.finish(generation, 0, 1));
        let (generation, active) = tiles.start().unwrap();
        tiles.invalidate();
        assert!(!tiles.finish(generation, active.top, active.revision));
        assert!(tiles.start().is_none());
    }

    #[test]
    fn lru_is_bounded_and_visible_tiles_are_pinned() {
        let mut tiles = super::Tiles::new();
        tiles.visible((0, 1024));
        for top in 0..100 {
            tiles.insert(top * 512, 512, top);
        }
        assert_eq!(tiles.cached.len(), super::TILE_LIMIT);
        assert!(tiles.cached.iter().any(|t| t.top == 0));
        assert!(tiles.cached.iter().any(|t| t.top == 512));
        assert!(!tiles.cached.iter().any(|t| t.top == 1024));
        tiles.visible((99 * 512, 100 * 512));
        tiles.insert(100 * 512, 512, 100);
        assert!(!tiles.cached.iter().any(|t| t.top == 0));
        assert!(tiles.cached.iter().any(|t| t.top == 99 * 512));
        tiles.revision(2);
        assert_eq!(tiles.cached.len(), super::TILE_LIMIT, "Reflow retains paint without allocating a second cache");
    }

    #[test]
    fn cached_overscan_is_refreshed_without_a_redecode() {
        let mut tiles = super::Tiles::new();
        for top in 0..super::TILE_LIMIT as u32 {
            tiles.insert(top * 512, 512, top);
        }
        tiles.visible((512, 1024));
        tiles.insert(12 * 512, 512, 12);
        assert!(tiles.cached.iter().any(|t| t.top == 0));
        assert!(tiles.cached.iter().any(|t| t.top == 3 * 512));
        assert!(!tiles.cached.iter().any(|t| t.top == 4 * 512));
        assert_eq!(tiles.cached.len(), super::TILE_LIMIT);
        assert!(tiles.pending.is_empty());
    }

    #[test]
    fn clipped_visibility_does_not_shrink_document_and_mouse_is_document_relative() {
        let bounds = Bounds::new(point(px(100.), px(-2048.)), size(px(400.), px(100_000.)));
        let mask = Bounds::new(point(px(100.), px(0.)), size(px(400.), px(160.)));
        assert_eq!(super::visible_range(bounds, mask, 100_000), (2048, 2208));
        assert_eq!(super::request_range((2048, 2208), 100_000), (1536, 2720));
        assert_eq!(
            local_position(point(px(125.), px(40.)), bounds),
            (25., 2088.)
        );
        assert_eq!(
            super::request_range((99_900, 100_000), 100_000),
            (99_388, 100_000)
        );
    }
    #[test]
    fn coordinates_are_css_relative() {
        assert_eq!(
            local_position(
                point(px(125.0), px(240.0)),
                Bounds::new(point(px(100.0), px(200.0)), size(px(400.0), px(300.0)))
            ),
            (25.0, 40.0)
        );
    }
    #[test]
    fn keys_and_unicode() {
        assert_eq!(
            browser_key("left"),
            ("ArrowLeft".into(), "ArrowLeft".into())
        );
        assert_eq!(browser_key("a"), ("a".into(), "KeyA".into()));
        assert_eq!(browser_key("é"), ("é".into(), String::new()));
    }
    #[test]
    fn safe_link_schemes_only() {
        for url in [
            "https://example.org",
            "HTTP://example.org",
            "mailto:a@example.org",
        ] {
            assert!(allowed_url(url));
        }
        for url in [
            "javascript:alert(1)",
            "file:///etc/passwd",
            "data:text/html,x",
            "https://",
            "https://a\nb",
        ] {
            assert!(!allowed_url(url));
        }
    }
    #[test]
    fn application_shortcuts_do_not_reach_chromium() {
        assert!(browser_handles_key(&Keystroke::parse("ctrl-c").unwrap()));
        assert!(browser_handles_key(&Keystroke::parse("ctrl-a").unwrap()));
        for key in [
            "down", "up", "left", "right", "pageup", "pagedown", "home", "end",
        ] {
            assert!(!browser_handles_key(&Keystroke::parse(key).unwrap()));
        }
        assert!(browser_handles_key(
            &Keystroke::parse("shift-down").unwrap()
        ));
        assert!(browser_handles_key(&Keystroke::parse("tab").unwrap()));
        assert!(browser_handles_key(&Keystroke::parse("enter").unwrap()));
        assert!(!browser_handles_key(&Keystroke::parse("ctrl-r").unwrap()));
        assert!(!browser_handles_key(&Keystroke::parse("ctrl-v").unwrap()));
        assert!(!browser_handles_key(&Keystroke::parse("j").unwrap()));
    }

    #[test]
    fn options_restart_engine() {
        assert_ne!(
            document_hash("a", false, false),
            document_hash("a", true, false)
        );
        assert_ne!(
            document_hash("a", false, false),
            document_hash("a", false, true)
        );
    }
}
