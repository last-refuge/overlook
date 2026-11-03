//! Embedded assets: brand artwork and fonts from `branding/`, plus the Lucide icon set
//! bundled with GPUI Kit. Paths under `overlook/` are ours; everything else is delegated.

use crate::theme::Palette;
use gpui_kit::{AssetSource, Hsla, Img, Result, Rgba, SharedString, Styled, img, px};
use overlook_core::ResolvedTheme;
use std::borrow::Cow;

/// Static Inter faces (instanced from the bundled variable font) so weights resolve.
pub const INTER_FACES: [&[u8]; 4] = [
    include_bytes!("../../../branding/fonts/static/Inter-Regular.ttf"),
    include_bytes!("../../../branding/fonts/static/Inter-Medium.ttf"),
    include_bytes!("../../../branding/fonts/static/Inter-SemiBold.ttf"),
    include_bytes!("../../../branding/fonts/static/Inter-Bold.ttf"),
];
pub const SOURCE_SERIF_DISPLAY: &[u8] =
    include_bytes!("../../../branding/fonts/SourceSerif4Display-Regular.otf");

const EMBEDDED: &[(&str, &[u8])] = &[
    (
        "overlook/app-icon.svg",
        include_bytes!("../../../branding/assets/overlook-app-icon.svg"),
    ),
    (
        "overlook/icon-128.png",
        include_bytes!("../../../branding/assets/overlook-icon-128.png"),
    ),
    (
        "overlook/icon-64.png",
        include_bytes!("../../../branding/assets/overlook-icon-64.png"),
    ),
    (
        "overlook/symbolic.svg",
        include_bytes!("../../../branding/assets/overlook-symbolic.svg"),
    ),
    (
        "overlook/wordmark.svg",
        include_bytes!("../../../branding/assets/overlook-wordmark-mono.svg"),
    ),
    (
        "overlook/mark-ink.svg",
        include_bytes!("../../../branding/assets/overlook-mark-ink.svg"),
    ),
    (
        "overlook/alpine-dawn.png",
        include_bytes!("../../../branding/assets/overlook-alpine-dawn.png"),
    ),
];

// Original full-color SVGs. Use images, not the monochrome icon mask renderer.
macro_rules! color_icons {
    ($($variant:ident => $file:literal),+ $(,)?) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum ColorIcon { $($variant),+ }
        impl ColorIcon {
            pub const ALL: &[Self] = &[$(Self::$variant),+];
            fn file(self) -> &'static str {
                match self { $(Self::$variant => concat!($file, ".svg")),+ }
            }
        }
        const COLOR_ASSETS: &[(&str, &[u8])] = &[
            $((concat!($file, ".svg"), include_bytes!(concat!("../../../branding/icons/", $file, ".svg")))),+
        ];
    }
}
color_icons! {
    Mail => "mail",
    Calendar => "calendar",
    Contacts => "contacts",
    Compose => "compose",
    Archive => "archive",
    Delete => "delete",
    Move => "move",
    Read => "read",
    Star => "star",
    Reply => "reply",
    ReplyAll => "reply-all",
    Forward => "forward",
    Sync => "sync",
    Edit => "edit",
    ContactAdd => "contact-add",
    CalendarAdd => "calendar-add",
    Day => "day",
    Week => "week",
    Month => "month",
    Agenda => "agenda",
    Appearance => "appearance",
    Layout => "layout",
    Settings => "settings",
}

const ICON_THEMES: &[(&str, ResolvedTheme)] = &[
    ("light", ResolvedTheme::Light),
    ("dark", ResolvedTheme::Dark),
    ("gray", ResolvedTheme::Gray),
    ("blue", ResolvedTheme::Blue),
    ("white", ResolvedTheme::White),
    ("alexander", ResolvedTheme::Alexander),
    ("pink", ResolvedTheme::Pink),
    ("system-light", ResolvedTheme::SystemLight),
    ("system-dark", ResolvedTheme::SystemDark),
];

fn color_icon_path(icon: ColorIcon, theme: ResolvedTheme, on_action: bool) -> SharedString {
    let key = ICON_THEMES
        .iter()
        .find(|(_, value)| *value == theme)
        .map_or("light", |(name, _)| name);
    let tone = if on_action { "action" } else { "normal" };
    match theme {
        // The desktop colors change at runtime; their generation in the path keeps the
        // image cache from serving icons tinted for the previous desktop theme.
        ResolvedTheme::SystemLight | ResolvedTheme::SystemDark => format!("overlook/color/{key}.{}/{tone}/{}", crate::theme::desktop_generation(), icon.file()).into(),
        _ => format!("overlook/color/{key}/{tone}/{}", icon.file()).into(),
    }
}

pub fn color_icon(icon: ColorIcon, size: f32, palette: &Palette) -> Img {
    img(color_icon_path(icon, palette.resolved, false))
        .size(px(size))
        .flex_none()
}

pub fn color_icon_on_action(icon: ColorIcon, size: f32, palette: &Palette) -> Img {
    img(color_icon_path(icon, palette.resolved, true))
        .size(px(size))
        .flex_none()
}

/// Resolve immutable, theme-specific resources when the image loader requests them.
/// The theme and contrast context are part of the cache key, never global mutable state.
fn themed_icon(path: &str) -> Option<Vec<u8>> {
    let rest = path.strip_prefix("overlook/color/")?;
    let mut parts = rest.split('/');
    let key = parts.next()?;
    let tone = parts.next()?;
    let file = parts.next()?;
    if parts.next().is_some() || !matches!(tone, "normal" | "action") {
        return None;
    }
    // A desktop theme's key carries a colors fingerprint after the dot; the colors
    // themselves are read fresh from the current desktop palette.
    let key = key.split_once('.').map_or(key, |(name, _)| name);
    let (_, theme) = ICON_THEMES.iter().find(|(name, _)| *name == key)?;
    let (_, source) = COLOR_ASSETS.iter().find(|(name, _)| *name == file)?;
    let p = Palette::for_theme(*theme);
    let mix = |a: Hsla, b: Hsla, amount: f32| -> Hsla {
        let a = Rgba::from(a);
        let b = Rgba::from(b);
        Rgba {
            r: a.r + (b.r - a.r) * amount,
            g: a.g + (b.g - a.g) * amount,
            b: a.b + (b.b - a.b) * amount,
            a: 1.,
        }
        .into()
    };
    let secondary = match theme {
        ResolvedTheme::Light | ResolvedTheme::Alexander => p.brand_accent,
        ResolvedTheme::Dark => p.label_plum,
        _ => p.selection_text,
    };
    let colors = if tone == "action" {
        [
            p.on_action,
            mix(p.on_action, p.selection, 0.2),
            p.selection,
            mix(p.action, p.on_action, 0.16),
            p.action,
            p.on_action,
        ]
    } else {
        [
            p.action,
            mix(p.action, p.on_action, 0.35),
            secondary,
            p.selection,
            p.message_surface(),
            p.foreground,
        ]
    };
    let tokens = [
        "#3F6654", "#76A58C", "#92581D", "#DDE9E1", "#FFFFFF", "#29332E",
    ];
    // A single replacement pass prevents a generated hex matching a later source token.
    let source = std::str::from_utf8(source).ok()?;
    let mut result = String::with_capacity(source.len());
    for (i, fragment) in source.split('#').enumerate() {
        if i == 0 {
            result.push_str(fragment);
            continue;
        }
        result.push('#');
        if let Some(index) = tokens
            .iter()
            .position(|token| fragment.starts_with(&token[1..]))
        {
            let rgb = Rgba::from(colors[index]);
            result.push_str(&format!(
                "{:02X}{:02X}{:02X}",
                (rgb.r * 255.).round() as u8,
                (rgb.g * 255.).round() as u8,
                (rgb.b * 255.).round() as u8
            ));
            result.push_str(&fragment[6..]);
        } else {
            result.push_str(fragment);
        }
    }
    Some(result.into_bytes())
}

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some((_, bytes)) = EMBEDDED.iter().find(|(p, _)| *p == path) {
            return Ok(Some(Cow::Borrowed(bytes)));
        }
        if let Some(bytes) = themed_icon(path) {
            return Ok(Some(Cow::Owned(bytes)));
        }
        gpui_kit::assets::AllAssets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut ours: Vec<SharedString> = EMBEDDED
            .iter()
            .filter(|(p, _)| p.starts_with(path))
            .map(|(p, _)| SharedString::from(*p))
            .collect();
        for (_, theme) in ICON_THEMES {
            for icon in ColorIcon::ALL {
                for on_action in [false, true] {
                    let resource = color_icon_path(*icon, *theme, on_action);
                    if resource.starts_with(path) {
                        ours.push(resource);
                    }
                }
            }
        }
        ours.extend(gpui_kit::assets::AllAssets.list(path)?);
        Ok(ours)
    }
}

/// Lucide icon by name, e.g. `icon("archive")`.
pub fn icon(name: &str) -> gpui_kit::component::Icon {
    gpui_kit::component::Icon::default().path(format!("icons/{name}.svg"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::HashSet, sync::Arc};

    #[test]
    fn color_icons_are_embedded_renderable_and_keep_their_color() {
        let assets = Assets;
        let renderer = gpui_kit::SvgRenderer::new(Arc::new(Assets));
        for icon in ColorIcon::ALL {
            let mut rendered_themes = HashSet::new();
            for (_, theme) in ICON_THEMES {
                let bytes = assets
                    .load(&color_icon_path(*icon, *theme, false))
                    .unwrap()
                    .expect("embedded color icon");
                let image = renderer.render_single_frame(&bytes, 1.).unwrap();
                assert!(
                    image.size(0).width.0 >= 128 && image.size(0).height.0 >= 128,
                    "{icon:?}"
                );
                let colors: HashSet<_> = image
                    .as_bytes(0)
                    .unwrap()
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .filter(|pixel| pixel[3] >= 250)
                    .map(|pixel| [pixel[0], pixel[1], pixel[2]])
                    .collect();
                assert!(colors.len() > 16, "{icon:?} lost its full-color shading");
                assert!(
                    colors.iter().any(|[r, g, b]| r != g || g != b),
                    "{icon:?} became monochrome"
                );
                // Without desktop colors, the desktop themes fall back to Light and Dark.
                let built_in = !matches!(theme, ResolvedTheme::SystemLight | ResolvedTheme::SystemDark);
                assert!(
                    !built_in || rendered_themes.insert(image.as_bytes(0).unwrap().to_vec()),
                    "{icon:?}: {theme:?} reused another theme's colors"
                );
                let on_action = assets
                    .load(&color_icon_path(*icon, *theme, true))
                    .unwrap()
                    .unwrap();
                let action_image = renderer.render_single_frame(&on_action, 1.).unwrap();
                assert_ne!(
                    image.as_bytes(0).unwrap(),
                    action_image.as_bytes(0).unwrap(),
                    "{icon:?}: primary button needs its own contrast colors"
                );
            }
        }
    }
}
