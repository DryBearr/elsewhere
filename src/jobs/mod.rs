//! Runs long work in the background (saving playlists now; scans, NAS sync and conversions
//! later) and tells the app exactly what is happening, also across a sudden close.
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
//!   [`Status::Cancelled`]. A runner that panics fails its job; it does not take the app
//!   down.
//! - **Sudden close.** [`Jobs::open`] keeps the jobs in the library database. The delivery
//!   thread saves each state change and new log line (one transaction per round, so never
//!   half-written) before it tells the app. When the app starts again, finished jobs come
//!   back as history, and jobs that were queued or running come back as
//!   [`Status::Interrupted`]: the app offers [`Jobs::resume`] or [`Jobs::discard`].
//!
//! A feature plugs in with one line: [`Jobs::register`] a runner for its kind, then
//! [`Jobs::start`] jobs of that kind with params (text the runner understands, like a
//! link and options). The runner must be safe to run again with the same params: resuming
//! runs it again from the start, and it should skip what is already done (playlists do).

mod runner;
mod store;
#[cfg(test)]
mod tests;

use std::path::Path;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use runner::Inner;

use crate::logs;
use crate::problem::{self, Problem};

pub type JobId = u64;

/// How many jobs run at the same time in the app; the rest wait in order.
const APP_MAX_RUNNING: usize = 2;
/// The shortest time between two byte-progress updates of one job.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);
/// Log lines kept in memory per job; the oldest go first (the database keeps them all).
const MAX_LOG_LINES: usize = 5000;
/// Finished jobs kept in the database; older ones are removed when jobs are opened.
const KEEP_FINISHED: usize = 200;
/// Default watchdog: no progress for this long means stalled...
const STALL: Duration = Duration::from_secs(600);
/// ...and this long without reacting to the interrupt means stuck.
const GRACE: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Waiting for a free slot.
    Queued,
    Running,
    /// Was queued or running when the app closed. Waits for resume or discard.
    Interrupted,
    Done,
    Failed,
    Cancelled,
}

impl Status {
    /// Done, Failed or Cancelled: nothing will change any more (except by resume).
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
    pub last_error: Option<Problem>,
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

/// What a kind of job does, given its params. Returns Err with a message when the whole job
/// failed; problems with single items are reported through the handle and the job can
/// still be Done.
pub type Runner = Arc<dyn Fn(&Job, &str) -> Result<(), Problem> + Send + Sync>;

/// The jobs of the app, or of a test.
pub struct Jobs {
    inner: Arc<Inner>,
}

impl Jobs {
    /// Jobs kept in memory only, running at most `max_running` at a time.
    pub fn new(max_running: usize) -> Jobs {
        Jobs {
            inner: Inner::start(max_running.max(1), None, None),
        }
    }

    /// Jobs kept in the library database at `db`. Jobs from earlier runs come back:
    /// finished ones as history (the newest 200), unfinished ones as Interrupted.
    pub fn open(db: &Path, max_running: usize) -> Result<Jobs, String> {
        Jobs::open_named(db, max_running, None)
    }

    /// `name` names the parts in `health` ("jobs" for the app's jobs; None makes one up).
    fn open_named(db: &Path, max_running: usize, name: Option<&str>) -> Result<Jobs, String> {
        let mut store = store::Store::open(db)?;
        let saved = store.load()?;
        let next_id = store
            .next_id()
            .map_err(|err| format!("could not read the saved jobs: {err}"))?;
        let restored = runner::Restored {
            store,
            saved,
            next_id,
        };
        Ok(Jobs {
            inner: Inner::start(max_running.max(1), Some(restored), name),
        })
    }

    /// How long a running job may show no progress before the watchdog interrupts what it
    /// is doing (default 10 minutes), and how long it then has to react before it counts as
    /// stuck (default 60 s): a stuck job fails and the app needs a restart.
    pub fn set_watchdog(&self, stall: Duration, grace: Duration) {
        self.inner.set_watchdog(stall, grace);
    }

    /// Says how to run jobs of `kind`. Register every kind before starting or resuming
    /// jobs of it; registering again replaces the runner.
    pub fn register(
        &self,
        kind: &str,
        runner: impl Fn(&Job, &str) -> Result<(), Problem> + Send + Sync + 'static,
    ) {
        self.inner.register(kind, Arc::new(runner));
    }

    /// Sets the one listener, replacing any earlier one. It gets the current state of
    /// every job at once, then every change. It runs on the delivery thread: keep it
    /// short and hand the state to the UI thread.
    pub fn set_listener(&self, listener: impl Fn(JobState) + Send + Sync + 'static) {
        self.inner.set_listener(Arc::new(listener));
    }

    /// Queues a job of a registered `kind` and returns its id at once. `params` is kept
    /// with the job and handed to the runner, also when the job is resumed.
    pub fn start(&self, kind: &str, title: &str, params: &str) -> Result<JobId, String> {
        self.inner.queue(kind, title, params)
    }

    /// Asks a job to stop. A queued or interrupted job is cancelled at once; a running one
    /// when its runner notices (see Job::is_cancelled and Job::on_cancel). False if the
    /// job is unknown or already finished.
    pub fn cancel(&self, id: JobId) -> bool {
        self.inner.cancel(id)
    }

    /// Runs an Interrupted (or Failed) job again from the start, with its params: the
    /// "Resume" (or "Retry") button. Its counts start over; its log goes on.
    pub fn resume(&self, id: JobId) -> Result<(), String> {
        self.inner.resume(id)
    }

    /// Gives up an Interrupted job: it ends as Cancelled. False if it is not interrupted.
    pub fn discard(&self, id: JobId) -> bool {
        self.inner.discard(id)
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

    /// Removes a finished job, also from the database. False if it is unknown or not
    /// finished.
    pub fn forget(&self, id: JobId) -> bool {
        self.inner.forget(id)
    }
}

impl Drop for Jobs {
    /// Saves what is not saved yet and stops the delivery thread. Running jobs keep their
    /// threads until they end; in the database they stay running, so the next open
    /// shows them as Interrupted.
    fn drop(&mut self) {
        self.inner.shut_down();
    }
}

static GLOBAL: OnceLock<Jobs> = OnceLock::new();

/// Opens the app's jobs, kept in the library database at `db`. Call it once at startup;
/// later calls return the same jobs.
pub fn open_global(db: &Path) -> Result<&'static Jobs, String> {
    if let Some(jobs) = GLOBAL.get() {
        return Ok(jobs);
    }
    let jobs = Jobs::open_named(db, APP_MAX_RUNNING, Some("jobs"))?;
    Ok(GLOBAL.get_or_init(|| jobs))
}

/// The app's jobs, once `open_global` has run.
pub fn global() -> Option<&'static Jobs> {
    GLOBAL.get()
}

/// What a runner gets: reports progress and asks whether to stop. Cheap to clone; clones
/// report to the same job from any thread.
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

    /// Byte progress of the current item. Sent at most 4 times a second, and not saved:
    /// it means nothing after a restart.
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

    /// The current item failed; the job goes on. The problem goes to the job's log and the
    /// core's log.
    pub fn item_failed(&self, problem: &Problem) {
        let position = self.inner.current_position(self.id);
        problem::log(problem, "jobs", Some(self.id), position);
        let problem = problem.clone();
        self.inner.change(self.id, true, |job| {
            job.state.failed += 1;
            job.state.current = None;
            job.write_log(Level::Error, problem.text.clone());
            job.state.last_error = Some(problem);
        });
    }

    /// A line for the job's log (and, for warnings and errors, the core's log).
    pub fn log(&self, level: Level, message: &str) {
        if level != Level::Info {
            let line = match level {
                Level::Error => logs::error("jobs", message),
                _ => logs::warn("jobs", message),
            };
            line.job(self.id).write();
        }
        let message = message.to_owned();
        self.inner
            .change(self.id, true, |job| job.write_log(level, message));
    }

    /// Says the job is still working when nothing visible changes (a long merge, a slow
    /// server), so the watchdog does not take it for stuck.
    pub fn keep_alive(&self) {
        self.inner.keep_alive(self.id);
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

    /// Runs `interrupt` when the watchdog sees no progress for too long: the work should
    /// stop what it is doing (not the whole job) and try that step again.
    pub fn on_interrupt(&self, interrupt: impl Fn() + Send + Sync + 'static) {
        self.inner.on_interrupt(self.id, Arc::new(interrupt));
    }
}
