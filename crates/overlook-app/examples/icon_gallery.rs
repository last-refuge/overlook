//! Native icon proof sheet, without opening a mail profile or connecting accounts.
//! Run with: cargo run -p overlook --example icon_gallery
#![allow(dead_code)]

#[path = "../src/assets.rs"]
mod assets;
#[path = "../src/theme.rs"]
mod theme;

use assets::{color_icon, color_icon_on_action, ColorIcon};
use gpui_kit::{prelude::*, App, Bounds, Context, FontWeight, Window, WindowBounds, WindowOptions, div, px, rgb, size};
use overlook_core::ResolvedTheme;
use std::borrow::Cow;
use theme::Palette;

struct IconGallery;

impl Render for IconGallery {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let mut sheet = div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(0xF3F5F4))
            .text_color(rgb(0x26332C))
            .font_family("Inter")
            .p(px(28.))
            .gap(px(20.))
            .child(div().text_size(px(24.)).font_weight(FontWeight::SEMIBOLD).child("Overlook · refined icons"))
            .child(div().text_size(px(13.)).child("Native vectors at actual UI sizes. Consistent outlines, subtle fills, no gradients."));

        for (label, resolved) in [
            ("Alexander", ResolvedTheme::Alexander),
            ("Light", ResolvedTheme::Light),
            ("Dark", ResolvedTheme::Dark),
        ] {
            let p = Palette::for_theme(resolved);
            let mut panel = div().flex().flex_col().gap(px(18.)).p(px(20.)).rounded(px(8.)).bg(p.chrome()).text_color(p.foreground)
                .child(div().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).child(label));
            for chunk in ColorIcon::ALL.chunks(13) {
                let mut row = div().flex().gap(px(4.));
                for icon in chunk {
                    row = row.child(div().flex().flex_col().items_center().gap(px(9.)).w(px(76.)).flex_none()
                        .child(color_icon(*icon, 20., &p))
                        .child(div().text_size(px(10.)).text_color(p.muted).child(format!("{icon:?}"))));
                }
                panel = panel.child(row);
            }
            sheet = sheet.child(panel);
        }

        let p = Palette::for_theme(ResolvedTheme::Alexander);
        let mut sizes = div().flex().items_center().gap(px(24.)).text_size(px(12.))
            .child("Size & state");
        for edge in [11., 16., 20., 21., 28.] {
            sizes = sizes.child(div().flex().items_center().gap(px(8.))
                .child(color_icon(ColorIcon::Mail, edge, &p)).child(format!("{edge}px")));
        }
        sizes = sizes
            .child(div().flex().items_center().gap(px(8.)).px(px(12.)).py(px(8.)).rounded(px(6.)).bg(p.action).text_color(p.on_action)
                .child(color_icon_on_action(ColorIcon::Compose, 20., &p)).child("New message"))
            .child(div().opacity(0.4).child(color_icon(ColorIcon::Delete, 20., &p)))
            .child("Disabled");
        sheet.child(sizes)
    }
}

fn main() {
    gpui_kit::application().with_assets(assets::Assets).run(|cx: &mut App| {
        gpui_kit::init(cx);
        cx.text_system().add_fonts(assets::INTER_FACES.iter().map(|face| Cow::Borrowed(*face)).collect()).unwrap();
        let bounds = Bounds::centered(None, size(px(1136.), px(820.)), cx);
        cx.open_window(WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            app_id: Some("ai.lastrefuge.Overlook.IconGallery".into()),
            ..Default::default()
        }, |_, cx| cx.new(|_| IconGallery)).unwrap();
        cx.activate(true);
    });
}
