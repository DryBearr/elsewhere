//! Elsewhere's shared core. The Android and iOS apps call it only through `api`, whose
//! Kotlin and Swift bindings UniFFI generates. Each module plugs in with one `mod` line
//! (and a feature flag where it carries weight, like ytdlp).

uniffi::setup_scaffolding!();

pub mod api;
mod db;
#[cfg(feature = "ytdlp")]
mod phone;
#[cfg(feature = "playlists")]
pub mod playlists;
#[cfg(feature = "ytdlp")]
pub mod ytdlp;
