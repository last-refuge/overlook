//! IMAP folders as [`Label`] rows, per the plan's "Folders as labels" table.
//!
//! `INBOX` is always the Inbox. Sent, Trash, Spam and Archive are found by the account's
//! `ImapPolicy` overrides first, then RFC 6154 SPECIAL-USE attributes (`\Archive` before an
//! `\All` view), then common names; the first candidate wins. `\Drafts`, `\Flagged`,
//! `\All`-less `\Important` and other
//! virtual folders are not mapped as mail (Drafts has its own synchronization). Everything else is a user
//! label whose name is the path with the delimiter shown as " / ".

use imap_proto::types::NameAttribute;
use overlook_core::*;

/// One `LIST` line, plus whether `LSUB` listed it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FolderInfo {
    pub path: String,
    pub delimiter: Option<String>,
    /// Raw attribute names as the server sent them (`\Sent`, `\HasChildren`, …).
    pub attributes: Vec<String>,
    pub subscribed: bool,
}

impl FolderInfo {
    fn has(&self, attribute: &str) -> bool {
        self.attributes.iter().any(|a| a.eq_ignore_ascii_case(attribute))
    }

    /// Whether `SELECT` would work.
    pub fn selectable(&self) -> bool {
        !self.has("\\Noselect") && !self.has("\\NonExistent")
    }

    /// Whether the server allows folders inside this one (RFC 3501 `\Noinferiors`).
    pub fn allows_children(&self) -> bool {
        !self.has("\\Noinferiors")
    }

    /// The SPECIAL-USE attribute, if any (`\Sent`, `\Trash`, `\Junk`, `\Archive`, `\All`,
    /// `\Drafts`, `\Flagged`, `\Important`).
    pub fn special_use(&self) -> Option<String> {
        const SPECIAL: [&str; 8] = ["\\Sent", "\\Trash", "\\Junk", "\\Archive", "\\All", "\\Drafts", "\\Flagged", "\\Important"];
        SPECIAL.iter().find(|s| self.has(s)).map(|s| (*s).to_owned())
    }

    /// The last path component, lower-cased, for name heuristics.
    fn leaf(&self) -> String {
        match &self.delimiter {
            Some(d) if !d.is_empty() => self.path.rsplit(d.as_str()).next().unwrap_or(&self.path),
            _ => self.path.as_str(),
        }
        .trim()
        .to_ascii_lowercase()
    }

    /// Whether name heuristics may apply: top level, or directly under INBOX / [Gmail].
    fn near_top(&self) -> bool {
        let Some(d) = self.delimiter.as_deref().filter(|d| !d.is_empty()) else { return true };
        let parts: Vec<&str> = self.path.split(d).collect();
        match parts.len() {
            1 => true,
            2 => {
                let head = parts[0].to_ascii_lowercase();
                head == "inbox" || head == "[gmail]" || head == "[google mail]"
            }
            _ => false,
        }
    }

    /// Display name: path with the delimiter shown as " / ", without a leading `INBOX`
    /// namespace (Courier/Dovecot "INBOX.Work" shows as "Work").
    pub fn display_name(&self) -> String {
        self.display_name_in(true)
    }

    /// Display name when the server does (`inbox_namespace`) or does not keep every folder
    /// under INBOX. Without that namespace a folder inside the Inbox shows as "Inbox / Work".
    pub fn display_name_in(&self, inbox_namespace: bool) -> String {
        let path = match &self.delimiter {
            Some(d) if !d.is_empty() => {
                let prefix = format!("INBOX{d}");
                let under_inbox = self.path.len() > prefix.len() && self.path[..prefix.len()].eq_ignore_ascii_case(&prefix);
                if under_inbox {
                    let rest = self.path[prefix.len()..].split(d.as_str()).collect::<Vec<_>>().join(" / ");
                    if inbox_namespace { rest } else { format!("Inbox / {rest}") }
                } else {
                    self.path.split(d.as_str()).collect::<Vec<_>>().join(" / ")
                }
            }
            _ => self.path.clone(),
        };
        if path.is_empty() {
            self.path.clone()
        } else {
            decode_mutf7(&path)
        }
    }
}

/// The modified-UTF-7 alphabet (RFC 3501 §5.1.3): base64 with ',' for '/', no padding.
const MUTF7: base64::engine::GeneralPurpose = base64::engine::GeneralPurpose::new(
    &match base64::alphabet::Alphabet::new("ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+,") {
        Ok(a) => a,
        Err(_) => panic!("valid alphabet"),
    },
    base64::engine::GeneralPurposeConfig::new().with_encode_padding(false).with_decode_padding_mode(base64::engine::DecodePaddingMode::RequireNone),
);

/// A mailbox name as IMAP4rev1 sends it: printable ASCII as is, "&" as "&-", anything
/// else as UTF-16 in modified base64 between "&" and "-".
pub fn encode_mutf7(name: &str) -> String {
    use base64::Engine;
    let mut out = String::new();
    let mut run: Vec<u16> = Vec::new();
    let flush = |run: &mut Vec<u16>, out: &mut String| {
        if !run.is_empty() {
            let bytes: Vec<u8> = run.iter().flat_map(|u| u.to_be_bytes()).collect();
            out.push('&');
            out.push_str(&MUTF7.encode(bytes));
            out.push('-');
            run.clear();
        }
    };
    for c in name.chars() {
        if (' '..='~').contains(&c) {
            flush(&mut run, &mut out);
            if c == '&' { out.push_str("&-") } else { out.push(c) }
        } else {
            let mut buf = [0u16; 2];
            run.extend_from_slice(c.encode_utf16(&mut buf));
        }
    }
    flush(&mut run, &mut out);
    out
}

/// The reverse of [`encode_mutf7`]. A name that is not valid modified UTF-7 comes back
/// unchanged, so a server that sends raw UTF-8 still shows readable names.
pub fn decode_mutf7(name: &str) -> String {
    use base64::Engine;
    let mut out = String::new();
    let mut rest = name;
    while let Some(start) = rest.find('&') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('-') else { return name.to_owned() };
        let chunk = &after[..end];
        if chunk.is_empty() {
            out.push('&');
        } else {
            let Ok(bytes) = MUTF7.decode(chunk) else { return name.to_owned() };
            if bytes.len() % 2 != 0 {
                return name.to_owned();
            }
            let units: Vec<u16> = bytes.chunks(2).map(|b| u16::from_be_bytes([b[0], b[1]])).collect();
            let Ok(text) = String::from_utf16(&units) else { return name.to_owned() };
            out.push_str(&text);
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

pub fn attribute_name(a: &NameAttribute<'_>) -> String {
    match a {
        NameAttribute::NoInferiors => "\\Noinferiors".into(),
        NameAttribute::NoSelect => "\\Noselect".into(),
        NameAttribute::Marked => "\\Marked".into(),
        NameAttribute::Unmarked => "\\Unmarked".into(),
        NameAttribute::All => "\\All".into(),
        NameAttribute::Archive => "\\Archive".into(),
        NameAttribute::Drafts => "\\Drafts".into(),
        NameAttribute::Flagged => "\\Flagged".into(),
        NameAttribute::Junk => "\\Junk".into(),
        NameAttribute::Sent => "\\Sent".into(),
        NameAttribute::Trash => "\\Trash".into(),
        NameAttribute::Extension(s) => s.to_string(),
        other => format!("{other:?}"),
    }
}

/// The mailbox name of a `LIST`, `LSUB` or `STATUS` line as the folder is really called.
/// `name` is what the response parser returned for `line`: a literal (`{N}` followed by
/// the raw bytes, which some servers use for names with 8-bit or special characters) is
/// already the real name, while a quoted string still carries its escapes.
pub fn decode_name(line: &[u8], name: &str) -> String {
    if sent_as_literal(line, name) {
        name.to_owned()
    } else {
        unescape_name(name)
    }
}

/// Whether `line` carries `name` as a literal: `{N}\r\n` directly followed by its bytes.
/// A quoted name can't look like that, since CR and LF are not allowed inside quotes.
fn sent_as_literal(line: &[u8], name: &str) -> bool {
    let marker = format!("{{{}}}\r\n{name}", name.len());
    line.windows(marker.len()).any(|w| w == marker.as_bytes())
}

/// A quoted mailbox name with the quoted-string escapes removed. The response parser
/// keeps `\\` and `\"` as sent, so a folder called `Back\slash` would otherwise be
/// remembered as `Back\\slash` and every command naming it (which escapes it again)
/// would miss. An unquoted (atom) name has no escapes.
pub fn unescape_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut chars = name.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some(next) => out.push(next),
                None => out.push(c),
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// What a folder is to Overlook. Not every folder is shown: Drafts and virtual Gmail views
/// are left out, and an unselectable container (`\Noselect`) is kept, as
/// [`LabelKind::Container`], only when user folders sit inside it. Dovecot and Courier
/// make such containers whenever a child is created without its parent, and keep them
/// when a parent is deleted with children left; `[Gmail]`, whose children are all system
/// folders, is dropped.
pub fn classify(folders: &[FolderInfo], policy: Option<&ImapPolicy>) -> Vec<(FolderInfo, LabelKind)> {
    let mut out: Vec<(FolderInfo, LabelKind)> = Vec::new();
    let taken = |out: &Vec<(FolderInfo, LabelKind)>, kind: LabelKind| out.iter().any(|(_, k)| *k == kind);
    let override_for = |path: &str| -> Option<LabelKind> {
        let p = policy?;
        if p.sent_folder.as_deref() == Some(path) {
            Some(LabelKind::Sent)
        } else if p.trash_folder.as_deref() == Some(path) {
            Some(LabelKind::Trash)
        } else if p.archive_folder.as_deref() == Some(path) {
            Some(LabelKind::Archive)
        } else {
            None
        }
    };
    // An `\Archive` folder is the Archive even when an `\All` view is listed first, as
    // Proton Mail Bridge lists All Mail beside its Archive.
    let archive_marked = folders.iter().any(|f| f.selectable() && f.special_use().as_deref() == Some("\\Archive"));
    // Three passes so an override beats SPECIAL-USE, which beats a name.
    for pass in 0..3 {
        for f in folders {
            if !f.selectable() || out.iter().any(|(o, _)| o.path == f.path) {
                continue;
            }
            if f.path.eq_ignore_ascii_case("INBOX") {
                if pass == 0 {
                    out.push((f.clone(), LabelKind::Inbox));
                }
                continue;
            }
            let kind = match pass {
                0 => override_for(&f.path),
                1 => match f.special_use().as_deref() {
                    Some("\\Sent") => Some(LabelKind::Sent),
                    Some("\\Trash") => Some(LabelKind::Trash),
                    Some("\\Junk") => Some(LabelKind::Spam),
                    Some("\\Archive") => Some(LabelKind::Archive),
                    Some("\\All") if !archive_marked => Some(LabelKind::Archive),
                    _ => None,
                },
                _ => match f.near_top().then(|| f.leaf()).as_deref() {
                    Some("sent" | "sent items" | "sent messages" | "sent mail" | "sent-mail") => Some(LabelKind::Sent),
                    Some("trash" | "deleted items" | "deleted messages" | "deleted" | "bin") => Some(LabelKind::Trash),
                    Some("junk" | "spam" | "junk e-mail" | "junk email" | "bulk mail") => Some(LabelKind::Spam),
                    Some("archive" | "archives" | "all mail") => Some(LabelKind::Archive),
                    _ => None,
                },
            };
            if let Some(kind) = kind {
                if !taken(&out, kind) {
                    out.push((f.clone(), kind));
                }
            }
        }
    }
    for f in folders {
        if !f.selectable() || out.iter().any(|(o, _)| o.path == f.path) {
            continue;
        }
        let virtual_view = matches!(f.special_use().as_deref(), Some("\\Drafts" | "\\Flagged" | "\\Important" | "\\All" | "\\Sent" | "\\Trash" | "\\Junk" | "\\Archive"));
        let drafts_name = f.near_top() && matches!(f.leaf().as_str(), "drafts" | "draft" | "draft messages");
        if virtual_view || drafts_name {
            continue;
        }
        out.push((f.clone(), LabelKind::User));
    }
    for f in folders {
        if f.selectable() || f.path.eq_ignore_ascii_case("INBOX") || out.iter().any(|(o, _)| o.path == f.path) {
            continue;
        }
        let inside = |path: &str| match &f.delimiter {
            Some(d) if !d.is_empty() => path.len() > f.path.len() + d.len() && path.starts_with(&f.path) && path[f.path.len()..].starts_with(d.as_str()),
            _ => false,
        };
        if out.iter().any(|(o, k)| *k == LabelKind::User && inside(&o.path)) {
            out.push((f.clone(), LabelKind::Container));
        }
    }
    out
}

/// SPECIAL-USE works for localized and namespaced mailboxes. Common names cover
/// servers without that extension; never mistake an arbitrary nested folder for Drafts.
pub fn drafts_folder(folders: &[FolderInfo]) -> Option<&FolderInfo> {
    folders.iter().find(|f| f.selectable() && f.has("\\Drafts"))
        .or_else(|| folders.iter().find(|f| f.selectable() && f.near_top() && matches!(f.leaf().as_str(), "draft" | "drafts" | "draft messages")))
}

/// The `INBOX.` or `INBOX/` prefix every folder but the Inbox lives under (Courier, some
/// Dovecot setups), or `None` when the server keeps folders at the top level.
pub fn inbox_namespace_prefix<'a>(paths: impl IntoIterator<Item = &'a str>) -> Option<&'static str> {
    let paths: Vec<&str> = paths.into_iter().collect();
    ["INBOX.", "INBOX/"].into_iter().find(|prefix| !paths.is_empty() && paths.iter().all(|p| p.len() > prefix.len() && p[..prefix.len()].eq_ignore_ascii_case(prefix)))
}

/// Whether the server keeps every folder under INBOX; see [`inbox_namespace_prefix`].
pub fn inbox_namespace<'a>(paths: impl IntoIterator<Item = &'a str>) -> bool {
    inbox_namespace_prefix(paths).is_some()
}

pub fn label_id(account: &AccountId, path: &str) -> LabelId {
    LabelId(format!("{}:{path}", account.0))
}

/// Labels in a stable order: system folders first, then user folders by name.
pub fn labels_for(account: &AccountId, classified: &[(FolderInfo, LabelKind)]) -> Vec<Label> {
    let rank = |k: LabelKind| match k {
        LabelKind::Inbox => 0,
        LabelKind::Sent => 1,
        LabelKind::Archive => 2,
        LabelKind::Trash => 3,
        LabelKind::Spam => 4,
        _ => 100,
    };
    let namespace = inbox_namespace(classified.iter().filter(|(_, k)| *k != LabelKind::Inbox).map(|(f, _)| f.path.as_str()));
    let mut labels: Vec<Label> = classified
        .iter()
        .map(|(f, kind)| Label {
            id: label_id(account, &f.path),
            account_id: account.clone(),
            kind: *kind,
            name: match kind {
                LabelKind::Inbox => "Inbox".into(),
                LabelKind::Sent => "Sent".into(),
                LabelKind::Archive => "Archive".into(),
                LabelKind::Trash => "Trash".into(),
                LabelKind::Spam => "Spam".into(),
                _ => f.display_name_in(namespace),
            },
            provider_id: Some(ProviderId(f.path.clone())),
            color: None,
            sort_order: rank(*kind),
        })
        .collect();
    labels.sort_by(|a, b| a.sort_order.cmp(&b.sort_order).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    for (i, l) in labels.iter_mut().enumerate() {
        l.sort_order = i as i32;
    }
    labels
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(path: &str, attrs: &[&str]) -> FolderInfo {
        FolderInfo { path: path.into(), delimiter: Some("/".into()), attributes: attrs.iter().map(|a| a.to_string()).collect(), subscribed: true }
    }

    #[test]
    fn special_use_then_names_then_user_labels() {
        let folders = vec![
            f("INBOX", &["\\HasNoChildren"]),
            f("Sent Items", &[]),
            f("Sent", &["\\Sent"]),
            f("Trash", &["\\Trash"]),
            f("Junk", &["\\Junk"]),
            f("Drafts", &["\\Drafts"]),
            f("Archive", &[]),
            f("Work", &["\\HasChildren"]),
            f("Work/Clients", &[]),
            f("[Gmail]", &["\\Noselect", "\\HasChildren"]),
            f("Deep/Nested/Trash", &[]),
        ];
        let classified = classify(&folders, None);
        let kinds: Vec<(&str, LabelKind)> = classified.iter().map(|(f, k)| (f.path.as_str(), *k)).collect();
        assert!(kinds.contains(&("INBOX", LabelKind::Inbox)));
        assert!(kinds.contains(&("Sent", LabelKind::Sent)), "SPECIAL-USE beats the name match: {kinds:?}");
        assert!(kinds.contains(&("Sent Items", LabelKind::User)), "the second candidate becomes a user label");
        assert!(kinds.contains(&("Trash", LabelKind::Trash)));
        assert!(kinds.contains(&("Junk", LabelKind::Spam)));
        assert!(kinds.contains(&("Archive", LabelKind::Archive)), "name heuristic");
        assert!(!kinds.iter().any(|(p, _)| *p == "Drafts"), "drafts synchronize separately from received mail");
        assert!(!kinds.iter().any(|(p, _)| *p == "[Gmail]"), "unselectable containers are skipped");
        assert!(kinds.contains(&("Work/Clients", LabelKind::User)));
        assert!(kinds.contains(&("Deep/Nested/Trash", LabelKind::User)), "names only count near the top");

        let account = AccountId::from("imap:j");
        let labels = labels_for(&account, &classified);
        assert_eq!(labels[0].name, "Inbox");
        assert_eq!(labels[0].id.as_str(), "imap:j:INBOX");
        assert_eq!(labels[0].provider_id.as_ref().unwrap().as_str(), "INBOX");
        let names: Vec<&str> = labels.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, ["Inbox", "Sent", "Archive", "Trash", "Spam", "Deep / Nested / Trash", "Sent Items", "Work", "Work / Clients"]);
        assert!(labels.iter().enumerate().all(|(i, l)| l.sort_order == i as i32));
    }

    #[test]
    fn policy_overrides_and_inbox_namespace() {
        let mut folders = vec![f("INBOX", &[]), f("INBOX.Sent", &["\\Sent"]), f("INBOX.Old", &[]), f("INBOX.Archive", &[])];
        for x in &mut folders {
            x.delimiter = Some(".".into());
        }
        let policy = ImapPolicy { archive_folder: Some("INBOX.Old".into()), ..Default::default() };
        let classified = classify(&folders, Some(&policy));
        let find = |p: &str| classified.iter().find(|(f, _)| f.path == p).map(|(_, k)| *k);
        assert_eq!(find("INBOX.Old"), Some(LabelKind::Archive), "the override wins");
        assert_eq!(find("INBOX.Archive"), Some(LabelKind::User), "only one Archive");
        assert_eq!(find("INBOX.Sent"), Some(LabelKind::Sent));
        let labels = labels_for(&AccountId::from("a"), &classified);
        assert_eq!(labels.iter().find(|l| l.kind == LabelKind::User).unwrap().name, "Archive", "INBOX namespace is hidden in names");
    }

    #[test]
    fn quoted_string_escapes_are_removed_from_names() {
        assert_eq!(unescape_name("Work/Clients"), "Work/Clients");
        assert_eq!(unescape_name("QA.Special.Back\\\\slash"), "QA.Special.Back\\slash");
        assert_eq!(unescape_name("QA.Special.Quote\\\"Inside"), "QA.Special.Quote\"Inside");
        assert_eq!(unescape_name("trailing\\"), "trailing\\", "a lone backslash at the end stays");
    }

    #[test]
    fn literal_names_keep_their_backslashes() {
        let quoted = b"* LIST (\\HasNoChildren) \"/\" \"QA/Back\\\\slash\"\r\n";
        assert_eq!(decode_name(quoted, "QA/Back\\\\slash"), "QA/Back\\slash");
        let literal = b"* LIST (\\HasNoChildren) \"/\" {13}\r\nQA/Back\\slash\r\n";
        assert_eq!(decode_name(literal, "QA/Back\\slash"), "QA/Back\\slash", "a literal has no escapes to remove");
        let status = b"* STATUS {13}\r\nQA/Back\\slash (MESSAGES 1)\r\n";
        assert_eq!(decode_name(status, "QA/Back\\slash"), "QA/Back\\slash");
        assert_eq!(decode_name(b"* LIST () \"/\" Plain\r\n", "Plain"), "Plain");
    }

    #[test]
    fn unselectable_parents_of_user_folders_are_containers() {
        let folders = vec![
            f("INBOX", &[]),
            f("Projects", &["\\Noselect", "\\HasChildren"]),
            f("Projects/2026", &["\\Noselect", "\\HasChildren"]),
            f("Projects/2026/Taxes", &["\\HasNoChildren"]),
            f("Old", &["\\Noselect", "\\HasChildren"]),
            f("Old/Sent", &["\\Sent"]),
            f("Empty", &["\\Noselect", "\\HasNoChildren"]),
            f("Projectsother", &["\\HasNoChildren"]),
        ];
        let classified = classify(&folders, None);
        let kinds: Vec<(&str, LabelKind)> = classified.iter().map(|(f, k)| (f.path.as_str(), *k)).collect();
        assert!(kinds.contains(&("Projects", LabelKind::Container)));
        assert!(kinds.contains(&("Projects/2026", LabelKind::Container)));
        assert!(kinds.contains(&("Projects/2026/Taxes", LabelKind::User)));
        assert!(!kinds.iter().any(|(p, _)| *p == "Old"), "a container of system folders only is dropped: {kinds:?}");
        assert!(!kinds.iter().any(|(p, _)| *p == "Empty"), "a container with nothing inside is dropped");
        let labels = labels_for(&AccountId::from("imap:j"), &classified);
        let names: Vec<(&str, LabelKind)> = labels.iter().map(|l| (l.name.as_str(), l.kind)).collect();
        assert_eq!(names, [("Inbox", LabelKind::Inbox), ("Sent", LabelKind::Sent), ("Projects", LabelKind::Container), ("Projects / 2026", LabelKind::Container), ("Projects / 2026 / Taxes", LabelKind::User), ("Projectsother", LabelKind::User)]);
    }

    #[test]
    fn gmail_layout() {
        let folders = vec![
            f("INBOX", &[]),
            f("[Gmail]", &["\\Noselect", "\\HasChildren"]),
            f("[Gmail]/All Mail", &["\\All", "\\HasNoChildren"]),
            f("[Gmail]/Drafts", &["\\Drafts"]),
            f("[Gmail]/Important", &["\\Important"]),
            f("[Gmail]/Sent Mail", &["\\Sent"]),
            f("[Gmail]/Spam", &["\\Junk"]),
            f("[Gmail]/Starred", &["\\Flagged"]),
            f("[Gmail]/Trash", &["\\Trash"]),
        ];
        let classified = classify(&folders, None);
        let kinds: Vec<(&str, LabelKind)> = classified.iter().map(|(f, k)| (f.path.as_str(), *k)).collect();
        assert_eq!(
            kinds,
            vec![("INBOX", LabelKind::Inbox), ("[Gmail]/All Mail", LabelKind::Archive), ("[Gmail]/Sent Mail", LabelKind::Sent), ("[Gmail]/Spam", LabelKind::Spam), ("[Gmail]/Trash", LabelKind::Trash)]
        );
    }

    #[test]
    fn proton_bridge_layout() {
        // Bridge lists All Mail and Starred views beside the real folders, and puts the
        // person's folders and labels under two containers.
        let folders = vec![
            f("All Mail", &["\\All"]),
            f("Archive", &["\\Archive"]),
            f("Drafts", &["\\Drafts"]),
            f("Folders", &["\\Noselect", "\\HasChildren"]),
            f("Folders/Work", &[]),
            f("INBOX", &[]),
            f("Labels", &["\\Noselect", "\\HasChildren"]),
            f("Labels/Receipts", &[]),
            f("Sent", &["\\Sent"]),
            f("Spam", &["\\Junk"]),
            f("Starred", &["\\Flagged"]),
            f("Trash", &["\\Trash"]),
        ];
        let classified = classify(&folders, None);
        let kinds: Vec<(&str, LabelKind)> = classified.iter().map(|(f, k)| (f.path.as_str(), *k)).collect();
        assert_eq!(
            kinds,
            vec![
                ("INBOX", LabelKind::Inbox),
                ("Archive", LabelKind::Archive),
                ("Sent", LabelKind::Sent),
                ("Spam", LabelKind::Spam),
                ("Trash", LabelKind::Trash),
                ("Folders/Work", LabelKind::User),
                ("Labels/Receipts", LabelKind::User),
                ("Folders", LabelKind::Container),
                ("Labels", LabelKind::Container),
            ],
            "the Archive, not All Mail, is the Archive; the views are left out"
        );
    }

    #[test]
    fn folders_inside_the_inbox_keep_the_inbox_in_their_name_without_a_namespace() {
        let names = |folders: Vec<FolderInfo>| -> Vec<String> {
            labels_for(&AccountId::from("imap:j"), &classify(&folders, None)).into_iter().filter(|l| l.kind == LabelKind::User).map(|l| l.name).collect()
        };
        // Top-level server: a child of INBOX is shown under the Inbox.
        assert_eq!(names(vec![f("INBOX", &[]), f("Work", &[]), f("INBOX/Receipts", &[])]), ["Inbox / Receipts", "Work"]);
        // Everything under INBOX: the prefix is a namespace and is hidden.
        assert_eq!(names(vec![f("INBOX", &[]), f("INBOX/Work", &[]), f("INBOX/Receipts", &[])]), ["Receipts", "Work"]);
        assert!(!f("INBOX", &["\\Noinferiors"]).allows_children());
        assert!(f("INBOX", &["\\HasChildren"]).allows_children());
    }

    #[test]
    fn modified_utf7_round_trips_the_rfc_examples() {
        // RFC 3501 §5.1.3, plus "&" and a character outside the BMP.
        for (plain, wire) in [("Entwürfe", "Entw&APw-rfe"), ("~peter/mail/台北/日本語", "~peter/mail/&U,BTFw-/&ZeVnLIqe-"), ("R&D", "R&-D"), ("Plain", "Plain"), ("📁 Box", "&2D3cwQ- Box")] {
            assert_eq!(encode_mutf7(plain), wire, "{plain}");
            assert_eq!(decode_mutf7(wire), plain, "{wire}");
        }
    }

    #[test]
    fn names_that_are_not_modified_utf7_show_as_sent() {
        assert_eq!(decode_mutf7("Broken &abc"), "Broken &abc");
        assert_eq!(decode_mutf7("Odd &AP-"), "Odd &AP-");
        assert_eq!(f("INBOX/Entw&APw-rfe", &[]).display_name(), "Entwürfe");
    }
}
