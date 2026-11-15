//! Contacts kept at a CardDAV server: the connection the person agreed to, and each card's
//! version as last read or written. Credentials are never stored here.

use super::*;

fn read_card(r: &Row) -> rusqlite::Result<(String, String, String, String, String)> {
    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
}

fn to_card(row: (String, String, String, String, String)) -> Result<ContactCard> {
    let (contact_id, address_book, url, etag, base) = row;
    Ok(ContactCard { contact_id, address_book, url, etag, base: serde_json::from_str(&base)? })
}

fn upsert_card_tx(tx: &Connection, account: &AccountId, card: &ContactCard) -> Result<()> {
    tx.execute(
        "INSERT INTO contact_cards(account_id, contact_id, address_book, url, etag, base_json) VALUES(?1,?2,?3,?4,?5,?6)
         ON CONFLICT(account_id, contact_id) DO UPDATE SET address_book=excluded.address_book, url=excluded.url, etag=excluded.etag, base_json=excluded.base_json",
        params![account.0, card.contact_id, card.address_book, card.url, card.etag, serde_json::to_string(&card.base)?],
    )?;
    Ok(())
}

/// Contacts with edits queued and not yet sent: changed, added or deleted here.
fn queued_contact_ids(tx: &Connection, account: &AccountId) -> Result<HashSet<String>> {
    let mut stmt = tx.prepare("SELECT kind_json FROM pending_operations WHERE account_id=?1 AND state != 'applied'")?;
    let kinds = stmt.query_map([&account.0], |r| r.get::<_, String>(0))?.collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(kinds
        .iter()
        .filter_map(|json| serde_json::from_str::<OperationKind>(json).ok())
        .filter_map(|kind| kind.contact_id().map(str::to_owned))
        .collect())
}

impl Store {
    pub fn contacts_connection(&self, account: &AccountId) -> Result<Option<ContactsConnection>> {
        self.with_read(|c| {
            Ok(c.query_row("SELECT server_url, username, default_book FROM contact_connections WHERE account_id=?1", [&account.0], |r| {
                Ok(ContactsConnection { account_id: account.clone(), server_url: r.get(0)?, username: r.get(1)?, default_book: r.get(2)? })
            })
            .optional()?)
        })
    }

    /// Saves a connection the person agreed to. Saving it again keeps the cards already read.
    pub fn save_contacts_connection(&self, connection: &ContactsConnection) -> Result<()> {
        self.with_conn(|c| {
            c.execute(
                "INSERT INTO contact_connections(account_id, server_url, username, default_book, created_at) VALUES(?1,?2,?3,?4,?5)
                 ON CONFLICT(account_id) DO UPDATE SET server_url=excluded.server_url, username=excluded.username, default_book=excluded.default_book",
                params![connection.account_id.0, connection.server_url, connection.username, connection.default_book, now()],
            )?;
            Ok(())
        })
    }

    pub fn set_contacts_default_book(&self, account: &AccountId, book: &str) -> Result<()> {
        self.with_conn(|c| {
            c.execute("UPDATE contact_connections SET default_book=?2 WHERE account_id=?1", params![account.0, book])?;
            Ok(())
        })
    }

    /// Stops syncing contacts with the server. The contacts read from it, their cards and the
    /// edits to them not sent yet are forgotten here; nothing changes at the server. Contacts
    /// made on this device and never sent stay.
    pub fn remove_contacts_connection(&self, account: &AccountId) -> Result<()> {
        self.with_tx(|tx| {
            let mut stmt = tx.prepare("SELECT id, kind_json FROM pending_operations WHERE account_id=?1 AND state != 'applied'")?;
            let ops = stmt.query_map([&account.0], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?.collect::<std::result::Result<Vec<_>, _>>()?;
            drop(stmt);
            for (id, json) in ops {
                let from_server = serde_json::from_str::<OperationKind>(&json).ok().and_then(|k| k.contact_id().map(|c| c.starts_with("carddav:"))).unwrap_or(false);
                if from_server {
                    tx.execute("DELETE FROM pending_operations WHERE id=?1", [&id])?;
                }
            }
            tx.execute("DELETE FROM people WHERE account_id=?1 AND id LIKE 'carddav:%'", [&account.0])?;
            tx.execute("DELETE FROM contact_cards WHERE account_id=?1", [&account.0])?;
            tx.execute("DELETE FROM contact_connections WHERE account_id=?1", [&account.0])?;
            Ok(())
        })
    }

    pub fn contact_cards(&self, account: &AccountId) -> Result<Vec<ContactCard>> {
        self.with_read(|c| {
            let mut stmt = c.prepare("SELECT contact_id, address_book, url, etag, base_json FROM contact_cards WHERE account_id=?1 ORDER BY contact_id")?;
            let rows = stmt.query_map([&account.0], read_card)?.collect::<std::result::Result<Vec<_>, _>>()?;
            rows.into_iter().map(to_card).collect()
        })
    }

    pub fn contact_card(&self, account: &AccountId, contact_id: &str) -> Result<Option<ContactCard>> {
        self.with_read(|c| {
            c.query_row("SELECT contact_id, address_book, url, etag, base_json FROM contact_cards WHERE account_id=?1 AND contact_id=?2", params![account.0, contact_id], read_card)
                .optional()?
                .map(to_card)
                .transpose()
        })
    }

    /// The server accepted an edit made here: the card's new version, and the contact as the
    /// server now has it. A contact added here takes the server's id. `sent` is when the edit
    /// that was sent had been saved; a newer edit made meanwhile stays queued.
    pub fn save_synced_contact(&self, account: &AccountId, local_id: &str, sent: Timestamp, card: &ContactCard) -> Result<()> {
        ensure_same_account(&card.base.account_id, account, "contact")?;
        let mut contact = card.base.clone();
        contact.pending = false;
        self.with_tx(|tx| {
            if local_id != contact.id {
                tx.execute("DELETE FROM people WHERE account_id=?1 AND id=?2", params![account.0, local_id])?;
                tx.execute("DELETE FROM contact_cards WHERE account_id=?1 AND contact_id=?2", params![account.0, local_id])?;
            }
            // A contact edited again while this edit was on its way keeps the newer edit.
            let edited_again: bool = tx
                .query_row("SELECT pending FROM people WHERE account_id=?1 AND id=?2 AND updated > ?3", params![account.0, contact.id, sent], |r| r.get::<_, i64>(0))
                .optional()?
                .is_some_and(|p| p != 0);
            if !edited_again {
                upsert_contact_tx(tx, &contact)?;
            }
            upsert_card_tx(tx, account, card)
        })
    }

    pub fn delete_contact_card(&self, account: &AccountId, contact_id: &str) -> Result<()> {
        self.with_conn(|c| {
            c.execute("DELETE FROM contact_cards WHERE account_id=?1 AND contact_id=?2", params![account.0, contact_id])?;
            Ok(())
        })
    }

    /// Replaces what was read from the server with a complete download: every card there, each
    /// with the contact it shows. A contact with an edit not sent yet keeps the edit and the
    /// version it was made against; one deleted here and not yet deleted there stays deleted.
    pub fn apply_contact_download(&self, account: &AccountId, cards: &[ContactCard]) -> Result<()> {
        for card in cards {
            ensure_same_account(&card.base.account_id, account, "contact")?;
        }
        self.with_tx(|tx| {
            let mut held = queued_contact_ids(tx, account)?;
            let mut stmt = tx.prepare("SELECT id FROM people WHERE account_id=?1 AND pending=1")?;
            held.extend(stmt.query_map([&account.0], |r| r.get::<_, String>(0))?.collect::<std::result::Result<Vec<_>, _>>()?);
            drop(stmt);
            let mut stmt = tx.prepare("SELECT contact_id FROM contact_cards WHERE account_id=?1")?;
            let existing = stmt.query_map([&account.0], |r| r.get::<_, String>(0))?.collect::<std::result::Result<Vec<_>, _>>()?;
            drop(stmt);
            for id in existing.iter().filter(|id| !held.contains(*id)) {
                tx.execute("DELETE FROM contact_cards WHERE account_id=?1 AND contact_id=?2", params![account.0, id])?;
            }
            tx.execute("DELETE FROM people WHERE account_id=?1 AND pending=0 AND id LIKE 'carddav:%'", [&account.0])?;
            for card in cards.iter().filter(|c| !held.contains(&c.contact_id)) {
                let mut contact = card.base.clone();
                contact.pending = false;
                upsert_contact_tx(tx, &contact)?;
                upsert_card_tx(tx, account, card)?;
            }
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (Store, AccountId) {
        let store = Store::open_in_memory().unwrap();
        let account = Account { id: AccountId::from("imap:contacts"), provider: ProviderKind::Imap, display_name: "Test".into(), email: "me@example.test".into(), created_at: 0 };
        store.upsert_account(&account).unwrap();
        (store, account.id)
    }

    fn card(account: &AccountId, n: u32, name: &str) -> ContactCard {
        let id = format!("carddav:https://dav.example.test/book/{n}.vcf");
        let mut base = Contact { id: id.clone(), account_id: account.clone(), name: Some(name.into()), in_address_book: true, updated: 1, ..Default::default() };
        base.email = format!("{n}@example.test");
        base.emails = vec![base.email.clone()];
        ContactCard { contact_id: id, address_book: "https://dav.example.test/book/".into(), url: format!("https://dav.example.test/book/{n}.vcf"), etag: format!("\"v{n}\""), base }
    }

    #[test]
    fn a_download_replaces_contacts_but_keeps_edits_not_yet_sent() {
        let (store, account) = store();
        store.save_contacts_connection(&ContactsConnection { account_id: account.clone(), server_url: "https://dav.example.test/".into(), username: "me".into(), default_book: None }).unwrap();
        store.apply_contact_download(&account, &[card(&account, 1, "Maya"), card(&account, 2, "Alex"), card(&account, 3, "Daniel")]).unwrap();
        assert_eq!(store.contacts(&account).unwrap().len(), 3);

        // Maya edited here, Alex deleted here; neither is sent yet.
        let mut maya = store.contact_by_id(&account, &card(&account, 1, "").contact_id).unwrap().unwrap();
        maya.name = Some("Maya Chen".into());
        store.save_contact_locally(&account, &maya).unwrap();
        let alex = store.contact_by_id(&account, &card(&account, 2, "").contact_id).unwrap().unwrap();
        store.delete_contact_locally(&account, &alex).unwrap();

        // The server meanwhile renamed Maya and dropped Daniel.
        let mut renamed = card(&account, 1, "Maya R.");
        renamed.etag = "\"v9\"".into();
        store.apply_contact_download(&account, &[renamed, card(&account, 2, "Alex")]).unwrap();
        let names: Vec<_> = store.contacts(&account).unwrap().into_iter().map(|c| c.display_name()).collect();
        assert_eq!(names, vec!["Maya Chen"], "the edit stays, the deletion stays, Daniel is gone");
        let kept = store.contact_card(&account, &card(&account, 1, "").contact_id).unwrap().unwrap();
        assert_eq!(kept.etag, "\"v1\"", "the edit keeps the version it was made against");
        assert!(store.contact_card(&account, &card(&account, 2, "").contact_id).unwrap().is_some(), "the deletion still needs its card");
        assert!(store.contact_card(&account, &card(&account, 3, "").contact_id).unwrap().is_none());
    }

    #[test]
    fn a_sent_contact_takes_the_server_id_and_a_newer_edit_survives() {
        let (store, account) = store();
        let mut local = Contact::new_local(account.clone(), 10);
        local.name = Some("Sophie".into());
        store.save_contact_locally(&account, &local).unwrap();
        let synced = card(&account, 4, "Sophie");
        let sent = store.contact_by_id(&account, &local.id).unwrap().unwrap().updated;
        store.save_synced_contact(&account, &local.id, sent, &synced).unwrap();
        let all = store.contacts(&account).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!((all[0].id.as_str(), all[0].pending), (synced.contact_id.as_str(), false));

        // Edited again while an older edit was on its way: the newer edit stays queued.
        let mut again = all[0].clone();
        again.name = Some("Sophie Williams".into());
        store.save_contact_locally(&account, &again).unwrap();
        store.save_synced_contact(&account, &synced.contact_id, 0, &synced).unwrap();
        let now_shown = store.contact_by_id(&account, &synced.contact_id).unwrap().unwrap();
        assert_eq!((now_shown.name.as_deref(), now_shown.pending), (Some("Sophie Williams"), true));
    }

    #[test]
    fn disconnecting_forgets_server_contacts_but_keeps_ones_made_here() {
        let (store, account) = store();
        store.save_contacts_connection(&ContactsConnection { account_id: account.clone(), server_url: "https://dav.example.test/".into(), username: "me".into(), default_book: Some("https://dav.example.test/book/".into()) }).unwrap();
        store.apply_contact_download(&account, &[card(&account, 1, "Maya")]).unwrap();
        let mut maya = store.contacts(&account).unwrap().remove(0);
        maya.title = Some("Director".into());
        store.save_contact_locally(&account, &maya).unwrap();
        let mut local = Contact::new_local(account.clone(), 10);
        local.name = Some("Only here".into());
        store.save_contact_locally(&account, &local).unwrap();
        store.remove_contacts_connection(&account).unwrap();
        assert!(store.contacts_connection(&account).unwrap().is_none());
        assert!(store.contact_cards(&account).unwrap().is_empty());
        let left: Vec<_> = store.contacts(&account).unwrap().into_iter().map(|c| c.display_name()).collect();
        assert_eq!(left, vec!["Only here"]);
        let ops = store.pending_operations(&account).unwrap();
        assert!(ops.iter().all(|op| op.kind.contact_id() == Some(local.id.as_str())), "only the device-only contact's edit is left");
    }

    #[test]
    fn removing_the_account_removes_its_connection_and_cards() {
        let (store, account) = store();
        store.save_contacts_connection(&ContactsConnection { account_id: account.clone(), server_url: "https://dav.example.test/".into(), username: "me".into(), default_book: None }).unwrap();
        store.apply_contact_download(&account, &[card(&account, 1, "Maya")]).unwrap();
        store.delete_account(&account).unwrap();
        assert!(store.contacts_connection(&account).unwrap().is_none());
        assert!(store.contact_cards(&account).unwrap().is_empty());
    }
}
