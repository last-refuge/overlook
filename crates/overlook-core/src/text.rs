//! Small text helpers shared across crates.

/// `s` with its first character in upper case ("work" → "Work"); empty stays empty.
pub fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::capitalize;

    #[test]
    fn first_character_only() {
        assert_eq!(capitalize("work"), "Work");
        assert_eq!(capitalize("Work"), "Work");
        assert_eq!(capitalize("ßtraße"), "SStraße");
        assert_eq!(capitalize(""), "");
    }
}
