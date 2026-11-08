//! The media engine: plays one local file at a time and says exactly what is happening.
//! Feature `player`. It knows nothing about the library, queues or resume points; the
//! parts of the app that play something (music, movies, episodes, ...) build on it, each
//! with its own `Player` (several can play at once).
//!
//! It is libmpv underneath (mpv.rs), set up for local files only: no network, no
//! scripts, no config files, no following references inside files. It opens only regular
//! files inside the folders `allow_folders` named. The library loads on the first
//! `Player::new`, so an app that never plays anything never loads it.
//!
//! Like jobs, the state is whole and numbered: every change bumps `revision`, and the
//! listener gets the whole `PlayerState` (position changes at most 4 times a second).
//! Each player has one event thread, run under `health::supervise`; a file that does not
//! open within 20 s fails instead of hanging.
//!
//! Video output comes with the apps' video surfaces. Until then the video track is off
//! (sound only), except with `set_null_output`, where video is decoded and thrown away.

mod machine;
mod mpv;

use std::fs;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use machine::{Event, Machine};
use mpv::{Handle, Waited};

use crate::problem::{Kind, Problem};
use crate::{health, logs};

/// A file that is not open after this long fails.
const LOAD_LIMIT: Duration = Duration::from_secs(20);
/// mpv's warnings and errors written to the core's log per file, at most.
const LOG_LINES_PER_FILE: usize = 50;
pub const MIN_SPEED: f64 = 0.25;
pub const MAX_SPEED: f64 = 4.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Nothing open.
    Idle,
    Loading,
    Playing,
    Paused,
    /// At the end of the file (paused there; play starts it over).
    Ended,
    /// The file could not be played; `problem` says why.
    Failed,
}

/// Everything about a player, whole: replace what is shown with a newer revision.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayerState {
    pub revision: u64,
    pub status: Status,
    pub file: Option<PathBuf>,
    pub position_ms: u64,
    /// Known once the file is open (live or broken files may have none).
    pub duration_ms: Option<u64>,
    pub speed: f64,
    /// 0 to 100.
    pub volume: f64,
    pub has_video: bool,
    pub has_audio: bool,
    pub problem: Option<Problem>,
}

type Listener = Arc<dyn Fn(PlayerState) + Send + Sync>;

static ALLOWED: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
static NULL_OUTPUT: AtomicBool = AtomicBool::new(false);

fn allowed() -> MutexGuard<'static, Vec<PathBuf>> {
    ALLOWED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The folders players may open files from (with their subfolders). Replaces earlier
/// ones. Folders that do not exist are left out.
pub fn allow_folders(folders: &[PathBuf]) {
    *allowed() = folders
        .iter()
        .filter_map(|folder| fs::canonicalize(folder).ok())
        .collect();
}

/// Players made after this have no window and no sound; video is still decoded. For
/// tests and machines without outputs.
pub fn set_null_output(on: bool) {
    NULL_OUTPUT.store(on, Ordering::SeqCst);
}

/// The real file behind `path`, if a player may open it.
fn checked(path: &str) -> Result<PathBuf, Problem> {
    let refuse = |detail: String| {
        Err(Problem {
            retry: false,
            ..Problem::new(
                Kind::Input,
                "Only media files saved in the app can be played.",
                detail,
            )
        })
    };
    if path.contains("://") || !Path::new(path).is_absolute() {
        return refuse(format!("not a local absolute path: {path:?}"));
    }
    let real = match fs::canonicalize(path) {
        Ok(real) => real,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Err(Problem {
                retry: false,
                ..Problem::new(
                    Kind::Unavailable,
                    "The file is missing.",
                    format!("{path}: {err}"),
                )
            });
        }
        Err(err) => return Err(crate::problem::from_io(&err, &format!("opening {path}"))),
    };
    if !real.is_file() {
        return refuse(format!("not a regular file: {}", real.display()));
    }
    if !allowed().iter().any(|folder| real.starts_with(folder)) {
        return refuse(format!("outside the allowed folders: {}", real.display()));
    }
    if real.to_str().is_none() {
        return refuse(format!("not a UTF-8 path: {}", real.display()));
    }
    Ok(real)
}

fn player_problem(text: &str, detail: String) -> Problem {
    Problem {
        retry: false,
        ..Problem::new(Kind::Setup, text, detail)
    }
}

/// mpv options, set before it starts. A failing one in the first list stops the player
/// from being made (they keep it local and quiet); the second list may be missing from
/// older mpv versions.
fn configure(mpv: &Handle) -> Result<(), Problem> {
    let mut required = vec![
        ("config", "no"),
        ("load-scripts", "no"),
        ("ytdl", "no"),
        ("access-references", "no"),
        ("load-unsafe-playlists", "no"),
        ("idle", "yes"),
        ("keep-open", "yes"),
    ];
    if NULL_OUTPUT.load(Ordering::SeqCst) {
        required.extend([("vo", "null"), ("ao", "null")]);
    } else {
        required.push(("vid", "no"));
    }
    for (name, value) in required {
        mpv.set_option(name, value).map_err(|code| {
            player_problem(
                "The player could not start.",
                format!("option {name}={value}: {}", mpv.error_text(code)),
            )
        })?;
    }
    let optional = [
        ("terminal", "no"),
        ("input-default-bindings", "no"),
        ("input-builtin-bindings", "no"),
        ("input-vo-keyboard", "no"),
        ("osc", "no"),
        ("load-stats-overlay", "no"),
        ("load-console", "no"),
        ("load-select", "no"),
        ("load-positioning", "no"),
        ("load-commands", "no"),
        ("load-context-menu", "no"),
        ("load-auto-profiles", "no"),
        ("resume-playback", "no"),
        ("save-position-on-quit", "no"),
        ("autoload-files", "no"),
        ("audio-display", "no"),
        ("hwdec", "auto-safe"),
        ("audio-client-name", "Elsewhere"),
    ];
    // Android: AudioTrack needs a Java VM handed to FFmpeg; AAudio and OpenSL ES do not.
    #[cfg(target_os = "android")]
    if !NULL_OUTPUT.load(Ordering::SeqCst) {
        mpv.set_option("ao", "aaudio,opensles").map_err(|code| {
            player_problem(
                "The player could not start.",
                format!("option ao: {}", mpv.error_text(code)),
            )
        })?;
    }
    for (name, value) in optional {
        if let Err(code) = mpv.set_option(name, value)
            && code != mpv::OPTION_NOT_FOUND
        {
            logs::warn("player", format!("mpv option {name}={value} failed"))
                .detail(mpv.error_text(code))
                .write();
        }
    }
    Ok(())
}

struct Inner {
    name: String,
    mpv: Handle,
    machine: Mutex<Machine>,
    listener: Mutex<Option<Listener>>,
    closing: AtomicBool,
    logged: AtomicUsize,
}

/// One player. Drop it to free it (its file stops).
pub struct Player {
    inner: Arc<Inner>,
}

impl Player {
    /// Loads libmpv if this is the first player, and starts one.
    pub fn new() -> Result<Player, Problem> {
        static PLAYERS: AtomicUsize = AtomicUsize::new(1);
        let mpv = Handle::create()
            .map_err(|detail| player_problem("The player is not part of this build.", detail))?;
        configure(&mpv)?;
        let start = |what: &str, code| {
            player_problem(
                "The player could not start.",
                format!("{what}: {}", mpv.error_text(code)),
            )
        };
        mpv.initialize()
            .map_err(|code| start("mpv_initialize", code))?;
        mpv.observe()
            .map_err(|code| start("observing properties", code))?;
        mpv.request_logs("warn")
            .map_err(|code| start("requesting logs", code))?;
        let inner = Arc::new(Inner {
            name: format!("player-{}", PLAYERS.fetch_add(1, Ordering::SeqCst)),
            mpv,
            machine: Mutex::new(Machine::new()),
            listener: Mutex::new(None),
            closing: AtomicBool::new(false),
            logged: AtomicUsize::new(0),
        });
        let events = Arc::clone(&inner);
        health::supervise(inner.name.clone(), move || events.run())
            .map_err(|err| Problem::bug(format!("could not start the player thread: {err}")))?;
        Ok(Player { inner })
    }

    /// Opens a file (an absolute path inside the allowed folders) at `start_ms`, playing
    /// at once or paused. Replaces what was open. How it goes arrives as states: Loading,
    /// then Playing or Paused, or Failed.
    pub fn open(&self, path: &str, start_ms: u64, autoplay: bool) -> Result<(), Problem> {
        let file = checked(path)?;
        let text = file.to_str().unwrap_or_default().to_owned();
        let inner = &self.inner;
        // Holding the machine while mpv takes the file: its first events wait until the
        // machine knows which playlist entry is the new one.
        let mut machine = inner.machine();
        machine.opening(file, start_ms, autoplay, Instant::now());
        inner.logged.store(0, Ordering::SeqCst);
        let start = if start_ms > 0 {
            format!("{:.3}", start_ms as f64 / 1000.0)
        } else {
            "none".to_owned()
        };
        let loaded = inner
            .mpv
            .set_string("start", &start)
            .and_then(|()| inner.mpv.set_flag("pause", !autoplay))
            .and_then(|()| inner.mpv.command(&["loadfile", &text, "replace"]));
        let entry = loaded.map(|()| inner.mpv.get_i64("playlist/0/id"));
        let result = match entry {
            Ok(Some(entry)) => {
                machine.expect(entry);
                Ok(())
            }
            Ok(None) => Err(Problem::bug("mpv took the file but has no playlist entry")),
            Err(code) => Err(Problem::bug(format!(
                "loadfile: {}",
                inner.mpv.error_text(code)
            ))),
        };
        if let Err(problem) = &result {
            machine.fail(problem.clone());
        }
        drop(machine);
        inner.mpv.wakeup();
        result
    }

    /// Plays from where it is; at the end, from the start.
    pub fn play(&self) -> Result<(), Problem> {
        let (status, at_end) = {
            let machine = self.inner.machine();
            (machine.state().status, machine.at_end())
        };
        match status {
            Status::Idle | Status::Failed => Err(nothing_open()),
            Status::Loading => self.mpv_call(|mpv| mpv.set_flag("pause", false)),
            _ => {
                if at_end {
                    self.mpv_call(|mpv| mpv.command(&["seek", "0", "absolute+exact"]))?;
                }
                self.mpv_call(|mpv| mpv.set_flag("pause", false))
            }
        }
    }

    pub fn pause(&self) -> Result<(), Problem> {
        match self.status() {
            Status::Idle | Status::Failed => Err(nothing_open()),
            _ => self.mpv_call(|mpv| mpv.set_flag("pause", true)),
        }
    }

    /// Goes to `position_ms` (within the file: before 0 is 0, past the end is the end).
    pub fn seek(&self, position_ms: i64) -> Result<(), Problem> {
        let (status, duration) = {
            let machine = self.inner.machine();
            (machine.state().status, machine.state().duration_ms)
        };
        if !matches!(status, Status::Playing | Status::Paused | Status::Ended) {
            return Err(nothing_open());
        }
        let mut target = position_ms.max(0) as u64;
        if let Some(duration) = duration {
            target = target.min(duration);
        }
        let seconds = format!("{:.3}", target as f64 / 1000.0);
        self.mpv_call(|mpv| mpv.command(&["seek", &seconds, "absolute+exact"]))
    }

    /// Playback speed, from MIN_SPEED to MAX_SPEED (1 is normal; the pitch stays).
    pub fn set_speed(&self, speed: f64) -> Result<(), Problem> {
        if !speed.is_finite() {
            return Err(Problem::input("That is not a speed."));
        }
        let speed = speed.clamp(MIN_SPEED, MAX_SPEED);
        self.mpv_call(|mpv| mpv.set_double("speed", speed))
    }

    /// Volume from 0 to 100.
    pub fn set_volume(&self, volume: f64) -> Result<(), Problem> {
        if !volume.is_finite() {
            return Err(Problem::input("That is not a volume."));
        }
        let volume = volume.clamp(0.0, 100.0);
        self.mpv_call(|mpv| mpv.set_double("volume", volume))
    }

    /// Closes the file: Idle.
    pub fn stop(&self) -> Result<(), Problem> {
        let mut machine = self.inner.machine();
        machine.idle();
        let stopped = self.inner.mpv.command(&["stop"]);
        drop(machine);
        self.inner.mpv.wakeup();
        stopped.map_err(|code| Problem::bug(format!("stop: {}", self.inner.mpv.error_text(code))))
    }

    pub fn state(&self) -> PlayerState {
        self.inner.machine().state().clone()
    }

    /// Sets the one listener (replacing any earlier one). It gets the current state at
    /// once, then every change, on the player's thread, in order: keep it short.
    pub fn set_listener(&self, listener: impl Fn(PlayerState) + Send + Sync + 'static) {
        *self
            .inner
            .listener
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Arc::new(listener));
        self.inner.machine().resend();
        self.inner.mpv.wakeup();
    }

    fn status(&self) -> Status {
        self.inner.machine().state().status
    }

    fn mpv_call(&self, call: impl FnOnce(&Handle) -> Result<(), i32>) -> Result<(), Problem> {
        let mpv = &self.inner.mpv;
        call(mpv).map_err(|code| Problem::bug(format!("mpv: {}", mpv.error_text(code))))?;
        mpv.wakeup();
        Ok(())
    }
}

impl Drop for Player {
    /// Ends the event thread, which frees mpv as it goes.
    fn drop(&mut self) {
        self.inner.closing.store(true, Ordering::SeqCst);
        self.inner.mpv.wakeup();
    }
}

fn nothing_open() -> Problem {
    Problem {
        retry: false,
        ..Problem::new(
            Kind::Input,
            "Nothing is open in the player.",
            "no file is open",
        )
    }
}

impl Inner {
    fn machine(&self) -> MutexGuard<'_, Machine> {
        self.machine
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The event thread: turns mpv's events into states and sends them.
    fn run(&self) {
        loop {
            if self.closing.load(Ordering::SeqCst) {
                return;
            }
            let wait = self
                .machine()
                .next_wake(Instant::now(), LOAD_LIMIT)
                .unwrap_or(Duration::from_millis(500))
                .min(Duration::from_millis(500));
            let mut waited = self.mpv.wait(wait.as_secs_f64());
            // Everything that is queued, then one delivery.
            loop {
                match waited {
                    Waited::Nothing => break,
                    Waited::Shutdown => {
                        self.machine()
                            .fail(Problem::bug("mpv shut down on its own"));
                        self.deliver();
                        return;
                    }
                    Waited::Overflow => {
                        logs::warn("player", "mpv's event queue overflowed").write()
                    }
                    Waited::Log(level, line) => self.log(&level, line),
                    Waited::FileLoaded => {
                        let (has_video, has_audio) = self.tracks();
                        self.machine().apply(Event::FileLoaded {
                            has_video,
                            has_audio,
                        });
                    }
                    Waited::Event(event) => self.machine().apply(event),
                }
                if self.closing.load(Ordering::SeqCst) {
                    return;
                }
                waited = self.mpv.wait(0.0);
            }
            if self.machine().check_loading(Instant::now(), LOAD_LIMIT) {
                let _ = self.mpv.command(&["stop"]);
                logs::warn("player", "A file did not open in time and was stopped").write();
            }
            self.deliver();
        }
    }

    /// Which kinds of tracks the open file has.
    fn tracks(&self) -> (bool, bool) {
        let count = self
            .mpv
            .get_i64("track-list/count")
            .unwrap_or(0)
            .clamp(0, 1000);
        let mut kinds = (false, false);
        for index in 0..count {
            match self
                .mpv
                .get_string(&format!("track-list/{index}/type"))
                .as_deref()
            {
                Some("video") => {
                    let image = self.mpv.get_string(&format!("track-list/{index}/image"));
                    // Cover art is a still picture, not a video.
                    if image.as_deref() != Some("yes") {
                        kinds.0 = true;
                    }
                }
                Some("audio") => kinds.1 = true,
                _ => {}
            }
        }
        kinds
    }

    fn log(&self, level: &str, line: String) {
        let error = matches!(level, "error" | "fatal");
        if error {
            self.machine().apply(Event::ErrorLog(line.clone()));
        }
        if self.logged.fetch_add(1, Ordering::SeqCst) >= LOG_LINES_PER_FILE {
            return;
        }
        let message = format!("{}: {line}", self.name);
        if error {
            logs::error("player", message).write();
        } else {
            logs::warn("player", message).write();
        }
    }

    fn deliver(&self) {
        let Some(state) = self.machine().take_due(Instant::now()) else {
            return;
        };
        let listener = self
            .listener
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        if let Some(listener) = listener
            && catch_unwind(AssertUnwindSafe(|| listener(state))).is_err()
        {
            logs::error("player", format!("{}: the listener panicked", self.name)).write();
        }
    }
}
