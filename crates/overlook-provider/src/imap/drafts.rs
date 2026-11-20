use super::*;

fn descriptor(path: &str, validity: u32, uid: u32) -> RemoteDraft {
    let id = MessageRef::provider_id(path, validity, uid);
    RemoteDraft {
        revision: id.0.clone(),
        id,
        thread_id: None,
    }
}

fn draft_folder(conn: &mut Conn, create: bool) -> Result<Option<String>, ProviderError> {
    if let Some(path) = conn.drafts_path.clone() {
        return Ok(Some(path));
    }
    let folders = conn.list_folders()?;
    if let Some(folder) = folders::drafts_folder(&folders) {
        conn.drafts_path = Some(folder.path.clone());
        return Ok(Some(folder.path.clone()));
    }
    if !create {
        return Ok(None);
    }
    let path = if folders
        .iter()
        .any(|f| f.delimiter.as_deref() == Some(".") && f.path.starts_with("INBOX."))
    {
        "INBOX.Drafts"
    } else {
        "Drafts"
    };
    conn.create_folder(path)?;
    conn.drafts_path = Some(path.into());
    Ok(Some(path.into()))
}

/// Without UIDPLUS, leave only this UID marked deleted. Plain EXPUNGE would also
/// erase unrelated messages another client has marked for deletion.
fn remove(conn: &mut Conn, remote: &RemoteDraft) -> Result<(), ProviderError> {
    let r = MessageRef::parse(&remote.id)
        .ok_or_else(|| ProviderError::Rejected("Invalid IMAP draft identity".into()))?;
    let mailbox = if conn.drafts_path.as_deref() == Some(r.folder.as_str()) {
        match conn.select(&r.folder) {
            Ok(m) => m,
            Err(ProviderError::Rejected(_)) => {
                conn.drafts_path = None;
                if !conn
                    .list_folders()?
                    .iter()
                    .any(|f| f.path == r.folder && f.selectable())
                {
                    return Ok(());
                }
                conn.select(&r.folder)?
            }
            Err(e) => return Err(e),
        }
    } else {
        if !conn
            .list_folders()?
            .iter()
            .any(|f| f.path == r.folder && f.selectable())
        {
            return Ok(());
        }
        conn.select(&r.folder)?
    };
    if mailbox.uid_validity != Some(r.uidvalidity) {
        return Ok(());
    }
    if let Err(e) = conn.session().uid_store(r.uid.to_string(), "+FLAGS.SILENT (\\Deleted)") {
        return Err(conn.err(&e));
    }
    if conn.caps.uidplus {
        if let Err(e) = conn.session().uid_expunge(r.uid.to_string()) {
            return Err(conn.err(&e));
        }
    }
    Ok(())
}

impl ImapProvider {
    pub(super) fn draft_listing(&self) -> Result<Vec<RemoteDraft>, ProviderError> {
        self.with_session(|conn| {
            let Some(mut path) = draft_folder(conn, false)? else {
                return Ok(Vec::new());
            };
            let mailbox = match conn.examine(&path) {
                Ok(m) => m,
                Err(ProviderError::Rejected(_)) => {
                    conn.drafts_path = None;
                    let Some(fresh) = draft_folder(conn, false)? else {
                        return Ok(Vec::new());
                    };
                    path = fresh;
                    conn.examine(&path)?
                }
                Err(e) => return Err(e),
            };
            let validity = mailbox.uid_validity.ok_or_else(|| {
                ProviderError::Temporary("Drafts mailbox has no UIDVALIDITY".into())
            })?;
            let uids: BTreeSet<u32> = match conn.session().uid_search("UNDELETED") {
                Ok(u) => u.into_iter().collect(),
                Err(e) => return Err(conn.err(&e)),
            };
            Ok(uids
                .into_iter()
                .map(|uid| descriptor(&path, validity, uid))
                .collect())
        })
    }

    pub(super) fn draft_content(
        &self,
        remote: &RemoteDraft,
    ) -> Result<Option<Vec<u8>>, ProviderError> {
        let r = MessageRef::parse(&remote.id)
            .ok_or_else(|| ProviderError::Rejected("Invalid IMAP draft identity".into()))?;
        self.with_session(|conn| {
            let mailbox = conn.examine(&r.folder)?;
            if mailbox.uid_validity != Some(r.uidvalidity) {
                return Ok(None);
            }
            let sizes = match conn.session().uid_fetch(r.uid.to_string(), "(UID FLAGS RFC822.SIZE)") {
                Ok(s) => s,
                Err(e) => return Err(conn.err(&e)),
            };
            let Some(info) = sizes
                .iter()
                .find(|f| f.uid == Some(r.uid) && !f.flags().contains(&Flag::Deleted))
            else {
                return Ok(None);
            };
            if info
                .size
                .is_none_or(|size| size as usize > crate::drafts::MAX_DRAFT_BYTES)
            {
                return Err(ProviderError::Rejected(
                    "This server draft exceeds the message size limit or has no size".into(),
                ));
            }
            let fetches = match conn.session().uid_fetch(
                r.uid.to_string(),
                format!(
                    "(UID FLAGS BODY.PEEK[]<0.{}>)",
                    crate::drafts::MAX_DRAFT_BYTES + 1
                ),
            ) {
                Ok(f) => f,
                Err(e) => return Err(conn.err(&e)),
            };
            Ok(fetches
                .iter()
                .find(|f| f.uid == Some(r.uid) && !f.flags().contains(&Flag::Deleted))
                .and_then(|f| f.body().map(<[u8]>::to_vec)))
        })
    }

    pub(super) fn write_draft(
        &self,
        remote: Option<&RemoteDraft>,
        raw: &[u8],
        message_id: &str,
    ) -> Result<Option<RemoteDraft>, ProviderError> {
        self.with_session(|conn| {
            let Some(mut path) = draft_folder(conn, true)? else { return Err(ProviderError::Temporary("Could not find the Drafts mailbox".into())) };
            let mailbox = match conn.select(&path) {
                Ok(m) => m,
                Err(ProviderError::Rejected(_)) => {
                    conn.drafts_path = None;
                    let Some(fresh) = draft_folder(conn, true)? else { return Err(ProviderError::Temporary("Could not find the Drafts mailbox".into())) };
                    path = fresh;
                    conn.select(&path)?
                }
                Err(e) => return Err(e),
            };
            let validity = mailbox.uid_validity.ok_or_else(|| ProviderError::Temporary("Drafts mailbox has no UIDVALIDITY".into()))?;
            let clean: String = message_id.chars().filter(|c| !matches!(c, '"' | '\\' | '\r' | '\n')).collect();
            let found = match conn.session().uid_search(format!("UNDELETED HEADER Message-ID \"{clean}\"")) {
                Ok(u) => u.into_iter().min(),
                Err(e) => return Err(conn.err(&e)),
            };
            let uid = if let Some(uid) = found {
                let saved = match conn.session().uid_fetch(uid.to_string(), "(UID BODY.PEEK[])") {
                    Ok(s) => s,
                    Err(e) => return Err(conn.err(&e)),
                };
                let matches = saved.iter().find(|f| f.uid == Some(uid)).and_then(|f| f.body()).map(|bytes| crate::drafts::same_content(bytes, raw)).transpose()?.unwrap_or(false);
                if !matches { return Ok(None); }
                uid
            } else {
                if let Some(remote) = remote {
                    let r = MessageRef::parse(&remote.id).ok_or_else(|| ProviderError::Rejected("Invalid IMAP draft identity".into()))?;
                    if r.folder != path || r.uidvalidity != validity { return Ok(None); }
                    let found = match conn.session().uid_fetch(r.uid.to_string(), "(UID FLAGS)") {
                        Ok(f) => f,
                        Err(e) => return Err(conn.err(&e)),
                    };
                    if !found.iter().any(|f| f.uid == Some(r.uid) && !f.flags().contains(&Flag::Deleted)) { return Ok(None); }
                }
                let appended = match conn.session().append(&path, raw).flag(Flag::Draft).flag(Flag::Seen).finish() {
                    Ok(a) => a,
                    Err(e) => return Err(conn.err(&e)),
                };
                let uid = appended.uids.as_ref().and_then(|u| u.first()).map(|u| match u { UidSetMember::Uid(u) => *u, UidSetMember::UidRange(r) => *r.start() });
                match uid {
                    Some(uid) if appended.uid_validity == Some(validity) => uid,
                    _ => conn.find_by_message_id(message_id)?.ok_or_else(|| ProviderError::Temporary("Could not locate the appended draft; it will be reconciled before retrying".into()))?,
                }
            };
            let saved = descriptor(&path, validity, uid);
            if let Some(old) = remote.filter(|old| old.id != saved.id) { remove(conn, old)?; }
            Ok(Some(saved))
        })
    }

    pub(super) fn remove_draft(&self, remote: &RemoteDraft) -> Result<(), ProviderError> {
        self.with_session(|conn| remove(conn, remote))
    }
}
