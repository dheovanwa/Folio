//! Shared test scaffolding.
//!
//! GTK must be initialised exactly once, on exactly one thread, and every widget
//! call must happen on that thread. `cargo test` runs test functions in parallel
//! on arbitrary threads, so all GTK work is funnelled through a dedicated owner
//! thread created here.

use std::sync::{mpsc, Mutex, OnceLock};

type Job = Box<dyn FnOnce() + Send>;

static SENDER: OnceLock<Mutex<Option<mpsc::Sender<Job>>>> = OnceLock::new();

/// Run `f` on the GTK owner thread and return its value.
///
/// Returns `None` when GTK cannot be initialised (e.g. headless CI with no
/// display), so callers can skip instead of failing.
pub fn with_gtk<T, F>(f: F) -> Option<T>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let slot = SENDER.get_or_init(|| Mutex::new(None));
    let mut guard = slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner());

    if guard.is_none() {
        let (job_tx, job_rx) = mpsc::channel::<Job>();
        let (ready_tx, ready_rx) = mpsc::channel::<bool>();
        std::thread::Builder::new()
            .name("gtk-test-owner".into())
            .spawn(move || {
                let ready = match gtk::init() {
                    Ok(()) => true,
                    Err(e) => {
                        eprintln!("gtk init failed on owner thread: {e}");
                        false
                    }
                };
                let _ = ready_tx.send(ready);
                if !ready {
                    return;
                }
                // Drain jobs forever; the process exits with the test binary.
                for job in job_rx {
                    job();
                }
            })
            .ok()?;
        match ready_rx.recv_timeout(std::time::Duration::from_secs(15)) {
            Ok(true) => *guard = Some(job_tx),
            _ => return None,
        }
    }

    let sender = guard.as_ref()?.clone();
    drop(guard);

    let (done_tx, done_rx) = mpsc::channel::<Result<T, String>>();
    sender
        .send(Box::new(move || {
            // A panic on the owner thread would otherwise look like a skip.
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).map_err(
                |payload| {
                    payload
                        .downcast_ref::<String>()
                        .cloned()
                        .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
                        .unwrap_or_else(|| "panic on the GTK thread".to_string())
                },
            );
            let _ = done_tx.send(outcome);
        }))
        .ok()?;

    match done_rx.recv_timeout(std::time::Duration::from_secs(60)) {
        Ok(Ok(value)) => Some(value),
        Ok(Err(message)) => panic!("{message}"),
        Err(_) => None,
    }
}

/// Marks a test as skipped when no GTK display is available.
pub fn skip_if_no_display() {
    eprintln!("skipping: no GTK display available");
}
