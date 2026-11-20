//! The MCP receipt and outbound operations commit with the local mutation.

mod delivery;
mod drafts;
mod organization;

use super::*;
use serde_json::Value;

pub struct McpWriteKey<'a> {
    pub account: &'a AccountId,
    pub client: &'a str,
    pub key_digest: &'a [u8; 32],
    pub request_digest: &'a [u8; 32],
    pub action: &'a str,
}

pub enum McpWriteOutcome {
    Applied { data: Value, queued: bool },
    Replayed(Value),
    KeyConflict,
}

#[derive(Default)]
struct Effects {
    commit: Vec<Box<dyn FnOnce()>>,
    rollback: Vec<Box<dyn FnOnce()>>,
}

pub struct McpWriteTxn<'a, 'conn> {
    pub(crate) tx: &'a Transaction<'conn>,
    pub(crate) account: &'a AccountId,
    editing: &'a HashMap<DraftId, usize>,
    effects: &'a mut Effects,
}

impl McpWriteTxn<'_, '_> {
    pub fn account(&self) -> &AccountId {
        self.account
    }

    pub fn draft_is_open(&self, id: &DraftId) -> bool {
        self.editing.contains_key(id)
    }

    pub fn on_rollback(&mut self, cleanup: impl FnOnce() + 'static) {
        self.effects.rollback.push(Box::new(cleanup));
    }

    pub fn on_commit(&mut self, cleanup: impl FnOnce() + 'static) {
        self.effects.commit.push(Box::new(cleanup));
    }

    pub fn find_message(&self, predicate: impl FnMut(&str) -> bool) -> Result<Option<Message>> {
        let Some(id) = find_id(self.tx, "messages", self.account, predicate)? else {
            return Ok(None);
        };
        Ok(load_messages(
            self.tx,
            "WHERE m.account_id=?1 AND m.id=?2",
            params![self.account.0, id],
        )?
        .pop())
    }

    pub fn find_draft(&self, predicate: impl FnMut(&str) -> bool) -> Result<Option<Draft>> {
        let Some(id) = find_id(self.tx, "drafts", self.account, predicate)? else {
            return Ok(None);
        };
        Ok(load_drafts(
            self.tx,
            "WHERE d.account_id=?1 AND d.id=?2",
            params![self.account.0, id],
        )?
        .pop())
    }

    pub fn find_event(&self, predicate: impl FnMut(&str) -> bool) -> Result<Option<Event>> {
        let Some(id) = find_id(self.tx, "events", self.account, predicate)? else {
            return Ok(None);
        };
        Ok(load_events(
            self.tx,
            "WHERE e.account_id=?1 AND e.id=?2",
            params![self.account.0, id],
        )?
        .pop())
    }
}

fn find_id(
    conn: &Connection,
    table: &'static str,
    account: &AccountId,
    mut matches: impl FnMut(&str) -> bool,
) -> Result<Option<String>> {
    let mut statement =
        conn.prepare_cached(&format!("SELECT id FROM {table} WHERE account_id=?1"))?;
    let mut rows = statement.query([&account.0])?;
    while let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        if matches(&id) {
            return Ok(Some(id));
        }
    }
    Ok(None)
}

pub struct McpOperationStatus {
    pub id: OperationId,
    pub action: String,
    pub state: String,
    pub updated_at: i64,
    pub source: String,
}

pub struct McpWriteActivity {
    pub receipt_id: String,
    pub client: String,
    pub action: String,
    pub created_at: i64,
}

impl Store {
    pub fn mcp_write<E: From<StoreError>>(
        &self,
        key: McpWriteKey<'_>,
        apply: impl FnOnce(&mut McpWriteTxn<'_, '_>, &str) -> std::result::Result<Value, E>,
    ) -> std::result::Result<McpWriteOutcome, E> {
        let mut effects = Effects::default();
        let outcome = self.mcp_write_transaction(key, &mut effects, apply);
        // Run attachment cleanup after releasing the transaction and both locks.
        let cleanup = if matches!(outcome, Ok(McpWriteOutcome::Applied { .. })) {
            effects.commit
        } else {
            effects.rollback
        };
        for action in cleanup {
            action();
        }
        outcome
    }

    fn mcp_write_transaction<E: From<StoreError>>(
        &self,
        key: McpWriteKey<'_>,
        effects: &mut Effects,
        apply: impl FnOnce(&mut McpWriteTxn<'_, '_>, &str) -> std::result::Result<Value, E>,
    ) -> std::result::Result<McpWriteOutcome, E> {
        let mut conn = self.conn.lock().expect("store mutex poisoned");
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(StoreError::from)?;
        let prior = tx.query_row(
            "SELECT request_digest, result_json FROM mcp_write_receipts WHERE account_id=?1 AND client=?2 AND key_digest=?3",
            params![key.account.0, key.client, key.key_digest.as_slice()],
            |r| Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, String>(1)?)),
        ).optional().map_err(StoreError::from)?;
        if let Some((digest, response)) = prior {
            return Ok(if digest == key.request_digest.as_slice() {
                McpWriteOutcome::Replayed(
                    serde_json::from_str(&response).map_err(StoreError::from)?,
                )
            } else {
                McpWriteOutcome::KeyConflict
            });
        }
        // Current IDs, rather than sequence numbers, also handle a mutation
        // that cancels the newest operation and creates another in the same tx.
        let before = operation_ids(&tx, key.account)?;
        let before_caldav = if key.action.starts_with("calendar_") {
            Some(caldav_revisions(&tx, key.account)?)
        } else {
            None
        };
        let editing = self
            .editing_drafts
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let receipt = OperationId::generate();
        let data = apply(
            &mut McpWriteTxn {
                tx: &tx,
                account: key.account,
                editing: &editing,
                effects,
            },
            receipt.as_str(),
        )?;
        let new_ids: Vec<_> = operation_ids(&tx, key.account)?
            .difference(&before)
            .cloned()
            .collect();
        tx.execute(
            "INSERT INTO mcp_operation_status(id, account_id, action, state, updated_at)
             SELECT id, account_id, json_extract(kind_json,'$.op'), state, updated_at
             FROM pending_operations WHERE account_id=?1 AND id IN (SELECT value FROM json_each(?2))",
            params![key.account.0, serde_json::to_string(&new_ids).map_err(StoreError::from)?],
        ).map_err(StoreError::from)?;
        let mut caldav_queued = false;
        if let Some(before) = before_caldav {
            for (id, revision) in caldav_revisions(&tx, key.account)? {
                if before.get(&id) == Some(&revision) {
                    continue;
                }
                tx.execute(
                    "INSERT INTO mcp_operation_status(id,account_id,action,state,updated_at,source)
                     SELECT id,?2,?3,json_extract(payload_json,'$.state'),?4,'caldav'
                     FROM caldav_outbox WHERE id=?1
                     ON CONFLICT(id) DO UPDATE SET state=excluded.state,updated_at=excluded.updated_at",
                    params![id, key.account.0, key.action, now()],
                ).map_err(StoreError::from)?;
                caldav_queued = true;
            }
        }
        tx.execute(
            "INSERT INTO mcp_write_receipts(account_id,client,key_digest,request_digest,receipt_id,action,result_json,created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![key.account.0, key.client, key.key_digest.as_slice(), key.request_digest.as_slice(),
                receipt.0, key.action, serde_json::to_string(&data).map_err(StoreError::from)?, now()],
        ).map_err(StoreError::from)?;
        tx.commit().map_err(StoreError::from)?;
        drop(editing);
        Ok(McpWriteOutcome::Applied {
            data,
            queued: !new_ids.is_empty() || caldav_queued,
        })
    }

    pub fn mcp_operation_status(
        &self,
        account: &AccountId,
        mut matches: impl FnMut(&str) -> bool,
    ) -> Result<Option<McpOperationStatus>> {
        self.with_read(|conn| {
            let mut stmt = conn.prepare_cached(
                "SELECT id,action,state,updated_at,source FROM mcp_operation_status WHERE account_id=?1",
            )?;
            let mut rows = stmt.query([&account.0])?;
            while let Some(row) = rows.next()? {
                let id: String = row.get(0)?;
                if matches(&id) {
                    return Ok(Some(McpOperationStatus {
                        id: OperationId(id),
                        action: row.get(1)?,
                        state: row.get(2)?,
                        updated_at: row.get(3)?,
                        source: row.get(4)?,
                    }));
                }
            }
            Ok(None)
        })
    }

    pub fn mcp_accounts_with_pending_writes(&self) -> Result<Vec<AccountId>> {
        self.with_read(|conn| {
            let mut stmt = conn.prepare_cached(
                "SELECT DISTINCT account_id FROM mcp_operation_status WHERE state IN ('pending','in-flight','uncertain')",
            )?;
            let rows = stmt.query_map([], |r| Ok(AccountId(r.get(0)?)))?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
    }

    pub fn mcp_write_activity(
        &self,
        account: &AccountId,
        limit: usize,
    ) -> Result<Vec<McpWriteActivity>> {
        self.with_read(|conn| {
            let mut stmt = conn.prepare_cached(
                "SELECT receipt_id,client,action,created_at FROM mcp_write_receipts WHERE account_id=?1
                 ORDER BY created_at DESC, receipt_id DESC LIMIT ?2",
            )?;
            let rows = stmt.query_map(params![account.0, limit as i64], |r| Ok(McpWriteActivity {
                receipt_id: r.get(0)?, client: r.get(1)?, action: r.get(2)?, created_at: r.get(3)?,
            }))?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
    }
}

pub(crate) fn finish_caldav_operation(
    conn: &Connection,
    id: &OperationId,
    discard_local: bool,
) -> Result<()> {
    conn.execute(
        "UPDATE mcp_operation_status SET state=?2,updated_at=?3 WHERE id=?1 AND source='caldav'",
        params![
            id.0,
            if discard_local {
                "discarded"
            } else {
                "applied"
            },
            now()
        ],
    )?;
    Ok(())
}

fn caldav_revisions(conn: &Connection, account: &AccountId) -> Result<BTreeMap<String, u32>> {
    let mut statement = conn.prepare_cached(
        "SELECT o.id,o.revision FROM caldav_outbox o JOIN calendar_connections c ON c.id=o.connection_id
         WHERE c.account_id=?1",
    )?;
    let rows = statement.query_map([&account.0], |row| Ok((row.get(0)?, row.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn operation_ids(conn: &Connection, account: &AccountId) -> Result<HashSet<String>> {
    let mut statement = conn.prepare_cached(
        "SELECT id FROM pending_operations WHERE account_id=?1 AND state != 'applied'",
    )?;
    let rows = statement.query_map([&account.0], |r| r.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample(store: &Store) -> AccountId {
        let account = Account {
            id: AccountId::from("sample"),
            provider: ProviderKind::Sample,
            display_name: "Sample".to_owned(),
            email: "sample@example.test".to_owned(),
            created_at: 1,
        };
        store.upsert_account(&account).unwrap();
        account.id
    }

    fn key<'a>(account: &'a AccountId, digest: &'a [u8; 32]) -> McpWriteKey<'a> {
        McpWriteKey {
            account,
            client: "harness",
            key_digest: &[1; 32],
            request_digest: digest,
            action: "fixture",
        }
    }

    #[test]
    fn receipt_survives_reopen_and_operation_status_survives_pruning() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("overlook.sqlite");
        let store = Store::open(&db).unwrap();
        let account = sample(&store);
        let McpWriteOutcome::Applied { data, queued: true } = store
            .mcp_write::<StoreError>(key(&account, &[2; 32]), |tx, receipt| {
                let op = insert_operation_tx(
                    tx.tx,
                    tx.account(),
                    OperationKind::SetRead {
                        message_ids: vec![MessageId::from("fake-message")],
                        read: true,
                    },
                    1,
                )?;
                Ok(json!({"receipt":receipt,"operation":op.id.0}))
            })
            .unwrap()
        else {
            panic!("new operation was not committed")
        };
        let op: OperationId = serde_json::from_value(data["operation"].clone()).unwrap();
        store
            .set_operation_state(&op, OperationState::Applied, None)
            .unwrap();
        store.prune_applied(i64::MAX).unwrap();
        drop(store);

        let store = Store::open(&db).unwrap();
        let McpWriteOutcome::Replayed(replayed) = store
            .mcp_write::<StoreError>(key(&account, &[2; 32]), |_, _| {
                panic!("A repeated write must never execute")
            })
            .unwrap()
        else {
            panic!("missing persisted receipt")
        };
        assert_eq!(data, replayed);
        assert!(matches!(
            store
                .mcp_write::<StoreError>(key(&account, &[3; 32]), |_, _| {
                    panic!("A conflicting write must never execute")
                })
                .unwrap(),
            McpWriteOutcome::KeyConflict
        ));
        let status = store
            .mcp_operation_status(&account, |id| id == op.as_str())
            .unwrap()
            .unwrap();
        assert_eq!(status.state, "applied");
        assert_eq!(status.action, "set-read");
    }

    #[test]
    fn failures_roll_back_mutations_and_run_cleanup_outside_the_transaction() {
        let store = Arc::new(Store::open_in_memory().unwrap());
        let account = sample(&store);
        let cleanup_store = store.clone();
        let result = store.mcp_write::<StoreError>(key(&account, &[2; 32]), |tx, _| {
            tx.tx.execute(
                "UPDATE accounts SET display_name='Never committed' WHERE id=?1",
                [&account.0],
            )?;
            tx.on_rollback(move || {
                cleanup_store.set_setting("rolled_back", &true).unwrap();
            });
            Err(StoreError::Invalid("test rollback".into()))
        });
        assert!(matches!(result, Err(StoreError::Invalid(_))));
        assert_eq!(store.accounts().unwrap()[0].display_name, "Sample");
        assert_eq!(store.setting::<bool>("rolled_back").unwrap(), Some(true));
        assert!(store.mcp_write_activity(&account, 10).unwrap().is_empty());

        let cleanup_store = store.clone();
        assert!(matches!(
            store
                .mcp_write::<StoreError>(key(&account, &[2; 32]), |tx, _| {
                    tx.on_commit(move || cleanup_store.set_setting("committed", &true).unwrap());
                    Ok(json!({"status":"saved"}))
                })
                .unwrap(),
            McpWriteOutcome::Applied { queued: false, .. }
        ));
        assert_eq!(store.setting::<bool>("committed").unwrap(), Some(true));
    }

    #[test]
    fn caldav_receipts_track_conflict_and_provider_completion() {
        let store = Store::open_in_memory().unwrap();
        let account = sample(&store);
        let connection = store
            .save_calendar_connection(
                &account,
                &CalDavSettings {
                    server_url: "https://calendar.example.test/dav".into(),
                    username: "sample@example.test".into(),
                    credential_source: CalendarCredentialSource::Separate,
                },
                true,
                None,
            )
            .unwrap();
        let calendar = CalendarId::generate();
        store
            .replace_calendars(
                &account,
                &[Calendar {
                    id: calendar.clone(),
                    account_id: account.clone(),
                    provider_id: None,
                    name: "Sample".into(),
                    color: None,
                    is_primary: true,
                    can_edit: true,
                    visible: true,
                    sort_order: 0,
                    default_reminders: vec![],
                }],
            )
            .unwrap();
        for (value, discard_local) in [(2, false), (3, true)] {
            let change = CalDavChange {
                id: OperationId::generate(),
                connection_id: connection.id.clone(),
                generation: connection.generation,
                account_id: account.clone(),
                calendar_id: calendar.clone(),
                href: "https://calendar.example.test/dav/sample.ics".into(),
                revision: 1,
                state: CalDavChangeState::Pending,
                base: None,
                desired: None,
                edits: vec![],
                remote: None,
                note: None,
                timezone: tz::cache_key(),
            };
            let digest = [value; 32];
            let mut write_key = key(&account, &digest);
            write_key.key_digest = &digest;
            write_key.action = "calendar_fixture";
            let outcome = store
                .mcp_write::<StoreError>(write_key, |tx, _| {
                    tx.tx.execute(
                        "INSERT INTO caldav_outbox VALUES(?1,?2,?3,?4,?5,?6)",
                        params![
                            change.id.0,
                            change.connection_id.0,
                            change.calendar_id.0,
                            change.href,
                            change.revision,
                            serde_json::to_string(&change)?
                        ],
                    )?;
                    Ok(json!({"operation_id":change.id.0}))
                })
                .unwrap();
            assert!(matches!(
                outcome,
                McpWriteOutcome::Applied { queued: true, .. }
            ));
            let status = || {
                store
                    .mcp_operation_status(&account, |raw| raw == change.id.as_str())
                    .unwrap()
                    .unwrap()
                    .state
            };
            assert_eq!(status(), "pending");
            let claimed = store.claim_caldav_change(&change).unwrap();
            assert_eq!(status(), "checking");
            let finishing = if discard_local {
                store
                    .mark_caldav_change(
                        &claimed,
                        CalDavChangeState::Conflict,
                        None,
                        "provider change".into(),
                    )
                    .unwrap();
                assert_eq!(status(), "conflict");
                store.caldav_changes(&account).unwrap().pop().unwrap()
            } else {
                claimed
            };
            store
                .complete_caldav_change(&finishing, None, discard_local)
                .unwrap();
            assert_eq!(
                status(),
                if discard_local {
                    "discarded"
                } else {
                    "applied"
                }
            );
        }
    }
}
