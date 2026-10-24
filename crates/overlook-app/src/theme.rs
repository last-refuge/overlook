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
            // Background, surface, sidebar, text, muted, divider, control border,
            // hover, action, selection, selected text, and focus ring.
            ResolvedTheme::Gray => [0xE5E7EB, 0xF3F4F6, 0xD1D5DB, 0x1F2937, 0x4B5563, 0xB8BEC7, 0x6B7280, 0xDDE1E7, 0x374151, 0xCDD5DF, 0x1F2937, 0x805015],
            ResolvedTheme::Blue => [0xEDF4FC, 0xF8FBFF, 0xDCE9F8, 0x172D4D, 0x4E6480, 0xC7D6E8, 0x657F9F, 0xE2EDFA, 0x245A96, 0xD3E4F8, 0x193F70, 0x805015],
            ResolvedTheme::White => [0xFFFFFF, 0xFFFFFF, 0xFAFAFA, 0x20242B, 0x59616D, 0xE0E3E7, 0x767D87, 0xF0F2F5, 0x374151, 0xE5E9EF, 0x20242B, 0x805015],
            ResolvedTheme::Alexander => [0xDFE3E8, 0xFFFFFF, 0xE3E6EB, 0x182B46, 0x48596C, 0xB8C2CF, 0x63748A, 0xECEEF1, 0x1F57A5, 0xDDE0E4, 0x17314D, 0x1F57A5],
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
            palette.chip = c(0xECEEF1);
            palette.brand_accent = c(0xFF9F0A);
            palette.label_amber = c(0xFF9F0A);
        }
        palette
    }

    /// Navigation has its own contrast context, independent of white mail surfaces.
    /// Keep generic segmented controls light while the left navigation is navy.
    pub fn navigation_palette(&self) -> Self {
        let mut p = *self;
        if self.resolved == ResolvedTheme::Alexander {
            p.sidebar = c(0x142433);
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

    /// Light shares its stone surface with reading and composition. Other light
    /// colorways retain their white message center.
    pub fn message_surface(&self) -> Hsla {
        if matches!(self.resolved, ResolvedTheme::Light | ResolvedTheme::Dark) { self.surface } else { c(0xFFFFFF) }
    }

    /// Theme defaults only; sender-authored HTML colors still take precedence.
    pub fn prepare_browser_document(&self, source: &str) -> overlook_core::BrowserDocument {
        if self.resolved == ResolvedTheme::Light {
            overlook_core::browser::prepare_document_with_colors(source, color_rgb(self.message_surface()), color_rgb(self.foreground))
        } else {
            overlook_core::prepare_document(source)
        }
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
    if theme == ResolvedTheme::Dark {
        return colors;
    }
    let p = Palette::for_theme(theme);
    let shade = |color: Hsla, alpha| -> Hsla {
        Rgba::from(color).blend(gpui_kit::rgb(0).alpha(alpha)).into()
    };
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
    if theme == ResolvedTheme::Light {
        colors["input.border"] = serde_json::Value::String(color_hex(p.control_border));
    }
    colors
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
    let dark = Rc::new(component_theme(ResolvedTheme::Dark));
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
pub fn apply_preference(pref: AppearancePreference, system: SystemAppearance, cx: &mut App) -> ResolvedTheme {
    let resolved = pref.resolve(system);
    let current = cx.try_global::<Palette>().map(|p| p.resolved);
    if current != Some(resolved) {
        apply(resolved, cx);
    }
    resolved
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

    #[test]
    fn message_center_uses_stone_for_light_and_preserves_other_colorways() {
        for pref in AppearancePreference::ALL {
            for system in [SystemAppearance::Light, SystemAppearance::Dark, SystemAppearance::Unknown] {
                let p = Palette::for_theme(pref.resolve(system));
                let expected = match p.resolved {
                    ResolvedTheme::Light => c(0xE1E2DC),
                    ResolvedTheme::Dark => p.surface,
                    _ => c(0xFFFFFF),
                };
                assert_eq!(p.message_surface(), expected, "{pref:?}, {system:?}");
            }
        }
    }

    #[test]
    fn alexander_has_navy_navigation_gray_highlights_and_amber_edge() {
        let p = Palette::for_theme(ResolvedTheme::Alexander);
        assert_eq!(p.surface, c(0xFFFFFF));
        assert_eq!(p.action, c(0x1F57A5));
        assert_eq!(p.chip, c(0xECEEF1));
        assert_eq!(p.selection, c(0xDDE0E4));
        assert_eq!(p.brand_accent, c(0xFF9F0A));
        assert_eq!(p.message_accent(), c(0xFF9F0A));
        assert_eq!(p.label_amber, c(0xFF9F0A));
        assert_eq!(p.focus, p.action, "Keep keyboard focus legible on white surfaces");
        let nav = p.navigation_palette();
        assert_eq!(nav.sidebar, c(0x142433));
        for (fg, bg) in [(nav.foreground, nav.sidebar), (nav.muted, nav.sidebar), (nav.foreground, nav.hover), (nav.accent, nav.selection)] {
            let (a, b) = (luminance(fg), luminance(bg));
            assert!((a.max(b) + 0.05) / (a.min(b) + 0.05) >= 4.5);
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
