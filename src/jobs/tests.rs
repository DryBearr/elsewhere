use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use super::*;

/// Jobs in memory with one runner for the kind "test".
fn jobs_with(
    max_running: usize,
    runner: impl Fn(&Job, &str) -> Result<(), String> + Send + Sync + 'static,
) -> Jobs {
    let jobs = Jobs::new(max_running);
    jobs.register("test", runner);
    jobs
}

fn start(jobs: &Jobs, title: &str, params: &str) -> JobId {
    jobs.start("test", title, params).unwrap()
}

/// Every state the listener got, in order.
fn recording(jobs: &Jobs) -> Arc<Mutex<Vec<JobState>>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    jobs.set_listener(move |state| sink.lock().unwrap().push(state));
    seen
}

/// Waits until the job is in `wanted` and the listener has seen that revision.
fn wait_for(
    jobs: &Jobs,
    seen: &Mutex<Vec<JobState>>,
    id: JobId,
    wanted: fn(Status) -> bool,
) -> JobState {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let state = jobs.get(id).unwrap();
        let delivered = seen
            .lock()
            .unwrap()
            .iter()
            .any(|seen| seen.id == id && seen.revision == state.revision);
        if wanted(state.status) && delivered {
            return state;
        }
        assert!(
            Instant::now() < deadline,
            "job {id} did not get there: {state:?}"
        );
        thread::sleep(Duration::from_millis(5));
    }
}

fn wait_finished(jobs: &Jobs, seen: &Mutex<Vec<JobState>>, id: JobId) -> JobState {
    wait_for(jobs, seen, id, Status::is_finished)
}

fn states_of(seen: &Mutex<Vec<JobState>>, id: JobId) -> Vec<JobState> {
    seen.lock()
        .unwrap()
        .iter()
        .filter(|state| state.id == id)
        .cloned()
        .collect()
}

/// A fresh database file for one test.
fn database(name: &str) -> PathBuf {
    let path =
        std::env::temp_dir().join(format!("elsewhere-jobs-{name}-{}.db", std::process::id()));
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
    path
}

#[test]
fn states_arrive_in_order_and_the_job_ends_once() {
    let jobs = jobs_with(2, |job, _| {
        job.set_total(3);
        for position in 1..=3 {
            job.item_started(position, &format!("item {position}"));
            job.progress(50, Some(100), Some(10.0), Some(5));
            job.item_done();
        }
        Ok(())
    });
    let seen = recording(&jobs);
    let id = start(&jobs, "Three items", "");
    let last = wait_finished(&jobs, &seen, id);
    assert_eq!(
        (last.status, last.done, last.total),
        (Status::Done, 3, Some(3))
    );
    assert_eq!(last.current, None);

    // The listener always gets the latest state, so a fast job may skip straight to Done.
    // What must hold: revisions only go up, there is one final state, and it is the last.
    let states = states_of(&seen, id);
    assert!(
        states
            .windows(2)
            .all(|pair| pair[0].revision < pair[1].revision),
        "{states:?}"
    );
    let ends = states
        .iter()
        .filter(|state| state.status.is_finished())
        .count();
    assert_eq!(ends, 1, "exactly one final state: {states:?}");
    assert_eq!(states.last().unwrap(), &last);
}

#[test]
fn a_slow_listener_gets_the_exact_final_state_without_slowing_the_work() {
    let jobs = jobs_with(1, |job, _| {
        job.set_total(1);
        job.item_started(1, "big file");
        for bytes in 0..100_000u64 {
            job.progress(bytes, Some(100_000), None, None);
        }
        job.item_done();
        Ok(())
    });
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    jobs.set_listener(move |state| {
        thread::sleep(Duration::from_millis(30));
        sink.lock().unwrap().push(state);
    });
    let started = Instant::now();
    let id = start(&jobs, "Fast", "");
    let work_time = loop {
        if jobs.get(id).unwrap().status.is_finished() {
            break started.elapsed();
        }
        thread::sleep(Duration::from_millis(1));
    };
    let last = wait_finished(&jobs, &seen, id);
    assert_eq!((last.status, last.done), (Status::Done, 1));
    // 100 000 progress calls, but only a handful of states reached the listener.
    let delivered = states_of(&seen, id).len();
    assert!(delivered < 50, "{delivered} states delivered");
    assert!(
        work_time < Duration::from_secs(2),
        "work took {work_time:?}"
    );
}

#[test]
fn log_lines_are_never_dropped() {
    let jobs = jobs_with(1, |job, _| {
        for n in 0..500 {
            let level = [Level::Info, Level::Warning, Level::Error][n % 3];
            job.log(level, &format!("line {n}"));
        }
        job.item_failed("one item broke");
        Ok(())
    });
    let seen = recording(&jobs);
    let id = start(&jobs, "Noisy", "");
    let last = wait_finished(&jobs, &seen, id);
    assert_eq!(last.log_lines, 501);
    assert_eq!(
        (last.failed, last.last_error.as_deref()),
        (1, Some("one item broke"))
    );
    let lines = jobs.logs(id, 0);
    assert_eq!(lines.len(), 501);
    assert!(
        lines
            .iter()
            .enumerate()
            .all(|(index, line)| line.number == index + 1)
    );
    assert_eq!(lines[499].message, "line 499");
    assert_eq!(lines[500].level, Level::Error);
    assert_eq!(jobs.logs(id, 499).len(), 2, "lines after number 499");
}

#[test]
fn failing_and_panicking_work_fails_the_job() {
    let jobs = jobs_with(2, |_, params| match params {
        "fail" => Err("no network".to_owned()),
        "panic" => panic!("bug in the work"),
        _ => Ok(()),
    });
    let seen = recording(&jobs);
    let failing = start(&jobs, "Fails", "fail");
    let panicking = start(&jobs, "Panics", "panic");
    for (id, message) in [(failing, "no network"), (panicking, "bug in the work")] {
        let last = wait_finished(&jobs, &seen, id);
        assert_eq!(last.status, Status::Failed);
        assert!(
            last.last_error.as_deref().unwrap().contains(message),
            "{last:?}"
        );
        assert_eq!(jobs.logs(id, 0).last().unwrap().level, Level::Error);
    }
    // The jobs keep working after a panic.
    let next = start(&jobs, "After", "ok");
    assert_eq!(wait_finished(&jobs, &seen, next).status, Status::Done);
}

#[test]
fn cancelling_a_running_job_stops_it() {
    let stops = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&stops);
    let jobs = jobs_with(1, move |job, _| {
        let counted = Arc::clone(&counted);
        job.on_cancel(move || {
            counted.fetch_add(1, Ordering::SeqCst);
        });
        while !job.is_cancelled() {
            thread::sleep(Duration::from_millis(1));
        }
        Ok(())
    });
    let seen = recording(&jobs);
    let id = start(&jobs, "Endless", "");
    wait_for(&jobs, &seen, id, |status| status == Status::Running);
    assert!(jobs.cancel(id));
    let last = wait_finished(&jobs, &seen, id);
    assert_eq!(last.status, Status::Cancelled);
    assert_eq!(stops.load(Ordering::SeqCst), 1);
    assert!(!jobs.cancel(id), "a finished job cannot be cancelled again");
}

#[test]
fn cancelling_a_queued_job_never_runs_it() {
    let release = Arc::new(Mutex::new(()));
    let held = release.lock().unwrap();
    let gate = Arc::clone(&release);
    let ran = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&ran);
    let jobs = jobs_with(1, move |_, params| {
        if params == "hold" {
            drop(gate.lock().unwrap());
        } else {
            counted.fetch_add(1, Ordering::SeqCst);
        }
        Ok(())
    });
    let seen = recording(&jobs);
    let first = start(&jobs, "Holds the slot", "hold");
    let second = start(&jobs, "Waits", "count");
    assert_eq!(jobs.get(second).unwrap().status, Status::Queued);
    assert!(jobs.cancel(second));
    assert_eq!(jobs.get(second).unwrap().status, Status::Cancelled);
    drop(held);
    wait_finished(&jobs, &seen, first);
    wait_finished(&jobs, &seen, second);
    assert_eq!(ran.load(Ordering::SeqCst), 0);
}

#[test]
fn at_most_max_running_jobs_run_at_once() {
    let now = Arc::new(AtomicUsize::new(0));
    let most = Arc::new(AtomicUsize::new(0));
    let (running, highest) = (Arc::clone(&now), Arc::clone(&most));
    let jobs = jobs_with(2, move |_, _| {
        let count = running.fetch_add(1, Ordering::SeqCst) + 1;
        highest.fetch_max(count, Ordering::SeqCst);
        thread::sleep(Duration::from_millis(20));
        running.fetch_sub(1, Ordering::SeqCst);
        Ok(())
    });
    let seen = recording(&jobs);
    let ids: Vec<JobId> = (0..6)
        .map(|n| start(&jobs, &format!("job {n}"), ""))
        .collect();
    for id in &ids {
        assert_eq!(wait_finished(&jobs, &seen, *id).status, Status::Done);
    }
    assert_eq!(most.load(Ordering::SeqCst), 2);
    let order: Vec<JobId> = jobs.list().iter().map(|state| state.id).collect();
    assert_eq!(order, ids, "oldest first");
}

#[test]
fn a_new_listener_gets_every_current_state() {
    let jobs = jobs_with(1, |_, _| Ok(()));
    let early = recording(&jobs);
    let id = start(&jobs, "Before the listener", "");
    wait_finished(&jobs, &early, id);
    let late = recording(&jobs);
    assert_eq!(wait_finished(&jobs, &late, id).status, Status::Done);
}

#[test]
fn a_panicking_listener_does_not_stop_delivery() {
    let jobs = jobs_with(1, |job, _| {
        job.log(Level::Info, "hello");
        Ok(())
    });
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&calls);
    jobs.set_listener(move |state| {
        if counted.fetch_add(1, Ordering::SeqCst) == 0 {
            panic!("listener bug");
        }
        sink.lock().unwrap().push(state);
    });
    // Even if the panic hits the final state, it is delivered again.
    let id = start(&jobs, "Survives", "");
    assert_eq!(wait_finished(&jobs, &seen, id).status, Status::Done);
}

#[test]
fn many_jobs_and_updates_at_once() {
    let jobs = jobs_with(4, |job, _| {
        job.set_total(20);
        for position in 1..=20 {
            job.item_started(position, "item");
            for bytes in 0..200 {
                job.progress(bytes, Some(200), None, None);
            }
            if position % 7 == 0 {
                job.item_failed("broken");
            } else {
                job.item_done();
            }
        }
        Ok(())
    });
    let seen = recording(&jobs);
    let ids: Vec<JobId> = (0..40)
        .map(|n| start(&jobs, &format!("job {n}"), ""))
        .collect();
    for id in &ids {
        let last = wait_finished(&jobs, &seen, *id);
        assert_eq!((last.status, last.done, last.failed), (Status::Done, 18, 2));
    }
    let mut last_revision: HashMap<JobId, u64> = HashMap::new();
    for state in seen.lock().unwrap().iter() {
        let previous = last_revision.insert(state.id, state.revision).unwrap_or(0);
        assert!(
            state.revision > previous,
            "job {} went back from {previous} to {}",
            state.id,
            state.revision
        );
    }
}

#[test]
fn forget_removes_only_finished_jobs() {
    let jobs = jobs_with(1, |_, _| Ok(()));
    let seen = recording(&jobs);
    let id = start(&jobs, "Done soon", "");
    wait_finished(&jobs, &seen, id);
    assert!(jobs.forget(id));
    assert!(jobs.get(id).is_none());
    assert!(!jobs.forget(id));
}

#[test]
fn unknown_kinds_and_wrong_states_are_refused() {
    let jobs = jobs_with(1, |_, _| Ok(()));
    let seen = recording(&jobs);
    assert!(jobs.start("nope", "x", "").is_err());
    let id = start(&jobs, "Done", "");
    wait_finished(&jobs, &seen, id);
    assert!(jobs.resume(id).is_err(), "a Done job cannot be resumed");
    assert!(!jobs.discard(id), "only Interrupted jobs can be discarded");
    assert!(jobs.resume(999).is_err());
}

// --- Saved in the database ------------------------------------------------------------

#[test]
fn finished_jobs_come_back_after_a_restart() {
    let db = database("history");
    let id = {
        let jobs = Jobs::open(&db, 1).unwrap();
        jobs.register("test", |job, _| {
            job.set_total(2);
            for position in 1..=2 {
                job.item_started(position, "item");
                job.log(Level::Warning, &format!("slow server for {position}"));
                job.item_done();
            }
            Ok(())
        });
        let seen = recording(&jobs);
        let id = start(&jobs, "Saved", "params");
        wait_finished(&jobs, &seen, id);
        id
    };
    let jobs = Jobs::open(&db, 1).unwrap();
    let state = jobs.get(id).expect("the job is still there");
    assert_eq!(
        (state.status, state.done, state.total, state.log_lines),
        (Status::Done, 2, Some(2), 2)
    );
    let lines = jobs.logs(id, 0);
    assert_eq!(lines.len(), 2);
    assert_eq!(
        (lines[1].number, lines[1].level, lines[1].message.as_str()),
        (2, Level::Warning, "slow server for 2")
    );
    // New jobs never reuse an old id.
    jobs.register("test", |_, _| Ok(()));
    assert!(start(&jobs, "Next", "") > id);
}

#[test]
fn unfinished_jobs_come_back_interrupted_and_can_be_resumed() {
    let db = database("interrupted");
    let release = Arc::new(AtomicBool::new(false));
    let (first, waiting) = {
        let jobs = Jobs::open(&db, 1).unwrap();
        let held = Arc::clone(&release);
        jobs.register("test", move |job, _| {
            job.set_total(2);
            job.item_started(1, "first");
            job.item_done();
            job.item_started(2, "second");
            // Stands for a download in progress when the app is closed.
            while !held.load(Ordering::SeqCst) {
                thread::sleep(Duration::from_millis(5));
            }
            Ok(())
        });
        let seen = recording(&jobs);
        let first = start(&jobs, "Cut short", "link-1");
        let waiting = start(&jobs, "Never started", "link-2");
        // Wait until the running job's progress reached the database.
        let deadline = Instant::now() + Duration::from_secs(10);
        while states_of(&seen, first)
            .last()
            .is_none_or(|state| state.done < 1 || state.current.is_none())
        {
            assert!(Instant::now() < deadline, "the first job did not get going");
            thread::sleep(Duration::from_millis(5));
        }
        (first, waiting)
        // The jobs are dropped here: the app closes while job 1 runs and job 2 waits.
    };

    let jobs = Jobs::open(&db, 1).unwrap();
    for id in [first, waiting] {
        let state = jobs.get(id).unwrap();
        assert_eq!(state.status, Status::Interrupted, "{state:?}");
        let last = jobs.logs(id, 0).pop().unwrap();
        assert_eq!(last.level, Level::Warning);
        assert!(last.message.starts_with("Interrupted"), "{last:?}");
    }
    assert_eq!(
        jobs.get(first).unwrap().done,
        1,
        "what was saved before the close"
    );

    // The app registers its runners again; the button calls resume.
    let params = Arc::new(Mutex::new(Vec::new()));
    let got = Arc::clone(&params);
    jobs.register("test", move |job, params| {
        got.lock().unwrap().push(params.to_owned());
        job.set_total(2);
        for position in 1..=2 {
            job.item_started(position, "item");
            job.item_done();
        }
        Ok(())
    });
    let seen = recording(&jobs);
    jobs.resume(first).unwrap();
    let last = wait_finished(&jobs, &seen, first);
    assert_eq!(
        (last.status, last.done),
        (Status::Done, 2),
        "counts start over: {last:?}"
    );
    assert_eq!(
        params.lock().unwrap().as_slice(),
        ["link-1"],
        "the same params as before"
    );
    assert!(
        jobs.logs(first, 0)
            .iter()
            .any(|line| line.message == "Resumed.")
    );
    release.store(true, Ordering::SeqCst);
}

#[test]
fn a_discarded_job_stays_cancelled() {
    let db = database("discard");
    let id = {
        let jobs = Jobs::open(&db, 1).unwrap();
        // No slot ever frees up: this job stays queued until the close.
        jobs.register("block", |_, _| {
            thread::sleep(Duration::from_secs(3));
            Ok(())
        });
        jobs.register("test", |_, _| Ok(()));
        jobs.start("block", "Holds the slot", "").unwrap();
        start(&jobs, "Queued", "")
    };
    {
        let jobs = Jobs::open(&db, 1).unwrap();
        let seen = recording(&jobs);
        assert_eq!(jobs.get(id).unwrap().status, Status::Interrupted);
        assert!(jobs.discard(id));
        let last = wait_finished(&jobs, &seen, id);
        assert_eq!(last.status, Status::Cancelled);
        assert_eq!(jobs.logs(id, 0).last().unwrap().message, "Discarded.");
    }
    let jobs = Jobs::open(&db, 1).unwrap();
    assert_eq!(jobs.get(id).unwrap().status, Status::Cancelled);
    assert!(jobs.resume(id).is_err(), "a cancelled job is not resumed");
}

#[test]
fn forgotten_jobs_are_gone_from_the_database() {
    let db = database("forget");
    let id = {
        let jobs = Jobs::open(&db, 1).unwrap();
        jobs.register("test", |_, _| Ok(()));
        let seen = recording(&jobs);
        let id = start(&jobs, "Temporary", "");
        wait_finished(&jobs, &seen, id);
        assert!(jobs.forget(id));
        id
    };
    let jobs = Jobs::open(&db, 1).unwrap();
    assert!(jobs.get(id).is_none());
}
