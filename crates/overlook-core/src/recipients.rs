//! Recipient-field editing shared by new messages, replies and forwards.

use crate::Address;
use std::ops::Range;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecipientField {
    To,
    Cc,
    Bcc,
}

/// A completion belongs to one field and one text/selection snapshot. Keeping
/// that snapshot prevents a click on an old suggestion from replacing new text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecipientQuery {
    pub field: RecipientField,
    value: String,
    selection: Range<usize>,
    token: Range<usize>,
}

impl RecipientQuery {
    /// Input selection offsets are UTF-8 bytes. A selection spanning multiple
    /// recipients must never be replaced by a single completion.
    pub fn new(field: RecipientField, value: &str, selection: Range<usize>) -> Option<Self> {
        if selection.start > selection.end
            || !value.is_char_boundary(selection.start)
            || !value.is_char_boundary(selection.end)
        {
            return None;
        }
        let token = recipient_ranges(value)
            .into_iter()
            .find(|r| r.start <= selection.start && selection.end <= r.end)?;
        let query = Self {
            field,
            value: value.to_owned(),
            selection,
            token,
        };
        (!query.text().is_empty()).then_some(query)
    }

    pub fn text(&self) -> &str {
        self.value[self.token.clone()].trim().trim_matches('"')
    }

    /// Whether the caret is at the very end of the field, straight after the recipient being
    /// typed. Only then can a suggestion be shown after the text without covering anything.
    pub fn completes_at_end(&self) -> bool {
        self.selection.is_empty()
            && self.selection.end == self.value.len()
            && self.token.end == self.value.len()
            && !self.value.ends_with(char::is_whitespace)
    }

    /// Replace only the active recipient, preserving all other addresses and
    /// separators. At the end of a field, leave room to enter another recipient.
    pub fn replacement(
        &self,
        current: &str,
        selection: Range<usize>,
        address: &Address,
    ) -> Option<(Range<usize>, String)> {
        if current != self.value || selection != self.selection {
            return None;
        }
        let part = &current[self.token.clone()];
        let start = self.token.start + part.len() - part.trim_start().len();
        let end = self.token.start + part.trim_end().len();
        let mut header = address.to_header();
        let range = if self.token.end == current.len() {
            header.push_str(", ");
            start..self.token.end
        } else {
            start..end
        };
        Some((range, header))
    }
}

/// `candidate` without the start that `typed` spells, ignoring case; `None` when it does
/// not start that way.
fn strip_prefix_ignore_case<'a>(candidate: &'a str, typed: &str) -> Option<&'a str> {
    let mut chars = candidate.char_indices();
    let mut end = 0;
    for t in typed.chars() {
        let (i, c) = chars.next()?;
        if c != t && !c.to_lowercase().eq(t.to_lowercase()) {
            return None;
        }
        end = i + c.len_utf8();
    }
    Some(&candidate[end..])
}

/// What to show after `typed` when `address` continues it: the rest of the name and then
/// the address, which is how it is written once chosen, or the rest of the address. `None`
/// when neither starts with what was typed. A later word of the name can still match, and
/// the list below the field covers that; only a start can be completed in place.
pub fn ghost_completion(typed: &str, address: &Address) -> Option<String> {
    let typed = typed.trim();
    if typed.is_empty() {
        return None;
    }
    if let Some(name) = address.name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
        // A name that has to be quoted would not read as what it becomes, so it is not offered.
        if address.to_header() == format!("{name} <{}>", address.email) {
            if let Some(rest) = strip_prefix_ignore_case(name, typed) {
                return Some(format!("{rest} <{}>", address.email));
            }
        }
    }
    strip_prefix_ignore_case(&address.email, typed).filter(|rest| !rest.is_empty()).map(str::to_owned)
}

/// The suggestion to offer inside the field: the first one that continues what is being
/// typed, with the text to show. `None` unless the caret is at the end of the field.
pub fn inline_suggestion<'a>(query: &RecipientQuery, suggestions: &'a [Address]) -> Option<(&'a Address, String)> {
    if !query.completes_at_end() {
        return None;
    }
    suggestions.iter().find_map(|a| ghost_completion(query.text(), a).map(|ghost| (a, ghost)))
}

/// Split composer address lists without treating punctuation inside quoted
/// display names or angle addresses as recipient separators. Also used when
/// saving the draft so the completion and parser agree on recipient boundaries.
pub(crate) fn recipient_ranges(input: &str) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut start = 0;
    let mut quoted = false;
    let mut escaped = false;
    let mut angle = false;
    for (offset, ch) in input.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' if quoted => escaped = true,
            '"' => quoted = !quoted,
            '<' if !quoted => angle = true,
            '>' if !quoted => angle = false,
            ',' | ';' if !quoted && !angle => {
                ranges.push(start..offset);
                start = offset + ch.len_utf8();
            }
            _ => {}
        }
    }
    ranges.push(start..input.len());
    ranges
}

#[cfg(test)]
mod tests {
    use super::*;

    fn maya() -> Address {
        Address::named("Maya Chen", "maya@northstar.example")
    }

    #[test]
    fn a_name_is_completed_to_the_address_it_stands_for() {
        assert_eq!(ghost_completion("ma", &maya()).as_deref(), Some("ya Chen <maya@northstar.example>"));
        assert_eq!(ghost_completion("MAYA c", &maya()).as_deref(), Some("hen <maya@northstar.example>"));
        // The whole name is typed: what is left is the address.
        assert_eq!(ghost_completion("Maya Chen", &maya()).as_deref(), Some(" <maya@northstar.example>"));
    }

    #[test]
    fn an_address_is_completed_to_the_rest_of_the_address() {
        let unnamed = Address::new(None, "sam@example.com");
        assert_eq!(ghost_completion("sa", &unnamed).as_deref(), Some("m@example.com"));
        assert_eq!(ghost_completion("maya@no", &maya()).as_deref(), Some("rthstar.example"));
        assert_eq!(ghost_completion("MAYA@", &maya()).as_deref(), Some("northstar.example"));
        assert_eq!(ghost_completion("sam@example.com", &unnamed), None, "nothing is left to add");
    }

    #[test]
    fn only_a_start_is_completed_in_place() {
        assert_eq!(ghost_completion("chen", &maya()), None, "a later word of the name only matches in the list");
        assert_eq!(ghost_completion("northstar", &maya()), None);
        assert_eq!(ghost_completion("", &maya()), None);
        assert_eq!(ghost_completion("  ", &maya()), None);
        assert_eq!(ghost_completion("maya chens", &maya()), None, "typing past the name leaves nothing to complete");
    }

    #[test]
    fn a_name_that_needs_quoting_is_not_spelled_out_but_its_address_still_completes() {
        let jane = Address::named("Doe, Jane", "jane@example.com");
        assert_eq!(ghost_completion("Doe", &jane), None);
        assert_eq!(ghost_completion("jan", &jane).as_deref(), Some("e@example.com"));
    }

    #[test]
    fn completion_ignores_case_across_scripts() {
        let emile = Address::named("Émile Zola", "emile@example.fr");
        assert_eq!(ghost_completion("ém", &emile).as_deref(), Some("ile Zola <emile@example.fr>"));
        assert_eq!(ghost_completion("ÉMI", &emile).as_deref(), Some("le Zola <emile@example.fr>"));
        let tokyo = Address::named("東京 太郎", "taro@example.jp");
        assert_eq!(ghost_completion("東", &tokyo).as_deref(), Some("京 太郎 <taro@example.jp>"));
    }

    #[test]
    fn a_suggestion_is_offered_in_the_field_only_with_the_caret_at_the_end() {
        let list = vec![Address::named("Maya Chen", "maya@northstar.example"), Address::named("Mateo Ruiz", "mateo@example.com")];
        let at = |value: &str, selection: Range<usize>| RecipientQuery::new(RecipientField::To, value, selection).unwrap();
        let end = |value: &str| at(value, value.len()..value.len());

        let (address, ghost) = inline_suggestion(&end("ma"), &list).unwrap();
        assert_eq!((address.email.as_str(), ghost.as_str()), ("maya@northstar.example", "ya Chen <maya@northstar.example>"));
        // The first suggestion that continues the text wins, not just the first.
        let (address, _) = inline_suggestion(&end("mat"), &list).unwrap();
        assert_eq!(address.email, "mateo@example.com");

        // After other recipients is still the end of the field.
        assert!(inline_suggestion(&end("lee@example.com, ma"), &list).is_some());
        // In the middle of the text, or of the word, or with a selection, it would cover text.
        assert_eq!(inline_suggestion(&at("ma, last@example.com", 2..2), &list), None);
        assert_eq!(inline_suggestion(&at("ma", 1..1), &list), None);
        assert_eq!(inline_suggestion(&at("ma", 0..2), &list), None);
        // After a trailing space the text no longer ends in what is being typed.
        assert_eq!(inline_suggestion(&end("ma "), &list), None);
        assert_eq!(inline_suggestion(&end("ma"), &[]), None);
    }

    fn complete(
        field: RecipientField,
        value: &str,
        selection: Range<usize>,
        address: &Address,
    ) -> String {
        let query = RecipientQuery::new(field, value, selection.clone()).unwrap();
        assert_eq!(query.field, field);
        let (range, replacement) = query.replacement(value, selection, address).unwrap();
        let mut result = value.to_owned();
        result.replace_range(range, &replacement);
        result
    }

    #[test]
    fn completes_each_recipient_field_after_existing_reply_recipients() {
        for field in [RecipientField::To, RecipientField::Cc, RecipientField::Bcc] {
            let value = "Sender <sender@example.com>, ma";
            let result = complete(
                field,
                value,
                value.len()..value.len(),
                &Address::named("Maya Chen", "maya@example.com"),
            );
            assert_eq!(
                result,
                "Sender <sender@example.com>, Maya Chen <maya@example.com>, "
            );
            assert_eq!(Address::parse_list(&result).len(), 2);
        }
    }

    #[test]
    fn editing_first_or_middle_recipient_preserves_later_recipients() {
        let address = Address::named("Maya", "maya@example.com");
        let value = "ma, last@example.com";
        let query = RecipientQuery::new(RecipientField::To, value, 2..2).unwrap();
        assert_eq!(query.text(), "ma");
        assert_eq!(
            complete(RecipientField::To, value, 2..2, &address),
            "Maya <maya@example.com>, last@example.com"
        );

        let value = "first@example.com; ma ; last@example.com";
        let offset = value.find("ma ;").unwrap();
        assert_eq!(
            complete(RecipientField::Cc, value, offset..offset + 2, &address),
            "first@example.com; Maya <maya@example.com> ; last@example.com"
        );
    }

    #[test]
    fn quoted_names_and_unicode_survive_completion_and_draft_parsing() {
        let existing = Address::named("Doe, Jane \"JJ\"", "jane@example.com");
        let target = Address::named("Élodie; Martin", "elodie@example.com");
        let value = format!("{}, Élo", existing.to_header());
        let query =
            RecipientQuery::new(RecipientField::Bcc, &value, value.len()..value.len()).unwrap();
        assert_eq!(query.text(), "Élo");
        let result = complete(
            RecipientField::Bcc,
            &value,
            value.len()..value.len(),
            &target,
        );
        assert_eq!(Address::parse_list(&result), vec![existing, target]);
    }

    #[test]
    fn rejects_stale_text_or_selection_and_multi_recipient_selections() {
        let value = "ma, jo";
        let query = RecipientQuery::new(RecipientField::To, value, 2..2).unwrap();
        let address = Address::new(None, "maya@example.com");
        assert!(query.replacement("may, jo", 2..2, &address).is_none());
        assert!(query.replacement(value, 6..6, &address).is_none());
        assert!(RecipientQuery::new(RecipientField::To, value, 0..6).is_none());
        assert!(RecipientQuery::new(RecipientField::To, "Élo", 1..1).is_none());
        assert!(RecipientQuery::new(RecipientField::To, value, 99..99).is_none());
    }

    #[test]
    fn blank_tokens_do_not_offer_or_replace_anything() {
        for value in ["", " ", "sender@example.com, ", "sender@example.com;"] {
            assert!(
                RecipientQuery::new(RecipientField::Cc, value, value.len()..value.len()).is_none()
            );
        }
    }

    #[test]
    fn completing_a_quoted_partial_replaces_the_whole_name() {
        let value = "\"Doe, Ja\"";
        let query = RecipientQuery::new(RecipientField::To, value, 8..8).unwrap();
        assert_eq!(query.text(), "Doe, Ja");
        let address = Address::named("Doe, Jane", "jane@example.com");
        let result = complete(RecipientField::To, value, 8..8, &address);
        assert_eq!(Address::parse_list(&result), vec![address]);
    }

    #[test]
    fn punctuation_in_contact_names_round_trips_without_changing_recipients() {
        let addresses: Vec<_> = [
            "Doe, Jane",
            "Maya; Research",
            "Alex \"Ace\" Smith",
            "Team \\ Support",
            "Team <Support>",
            "Help (US)",
        ]
        .into_iter()
        .enumerate()
        .map(|(i, name)| Address::named(name, &format!("contact{i}@example.com")))
        .collect();
        let headers = addresses
            .iter()
            .map(Address::to_header)
            .collect::<Vec<_>>()
            .join(", ");
        assert_eq!(Address::parse_list(&headers), addresses);
    }
}
