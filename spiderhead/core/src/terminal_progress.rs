//! Small stderr-only terminal status helpers for indeterminate local operations.
//!
//! Everything here writes to stderr only, so stdout stays machine-readable. The spinner paints
//! only on a terminal; a redirected stream stays silent.

use std::io::{IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Interval between two spinner frames.
const FRAME_INTERVAL: Duration = Duration::from_millis(200);

/// Spinner glyphs, cycled in this order.
const MARKS: [char; 4] = ['|', '/', '-', '\\'];

/// Print one startup line `seohead: <command>` to stderr unless `quiet` is set.
///
/// Write errors are ignored: a closed stderr must never abort the command.
pub fn show_banner(command: &str, quiet: bool) {
    if quiet {
        return;
    }
    let mut stderr = std::io::stderr();
    let _ = writeln!(stderr, "seohead: {command}");
    let _ = stderr.flush();
}

/// Guard returned by [`elapsed_progress`]. The spinner runs while the guard is alive.
///
/// On drop the spinner stops and, if it painted anything, the line is terminated with a newline.
#[must_use = "the spinner stops as soon as the guard is dropped"]
pub struct ElapsedProgress {
    worker: Option<Worker>,
}

/// Background spinner thread plus the handles needed to stop it.
struct Worker {
    /// Dropping this sender wakes the thread and tells it to stop.
    stop: Option<Sender<()>>,
    handle: Option<JoinHandle<()>>,
    painted: Arc<AtomicBool>,
}

/// Start an elapsed-time spinner labelled `label` on stderr.
///
/// The spinner runs only when `enabled` is true and stderr is a terminal. Otherwise the guard is
/// inert and nothing is written.
pub fn elapsed_progress(label: &str, enabled: bool) -> ElapsedProgress {
    if !enabled || !std::io::stderr().is_terminal() {
        return ElapsedProgress { worker: None };
    }
    let (stop, stop_rx) = mpsc::channel::<()>();
    let painted = Arc::new(AtomicBool::new(false));
    let label = label.to_owned();
    let painted_worker = Arc::clone(&painted);
    let handle = std::thread::Builder::new()
        .name("seohead-elapsed".to_owned())
        .spawn(move || {
            let started = Instant::now();
            for mark in MARKS.iter().cycle() {
                let elapsed = started.elapsed().as_secs();
                let mut stderr = std::io::stderr();
                let written = write!(stderr, "\r{label}: {mark} {elapsed}s elapsed")
                    .and_then(|()| stderr.flush());
                if written.is_err() {
                    return;
                }
                painted_worker.store(true, Ordering::SeqCst);
                match stop_rx.recv_timeout(FRAME_INTERVAL) {
                    Err(RecvTimeoutError::Timeout) => {}
                    Ok(()) | Err(RecvTimeoutError::Disconnected) => return,
                }
            }
        });
    match handle {
        Ok(handle) => ElapsedProgress {
            worker: Some(Worker {
                stop: Some(stop),
                handle: Some(handle),
                painted,
            }),
        },
        // A spinner that cannot start is cosmetic: the operation itself must still run.
        Err(_) => ElapsedProgress { worker: None },
    }
}

impl Drop for ElapsedProgress {
    fn drop(&mut self) {
        let Some(mut worker) = self.worker.take() else {
            return;
        };
        drop(worker.stop.take());
        if let Some(handle) = worker.handle.take() {
            let _ = handle.join();
        }
        if worker.painted.load(Ordering::SeqCst) {
            let mut stderr = std::io::stderr();
            let _ = writeln!(stderr);
            let _ = stderr.flush();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quiet_banner_is_silent_and_loud_banner_does_not_panic() {
        show_banner("project list", true);
        show_banner("project list", false);
    }

    /// A disabled spinner never starts its thread, so dropping it writes nothing.
    #[test]
    fn disabled_spinner_has_no_worker() {
        let guard = elapsed_progress("scan", false);
        assert!(guard.worker.is_none());
    }
}
