//! The media library (src/library): tabs, playlists, their items a page at a time, one
//! item's details, the checked file to play, and what the person marks: where they
//! stopped, what they finished, favorites and ratings. The apps hold ids; paths only come
//! from `item_file`.

use super::{ApiError, core};
use crate::library;

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum MediaKind {
    Audio,
    Video,
    Ebook,
    Pages,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum ReadingDirection {
    Ltr,
    Rtl,
    Vertical,
}

/// A library tab.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct Category {
    pub id: i64,
    pub name: String,
    /// Which player or reader opens its items.
    pub media: MediaKind,
    pub direction: ReadingDirection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum ItemKind {
    /// Album, show, series: holds entries.
    Group,
    /// Track, episode, movie, chapter: one file.
    Entry,
}

/// Where the person stopped.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct ItemProgress {
    /// Milliseconds for audio and video, the page for pages.
    pub position: i64,
    /// 0 to 1.
    pub percent: f64,
    pub done: bool,
    pub plays: i64,
}

/// What a list row shows.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct ItemSummary {
    pub id: i64,
    pub kind: ItemKind,
    pub category_id: i64,
    pub media: MediaKind,
    pub title: String,
    pub artist: Option<String>,
    pub artists: Vec<String>,
    pub year: Option<i64>,
    pub duration_ms: Option<i64>,
    /// Season, disc or volume.
    pub group_no: Option<i64>,
    /// Episode, track or chapter.
    pub number: Option<f64>,
    /// An image file inside the media folder, ready to show.
    pub cover: Option<String>,
    pub favorite: bool,
    pub rating: Option<i64>,
    /// The file is gone: show it, but it cannot be played.
    pub missing: bool,
    pub progress: Option<ItemProgress>,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ItemTag {
    /// artist, genre, author, studio, publisher, tag
    pub kind: String,
    pub name: String,
}

/// Everything about one item.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct ItemDetail {
    pub summary: ItemSummary,
    pub parent_id: Option<i64>,
    pub original_title: Option<String>,
    pub description: Option<String>,
    pub page_count: Option<i64>,
    /// Seconds since 1970.
    pub added_at: i64,
    pub album: Option<String>,
    pub source_url: Option<String>,
    pub tags: Vec<ItemTag>,
}

/// A playlist or collection.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct CollectionSummary {
    pub id: i64,
    pub category_id: i64,
    pub name: String,
    pub cover: Option<String>,
    /// How many entries (the same item can be in it twice).
    pub entries: i64,
    pub source: Option<String>,
    /// Filled by a saved search; its items come in a later version.
    pub smart: bool,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct CollectionEntry {
    /// This row's own id (the same item can appear twice).
    pub entry_id: i64,
    pub position: f64,
    pub item: ItemSummary,
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct CollectionPage {
    pub entries: Vec<CollectionEntry>,
    /// Pass it as `after` for the next page; none at the end.
    pub next: Option<String>,
}

/// The library's tabs, in display order.
#[uniffi::export]
pub fn categories() -> Result<Vec<Category>, ApiError> {
    Ok(core()?
        .library
        .categories()?
        .into_iter()
        .map(|category| Category {
            id: category.id,
            name: category.name,
            media: media(category.media),
            direction: match category.direction {
                library::Direction::Ltr => ReadingDirection::Ltr,
                library::Direction::Rtl => ReadingDirection::Rtl,
                library::Direction::Vertical => ReadingDirection::Vertical,
            },
        })
        .collect())
}

/// The playlists of a tab, in their order.
#[uniffi::export]
pub fn collections(category_id: i64) -> Result<Vec<CollectionSummary>, ApiError> {
    Ok(core()?
        .library
        .collections(category_id)?
        .into_iter()
        .map(collection_summary)
        .collect())
}

#[uniffi::export]
pub fn collection(id: i64) -> Result<CollectionSummary, ApiError> {
    Ok(collection_summary(core()?.library.collection(id)?))
}

/// A page of a playlist's entries, in order: `after` is the `next` of the page before (none
/// for the first page); `limit` 0 means 100, and at most 500 come.
#[uniffi::export]
pub fn collection_items(
    collection_id: i64,
    after: Option<String>,
    limit: u32,
) -> Result<CollectionPage, ApiError> {
    let page = core()?
        .library
        .collection_items(collection_id, after.as_deref(), limit as usize)?;
    Ok(CollectionPage {
        entries: page
            .rows
            .into_iter()
            .map(|entry| CollectionEntry {
                entry_id: entry.entry_id,
                position: entry.position,
                item: summary(entry.item),
            })
            .collect(),
        next: page.next,
    })
}

#[uniffi::export]
pub fn item(id: i64) -> Result<ItemDetail, ApiError> {
    let detail = core()?.library.item(id)?;
    Ok(ItemDetail {
        summary: summary(detail.summary),
        parent_id: detail.parent_id,
        original_title: detail.original_title,
        description: detail.description,
        page_count: detail.page_count,
        added_at: detail.added_at,
        album: detail.album,
        source_url: detail.source_url,
        tags: detail
            .tags
            .into_iter()
            .map(|tag| ItemTag {
                kind: tag.kind,
                name: tag.name,
            })
            .collect(),
    })
}

/// The file to play or open, after checking that it exists and lies inside the media
/// folder.
#[uniffi::export]
pub fn item_file(id: i64) -> Result<String, ApiError> {
    Ok(core()?
        .library
        .item_file(id)?
        .to_string_lossy()
        .into_owned())
}

/// Adds a file already inside a tab's folder (`<media>/<tab name>/...`) to the library;
/// returns its item id (the same file again: the same id).
#[uniffi::export]
pub fn add_file(tab_id: i64, path: String) -> Result<i64, ApiError> {
    Ok(core()?.library.add_file(tab_id, std::path::Path::new(&path))?)
}

/// Where to start an item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct PlayStart {
    pub position_ms: i64,
    /// True when that is a resume point ("Resuming at 12:34").
    pub resumed: bool,
}

/// Begins playing an item: where to start (songs and clips from 0, longer things from
/// their resume point), and a new play that counts once when it finishes.
#[uniffi::export]
pub fn start_play(item_id: i64) -> Result<PlayStart, ApiError> {
    let start = core()?.library.start_play(item_id)?;
    Ok(PlayStart {
        position_ms: start.position_ms,
        resumed: start.resumed,
    })
}

/// Saves where playback of an item is; `duration_ms` as the player knows it. Call it on
/// pause, every few seconds and at the end. False when the item is gone (nothing to save).
#[uniffi::export]
pub fn save_progress(
    item_id: i64,
    position_ms: i64,
    duration_ms: Option<i64>,
) -> Result<bool, ApiError> {
    Ok(core()?
        .library
        .save_progress(item_id, position_ms, duration_ms)?)
}

/// Marks an item watched (done: it starts over) or not watched.
#[uniffi::export]
pub fn set_watched(item_id: i64, watched: bool) -> Result<(), ApiError> {
    Ok(core()?.library.set_watched(item_id, watched)?)
}

#[uniffi::export]
pub fn set_favorite(item_id: i64, favorite: bool) -> Result<(), ApiError> {
    Ok(core()?.library.set_favorite(item_id, favorite)?)
}

/// 1 to 10, or none to clear it.
#[uniffi::export]
pub fn set_rating(item_id: i64, rating: Option<i64>) -> Result<(), ApiError> {
    Ok(core()?.library.set_rating(item_id, rating)?)
}

pub(super) fn media(media: library::Media) -> MediaKind {
    match media {
        library::Media::Audio => MediaKind::Audio,
        library::Media::Video => MediaKind::Video,
        library::Media::Ebook => MediaKind::Ebook,
        library::Media::Pages => MediaKind::Pages,
    }
}

fn path(path: Option<std::path::PathBuf>) -> Option<String> {
    path.map(|path| path.to_string_lossy().into_owned())
}

fn summary(item: library::ItemSummary) -> ItemSummary {
    ItemSummary {
        id: item.id,
        kind: match item.kind {
            library::ItemKind::Group => ItemKind::Group,
            library::ItemKind::Entry => ItemKind::Entry,
        },
        category_id: item.category_id,
        media: media(item.media),
        title: item.title,
        artist: item.artist,
        artists: item.artists,
        year: item.year,
        duration_ms: item.duration_ms,
        group_no: item.group_no,
        number: item.number,
        cover: path(item.cover),
        favorite: item.favorite,
        rating: item.rating,
        missing: item.missing,
        progress: item.progress.map(|progress| ItemProgress {
            position: progress.position,
            percent: progress.percent,
            done: progress.done,
            plays: progress.plays,
        }),
    }
}

fn collection_summary(collection: library::CollectionSummary) -> CollectionSummary {
    CollectionSummary {
        id: collection.id,
        category_id: collection.category_id,
        name: collection.name,
        cover: path(collection.cover),
        entries: collection.entries,
        source: collection.source,
        smart: collection.smart,
        created_at: collection.created_at,
    }
}
