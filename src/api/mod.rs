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

pub use self::core::*;
pub use self::demo::*;
pub use self::jobs::*;
pub use self::library::*;
#[cfg(feature = "playlists")]
pub use self::playlists::*;
#[cfg(feature = "ytdlp")]
pub use self::ytdlp::*;

/// Every error the apps can get. The apps receive the variant and its text (Display).
#[derive(Debug, uniffi::Error)]
#[uniffi(flat_error)]
pub enum ApiError {
    /// Not set up yet, or the setup is broken (for example Python is missing).
    NotReady { message: String },
    /// The arguments cannot be used.
    BadArgs { message: String },
    /// Stopped by a CancelToken.
    Cancelled,
    /// yt-dlp failed; `kind` is the yt-dlp error type, such as "DownloadError".
    YtDlp { kind: String, message: String },
    /// Anything else, such as a database or file error.
    Failed { message: String },
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ApiError::NotReady { message } => write!(f, "not ready: {message}"),
            ApiError::BadArgs { message } => write!(f, "bad arguments: {message}"),
            ApiError::Cancelled => f.write_str("cancelled"),
            ApiError::YtDlp { message, .. } => f.write_str(message),
            ApiError::Failed { message } => f.write_str(message),
        }
    }
}

impl std::error::Error for ApiError {}

fn failed(err: impl fmt::Display) -> ApiError {
    ApiError::Failed {
        message: err.to_string(),
    }
}

fn bad_args(message: impl Into<String>) -> ApiError {
    ApiError::BadArgs {
        message: message.into(),
    }
}

/// How serious a log line is (jobs and yt-dlp events).
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum LogLevel {
    Debug,
    Info,
    Warning,
    Error,
}
