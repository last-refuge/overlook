//! `mailto:` links, as the desktop hands them to the app.

use crate::model::Address;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Mailto {
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub bcc: Vec<Address>,
    pub subject: String,
    pub body: String,
}

/// Parses `mailto:a@b.c,d@e.f?subject=Hi&body=...&cc=...`. Anything that is not a
/// `mailto:` URL returns `None`.
pub fn parse(url: &str) -> Option<Mailto> {
    let trimmed = url.trim();
    let rest = if trimmed.get(..7).is_some_and(|p| p.eq_ignore_ascii_case("mailto:")) {
        &trimmed[7..]
    } else {
        return None;
    };
    let (addresses, query) = rest.split_once('?').unwrap_or((rest, ""));
    let mut m = Mailto { to: Address::parse_list(&percent_decode(addresses)), ..Default::default() };
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let value = percent_decode(value);
        match key.to_ascii_lowercase().as_str() {
            "to" => m.to.extend(Address::parse_list(&value)),
            "cc" => m.cc.extend(Address::parse_list(&value)),
            "bcc" => m.bcc.extend(Address::parse_list(&value)),
            "subject" => m.subject = value,
            "body" => m.body = value,
            _ => {}
        }
    }
    Some(m)
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex(bytes.get(i + 1).copied()), hex(bytes.get(i + 2).copied())) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(b: Option<u8>) -> Option<u8> {
    match b? {
        c @ b'0'..=b'9' => Some(c - b'0'),
        c @ b'a'..=b'f' => Some(c - b'a' + 10),
        c @ b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_addresses_and_fields() {
        let m = parse("mailto:maya@northstar.example?subject=Launch%20plan&cc=alex@northstar.example&body=Hi%20Maya,%0A%0Asee%20attached").unwrap();
        assert_eq!(m.to[0].email, "maya@northstar.example");
        assert_eq!(m.cc[0].email, "alex@northstar.example");
        assert_eq!(m.subject, "Launch plan");
        assert_eq!(m.body, "Hi Maya,\n\nsee attached");
        assert!(parse("https://example.com").is_none());
        assert!(parse("mailto:").unwrap().to.is_empty());
    }

    #[test]
    fn mixed_case_scheme_and_plus_signs_are_preserved() {
        let m = parse("MailTo:user+tag@example.com?cc=team+ops@example.com&subject=C++%20Guide&body=1+1=2").unwrap();
        assert_eq!(m.to[0].email, "user+tag@example.com");
        assert_eq!(m.cc[0].email, "team+ops@example.com");
        assert_eq!(m.subject, "C++ Guide");
        assert_eq!(m.body, "1+1=2");
    }
}
