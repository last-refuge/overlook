//! The tray icon, while "Keep running when the window is closed" is on (`lifecycle` starts and
//! stops it). A StatusNotifierItem, which Omarchy's bar and most Linux trays show: the
//! Overlook mark, with a dot while the Inbox has unread mail, and the count in its tooltip.
//! A click opens the window; the menu also has Check for new mail and Quit.
//!
//! Its D-Bus service runs on ksni's own thread. What it's asked to do goes through the same
//! queue as a later launch's request (`single_instance::push`), so the app serves it the
//! same way, hidden or not.

use ksni::blocking::{Handle, TrayMethods};
use ksni::menu::{MenuItem, StandardItem};
use std::path::PathBuf;
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
        let apps = base.join("hicolor/scalable/apps");
        std::fs::create_dir_all(&apps).ok()?;
        for (name, svg) in [(PLAIN_ICON, PLAIN_SVG), (UNREAD_ICON, UNREAD_SVG)] {
            for dir in [&base, &apps] {
                std::fs::write(dir.join(format!("{name}.svg")), svg).ok()?;
            }
        }
        Some(base)
    })
    .clone()
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
    fn the_unread_icon_is_well_formed() {
        assert!(UNREAD_SVG.starts_with("<svg") && UNREAD_SVG.ends_with("</svg>"));
        assert!(PLAIN_SVG.contains("currentColor"));
    }
}
