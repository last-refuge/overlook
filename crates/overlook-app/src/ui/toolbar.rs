//! Customizable command strip and its overflow. Both surfaces execute the same commands.
use super::*;
use super::shell::{header_height, HEADER_CONTROL_HEIGHT};
use crate::assets::{color_icon, color_icon_on_action, ColorIcon as C};
use gpui_kit::base::actions::Confirm;
use gpui_kit::component::button::ButtonCustomVariant;
use gpui_kit::component::{popover::Popover, scroll::ScrollableElement, Disableable};
use overlook_core::toolbar::{
    ToolbarAction as A, ToolbarArea as Area, ToolbarLayout, ToolbarModule as M,
};

impl From<Module> for M {
    fn from(module: Module) -> Self {
        match module {
            Module::Mail => M::Mail,
            Module::Calendar => M::Calendar,
            Module::Contacts => M::Contacts,
        }
    }
}

#[derive(Clone)]
struct ToolbarDrag {
    module: M,
    action: A,
    label: String,
    glyph: Glyph,
}

impl Render for ToolbarDrag {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = *cx.palette();
        h_flex()
            .items_center()
            .gap(px(8.))
            .px(px(12.))
            .py(px(8.))
            .rounded(px(8.))
            .bg(p.surface)
            .border_1()
            .border_color(p.action)
            .shadow_md()
            .text_size(px(13.))
            .text_color(p.foreground)
            .child(self.glyph.render(&p, false))
            .child(self.label.clone())
    }
}

#[derive(Clone, Copy)]
enum Glyph {
    Color(C),
    Symbol(&'static str),
}
impl Glyph {
    fn render(self, p: &Palette, primary: bool) -> AnyElement {
        match self {
            Self::Color(c) => if primary {
                color_icon_on_action(c, 20., p)
            } else {
                color_icon(c, 20., p)
            }
            .into_any_element(),
            Self::Symbol(name) => icon(name).size(px(18.)).into_any_element(),
        }
    }
}
struct Command {
    label: String,
    glyph: Glyph,
    enabled: bool,
    selected: bool,
    primary: bool,
    hint: &'static str,
}

#[derive(Clone, Copy)]
enum ToolbarEdit {
    Customize,
    Done,
    Reset,
    Nudge(A, Area, bool),
    Move(A, Area),
}

impl MailWindow {
    fn toolbar_command(&self, action: A) -> Command {
        use Glyph::{Color, Symbol};
        let (label, glyph, hint) = match action {
            A::NewMessage => ("New message", Color(C::Compose), "Ctrl N"),
            A::Archive => ("Archive", Color(C::Archive), "E"),
            A::DeleteMail => ("Delete", Color(C::Delete), "Del"),
            A::MoveMail => ("Move to", Color(C::Move), "V"),
            A::ToggleRead => ("Mark unread", Color(C::Mail), "Ctrl U"),
            A::ToggleStar => ("Star", Color(C::Star), "S"),
            A::Reply => ("Reply", Color(C::Reply), "Ctrl R"),
            A::ReplyAll => ("Reply all", Color(C::ReplyAll), "Ctrl Shift R"),
            A::Forward => ("Forward", Color(C::Forward), "Ctrl F"),
            A::JumpLatest => ("Latest message", Symbol("arrow-down-to-line"), ""),
            A::PreviousConversation => ("Previous conversation", Symbol("chevron-left"), "K"),
            A::NextConversation => ("Next conversation", Symbol("chevron-right"), "J"),
            A::MoveFocus => ("Move to Other", Color(C::Move), ""),
            A::AlwaysMoveFocus => ("Always move sender", Color(C::Read), ""),
            A::NewEvent => ("New event", Color(C::CalendarAdd), "Ctrl Shift A"),
            A::Today => ("Today", Color(C::Calendar), "T"),
            A::PreviousPeriod => ("Previous period", Symbol("chevron-left"), "←"),
            A::NextPeriod => ("Next period", Symbol("chevron-right"), "→"),
            A::Day => ("Day", Color(C::Day), "D"),
            A::Week => ("Week", Color(C::Week), "W"),
            A::Month => ("Month", Color(C::Month), "M"),
            A::Agenda => ("Agenda", Color(C::Agenda), "A"),
            A::EditEvent | A::EditContact => ("Edit", Color(C::Edit), ""),
            A::DeleteEvent | A::DeleteContact => ("Delete", Color(C::Delete), ""),
            A::NewContact => ("New contact", Color(C::ContactAdd), ""),
            A::ContactMessage => ("New message", Color(C::Compose), ""),
            A::ContactMail => ("Mail from", Color(C::Mail), ""),
            A::Sync => ("Sync", Color(C::Sync), "F5"),
            A::Appearance => ("Appearance", Color(C::Appearance), ""),
            A::Layout => ("Layout", Color(C::Layout), ""),
            A::Unknown => ("Unavailable", Symbol("circle-help"), ""),
        };
        let mut command = Command {
            label: label.into(),
            glyph,
            enabled: true,
            selected: false,
            primary: matches!(action, A::NewMessage | A::NewEvent | A::NewContact),
            hint,
        };
        let reading = self.outbox_selected.is_none()
            && (self.composer.is_none() || self.composer_is_inline());
        let multi = self.multi_selected();
        let active = reading && (multi || (self.selected_summary().is_some() && !self.thread.is_empty()));
        let (trash, inbox) = self.removal_state();
        match action {
            A::Archive => {
                command.enabled = active;
                if trash || !inbox {
                    command.label = if trash { "Restore" } else { "To Inbox" }.into();
                    command.glyph = Color(C::Move);
                }
            }
            A::DeleteMail => command.enabled = active && !trash,
            A::MoveMail => command.enabled = active,
            A::Reply | A::ReplyAll | A::Forward => command.enabled = active && !multi,
            A::ToggleRead => {
                command.enabled = active;
                let any_unread = if multi { self.selection_state().any_unread } else { self.thread.iter().any(|m| !m.is_read) };
                if any_unread {
                    command.label = "Mark read".into();
                    command.glyph = Color(C::Read);
                }
            }
            A::ToggleStar => {
                command.enabled = active;
                command.selected = if multi { self.selection_state().all_starred } else { self.selected_summary().is_some_and(|s| s.starred) };
                if command.selected {
                    command.label = "Unstar".into();
                }
            }
            A::JumpLatest => command.enabled = active && !multi && self.thread.len() > 1,
            A::PreviousConversation | A::NextConversation => {
                command.enabled = reading && !self.conversations.is_empty()
            }
            A::MoveFocus | A::AlwaysMoveFocus => {
                command.enabled = active
                    && !multi
                    && self.focused_enabled
                    && self.view == MailboxView::Kind(LabelKind::Inbox);
                if self.inbox_tab != FocusTab::Focused && action == A::MoveFocus {
                    command.label = "Move to Focused".into();
                }
            }
            A::NewEvent => {
                command.enabled =
                    self.new_event.is_none() && self.calendars.iter().any(|c| c.can_edit)
            }
            A::EditEvent | A::DeleteEvent => {
                command.enabled = self.new_event.is_none()
                    && self
                        .cal_selected
                        .as_ref()
                        .and_then(|id| self.cal_events.iter().find(|e| &e.id == id))
                        .is_some_and(|e| self.can_edit_calendar_event(e))
            }
            A::Day | A::Week | A::Month | A::Agenda => {
                command.selected = self.cal_view
                    == match action {
                        A::Day => CalView::Day,
                        A::Week => CalView::Week,
                        A::Month => CalView::Month,
                        _ => CalView::Agenda,
                    }
            }
            A::NewContact => command.enabled = self.contact_form.is_none(),
            A::ContactMessage | A::ContactMail | A::EditContact | A::DeleteContact => {
                let selected = self
                    .contact_selected
                    .as_ref()
                    .and_then(|id| self.contacts.iter().find(|c| &c.id == id));
                command.enabled = self.contact_form.is_none()
                    && selected.is_some_and(|c| match action {
                        A::ContactMessage | A::ContactMail => !c.email.is_empty(),
                        A::DeleteContact => c.is_editable(),
                        _ => true,
                    });
                if action == A::EditContact && !selected.is_some_and(|c| c.is_editable()) {
                    command.label = "Add to contacts".into();
                    command.glyph = Color(C::ContactAdd);
                }
            }
            A::Sync => command.enabled = !self.is_syncing_active(),
            A::Unknown => command.enabled = false,
            _ => {}
        }
        command
    }

    fn run_toolbar_command(&mut self, action: A, window: &mut Window, cx: &mut Context<Self>) {
        if !self.toolbar_command(action).enabled {
            return;
        }
        self.toolbar_more_open = false;
        self.toolbar_editing = false;
        match action {
            A::NewMessage => self.compose_new(window, cx),
            A::Archive => self.on_archive(&ArchiveSelected, window, cx),
            A::DeleteMail => self.on_trash(&TrashSelected, window, cx),
            A::ToggleRead => self.on_toggle_read(&ToggleRead, window, cx),
            A::ToggleStar => self.on_toggle_star(&ToggleStar, window, cx),
            A::Reply => self.reply_default(window, cx),
            A::ReplyAll => self.reply(DraftKind::ReplyAll, window, cx),
            A::Forward => self.reply(DraftKind::Forward, window, cx),
            A::JumpLatest => {
                self.cancel_composer_reveal();
                if let Some(index) = self
                    .selected_summary()
                    .and_then(|s| self.reader_message_index(&s.latest_message_id))
                {
                    self.reader_scroll.scroll_to_top_of_item(index);
                }
            }
            A::PreviousConversation => self.move_selection(-1, window, cx),
            A::NextConversation => self.move_selection(1, window, cx),
            A::MoveFocus | A::AlwaysMoveFocus => self.move_focus(
                self.inbox_tab != FocusTab::Focused,
                action == A::AlwaysMoveFocus,
                window,
                cx,
            ),
            A::NewEvent => self.start_new_event(window, cx),
            A::Today => self.cal_today(cx),
            A::PreviousPeriod => self.cal_step(-1, cx),
            A::NextPeriod => self.cal_step(1, cx),
            A::Day => self.set_cal_view(CalView::Day, cx),
            A::Week => self.set_cal_view(CalView::Week, cx),
            A::Month => self.set_cal_view(CalView::Month, cx),
            A::Agenda => self.set_cal_view(CalView::Agenda, cx),
            A::EditEvent => {
                if let Some(id) = self.cal_selected.clone() {
                    self.request_edit_event(id, window, cx);
                }
            }
            A::DeleteEvent => {
                if let Some(id) = self.cal_selected.clone() {
                    self.request_delete_event(id, window, cx);
                }
            }
            A::NewContact => self.start_new_contact(None, window, cx),
            A::ContactMessage => {
                if let Some(who) = self
                    .contact_selected
                    .as_ref()
                    .and_then(|id| self.contacts.iter().find(|c| &c.id == id))
                    .map(|c| c.address())
                {
                    self.compose_to_contact(who, window, cx);
                }
            }
            A::ContactMail => {
                if let Some(email) = self
                    .contact_selected
                    .as_ref()
                    .and_then(|id| self.contacts.iter().find(|c| &c.id == id))
                    .map(|c| c.email.clone())
                {
                    let query = format!("from:{email}");
                    self.set_module(Module::Mail, window, cx);
                    self.search_input
                        .update(cx, |s, cx| s.set_value(query.clone(), window, cx));
                    self.set_search(query, window, cx);
                }
            }
            A::EditContact => {
                if let Some(contact) = self
                    .contact_selected
                    .as_ref()
                    .and_then(|id| self.contacts.iter().find(|c| &c.id == id))
                    .cloned()
                {
                    if contact.is_editable() {
                        self.start_edit_contact(contact.id, window, cx);
                    } else {
                        self.start_new_contact(Some(contact.address()), window, cx);
                    }
                }
            }
            A::DeleteContact => {
                if let Some(id) = self.contact_selected.clone() {
                    self.delete_contact(id, window, cx);
                }
            }
            A::Sync => self.sync_now(window, cx),
            A::MoveMail => self.open_move_palette(window, cx),
            A::Appearance | A::Layout | A::Unknown => {}
        }
        cx.notify();
    }

    fn toolbar_button(
        &self,
        action: A,
        compact: bool,
        in_more: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = *cx.palette();
        let command = self.toolbar_command(action);
        let label: SharedString = command.label.clone().into();
        let only_icon = compact
            || matches!(
                action,
                A::PreviousConversation
                    | A::NextConversation
                    | A::PreviousPeriod
                    | A::NextPeriod
                    | A::Appearance
                    | A::Layout
                    | A::JumpLatest
            );
        let only_icon = !in_more && !command.primary && only_icon;
        let tip = if command.hint.is_empty() {
            command.label.clone()
        } else {
            format!("{} ({})", command.label, command.hint)
        };
        let button = Button::new(SharedString::from(format!("toolbar-{action:?}")))
            .ghost()
            .small()
            .h(px(if in_more { 34. } else { HEADER_CONTROL_HEIGHT }))
            .px(px(8.))
            .flex_none()
            .accessibility_label(label.clone())
            .tooltip(format!("{tip} · Drag to rearrange"))
            .disabled(!command.enabled && !self.toolbar_editing)
            .when(!command.enabled && !self.toolbar_editing, |b| {
                b.opacity(0.45)
            })
            .toggled(command.selected)
            .when(command.selected, |b| {
                b.bg(p.selection).text_color(p.selection_text)
            })
            .when(command.primary && !in_more, |b| {
                if action == A::NewMessage && p.resolved == ResolvedTheme::Alexander {
                    b.custom(ButtonCustomVariant::new(cx)
                        .color(rgb(0x3E687E).into())
                        .foreground(p.on_action)
                        .hover(rgb(0x375C6F).into())
                        .active(rgb(0x2F4F60).into()))
                        // Custom variants tint the normal fill; this action stays solid.
                        .bg(rgb(0x3E687E))
                } else {
                    b.primary()
                }
            })
            .when(in_more, |b| b.w_full().justify_start())
            .child(
                h_flex()
                    .items_center()
                    .gap(px(6.))
                    .when(in_more, |row| row.w_full())
                    .child(command.glyph.render(&p, command.primary && !in_more))
                    .when(!only_icon, |row| row.child(label)),
            );
        if self.toolbar_editing {
            return button.into_any_element();
        }
        match action {
            A::Appearance => self.appearance_menu(button, cx).into_any_element(),
            A::Layout => self.layout_menu(button, cx).into_any_element(),
            _ => button
                .on_action(cx.listener(move |this, _: &Confirm, window, cx| {
                    cx.stop_propagation();
                    this.run_toolbar_command(action, window, cx);
                }))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.run_toolbar_command(action, window, cx)
                }))
                .into_any_element(),
        }
    }

    fn save_toolbar_layout(&mut self, layout: ToolbarLayout, cx: &mut Context<Self>) {
        self.toolbar_layouts.set(self.module.into(), layout);
        self.save_setting("toolbar.layouts", &self.toolbar_layouts);
        cx.notify();
    }

    fn drop_toolbar_action(
        &mut self,
        drag: &ToolbarDrag,
        area: Area,
        before: Option<A>,
        cx: &mut Context<Self>,
    ) {
        if drag.module != M::from(self.module) {
            return;
        }
        let mut layout = self.toolbar_layouts.layout(drag.module);
        if layout.move_action(drag.action, area, before) {
            self.save_toolbar_layout(layout, cx);
        }
        // Keep the editor open for multiple edits; dismiss a regular overflow after a drop.
        if !self.toolbar_editing {
            self.toolbar_more_open = false;
            cx.notify();
        }
    }

    fn draggable_command(
        &self,
        action: A,
        area: Area,
        child: AnyElement,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let p = *cx.palette();
        let module = M::from(self.module);
        let command = self.toolbar_command(action);
        let drag = ToolbarDrag {
            module,
            action,
            label: command.label,
            glyph: command.glyph,
        };
        div()
            .id(SharedString::from(format!(
                "toolbar-slot-{area:?}-{action:?}"
            )))
            .flex_none()
            .border_l_2()
            .border_color(gpui_kit::transparent_black())
            .when(area == Area::More, |row| row.w_full())
            .child(
                h_flex()
                    .items_center()
                    .group("toolbar-command")
                    .when(area == Area::Strip, |row| {
                        row.child(
                            div()
                                .w(px(10.))
                                .h(px(30.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .opacity(0.)
                                .group_hover("toolbar-command", |style| style.opacity(0.75))
                                .child(icon("grip-vertical").size(px(10.)).text_color(p.muted)),
                        )
                    })
                    .child(child),
            )
            .can_drop(move |value, _, _| {
                value
                    .downcast_ref::<ToolbarDrag>()
                    .is_some_and(|d| d.module == module)
            })
            .drag_over::<ToolbarDrag>(move |style, _, _, _| {
                style.border_color(p.action).bg(p.hover)
            })
            .on_drag(drag, |d, _, _, cx| cx.new(|_| d.clone()))
            .on_drop(cx.listener(move |this, d: &ToolbarDrag, _, cx| {
                this.drop_toolbar_action(d, area, Some(action), cx)
            }))
    }

    pub(super) fn render_app_toolbar(
        &self,
        width: Pixels,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let p = *cx.palette();
        let compact = width < px(1160.);
        let layout = self.toolbar_layouts.layout(self.module.into());
        let visible = layout.visible_count(f32::from(width) - 90., |action| {
            let command = self.toolbar_command(action);
            if command.primary {
                command.label.len() as f32 * 7.5 + 54.
            } else if compact
                || matches!(command.glyph, Glyph::Symbol(_))
                || matches!(action, A::Appearance | A::Layout)
            {
                50.
            } else {
                command.label.len() as f32 * 7.5 + 54.
            }
        });
        let overflow = layout.strip[visible..].to_vec();
        let before = overflow.first().copied();
        let module = M::from(self.module);
        let toolbar = h_flex()
            .w_full()
            .flex_none()
            .h(px(header_height(self.density)))
            .items_center()
            .gap(px(2.))
            .px(px(8.))
            .rounded(px(8.))
            .bg(p.message_surface())
            .shadow_sm()
            .children(
                layout.strip[..visible]
                    .iter()
                    .map(|action| {
                        let button = self.toolbar_button(*action, compact, false, cx);
                        self.draggable_command(*action, Area::Strip, button, cx)
                            .into_any_element()
                    })
                    .collect::<Vec<_>>(),
            )
            .child(
                div()
                    .id("toolbar-drop-end")
                    .flex_1()
                    .min_w(px(22.))
                    .h(px(HEADER_CONTROL_HEIGHT))
                    .rounded(px(5.))
                    .can_drop(move |value, _, _| {
                        value
                            .downcast_ref::<ToolbarDrag>()
                            .is_some_and(|d| d.module == module)
                    })
                    .drag_over::<ToolbarDrag>(move |style, _, _, _| {
                        style.bg(p.selection).border_1().border_color(p.action)
                    })
                    .on_drop(cx.listener(move |this, d: &ToolbarDrag, _, cx| {
                        this.drop_toolbar_action(d, Area::Strip, before, cx)
                    }))
                    .when(layout.strip.is_empty(), |d| {
                        d.child(
                            div()
                                .text_size(px(12.))
                                .text_color(p.muted)
                                .child("Drag buttons here from More"),
                        )
                    }),
            )
            .child(self.toolbar_more(overflow, cx));

        // The outer inset replaces inner padding so commands stay aligned with the title.
        div().w_full().flex_none().px(px(4.)).pt(px(3.))
            .bg(p.navigation_palette().sidebar)
            .child(toolbar)
    }

    fn toolbar_more(&self, overflow: Vec<A>, cx: &mut Context<Self>) -> impl IntoElement {
        let p = *cx.palette();
        let module = M::from(self.module);
        let this = cx.entity().downgrade();
        let trigger = Button::new("toolbar-more-button")
            .ghost()
            .small()
            .h(px(HEADER_CONTROL_HEIGHT))
            .w(px(36.))
            .icon(icon("ellipsis").size(px(19.)))
            .accessibility_label("More actions and toolbar customization")
            .tooltip("More actions · Drop buttons here to move them into More");
        div()
            .id("toolbar-more-drop")
            .flex_none()
            .rounded(px(6.))
            .can_drop(move |value, _, _| {
                value
                    .downcast_ref::<ToolbarDrag>()
                    .is_some_and(|d| d.module == module)
            })
            .drag_over::<ToolbarDrag>(move |style, _, _, _| {
                style.bg(p.selection).border_1().border_color(p.action)
            })
            .on_drop(cx.listener(|this, d: &ToolbarDrag, _, cx| {
                this.drop_toolbar_action(d, Area::More, None, cx)
            }))
            .child(
                Popover::new("toolbar-more-popover")
                    .anchor(Anchor::TopRight)
                    .open(self.toolbar_more_open)
                    .overlay_closable(!self.toolbar_editing)
                    .on_open_change(cx.listener(|this, open: &bool, _, cx| {
                        this.toolbar_more_open = *open;
                        if !open {
                            this.toolbar_editing = false;
                        }
                        cx.notify();
                    }))
                    .trigger(trigger)
                    .content(move |_, window, cx| {
                        this.update(cx, |this, cx| {
                            this.render_toolbar_more(&overflow, window.viewport_size().height, cx)
                        })
                        .unwrap_or_else(|_| div().into_any_element())
                    }),
            )
    }

    fn render_toolbar_more(
        &self,
        overflow: &[A],
        height: Pixels,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = *cx.palette();
        let layout = self.toolbar_layouts.layout(self.module.into());
        let mut rows = Vec::new();
        let module = M::from(self.module);
        if self.toolbar_editing {
            for (area, title) in [(Area::Strip, "On the toolbar"), (Area::More, "In More")] {
                rows.push(
                    div()
                        .id(SharedString::from(format!("toolbar-area-{area:?}")))
                        .pt(px(10.))
                        .pb(px(5.))
                        .px(px(4.))
                        .rounded(px(4.))
                        .can_drop(move |value, _, _| {
                            value
                                .downcast_ref::<ToolbarDrag>()
                                .is_some_and(|d| d.module == module)
                        })
                        .drag_over::<ToolbarDrag>(move |style, _, _, _| style.bg(p.selection))
                        .on_drop(cx.listener(move |this, d: &ToolbarDrag, _, cx| {
                            this.drop_toolbar_action(d, area, None, cx)
                        }))
                        .text_size(px(11.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(p.muted)
                        .child(title)
                        .into_any_element(),
                );
                if layout.actions(area).is_empty() {
                    rows.push(
                        div()
                            .p(px(8.))
                            .text_size(px(12.))
                            .text_color(p.muted)
                            .child("No buttons here")
                            .into_any_element(),
                    );
                }
                for (index, action) in layout.actions(area).iter().enumerate() {
                    rows.push(self.toolbar_editor_row(
                        *action,
                        area,
                        index,
                        layout.actions(area).len(),
                        cx,
                    ));
                }
            }
        } else {
            if !overflow.is_empty() {
                rows.push(
                    div()
                        .pb(px(6.))
                        .text_size(px(11.))
                        .text_color(p.muted)
                        .child("More room needed — your order is saved")
                        .into_any_element(),
                );
            }
            let actions: Vec<_> = overflow.iter().chain(&layout.more).copied().collect();
            if actions.is_empty() {
                rows.push(
                    div()
                        .p(px(8.))
                        .text_size(px(12.))
                        .text_color(p.muted)
                        .child("All buttons are on the toolbar")
                        .into_any_element(),
                );
            }
            for action in actions {
                let button = self.toolbar_button(action, false, true, cx);
                let row = h_flex()
                    .w_full()
                    .items_center()
                    .child(div().flex_1().min_w_0().child(button))
                    .child(icon("grip-vertical").size(px(14.)).text_color(p.muted))
                    .into_any_element();
                rows.push(
                    self.draggable_command(action, Area::More, row, cx)
                        .into_any_element(),
                );
            }
        }
        v_flex()
            .w(px(350.))
            .gap(px(6.))
            .child(
                h_flex()
                    .items_center()
                    .gap(px(8.))
                    .child(div().flex_1().font_weight(FontWeight::SEMIBOLD).child(
                        if self.toolbar_editing {
                            "Customize toolbar"
                        } else {
                            "More actions"
                        },
                    ))
                    .when(self.toolbar_editing, |row| {
                        row.child(
                            self.bind_toolbar_edit(
                                Button::new("toolbar-customize-done")
                                    .ghost()
                                    .small()
                                    .label("Done"),
                                ToolbarEdit::Done,
                                cx,
                            ),
                        )
                    }),
            )
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(p.muted)
                    .w_full()
                    .whitespace_normal()
                    .child("Drag buttons to reorder, or between the toolbar and More."),
            )
            .child(
                v_flex()
                    .id("toolbar-more-scroll")
                    .gap(px(2.))
                    .max_h((height - px(260.)).clamp(px(120.), px(470.)))
                    .overflow_y_scrollbar()
                    .children(rows),
            )
            .child(divider(&p))
            .child(
                h_flex()
                    .items_center()
                    .justify_between()
                    .child(
                        self.bind_toolbar_edit(
                            Button::new("toolbar-customize")
                                .ghost()
                                .small()
                                .icon(icon("sliders-horizontal").size(px(15.)))
                                .label(if self.toolbar_editing {
                                    "Reset this toolbar"
                                } else {
                                    "Customize toolbar"
                                }),
                            if self.toolbar_editing {
                                ToolbarEdit::Reset
                            } else {
                                ToolbarEdit::Customize
                            },
                            cx,
                        ),
                    )
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(p.muted)
                            .child("Changes save automatically"),
                    ),
            )
            .into_any_element()
    }

    fn apply_toolbar_edit(
        &mut self,
        edit: ToolbarEdit,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match edit {
            ToolbarEdit::Customize => self.toolbar_editing = true,
            ToolbarEdit::Done => {
                self.toolbar_editing = false;
                self.toolbar_more_open = false;
                self.focus_current_module(window, cx);
            }
            ToolbarEdit::Reset => {
                self.toolbar_layouts.reset(self.module.into());
                self.save_setting("toolbar.layouts", &self.toolbar_layouts);
            }
            ToolbarEdit::Nudge(action, area, forward) => {
                let mut layout = self.toolbar_layouts.layout(self.module.into());
                if layout.nudge(action, area, forward) {
                    self.save_toolbar_layout(layout, cx);
                }
            }
            ToolbarEdit::Move(action, area) => {
                let mut layout = self.toolbar_layouts.layout(self.module.into());
                if layout.move_action(action, area, None) {
                    self.save_toolbar_layout(layout, cx);
                }
            }
        }
        cx.notify();
    }

    fn bind_toolbar_edit(
        &self,
        button: Button,
        edit: ToolbarEdit,
        cx: &mut Context<Self>,
    ) -> Button {
        button
            .on_click(
                cx.listener(move |this, _, window, cx| this.apply_toolbar_edit(edit, window, cx)),
            )
            // Popover binds Enter to Confirm; consume it here instead of toggling its trigger.
            .on_action(cx.listener(move |this, _: &Confirm, window, cx| {
                cx.stop_propagation();
                this.apply_toolbar_edit(edit, window, cx);
            }))
    }

    fn toolbar_editor_row(
        &self,
        action: A,
        area: Area,
        index: usize,
        count: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = *cx.palette();
        let command = self.toolbar_command(action);
        let mut row = h_flex()
            .w_full()
            .h(px(38.))
            .items_center()
            .gap(px(6.))
            .px(px(4.))
            .child(icon("grip-vertical").size(px(12.)).text_color(p.muted))
            .child(command.glyph.render(&p, false))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(px(12.))
                    .child(command.label.clone()),
            );
        for forward in [false, true] {
            let label = format!(
                "Move {} {}",
                command.label,
                if forward { "later" } else { "earlier" }
            );
            let button = Button::new(SharedString::from(format!(
                "toolbar-nudge-{area:?}-{action:?}-{forward}"
            )))
            .ghost()
            .xsmall()
            .w(px(24.))
            .icon(
                icon(if forward {
                    "chevron-down"
                } else {
                    "chevron-up"
                })
                .size(px(14.)),
            )
            .accessibility_label(label.clone())
            .tooltip(label)
            .disabled(if forward {
                index + 1 == count
            } else {
                index == 0
            });
            row = row.child(self.bind_toolbar_edit(
                button,
                ToolbarEdit::Nudge(action, area, forward),
                cx,
            ));
        }
        let to_strip = area == Area::More;
        let label = format!(
            "Move {} to {}",
            command.label,
            if to_strip { "toolbar" } else { "More" }
        );
        let button = Button::new(SharedString::from(format!(
            "toolbar-toggle-{area:?}-{action:?}"
        )))
        .ghost()
        .xsmall()
        .w(px(24.))
        .icon(icon(if to_strip { "plus" } else { "minus" }).size(px(14.)))
        .accessibility_label(label.clone())
        .tooltip(label);
        row = row.child(self.bind_toolbar_edit(
            button,
            ToolbarEdit::Move(action, if to_strip { Area::Strip } else { Area::More }),
            cx,
        ));
        self.draggable_command(action, area, row.into_any_element(), cx)
            .into_any_element()
    }
}
