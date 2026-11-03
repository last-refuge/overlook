//! App navigation and contextual commands, independent of the reading panes.

use super::*;
use crate::assets::{color_icon, ColorIcon as C};

pub(super) const RAIL_WIDTH: f32 = 48.;
/// The title bar and the command toolbar share one control size and one vertical inset so
/// their contents sit on the same rhythm; the inset follows the density setting.
pub(super) const HEADER_CONTROL_HEIGHT: f32 = 36.;

pub(super) fn header_height(density: Density) -> f32 {
    let inset = match density {
        Density::Comfortable => 8.,
        Density::Compact => 4.,
    };
    HEADER_CONTROL_HEIGHT + inset * 2.
}

impl MailWindow {
    pub(super) fn render_app_rail(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = *cx.palette();
        v_flex()
            .w(px(RAIL_WIDTH))
            .h_full()
            .flex_none()
            .items_center()
            .bg(p.surface)
            .border_r_1()
            .border_color(p.divider.opacity(0.6))
            .child(
                div().h(px(header_height(self.density))).w_full().flex_none().child(
                    div()
                        .size_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(brand_tile(24.)),
                ),
            )
            .child(
                v_flex().gap(px(4.)).pt(px(8.)).children(
                    [
                        ("rail-mail", "Mail", "Mail (Ctrl 1)", Module::Mail, C::Mail),
                        (
                            "rail-calendar",
                            "Calendar",
                            "Calendar (Ctrl 2)",
                            Module::Calendar,
                            C::Calendar,
                        ),
                        (
                            "rail-contacts",
                            "Contacts",
                            "Contacts (Ctrl 3)",
                            Module::Contacts,
                            C::Contacts,
                        ),
                    ]
                    .into_iter()
                    .map(|(id, label, tip, module, glyph)| {
                        let selected = self.module == module;
                        div()
                            .relative()
                            .w(px(RAIL_WIDTH))
                            .h(px(44.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .when(selected, |d| {
                                d.child(
                                    div()
                                        .absolute()
                                        .left_0()
                                        .top(px(12.))
                                        .w(px(2.))
                                        .h(px(20.))
                                        .rounded_r(px(3.))
                                        .bg(p.action),
                                )
                            })
                            .child(
                                Button::new(id)
                                    .ghost()
                                    .w(px(38.))
                                    .h(px(40.))
                                    .p(px(4.))
                                    .rounded(px(8.))
                                    .toggled(selected)
                                    .when(selected, |b| b.bg(p.selection))
                                    .accessibility_label(label)
                                    .tooltip(tip)
                                    .child(color_icon(glyph, 21., &p))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.set_module(module, window, cx)
                                    })),
                            )
                    }),
                ),
            )
            .child(div().flex_1())
            .child(
                Button::new("rail-settings")
                    .ghost()
                    .w(px(38.))
                    .h(px(40.))
                    .p(px(4.))
                    .mb(px(10.))
                    .accessibility_label("Settings")
                    .tooltip("Settings (Ctrl ,)")
                    .child(color_icon(C::Settings, 20., &p))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.open_settings(SettingsPage::General, window, cx)
                    })),
            )
    }

    pub(super) fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = *cx.palette();
        let owner = self.owner();
        let search: AnyElement = match self.module {
            Module::Mail => Input::new(&self.search_input)
                .prefix(icon("search").size(px(16.)).text_color(p.muted))
                .suffix(kbd("Ctrl E", &p))
                .cleanable(true)
                .into_any_element(),
            Module::Contacts => Input::new(&self.contact_search)
                .prefix(icon("search").size(px(16.)).text_color(p.muted))
                .suffix(kbd("Ctrl E", &p))
                .cleanable(true)
                .into_any_element(),
            Module::Calendar => Button::new("calendar-commands")
                .ghost()
                .w_full()
                .h(px(HEADER_CONTROL_HEIGHT))
                .justify_start()
                .bg(p.message_surface())
                .icon(icon("search").size(px(16.)))
                .label("Search commands")
                .tooltip("Search commands (Ctrl K)")
                .on_click(cx.listener(|this, _, window, cx| this.open_palette(window, cx)))
                .into_any_element(),
        };
        TitleBar::new().h(px(header_height(self.density))).bg(p.surface).child(
            h_flex()
                .size_full()
                .items_center()
                .gap(px(24.))
                .pl(px(12.))
                .pr(px(20.))
                .child(
                    div()
                        .w(px(152.))
                        .flex_none()
                        .font_family("Source Serif 4 Display")
                        .text_size(px(23.))
                        .line_height(px(30.))
                        .child("Overlook"),
                )
                .child(div().flex_1().min_w(px(140.)).max_w(px(640.)).child(search))
                .child(div().flex_1())
                .child(
                    Button::new("account-settings")
                        .ghost()
                        .size(px(HEADER_CONTROL_HEIGHT))
                        .p(px(3.))
                        .rounded_full()
                        .accessibility_label("Account settings")
                        .tooltip(format!("{} · {}", owner.display_name(), owner.email))
                        .child(owner_avatar(&owner, 29., &p))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.open_settings(SettingsPage::Accounts, window, cx)
                        })),
                ),
        )
    }

    pub(super) fn mail_move_menu(
        &self,
        button: Button,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let labels: Vec<(LabelId, String)> = self
            .labels
            .iter()
            .filter(|l| l.kind == LabelKind::User)
            .map(|l| (l.id.clone(), l.name.clone()))
            .collect();
        let ids = self.selected_message_ids();
        let this = cx.entity().downgrade();
        button.dropdown_menu(move |mut menu, _, _| {
            for (label, view) in [
                ("Inbox", MailboxView::Kind(LabelKind::Inbox)),
                ("Archive", MailboxView::Kind(LabelKind::Archive)),
                ("Trash", MailboxView::Kind(LabelKind::Trash)),
            ] {
                let this = this.clone();
                let ids = ids.clone();
                menu = menu.item(PopupMenuItem::new(label).on_click(move |_, window, cx| {
                    let drag = ConversationDrag {
                        mail: DraggedMail::Messages(ids.clone()),
                        title: String::new(),
                        count: 0,
                    };
                    this.update(cx, |this, cx| {
                        this.drop_on_view(drag, view.clone(), window, cx)
                    })
                    .ok();
                }));
            }
            if !labels.is_empty() {
                menu = menu.separator().label("Labels");
            }
            for (id, name) in &labels {
                let this = this.clone();
                let ids = ids.clone();
                let id = id.clone();
                menu = menu.item(PopupMenuItem::new(name.clone()).on_click(
                    move |_, window, cx| {
                        this.update(cx, |this, cx| {
                            this.move_to_label(ids.clone(), id.clone(), window, cx)
                        })
                        .ok();
                    },
                ));
            }
            menu
        })
    }

    pub(super) fn appearance_menu(
        &self,
        button: Button,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let appearance = self.appearance;
        let this = cx.entity().downgrade();
        button.dropdown_menu(move |menu, window, _cx| {
            let system = theme::system_appearance(window.appearance());
            let mut menu = menu.label("Appearance");
            for pref in AppearancePreference::ALL {
                let this = this.clone();
                let label = if pref == AppearancePreference::FollowSystem {
                    match (theme::desktop_theme_name(), system) {
                        (Some(name), _) => format!("Follow system (currently {name})"),
                        (None, SystemAppearance::Dark) => "Follow system (currently Dark)".to_owned(),
                        (None, SystemAppearance::Light) => "Follow system (currently Light)".to_owned(),
                        (None, SystemAppearance::Unknown) => "Follow system".to_owned(),
                    }
                } else {
                    pref.label().to_owned()
                };
                menu = menu.item(
                    PopupMenuItem::new(label)
                        .checked(pref == appearance)
                        .on_click(move |_, window, cx| {
                            let _ =
                                this.update(cx, |this, cx| this.set_appearance(pref, window, cx));
                        }),
                );
            }
            menu
        })
    }

    pub(super) fn layout_menu(&self, button: Button, cx: &mut Context<Self>) -> impl IntoElement {
        let reading_pane = self.reading_pane;
        let density = self.density;
        let focused_enabled = self.focused_enabled;
        let reminders_enabled = self.reminders_enabled;
        let this2 = cx.entity().downgrade();
        button.dropdown_menu(move |menu, _window, _cx| {
            let a = this2.clone();
            let b = this2.clone();
            let c = this2.clone();
            let d = this2.clone();
            let r = this2.clone();
            menu.label("Reading pane")
                .item(
                    PopupMenuItem::new("On the right")
                        .checked(reading_pane == ReadingPane::Right)
                        .on_click(move |_, _w, cx| {
                            let _ = a.update(cx, |this, cx| {
                                this.set_reading_pane(ReadingPane::Right, cx)
                            });
                        }),
                )
                .item(
                    PopupMenuItem::new("Below the list")
                        .checked(reading_pane == ReadingPane::Bottom)
                        .on_click(move |_, _w, cx| {
                            let _ = b.update(cx, |this, cx| {
                                this.set_reading_pane(ReadingPane::Bottom, cx)
                            });
                        }),
                )
                .separator()
                .label("Density")
                .item(
                    PopupMenuItem::new(if density == Density::Compact {
                        "Comfortable"
                    } else {
                        "Compact"
                    })
                    .on_click(move |_, window, cx| {
                        let _ = c.update(cx, |this, cx| this.toggle_density(window, cx));
                    }),
                )
                .separator()
                .label("Inbox")
                .item(
                    PopupMenuItem::new("Focused Inbox")
                        .checked(focused_enabled)
                        .on_click(move |_, _w, cx| {
                            let _ = d.update(cx, |this, cx| this.toggle_focused_inbox(cx));
                        }),
                )
                .separator()
                .label("Calendar")
                .item(
                    PopupMenuItem::new("Event reminders")
                        .checked(reminders_enabled)
                        .on_click(move |_, _w, cx| {
                            let _ = r.update(cx, |this, cx| {
                                this.reminders_enabled = !this.reminders_enabled;
                                this.save_setting("reminders", &this.reminders_enabled);
                                cx.notify();
                            });
                        }),
                )
        })
    }
}
