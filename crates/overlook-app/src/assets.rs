//! Embedded assets: brand artwork and fonts from `branding/`, plus the Lucide icon set
//! bundled with GPUI Kit. Paths under `overlook/` are ours; everything else is delegated.

use crate::theme::Palette;
use gpui_kit::{AssetSource, Div, ParentElement, Result, SharedString, Styled, div, px, svg};
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
    // The services' own marks for the welcome screen; provenance in branding/providers/README.md.
    ("overlook/providers/google-g.png", include_bytes!("../../../branding/providers/google-g.png")),
    ("overlook/providers/microsoft.svg", include_bytes!("../../../branding/providers/microsoft.svg")),
    ("overlook/providers/icloud.svg", include_bytes!("../../../branding/providers/icloud.svg")),
    ("overlook/providers/aol.svg", include_bytes!("../../../branding/providers/aol.svg")),
    ("overlook/providers/proton-mail.svg", include_bytes!("../../../branding/providers/proton-mail.svg")),
    ("overlook/providers/yahoo.svg", include_bytes!("../../../branding/providers/yahoo.svg")),
    ("overlook/providers/fastmail.svg", include_bytes!("../../../branding/providers/fastmail.svg")),
];


// Original two-tone SVGs. Paint the fill and outline as separate native vector masks.
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
    History => "history",
    Imap => "imap",
    Pop3 => "pop3",
}

fn icon_layer_path(icon: ColorIcon, layer: &str) -> SharedString {
    format!("overlook/icons/{layer}/{}", icon.file()).into()
}

pub fn color_icon(icon: ColorIcon, size: f32, palette: &Palette) -> Div {
    vector_icon(icon, size, palette.action)
}

pub fn color_icon_on_action(icon: ColorIcon, size: f32, palette: &Palette) -> Div {
    vector_icon(icon, size, palette.on_action)
}

fn vector_icon(icon: ColorIcon, size: f32, color: gpui_kit::Hsla) -> Div {
    // `img` rasterizes SVGs at their intrinsic size, then downsamples that texture.
    // `svg` rasterizes its mask for the actual bounds and display scale instead.
    // Tint at paint time: a theme switch never reuses stale colored image pixels.
    div()
        .relative()
        .size(px(size))
        .flex_none()
        .child(
            svg()
                .path(icon_layer_path(icon, "fill"))
                .absolute()
                .top(px(0.))
                .left(px(0.))
                .size(px(size))
                .text_color(color.opacity(0.12)),
        )
        .child(
            svg()
                .path(icon_layer_path(icon, "stroke"))
                .absolute()
                .top(px(0.))
                .left(px(0.))
                .size(px(size))
                .text_color(color),
        )
}

/// Extract one flat group from our generated artwork, retaining the shared view box.
/// These immutable masks are shared by every colorway and contrast context.
fn icon_layer(path: &str) -> Option<Vec<u8>> {
    let (layer, file) = path.strip_prefix("overlook/icons/")?.split_once('/')?;
    if !matches!(layer, "fill" | "stroke") {
        return None;
    }
    let (_, source) = COLOR_ASSETS.iter().find(|(name, _)| *name == file)?;
    let source = std::str::from_utf8(source).ok()?;
    let header = source.split_once('>')?.0;
    let start = source.find(&format!("<g id=\"{layer}\""))?;
    let end = start + source[start..].find("</g>")? + "</g>".len();
    Some(format!("{header}>{}</svg>", &source[start..end]).into_bytes())
}

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some((_, bytes)) = EMBEDDED.iter().find(|(p, _)| *p == path) {
            return Ok(Some(Cow::Borrowed(bytes)));
        }
        if let Some(bytes) = icon_layer(path) {
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
        for icon in ColorIcon::ALL {
            for layer in ["fill", "stroke"] {
                let resource = icon_layer_path(*icon, layer);
                if resource.starts_with(path) {
                    ours.push(resource);
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
    use std::sync::Arc;

    #[test]
    fn provider_marks_are_embedded_and_keep_their_colors() {
        let assets = Assets;
        let renderer = gpui_kit::SvgRenderer::new(Arc::new(Assets));
        let marks = EMBEDDED.iter().map(|(path, _)| *path).filter(|path| path.starts_with("overlook/providers/"));
        assert_eq!(marks.clone().count(), 7);
        for path in marks {
            let bytes = assets.load(path).unwrap().unwrap_or_else(|| panic!("{path} is embedded"));
            if !path.ends_with(".svg") {
                assert!(bytes.starts_with(b"\x89PNG"), "{path}");
                continue;
            }
            let image = renderer.render_single_frame(&bytes, 4.).unwrap();
            let colored = image.as_bytes(0).unwrap().as_chunks::<4>().0.iter().filter(|pixel| pixel[3] >= 250 && (pixel[0] != pixel[1] || pixel[1] != pixel[2])).count();
            assert!(colored > 50, "{path} renders in its colors");
        }
    }

    #[test]
    fn icon_layers_render_at_control_sizes_and_display_scales() {
        let assets = Assets;
        let renderer = gpui_kit::SvgRenderer::new(Arc::new(Assets));
        for icon in ColorIcon::ALL {
            for layer in ["fill", "stroke"] {
                let bytes = assets
                    .load(&icon_layer_path(*icon, layer))
                    .unwrap()
                    .expect("embedded icon layer");
                let parsed = renderer.parse_svg(&bytes).unwrap();
                // GPUI's vector path supersamples the actual device-pixel bounds by 2.
                // Include the tiny server badges, reader actions, toolbar, rail and welcome.
                for logical_size in [11_f32, 15., 16., 20., 21., 28.] {
                    for scale in [1_f32, 1.25, 1.5, 2.] {
                        let edge = (logical_size * scale * 2.).ceil() as i32;
                        let size = gpui_kit::size(gpui_kit::DevicePixels(edge), gpui_kit::DevicePixels(edge));
                        let image = renderer.render_parsed(&parsed, gpui_kit::SvgSize::ExactSize(size)).unwrap();
                        assert_eq!(image.size(0), size, "{icon:?}/{layer} at {logical_size} × {scale}");
                        let pixels = image.as_bytes(0).unwrap();
                        assert!(pixels.chunks_exact(4).any(|pixel| pixel[3] > 200), "{icon:?}/{layer} is empty");
                        // Keep a transparent safety margin: no strokes should be cut off.
                        let edge = edge as usize;
                        for i in 0..edge {
                            for pixel in [i, (edge - 1) * edge + i, i * edge, i * edge + edge - 1] {
                                assert_eq!(pixels[pixel * 4 + 3], 0, "{icon:?}/{layer} clips at {logical_size} × {scale}");
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn icon_layer_paths_are_complete_and_reject_unknown_resources() {
        let assets = Assets;
        let paths = assets.list("overlook/icons/").unwrap();
        assert_eq!(paths.len(), ColorIcon::ALL.len() * 2);
        for path in paths {
            assert!(assets.load(&path).unwrap().is_some(), "{path}");
        }
        for path in [
            "overlook/icons/unknown/mail.svg",
            "overlook/icons/fill/missing.svg",
            "overlook/icons/fill/mail.svg/extra",
            "overlook/icons/fill/../mail.svg",
        ] {
            assert!(icon_layer(path).is_none(), "{path}");
        }
    }
}
