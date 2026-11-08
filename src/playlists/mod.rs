//! Saves a YouTube or YouTube Music playlist (or a single video) into the library: each
//! item's media file with its image and lyrics, and the playlist itself (a collection) with
//! its title, banner and order. Feature `playlists`, built on `ytdlp`.
//!
//! Files go in one folder per link:
//!
//! ```text
//! <media>/<Music|Videos>/<title> [<id>]/
//!   cover.jpg                    the playlist's banner
//!   001 - <title> [<id>].m4a     audio (m4a) or video (mp4)
//!   001 - <title> [<id>].jpg     the item's image
//!   001 - <title> [<id>].lrc     synced lyrics (.txt when only plain lyrics exist)
//! ```
//!
//! Saving the same link again downloads only the new items and refreshes the order. An
//! item that fails (private, deleted, blocked) is reported and skipped; the rest go on.

mod lyrics;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};

use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

use crate::jobs::{Job, Level};
use crate::problem::{self, Kind, Problem};
use crate::ytdlp::{self, Cancel, Event};

/// What to save each item as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// m4a: YouTube's own AAC audio, no conversion. Goes to the Music tab.
    Audio,
    /// mp4, at most `max_height` pixels tall (2160, 1440, 1080, 720, 480, 360, 240, 144);
    /// None for the best there is. When a video has no such size, the closest one is taken.
    /// Goes to the Videos tab.
    ///
    /// Picking the quality needs ffmpeg (desktop, Android), because YouTube serves HD video
    /// and audio separately. Without it (iOS) only single-file videos exist, usually 360p.
    /// Items saved before keep the quality they were saved with.
    Video { max_height: Option<u32> },
}

#[derive(Debug, Clone)]
pub struct Options {
    pub format: Format,
    /// Look up lyrics on lrclib.net (sends each item's artist and title there).
    pub lyrics: bool,
    /// Save only the first this many items. None saves them all.
    pub limit: Option<usize>,
}

/// What `save` did.
#[derive(Debug)]
pub struct Saved {
    /// The playlist in the library; None when the link was a single video.
    pub collection_id: Option<i64>,
    pub title: String,
    pub folder: PathBuf,
    pub cover: Option<PathBuf>,
    /// Every item now in the playlist, in order (new and already saved).
    pub items: Vec<SavedItem>,
    pub failed: Vec<Failed>,
    /// Entries skipped because they cannot be saved (removed, private, ...).
    pub unavailable: Vec<Unavailable>,
    /// Items saved earlier that are no longer in the playlist: they left it, but their
    /// files stay in the library.
    pub dropped: usize,
}

#[derive(Debug)]
pub struct SavedItem {
    pub item_id: i64,
    pub position: usize,
    pub title: String,
    pub artist: Option<String>,
    pub file: PathBuf,
    pub image: Option<PathBuf>,
    pub lyrics: Option<PathBuf>,
}

/// A playlist entry that cannot be saved, known from the listing alone (no download is
/// tried): removed, private, members-only, not out yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unavailable {
    /// Where it is in the playlist (1-based, counting every entry).
    pub position: usize,
    pub title: String,
    /// Why, for people.
    pub reason: String,
}

/// What a link holds, before saving it: for showing the size and offering a limit.
#[derive(Debug, Clone, PartialEq)]
pub struct Preview {
    pub title: String,
    pub is_playlist: bool,
    /// Made by YouTube (a Mix, "radio"): it can go on and on, so save a limited number.
    pub generated: bool,
    /// Entries that can be saved: the largest useful limit.
    pub available: usize,
    pub unavailable: Vec<Unavailable>,
    /// False when the playlist has more than the PREVIEW_ENTRIES that were read.
    pub complete: bool,
    /// A sensible limit to start with (generated playlists).
    pub suggested_limit: Option<usize>,
}

/// How many entries `preview` reads at most.
pub const PREVIEW_ENTRIES: usize = 1000;
/// The limit offered first for a generated playlist.
pub const GENERATED_LIMIT: usize = 50;

#[derive(Debug)]
pub struct Failed {
    pub position: usize,
    pub title: String,
    pub problem: Problem,
}

/// Reported while `save` works.
#[derive(Debug)]
pub enum Progress {
    /// The link was read: its title, how many items will be saved, and how many cannot be.
    Listed {
        title: String,
        total: usize,
        unavailable: usize,
    },
    /// Something about the whole playlist the person should know.
    Note {
        message: String,
    },
    Started {
        position: usize,
        title: String,
    },
    /// Bytes of the current item's download, with speed (bytes per second) and seconds left.
    Downloading {
        position: usize,
        done: u64,
        total: Option<u64>,
        speed: Option<f64>,
        eta: Option<u64>,
    },
    /// A warning or error yt-dlp reported while saving an item.
    Log {
        position: usize,
        level: Level,
        message: String,
    },
    Done {
        position: usize,
    },
    /// Saved before; nothing to download.
    Skipped {
        position: usize,
    },
    Failed {
        position: usize,
        problem: Problem,
    },
    /// A step failed in a way that can heal (the connection); it runs again after a pause.
    /// `position` is None while reading the link.
    Retrying {
        position: Option<usize>,
        attempt: usize,
        of: usize,
        problem: Problem,
    },
    /// Still working, nothing to show (yt-dlp's own chatter): keeps the watchdog calm.
    Working,
}

/// What a yt-dlp failure means here. A stop that nobody asked for came from the watchdog
/// (no progress for too long): that is a connection that hangs, and the step runs again.
fn ytdlp_problem(err: &ytdlp::Error, cancel: &Cancel) -> Problem {
    if matches!(err, ytdlp::Error::Cancelled) && cancel.take_interrupted() && !cancel.is_cancelled()
    {
        return Problem::new(
            Kind::Network,
            "No progress for a long time; trying again.",
            "interrupted by the watchdog",
        );
    }
    problem::from_ytdlp(err)
}

fn db_problem(err: rusqlite::Error) -> Problem {
    problem::from_sqlite(&err, "saving to the library")
}

fn files_problem(err: std::io::Error) -> Problem {
    problem::from_io(&err, "saving files")
}

/// Saves everything behind `url` under `media` (an absolute folder) and records it in the
/// library database at `db`. Blocks for the whole download: call it on a worker thread.
pub fn save<F>(
    db: &Path,
    media: &Path,
    url: &str,
    options: &Options,
    on_progress: F,
    cancel: &Cancel,
) -> Result<Saved, Problem>
where
    F: Fn(Progress) + Send + Sync + 'static,
{
    let on_progress = Arc::new(on_progress);
    let retrying = |position: Option<usize>| {
        let progress = Arc::clone(&on_progress);
        move |attempt, of, problem: &Problem| {
            progress(Progress::Retrying {
                position,
                attempt,
                of,
                problem: problem.clone(),
            })
        }
    };
    let alive = Arc::clone(&on_progress);
    let listing = problem::with_retries(
        || list_limited(url, options.limit, cancel, &alive),
        retrying(None),
        || cancel.is_cancelled(),
    )?;
    if listing.entries.is_empty() {
        // Nothing is made or changed: a playlist saved before keeps what it had.
        return Err(Problem {
            retry: false,
            ..Problem::new(
                problem::Kind::Unavailable,
                "Nothing here can be saved: the playlist is empty, or everything in it was removed or made private.",
                format!(
                    "{url}: no saveable entries, {} unavailable",
                    listing.unavailable.len()
                ),
            )
        });
    }
    on_progress(Progress::Listed {
        title: listing.title.clone(),
        total: listing.entries.len(),
        unavailable: listing.unavailable.len(),
    });
    if !listing.unavailable.is_empty() {
        on_progress(Progress::Note {
            message: format!(
                "{} in this playlist cannot be saved and {} skipped:{}",
                count(listing.unavailable.len(), "entry", "entries"),
                if listing.unavailable.len() == 1 {
                    "is"
                } else {
                    "are"
                },
                listing
                    .unavailable
                    .iter()
                    .take(NOTED_UNAVAILABLE)
                    .map(|gone| format!("\n#{} {}: {}", gone.position, gone.title, gone.reason))
                    .collect::<String>()
                    + if listing.unavailable.len() > NOTED_UNAVAILABLE {
                        "\n…"
                    } else {
                        ""
                    }
            ),
        });
    }

    let category = match options.format {
        Format::Audio => "Music",
        Format::Video { .. } => "Videos",
    };
    let root = media.join(category);
    let folder = root.join(folder_name(&listing.title, &listing.id));
    // A second save of the same playlist (Save tapped twice) waits for the first, then
    // only adds what is new: two at once would write the same files.
    let _folder = FolderLock::take(&folder, cancel, &*on_progress)?;
    fs::create_dir_all(&folder).map_err(files_problem)?;
    let cover = if listing.is_playlist {
        save_cover(url, &folder, cancel)
    } else {
        None
    };

    let mut db = crate::db::open(db).map_err(db_problem)?;
    let category_id: i64 = db
        .query_row(
            "SELECT id FROM category WHERE name = ?1",
            [category],
            |row| row.get(0),
        )
        .map_err(db_problem)?;
    let library_id: i64 = db
        .query_row(
            "INSERT INTO library (category_id, root) VALUES (?1, ?2)
             ON CONFLICT (root) DO UPDATE SET root = excluded.root RETURNING id",
            params![category_id, text_path(&root)],
            |row| row.get(0),
        )
        .map_err(db_problem)?;

    let format_args = format_args(options.format, has_program("ffmpeg"));
    let format_args: Vec<&str> = format_args.iter().map(String::as_str).collect();
    let mut items = Vec::new();
    let mut failed = Vec::new();
    for (index, entry) in listing.entries.iter().enumerate() {
        let position = index + 1;
        if cancel.is_cancelled() {
            return Err(Problem::cancelled());
        }
        if let Some(item) =
            saved_before(&db, library_id, &root, &entry.source_id, position).map_err(db_problem)?
        {
            on_progress(Progress::Skipped { position });
            items.push(item);
            continue;
        }
        on_progress(Progress::Started {
            position,
            title: entry.title.clone(),
        });
        let downloaded = problem::with_retries(
            || {
                let progress = Arc::clone(&on_progress);
                download(
                    entry,
                    position,
                    &folder,
                    &format_args,
                    cancel,
                    move |event| item_event(&*progress, position, event),
                )
            },
            retrying(Some(position)),
            || cancel.is_cancelled(),
        );
        match downloaded.and_then(|item| {
            record(
                &mut db,
                category_id,
                library_id,
                &root,
                entry,
                position,
                item,
                options,
            )
        }) {
            Ok(item) => {
                on_progress(Progress::Done { position });
                items.push(item);
            }
            Err(problem) if problem.is_cancelled() => return Err(problem),
            Err(problem) => {
                on_progress(Progress::Failed {
                    position,
                    problem: problem.clone(),
                });
                failed.push(Failed {
                    position,
                    title: entry.title.clone(),
                    problem,
                });
            }
        }
    }

    let (collection_id, dropped) = if listing.is_playlist {
        let (id, dropped) =
            record_collection(&mut db, category_id, &listing, cover.as_deref(), &items)
                .map_err(db_problem)?;
        (Some(id), dropped)
    } else {
        (None, 0)
    };
    if dropped > 0 {
        on_progress(Progress::Note {
            message: format!(
                "{} no longer in this playlist; {} files stay in the library.",
                count(dropped, "item is", "items are"),
                if dropped == 1 { "its" } else { "their" },
            ),
        });
    }
    Ok(Saved {
        collection_id,
        title: listing.title,
        folder,
        cover,
        items,
        failed,
        unavailable: listing.unavailable,
        dropped,
    })
}

/// Entries named one by one in the job's log, at most.
const NOTED_UNAVAILABLE: usize = 20;

fn count(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// What the link holds, without saving anything: the size (for a limit), what cannot be
/// saved, and whether YouTube made the playlist up (a Mix). Reads at most PREVIEW_ENTRIES.
pub fn preview(url: &str, cancel: &Cancel) -> Result<Preview, Problem> {
    let quiet: Arc<dyn Fn(Progress) + Send + Sync> = Arc::new(|_| {});
    let listing = problem::with_retries(
        || list(url, Some(PREVIEW_ENTRIES), cancel, &quiet),
        |_, _, _| {},
        || cancel.is_cancelled(),
    )?;
    let generated = listing.is_playlist && is_generated(&listing.id);
    let available = listing.entries.len();
    Ok(Preview {
        title: listing.title,
        is_playlist: listing.is_playlist,
        generated,
        available,
        unavailable: listing.unavailable,
        complete: listing.complete,
        suggested_limit: generated.then(|| available.min(GENERATED_LIMIT)),
    })
}

/// Playlists YouTube makes up from a song or a channel (Mixes, "radio"): they can go on and
/// on. Their ids start with RD; RDCLAK ones are YouTube Music's own curated lists, which do
/// end.
pub fn is_generated(id: &str) -> bool {
    id.starts_with("RD") && !id.starts_with("RDCLAK")
}

/// Why a listed entry cannot be saved, from the listing alone; None when it can.
fn unavailable_reason(entry: &Value) -> Option<&'static str> {
    let title = entry["title"].as_str().unwrap_or_default().trim();
    if matches!(
        title,
        "[Deleted video]" | "[Private video]" | "[Unavailable video]"
    ) {
        return Some("removed or made private on YouTube");
    }
    match entry["availability"].as_str() {
        Some("private") => return Some("private"),
        Some("needs_auth") => return Some("needs signing in"),
        Some("subscriber_only") => return Some("for channel members only"),
        Some("premium_only") => return Some("for YouTube Premium only"),
        _ => {}
    }
    match entry["live_status"].as_str() {
        Some("is_upcoming") => Some("not out yet (a premiere or a scheduled stream)"),
        Some("is_live") => Some("live right now; save it once the stream has ended"),
        _ => None,
    }
}

/// The listing for a save: with a limit, the first `limit` entries that can be saved
/// (unavailable ones do not count), reading further when too many were unavailable.
fn list_limited(
    url: &str,
    limit: Option<usize>,
    cancel: &Cancel,
    progress: &Arc<impl Fn(Progress) + Send + Sync + 'static + ?Sized>,
) -> Result<Listing, Problem> {
    let Some(limit) = limit else {
        return list(url, None, cancel, progress);
    };
    let limit = limit.max(1);
    let mut end = limit.saturating_add(LIMIT_SLACK);
    for _ in 0..4 {
        let mut listing = list(url, Some(end), cancel, progress)?;
        if listing.entries.len() >= limit || listing.complete {
            keep_first(&mut listing, limit);
            return Ok(listing);
        }
        end = end.saturating_mul(4);
    }
    let mut listing = list(url, None, cancel, progress)?;
    keep_first(&mut listing, limit);
    Ok(listing)
}

/// Unavailable entries read beyond the limit, on top of it, so a few gone ones do not cut
/// the save short.
const LIMIT_SLACK: usize = 50;

/// Keeps the first `limit` saveable entries, and the unavailable ones before the last kept.
fn keep_first(listing: &mut Listing, limit: usize) {
    listing.entries.truncate(limit);
    if let Some(last) = listing.entries.last().map(|entry| entry.listed_at) {
        listing.unavailable.retain(|gone| gone.position < last);
    }
}

/// The playlist folders being saved into right now.
static BUSY_FOLDERS: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
static FOLDER_FREED: Condvar = Condvar::new();

/// Holds a playlist folder for one save; the next save of it waits.
struct FolderLock(PathBuf);

impl FolderLock {
    fn take(
        folder: &Path,
        cancel: &Cancel,
        progress: &dyn Fn(Progress),
    ) -> Result<FolderLock, Problem> {
        let mut busy = BUSY_FOLDERS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut said = false;
        while busy.iter().any(|taken| taken == folder) {
            if cancel.is_cancelled() {
                return Err(Problem::cancelled());
            }
            if !said {
                progress(Progress::Log {
                    position: 0,
                    level: Level::Info,
                    message: "Waiting for another save of this playlist to finish".to_owned(),
                });
                said = true;
            }
            // Waiting is a sign of life for the watchdog, not a stall.
            progress(Progress::Working);
            busy = FOLDER_FREED
                .wait_timeout(busy, std::time::Duration::from_millis(200))
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .0;
        }
        busy.push(folder.to_owned());
        Ok(FolderLock(folder.to_owned()))
    }
}

impl Drop for FolderLock {
    fn drop(&mut self) {
        let mut busy = BUSY_FOLDERS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        busy.retain(|taken| *taken != self.0);
        FOLDER_FREED.notify_all();
    }
}

/// One yt-dlp event while an item downloads, as progress.
fn item_event(progress: &dyn Fn(Progress), position: usize, event: Event) {
    match event {
        Event::Progress {
            done,
            total,
            speed,
            eta,
            ..
        } => progress(Progress::Downloading {
            position,
            done,
            total,
            speed,
            eta,
        }),
        Event::Log {
            level: level @ (ytdlp::Level::Warning | ytdlp::Level::Error),
            message,
        } => progress(Progress::Log {
            position,
            level: if level == ytdlp::Level::Warning {
                Level::Warning
            } else {
                Level::Error
            },
            message,
        }),
        _ => progress(Progress::Working),
    }
}

/// The params of a playlist job: its link and options, as JSON. Kept with the job, so
/// it can run again after the app was closed.
pub fn job_params(link: &str, options: &Options) -> String {
    let (format, max_height) = match options.format {
        Format::Audio => ("audio", None),
        Format::Video { max_height } => ("video", max_height),
    };
    json!({
        "link": link,
        "format": format,
        "max_height": max_height,
        "lyrics": options.lyrics,
        "limit": options.limit,
    })
    .to_string()
}

/// The runner for playlist jobs: reads `job_params` and calls `run`. The app registers it
/// once, with its own folders (they can move between app versions, so they are not
/// part of the params):
///
/// ```ignore
/// jobs.register("playlist", move |job, params| playlists::run_job(job, &db, &media, params));
/// let id = jobs.start("playlist", &link, &playlists::job_params(&link, &options))?;
/// ```
pub fn run_job(job: &Job, db: &Path, media: &Path, params: &str) -> Result<(), Problem> {
    let value: Value = serde_json::from_str(params)
        .map_err(|err| Problem::bug(format!("bad playlist job params: {err}")))?;
    let link =
        text(&value, &["link"]).ok_or_else(|| Problem::bug("the playlist job has no link"))?;
    let format = match value["format"].as_str() {
        Some("audio") => Format::Audio,
        Some("video") => Format::Video {
            max_height: value["max_height"].as_u64().map(|height| height as u32),
        },
        other => return Err(Problem::bug(format!("unknown playlist format {other:?}"))),
    };
    let options = Options {
        format,
        lyrics: value["lyrics"].as_bool().unwrap_or(false),
        limit: value["limit"].as_u64().map(|limit| limit as usize),
    };
    run(job, db, media, &link, &options)
}

/// Runs `save` as a job (see `jobs`): the job's state follows the playlist item by item,
/// yt-dlp's warnings and errors go to the job's log, and cancelling the job stops the
/// download. Failed items are reported but do not fail the job. Connection trouble is
/// tried again by itself (3 more times, after 2, 10 and 30 s), also when the watchdog
/// finds a download that hangs.
pub fn run(
    job: &Job,
    db: &Path,
    media: &Path,
    link: &str,
    options: &Options,
) -> Result<(), Problem> {
    let cancel = Cancel::new();
    let stop = cancel.clone();
    job.on_cancel(move || stop.cancel());
    let interrupt = cancel.clone();
    job.on_interrupt(move || interrupt.interrupt());
    let reporter = job.clone();
    let saved = save(
        db,
        media,
        link,
        options,
        move |progress| report(&reporter, progress),
        &cancel,
    );
    match saved {
        Ok(saved) => {
            let total = saved.items.len() + saved.failed.len();
            let message = format!(
                "Saved {} of {total} into {}",
                saved.items.len(),
                saved.folder.display()
            );
            job.log(Level::Info, &message);
            Ok(())
        }
        // The job ends as Cancelled; nothing more to say.
        Err(problem) if problem.is_cancelled() => Ok(()),
        Err(problem) => Err(problem),
    }
}

/// Turns one step of `save` into the job's state and log.
fn report(job: &Job, progress: Progress) {
    match progress {
        Progress::Listed { title, total, .. } => {
            job.set_title(&title);
            job.set_total(total);
        }
        Progress::Note { message } => job.log(Level::Warning, &message),
        Progress::Started { position, title } => job.item_started(position, &title),
        Progress::Downloading {
            done,
            total,
            speed,
            eta,
            ..
        } => job.progress(done, total, speed, eta),
        Progress::Done { .. } | Progress::Skipped { .. } => job.item_done(),
        Progress::Failed { position, problem } => job.item_failed(&Problem {
            text: format!("#{position}: {}", problem.text),
            ..problem
        }),
        Progress::Retrying {
            position,
            attempt,
            of,
            problem,
        } => {
            let place = position.map_or(String::new(), |position| format!("#{position}: "));
            job.log(
                Level::Warning,
                &format!("{place}{} (try {attempt} of {of})", problem.text),
            );
        }
        Progress::Working => job.keep_alive(),
        Progress::Log {
            position,
            level,
            message,
        } => job.log(level, &format!("#{position}: {message}")),
    }
}

/// A link's title and entries, read without downloading anything.
struct Listing {
    /// Where it came from, e.g. "YoutubeTab:PLxxxx". Identifies the playlist across runs.
    source: String,
    id: String,
    title: String,
    is_playlist: bool,
    entries: Vec<Entry>,
    unavailable: Vec<Unavailable>,
    /// Whether the listing reached the playlist's end (not cut by `end`).
    complete: bool,
}

struct Entry {
    /// Where it is in the playlist (1-based, counting every entry).
    listed_at: usize,
    /// e.g. "Youtube:dQw4w9WgXcQ". Identifies the item across runs.
    source_id: String,
    title: String,
    url: String,
}

/// Reads a link's entries without downloading anything: at most `end` of them.
fn list(
    url: &str,
    end: Option<usize>,
    cancel: &Cancel,
    progress: &Arc<impl Fn(Progress) + Send + Sync + 'static + ?Sized>,
) -> Result<Listing, Problem> {
    let end_text = end.map(|end| end.max(1).to_string());
    let mut args = vec!["--flat-playlist", "--yes-playlist"];
    if let Some(end) = &end_text {
        args.extend(["--playlist-end", end]);
    }
    let alive = Arc::clone(progress);
    let json = ytdlp::info(url, &args, move |_| alive(Progress::Working), cancel)
        .map_err(|err| ytdlp_problem(&err, cancel))?;
    let info: Value = serde_json::from_str(&json)
        .map_err(|err| Problem::bug(format!("yt-dlp's answer is not JSON: {err}")))?;
    let id = text(&info, &["id"]).unwrap_or_default();
    let title = text(&info, &["title", "id"]).unwrap_or_else(|| "Untitled".to_owned());
    let extractor = text(&info, &["extractor_key", "ie_key"]).unwrap_or_else(|| "url".to_owned());
    let source = format!("{extractor}:{}", if id.is_empty() { url } else { &id });

    if info["_type"] != "playlist" {
        let entry = Entry {
            listed_at: 1,
            source_id: source.clone(),
            title: title.clone(),
            url: text(&info, &["webpage_url", "original_url"]).unwrap_or_else(|| url.to_owned()),
        };
        return Ok(Listing {
            source,
            id,
            title,
            is_playlist: false,
            entries: vec![entry],
            unavailable: Vec::new(),
            complete: true,
        });
    }

    let listed = info["entries"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default();
    let (entries, unavailable) = split_entries(listed);
    let complete = end.is_none_or(|end| listed.len() < end.max(1));
    Ok(Listing {
        source,
        id,
        title,
        is_playlist: true,
        entries,
        unavailable,
        complete,
    })
}

/// The entries that can be saved, and the ones that cannot (with why).
fn split_entries(listed: &[Value]) -> (Vec<Entry>, Vec<Unavailable>) {
    let mut entries = Vec::new();
    let mut unavailable = Vec::new();
    for (index, entry) in listed.iter().enumerate() {
        let position = index + 1;
        let title = text(entry, &["title"]);
        if let Some(reason) = unavailable_reason(entry) {
            unavailable.push(Unavailable {
                position,
                title: title.unwrap_or_else(|| "Untitled".to_owned()),
                reason: reason.to_owned(),
            });
            continue;
        }
        // YouTube lists link to watch pages; other sites may only give the media URL.
        let Some(url) =
            text(entry, &["url", "webpage_url"]).or_else(|| text(&entry["formats"][0], &["url"]))
        else {
            unavailable.push(Unavailable {
                position,
                title: title.unwrap_or_else(|| "Untitled".to_owned()),
                reason: "has no link to save".to_owned(),
            });
            continue;
        };
        let id = text(entry, &["id"]).unwrap_or_else(|| url.clone());
        let extractor =
            text(entry, &["ie_key", "extractor_key"]).unwrap_or_else(|| "url".to_owned());
        // Sites with their own ids (YouTube) keep a video's id wherever it moves in a list.
        // Generic pages number their entries by place, so the media link is what stays.
        let stable = if matches!(extractor.as_str(), "Generic" | "HTML5MediaEmbed" | "url") {
            url.clone()
        } else {
            id.clone()
        };
        entries.push(Entry {
            listed_at: position,
            source_id: format!("{extractor}:{stable}"),
            title: title.unwrap_or_else(|| id.clone()),
            url,
        });
    }
    (entries, unavailable)
}

/// Saves the playlist's banner as `cover.<ext>`. A playlist without one is fine.
fn save_cover(url: &str, folder: &Path, cancel: &Cancel) -> Option<PathBuf> {
    let folder_text = text_path(folder);
    let args = [
        "--flat-playlist",
        "--yes-playlist",
        "--skip-download",
        "--write-thumbnail",
        "-P",
        &folder_text,
        "-o",
        "pl_thumbnail:cover.%(ext)s",
    ];
    let _ = ytdlp::download(url, &args, |_| {}, cancel);
    sibling(&folder.join("cover"), IMAGES)
}

fn format_args(format: Format, ffmpeg: bool) -> Vec<String> {
    let Format::Video { max_height } = format else {
        return ["-f", "ba[ext=m4a]/ba"].map(String::from).to_vec();
    };
    // yt-dlp's format sorting: the largest video up to max_height (or the closest, when
    // none is that small), then H.264 video and AAC audio in mp4, which play on every phone.
    let size = max_height.map_or("res".to_owned(), |height| format!("res:{height}"));
    let sort = format!("{size},vcodec:h264,acodec:aac,ext:mp4:m4a");
    let select = if ffmpeg { "bv*+ba/b" } else { "b" };
    let mut args = vec!["-f".to_owned(), select.to_owned(), "-S".to_owned(), sort];
    if ffmpeg {
        args.extend(["--merge-output-format".to_owned(), "mp4".to_owned()]);
    }
    args
}

/// What one item's download produced, before it goes into the database.
struct Downloaded {
    file: PathBuf,
    info: Value,
}

fn download(
    entry: &Entry,
    position: usize,
    folder: &Path,
    format_args: &[&str],
    cancel: &Cancel,
    on_event: impl Fn(Event) + Send + Sync + 'static,
) -> Result<Downloaded, Problem> {
    // yt-dlp fills in the title and id and makes the name safe for the file system.
    let name = format!("{position:03} - %(title).100B [%(id)s]");
    let media = format!("{name}.%(ext)s");
    let image = format!("thumbnail:{name}.%(ext)s");
    let folder_text = text_path(folder);
    let mut args = vec![
        "--no-playlist",
        "-P",
        &folder_text,
        "-o",
        &media,
        "--write-thumbnail",
        "-o",
        &image,
    ];
    args.extend_from_slice(format_args);

    let done = ytdlp::download(&entry.url, &args, on_event, cancel)
        .map_err(|err| ytdlp_problem(&err, cancel))?;
    let file = done
        .files
        .into_iter()
        .next()
        .ok_or_else(|| Problem::bug("yt-dlp saved no file"))?;
    let info = done
        .json
        .and_then(|json| serde_json::from_str(&json).ok())
        .unwrap_or(Value::Null);
    Ok(Downloaded { file, info })
}

/// Puts one downloaded item (with its lyrics, when asked) into the library.
#[allow(clippy::too_many_arguments)]
fn record(
    db: &mut Connection,
    category_id: i64,
    library_id: i64,
    root: &Path,
    entry: &Entry,
    position: usize,
    downloaded: Downloaded,
    options: &Options,
) -> Result<SavedItem, Problem> {
    let info = &downloaded.info;
    let file = downloaded.file;
    let meta = Meta::read(info, entry);
    let image = sibling(&file.with_extension(""), IMAGES);
    let lyrics = if options.lyrics {
        lyrics::save(&meta, &file)
    } else {
        None
    };

    let relative = file
        .strip_prefix(root)
        .map_err(|_| Problem::bug(format!("{} is outside {}", file.display(), root.display())))?;
    let mtime = fs::metadata(&file)
        .map_err(files_problem)?
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|time| time.as_secs() as i64);
    let extra = json!({
        "source_id": entry.source_id,
        "url": text(info, &["webpage_url", "original_url"]).unwrap_or_else(|| entry.url.clone()),
        "artist": meta.artist,
        "album": meta.album,
        "uploader": meta.uploader,
        "lyrics": lyrics.as_deref().map(text_path),
    });

    let item_id = insert_item(
        db,
        category_id,
        library_id,
        relative,
        mtime,
        &meta,
        image.as_deref(),
        &extra,
    )
    .map_err(db_problem)?;

    Ok(SavedItem {
        item_id,
        position,
        title: meta.title,
        artist: meta.artist,
        file,
        image,
        lyrics,
    })
}

/// Writes one item and its artists, all or nothing.
#[allow(clippy::too_many_arguments)]
fn insert_item(
    db: &mut Connection,
    category_id: i64,
    library_id: i64,
    relative: &Path,
    mtime: Option<i64>,
    meta: &Meta,
    image: Option<&Path>,
    extra: &Value,
) -> rusqlite::Result<i64> {
    let tx = db.transaction()?;
    let item_id: i64 = tx.query_row(
        "INSERT INTO item (category_id, library_id, kind, path, mtime, title, sort_title,
                           year, summary, cover, duration_ms, extra)
         VALUES (?1, ?2, 'entry', ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
         ON CONFLICT (library_id, path) DO UPDATE SET
           mtime = excluded.mtime, title = excluded.title, sort_title = excluded.sort_title,
           year = excluded.year, summary = excluded.summary, cover = excluded.cover,
           duration_ms = excluded.duration_ms, extra = excluded.extra, missing_since = NULL
         RETURNING id",
        params![
            category_id,
            library_id,
            text_path(relative),
            mtime,
            meta.title,
            sort_title(&meta.title),
            meta.year,
            meta.description,
            image.map(text_path),
            meta.duration_ms,
            extra.to_string(),
        ],
        |row| row.get(0),
    )?;
    for artist in &meta.artists {
        let tag_id: i64 = tx.query_row(
            "INSERT INTO tag (kind, name) VALUES ('artist', ?1)
             ON CONFLICT (kind, name) DO UPDATE SET name = name RETURNING id",
            [artist],
            |row| row.get(0),
        )?;
        tx.execute(
            "INSERT OR IGNORE INTO item_tag (item_id, tag_id) VALUES (?1, ?2)",
            [item_id, tag_id],
        )?;
    }
    tx.commit()?;
    Ok(item_id)
}

/// An item saved on an earlier run whose file is still there.
fn saved_before(
    db: &Connection,
    library_id: i64,
    root: &Path,
    source_id: &str,
    position: usize,
) -> rusqlite::Result<Option<SavedItem>> {
    let found = db
        .query_row(
            "SELECT id, path, title, cover, extra FROM item
             WHERE library_id = ?1 AND json_extract(extra, '$.source_id') = ?2
               AND missing_since IS NULL",
            params![library_id, source_id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            },
        )
        .optional()?;
    let Some((item_id, path, title, cover, extra)) = found else {
        return Ok(None);
    };
    let file = root.join(path);
    if !file.is_file() {
        return Ok(None);
    }
    let extra: Value = extra
        .and_then(|extra| serde_json::from_str(&extra).ok())
        .unwrap_or(Value::Null);
    Ok(Some(SavedItem {
        item_id,
        position,
        title,
        artist: text(&extra, &["artist"]),
        file,
        image: cover.map(PathBuf::from),
        lyrics: text(&extra, &["lyrics"]).map(PathBuf::from),
    }))
}

/// Creates or updates the playlist and sets its items, in order.
fn record_collection(
    db: &mut Connection,
    category_id: i64,
    listing: &Listing,
    cover: Option<&Path>,
    items: &[SavedItem],
) -> rusqlite::Result<(i64, usize)> {
    let tx = db.transaction()?;
    let collection_id: i64 = tx.query_row(
        "INSERT INTO collection (category_id, name, cover, source) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (source) DO UPDATE SET
           name = excluded.name, cover = coalesce(excluded.cover, collection.cover)
         RETURNING id",
        params![
            category_id,
            listing.title,
            cover.map(text_path),
            listing.source
        ],
        |row| row.get(0),
    )?;
    // Items saved before that are not in the playlist any more leave it (their files and
    // library entries stay).
    let before: Vec<i64> = tx
        .prepare("SELECT DISTINCT item_id FROM collection_item WHERE collection_id = ?1")?
        .query_map([collection_id], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let dropped = before
        .iter()
        .filter(|id| !items.iter().any(|item| item.item_id == **id))
        .count();
    tx.execute(
        "DELETE FROM collection_item WHERE collection_id = ?1",
        [collection_id],
    )?;
    for item in items {
        tx.execute(
            "INSERT INTO collection_item (collection_id, item_id, position) VALUES (?1, ?2, ?3)",
            params![collection_id, item.item_id, item.position as f64],
        )?;
    }
    tx.commit()?;
    Ok((collection_id, dropped))
}

/// The rich part of an item, from yt-dlp's info JSON.
///
/// YouTube Music gives proper music fields (track, artists, album, release year). A plain
/// YouTube music video (category Music) has none, but its channel is nearly always the
/// artist, and its title names the song between noise like "(Official Video)":
/// "BELLAKEO (Video Oficial) - Peso Pluma, Anitta" on the channel "Peso Pluma" becomes the
/// song "BELLAKEO" by "Peso Pluma". Other videos keep their title and have no artist.
pub(crate) struct Meta {
    pub title: String,
    pub artist: Option<String>,
    pub artists: Vec<String>,
    pub album: Option<String>,
    pub uploader: Option<String>,
    pub year: Option<i64>,
    pub duration_ms: Option<i64>,
    pub description: Option<String>,
    /// True when the music fields came from YouTube Music itself (exact song lengths).
    pub from_music_fields: bool,
}

impl Meta {
    fn read(info: &Value, entry: &Entry) -> Meta {
        let mut artists: Vec<String> = info["artists"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|artist| artist.as_str())
            .map(str::to_owned)
            .collect();
        if artists.is_empty()
            && let Some(artist) = text(info, &["artist", "creator"])
        {
            artists = artist.split(", ").map(str::to_owned).collect();
        }
        let from_music_fields = !artists.is_empty() || text(info, &["track"]).is_some();
        let uploader = text(info, &["channel", "uploader"]);
        let mut title = text(info, &["track", "title"]).unwrap_or_else(|| entry.title.clone());

        let is_music = info["categories"]
            .as_array()
            .is_some_and(|categories| categories.iter().any(|category| category == "Music"));
        if !from_music_fields
            && is_music
            && let Some(artist) = uploader
                .as_deref()
                .map(channel_artist)
                .filter(|artist| !artist.is_empty())
        {
            title = song_title(&title, &artist);
            artists = vec![artist];
        }

        let year = info["release_year"].as_i64().or_else(|| {
            text(info, &["release_date", "upload_date"])
                .and_then(|date| date.get(..4)?.parse().ok())
        });
        Meta {
            title,
            artist: (!artists.is_empty()).then(|| artists.join(", ")),
            artists,
            album: text(info, &["album"]),
            uploader,
            year,
            duration_ms: info["duration"]
                .as_f64()
                .map(|seconds| (seconds * 1000.0) as i64),
            description: text(info, &["description"]),
            from_music_fields,
        }
    }
}

/// Words that mark a bracketed part of a video title as noise, like "(Official Video)".
const NOISE: &[&str] = &[
    "official", "oficial", "video", "audio", "lyric", "letra", "visuali", "mv", "hd", "4k", "hq",
    "feat", "ft.", "explicit", "clean",
];

/// The artist behind a music channel: "Artist - Topic" and "ArtistVEVO" are YouTube's
/// automatic and label channels.
fn channel_artist(channel: &str) -> String {
    let artist = channel.trim();
    let artist = artist.strip_suffix(" - Topic").unwrap_or(artist);
    let artist = artist.strip_suffix("VEVO").unwrap_or(artist);
    artist.trim().to_owned()
}

/// The song in a music video title: noise dropped, and of "A - B" the part that does not
/// name the artist. Falls back to the cleaned title.
fn song_title(title: &str, artist: &str) -> String {
    let cleaned = clean_title(title);
    let artist = fold(artist);
    let parts: Vec<&str> = cleaned
        .split(" - ")
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect();
    let songs: Vec<&str> = parts
        .iter()
        .copied()
        .filter(|part| !fold(part).contains(&artist))
        .collect();
    match songs.as_slice() {
        [song] if parts.len() > 1 => song.to_string(),
        _ => cleaned,
    }
}

/// Drops bracketed noise such as "(Official Video)" or "[Lyrics]" from a title, and keeps
/// meaningful brackets such as "(Remix)".
pub(crate) fn clean_title(title: &str) -> String {
    let mut out = String::new();
    let mut rest = title;
    while let Some(open) = rest.find(['(', '[']) {
        let close = if rest[open..].starts_with('(') {
            ')'
        } else {
            ']'
        };
        let Some(length) = rest[open..].find(close) else {
            break;
        };
        let inside = rest[open + 1..open + length].to_lowercase();
        out.push_str(&rest[..open]);
        if !NOISE.iter().any(|word| inside.contains(word)) {
            out.push_str(&rest[open..=open + length]);
        }
        rest = &rest[open + length + 1..];
    }
    out.push_str(rest);
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Lowercase without the common Latin accents, for comparing names ("Arcángel" = "arcangel").
fn fold(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .map(|c| match c {
            'á' | 'à' | 'â' | 'ä' | 'ã' | 'å' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'í' | 'ì' | 'î' | 'ï' => 'i',
            'ó' | 'ò' | 'ô' | 'ö' | 'õ' | 'ø' => 'o',
            'ú' | 'ù' | 'û' | 'ü' => 'u',
            'ñ' => 'n',
            'ç' => 'c',
            'ý' | 'ÿ' => 'y',
            c => c,
        })
        .collect()
}

const IMAGES: &[&str] = &["jpg", "jpeg", "png", "webp"];

/// `stem` with the first of `extensions` that exists.
fn sibling(stem: &Path, extensions: &[&str]) -> Option<PathBuf> {
    extensions
        .iter()
        .map(|ext| {
            let mut name = stem.as_os_str().to_owned();
            name.push(".");
            name.push(ext);
            PathBuf::from(name)
        })
        .find(|path| path.is_file())
}

/// The first non-empty string among `keys`.
fn text(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .filter_map(|key| value[*key].as_str())
        .map(str::trim)
        .find(|text| !text.is_empty())
        .map(str::to_owned)
}

fn text_path(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// `<title> [<id>]`, safe on every file system (also FAT and SMB shares on a NAS).
fn folder_name(title: &str, id: &str) -> String {
    let safe: String = title
        .chars()
        .map(|c| {
            if c.is_control() || r#"/\:*?"<>|"#.contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    let safe = safe.split_whitespace().collect::<Vec<_>>().join(" ");
    let safe: String = safe.chars().take(80).collect();
    let safe = safe.trim_matches(|c: char| c == '.' || c == ' ');
    let safe = if safe.is_empty() { "Untitled" } else { safe };
    let id: String = id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(64)
        .collect();
    if id.is_empty() {
        safe.to_owned()
    } else {
        format!("{safe} [{id}]")
    }
}

/// Lowercase, with numbers padded so "Track 2" sorts before "Track 10".
fn sort_title(title: &str) -> String {
    let mut sorted = String::new();
    let mut digits = String::new();
    for c in title.to_lowercase().chars() {
        if c.is_ascii_digit() {
            digits.push(c);
            continue;
        }
        if !digits.is_empty() {
            sorted.push_str(&format!("{digits:0>10}"));
            digits.clear();
        }
        sorted.push(c);
    }
    if !digits.is_empty() {
        sorted.push_str(&format!("{digits:0>10}"));
    }
    sorted
}

/// Whether a program is on PATH (ffmpeg on desktop and Android; never on iOS).
fn has_program(name: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(name).is_file()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_names_are_safe() {
        assert_eq!(
            folder_name("My: Mix / 2024?", "PL_a-1"),
            "My_ Mix _ 2024_ [PL_a-1]"
        );
        assert_eq!(folder_name("  ...  ", ""), "Untitled");
        assert_eq!(folder_name("ok", "../x"), "ok [x]");
        assert_eq!(folder_name(&"a".repeat(200), "id").chars().count(), 85);
    }

    #[test]
    fn video_quality_becomes_format_sorting() {
        let args = |format, ffmpeg| format_args(format, ffmpeg).join(" ");
        assert_eq!(
            args(
                Format::Video {
                    max_height: Some(720)
                },
                true
            ),
            "-f bv*+ba/b -S res:720,vcodec:h264,acodec:aac,ext:mp4:m4a --merge-output-format mp4"
        );
        assert_eq!(
            args(Format::Video { max_height: None }, false),
            "-f b -S res,vcodec:h264,acodec:aac,ext:mp4:m4a"
        );
        assert_eq!(args(Format::Audio, true), "-f ba[ext=m4a]/ba");
    }

    #[test]
    fn job_params_keep_link_and_options() {
        let options = Options {
            format: Format::Video {
                max_height: Some(720),
            },
            lyrics: true,
            limit: Some(3),
        };
        let params: Value = serde_json::from_str(&job_params("https://x/list", &options)).unwrap();
        assert_eq!(params["link"], "https://x/list");
        assert_eq!(
            (params["format"].as_str(), params["max_height"].as_u64()),
            (Some("video"), Some(720))
        );
        assert_eq!(
            (params["lyrics"].as_bool(), params["limit"].as_u64()),
            (Some(true), Some(3))
        );
    }

    #[test]
    fn sort_titles_pad_numbers() {
        assert!(sort_title("Track 2") < sort_title("Track 10"));
        assert_eq!(sort_title("A1b"), "a0000000001b");
    }

    #[test]
    fn music_fields_win_over_video_fields() {
        let entry = Entry {
            listed_at: 1,
            source_id: "Youtube:x".to_owned(),
            title: "fallback".to_owned(),
            url: "u".to_owned(),
        };
        let info = json!({
            "title": "Artist - Song (Official Video)", "track": "Song",
            "artists": ["Artist", "Guest"], "album": "Album", "release_year": 2021,
            "channel": "Artist - Topic", "duration": 201.5, "upload_date": "20200101"
        });
        let meta = Meta::read(&info, &entry);
        assert_eq!(meta.title, "Song");
        assert_eq!(meta.artist.as_deref(), Some("Artist, Guest"));
        assert_eq!(meta.album.as_deref(), Some("Album"));
        assert_eq!(meta.year, Some(2021));
        assert_eq!(meta.duration_ms, Some(201_500));

        let plain = Meta::read(&json!({"title": "Clip", "upload_date": "20190305"}), &entry);
        assert_eq!(
            (plain.title.as_str(), plain.artist, plain.year),
            ("Clip", None, Some(2019))
        );
        assert_eq!(Meta::read(&Value::Null, &entry).title, "fallback");
    }

    #[test]
    fn music_videos_get_song_and_artist_from_title_and_channel() {
        let entry = Entry {
            listed_at: 1,
            source_id: "Youtube:x".to_owned(),
            title: "x".to_owned(),
            url: "u".to_owned(),
        };
        let read = |title: &str, channel: &str| {
            let meta = Meta::read(
                &json!({"title": title, "channel": channel, "categories": ["Music"]}),
                &entry,
            );
            (meta.title, meta.artist)
        };
        let song = |title: &str, artist: &str| (title.to_owned(), Some(artist.to_owned()));
        assert_eq!(
            read(
                "BELLAKEO (Video Oficial) - Peso Pluma, Anitta",
                "Peso Pluma"
            ),
            song("BELLAKEO", "Peso Pluma")
        );
        assert_eq!(
            read("Arcángel - FN8 ( Video Lyric )", "Arcangel"),
            song("FN8", "Arcangel")
        );
        assert_eq!(
            read("Song Name", "Artist - Topic"),
            song("Song Name", "Artist")
        );
        assert_eq!(
            read("Band - Hit (Remix) [Official Audio]", "BandVEVO"),
            song("Hit (Remix)", "Band")
        );
        // Not a music video: the title stays, no artist is guessed.
        let vlog = Meta::read(
            &json!({"title": "My trip - day 1", "channel": "Me", "categories": ["Travel"]}),
            &entry,
        );
        assert_eq!(
            (vlog.title.as_str(), vlog.artist),
            ("My trip - day 1", None)
        );
    }

    #[test]
    fn titles_lose_noise_but_keep_meaning() {
        assert_eq!(clean_title("Song (Official Video) [4K]"), "Song");
        assert_eq!(clean_title("Song (Remix) (Lyric Video)"), "Song (Remix)");
        assert_eq!(clean_title("Song (feat. Guest)"), "Song");
        assert_eq!(clean_title("Unclosed (bracket"), "Unclosed (bracket");
    }

    fn listing_of(entries: Vec<Value>) -> Listing {
        let (entries, unavailable) = split_entries(&entries);
        Listing {
            source: "Test:PL1".into(),
            id: "PL1".into(),
            title: "List".into(),
            is_playlist: true,
            entries,
            unavailable,
            complete: true,
        }
    }

    fn video(id: &str) -> Value {
        json!({"id": id, "title": format!("Video {id}"), "url": format!("https://www.youtube.com/watch?v={id}"), "ie_key": "Youtube"})
    }

    #[test]
    fn entries_that_cannot_be_saved_are_known_from_the_listing() {
        let cases = [
            (
                json!({"title": "[Deleted video]", "url": "https://youtube.com/watch?v=a"}),
                Some("removed or made private on YouTube"),
            ),
            (
                json!({"title": " [Private video] ", "url": "u"}),
                Some("removed or made private on YouTube"),
            ),
            (
                json!({"title": "[Unavailable video]", "url": "u"}),
                Some("removed or made private on YouTube"),
            ),
            (
                json!({"title": "Song", "availability": "private", "url": "u"}),
                Some("private"),
            ),
            (
                json!({"title": "Song", "availability": "subscriber_only", "url": "u"}),
                Some("for channel members only"),
            ),
            (
                json!({"title": "Song", "availability": "premium_only", "url": "u"}),
                Some("for YouTube Premium only"),
            ),
            (
                json!({"title": "Song", "availability": "needs_auth", "url": "u"}),
                Some("needs signing in"),
            ),
            (
                json!({"title": "Premiere", "live_status": "is_upcoming", "url": "u"}),
                Some("not out yet (a premiere or a scheduled stream)"),
            ),
            (
                json!({"title": "Live", "live_status": "is_live", "url": "u"}),
                Some("live right now; save it once the stream has ended"),
            ),
            (
                json!({"title": "Song", "availability": "public", "url": "u"}),
                None,
            ),
            (
                json!({"title": "Song", "availability": "unlisted", "url": "u"}),
                None,
            ),
            (
                json!({"title": "Old stream", "live_status": "was_live", "url": "u"}),
                None,
            ),
            (
                json!({"title": null, "availability": null, "live_status": null, "url": "u"}),
                None,
            ),
            (json!({"title": "[Deleted video] remix", "url": "u"}), None),
        ];
        for (entry, reason) in cases {
            assert_eq!(unavailable_reason(&entry), reason, "{entry}");
        }
    }

    #[test]
    fn unavailable_entries_are_split_off_with_their_place() {
        let listing = listing_of(vec![
            video("a"),
            json!({"title": "[Deleted video]", "url": "u1", "id": "x"}),
            video("b"),
            json!({"title": "No link"}),
            json!({"title": "[Private video]", "url": "u2", "id": "y"}),
            video("c"),
        ]);
        let kept: Vec<(usize, &str)> = listing
            .entries
            .iter()
            .map(|e| (e.listed_at, e.source_id.as_str()))
            .collect();
        assert_eq!(kept, [(1, "Youtube:a"), (3, "Youtube:b"), (6, "Youtube:c")]);
        let gone: Vec<(usize, &str)> = listing
            .unavailable
            .iter()
            .map(|g| (g.position, g.reason.as_str()))
            .collect();
        assert_eq!(
            gone,
            [
                (2, "removed or made private on YouTube"),
                (4, "has no link to save"),
                (5, "removed or made private on YouTube")
            ]
        );
    }

    #[test]
    fn a_limit_counts_only_entries_that_can_be_saved() {
        let mut listing = listing_of(vec![
            json!({"title": "[Deleted video]", "url": "u"}),
            video("a"),
            json!({"title": "[Private video]", "url": "u"}),
            video("b"),
            video("c"),
            json!({"title": "[Deleted video]", "url": "u"}),
        ]);
        keep_first(&mut listing, 2);
        let ids: Vec<&str> = listing
            .entries
            .iter()
            .map(|e| e.source_id.as_str())
            .collect();
        assert_eq!(
            ids,
            ["Youtube:a", "Youtube:b"],
            "two saveable ones, though four were read"
        );
        let gone: Vec<usize> = listing.unavailable.iter().map(|g| g.position).collect();
        assert_eq!(
            gone,
            [1, 3],
            "only the unavailable ones before the cut are mentioned"
        );
        // A limit past the end keeps everything.
        let mut all = listing_of(vec![video("a"), video("b")]);
        keep_first(&mut all, 100);
        assert_eq!(all.entries.len(), 2);
    }

    #[test]
    fn generated_playlists_are_recognised() {
        for id in [
            "RDdQw4w9WgXcQ",
            "RDMMdQw4w9WgXcQ",
            "RDAMVMdQw4w9WgXcQ",
            "RDEMx",
        ] {
            assert!(is_generated(id), "{id}");
        }
        for id in [
            "RDCLAK5uy_kmPRjHDECIcuVwnKsx2Ng7fyNgFKWNJFs",
            "PLrAXtmErZgOeiKm4sgNOknGvNjby9efdf",
            "OLAK5uy_x",
            "UUx",
            "",
            "rdx",
        ] {
            assert!(!is_generated(id), "{id}");
        }
    }
}
