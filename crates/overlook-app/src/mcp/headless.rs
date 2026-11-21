//! Foreground owner of the regular desktop profile. The launch lease outlives
//! the MCP dispatcher, native sync worker, and Store.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc,
};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use overlook_core::{
    time::now, tz, Account, AccountId, OperationKind, OperationState, Prefs, ProviderKind,
    SyncState,
};
use overlook_store::Store;
use tokio::signal::unix::{signal, SignalKind};

use super::{service, take_changes, WriteDomain};
use crate::providers::{providers_for, AccountProviders};
use crate::single_instance::{self, Role};
use crate::sync::{pollster_block, run_sync, sync_connected_contacts};
use crate::{caldav_sync, init_logging, open_store, restrict_to_owner, sample_provider};

const POLL: Duration = Duration::from_secs(1);
const MAIL_INTERVAL: Duration = Duration::from_secs(60);
const DAV_INTERVAL: Duration = Duration::from_secs(5 * 60);

pub(super) fn run(profile: &Path) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    // Register before claiming the profile. Store and keyring I/O are driven
    // outside this runtime so credential backends can run their own runtime.
    let (mut interrupt, mut terminate) = runtime.block_on(async {
        Ok::<_, std::io::Error>((
            signal(SignalKind::interrupt())?,
            signal(SignalKind::terminate())?,
        ))
    })?;
    restrict_to_owner(profile);
    let mut owner = single_instance::listen(profile, Role::Headless)
        .context("could not claim the desktop profile")?
        .context("Overlook is already running for this profile")?;
    init_logging();
    crate::install_panic_hook();

    let (store, _) = open_store(owner.profile())?;
    let prefs: Prefs = store.setting("prefs")?.unwrap_or_default();
    crate::set_inbox_limit(prefs.inbox_messages as usize);
    let sample = sample_provider();
    let stopping = Arc::new(AtomicBool::new(false));
    let (wake_sync, schedule) = mpsc::sync_channel(1);
    let mut launches = single_instance::wakeups();
    let server = service::start(owner.profile(), store.clone(), sample.clone())?;
    let worker = {
        let (store, stopping) = (store.clone(), stopping.clone());
        thread::Builder::new()
            .name("overlook-headless-sync".into())
            .spawn(move || {
                let mut accounts = HashMap::new();
                while schedule.recv().is_ok() && !stopping.load(Ordering::Acquire) {
                    let changes = take_changes();
                    let current = store.accounts()?;
                    for account in current {
                        accounts
                            .entry(account.id.clone())
                            .or_insert_with(AccountSync::default)
                            .run(
                                &store,
                                &sample,
                                &account,
                                &changes,
                                prefs.undo_send_secs,
                                &stopping,
                            );
                        if stopping.load(Ordering::Acquire) {
                            break;
                        }
                    }
                }
                Ok::<_, anyhow::Error>(())
            })
    };
    let worker = match worker {
        Ok(worker) => worker,
        Err(error) => {
            server.drain();
            return Err(error.into());
        }
    };

    eprintln!("Overlook MCP headless profile is ready.");
    runtime.block_on(async {
        let mut timer = tokio::time::interval(POLL);
        loop {
            if single_instance::take()
                .iter()
                .any(|line| single_instance::headless_stop_requested(line))
            {
                break;
            }
            if let Err(mpsc::TrySendError::Disconnected(_)) = wake_sync.try_send(()) {
                break;
            }
            tokio::select! {
                biased;
                _ = interrupt.recv() => break,
                _ = terminate.recv() => break,
                _ = launches.recv() => {}
                _ = timer.tick() => {}
            }
        }
    });
    stopping.store(true, Ordering::Release);
    drop(wake_sync);
    owner.close_listener();
    server.drain();
    worker
        .join()
        .map_err(|_| anyhow::anyhow!("headless sync worker stopped unexpectedly"))??;
    drop(store);
    Ok(())
}

#[derive(Default)]
struct AccountSync {
    provider: Option<Option<AccountProviders>>,
    mail_next: Option<Instant>,
    mail_backoff: bool,
    calendar_next: Option<Instant>,
    calendar_backoff: bool,
    contacts_next: Option<Instant>,
}

impl AccountSync {
    fn run(
        &mut self,
        store: &Arc<Store>,
        sample: &overlook_provider::sample::SampleProvider,
        account: &Account,
        changes: &BTreeMap<(AccountId, WriteDomain), bool>,
        undo_secs: u32,
        stopping: &AtomicBool,
    ) {
        let changed = |domain| changes.get(&(account.id.clone(), domain)) == Some(&true);
        let due = |next: Option<Instant>| next.is_none_or(|next| Instant::now() >= next);
        let active = || !stopping.load(Ordering::Acquire);

        if active()
            && (due(self.calendar_next)
                || (!self.calendar_backoff && changed(WriteDomain::Calendar)))
        {
            let delay = match store.calendar_connection(&account.id) {
                Ok(Some(connection)) if connection.enabled => {
                    let day = tz::day_start(now());
                    caldav_sync::synchronize(
                        store,
                        account,
                        &connection,
                        tz::add_days(day, -60),
                        tz::add_days(day, 366),
                    )
                }
                _ => 0,
            };
            self.calendar_backoff = delay > 0;
            let pause = if delay > 0 {
                Duration::from_secs(delay)
            } else {
                DAV_INTERVAL
            };
            self.calendar_next = Some(Instant::now() + pause);
        }

        let checked_contacts = active()
            && (due(self.mail_next)
                || (!self.mail_backoff
                    && (changed(WriteDomain::Mail)
                        || changed(WriteDomain::Calendar)
                        || changed(WriteDomain::Contacts))))
            && self.sync_mail(store, sample, account, undo_secs);

        // The native mail pass usually checks connected contacts. If mail was
        // unavailable (or skipped), give the independent connection its turn.
        if active()
            && !checked_contacts
            && (due(self.contacts_next) || changed(WriteDomain::Contacts))
        {
            sync_connected_contacts(store, account);
            self.contacts_next = Some(Instant::now() + MAIL_INTERVAL);
        }
    }

    fn sync_mail(
        &mut self,
        store: &Arc<Store>,
        sample: &overlook_provider::sample::SampleProvider,
        account: &Account,
        undo_secs: u32,
    ) -> bool {
        self.mail_next = Some(Instant::now() + MAIL_INTERVAL);
        if account.provider == ProviderKind::Local
            || (account.provider == ProviderKind::Sample && !sample.is_online())
        {
            return false;
        }
        let providers = self
            .provider
            .get_or_insert_with(|| providers_for(account, sample, store).ok());
        let Some(bundle) = providers else {
            let _ = store.set_sync_state(&account.id, &SyncState::SignInRequired, None);
            return false;
        };
        let expired = bundle
            .gmail
            .as_ref()
            .is_some_and(|client| client.session_expiry(now()).is_some())
            || bundle
                .microsoft
                .as_ref()
                .is_some_and(|session| session.session_expiry(now()).is_some());
        if expired {
            let _ = store.set_sync_state(&account.id, &SyncState::SignInRequired, None);
            self.provider = Some(None);
            return false;
        }
        let outcome = pollster_block(run_sync(
            store.clone(),
            bundle.mail.clone(),
            bundle.calendar.clone(),
            account.clone(),
            false,
        ));
        if outcome.checked_connected_contacts {
            self.contacts_next = Some(Instant::now() + MAIL_INTERVAL);
        }
        let success = outcome.state == Some(SyncState::UpToDate);
        self.mail_backoff = !success;
        if outcome.state == Some(SyncState::SignInRequired)
            && account.provider != ProviderKind::Sample
        {
            // As on desktop, do not repeat a rejected login.
            self.provider = Some(None);
        }
        let delay = if outcome.rate_limited {
            Duration::from_secs(65)
        } else if success && outcome.more_to_download {
            POLL
        } else if success {
            pending_send_delay(store, &account.id, undo_secs).unwrap_or(MAIL_INTERVAL)
        } else {
            MAIL_INTERVAL
        };
        self.mail_next = Some(Instant::now() + delay);
        outcome.checked_connected_contacts
    }
}

/// Wake at the end of an Outbox undo window. Store still checks the delay and
/// claims the send transactionally when the native replay engine resumes it.
fn pending_send_delay(store: &Store, account: &AccountId, undo_secs: u32) -> Option<Duration> {
    let now = now();
    let undo_ms = i64::from(undo_secs) * 1000;
    store
        .pending_operations(account)
        .ok()?
        .iter()
        .filter(|op| {
            op.state == OperationState::Pending
                && matches!(op.kind, OperationKind::SendDraft { .. })
        })
        .map(|op| {
            Duration::from_millis(
                (op.created_at + undo_ms - now).max(POLL.as_millis() as i64) as u64
            )
        })
        .min()
        .map(|delay| delay.min(MAIL_INTERVAL))
}

#[cfg(test)]
mod tests {
    use super::*;
    use overlook_core::{Address, Draft, DraftKind};
    use overlook_provider::sample::SampleProvider;

    #[test]
    fn native_mail_replay_resumes_a_queued_send_after_the_undo_window() {
        let store = Arc::new(Store::open_in_memory().unwrap());
        let account = Account {
            id: AccountId::from("sample"),
            provider: ProviderKind::Sample,
            display_name: "Synthetic mailbox".into(),
            email: "sample@example.com".into(),
            created_at: 0,
        };
        store.upsert_account(&account).unwrap();
        let prefs = Prefs {
            undo_send_secs: 60,
            ..Prefs::default()
        };
        store.set_setting("prefs", &prefs).unwrap();
        let mut draft = Draft::new(account.id.clone(), DraftKind::New, now());
        draft.to.push(Address {
            name: None,
            email: "recipient@example.com".into(),
        });
        draft.subject = "Headless send replay".into();
        store.queue_send(&draft).unwrap();
        let sample = SampleProvider::new().with_latency(Duration::ZERO);
        sample.set_online(true);

        let mut sync = AccountSync::default();
        sync.sync_mail(&store, &sample, &account, prefs.undo_send_secs);
        assert!(store.draft(&draft.id).unwrap().is_some());
        assert!(pending_send_delay(&store, &account.id, prefs.undo_send_secs).is_some());

        let prefs = Prefs {
            undo_send_secs: 0,
            ..prefs
        };
        store.set_setting("prefs", &prefs).unwrap();
        sync.sync_mail(&store, &sample, &account, prefs.undo_send_secs);
        assert!(store.draft(&draft.id).unwrap().is_none());
        assert_eq!(store.pending_count(&account.id).unwrap(), 0);
    }
}
