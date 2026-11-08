//! The machinery behind `jobs`: one table of jobs behind a lock, a thread per running job,
//! and one delivery thread that saves changed jobs and then hands their states to the
//! listener.
//!
//! Locks are always taken in this order: table, then store (or runners). The delivery
//! thread never holds the table while it writes to the store or calls the listener.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use super::store::{Saved, Store};
use super::{
    Job, JobId, JobState, Level, LogLine, MAX_LOG_LINES, PROGRESS_INTERVAL, Runner, Status,
};

type Listener = Arc<dyn Fn(JobState) + Send + Sync>;
type Stop = Arc<dyn Fn() + Send + Sync>;

/// What `Jobs::open` found in the database.
pub(super) struct Restored {
    pub store: Store,
    pub saved: Vec<Saved>,
    pub next_id: JobId,
}

pub(super) struct Inner {
    table: Mutex<Table>,
    /// Wakes the delivery thread when something changed (or on shutdown).
    changed: Condvar,
    listener: Mutex<Option<Listener>>,
    runners: Mutex<HashMap<String, Runner>>,
    store: Mutex<Option<Store>>,
    max_running: usize,
}

#[derive(Default)]
struct Table {
    jobs: BTreeMap<JobId, Entry>,
    queue: VecDeque<JobId>,
    running: usize,
    next_id: JobId,
    shut_down: bool,
}

pub(super) struct Entry {
    pub(super) state: JobState,
    params: String,
    log: VecDeque<LogLine>,
    cancelled: bool,
    stops: Vec<Stop>,
    /// What the listener still has to get: nothing, a throttled progress change, or a
    /// change to send right away.
    pending: Pending,
    last_sent: Option<Instant>,
    /// Whether the state changed since it was last saved (byte progress does not count).
    unsaved: bool,
    /// The highest log line number saved. Lines above it must stay in memory until saved.
    log_saved: usize,
    /// False for jobs kept in memory only: their log can always be trimmed.
    kept: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Pending {
    Nothing,
    Throttled,
    Now,
}

impl Entry {
    fn new(state: JobState, params: String, log: Vec<LogLine>, kept: bool) -> Entry {
        Entry {
            log_saved: state.log_lines,
            state,
            params,
            log: log.into(),
            cancelled: false,
            stops: Vec::new(),
            pending: Pending::Now,
            last_sent: None,
            unsaved: false,
            kept,
        }
    }

    pub(super) fn write_log(&mut self, level: Level, message: String) {
        self.state.log_lines += 1;
        let at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |time| time.as_secs());
        self.log.push_back(LogLine {
            number: self.state.log_lines,
            level,
            message,
            at,
        });
        // Trim the oldest lines, but never ones not saved yet.
        while self.log.len() > MAX_LOG_LINES
            && self
                .log
                .front()
                .is_some_and(|line| !self.kept || line.number <= self.log_saved)
        {
            self.log.pop_front();
        }
    }

    /// Bumps the revision; the change is sent at once (and saved) or, for byte progress,
    /// throttled (and not saved).
    fn touch(&mut self, now: bool) {
        self.state.revision += 1;
        let pending = if now {
            Pending::Now
        } else {
            Pending::Throttled
        };
        self.pending = self.pending.max(pending);
        self.unsaved |= now;
    }
}

impl Inner {
    pub(super) fn start(max_running: usize, restored: Option<Restored>) -> Arc<Inner> {
        let mut table = Table {
            next_id: 1,
            ..Table::default()
        };
        let store = restored.map(|restored| {
            table.next_id = restored.next_id;
            for saved in restored.saved {
                let id = saved.state.id;
                table
                    .jobs
                    .insert(id, Entry::new(saved.state, saved.params, saved.log, true));
            }
            restored.store
        });
        let inner = Arc::new(Inner {
            table: Mutex::new(table),
            changed: Condvar::new(),
            listener: Mutex::new(None),
            runners: Mutex::new(HashMap::new()),
            store: Mutex::new(store),
            max_running,
        });
        let delivery = Arc::clone(&inner);
        thread::Builder::new()
            .name("jobs-delivery".to_owned())
            .spawn(move || delivery.deliver())
            .expect("could not start the jobs delivery thread");
        inner
    }

    /// The lock, even after a panic elsewhere left it poisoned: the table stays usable.
    fn lock(&self) -> MutexGuard<'_, Table> {
        self.table
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn store(&self) -> MutexGuard<'_, Option<Store>> {
        self.store
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn runner(&self, kind: &str) -> Option<Runner> {
        self.runners
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(kind)
            .cloned()
    }

    pub(super) fn register(&self, kind: &str, runner: Runner) {
        self.runners
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(kind.to_owned(), runner);
    }

    pub(super) fn set_listener(&self, listener: Listener) {
        *self
            .listener
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(listener);
        // The new listener gets every job's current state.
        let mut table = self.lock();
        for entry in table.jobs.values_mut() {
            entry.pending = Pending::Now;
        }
        self.changed.notify_all();
    }

    pub(super) fn queue(
        self: &Arc<Self>,
        kind: &str,
        title: &str,
        params: &str,
    ) -> Result<JobId, String> {
        if self.runner(kind).is_none() {
            return Err(format!("no runner is registered for {kind} jobs"));
        }
        let mut table = self.lock();
        if table.shut_down {
            return Err("the jobs are shut down".to_owned());
        }
        let id = table.next_id;
        let state = JobState {
            id,
            revision: 1,
            kind: kind.to_owned(),
            title: title.to_owned(),
            status: Status::Queued,
            done: 0,
            failed: 0,
            total: None,
            current: None,
            last_error: None,
            log_lines: 0,
        };
        // Saved before it runs, so a crash right after this still finds the job.
        let kept = match self.store().as_ref() {
            Some(store) => {
                store
                    .insert(&state, params)
                    .map_err(|err| format!("could not save the job: {err}"))?;
                true
            }
            None => false,
        };
        table.next_id += 1;
        table
            .jobs
            .insert(id, Entry::new(state, params.to_owned(), Vec::new(), kept));
        table.queue.push_back(id);
        self.run_queued(&mut table);
        self.changed.notify_all();
        Ok(id)
    }

    /// Starts queued jobs while there are free slots.
    fn run_queued(self: &Arc<Self>, table: &mut Table) {
        while table.running < self.max_running && !table.shut_down {
            let Some(id) = table.queue.pop_front() else {
                return;
            };
            let Some(entry) = table.jobs.get_mut(&id) else {
                continue;
            };
            let Some(runner) = self.runner(&entry.state.kind) else {
                let kind = entry.state.kind.clone();
                finish(
                    entry,
                    Err(format!("no runner is registered for {kind} jobs")),
                );
                continue;
            };
            entry.state.status = Status::Running;
            entry.touch(true);
            let params = entry.params.clone();
            table.running += 1;
            let inner = Arc::clone(self);
            let spawned = thread::Builder::new()
                .name(format!("job-{id}"))
                .spawn(move || inner.run(id, runner, params));
            if let Err(err) = spawned {
                table.running -= 1;
                if let Some(entry) = table.jobs.get_mut(&id) {
                    finish(entry, Err(format!("could not start a thread: {err}")));
                }
            }
        }
    }

    /// The body of a job's thread.
    fn run(self: Arc<Self>, id: JobId, runner: Runner, params: String) {
        let job = Job {
            id,
            inner: Arc::clone(&self),
        };
        let result =
            catch_unwind(AssertUnwindSafe(|| runner(&job, &params))).unwrap_or_else(|panic| {
                let reason = panic
                    .downcast_ref::<&str>()
                    .map(|text| text.to_string())
                    .or_else(|| panic.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "unknown reason".to_owned());
                Err(format!("the job crashed: {reason}"))
            });
        let mut table = self.lock();
        table.running -= 1;
        if let Some(entry) = table.jobs.get_mut(&id) {
            finish(entry, result);
        }
        self.run_queued(&mut table);
        self.changed.notify_all();
    }

    /// Applies `edit` to a running job and bumps its revision. `now` is false only for byte
    /// progress, which is throttled and not saved. Jobs that are not running do not change.
    pub(super) fn change(&self, id: JobId, now: bool, edit: impl FnOnce(&mut Entry)) {
        let mut table = self.lock();
        let Some(entry) = table.jobs.get_mut(&id) else {
            return;
        };
        if entry.state.status != Status::Running {
            return;
        }
        edit(entry);
        entry.touch(now);
        self.changed.notify_all();
    }

    pub(super) fn cancel(&self, id: JobId) -> bool {
        let stops = {
            let mut table = self.lock();
            let Some(entry) = table.jobs.get_mut(&id) else {
                return false;
            };
            if entry.state.status.is_finished() {
                return false;
            }
            entry.cancelled = true;
            if matches!(entry.state.status, Status::Queued | Status::Interrupted) {
                // Not running: it ends now, and its runner never starts.
                finish(entry, Ok(()));
            }
            let stops = entry.stops.clone();
            table.queue.retain(|queued| *queued != id);
            self.changed.notify_all();
            stops
        };
        // Outside the lock: a stop function may call back into the job.
        for stop in stops {
            stop();
        }
        true
    }

    pub(super) fn resume(self: &Arc<Self>, id: JobId) -> Result<(), String> {
        let mut table = self.lock();
        let entry = table.jobs.get_mut(&id).ok_or("no such job")?;
        if !matches!(entry.state.status, Status::Interrupted | Status::Failed) {
            return Err(format!("a {:?} job cannot be resumed", entry.state.status));
        }
        if self.runner(&entry.state.kind).is_none() {
            return Err(format!(
                "no runner is registered for {} jobs",
                entry.state.kind
            ));
        }
        // It runs again from the start, so its counts start over too.
        let state = &mut entry.state;
        state.status = Status::Queued;
        state.done = 0;
        state.failed = 0;
        state.total = None;
        state.current = None;
        state.last_error = None;
        entry.cancelled = false;
        entry.stops.clear();
        entry.write_log(Level::Info, "Resumed.".to_owned());
        entry.touch(true);
        table.queue.push_back(id);
        self.run_queued(&mut table);
        self.changed.notify_all();
        Ok(())
    }

    pub(super) fn discard(&self, id: JobId) -> bool {
        let mut table = self.lock();
        let Some(entry) = table.jobs.get_mut(&id) else {
            return false;
        };
        if entry.state.status != Status::Interrupted {
            return false;
        }
        entry.write_log(Level::Info, "Discarded.".to_owned());
        entry.cancelled = true;
        finish(entry, Ok(()));
        self.changed.notify_all();
        true
    }

    pub(super) fn is_cancelled(&self, id: JobId) -> bool {
        self.lock()
            .jobs
            .get(&id)
            .is_some_and(|entry| entry.cancelled)
    }

    pub(super) fn on_cancel(&self, id: JobId, stop: Stop) {
        let cancelled = {
            let mut table = self.lock();
            let Some(entry) = table.jobs.get_mut(&id) else {
                return;
            };
            entry.stops.push(Arc::clone(&stop));
            entry.cancelled
        };
        if cancelled {
            stop();
        }
    }

    pub(super) fn state(&self, id: JobId) -> Option<JobState> {
        self.lock().jobs.get(&id).map(|entry| entry.state.clone())
    }

    pub(super) fn states(&self) -> Vec<JobState> {
        self.lock()
            .jobs
            .values()
            .map(|entry| entry.state.clone())
            .collect()
    }

    pub(super) fn logs(&self, id: JobId, since: usize) -> Vec<LogLine> {
        self.lock()
            .jobs
            .get(&id)
            .map(|entry| {
                entry
                    .log
                    .iter()
                    .filter(|line| line.number > since)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    pub(super) fn forget(&self, id: JobId) -> bool {
        let mut table = self.lock();
        match table.jobs.get(&id) {
            Some(entry) if entry.state.status.is_finished() => {}
            _ => return false,
        }
        if let Some(store) = self.store().as_ref()
            && store.delete(id).is_err()
        {
            return false;
        }
        table.jobs.remove(&id);
        true
    }

    /// Stops the delivery thread after it saves what is not saved yet.
    pub(super) fn shut_down(&self) {
        self.lock().shut_down = true;
        self.changed.notify_all();
    }

    /// The delivery thread: waits for changes, saves the changed jobs, then hands each
    /// changed job's latest state to the listener, in job order and outside the lock.
    fn deliver(self: Arc<Self>) {
        let mut table = self.lock();
        loop {
            let closing = table.shut_down;
            let now = Instant::now();
            let mut ready = Vec::new();
            let mut to_save = Vec::new();
            let mut lines = Vec::new();
            let mut next_wake: Option<Instant> = None;
            for entry in table.jobs.values_mut() {
                let due = match entry.pending {
                    Pending::Nothing => None,
                    Pending::Now => Some(now),
                    Pending::Throttled => {
                        Some(entry.last_sent.map_or(now, |sent| sent + PROGRESS_INTERVAL))
                    }
                };
                let due_now = due.is_some_and(|due| due <= now);
                if let Some(due) = due.filter(|_| !due_now) {
                    next_wake = Some(next_wake.map_or(due, |wake| wake.min(due)));
                }
                if entry.unsaved && entry.kept && (due_now || closing) {
                    to_save.push((entry.state.clone(), entry.log_saved));
                    lines.extend(
                        entry
                            .log
                            .iter()
                            .filter(|line| line.number > entry.log_saved)
                            .map(|line| (entry.state.id, line.clone())),
                    );
                    entry.log_saved = entry.state.log_lines;
                    entry.unsaved = false;
                }
                if due_now {
                    ready.push(entry.state.clone());
                    entry.pending = Pending::Nothing;
                    entry.last_sent = Some(now);
                }
            }

            if ready.is_empty() && to_save.is_empty() {
                if closing {
                    return;
                }
                table = match next_wake {
                    Some(wake) => {
                        let wait = wake.saturating_duration_since(now);
                        self.changed
                            .wait_timeout(table, wait)
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .0
                    }
                    None => self
                        .changed
                        .wait(table)
                        .unwrap_or_else(|poisoned| poisoned.into_inner()),
                };
                continue;
            }
            drop(table);

            // Saved first, so the app never shows what a crash could undo.
            let states: Vec<JobState> = to_save.iter().map(|(state, _)| state.clone()).collect();
            let saved = match self.store().as_mut() {
                Some(store) if !states.is_empty() => store.save(&states, &lines),
                _ => Ok(()),
            };

            let listener = self
                .listener
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone();
            let mut failed = Vec::new();
            if let Some(listener) = listener.filter(|_| !closing) {
                for state in ready {
                    let id = state.id;
                    // A listener that panics must not stop delivery or lose a final state:
                    // the job is sent again after PROGRESS_INTERVAL.
                    if catch_unwind(AssertUnwindSafe(|| listener(state))).is_err() {
                        failed.push(id);
                    }
                }
            }

            table = self.lock();
            for id in failed {
                if let Some(entry) = table.jobs.get_mut(&id) {
                    entry.pending = entry.pending.max(Pending::Throttled);
                }
            }
            if let Err(err) = saved {
                // Try again next round: nothing that was not saved counts as saved.
                eprintln!("jobs: could not save job states: {err}");
                for (state, log_saved) in &to_save {
                    if let Some(entry) = table.jobs.get_mut(&state.id) {
                        entry.unsaved = true;
                        entry.log_saved = entry.log_saved.min(*log_saved);
                    }
                }
                if closing {
                    return;
                }
                table = self
                    .changed
                    .wait_timeout(table, PROGRESS_INTERVAL)
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .0;
            }
        }
    }
}

/// Ends a job exactly once: Cancelled if it was asked to stop, else Done or Failed.
fn finish(entry: &mut Entry, result: Result<(), String>) {
    if entry.state.status.is_finished() {
        return;
    }
    entry.state.status = match (&result, entry.cancelled) {
        (_, true) => Status::Cancelled,
        (Ok(()), false) => Status::Done,
        (Err(_), false) => Status::Failed,
    };
    if let (Err(error), false) = (result, entry.cancelled) {
        entry.state.last_error = Some(error.clone());
        entry.write_log(Level::Error, error);
    }
    entry.state.current = None;
    entry.touch(true);
}
