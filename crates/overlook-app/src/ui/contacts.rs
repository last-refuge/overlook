//! Contacts module: the address book beside Mail and Calendar. A list with search and a
//! detail pane; address-book entries can be added, edited and deleted, and every change is
//! saved on this device first and pushed through the operation log like mail and events.

use super::widgets::*;
use super::MailWindow;
use crate::assets::icon;
use crate::theme::ActivePalette;
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputState, Textarea, TextareaState};
use gpui_kit::component::{Sizable};
use gpui_kit::prelude::*;
use gpui_kit::*;
use overlook_core::time::now;
use overlook_core::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContactFilter {
    All,
    AddressBook,
    Other,
}

pub struct ContactForm {
    pub given: Entity<InputState>,
    pub family: Entity<InputState>,
    pub email: Entity<InputState>,
    pub phone: Entity<InputState>,
    pub organization: Entity<InputState>,
    pub title: Entity<InputState>,
    pub notes: Entity<TextareaState>,
    /// `Some` when editing an existing contact.
    pub editing: Option<String>,
    pub error: Option<String>,
}

impl MailWindow {
    pub(crate) fn reload_contacts(&mut self) {
        self.contacts = self.store.contacts(&self.account.id).unwrap_or_default();
        self.contact_index = super::contact_card::index_contacts(&self.contacts);
        if let Some(sel) = &self.contact_selected {
            if !self.contacts.iter().any(|c| &c.id == sel) {
                self.contact_selected = None;
            }
        }
    }

    fn visible_contacts(&self) -> Vec<&Contact> {
        let q = self.contact_query.trim().to_lowercase();
        self.contacts
            .iter()
            .filter(|c| match self.contact_filter {
                ContactFilter::All => true,
                ContactFilter::AddressBook => c.in_address_book,
                ContactFilter::Other => !c.in_address_book,
            })
            .filter(|c| {
                q.is_empty()
                    || c.display_name().to_lowercase().contains(&q)
                    || c.emails.iter().any(|e| e.to_lowercase().contains(&q))
                    || c.email.to_lowercase().contains(&q)
                    || c.organization.as_deref().is_some_and(|o| o.to_lowercase().contains(&q))
            })
            .collect()
    }

    pub(crate) fn set_contact_filter(&mut self, filter: ContactFilter, cx: &mut Context<Self>) {
        self.contact_filter = filter;
        cx.notify();
    }

    fn contacts_writable(&self) -> bool {
        self.providers.get(&self.account.id).is_some_and(|p| p.mail.has_contacts_write_access())
    }

    pub(crate) fn start_new_contact(&mut self, prefill: Option<Address>, window: &mut Window, cx: &mut Context<Self>) {
        let (given, family) = prefill
            .as_ref()
            .and_then(|a| a.name.clone())
            .map(|n| {
                let mut parts = n.trim().splitn(2, ' ');
                (parts.next().unwrap_or("").to_owned(), parts.next().unwrap_or("").to_owned())
            })
            .unwrap_or_default();
        let email = prefill.map(|a| a.email).unwrap_or_default();
        self.contact_form = Some(self.build_contact_form(given, family, email, String::new(), String::new(), String::new(), String::new(), None, window, cx));
        self.contact_selected = None;
        cx.notify();
    }

    pub(crate) fn start_edit_contact(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(c) = self.contacts.iter().find(|c| c.id == id).cloned() else { return };
        let (given, family) = match (c.given_name.clone(), c.family_name.clone()) {
            (None, None) => {
                let n = c.name.clone().unwrap_or_default();
                let mut parts = n.trim().splitn(2, ' ');
                (parts.next().unwrap_or("").to_owned(), parts.next().unwrap_or("").to_owned())
            }
            (g, f) => (g.unwrap_or_default(), f.unwrap_or_default()),
        };
        self.contact_form = Some(self.build_contact_form(
            given,
            family,
            c.email.clone(),
            c.phone.clone().unwrap_or_default(),
            c.organization.clone().unwrap_or_default(),
            c.title.clone().unwrap_or_default(),
            c.notes.clone().unwrap_or_default(),
            Some(id),
            window,
            cx,
        ));
        cx.notify();
    }

    #[allow(clippy::too_many_arguments)]
    fn build_contact_form(&self, given: String, family: String, email: String, phone: String, organization: String, title: String, notes: String, editing: Option<String>, window: &mut Window, cx: &mut Context<Self>) -> ContactForm {
        let mk = |placeholder: &'static str, value: String, window: &mut Window, cx: &mut Context<Self>| cx.new(|cx| InputState::new(window, cx).placeholder(placeholder).default_value(value));
        let given_input = mk("First name", given, window, cx);
        let form = ContactForm {
            family: mk("Last name", family, window, cx),
            email: mk("Email", email, window, cx),
            phone: mk("Phone", phone, window, cx),
            organization: mk("Company", organization, window, cx),
            title: mk("Job title", title, window, cx),
            notes: cx.new(|cx| TextareaState::new(window, cx).placeholder("Notes").default_value(notes)),
            given: given_input.clone(),
            editing,
            error: None,
        };
        given_input.update(cx, |s, cx| s.focus(window, cx));
        form
    }

    pub(crate) fn cancel_contact_form(&mut self, cx: &mut Context<Self>) {
        self.contact_form = None;
        cx.notify();
    }

    pub(crate) fn save_contact_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(form) = self.contact_form.as_ref() else { return };
        let read = |s: &Entity<InputState>| s.read(cx).value().trim().to_owned();
        let (given, family, email, phone, organization, title) = (read(&form.given), read(&form.family), read(&form.email), read(&form.phone), read(&form.organization), read(&form.title));
        let notes = form.notes.read(cx).value().trim().to_owned();
        let editing = form.editing.clone();
        if given.is_empty() && family.is_empty() && email.is_empty() {
            if let Some(f) = self.contact_form.as_mut() {
                f.error = Some("Give the contact a name or an email address.".into());
            }
            cx.notify();
            return;
        }
        if !email.is_empty() && !(Address { name: None, email: email.clone() }).looks_valid() {
            if let Some(f) = self.contact_form.as_mut() {
                f.error = Some("That email address doesn’t look right.".into());
            }
            cx.notify();
            return;
        }
        let mut contact = match editing.as_deref().and_then(|id| self.contacts.iter().find(|c| &c.id == id).cloned()) {
            Some(c) => c,
            None => Contact::new_local(self.account.id.clone(), now()),
        };
        let opt = |s: String| (!s.is_empty()).then_some(s);
        contact.given_name = opt(given.clone());
        contact.family_name = opt(family.clone());
        contact.name = opt(format!("{given} {family}").trim().to_owned());
        if !email.is_empty() {
            let lower = email.to_ascii_lowercase();
            contact.emails.retain(|e| !e.eq_ignore_ascii_case(&contact.email));
            contact.emails.insert(0, lower.clone());
            contact.email = lower;
        } else {
            contact.email = String::new();
            contact.emails.clear();
        }
        contact.phone = opt(phone);
        contact.organization = opt(organization);
        contact.title = opt(title);
        contact.notes = opt(notes);
        contact.in_address_book = true;
        match self.store.save_contact_locally(&self.account.id, &contact) {
            Ok(_) => {
                self.contact_form = None;
                self.reload_contacts();
                self.contact_selected = Some(contact.id.clone());
                crate::ui::toast(window, cx, crate::notify::Level::Info, if self.contacts_writable() { "Saved on this device. Syncing to Google Contacts…" } else { "Saved on this device. Sign in again to sync it to Google Contacts." });
                if self.connected(&self.account.id) {
                    self.sync_now(window, cx);
                }
                cx.notify();
            }
            Err(e) => {
                if let Some(f) = self.contact_form.as_mut() {
                    f.error = Some(format!("Couldn’t save: {e}"));
                }
                cx.notify();
            }
        }
    }

    pub(crate) fn delete_contact(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(c) = self.contacts.iter().find(|c| c.id == id).cloned() else { return };
        match self.store.delete_contact_locally(&self.account.id, &c) {
            Ok(_) => {
                self.contact_selected = None;
                self.contact_form = None;
                self.reload_contacts();
                crate::ui::toast(window, cx, crate::notify::Level::Info, format!("Deleted {}.", c.display_name()));
                if self.connected(&self.account.id) {
                    self.sync_now(window, cx);
                }
                cx.notify();
            }
            Err(e) => crate::ui::toast(window, cx, crate::notify::Level::Error, format!("Couldn’t delete the contact: {e}")),
        }
    }

    /// Opens the Contacts module on this person, or on a new contact for them.
    pub(crate) fn show_in_contacts(&mut self, who: Address, window: &mut Window, cx: &mut Context<Self>) {
        self.set_module(super::calendar::Module::Contacts, window, cx);
        self.reload_contacts();
        match self.contacts.iter().find(|c| c.has_email(&who.email) && c.in_address_book).map(|c| c.id.clone()) {
            Some(id) => {
                self.contact_form = None;
                self.contact_selected = Some(id);
                cx.notify();
            }
            None => self.start_new_contact(Some(who), window, cx),
        }
    }

    pub(super) fn compose_to_contact(&mut self, who: Address, window: &mut Window, cx: &mut Context<Self>) {
        self.set_module(super::calendar::Module::Mail, window, cx);
        self.compose_new(window, cx);
        if let Some(c) = self.composer.as_ref() {
            let to = c.to.clone();
            let header = who.to_header();
            to.update(cx, |s, cx| s.set_value(header, window, cx));
        }
        self.sync_composer_fields(cx);
    }

    // --------------------------------------------------------------- render

    pub(crate) fn render_contacts_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = cx.palette().navigation_palette();
        let total = self.contacts.len();
        let book = self.contacts.iter().filter(|c| c.in_address_book).count();
        let other = total - book;
        let writable = self.contacts_writable();
        let item = |label: &'static str, count: usize, filter: ContactFilter, icon_name: &'static str, cx: &mut Context<Self>| {
            let active = self.contact_filter == filter;
            h_flex()
                .id(SharedString::from(format!("cfilter-{label}")))
                .items_center()
                .gap(px(10.))
                .min_h(px(32.))
                .px(px(11.))
                .rounded(px(7.))
                .cursor_pointer()
                .text_size(px(13.))
                .text_color(if active { p.selection_text } else { p.foreground })
                .when(active, |d| d.bg(p.selection).font_weight(FontWeight::SEMIBOLD))
                .when(!active, |d| d.hover(|d| d.bg(p.hover)))
                .child(icon(icon_name).size(px(15.)).text_color(if active { p.accent } else { p.muted }))
                .child(div().flex_1().child(label))
                .child(div().text_size(px(11.)).text_color(if active { p.selection_text } else { p.muted }).child(count.to_string()))
                .on_click(cx.listener(move |this, _, _w, cx| this.set_contact_filter(filter, cx)))
        };
        v_flex()
            .size_full()
            .bg(p.sidebar)
            .text_color(p.foreground)
            .p(px(10.))
            .gap(px(12.))
            .child(
                Button::new("new-contact")
                    .primary()
                    .w_full()
                    .icon(icon("user-plus").size(px(15.)))
                    .label("New contact")
                    .on_click(cx.listener(|this, _, window, cx| this.start_new_contact(None, window, cx))),
            )
            .child(v_flex().gap(px(2.)).child(item("All", total, ContactFilter::All, "users", cx)).child(item("Google Contacts", book, ContactFilter::AddressBook, "book-user", cx)).child(item("Other contacts", other, ContactFilter::Other, "user-round", cx)))
            .child(div().flex_1())
            .when(!writable, |d| {
                let email = self.account.email.clone();
                d.child(
                    v_flex()
                        .gap(px(6.))
                        .p(px(10.))
                        .rounded(px(8.))
                        .bg(p.chip)
                        .text_size(px(11.))
                        .text_color(cx.palette().muted)
                        .child("Changes stay on this device until Overlook may edit your Google Contacts.")
                        .child(Button::new("contacts-write-signin").xsmall().label("Sign in again").on_click(cx.listener(move |this, _, window, cx| this.start_google_sign_in(Some(email.clone()), window, cx)))),
                )
            })
    }

    pub(crate) fn render_contacts_main(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = *cx.palette();
        let visible = self.visible_contacts();
        let selected = self.contact_selected.clone();
        let list = v_flex()
            .w(px(340.))
            .flex_none()
            .h_full()
            .border_r_1()
            .border_color(p.divider)
            .child(
                div()
                    .p(px(12.))
                    .border_b_1()
                    .border_color(p.divider)
                    .child(Input::new(&self.contact_search).prefix(icon("search").size(px(15.)).text_color(p.muted)).cleanable(true)),
            )
            .child(
                div()
                    .id("contact-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(if visible.is_empty() {
                        div()
                            .p(px(20.))
                            .text_size(px(13.))
                            .text_color(p.muted)
                            .child(if self.contacts.is_empty() { "No contacts yet. They arrive with the next sync, or add one." } else { "No contacts match." })
                            .into_any_element()
                    } else {
                        v_flex()
                            .children(visible.iter().map(|c| {
                                let id = c.id.clone();
                                let active = selected.as_deref() == Some(&c.id);
                                h_flex()
                                    .id(SharedString::from(format!("contact-{}", c.id)))
                                    .items_center()
                                    .gap(px(12.))
                                    .px(px(14.))
                                    .py(px(9.))
                                    .cursor_pointer()
                                    .border_b_1()
                                    .border_color(p.divider)
                                    .when(active, |d| d.bg(p.selection))
                                    .when(!active, |d| d.hover(|d| d.bg(p.hover)))
                                    .child(avatar(&c.address(), 32., &p))
                                    .child(
                                        v_flex()
                                            .flex_1()
                                            .min_w_0()
                                            .child(div().truncate().text_size(px(13.)).font_weight(FontWeight::MEDIUM).child(c.display_name()))
                                            .child(div().truncate().text_size(px(11.)).text_color(p.muted).child(if c.email.is_empty() { c.organization.clone().unwrap_or_default() } else { c.email.clone() })),
                                    )
                                    .when(c.pending, |d| d.child(icon("cloud-off").size(px(13.)).text_color(p.warning)))
                                    .when(!c.in_address_book, |d| d.child(div().text_size(px(10.)).text_color(p.muted).child("other")))
                                    .on_click(cx.listener(move |this, _, _w, cx| {
                                        this.contact_form = None;
                                        this.contact_selected = Some(id.clone());
                                        cx.notify();
                                    }))
                            }))
                            .into_any_element()
                    }),
            );
        let detail: AnyElement = if self.contact_form.is_some() {
            self.render_contact_form(cx).into_any_element()
        } else if let Some(c) = selected.as_deref().and_then(|id| self.contacts.iter().find(|c| c.id == id)) {
            self.render_contact_detail(c, cx).into_any_element()
        } else {
            v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap(px(8.))
                .child(icon("book-user").size(px(40.)).text_color(p.divider))
                .child(div().text_size(px(14.)).text_color(p.muted).child("Select a contact, or add one."))
                .into_any_element()
        };
        h_flex().size_full().bg(p.surface).child(list).child(div().flex_1().min_w_0().h_full().child(detail))
    }

    fn render_contact_detail(&self, c: &Contact, cx: &mut Context<Self>) -> impl IntoElement {
        let p = *cx.palette();
        let who = c.address();
        let compose = who.clone();
        let edit_id = c.id.clone();
        let del_id = c.id.clone();
        let search_from = (!c.email.is_empty()).then(|| format!("from:{}", c.email));
        let row = |icon_name: &'static str, text: String| {
            h_flex().items_center().gap(px(10.)).text_size(px(13.)).child(icon(icon_name).size(px(15.)).text_color(p.muted)).child(div().child(text))
        };
        v_flex()
            .id("contact-detail")
            .size_full()
            .overflow_y_scroll()
            .p(px(28.))
            .gap(px(18.))
            .child(
                h_flex()
                    .items_center()
                    .gap(px(16.))
                    .child(match c.photo_url.clone() {
                        Some(url) => div().size(px(64.)).rounded_full().overflow_hidden().bg(p.avatar).child(img(SharedUri::from(url)).size_full().object_fit(ObjectFit::Cover)).into_any_element(),
                        None => avatar(&who, 64., &p).into_any_element(),
                    })
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_size(px(22.)).font_weight(FontWeight::SEMIBOLD).child(c.display_name()))
                            .children(c.title.clone().or_else(|| c.organization.clone()).map(|t| div().text_size(px(13.)).text_color(p.muted).child(match (&c.title, &c.organization) {
                                (Some(t), Some(o)) => format!("{t} · {o}"),
                                _ => t,
                            })))
                            .child(div().text_size(px(11.)).text_color(p.muted).child(if c.pending { "Saved on this device, waiting to sync" } else if c.in_address_book { "In your Google Contacts" } else { "Someone you have corresponded with" })),
                    ),
            )
            .child(
                h_flex()
                    .gap(px(6.))
                    .when(!c.email.is_empty(), |r| r.child(Button::new("contact-compose").small().icon(icon("pen-line").size(px(14.))).label("New message").on_click(cx.listener(move |this, _, window, cx| this.compose_to_contact(compose.clone(), window, cx)))))
                    .when_some(search_from, |r, q| {
                        r.child(Button::new("contact-mail").ghost().small().icon(icon("search").size(px(14.))).label("Mail from them").on_click(cx.listener(move |this, _, window, cx| {
                            this.set_module(super::calendar::Module::Mail, window, cx);
                            let input = this.search_input.clone();
                            let q = q.clone();
                            input.update(cx, |s, cx| s.set_value(q.clone(), window, cx));
                            this.set_search(q, window, cx);
                        })))
                    })
                    .when(c.is_editable(), |r| {
                        r.child(Button::new("contact-edit").ghost().small().icon(icon("pencil").size(px(14.))).label("Edit").on_click(cx.listener(move |this, _, window, cx| this.start_edit_contact(edit_id.clone(), window, cx))))
                            .child(Button::new("contact-delete").ghost().small().icon(icon("trash").size(px(14.)).text_color(p.error)).label("Delete").on_click(cx.listener(move |this, _, window, cx| this.delete_contact(del_id.clone(), window, cx))))
                    })
                    .when(!c.is_editable(), |r| {
                        let who = who.clone();
                        r.child(Button::new("contact-add").ghost().small().icon(icon("user-plus").size(px(14.))).label("Add to Google Contacts").on_click(cx.listener(move |this, _, window, cx| this.start_new_contact(Some(who.clone()), window, cx))))
                    }),
            )
            .child(divider(&p))
            .child(
                v_flex()
                    .gap(px(10.))
                    .children(c.emails.iter().chain(std::iter::once(&c.email)).fold(Vec::<String>::new(), |mut acc, e| { if !e.is_empty() && !acc.iter().any(|x| x.eq_ignore_ascii_case(e)) { acc.push(e.clone()); } acc }).into_iter().map(|e| row("mail", e)))
                    .children(c.phone.clone().map(|ph| row("phone", ph)))
                    .children(c.organization.clone().map(|o| row("building-2", o)))
                    .children(c.title.clone().map(|t| row("briefcase", t)))
                    .children(c.notes.clone().map(|n| row("notebook-tabs", n))),
            )
    }

    fn render_contact_form(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = *cx.palette();
        let Some(form) = self.contact_form.as_ref() else { return div().into_any_element() };
        let field = |label: &'static str, input: AnyElement| v_flex().gap(px(4.)).child(div().text_size(px(11.)).font_weight(FontWeight::MEDIUM).text_color(p.muted).child(label)).child(input);
        v_flex()
            .id("contact-form")
            .size_full()
            .overflow_y_scroll()
            .p(px(28.))
            .gap(px(12.))
            .max_w(px(560.))
            .child(div().text_size(px(18.)).font_weight(FontWeight::SEMIBOLD).child(if form.editing.is_some() { "Edit contact" } else { "New contact" }))
            .child(h_flex().gap(px(10.)).child(div().flex_1().child(field("First name", Input::new(&form.given).into_any_element()))).child(div().flex_1().child(field("Last name", Input::new(&form.family).into_any_element()))))
            .child(field("Email", Input::new(&form.email).into_any_element()))
            .child(field("Phone", Input::new(&form.phone).into_any_element()))
            .child(h_flex().gap(px(10.)).child(div().flex_1().child(field("Company", Input::new(&form.organization).into_any_element()))).child(div().flex_1().child(field("Job title", Input::new(&form.title).into_any_element()))))
            .child(field("Notes", div().h(px(100.)).child(Textarea::new(&form.notes).h_full()).into_any_element()))
            .children(form.error.clone().map(|e| h_flex().gap(px(6.)).items_center().text_size(px(12.)).text_color(p.error).child(icon("circle-alert").size(px(14.)).text_color(p.error)).child(e)))
            .child(
                h_flex()
                    .gap(px(8.))
                    .child(Button::new("contact-save").primary().label("Save").on_click(cx.listener(|this, _, window, cx| this.save_contact_form(window, cx))))
                    .child(Button::new("contact-cancel").ghost().label("Cancel").on_click(cx.listener(|this, _, _w, cx| this.cancel_contact_form(cx)))),
            )
            .into_any_element()
    }
}
