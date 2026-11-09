//! The core's log: what happened, on the device only. Nothing here ever leaves the phone.
//!
//! One JSON object per line in `<data>/logs/elsewhere.log` (a later screen can filter and
//! colour it; it also reads fine as text). When the file passes MAX_BYTES it becomes
//! `elsewhere.1.log` (replacing the older one) and a new file starts, so the logs never take
//! more than about 1 MB. A panic anywhere writes a `crash-<time>.txt` with its backtrace.
//!
//! ```text
//! {"t":"2026-11-08T14:03:12Z","lvl":"warn","area":"playlist","job":12,"item":3,"msg":"Slow server, trying again (2 of 3)","detail":"HTTP Error 503"}
//! ```
//!
//! Writing a log line never fails the caller: if the file cannot be written, the line is
//! dropped. Before `init`, lines wait in memory (up to 200) and are written by `init`.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, Once};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_BYTES: u64 = 512 * 1024;
const EARLY_LINES: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Error,
    Warn,
    Info,
}

/// One log line. Build it with `Line::new`, add context, then `write` it.
#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    pub level: Level,
    /// Which part of the core: core, jobs, playlist, ytdlp, db, python, ...
    pub area: &'static str,
    pub job: Option<u64>,
    pub item: Option<usize>,
    /// For people.
    pub msg: String,
    /// The technical part.
    pub detail: Option<String>,
}

impl Line {
    pub fn new(level: Level, area: &'static str, msg: impl Into<String>) -> Line {
        Line {
            level,
            area,
            job: None,
            item: None,
            msg: msg.into(),
            detail: None,
        }
    }

    pub fn job(mut self, job: u64) -> Line {
        self.job = Some(job);
        self
    }

    pub fn item(mut self, item: usize) -> Line {
        self.item = Some(item);
        self
    }

    pub fn detail(mut self, detail: impl Into<String>) -> Line {
        self.detail = Some(detail.into());
        self
    }

    pub fn write(self) {
        write_line(&self.json(now()));
    }

    fn json(&self, unix: u64) -> String {
        let mut out = format!(
            "{{\"t\":\"{}\",\"lvl\":\"{}\",\"area\":",
            timestamp(unix),
            match self.level {
                Level::Error => "error",
                Level::Warn => "warn",
                Level::Info => "info",
            }
        );
        push_string(&mut out, self.area);
        if let Some(job) = self.job {
            out.push_str(&format!(",\"job\":{job}"));
        }
        if let Some(item) = self.item {
            out.push_str(&format!(",\"item\":{item}"));
        }
        out.push_str(",\"msg\":");
        push_string(&mut out, &self.msg);
        if let Some(detail) = &self.detail {
            out.push_str(",\"detail\":");
            push_string(&mut out, detail);
        }
        out.push('}');
        out
    }
}

pub fn error(area: &'static str, msg: impl Into<String>) -> Line {
    Line::new(Level::Error, area, msg)
}

pub fn warn(area: &'static str, msg: impl Into<String>) -> Line {
    Line::new(Level::Warn, area, msg)
}

pub fn info(area: &'static str, msg: impl Into<String>) -> Line {
    Line::new(Level::Info, area, msg)
}

/// Where the log goes, or the lines that came before it was known.
enum Sink {
    Early(Vec<String>),
    File { dir: PathBuf, size: u64 },
}

static SINK: Mutex<Sink> = Mutex::new(Sink::Early(Vec::new()));

fn sink() -> MutexGuard<'static, Sink> {
    SINK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Starts writing to `<dir>` (made if needed), writes the lines that waited, and installs
/// the panic hook. Calling it again moves the log to the new folder.
/// Whether lines go to the log file yet (false until `init`).
pub fn to_file() -> bool {
    matches!(&*sink(), Sink::File { .. })
}

pub fn init(dir: &Path) {
    let _ = fs::create_dir_all(dir);
    let size = fs::metadata(dir.join("elsewhere.log")).map_or(0, |meta| meta.len());
    let early = {
        let mut sink = sink();
        let early = match &mut *sink {
            Sink::Early(lines) => std::mem::take(lines),
            Sink::File { .. } => Vec::new(),
        };
        *sink = Sink::File {
            dir: dir.to_owned(),
            size,
        };
        early
    };
    for line in early {
        write_line(&line);
    }
    install_panic_hook();
}

fn write_line(line: &str) {
    let mut sink = sink();
    match &mut *sink {
        Sink::Early(lines) => {
            if lines.len() < EARLY_LINES {
                lines.push(line.to_owned());
            }
        }
        Sink::File { dir, size } => {
            if *size >= MAX_BYTES {
                let _ = fs::rename(dir.join("elsewhere.log"), dir.join("elsewhere.1.log"));
                *size = 0;
            }
            let written = OpenOptions::new()
                .create(true)
                .append(true)
                .open(dir.join("elsewhere.log"))
                .and_then(|mut file| writeln!(file, "{line}"));
            if written.is_ok() {
                *size += line.len() as u64 + 1;
            }
        }
    }
}

/// The newest `limit` lines, oldest first (from both files). For the future logs screen
/// and for tests.
pub fn recent(limit: usize) -> Vec<String> {
    let dir = match &*sink() {
        Sink::Early(lines) => return lines.iter().rev().take(limit).rev().cloned().collect(),
        Sink::File { dir, .. } => dir.clone(),
    };
    let mut lines: Vec<String> = ["elsewhere.1.log", "elsewhere.log"]
        .iter()
        .filter_map(|name| fs::read_to_string(dir.join(name)).ok())
        .flat_map(|text| text.lines().map(str::to_owned).collect::<Vec<_>>())
        .collect();
    let skip = lines.len().saturating_sub(limit);
    lines.drain(..skip);
    lines
}

/// The crash notes written so far (newest last).
pub fn crash_notes() -> Vec<PathBuf> {
    let Sink::File { dir, .. } = &*sink() else {
        return Vec::new();
    };
    let mut notes: Vec<PathBuf> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("crash-"))
        })
        .collect();
    notes.sort();
    notes
}

/// Every panic, also the ones the core catches and recovers from, leaves a log line and a
/// crash note, then the usual panic message.
fn install_panic_hook() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let thread = std::thread::current()
                .name()
                .unwrap_or("unnamed")
                .to_owned();
            let message = info
                .payload()
                .downcast_ref::<&str>()
                .map(|text| text.to_string())
                .or_else(|| info.payload().downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown".to_owned());
            let place = info
                .location()
                .map(|place| format!("{}:{}", place.file(), place.line()))
                .unwrap_or_default();
            error("core", format!("Panic in {thread}"))
                .detail(format!("{message} at {place}"))
                .write();
            write_crash_note(&thread, &message, &place);
            previous(info);
        }));
    });
}

fn write_crash_note(thread: &str, message: &str, place: &str) {
    let Sink::File { dir, .. } = &*sink() else {
        return;
    };
    let unix = now();
    let backtrace = std::backtrace::Backtrace::force_capture();
    let mut index = 0;
    // Two panics in the same second get their own notes.
    let path = loop {
        let suffix = if index == 0 {
            String::new()
        } else {
            format!("-{index}")
        };
        let path = dir.join(format!(
            "crash-{}{suffix}.txt",
            timestamp(unix).replace(':', "-")
        ));
        if !path.exists() || index > 99 {
            break path;
        }
        index += 1;
    };
    let note = format!(
        "time: {}\nthread: {thread}\nmessage: {message}\nwhere: {place}\nversion: {}\n\n{backtrace}\n",
        timestamp(unix),
        env!("CARGO_PKG_VERSION")
    );
    let _ = File::create(path).and_then(|mut file| file.write_all(note.as_bytes()));
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |time| time.as_secs())
}

/// `2026-11-08T14:03:12Z` from seconds since 1970 (UTC).
fn timestamp(unix: u64) -> String {
    let days = (unix / 86_400) as i64;
    let seconds = unix % 86_400;
    // Days to a civil date (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        seconds / 3600,
        seconds % 3600 / 60,
        seconds % 60
    )
}

/// Appends `text` as a JSON string.
fn push_string(out: &mut String, text: &str) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_are_json_with_escaping() {
        let line = warn("playlist", "Slow \"server\"\nagain")
            .job(12)
            .item(3)
            .detail("tab\there \\ é ✓ \u{1}");
        assert_eq!(
            line.json(0),
            r#"{"t":"1970-01-01T00:00:00Z","lvl":"warn","area":"playlist","job":12,"item":3,"msg":"Slow \"server\"\nagain","detail":"tab\there \\ é ✓ \u0001"}"#
        );
        assert_eq!(
            info("core", "hi").json(0),
            r#"{"t":"1970-01-01T00:00:00Z","lvl":"info","area":"core","msg":"hi"}"#
        );
    }

    #[test]
    fn timestamps_are_utc_dates() {
        assert_eq!(timestamp(0), "1970-01-01T00:00:00Z");
        assert_eq!(timestamp(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(timestamp(1_794_143_592), "2026-11-08T13:13:12Z");
        assert_eq!(timestamp(4_102_444_799), "2099-12-31T23:59:59Z");
    }
}
