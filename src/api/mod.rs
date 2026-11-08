//! Everything the Android and iOS apps can call. UniFFI generates the Kotlin and Swift
//! bindings from these files (scripts/android_rust.sh, scripts/ios_rust.sh), so names here
//! are the names the apps see: `start_demo_job` becomes `startDemoJob`.
//!
//! One file per module; a module plugs out with its feature flag and its `mod` line.
//! All functions block: the apps call them from a background thread.
//!
//! The core loads lazily. `start` is cheap (folders, database, jobs); Python and yt-dlp
//! start on the first call or job that needs them, and stay until the app ends (Python
//! cannot be stopped again). `core_status` says what is loaded.

mod core;
mod demo;
mod jobs;
mod library;
#[cfg(feature = "playlists")]
mod playlists;
#[cfg(feature = "ytdlp")]
mod ytdlp;

use std::fmt;

use crate::problem;

pub use self::core::*;
pub use self::demo::*;
pub use self::jobs::*;
pub use self::library::*;
#[cfg(feature = "playlists")]
pub use self::playlists::*;
#[cfg(feature = "ytdlp")]
pub use self::ytdlp::*;

/// What kind of failure a problem is, so the app can pick what to show and offer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum ProblemKind {
    /// The request is wrong (bad link, bad option): the person fixes it.
    Input,
    /// The thing is not there for us: private, removed, blocked in this country.
    Unavailable,
    /// The connection failed: offline, timeout, server trouble, rate limits.
    Network,
    /// The phone's storage: full, read-only, no permission.
    Storage,
    /// The app's own setup is broken (for example Python is missing).
    Setup,
    /// Someone stopped it. Not an error to show.
    Cancelled,
    /// Something that should never happen: a bug in the core.
    Bug,
}

/// A failure, ready to show: `text` is for people, `detail` for the log, `retry` says
/// whether a "Try again" button makes sense.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct Problem {
    pub kind: ProblemKind,
    pub text: String,
    pub detail: String,
    pub retry: bool,
}

/// Every error the apps get: one problem (Kotlin `ApiException.Failed`, Swift
/// `ApiError.Failed`).
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Error)]
pub enum ApiError {
    Failed { problem: Problem },
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let ApiError::Failed { problem } = self;
        f.write_str(&problem.text)
    }
}

impl std::error::Error for ApiError {}

impl From<problem::Problem> for Problem {
    fn from(problem: problem::Problem) -> Self {
        Problem {
            kind: match problem.kind {
                problem::Kind::Input => ProblemKind::Input,
                problem::Kind::Unavailable => ProblemKind::Unavailable,
                problem::Kind::Network => ProblemKind::Network,
                problem::Kind::Storage => ProblemKind::Storage,
                problem::Kind::Setup => ProblemKind::Setup,
                problem::Kind::Cancelled => ProblemKind::Cancelled,
                problem::Kind::Bug => ProblemKind::Bug,
            },
            text: problem.text,
            detail: problem.detail,
            retry: problem.retry,
        }
    }
}

impl From<problem::Problem> for ApiError {
    fn from(problem: problem::Problem) -> Self {
        ApiError::Failed {
            problem: problem.into(),
        }
    }
}

/// Back to the core's problem, for job runners that call the API's own functions.
#[cfg_attr(not(feature = "playlists"), allow(dead_code))]
fn core_problem(err: ApiError) -> problem::Problem {
    let ApiError::Failed { problem } = err;
    problem::Problem {
        kind: match problem.kind {
            ProblemKind::Input => problem::Kind::Input,
            ProblemKind::Unavailable => problem::Kind::Unavailable,
            ProblemKind::Network => problem::Kind::Network,
            ProblemKind::Storage => problem::Kind::Storage,
            ProblemKind::Setup => problem::Kind::Setup,
            ProblemKind::Cancelled => problem::Kind::Cancelled,
            ProblemKind::Bug => problem::Kind::Bug,
        },
        text: problem.text,
        detail: problem.detail,
        retry: problem.retry,
    }
}

fn bad_args(message: impl Into<String>) -> ApiError {
    problem::Problem::input(message).into()
}

fn not_ready(detail: impl Into<String>) -> ApiError {
    problem::Problem::setup(detail).into()
}

/// How serious a log line is (jobs and yt-dlp events).
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum LogLevel {
    Debug,
    Info,
    Warning,
    Error,
}
