//! Overlook's built-in themes. Light uses the Soft stone palette.
//!
//! `Palette` holds the semantic tokens the custom mail views paint with. The same values are
//! projected onto the component library's theme so inputs, menus, buttons, and scrollbars
//! match. Switching is instant and touches only colors: entities, focus, drafts, selection,
//! and sync state are untouched.

use gpui_kit::component::{Theme, ThemeConfig, ThemeMode};
use gpui_kit::{App, Global, Hsla, Rgba, WindowAppearance};
use overlook_core::{AppearancePreference, ResolvedTheme, SystemAppearance};
use std::rc::Rc;

#[derive(Clone, Copy, Debug)]
#[allow(dead_code)] // Every brand token is defined even where the shell does not use it yet.
pub struct Palette {
    pub resolved: ResolvedTheme,
    pub background: Hsla,
    pub surface: Hsla,
    pub sidebar: Hsla,
    pub foreground: Hsla,
    pub muted: Hsla,
    pub divider: Hsla,
    pub control_border: Hsla,
    pub hover: Hsla,
    pub accent: Hsla,
    pub action: Hsla,
    pub on_action: Hsla,
    pub selection: Hsla,
    pub selection_text: Hsla,
    pub focus: Hsla,
    pub success: Hsla,
    pub warning: Hsla,
    pub error: Hsla,
    pub brand_accent: Hsla,
    pub avatar: Hsla,
    pub avatar_ink: Hsla,
    pub chip: Hsla,
    pub label_slate: Hsla,
    pub label_plum: Hsla,
    pub label_amber: Hsla,
}

impl Global for Palette {}

/// Email paper is independent of the app chrome, including its Dark color scheme.
pub const EMAIL_PAPER: u32 = 0xFFFFFF;
pub const EMAIL_INK: u32 = 0x20242B;

fn c(hex: u32) -> Hsla {
    Rgba::from(gpui_kit::rgb(hex)).into()
}

impl Palette {
    pub fn light() -> Self {
        Self {
            resolved: ResolvedTheme::Light,
            background: c(0xCDD1C9),
            surface: c(0xE1E2DC),
            sidebar: c(0xC4CBC1),
            foreground: c(0x29332E),
            muted: c(0x4B564D),
            divider: c(0xA7B0A5),
            control_border: c(0x657267),
            hover: c(0xD5DAD1),
            accent: c(0x3F6654),
            action: c(0x3F6654),
            on_action: c(0xFFFFFF),
            selection: c(0xBDCDBF),
            selection_text: c(0x29332E),
            focus: c(0x92581D),
            success: c(0x386449),
            warning: c(0x805015),
            error: c(0xA13E37),
            brand_accent: c(0x92581D),
            avatar: c(0xBDCDBF),
            avatar_ink: c(0x29332E),
            chip: c(0xD5DAD1),
            label_slate: c(0x3F6654),
            label_plum: c(0x7C6BB6),
            label_amber: c(0xC98A3A),
        }
    }

    pub fn dark() -> Self {
        Self {
            resolved: ResolvedTheme::Dark,
            background: c(0x101A21),
            surface: c(0x17242C),
            sidebar: c(0x121F27),
            foreground: c(0xF0EBDD),
            muted: c(0xA8B7BA),
            divider: c(0x30424B),
            control_border: c(0x71878D),
            hover: c(0x23353F),
            accent: c(0xA9CDD9),
            action: c(0xA9CDD9),
            on_action: c(0x10232D),
            selection: c(0x28404C),
            selection_text: c(0xDCECEF),
            focus: c(0xFFBA6B),
            success: c(0xA8CEB1),
            warning: c(0xF1BE78),
            error: c(0xFFB0A6),
            brand_accent: c(0xFF9D32),
            avatar: c(0x34483F),
            avatar_ink: c(0xC2D5C3),
            chip: c(0x22323B),
            label_slate: c(0xA9CDD9),
            label_plum: c(0xC5AAFF),
            label_amber: c(0xDBB980),
        }
    }

    pub fn for_theme(theme: ResolvedTheme) -> Self {
        let colors = match theme {
            ResolvedTheme::Light => return Self::light(),
            ResolvedTheme::Dark => return Self::dark(),
            ResolvedTheme::SystemLight | ResolvedTheme::SystemDark => {
                let desktop = DESKTOP.read().ok().and_then(|d| d.as_ref().map(|(_, p)| *p)).filter(|p| p.resolved == theme);
                return desktop.unwrap_or_else(|| Self { resolved: theme, ..if theme.is_dark() { Self::dark() } else { Self::light() } });
            }
            // Background, surface, sidebar, text, muted, divider, control border,
            // hover, action, selection, selected text, and focus ring.
            ResolvedTheme::Gray => [0xE5E7EB, 0xF3F4F6, 0xD1D5DB, 0x1F2937, 0x4B5563, 0xB8BEC7, 0x6B7280, 0xDDE1E7, 0x374151, 0xCDD5DF, 0x1F2937, 0x805015],
            ResolvedTheme::Blue => [0xEDF4FC, 0xF8FBFF, 0xDCE9F8, 0x172D4D, 0x4E6480, 0xC7D6E8, 0x657F9F, 0xE2EDFA, 0x245A96, 0xD3E4F8, 0x193F70, 0x805015],
            ResolvedTheme::White => [0xFFFFFF, 0xFFFFFF, 0xFAFAFA, 0x20242B, 0x59616D, 0xE0E3E7, 0x767D87, 0xF0F2F5, 0x374151, 0xE5E9EF, 0x20242B, 0x805015],
            ResolvedTheme::Alexander => [0xE3E9EE, 0xFFFFFF, 0xE0E7ED, 0x182B46, 0x48596C, 0xC3CFD9, 0x63748A, 0xEAF0F4, 0x1F57A5, 0xE0EAF0, 0x17314D, 0x1F57A5],
            ResolvedTheme::Pink => [0xFCF0F5, 0xFFFAFC, 0xF5DFE9, 0x402536, 0x765364, 0xE8C9D8, 0xA06A84, 0xF6E4ED, 0x963D68, 0xF0D4E2, 0x692C4B, 0x805015],
        };
        let [background, surface, sidebar, foreground, muted, divider, control_border, hover, action, selection, selection_text, focus] = colors.map(c);
        let mut palette = Self {
            resolved: theme,
            background, surface, sidebar, foreground, muted, divider, control_border,
            hover, accent: action, action, selection, selection_text, focus,
            avatar: selection, avatar_ink: selection_text, chip: hover, label_slate: action,
            brand_accent: c(0xFF9D32),
            ..Self::light()
        };
        if theme == ResolvedTheme::Alexander {
            // Washed-out blue marks selected folders and messages; avatars stay gray.
            palette.avatar = c(0xDDE0E4);
            palette.chip = palette.hover;
            palette.brand_accent = c(0xFF9F0A);
            palette.label_amber = c(0xFF9F0A);
        }
        palette
    }

    /// Navigation has its own contrast context, independent of white mail surfaces.
    /// Alexander shares its custom blue across navigation and matching pane gutters.
    pub fn navigation_palette(&self) -> Self {
        let mut p = *self;
        if self.resolved == ResolvedTheme::Alexander {
            p.sidebar = c(0x1F343F);
            p.foreground = c(0xF3F5F7);
            p.muted = c(0xBFCAD7);
            p.hover = c(0x3A4758);
            p.divider = c(0x405064);
            p.accent = self.selection_text;
            p.success = c(0xA8CEB1);
            p.warning = c(0xF1BE78);
            p.error = c(0xFFB0A6);
        }
        p
    }

    pub fn message_accent(&self) -> Hsla {
        if self.resolved == ResolvedTheme::Alexander { c(0xFF9F0A) } else { self.accent }
    }

    /// Keep the message center white in every light colorway, including the stone Light.
    pub fn message_surface(&self) -> Hsla {
        if self.is_dark() { self.surface } else { c(0xFFFFFF) }
    }

    /// Light lifts the message list to a pale green above its stone frame; other
    /// themes paint the list with their surface.
    pub fn list_surface(&self) -> Hsla {
        if self.resolved == ResolvedTheme::Light { c(0xF3F7F1) } else { self.surface }
    }

    /// Unread rows sit one step darker than the list they belong to.
    pub fn list_unread(&self) -> Hsla {
        if self.resolved == ResolvedTheme::Light { c(0xE3EBE0) } else { self.background }
    }

    /// Keep mail on light paper in every shell theme; sender-authored colors still win.
    pub fn prepare_browser_document(&self, source: &str) -> overlook_core::BrowserDocument {
        overlook_core::browser::prepare_document_with_colors(source, EMAIL_PAPER, EMAIL_INK)
    }

    pub fn is_dark(&self) -> bool {
        self.resolved.is_dark()
    }

    /// Color for a user label dot by its token name.
    pub fn label_color(&self, token: Option<&str>) -> Hsla {
        match token {
            Some("plum") => self.label_plum,
            Some("amber") => self.label_amber,
            _ => self.label_slate,
        }
    }
}

pub trait ActivePalette {
    fn palette(&self) -> &Palette;
}

impl ActivePalette for App {
    fn palette(&self) -> &Palette {
        self.global::<Palette>()
    }
}

/// Project the same palette onto native controls and rich-text rendering.
fn component_colors(theme: ResolvedTheme) -> serde_json::Value {
    let base = if theme.is_dark() { DARK_JSON } else { LIGHT_JSON };
    let mut colors: serde_json::Value = serde_json::from_str(base).expect("valid color tokens");
    let p = Palette::for_theme(theme);
    if !theme.is_dark() {
        light_component_colors(&mut colors, &p);
    } else if theme != ResolvedTheme::Dark {
        // The dark tokens are written in the built-in Dark palette already.
        dark_component_colors(&mut colors, &p);
    }
    if theme == ResolvedTheme::Light {
        colors["input.border"] = serde_json::Value::String(color_hex(p.control_border));
    }
    // Selected text: a translucent action tint shows on every field background, where the
    // list highlight (`p.selection`) nearly matches the Light theme's inputs.
    colors["selection.background"] = serde_json::Value::String(format!("{}55", color_hex(p.action)));
    // A switch that is off is grey; only an on switch takes the action color.
    colors["switch.background"] = serde_json::Value::String(color_hex(p.muted));
    colors
}

fn shade(color: Hsla, alpha: f32) -> Hsla {
    Rgba::from(color).blend(gpui_kit::rgb(0).alpha(alpha)).into()
}

fn lift(color: Hsla, alpha: f32) -> Hsla {
    Rgba::from(color).blend(gpui_kit::rgb(0xFFFFFF).alpha(alpha)).into()
}

/// Rewrites the dark tokens (written in [`Palette::dark`]) into a dark desktop palette.
fn dark_component_colors(colors: &mut serde_json::Value, p: &Palette) {
    for value in colors.as_object_mut().expect("color object").values_mut() {
        let replacement = match value.as_str().unwrap_or_default() {
            "#101A21" => p.background,
            "#17242C" => p.surface,
            "#121F27" => p.sidebar,
            "#F0EBDD" => p.foreground,
            "#A8B7BA" => p.muted,
            "#30424B" | "#3E535D" => p.divider,
            "#3B4F59" | "#55707C" => p.control_border,
            "#FFBA6B" => p.focus,
            "#A9CDD9" => p.action,
            "#B8D7E1" => lift(p.action, 0.12),
            "#98BFCC" => shade(p.action, 0.12),
            "#10232D" => p.on_action,
            "#23353F" => p.hover,
            "#2A3F4A" => lift(p.hover, 0.04),
            "#1E2E37" => shade(p.hover, 0.1),
            "#28404C" => p.selection,
            "#DCECEF" => p.selection_text,
            "#FFB0A6" => p.error,
            "#FFC0B8" => lift(p.error, 0.12),
            "#F09E94" => shade(p.error, 0.08),
            "#A8CEB1" => p.success,
            "#F1BE78" => p.warning,
            _ => continue, // Overlays and transparent scrollbar tracks.
        };
        *value = serde_json::Value::String(color_hex(replacement));
    }
}

/// Rewrites the light tokens into any light palette.
fn light_component_colors(colors: &mut serde_json::Value, p: &Palette) {
    for value in colors.as_object_mut().expect("color object").values_mut() {
        let replacement = match value.as_str().unwrap_or_default() {
            "#F4F3EE" => p.background,
            "#FFFEFB" => p.surface,
            "#F0F1EC" => p.sidebar,
            "#192B35" => p.foreground,
            "#5E6D73" => p.muted,
            "#DCE2DF" | "#C9D2CF" => p.divider,
            "#986020" => p.focus,
            "#365F73" => p.action,
            "#2E5366" => shade(p.action, 0.12),
            "#264757" => shade(p.action, 0.24),
            "#E8EDE9" => p.hover,
            "#DFE6E1" => shade(p.hover, 0.03),
            "#D4DDD7" => shade(p.hover, 0.06),
            "#E3EDEA" | "#C9DCD6" => p.selection,
            "#284D5D" => p.selection_text,
            "#B9C4C0" => p.divider,
            "#9DABA7" => p.control_border,
            _ => continue, // Preserve status colors, white action text, and overlays.
        };
        *value = serde_json::Value::String(color_hex(replacement));
    }
    let nav = p.navigation_palette();
    for (key, value) in [
        ("sidebar.background", nav.sidebar), ("sidebar.foreground", nav.foreground),
        ("sidebar.border", nav.divider),
    ] {
        colors[key] = serde_json::Value::String(color_hex(value));
    }
}

fn color_rgb(color: Hsla) -> u32 {
    let rgba = Rgba::from(color);
    ((rgba.r * 255.).round() as u32) << 16
        | ((rgba.g * 255.).round() as u32) << 8
        | (rgba.b * 255.).round() as u32
}

fn color_hex(color: Hsla) -> String {
    format!("#{:06X}", color_rgb(color))
}

fn component_theme(theme: ResolvedTheme) -> ThemeConfig {
    let mode = if theme.is_dark() { "dark" } else { "light" };
    serde_json::from_value(serde_json::json!({
        "name": format!("Overlook {theme:?}"), "mode": mode,
        "font.family": "Inter", "font.size": 14, "radius": 6,
        "radius.lg": 10, "shadow": true, "colors": component_colors(theme),
    })).expect("theme json is valid")
}

const LIGHT_JSON: &str = r##"{
  "background": "#F4F3EE", "foreground": "#192B35", "border": "#DCE2DF", "input.border": "#C9D2CF", "ring": "#986020", "caret": "#192B35",
  "primary.background": "#365F73", "primary.foreground": "#FFFFFF", "primary.hover.background": "#2E5366", "primary.active.background": "#264757",
  "secondary.background": "#E8EDE9", "secondary.foreground": "#192B35", "secondary.hover.background": "#DFE6E1", "secondary.active.background": "#D4DDD7",
  "accent.background": "#E3EDEA", "accent.foreground": "#284D5D",
  "muted.background": "#E8EDE9", "muted.foreground": "#5E6D73",
  "popover.background": "#FFFEFB", "popover.foreground": "#192B35",
  "list.background": "#FFFEFB", "list.hover.background": "#E8EDE9", "list.active.background": "#E3EDEA", "list.active.border": "#365F73", "list.even.background": "#FFFEFB", "list.head.background": "#F0F1EC",
  "sidebar.background": "#F0F1EC", "sidebar.foreground": "#192B35", "sidebar.border": "#DCE2DF", "sidebar.accent.background": "#E3EDEA", "sidebar.accent.foreground": "#284D5D", "sidebar.primary.background": "#365F73", "sidebar.primary.foreground": "#FFFFFF",
  "button.background": "#FFFEFB", "button.foreground": "#192B35", "button.hover.background": "#E8EDE9", "button.active.background": "#DFE6E1",
  "button.primary.background": "#365F73", "button.primary.foreground": "#FFFFFF", "button.primary.hover.background": "#2E5366", "button.primary.active.background": "#264757",
  "button.secondary.background": "#E8EDE9", "button.secondary.foreground": "#192B35", "button.secondary.hover.background": "#DFE6E1", "button.secondary.active.background": "#D4DDD7",
  "button.danger.background": "#A13E37", "button.danger.foreground": "#FFFFFF", "button.danger.hover.background": "#8F342E", "button.danger.active.background": "#7B2C27",
  "danger.background": "#A13E37", "danger.foreground": "#FFFFFF", "danger.hover.background": "#8F342E", "danger.active.background": "#7B2C27",
  "success.background": "#386449", "success.foreground": "#FFFFFF", "warning.background": "#805015", "warning.foreground": "#FFFFFF", "info.background": "#365F73", "info.foreground": "#FFFFFF",
  "link": "#365F73", "link.hover": "#2E5366", "link.active": "#264757",
  "selection.background": "#C9DCD6",
  "scrollbar.background": "#00000000", "scrollbar.thumb.background": "#B9C4C0", "scrollbar.thumb.hover.background": "#9DABA7",
  "title_bar.background": "#FFFEFB", "title_bar.border": "#DCE2DF", "status_bar.background": "#F4F3EE", "status_bar.border": "#DCE2DF",
  "tab_bar.background": "#FFFEFB", "tab.background": "#FFFEFB", "tab.foreground": "#5E6D73", "tab.active.background": "#FFFEFB", "tab.active.foreground": "#365F73",
  "switch.background": "#365F73", "switch.thumb.background": "#FFFFFF", "skeleton.background": "#E8EDE9", "overlay": "#101C2466", "window.border": "#DCE2DF",
  "drag.border": "#365F73", "drop_target.background": "#E3EDEA", "progress.bar.background": "#365F73", "slider.background": "#DCE2DF", "slider.thumb.background": "#365F73",
  "accordion.background": "#FFFEFB", "group_box.background": "#FFFEFB", "group_box.foreground": "#192B35",
  "table.background": "#FFFEFB", "table.hover.background": "#E8EDE9", "table.active.background": "#E3EDEA", "table.active.border": "#365F73", "table.even.background": "#FFFEFB", "table.head.background": "#F0F1EC", "table.head.foreground": "#5E6D73", "table.row.border": "#DCE2DF"
}"##;

const DARK_JSON: &str = r##"{
  "background": "#101A21", "foreground": "#F0EBDD", "border": "#30424B", "input.border": "#3B4F59", "ring": "#FFBA6B", "caret": "#F0EBDD",
  "primary.background": "#A9CDD9", "primary.foreground": "#10232D", "primary.hover.background": "#B8D7E1", "primary.active.background": "#98BFCC",
  "secondary.background": "#23353F", "secondary.foreground": "#F0EBDD", "secondary.hover.background": "#2A3F4A", "secondary.active.background": "#1E2E37",
  "accent.background": "#28404C", "accent.foreground": "#DCECEF",
  "muted.background": "#23353F", "muted.foreground": "#A8B7BA",
  "popover.background": "#17242C", "popover.foreground": "#F0EBDD",
  "list.background": "#17242C", "list.hover.background": "#23353F", "list.active.background": "#28404C", "list.active.border": "#A9CDD9", "list.even.background": "#17242C", "list.head.background": "#121F27",
  "sidebar.background": "#121F27", "sidebar.foreground": "#F0EBDD", "sidebar.border": "#30424B", "sidebar.accent.background": "#28404C", "sidebar.accent.foreground": "#DCECEF", "sidebar.primary.background": "#A9CDD9", "sidebar.primary.foreground": "#10232D",
  "button.background": "#17242C", "button.foreground": "#F0EBDD", "button.hover.background": "#23353F", "button.active.background": "#2A3F4A",
  "button.primary.background": "#A9CDD9", "button.primary.foreground": "#10232D", "button.primary.hover.background": "#B8D7E1", "button.primary.active.background": "#98BFCC",
  "button.secondary.background": "#23353F", "button.secondary.foreground": "#F0EBDD", "button.secondary.hover.background": "#2A3F4A", "button.secondary.active.background": "#1E2E37",
  "button.danger.background": "#FFB0A6", "button.danger.foreground": "#10232D", "button.danger.hover.background": "#FFC0B8", "button.danger.active.background": "#F09E94",
  "danger.background": "#FFB0A6", "danger.foreground": "#10232D", "danger.hover.background": "#FFC0B8", "danger.active.background": "#F09E94",
  "success.background": "#A8CEB1", "success.foreground": "#10232D", "warning.background": "#F1BE78", "warning.foreground": "#10232D", "info.background": "#A9CDD9", "info.foreground": "#10232D",
  "link": "#A9CDD9", "link.hover": "#B8D7E1", "link.active": "#98BFCC",
  "selection.background": "#355566",
  "scrollbar.background": "#00000000", "scrollbar.thumb.background": "#3E535D", "scrollbar.thumb.hover.background": "#55707C",
  "title_bar.background": "#17242C", "title_bar.border": "#30424B", "status_bar.background": "#101A21", "status_bar.border": "#30424B",
  "tab_bar.background": "#17242C", "tab.background": "#17242C", "tab.foreground": "#A8B7BA", "tab.active.background": "#17242C", "tab.active.foreground": "#A9CDD9",
  "switch.background": "#A9CDD9", "switch.thumb.background": "#10232D", "skeleton.background": "#23353F", "overlay": "#00000099", "window.border": "#30424B",
  "drag.border": "#A9CDD9", "drop_target.background": "#28404C", "progress.bar.background": "#A9CDD9", "slider.background": "#30424B", "slider.thumb.background": "#A9CDD9",
  "accordion.background": "#17242C", "group_box.background": "#17242C", "group_box.foreground": "#F0EBDD",
  "table.background": "#17242C", "table.hover.background": "#23353F", "table.active.background": "#28404C", "table.active.border": "#A9CDD9", "table.even.background": "#17242C", "table.head.background": "#121F27", "table.head.foreground": "#A8B7BA", "table.row.border": "#30424B"
}"##;

/// Installs the selected colorway and applies its light/dark rendering mode.
pub fn apply(theme: ResolvedTheme, cx: &mut App) {
    let light = Rc::new(component_theme(if theme.is_dark() { ResolvedTheme::Light } else { theme }));
    let dark = Rc::new(component_theme(if theme.is_dark() { theme } else { ResolvedTheme::Dark }));
    Theme::update(cx, |t| {
        t.light_theme = light;
        t.dark_theme = dark;
    });
    let mode = if theme.is_dark() { ThemeMode::Dark } else { ThemeMode::Light };
    Theme::change(mode, None, cx);
    cx.set_global(Palette::for_theme(theme));
    cx.refresh_windows();
}

pub fn system_appearance(appearance: WindowAppearance) -> SystemAppearance {
    match appearance {
        WindowAppearance::Dark | WindowAppearance::VibrantDark => SystemAppearance::Dark,
        WindowAppearance::Light | WindowAppearance::VibrantLight => SystemAppearance::Light,
    }
}

/// Resolves and applies the stored preference against the current system appearance.
/// Follow system uses the desktop theme's colors when they can be read, and the desktop's
/// light/dark appearance otherwise. The colors are re-read on every call (at launch, on an
/// appearance change, and when Omarchy's theme-set hook runs `overlook --reload-theme`); a
/// changed desktop theme is applied, an unchanged one is not.
pub fn apply_preference(pref: AppearancePreference, system: SystemAppearance, cx: &mut App) -> ResolvedTheme {
    let desktop_changed = refresh_desktop_colors();
    let resolved = match (pref, desktop_theme()) {
        (AppearancePreference::FollowSystem, Some(desktop)) => desktop,
        _ => pref.resolve(system),
    };
    let current = cx.try_global::<Palette>().map(|p| p.resolved);
    let desktop_in_use = matches!(resolved, ResolvedTheme::SystemLight | ResolvedTheme::SystemDark);
    if current != Some(resolved) || (desktop_in_use && desktop_changed) {
        apply(resolved, cx);
    }
    resolved
}

// ------------------------------------------------------------- desktop theme

/// The desktop theme's palette and the file text it came from.
static DESKTOP: std::sync::RwLock<Option<(String, Palette)>> = std::sync::RwLock::new(None);

/// Bumped whenever the desktop colors change, for caches keyed on them (tinted icons).
static DESKTOP_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The current desktop colors' generation; see [`DESKTOP_GENERATION`].
pub fn desktop_generation() -> u64 {
    DESKTOP_GENERATION.load(std::sync::atomic::Ordering::Relaxed)
}

/// Omarchy's current theme: `~/.local/state/omarchy/current/theme` (older releases kept it
/// under `~/.config/omarchy/current/theme`).
fn omarchy_theme_dirs() -> Vec<std::path::PathBuf> {
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    let state = std::env::var_os("XDG_STATE_HOME").map(std::path::PathBuf::from).or_else(|| home.as_ref().map(|h| h.join(".local/state")));
    let config = std::env::var_os("XDG_CONFIG_HOME").map(std::path::PathBuf::from).or_else(|| home.as_ref().map(|h| h.join(".config")));
    [state, config].into_iter().flatten().map(|base| base.join("omarchy/current/theme")).collect()
}

/// Marks the hook as Overlook's, so it is only ever rewritten by Overlook.
const HOOK_MARKER: &str = "# Installed by Overlook";

/// Installs `~/.config/omarchy/hooks/theme-set.d/overlook`, which Omarchy runs after every
/// theme switch, so a running Overlook following the system picks the new colors up at once.
/// Only on Omarchy (the hooks folder exists). The hook prefers `overlook` on PATH and falls
/// back to this executable; an existing hook is rewritten only when it is Overlook's and its
/// fallback no longer exists, so a development build and the installed one never take turns.
pub fn install_omarchy_hook() {
    let Some(config) = std::env::var_os("XDG_CONFIG_HOME").map(std::path::PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config"))) else { return };
    let Ok(exe) = std::env::current_exe() else { return };
    install_hook_in(&config, &exe);
}

fn install_hook_in(config: &std::path::Path, exe: &std::path::Path) {
    let hooks = config.join("omarchy/hooks");
    if !hooks.is_dir() {
        return;
    }
    let path = hooks.join("theme-set.d/overlook");
    if let Ok(existing) = std::fs::read_to_string(&path) {
        let fallback_alive = existing.lines().find_map(|l| l.strip_prefix("fallback=")).is_some_and(|f| std::path::Path::new(f.trim_matches('\'')).is_file());
        if !existing.starts_with(&format!("#!/bin/bash\n{HOOK_MARKER}")) || fallback_alive {
            return;
        }
    }
    let script = format!(
        "#!/bin/bash\n{HOOK_MARKER}: a running Overlook re-reads the Omarchy theme colors.\n\
fallback='{}'\n\
if command -v overlook >/dev/null; then\n  overlook --reload-theme\nelif [[ -x $fallback ]]; then\n  \"$fallback\" --reload-theme\nfi\nexit 0\n",
        exe.display()
    );
    let written = std::fs::create_dir_all(path.parent().expect("hook folder")).and_then(|_| std::fs::write(&path, script));
    match written {
        Ok(()) => tracing::info!("installed the Omarchy theme hook at {}", path.display()),
        Err(e) => tracing::warn!("could not install the Omarchy theme hook: {e}"),
    }
}

/// Re-reads the desktop colors; true when they changed since the last read.
fn refresh_desktop_colors() -> bool {
    let source = omarchy_theme_dirs().into_iter().find_map(|dir| std::fs::read_to_string(dir.join("colors.toml")).ok());
    let Ok(mut slot) = DESKTOP.write() else { return false };
    if slot.as_ref().map(|(text, _)| text) == source.as_ref() {
        return false;
    }
    *slot = source.and_then(|text| desktop_palette(&text).map(|p| (text, p)));
    DESKTOP_GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    true
}

/// The desktop theme to use for Follow system, when its colors were readable.
pub fn desktop_theme() -> Option<ResolvedTheme> {
    DESKTOP.read().ok().and_then(|d| d.as_ref().map(|(_, p)| p.resolved))
}

/// The desktop theme's name for menus ("Tokyo Night"), when Omarchy names it.
pub fn desktop_theme_name() -> Option<String> {
    desktop_theme()?;
    let name = omarchy_theme_dirs().into_iter().find_map(|dir| std::fs::read_to_string(dir.with_file_name("theme.name")).ok())?;
    let words: Vec<String> = name.trim().split(['-', '_', ' ']).filter(|w| !w.is_empty()).map(overlook_core::text::capitalize).collect();
    (!words.is_empty()).then(|| words.join(" "))
}

fn parse_hex(value: &str) -> Option<Hsla> {
    let hex = value.trim().trim_start_matches('#');
    (hex.len() == 6).then(|| u32::from_str_radix(hex, 16).ok()).flatten().map(c)
}

fn relative_luminance(color: Hsla) -> f32 {
    let rgba = Rgba::from(color);
    let channel = |v: f32| if v <= 0.03928 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) };
    0.2126 * channel(rgba.r) + 0.7152 * channel(rgba.g) + 0.0722 * channel(rgba.b)
}

fn contrast(a: Hsla, b: Hsla) -> f32 {
    let (a, b) = (relative_luminance(a), relative_luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

fn mix(a: Hsla, b: Hsla, t: f32) -> Hsla {
    let (a, b) = (Rgba::from(a), Rgba::from(b));
    Rgba { r: a.r + (b.r - a.r) * t, g: a.g + (b.g - a.g) * t, b: a.b + (b.b - a.b) * t, a: 1. }.into()
}

/// `color`, moved toward `toward` just far enough to reach `ratio` against `background`.
fn readable(color: Hsla, background: Hsla, toward: Hsla, ratio: f32) -> Hsla {
    (0..=20).map(|step| mix(color, toward, step as f32 / 20.)).find(|c| contrast(*c, background) >= ratio).unwrap_or(toward)
}

/// A palette from Omarchy's `colors.toml` (`key = "#rrggbb"` lines). Needs at least the
/// background, foreground and accent; other keys fall back to blends of those.
fn desktop_palette(source: &str) -> Option<Palette> {
    let mut values = std::collections::HashMap::new();
    for line in source.lines() {
        let Some((key, value)) = line.split_once('=') else { continue };
        values.insert(key.trim().to_owned(), value.trim().trim_matches('"').to_owned());
    }
    let get = |key: &str| values.get(key).and_then(|v| parse_hex(v));
    let surface = get("background")?;
    let foreground = get("foreground")?;
    let accent = get("accent").or_else(|| get("blue"))?;
    let dark = match values.get("mode").map(String::as_str) {
        Some("dark") => true,
        Some("light") => false,
        _ => relative_luminance(surface) < relative_luminance(foreground),
    };
    let base = if dark { Palette::dark() } else { Palette::light() };
    let (white, black) = (c(0xFFFFFF), c(0x000000));
    let ink = if dark { white } else { black };
    let frame = get("dark_background").unwrap_or_else(|| shade(surface, 0.12));
    let raised = get("lighter_background").unwrap_or_else(|| mix(surface, foreground, 0.06));
    let selection = get("selection").filter(|s| contrast(*s, surface) > 1.1).unwrap_or_else(|| mix(surface, accent, 0.25));
    let selection_text = readable(get("bright_foreground").unwrap_or(foreground), selection, ink, 4.5);
    let on_action = if contrast(accent, surface) >= contrast(accent, white) { surface } else { white };
    let tone = |key: &str, fallback: Hsla| readable(get(key).unwrap_or(fallback), surface, ink, 4.5);
    let foreground = readable(foreground, surface, ink, 4.5);
    let muted = readable(get("dark_foreground").unwrap_or_else(|| mix(foreground, surface, 0.4)), surface, foreground, 4.5);
    let accent_text = readable(accent, surface, ink, 4.5);
    Some(Palette {
        resolved: if dark { ResolvedTheme::SystemDark } else { ResolvedTheme::SystemLight },
        background: frame,
        surface,
        sidebar: frame,
        foreground,
        muted,
        divider: get("muted").unwrap_or_else(|| mix(surface, foreground, 0.18)),
        control_border: readable(get("dark_foreground").unwrap_or_else(|| mix(surface, foreground, 0.4)), surface, foreground, 3.0),
        hover: raised,
        accent: accent_text,
        action: accent,
        on_action,
        selection,
        selection_text,
        focus: accent,
        success: tone("green", base.success),
        warning: tone("yellow", base.warning),
        error: tone("red", base.error),
        brand_accent: get("orange").or_else(|| get("yellow")).unwrap_or(base.brand_accent),
        avatar: selection,
        avatar_ink: selection_text,
        chip: raised,
        label_slate: accent_text,
        label_plum: tone("magenta", base.label_plum),
        label_amber: tone("yellow", base.label_amber),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn luminance(color: Hsla) -> f32 {
        let c = Rgba::from(color);
        let linear = |v: f32| if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) };
        0.2126 * linear(c.r) + 0.7152 * linear(c.g) + 0.0722 * linear(c.b)
    }

    #[test]
    fn all_themes_have_readable_text_and_matching_component_tokens() {
        for pref in AppearancePreference::ALL {
            let theme = pref.resolve(SystemAppearance::Light);
            let p = Palette::for_theme(theme);
            assert_eq!(p.resolved, theme);
            for (fg, bg) in [
                (p.foreground, p.background), (p.foreground, p.surface),
                (p.foreground, p.sidebar), (p.muted, p.surface),
                (p.foreground, p.message_surface()), (p.muted, p.message_surface()),
                (p.foreground, p.list_surface()), (p.muted, p.list_surface()),
                (p.foreground, p.list_unread()), (p.accent, p.list_unread()),
                (p.muted, p.sidebar), (p.selection_text, p.selection),
                (p.on_action, p.action), (p.avatar_ink, p.avatar),
            ] {
                let (a, b) = (luminance(fg), luminance(bg));
                let ratio = (a.max(b) + 0.05) / (a.min(b) + 0.05);
                assert!(ratio >= 4.5, "{theme:?}: contrast {ratio}");
            }
            let colors = component_colors(theme);
            for (key, color) in [
                ("background", p.background), ("foreground", p.foreground),
                ("popover.background", p.surface), ("sidebar.background", p.navigation_palette().sidebar),
                ("primary.background", p.action), ("accent.background", p.selection),
                ("accent.foreground", p.selection_text), ("ring", p.focus),
            ] {
                assert_eq!(colors[key].as_str(), Some(color_hex(color).as_str()), "{theme:?}: {key}");
            }
            let _ = component_theme(theme);
        }
    }

    const TOKYO_NIGHT: &str = r##"mode = "dark"
accent = "#7aa2f7"
selection = "#292e42"
muted = "#414868"
background = "#1a1b26"
dark_background = "#13141c"
lighter_background = "#24283b"
foreground = "#a9b1d6"
dark_foreground = "#565f89"
bright_foreground = "#c0caf5"
red = "#f7768e"
yellow = "#e0af68"
green = "#9ece6a"
magenta = "#ad8ee6"
"##;

    #[test]
    fn desktop_colors_become_a_readable_palette() {
        let p = desktop_palette(TOKYO_NIGHT).expect("palette");
        assert_eq!(p.resolved, ResolvedTheme::SystemDark);
        assert_eq!(color_hex(p.surface), "#1A1B26");
        assert_eq!(color_hex(p.action), "#7AA2F7");
        for (fg, bg) in [
            (p.foreground, p.surface), (p.muted, p.surface), (p.accent, p.surface),
            (p.selection_text, p.selection), (p.error, p.surface), (p.success, p.surface),
        ] {
            assert!(contrast(fg, bg) >= 4.5, "contrast {}", contrast(fg, bg));
        }
        // A light theme with only the required keys still works, by luminance.
        let light = desktop_palette("background = \"#fafafa\"\nforeground = \"#333333\"\naccent = \"#0055aa\"").expect("light palette");
        assert_eq!(light.resolved, ResolvedTheme::SystemLight);
        assert!(desktop_palette("background = \"#000000\"").is_none(), "foreground and accent are required");
    }

    #[test]
    fn desktop_dark_theme_rewrites_every_built_in_dark_color() {
        let p = desktop_palette(TOKYO_NIGHT).unwrap();
        *DESKTOP.write().unwrap() = Some((TOKYO_NIGHT.to_owned(), p));
        let colors = component_colors(ResolvedTheme::SystemDark);
        assert_eq!(colors["background"].as_str(), Some("#13141C"));
        assert_eq!(colors["popover.background"].as_str(), Some("#1A1B26"));
        assert_eq!(colors["switch.background"].as_str(), Some(color_hex(p.muted).as_str()), "an off switch is grey");
        let dark: serde_json::Value = serde_json::from_str(DARK_JSON).unwrap();
        let built_in: std::collections::HashSet<&str> = dark.as_object().unwrap().values().filter_map(|v| v.as_str()).filter(|v| v.len() == 7).collect();
        for (key, value) in colors.as_object().unwrap() {
            let value = value.as_str().unwrap();
            assert!(!built_in.contains(value) || dark[key].as_str() != Some(value), "{key} kept the built-in {value}");
        }
        let _ = component_theme(ResolvedTheme::SystemDark);
    }

    #[test]
    fn built_in_dark_turns_switches_grey_when_off() {
        assert_eq!(component_colors(ResolvedTheme::Dark)["switch.background"].as_str(), Some(color_hex(Palette::dark().muted).as_str()));
    }

    #[test]
    fn omarchy_hook_is_installed_once_and_runs_the_fallback() {
        let dir = std::env::temp_dir().join(format!("overlook-hook-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        install_hook_in(&dir, std::path::Path::new("/bin/true"));
        assert!(!dir.join("omarchy").exists(), "nothing is written without Omarchy");
        std::fs::create_dir_all(dir.join("omarchy/hooks")).unwrap();
        // A fallback that echoes its argument stands in for the binary.
        let fake = dir.join("fake-overlook");
        std::fs::write(&fake, "#!/bin/bash\necho \"called $1\"\n").unwrap();
        std::fs::set_permissions(&fake, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        install_hook_in(&dir, &fake);
        let hook = dir.join("omarchy/hooks/theme-set.d/overlook");
        let out = std::process::Command::new("bash").arg(&hook).env("PATH", "/usr/bin:/bin").output().unwrap();
        assert!(out.status.success());
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "called --reload-theme");
        // A second build does not take the hook over while the first one's binary exists.
        install_hook_in(&dir, std::path::Path::new("/somewhere/else/overlook"));
        assert!(std::fs::read_to_string(&hook).unwrap().contains(&fake.display().to_string()));
        // A user's own hook of that name is never replaced.
        std::fs::write(&hook, "#!/bin/bash\necho mine\n").unwrap();
        install_hook_in(&dir, &fake);
        assert_eq!(std::fs::read_to_string(&hook).unwrap(), "#!/bin/bash\necho mine\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn email_paper_is_independent_of_shell_appearance() {
        for source in ["<p>Fictional message</p>", "<body bgcolor='#203040' text='#fafafa'><p>Authored colors</p></body>"] {
            let expected = overlook_core::browser::prepare_document_with_colors(source, EMAIL_PAPER, EMAIL_INK);
            for pref in AppearancePreference::ALL {
                for system in [SystemAppearance::Light, SystemAppearance::Dark] {
                    let palette = Palette::for_theme(pref.resolve(system));
                    assert_eq!(palette.prepare_browser_document(source), expected, "{pref:?}, {system:?}");
                }
            }
        }
    }

    #[test]
    fn message_center_is_white_except_in_dark_mode() {
        for pref in AppearancePreference::ALL {
            for system in [SystemAppearance::Light, SystemAppearance::Dark, SystemAppearance::Unknown] {
                let p = Palette::for_theme(pref.resolve(system));
                let expected = if p.is_dark() { p.surface } else { c(0xFFFFFF) };
                assert_eq!(p.message_surface(), expected, "{pref:?}, {system:?}");
            }
        }
    }

    #[test]
    fn alexander_has_soft_blue_navigation_washed_out_selection_and_amber_edge() {
        let p = Palette::for_theme(ResolvedTheme::Alexander);
        assert_eq!(p.surface, c(0xFFFFFF));
        assert_eq!(p.action, c(0x1F57A5));
        assert_eq!(p.chip, p.hover);
        assert_eq!(p.selection, c(0xE0EAF0));
        assert_eq!(p.avatar, c(0xDDE0E4));
        assert_eq!(p.brand_accent, c(0xFF9F0A));
        assert_eq!(p.message_accent(), c(0xFF9F0A));
        assert_eq!(p.label_amber, c(0xFF9F0A));
        assert_eq!(p.focus, p.action, "Keep keyboard focus legible on white surfaces");
        let nav = p.navigation_palette();
        assert_eq!(nav.sidebar, c(0x1F343F));
        for (fg, bg) in [
            (nav.foreground, nav.sidebar), (nav.muted, nav.sidebar),
            (nav.foreground, nav.hover), (nav.accent, nav.selection),
            (nav.muted, nav.hover), (nav.success, nav.sidebar),
            (nav.warning, nav.sidebar), (nav.error, nav.sidebar),
        ] {
            let (a, b) = (luminance(fg), luminance(bg));
            let ratio = (a.max(b) + 0.05) / (a.min(b) + 0.05);
            assert!(ratio >= 4.5, "Alexander navigation: {} on {} has contrast {ratio}", color_hex(fg), color_hex(bg));
        }
        for (fg, bg) in [(p.foreground, p.chip), (p.foreground, p.brand_accent), (p.focus, p.surface)] {
            let (a, b) = (luminance(fg), luminance(bg));
            assert!((a.max(b) + 0.05) / (a.min(b) + 0.05) >= 3.0);
        }
    }

    #[test]
    fn colorways_are_distinct() {
        let themes = [ResolvedTheme::Gray, ResolvedTheme::Blue, ResolvedTheme::White, ResolvedTheme::Alexander, ResolvedTheme::Pink];
        for (i, theme) in themes.iter().enumerate() {
            for other in &themes[i + 1..] {
                assert_ne!(component_colors(*theme), component_colors(*other));
            }
        }
    }
}
