//! Local projections and their immutable-attempt outbox commit together.
use super::*;

fn invalid(s: &str) -> StoreError {
    StoreError::Invalid(s.into())
}
fn read(tx: &Connection, calendar: &CalendarId, href: &str) -> Result<Option<CalDavChange>> {
    let json: Option<String> = tx
        .query_row(
            "SELECT payload_json FROM caldav_outbox WHERE calendar_id=?1 AND href=?2",
            params![calendar.0, href],
            |r| r.get(0),
        )
        .optional()?;
    json.map(|s| serde_json::from_str(&s).map_err(Into::into))
        .transpose()
}
fn connection_current(tx: &Connection, change: &CalDavChange) -> Result<bool> {
    Ok(tx.query_row("SELECT EXISTS(SELECT 1 FROM calendar_connections WHERE id=?1 AND account_id=?2 AND generation=?3 AND enabled=1)",params![change.connection_id.0,change.account_id.0,change.generation],|r|r.get(0))?)
}
fn expected(tx: &Connection, change: &CalDavChange) -> Result<()> {
    if !connection_current(tx, change)?
        || read(tx, &change.calendar_id, &change.href)?.as_ref() != Some(change)
    {
        return Err(invalid(
            "Calendar change was superseded. Refresh and try again.",
        ));
    }
    Ok(())
}
fn save(tx: &Connection, change: &CalDavChange) -> Result<()> {
    tx.execute("INSERT INTO caldav_outbox(id,connection_id,calendar_id,href,revision,payload_json) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(id) DO UPDATE SET revision=excluded.revision,payload_json=excluded.payload_json",params![change.id.0,change.connection_id.0,change.calendar_id.0,change.href,change.revision,serde_json::to_string(change)?])?;
    Ok(())
}
pub(super) fn ensure_no_changes(tx: &Connection, account: &AccountId) -> Result<()> {
    let exists:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM caldav_outbox o JOIN calendar_connections c ON c.id=o.connection_id WHERE c.account_id=?1)",[&account.0],|r|r.get(0))?;
    if exists {
        return Err(invalid("Sync or resolve pending calendar changes before reconnecting or disconnecting calendars."));
    }
    Ok(())
}

impl Store {
    pub fn caldav_changes(&self, account: &AccountId) -> Result<Vec<CalDavChange>> {
        self.with_read(|tx| {
            let strings:Vec<String>=tx.prepare("SELECT o.payload_json FROM caldav_outbox o JOIN calendar_connections c ON c.id=o.connection_id WHERE c.account_id=?1 ORDER BY o.rowid")?.query_map([&account.0],|r|r.get(0))?.collect::<rusqlite::Result<_>>()?;
            strings.into_iter().map(|s|serde_json::from_str(&s).map_err(Into::into)).collect()
        })
    }

    /// Returns a CAS token and the exact original resource; this does not queue a write.
    pub fn caldav_change_for_event(&self, event: &Event, creating: bool) -> Result<CalDavChange> {
        self.with_read(|tx| {
            let connection=super::calendar_connections::read_connection(tx,&event.account_id)?.ok_or_else(||invalid("Connect a calendar service first."))?;
            let (owner,url,editable):(String,String,bool)=tx.query_row("SELECT c.account_id,c.provider_id,c.can_edit FROM calendars c JOIN caldav_collections d ON d.calendar_id=c.id WHERE c.id=?1 AND d.connection_id=?2",params![event.calendar_id.0,connection.id.0],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
            if owner!=event.account_id.0 || !connection.enabled || !editable {return Err(invalid("This calendar is read-only. Sync to check its permissions."));}
            let (href,base)=if creating {
                let uid=event.ical_uid.as_ref().ok_or_else(||invalid("Missing event identity."))?;
                if !uid.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.@".contains(&b)) {return Err(invalid("Invalid event identity."));}
                (format!("{}/{uid}.ics",url.trim_end_matches('/')),None)
            } else {
                let row:(String,String,String)=tx.query_row("SELECT r.href,r.etag,r.icalendar FROM caldav_event_resources e JOIN caldav_resources r ON r.calendar_id=e.calendar_id AND r.href=e.href WHERE e.event_id=?1 AND e.calendar_id=?2",params![event.id.0,event.calendar_id.0],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
                (row.0,Some(CalDavRemote{etag:row.1,icalendar:row.2}))
            };
            if let Some(change)=read(tx,&event.calendar_id,&href)? {
                if change.state!=CalDavChangeState::Pending {return Err(invalid("This event has an upload being checked or a conflict. Review calendar changes before editing it again."));} return Ok(change);
            }
            Ok(CalDavChange{id:OperationId::generate(),connection_id:connection.id,generation:connection.generation,account_id:event.account_id.clone(),calendar_id:event.calendar_id.clone(),href,revision:0,state:CalDavChangeState::Pending,base,desired:None,edits:vec![],remote:None,note:None,timezone:tz::cache_key()})
        })
    }

    pub fn queue_caldav_change(
        &self,
        token: &CalDavChange,
        desired: Option<String>,
        edit: CalDavEdit,
    ) -> Result<()> {
        self.with_tx(|tx| {
            if !connection_current(tx,token)? {return Err(invalid("Calendar settings changed."));}
            if token.state!=CalDavChangeState::Pending || token.timezone!=tz::cache_key() {return Err(invalid("Refresh this calendar before saving another change."));}
            let writable:bool=tx.query_row("SELECT can_edit AND account_id=?2 FROM calendars WHERE id=?1",params![token.calendar_id.0,token.account_id.0],|r|r.get(0))?;
            if !writable {return Err(invalid("This calendar is read-only."));}
            if token.revision==0 {
                if read(tx,&token.calendar_id,&token.href)?.is_some() {return Err(invalid("Another edit was saved first. Try again."));}
                if let Some(base)=&token.base {
                    let stored:Option<(String,String)>=tx.query_row("SELECT etag,icalendar FROM caldav_resources WHERE calendar_id=?1 AND href=?2",params![token.calendar_id.0,token.href],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
                    if stored!=Some((base.etag.clone(),base.icalendar.clone())) {return Err(invalid("The event changed while you were editing it. Reopen it before saving."));}
                }
            } else {expected(tx,token)?;}
            for e in edit.before.iter().chain(edit.after.iter()) {
                if e.account_id!=token.account_id || e.calendar_id!=token.calendar_id {return Err(invalid("Calendar edit identity mismatch."));}
            }
            if let Some(before)=&edit.before {
                let stored=load_events(tx,"WHERE e.id=?1",[&before.id.0])?.pop();
                if stored.as_ref()!=Some(before) {return Err(invalid("This event changed while you were editing it. Reopen it before saving."));}
            }
            let mut change=token.clone(); change.revision=change.revision.checked_add(1).ok_or_else(||invalid("Too many revisions."))?;
            change.desired=desired; change.edits.push(edit.clone());
            if change.edits.len()>128 || change.desired.as_ref().is_some_and(|s|s.len()>2*1024*1024) {return Err(invalid("Sync this event before making more changes."));}
            tx.execute("INSERT OR IGNORE INTO caldav_resources(calendar_id,href,etag,icalendar) VALUES(?1,?2,?3,?4)",params![change.calendar_id.0,change.href,change.base.as_ref().map(|b|b.etag.as_str()).unwrap_or(""),change.base.as_ref().map(|b|b.icalendar.as_str()).or(change.desired.as_deref()).unwrap_or("")])?;
            if change.desired.is_none() {
                tx.execute("DELETE FROM events WHERE id IN (SELECT event_id FROM caldav_event_resources WHERE calendar_id=?1 AND href=?2)",params![change.calendar_id.0,change.href])?;
            } else if let Some(after)=&edit.after {
                let mut after=after.clone(); after.pending=true; upsert_event_tx(tx,&after)?;
                tx.execute("INSERT OR REPLACE INTO caldav_event_resources(event_id,calendar_id,href) VALUES(?1,?2,?3)",params![after.id.0,change.calendar_id.0,change.href])?;
            } else if let Some(before)=&edit.before {tx.execute("DELETE FROM events WHERE id=?1",[&before.id.0])?;}
            if change.base.is_none() && change.desired.is_none() {
                tx.execute("DELETE FROM caldav_outbox WHERE id=?1",[&change.id.0])?;
                tx.execute("DELETE FROM caldav_resources WHERE calendar_id=?1 AND href=?2",params![change.calendar_id.0,change.href])?;
            } else {save(tx,&change)?;}
            Ok(())
        })
    }

    pub fn claim_caldav_change(&self, token: &CalDavChange) -> Result<CalDavChange> {
        self.with_tx(|tx| {
            expected(tx, token)?;
            if !matches!(
                token.state,
                CalDavChangeState::Pending | CalDavChangeState::Checking
            ) {
                return Err(invalid("Calendar change needs review."));
            }
            let mut change = token.clone();
            change.state = CalDavChangeState::Checking;
            save(tx, &change)?;
            Ok(change)
        })
    }
    pub fn caldav_change_current(&self, token: &CalDavChange) -> Result<bool> {
        self.with_read(|tx| {
            Ok(connection_current(tx, token)?
                && read(tx, &token.calendar_id, &token.href)?.as_ref() == Some(token))
        })
    }

    pub fn mark_caldav_change(
        &self,
        token: &CalDavChange,
        state: CalDavChangeState,
        remote: Option<CalDavRemote>,
        note: String,
    ) -> Result<()> {
        self.with_tx(|tx| {
            expected(tx, token)?;
            let mut change = token.clone();
            change.state = state;
            change.remote = remote;
            change.note = Some(note);
            save(tx, &change)
        })
    }

    /// Clearing an acknowledged write does not mark old coverage fresh. A subsequent
    /// snapshot supplies the provider's canonical display projection.
    pub fn complete_caldav_change(
        &self,
        token: &CalDavChange,
        remote: Option<&CalDavRemote>,
        discard_local: bool,
    ) -> Result<()> {
        self.with_tx(|tx|{expected(tx,token)?;
            if discard_local || remote.is_none() {tx.execute("DELETE FROM events WHERE id IN (SELECT event_id FROM caldav_event_resources WHERE calendar_id=?1 AND href=?2)",params![token.calendar_id.0,token.href])?;}
            if let Some(remote)=remote {tx.execute("UPDATE caldav_resources SET etag=?3,icalendar=?4 WHERE calendar_id=?1 AND href=?2",params![token.calendar_id.0,token.href,remote.etag,remote.icalendar])?;}
            else {tx.execute("DELETE FROM caldav_resources WHERE calendar_id=?1 AND href=?2",params![token.calendar_id.0,token.href])?;}
            tx.execute("DELETE FROM caldav_outbox WHERE id=?1",[&token.id.0])?;
            tx.execute("DELETE FROM caldav_coverage WHERE connection_id=?1",[&token.connection_id.0])?;
            // Keep the projection's zone while invalidating range coverage. A failed
            // follow-up download must not hide a successfully acknowledged local event.
            tx.execute("INSERT INTO caldav_coverage(connection_id,range_start,range_end,timezone,synced_at) VALUES(?1,0,0,?2,0)",params![token.connection_id.0,token.timezone])?;Ok(())
        })
    }

    pub fn rebase_caldav_change(
        &self,
        token: &CalDavChange,
        desired: Option<String>,
    ) -> Result<()> {
        self.with_tx(|tx| {
            expected(tx, token)?;
            if token.state != CalDavChangeState::Conflict {
                return Err(invalid("This change is not a conflict."));
            }
            let mut change = token.clone();
            change.revision = change
                .revision
                .checked_add(1)
                .ok_or_else(|| invalid("Too many revisions."))?;
            change.base = change.remote.take();
            change.desired = desired;
            change.state = CalDavChangeState::Pending;
            change.note = None;
            save(tx, &change)
        })
    }
}
