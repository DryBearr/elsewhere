//! The playlist player (src/queue) for the apps: one `CoreQueue` plays library items one
//! after the other, with next and previous, repeat, shuffle and editing. It works with
//! item ids; files and where to start come from the library, which also keeps progress.
//!
//! Commands answer once they are done, and their result is in `state()` by then. The
//! listener runs on the queue's own thread: it must hand states to the UI and return;
//! a command sent from inside it is refused at once (it would wait for itself).
//! Close a queue (Kotlin `close`, Swift: let it go) off the main thread: it waits up to
//! 2 s for the last progress to be saved.

use std::sync::Arc;

use super::{ApiError, PlayerState, Problem, bad_args, core};
use crate::queue::{self, Command, QueuePlayer, Repeat};

/// Entries `play_collection` loads at most.
const MAX_COLLECTION: usize = 50_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum RepeatMode {
    Off,
    /// The queue starts over after its end.
    All,
    /// The current entry plays again when it ends (next still moves on).
    One,
}

/// One queued entry: `key` is its own (the same item can be queued twice).
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct QueueEntry {
    pub key: i64,
    pub item_id: i64,
}

/// An entry that could not be played, with why.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct SkippedEntry {
    pub key: i64,
    pub item_id: i64,
    pub problem: Problem,
}

/// Everything about the queue. Replace what is shown with a newer revision.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct QueueState {
    pub revision: i64,
    pub entries: Vec<QueueEntry>,
    /// Index into `entries` of the current one.
    pub current: Option<u32>,
    pub repeat: RepeatMode,
    pub shuffle: bool,
    /// The engine's state for the current entry (position, duration, status).
    pub playback: PlayerState,
    /// The last entry ended and nothing comes after it.
    pub finished: bool,
    /// Why an entry was skipped or the queue stopped.
    pub problem: Option<Problem>,
    /// Entries skipped since the queue was set (the newest 100).
    pub skipped: Vec<SkippedEntry>,
    /// Whether playback should be going (also while a file opens): the play/pause button.
    pub wants_play: bool,
    /// The current entry, for the lock screen and the notification.
    pub now_playing: Option<NowPlaying>,
    /// Whether next has somewhere to go: show or hide the button.
    pub has_next: bool,
    /// Whether previous goes to another entry (else it starts the current one over).
    pub has_previous: bool,
}

/// The current entry as a lock screen shows it.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct NowPlaying {
    pub item_id: i64,
    pub title: String,
    pub artist: Option<String>,
    pub album: Option<String>,
    /// An image file inside the media folder.
    pub cover: Option<String>,
    pub media: super::MediaKind,
}

/// Gets the queue's state at once, then every change, on the queue's thread: hand it to
/// the UI thread and return. Do not call the queue from here.
#[uniffi::export(callback_interface)]
pub trait QueueListener: Send + Sync {
    fn on_state(&self, state: QueueState);
}

#[derive(uniffi::Object)]
pub struct CoreQueue {
    queue: QueuePlayer,
}

fn key(key: i64) -> Result<u64, ApiError> {
    u64::try_from(key).map_err(|_| bad_args("That entry is not in the queue."))
}

#[uniffi::export]
impl CoreQueue {
    /// A playlist player with its own engine. The first one loads the media engine.
    #[uniffi::constructor]
    pub fn new() -> Result<Arc<CoreQueue>, ApiError> {
        let library = &core()?.library;
        Ok(Arc::new(CoreQueue {
            queue: QueuePlayer::new(library)?,
        }))
    }

    /// Replaces the queue with `items` (item ids) and starts at index `start`.
    pub fn play(&self, items: Vec<i64>, start: u32, autoplay: bool) -> Result<(), ApiError> {
        Ok(self.queue.command(Command::Play {
            items,
            start: start as usize,
            autoplay,
        })?)
    }

    /// Plays a saved playlist from its first entry, or from `start_entry` (an entry id from
    /// `collection_items`). Entries whose files are gone are queued too and skipped.
    pub fn play_collection(
        &self,
        collection_id: i64,
        start_entry: Option<i64>,
        autoplay: bool,
    ) -> Result<(), ApiError> {
        let library = &core()?.library;
        let mut items = Vec::new();
        let mut start = None;
        let mut after: Option<String> = None;
        loop {
            let page = library.collection_items(
                collection_id,
                after.as_deref(),
                crate::library::MAX_PAGE,
            )?;
            for entry in &page.rows {
                if start.is_none() && Some(entry.entry_id) == start_entry {
                    start = Some(items.len());
                }
                items.push(entry.item.id);
            }
            if items.len() > MAX_COLLECTION {
                return Err(bad_args(format!(
                    "This playlist has more than {MAX_COLLECTION} entries; play a part of it."
                )));
            }
            match page.next {
                Some(next) => after = Some(next),
                None => break,
            }
        }
        if items.is_empty() {
            return Err(bad_args("This playlist is empty."));
        }
        if start_entry.is_some() && start.is_none() {
            return Err(bad_args("That entry is not in this playlist."));
        }
        let start = start.unwrap_or(0);
        self.play(items, start as u32, autoplay)
    }

    /// Plays; after the end, from the top.
    pub fn resume(&self) -> Result<(), ApiError> {
        Ok(self.queue.command(Command::Resume)?)
    }

    pub fn pause(&self) -> Result<(), ApiError> {
        Ok(self.queue.command(Command::Pause)?)
    }

    pub fn next(&self) -> Result<(), ApiError> {
        Ok(self.queue.command(Command::Next)?)
    }

    /// Starts the entry over; within its first 3 s, goes to the one before.
    pub fn previous(&self) -> Result<(), ApiError> {
        Ok(self.queue.command(Command::Previous)?)
    }

    pub fn seek(&self, position_ms: i64) -> Result<(), ApiError> {
        Ok(self.queue.command(Command::Seek(position_ms))?)
    }

    /// Plays the entry with this key.
    pub fn jump(&self, entry_key: i64) -> Result<(), ApiError> {
        Ok(self.queue.command(Command::Jump(key(entry_key)?))?)
    }

    pub fn set_repeat(&self, repeat: RepeatMode) -> Result<(), ApiError> {
        Ok(self.queue.command(Command::SetRepeat(match repeat {
            RepeatMode::Off => Repeat::Off,
            RepeatMode::All => Repeat::All,
            RepeatMode::One => Repeat::One,
        }))?)
    }

    pub fn set_shuffle(&self, shuffle: bool) -> Result<(), ApiError> {
        Ok(self.queue.command(Command::SetShuffle(shuffle))?)
    }

    /// 0.25 to 4 (1 is normal).
    pub fn set_speed(&self, speed: f64) -> Result<(), ApiError> {
        Ok(self.queue.command(Command::SetSpeed(speed))?)
    }

    /// 0 to 100 (for example lower while a navigation voice speaks).
    pub fn set_volume(&self, volume: f64) -> Result<(), ApiError> {
        Ok(self.queue.command(Command::SetVolume(volume))?)
    }

    /// Adds item ids at the end (shuffled: among the entries still to come).
    pub fn add(&self, items: Vec<i64>) -> Result<(), ApiError> {
        Ok(self.queue.command(Command::Add(items))?)
    }

    pub fn remove(&self, entry_key: i64) -> Result<(), ApiError> {
        Ok(self.queue.command(Command::Remove(key(entry_key)?))?)
    }

    /// Moves an entry to place `to` in the list (what plays goes on playing).
    pub fn move_entry(&self, entry_key: i64, to: u32) -> Result<(), ApiError> {
        Ok(self.queue.command(Command::Move {
            key: key(entry_key)?,
            to: to as usize,
        })?)
    }

    /// Ends playback and empties the queue.
    pub fn stop(&self) -> Result<(), ApiError> {
        Ok(self.queue.command(Command::Stop)?)
    }

    pub fn state(&self) -> QueueState {
        self.queue.state().into()
    }

    pub fn set_listener(&self, listener: Box<dyn QueueListener>) {
        self.queue
            .set_listener(move |state| listener.on_state(state.into()));
    }
}

impl From<queue::QueueState> for QueueState {
    fn from(state: queue::QueueState) -> Self {
        let signed = |value: u64| value.min(i64::MAX as u64) as i64;
        QueueState {
            revision: signed(state.revision),
            entries: state
                .entries
                .iter()
                .map(|slot| QueueEntry {
                    key: signed(slot.key),
                    item_id: slot.item_id,
                })
                .collect(),
            current: state
                .current
                .map(|index| index.min(u32::MAX as usize) as u32),
            repeat: match state.repeat {
                Repeat::Off => RepeatMode::Off,
                Repeat::All => RepeatMode::All,
                Repeat::One => RepeatMode::One,
            },
            shuffle: state.shuffle,
            playback: state.playback.into(),
            finished: state.finished,
            problem: state.problem.map(Problem::from),
            skipped: state
                .skipped
                .into_iter()
                .map(|skip| SkippedEntry {
                    key: signed(skip.key),
                    item_id: skip.item_id,
                    problem: skip.problem.into(),
                })
                .collect(),
            wants_play: state.wants_play,
            now_playing: state.now_playing.map(|now| NowPlaying {
                item_id: now.item_id,
                title: now.title,
                artist: now.artist,
                album: now.album,
                cover: now.cover.map(|cover| cover.to_string_lossy().into_owned()),
                media: super::library::media(now.media),
            }),
            has_next: state.has_next,
            has_previous: state.has_previous,
        }
    }
}
