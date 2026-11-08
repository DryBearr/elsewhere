//! Runs long work in the background (saving playlists now; scans, NAS sync and conversions
//! later) and tells the app exactly what is happening.
//!
//! How the app sees a job:
//!
//! - **State.** Every change to a job updates its [`JobState`] and bumps its `revision`.
//!   The listener gets the whole current state of the job, and the app simply replaces
//!   what it shows: nothing to add up, nothing to get out of step. One delivery thread
//!   calls the listener, so a job's states arrive in order, and the work never waits for
//!   the app. Byte progress is sent at most 4 times a second (values in between are
//!   skipped, which is fine for a progress bar); everything else is sent right away.
//! - **Log.** Info, warnings and errors go to the job's log, numbered from 1, and are never
//!   dropped by the throttling. `JobState::log_lines` says how many there are; the app
//!   reads new ones with [`Jobs::logs`].
//! - **End.** Every job ends exactly once: [`Status::Done`], [`Status::Failed`] or
//!   [`Status::Cancelled`]. A work function that panics fails its job; it does not take
//!   the app down.
//!
//! A feature plugs in by handing [`Jobs::start`] a function that does the work and reports
//! through its [`Job`] handle; see `playlists::run` for one.
//!
//! The jobs live in memory for now: they are lost if the app is closed.

mod runner;
#[cfg(test)]
mod tests;

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use runner::Inner;

pub type JobId = u64;

/// How many jobs run at the same time in the app; the rest wait in order.
const APP_MAX_RUNNING: usize = 2;
/// The shortest time between two byte-progress updates of one job.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);
/// Log lines kept in memory per job; the oldest go first.
const MAX_LOG_LINES: usize = 5000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Waiting for a free slot.
    Queued,
    Running,
    Done,
    Failed,
    Cancelled,
}

impl Status {
    /// Done, Failed or Cancelled: nothing will change any more.
    pub fn is_finished(self) -> bool {
        matches!(self, Status::Done | Status::Failed | Status::Cancelled)
    }
}

/// Everything the app shows about a job.
#[derive(Debug, Clone, PartialEq)]
pub struct JobState {
    pub id: JobId,
    /// Goes up with every change. A state with a lower revision than the one shown is old.
    pub revision: u64,
    /// What kind of work it is, e.g. "playlist".
    pub kind: String,
    pub title: String,
    pub status: Status,
    /// Items finished (saved, or found already saved).
    pub done: usize,
    pub failed: usize,
    /// How many items there are, once known.
    pub total: Option<usize>,
    /// The item being worked on.
    pub current: Option<Current>,
    /// The latest error: of the job when it failed, else of the last failed item.
    pub last_error: Option<String>,
    /// How many lines the job's log has (see Jobs::logs).
    pub log_lines: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Current {
    pub position: usize,
    pub title: String,
    pub bytes: u64,
    pub total_bytes: Option<u64>,
    /// Bytes per second.
    pub speed: Option<f64>,
    /// Seconds left.
    pub eta: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LogLine {
    /// 1, 2, 3, ... in the order the lines were written.
    pub number: usize,
    pub level: Level,
    pub message: String,
    /// Seconds since 1970.
    pub at: u64,
}

/// The work of a job. Returns Err with a message when the whole job failed; problems with
/// single items are reported through the handle and the job can still be Done.
pub type Work = Box<dyn FnOnce(&Job) -> Result<(), String> + Send + 'static>;

/// The jobs of the app, or of a test.
pub struct Jobs {
    inner: Arc<Inner>,
}

impl Jobs {
    /// Jobs that run at most `max_running` at a time (at least 1).
    pub fn new(max_running: usize) -> Jobs {
        Jobs {
            inner: Inner::start(max_running.max(1)),
        }
    }

    /// Sets the one listener, replacing any earlier one. It gets the current state of
    /// every job at once, then every change. It runs on the delivery thread: keep it
    /// short and hand the state to the UI thread.
    pub fn set_listener(&self, listener: impl Fn(JobState) + Send + Sync + 'static) {
        self.inner.set_listener(Arc::new(listener));
    }

    /// Queues `work` and returns its id at once.
    pub fn start(
        &self,
        kind: &str,
        title: &str,
        work: impl FnOnce(&Job) -> Result<(), String> + Send + 'static,
    ) -> JobId {
        self.inner.queue(kind, title, Box::new(work))
    }

    /// Asks a job to stop. A queued job is cancelled at once; a running one when its work
    /// notices (see Job::is_cancelled and Job::on_cancel). False if the job is unknown
    /// or already finished.
    pub fn cancel(&self, id: JobId) -> bool {
        self.inner.cancel(id)
    }

    pub fn get(&self, id: JobId) -> Option<JobState> {
        self.inner.state(id)
    }

    /// Every job, oldest first.
    pub fn list(&self) -> Vec<JobState> {
        self.inner.states()
    }

    /// The job's log lines numbered above `since` (0 for all).
    pub fn logs(&self, id: JobId, since: usize) -> Vec<LogLine> {
        self.inner.logs(id, since)
    }

    /// Removes a finished job from the list. False if it is unknown or still going.
    pub fn forget(&self, id: JobId) -> bool {
        self.inner.forget(id)
    }
}

impl Drop for Jobs {
    fn drop(&mut self) {
        self.inner.shut_down();
    }
}

/// The app's jobs, created on first use.
pub fn global() -> &'static Jobs {
    static JOBS: OnceLock<Jobs> = OnceLock::new();
    JOBS.get_or_init(|| Jobs::new(APP_MAX_RUNNING))
}

/// What a work function gets: reports progress and asks whether to stop. Cheap to clone;
/// clones report to the same job from any thread.
#[derive(Clone)]
pub struct Job {
    id: JobId,
    inner: Arc<Inner>,
}

impl Job {
    pub fn id(&self) -> JobId {
        self.id
    }

    pub fn set_title(&self, title: &str) {
        let title = title.to_owned();
        self.inner
            .change(self.id, true, |job| job.state.title = title);
    }

    pub fn set_total(&self, total: usize) {
        self.inner
            .change(self.id, true, |job| job.state.total = Some(total));
    }

    /// Starts work on an item: it becomes `current`.
    pub fn item_started(&self, position: usize, title: &str) {
        let current = Current {
            position,
            title: title.to_owned(),
            bytes: 0,
            total_bytes: None,
            speed: None,
            eta: None,
        };
        self.inner
            .change(self.id, true, |job| job.state.current = Some(current));
    }

    /// Byte progress of the current item. Sent at most 4 times a second.
    pub fn progress(
        &self,
        bytes: u64,
        total_bytes: Option<u64>,
        speed: Option<f64>,
        eta: Option<u64>,
    ) {
        self.inner.change(self.id, false, |job| {
            if let Some(current) = &mut job.state.current {
                current.bytes = bytes;
                current.total_bytes = total_bytes;
                current.speed = speed;
                current.eta = eta;
            }
        });
    }

    /// The current item is finished (also: found already done).
    pub fn item_done(&self) {
        self.inner.change(self.id, true, |job| {
            job.state.done += 1;
            job.state.current = None;
        });
    }

    /// The current item failed; the job goes on. The error also goes to the log.
    pub fn item_failed(&self, error: &str) {
        let error = error.to_owned();
        self.inner.change(self.id, true, |job| {
            job.state.failed += 1;
            job.state.current = None;
            job.state.last_error = Some(error.clone());
            job.write_log(Level::Error, error);
        });
    }

    pub fn log(&self, level: Level, message: &str) {
        let message = message.to_owned();
        self.inner
            .change(self.id, true, |job| job.write_log(level, message));
    }

    /// Whether someone asked the job to stop. Long work checks it between steps.
    pub fn is_cancelled(&self) -> bool {
        self.inner.is_cancelled(self.id)
    }

    /// Runs `stop` when the job is cancelled (at once if it already is). Lets work that
    /// blocks, like a download, be interrupted.
    pub fn on_cancel(&self, stop: impl Fn() + Send + Sync + 'static) {
        self.inner.on_cancel(self.id, Arc::new(stop));
    }
}
