//! Contact cards: who a name in a message header is, and what Overlook knows about them.
//! Hovering a name in From, To or Cc shows the card after a short delay; clicking the name
//! pins the same card under the header until it is clicked again.

use super::widgets::*;
use super::MailWindow;
use crate::assets::icon;
use crate::theme::{ActivePalette, Palette};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::hover_card::HoverCard;
use gpui_kit::component::Sizable;
use gpui_kit::prelude::*;
use gpui_kit::*;
use gpui_kit::base::{h_flex, v_flex};
use overlook_core::*;
use std::collections::HashMap;
use std::time::Duration;

/// How many recipients a message header lists before "+N more".
const RECIPIENTS_SHOWN: usize = 5;

/// How many of `total` recipients to list. One name more than the limit is listed rather
/// than replaced by "+1 more", which would take the same room.
fn visible_recipients(total: usize, expanded: bool) -> usize {
    if expanded || total <= RECIPIENTS_SHOWN + 1 {
        total
    } else {
        RECIPIENTS_SHOWN
    }
}

/// Lower-cased address → position in `contacts`, for every address a contact has. A person
/// in the address book wins over one only inferred from correspondence, like
/// `Store::contact`.
pub(super) fn index_contacts(contacts: &[Contact]) -> HashMap<String, usize> {
    let mut index: HashMap<String, usize> = HashMap::new();
    for (i, c) in contacts.iter().enumerate() {
        for email in std::iter::once(&c.email).chain(&c.emails).filter(|e| !e.trim().is_empty()) {
            let key = email.trim().to_ascii_lowercase();
            match index.get(&key) {
                Some(&kept) if contacts[kept].in_address_book || !c.in_address_book => {}
                _ => {
                    index.insert(key, i);
                }
            }
        }
    }
    index
}

/// "Title · Organization", whichever of the two are known.
fn role_line(c: &Contact) -> Option<String> {
    let parts: Vec<&str> = [c.title.as_deref(), c.organization.as_deref()].into_iter().flatten().map(str::trim).filter(|s| !s.is_empty()).collect();
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// The start of a contact's notes: three lines at most and about 200 characters, with an
/// ellipsis where it was cut.
fn notes_excerpt(notes: &str) -> Option<String> {
    const LINES: usize = 3;
    const CHARS: usize = 200;
    let lines: Vec<&str> = notes.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    if lines.is_empty() {
        return None;
    }
    let mut text = lines.iter().take(LINES).copied().collect::<Vec<_>>().join("\n");
    let mut cut = lines.len() > LINES;
    if text.chars().count() > CHARS {
        text = text.chars().take(CHARS).collect::<String>().trim_end().to_owned();
        cut = true;
    }
    if cut {
        text.push('…');
    }
    Some(text)
}

/// Everything a card shows, copied out so a hover card can keep it.
#[derive(Clone)]
struct CardData {
    who: Address,
    contact: Option<Contact>,
    is_me: bool,
}

impl CardData {
    fn name(&self) -> String {
        self.contact
            .as_ref()
            .and_then(Contact::known_name)
            .or_else(|| self.who.name.clone().filter(|n| !n.trim().is_empty()))
            .unwrap_or_else(|| self.who.email.clone())
    }

    fn source(&self) -> &'static str {
        match &self.contact {
            _ if self.is_me => "This is you",
            Some(c) if c.in_address_book => "In your Google Contacts",
            Some(_) => "Someone you have corresponded with",
            None => "Not in your contacts",
        }
    }

    /// The contact's other addresses, without the one the card was opened for.
    fn other_emails(&self) -> Vec<String> {
        let Some(c) = &self.contact else { return Vec::new() };
        let mut seen = vec![self.who.email.to_ascii_lowercase()];
        std::iter::once(&c.email)
            .chain(&c.emails)
            .filter(|e| !e.trim().is_empty())
            .filter(|e| {
                let key = e.trim().to_ascii_lowercase();
                let new = !seen.contains(&key);
                seen.push(key);
                new
            })
            .cloned()
            .collect()
    }
}

/// The card itself, without a frame: the hover card and the pinned card each supply their
/// own. `scope` keeps the button ids of cards on screen together apart, and the actions
/// reach the window through `t` because a hover card is not drawn by the window's own view.
fn card_body(data: &CardData, p: &Palette, t: &WeakEntity<MailWindow>, scope: &str) -> Div {
    let who = &data.who;
    let contact = data.contact.as_ref();
    let id = |part: &str| SharedString::from(format!("{scope}-{part}"));
    let photo = match contact.and_then(|c| c.photo_url.clone()) {
        Some(url) => div().flex_none().size(px(44.)).rounded_full().overflow_hidden().bg(p.avatar).child(img(SharedUri::from(url)).size_full().object_fit(ObjectFit::Cover)).into_any_element(),
        None if data.is_me => owner_avatar(who, 44., p).into_any_element(),
        None => avatar(who, 44., p).into_any_element(),
    };
    let row = |name: &'static str, content: AnyElement| {
        h_flex().items_start().gap(px(8.)).text_size(px(12.)).child(icon(name).size(px(13.)).text_color(p.muted).mt(px(2.))).child(div().flex_1().min_w_0().child(content))
    };

    let email = who.email.clone();
    let copy = Button::new(id("copy")).ghost().xsmall().icon(icon("copy").size(px(12.))).tooltip("Copy address").on_click(move |_, window, cx| {
        cx.write_to_clipboard(ClipboardItem::new_string(email.clone()));
        crate::ui::toast(window, cx, crate::notify::Level::Info, "Address copied.");
    });
    let mut details = v_flex().gap(px(6.)).child(
        h_flex().items_center().gap(px(4.)).child(row("mail", div().truncate().child(who.email.clone()).into_any_element()).flex_1().min_w_0()).child(copy),
    );
    for other in data.other_emails() {
        details = details.child(row("mail", div().truncate().text_color(p.muted).child(other).into_any_element()));
    }
    if let Some(phone) = contact.and_then(|c| c.phone.as_deref()).filter(|s| !s.trim().is_empty()) {
        details = details.child(row("phone", div().child(phone.to_owned()).into_any_element()));
    }
    if let Some(notes) = contact.and_then(|c| c.notes.as_deref()).and_then(notes_excerpt) {
        details = details.child(row("sticky-note", div().text_color(p.muted).child(notes).into_any_element()));
    }

    let in_book = contact.is_some_and(|c| c.in_address_book);
    let (write_to, write_t) = (who.clone(), t.clone());
    let (book_who, book_t) = (who.clone(), t.clone());
    let (search_email, search_t) = (who.email.clone(), t.clone());
    let actions = h_flex()
        .flex_wrap()
        .gap(px(6.))
        .child(Button::new(id("write")).small().icon(icon("pen-line").size(px(14.))).label("New message").on_click(move |_, window, cx| {
            let _ = write_t.update(cx, |this, cx| this.compose_to_contact(write_to.clone(), window, cx));
        }))
        .child(
            Button::new(id("book")).ghost().small().icon(icon(if in_book { "book-user" } else { "user-plus" }).size(px(14.))).label(if in_book { "Open in Contacts" } else { "Add to contacts" }).on_click(move |_, window, cx| {
                let _ = book_t.update(cx, |this, cx| this.show_in_contacts(book_who.clone(), window, cx));
            }),
        )
        .child(Button::new(id("search")).ghost().small().icon(icon("search").size(px(14.))).label("Mail from them").on_click(move |_, window, cx| {
            let _ = search_t.update(cx, |this, cx| this.search_from(&search_email, window, cx));
        }));

    v_flex()
        .w(px(320.))
        .gap(px(12.))
        .child(
            h_flex()
                .items_center()
                .gap(px(12.))
                .child(photo)
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(div().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).child(data.name()))
                        .children(contact.and_then(role_line).map(|r| div().text_size(px(12.)).child(r)))
                        .child(div().text_size(px(11.)).text_color(p.muted).child(data.source())),
                ),
        )
        .child(details)
        .child(actions)
}

impl MailWindow {
    /// The contact with this address, in memory: an address-book entry before one only
    /// inferred from correspondence.
    pub(super) fn contact_for(&self, email: &str) -> Option<&Contact> {
        self.contact_index.get(&email.trim().to_ascii_lowercase()).and_then(|&i| self.contacts.get(i))
    }

    fn card_data(&self, who: &Address) -> CardData {
        let is_me = who.email.eq_ignore_ascii_case(&self.account.email);
        CardData { who: if is_me { self.owner() } else { who.clone() }, contact: self.contact_for(&who.email).cloned(), is_me }
    }

    pub(super) fn search_from(&mut self, email: &str, window: &mut Window, cx: &mut Context<Self>) {
        let query = format!("from:{email}");
        let input = self.search_input.clone();
        input.update(cx, |s, cx| s.set_value(query.clone(), window, cx));
        self.set_search(query, window, cx);
    }

    /// Makes `label`, a name in the header of message `msg`, open the card for `who`:
    /// hovering shows it; clicking pins it under the header, and while it is pinned the
    /// hover card steps aside. `key` tells the names in one header apart.
    pub(super) fn contact_trigger(&self, key: &str, msg: &MessageId, who: &Address, label: Stateful<Div>, cx: &mut Context<Self>) -> AnyElement {
        let p = *cx.palette();
        let target = (msg.clone(), who.email.to_ascii_lowercase());
        let pinned = self.contact_card.as_ref() == Some(&target);
        let label = label.cursor_pointer().on_click(cx.listener(move |this, _, _window, cx| {
            cx.stop_propagation();
            this.contact_card = if this.contact_card.as_ref() == Some(&target) { None } else { Some(target.clone()) };
            cx.notify();
        }));
        if pinned {
            return label.into_any_element();
        }
        let data = self.card_data(who);
        let t = cx.entity().downgrade();
        let scope = format!("card-{msg}-{key}");
        HoverCard::new(SharedString::from(scope.clone()))
            .anchor(Anchor::TopLeft)
            .open_delay(Duration::from_millis(350))
            .trigger(label)
            .content(move |_state, _window, _cx| card_body(&data, &p, &t, &scope))
            .into_any_element()
    }

    /// The To and Cc line of a message header: each name opens its card. Past a handful of
    /// names the rest hide behind "+N more", because a long list cannot be read, or reached,
    /// when it is cut off at the edge.
    pub(super) fn render_recipients(&self, m: &Message, cx: &mut Context<Self>) -> AnyElement {
        let p = *cx.palette();
        let to: Vec<Address> = if m.to.is_empty() { vec![self.owner()] } else { m.to.clone() };
        let total = to.len() + m.cc.len();
        let expanded = self.recipients_expanded.contains(&m.id);
        let shown = visible_recipients(total, expanded);
        let mut line = h_flex().flex_wrap().items_baseline().gap_x(px(5.)).gap_y(px(1.)).text_size(px(12.)).text_color(p.muted).child("to");
        let mut previous_was_to = true;
        for (i, who) in to.iter().chain(&m.cc).take(shown).enumerate() {
            let is_to = i < to.len();
            if previous_was_to && !is_to {
                line = line.child("· cc");
            }
            previous_was_to = is_to;
            let last_of_group = if is_to { i + 1 == to.len() } else { i + 1 == total };
            let text = if who.email.eq_ignore_ascii_case(&self.account.email) { "me".to_owned() } else { who.display_name() };
            let label = div()
                .id(SharedString::from(format!("rcpt-{}-{i}", m.id)))
                .max_w(px(260.))
                .truncate()
                .hover(|d| d.text_color(p.accent))
                .child(text);
            let trigger = self.contact_trigger(&format!("rcpt-{i}"), &m.id, who, label, cx);
            line = line.child(h_flex().items_baseline().child(trigger).when(i + 1 < shown && !last_of_group, |d| d.child(",")));
        }
        if total > RECIPIENTS_SHOWN + 1 {
            let id = m.id.clone();
            let text = if expanded { "show fewer".to_owned() } else { format!("+{} more", total - shown) };
            line = line.child(
                div().id(SharedString::from(format!("rcpt-more-{}", m.id))).cursor_pointer().text_color(p.accent).child(text).on_click(cx.listener(move |this, _, _window, cx| {
                    cx.stop_propagation();
                    if !this.recipients_expanded.remove(&id) {
                        this.recipients_expanded.insert(id.clone());
                    }
                    cx.notify();
                })),
            );
        }
        line.into_any_element()
    }

    /// The card pinned under the header of `m`, when one of its names was clicked.
    pub(super) fn pinned_contact_card(&self, m: &Message, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (msg, email) = self.contact_card.as_ref()?;
        if msg != &m.id {
            return None;
        }
        let who = std::iter::once(&m.from)
            .chain(&m.to)
            .chain(&m.cc)
            .find(|a| a.email.eq_ignore_ascii_case(email))
            .cloned()
            .or_else(|| email.eq_ignore_ascii_case(&self.account.email).then(|| self.owner()))?;
        let p = *cx.palette();
        let t = cx.entity().downgrade();
        let body = card_body(&self.card_data(&who), &p, &t, &format!("pinned-{}", m.id));
        Some(
            div()
                .relative()
                .flex_none()
                .w(px(348.))
                .p(px(14.))
                .rounded(px(10.))
                .bg(p.chip)
                .border_1()
                .border_color(p.divider)
                .child(body)
                .child(div().absolute().top(px(6.)).right(px(6.)).child(
                    Button::new(SharedString::from(format!("pinned-close-{}", m.id))).ghost().xsmall().icon(icon("x").size(px(12.))).tooltip("Close").on_click(cx.listener(|this, _, _window, cx| {
                        this.contact_card = None;
                        cx.notify();
                    })),
                ))
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // The parent's GPUI glob also exports a `test` proc macro.
    use std::prelude::v1::test;

    fn contact(id: &str, email: &str, others: &[&str], in_address_book: bool) -> Contact {
        Contact { id: id.into(), email: email.into(), emails: others.iter().map(|e| e.to_string()).collect(), in_address_book, ..Default::default() }
    }

    #[test]
    fn every_address_of_a_contact_finds_it_whatever_the_case() {
        let contacts = vec![contact("a", "Maya@Northstar.example", &["maya.chen@home.example"], true)];
        let index = index_contacts(&contacts);
        assert_eq!(index.get("maya@northstar.example"), Some(&0));
        assert_eq!(index.get("maya.chen@home.example"), Some(&0));
        assert_eq!(index.len(), 2);
    }

    #[test]
    fn the_address_book_wins_over_a_correspondent_whichever_comes_first() {
        let correspondent = contact("other", "sam@example.com", &[], false);
        let entry = contact("book", "sam@example.com", &[], true);
        assert_eq!(index_contacts(&[correspondent.clone(), entry.clone()]).get("sam@example.com"), Some(&1));
        assert_eq!(index_contacts(&[entry, correspondent]).get("sam@example.com"), Some(&0));
    }

    #[test]
    fn contacts_without_an_address_are_not_indexed() {
        assert!(index_contacts(&[contact("a", "", &["  "], true)]).is_empty());
    }

    #[test]
    fn long_recipient_lists_stop_at_five_unless_expanded() {
        assert_eq!(visible_recipients(3, false), 3);
        assert_eq!(visible_recipients(6, false), 6, "one more is listed rather than '+1 more'");
        assert_eq!(visible_recipients(7, false), 5);
        assert_eq!(visible_recipients(40, false), 5);
        assert_eq!(visible_recipients(40, true), 40);
    }

    #[test]
    fn a_role_is_the_title_and_organization_that_are_known() {
        let mut c = Contact::default();
        assert_eq!(role_line(&c), None);
        c.organization = Some("Northstar".into());
        assert_eq!(role_line(&c).as_deref(), Some("Northstar"));
        c.title = Some(" Head of Design ".into());
        assert_eq!(role_line(&c).as_deref(), Some("Head of Design · Northstar"));
        c.organization = Some("  ".into());
        assert_eq!(role_line(&c).as_deref(), Some("Head of Design"));
    }

    #[test]
    fn notes_are_cut_to_three_lines_and_marked_when_they_are() {
        assert_eq!(notes_excerpt("  \n\n"), None);
        assert_eq!(notes_excerpt("Met at the offsite\n\nPrefers email").as_deref(), Some("Met at the offsite\nPrefers email"));
        assert_eq!(notes_excerpt("one\ntwo\nthree\nfour").as_deref(), Some("one\ntwo\nthree…"));
        let long = "x".repeat(500);
        let cut = notes_excerpt(&long).unwrap();
        assert_eq!(cut.chars().count(), 201);
        assert!(cut.ends_with('…'));
    }

    #[test]
    fn a_card_lists_the_other_addresses_without_repeating_the_one_it_opened_for() {
        let data = |opened_for: &str| CardData {
            who: Address::new(None, opened_for),
            contact: Some(contact("a", "maya@northstar.example", &["Maya.Chen@home.example", "MAYA@northstar.example", "work@northstar.example"], true)),
            is_me: false,
        };
        assert_eq!(data("maya@northstar.example").other_emails(), vec!["Maya.Chen@home.example", "work@northstar.example"]);
        assert_eq!(data("work@northstar.example").other_emails(), vec!["maya@northstar.example", "Maya.Chen@home.example"]);
        assert!(CardData { who: Address::new(None, "x@example.com"), contact: None, is_me: false }.other_emails().is_empty());
    }

    #[test]
    fn a_card_says_where_it_knows_the_person_from() {
        let who = Address::named("Sam", "sam@example.com");
        let source = |contact: Option<Contact>, is_me| CardData { who: who.clone(), contact, is_me }.source();
        assert_eq!(source(None, false), "Not in your contacts");
        assert_eq!(source(Some(contact("o", "sam@example.com", &[], false)), false), "Someone you have corresponded with");
        assert_eq!(source(Some(contact("b", "sam@example.com", &[], true)), false), "In your Google Contacts");
        assert_eq!(source(None, true), "This is you");
    }

    #[test]
    fn a_card_prefers_the_address_book_name_to_the_one_in_the_header() {
        let who = Address::named("sam via list", "sam@example.com");
        let mut c = contact("b", "sam@example.com", &[], true);
        assert_eq!(CardData { who: who.clone(), contact: Some(c.clone()), is_me: false }.name(), "sam via list", "a contact without a name defers to the header");
        c.name = Some("Sam Okafor".into());
        assert_eq!(CardData { who: who.clone(), contact: Some(c), is_me: false }.name(), "Sam Okafor");
        assert_eq!(CardData { who: Address::new(None, "sam@example.com"), contact: None, is_me: false }.name(), "sam@example.com");
    }
}
