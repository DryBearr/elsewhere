//! The media library, read side, step 1: the tabs, the playlists of a tab, a playlist's
//! items in order (a page at a time), one item's details, and the file to play.
//! Browsing, user marks, editing, change notices, search and folder scans come later.
//!
//! The rules every call keeps:
//!
//! - **Ids, never paths.** The apps hold item ids. The only way to a file is `item_file`,
//!   which returns the real path after checking that it exists and lies inside the item's
//!   library folder and the media folder.
//! - **Bad data never fails a list.** Missing fields, odd values in `extra`, a deleted
//!   file or cover: the row still comes, marked `missing` or without a cover.
//! - **Pages, not everything.** Long lists come a page at a time, ordered by position and
//!   then id, with an opaque cursor for the next page: rows added or removed meanwhile
//!   never make a row appear twice or get skipped among the ones that stay.
//! - **One connection, healed.** Opened once and reused; a call that fails because the
//!   connection broke reopens it and runs once more. Errors are Problems.

#[cfg(test)]
mod tests;

use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use rusqlite::{Connection, ErrorCode, OptionalExtension, Row, params};

use crate::logs;
use crate::problem::{self, Kind as ProblemKind, Problem};

/// Rows per page when the caller does not say, and the most it may ask for.
pub const DEFAULT_PAGE: usize = 100;
pub const MAX_PAGE: usize = 500;

/// Which player or reader opens a tab's items.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Media {
    Audio,
    Video,
    Ebook,
    Pages,
}

/// Reading direction of a tab (books and comics).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Ltr,
    Rtl,
    Vertical,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Category {
    pub id: i64,
    pub name: String,
    pub media: Media,
    pub direction: Direction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemKind {
    /// Album, show, series: holds entries.
    Group,
    /// Track, episode, movie, chapter: one file.
    Entry,
}

/// Where the person stopped, and how often they played it.
#[derive(Debug, Clone, PartialEq)]
pub struct Progress {
    /// Milliseconds for audio and video, the page for pages.
    pub position: i64,
    /// 0 to 1.
    pub percent: f64,
    pub done: bool,
    pub plays: i64,
}

/// What a list row shows.
#[derive(Debug, Clone, PartialEq)]
pub struct ItemSummary {
    pub id: i64,
    pub kind: ItemKind,
    pub category_id: i64,
    pub media: Media,
    pub title: String,
    /// The main artist (music).
    pub artist: Option<String>,
    /// Every artist, in the order they were first seen.
    pub artists: Vec<String>,
    pub year: Option<i64>,
    pub duration_ms: Option<i64>,
    /// Season, disc or volume.
    pub group_no: Option<i64>,
    /// Episode, track or chapter (10.5 happens).
    pub number: Option<f64>,
    /// An image file that exists inside the media folder.
    pub cover: Option<PathBuf>,
    pub favorite: bool,
    pub rating: Option<i64>,
    /// The file is gone (or an entry has none): it cannot be played.
    pub missing: bool,
    pub progress: Option<Progress>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tag {
    /// artist, genre, author, studio, publisher, tag
    pub kind: String,
    pub name: String,
}

/// Everything about one item.
#[derive(Debug, Clone, PartialEq)]
pub struct ItemDetail {
    pub summary: ItemSummary,
    pub parent_id: Option<i64>,
    pub original_title: Option<String>,
    pub description: Option<String>,
    pub page_count: Option<i64>,
    /// Seconds since 1970.
    pub added_at: i64,
    pub album: Option<String>,
    /// Where it was saved from (a web page).
    pub source_url: Option<String>,
    pub tags: Vec<Tag>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CollectionSummary {
    pub id: i64,
    pub category_id: i64,
    pub name: String,
    pub cover: Option<PathBuf>,
    /// How many entries it has (the same item can be in it twice).
    pub entries: i64,
    /// Where it came from, for saved playlists ("YoutubeTab:PL...").
    pub source: Option<String>,
    /// Filled by a saved search instead of by hand (its items come in a later step).
    pub smart: bool,
    pub created_at: i64,
}

/// One row of a collection: the same item can appear more than once, each with its own
/// entry id.
#[derive(Debug, Clone, PartialEq)]
pub struct CollectionEntry {
    pub entry_id: i64,
    pub position: f64,
    pub item: ItemSummary,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Page<T> {
    pub rows: Vec<T>,
    /// Pass it back for the next page; None at the end.
    pub next: Option<String>,
}

pub struct Library {
    path: PathBuf,
    /// The media folder, with links resolved.
    media: PathBuf,
    db: Mutex<Option<Connection>>,
    #[cfg(test)]
    fail_next: std::sync::atomic::AtomicUsize,
}

impl Library {
    /// The library in the database at `db`, with its files under `media`.
    pub fn open(db: &Path, media: &Path) -> Result<Library, Problem> {
        let media = fs::canonicalize(media).map_err(|err| {
            problem::from_io(
                &err,
                &format!("opening the media folder {}", media.display()),
            )
        })?;
        let connection =
            crate::db::open(db).map_err(|err| problem::from_sqlite(&err, "opening the library"))?;
        Ok(Library {
            path: db.to_owned(),
            media,
            db: Mutex::new(Some(connection)),
            #[cfg(test)]
            fail_next: std::sync::atomic::AtomicUsize::new(0),
        })
    }

    /// Every tab, in display order.
    pub fn categories(&self) -> Result<Vec<Category>, Problem> {
        self.read("reading the tabs", |db| {
            let mut select = db.prepare_cached(
                "SELECT id, name, media, direction FROM category ORDER BY position, id",
            )?;
            let rows = select.query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })?;
            let mut categories = Vec::new();
            for row in rows {
                let (id, name, media, direction): (i64, String, String, String) = row?;
                // The database only allows known values; anything else is skipped, not fatal.
                match (media_from(&media), direction_from(&direction)) {
                    (Some(media), Some(direction)) => categories.push(Category {
                        id,
                        name,
                        media,
                        direction,
                    }),
                    _ => logs::warn(
                        "library",
                        format!("Tab {name:?} has an unknown kind; left out"),
                    )
                    .detail(format!("media {media:?}, direction {direction:?}"))
                    .write(),
                }
            }
            Ok(categories)
        })
    }

    /// The collections (playlists) of a tab, in their order, then by name.
    pub fn collections(&self, category_id: i64) -> Result<Vec<CollectionSummary>, Problem> {
        let media = self.media.clone();
        self.read("reading the playlists", move |db| {
            if !exists(db, "SELECT 1 FROM category WHERE id = ?1", category_id)? {
                return Ok(Err(no_such("tab", category_id)));
            }
            let mut select = db.prepare_cached(&format!(
                "{COLLECTION} WHERE c.category_id = ?1 ORDER BY c.position, c.name COLLATE NOCASE, c.id"
            ))?;
            let rows = select.query_map([category_id], |row| collection_row(row, &media))?;
            Ok(Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?))
        })?
    }

    pub fn collection(&self, id: i64) -> Result<CollectionSummary, Problem> {
        let media = self.media.clone();
        self.read("reading a playlist", move |db| {
            db.prepare_cached(&format!("{COLLECTION} WHERE c.id = ?1"))?
                .query_row([id], |row| collection_row(row, &media))
                .optional()
        })?
        .ok_or_else(|| no_such("playlist", id))
    }

    /// A page of a collection's entries, in order. `after` is the `next` of the page before
    /// (None for the first page); `limit` 0 means DEFAULT_PAGE, and at most MAX_PAGE.
    pub fn collection_items(
        &self,
        collection_id: i64,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Page<CollectionEntry>, Problem> {
        let start = match after {
            Some(cursor) => Some(decode_cursor(cursor)?),
            None => None,
        };
        let limit = if limit == 0 {
            DEFAULT_PAGE
        } else {
            limit.min(MAX_PAGE)
        };
        let media = self.media.clone();
        self.read("reading a playlist's items", move |db| {
            let smart: Option<bool> = db
                .prepare_cached("SELECT query IS NOT NULL FROM collection WHERE id = ?1")?
                .query_row([collection_id], |row| row.get(0))
                .optional()?;
            match smart {
                None => return Ok(Err(no_such("playlist", collection_id))),
                Some(true) => {
                    return Ok(Err(Problem {
                        retry: false,
                        ..Problem::new(
                            ProblemKind::Input,
                            "This list is filled by a search, which comes in a later version.",
                            format!("collection {collection_id} is a smart list"),
                        )
                    }));
                }
                Some(false) => {}
            }
            let (position, entry) = start.unwrap_or((f64::NEG_INFINITY, i64::MIN));
            let mut select = db.prepare_cached(&format!(
                "SELECT ci.id, ci.position, {SUMMARY}
                 FROM collection_item ci JOIN item i ON i.id = ci.item_id {SUMMARY_JOINS}
                 WHERE ci.collection_id = ?1 AND (ci.position, ci.id) > (?2, ?3)
                 ORDER BY ci.position, ci.id LIMIT ?4"
            ))?;
            let found = select
                .query_map(
                    params![collection_id, position, entry, limit as i64 + 1],
                    |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, f64>(1)?,
                            summary_row(row, 2, &media)?,
                        ))
                    },
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let more = found.len() > limit;
            let mut rows = Vec::with_capacity(found.len().min(limit));
            for (entry_id, position, summary) in found.into_iter().take(limit) {
                rows.push(CollectionEntry {
                    entry_id,
                    position,
                    item: with_artists(db, summary)?,
                });
            }
            let next = more
                .then(|| {
                    rows.last()
                        .map(|last| encode_cursor(last.position, last.entry_id))
                })
                .flatten();
            Ok(Ok(Page { rows, next }))
        })?
    }

    pub fn item(&self, id: i64) -> Result<ItemDetail, Problem> {
        let media = self.media.clone();
        self.read("reading an item", move |db| {
            let found = db
                .prepare_cached(&format!(
                    "SELECT {SUMMARY}, i.parent_id, i.original_title, i.summary, i.page_count, i.added_at,
                       CASE json_type(i.extra, '$.album') WHEN 'text' THEN json_extract(i.extra, '$.album') END,
                       CASE json_type(i.extra, '$.url') WHEN 'text' THEN json_extract(i.extra, '$.url') END
                     FROM item i {SUMMARY_JOINS} WHERE i.id = ?1"
                ))?
                .query_row([id], |row| {
                    let base = SUMMARY_COLUMNS;
                    Ok((
                        summary_row(row, 0, &media)?,
                        row.get(base)?,
                        row.get(base + 1)?,
                        row.get(base + 2)?,
                        row.get(base + 3)?,
                        row.get(base + 4)?,
                        row.get(base + 5)?,
                        row.get(base + 6)?,
                    ))
                })
                .optional()?;
            let Some((summary, parent_id, original_title, description, page_count, added_at, album, source_url)) =
                found
            else {
                return Ok(Err(no_such("item", id)));
            };
            let summary = with_artists(db, summary)?;
            let tags = db
                .prepare_cached(
                    "SELECT t.kind, t.name FROM item_tag it JOIN tag t ON t.id = it.tag_id
                     WHERE it.item_id = ?1 ORDER BY t.kind, t.id",
                )?
                .query_map([id], |row| {
                    Ok(Tag {
                        kind: row.get(0)?,
                        name: row.get(1)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(Ok(ItemDetail {
                summary,
                parent_id,
                original_title: non_empty(original_title),
                description: non_empty(description),
                page_count,
                added_at,
                album: non_empty(album),
                source_url: non_empty(source_url),
                tags,
            }))
        })?
    }

    /// The file to play or open: the real path, after checking that it exists, is a
    /// regular file, and lies inside the item's library folder and the media folder.
    pub fn item_file(&self, id: i64) -> Result<PathBuf, Problem> {
        let found = self.read("finding an item's file", move |db| {
            db.prepare_cached(
                "SELECT i.kind, i.path, l.root FROM item i LEFT JOIN library l ON l.id = i.library_id
                 WHERE i.id = ?1",
            )?
            .query_row([id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?, row.get::<_, Option<String>>(2)?))
            })
            .optional()
        })?;
        let Some((kind, path, root)) = found else {
            return Err(no_such("item", id));
        };
        if kind != "entry" {
            return Err(refused(
                "This is a folder of items, not a file.",
                format!("item {id} is a {kind}"),
            ));
        }
        let (Some(path), Some(root)) = (path, root) else {
            return Err(missing(format!(
                "item {id} has no file or no library folder"
            )));
        };
        let relative = Path::new(&path);
        if !relative
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
        {
            return Err(refused(
                "This item's file is not inside its library folder.",
                format!("item {id} has the path {path:?}"),
            ));
        }
        let root = match fs::canonicalize(located(&root, &self.media)) {
            Ok(root) => root,
            Err(err) => return Err(missing(format!("library folder {root}: {err}"))),
        };
        let file = match fs::canonicalize(root.join(relative)) {
            Ok(file) => file,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Err(missing(format!(
                    "item {id}: {}: {err}",
                    root.join(relative).display()
                )));
            }
            Err(err) => {
                return Err(problem::from_io(
                    &err,
                    &format!("finding the file of item {id}"),
                ));
            }
        };
        if !file.starts_with(&root) || !file.starts_with(&self.media) {
            return Err(refused(
                "This item's file is not inside the app's media folder.",
                format!("item {id} resolves to {}", file.display()),
            ));
        }
        if !file.is_file() {
            return Err(refused(
                "This item is not a file.",
                format!("item {id} resolves to {}", file.display()),
            ));
        }
        Ok(file)
    }

    /// Runs `read` on the connection; a broken connection is reopened and `read` runs once
    /// more.
    fn read<T>(
        &self,
        doing: &str,
        read: impl Fn(&Connection) -> rusqlite::Result<T>,
    ) -> Result<T, Problem> {
        let mut db = self.lock();
        for attempt in 0..2 {
            if db.is_none() {
                *db = Some(crate::db::open(&self.path).map_err(|err| failed(&err, doing))?);
            }
            let Some(connection) = db.as_ref() else {
                continue;
            };
            #[cfg(test)]
            let result = if self.take_failure() {
                Err(io_error())
            } else {
                read(connection)
            };
            #[cfg(not(test))]
            let result = read(connection);
            match result {
                Ok(value) => return Ok(value),
                Err(err) if attempt == 0 && reopening_helps(&err) => {
                    logs::warn("library", "The library connection failed; reopening it")
                        .detail(format!("{doing}: {err}"))
                        .write();
                    *db = None;
                }
                Err(err) => return Err(failed(&err, doing)),
            }
        }
        Err(Problem::bug(format!(
            "{doing}: the connection could not be reopened"
        )))
    }

    fn lock(&self) -> MutexGuard<'_, Option<Connection>> {
        self.db
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    #[cfg(test)]
    fn take_failure(&self) -> bool {
        use std::sync::atomic::Ordering;
        let mut left = self.fail_next.load(Ordering::SeqCst);
        while left > 0 {
            match self.fail_next.compare_exchange(
                left,
                left - 1,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => return true,
                Err(now) => left = now,
            }
        }
        false
    }
}

#[cfg(test)]
fn io_error() -> rusqlite::Error {
    rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(10),
        Some("test: disk I/O error".to_owned()),
    )
}

fn reopening_helps(err: &rusqlite::Error) -> bool {
    matches!(
        err.sqlite_error_code(),
        Some(ErrorCode::SystemIoFailure | ErrorCode::CannotOpen | ErrorCode::NotADatabase)
    )
}

fn failed(err: &rusqlite::Error, doing: &str) -> Problem {
    let problem = problem::from_sqlite(err, doing);
    problem::log(&problem, "library", None, None);
    problem
}

fn exists(db: &Connection, sql: &str, id: i64) -> rusqlite::Result<bool> {
    Ok(db
        .prepare_cached(sql)?
        .query_row([id], |_| Ok(()))
        .optional()?
        .is_some())
}

fn no_such(what: &str, id: i64) -> Problem {
    Problem {
        retry: false,
        ..Problem::new(
            ProblemKind::Unavailable,
            format!("This {what} is no longer in the library."),
            format!("no {what} with id {id}"),
        )
    }
}

fn missing(detail: String) -> Problem {
    Problem {
        retry: false,
        ..Problem::new(ProblemKind::Unavailable, "The file is missing.", detail)
    }
}

fn refused(text: &str, detail: String) -> Problem {
    Problem {
        retry: false,
        ..Problem::new(ProblemKind::Input, text, detail)
    }
}

fn media_from(text: &str) -> Option<Media> {
    Some(match text {
        "audio" => Media::Audio,
        "video" => Media::Video,
        "ebook" => Media::Ebook,
        "pages" => Media::Pages,
        _ => return None,
    })
}

fn direction_from(text: &str) -> Option<Direction> {
    Some(match text {
        "ltr" => Direction::Ltr,
        "rtl" => Direction::Rtl,
        "vertical" => Direction::Vertical,
        _ => return None,
    })
}

fn non_empty(text: Option<String>) -> Option<String> {
    text.filter(|text| !text.trim().is_empty())
}

/// Where a stored absolute path is now. iOS moves an app's folders when the app is
/// updated (the container's id changes), and Android can move them to another storage:
/// a stored path that no longer exists but runs through a folder named like the media
/// folder is taken to mean the same place inside today's media folder.
fn located(stored: &str, media: &Path) -> PathBuf {
    let path = Path::new(stored);
    if path.exists() || path.starts_with(media) {
        return path.to_owned();
    }
    let Some(name) = media.file_name() else {
        return path.to_owned();
    };
    let parts: Vec<Component> = path.components().collect();
    match parts.iter().rposition(|part| part.as_os_str() == name) {
        Some(at) => parts[at + 1..]
            .iter()
            .fold(media.to_owned(), |rebased, part| rebased.join(part)),
        None => path.to_owned(),
    }
}

/// An image to show: only if it is a file inside the media folder (absolute, or relative to
/// the item's library folder).
fn checked_cover(cover: Option<String>, root: Option<&str>, media: &Path) -> Option<PathBuf> {
    let cover = non_empty(cover)?;
    let path = Path::new(&cover);
    let path = if path.is_absolute() {
        located(&cover, media)
    } else {
        located(root?, media).join(path)
    };
    let real = fs::canonicalize(path).ok()?;
    (real.starts_with(media) && real.is_file()).then_some(real)
}

const COLLECTION: &str =
    "SELECT c.id, c.category_id, c.name, c.cover, c.source, c.query IS NOT NULL, c.created_at,
       (SELECT count(*) FROM collection_item ci WHERE ci.collection_id = c.id)
     FROM collection c";

fn collection_row(row: &Row, media: &Path) -> rusqlite::Result<CollectionSummary> {
    Ok(CollectionSummary {
        id: row.get(0)?,
        category_id: row.get(1)?,
        name: row.get(2)?,
        cover: checked_cover(row.get(3)?, None, media),
        source: non_empty(row.get(4)?),
        smart: row.get(5)?,
        created_at: row.get(6)?,
        entries: row.get(7)?,
    })
}

/// The columns `summary_row` reads, in order, and the joins they need.
const SUMMARY: &str =
    "i.id, i.kind, i.category_id, cat.media, i.title, i.year, i.duration_ms, i.group_no,
       i.number, i.cover, i.favorite, i.rating, i.missing_since, i.path, lib.root,
       p.position, p.percent, p.done, p.plays,
       CASE json_type(i.extra, '$.artist') WHEN 'text' THEN json_extract(i.extra, '$.artist') END";
const SUMMARY_COLUMNS: usize = 20;
const SUMMARY_JOINS: &str = "JOIN category cat ON cat.id = i.category_id
       LEFT JOIN library lib ON lib.id = i.library_id
       LEFT JOIN progress p ON p.item_id = i.id";

/// Reads SUMMARY starting at column `at`. Artists come separately (`with_artists`).
fn summary_row(row: &Row, at: usize, media: &Path) -> rusqlite::Result<ItemSummary> {
    let get_kind: String = row.get(at + 1)?;
    let kind = if get_kind == "group" {
        ItemKind::Group
    } else {
        ItemKind::Entry
    };
    let media_text: String = row.get(at + 3)?;
    let path: Option<String> = row.get(at + 13)?;
    let root: Option<String> = row.get(at + 14)?;
    let missing_since: Option<i64> = row.get(at + 12)?;
    let missing = missing_since.is_some()
        || (kind == ItemKind::Entry
            && match (&path, &root) {
                (Some(path), Some(root)) => !located(root, media).join(path).is_file(),
                _ => true,
            });
    let progress = match row.get::<_, Option<i64>>(at + 15)? {
        Some(position) => Some(Progress {
            position: position.max(0),
            percent: clamp_percent(row.get::<_, Option<f64>>(at + 16)?.unwrap_or(0.0)),
            done: row.get::<_, Option<bool>>(at + 17)?.unwrap_or(false),
            plays: row.get::<_, Option<i64>>(at + 18)?.unwrap_or(0).max(0),
        }),
        None => None,
    };
    Ok(ItemSummary {
        id: row.get(at)?,
        kind,
        category_id: row.get(at + 2)?,
        media: media_from(&media_text).unwrap_or(Media::Video),
        title: row.get(at + 4)?,
        artist: non_empty(row.get(at + 19)?),
        artists: Vec::new(),
        year: row.get(at + 5)?,
        duration_ms: row.get::<_, Option<i64>>(at + 6)?.filter(|ms| *ms >= 0),
        group_no: row.get(at + 7)?,
        number: row
            .get::<_, Option<f64>>(at + 8)?
            .filter(|number| number.is_finite()),
        cover: checked_cover(row.get(at + 9)?, root.as_deref(), media),
        favorite: row.get(at + 10)?,
        rating: row.get(at + 11)?,
        missing,
        progress,
    })
}

fn clamp_percent(percent: f64) -> f64 {
    if percent.is_finite() {
        percent.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// Fills in the artists, and the main artist from them when the item names none.
fn with_artists(db: &Connection, mut summary: ItemSummary) -> rusqlite::Result<ItemSummary> {
    summary.artists = db
        .prepare_cached(
            "SELECT t.name FROM item_tag it JOIN tag t ON t.id = it.tag_id
             WHERE it.item_id = ?1 AND t.kind = 'artist' ORDER BY t.id",
        )?
        .query_map([summary.id], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    if summary.artist.is_none() {
        summary.artist = summary.artists.first().cloned();
    }
    Ok(summary)
}

/// A page cursor: the position and entry id of the last row, as text the apps pass back.
fn encode_cursor(position: f64, entry: i64) -> String {
    format!("c1:{:016x}:{entry}", position.to_bits())
}

fn decode_cursor(cursor: &str) -> Result<(f64, i64), Problem> {
    let bad = || refused("That page link is not valid.", format!("cursor {cursor:?}"));
    let mut parts = cursor.split(':');
    if parts.next() != Some("c1") {
        return Err(bad());
    }
    let position = parts
        .next()
        .filter(|hex| hex.len() == 16)
        .and_then(|hex| u64::from_str_radix(hex, 16).ok())
        .map(f64::from_bits)
        .filter(|position| position.is_finite())
        .ok_or_else(bad)?;
    let entry = parts
        .next()
        .and_then(|id| id.parse::<i64>().ok())
        .ok_or_else(bad)?;
    if parts.next().is_some() {
        return Err(bad());
    }
    Ok((position, entry))
}
