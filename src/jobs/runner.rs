//! The machinery behind `jobs`: one table of jobs behind a lock, a thread per running job,
//! and one delivery thread that hands changed states to the listener.

use std::collections::{BTreeMap, VecDeque};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use super::{Job, JobId, JobState, Level, LogLine, MAX_LOG_LINES, PROGRESS_INTERVAL, Status, Work};

type Listener = Arc<dyn Fn(JobState) + Send + Sync>;
type Stop = Arc<dyn Fn() + Send + Sync>;

pub(super) struct Inner {
    table: Mutex<Table>,
    /// Wakes the delivery thread when something changed (or on shutdown).
    changed: Condvar,
    listener: Mutex<Option<Listener>>,
    max_running: usize,
}

#[derive(Default)]
struct Table {
    jobs: BTreeMap<JobId, Entry>,
    queue: VecDeque<(JobId, Work)>,
    running: usize,
    next_id: JobId,
    shut_down: bool,
}

pub(super) struct Entry {
    pub(super) state: JobState,
    log: VecDeque<LogLine>,
    cancelled: bool,
    stops: Vec<Stop>,
    /// What the listener still has to get: nothing, a throttled progress change, or a
    /// change to send right away.
    pending: Pending,
    last_sent: Option<Instant>,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Pending {
    Nothing,
    Throttled,
    Now,
}

impl Entry {
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
        if self.log.len() > MAX_LOG_LINES {
            self.log.pop_front();
        }
    }
}

impl Inner {
    pub(super) fn start(max_running: usize) -> Arc<Inner> {
        let inner = Arc::new(Inner {
            table: Mutex::new(Table {
                next_id: 1,
                ..Table::default()
            }),
            changed: Condvar::new(),
            listener: Mutex::new(None),
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

    pub(super) fn queue(self: &Arc<Self>, kind: &str, title: &str, work: Work) -> JobId {
        let mut table = self.lock();
        let id = table.next_id;
        table.next_id += 1;
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
        table.jobs.insert(
            id,
            Entry {
                state,
                log: VecDeque::new(),
                cancelled: false,
                stops: Vec::new(),
                pending: Pending::Now,
                last_sent: None,
            },
        );
        table.queue.push_back((id, work));
        self.run_queued(&mut table);
        self.changed.notify_all();
        id
    }

    /// Starts queued jobs while there are free slots.
    fn run_queued(self: &Arc<Self>, table: &mut Table) {
        while table.running < self.max_running && !table.shut_down {
            let Some((id, work)) = table.queue.pop_front() else {
                return;
            };
            table.running += 1;
            if let Some(entry) = table.jobs.get_mut(&id) {
                entry.state.status = Status::Running;
                entry.state.revision += 1;
                entry.pending = Pending::Now;
            }
            let inner = Arc::clone(self);
            let spawned = thread::Builder::new()
                .name(format!("job-{id}"))
                .spawn(move || inner.run(id, work));
            if let Err(err) = spawned {
                table.running -= 1;
                if let Some(entry) = table.jobs.get_mut(&id) {
                    finish(entry, Err(format!("could not start a thread: {err}")));
                }
            }
        }
    }

    /// The body of a job's thread.
    fn run(self: Arc<Self>, id: JobId, work: Work) {
        let job = Job {
            id,
            inner: Arc::clone(&self),
        };
        let result = catch_unwind(AssertUnwindSafe(|| work(&job))).unwrap_or_else(|panic| {
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

    /// Applies `edit` to a job, bumps its revision and wakes the delivery thread. `now` is
    /// false only for byte progress, which is throttled. Finished jobs do not change.
    pub(super) fn change(&self, id: JobId, now: bool, edit: impl FnOnce(&mut Entry)) {
        let mut table = self.lock();
        let Some(entry) = table.jobs.get_mut(&id) else {
            return;
        };
        if entry.state.status.is_finished() {
            return;
        }
        edit(entry);
        entry.state.revision += 1;
        let pending = if now {
            Pending::Now
        } else {
            Pending::Throttled
        };
        entry.pending = entry.pending.max(pending);
        self.changed.notify_all();
    }

    pub(super) fn cancel(&self, id: JobId) -> bool {
        let stops = {
            let mut table = self.lock();
            let queued = table.queue.iter().position(|(queued, _)| *queued == id);
            let Some(entry) = table.jobs.get_mut(&id) else {
                return false;
            };
            if entry.state.status.is_finished() {
                return false;
            }
            entry.cancelled = true;
            if queued.is_some() {
                // Never started: it ends now, and its work is dropped unrun.
                finish(entry, Ok(()));
            }
            let stops = entry.stops.clone();
            if let Some(position) = queued {
                table.queue.remove(position);
            }
            self.changed.notify_all();
            stops
        };
        // Outside the lock: a stop function may call back into the job.
        for stop in stops {
            stop();
        }
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
            Some(entry) if entry.state.status.is_finished() => table.jobs.remove(&id).is_some(),
            _ => false,
        }
    }

    pub(super) fn shut_down(&self) {
        self.lock().shut_down = true;
        self.changed.notify_all();
    }

    /// The delivery thread: waits for changes, then hands each changed job's latest state
    /// to the listener, outside the lock and in job order.
    fn deliver(self: Arc<Self>) {
        let mut table = self.lock();
        loop {
            if table.shut_down {
                return;
            }
            let now = Instant::now();
            let mut ready = Vec::new();
            let mut next_wake: Option<Instant> = None;
            for entry in table.jobs.values_mut() {
                let due = match entry.pending {
                    Pending::Nothing => continue,
                    Pending::Now => now,
                    Pending::Throttled => {
                        entry.last_sent.map_or(now, |sent| sent + PROGRESS_INTERVAL)
                    }
                };
                if due <= now {
                    ready.push(entry.state.clone());
                    entry.pending = Pending::Nothing;
                    entry.last_sent = Some(now);
                } else {
                    next_wake = Some(next_wake.map_or(due, |wake| wake.min(due)));
                }
            }

            if ready.is_empty() {
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
            let listener = self
                .listener
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone();
            let mut failed = Vec::new();
            if let Some(listener) = listener {
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
    entry.state.revision += 1;
    entry.pending = Pending::Now;
}
