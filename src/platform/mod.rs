//! The only part of yt-dlp support that differs per platform: where Python lives and the
//! environment it needs. Every platform file has the same three functions, and the rest of
//! the app calls them through this module:
//!
//! - `set_env()` sets the environment variables Python reads when it starts. Call it first
//!   thing at startup, before other threads exist.
//! - `python_setup()` finds Python and yt-dlp (Android unpacks them first) and returns the
//!   setup for `ytdlp::init`. It can be slow, so call it on a worker thread.
//! - `log(message)` writes a line where developers can see it: logcat (tag Elsewhere) on
//!   Android, stderr elsewhere.
//!
//! Android also has `keep_app()`, which hands over the app handle before `set_env()`.

#[cfg(target_os = "android")]
mod android;
#[cfg(target_os = "android")]
pub use android::*;

#[cfg(target_os = "ios")]
mod ios;
#[cfg(target_os = "ios")]
pub use ios::*;

#[cfg(not(any(target_os = "android", target_os = "ios")))]
mod desktop;
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub use desktop::*;
