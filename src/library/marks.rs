//! The library's write side, step 3: where the person stopped (resume points), what they
//! finished and how often, favorites and ratings. The resume rules live here, so every
//! player follows the same ones:
//!
//! - **Songs and clips start over.** Audio shorter than 20 minutes and video shorter than 5
//!   minutes always start from the beginning. Longer audio (audiobooks, podcasts) and
//!   longer video resume where they stopped.
//! - **No resume point before 30 s,** and none once the item is done.
//! - **Done** at 95 %, or, for items of 10 minutes and more, with 2 minutes or less left
//!   (the credits). A done item starts over next time.
//! - **Plays count once per play:** `start_play` begins a play; the first save that
//!   reaches done counts it, and seeking back and forth near the end does not count it
//!   again. Starting over and finishing again is a new play.
//! - **Progress belongs to the item,** not to a playlist: the same resume point wherever the
//!   item is opened from.
//! - **A save for an item that is gone does nothing** (the player may save after a rescan
//!   removed it). Marks the person sets on a gone item say so.

use rusqlite::{OptionalExtension, Transaction, params};

use super::{ItemKind, Library, Media, media_from, no_such, refused};
use crate::problem::Problem;

/// Plays remembered for counting (one small entry per item played since the app started).
const COUNTED_KEPT: usize = 10_000;

/// The thresholds of the resume rules.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rules {
    /// Before this much, no resume point is kept.
    pub min_resume_ms: i64,
    /// Done from this share of the length on...
    pub done_share: f64,
    /// ...or with this little left, for items at least `credits_from_ms` long.
    pub done_left_ms: i64,
    pub credits_from_ms: i64,
    /// Video shorter than this always starts over (clips).
    pub short_video_ms: i64,
    /// Audio shorter than this always starts over (songs).
    pub long_audio_ms: i64,
}

impl Default for Rules {
    fn default() -> Self {
        Rules {
            min_resume_ms: 30_000,
            done_share: 0.95,
            done_left_ms: 120_000,
            credits_from_ms: 600_000,
            short_video_ms: 300_000,
            long_audio_ms: 1_200_000,
        }
    }
}

impl Rules {
    /// Whether an item of this kind and length keeps a resume point at all. A video of
    /// unknown length does (it is probably long); audio of unknown length does not.
    pub fn resumes(&self, media: Media, duration_ms: Option<i64>) -> bool {
        match media {
            Media::Video => duration_ms.is_none_or(|duration| duration >= self.short_video_ms),
            Media::Audio => duration_ms.is_some_and(|duration| duration >= self.long_audio_ms),
            Media::Ebook | Media::Pages => false,
        }
    }

    /// Whether playback at `position_ms` means the item is finished. Never without a length.
    pub fn done(&self, position_ms: i64, duration_ms: Option<i64>) -> bool {
        let Some(duration) = duration_ms.filter(|duration| *duration > 0) else {
            return false;
        };
        position_ms as f64 >= duration as f64 * self.done_share
            || (duration >= self.credits_from_ms && duration - position_ms <= self.done_left_ms)
    }

    /// The resume point to keep for playback at `position_ms`: 0 when there is none.
    fn resume_point(&self, media: Media, position_ms: i64, duration_ms: Option<i64>) -> i64 {
        if !self.resumes(media, duration_ms)
            || self.done(position_ms, duration_ms)
            || position_ms < self.min_resume_ms
        {
            0
        } else {
            position_ms
        }
    }
}

/// Where to start an item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlayStart {
    pub position_ms: i64,
    /// True when that is a resume point (the screen can say "Resuming at 12:34").
    pub resumed: bool,
}

/// What a write found.
enum Found {
    Gone,
    Group,
    Unplayable,
    Done(bool),
}

/// An item's kind, media and stored length.
type ItemInfo = (ItemKind, Option<Media>, Option<i64>);

/// The item's kind, media and stored length, inside a write.
fn item_info(tx: &Transaction, id: i64) -> rusqlite::Result<Option<ItemInfo>> {
    tx.prepare_cached(
        "SELECT i.kind, cat.media, i.duration_ms FROM item i JOIN category cat ON cat.id = i.category_id
         WHERE i.id = ?1",
    )?
    .query_row([id], |row| {
        let kind: String = row.get(0)?;
        let media: String = row.get(1)?;
        Ok((
            if kind == "group" { ItemKind::Group } else { ItemKind::Entry },
            media_from(&media),
            row.get::<_, Option<i64>>(2)?.filter(|duration| *duration > 0),
        ))
    })
    .optional()
}

fn not_a_file(id: i64) -> Problem {
    refused(
        "This is a folder of items, not something to play.",
        format!("item {id} is a group"),
    )
}

fn not_playable(id: i64) -> Problem {
    refused(
        "Reading progress comes in a later version.",
        format!("item {id} is a book or comic"),
    )
}

impl Library {
    /// Begins playing an item: where to start (0, or its resume point), and a new play that
    /// counts once when it reaches done.
    pub fn start_play(&self, id: i64) -> Result<PlayStart, Problem> {
        let rules = self.rules;
        let found = self.read("starting to play an item", move |db| {
            db.prepare_cached(
                "SELECT i.kind, cat.media, i.duration_ms, p.position, p.done
                 FROM item i JOIN category cat ON cat.id = i.category_id
                 LEFT JOIN progress p ON p.item_id = i.id WHERE i.id = ?1",
            )?
            .query_row([id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<i64>>(2)?
                        .filter(|duration| *duration > 0),
                    row.get::<_, Option<i64>>(3)?.unwrap_or(0),
                    row.get::<_, Option<bool>>(4)?.unwrap_or(false),
                ))
            })
            .optional()
        })?;
        let Some((kind, media, duration, position, done)) = found else {
            return Err(no_such("item", id));
        };
        if kind == "group" {
            return Err(not_a_file(id));
        }
        let media = match media_from(&media) {
            Some(media @ (Media::Audio | Media::Video)) => media,
            _ => return Err(not_playable(id)),
        };
        let mut counted = self.counted();
        // Bounded: plays already counted can be forgotten first; past the bound, all of
        // them (at worst one play is counted twice).
        if counted.len() >= COUNTED_KEPT {
            counted.retain(|_, done| !*done);
            if counted.len() >= COUNTED_KEPT {
                counted.clear();
            }
        }
        counted.insert(id, false);
        drop(counted);
        let resumable = rules.resumes(media, duration)
            && !done
            && position >= rules.min_resume_ms
            && duration.is_none_or(|duration| position < duration);
        Ok(if resumable {
            PlayStart {
                position_ms: position,
                resumed: true,
            }
        } else {
            PlayStart {
                position_ms: 0,
                resumed: false,
            }
        })
    }

    /// Saves where playback of an item is (`duration_ms` as the player knows it, which also
    /// corrects the stored length). Call it on pause, every few seconds, and at the end.
    /// False when the item is gone: there is nothing to save then, and that is fine.
    pub fn save_progress(
        &self,
        id: i64,
        position_ms: i64,
        duration_ms: Option<i64>,
    ) -> Result<bool, Problem> {
        let rules = self.rules;
        let reported = duration_ms.filter(|duration| *duration > 0);
        // Held across the write, so two saves of the same play cannot both count it.
        let mut counted = self.counted();
        let already = counted.get(&id).copied().unwrap_or(false);
        let found = self.write("saving where playback is", move |tx| {
            let Some((kind, media, stored)) = item_info(tx, id)? else {
                return Ok(Found::Gone);
            };
            if kind == ItemKind::Group {
                return Ok(Found::Group);
            }
            let Some(media @ (Media::Audio | Media::Video)) = media else {
                return Ok(Found::Unplayable);
            };
            // The player knows the real length; the library learns it.
            if let Some(duration) = reported
                && stored.is_none_or(|stored| (stored - duration).abs() > 1000)
            {
                tx.prepare_cached("UPDATE item SET duration_ms = ?2 WHERE id = ?1")?
                    .execute(params![id, duration])?;
            }
            let duration = reported.or(stored);
            let position = match duration {
                Some(duration) => position_ms.clamp(0, duration),
                None => position_ms.max(0),
            };
            let done = rules.done(position, duration);
            let resume = rules.resume_point(media, position, duration);
            let share = match duration {
                _ if done => 1.0,
                Some(duration) => (position as f64 / duration as f64).clamp(0.0, 1.0),
                None => 0.0,
            };
            let count = i64::from(done && !already);
            tx.prepare_cached(
                "INSERT INTO progress (item_id, position, percent, done, plays, opened_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, unixepoch())
                 ON CONFLICT (item_id) DO UPDATE SET position = excluded.position,
                   percent = excluded.percent, done = excluded.done,
                   plays = progress.plays + excluded.plays, opened_at = excluded.opened_at",
            )?
            .execute(params![id, resume, share, done, count])?;
            Ok(Found::Done(done))
        })?;
        match found {
            Found::Gone => Ok(false),
            Found::Group => Err(not_a_file(id)),
            Found::Unplayable => Err(not_playable(id)),
            Found::Done(done) => {
                if done {
                    counted.insert(id, true);
                }
                Ok(true)
            }
        }
    }

    /// Marks an item watched (done, starts over) or not watched (no resume point). The
    /// play count stays as it is.
    pub fn set_watched(&self, id: i64, watched: bool) -> Result<(), Problem> {
        let found = self.write("marking an item watched", move |tx| {
            let Some((kind, _, _)) = item_info(tx, id)? else {
                return Ok(Found::Gone);
            };
            if kind == ItemKind::Group {
                return Ok(Found::Group);
            }
            tx.prepare_cached(
                "INSERT INTO progress (item_id, position, percent, done) VALUES (?1, 0, ?2, ?3)
                 ON CONFLICT (item_id) DO UPDATE SET position = 0, percent = excluded.percent,
                   done = excluded.done",
            )?
            .execute(params![id, if watched { 1.0 } else { 0.0 }, watched])?;
            Ok(Found::Done(watched))
        })?;
        match found {
            Found::Gone => Err(no_such("item", id)),
            Found::Group => Err(not_a_file(id)),
            _ => Ok(()),
        }
    }

    /// Albums, shows and single items can all be favorites.
    pub fn set_favorite(&self, id: i64, favorite: bool) -> Result<(), Problem> {
        let changed = self.write("marking a favorite", move |tx| {
            tx.prepare_cached("UPDATE item SET favorite = ?2 WHERE id = ?1")?
                .execute(params![id, favorite])
        })?;
        if changed == 0 {
            Err(no_such("item", id))
        } else {
            Ok(())
        }
    }

    /// A rating from 1 to 10, or None to clear it.
    pub fn set_rating(&self, id: i64, rating: Option<i64>) -> Result<(), Problem> {
        if rating.is_some_and(|rating| !(1..=10).contains(&rating)) {
            return Err(refused(
                "A rating goes from 1 to 10.",
                format!("rating {rating:?}"),
            ));
        }
        let changed = self.write("rating an item", move |tx| {
            tx.prepare_cached("UPDATE item SET rating = ?2 WHERE id = ?1")?
                .execute(params![id, rating])
        })?;
        if changed == 0 {
            Err(no_such("item", id))
        } else {
            Ok(())
        }
    }

    /// Adds a file that is already in a tab's folder (`<media>/<tab name>/...`) to the
    /// library and returns its item id; the same file again gives the same id. The title is
    /// the file's name until something better is known.
    pub fn add_file(&self, category_id: i64, path: &std::path::Path) -> Result<i64, Problem> {
        let real = match std::fs::canonicalize(path) {
            Ok(real) => real,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Err(super::missing(format!("{}: {err}", path.display())));
            }
            Err(err) => {
                return Err(crate::problem::from_io(
                    &err,
                    &format!("adding {}", path.display()),
                ));
            }
        };
        if !real.is_file() {
            return Err(refused(
                "Only files can be added.",
                format!("{} is not a file", real.display()),
            ));
        }
        let media = self.media.clone();
        let tab: Option<String> = self.read("finding a tab", move |db| {
            db.prepare_cached("SELECT name FROM category WHERE id = ?1")?
                .query_row([category_id], |row| row.get(0))
                .optional()
        })?;
        let Some(tab) = tab else {
            return Err(no_such("tab", category_id));
        };
        let root = media.join(&tab);
        let Ok(relative) = real.strip_prefix(&root).map(std::path::Path::to_owned) else {
            return Err(refused(
                "Files for this tab go into its own folder in the app's media folder.",
                format!("{} is not inside {}", real.display(), root.display()),
            ));
        };
        let (Some(relative), Some(root_text)) = (
            relative.to_str().map(str::to_owned),
            root.to_str().map(str::to_owned),
        ) else {
            return Err(refused(
                "That file's name cannot be used.",
                format!("not UTF-8: {}", real.display()),
            ));
        };
        let title = real.file_stem().map_or_else(
            || "Untitled".to_owned(),
            |stem| stem.to_string_lossy().into_owned(),
        );
        self.write("adding a file", move |tx| {
            let library: i64 = tx.query_row(
                "INSERT INTO library (category_id, root) VALUES (?1, ?2)
                 ON CONFLICT (root) DO UPDATE SET root = excluded.root RETURNING id",
                params![category_id, root_text],
                |row| row.get(0),
            )?;
            tx.query_row(
                "INSERT INTO item (category_id, library_id, kind, path, title, sort_title)
                 VALUES (?1, ?2, 'entry', ?3, ?4, lower(?4))
                 ON CONFLICT (library_id, path) DO UPDATE SET missing_since = NULL
                 RETURNING id",
                params![category_id, library, relative, title],
                |row| row.get(0),
            )
        })
    }

    fn counted(&self) -> std::sync::MutexGuard<'_, std::collections::HashMap<i64, bool>> {
        self.counted
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn songs_and_clips_start_over_long_things_resume() {
        let rules = Rules::default();
        let minute = 60_000;
        assert!(!rules.resumes(Media::Audio, Some(4 * minute)), "a song");
        assert!(!rules.resumes(Media::Audio, Some(20 * minute - 1)));
        assert!(
            rules.resumes(Media::Audio, Some(20 * minute)),
            "an audiobook chapter"
        );
        assert!(
            !rules.resumes(Media::Audio, None),
            "audio of unknown length is a song"
        );
        assert!(!rules.resumes(Media::Video, Some(5 * minute - 1)), "a clip");
        assert!(rules.resumes(Media::Video, Some(5 * minute)));
        assert!(
            rules.resumes(Media::Video, None),
            "video of unknown length resumes"
        );
        assert!(!rules.resumes(Media::Ebook, Some(10 * minute)));
        assert!(!rules.resumes(Media::Pages, None));
    }

    #[test]
    fn done_at_95_percent_or_in_the_credits() {
        let rules = Rules::default();
        let minute = 60_000;
        // 95 %, exactly.
        assert!(!rules.done(94_999, Some(100_000)));
        assert!(rules.done(95_000, Some(100_000)));
        // The last 2 minutes of an item 10 minutes or longer: an episode's credits come
        // before its 95 % (past 40 minutes, 95 % comes first anyway).
        assert!(!rules.done(30 * minute - 120_001, Some(30 * minute)));
        assert!(rules.done(30 * minute - 120_000, Some(30 * minute)));
        assert!(!rules.done(90 * minute * 95 / 100 - 1, Some(90 * minute)));
        assert!(
            rules.done(90 * minute * 95 / 100, Some(90 * minute)),
            "a movie: 95 %"
        );
        assert!(
            rules.done(8 * minute, Some(10 * minute)),
            "10 minutes: credits count"
        );
        assert!(
            !rules.done(7 * minute, Some(10 * minute - 1)),
            "under 10 minutes: only 95 %"
        );
        // Without a length, never.
        assert!(!rules.done(500 * minute, None));
        assert!(!rules.done(10, Some(0)));
        assert!(!rules.done(10, Some(-5)));
    }

    #[test]
    fn resume_points_start_at_30_seconds_and_end_when_done() {
        let rules = Rules::default();
        let movie = Some(100 * 60_000);
        assert_eq!(rules.resume_point(Media::Video, 29_999, movie), 0);
        assert_eq!(rules.resume_point(Media::Video, 30_000, movie), 30_000);
        assert_eq!(
            rules.resume_point(Media::Video, 99 * 60_000, movie),
            0,
            "in the credits: done"
        );
        assert_eq!(
            rules.resume_point(Media::Audio, 120_000, Some(180_000)),
            0,
            "a song"
        );
    }
}
