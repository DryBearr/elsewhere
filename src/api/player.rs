//! The media engine (src/player) for the apps: one `CorePlayer` per thing that plays.
//! It opens files inside the app's media folder only (set at `start`). Video output and
//! the lock screen come later, from the apps' side.

use std::sync::Arc;

use super::{ApiError, Problem};
use crate::player::{self, Player};

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum PlayerStatus {
    /// Nothing open.
    Idle,
    Loading,
    Playing,
    Paused,
    /// At the end (play starts it over).
    Ended,
    /// The file could not be played: see `problem`.
    Failed,
}

/// Everything about a player. Replace the shown state with a newer revision.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct PlayerState {
    pub revision: i64,
    pub status: PlayerStatus,
    pub file: Option<String>,
    pub position_ms: i64,
    pub duration_ms: Option<i64>,
    pub speed: f64,
    /// 0 to 100.
    pub volume: f64,
    pub has_video: bool,
    pub has_audio: bool,
    /// The picture's width and height as shown (rotation applied), for a video: the
    /// shape of the video's frame on screen.
    pub video_width: Option<u32>,
    pub video_height: Option<u32>,
    /// A picture shows: video is attached and a video plays.
    pub video_shown: bool,
    pub problem: Option<Problem>,
}

/// Gets the player's state at once, then every change (position at most 4 times a
/// second), on the player's own thread: hand it to the UI thread and return.
#[uniffi::export(callback_interface)]
pub trait PlayerListener: Send + Sync {
    fn on_state(&self, state: PlayerState);
}

#[derive(uniffi::Object)]
pub struct CorePlayer {
    player: Player,
}

#[uniffi::export]
impl CorePlayer {
    /// A new player. The first one loads the media engine.
    #[uniffi::constructor]
    pub fn new() -> Result<Arc<CorePlayer>, ApiError> {
        Ok(Arc::new(CorePlayer {
            player: Player::new()?,
        }))
    }

    /// Opens a file in the app's media folder at `start_ms`, playing or paused.
    pub fn open(&self, path: String, start_ms: i64, autoplay: bool) -> Result<(), ApiError> {
        Ok(self.player.open(&path, start_ms.max(0) as u64, autoplay)?)
    }

    pub fn play(&self) -> Result<(), ApiError> {
        Ok(self.player.play()?)
    }

    pub fn pause(&self) -> Result<(), ApiError> {
        Ok(self.player.pause()?)
    }

    pub fn seek(&self, position_ms: i64) -> Result<(), ApiError> {
        Ok(self.player.seek(position_ms)?)
    }

    /// 0.25 to 4 (1 is normal).
    pub fn set_speed(&self, speed: f64) -> Result<(), ApiError> {
        Ok(self.player.set_speed(speed)?)
    }

    /// 0 to 100.
    pub fn set_volume(&self, volume: f64) -> Result<(), ApiError> {
        Ok(self.player.set_volume(volume)?)
    }

    pub fn stop(&self) -> Result<(), ApiError> {
        Ok(self.player.stop()?)
    }

    pub fn state(&self) -> PlayerState {
        self.player.state().into()
    }

    pub fn set_listener(&self, listener: Box<dyn PlayerListener>) {
        self.player
            .set_listener(move |state| listener.on_state(state.into()));
    }
}

impl From<player::PlayerState> for PlayerState {
    fn from(state: player::PlayerState) -> Self {
        let signed = |value: u64| value.min(i64::MAX as u64) as i64;
        PlayerState {
            revision: signed(state.revision),
            status: match state.status {
                player::Status::Idle => PlayerStatus::Idle,
                player::Status::Loading => PlayerStatus::Loading,
                player::Status::Playing => PlayerStatus::Playing,
                player::Status::Paused => PlayerStatus::Paused,
                player::Status::Ended => PlayerStatus::Ended,
                player::Status::Failed => PlayerStatus::Failed,
            },
            file: state.file.map(|file| file.to_string_lossy().into_owned()),
            position_ms: signed(state.position_ms),
            duration_ms: state.duration_ms.map(signed),
            speed: state.speed,
            volume: state.volume,
            has_video: state.has_video,
            has_audio: state.has_audio,
            video_width: state.video_size.map(|size| size.0),
            video_height: state.video_size.map(|size| size.1),
            video_shown: state.video_shown,
            problem: state.problem.map(Problem::from),
        }
    }
}
