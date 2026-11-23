//! The tray icon for the life of the app. A StatusNotifierItem, which Omarchy's bar and
//! other compatible Linux trays show: the
//! Overlook mark, with a dot while the Inbox has unread mail, and the count in its tooltip.
//! A click opens Overlook; the menu also opens reminders, checks for new mail, and quits.
//!
//! Its D-Bus service runs on ksni's own thread. What it's asked to do goes through the same
//! queue as a later launch's request (`single_instance::push`), so the app serves it the
//! same way, hidden or not.

use ksni::blocking::{Handle, TrayMethods};
use ksni::menu::{MenuItem, StandardItem};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Sender};
use std::sync::OnceLock;

const PLAIN_ICON: &str = "ai.lastrefuge.Overlook-symbolic";
const UNREAD_ICON: &str = "ai.lastrefuge.Overlook-unread-symbolic";

const PLAIN_SVG: &str = include_str!("../../../branding/assets/overlook-symbolic.svg");
/// The symbolic mark with a dot at the top right; the ring opens around the dot, since a
/// symbolic icon is drawn in one color.
const UNREAD_SVG: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="128" height="128" viewBox="0 0 128 128"><path d="M 111.33 51.32 A 49 49 0 1 1 76.68 16.67" fill="none" stroke="currentColor" stroke-width="10" stroke-linecap="round"/><path d="M 21 79 L 48 59 L 72 77 L 92 64 L 109 78" fill="none" stroke="currentColor" stroke-width="8" stroke-linejoin="round" stroke-linecap="round"/><circle cx="104" cy="24" r="15" fill="currentColor"/></svg>"#;

pub struct TrayIcon {
    handle: Handle<OverlookTray>,
    unread: usize,
}

impl TrayIcon {
    /// Registers the icon. It shows as soon as a tray is there, including one that starts
    /// after Overlook does at login.
    pub fn start(unread: usize) -> Option<Self> {
        let tray = OverlookTray { unread, icon_dir: icon_dir().map(|d| d.to_string_lossy().into_owned()).unwrap_or_default() };
        match tray.assume_sni_available(true).spawn() {
            Ok(handle) => Some(Self { handle, unread }),
            Err(e) => {
                tracing::warn!("the tray icon could not start: {e}");
                None
            }
        }
    }

    /// Shows a new unread count; does nothing when it hasn't changed.
    pub fn set_unread(&mut self, unread: usize) {
        if unread != self.unread {
            self.unread = unread;
            self.handle.update(|t| t.unread = unread);
        }
    }
}

impl Drop for TrayIcon {
    fn drop(&mut self) {
        let _ = self.handle.shutdown();
    }
}

enum TrayCommand {
    Sync { enabled: bool, unread: Option<usize> },
    Shutdown,
}

/// Non-blocking handle to a dedicated tray worker thread so D-Bus startup, updates and
/// shutdown never block the GPUI UI thread.
#[derive(Default)]
pub struct TrayWorker {
    tx: Option<Sender<TrayCommand>>,
    last_enabled: bool,
    last_unread: Option<usize>,
}

impl TrayWorker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Reconciles the tray icon with `enabled` and `unread` asynchronously off the UI thread.
    pub fn sync(&mut self, enabled: bool, unread: Option<usize>) {
        if enabled == self.last_enabled && (!enabled || unread.is_none() || unread == self.last_unread) {
            return;
        }
        self.last_enabled = enabled;
        if enabled {
            if unread.is_some() {
                self.last_unread = unread;
            }
        } else {
            self.last_unread = None;
        }
        if !enabled && self.tx.is_none() {
            return;
        }
        let tx = self.tx.get_or_insert_with(spawn_tray_worker);
        if tx.send(TrayCommand::Sync { enabled, unread }).is_err() {
            // Worker exited unexpectedly; recreate if still enabled.
            self.tx = None;
            if enabled {
                let fresh = spawn_tray_worker();
                let _ = fresh.send(TrayCommand::Sync { enabled, unread });
                self.tx = Some(fresh);
            }
        }
    }

    /// Asks the tray worker to shut down the icon and exit without blocking the caller.
    pub fn shutdown(&mut self) {
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(TrayCommand::Shutdown);
        }
        self.last_enabled = false;
        self.last_unread = None;
    }
}

impl Drop for TrayWorker {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[derive(Debug, PartialEq, Eq)]
enum CoalescedTrayCommand {
    Sync { enabled: bool, unread: Option<usize>, reset_failed: bool },
    Shutdown,
}

fn coalesce_tray_commands(first: TrayCommand, rest: impl IntoIterator<Item = TrayCommand>) -> CoalescedTrayCommand {
    let (mut enabled, mut unread, mut reset_failed) = match first {
        TrayCommand::Shutdown => return CoalescedTrayCommand::Shutdown,
        TrayCommand::Sync { enabled, unread } => (enabled, if enabled { unread } else { None }, !enabled),
    };
    for next in rest {
        match next {
            TrayCommand::Shutdown => return CoalescedTrayCommand::Shutdown,
            TrayCommand::Sync { enabled: next_enabled, unread: next_unread } => {
                if !next_enabled {
                    reset_failed = true;
                    unread = None;
                } else if next_unread.is_some() {
                    unread = next_unread;
                }
                enabled = next_enabled;
            }
        }
    }
    CoalescedTrayCommand::Sync { enabled, unread, reset_failed }
}

fn spawn_tray_worker() -> Sender<TrayCommand> {
    let (tx, rx) = channel::<TrayCommand>();
    let _ = std::thread::Builder::new().name("overlook-tray".into()).spawn(move || {
        let mut tray: Option<TrayIcon> = None;
        let mut failed = false;
        while let Ok(first) = rx.recv() {
            // Coalesce queued state updates so only the latest state hits D-Bus,
            // while preserving any disabled edge so a prior start failure retries.
            let mut rest = Vec::new();
            while let Ok(newer) = rx.try_recv() {
                let is_shutdown = matches!(newer, TrayCommand::Shutdown);
                rest.push(newer);
                if is_shutdown {
                    break;
                }
            }
            match coalesce_tray_commands(first, rest) {
                CoalescedTrayCommand::Shutdown => break,
                CoalescedTrayCommand::Sync { enabled, unread, reset_failed } => {
                    if reset_failed {
                        failed = false;
                    }
                    match (&mut tray, enabled) {
                        (None, true) if !failed => {
                            tray = TrayIcon::start(unread.unwrap_or(0));
                            failed = tray.is_none();
                        }
                        (None, true) => {}
                        (Some(_), false) => {
                            tray = None;
                        }
                        (Some(icon), true) => {
                            if let Some(count) = unread {
                                icon.set_unread(count);
                            }
                        }
                        (None, false) => {
                            failed = false;
                        }
                    }
                }
            }
        }
        drop(tray);
    });
    tx
}

struct OverlookTray {
    unread: usize,
    icon_dir: String,
}

impl ksni::Tray for OverlookTray {
    fn id(&self) -> String {
        crate::APP_ID.into()
    }

    fn title(&self) -> String {
        "Overlook".into()
    }

    fn category(&self) -> ksni::Category {
        ksni::Category::Communications
    }

    fn icon_theme_path(&self) -> String {
        self.icon_dir.clone()
    }

    fn icon_name(&self) -> String {
        if self.unread > 0 { UNREAD_ICON } else { PLAIN_ICON }.into()
    }

    /// For a tray that can't find the named icon: the full-color app icon.
    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        pixmaps().clone()
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip { title: tooltip(self.unread), ..Default::default() }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        crate::single_instance::push(crate::single_instance::SHOW.into());
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let item = |label: &str, line: &'static str| -> MenuItem<Self> {
            StandardItem { label: label.into(), activate: Box::new(move |_| crate::single_instance::push(line.into())), ..Default::default() }.into()
        };
        vec![
            item("Open Overlook", crate::single_instance::SHOW),
            item("Calendar reminders", crate::single_instance::REMINDERS),
            item("Check for new mail", crate::single_instance::SYNC),
            MenuItem::Separator,
            item("Quit Overlook", crate::single_instance::QUIT),
        ]
    }

    /// The bar went away (restarted, say): keep the service, so the icon returns with it.
    fn watcher_offline(&self, _reason: ksni::OfflineReason) -> bool {
        true
    }
}

fn tooltip(unread: usize) -> String {
    match unread {
        0 => "Overlook".into(),
        1 => "Overlook · 1 unread".into(),
        n => format!("Overlook · {n} unread"),
    }
}

/// Writes the two symbolic icons where the tray can find them by name: directly in the folder
/// and as a `hicolor` theme inside it, since trays search `IconThemePath` either way.
fn icon_dir() -> Option<PathBuf> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| {
        let base = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir).join("overlook-tray-icons");
        write_icons(&base).ok()?;
        Some(base)
    })
    .clone()
}

/// The folder also holds each icon under its bare name, with no extension. Quickshell, which draws
/// Omarchy's bar, falls back to `IconThemePath` by opening `<IconThemePath>/<IconName>` exactly as
/// written, so a file called `<IconName>.svg` is never found and the bar shows its missing-icon
/// checkerboard instead of the Overlook mark. Qt reads the SVG from the bare file by its contents.
fn write_icons(base: &Path) -> std::io::Result<()> {
    let apps = base.join("hicolor/scalable/apps");
    std::fs::create_dir_all(&apps)?;
    for (name, svg) in [(PLAIN_ICON, PLAIN_SVG), (UNREAD_ICON, UNREAD_SVG)] {
        for dir in [base, apps.as_path()] {
            std::fs::write(dir.join(format!("{name}.svg")), svg)?;
        }
        std::fs::write(base.join(name), svg)?;
    }
    Ok(())
}

fn pixmaps() -> &'static Vec<ksni::Icon> {
    static ICONS: OnceLock<Vec<ksni::Icon>> = OnceLock::new();
    ICONS.get_or_init(|| {
        [&include_bytes!("../../../branding/assets/overlook-icon-32.png")[..], &include_bytes!("../../../branding/assets/overlook-icon-64.png")[..]]
            .iter()
            .filter_map(|png| image::load_from_memory_with_format(png, image::ImageFormat::Png).ok())
            .map(|img| argb_icon(&img.to_rgba8()))
            .collect()
    })
}

/// StatusNotifierItem pixmaps are ARGB32 in network byte order.
fn argb_icon(rgba: &image::RgbaImage) -> ksni::Icon {
    let data = rgba.pixels().flat_map(|p| [p[3], p[0], p[1], p[2]]).collect();
    ksni::Icon { width: rgba.width() as i32, height: rgba.height() as i32, data }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tooltip_counts_unread_mail() {
        assert_eq!(tooltip(0), "Overlook");
        assert_eq!(tooltip(1), "Overlook · 1 unread");
        assert_eq!(tooltip(12), "Overlook · 12 unread");
    }

    #[test]
    fn pixmaps_are_argb_in_network_order() {
        let mut img = image::RgbaImage::new(1, 1);
        img.put_pixel(0, 0, image::Rgba([10, 20, 30, 40]));
        let icon = argb_icon(&img);
        assert_eq!(icon.data, vec![40, 10, 20, 30]);
        assert!(pixmaps().iter().any(|i| i.width == 32) && pixmaps().iter().any(|i| i.width == 64), "both sizes decode");
    }

    #[test]
    fn each_icon_is_written_under_its_bare_name_and_as_svg() {
        let base = std::env::temp_dir().join(format!("overlook-tray-icons-test-{}", std::process::id()));
        write_icons(&base).unwrap();
        for (name, svg) in [(PLAIN_ICON, PLAIN_SVG), (UNREAD_ICON, UNREAD_SVG)] {
            // Quickshell opens `<IconThemePath>/<IconName>` as is; other trays look for the .svg
            // beside it or in the `hicolor` theme.
            assert_eq!(std::fs::read_to_string(base.join(name)).unwrap(), svg);
            assert_eq!(std::fs::read_to_string(base.join(format!("{name}.svg"))).unwrap(), svg);
            assert_eq!(std::fs::read_to_string(base.join("hicolor/scalable/apps").join(format!("{name}.svg"))).unwrap(), svg);
        }
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn the_unread_icon_is_well_formed() {
        assert!(UNREAD_SVG.starts_with("<svg") && UNREAD_SVG.ends_with("</svg>"));
        assert!(PLAIN_SVG.contains("currentColor"));
    }

    #[test]
    fn tray_worker_clears_last_unread_on_disable_and_shutdown() {
        let (tx, rx) = channel();
        let mut worker = TrayWorker {
            tx: Some(tx),
            last_enabled: false,
            last_unread: None,
        };
        worker.sync(true, Some(5));
        worker.sync(false, None);
        worker.sync(true, None);
        worker.sync(true, Some(5));
        worker.shutdown();
        assert_eq!(worker.last_unread, None);
        assert!(!worker.last_enabled);

        let sent: Vec<_> = rx.try_iter().collect();
        assert_eq!(sent.len(), 5, "re-enabling with None must not suppress subsequent Some(5)");
        assert!(matches!(sent[0], TrayCommand::Sync { enabled: true, unread: Some(5) }));
        assert!(matches!(sent[1], TrayCommand::Sync { enabled: false, unread: None }));
        assert!(matches!(sent[2], TrayCommand::Sync { enabled: true, unread: None }));
        assert!(matches!(sent[3], TrayCommand::Sync { enabled: true, unread: Some(5) }));
        assert!(matches!(sent[4], TrayCommand::Shutdown));
    }

    #[test]
    fn coalescing_preserves_disabled_edge_for_failed_reset() {
        let res = coalesce_tray_commands(
            TrayCommand::Sync { enabled: false, unread: None },
            [TrayCommand::Sync { enabled: true, unread: Some(3) }],
        );
        assert_eq!(res, CoalescedTrayCommand::Sync { enabled: true, unread: Some(3), reset_failed: true });

        let res = coalesce_tray_commands(
            TrayCommand::Sync { enabled: true, unread: Some(5) },
            [TrayCommand::Sync { enabled: true, unread: None }],
        );
        assert_eq!(res, CoalescedTrayCommand::Sync { enabled: true, unread: Some(5), reset_failed: false });

        let res = coalesce_tray_commands(
            TrayCommand::Sync { enabled: true, unread: Some(5) },
            [TrayCommand::Shutdown],
        );
        assert_eq!(res, CoalescedTrayCommand::Shutdown);
    }
}
