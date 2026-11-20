//! Contacts kept at a contacts server (CardDAV, or Microsoft Graph for accounts that sign in
//! with Microsoft): the connection the person agreed to, and each contact's version as last
//! read or written. Credentials are never stored here.

use super::*;

/// The id prefixes of contacts read from a contacts server: `carddav:<card URL>` and
/// `graph:<contact URL>`. Contacts made here start as `local:` until the server has them.
const FROM_SERVER: &str = "(id LIKE 'carddav:%' OR id LIKE 'graph:%')";

fn from_server(contact_id: &str) -> bool {
    contact_id.starts_with("carddav:") || contact_id.starts_with("graph:")
}

/// Whether `connection` is still the account's connection: the same server and sign-in, made
/// at the same time. Checked inside the transaction that saves a sync's results, so a sync that
/// finishes after Disconnect, or after connecting again (every Microsoft connection has the
/// same address), saves nothing.
fn connected_tx(tx: &Connection, connection: &ContactsConnection) -> Result<bool> {
    Ok(tx
        .query_row(
            "SELECT 1 FROM contact_connections WHERE account_id=?1 AND server_url=?2 AND username=?3 AND created_at=?4",
            params![connection.account_id.0, connection.server_url, connection.username, connection.connected_at],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

/// Points the queued edits of a contact added here at the id the server gave it: a later
/// edit becomes an update, a deletion a deletion of the server's copy. Returns whether a
/// deletion is among them.
fn move_queued_edits_tx(tx: &Connection, account: &AccountId, local_id: &str, server_id: &str) -> Result<bool> {
    let mut stmt = tx.prepare("SELECT id, kind_json FROM pending_operations WHERE account_id=?1 AND state NOT IN ('applied', 'failed')")?;
    let ops = stmt.query_map([&account.0], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?.collect::<std::result::Result<Vec<_>, _>>()?;
    drop(stmt);
    let mut deleted = false;
    for (id, json) in ops {
        let kind: OperationKind = serde_json::from_str(&json).map_err(|e| {
            tracing::warn!(account = %account.0, op_id = %id, error = %e, "unparseable pending_operations.kind_json in move_queued_edits_tx");
            e
        })?;
        let moved = match kind {
            OperationKind::CreateContact { contact_id } | OperationKind::UpdateContact { contact_id } if contact_id == local_id => OperationKind::UpdateContact { contact_id: server_id.to_owned() },
            OperationKind::DeleteContact { contact_id, .. } if contact_id == local_id => {
                deleted = true;
                OperationKind::DeleteContact { contact_id: server_id.to_owned(), resource_name: Some(server_id.to_owned()) }
            }
            _ => continue,
        };
        tx.execute("UPDATE pending_operations SET kind_json=?2 WHERE id=?1", params![id, serde_json::to_string(&moved)?])?;
    }
    Ok(deleted)
}

fn read_card(r: &Row) -> rusqlite::Result<(String, String, String, String, String)> {
    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
}

fn to_card(row: (String, String, String, String, String)) -> Result<ContactCard> {
    let (contact_id, address_book, url, etag, base) = row;
    Ok(ContactCard { contact_id, address_book, url, etag, base: serde_json::from_str(&base)? })
}

fn upsert_card_tx(tx: &Connection, account: &AccountId, card: &ContactCard) -> Result<()> {
    tx.prepare_cached(
        "INSERT INTO contact_cards(account_id, contact_id, address_book, url, etag, base_json) VALUES(?1,?2,?3,?4,?5,?6)
         ON CONFLICT(account_id, contact_id) DO UPDATE SET address_book=excluded.address_book, url=excluded.url, etag=excluded.etag, base_json=excluded.base_json",
    )?
    .execute(params![account.0, card.contact_id, card.address_book, card.url, card.etag, serde_json::to_string(&card.base)?])?;
    Ok(())
}

/// Contacts with edits queued and not yet sent: changed, added or deleted here. An edit the
/// server refused for good doesn't hold its contact; the server's version is shown again.
fn queued_contact_ids(tx: &Connection, account: &AccountId) -> Result<HashSet<String>> {
    let mut stmt = tx.prepare("SELECT kind_json FROM pending_operations WHERE account_id=?1 AND state NOT IN ('applied', 'failed')")?;
    let kinds = stmt.query_map([&account.0], |r| r.get::<_, String>(0))?.collect::<std::result::Result<Vec<_>, _>>()?;
    let mut out = HashSet::new();
    for json in &kinds {
        let kind: OperationKind = serde_json::from_str(json).map_err(|e| {
            tracing::warn!(account = %account.0, error = %e, "unparseable pending_operations.kind_json in queued_contact_ids");
            e
        })?;
        if let Some(id) = kind.contact_id() {
            out.insert(id.to_owned());
        }
    }
    Ok(out)
}

impl Store {
    pub fn contacts_connection(&self, account: &AccountId) -> Result<Option<ContactsConnection>> {
        self.with_read(|c| {
            Ok(c.query_row("SELECT server_url, username, default_book, credential, created_at FROM contact_connections WHERE account_id=?1", [&account.0], |r| {
                Ok(ContactsConnection {
                    account_id: account.clone(),
                    server_url: r.get(0)?,
                    username: r.get(1)?,
                    default_book: r.get(2)?,
                    credential: ContactsCredential::from_key(&r.get::<_, String>(3)?),
                    connected_at: r.get(4)?,
                })
            })
            .optional()?)
        })
    }

    /// Saves a connection the person agreed to. Saving it again keeps the cards already read
    /// and the time it was first made. Each new connection after a disconnect receives a
    /// strictly greater monotonic `generation` and `created_at` so rapid disconnect/reconnect
    /// or backward clock skew can never collide with an older sync.
    pub fn save_contacts_connection(&self, connection: &ContactsConnection) -> Result<()> {
        let ts = now();
        self.with_tx(|tx| {
            let exists: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM contact_connections WHERE account_id=?1)",
                [&connection.account_id.0],
                |r| r.get(0),
            )?;
            if exists {
                tx.execute(
                    "UPDATE contact_connections SET server_url=?2, username=?3, default_book=?4, credential=?5 WHERE account_id=?1",
                    params![connection.account_id.0, connection.server_url, connection.username, connection.default_book, connection.credential.as_key()],
                )?;
            } else {
                let prev: Option<(i64, Timestamp)> = tx
                    .query_row(
                        "SELECT generation, last_connected_at FROM contact_connection_generations WHERE account_id=?1",
                        [&connection.account_id.0],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .optional()?;
                let (generation, created_at) = match prev {
                    Some((prev_gen, prev_ts)) => (prev_gen + 1, ts.max(prev_ts + 1)),
                    None => (1, ts),
                };
                tx.execute(
                    "INSERT INTO contact_connection_generations(account_id, generation, last_connected_at) VALUES(?1,?2,?3)
                     ON CONFLICT(account_id) DO UPDATE SET generation=excluded.generation, last_connected_at=excluded.last_connected_at",
                    params![connection.account_id.0, generation, created_at],
                )?;
                tx.execute(
                    "INSERT INTO contact_connections(account_id, server_url, username, default_book, credential, created_at, generation) VALUES(?1,?2,?3,?4,?5,?6,?7)",
                    params![connection.account_id.0, connection.server_url, connection.username, connection.default_book, connection.credential.as_key(), created_at, generation],
                )?;
            }
            Ok(())
        })
    }

    /// Sets (or, with `None`, forgets) the address book new contacts go to, while `connection`
    /// is still the account's: a sync of a connection replaced meanwhile mustn't point the new
    /// one at the old server's book.
    pub fn set_contacts_default_book(&self, connection: &ContactsConnection, book: Option<&str>) -> Result<()> {
        self.with_conn(|c| {
            c.execute(
                "UPDATE contact_connections SET default_book=?5 WHERE account_id=?1 AND server_url=?2 AND username=?3 AND created_at=?4",
                params![connection.account_id.0, connection.server_url, connection.username, connection.connected_at, book],
            )?;
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
                let kind: OperationKind = serde_json::from_str(&json).map_err(|e| {
                    tracing::warn!(account = %account.0, op_id = %id, error = %e, "unparseable pending_operations.kind_json in remove_contacts_connection");
                    e
                })?;
                if kind.contact_id().is_some_and(from_server) {
                    tx.execute("DELETE FROM pending_operations WHERE id=?1", [&id])?;
                }
            }
            tx.execute(&format!("DELETE FROM people WHERE account_id=?1 AND {FROM_SERVER}"), [&account.0])?;
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

    /// The server of `connection` accepted an edit made here: the card's new version, and the
    /// contact as the server now has it. A contact added here takes the server's id. `sent` is
    /// when the edit that was sent had been saved; a newer edit made meanwhile stays queued,
    /// and so does a deletion made while a new contact was on its way. Nothing is saved once
    /// `connection` is no longer the account's.
    pub fn save_synced_contact(&self, connection: &ContactsConnection, local_id: &str, sent: Timestamp, card: &ContactCard) -> Result<()> {
        let account = &connection.account_id;
        ensure_same_account(&card.base.account_id, account, "contact")?;
        let mut contact = card.base.clone();
        contact.pending = false;
        self.with_tx(|tx| {
            if !connected_tx(tx, connection)? {
                return Ok(());
            }
            if local_id != contact.id {
                let local: Option<Timestamp> = tx.query_row("SELECT updated FROM people WHERE account_id=?1 AND id=?2", params![account.0, local_id], |r| r.get(0)).optional()?;
                tx.execute("DELETE FROM contact_cards WHERE account_id=?1 AND contact_id=?2", params![account.0, local_id])?;
                match local {
                    // Edited again while it was on its way: the newer edit moves to the
                    // server's id and goes as an update.
                    Some(updated) if updated > sent => {
                        move_queued_edits_tx(tx, account, local_id, &contact.id)?;
                        tx.execute("DELETE FROM people WHERE account_id=?1 AND id=?2", params![account.0, contact.id])?;
                        tx.execute("UPDATE people SET id=?3, pending=1, etag=?4 WHERE account_id=?1 AND id=?2", params![account.0, local_id, contact.id, card.etag])?;
                        return upsert_card_tx(tx, account, card);
                    }
                    Some(_) => {
                        tx.execute("DELETE FROM people WHERE account_id=?1 AND id=?2", params![account.0, local_id])?;
                    }
                    // Deleted while it was on its way: the server's copy goes too, so the
                    // card stays for that deletion and the contact isn't shown again.
                    None => {
                        if move_queued_edits_tx(tx, account, local_id, &contact.id)? {
                            return upsert_card_tx(tx, account, card);
                        }
                    }
                }
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

    /// Records where a contact added here is being sent, and what is sent, before the request
    /// goes: if the reply is lost, the retry knows both. Its version stays empty until the
    /// server's copy is read. Returns `false`, saving nothing, once `connection` is no longer
    /// the account's, so a sync of a replaced connection leaves no record pointing at its
    /// server.
    pub fn save_contact_card(&self, connection: &ContactsConnection, card: &ContactCard) -> Result<bool> {
        let account = &connection.account_id;
        ensure_same_account(&card.base.account_id, account, "contact")?;
        self.with_tx(|tx| {
            if !connected_tx(tx, connection)? {
                return Ok(false);
            }
            upsert_card_tx(tx, account, card)?;
            Ok(true)
        })
    }

    pub fn delete_contact_card(&self, account: &AccountId, contact_id: &str) -> Result<()> {
        self.with_conn(|c| {
            c.execute("DELETE FROM contact_cards WHERE account_id=?1 AND contact_id=?2", params![account.0, contact_id])?;
            Ok(())
        })
    }

    /// The server refused an edit for good: the contact shows the server's version again, as
    /// last read, in place of the edit (or the deletion) it refused. Returns whether there
    /// was a version to show.
    pub fn restore_server_contact(&self, account: &AccountId, contact_id: &str) -> Result<bool> {
        // A contact added here has no version of the server's to go back to, only the record
        // of what was being sent.
        if !from_server(contact_id) {
            return Ok(false);
        }
        self.with_tx(|tx| {
            let base: Option<String> = tx.query_row("SELECT base_json FROM contact_cards WHERE account_id=?1 AND contact_id=?2", params![account.0, contact_id], |r| r.get(0)).optional()?;
            let Some(base) = base else { return Ok(false) };
            let mut contact: Contact = serde_json::from_str(&base)?;
            ensure_same_account(&contact.account_id, account, "contact")?;
            contact.pending = false;
            upsert_contact_tx(tx, &contact)?;
            Ok(true)
        })
    }

    /// Replaces what was read from the server of `connection` with a complete download: every
    /// card there, each with the contact it shows. A contact with an edit not sent yet keeps
    /// the edit and the version it was made against; one deleted here and not yet deleted
    /// there stays deleted. Nothing is saved once `connection` is no longer the account's.
    pub fn apply_contact_download(&self, connection: &ContactsConnection, cards: &[ContactCard]) -> Result<()> {
        let account = &connection.account_id;
        for card in cards {
            ensure_same_account(&card.base.account_id, account, "contact")?;
        }
        self.with_tx(|tx| {
            if !connected_tx(tx, connection)? {
                return Ok(());
            }
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
            tx.execute(&format!("DELETE FROM people WHERE account_id=?1 AND pending=0 AND {FROM_SERVER}"), [&account.0])?;
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

    const SERVER: &str = "https://dav.example.test/";

    fn store() -> (Store, AccountId) {
        let store = Store::open_in_memory().unwrap();
        let account = Account { id: AccountId::from("imap:contacts"), provider: ProviderKind::Imap, display_name: "Test".into(), email: "me@example.test".into(), created_at: 0 };
        store.upsert_account(&account).unwrap();
        (store, account.id)
    }

    /// Connects `account` and returns the connection as the store has it, which a sync uses.
    fn connect_to(store: &Store, account: &AccountId, server: &str, credential: ContactsCredential, default_book: Option<&str>) -> ContactsConnection {
        let connection = ContactsConnection { account_id: account.clone(), server_url: server.into(), username: "me".into(), default_book: default_book.map(str::to_owned), credential, connected_at: 0 };
        store.save_contacts_connection(&connection).unwrap();
        store.contacts_connection(account).unwrap().unwrap()
    }

    fn connect(store: &Store, account: &AccountId) -> ContactsConnection {
        connect_to(store, account, SERVER, ContactsCredential::IncomingMail, None)
    }

    fn queued(store: &Store, account: &AccountId) -> Vec<OperationKind> {
        store.pending_operations(account).unwrap().into_iter().filter(|op| op.state != OperationState::Applied).map(|op| op.kind).collect()
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
        let conn = connect(&store, &account);
        store.apply_contact_download(&conn, &[card(&account, 1, "Maya"), card(&account, 2, "Alex"), card(&account, 3, "Daniel")]).unwrap();
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
        store.apply_contact_download(&conn, &[renamed, card(&account, 2, "Alex")]).unwrap();
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
        let conn = connect(&store, &account);
        let mut local = Contact::new_local(account.clone(), 10);
        local.name = Some("Sophie".into());
        store.save_contact_locally(&account, &local).unwrap();
        let synced = card(&account, 4, "Sophie");
        let sent = store.contact_by_id(&account, &local.id).unwrap().unwrap().updated;
        store.save_synced_contact(&conn, &local.id, sent, &synced).unwrap();
        let all = store.contacts(&account).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!((all[0].id.as_str(), all[0].pending), (synced.contact_id.as_str(), false));

        // Edited again while an older edit was on its way: the newer edit stays queued.
        let mut again = all[0].clone();
        again.name = Some("Sophie Williams".into());
        store.save_contact_locally(&account, &again).unwrap();
        store.save_synced_contact(&conn, &synced.contact_id, 0, &synced).unwrap();
        let now_shown = store.contact_by_id(&account, &synced.contact_id).unwrap().unwrap();
        assert_eq!((now_shown.name.as_deref(), now_shown.pending), (Some("Sophie Williams"), true));
    }

    #[test]
    fn an_edit_made_while_a_new_contact_uploads_moves_to_the_servers_id() {
        let (store, account) = store();
        let conn = connect(&store, &account);
        let mut local = Contact::new_local(account.clone(), 10);
        local.name = Some("Sophie".into());
        store.save_contact_locally(&account, &local).unwrap();
        // While it's on its way, the title is added: another save of the same new contact.
        let mut edited = store.contact_by_id(&account, &local.id).unwrap().unwrap();
        edited.title = Some("Director".into());
        store.save_contact_locally(&account, &edited).unwrap();
        let newer = store.contact_by_id(&account, &local.id).unwrap().unwrap().updated;
        let synced = card(&account, 4, "Sophie");
        store.save_synced_contact(&conn, &local.id, newer - 1, &synced).unwrap();
        let shown = store.contacts(&account).unwrap();
        assert_eq!(shown.len(), 1);
        assert_eq!((shown[0].id.as_str(), shown[0].title.as_deref(), shown[0].pending), (synced.contact_id.as_str(), Some("Director"), true));
        assert_eq!(store.contact_card(&account, &synced.contact_id).unwrap().unwrap().base.title, None, "the card is the version the server has");
        assert!(queued(&store, &account).iter().all(|k| *k == OperationKind::UpdateContact { contact_id: synced.contact_id.clone() }), "the edit goes as an update to the server's copy");
    }

    #[test]
    fn a_new_contact_deleted_while_it_uploads_is_deleted_there_too() {
        let (store, account) = store();
        let conn = connect(&store, &account);
        let mut local = Contact::new_local(account.clone(), 10);
        local.name = Some("Sophie".into());
        store.save_contact_locally(&account, &local).unwrap();
        let sent = store.contact_by_id(&account, &local.id).unwrap().unwrap().updated;
        let row = store.contact_by_id(&account, &local.id).unwrap().unwrap();
        store.delete_contact_locally(&account, &row).unwrap();
        let synced = card(&account, 4, "Sophie");
        store.save_synced_contact(&conn, &local.id, sent, &synced).unwrap();
        assert!(store.contacts(&account).unwrap().is_empty(), "it isn't shown again");
        assert!(store.contact_card(&account, &synced.contact_id).unwrap().is_some(), "the deletion has the card it needs");
        assert!(queued(&store, &account).contains(&OperationKind::DeleteContact { contact_id: synced.contact_id.clone(), resource_name: Some(synced.contact_id.clone()) }));
        // A download meanwhile doesn't bring it back either.
        store.apply_contact_download(&conn, &[synced.clone()]).unwrap();
        assert!(store.contacts(&account).unwrap().is_empty());
    }

    #[test]
    fn a_sync_that_finishes_after_disconnecting_saves_nothing() {
        let (store, account) = store();
        let conn = connect(&store, &account);
        store.remove_contacts_connection(&account).unwrap();
        store.apply_contact_download(&conn, &[card(&account, 1, "Maya")]).unwrap();
        let mut local = Contact::new_local(account.clone(), 10);
        local.name = Some("Only here".into());
        store.save_contact_locally(&account, &local).unwrap();
        store.save_synced_contact(&conn, &local.id, 0, &card(&account, 2, "Only here")).unwrap();
        let shown: Vec<_> = store.contacts(&account).unwrap().into_iter().map(|c| c.id).collect();
        assert_eq!(shown, vec![local.id.clone()], "nothing from the server came back");
        assert!(store.contact_cards(&account).unwrap().is_empty());
        // Connected to another server since: the old sync's results don't belong there.
        connect_to(&store, &account, "https://other.example.test/", ContactsCredential::Separate, None);
        store.apply_contact_download(&conn, &[card(&account, 1, "Maya")]).unwrap();
        assert!(store.contact_cards(&account).unwrap().is_empty());
        // Nor do they belong to a connection made again at the same address (every Microsoft
        // connection has the same one), or to another sign-in there.
        store.remove_contacts_connection(&account).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(3));
        let again = connect(&store, &account);
        assert_ne!(again.connected_at, conn.connected_at);
        store.apply_contact_download(&conn, &[card(&account, 1, "Maya")]).unwrap();
        assert!(store.contact_cards(&account).unwrap().is_empty());
        let other_sign_in = ContactsConnection { username: "someone-else".into(), ..again.clone() };
        store.apply_contact_download(&other_sign_in, &[card(&account, 1, "Maya")]).unwrap();
        assert!(store.contact_cards(&account).unwrap().is_empty());
        store.apply_contact_download(&again, &[card(&account, 1, "Maya")]).unwrap();
        assert_eq!(store.contact_cards(&account).unwrap().len(), 1, "the current connection's results are saved");
    }

    #[test]
    fn a_replaced_connections_sync_writes_nothing_into_the_new_one() {
        let (store, account) = store();
        let old = connect(&store, &account);
        store.remove_contacts_connection(&account).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(3));
        // Made again with the same server and user name: still another connection.
        let new = connect(&store, &account);
        let local = Contact::new_local(account.clone(), 10);
        let record = ContactCard { contact_id: local.id.clone(), address_book: "https://dav.example.test/book/".into(), url: "https://dav.example.test/book/x.vcf".into(), etag: String::new(), base: local };
        assert!(!store.save_contact_card(&old, &record).unwrap());
        assert!(store.contact_cards(&account).unwrap().is_empty());
        store.set_contacts_default_book(&old, Some("https://dav.example.test/old-book/")).unwrap();
        assert_eq!(store.contacts_connection(&account).unwrap().unwrap().default_book, None);
        assert!(store.save_contact_card(&new, &record).unwrap());
        store.set_contacts_default_book(&new, Some("https://dav.example.test/book/")).unwrap();
        assert_eq!(store.contacts_connection(&account).unwrap().unwrap().default_book.as_deref(), Some("https://dav.example.test/book/"));
        store.set_contacts_default_book(&new, None).unwrap();
        assert_eq!(store.contacts_connection(&account).unwrap().unwrap().default_book, None);
    }

    #[test]
    fn a_refused_edit_gives_way_to_the_servers_version() {
        let (store, account) = store();
        let conn = connect(&store, &account);
        store.apply_contact_download(&conn, &[card(&account, 1, "Maya")]).unwrap();
        let mut maya = store.contacts(&account).unwrap().remove(0);
        maya.name = Some("Maya Chen".into());
        let op = store.save_contact_locally(&account, &maya).unwrap();
        store.set_operation_state(&op.id, OperationState::Failed, Some("read-only")).unwrap();
        assert!(store.restore_server_contact(&account, &maya.id).unwrap());
        let shown = store.contact_by_id(&account, &maya.id).unwrap().unwrap();
        assert_eq!((shown.name.as_deref(), shown.pending), (Some("Maya"), false));
        // No longer held: later changes at the server arrive.
        let mut renamed = card(&account, 1, "Maya R.");
        renamed.etag = "\"v9\"".into();
        store.apply_contact_download(&conn, &[renamed]).unwrap();
        assert_eq!(store.contact_by_id(&account, &maya.id).unwrap().unwrap().name.as_deref(), Some("Maya R."));
        assert!(!store.restore_server_contact(&account, "carddav:https://dav.example.test/book/none.vcf").unwrap());
    }

    #[test]
    fn disconnecting_forgets_server_contacts_but_keeps_ones_made_here() {
        let (store, account) = store();
        let conn = connect_to(&store, &account, SERVER, ContactsCredential::Separate, Some("https://dav.example.test/book/"));
        store.apply_contact_download(&conn, &[card(&account, 1, "Maya")]).unwrap();
        let mut maya = store.contacts(&account).unwrap().remove(0);
        maya.title = Some("Director".into());
        store.save_contact_locally(&account, &maya).unwrap();
        let mut local = Contact::new_local(account.clone(), 10);
        local.name = Some("Only here".into());
        store.save_contact_locally(&account, &local).unwrap();
        assert_eq!(store.contacts_connection(&account).unwrap().unwrap().credential, ContactsCredential::Separate, "the password choice is kept");
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
        let conn = connect(&store, &account);
        store.apply_contact_download(&conn, &[card(&account, 1, "Maya")]).unwrap();
        store.delete_account(&account).unwrap();
        assert!(store.contacts_connection(&account).unwrap().is_none());
        assert!(store.contact_cards(&account).unwrap().is_empty());
    }

    #[test]
    fn rapid_same_ms_reconnect_and_same_ms_edits_never_clobber() {
        let (store, account) = store();
        let first = connect(&store, &account);
        store.remove_contacts_connection(&account).unwrap();
        // Immediately reconnect with zero sleep: monotonic generation and connected_at still distinguish them.
        let second = connect(&store, &account);
        assert!(second.connected_at > first.connected_at, "same-ms reconnect must produce strictly greater connected_at");
        let gen: i64 = store
            .with_read(|c| Ok(c.query_row("SELECT generation FROM contact_connections WHERE account_id=?1", [&account.0], |r| r.get(0))?))
            .unwrap();
        assert_eq!(gen, 2);
        // Old connection's download must be ignored.
        store.apply_contact_download(&first, &[card(&account, 1, "Stale")]).unwrap();
        assert!(store.contact_cards(&account).unwrap().is_empty());

        // Rapid same-ms local edits without sleep must strictly advance updated & edit_revision.
        store.apply_contact_download(&second, &[card(&account, 1, "Maya")]).unwrap();
        let mut edit1 = store.contact_by_id(&account, &card(&account, 1, "").contact_id).unwrap().unwrap();
        edit1.name = Some("Maya v1".into());
        store.save_contact_locally(&account, &edit1).unwrap();
        let sent1 = store.contact_by_id(&account, &edit1.id).unwrap().unwrap().updated;

        let mut edit2 = store.contact_by_id(&account, &edit1.id).unwrap().unwrap();
        edit2.name = Some("Maya v2".into());
        store.save_contact_locally(&account, &edit2).unwrap();
        let after2 = store.contact_by_id(&account, &edit1.id).unwrap().unwrap();
        assert!(after2.updated > sent1, "same-ms edit must have strictly greater updated timestamp");
        let rev: i64 = store
            .with_read(|c| Ok(c.query_row("SELECT edit_revision FROM people WHERE account_id=?1 AND id=?2", params![account.0, edit1.id], |r| r.get(0))?))
            .unwrap();
        assert_eq!(rev, 2);

        // Server reply for edit1 arrives: must NOT clobber edit2.
        store.save_synced_contact(&second, &edit1.id, sent1, &card(&account, 1, "Maya v1")).unwrap();
        let shown = store.contact_by_id(&account, &edit1.id).unwrap().unwrap();
        assert_eq!((shown.name.as_deref(), shown.pending), (Some("Maya v2"), true));
    }

    #[test]
    fn corrupt_pending_operation_json_is_not_silently_skipped_by_contact_sync() {
        let (store, account) = store();
        let conn = connect(&store, &account);
        store
            .with_tx(|tx| {
                tx.execute(
                    "INSERT INTO pending_operations(id, account_id, seq, kind_json, state, created_at, updated_at) VALUES('bad-op', ?1, 1, '{not-json', 'pending', 1, 1)",
                    [&account.0],
                )?;
                Ok(())
            })
            .unwrap();
        assert!(store.apply_contact_download(&conn, &[card(&account, 1, "Maya")]).is_err());
        assert!(store.remove_contacts_connection(&account).is_err());
    }
}
