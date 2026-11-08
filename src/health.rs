//! The core heals itself: long-lived parts run under `supervise`, which restarts them when
//! they crash (after 1 s, 5 s, 30 s) and gives up on a part that keeps crashing (5 restarts
//! within 10 minutes): it is then Broken, and only restarting the app helps. That is safe
//! because everything that matters is in the database; the app's start is the recovery.
//!
//! `parts()` says how each part is doing; `needs_restart()` is true once one is Broken.

use std::collections::{BTreeMap, VecDeque};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use crate::logs;

const RESTART_BUDGET: usize = 5;
const BUDGET_WINDOW: Duration = Duration::from_secs(600);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    Healthy,
    /// Failed and is being restarted.
    Recovering(String),
    /// Gave up: only an app restart helps.
    Broken(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Part {
    pub name: String,
    pub state: State,
    pub restarts: usize,
}

struct Record {
    state: State,
    restarts: usize,
    recent: VecDeque<Instant>,
}

static PARTS: Mutex<BTreeMap<String, Record>> = Mutex::new(BTreeMap::new());

fn parts_lock() -> MutexGuard<'static, BTreeMap<String, Record>> {
    PARTS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn record<'a>(parts: &'a mut BTreeMap<String, Record>, name: &str) -> &'a mut Record {
    parts.entry(name.to_owned()).or_insert(Record {
        state: State::Healthy,
        restarts: 0,
        recent: VecDeque::new(),
    })
}

/// Every part the core knows, by name.
pub fn parts() -> Vec<Part> {
    parts_lock()
        .iter()
        .map(|(name, record)| Part {
            name: name.clone(),
            state: record.state.clone(),
            restarts: record.restarts,
        })
        .collect()
}

pub fn state(name: &str) -> State {
    parts_lock()
        .get(name)
        .map_or(State::Healthy, |record| record.state.clone())
}

/// True once a part is Broken: the app should restart (Android does so by itself).
pub fn needs_restart() -> bool {
    parts_lock()
        .values()
        .any(|record| matches!(record.state, State::Broken(_)))
}

pub fn healthy(name: &str) {
    let mut parts = parts_lock();
    let record = record(&mut parts, name);
    if !matches!(record.state, State::Broken(_)) {
        record.state = State::Healthy;
    }
}

pub fn broken(name: &str, reason: impl Into<String>) {
    let reason = reason.into();
    logs::error(
        "core",
        format!("{name} stopped working; the app needs a restart"),
    )
    .detail(reason.clone())
    .write();
    record(&mut parts_lock(), name).state = State::Broken(reason);
}

/// Counts a failure of `name`. Returns the pause before the restart, or None when the
/// part used up its restart budget (it is then Broken).
pub fn failed(name: &str, reason: &str) -> Option<Duration> {
    let mut parts = parts_lock();
    let record = record(&mut parts, name);
    if matches!(record.state, State::Broken(_)) {
        return None;
    }
    let now = Instant::now();
    while record
        .recent
        .front()
        .is_some_and(|at| now.duration_since(*at) > BUDGET_WINDOW)
    {
        record.recent.pop_front();
    }
    if record.recent.len() >= RESTART_BUDGET {
        drop(parts);
        broken(
            name,
            format!("failed {RESTART_BUDGET} times within 10 minutes; last: {reason}"),
        );
        return None;
    }
    record.recent.push_back(now);
    record.restarts += 1;
    record.state = State::Recovering(reason.to_owned());
    let pause = restart_pauses()[(record.recent.len() - 1).min(2)];
    drop(parts);
    logs::warn("core", format!("{name} failed; restarting it"))
        .detail(reason.to_owned())
        .write();
    Some(pause)
}

static RESTART_PAUSES: Mutex<Option<[Duration; 3]>> = Mutex::new(None);

/// The pauses before the first, second and later restarts (1 s, 5 s, 30 s).
fn restart_pauses() -> [Duration; 3] {
    RESTART_PAUSES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .unwrap_or([1, 5, 30].map(Duration::from_secs))
}

/// Replaces the pauses before restarts. For tests.
pub fn set_restart_pauses(pauses: [Duration; 3]) {
    *RESTART_PAUSES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(pauses);
}

/// Runs `body` on its own thread named `name`, restarting it whenever it panics, until it
/// returns normally (its way of saying "stop") or `name` runs out of restarts.
pub fn supervise(name: impl Into<String>, body: impl Fn() + Send + 'static) -> std::io::Result<()> {
    let name = name.into();
    thread::Builder::new().name(name.clone()).spawn(move || {
        let name = name.as_str();
        healthy(name);
        loop {
            match catch_unwind(AssertUnwindSafe(&body)) {
                Ok(()) => return,
                Err(panic) => {
                    let reason = panic
                        .downcast_ref::<&str>()
                        .map(|text| text.to_string())
                        .or_else(|| panic.downcast_ref::<String>().cloned())
                        .unwrap_or_else(|| "unknown panic".to_owned());
                    let Some(pause) = failed(name, &reason) else {
                        return;
                    };
                    thread::sleep(pause);
                    healthy(name);
                }
            }
        }
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn wait_until(check: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !check() {
            assert!(Instant::now() < deadline, "timed out");
            thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn a_crashing_part_is_restarted_then_given_up() {
        set_restart_pauses([Duration::from_millis(1); 3]);
        let runs = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&runs);
        supervise("test-crashing", move || {
            counted.fetch_add(1, Ordering::SeqCst);
            panic!("always broken");
        })
        .unwrap();
        wait_until(|| matches!(state("test-crashing"), State::Broken(_)));
        assert_eq!(
            runs.load(Ordering::SeqCst),
            RESTART_BUDGET + 1,
            "the first run plus every restart"
        );
        assert!(needs_restart());
        let part = parts()
            .into_iter()
            .find(|part| part.name == "test-crashing")
            .unwrap();
        assert_eq!(part.restarts, RESTART_BUDGET);
    }

    #[test]
    fn a_part_that_crashes_once_heals() {
        set_restart_pauses([Duration::from_millis(1); 3]);
        let runs = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&runs);
        supervise("test-once", move || {
            if counted.fetch_add(1, Ordering::SeqCst) == 0 {
                panic!("first run fails");
            }
        })
        .unwrap();
        wait_until(|| runs.load(Ordering::SeqCst) == 2);
        wait_until(|| state("test-once") == State::Healthy);
    }
}
