use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::thread;

use rusqlite::{Connection, params};

use super::*;

/// A library in a fresh folder: media/Music is a library folder of the Music tab, and
/// `db` is a second connection for the test to write with (like a scanner would).
struct Fixture {
    dir: PathBuf,
    media: PathBuf,
    music: PathBuf,
    db: Connection,
    library: Library,
    music_tab: i64,
    music_folder: i64,
}

fn fixture(name: &str) -> Fixture {
    let dir = std::env::temp_dir().join(format!("elsewhere-library-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let media = dir.join("media");
    let music = media.join("Music");
    fs::create_dir_all(&music).unwrap();
    let path = dir.join("library.db");
    let db = crate::db::open(&path).unwrap();
    let music_tab: i64 = db
        .query_row("SELECT id FROM category WHERE name = 'Music'", [], |row| {
            row.get(0)
        })
        .unwrap();
    let music_folder: i64 = db
        .query_row(
            "INSERT INTO library (category_id, root) VALUES (?1, ?2) RETURNING id",
            params![music_tab, music.to_str().unwrap()],
            |row| row.get(0),
        )
        .unwrap();
    let library = Library::open(&path, &media).unwrap();
    Fixture {
        dir,
        media,
        music,
        db,
        library,
        music_tab,
        music_folder,
    }
}

impl Fixture {
    /// A song with a real file (unless `file` is None).
    fn song(&self, title: &str, file: Option<&str>) -> i64 {
        if let Some(file) = file {
            let path = self.music.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, b"not really audio").unwrap();
        }
        self.db
            .query_row(
                "INSERT INTO item (category_id, library_id, kind, path, title, sort_title)
                 VALUES (?1, ?2, 'entry', ?3, ?4, lower(?4)) RETURNING id",
                params![self.music_tab, self.music_folder, file, title],
                |row| row.get(0),
            )
            .unwrap()
    }

    fn playlist(&self, name: &str, items: &[i64]) -> i64 {
        let id: i64 = self
            .db
            .query_row(
                "INSERT INTO collection (category_id, name) VALUES (?1, ?2) RETURNING id",
                params![self.music_tab, name],
                |row| row.get(0),
            )
            .unwrap();
        for (index, item) in items.iter().enumerate() {
            self.add_entry(id, *item, index as f64 + 1.0);
        }
        id
    }

    fn add_entry(&self, collection: i64, item: i64, position: f64) -> i64 {
        self.db
            .query_row(
                "INSERT INTO collection_item (collection_id, item_id, position) VALUES (?1, ?2, ?3)
                 RETURNING id",
                params![collection, item, position],
                |row| row.get(0),
            )
            .unwrap()
    }

    /// Every entry of a collection, page by page.
    fn all_entries(&self, collection: i64, limit: usize) -> (Vec<CollectionEntry>, usize) {
        let mut rows = Vec::new();
        let mut pages = 0;
        let mut after: Option<String> = None;
        loop {
            let page = self
                .library
                .collection_items(collection, after.as_deref(), limit)
                .unwrap();
            pages += 1;
            assert!(page.rows.len() <= limit.max(1));
            rows.extend(page.rows);
            match page.next {
                Some(next) => after = Some(next),
                None => return (rows, pages),
            }
        }
    }
}

fn kind_of<T: std::fmt::Debug>(result: Result<T, Problem>) -> ProblemKind {
    result.expect_err("a problem").kind
}

#[test]
fn tabs_come_in_order_with_their_kinds() {
    let f = fixture("tabs");
    let tabs = f.library.categories().unwrap();
    let names: Vec<&str> = tabs.iter().map(|tab| tab.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "Music", "Movies", "TV Shows", "Anime", "Books", "Comics", "Manga", "Manhwa", "Videos"
        ]
    );
    assert_eq!(
        (tabs[0].media, tabs[0].direction),
        (Media::Audio, Direction::Ltr)
    );
    assert_eq!(tabs[1].media, Media::Video);
    assert_eq!(tabs[4].media, Media::Ebook);
    assert_eq!(
        (tabs[6].media, tabs[6].direction),
        (Media::Pages, Direction::Rtl)
    );
    assert_eq!(tabs[7].direction, Direction::Vertical);
}

#[test]
fn playlists_of_a_tab_in_their_order_with_counts_and_checked_covers() {
    let f = fixture("collections");
    let song = f.song("Song", Some("song.m4a"));
    let b = f.playlist("b", &[song]);
    let a = f.playlist("A", &[song, song, song]);
    let c = f.playlist("c", &[]);
    fs::write(f.media.join("cover.jpg"), b"jpeg").unwrap();
    let outside = f.dir.join("outside.jpg");
    fs::write(&outside, b"jpeg").unwrap();
    f.db.execute_batch(&format!(
        "UPDATE collection SET position = 1, cover = '{}' WHERE id = {b};
         UPDATE collection SET cover = '{}', source = 'YoutubeTab:PL1' WHERE id = {a};
         UPDATE collection SET cover = '{}' WHERE id = {c};",
        f.media.join("cover.jpg").display(),
        f.media.join("gone.jpg").display(),
        outside.display(),
    ))
    .unwrap();
    let lists = f.library.collections(f.music_tab).unwrap();
    let order: Vec<(&str, i64)> = lists
        .iter()
        .map(|list| (list.name.as_str(), list.entries))
        .collect();
    assert_eq!(
        order,
        [("A", 3), ("c", 0), ("b", 1)],
        "position, then name ignoring case"
    );
    assert_eq!(lists[0].source.as_deref(), Some("YoutubeTab:PL1"));
    assert_eq!(lists[0].cover, None, "a cover that is gone is left out");
    assert_eq!(
        lists[1].cover, None,
        "a cover outside the media folder is left out"
    );
    assert_eq!(
        lists[2].cover.as_deref(),
        Some(
            fs::canonicalize(f.media.join("cover.jpg"))
                .unwrap()
                .as_path()
        )
    );
    assert!(
        lists
            .iter()
            .all(|list| !list.smart && list.category_id == f.music_tab)
    );

    assert_eq!(f.library.collection(a).unwrap(), lists[0]);
    assert_eq!(
        kind_of(f.library.collection(9999)),
        ProblemKind::Unavailable
    );
    assert_eq!(
        kind_of(f.library.collections(9999)),
        ProblemKind::Unavailable
    );
    let movies: i64 =
        f.db.query_row("SELECT id FROM category WHERE name = 'Movies'", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert!(f.library.collections(movies).unwrap().is_empty());
}

#[test]
fn pages_give_every_entry_once_in_order() {
    let f = fixture("pages");
    let songs: Vec<i64> = (0..10)
        .map(|n| f.song(&format!("Song {n}"), Some(&format!("{n}.m4a"))))
        .collect();
    // 250 entries; every song appears 25 times.
    let items: Vec<i64> = (0..250).map(|n| songs[n % songs.len()]).collect();
    let list = f.playlist("Long", &items);
    for limit in [1, 7, 100, 249, 250, 251, 500] {
        let (rows, pages) = f.all_entries(list, limit);
        assert_eq!(rows.len(), 250, "limit {limit}");
        assert_eq!(
            pages,
            250usize.div_ceil(limit).max(1),
            "limit {limit}: no empty last page"
        );
        let ids: HashSet<i64> = rows.iter().map(|row| row.entry_id).collect();
        assert_eq!(ids.len(), 250, "limit {limit}: every entry once");
        assert!(
            rows.windows(2)
                .all(|pair| pair[0].position < pair[1].position),
            "limit {limit}"
        );
        let titles: Vec<&str> = rows
            .iter()
            .take(3)
            .map(|row| row.item.title.as_str())
            .collect();
        assert_eq!(titles, ["Song 0", "Song 1", "Song 2"]);
    }
    // 0 is the default page; more than the most is the most.
    assert_eq!(
        f.library
            .collection_items(list, None, 0)
            .unwrap()
            .rows
            .len(),
        DEFAULT_PAGE
    );
    assert_eq!(
        f.library
            .collection_items(list, None, 100_000)
            .unwrap()
            .rows
            .len(),
        250
    );
    let big: Vec<i64> = (0..MAX_PAGE + 10).map(|_| songs[0]).collect();
    let big = f.playlist("Big", &big);
    let page = f.library.collection_items(big, None, 100_000).unwrap();
    assert_eq!((page.rows.len(), page.next.is_some()), (MAX_PAGE, true));

    let empty = f.playlist("Empty", &[]);
    let page = f.library.collection_items(empty, None, 10).unwrap();
    assert_eq!((page.rows.len(), page.next), (0, None));
}

#[test]
fn rows_added_or_removed_between_pages_never_repeat_or_vanish() {
    let f = fixture("between");
    let song = f.song("Song", Some("song.m4a"));
    let list = f.playlist("List", &[song; 20]);
    let first = f.library.collection_items(list, None, 10).unwrap();
    let seen: HashSet<i64> = first.rows.iter().map(|row| row.entry_id).collect();
    let last = first.rows.last().unwrap().position;
    // Meanwhile: one row inserted before the cursor, one after, one after removed.
    f.add_entry(list, song, 0.5);
    let late = f.add_entry(list, song, last + 0.5);
    let gone: i64 = f
        .db
        .query_row(
            "SELECT id FROM collection_item WHERE collection_id = ?1 AND position > ?2 ORDER BY position LIMIT 1 OFFSET 3",
            params![list, last],
            |row| row.get(0),
        )
        .unwrap();
    f.db.execute("DELETE FROM collection_item WHERE id = ?1", [gone])
        .unwrap();
    let mut rest = Vec::new();
    let mut after = first.next.clone();
    while let Some(cursor) = after {
        let page = f.library.collection_items(list, Some(&cursor), 10).unwrap();
        rest.extend(page.rows);
        after = page.next;
    }
    let rest_ids: Vec<i64> = rest.iter().map(|row| row.entry_id).collect();
    assert!(
        rest_ids.iter().all(|id| !seen.contains(id)),
        "nothing from the first page again"
    );
    assert!(
        rest_ids.contains(&late),
        "a row added after the cursor comes"
    );
    assert!(!rest_ids.contains(&gone), "a removed row does not");
    assert_eq!(rest.len(), 10, "10 left, one added, one removed");
}

#[test]
fn equal_positions_are_ordered_by_entry_and_page_cleanly() {
    let f = fixture("ties");
    let song = f.song("Song", Some("song.m4a"));
    let list = f.playlist("Ties", &[]);
    let entries: Vec<i64> = (0..5).map(|_| f.add_entry(list, song, 1.0)).collect();
    let (rows, _) = f.all_entries(list, 2);
    let order: Vec<i64> = rows.iter().map(|row| row.entry_id).collect();
    assert_eq!(order, entries);
}

#[test]
fn bad_page_links_are_refused() {
    let f = fixture("cursors");
    let song = f.song("Song", Some("song.m4a"));
    let list = f.playlist("List", &[song, song]);
    let nan = format!("c1:{:016x}:1", f64::NAN.to_bits());
    let infinite = format!("c1:{:016x}:1", f64::INFINITY.to_bits());
    for cursor in [
        "",
        "garbage",
        "c1",
        "c1::",
        "c2:3ff0000000000000:1",
        "c1:3ff0000000000000",
        "c1:3ff0000000000000:x",
        "c1:3ff00000:1",
        "c1:zzzzzzzzzzzzzzzz:1",
        "c1:3ff0000000000000:1:extra",
        &nan,
        &infinite,
    ] {
        let problem = f
            .library
            .collection_items(list, Some(cursor), 10)
            .unwrap_err();
        assert_eq!(
            (problem.kind, problem.retry),
            (ProblemKind::Input, false),
            "{cursor:?}"
        );
    }
    // A good one from a page works.
    let page = f.library.collection_items(list, None, 1).unwrap();
    let next = f
        .library
        .collection_items(list, page.next.as_deref(), 1)
        .unwrap();
    assert_eq!((next.rows.len(), next.next), (1, None));
}

#[test]
fn a_playlist_deleted_while_paging_or_a_smart_one_says_so() {
    let f = fixture("deleted");
    let song = f.song("Song", Some("song.m4a"));
    let list = f.playlist("List", &[song, song, song]);
    let page = f.library.collection_items(list, None, 1).unwrap();
    f.db.execute("DELETE FROM collection WHERE id = ?1", [list])
        .unwrap();
    let problem = f
        .library
        .collection_items(list, page.next.as_deref(), 1)
        .unwrap_err();
    assert_eq!(problem.kind, ProblemKind::Unavailable);
    assert!(problem.text.contains("no longer"), "{problem:?}");

    let smart: i64 = f
        .db
        .query_row(
            "INSERT INTO collection (category_id, name, query) VALUES (?1, 'Recent', '{\"added\":30}') RETURNING id",
            [f.music_tab],
            |row| row.get(0),
        )
        .unwrap();
    assert!(f.library.collection(smart).unwrap().smart);
    assert_eq!(
        kind_of(f.library.collection_items(smart, None, 10)),
        ProblemKind::Input
    );
}

#[test]
fn item_details_survive_odd_and_missing_data() {
    let f = fixture("details");
    let song = f.song("Hit", Some("hit.m4a"));
    f.db.execute_batch(&format!(
        "UPDATE item SET original_title = '  ', summary = 'About it', year = 2024, duration_ms = -5,
           number = 3, group_no = 1, rating = 8, favorite = 1,
           extra = '{{\"artist\": \"Band\", \"album\": \"Record\", \"url\": \"https://x.test/v\"}}'
         WHERE id = {song};
         INSERT INTO tag (kind, name) VALUES ('artist', 'Band'), ('artist', 'Guest'), ('genre', 'Pop');
         INSERT INTO item_tag (item_id, tag_id) SELECT {song}, id FROM tag;"
    ))
    .unwrap();
    let detail = f.library.item(song).unwrap();
    let summary = &detail.summary;
    assert_eq!(summary.title, "Hit");
    assert_eq!(summary.artist.as_deref(), Some("Band"));
    assert_eq!(summary.artists, ["Band", "Guest"]);
    assert_eq!(
        (
            summary.year,
            summary.duration_ms,
            summary.number,
            summary.group_no
        ),
        (Some(2024), None, Some(3.0), Some(1))
    );
    assert_eq!(
        (summary.favorite, summary.rating, summary.missing),
        (true, Some(8), false)
    );
    assert_eq!(
        (summary.kind, summary.media),
        (ItemKind::Entry, Media::Audio)
    );
    assert_eq!(detail.original_title, None, "blank text is no text");
    assert_eq!(detail.description.as_deref(), Some("About it"));
    assert_eq!(
        (detail.album.as_deref(), detail.source_url.as_deref()),
        (Some("Record"), Some("https://x.test/v"))
    );
    assert_eq!(
        detail.tags,
        [
            Tag {
                kind: "artist".into(),
                name: "Band".into()
            },
            Tag {
                kind: "artist".into(),
                name: "Guest".into()
            },
            Tag {
                kind: "genre".into(),
                name: "Pop".into()
            },
        ]
    );

    // extra with the wrong types: ignored; the main artist then comes from the tags.
    f.db.execute(
        "UPDATE item SET extra = '{\"artist\": 5, \"album\": [1], \"url\": null}' WHERE id = ?1",
        [song],
    )
    .unwrap();
    let detail = f.library.item(song).unwrap();
    assert_eq!(detail.summary.artist.as_deref(), Some("Band"));
    assert_eq!((detail.album, detail.source_url), (None, None));

    // Hardly anything set.
    let bare = f.song("Bare", None);
    let detail = f.library.item(bare).unwrap();
    assert_eq!(
        (
            detail.summary.artist,
            detail.summary.artists.len(),
            detail.tags.len()
        ),
        (None, 0, 0)
    );
    assert_eq!(kind_of(f.library.item(9999)), ProblemKind::Unavailable);
}

#[test]
fn missing_files_and_covers_are_marked_not_fatal() {
    let f = fixture("missing");
    let here = f.song("Here", Some("here.m4a"));
    let deleted = f.song("Deleted", Some("deleted.m4a"));
    fs::remove_file(f.music.join("deleted.m4a")).unwrap();
    let flagged = f.song("Flagged", Some("flagged.m4a"));
    f.db.execute("UPDATE item SET missing_since = 1 WHERE id = ?1", [flagged])
        .unwrap();
    let no_file = f.song("No file", None);
    let album: i64 =
        f.db.query_row(
            "INSERT INTO item (category_id, library_id, kind, path, title, sort_title)
             VALUES (?1, ?2, 'group', 'Album', 'Album', 'album') RETURNING id",
            params![f.music_tab, f.music_folder],
            |row| row.get(0),
        )
        .unwrap();
    fs::write(f.music.join("here.jpg"), b"jpeg").unwrap();
    f.db.execute("UPDATE item SET cover = 'here.jpg' WHERE id = ?1", [here])
        .unwrap();
    f.db.execute(
        "UPDATE item SET cover = '/nowhere/cover.jpg' WHERE id = ?1",
        [deleted],
    )
    .unwrap();
    let list = f.playlist("Mixed", &[here, deleted, flagged, no_file, album]);
    let rows = f.library.collection_items(list, None, 10).unwrap().rows;
    let marks: Vec<(&str, bool)> = rows
        .iter()
        .map(|row| (row.item.title.as_str(), row.item.missing))
        .collect();
    assert_eq!(
        marks,
        [
            ("Here", false),
            ("Deleted", true),
            ("Flagged", true),
            ("No file", true),
            ("Album", false)
        ]
    );
    assert_eq!(
        rows[0].item.cover.as_deref(),
        Some(
            fs::canonicalize(f.music.join("here.jpg"))
                .unwrap()
                .as_path()
        )
    );
    assert_eq!(rows[1].item.cover, None);
    assert_eq!(rows[4].item.kind, ItemKind::Group);
}

#[test]
fn progress_comes_with_the_rows_kept_in_range() {
    let f = fixture("progress");
    let watched = f.song("Watched", Some("a.m4a"));
    let odd = f.song("Odd", Some("b.m4a"));
    let fresh = f.song("Fresh", Some("c.m4a"));
    f.db.execute_batch(&format!(
        "INSERT INTO progress (item_id, position, percent, done, plays) VALUES ({watched}, 61000, 0.5, 1, 3);
         INSERT INTO progress (item_id, position, percent, done, plays) VALUES ({odd}, -40, 2.5, 0, -1);"
    ))
    .unwrap();
    let list = f.playlist("List", &[watched, odd, fresh]);
    let rows = f.library.collection_items(list, None, 10).unwrap().rows;
    assert_eq!(
        rows[0].item.progress,
        Some(Progress {
            position: 61000,
            percent: 0.5,
            done: true,
            plays: 3
        })
    );
    assert_eq!(
        rows[1].item.progress,
        Some(Progress {
            position: 0,
            percent: 1.0,
            done: false,
            plays: 0
        })
    );
    assert_eq!(rows[2].item.progress, None);
}

#[test]
fn the_file_of_an_item_is_checked_before_it_is_given_out() {
    let f = fixture("files");
    let good = f.song("Good", Some("deep/folder/good.m4a"));
    let real = f.library.item_file(good).unwrap();
    assert_eq!(
        real,
        fs::canonicalize(f.music.join("deep/folder/good.m4a")).unwrap()
    );
    assert!(real.is_absolute());

    let gone = f.song("Gone", Some("gone.m4a"));
    fs::remove_file(f.music.join("gone.m4a")).unwrap();
    let no_file = f.song("No file", None);
    let escape = f.song("Escape", None);
    f.db.execute(
        "UPDATE item SET path = '../../outside.m4a' WHERE id = ?1",
        [escape],
    )
    .unwrap();
    fs::write(f.dir.join("outside.m4a"), b"x").unwrap();
    let absolute = f.song("Absolute", None);
    f.db.execute(
        "UPDATE item SET path = ?2 WHERE id = ?1",
        params![absolute, f.dir.join("outside.m4a").to_str()],
    )
    .unwrap();
    let linked = f.song("Linked", None);
    std::os::unix::fs::symlink(f.dir.join("outside.m4a"), f.music.join("link.m4a")).unwrap();
    f.db.execute("UPDATE item SET path = 'link.m4a' WHERE id = ?1", [linked])
        .unwrap();
    let folder = f.song("Folder", None);
    fs::create_dir_all(f.music.join("a-folder")).unwrap();
    f.db.execute("UPDATE item SET path = 'a-folder' WHERE id = ?1", [folder])
        .unwrap();
    let album: i64 =
        f.db.query_row(
            "INSERT INTO item (category_id, library_id, kind, path, title, sort_title)
             VALUES (?1, ?2, 'group', 'deep', 'Album', 'album') RETURNING id",
            params![f.music_tab, f.music_folder],
            |row| row.get(0),
        )
        .unwrap();
    // A library folder outside the media folder (it cannot be added yet, but say it was).
    let outside_root = f.dir.join("elsewhere-root");
    fs::create_dir_all(&outside_root).unwrap();
    fs::write(outside_root.join("song.m4a"), b"x").unwrap();
    let outside_library: i64 =
        f.db.query_row(
            "INSERT INTO library (category_id, root) VALUES (?1, ?2) RETURNING id",
            params![f.music_tab, outside_root.to_str()],
            |row| row.get(0),
        )
        .unwrap();
    let outside_item = f.song("Outside", None);
    f.db.execute(
        "UPDATE item SET library_id = ?2, path = 'song.m4a' WHERE id = ?1",
        params![outside_item, outside_library],
    )
    .unwrap();
    let orphan = f.song("Orphan", None);
    f.db.execute(
        "UPDATE item SET library_id = NULL, path = 'x.m4a' WHERE id = ?1",
        [orphan],
    )
    .unwrap();

    for (id, kind, what) in [
        (gone, ProblemKind::Unavailable, "deleted file"),
        (no_file, ProblemKind::Unavailable, "no path"),
        (orphan, ProblemKind::Unavailable, "no library folder"),
        (escape, ProblemKind::Input, "path with .."),
        (absolute, ProblemKind::Input, "absolute path"),
        (linked, ProblemKind::Input, "link out of the media folder"),
        (folder, ProblemKind::Input, "a folder"),
        (album, ProblemKind::Input, "a group"),
        (
            outside_item,
            ProblemKind::Input,
            "library folder outside the media folder",
        ),
        (9999, ProblemKind::Unavailable, "unknown item"),
    ] {
        let problem = f.library.item_file(id).unwrap_err();
        assert_eq!(
            (problem.kind, problem.retry),
            (kind, false),
            "{what}: {problem:?}"
        );
    }
}

#[test]
fn text_in_any_language_comes_back_unchanged() {
    let f = fixture("text");
    let titles = [
        "გამარჯობა",
        "مرحبا بالعالم",
        "日本語のタイトル",
        "emoji 🎵🎬",
        "quote ' and \" and ;--",
    ];
    let ids: Vec<i64> = titles
        .iter()
        .enumerate()
        .map(|(n, title)| f.song(title, Some(&format!("{n}.m4a"))))
        .collect();
    let list = f.playlist("Ünïcödé ✓", &ids);
    assert_eq!(f.library.collection(list).unwrap().name, "Ünïcödé ✓");
    let rows = f.library.collection_items(list, None, 10).unwrap().rows;
    let back: Vec<&str> = rows.iter().map(|row| row.item.title.as_str()).collect();
    assert_eq!(back, titles);
}

#[test]
fn many_readers_while_a_writer_adds() {
    let f = fixture("threads");
    let song = f.song("Song", Some("song.m4a"));
    let list = f.playlist("List", &[song; 50]);
    let library = Arc::new(Library::open(&f.dir.join("library.db"), &f.media).unwrap());
    let writer_path = f.dir.join("library.db");
    let writer = thread::spawn(move || {
        let db = crate::db::open(&writer_path).unwrap();
        for n in 0..200 {
            db.execute(
                "INSERT INTO collection_item (collection_id, item_id, position) VALUES (?1, ?2, ?3)",
                params![list, song, 100.0 + n as f64],
            )
            .unwrap();
        }
    });
    let readers: Vec<_> = (0..8)
        .map(|n| {
            let library = Arc::clone(&library);
            thread::spawn(move || {
                for round in 0..60 {
                    match (n + round) % 4 {
                        0 => assert!(
                            library.collection_items(list, None, 25).unwrap().rows.len() == 25
                        ),
                        1 => assert_eq!(library.item(song).unwrap().summary.title, "Song"),
                        2 => assert!(library.item_file(song).unwrap().is_file()),
                        _ => assert!(
                            !library
                                .collections(library.categories().unwrap()[0].id)
                                .unwrap()
                                .is_empty()
                        ),
                    }
                }
            })
        })
        .collect();
    writer.join().unwrap();
    for reader in readers {
        reader.join().unwrap();
    }
    let (rows, _) = f.all_entries(list, 100);
    assert_eq!(rows.len(), 250);
}

#[test]
fn a_broken_connection_is_reopened_once() {
    let f = fixture("reopen");
    let song = f.song("Song", Some("song.m4a"));
    f.library.fail_next.store(1, Ordering::SeqCst);
    assert_eq!(
        f.library.item(song).unwrap().summary.title,
        "Song",
        "healed by reopening"
    );
    f.library.fail_next.store(2, Ordering::SeqCst);
    let problem = f.library.item(song).unwrap_err();
    assert!(problem.detail.contains("disk I/O"), "{problem:?}");
    assert_eq!(
        f.library.item(song).unwrap().summary.title,
        "Song",
        "and fine again after"
    );
}

#[test]
fn the_media_folder_moving_does_not_lose_the_library() {
    // iOS gives an updated app a new container: every stored absolute path is stale.
    let f = fixture("moved");
    let song = f.song("Song", Some("album/song.m4a"));
    fs::write(f.music.join("album/song.jpg"), b"jpeg").unwrap();
    f.db.execute(
        "UPDATE item SET cover = ?2 WHERE id = ?1",
        params![song, f.music.join("album/song.jpg").to_str()],
    )
    .unwrap();
    let list = f.playlist("List", &[song]);
    f.db.execute(
        "UPDATE collection SET cover = ?2 WHERE id = ?1",
        params![list, f.music.join("album/song.jpg").to_str()],
    )
    .unwrap();
    let new_home = f.dir.join("new-container");
    fs::create_dir_all(&new_home).unwrap();
    let moved = new_home.join("media");
    fs::rename(&f.media, &moved).unwrap();

    let library = Library::open(&f.dir.join("library.db"), &moved).unwrap();
    let row = &library.collection_items(list, None, 10).unwrap().rows[0];
    assert!(!row.item.missing, "found in the moved folder");
    let real_moved = fs::canonicalize(&moved).unwrap();
    assert!(row.item.cover.as_deref().unwrap().starts_with(&real_moved));
    assert!(
        library
            .collection(list)
            .unwrap()
            .cover
            .unwrap()
            .starts_with(&real_moved)
    );
    let file = library.item_file(song).unwrap();
    assert_eq!(file, real_moved.join("Music/album/song.m4a"));

    // A stored path that never ran through a media folder stays as it was (and is refused).
    f.db.execute("UPDATE library SET root = '/somewhere/else/Music'", [])
        .unwrap();
    assert_eq!(
        library.item_file(song).unwrap_err().kind,
        ProblemKind::Unavailable
    );
}

#[test]
fn moved_paths_are_rebased_only_through_the_media_folder_name() {
    let media = Path::new("/new/Documents/Media");
    assert_eq!(
        located("/old/Documents/Media/Videos/x [1]/001.mp4", media),
        Path::new("/new/Documents/Media/Videos/x [1]/001.mp4")
    );
    // The last folder of that name counts.
    assert_eq!(
        located("/Media/old/Documents/Media/Music", media),
        Path::new("/new/Documents/Media/Music")
    );
    assert_eq!(located("/old/Documents/Media", media), media);
    assert_eq!(
        located("/elsewhere/Music", media),
        Path::new("/elsewhere/Music")
    );
    assert_eq!(
        located("/new/Documents/Media/Music", media),
        Path::new("/new/Documents/Media/Music")
    );
}
