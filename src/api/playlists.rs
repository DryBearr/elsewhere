//! Saving YouTube and YouTube Music playlists (src/playlists) as jobs.

use std::path::{Path, PathBuf};

use super::{ApiError, bad_args, core};
use crate::jobs::{Jobs, Level};
use crate::playlists::{self, Format, Options};

const KIND: &str = "playlist";

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum MediaFormat {
    /// m4a, into the Music tab.
    Audio,
    /// mp4 at most `max_height` pixels tall (None: the best), into the Videos tab.
    Video { max_height: Option<u32> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct PlaylistOptions {
    pub format: MediaFormat,
    /// Look up lyrics on lrclib.net.
    pub lyrics: bool,
    /// Save only the first this many items.
    pub limit: Option<u32>,
}

/// Queues saving the playlist (or single video) behind `link` and returns the job's id.
/// Problems with the link itself (private, removed, not a video) show up in the job.
#[uniffi::export]
pub fn save_playlist(link: String, options: PlaylistOptions) -> Result<i64, ApiError> {
    let link = link.trim();
    check_link(link)?;
    let format = match options.format {
        MediaFormat::Audio => Format::Audio,
        MediaFormat::Video {
            max_height: Some(0),
        } => {
            return Err(bad_args("a video quality must be above 0 pixels"));
        }
        MediaFormat::Video { max_height } => Format::Video { max_height },
    };
    if options.limit == Some(0) {
        return Err(bad_args("a limit must be at least 1"));
    }
    let options = Options {
        format,
        lyrics: options.lyrics,
        limit: options.limit.map(|limit| limit as usize),
    };
    let id = core()?
        .jobs
        .start(KIND, link, &playlists::job_params(link, &options))
        .map_err(bad_args)?;
    Ok(id as i64)
}

/// A web link: http(s), with a host.
fn check_link(link: &str) -> Result<(), ApiError> {
    let rest = link
        .strip_prefix("https://")
        .or_else(|| link.strip_prefix("http://"))
        .ok_or_else(|| bad_args("the link must start with https:// or http://"))?;
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    if host.is_empty() || link.chars().any(char::is_whitespace) {
        return Err(bad_args("that is not a link"));
    }
    Ok(())
}

/// At `start`: playlist jobs run with the app's database and media folder, and start
/// yt-dlp first if it is not up yet.
pub(super) fn register(jobs: &Jobs, db: &Path, media: &Path) {
    let (db, media): (PathBuf, PathBuf) = (db.to_owned(), media.to_owned());
    jobs.register(KIND, move |job, params| {
        if !crate::ytdlp::is_started() {
            job.log(Level::Info, "Starting yt-dlp…");
        }
        super::ytdlp::ensure().map_err(super::core_problem)?;
        playlists::run_job(job, &db, &media, params)
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_are_checked() {
        for good in [
            "https://www.youtube.com/playlist?list=PL1",
            "http://music.youtube.com/watch?v=x",
            "https://youtu.be/x",
        ] {
            assert!(check_link(good).is_ok(), "{good}");
        }
        for bad in [
            "",
            "youtube.com/x",
            "ftp://x",
            "https://",
            "https:///x",
            "https://a b",
            "https://?x",
        ] {
            assert!(check_link(bad).is_err(), "{bad}");
        }
    }
}
