//! The library, editing the person's own playlists: make, rename, copy and delete one;
//! add items, take entries out, move them.
//!
//! The rules every call keeps:
//!
//! - **A saved playlist follows its link.** Its entries come from the playlist it was saved
//!   from (saving again updates them), so they are not changed by hand: copy it first. Its
//!   name can change, and it can be deleted (its files stay in the library).
//! - **A smart playlist is filled by its search**: its entries are not edited either.
//! - **One tab.** A playlist holds items of its own tab, playable ones (not albums).
//! - **Order by position.** An entry goes between two others at the midpoint of their
//!   positions; when two positions get too close, the playlist is numbered 1, 2, 3 again.
//! - **All or nothing.** A call that changes several rows changes all of them or none.

use rusqlite::{OptionalExtension, Transaction, params};

use super::{Library, no_such, refused};
use crate::problem::Problem;

/// The longest playlist name, in characters.
pub const MAX_NAME: usize = 200;
/// Items added in one call at most.
pub const MAX_ADD: usize = 10_000;
/// Two positions closer than this: the playlist is numbered again.
const MIN_GAP: f64 = 1e-6;

/// What a playlist is, for editing.
struct Kind {
    category_id: i64,
    saved: bool,
    smart: bool,
}

fn kind_of(tx: &Transaction, id: i64) -> rusqlite::Result<Option<Kind>> {
    tx.prepare_cached(
        "SELECT category_id, source IS NOT NULL, query IS NOT NULL FROM collection WHERE id = ?1",
    )?
    .query_row([id], |row| {
        Ok(Kind {
            category_id: row.get(0)?,
            saved: row.get(1)?,
            smart: row.get(2)?,
        })
    })
    .optional()
}

/// A playlist whose entries may be edited, or why not.
fn editable(tx: &Transaction, id: i64) -> rusqlite::Result<Result<Kind, Problem>> {
    Ok(match kind_of(tx, id)? {
        None => Err(no_such("playlist", id)),
        Some(kind) if kind.smart => Err(refused(
            "This list is filled by a search: its entries cannot be changed by hand.",
            format!("collection {id} is a smart list"),
        )),
        Some(kind) if kind.saved => Err(refused(
            "This playlist follows the playlist it was saved from. Copy it to change it.",
            format!("collection {id} has a source"),
        )),
        Some(kind) => Ok(kind),
    })
}

fn checked_name(name: &str) -> Result<String, Problem> {
    let name: String = name.split_whitespace().collect::<Vec<_>>().join(" ");
    if name.is_empty() {
        return Err(refused("A playlist needs a name.", "empty name".to_owned()));
    }
    if name.chars().count() > MAX_NAME {
        return Err(refused(
            &format!("A playlist name can have {MAX_NAME} characters at most."),
            format!("{} characters", name.chars().count()),
        ));
    }
    Ok(name)
}

/// Numbers a playlist's entries 1, 2, 3 in their order (positions got too close).
fn renumber(tx: &Transaction, id: i64) -> rusqlite::Result<()> {
    let entries: Vec<i64> = tx
        .prepare_cached(
            "SELECT id FROM collection_item WHERE collection_id = ?1 ORDER BY position, id",
        )?
        .query_map([id], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut update = tx.prepare_cached("UPDATE collection_item SET position = ?2 WHERE id = ?1")?;
    for (at, entry) in entries.iter().enumerate() {
        update.execute(params![entry, at as f64 + 1.0])?;
    }
    Ok(())
}

/// The position of an entry, if it is in the playlist.
fn position_of(tx: &Transaction, id: i64, entry: i64) -> rusqlite::Result<Option<f64>> {
    tx.prepare_cached("SELECT position FROM collection_item WHERE id = ?1 AND collection_id = ?2")?
        .query_row(params![entry, id], |row| row.get(0))
        .optional()
}

/// The position right before `before` (or after the last entry, for none), renumbering
/// the playlist first when there is no room between the two.
fn slot(
    tx: &Transaction,
    id: i64,
    before: Option<i64>,
    skip: Option<i64>,
) -> rusqlite::Result<Result<f64, Problem>> {
    for attempt in 0..2 {
        let found = match before {
            None => {
                let last: Option<f64> = tx
                    .prepare_cached(
                        "SELECT MAX(position) FROM collection_item WHERE collection_id = ?1 AND id IS NOT ?2",
                    )?
                    .query_row(params![id, skip], |row| row.get(0))?;
                return Ok(Ok(last.map_or(1.0, |last| last.floor() + 1.0)));
            }
            Some(entry) => {
                let Some(next) = position_of(tx, id, entry)? else {
                    return Ok(Err(no_such("playlist entry", entry)));
                };
                let previous: Option<f64> = tx
                    .prepare_cached(
                        "SELECT position FROM collection_item WHERE collection_id = ?1 AND id IS NOT ?4
                           AND (position, id) < (?2, ?3) ORDER BY position DESC, id DESC LIMIT 1",
                    )?
                    .query_row(params![id, next, entry, skip], |row| row.get(0))
                    .optional()?;
                (previous.unwrap_or(next - 1.0), next)
            }
        };
        let (low, high) = found;
        let middle = (low + high) / 2.0;
        if high - low >= MIN_GAP && middle > low && middle < high {
            return Ok(Ok(middle));
        }
        if attempt == 0 {
            renumber(tx, id)?;
        }
    }
    Ok(Err(Problem::bug(format!(
        "no room in playlist {id} after numbering it again"
    ))))
}

impl Library {
    /// Makes an empty playlist in a tab; returns its id.
    pub fn create_collection(&self, category_id: i64, name: &str) -> Result<i64, Problem> {
        let name = checked_name(name)?;
        self.change("making a playlist", move |tx| {
            if !super::exists(tx, "SELECT 1 FROM category WHERE id = ?1", category_id)? {
                return Ok(Err(no_such("tab", category_id)));
            }
            let id = tx
                .prepare_cached(
                    "INSERT INTO collection (category_id, name, position)
                     VALUES (?1, ?2, (SELECT coalesce(MAX(position), 0) + 1 FROM collection WHERE category_id = ?1))
                     RETURNING id",
                )?
                .query_row(params![category_id, name], |row| row.get(0))?;
            Ok(Ok(id))
        })
    }

    pub fn rename_collection(&self, id: i64, name: &str) -> Result<(), Problem> {
        let name = checked_name(name)?;
        self.change("renaming a playlist", move |tx| {
            let changed = tx
                .prepare_cached("UPDATE collection SET name = ?2 WHERE id = ?1")?
                .execute(params![id, name])?;
            Ok(if changed == 0 {
                Err(no_such("playlist", id))
            } else {
                Ok(())
            })
        })
    }

    /// Deletes a playlist: its entries go, the items and their files stay in the library.
    pub fn delete_collection(&self, id: i64) -> Result<(), Problem> {
        self.change("deleting a playlist", move |tx| {
            let changed = tx
                .prepare_cached("DELETE FROM collection WHERE id = ?1")?
                .execute([id])?;
            Ok(if changed == 0 {
                Err(no_such("playlist", id))
            } else {
                Ok(())
            })
        })
    }

    /// A new playlist of the person's own with the same entries in the same order (a saved
    /// one too: the copy no longer follows the link). Returns its id.
    pub fn copy_collection(&self, id: i64, name: &str) -> Result<i64, Problem> {
        let name = checked_name(name)?;
        self.change("copying a playlist", move |tx| {
            let Some(kind) = kind_of(tx, id)? else {
                return Ok(Err(no_such("playlist", id)));
            };
            if kind.smart {
                return Ok(Err(refused(
                    "This list is filled by a search, which comes in a later version.",
                    format!("collection {id} is a smart list"),
                )));
            }
            let copy: i64 = tx
                .prepare_cached(
                    "INSERT INTO collection (category_id, name, cover, position)
                     SELECT category_id, ?2, cover,
                       (SELECT coalesce(MAX(position), 0) + 1 FROM collection WHERE category_id = c.category_id)
                     FROM collection c WHERE id = ?1 RETURNING id",
                )?
                .query_row(params![id, name], |row| row.get(0))?;
            // Numbered 1, 2, 3 in the original's order.
            tx.prepare_cached(
                "INSERT INTO collection_item (collection_id, item_id, position)
                 SELECT ?2, item_id, ROW_NUMBER() OVER (ORDER BY position, id)
                 FROM collection_item WHERE collection_id = ?1",
            )?
            .execute(params![id, copy])?;
            Ok(Ok(copy))
        })
    }

    /// Adds items before the entry `before` (none: at the end), in the given order; an item
    /// can be added more than once. Returns the new entries' ids.
    pub fn add_to_collection(
        &self,
        id: i64,
        items: &[i64],
        before: Option<i64>,
    ) -> Result<Vec<i64>, Problem> {
        if items.len() > MAX_ADD {
            return Err(refused(
                &format!("Add {MAX_ADD} items at most at once."),
                format!("{} items", items.len()),
            ));
        }
        let items = items.to_vec();
        self.change("adding to a playlist", move |tx| {
            let kind = match editable(tx, id)? {
                Ok(kind) => kind,
                Err(problem) => return Ok(Err(problem)),
            };
            // Every item first: all are added or none.
            for &item in &items {
                let found: Option<(i64, String)> = tx
                    .prepare_cached("SELECT category_id, kind FROM item WHERE id = ?1")?
                    .query_row([item], |row| Ok((row.get(0)?, row.get(1)?)))
                    .optional()?;
                match found {
                    None => return Ok(Err(no_such("item", item))),
                    Some((_, kind_text)) if kind_text != "entry" => {
                        return Ok(Err(refused(
                            "Albums and shows cannot go in a playlist; their tracks or episodes can.",
                            format!("item {item} is a {kind_text}"),
                        )));
                    }
                    Some((category, _)) if category != kind.category_id => {
                        return Ok(Err(refused(
                            "That item belongs to another tab than this playlist.",
                            format!("item {item} is in tab {category}, the playlist in {}", kind.category_id),
                        )));
                    }
                    Some(_) => {}
                }
            }
            let mut added = Vec::with_capacity(items.len());
            let mut insert = tx.prepare_cached(
                "INSERT INTO collection_item (collection_id, item_id, position) VALUES (?1, ?2, ?3) RETURNING id",
            )?;
            for &item in &items {
                // Each before `before`, after the ones added already.
                let position = match slot(tx, id, before, None)? {
                    Ok(position) => position,
                    Err(problem) => return Ok(Err(problem)),
                };
                added.push(insert.query_row(params![id, item, position], |row| row.get(0))?);
            }
            Ok(Ok(added))
        })
    }

    /// Takes entries out of a playlist (the items stay in the library). Entries that are
    /// not in it are refused, and then nothing is taken out.
    pub fn remove_from_collection(&self, id: i64, entries: &[i64]) -> Result<(), Problem> {
        let entries = entries.to_vec();
        self.change("taking entries out of a playlist", move |tx| {
            if let Err(problem) = editable(tx, id)? {
                return Ok(Err(problem));
            }
            let mut delete = tx.prepare_cached(
                "DELETE FROM collection_item WHERE id = ?1 AND collection_id = ?2",
            )?;
            for &entry in &entries {
                if delete.execute(params![entry, id])? == 0 {
                    // The transaction is dropped, not committed: nothing was taken out.
                    return Ok(Err(no_such("playlist entry", entry)));
                }
            }
            Ok(Ok(()))
        })
    }

    /// Moves an entry right before `before` (none: to the end).
    pub fn move_in_collection(
        &self,
        id: i64,
        entry: i64,
        before: Option<i64>,
    ) -> Result<(), Problem> {
        self.change("moving a playlist entry", move |tx| {
            if let Err(problem) = editable(tx, id)? {
                return Ok(Err(problem));
            }
            if position_of(tx, id, entry)?.is_none() {
                return Ok(Err(no_such("playlist entry", entry)));
            }
            if before == Some(entry) {
                return Ok(Ok(()));
            }
            // Its own position does not count among its neighbours.
            let position = match slot(tx, id, before, Some(entry))? {
                Ok(position) => position,
                Err(problem) => return Ok(Err(problem)),
            };
            tx.prepare_cached("UPDATE collection_item SET position = ?2 WHERE id = ?1")?
                .execute(params![entry, position])?;
            Ok(Ok(()))
        })
    }
}
