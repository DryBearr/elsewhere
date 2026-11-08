//! The machinery behind `jobs`: one table of jobs behind a lock, a thread per running job,
//! one delivery thread that saves changed jobs and then hands their states to the
//! listener, and one watchdog thread. Delivery and watchdog run under `health::supervise`:
//! when one crashes it is restarted, and delivery then saves and sends everything again.
//!
//! Locks are always taken in this order: table, then store (or runners). The delivery
//! thread never holds the table while it writes to the store or calls the listener.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::store::{Saved, Store};
use super::{
    GRACE, Job, JobId, JobState, Level, LogLine, MAX_LOG_LINES, PROGRESS_INTERVAL, Runner, STALL,
    Status,
};
use crate::health;
use crate::logs;
use crate::problem::{self, Problem};

type Listener = Arc<dyn Fn(JobState) + Send + Sync>;
type Stop = Arc<dyn Fn() + Send + Sync>;

/// Store saves that may fail in a row (each followed by reopening the database) before the
/// store counts as broken.
const STORE_FAILURES: usize = 5;

/// What `Jobs::open` found in the database.
pub(super) struct Restored {
    pub store: Store,
    pub saved: Vec<Saved>,
    pub next_id: JobId,
}

pub(super) struct Inner {
    /// Names this instance's parts in `health`: "<name>-delivery", "-watchdog", "-store",
    /// "-workers".
    pub(super) name: String,
    table: Mutex<Table>,
    /// Wakes the delivery thread when something changed (or on shutdown).
    changed: Condvar,
    listener: Mutex<Option<Listener>>,
    runners: Mutex<HashMap<String, Runner>>,
    store: Mutex<Option<Store>>,
    store_failures: AtomicUsize,
    delivery_runs: AtomicUsize,
    watchdog: Mutex<(Duration, Duration)>,
    max_running: usize,
    #[cfg(test)]
    pub(super) faults: Faults,
}

/// Failures tests can ask for, per jobs instance.
#[cfg(test)]
#[derive(Default)]
pub(super) struct Faults {
    /// The delivery thread panics this many times.
    pub panic_delivery: AtomicUsize,
    /// This many saves fail.
    pub fail_saves: AtomicUsize,
}

/// Counts one down; false when it is already 0.
#[cfg(test)]
fn take(counter: &AtomicUsize) -> bool {
    let mut left = counter.load(Ordering::SeqCst);
    while left > 0 {
        match counter.compare_exchange(left, left - 1, Ordering::SeqCst, Ordering::SeqCst) {
            Ok(_) => return true,
            Err(now) => left = now,
        }
    }
    false
}

#[derive(Default)]
struct Table {
    jobs: BTreeMap<JobId, Entry>,
    queue: VecDeque<JobId>,
    running: usize,
    next_id: JobId,
    shut_down: bool,
    /// Set by the delivery thread when it saved the last changes after shut_down.
    closed: bool,
}

pub(super) struct Entry {
    pub(super) state: JobState,
    params: String,
    log: VecDeque<LogLine>,
    cancelled: bool,
    stops: Vec<Stop>,
    interrupts: Vec<Stop>,
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
    /// The last sign of life of a running job (watchdog).
    alive: Instant,
    /// When the watchdog interrupted it, until it shows life again.
    interrupted: Option<Instant>,
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
            interrupts: Vec::new(),
            pending: Pending::Now,
            last_sent: None,
            unsaved: false,
            kept,
            alive: Instant::now(),
            interrupted: None,
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
    /// throttled (and not saved). Any change is a sign of life.
    fn touch(&mut self, now: bool) {
        self.state.revision += 1;
        let pending = if now {
            Pending::Now
        } else {
            Pending::Throttled
        };
        self.pending = self.pending.max(pending);
        self.unsaved |= now;
        self.alive = Instant::now();
        self.interrupted = None;
    }
}

impl Inner {
    /// Starts the jobs: the table, then the supervised delivery and watchdog threads.
    /// `name` names the parts in `health`; None makes a unique one (tests run many).
    pub(super) fn start(
        max_running: usize,
        restored: Option<Restored>,
        name: Option<&str>,
    ) -> Arc<Inner> {
        static INSTANCES: AtomicUsize = AtomicUsize::new(0);
        let name = name.map_or_else(
            || format!("jobs#{}", INSTANCES.fetch_add(1, Ordering::SeqCst)),
            str::to_owned,
        );
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
            name,
            table: Mutex::new(table),
            changed: Condvar::new(),
            listener: Mutex::new(None),
            runners: Mutex::new(HashMap::new()),
            store: Mutex::new(store),
            store_failures: AtomicUsize::new(0),
            delivery_runs: AtomicUsize::new(0),
            watchdog: Mutex::new((STALL, GRACE)),
            max_running,
            #[cfg(test)]
            faults: Faults::default(),
        });
        let delivery = Arc::clone(&inner);
        let started = health::supervise(format!("{}-delivery", inner.name), move || {
            delivery.deliver()
        });
        let watchdog = Arc::clone(&inner);
        let watching =
            health::supervise(format!("{}-watchdog", inner.name), move || watchdog.watch());
        for (part, result) in [("delivery", started), ("watchdog", watching)] {
            if let Err(err) = result {
                health::broken(
                    &format!("{}-{part}", inner.name),
                    format!("could not start a thread: {err}"),
                );
            }
        }
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

    pub(super) fn set_watchdog(&self, stall: Duration, grace: Duration) {
        *self
            .watchdog
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = (stall, grace);
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
                    Err(Problem::bug(format!(
                        "no runner is registered for {kind} jobs"
                    ))),
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
                    finish(
                        entry,
                        Err(Problem::bug(format!("could not start a thread: {err}"))),
                    );
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
                Err(Problem::bug(format!("the job crashed: {reason}")))
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

    pub(super) fn keep_alive(&self, id: JobId) {
        if let Some(entry) = self.lock().jobs.get_mut(&id) {
            entry.alive = Instant::now();
            entry.interrupted = None;
        }
    }

    pub(super) fn current_position(&self, id: JobId) -> Option<usize> {
        self.lock()
            .jobs
            .get(&id)
            .and_then(|entry| entry.state.current.as_ref())
            .map(|current| current.position)
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
        entry.interrupts.clear();
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

    pub(super) fn on_interrupt(&self, id: JobId, interrupt: Stop) {
        if let Some(entry) = self.lock().jobs.get_mut(&id) {
            entry.interrupts.push(interrupt);
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

    /// Stops the delivery and watchdog threads. Waits (at most 5 s) until delivery saved
    /// what is not saved yet.
    pub(super) fn shut_down(&self) {
        let mut table = self.lock();
        table.shut_down = true;
        self.changed.notify_all();
        let _ = self
            .changed
            .wait_timeout_while(table, Duration::from_secs(5), |table| !table.closed);
    }

    /// The delivery thread: waits for changes, saves the changed jobs, then hands each
    /// changed job's latest state to the listener, in job order and outside the lock.
    fn deliver(self: &Arc<Self>) {
        let mut table = self.lock();
        // Restarting after a crash: whatever the crashed round did not finish is sent and
        // saved again (saving is idempotent, sending replaces the shown state). Rare, so
        // simply everything in memory.
        let restarted = self.delivery_runs.fetch_add(1, Ordering::SeqCst) > 0;
        for entry in table.jobs.values_mut().filter(|_| restarted) {
            entry.pending = Pending::Now;
            if entry.kept {
                entry.unsaved = true;
                entry.log_saved = entry
                    .log
                    .front()
                    .map_or(entry.log_saved, |first| first.number.saturating_sub(1))
                    .min(entry.log_saved);
            }
        }
        loop {
            #[cfg(test)]
            if take(&self.faults.panic_delivery) {
                panic!("test: the delivery thread crashes");
            }
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
                // Saved at once, also when a failed save left it unsaved with nothing to send.
                if entry.unsaved && entry.kept {
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
                    table.closed = true;
                    self.changed.notify_all();
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
            let saved = if states.is_empty() {
                Ok(())
            } else {
                self.save(&states, &lines)
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
            if saved.is_err() {
                // Try again next round: nothing that was not saved counts as saved.
                for (state, log_saved) in &to_save {
                    if let Some(entry) = table.jobs.get_mut(&state.id) {
                        entry.unsaved = true;
                        entry.log_saved = entry.log_saved.min(*log_saved);
                    }
                }
                if closing {
                    table.closed = true;
                    self.changed.notify_all();
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

    /// Saves states and lines. On failure the store reopens its database for the next
    /// round; after STORE_FAILURES failures in a row it counts as broken.
    fn save(&self, states: &[JobState], lines: &[(JobId, LogLine)]) -> Result<(), ()> {
        let part = format!("{}-store", self.name);
        let mut store = self.store();
        let Some(store) = store.as_mut() else {
            return Ok(());
        };
        #[cfg(test)]
        let saved = if take(&self.faults.fail_saves) {
            Err(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(10),
                Some("test: disk I/O error".to_owned()),
            ))
        } else {
            store.save(states, lines)
        };
        #[cfg(not(test))]
        let saved = store.save(states, lines);
        match saved {
            Ok(()) => {
                if self.store_failures.swap(0, Ordering::SeqCst) > 0 {
                    logs::info("jobs", "Saving jobs works again").write();
                    health::healthy(&part);
                }
                Ok(())
            }
            Err(err) => {
                let failures = self.store_failures.fetch_add(1, Ordering::SeqCst) + 1;
                let problem = problem::from_sqlite(&err, "saving jobs");
                if failures >= STORE_FAILURES {
                    if !matches!(health::state(&part), health::State::Broken(_)) {
                        health::broken(&part, problem.detail.clone());
                    }
                } else {
                    logs::warn(
                        "jobs",
                        format!("Saving jobs failed ({failures} in a row); reopening the database"),
                    )
                    .detail(problem.detail.clone())
                    .write();
                    let _ = store.reopen();
                }
                Err(())
            }
        }
    }

    /// The watchdog thread: a running job with no sign of life for `stall` is interrupted
    /// (its runner tries the step again); one that still shows no life `grace` later is
    /// stuck: it fails, and the app needs a restart (its thread may never come back).
    fn watch(self: &Arc<Self>) {
        loop {
            let (stall, grace) = *self
                .watchdog
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let tick =
                (stall.min(grace) / 4).clamp(Duration::from_millis(10), Duration::from_secs(1));
            thread::sleep(tick);
            let mut to_interrupt = Vec::new();
            let mut stuck = false;
            {
                let mut table = self.lock();
                if table.shut_down {
                    return;
                }
                let now = Instant::now();
                for entry in table.jobs.values_mut() {
                    if entry.state.status != Status::Running {
                        continue;
                    }
                    match entry.interrupted {
                        None if now.duration_since(entry.alive) > stall => {
                            entry.interrupted = Some(now);
                            let message =
                                format!("No progress for {}; trying again.", duration_text(stall));
                            entry.write_log(Level::Warning, message.clone());
                            entry.state.revision += 1;
                            entry.pending = Pending::Now;
                            entry.unsaved = true;
                            logs::warn("jobs", message).job(entry.state.id).write();
                            to_interrupt.extend(entry.interrupts.iter().cloned());
                        }
                        Some(at) if now.duration_since(at) > grace => {
                            let problem = Problem::bug(format!(
                                "job {} stopped responding for {}",
                                entry.state.id,
                                duration_text(stall + grace)
                            ));
                            // Broken first: whoever sees the failed job (the app checks
                            // health after a Bug) must already see the restart need.
                            health::broken(
                                &format!("{}-workers", self.name),
                                problem.detail.clone(),
                            );
                            stuck = true;
                            finish(entry, Err(problem));
                        }
                        _ => {}
                    }
                }
                if !to_interrupt.is_empty() || stuck {
                    self.changed.notify_all();
                }
            }
            for interrupt in to_interrupt {
                interrupt();
            }
        }
    }
}

fn duration_text(duration: Duration) -> String {
    match duration.as_secs() {
        0 => format!("{} ms", duration.as_millis()),
        seconds if seconds < 120 => format!("{seconds} s"),
        seconds => format!("{} minutes", seconds / 60),
    }
}

/// Ends a job exactly once: Cancelled if it was asked to stop, else Done or Failed.
fn finish(entry: &mut Entry, result: Result<(), Problem>) {
    if entry.state.status.is_finished() {
        return;
    }
    entry.state.status = match (&result, entry.cancelled) {
        (_, true) => Status::Cancelled,
        (Err(problem), false) if problem.is_cancelled() => Status::Cancelled,
        (Ok(()), false) => Status::Done,
        (Err(_), false) => Status::Failed,
    };
    if let (Err(problem), Status::Failed) = (result, entry.state.status) {
        problem::log(&problem, "jobs", Some(entry.state.id), None);
        entry.write_log(Level::Error, problem.text.clone());
        entry.state.last_error = Some(problem);
    }
    entry.state.current = None;
    entry.touch(true);
}
