//! Writes the UI thread hands off. One background thread applies them in order, so the
//! window never waits for the disk or for a sync's write, and the last value always wins.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::mpsc::{channel, sync_channel, Sender};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

type Job = Box<dyn FnOnce() + Send>;

static QUEUE: OnceLock<Mutex<Sender<Job>>> = OnceLock::new();

fn queue() -> &'static Mutex<Sender<Job>> {
    QUEUE.get_or_init(|| {
        let (tx, rx) = channel::<Job>();
        let _ = std::thread::Builder::new().name("overlook-persist".into()).spawn(move || {
            for job in rx {
                if let Err(payload) = catch_unwind(AssertUnwindSafe(job)) {
                    let message = payload
                        .downcast_ref::<&str>()
                        .copied()
                        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
                        .unwrap_or("unknown panic");
                    tracing::error!("background persist job panicked: {message}");
                }
            }
        });
        Mutex::new(tx)
    })
}

/// Runs `job` on the background writer, after every job queued before it.
pub fn later(job: impl FnOnce() + Send + 'static) {
    if queue().lock().unwrap_or_else(|e| e.into_inner()).send(Box::new(job)).is_err() {
        tracing::warn!("background writer is gone; a setting was not saved");
    }
}

/// Waits until every job queued before this call has completed on the background writer.
pub fn flush() {
    let Some(queue) = QUEUE.get() else {
        return;
    };
    let (tx, rx) = sync_channel::<()>(1);
    let sent = queue
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .send(Box::new(move || {
            let _ = tx.send(());
        }))
        .is_ok();
    if sent {
        let _ = rx.recv_timeout(Duration::from_secs(5));
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    #[test]
    fn jobs_run_in_order() {
        let (tx, rx) = std::sync::mpsc::channel();
        for i in 0..50 {
            let tx = tx.clone();
            super::later(move || tx.send(i).unwrap());
        }
        let got: Vec<i32> = (0..50).map(|_| rx.recv().unwrap()).collect();
        assert_eq!(got, (0..50).collect::<Vec<_>>());
    }

    #[test]
    fn flush_waits_for_prior_jobs() {
        let done = Arc::new(AtomicBool::new(false));
        let flag = done.clone();
        super::later(move || {
            std::thread::sleep(std::time::Duration::from_millis(20));
            flag.store(true, Ordering::Release);
        });
        super::flush();
        assert!(done.load(Ordering::Acquire), "flush must wait for queued work to finish");
    }

    #[test]
    fn worker_survives_panicking_job() {
        super::later(|| panic!("intentional persist test panic"));
        let ran = Arc::new(AtomicBool::new(false));
        let flag = ran.clone();
        super::later(move || {
            flag.store(true, Ordering::Release);
        });
        super::flush();
        assert!(ran.load(Ordering::Acquire), "persist thread must survive a panicking job");
    }
}
