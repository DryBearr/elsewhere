use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use super::*;

/// Every state the listener got, in order.
fn recording(jobs: &Jobs) -> Arc<Mutex<Vec<JobState>>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    jobs.set_listener(move |state| sink.lock().unwrap().push(state));
    seen
}

/// Waits until the job is finished and the listener has seen its last revision.
fn wait_finished(jobs: &Jobs, seen: &Mutex<Vec<JobState>>, id: JobId) -> JobState {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let state = jobs.get(id).unwrap();
        let delivered = seen
            .lock()
            .unwrap()
            .iter()
            .any(|seen| seen.id == id && seen.revision == state.revision);
        if state.status.is_finished() && delivered {
            return state;
        }
        assert!(
            Instant::now() < deadline,
            "job {id} did not finish: {state:?}"
        );
        thread::sleep(Duration::from_millis(5));
    }
}

fn states_of(seen: &Mutex<Vec<JobState>>, id: JobId) -> Vec<JobState> {
    seen.lock()
        .unwrap()
        .iter()
        .filter(|state| state.id == id)
        .cloned()
        .collect()
}

#[test]
fn states_arrive_in_order_and_the_job_ends_once() {
    let jobs = Jobs::new(2);
    let seen = recording(&jobs);
    let id = jobs.start("test", "Three items", |job| {
        job.set_total(3);
        for position in 1..=3 {
            job.item_started(position, &format!("item {position}"));
            job.progress(50, Some(100), Some(10.0), Some(5));
            job.item_done();
        }
        Ok(())
    });
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
    let jobs = Jobs::new(1);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    jobs.set_listener(move |state| {
        thread::sleep(Duration::from_millis(30));
        sink.lock().unwrap().push(state);
    });
    let started = Instant::now();
    let id = jobs.start("test", "Fast", |job| {
        job.set_total(1);
        job.item_started(1, "big file");
        for bytes in 0..100_000u64 {
            job.progress(bytes, Some(100_000), None, None);
        }
        job.item_done();
        Ok(())
    });
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
    let jobs = Jobs::new(1);
    let seen = recording(&jobs);
    let id = jobs.start("test", "Noisy", |job| {
        for n in 0..500 {
            let level = [Level::Info, Level::Warning, Level::Error][n % 3];
            job.log(level, &format!("line {n}"));
        }
        job.item_failed("one item broke");
        Ok(())
    });
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
    let jobs = Jobs::new(2);
    let seen = recording(&jobs);
    let failing = jobs.start("test", "Fails", |_| Err("no network".to_owned()));
    let panicking = jobs.start("test", "Panics", |_| panic!("bug in the work"));
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
    let next = jobs.start("test", "After", |_| Ok(()));
    assert_eq!(wait_finished(&jobs, &seen, next).status, Status::Done);
}

#[test]
fn cancelling_a_running_job_stops_it() {
    let jobs = Jobs::new(1);
    let seen = recording(&jobs);
    let stops = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&stops);
    let id = jobs.start("test", "Endless", move |job| {
        let counted = Arc::clone(&counted);
        job.on_cancel(move || {
            counted.fetch_add(1, Ordering::SeqCst);
        });
        while !job.is_cancelled() {
            thread::sleep(Duration::from_millis(1));
        }
        Ok(())
    });
    while jobs.get(id).unwrap().status != Status::Running {
        thread::sleep(Duration::from_millis(1));
    }
    assert!(jobs.cancel(id));
    let last = wait_finished(&jobs, &seen, id);
    assert_eq!(last.status, Status::Cancelled);
    assert_eq!(stops.load(Ordering::SeqCst), 1);
    assert!(!jobs.cancel(id), "a finished job cannot be cancelled again");
}

#[test]
fn cancelling_a_queued_job_never_runs_it() {
    let jobs = Jobs::new(1);
    let seen = recording(&jobs);
    let release = Arc::new(Mutex::new(()));
    let held = release.lock().unwrap();
    let gate = Arc::clone(&release);
    let first = jobs.start("test", "Holds the slot", move |_| {
        drop(gate.lock().unwrap());
        Ok(())
    });
    let ran = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&ran);
    let second = jobs.start("test", "Waits", move |_| {
        counted.fetch_add(1, Ordering::SeqCst);
        Ok(())
    });
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
    let jobs = Jobs::new(2);
    let seen = recording(&jobs);
    let now = Arc::new(AtomicUsize::new(0));
    let most = Arc::new(AtomicUsize::new(0));
    let ids: Vec<JobId> = (0..6)
        .map(|n| {
            let (now, most) = (Arc::clone(&now), Arc::clone(&most));
            jobs.start("test", &format!("job {n}"), move |_| {
                let running = now.fetch_add(1, Ordering::SeqCst) + 1;
                most.fetch_max(running, Ordering::SeqCst);
                thread::sleep(Duration::from_millis(20));
                now.fetch_sub(1, Ordering::SeqCst);
                Ok(())
            })
        })
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
    let jobs = Jobs::new(1);
    let early = recording(&jobs);
    let id = jobs.start("test", "Before the listener", |_| Ok(()));
    wait_finished(&jobs, &early, id);
    let late = recording(&jobs);
    let last = wait_finished(&jobs, &late, id);
    assert_eq!(last.status, Status::Done);
}

#[test]
fn a_panicking_listener_does_not_stop_delivery() {
    let jobs = Jobs::new(1);
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
    let id = jobs.start("test", "Survives", |job| {
        job.log(Level::Info, "hello");
        Ok(())
    });
    assert_eq!(wait_finished(&jobs, &seen, id).status, Status::Done);
}

#[test]
fn many_jobs_and_updates_at_once() {
    let jobs = Jobs::new(4);
    let seen = recording(&jobs);
    let ids: Vec<JobId> = (0..40)
        .map(|n| {
            jobs.start("test", &format!("job {n}"), move |job| {
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
            })
        })
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
    let jobs = Jobs::new(1);
    let seen = recording(&jobs);
    let id = jobs.start("test", "Done soon", |_| Ok(()));
    wait_finished(&jobs, &seen, id);
    assert!(jobs.forget(id));
    assert!(jobs.get(id).is_none());
    assert!(!jobs.forget(id));
}
