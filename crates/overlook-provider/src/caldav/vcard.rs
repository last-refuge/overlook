//! vCard 3.0 and 4.0 (RFC 2426, RFC 6350): the fields Overlook shows, read from a card, and
//! edits that change exactly those fields in a card from the server.
//!
//! An edit never rebuilds a card. It works line by line on the server's own text, so the
//! properties Overlook doesn't model (more phones and addresses, birthdays, photos, groups,
//! custom fields) come back exactly as they were. Only the lines a change touches are
//! rewritten.

use overlook_core::Contact;

/// A card larger than this is refused rather than parsed.
pub const MAX_CARD_BYTES: usize = 4 * 1024 * 1024;
const MAX_LINES: usize = 20_000;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VCardError {
    #[error("The contact card is not a vCard.")]
    NotACard,
    #[error("The contact card is too large.")]
    TooLarge,
}

/// One content line after unfolding, with the text it came from, so an untouched line is
/// written back byte for byte.
#[derive(Debug, Clone)]
struct Line {
    original: Option<String>,
    group: Option<String>,
    name: String,
    params: Vec<(String, String)>,
    value: String,
}

impl Line {
    fn new(name: &str, params: Vec<(String, String)>, value: String) -> Self {
        Self { original: None, group: None, name: name.to_owned(), params, value }
    }

    fn is(&self, name: &str) -> bool {
        self.name.eq_ignore_ascii_case(name)
    }

    fn param<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a str> + 'a {
        self.params.iter().filter(move |(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    /// vCard 4 `PREF=1` (the lower, the more preferred); vCard 3 `TYPE=pref`.
    fn preference(&self) -> u32 {
        if let Some(pref) = self.param("PREF").find_map(|v| v.trim().parse::<u32>().ok()) {
            return pref.clamp(1, 100);
        }
        let typed = self.param("TYPE").flat_map(|v| v.split(',')).any(|t| t.trim().trim_matches('"').eq_ignore_ascii_case("pref"));
        if typed { 1 } else { 101 }
    }

    fn text(&self) -> String {
        unescape(&self.value)
    }

    fn components(&self) -> Vec<String> {
        split_unescaped(&self.value, ';').iter().map(|c| unescape(c)).collect()
    }

    fn render(&self) -> String {
        if let Some(original) = &self.original {
            return original.clone();
        }
        let mut line = String::new();
        if let Some(group) = &self.group {
            line.push_str(group);
            line.push('.');
        }
        line.push_str(&self.name);
        for (key, value) in &self.params {
            line.push(';');
            line.push_str(key);
            line.push('=');
            line.push_str(value);
        }
        line.push(':');
        line.push_str(&self.value);
        fold(&line)
    }
}

/// A parsed card, ready to read or edit.
#[derive(Debug, Clone)]
pub struct Card {
    lines: Vec<Line>,
    version4: bool,
}

/// What Overlook reads from a card. `uid` and `is_group` let the caller key and filter it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CardFields {
    pub uid: Option<String>,
    pub is_group: bool,
    pub name: Option<String>,
    pub given_name: Option<String>,
    pub family_name: Option<String>,
    /// Every address, the preferred one first.
    pub emails: Vec<String>,
    pub phone: Option<String>,
    pub organization: Option<String>,
    pub title: Option<String>,
    pub notes: Option<String>,
}

impl Card {
    pub fn parse(text: &str) -> Result<Self, VCardError> {
        if text.len() > MAX_CARD_BYTES {
            return Err(VCardError::TooLarge);
        }
        let mut physical: Vec<&str> = Vec::new();
        for raw in text.split('\n') {
            physical.push(raw.strip_suffix('\r').unwrap_or(raw));
        }
        // Unfold: a line that starts with a space or tab continues the one before it.
        let mut logical: Vec<(String, String)> = Vec::new();
        for raw in physical {
            if let Some(rest) = raw.strip_prefix([' ', '\t']) {
                if let Some((unfolded, original)) = logical.last_mut() {
                    unfolded.push_str(rest);
                    original.push_str("\r\n");
                    original.push_str(raw);
                    continue;
                }
            }
            if raw.is_empty() {
                continue;
            }
            logical.push((raw.to_owned(), raw.to_owned()));
            if logical.len() > MAX_LINES {
                return Err(VCardError::TooLarge);
            }
        }
        let mut lines = Vec::with_capacity(logical.len());
        for (unfolded, original) in logical {
            let Some(mut line) = parse_line(&unfolded) else { continue };
            line.original = Some(original);
            lines.push(line);
        }
        let begins = lines.first().is_some_and(|l| l.is("BEGIN") && l.value.trim().eq_ignore_ascii_case("VCARD"));
        let ends = lines.last().is_some_and(|l| l.is("END") && l.value.trim().eq_ignore_ascii_case("VCARD"));
        if !begins || !ends {
            return Err(VCardError::NotACard);
        }
        // One card per resource: a nested or second card is not something to edit.
        if lines.iter().filter(|l| l.is("BEGIN")).count() != 1 {
            return Err(VCardError::NotACard);
        }
        let version4 = lines.iter().any(|l| l.is("VERSION") && l.value.trim().starts_with('4'));
        Ok(Self { lines, version4 })
    }

    pub fn fields(&self) -> CardFields {
        let first = |name: &str| self.lines.iter().find(|l| l.is(name));
        let text = |name: &str| first(name).map(Line::text).map(|t| t.trim().to_owned()).filter(|t| !t.is_empty());
        let n = first("N").map(Line::components).unwrap_or_default();
        let component = |i: usize| n.get(i).map(|s| s.trim().to_owned()).filter(|s| !s.is_empty());
        let kind = |name: &str| first(name).is_some_and(|l| l.text().trim().eq_ignore_ascii_case("group"));
        let mut emails: Vec<(u32, usize, String)> = Vec::new();
        for (index, line) in self.lines.iter().enumerate().filter(|(_, l)| l.is("EMAIL")) {
            let address = strip_scheme(line.text().trim(), "mailto:").to_ascii_lowercase();
            if !address.is_empty() && !emails.iter().any(|(_, _, e)| e == &address) {
                emails.push((line.preference(), index, address));
            }
        }
        emails.sort_by_key(|(pref, index, _)| (*pref, *index));
        let phone = self
            .lines
            .iter()
            .enumerate()
            .filter(|(_, l)| l.is("TEL"))
            .map(|(i, l)| (l.preference(), i, strip_scheme(l.text().trim(), "tel:").to_owned()))
            .filter(|(_, _, t)| !t.is_empty())
            .min_by_key(|(pref, i, _)| (*pref, *i))
            .map(|(_, _, t)| t);
        CardFields {
            uid: text("UID"),
            is_group: kind("KIND") || kind("X-ADDRESSBOOKSERVER-KIND"),
            name: text("FN"),
            given_name: component(1),
            family_name: component(0),
            emails: emails.into_iter().map(|(_, _, e)| e).collect(),
            phone,
            organization: first("ORG").map(Line::components).and_then(|c| c.into_iter().next()).map(|s| s.trim().to_owned()).filter(|s| !s.is_empty()),
            title: text("TITLE"),
            notes: text("NOTE"),
        }
    }

    pub fn render(&self) -> String {
        let mut out = String::new();
        for line in &self.lines {
            out.push_str(&line.render());
            out.push_str("\r\n");
        }
        out
    }

    fn end_index(&self) -> usize {
        self.lines.iter().rposition(|l| l.is("END")).unwrap_or(self.lines.len())
    }

    fn set_text(&mut self, name: &str, value: Option<&str>) {
        let value = value.map(str::trim).filter(|v| !v.is_empty());
        match (self.lines.iter().position(|l| l.is(name)), value) {
            (Some(i), Some(v)) => {
                let line = &mut self.lines[i];
                line.value = escape(v);
                line.original = None;
            }
            (Some(_), None) => self.lines.retain(|l| !l.is(name)),
            (None, Some(v)) => {
                let at = self.end_index();
                self.lines.insert(at, Line::new(name, Vec::new(), escape(v)));
            }
            (None, None) => {}
        }
    }

    /// Replaces the line holding `old` (or adds one), so the parameters (`TYPE=work`, the
    /// preference) of an edited address stay with it. A cleared value removes only that line.
    fn replace_value(&mut self, name: &str, old: Option<&str>, new: Option<&str>, scheme: &str) {
        let same = |line: &Line, value: &str| strip_scheme(line.text().trim(), scheme).eq_ignore_ascii_case(value.trim());
        let new = new.map(str::trim).filter(|v| !v.is_empty());
        let position = old.filter(|o| !o.trim().is_empty()).and_then(|o| self.lines.iter().position(|l| l.is(name) && same(l, o)));
        match (position, new) {
            (Some(i), Some(v)) => {
                let line = &mut self.lines[i];
                let uri = line.value.trim_start().to_ascii_lowercase().starts_with(scheme) && !scheme.is_empty();
                line.value = if uri { format!("{scheme}{}", escape(v)) } else { escape(v) };
                line.original = None;
            }
            (Some(i), None) => {
                self.lines.remove(i);
            }
            (None, Some(v)) => {
                if self.lines.iter().any(|l| l.is(name) && same(l, v)) {
                    return;
                }
                // A new address goes first among its kind, where readers look for the main one.
                let at = self.lines.iter().position(|l| l.is(name)).unwrap_or_else(|| self.end_index());
                let params = if name == "EMAIL" && !self.version4 { vec![("TYPE".to_owned(), "INTERNET".to_owned())] } else { Vec::new() };
                self.lines.insert(at, Line::new(name, params, escape(v)));
            }
            (None, None) => {}
        }
    }

    /// Sets the given and family names in `N`, keeping its other parts, and the display name.
    /// Sets the name parts given as `Some` and keeps the others as the server has them, so an
    /// edit to the given name here and one to the family name elsewhere both survive. The
    /// display name follows the names that result, or `fallback` when both are empty.
    fn set_names(&mut self, given: Option<Option<&str>>, family: Option<Option<&str>>, fallback: Option<&str>) {
        let clean = |v: Option<&str>| v.map(str::trim).unwrap_or_default().to_owned();
        let (given_now, family_now) = match self.lines.iter().position(|l| l.is("N")) {
            Some(i) => {
                let line = &mut self.lines[i];
                let mut parts = split_unescaped(&line.value, ';');
                parts.resize(parts.len().max(5), String::new());
                if let Some(family) = family {
                    parts[0] = escape_component(&clean(family));
                }
                if let Some(given) = given {
                    parts[1] = escape_component(&clean(given));
                }
                line.value = parts.join(";");
                line.original = None;
                (unescape(&parts[1]), unescape(&parts[0]))
            }
            None => {
                let (given, family) = (clean(given.flatten()), clean(family.flatten()));
                let at = self.lines.iter().position(|l| l.is("FN")).map(|i| i + 1).unwrap_or_else(|| self.end_index());
                let value = format!("{};{};;;", escape_component(&family), escape_component(&given));
                self.lines.insert(at, Line::new("N", Vec::new(), value));
                (given, family)
            }
        };
        let display = format!("{} {}", given_now.trim(), family_now.trim()).trim().to_owned();
        let display = if display.is_empty() { fallback.map(str::trim).unwrap_or_default().to_owned() } else { display };
        if !display.is_empty() {
            self.set_text("FN", Some(&display));
        }
    }

    fn set_organization(&mut self, organization: Option<&str>) {
        let organization = organization.map(str::trim).filter(|o| !o.is_empty());
        match self.lines.iter().position(|l| l.is("ORG")) {
            Some(i) => {
                let mut parts = split_unescaped(&self.lines[i].value, ';');
                let rest_empty = parts.iter().skip(1).all(|p| p.trim().is_empty());
                match organization {
                    None if rest_empty => {
                        self.lines.remove(i);
                    }
                    value => {
                        parts[0] = escape_component(value.unwrap_or_default());
                        let line = &mut self.lines[i];
                        line.value = parts.join(";");
                        line.original = None;
                    }
                }
            }
            None => {
                if let Some(value) = organization {
                    let at = self.end_index();
                    self.lines.insert(at, Line::new("ORG", Vec::new(), escape_component(value)));
                }
            }
        }
    }

    fn touch_revision(&mut self, now: i64) {
        if let Some(line) = self.lines.iter_mut().find(|l| l.is("REV")) {
            line.value = overlook_core::tz::utc_basic(now);
            line.params.retain(|(k, _)| !k.eq_ignore_ascii_case("VALUE"));
            line.original = None;
        }
    }
}

/// The contact Overlook shows for a card, with the given id.
pub fn contact_from_fields(fields: &CardFields, id: String, account: overlook_core::AccountId, etag: Option<String>, updated: i64) -> Contact {
    let email = fields.emails.first().cloned().unwrap_or_default();
    let name = fields.name.clone().or_else(|| {
        let joined = format!("{} {}", fields.given_name.clone().unwrap_or_default(), fields.family_name.clone().unwrap_or_default());
        let joined = joined.trim().to_owned();
        (!joined.is_empty()).then_some(joined)
    });
    Contact {
        id,
        account_id: account,
        email,
        emails: fields.emails.clone(),
        name,
        given_name: fields.given_name.clone(),
        family_name: fields.family_name.clone(),
        phone: fields.phone.clone(),
        organization: fields.organization.clone(),
        title: fields.title.clone(),
        notes: fields.notes.clone(),
        photo_url: None,
        in_address_book: true,
        etag,
        updated,
        pending: false,
    }
}

/// Applies what changed between `before` and `after` (the contact as read, and as edited) to
/// `card`, which may be a newer copy from the server. Fields the edit didn't change keep the
/// card's values, even where the server's copy has moved on.
pub fn apply_changes(card: &str, before: &Contact, after: &Contact, now: i64) -> Result<String, VCardError> {
    let mut card = Card::parse(card)?;
    let changed = |a: &Option<String>, b: &Option<String>| a.as_deref().map(str::trim).filter(|s| !s.is_empty()) != b.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let mut touched = false;
    // The form rebuilds the display name from the given and family names, so only a change
    // to those renames the card; a custom display name ("Dr. Maya Chen") otherwise stays.
    let (given, family) = (changed(&before.given_name, &after.given_name), changed(&before.family_name, &after.family_name));
    if given || family {
        card.set_names(given.then_some(after.given_name.as_deref()), family.then_some(after.family_name.as_deref()), after.name.as_deref().or(Some(after.email.as_str())));
        touched = true;
    }
    if !before.email.trim().eq_ignore_ascii_case(after.email.trim()) {
        card.replace_value("EMAIL", Some(&before.email), Some(&after.email), "mailto:");
        touched = true;
    }
    if changed(&before.phone, &after.phone) {
        card.replace_value("TEL", before.phone.as_deref(), after.phone.as_deref(), "tel:");
        touched = true;
    }
    if changed(&before.organization, &after.organization) {
        card.set_organization(after.organization.as_deref());
        touched = true;
    }
    if changed(&before.title, &after.title) {
        card.set_text("TITLE", after.title.as_deref());
        touched = true;
    }
    if changed(&before.notes, &after.notes) {
        card.set_text("NOTE", after.notes.as_deref());
        touched = true;
    }
    if touched {
        card.touch_revision(now);
    }
    Ok(card.render())
}

/// A new vCard 3.0 for a contact made in Overlook. Version 3.0 is the one every CardDAV
/// server accepts.
pub fn new_card(contact: &Contact, uid: &str, now: i64) -> String {
    let display = contact.known_name().unwrap_or_else(|| contact.email.clone());
    let mut lines = vec![
        Line::new("BEGIN", Vec::new(), "VCARD".into()),
        Line::new("VERSION", Vec::new(), "3.0".into()),
        Line::new("PRODID", Vec::new(), "-//Last Refuge//Overlook//EN".into()),
        Line::new("UID", Vec::new(), escape(uid)),
        Line::new("FN", Vec::new(), escape(&display)),
        Line::new(
            "N",
            Vec::new(),
            format!(
                "{};{};;;",
                escape_component(contact.family_name.as_deref().unwrap_or_default().trim()),
                escape_component(contact.given_name.as_deref().unwrap_or_default().trim())
            ),
        ),
    ];
    let mut emails: Vec<&str> = Vec::new();
    for email in std::iter::once(contact.email.as_str()).chain(contact.emails.iter().map(String::as_str)) {
        let email = email.trim();
        if !email.is_empty() && !emails.iter().any(|e| e.eq_ignore_ascii_case(email)) {
            emails.push(email);
        }
    }
    for email in emails {
        lines.push(Line::new("EMAIL", vec![("TYPE".into(), "INTERNET".into())], escape(email)));
    }
    let optional = [("TEL", &contact.phone), ("TITLE", &contact.title), ("NOTE", &contact.notes)];
    for (name, value) in optional {
        if let Some(v) = value.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
            lines.push(Line::new(name, Vec::new(), escape(v)));
        }
    }
    if let Some(org) = contact.organization.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
        lines.push(Line::new("ORG", Vec::new(), escape_component(org)));
    }
    lines.push(Line::new("REV", Vec::new(), overlook_core::tz::utc_basic(now)));
    lines.push(Line::new("END", Vec::new(), "VCARD".into()));
    Card { lines, version4: false }.render()
}

/// `[group.]NAME;param=value;…:value`. Parameters split outside quotes; a line without a colon
/// is not a property and is skipped.
fn parse_line(line: &str) -> Option<Line> {
    let mut in_quotes = false;
    let mut colon = None;
    for (i, c) in line.char_indices() {
        match c {
            '"' => in_quotes = !in_quotes,
            ':' if !in_quotes => {
                colon = Some(i);
                break;
            }
            _ => {}
        }
    }
    let colon = colon?;
    let (head, value) = (&line[..colon], &line[colon + 1..]);
    let mut parts = split_outside_quotes(head, ';').into_iter();
    let full_name = parts.next()?.trim().to_owned();
    if full_name.is_empty() {
        return None;
    }
    let (group, name) = match full_name.rsplit_once('.') {
        Some((group, name)) => (Some(group.to_owned()), name.to_ascii_uppercase()),
        None => (None, full_name.to_ascii_uppercase()),
    };
    let params = parts
        .map(|p| match p.split_once('=') {
            Some((k, v)) => (k.trim().to_ascii_uppercase(), v.trim().to_owned()),
            // vCard 2.1 bare parameters (`TEL;CELL:…`) are types.
            None => ("TYPE".to_owned(), p.trim().to_owned()),
        })
        .collect();
    Some(Line { original: None, group, name, params, value: value.to_owned() })
}

fn split_outside_quotes(text: &str, separator: char) -> Vec<String> {
    let mut parts = vec![String::new()];
    let mut in_quotes = false;
    for c in text.chars() {
        if c == '"' {
            in_quotes = !in_quotes;
        }
        if c == separator && !in_quotes {
            parts.push(String::new());
        } else if let Some(last) = parts.last_mut() {
            last.push(c);
        }
    }
    parts
}

/// Splits a structured value on separators that aren't escaped, leaving each part escaped.
fn split_unescaped(value: &str, separator: char) -> Vec<String> {
    let mut parts = vec![String::new()];
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            let last = parts.last_mut().expect("one part");
            last.push(c);
            if let Some(next) = chars.next() {
                last.push(next);
            }
        } else if c == separator {
            parts.push(String::new());
        } else {
            parts.last_mut().expect("one part").push(c);
        }
    }
    parts
}

fn unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n' | 'N') => out.push('\n'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

fn escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace("\r\n", "\n").replace('\n', "\\n").replace(',', "\\,").replace(';', "\\;")
}

/// A part of a structured value, where `;` separates the parts and `,` lists values.
fn escape_component(value: &str) -> String {
    escape(value)
}

fn strip_scheme<'a>(value: &'a str, scheme: &str) -> &'a str {
    // `get` rather than slicing: a value from the server may have a multi-byte character
    // where the scheme would end ("１２３４" in a TEL).
    match value.get(..scheme.len()) {
        Some(prefix) if !scheme.is_empty() && prefix.eq_ignore_ascii_case(scheme) => &value[scheme.len()..],
        _ => value,
    }
}

/// Folds at 75 octets, as both vCard versions ask, never inside a character.
fn fold(line: &str) -> String {
    const WIDTH: usize = 75;
    if line.len() <= WIDTH {
        return line.to_owned();
    }
    let mut out = String::with_capacity(line.len() + line.len() / WIDTH * 3);
    let mut width = 0;
    let mut limit = WIDTH;
    for c in line.chars() {
        if width + c.len_utf8() > limit {
            out.push_str("\r\n ");
            width = 0;
            // Continuation lines start with the space, which counts toward their length.
            limit = WIDTH - 1;
        }
        out.push(c);
        width += c.len_utf8();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use overlook_core::AccountId;

    const FASTMAIL_STYLE: &str = "BEGIN:VCARD\r\nVERSION:3.0\r\nPRODID:-//CyrusIMAP.org//Cyrus 3.9//EN\r\nUID:1f2e3d4c-aaaa-bbbb-cccc-0123456789ab\r\nN:Chen;Maya;;;\r\nFN:Maya Chen\r\nORG:Northstar Studio;Design\r\nTITLE:Design Director\r\nitem1.EMAIL;TYPE=INTERNET;TYPE=pref:maya@northstar.example\r\nitem1.X-ABLabel:Work\r\nEMAIL;TYPE=INTERNET;TYPE=HOME:maya.chen@home.example\r\nTEL;TYPE=CELL:+1 555 0100\r\nTEL;TYPE=WORK:+1 555 0199\r\nADR;TYPE=WORK:;;1 Ridge Road;Pine Ridge;;;\r\nBDAY:1988-04-12\r\nNOTE:Prefers mornings\\, not Fridays.\\nCall first.\r\nPHOTO;ENCODING=b;TYPE=JPEG:/9j/4AAQSkZJRgABAQAAAQABAAD/2wBDAAgGBgcGBQgHBwcJCQgKDBQNDAsLDBkSEw8UHRofHh0aHBwgJC4nICIsIxwcKDcpLDAxNDQ0Hyc5PTgy\r\n PDsuMzQy\r\nREV:20261101T120000Z\r\nEND:VCARD\r\n";

    fn contact(fields: &CardFields) -> Contact {
        contact_from_fields(fields, "carddav:x".into(), AccountId::from("a"), None, 0)
    }

    #[test]
    fn reads_the_fields_overlook_shows() {
        let card = Card::parse(FASTMAIL_STYLE).unwrap();
        let fields = card.fields();
        assert_eq!(fields.uid.as_deref(), Some("1f2e3d4c-aaaa-bbbb-cccc-0123456789ab"));
        assert_eq!(fields.name.as_deref(), Some("Maya Chen"));
        assert_eq!((fields.given_name.as_deref(), fields.family_name.as_deref()), (Some("Maya"), Some("Chen")));
        assert_eq!(fields.emails, vec!["maya@northstar.example", "maya.chen@home.example"]);
        assert_eq!(fields.phone.as_deref(), Some("+1 555 0100"));
        assert_eq!(fields.organization.as_deref(), Some("Northstar Studio"));
        assert_eq!(fields.title.as_deref(), Some("Design Director"));
        assert_eq!(fields.notes.as_deref(), Some("Prefers mornings, not Fridays.\nCall first."));
        assert!(!fields.is_group);
    }

    #[test]
    fn an_untouched_card_is_written_back_byte_for_byte() {
        let card = Card::parse(FASTMAIL_STYLE).unwrap();
        assert_eq!(card.render(), FASTMAIL_STYLE);
        let before = contact(&card.fields());
        assert_eq!(apply_changes(FASTMAIL_STYLE, &before, &before, 0).unwrap(), FASTMAIL_STYLE);
    }

    #[test]
    fn an_edit_changes_only_its_fields_and_keeps_everything_else() {
        let before = contact(&Card::parse(FASTMAIL_STYLE).unwrap().fields());
        let mut after = before.clone();
        after.given_name = Some("Maya R.".into());
        after.name = Some("Maya R. Chen".into());
        after.email = "maya@northstar-studio.example".into();
        after.phone = Some("+1 555 0111".into());
        after.organization = Some("Northstar".into());
        after.title = None;
        let edited = apply_changes(FASTMAIL_STYLE, &before, &after, 1_800_000_000_000).unwrap();
        // The changed address keeps its group, type and preference; the other one stays.
        assert!(edited.contains("item1.EMAIL;TYPE=INTERNET;TYPE=pref:maya@northstar-studio.example\r\n"));
        assert!(edited.contains("EMAIL;TYPE=INTERNET;TYPE=HOME:maya.chen@home.example\r\n"));
        assert!(edited.contains("TEL;TYPE=CELL:+1 555 0111\r\n") && edited.contains("TEL;TYPE=WORK:+1 555 0199\r\n"));
        assert!(edited.contains("N:Chen;Maya R.;;;\r\n") && edited.contains("FN:Maya R. Chen\r\n"));
        assert!(edited.contains("ORG:Northstar;Design\r\n"), "the department stays");
        assert!(!edited.contains("TITLE:"));
        for kept in ["ADR;TYPE=WORK:;;1 Ridge Road;Pine Ridge;;;\r\n", "BDAY:1988-04-12\r\n", "item1.X-ABLabel:Work\r\n", " PDsuMzQy\r\n", "NOTE:Prefers mornings\\, not Fridays.\\nCall first.\r\n"] {
            assert!(edited.contains(kept), "kept {kept:?}");
        }
        assert!(edited.contains("REV:20270115T080000Z\r\n"));
        let reread = contact(&Card::parse(&edited).unwrap().fields());
        assert_eq!(reread.email, "maya@northstar-studio.example");
        assert_eq!(reread.phone.as_deref(), Some("+1 555 0111"));
    }

    #[test]
    fn edits_land_on_a_newer_server_copy_without_undoing_its_changes() {
        let before = contact(&Card::parse(FASTMAIL_STYLE).unwrap().fields());
        // Changed on a phone meanwhile: a new title.
        let newer = FASTMAIL_STYLE.replace("TITLE:Design Director", "TITLE:Head of Design");
        let mut after = before.clone();
        after.notes = Some("Call first.".into());
        let edited = apply_changes(&newer, &before, &after, 0).unwrap();
        assert!(edited.contains("TITLE:Head of Design\r\n"), "the server's change stays");
        assert!(edited.contains("NOTE:Call first.\r\n"));
    }

    #[test]
    fn a_name_edit_keeps_the_other_name_part_as_the_server_has_it() {
        let before = contact(&Card::parse(FASTMAIL_STYLE).unwrap().fields());
        // Married name added on a phone while a given-name edit waited here.
        let newer = FASTMAIL_STYLE.replace("N:Chen;Maya;;;", "N:Chen-Rivera;Maya;;;");
        let mut after = before.clone();
        after.given_name = Some("Maya R.".into());
        after.name = Some("Maya R. Chen".into());
        let edited = apply_changes(&newer, &before, &after, 0).unwrap();
        assert!(edited.contains("N:Chen-Rivera;Maya R.;;;\r\n"), "{edited}");
        assert!(edited.contains("FN:Maya R. Chen-Rivera\r\n"), "{edited}");
    }

    #[test]
    fn values_with_wide_characters_where_a_scheme_would_end_are_read() {
        // Full-width digits are three bytes each, so "tel:" would end inside one.
        let card = "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Kenji Mori\r\nTEL:１２３４５\r\nEMAIL:ñandú@example.test\r\nEND:VCARD\r\n";
        let fields = Card::parse(card).unwrap().fields();
        assert_eq!(fields.phone.as_deref(), Some("１２３４５"));
        assert_eq!(fields.emails, vec!["ñandú@example.test"]);
        assert_eq!(strip_scheme("tél", "tel:"), "tél");
        assert_eq!(strip_scheme("TEL:+1 555", "tel:"), "+1 555");
    }

    #[test]
    fn a_custom_display_name_stays_unless_the_names_change() {
        let card = FASTMAIL_STYLE.replace("FN:Maya Chen", "FN:Dr. Maya Chen");
        let before = contact(&Card::parse(&card).unwrap().fields());
        let mut after = before.clone();
        // The form always rebuilds the display name from the given and family names.
        after.name = Some("Maya Chen".into());
        after.phone = Some("+1 555 0123".into());
        let edited = apply_changes(&card, &before, &after, 0).unwrap();
        assert!(edited.contains("FN:Dr. Maya Chen\r\n"), "{edited}");
    }

    #[test]
    fn clearing_an_address_removes_only_that_address() {
        let before = contact(&Card::parse(FASTMAIL_STYLE).unwrap().fields());
        let mut after = before.clone();
        after.email.clear();
        after.emails.clear();
        let edited = apply_changes(FASTMAIL_STYLE, &before, &after, 0).unwrap();
        assert!(!edited.contains("maya@northstar.example"));
        assert!(edited.contains("maya.chen@home.example"));
    }

    #[test]
    fn a_new_address_becomes_the_first_of_its_kind() {
        let card = "BEGIN:VCARD\r\nVERSION:4.0\r\nFN:Alex\r\nEMAIL;PREF=1:alex@studio.example\r\nEND:VCARD\r\n";
        let before = contact(&Card::parse(card).unwrap().fields());
        let mut after = before.clone();
        after.email = "alex@rivera.example".into();
        // The old address isn't on the server copy any more, so the new one is added.
        let moved = card.replace("alex@studio.example", "alex@elsewhere.example");
        let edited = apply_changes(&moved, &before, &after, 0).unwrap();
        assert!(edited.contains("EMAIL:alex@rivera.example\r\nEMAIL;PREF=1:alex@elsewhere.example\r\n"), "{edited}");
    }

    #[test]
    fn reads_vcard_4_preferences_uris_and_groups() {
        let card = "BEGIN:VCARD\r\nVERSION:4.0\r\nUID:urn:uuid:42\r\nFN:Daniel Park\r\nEMAIL;PREF=2:d@one.example\r\nEMAIL;PREF=1:D@Two.example\r\nTEL;VALUE=uri;TYPE=cell:tel:+1-555-0142\r\nEND:VCARD\r\n";
        let fields = Card::parse(card).unwrap().fields();
        assert_eq!(fields.emails, vec!["d@two.example", "d@one.example"]);
        assert_eq!(fields.phone.as_deref(), Some("+1-555-0142"));
        let group = "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Studio\r\nX-ADDRESSBOOKSERVER-KIND:group\r\nEND:VCARD\r\n";
        assert!(Card::parse(group).unwrap().fields().is_group);
        // A changed URI phone keeps its scheme.
        let before = contact(&fields);
        let mut after = before.clone();
        after.phone = Some("+1-555-0143".into());
        assert!(apply_changes(card, &before, &after, 0).unwrap().contains("TEL;VALUE=uri;TYPE=cell:tel:+1-555-0143\r\n"));
    }

    #[test]
    fn new_cards_are_complete_escaped_and_folded() {
        let mut c = Contact::new_local(AccountId::from("a"), 0);
        c.given_name = Some("Sophie".into());
        c.family_name = Some("Williams".into());
        c.name = Some("Sophie Williams".into());
        c.email = "sophie@example.com".into();
        c.emails = vec!["sophie@example.com".into()];
        c.organization = Some("Trails; Coffee".into());
        c.notes = Some("A long note that keeps going well past the seventy-five octets a vCard line may hold, with ümlauts.".into());
        let text = new_card(&c, "new-uid", 0);
        assert!(text.starts_with("BEGIN:VCARD\r\nVERSION:3.0\r\n") && text.ends_with("END:VCARD\r\n"));
        assert!(text.contains("UID:new-uid\r\n") && text.contains("N:Williams;Sophie;;;\r\n"));
        assert!(text.contains("ORG:Trails\\; Coffee\r\n"));
        assert!(text.lines().all(|l| l.len() <= 76), "folded");
        let fields = Card::parse(&text).unwrap().fields();
        assert_eq!(fields.notes, c.notes);
        assert_eq!(fields.organization.as_deref(), Some("Trails; Coffee"));
        assert_eq!(fields.emails, vec!["sophie@example.com"]);
    }

    #[test]
    fn rejects_what_is_not_one_card() {
        assert_eq!(Card::parse("hello").unwrap_err(), VCardError::NotACard);
        assert_eq!(Card::parse("BEGIN:VCARD\r\nFN:x\r\n").unwrap_err(), VCardError::NotACard);
        let two = "BEGIN:VCARD\r\nFN:a\r\nEND:VCARD\r\nBEGIN:VCARD\r\nFN:b\r\nEND:VCARD\r\n";
        assert_eq!(Card::parse(two).unwrap_err(), VCardError::NotACard);
        assert_eq!(Card::parse(&"x".repeat(MAX_CARD_BYTES + 1)).unwrap_err(), VCardError::TooLarge);
    }

    #[test]
    fn unix_line_endings_and_quoted_parameters_parse() {
        let card = "BEGIN:VCARD\nVERSION:3.0\nFN:Quoted\nEMAIL;TYPE=\"work,pref\":q@quoted.example\nEND:VCARD\n";
        let fields = Card::parse(card).unwrap().fields();
        assert_eq!(fields.emails, vec!["q@quoted.example"]);
    }
}
