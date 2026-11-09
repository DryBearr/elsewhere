//! The library, browsing: everything a tab holds (by title, newest, last played), its
//! artists and albums with what is theirs, and search. Long lists come a page at a time
//! with a cursor of their own, as playlists do; the same rules hold (bad data never fails
//! a list, ids not paths).
//!
//! Artists are the item's artist tags and the artist its saved data names; albums are the
//! album its saved data names. Names that differ only in case are one artist or album.

use rusqlite::{Connection, params};

use super::{
    CollectionSummary, DEFAULT_PAGE, ItemSummary, Library, MAX_PAGE, Page, SUMMARY, SUMMARY_JOINS,
    collection_row, no_such, refused, summary_row, with_artists,
};
use crate::problem::Problem;

/// How a tab's items are listed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Order {
    /// A to Z (the sort title, then the id).
    Title,
    /// Newest first.
    Added,
    /// Last played first; only what was played.
    Played,
}

/// An artist or album: its name and how many of the tab's items it has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Named {
    pub name: String,
    pub items: i64,
    /// An item of it, for a picture (an album's first track).
    pub first_item: i64,
}

/// What a search found.
#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    pub items: Vec<ItemSummary>,
    pub collections: Vec<CollectionSummary>,
    pub artists: Vec<Named>,
}

/// Search terms at most, and the longest term.
const MAX_TERMS: usize = 8;
const MAX_TERM: usize = 100;
/// Rows a search gives of each kind at most.
const SEARCH_ROWS: usize = 50;

/// Every name an item goes by as an artist: its artist tags and its saved artist.
const ARTISTS: &str = "SELECT t.name AS name, i.id AS item FROM tag t
       JOIN item_tag it ON it.tag_id = t.id JOIN item i ON i.id = it.item_id
       WHERE t.kind = 'artist' AND i.category_id = ?1 AND i.kind = 'entry' AND trim(t.name) <> ''
     UNION
     SELECT json_extract(i.extra, '$.artist'), i.id FROM item i
       WHERE i.category_id = ?1 AND i.kind = 'entry' AND json_type(i.extra, '$.artist') = 'text'
         AND trim(json_extract(i.extra, '$.artist')) <> ''";

/// The album an item's saved data names (as an expression on `i`).
const ALBUM: &str = "CASE json_type(i.extra, '$.album') WHEN 'text' THEN trim(json_extract(i.extra, '$.album')) END";

fn limit_of(limit: usize) -> usize {
    if limit == 0 {
        DEFAULT_PAGE
    } else {
        limit.min(MAX_PAGE)
    }
}

/// A page cursor for a browsing list: which list, the last row's sort key, its id.
fn encode(list: char, key: &str, id: i64) -> String {
    let hex: String = key.bytes().map(|byte| format!("{byte:02x}")).collect();
    format!("b1:{list}:{hex}:{id}")
}

fn decode(cursor: &str, list: char) -> Result<(String, i64), Problem> {
    let bad = || refused("That page link is not valid.", format!("cursor {cursor:?}"));
    let mut parts = cursor.split(':');
    if parts.next() != Some("b1") || parts.next() != Some(&list.to_string()) {
        return Err(bad());
    }
    let hex = parts.next().ok_or_else(bad)?;
    if hex.len() % 2 != 0 || hex.len() > 4096 {
        return Err(bad());
    }
    let bytes = (0..hex.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&hex[at..at + 2], 16))
        .collect::<Result<Vec<u8>, _>>()
        .map_err(|_| bad())?;
    let key = String::from_utf8(bytes).map_err(|_| bad())?;
    let id = parts
        .next()
        .and_then(|id| id.parse::<i64>().ok())
        .ok_or_else(bad)?;
    if parts.next().is_some() {
        return Err(bad());
    }
    Ok((key, id))
}

/// The search terms: words, folded to lower case, with LIKE's own characters escaped.
fn terms(query: &str) -> Vec<String> {
    query
        .split_whitespace()
        .take(MAX_TERMS)
        .map(|term| {
            let term: String = term
                .chars()
                .take(MAX_TERM)
                .collect::<String>()
                .to_lowercase();
            let escaped = term
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_");
            format!("%{escaped}%")
        })
        .collect()
}

fn category_exists(db: &Connection, category_id: i64) -> rusqlite::Result<bool> {
    super::exists(db, "SELECT 1 FROM category WHERE id = ?1", category_id)
}

/// Turns rows of SUMMARY (at column 0) plus a sort key column into a page.
fn page_of(
    db: &Connection,
    found: Vec<(String, ItemSummary)>,
    limit: usize,
    list: char,
) -> rusqlite::Result<Page<ItemSummary>> {
    let more = found.len() > limit;
    let mut rows = Vec::with_capacity(found.len().min(limit));
    let mut last_key = String::new();
    for (key, summary) in found.into_iter().take(limit) {
        last_key = key;
        rows.push(with_artists(db, summary)?);
    }
    let next = more
        .then(|| rows.last().map(|last| encode(list, &last_key, last.id)))
        .flatten();
    Ok(Page { rows, next })
}

impl Library {
    /// A page of a tab's playable items (tracks, episodes, films; not albums or shows) in
    /// `order`. `after` is the `next` of the page before.
    pub fn tab_items(
        &self,
        category_id: i64,
        order: Order,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Page<ItemSummary>, Problem> {
        let list = match order {
            Order::Title => 't',
            Order::Added => 'a',
            Order::Played => 'p',
        };
        let start = after.map(|cursor| decode(cursor, list)).transpose()?;
        let limit = limit_of(limit);
        let media = self.media.clone();
        self.read("listing a tab", move |db| {
            if !category_exists(db, category_id)? {
                return Ok(Err(no_such("tab", category_id)));
            }
            let (key_column, filter, order_by) = match order {
                Order::Title => (
                    "i.sort_title",
                    "(i.sort_title, i.id) > (?2, ?3)",
                    "i.sort_title, i.id",
                ),
                // Keys of the newest-first lists are numbers, kept as text in the cursor.
                Order::Added => (
                    "CAST(i.added_at AS TEXT)",
                    "(i.added_at, i.id) < (CAST(?2 AS INTEGER), ?3)",
                    "i.added_at DESC, i.id DESC",
                ),
                Order::Played => (
                    "CAST(p.opened_at AS TEXT)",
                    "p.item_id IS NOT NULL AND (p.opened_at, i.id) < (CAST(?2 AS INTEGER), ?3)",
                    "p.opened_at DESC, i.id DESC",
                ),
            };
            let (key, id) = start.clone().unwrap_or_else(|| match order {
                Order::Title => (String::new(), i64::MIN),
                _ => (i64::MAX.to_string(), i64::MAX),
            });
            let found = db
                .prepare_cached(&format!(
                    "SELECT {SUMMARY}, {key_column} FROM item i {SUMMARY_JOINS}
                     WHERE i.category_id = ?1 AND i.kind = 'entry' AND {filter}
                     ORDER BY {order_by} LIMIT ?4"
                ))?
                .query_map(params![category_id, key, id, limit as i64 + 1], |row| {
                    Ok((
                        row.get::<_, String>(super::SUMMARY_COLUMNS)?,
                        summary_row(row, 0, &media)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(Ok(page_of(db, found, limit, list)?))
        })?
    }

    /// The tab's artists, A to Z, with how many items each has.
    pub fn artists(&self, category_id: i64) -> Result<Vec<Named>, Problem> {
        self.read("listing artists", move |db| {
            if !category_exists(db, category_id)? {
                return Ok(Err(no_such("tab", category_id)));
            }
            let rows = db
                .prepare_cached(&format!(
                    "WITH named AS ({ARTISTS})
                     SELECT MIN(name), COUNT(DISTINCT item), MIN(item) FROM named
                     GROUP BY fold(name) ORDER BY fold(MIN(name)), MIN(name)"
                ))?
                .query_map([category_id], |row| {
                    Ok(Named {
                        name: row.get(0)?,
                        items: row.get(1)?,
                        first_item: row.get(2)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(Ok(rows))
        })?
    }

    /// A page of an artist's items in the tab, A to Z (any case of the name).
    pub fn artist_items(
        &self,
        category_id: i64,
        artist: &str,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Page<ItemSummary>, Problem> {
        let start = after.map(|cursor| decode(cursor, 'r')).transpose()?;
        let limit = limit_of(limit);
        let media = self.media.clone();
        let artist = artist.trim().to_owned();
        self.read("listing an artist", move |db| {
            if !category_exists(db, category_id)? {
                return Ok(Err(no_such("tab", category_id)));
            }
            let (key, id) = start.clone().unwrap_or((String::new(), i64::MIN));
            let found = db
                .prepare_cached(&format!(
                    "WITH named AS ({ARTISTS})
                     SELECT {SUMMARY}, i.sort_title FROM item i {SUMMARY_JOINS}
                     WHERE i.id IN (SELECT item FROM named WHERE fold(name) = fold(?2))
                       AND (i.sort_title, i.id) > (?3, ?4)
                     ORDER BY i.sort_title, i.id LIMIT ?5"
                ))?
                .query_map(
                    params![category_id, artist, key, id, limit as i64 + 1],
                    |row| {
                        Ok((
                            row.get::<_, String>(super::SUMMARY_COLUMNS)?,
                            summary_row(row, 0, &media)?,
                        ))
                    },
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(Ok(page_of(db, found, limit, 'r')?))
        })?
    }

    /// The tab's albums, A to Z, with how many items each has.
    pub fn albums(&self, category_id: i64) -> Result<Vec<Named>, Problem> {
        self.read("listing albums", move |db| {
            if !category_exists(db, category_id)? {
                return Ok(Err(no_such("tab", category_id)));
            }
            let rows = db
                .prepare_cached(&format!(
                    "SELECT MIN({ALBUM}), COUNT(*), MIN(i.id) FROM item i
                     WHERE i.category_id = ?1 AND i.kind = 'entry' AND {ALBUM} <> ''
                     GROUP BY fold({ALBUM}) ORDER BY fold(MIN({ALBUM})), MIN({ALBUM})"
                ))?
                .query_map([category_id], |row| {
                    Ok(Named {
                        name: row.get(0)?,
                        items: row.get(1)?,
                        first_item: row.get(2)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(Ok(rows))
        })?
    }

    /// A page of an album's items in the tab, in track order (then A to Z).
    pub fn album_items(
        &self,
        category_id: i64,
        album: &str,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Page<ItemSummary>, Problem> {
        let start = after.map(|cursor| decode(cursor, 'l')).transpose()?;
        let limit = limit_of(limit);
        let media = self.media.clone();
        let album = album.trim().to_owned();
        self.read("listing an album", move |db| {
            if !category_exists(db, category_id)? {
                return Ok(Err(no_such("tab", category_id)));
            }
            // The sort key: the track number (zero-padded, unknown last), then the title.
            let key_expr = "printf('%012.3f|', coalesce(i.number, 999999999)) || i.sort_title";
            let (key, id) = start.clone().unwrap_or((String::new(), i64::MIN));
            let found = db
                .prepare_cached(&format!(
                    "SELECT {SUMMARY}, {key_expr} FROM item i {SUMMARY_JOINS}
                     WHERE i.category_id = ?1 AND i.kind = 'entry' AND fold({ALBUM}) = fold(?2)
                       AND ({key_expr}, i.id) > (?3, ?4)
                     ORDER BY {key_expr}, i.id LIMIT ?5"
                ))?
                .query_map(
                    params![category_id, album, key, id, limit as i64 + 1],
                    |row| {
                        Ok((
                            row.get::<_, String>(super::SUMMARY_COLUMNS)?,
                            summary_row(row, 0, &media)?,
                        ))
                    },
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(Ok(page_of(db, found, limit, 'l')?))
        })?
    }

    /// Items, playlists and artists whose names hold every word of `query` (any case, in
    /// any alphabet), in one tab or (none) every tab; at most 50 of each.
    pub fn search(&self, category_id: Option<i64>, query: &str) -> Result<Found, Problem> {
        let terms = terms(query);
        if terms.is_empty() {
            return Ok(Found {
                items: Vec::new(),
                collections: Vec::new(),
                artists: Vec::new(),
            });
        }
        let media = self.media.clone();
        self.read("searching", move |db| {
            if let Some(category) = category_id
                && !category_exists(db, category)?
            {
                return Ok(Err(no_such("tab", category)));
            }
            // Every term must be in the title, an artist or the album.
            let item_terms: Vec<String> = (0..terms.len())
                .map(|at| {
                    let term = at + 2;
                    format!(
                        "(fold(i.title) LIKE ?{term} ESCAPE '\\'
                          OR fold(coalesce(json_extract(i.extra, '$.artist'), '')) LIKE ?{term} ESCAPE '\\'
                          OR fold(coalesce({ALBUM}, '')) LIKE ?{term} ESCAPE '\\'
                          OR EXISTS (SELECT 1 FROM item_tag it JOIN tag t ON t.id = it.tag_id
                                     WHERE it.item_id = i.id AND t.kind = 'artist'
                                       AND fold(t.name) LIKE ?{term} ESCAPE '\\'))"
                    )
                })
                .collect();
            let name_terms = |column: &str| -> String {
                (0..terms.len())
                    .map(|at| format!("fold({column}) LIKE ?{} ESCAPE '\\'", at + 2))
                    .collect::<Vec<_>>()
                    .join(" AND ")
            };
            let tab = "(?1 IS NULL OR i.category_id = ?1)";
            let mut values: Vec<rusqlite::types::Value> = vec![category_id.into()];
            values.extend(terms.iter().cloned().map(rusqlite::types::Value::from));
            let items = db
                .prepare(&format!(
                    "SELECT {SUMMARY} FROM item i {SUMMARY_JOINS}
                     WHERE i.kind = 'entry' AND {tab} AND {}
                     ORDER BY i.sort_title, i.id LIMIT {SEARCH_ROWS}",
                    item_terms.join(" AND ")
                ))?
                .query_map(rusqlite::params_from_iter(values.iter()), |row| summary_row(row, 0, &media))?
                .collect::<rusqlite::Result<Vec<_>>>()?
                .into_iter()
                .map(|summary| with_artists(db, summary))
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let collections = db
                .prepare(&format!(
                    "SELECT c.id, c.category_id, c.name, c.cover, c.source, c.query IS NOT NULL, c.created_at,
                       (SELECT COUNT(*) FROM collection_item ci WHERE ci.collection_id = c.id)
                     FROM collection c
                     WHERE (?1 IS NULL OR c.category_id = ?1) AND {}
                     ORDER BY fold(c.name), c.id LIMIT {SEARCH_ROWS}",
                    name_terms("c.name")
                ))?
                .query_map(rusqlite::params_from_iter(values.iter()), |row| collection_row(row, &media))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            // Artists of the one tab, or of each tab.
            let artists_sql = format!(
                "WITH named AS (
                   SELECT t.name AS name, i.id AS item FROM tag t
                     JOIN item_tag it ON it.tag_id = t.id JOIN item i ON i.id = it.item_id
                     WHERE t.kind = 'artist' AND i.kind = 'entry' AND {tab} AND trim(t.name) <> ''
                   UNION
                   SELECT json_extract(i.extra, '$.artist'), i.id FROM item i
                     WHERE i.kind = 'entry' AND {tab} AND json_type(i.extra, '$.artist') = 'text'
                       AND trim(json_extract(i.extra, '$.artist')) <> '')
                 SELECT MIN(name), COUNT(DISTINCT item), MIN(item) FROM named WHERE {}
                 GROUP BY fold(name) ORDER BY fold(MIN(name)) LIMIT {SEARCH_ROWS}",
                name_terms("name")
            );
            let artists = db
                .prepare(&artists_sql)?
                .query_map(rusqlite::params_from_iter(values.iter()), |row| {
                    Ok(Named {
                        name: row.get(0)?,
                        items: row.get(1)?,
                        first_item: row.get(2)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(Ok(Found {
                items,
                collections,
                artists,
            }))
        })?
    }
}
