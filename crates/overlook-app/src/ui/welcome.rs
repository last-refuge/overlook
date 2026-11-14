//! The window while the device has no accounts: a welcome beneath the alpine-dawn horizon,
//! then the services to connect. Gmail, Microsoft and iCloud Mail lead as larger choices, the
//! other guided services follow, and any IMAP or POP3 server sits on a quieter line.
//!
//! Every choice is a focusable button with an accessible name, wearing its service's own mark
//! (see `branding/providers/README.md`). Hovering or focusing a choice draws its edge and lights
//! the beacon on its plate, the small amber light of the horizon mark; keyboard focus adds a
//! solid ring. The banner's dusk veil lifts once as the window opens; the words never wait
//! for it.

use super::*;
use crate::assets::{color_icon, ColorIcon};
use gpui_kit::component::scroll::ScrollableElement;
use overlook_provider::app_passwords::{self, AppPasswordGuide};
use crate::theme::{mix, relative_luminance as luminance};
use std::rc::Rc;

/// The widest the choices grow; the banner's words align with their left edge.
const COLUMN: f32 = 1040.;
const GUTTER: f32 = 32.;
/// The banner takes about a third of the window, within these bounds, so a short window
/// still shows every choice.
const BANNER_MIN: f32 = 220.;
const BANNER_MAX: f32 = 300.;

/// Brand colours from `llm-docs/overlook-brand.md`. The banner is always the photograph, so
/// its words are ivory in every theme.
const MIDNIGHT: u32 = 0x101C24;
const IVORY: u32 = 0xF0EBDD;
const BEACON: u32 = 0xFF9D32;

fn hex(value: u32) -> Hsla {
    gpui_kit::rgb(value).into()
}

/// What a choice opens.
#[derive(Clone, Copy)]
enum Connect {
    Google,
    Microsoft,
    Guide(&'static AppPasswordGuide),
    Server(ProviderKind),
}

/// How a choice is pictured.
#[derive(Clone, Copy)]
enum Mark {
    /// The service's logo on a white plate, with an optical scale: a full square like
    /// Microsoft's reads larger than a glyph at the same size.
    Logo(&'static str, f32),
    /// One of Overlook's colour icons, on a plate tinted by the theme.
    Own(ColorIcon),
}

struct Choice {
    id: SharedString,
    name: SharedString,
    detail: &'static str,
    mark: Mark,
    connect: Connect,
}

impl Choice {
    /// A service with an app-password guide. One added later still gets a choice, with
    /// Overlook's mail icon until it has a mark.
    fn guide(guide: &'static AppPasswordGuide) -> Self {
        let (mark, detail) = match guide.short_name {
            "iCloud" => (Mark::Logo("overlook/providers/icloud.svg", 1.), "App-specific password"),
            "Yahoo" => (Mark::Logo("overlook/providers/yahoo.svg", 0.9), "App password"),
            "AOL" => (Mark::Logo("overlook/providers/aol.svg", 1.04), "App password"),
            "Fastmail" => (Mark::Logo("overlook/providers/fastmail.svg", 0.9), "App password"),
            "Proton" => (Mark::Logo("overlook/providers/proton-mail.svg", 0.9), "Via Proton Mail Bridge"),
            _ => (Mark::Own(ColorIcon::Mail), "App password"),
        };
        Self { id: format!("welcome-{}", guide.short_name.to_ascii_lowercase()).into(), name: guide.provider.into(), detail, mark, connect: Connect::Guide(guide) }
    }

    /// The most used services, shown larger.
    fn featured() -> [Self; 3] {
        [
            Self { id: "welcome-gmail".into(), name: "Gmail".into(), detail: "Sign in with Google", mark: Mark::Logo("overlook/providers/google-g.png", 1.), connect: Connect::Google },
            Self { id: "welcome-microsoft".into(), name: "Microsoft".into(), detail: "Outlook and Microsoft\u{a0}365", mark: Mark::Logo("overlook/providers/microsoft.svg", 0.8), connect: Connect::Microsoft },
            Self::guide(&app_passwords::ICLOUD),
        ]
    }

    /// The other guided services, in the order the guide list keeps.
    fn others() -> Vec<Self> {
        app_passwords::GUIDES.iter().filter(|guide| !std::ptr::eq(**guide, &app_passwords::ICLOUD)).map(|guide| Self::guide(guide)).collect()
    }

    /// Any server, for everything else.
    fn servers() -> [Self; 2] {
        [
            Self { id: "welcome-imap".into(), name: "IMAP account".into(), detail: "Keeps your mail on the server", mark: Mark::Own(ColorIcon::Imap), connect: Connect::Server(ProviderKind::Imap) },
            Self { id: "welcome-pop3".into(), name: "POP3 account".into(), detail: "Downloads mail to this device", mark: Mark::Own(ColorIcon::Pop3), connect: Connect::Server(ProviderKind::Pop3) },
        ]
    }
}

/// The colours of the choices in the current theme.
#[derive(Clone, Copy)]
struct Tones {
    /// The page under the choices.
    ground: Hsla,
    card: Hsla,
    card_hover: Hsla,
    card_active: Hsla,
    border: Hsla,
    own_plate: Hsla,
    logo_plate_border: Hsla,
    action: Hsla,
    focus: Hsla,
    foreground: Hsla,
    muted: Hsla,
}

impl Tones {
    fn of(p: &Palette) -> Self {
        let card = if p.is_dark() { p.chip } else { p.message_surface() };
        // A light theme whose surface is as white as the cards (Alexander, White) steps the
        // ground toward its frame colour, or failing that toward its ink, so the cards lift
        // off the page instead of resting on a hairline alone.
        let close = |a: Hsla, b: Hsla| (luminance(a) - luminance(b)).abs() < 0.06;
        let ground = if p.is_dark() || !close(card, p.surface) {
            p.surface
        } else if !close(card, p.background) {
            p.background
        } else {
            mix(p.surface, p.foreground, 0.045)
        };
        Self {
            ground,
            card,
            card_hover: mix(card, p.selection, 0.3),
            card_active: mix(card, p.selection, 0.55),
            border: if p.is_dark() { p.divider } else { mix(p.divider, card, 0.35) },
            own_plate: mix(card, p.selection, 0.5),
            // White plates need an edge on a white card, none on a dark one.
            logo_plate_border: if p.is_dark() { gpui_kit::transparent_black() } else { mix(p.divider, card, 0.55) },
            action: p.action,
            focus: p.focus,
            foreground: p.foreground,
            muted: p.muted,
        }
    }
}

/// How large a choice is drawn.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Size {
    /// The three most used services: a large card with an arrow.
    Featured,
    /// The other guided services: a smaller card.
    Compact,
    /// Any server: no card until it is hovered or focused.
    Quiet,
}

/// What a choice does when clicked or activated from the keyboard.
type ClickHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// One choice as a focusable button. It keeps its own focus handle so its edge, ring, arrow
/// and beacon can answer keyboard focus as well as the pointer; a click doesn't take focus,
/// so only the keyboard lights a choice that way.
#[derive(IntoElement)]
struct ChoiceCard {
    choice: Choice,
    size: Size,
    tones: Tones,
    palette: Palette,
    on_click: ClickHandler,
}

impl RenderOnce for ChoiceCard {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let Self { choice, size, tones: t, palette, on_click } = self;
        // A handle the element tracks carries its own place in the tab order.
        let focus = window.use_keyed_state(choice.id.clone(), cx, |_, cx| cx.focus_handle().tab_index(0).tab_stop(true)).read(cx).clone();
        let focused = focus.is_focused(window);
        let group: SharedString = format!("{}-card", choice.id).into();
        // Contents sit at the top, sized so a one-line card is exactly its row's height and
        // the plate centres on the words; a detail that wraps grows the card downward and
        // leaves every name on its row's shared line.
        let (height, plate, gap, pad_x, pad_y, plate_top, text_top, name_size, detail_size) = match size {
            Size::Featured => (112., 52., 16., 20., 30., 0., 5., 16., 13.),
            Size::Compact => (68., 36., 12., 14., 14.5, 1.5, 0., 14., 12.),
            Size::Quiet => (56., 32., 12., 12., 9., 3., 0., 13., 12.),
        };
        let quiet = size == Size::Quiet;
        let edge = if focused { t.focus } else if quiet { gpui_kit::transparent_black() } else { t.border };
        div()
            .id(choice.id.clone())
            .group(group.clone())
            .track_focus(&focus)
            .tab_index(0)
            .role(Role::Button)
            .aria_label(SharedString::from(format!("{}: {}", choice.name, choice.detail)))
            .cursor_pointer()
            // A pointer click opens the service without leaving a focus ring behind.
            .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
            .on_click(move |event, window, cx| on_click(event, window, cx))
            .map(|d| match size {
                Size::Featured => d.flex_grow(1.).flex_shrink(1.).flex_basis(px(260.)),
                Size::Compact => d.flex_grow(1.).flex_shrink(1.).flex_basis(px(200.)),
                Size::Quiet => d.flex_none(),
            })
            // GPUI measures text at its unwrapped width, which would push every card past
            // its basis and onto a row of its own; the basis decides the columns instead.
            .min_w_0()
            .min_h(px(height))
            .flex()
            .items_start()
            .gap(px(gap))
            .px(px(pad_x))
            .py(px(pad_y))
            .rounded(px(12.))
            .bg(if quiet { gpui_kit::transparent_black() } else { t.card })
            .border_1()
            .border_color(edge)
            .hover(|s| s.bg(t.card_hover).border_color(if focused { t.focus } else { t.action }))
            .active(|s| s.bg(t.card_active))
            // Keyboard focus: a solid ring in the theme's focus colour, outside the edge.
            .when(focused, |d| d.shadow(vec![BoxShadow { color: t.focus, offset: point(px(0.), px(0.)), blur_radius: px(0.), spread_radius: px(2.), inset: false }]))
            .child(div().flex_none().mt(px(plate_top)).child(mark_plate(choice.mark, plate, &group, focused, &palette, t)))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .mt(px(text_top))
                    .gap(px(2.))
                    .child(
                        div()
                            .text_size(px(name_size))
                            .line_height(px(name_size + 6.))
                            .font_weight(if quiet { FontWeight::MEDIUM } else { FontWeight::SEMIBOLD })
                            .text_color(t.foreground)
                            .truncate()
                            .child(choice.name),
                    )
                    // One line where the window allows; a narrow one wraps rather than cuts.
                    .child(div().text_size(px(detail_size)).line_height(px(detail_size + 5.)).text_color(t.muted).whitespace_normal().line_clamp(2).child(choice.detail)),
            )
            .when(size == Size::Featured, |d| {
                d.child(
                    div()
                        .flex_none()
                        .h(px(plate))
                        .flex()
                        .items_center()
                        .text_color(if focused { t.action } else { t.muted })
                        .group_hover(group, |s| s.text_color(t.action))
                        .child(icon("arrow-right").size(px(16.))),
                )
            })
    }
}

/// A choice's plate: a service's logo on white, or an Overlook icon on the theme's tint,
/// with the beacon that lights while the choice is hovered or focused.
fn mark_plate(mark: Mark, size: f32, group: &SharedString, lit: bool, p: &Palette, t: Tones) -> impl IntoElement {
    let (bg, border) = match mark {
        Mark::Logo(..) => (hex(0xFFFFFF), t.logo_plate_border),
        Mark::Own(_) => (t.own_plate, gpui_kit::transparent_black()),
    };
    div()
        .relative()
        .flex_none()
        .size(px(size))
        .rounded(px((size * 0.27).round()))
        .bg(bg)
        .border_1()
        .border_color(border)
        .flex()
        .items_center()
        .justify_center()
        .child(match mark {
            Mark::Logo(path, scale) => img(path).size(px((size * 0.58 * scale).round())).flex_none().into_any_element(),
            Mark::Own(own) => color_icon(own, (size * 0.68).round(), p).into_any_element(),
        })
        // The beacon: a 7 px amber light, ringed by the hovered card's own fill.
        .child(
            div()
                .absolute()
                .top(px(-3.))
                .right(px(-3.))
                .size(px(9.))
                .rounded_full()
                .bg(hex(BEACON))
                .border_1()
                .border_color(t.card_hover)
                .opacity(if lit { 1. } else { 0. })
                .group_hover(group.clone(), |s| s.opacity(1.)),
        )
}

impl MailWindow {
    /// The whole window while the device has no accounts: only adding one is possible.
    pub(super) fn render_no_account(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p: Palette = *cx.palette();
        let tones = Tones::of(&p);
        let banner = (f32::from(window.viewport_size().height) * 0.32).clamp(BANNER_MIN, BANNER_MAX);
        v_flex()
            .id("mail-window")
            .key_context(WINDOW_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|_, _: &QuitOverlook, _window, cx| cx.defer(crate::lifecycle::quit)))
            .on_action(cx.listener(Self::on_escape))
            .size_full()
            .bg(tones.ground)
            .text_color(p.foreground)
            .font_family("Inter")
            .text_size(px(13.))
            .relative()
            .child(TitleBar::new().h(px(52.)).bg(p.surface).child(h_flex().size_full().items_center().pl(px(12.)).child(endorsed_lockup(&p))))
            .children(self.render_banner(cx))
            .child(
                v_flex()
                    .id("welcome")
                    .flex_1()
                    .min_h_0()
                    .child(div().flex_none().w_full().child(horizon(banner)).child(self.render_choices(&p, tones, cx)))
                    .overflow_y_scrollbar(),
            )
            .when(self.account_setup.is_some(), |d| d.child(self.render_account_setup(cx)))
    }

    /// "Connect your mail": the featured services, the others, then any server.
    fn render_choices(&self, p: &Palette, tones: Tones, cx: &mut Context<Self>) -> impl IntoElement {
        let card = |choice: Choice, size: Size, cx: &mut Context<Self>| {
            let connect = choice.connect;
            let on_click = cx.listener(move |this, _: &ClickEvent, window, cx| this.connect_service(connect, window, cx));
            ChoiceCard { choice, size, tones, palette: *p, on_click: Rc::new(on_click) }
        };
        let featured: Vec<ChoiceCard> = Choice::featured().into_iter().map(|choice| card(choice, Size::Featured, cx)).collect();
        let others: Vec<ChoiceCard> = Choice::others().into_iter().map(|choice| card(choice, Size::Compact, cx)).collect();
        let servers: Vec<ChoiceCard> = Choice::servers().into_iter().map(|choice| card(choice, Size::Quiet, cx)).collect();
        h_flex().w_full().justify_center().child(
            v_flex()
                .w_full()
                .max_w(px(COLUMN))
                .px(px(GUTTER))
                .pt(px(36.))
                .pb(px(44.))
                .child(
                    v_flex()
                        .gap(px(4.))
                        .child(div().text_size(px(18.)).line_height(px(24.)).font_weight(FontWeight::SEMIBOLD).child("Connect your mail"))
                        .child(div().text_size(px(13.)).line_height(px(19.)).text_color(p.muted).child("Choose where your mail lives. Overlook fills in the settings and shows you how to sign in.")),
                )
                .child(h_flex().flex_wrap().items_stretch().gap(px(16.)).mt(px(20.)).children(featured))
                .child(h_flex().flex_wrap().items_stretch().gap(px(12.)).mt(px(12.)).children(others))
                .child(div().mt(px(32.)).text_size(px(12.)).line_height(px(16.)).font_weight(FontWeight::MEDIUM).text_color(p.muted).child("Another provider, or your own server"))
                // Lighter than the cards above: their own width, no fill until pointed at.
                .child(h_flex().flex_wrap().gap(px(8.)).mt(px(6.)).ml(px(-12.)).children(servers))
                .child(
                    h_flex()
                        .mt(px(28.))
                        .gap(px(8.))
                        .items_center()
                        .text_size(px(12.))
                        .line_height(px(16.))
                        .text_color(p.muted)
                        .child(icon("lock-keyhole").size(px(14.)))
                        .child("Passwords and sign-ins are kept in your system keyring, never in files."),
                ),
        )
    }

    fn connect_service(&mut self, connect: Connect, window: &mut Window, cx: &mut Context<Self>) {
        match connect {
            Connect::Google => self.start_google_sign_in(None, window, cx),
            Connect::Microsoft => self.open_microsoft_setup(window, cx),
            Connect::Guide(guide) => self.open_guided_setup(guide, window, cx),
            Connect::Server(kind) => self.open_account_setup(kind, SetupMode::New, window, cx),
        }
    }
}

/// The alpine-dawn lake with the welcome on its dark water, aligned to the choices below. A
/// midnight shade under the words keeps them legible whatever the crop; the dusk veil over
/// the whole scene thins once as the window opens.
fn horizon(height: f32) -> impl IntoElement {
    let midnight = hex(MIDNIGHT);
    let ivory = hex(IVORY);
    div()
        .relative()
        .w_full()
        .h(px(height))
        .overflow_hidden()
        .bg(midnight)
        .child(img("overlook/alpine-dawn.png").absolute().inset_0().size_full().object_fit(ObjectFit::Cover))
        .child(div().absolute().inset_0().bg(linear_gradient(90., linear_color_stop(midnight.opacity(0.82), 0.), linear_color_stop(midnight.opacity(0.), 0.68))))
        .child(
            div()
                .absolute()
                .inset_0()
                .bg(midnight)
                .with_animation("welcome-dawn", Animation::new(Duration::from_millis(1400)).with_easing(ease_out_quint()), |veil, delta| veil.opacity(0.6 * (1. - delta))),
        )
        .child(
            h_flex().absolute().inset_0().justify_center().child(
                v_flex()
                    .w_full()
                    .max_w(px(COLUMN))
                    .h_full()
                    .px(px(GUTTER))
                    .justify_center()
                    .gap(px(12.))
                    .child(div().max_w(px(640.)).font_family("Source Serif 4 Display").text_size(px(48.)).line_height(px(56.)).text_color(ivory).child("A calmer place for your mail."))
                    .child(div().max_w(px(520.)).text_size(px(15.)).line_height(px(22.)).text_color(ivory.opacity(0.84)).child("Welcome to Overlook. Connect an account to bring your mail in.")),
            ),
        )
}

/// The title bar's lockup with the parent endorsement the welcome screen carries.
fn endorsed_lockup(p: &Palette) -> impl IntoElement {
    h_flex()
        .items_center()
        .gap(px(10.))
        .child(brand_tile(30.))
        .child(div().font_family("Source Serif 4 Display").text_size(px(22.)).line_height(px(30.)).text_color(p.foreground).child("Overlook"))
        .child(div().pt(px(3.)).text_size(px(12.)).text_color(p.muted).child("by Last Refuge"))
}
