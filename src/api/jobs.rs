//! Background jobs (see src/jobs): their states for the screen, their logs, and the
//! buttons (cancel, resume, discard, forget).

use super::{ApiError, LogLevel, bad_args, core};
use crate::jobs::{self, Status};

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum JobStatus {
    /// Waiting for a free slot.
    Queued,
    Running,
    /// Was queued or running when the app closed: offer Resume or Discard.
    Interrupted,
    Done,
    Failed,
    Cancelled,
}

/// Everything the screen shows about a job. Replace the shown state with a newer one
/// (higher `revision`) of the same job; ignore older ones.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct JobState {
    pub id: i64,
    pub revision: i64,
    /// What kind of work it is: "playlist", "demo", ...
    pub kind: String,
    pub title: String,
    pub status: JobStatus,
    pub done: i32,
    pub failed: i32,
    pub total: Option<i32>,
    pub current: Option<JobItem>,
    pub last_error: Option<String>,
    /// How many log lines exist; fetch new ones with job_logs.
    pub log_lines: i64,
}

/// The item being worked on.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct JobItem {
    pub position: i32,
    pub title: String,
    pub bytes: i64,
    pub total_bytes: Option<i64>,
    /// Bytes per second.
    pub speed: Option<f64>,
    /// Seconds left.
    pub eta: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct JobLogLine {
    pub number: i64,
    pub level: LogLevel,
    pub message: String,
    /// Seconds since 1970.
    pub at: i64,
}

/// Gets every job's state right away, then every change. Called on the core's delivery
/// thread, one state at a time and in order: hand the state to the UI thread and return.
#[uniffi::export(callback_interface)]
pub trait JobListener: Send + Sync {
    fn on_job(&self, job: JobState);
}

/// Sets the one listener (replacing any earlier one).
#[uniffi::export]
pub fn set_job_listener(listener: Box<dyn JobListener>) -> Result<(), ApiError> {
    core()?
        .jobs
        .set_listener(move |state| listener.on_job(state.into()));
    Ok(())
}

/// Every job, oldest first.
#[uniffi::export]
pub fn jobs() -> Result<Vec<JobState>, ApiError> {
    Ok(core()?
        .jobs
        .list()
        .into_iter()
        .map(JobState::from)
        .collect())
}

#[uniffi::export]
pub fn job(id: i64) -> Result<Option<JobState>, ApiError> {
    Ok(job_id(id)
        .and_then(|id| core().ok()?.jobs.get(id))
        .map(JobState::from))
}

/// A job's log lines numbered above `since` (0 for all).
#[uniffi::export]
pub fn job_logs(id: i64, since: i64) -> Result<Vec<JobLogLine>, ApiError> {
    let jobs = &core()?.jobs;
    let Some(id) = job_id(id) else {
        return Ok(Vec::new());
    };
    let since = usize::try_from(since).unwrap_or(0);
    Ok(jobs
        .logs(id, since)
        .into_iter()
        .map(JobLogLine::from)
        .collect())
}

/// Stops a queued, running or interrupted job. False if it is unknown or finished.
#[uniffi::export]
pub fn cancel_job(id: i64) -> Result<bool, ApiError> {
    let jobs = &core()?.jobs;
    Ok(job_id(id).is_some_and(|id| jobs.cancel(id)))
}

/// Runs an Interrupted or Failed job again from the start: the Resume and Retry buttons.
#[uniffi::export]
pub fn resume_job(id: i64) -> Result<(), ApiError> {
    let jobs = &core()?.jobs;
    let id = job_id(id).ok_or_else(|| bad_args("no such job"))?;
    jobs.resume(id).map_err(bad_args)
}

/// Gives up an Interrupted job: it ends Cancelled. False if it is not interrupted.
#[uniffi::export]
pub fn discard_job(id: i64) -> Result<bool, ApiError> {
    let jobs = &core()?.jobs;
    Ok(job_id(id).is_some_and(|id| jobs.discard(id)))
}

/// Removes a finished job and its log. False if it is unknown or not finished.
#[uniffi::export]
pub fn forget_job(id: i64) -> Result<bool, ApiError> {
    let jobs = &core()?.jobs;
    Ok(job_id(id).is_some_and(|id| jobs.forget(id)))
}

fn job_id(id: i64) -> Option<jobs::JobId> {
    u64::try_from(id).ok()
}

/// Counts and sizes in the core are unsigned; the apps get signed numbers (Kotlin's
/// unsigned types are awkward). Real values never come near the limits.
fn signed(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn count(value: usize) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}

impl From<jobs::JobState> for JobState {
    fn from(state: jobs::JobState) -> Self {
        JobState {
            id: signed(state.id),
            revision: signed(state.revision),
            kind: state.kind,
            title: state.title,
            status: match state.status {
                Status::Queued => JobStatus::Queued,
                Status::Running => JobStatus::Running,
                Status::Interrupted => JobStatus::Interrupted,
                Status::Done => JobStatus::Done,
                Status::Failed => JobStatus::Failed,
                Status::Cancelled => JobStatus::Cancelled,
            },
            done: count(state.done),
            failed: count(state.failed),
            total: state.total.map(count),
            current: state.current.map(|item| JobItem {
                position: count(item.position),
                title: item.title,
                bytes: signed(item.bytes),
                total_bytes: item.total_bytes.map(signed),
                speed: item.speed,
                eta: item.eta.map(signed),
            }),
            last_error: state.last_error,
            log_lines: signed(state.log_lines as u64),
        }
    }
}

impl From<jobs::LogLine> for JobLogLine {
    fn from(line: jobs::LogLine) -> Self {
        JobLogLine {
            number: signed(line.number as u64),
            level: match line.level {
                jobs::Level::Info => LogLevel::Info,
                jobs::Level::Warning => LogLevel::Warning,
                jobs::Level::Error => LogLevel::Error,
            },
            message: line.message,
            at: signed(line.at),
        }
    }
}
