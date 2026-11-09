//! The playlist player (feature `player`): a queue of library items played one after the
//! other through the media engine, with next and previous, repeat, shuffle and editing.
//! It works with item ids; the library gives the files and where to start (songs from
//! the top, videos from their resume point) and keeps the progress the player reports.
//!
//! One thread owns the queue and the engine. App commands and engine states go through
//! one channel and are handled one after the other, so nothing happens out of order and
//! nothing needs locks of its own; the thread runs under `health::supervise`. The state
//! is whole and numbered (like jobs): the listener gets every change.
//!
//! Robust on purpose:
//! - An entry that cannot be played (file gone, broken file) is skipped and its problem
//!   shown; when nothing in the queue can be played, it stops with a Problem instead of
//!   going round forever.
//! - Late engine states of the previous file are ignored (each open has a newer revision).
//! - Progress is saved on pause, every 10 s while playing, when an entry ends or is left,
//!   and on stop.

mod order;

use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use order::Order;
pub use order::{Repeat, Slot};

use crate::library::Library;
use crate::player::{Player, PlayerState, Status};
use crate::problem::{Kind, Problem};
use crate::{health, logs};

/// Progress is saved at least this often while playing (milliseconds; see
/// `set_save_interval`).
static SAVE_EVERY_MS: AtomicU64 = AtomicU64::new(10_000);

/// How often progress is saved while playing (10 s). For tests.
pub fn set_save_interval(interval: Duration) {
    SAVE_EVERY_MS.store(
        interval.as_millis().min(u64::MAX as u128) as u64,
        Ordering::SeqCst,
    );
}

fn save_every() -> Duration {
    Duration::from_millis(SAVE_EVERY_MS.load(Ordering::SeqCst))
}
/// Previous this far into an entry starts it over; earlier it goes to the one before.
const RESTART_AFTER_MS: u64 = 3_000;
/// How long a command may wait for the player's thread.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);

/// Everything about the queue, whole: replace what is shown with a newer revision.
#[derive(Debug, Clone, PartialEq)]
pub struct QueueState {
    pub revision: u64,
    pub entries: Vec<Slot>,
    /// Goes up only when `entries` change (added, removed, moved, a new queue), so a list
    /// of thousands is sent to the apps only then.
    pub entries_revision: u64,
    /// Index into `entries` of the current one.
    pub current: Option<usize>,
    pub repeat: Repeat,
    pub shuffle: bool,
    /// The engine's state for the current entry.
    pub playback: PlayerState,
    /// The last entry ended and nothing comes after it.
    pub finished: bool,
    /// Why an entry was skipped or the queue stopped.
    pub problem: Option<Problem>,
    /// The entries skipped since the queue was set, with why (the newest 100), so a list
    /// can mark them.
    pub skipped: Vec<Skipped>,
    /// Whether playback should be going (also while a file opens): what a lock screen's
    /// play/pause button shows.
    pub wants_play: bool,
    /// What the current entry is, for lock screens and notifications.
    pub now_playing: Option<NowPlaying>,
    /// Whether next has somewhere to go (a later entry, or round again with repeat).
    pub has_next: bool,
    /// Whether previous can go to another entry (else it starts the current one over).
    pub has_previous: bool,
}

/// The current entry as a lock screen shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct NowPlaying {
    pub item_id: i64,
    pub title: String,
    pub artist: Option<String>,
    pub album: Option<String>,
    /// An image file inside the media folder.
    pub cover: Option<std::path::PathBuf>,
    pub media: crate::library::Media,
}

/// An entry that could not be played.
#[derive(Debug, Clone, PartialEq)]
pub struct Skipped {
    pub key: u64,
    pub item_id: i64,
    pub problem: Problem,
}

/// Skipped entries kept in the state, at most.
const SKIPPED_KEPT: usize = 100;

/// What the app can ask for.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// Replaces the queue with `items` and starts at index `start`.
    Play {
        items: Vec<i64>,
        start: usize,
        autoplay: bool,
    },
    /// Plays (after the end: from the top).
    Resume,
    Pause,
    Next,
    /// Starts the entry over, or this early in it, goes to the one before.
    Previous,
    /// Goes to the entry before, however far into this one (for system controls that make
    /// the start-over decision themselves).
    PreviousEntry,
    Seek(i64),
    /// Makes the entry with this key current.
    Jump(u64),
    SetRepeat(Repeat),
    SetShuffle(bool),
    SetSpeed(f64),
    /// 0 to 100 (for example lower while a navigation voice speaks).
    SetVolume(f64),
    /// Builds the sound output again (iOS after its media services were reset).
    ReloadAudio,
    Add(Vec<i64>),
    Remove(u64),
    Move {
        key: u64,
        to: usize,
    },
    /// Plays the entry with this key right after the current one (shuffled too).
    PlayNext(u64),
    /// Ends playback and empties the queue.
    Stop,
}

type Listener = Arc<dyn Fn(QueueState) + Send + Sync>;

enum Message {
    Command(Command, SyncSender<Result<(), Problem>>),
    Engine(PlayerState),
    /// Saves the last progress, answers, and ends the thread.
    Close(SyncSender<()>),
}

struct Shared {
    state: Mutex<QueueState>,
    listener: Mutex<Option<Listener>>,
}

/// The playlist player. Drop it to stop playback and free the engine.
pub struct QueuePlayer {
    messages: Sender<Message>,
    shared: Arc<Shared>,
    video: crate::player::Video,
}

impl QueuePlayer {
    /// A new playlist player with its own engine, playing items of `library`.
    pub fn new(library: &'static Library) -> Result<QueuePlayer, Problem> {
        let player = Player::new()?;
        let video = player.video();
        let (messages, received) = mpsc::channel();
        let engine = messages.clone();
        player.set_listener(move |state| {
            let _ = engine.send(Message::Engine(state));
        });
        let shared = Arc::new(Shared {
            state: Mutex::new(QueueState {
                revision: 1,
                entries: Vec::new(),
                entries_revision: 1,
                current: None,
                repeat: Repeat::Off,
                shuffle: false,
                playback: player.state(),
                finished: false,
                problem: None,
                skipped: Vec::new(),
                wants_play: false,
                now_playing: None,
                has_next: false,
                has_previous: false,
            }),
            listener: Mutex::new(None),
        });
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(1, |time| time.as_nanos() as u64);
        let actor = Arc::new(Mutex::new(Actor {
            player,
            order: Order::new(seed),
            library,
            playing: None,
            wants_play: false,
            fresh_from: 0,
            failures: 0,
            finished: false,
            problem: None,
            skipped: Vec::new(),
            now: None,
        }));
        let received = Arc::new(Mutex::new(received));
        let thread_shared = Arc::clone(&shared);
        static QUEUES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(1);
        let name = format!(
            "queue-{}",
            QUEUES.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        );
        health::supervise(name, move || run(&actor, &received, &thread_shared))
            .map_err(|err| Problem::bug(format!("could not start the queue thread: {err}")))?;
        Ok(QueuePlayer {
            messages,
            shared,
            video,
        })
    }

    /// The engine's video: attach a screen to it to see the picture (player/video.rs).
    /// It goes straight to the engine, not through the queue's thread: frames are drawn
    /// many times a second.
    pub fn video(&self) -> &crate::player::Video {
        &self.video
    }

    /// Runs a command on the player's thread and returns how it went.
    pub fn command(&self, command: Command) -> Result<(), Problem> {
        if ON_QUEUE_THREAD.with(Cell::get) {
            // The thread would wait for itself: a listener must hand commands elsewhere.
            return Err(Problem::bug(
                "a queue command was sent from the queue's own thread (its listener)",
            ));
        }
        let (reply, answer) = mpsc::sync_channel(1);
        self.messages
            .send(Message::Command(command, reply))
            .map_err(|_| Problem::bug("the queue thread is gone"))?;
        match answer.recv_timeout(COMMAND_TIMEOUT) {
            Ok(result) => result,
            Err(RecvTimeoutError::Timeout) => {
                Err(Problem::bug("the queue thread did not answer in 30 s"))
            }
            Err(RecvTimeoutError::Disconnected) => Err(Problem::bug("the queue thread stopped")),
        }
    }

    pub fn state(&self) -> QueueState {
        lock(&self.shared.state).clone()
    }

    /// Sets the one listener; it gets the current state at once, then every change, on
    /// the player's thread: keep it short.
    pub fn set_listener(&self, listener: impl Fn(QueueState) + Send + Sync + 'static) {
        let listener: Listener = Arc::new(listener);
        *lock(&self.shared.listener) = Some(Arc::clone(&listener));
        listener(self.state());
    }
}

/// How long dropping a queue waits for its last save.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(2);

impl Drop for QueuePlayer {
    /// Waits (at most 2 s) until the last progress is saved, so closing the app right after
    /// loses nothing. Dropped on its own thread (by its listener), it cannot wait.
    fn drop(&mut self) {
        let (done, saved) = mpsc::sync_channel(1);
        if self.messages.send(Message::Close(done)).is_ok() && !ON_QUEUE_THREAD.with(Cell::get) {
            let _ = saved.recv_timeout(CLOSE_TIMEOUT);
        }
    }
}

thread_local! {
    /// True on a queue's own thread.
    static ON_QUEUE_THREAD: Cell<bool> = const { Cell::new(false) };
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// What is playing now.
struct Playing {
    slot: Slot,
    saved_at: Instant,
}

struct Actor {
    player: Player,
    order: Order,
    library: &'static Library,
    playing: Option<Playing>,
    /// Whether playback should go on by itself (paused stays paused across next/previous).
    wants_play: bool,
    /// Engine states older than this belong to an earlier file.
    fresh_from: u64,
    /// Entries skipped in a row because they could not be played.
    failures: usize,
    finished: bool,
    problem: Option<Problem>,
    skipped: Vec<Skipped>,
    /// The current entry, as a lock screen shows it.
    now: Option<NowPlaying>,
}

/// The player's thread: one message at a time, then the new state goes out.
fn run(actor: &Mutex<Actor>, received: &Mutex<Receiver<Message>>, shared: &Shared) {
    ON_QUEUE_THREAD.with(|flag| flag.set(true));
    let received = lock(received);
    while let Ok(message) = received.recv() {
        let mut actor = lock(actor);
        match message {
            Message::Close(done) => {
                actor.save_progress();
                let _ = done.send(());
                return;
            }
            Message::Command(command, reply) => {
                let result = actor.handle(command);
                // The new state goes out before the answer: whoever sent the command sees
                // its result in `state()` as soon as `command` returns.
                let state = actor.snapshot();
                drop(actor);
                publish(shared, state);
                let _ = reply.send(result);
            }
            Message::Engine(state) => {
                actor.on_engine(state);
                let state = actor.snapshot();
                drop(actor);
                publish(shared, state);
            }
        }
    }
}

fn publish(shared: &Shared, mut state: QueueState) {
    {
        let mut shown = lock(&shared.state);
        state.revision = shown.revision;
        state.entries_revision = shown.entries_revision;
        if state == *shown {
            return;
        }
        if state.entries != shown.entries {
            state.entries_revision += 1;
        }
        state.revision = shown.revision + 1;
        *shown = state.clone();
    }
    let listener = lock(&shared.listener).clone();
    if let Some(listener) = listener
        && std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| listener(state))).is_err()
    {
        logs::error("queue", "The queue listener panicked").write();
    }
}

fn refused(text: &str, detail: String) -> Problem {
    Problem {
        retry: false,
        ..Problem::new(Kind::Input, text, detail)
    }
}

impl Actor {
    fn snapshot(&self) -> QueueState {
        QueueState {
            revision: 0,
            entries: self.order.slots().to_vec(),
            entries_revision: 0,
            current: self.order.current_index(),
            repeat: self.order.repeat,
            shuffle: self.order.shuffle(),
            playback: self.player.state(),
            finished: self.finished,
            problem: self.problem.clone(),
            skipped: self.skipped.clone(),
            wants_play: self.wants_play && !self.finished && self.order.current().is_some(),
            now_playing: self.now.clone(),
            has_next: !self.finished && self.order.has_next(),
            has_previous: self.order.has_previous(),
        }
    }

    fn handle(&mut self, command: Command) -> Result<(), Problem> {
        match command {
            Command::Play {
                items,
                start,
                autoplay,
            } => {
                if items.is_empty() {
                    return Err(refused(
                        "There is nothing to play.",
                        "an empty queue".to_owned(),
                    ));
                }
                if start >= items.len() {
                    return Err(refused(
                        "That entry is not in the queue.",
                        format!("start {start} of {}", items.len()),
                    ));
                }
                self.leave_current();
                self.now = None;
                self.order.set(&items, start);
                self.wants_play = autoplay;
                self.problem = None;
                self.skipped.clear();
                self.failures = 0;
                self.open_current();
                Ok(())
            }
            Command::Resume => {
                if self.order.current().is_none() {
                    return Err(nothing_queued());
                }
                self.wants_play = true;
                if self.finished || self.playing.is_none() {
                    // After the end: from the top.
                    if self.finished {
                        self.order.restart();
                    }
                    self.failures = 0;
                    self.open_current();
                    Ok(())
                } else {
                    self.player.play()
                }
            }
            Command::Pause => {
                self.wants_play = false;
                if self.playing.is_none() {
                    return Ok(());
                }
                self.player.pause()?;
                self.save_progress();
                Ok(())
            }
            Command::Next => {
                if self.order.current().is_none() {
                    return Err(nothing_queued());
                }
                self.leave_current();
                self.failures = 0;
                self.advance(false);
                Ok(())
            }
            Command::Previous => {
                if self.order.current().is_none() {
                    return Err(nothing_queued());
                }
                let position = self.player.state().position_ms;
                if self.playing.is_some() && position > RESTART_AFTER_MS {
                    return self.player.seek(0);
                }
                self.leave_current();
                self.order.previous();
                self.failures = 0;
                self.open_current();
                Ok(())
            }
            Command::PreviousEntry => {
                if self.order.current().is_none() {
                    return Err(nothing_queued());
                }
                self.leave_current();
                self.order.previous();
                self.failures = 0;
                self.open_current();
                Ok(())
            }
            Command::Seek(position) => {
                if self.playing.is_none() {
                    return Err(nothing_queued());
                }
                self.player.seek(position)
            }
            Command::Jump(key) => {
                if !self.order.slots().iter().any(|slot| slot.key == key) {
                    return Err(no_entry(key));
                }
                self.leave_current();
                self.order.jump(key);
                self.failures = 0;
                self.open_current();
                Ok(())
            }
            Command::SetRepeat(repeat) => {
                self.order.repeat = repeat;
                Ok(())
            }
            Command::SetShuffle(on) => {
                self.order.set_shuffle(on);
                Ok(())
            }
            Command::SetSpeed(speed) => self.player.set_speed(speed),
            Command::SetVolume(volume) => self.player.set_volume(volume),
            Command::ReloadAudio => self.player.reload_audio(),
            Command::Add(items) => {
                if items.is_empty() {
                    return Ok(());
                }
                let was_empty = self.order.current().is_none();
                let keys = self.order.add(&items);
                if was_empty || self.finished {
                    // Nothing was playing, or the queue had ended: the first new entry plays.
                    self.order.jump(keys[0]);
                    self.failures = 0;
                    self.open_current();
                }
                Ok(())
            }
            Command::Remove(key) => {
                let removes_current = self.order.current().is_some_and(|slot| slot.key == key);
                if removes_current {
                    self.leave_current();
                }
                let (found, _) = self.order.remove(key);
                if !found {
                    return Err(no_entry(key));
                }
                if removes_current {
                    if self.order.current().is_some() {
                        self.failures = 0;
                        self.open_current();
                    } else {
                        self.stop_engine();
                        self.finished = true;
                    }
                }
                Ok(())
            }
            Command::Move { key, to } => {
                if self.order.move_to(key, to) {
                    Ok(())
                } else {
                    Err(no_entry(key))
                }
            }
            Command::PlayNext(key) => {
                if !self.order.play_next(key) {
                    return Err(no_entry(key));
                }
                if self.finished && self.order.current().is_some_and(|slot| slot.key != key) {
                    // The queue had ended: what was asked for comes now (as Add does).
                    self.order.jump(key);
                    self.failures = 0;
                    self.open_current();
                }
                Ok(())
            }
            Command::Stop => {
                self.leave_current();
                self.stop_engine();
                self.order.clear();
                self.now = None;
                self.finished = false;
                self.problem = None;
                self.skipped.clear();
                Ok(())
            }
        }
    }

    fn on_engine(&mut self, state: PlayerState) {
        if state.revision < self.fresh_from || self.playing.is_none() {
            return;
        }
        match state.status {
            Status::Playing => {
                self.failures = 0;
                let due = self
                    .playing
                    .as_ref()
                    .is_some_and(|playing| playing.saved_at.elapsed() >= save_every());
                if due {
                    self.save_state(&state);
                }
            }
            Status::Paused => {
                // Opened (paused): not a failure in a row any more.
                self.failures = 0;
                self.save_state(&state);
            }
            Status::Ended => {
                self.save_state(&state);
                self.playing = None;
                self.wants_play = true;
                self.advance(true);
            }
            Status::Failed => {
                let problem = state
                    .problem
                    .clone()
                    .unwrap_or_else(|| Problem::bug("the player failed without saying why"));
                let slot = self.playing.take().map(|playing| playing.slot);
                self.skip(slot, problem);
            }
            Status::Idle | Status::Loading => {}
        }
    }

    /// Moves on after the current entry ended (`ended`) or was skipped.
    fn advance(&mut self, ended: bool) {
        match self.order.next(ended) {
            Some(_) => self.open_current(),
            None => {
                // Skipped past the last entry: its file must not play on. (One that
                // ended is at its end already, paused there.)
                if !ended {
                    self.stop_engine();
                }
                self.finished = true;
                self.playing = None;
            }
        }
    }

    /// An entry could not be played: say why and go on, unless nothing can be played.
    fn skip(&mut self, slot: Option<Slot>, problem: Problem) {
        if self.record_failure(slot, problem) {
            self.advance(false);
        }
    }

    /// Notes a failed entry. False when everything failed: the queue then stops.
    fn record_failure(&mut self, slot: Option<Slot>, problem: Problem) -> bool {
        if let Some(slot) = slot {
            if self.skipped.len() >= SKIPPED_KEPT {
                self.skipped.remove(0);
            }
            self.skipped.push(Skipped {
                key: slot.key,
                item_id: slot.item_id,
                problem: problem.clone(),
            });
        }
        logs::warn("queue", format!("Skipped an entry: {}", problem.text))
            .detail(problem.detail.clone())
            .write();
        self.problem = Some(problem);
        self.failures += 1;
        if self.failures < self.order.slots().len() {
            return true;
        }
        self.problem = Some(Problem {
            retry: false,
            ..Problem::new(
                Kind::Unavailable,
                "Nothing in this queue can be played.",
                format!("{} entries failed in a row", self.failures),
            )
        });
        self.stop_engine();
        self.finished = true;
        false
    }

    /// Opens the current entry (its file and where to start come from the library). One
    /// that cannot be opened is skipped, in a loop: a long queue of missing files cannot
    /// pile up calls.
    fn open_current(&mut self) {
        loop {
            let Some(slot) = self.order.current() else {
                self.finished = true;
                self.playing = None;
                return;
            };
            self.finished = false;
            match self.try_open(slot) {
                Ok(()) => return,
                Err(problem) => {
                    self.playing = None;
                    if !self.record_failure(Some(slot), problem) {
                        return;
                    }
                    if self.order.next(false).is_none() {
                        self.finished = true;
                        return;
                    }
                }
            }
        }
    }

    fn try_open(&mut self, slot: Slot) -> Result<(), Problem> {
        let file = self.library.item_file(slot.item_id)?;
        let start = self.library.start_play(slot.item_id)?;
        let path = file.to_string_lossy().into_owned();
        self.player
            .open(&path, start.position_ms.max(0) as u64, self.wants_play)?;
        self.fresh_from = self.player.state().revision;
        self.playing = Some(Playing {
            slot,
            saved_at: Instant::now(),
        });
        self.now = Some(now_playing(self.library, slot, &file));
        Ok(())
    }

    /// Saves the progress of the entry being left.
    fn leave_current(&mut self) {
        self.save_progress();
        self.playing = None;
    }

    fn save_progress(&mut self) {
        let state = self.player.state();
        if state.revision >= self.fresh_from {
            self.save_state(&state);
        }
    }

    fn save_state(&mut self, state: &PlayerState) {
        let Some(playing) = self.playing.as_mut() else {
            return;
        };
        if !matches!(
            state.status,
            Status::Playing | Status::Paused | Status::Ended
        ) {
            return;
        }
        playing.saved_at = Instant::now();
        let duration = state.duration_ms.map(|ms| ms.min(i64::MAX as u64) as i64);
        let position = state.position_ms.min(i64::MAX as u64) as i64;
        if let Err(problem) = self
            .library
            .save_progress(playing.slot.item_id, position, duration)
        {
            logs::warn("queue", "Could not save where playback is")
                .detail(problem.detail)
                .write();
        }
    }

    fn stop_engine(&mut self) {
        self.playing = None;
        if let Err(problem) = self.player.stop() {
            logs::warn("queue", "Could not stop the player")
                .detail(problem.detail)
                .write();
        }
    }
}

fn nothing_queued() -> Problem {
    refused("Nothing is queued.", "the queue is empty".to_owned())
}

fn no_entry(key: u64) -> Problem {
    refused(
        "That entry is not in the queue any more.",
        format!("no entry with key {key}"),
    )
}

/// What a lock screen shows for an entry: from the library, or (if its details cannot be
/// read) the file's name.
fn now_playing(library: &Library, slot: Slot, file: &std::path::Path) -> NowPlaying {
    match library.item(slot.item_id) {
        Ok(detail) => NowPlaying {
            item_id: slot.item_id,
            title: detail.summary.title,
            artist: detail.summary.artist,
            album: detail.album,
            cover: detail.summary.cover,
            media: detail.summary.media,
        },
        Err(_) => NowPlaying {
            item_id: slot.item_id,
            title: file.file_stem().map_or_else(
                || "Untitled".to_owned(),
                |stem| stem.to_string_lossy().into_owned(),
            ),
            artist: None,
            album: None,
            cover: None,
            media: crate::library::Media::Audio,
        },
    }
}
