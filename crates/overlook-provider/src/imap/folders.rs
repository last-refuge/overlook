//! IMAP folders as [`Label`] rows, per the plan's "Folders as labels" table.
//!
//! `INBOX` is always the Inbox. Sent, Trash, Spam and Archive are found by the account's
//! `ImapPolicy` overrides first, then RFC 6154 SPECIAL-USE attributes, then common names;
//! the first candidate wins. `\Drafts`, `\Flagged`, `\All`-less `\Important` and other
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
    pub fn from_name(name: &imap::types::Name<'_>) -> Self {
        Self { path: name.name().to_owned(), delimiter: name.delimiter().map(str::to_owned), attributes: name.attributes().iter().map(attribute_name).collect(), subscribed: true }
    }

    fn has(&self, attribute: &str) -> bool {
        self.attributes.iter().any(|a| a.eq_ignore_ascii_case(attribute))
    }

    /// Whether `SELECT` would work.
    pub fn selectable(&self) -> bool {
        !self.has("\\Noselect") && !self.has("\\NonExistent")
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
        let path = match &self.delimiter {
            Some(d) if !d.is_empty() => {
                let prefix = format!("INBOX{d}");
                let trimmed = if self.path.len() > prefix.len() && self.path[..prefix.len()].eq_ignore_ascii_case(&prefix) { &self.path[prefix.len()..] } else { self.path.as_str() };
                trimmed.split(d.as_str()).collect::<Vec<_>>().join(" / ")
            }
            _ => self.path.clone(),
        };
        if path.is_empty() {
            self.path.clone()
        } else {
            path
        }
    }
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

/// What a folder is to Overlook. `None` means "not shown" (Drafts, virtual Gmail views,
/// unselectable containers).
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
                    Some("\\Archive") | Some("\\All") => Some(LabelKind::Archive),
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
    out
}

/// SPECIAL-USE works for localized and namespaced mailboxes. Common names cover
/// servers without that extension; never mistake an arbitrary nested folder for Drafts.
pub fn drafts_folder(folders: &[FolderInfo]) -> Option<&FolderInfo> {
    folders.iter().find(|f| f.selectable() && f.has("\\Drafts"))
        .or_else(|| folders.iter().find(|f| f.selectable() && f.near_top() && matches!(f.leaf().as_str(), "draft" | "drafts" | "draft messages")))
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
                _ => f.display_name(),
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
}
