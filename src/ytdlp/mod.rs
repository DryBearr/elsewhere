//! yt-dlp as plain Rust functions, through an embedded Python interpreter.
//!
//! - Options are yt-dlp's own command-line flags, so everything in yt-dlp's README works:
//!   `-f`, `-P`, `-o`, `--ffmpeg-location`, `--js-runtimes`, `--cookies`, `--print`, ...
//! - Everything yt-dlp logs or prints arrives as an [`Event`]. Nothing is written to stdout
//!   or stderr, and yt-dlp never waits for typed input.
//! - Every failure is an [`Error`]. Python and yt-dlp problems never panic.
//!
//! ```no_run
//! use elsewhere::ytdlp::{self, Cancel, Setup};
//!
//! ytdlp::init(&Setup::default())?;
//! let url = "https://example.com/video";
//! let json = ytdlp::info(url, &[], |_| {}, &Cancel::new())?;
//! let done = ytdlp::download(url, &["-P", "/home/me/Videos"], |event| println!("{event:?}"), &Cancel::new())?;
//! println!("{json}\n{:?}", done.files);
//! # Ok::<(), ytdlp::Error>(())
//! ```
//!
//! Notes:
//! - [`info`] and [`download`] block until yt-dlp is done. Call them from a worker thread,
//!   never the UI thread. Several threads may call them at the same time.
//! - Python starts once per process. Call [`init`] early, before the app starts other
//!   threads, because it sets environment variables that Python reads when it starts.
//! - `on_event` runs on the calling thread without the Python lock held, so it may block
//!   or call back into this module.
//! - On Android and iOS, pass `--cache-dir <app folder>` or `--no-cache-dir`; yt-dlp's
//!   default cache folder is under `$HOME`.
//! - iOS apps cannot start processes, so anything that needs ffmpeg, a JavaScript runtime
//!   or `--exec` is unavailable there.

mod python;

use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Where Python and yt-dlp live.
#[derive(Debug, Clone, Default)]
pub struct Setup {
    /// Python's home folder (the one holding `lib/python3.x`). `None` uses the Python the
    /// app was linked against, which is right on desktop.
    pub python_home: Option<PathBuf>,
    /// Folders put first on Python's import path, in order: the standard library when
    /// `python_home` is set, then the folder holding the `yt_dlp` package. A folder with a
    /// newer yt-dlp goes first.
    pub python_paths: Vec<PathBuf>,
}

/// Result of [`download`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Done {
    /// Final files, after merging, converting and moving. Empty when nothing was written,
    /// for example with `--simulate` or `--print`.
    pub files: Vec<PathBuf>,
    /// yt-dlp's information about the URL, as JSON. For playlists the entries are left out,
    /// as in the command-line program. `None` when yt-dlp stopped early on purpose
    /// (`--max-downloads`, `--break-on-existing`, ...).
    pub json: Option<String>,
}

/// Something yt-dlp reported while working.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// A log message, as yt-dlp would show it on screen.
    Log { level: Level, message: String },
    /// A line yt-dlp wrote to stdout: `--print`, `-j`, `-F` and similar flags.
    Output(String),
    /// Download progress for one file.
    Progress {
        file: PathBuf,
        done: u64,
        /// Exact or estimated size. `None` when the server does not say.
        total: Option<u64>,
        /// Bytes per second.
        speed: Option<f64>,
        /// Seconds left.
        eta: Option<u64>,
        /// For streams that come in pieces (HLS, DASH): the current piece.
        fragment_index: Option<u64>,
        /// For streams that come in pieces: the number of pieces, when known.
        fragment_count: Option<u64>,
    },
    /// A file finished downloading. Post-processing may still follow.
    Downloaded { file: PathBuf },
    /// A post-processor (merge, convert, embed, ...) changed state.
    /// `status` is `started`, `processing` or `finished`.
    PostProcess { name: String, status: String },
}

/// Level of an [`Event::Log`] message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Debug,
    Info,
    Warning,
    Error,
}

/// Why a call failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// [`init`] was not called or failed: Python or yt-dlp is missing, or too old.
    NotReady(String),
    /// The flags cannot be used: unknown flag, wrong value, a command-line-only flag,
    /// the URL inside the flags, or no absolute output folder for [`download`].
    BadArgs(String),
    /// [`Cancel::cancel`] was called.
    Cancelled,
    /// yt-dlp failed. `kind` is yt-dlp's own error class, for example `ExtractorError`,
    /// `UnsupportedError`, `GeoRestrictedError` or `HTTPError`.
    YtDlp { kind: String, message: String },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NotReady(reason) => write!(f, "yt-dlp is not ready: {reason}"),
            Error::BadArgs(reason) => write!(f, "bad yt-dlp flags: {reason}"),
            Error::Cancelled => write!(f, "cancelled"),
            Error::YtDlp { kind, message } => write!(f, "{message} ({kind})"),
        }
    }
}

impl std::error::Error for Error {}

/// Stops a running [`info`] or [`download`]. Clones share the same flag.
///
/// yt-dlp checks it at every log message, progress update and post-processing step,
/// so it stops at the next step, not mid-step. Half-downloaded `.part` files are kept,
/// and yt-dlp resumes them the next time.
#[derive(Debug, Clone, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// Starts Python and loads yt-dlp. Returns yt-dlp's version.
///
/// Safe to call more than once and from several threads: after the first success, later
/// calls only return the version. After a failure, fix the setup and call it again.
pub fn init(setup: &Setup) -> Result<String, Error> {
    python::init(setup)
}

/// yt-dlp's version, for example `2026.10.26`.
pub fn version() -> Result<String, Error> {
    python::version()
}

/// Extracts information about `url` without downloading. Returns yt-dlp's JSON, the same
/// as `yt-dlp -J`, with playlist entries included (pass `--flat-playlist` to keep them short).
pub fn info<F>(url: &str, args: &[&str], on_event: F, cancel: &Cancel) -> Result<String, Error>
where
    F: Fn(Event) + Send + Sync + 'static,
{
    let (_, json) = python::run("info", url, args, on_event, cancel)?;
    Ok(json.unwrap_or_else(|| "null".to_owned()))
}

/// Downloads `url`. `args` must include an absolute output folder, `-P <folder>`, or an
/// absolute `-o` template.
pub fn download<F>(url: &str, args: &[&str], on_event: F, cancel: &Cancel) -> Result<Done, Error>
where
    F: Fn(Event) + Send + Sync + 'static,
{
    let (files, json) = python::run("download", url, args, on_event, cancel)?;
    Ok(Done { files, json })
}

#[cfg(test)]
mod tests {
    use super::*;

    // These run in their own test binary, where nothing calls init() successfully.

    #[test]
    fn calls_before_init_are_not_ready() {
        assert!(matches!(version(), Err(Error::NotReady(_))));
        assert!(matches!(
            info("https://example.com", &[], |_| {}, &Cancel::new()),
            Err(Error::NotReady(_))
        ));
    }

    #[test]
    fn cancelled_before_start_needs_no_python() {
        let cancel = Cancel::new();
        cancel.cancel();
        assert_eq!(
            download("https://example.com", &[], |_| {}, &cancel),
            Err(Error::Cancelled)
        );
    }

    #[test]
    fn missing_python_home_is_not_ready_instead_of_aborting() {
        let setup = Setup {
            python_home: Some("/nonexistent/python".into()),
            python_paths: vec![],
        };
        assert!(
            matches!(init(&setup), Err(Error::NotReady(reason)) if reason.contains("not a folder"))
        );
    }

    #[test]
    fn python_home_without_standard_library_is_not_ready() {
        let home = std::env::temp_dir().join(format!("ytdlp-empty-home-{}", std::process::id()));
        std::fs::create_dir_all(&home).unwrap();
        let setup = Setup {
            python_home: Some(home.clone()),
            python_paths: vec![],
        };
        let result = init(&setup);
        std::fs::remove_dir_all(&home).unwrap();
        assert!(
            matches!(result, Err(Error::NotReady(reason)) if reason.contains("standard library"))
        );
    }

    #[test]
    fn cancel_clones_share_the_flag() {
        let cancel = Cancel::new();
        let other = cancel.clone();
        assert!(!other.is_cancelled());
        cancel.cancel();
        assert!(other.is_cancelled());
    }

    #[test]
    fn errors_read_well() {
        let error = Error::YtDlp {
            kind: "HTTPError".into(),
            message: "HTTP Error 404: Not Found".into(),
        };
        assert_eq!(error.to_string(), "HTTP Error 404: Not Found (HTTPError)");
        assert_eq!(Error::Cancelled.to_string(), "cancelled");
    }
}
