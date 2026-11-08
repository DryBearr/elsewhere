//! What a failure means for the person using the app. Modules keep their own detailed
//! errors; `Problem` is what they become at the edges (the API, a job, an item), always
//! through the functions here, so one kind of failure looks and behaves the same
//! everywhere.

use std::fmt;
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

use crate::logs;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The request itself is wrong (bad link, bad option): the person fixes it.
    Input,
    /// The thing is not there for us: private, removed, blocked in this country.
    Unavailable,
    /// The connection failed: offline, timeout, server trouble, rate limits.
    Network,
    /// The phone's storage: full, read-only, no permission.
    Storage,
    /// Our own setup is broken (for example Python is missing).
    Setup,
    /// Someone stopped it. Not an error to show.
    Cancelled,
    /// Something that should never happen: a bug in the core.
    Bug,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub kind: Kind,
    /// Short, for people: what happened (and what to do).
    pub text: String,
    /// Technical, for the log.
    pub detail: String,
    /// Whether trying again can help.
    pub retry: bool,
}

impl Problem {
    pub fn new(kind: Kind, text: impl Into<String>, detail: impl Into<String>) -> Problem {
        Problem {
            kind,
            text: text.into(),
            detail: detail.into(),
            retry: matches!(kind, Kind::Network | Kind::Storage | Kind::Bug),
        }
    }

    pub fn input(text: impl Into<String>) -> Problem {
        let text = text.into();
        Problem::new(Kind::Input, text.clone(), text)
    }

    pub fn cancelled() -> Problem {
        Problem::new(Kind::Cancelled, "Stopped.", "cancelled")
    }

    pub fn setup(detail: impl Into<String>) -> Problem {
        Problem::new(
            Kind::Setup,
            "The app's downloader could not start. Reopen the app.",
            detail,
        )
    }

    pub fn bug(detail: impl Into<String>) -> Problem {
        Problem::new(Kind::Bug, "Something went wrong.", detail)
    }

    pub fn is_cancelled(&self) -> bool {
        self.kind == Kind::Cancelled
    }
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

impl std::error::Error for Problem {}

/// What a yt-dlp failure means, from its kind and message.
#[cfg(feature = "ytdlp")]
pub fn from_ytdlp(err: &crate::ytdlp::Error) -> Problem {
    use crate::ytdlp::Error;
    match err {
        Error::Cancelled => Problem::cancelled(),
        Error::NotReady(detail) => Problem::setup(detail.clone()),
        Error::BadArgs(detail) => {
            Problem::new(Kind::Input, "That request cannot be used.", detail.clone())
        }
        Error::YtDlp { kind, message } => from_message(&format!("{kind}: {message}")),
    }
}

/// Sorts a failure by its message (yt-dlp, Python, HTTP and system texts).
pub fn from_message(detail: &str) -> Problem {
    let text = detail.to_lowercase();
    let has = |words: &[&str]| words.iter().any(|word| text.contains(word));
    if has(&[
        "no space left",
        "errno 28",
        "disk full",
        "database or disk is full",
    ]) {
        return Problem::new(
            Kind::Storage,
            "The phone is out of space. Free some up and try again.",
            detail,
        );
    }
    if has(&[
        "read-only file system",
        "errno 30",
        "permission denied",
        "errno 13",
        "operation not permitted",
    ]) {
        return Problem::new(Kind::Storage, "The app cannot write its files.", detail);
    }
    if has(&[
        "confirm you’re not a bot",
        "confirm you're not a bot",
        "http error 429",
        "too many requests",
    ]) {
        return Problem::new(
            Kind::Network,
            "YouTube is limiting this network for now. Try again later.",
            detail,
        );
    }
    if has(&[
        "private video",
        "video unavailable",
        "has been removed",
        "is not available",
        "not available in your country",
        "members-only",
        "members only",
        "sign in to confirm your age",
        "account associated with this video has been terminated",
        "http error 404",
        "http error 410",
        "does not exist",
    ]) {
        return Problem {
            retry: false,
            ..Problem::new(
                Kind::Unavailable,
                "This item is private, removed or not available here.",
                detail,
            )
        };
    }
    if has(&["unsupported url", "is not a valid url", "invalid url"]) {
        return Problem::new(
            Kind::Input,
            "That link is not something the app can save.",
            detail,
        );
    }
    if has(&[
        "timed out",
        "timeout",
        "connection reset",
        "connection refused",
        "connection aborted",
        "network is unreachable",
        "temporary failure in name resolution",
        "name or service not known",
        "nodename nor servname",
        "getaddrinfo",
        "no address associated",
        "unable to download webpage",
        "incompleteread",
        "remote end closed",
        "ssl",
        "certificate",
        "http error 5",
        "eof occurred",
        "broken pipe",
        "unreachable",
        "not connected",
    ]) {
        return Problem::new(
            Kind::Network,
            "The connection failed. Check the internet and try again.",
            detail,
        );
    }
    if has(&["cancelled", "canceled"]) {
        return Problem::cancelled();
    }
    // Anything else from a site: the item could not be fetched, but trying again may help
    // (sites change, servers hiccup). Not a bug in the core.
    Problem {
        retry: true,
        ..Problem::new(Kind::Unavailable, "This item could not be saved.", detail)
    }
}

/// What a file system failure means.
pub fn from_io(err: &std::io::Error, doing: &str) -> Problem {
    use std::io::ErrorKind;
    let detail = format!("{doing}: {err}");
    match err.kind() {
        ErrorKind::StorageFull | ErrorKind::QuotaExceeded => Problem::new(
            Kind::Storage,
            "The phone is out of space. Free some up and try again.",
            detail,
        ),
        ErrorKind::PermissionDenied | ErrorKind::ReadOnlyFilesystem => {
            Problem::new(Kind::Storage, "The app cannot write its files.", detail)
        }
        ErrorKind::TimedOut
        | ErrorKind::ConnectionReset
        | ErrorKind::ConnectionRefused
        | ErrorKind::ConnectionAborted => Problem::new(
            Kind::Network,
            "The connection failed. Check the internet and try again.",
            detail,
        ),
        _ => from_message(&detail),
    }
}

/// What a database failure means.
pub fn from_sqlite(err: &rusqlite::Error, doing: &str) -> Problem {
    use rusqlite::ErrorCode;
    let detail = format!("{doing}: {err}");
    match err.sqlite_error_code() {
        Some(ErrorCode::DiskFull) => Problem::new(
            Kind::Storage,
            "The phone is out of space. Free some up and try again.",
            detail,
        ),
        Some(ErrorCode::ReadOnly | ErrorCode::CannotOpen | ErrorCode::PermissionDenied) => {
            Problem::new(Kind::Storage, "The app cannot write its files.", detail)
        }
        Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked) => Problem::bug(detail),
        _ => Problem::bug(detail),
    }
}

static RETRY_DELAYS: Mutex<Option<Vec<Duration>>> = Mutex::new(None);

/// The waits between tries of a failure that can heal by itself (2 s, 10 s, 30 s).
pub fn retry_delays() -> Vec<Duration> {
    RETRY_DELAYS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
        .unwrap_or_else(|| [2, 10, 30].map(Duration::from_secs).to_vec())
}

/// Replaces the waits (and so the number of tries: one more than waits). For tests.
pub fn set_retry_delays(delays: Vec<Duration>) {
    *RETRY_DELAYS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(delays);
}

/// Runs `attempt` until it succeeds, fails with something trying again cannot fix, or the
/// tries run out. Only Network problems are tried again by themselves. `on_retry(try, of,
/// problem)` reports each new try; `stopped()` ends the waiting early (cancel).
pub fn with_retries<T>(
    mut attempt: impl FnMut() -> Result<T, Problem>,
    on_retry: impl Fn(usize, usize, &Problem),
    stopped: impl Fn() -> bool,
) -> Result<T, Problem> {
    let delays = retry_delays();
    let tries = delays.len() + 1;
    let mut tried = 1;
    loop {
        match attempt() {
            Ok(value) => return Ok(value),
            Err(problem) if problem.kind != Kind::Network || tried >= tries => return Err(problem),
            Err(problem) => {
                tried += 1;
                on_retry(tried, tries, &problem);
                if !sleep_unless(delays[tried - 2], &stopped) {
                    return Err(Problem::cancelled());
                }
            }
        }
    }
}

/// Sleeps in small steps; false when `stopped` turned true meanwhile.
fn sleep_unless(total: Duration, stopped: &impl Fn() -> bool) -> bool {
    let step = Duration::from_millis(50);
    let mut left = total;
    while !left.is_zero() {
        if stopped() {
            return false;
        }
        let now = left.min(step);
        thread::sleep(now);
        left -= now;
    }
    !stopped()
}

/// Logs a problem once where it ends up (job, item), at the right level.
pub fn log(problem: &Problem, area: &'static str, job: Option<u64>, item: Option<usize>) {
    let line = match problem.kind {
        Kind::Cancelled => return,
        Kind::Bug | Kind::Setup | Kind::Storage => logs::error(area, problem.text.clone()),
        _ => logs::warn(area, problem.text.clone()),
    };
    let line = line.detail(problem.detail.clone());
    let line = match job {
        Some(job) => line.job(job),
        None => line,
    };
    match item {
        Some(item) => line.item(item).write(),
        None => line.write(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn real_messages_get_the_right_kind() {
        let cases = [
            (
                "DownloadError: ERROR: [youtube] abc: Private video. Sign in if you've been granted access",
                Kind::Unavailable,
            ),
            (
                "DownloadError: ERROR: [youtube] abc: Video unavailable. This video has been removed by the uploader",
                Kind::Unavailable,
            ),
            (
                "DownloadError: ERROR: [youtube] abc: The uploader has not made this video available in your country",
                Kind::Unavailable,
            ),
            (
                "ExtractorError: [youtube] abc: Sign in to confirm you’re not a bot. This helps protect our community.",
                Kind::Network,
            ),
            (
                "HTTPError: HTTP Error 429: Too Many Requests",
                Kind::Network,
            ),
            (
                "DownloadError: ERROR: Unable to download webpage: <urlopen error [Errno -3] Temporary failure in name resolution>",
                Kind::Network,
            ),
            (
                "TransportError: The read operation timed out",
                Kind::Network,
            ),
            (
                "HTTPError: HTTP Error 503: Service Unavailable",
                Kind::Network,
            ),
            (
                "DownloadError: ERROR: unable to download video data: HTTP Error 404: Not Found",
                Kind::Unavailable,
            ),
            (
                "DownloadError: ERROR: Unsupported URL: https://example.com/page",
                Kind::Input,
            ),
            ("OSError: [Errno 28] No space left on device", Kind::Storage),
            (
                "PermissionError: [Errno 13] Permission denied: '/x'",
                Kind::Storage,
            ),
            (
                "ExtractorError: something new and strange happened",
                Kind::Unavailable,
            ),
        ];
        for (detail, kind) in cases {
            let problem = from_message(detail);
            assert_eq!(problem.kind, kind, "{detail}");
            assert_eq!(problem.detail, detail);
            assert!(!problem.text.is_empty());
        }
        assert!(
            !from_message("Private video").retry,
            "trying again does not make a video public"
        );
        assert!(from_message("HTTP Error 503").retry);
        assert!(from_message("strange").retry);
    }

    #[test]
    fn io_and_database_errors() {
        let full = std::io::Error::new(std::io::ErrorKind::StorageFull, "full");
        assert_eq!(from_io(&full, "saving").kind, Kind::Storage);
        let denied = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "no");
        assert_eq!(from_io(&denied, "saving").kind, Kind::Storage);
        let other = std::io::Error::other("Connection reset by peer");
        assert_eq!(from_io(&other, "reading").kind, Kind::Network);
        let busy = rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(5), None);
        assert_eq!(from_sqlite(&busy, "saving").kind, Kind::Bug);
        let disk = rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(13), None);
        assert_eq!(from_sqlite(&disk, "saving").kind, Kind::Storage);
    }

    #[test]
    fn only_network_problems_heal_by_trying_again() {
        set_retry_delays(vec![Duration::from_millis(1); 3]);
        let calls = Cell::new(0);
        let retries = Cell::new(0);
        let flaky = with_retries(
            || {
                calls.set(calls.get() + 1);
                if calls.get() < 3 {
                    Err(from_message("HTTP Error 503"))
                } else {
                    Ok("done")
                }
            },
            |_, _, _| retries.set(retries.get() + 1),
            || false,
        );
        assert_eq!((flaky, calls.get(), retries.get()), (Ok("done"), 3, 2));

        calls.set(0);
        let down: Result<(), _> = with_retries(
            || {
                calls.set(calls.get() + 1);
                Err(from_message("timed out"))
            },
            |_, _, _| {},
            || false,
        );
        assert_eq!((down.unwrap_err().kind, calls.get()), (Kind::Network, 4));

        calls.set(0);
        let gone: Result<(), _> = with_retries(
            || {
                calls.set(calls.get() + 1);
                Err(from_message("Private video"))
            },
            |_, _, _| {},
            || false,
        );
        assert_eq!(
            (gone.unwrap_err().kind, calls.get()),
            (Kind::Unavailable, 1)
        );

        let stopped: Result<(), _> =
            with_retries(|| Err(from_message("timed out")), |_, _, _| {}, || true);
        assert!(stopped.unwrap_err().is_cancelled());
    }
}
