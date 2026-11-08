//! A job that needs nothing (no network, no Python): checks the whole path from the core
//! to the screen, in CI and on a phone. Plugs out with its `mod` line and the register call
//! in core.rs.

use std::thread;
use std::time::Duration;

use super::{ApiError, bad_args, core};
use crate::jobs::{Job, Jobs, Level};
use crate::problem::Problem;

const KIND: &str = "demo";

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct DemoOptions {
    /// How many fake items (at most 1000).
    pub items: u32,
    /// How long each item takes, in milliseconds (at most 60 000).
    pub item_ms: u32,
    /// This item (1-based) fails; the job goes on.
    pub fail_at: Option<u32>,
    /// This item logs a warning.
    pub warn_at: Option<u32>,
}

/// Queues a demo job and returns its id.
#[uniffi::export]
pub fn start_demo_job(options: DemoOptions) -> Result<i64, ApiError> {
    if options.items > 1000 || options.item_ms > 60_000 {
        return Err(bad_args(
            "a demo job has at most 1000 items of at most 60 s",
        ));
    }
    let id = core()?
        .jobs
        .start(KIND, "Demo job", &params(&options))
        .map_err(bad_args)?;
    Ok(id as i64)
}

pub(super) fn register(jobs: &Jobs) {
    jobs.register(KIND, run);
}

fn params(options: &DemoOptions) -> String {
    let optional = |value: Option<u32>| value.map_or(String::new(), |value| value.to_string());
    format!(
        "{} {} {} {}",
        options.items,
        options.item_ms,
        optional(options.fail_at),
        optional(options.warn_at)
    )
}

fn parse(params: &str) -> Option<DemoOptions> {
    let mut parts = params.split(' ');
    let mut next = || parts.next();
    let items = next()?.parse().ok()?;
    let item_ms = next()?.parse().ok()?;
    let fail_at = next()?.parse().ok();
    let warn_at = next()?.parse().ok();
    Some(DemoOptions {
        items,
        item_ms,
        fail_at,
        warn_at,
    })
}

/// Ten progress steps per item, then done (or failed); stops between steps when cancelled.
fn run(job: &Job, params: &str) -> Result<(), Problem> {
    let options =
        parse(params).ok_or_else(|| Problem::bug(format!("bad demo params {params:?}")))?;
    job.set_total(options.items as usize);
    const STEPS: u64 = 10;
    const SIZE: u64 = 1_000_000;
    let step = Duration::from_millis(u64::from(options.item_ms) / STEPS);
    for position in 1..=options.items {
        job.item_started(position as usize, &format!("Demo item {position}"));
        if Some(position) == options.warn_at {
            job.log(Level::Warning, &format!("Demo item {position} is slow"));
        }
        for done in 1..=STEPS {
            if job.is_cancelled() {
                return Ok(());
            }
            thread::sleep(step);
            let bytes = SIZE * done / STEPS;
            job.progress(bytes, Some(SIZE), Some(SIZE as f64), Some(STEPS - done));
        }
        if Some(position) == options.fail_at {
            job.item_failed(&Problem::new(
                crate::problem::Kind::Unavailable,
                format!("Demo item {position} failed on purpose"),
                "demo",
            ));
        } else {
            job.item_done();
        }
    }
    job.log(Level::Info, "Demo finished.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn params_round_trip() {
        for options in [
            DemoOptions {
                items: 3,
                item_ms: 50,
                fail_at: Some(2),
                warn_at: None,
            },
            DemoOptions {
                items: 0,
                item_ms: 0,
                fail_at: None,
                warn_at: Some(1),
            },
        ] {
            assert_eq!(parse(&params(&options)), Some(options));
        }
        assert_eq!(parse("x"), None);
        assert_eq!(parse(""), None);
    }
}
