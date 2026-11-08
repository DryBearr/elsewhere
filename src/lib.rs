//! Elsewhere's shared core. The Android and iOS apps call it only through `api`, whose
//! Kotlin and Swift bindings UniFFI generates. Each module plugs in with one `mod` line
//! (and a feature flag where it carries weight, like ytdlp).
//!
//! The core must not panic: errors are values (`problem::Problem`). Outside tests, clippy
//! refuses unwrap, expect and panic; whatever panics anyway is caught at every boundary and
//! logged (see `health` and `logs`).
#![cfg_attr(
    not(test),
    deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)
)]

uniffi::setup_scaffolding!();

pub mod api;
mod db;
pub mod health;
pub mod jobs;
pub mod logs;
#[cfg(feature = "ytdlp")]
mod phone;
#[cfg(feature = "player")]
pub mod player;
#[cfg(feature = "playlists")]
pub mod playlists;
pub mod problem;
#[cfg(feature = "ytdlp")]
pub mod ytdlp;
