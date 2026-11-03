//! Appearance preference and theme resolution.
//!
//! Explicit themes are independent of the desktop appearance. "Follow system"
//! selects Light or Dark, with Light as the fallback; the app upgrades that to
//! [`ResolvedTheme::SystemLight`] / [`ResolvedTheme::SystemDark`] when it can read the
//! desktop theme's own colors (Omarchy's current theme).

use serde::{Deserialize, Serialize};

/// The user's stored appearance preference.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum AppearancePreference {
    Light,
    Dark,
    #[serde(alias = "gray-white")]
    Gray,
    Blue,
    White,
    Alexander,
    Pink,
    #[default]
    FollowSystem,
}

impl AppearancePreference {
    pub const ALL: [AppearancePreference; 8] = [
        AppearancePreference::Light,
        AppearancePreference::Dark,
        AppearancePreference::Gray,
        AppearancePreference::Blue,
        AppearancePreference::White,
        AppearancePreference::Alexander,
        AppearancePreference::Pink,
        AppearancePreference::FollowSystem,
    ];

    /// Label used in Settings and menus.
    pub fn label(self) -> &'static str {
        match self {
            AppearancePreference::Light => "Light",
            AppearancePreference::Dark => "Dark",
            AppearancePreference::Gray => "Gray",
            AppearancePreference::Blue => "Blue",
            AppearancePreference::White => "White",
            AppearancePreference::Alexander => "Alexander",
            AppearancePreference::Pink => "Pink",
            AppearancePreference::FollowSystem => "Follow system",
        }
    }

    pub fn as_key(self) -> &'static str {
        match self {
            AppearancePreference::Light => "light",
            AppearancePreference::Dark => "dark",
            AppearancePreference::Gray => "gray",
            AppearancePreference::Blue => "blue",
            AppearancePreference::White => "white",
            AppearancePreference::Alexander => "alexander",
            AppearancePreference::Pink => "pink",
            AppearancePreference::FollowSystem => "follow-system",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "light" => Some(AppearancePreference::Light),
            "dark" => Some(AppearancePreference::Dark),
            "gray" | "gray-white" => Some(AppearancePreference::Gray),
            "blue" => Some(AppearancePreference::Blue),
            "white" => Some(AppearancePreference::White),
            "alexander" => Some(AppearancePreference::Alexander),
            "pink" => Some(AppearancePreference::Pink),
            "follow-system" | "system" => Some(AppearancePreference::FollowSystem),
            _ => None,
        }
    }

    /// Resolves the preference, consulting the desktop only for Follow system.
    pub fn resolve(self, system: SystemAppearance) -> ResolvedTheme {
        match self {
            AppearancePreference::Light => ResolvedTheme::Light,
            AppearancePreference::Dark => ResolvedTheme::Dark,
            AppearancePreference::Gray => ResolvedTheme::Gray,
            AppearancePreference::Blue => ResolvedTheme::Blue,
            AppearancePreference::White => ResolvedTheme::White,
            AppearancePreference::Alexander => ResolvedTheme::Alexander,
            AppearancePreference::Pink => ResolvedTheme::Pink,
            AppearancePreference::FollowSystem => match system {
                SystemAppearance::Dark => ResolvedTheme::Dark,
                // Light is the documented fallback when the desktop has no preference or
                // the preference cannot be read.
                SystemAppearance::Light | SystemAppearance::Unknown => ResolvedTheme::Light,
            },
        }
    }
}

/// What the desktop reports through the settings portal, if anything.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SystemAppearance {
    Light,
    Dark,
    Unknown,
}

/// A built-in theme.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResolvedTheme {
    Light,
    Dark,
    #[serde(alias = "GrayWhite")]
    Gray,
    Blue,
    White,
    Alexander,
    Pink,
    /// The desktop theme's colors, light or dark by that theme's own mode.
    SystemLight,
    SystemDark,
}

impl ResolvedTheme {
    pub fn is_dark(self) -> bool {
        matches!(self, ResolvedTheme::Dark | ResolvedTheme::SystemDark)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn follow_system_selects_light_or_dark() {
        assert_eq!(
            AppearancePreference::FollowSystem.resolve(SystemAppearance::Dark),
            ResolvedTheme::Dark
        );
        assert_eq!(
            AppearancePreference::FollowSystem.resolve(SystemAppearance::Light),
            ResolvedTheme::Light
        );
        assert_eq!(
            AppearancePreference::FollowSystem.resolve(SystemAppearance::Unknown),
            ResolvedTheme::Light,
            "Light is the fallback"
        );
        assert_eq!(
            AppearancePreference::Dark.resolve(SystemAppearance::Light),
            ResolvedTheme::Dark
        );
    }

    #[test]
    fn explicit_themes_ignore_system() {
        for (pref, theme, dark) in [
            (AppearancePreference::Light, ResolvedTheme::Light, false),
            (AppearancePreference::Dark, ResolvedTheme::Dark, true),
            (AppearancePreference::Gray, ResolvedTheme::Gray, false),
            (AppearancePreference::Blue, ResolvedTheme::Blue, false),
            (AppearancePreference::White, ResolvedTheme::White, false),
            (AppearancePreference::Alexander, ResolvedTheme::Alexander, false),
            (AppearancePreference::Pink, ResolvedTheme::Pink, false),
        ] {
            for system in [SystemAppearance::Light, SystemAppearance::Dark, SystemAppearance::Unknown] {
                assert_eq!(pref.resolve(system), theme);
            }
            assert_eq!(theme.is_dark(), dark);
            let json = serde_json::to_string(&theme).unwrap();
            assert_eq!(serde_json::from_str::<ResolvedTheme>(&json).unwrap(), theme);
        }
    }

    #[test]
    fn preferences_serialize_as_keys() {
        for pref in AppearancePreference::ALL {
            let json = serde_json::to_string(&pref).unwrap();
            assert_eq!(json, format!("\"{}\"", pref.as_key()));
            assert_eq!(serde_json::from_str::<AppearancePreference>(&json).unwrap(), pref);
        }
        assert_eq!(AppearancePreference::Alexander.label(), "Alexander");
        assert_eq!(AppearancePreference::Alexander.as_key(), "alexander");
        assert_eq!(AppearancePreference::default(), AppearancePreference::FollowSystem);
    }

    #[test]
    fn retired_gray_white_settings_migrate_to_gray() {
        assert_eq!(AppearancePreference::from_key("gray-white"), Some(AppearancePreference::Gray));
        assert_eq!(serde_json::from_str::<AppearancePreference>("\"gray-white\"").unwrap(), AppearancePreference::Gray);
        assert_eq!(serde_json::from_str::<ResolvedTheme>("\"GrayWhite\"").unwrap(), ResolvedTheme::Gray);
        assert!(AppearancePreference::ALL.iter().all(|pref| pref.label() != "Gray/White"));
    }

    #[test]
    fn keys_round_trip() {
        for pref in AppearancePreference::ALL {
            assert_eq!(AppearancePreference::from_key(pref.as_key()), Some(pref));
        }
    }
}
