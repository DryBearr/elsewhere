//! What a player shows, worked out from what mpv reports. Plain logic with no mpv inside,
//! so every rule is tested without the library (and on any machine).
//!
//! mpv reports a file's life as events: START_FILE, FILE_LOADED, property changes
//! (position, pause, end reached, ...) and END_FILE. A new file replaces the old one, and
//! the old file's last events can still arrive after the new one was asked for. So the
//! machine only listens to the playlist entry it expects (the one `open` started), and
//! ignores everything about other entries.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::{PlayerState, Status};
use crate::problem::{Kind, Problem};

/// Position changes go out at most this often; everything else at once.
pub(super) const POSITION_INTERVAL: Duration = Duration::from_millis(250);

/// What mpv reported, already copied out of its memory.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Event {
    StartFile {
        entry: i64,
    },
    /// The file is open and its tracks are known.
    FileLoaded {
        has_video: bool,
        has_audio: bool,
        /// The picture's width and height as shown (rotation applied), for a video.
        video_size: Option<(u32, u32)>,
    },
    EndFile {
        entry: i64,
        reason: EndReason,
        error: i32,
    },
    Position(Option<f64>),
    Duration(Option<f64>),
    Pause(bool),
    EndReached(bool),
    Speed(f64),
    Volume(f64),
    /// Whether the video output shows a picture.
    VideoShown(bool),
    /// An error line from mpv: kept as the detail of a failure that follows.
    ErrorLog(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EndReason {
    Eof,
    Stop,
    Error,
    Other,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Pending {
    Nothing,
    Throttled,
    Now,
}

pub(super) struct Machine {
    state: PlayerState,
    /// The playlist entry of the file `open` asked for, once mpv said which it is.
    expected: Option<i64>,
    started: bool,
    loaded: bool,
    paused: bool,
    end_reached: bool,
    loading_since: Option<Instant>,
    /// The position mpv reported last. Shown, except at the end, where mpv can report an
    /// earlier time than the end it reached (the duration is shown then).
    reported_ms: u64,
    error_log: Option<String>,
    pending: Pending,
    last_sent: Option<Instant>,
}

impl Machine {
    pub fn new() -> Machine {
        Machine {
            state: PlayerState {
                revision: 1,
                status: Status::Idle,
                file: None,
                position_ms: 0,
                duration_ms: None,
                speed: 1.0,
                volume: 100.0,
                has_video: false,
                has_audio: false,
                video_size: None,
                video_shown: false,
                problem: None,
            },
            expected: None,
            started: false,
            loaded: false,
            paused: false,
            end_reached: false,
            loading_since: None,
            reported_ms: 0,
            error_log: None,
            pending: Pending::Now,
            last_sent: None,
        }
    }

    pub fn state(&self) -> &PlayerState {
        &self.state
    }

    /// Whether a file is open (playing, paused or at its end).
    pub fn is_loaded(&self) -> bool {
        self.loaded && !matches!(self.state.status, Status::Idle | Status::Failed)
    }

    /// `open` asked for `file`; its events count once `expect` names its entry.
    pub fn opening(&mut self, file: PathBuf, start_ms: u64, autoplay: bool, now: Instant) {
        let state = &mut self.state;
        state.status = Status::Loading;
        state.file = Some(file);
        state.position_ms = start_ms;
        state.duration_ms = None;
        state.has_video = false;
        state.has_audio = false;
        state.video_size = None;
        state.problem = None;
        self.expected = None;
        self.started = false;
        self.loaded = false;
        self.paused = !autoplay;
        self.end_reached = false;
        self.loading_since = Some(now);
        self.reported_ms = start_ms;
        self.error_log = None;
        self.touch(true);
    }

    /// The playlist entry mpv made for the file `opening` named.
    pub fn expect(&mut self, entry: i64) {
        self.expected = Some(entry);
    }

    /// Nothing open any more (stop).
    pub fn idle(&mut self) {
        let state = &mut self.state;
        state.status = Status::Idle;
        state.file = None;
        state.position_ms = 0;
        state.duration_ms = None;
        state.has_video = false;
        state.has_audio = false;
        state.video_size = None;
        state.problem = None;
        self.expected = None;
        self.started = false;
        self.loaded = false;
        self.end_reached = false;
        self.loading_since = None;
        self.touch(true);
    }

    pub fn fail(&mut self, problem: Problem) {
        self.state.status = Status::Failed;
        self.state.problem = Some(problem);
        self.expected = None;
        self.loaded = false;
        self.loading_since = None;
        self.touch(true);
    }

    pub fn apply(&mut self, event: Event) {
        match event {
            Event::StartFile { entry } => {
                if Some(entry) == self.expected {
                    self.started = true;
                }
            }
            Event::FileLoaded {
                has_video,
                has_audio,
                video_size,
            } => {
                // FILE_LOADED carries no entry: it is ours once our entry started.
                if !self.started || self.expected.is_none() {
                    return;
                }
                self.loaded = true;
                self.loading_since = None;
                self.state.has_video = has_video;
                self.state.has_audio = has_audio;
                self.state.video_size = video_size;
                self.refresh();
            }
            Event::EndFile {
                entry,
                reason,
                error,
            } => {
                if Some(entry) != self.expected {
                    return;
                }
                match reason {
                    EndReason::Eof => {
                        self.end_reached = true;
                        self.refresh();
                    }
                    EndReason::Error => {
                        let problem = problem_for(error, self.error_log.take());
                        self.fail(problem);
                    }
                    EndReason::Stop => self.idle(),
                    EndReason::Other => {}
                }
            }
            Event::Position(position) => {
                if !self.is_loaded() {
                    return;
                }
                if let Some(seconds) = position {
                    self.reported_ms = millis(seconds);
                    if self.state.status != Status::Ended
                        && self.reported_ms != self.state.position_ms
                    {
                        self.state.position_ms = self.reported_ms;
                        self.touch(false);
                    }
                }
            }
            Event::Duration(duration) => {
                if !self.started {
                    return;
                }
                let ms = duration.map(millis);
                if ms != self.state.duration_ms {
                    self.state.duration_ms = ms;
                    self.touch(true);
                }
            }
            Event::Pause(paused) => {
                self.paused = paused;
                self.refresh();
            }
            Event::EndReached(reached) => {
                if !self.is_loaded() {
                    return;
                }
                self.end_reached = reached;
                self.refresh();
            }
            Event::Speed(speed) => {
                if speed != self.state.speed {
                    self.state.speed = speed;
                    self.touch(true);
                }
            }
            Event::Volume(volume) => {
                if volume != self.state.volume {
                    self.state.volume = volume;
                    self.touch(true);
                }
            }
            Event::VideoShown(shown) => {
                if shown != self.state.video_shown {
                    self.state.video_shown = shown;
                    self.touch(true);
                }
            }
            Event::ErrorLog(line) => self.error_log = Some(line),
        }
    }

    /// A file that does not open within `limit` fails (the caller stops mpv). True then.
    pub fn check_loading(&mut self, now: Instant, limit: Duration) -> bool {
        match self.loading_since {
            Some(since) if now.duration_since(since) > limit => {
                let detail = format!("the file did not open within {} s", limit.as_secs());
                self.fail(Problem {
                    retry: false,
                    ..Problem::new(Kind::Unavailable, "The file took too long to open.", detail)
                });
                true
            }
            _ => false,
        }
    }

    /// Sends the whole state again (a new listener).
    pub fn resend(&mut self) {
        self.pending = Pending::Now;
    }

    /// The state to send now, if any (position changes at most every POSITION_INTERVAL).
    pub fn take_due(&mut self, now: Instant) -> Option<PlayerState> {
        let due = match self.pending {
            Pending::Nothing => false,
            Pending::Now => true,
            Pending::Throttled => self
                .last_sent
                .is_none_or(|sent| now.duration_since(sent) >= POSITION_INTERVAL),
        };
        if !due {
            return None;
        }
        self.pending = Pending::Nothing;
        self.last_sent = Some(now);
        Some(self.state.clone())
    }

    /// How long until something is due: a throttled position, or the loading limit.
    pub fn next_wake(&self, now: Instant, limit: Duration) -> Option<Duration> {
        let throttled = (self.pending == Pending::Throttled).then(|| {
            self.last_sent.map_or(Duration::ZERO, |sent| {
                (sent + POSITION_INTERVAL).saturating_duration_since(now)
            })
        });
        let loading = self
            .loading_since
            .map(|since| (since + limit).saturating_duration_since(now) + Duration::from_millis(1));
        match (throttled, loading) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    fn refresh(&mut self) {
        if !self.loaded || matches!(self.state.status, Status::Idle | Status::Failed) {
            return;
        }
        let status = if self.end_reached {
            Status::Ended
        } else if self.paused {
            Status::Paused
        } else {
            Status::Playing
        };
        if status != self.state.status {
            self.state.status = status;
            self.touch(true);
        }
        let position = match (status, self.state.duration_ms) {
            (Status::Ended, Some(duration)) => duration,
            _ => self.reported_ms,
        };
        if position != self.state.position_ms {
            self.state.position_ms = position;
            self.touch(true);
        }
    }

    /// At the end, or so close that playing would end at once.
    pub fn at_end(&self) -> bool {
        self.state.status == Status::Ended
            || self
                .state
                .duration_ms
                .is_some_and(|duration| self.state.position_ms + 100 >= duration)
    }

    fn touch(&mut self, now: bool) {
        self.state.revision += 1;
        let pending = if now {
            Pending::Now
        } else {
            Pending::Throttled
        };
        self.pending = self.pending.max(pending);
    }
}

fn millis(seconds: f64) -> u64 {
    if seconds.is_finite() && seconds > 0.0 {
        (seconds * 1000.0).round() as u64
    } else {
        0
    }
}

/// What an mpv error code at the end of a file means for people.
pub(super) fn problem_for(error: i32, log: Option<String>) -> Problem {
    let (kind, text) = match error {
        -13 => (Kind::Unavailable, "This file cannot be played."),
        -16 => (
            Kind::Unavailable,
            "This file has no sound or picture the player can use.",
        ),
        -17 => (
            Kind::Unavailable,
            "This is not a media file the player knows.",
        ),
        -14 | -15 => (
            Kind::Setup,
            "The phone's sound or video output could not start.",
        ),
        _ => (Kind::Bug, "The player failed."),
    };
    let detail = match log {
        Some(line) => format!("mpv error {error}: {line}"),
        None => format!("mpv error {error}"),
    };
    Problem::new(kind, text, detail).with_retry(matches!(kind, Kind::Setup | Kind::Bug))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loaded(machine: &mut Machine, now: Instant) {
        machine.opening(PathBuf::from("/media/a.mp4"), 0, true, now);
        machine.expect(1);
        machine.apply(Event::StartFile { entry: 1 });
        machine.apply(Event::Duration(Some(2.0)));
        machine.apply(Event::Pause(false));
        machine.apply(Event::FileLoaded {
            has_video: true,
            has_audio: true,
            video_size: None,
        });
    }

    #[test]
    fn a_file_goes_from_loading_to_playing_to_ended() {
        let now = Instant::now();
        let mut machine = Machine::new();
        assert_eq!(machine.state().status, Status::Idle);
        machine.opening(PathBuf::from("/media/a.mp4"), 0, true, now);
        assert_eq!(machine.state().status, Status::Loading);
        loaded(&mut machine, now);
        let state = machine.state().clone();
        assert_eq!(
            (
                state.status,
                state.duration_ms,
                state.has_video,
                state.has_audio
            ),
            (Status::Playing, Some(2000), true, true)
        );
        machine.apply(Event::Position(Some(1.5)));
        assert_eq!(machine.state().position_ms, 1500);
        // keep-open: at the end mpv pauses and says the end is reached.
        machine.apply(Event::Pause(true));
        machine.apply(Event::EndReached(true));
        assert_eq!(
            (machine.state().status, machine.state().position_ms),
            (Status::Ended, 2000)
        );
        // mpv can report an earlier time once at the end: the end stays the end.
        machine.apply(Event::Position(Some(1.2)));
        assert_eq!(machine.state().position_ms, 2000);
        // Seeking back leaves the end: paused where the seek went (in either order).
        machine.apply(Event::Position(Some(0.5)));
        machine.apply(Event::EndReached(false));
        assert_eq!(
            (machine.state().status, machine.state().position_ms),
            (Status::Paused, 500)
        );
    }

    #[test]
    fn events_of_the_file_before_are_ignored() {
        let now = Instant::now();
        let mut machine = Machine::new();
        loaded(&mut machine, now);
        machine.opening(PathBuf::from("/media/b.mp4"), 0, true, now);
        // The old file's last events arrive after the new one was asked for.
        machine.apply(Event::Position(Some(1.9)));
        machine.apply(Event::EndFile {
            entry: 1,
            reason: EndReason::Stop,
            error: 0,
        });
        machine.apply(Event::FileLoaded {
            has_video: true,
            has_audio: true,
            video_size: None,
        });
        assert_eq!(machine.state().status, Status::Loading);
        assert_eq!(machine.state().position_ms, 0);
        machine.expect(2);
        // An entry nobody asked for (an aborted earlier open) changes nothing either.
        machine.apply(Event::StartFile { entry: 7 });
        machine.apply(Event::FileLoaded {
            has_video: false,
            has_audio: true,
            video_size: None,
        });
        assert_eq!(machine.state().status, Status::Loading);
        machine.apply(Event::StartFile { entry: 2 });
        machine.apply(Event::FileLoaded {
            has_video: false,
            has_audio: true,
            video_size: None,
        });
        let state = machine.state();
        assert_eq!(
            (state.status, state.file.as_deref(), state.has_video),
            (
                Status::Playing,
                Some(std::path::Path::new("/media/b.mp4")),
                false
            )
        );
    }

    #[test]
    fn a_broken_file_fails_with_the_reason_and_the_next_open_starts_clean() {
        let now = Instant::now();
        let mut machine = Machine::new();
        machine.opening(PathBuf::from("/media/bad.mp4"), 0, true, now);
        machine.expect(3);
        machine.apply(Event::StartFile { entry: 3 });
        machine.apply(Event::ErrorLog(
            "Failed to recognize file format.".to_owned(),
        ));
        machine.apply(Event::EndFile {
            entry: 3,
            reason: EndReason::Error,
            error: -17,
        });
        let state = machine.state().clone();
        assert_eq!(state.status, Status::Failed);
        let problem = state.problem.unwrap();
        assert_eq!((problem.kind, problem.retry), (Kind::Unavailable, false));
        assert!(problem.detail.contains("recognize"), "{problem:?}");
        // Late events of the failed file change nothing.
        machine.apply(Event::Pause(false));
        machine.apply(Event::Position(Some(1.0)));
        assert_eq!(machine.state().status, Status::Failed);
        loaded(&mut machine, now);
        assert_eq!(machine.state().status, Status::Playing);
        assert_eq!(machine.state().problem, None);
    }

    #[test]
    fn stop_and_pause() {
        let now = Instant::now();
        let mut machine = Machine::new();
        loaded(&mut machine, now);
        machine.apply(Event::Pause(true));
        assert_eq!(machine.state().status, Status::Paused);
        machine.apply(Event::Pause(false));
        assert_eq!(machine.state().status, Status::Playing);
        machine.apply(Event::EndFile {
            entry: 1,
            reason: EndReason::Stop,
            error: 0,
        });
        let state = machine.state();
        assert_eq!(
            (state.status, state.file.clone(), state.position_ms),
            (Status::Idle, None, 0)
        );
        // A paused open: Paused once loaded, at its start position.
        machine.opening(PathBuf::from("/media/c.mp4"), 12_000, false, now);
        machine.expect(4);
        machine.apply(Event::StartFile { entry: 4 });
        machine.apply(Event::FileLoaded {
            has_video: true,
            has_audio: false,
            video_size: None,
        });
        assert_eq!(
            (machine.state().status, machine.state().position_ms),
            (Status::Paused, 12_000)
        );
    }

    #[test]
    fn a_file_that_never_opens_fails_after_the_limit() {
        let start = Instant::now();
        let limit = Duration::from_secs(20);
        let mut machine = Machine::new();
        machine.opening(PathBuf::from("/media/slow.mkv"), 0, true, start);
        assert!(!machine.check_loading(start + Duration::from_secs(19), limit));
        assert_eq!(
            machine
                .next_wake(start + Duration::from_secs(19), limit)
                .map(|wait| wait.as_secs()),
            Some(1)
        );
        assert!(machine.check_loading(start + Duration::from_secs(21), limit));
        let state = machine.state().clone();
        assert_eq!(state.status, Status::Failed);
        assert_eq!(state.problem.unwrap().kind, Kind::Unavailable);
        assert!(
            !machine.check_loading(start + Duration::from_secs(60), limit),
            "fails once"
        );
        // A file that opened in time never times out.
        loaded(&mut machine, start);
        assert!(!machine.check_loading(start + Duration::from_secs(600), limit));
    }

    #[test]
    fn positions_are_throttled_but_other_changes_go_at_once() {
        let start = Instant::now();
        let mut machine = Machine::new();
        loaded(&mut machine, start);
        assert_eq!(machine.take_due(start).unwrap().status, Status::Playing);
        assert!(machine.take_due(start).is_none(), "nothing new");
        machine.apply(Event::Position(Some(0.1)));
        assert!(
            machine
                .take_due(start + Duration::from_millis(100))
                .is_none()
        );
        let wait = machine.next_wake(start + Duration::from_millis(100), Duration::from_secs(20));
        assert_eq!(wait, Some(Duration::from_millis(150)));
        machine.apply(Event::Position(Some(0.2)));
        let state = machine
            .take_due(start + Duration::from_millis(250))
            .unwrap();
        assert_eq!(state.position_ms, 200, "the newest position");
        machine.apply(Event::Pause(true));
        let state = machine
            .take_due(start + Duration::from_millis(260))
            .unwrap();
        assert_eq!(
            state.status,
            Status::Paused,
            "status changes are not throttled"
        );
    }

    #[test]
    fn mpv_errors_become_problems() {
        assert_eq!(problem_for(-13, None).kind, Kind::Unavailable);
        assert_eq!(problem_for(-16, None).kind, Kind::Unavailable);
        assert_eq!(problem_for(-14, None).kind, Kind::Setup);
        assert!(problem_for(-14, None).retry);
        let bug = problem_for(-20, Some("odd".to_owned()));
        assert_eq!(
            (bug.kind, bug.detail.as_str()),
            (Kind::Bug, "mpv error -20: odd")
        );
    }
}
