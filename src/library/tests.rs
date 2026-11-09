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

// --- Step 3: where the person stopped, what they finished, favorites and ratings ----

impl Fixture {
    /// An entry of `tab` with a real file, `duration_ms` long (None: unknown).
    fn entry(&self, tab: &str, title: &str, duration_ms: Option<i64>) -> i64 {
        let folder = self.media.join(tab);
        fs::create_dir_all(&folder).unwrap();
        let tab_id: i64 = self
            .db
            .query_row("SELECT id FROM category WHERE name = ?1", [tab], |row| {
                row.get(0)
            })
            .unwrap();
        let library: i64 = self
            .db
            .query_row(
                "INSERT INTO library (category_id, root) VALUES (?1, ?2)
                 ON CONFLICT (root) DO UPDATE SET root = excluded.root RETURNING id",
                params![tab_id, folder.to_str().unwrap()],
                |row| row.get(0),
            )
            .unwrap();
        let file = format!("{title}.bin");
        fs::write(folder.join(&file), b"media").unwrap();
        self.db
            .query_row(
                "INSERT INTO item (category_id, library_id, kind, path, title, sort_title, duration_ms)
                 VALUES (?1, ?2, 'entry', ?3, ?4, lower(?4), ?5) RETURNING id",
                params![tab_id, library, file, title, duration_ms],
                |row| row.get(0),
            )
            .unwrap()
    }

    fn progress(&self, id: i64) -> Option<Progress> {
        self.library.item(id).unwrap().summary.progress
    }
}

const MINUTE: i64 = 60_000;

#[test]
fn a_song_always_starts_over_and_counts_each_full_play_once() {
    let f = fixture("song");
    let song = f.entry("Music", "Song", Some(3 * MINUTE));
    assert_eq!(
        f.library.start_play(song).unwrap(),
        PlayStart {
            position_ms: 0,
            resumed: false
        }
    );
    assert!(
        f.library
            .save_progress(song, 90_000, Some(3 * MINUTE))
            .unwrap()
    );
    let half = f.progress(song).unwrap();
    assert_eq!(
        (half.position, half.percent, half.done, half.plays),
        (0, 0.5, false, 0),
        "songs keep no resume point"
    );
    f.library
        .save_progress(song, 175_000, Some(3 * MINUTE))
        .unwrap();
    assert_eq!(
        (
            f.progress(song).unwrap().done,
            f.progress(song).unwrap().plays
        ),
        (true, 1)
    );
    // Near the end again, back, and to the end once more: still the same play.
    f.library
        .save_progress(song, 178_000, Some(3 * MINUTE))
        .unwrap();
    f.library
        .save_progress(song, 60_000, Some(3 * MINUTE))
        .unwrap();
    assert!(
        !f.progress(song).unwrap().done,
        "playing it again: not done any more"
    );
    f.library
        .save_progress(song, 176_000, Some(3 * MINUTE))
        .unwrap();
    assert_eq!(
        f.progress(song).unwrap().plays,
        1,
        "seeking does not count it twice"
    );
    // A new play is counted again.
    assert_eq!(f.library.start_play(song).unwrap().position_ms, 0);
    f.library
        .save_progress(song, 3 * MINUTE, Some(3 * MINUTE))
        .unwrap();
    assert_eq!(f.progress(song).unwrap().plays, 2);
}

#[test]
fn a_movie_resumes_from_30_seconds_until_it_is_done() {
    let f = fixture("movie");
    let movie = f.entry("Movies", "Movie", Some(100 * MINUTE));
    assert_eq!(
        f.library.start_play(movie).unwrap(),
        PlayStart {
            position_ms: 0,
            resumed: false
        }
    );
    f.library.save_progress(movie, 29_999, None).unwrap();
    assert_eq!(
        f.library.start_play(movie).unwrap().position_ms,
        0,
        "under 30 s: no resume point"
    );
    f.library.save_progress(movie, 30_000, None).unwrap();
    assert_eq!(
        f.library.start_play(movie).unwrap(),
        PlayStart {
            position_ms: 30_000,
            resumed: true
        }
    );
    f.library.save_progress(movie, 61 * MINUTE, None).unwrap();
    let midway = f.progress(movie).unwrap();
    assert_eq!((midway.position, midway.done), (61 * MINUTE, false));
    assert!((midway.percent - 0.61).abs() < 1e-9, "{midway:?}");
    // In the credits (the last 2 minutes): done, counted once, and it starts over next time.
    f.library.save_progress(movie, 98 * MINUTE, None).unwrap();
    f.library.save_progress(movie, 99 * MINUTE, None).unwrap();
    let done = f.progress(movie).unwrap();
    assert_eq!(
        (done.position, done.percent, done.done, done.plays),
        (0, 1.0, true, 1)
    );
    assert_eq!(
        f.library.start_play(movie).unwrap(),
        PlayStart {
            position_ms: 0,
            resumed: false
        }
    );
}

#[test]
fn clips_start_over_and_long_audio_resumes() {
    let f = fixture("lengths");
    let clip = f.entry("Videos", "Clip", Some(5 * MINUTE - 1));
    let video = f.entry("Videos", "Video", Some(5 * MINUTE));
    let chapter = f.entry("Music", "Audiobook chapter", Some(40 * MINUTE));
    for id in [clip, video, chapter] {
        f.library.start_play(id).unwrap();
        f.library.save_progress(id, 2 * MINUTE, None).unwrap();
    }
    assert_eq!(
        f.library.start_play(clip).unwrap().position_ms,
        0,
        "a clip starts over"
    );
    assert_eq!(f.library.start_play(video).unwrap().position_ms, 2 * MINUTE);
    assert_eq!(
        f.library.start_play(chapter).unwrap().position_ms,
        2 * MINUTE,
        "long audio resumes"
    );
}

#[test]
fn the_player_teaches_the_library_the_real_length() {
    let f = fixture("length");
    let unknown = f.entry("Movies", "Unknown", None);
    // Without any length: the position is kept, but it is never done.
    f.library.save_progress(unknown, 45 * MINUTE, None).unwrap();
    let kept = f.progress(unknown).unwrap();
    assert_eq!(
        (kept.position, kept.percent, kept.done),
        (45 * MINUTE, 0.0, false)
    );
    assert_eq!(
        f.library.start_play(unknown).unwrap().position_ms,
        45 * MINUTE
    );
    // The player says how long it is: the library keeps that length from now on.
    f.library
        .save_progress(unknown, 45 * MINUTE, Some(90 * MINUTE))
        .unwrap();
    assert_eq!(
        f.library.item(unknown).unwrap().summary.duration_ms,
        Some(90 * MINUTE)
    );
    assert!((f.progress(unknown).unwrap().percent - 0.5).abs() < 1e-9);
    // Within a second of what is stored: left alone. More off: corrected.
    f.library
        .save_progress(unknown, MINUTE, Some(90 * MINUTE + 900))
        .unwrap();
    assert_eq!(
        f.library.item(unknown).unwrap().summary.duration_ms,
        Some(90 * MINUTE)
    );
    f.library
        .save_progress(unknown, MINUTE, Some(91 * MINUTE))
        .unwrap();
    assert_eq!(
        f.library.item(unknown).unwrap().summary.duration_ms,
        Some(91 * MINUTE)
    );
    // Nonsense lengths are ignored.
    f.library.save_progress(unknown, MINUTE, Some(-1)).unwrap();
    f.library.save_progress(unknown, MINUTE, Some(0)).unwrap();
    assert_eq!(
        f.library.item(unknown).unwrap().summary.duration_ms,
        Some(91 * MINUTE)
    );
}

#[test]
fn positions_outside_the_item_are_brought_inside() {
    let f = fixture("clamp");
    let movie = f.entry("Movies", "Movie", Some(100 * MINUTE));
    f.library.save_progress(movie, -5_000, None).unwrap();
    let start = f.progress(movie).unwrap();
    assert_eq!((start.position, start.percent, start.done), (0, 0.0, false));
    f.library.save_progress(movie, 500 * MINUTE, None).unwrap();
    let past = f.progress(movie).unwrap();
    assert_eq!(
        (past.position, past.percent, past.done, past.plays),
        (0, 1.0, true, 1),
        "past the end is the end"
    );
    f.library
        .save_progress(movie, i64::MAX, Some(i64::MAX))
        .unwrap();
    assert!(f.progress(movie).unwrap().done);
}

#[test]
fn what_cannot_be_played_or_is_gone_says_so() {
    let f = fixture("refusals");
    let book = f.entry("Books", "Book", None);
    let album: i64 = f
        .db
        .query_row(
            "INSERT INTO item (category_id, kind, title, sort_title) VALUES (?1, 'group', 'Album', 'album') RETURNING id",
            [f.music_tab],
            |row| row.get(0),
        )
        .unwrap();
    for id in [book, album] {
        assert_eq!(kind_of(f.library.start_play(id)), ProblemKind::Input);
        assert_eq!(
            kind_of(f.library.save_progress(id, MINUTE, None)),
            ProblemKind::Input
        );
    }
    assert_eq!(
        kind_of(f.library.set_watched(album, true)),
        ProblemKind::Input
    );
    // Gone: a save does nothing (the player may still be saving), the rest says so.
    assert!(!f.library.save_progress(9999, MINUTE, None).unwrap());
    assert_eq!(
        kind_of(f.library.start_play(9999)),
        ProblemKind::Unavailable
    );
    assert_eq!(
        kind_of(f.library.set_watched(9999, true)),
        ProblemKind::Unavailable
    );
    assert_eq!(
        kind_of(f.library.set_favorite(9999, true)),
        ProblemKind::Unavailable
    );
    assert_eq!(
        kind_of(f.library.set_rating(9999, Some(5))),
        ProblemKind::Unavailable
    );
    let movie = f.entry("Movies", "Movie", Some(100 * MINUTE));
    f.library.start_play(movie).unwrap();
    f.db.execute("DELETE FROM item WHERE id = ?1", [movie])
        .unwrap();
    assert!(
        !f.library.save_progress(movie, 10 * MINUTE, None).unwrap(),
        "deleted while playing"
    );
}

#[test]
fn watched_favorite_and_rating_marks() {
    let f = fixture("marks");
    let episode = f.entry("TV Shows", "Episode", Some(45 * MINUTE));
    f.library.save_progress(episode, 20 * MINUTE, None).unwrap();
    f.library.set_watched(episode, true).unwrap();
    let watched = f.progress(episode).unwrap();
    assert_eq!(
        (
            watched.position,
            watched.percent,
            watched.done,
            watched.plays
        ),
        (0, 1.0, true, 0),
        "marking adds no play"
    );
    f.library.set_watched(episode, false).unwrap();
    let fresh = f.progress(episode).unwrap();
    assert_eq!((fresh.position, fresh.percent, fresh.done), (0, 0.0, false));
    let never = f.entry("Movies", "Never played", Some(MINUTE * 100));
    f.library.set_watched(never, true).unwrap();
    assert!(
        f.progress(never).unwrap().done,
        "works without any progress yet"
    );

    let album: i64 = f
        .db
        .query_row(
            "INSERT INTO item (category_id, kind, title, sort_title) VALUES (?1, 'group', 'Album', 'album') RETURNING id",
            [f.music_tab],
            |row| row.get(0),
        )
        .unwrap();
    for id in [episode, album] {
        f.library.set_favorite(id, true).unwrap();
        assert!(f.library.item(id).unwrap().summary.favorite);
        f.library.set_favorite(id, false).unwrap();
        assert!(!f.library.item(id).unwrap().summary.favorite);
    }
    f.library.set_rating(episode, Some(1)).unwrap();
    f.library.set_rating(episode, Some(10)).unwrap();
    assert_eq!(f.library.item(episode).unwrap().summary.rating, Some(10));
    for bad in [0, 11, -1, i64::MAX] {
        assert_eq!(
            kind_of(f.library.set_rating(episode, Some(bad))),
            ProblemKind::Input,
            "{bad}"
        );
    }
    assert_eq!(
        f.library.item(episode).unwrap().summary.rating,
        Some(10),
        "a bad rating changes nothing"
    );
    f.library.set_rating(episode, None).unwrap();
    assert_eq!(f.library.item(episode).unwrap().summary.rating, None);
}

#[test]
fn progress_is_the_items_wherever_it_is_listed() {
    let f = fixture("everywhere");
    let movie = f.entry("Movies", "Movie", Some(100 * MINUTE));
    let one = f.playlist("One", &[movie]);
    let two = f.playlist("Two", &[movie, movie]);
    f.library.save_progress(movie, 40 * MINUTE, None).unwrap();
    for list in [one, two] {
        for row in f.library.collection_items(list, None, 10).unwrap().rows {
            assert_eq!(row.item.progress.as_ref().unwrap().position, 40 * MINUTE);
        }
    }
}

#[test]
fn saves_from_many_threads_count_one_play_once() {
    let f = fixture("threads-marks");
    let movie = f.entry("Movies", "Movie", Some(100 * MINUTE));
    let library = Arc::new(Library::open(&f.dir.join("library.db"), &f.media).unwrap());
    library.start_play(movie).unwrap();
    let savers: Vec<_> = (0..8)
        .map(|n| {
            let library = Arc::clone(&library);
            thread::spawn(move || {
                for step in 0..50 {
                    // Half the saves land in the credits.
                    let position = if (n + step) % 2 == 0 {
                        99 * MINUTE
                    } else {
                        50 * MINUTE
                    };
                    assert!(
                        library
                            .save_progress(movie, position, Some(100 * MINUTE))
                            .unwrap()
                    );
                    library.item(movie).unwrap();
                }
            })
        })
        .collect();
    for saver in savers {
        saver.join().unwrap();
    }
    assert_eq!(
        library.item(movie).unwrap().summary.progress.unwrap().plays,
        1
    );
}

#[test]
fn a_save_that_hit_a_broken_connection_is_saved_and_counted_once() {
    let f = fixture("retry-marks");
    let movie = f.entry("Movies", "Movie", Some(100 * MINUTE));
    f.library.start_play(movie).unwrap();
    f.library.fail_next.store(1, Ordering::SeqCst);
    assert!(f.library.save_progress(movie, 99 * MINUTE, None).unwrap());
    let progress = f.progress(movie).unwrap();
    assert_eq!((progress.done, progress.plays), (true, 1));
    f.library.fail_next.store(2, Ordering::SeqCst);
    assert!(
        f.library.set_favorite(movie, true).is_err(),
        "two failures in a row: an error"
    );
    assert!(
        !f.library.item(movie).unwrap().summary.favorite,
        "and nothing half-saved"
    );
}

// --- Step 3, edge cases from a full review ----------------------------------------------

#[test]
fn a_resume_point_past_a_shorter_file_starts_over() {
    // The file was replaced by a shorter one: the old resume point lies past its end.
    let f = fixture("shorter");
    let movie = f.entry("Movies", "Movie", Some(100 * MINUTE));
    f.library.save_progress(movie, 80 * MINUTE, None).unwrap();
    f.db.execute(
        "UPDATE item SET duration_ms = ?2 WHERE id = ?1",
        params![movie, 60 * MINUTE],
    )
    .unwrap();
    assert_eq!(
        f.library.start_play(movie).unwrap(),
        PlayStart {
            position_ms: 0,
            resumed: false
        }
    );
}

#[test]
fn audio_of_unknown_length_starts_over_like_a_song() {
    let f = fixture("unknown-audio");
    let track = f.entry("Music", "Track", None);
    f.library.save_progress(track, 25 * MINUTE, None).unwrap();
    let saved = f.progress(track).unwrap();
    assert_eq!(
        (saved.position, saved.done),
        (0, false),
        "no resume point without a length"
    );
    assert_eq!(f.library.start_play(track).unwrap().position_ms, 0);
}

#[test]
fn a_play_without_start_play_counts_once_until_the_next_start() {
    let f = fixture("no-start");
    let song = f.entry("Music", "Song", Some(3 * MINUTE));
    f.library.save_progress(song, 3 * MINUTE, None).unwrap();
    f.library.save_progress(song, MINUTE, None).unwrap();
    f.library.save_progress(song, 3 * MINUTE, None).unwrap();
    assert_eq!(
        f.progress(song).unwrap().plays,
        1,
        "the same play, never started"
    );
    f.library.start_play(song).unwrap();
    f.library.save_progress(song, 3 * MINUTE, None).unwrap();
    assert_eq!(f.progress(song).unwrap().plays, 2);
}

#[test]
fn going_back_after_the_credits_keeps_the_new_spot() {
    let f = fixture("back-from-credits");
    let movie = f.entry("Movies", "Movie", Some(100 * MINUTE));
    f.library.start_play(movie).unwrap();
    f.library.save_progress(movie, 99 * MINUTE, None).unwrap();
    // Back to a favourite scene, then paused there.
    f.library.save_progress(movie, 50 * MINUTE, None).unwrap();
    let back = f.progress(movie).unwrap();
    assert_eq!(
        (back.position, back.done, back.plays),
        (50 * MINUTE, false, 1)
    );
    assert_eq!(
        f.library.start_play(movie).unwrap().position_ms,
        50 * MINUTE
    );
    f.library.set_watched(movie, false).unwrap();
    assert_eq!(
        f.library.start_play(movie).unwrap().position_ms,
        0,
        "not watched: from the top"
    );
}

#[test]
fn saves_wait_for_another_writer_instead_of_failing() {
    let f = fixture("busy");
    let movie = f.entry("Movies", "Movie", Some(100 * MINUTE));
    let path = f.dir.join("library.db");
    let (locked, is_locked) = std::sync::mpsc::channel();
    // Another connection (a playlist save, say) holds the write lock for 0.8 s.
    let writer = thread::spawn(move || {
        let mut db = crate::db::open(&path).unwrap();
        let tx = db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .unwrap();
        tx.execute("UPDATE item SET title = title", []).unwrap();
        locked.send(()).unwrap();
        thread::sleep(std::time::Duration::from_millis(800));
        tx.commit().unwrap();
    });
    is_locked.recv().unwrap();
    let started = std::time::Instant::now();
    assert!(
        f.library.save_progress(movie, 40 * MINUTE, None).unwrap(),
        "waited, then saved"
    );
    assert!(
        started.elapsed() >= std::time::Duration::from_millis(500),
        "it did wait for the lock"
    );
    writer.join().unwrap();
    assert_eq!(f.progress(movie).unwrap().position, 40 * MINUTE);
}

#[test]
fn every_save_moves_the_item_up_in_recently_played() {
    let f = fixture("recent");
    let movie = f.entry("Movies", "Movie", Some(100 * MINUTE));
    f.library.save_progress(movie, MINUTE, None).unwrap();
    f.db.execute(
        "UPDATE progress SET opened_at = 0 WHERE item_id = ?1",
        [movie],
    )
    .unwrap();
    f.library.save_progress(movie, 2 * MINUTE, None).unwrap();
    let opened: i64 =
        f.db.query_row(
            "SELECT opened_at FROM progress WHERE item_id = ?1",
            [movie],
            |row| row.get(0),
        )
        .unwrap();
    assert!(opened > 1_700_000_000, "{opened}");
}

#[test]
fn many_items_played_from_many_threads_each_count_their_own_plays() {
    let f = fixture("many-items");
    let items: Vec<i64> = (0..4)
        .map(|n| f.entry("Movies", &format!("Movie {n}"), Some(100 * MINUTE)))
        .collect();
    let library = Arc::new(Library::open(&f.dir.join("library.db"), &f.media).unwrap());
    let players: Vec<_> = items
        .iter()
        .map(|&item| {
            let library = Arc::clone(&library);
            thread::spawn(move || {
                for _ in 0..5 {
                    library.start_play(item).unwrap();
                    library.save_progress(item, 30 * MINUTE, None).unwrap();
                    library.save_progress(item, 99 * MINUTE, None).unwrap();
                    library.save_progress(item, 99 * MINUTE + 1, None).unwrap();
                }
            })
        })
        .collect();
    for player in players {
        player.join().unwrap();
    }
    for item in items {
        assert_eq!(
            library.item(item).unwrap().summary.progress.unwrap().plays,
            5,
            "item {item}"
        );
    }
}

#[test]
fn remembered_plays_stay_bounded() {
    let f = fixture("bounded");
    let movie = f.entry("Movies", "Movie", Some(100 * MINUTE));
    {
        let mut counted = f.library.counted.lock().unwrap();
        for id in 0..10_000 {
            counted.insert(-1 - id, id % 2 == 0);
        }
    }
    f.library.start_play(movie).unwrap();
    let size = f.library.counted.lock().unwrap().len();
    assert!(size <= 5_001, "counted plays are forgotten first: {size}");
    {
        let mut counted = f.library.counted.lock().unwrap();
        for id in 0..10_000 {
            counted.insert(-20_000 - id, false);
        }
    }
    f.library.start_play(movie).unwrap();
    assert_eq!(
        f.library.counted.lock().unwrap().len(),
        1,
        "past the bound: only the new play"
    );
    // And counting still works.
    f.library.save_progress(movie, 99 * MINUTE, None).unwrap();
    assert_eq!(f.progress(movie).unwrap().plays, 1);
}

#[test]
fn ratings_and_favorites_for_albums_and_shows() {
    let f = fixture("group-marks");
    let show: i64 = f
        .db
        .query_row(
            "INSERT INTO item (category_id, kind, title, sort_title) VALUES (?1, 'group', 'Show', 'show') RETURNING id",
            [f.music_tab],
            |row| row.get(0),
        )
        .unwrap();
    f.library.set_rating(show, Some(7)).unwrap();
    f.library.set_favorite(show, true).unwrap();
    let summary = f.library.item(show).unwrap().summary;
    assert_eq!((summary.rating, summary.favorite), (Some(7), true));
}

#[test]
fn stored_lengths_of_zero_or_less_count_as_unknown() {
    let f = fixture("bad-lengths");
    let movie = f.entry("Movies", "Movie", Some(0));
    f.library.save_progress(movie, 45 * MINUTE, None).unwrap();
    let saved = f.progress(movie).unwrap();
    assert_eq!(
        (saved.position, saved.done),
        (45 * MINUTE, false),
        "unknown length: kept, never done"
    );
    f.db.execute("UPDATE item SET duration_ms = -10 WHERE id = ?1", [movie])
        .unwrap();
    assert_eq!(
        f.library.start_play(movie).unwrap().position_ms,
        45 * MINUTE
    );
}

// --- Adding files that are already in a tab's folder ------------------------------------

#[test]
fn a_file_in_a_tab_folder_is_added_once() {
    let f = fixture("add-file");
    let folder = f.media.join("Music/Album");
    fs::create_dir_all(&folder).unwrap();
    fs::write(folder.join("01 Song.m4a"), b"audio").unwrap();
    let id = f
        .library
        .add_file(f.music_tab, &folder.join("01 Song.m4a"))
        .unwrap();
    let detail = f.library.item(id).unwrap();
    assert_eq!(
        (
            detail.summary.title.as_str(),
            detail.summary.kind,
            detail.summary.missing
        ),
        ("01 Song", ItemKind::Entry, false)
    );
    assert_eq!(
        f.library.item_file(id).unwrap(),
        fs::canonicalize(folder.join("01 Song.m4a")).unwrap()
    );
    assert_eq!(
        f.library
            .add_file(f.music_tab, &folder.join("01 Song.m4a"))
            .unwrap(),
        id,
        "the same file, the same id"
    );
    // Through a link inside the tab folder to the same file: still the same item.
    std::os::unix::fs::symlink(folder.join("01 Song.m4a"), f.music.join("link.m4a")).unwrap();
    assert_eq!(
        f.library
            .add_file(f.music_tab, &f.music.join("link.m4a"))
            .unwrap(),
        id
    );
    // A file that went missing and came back is found again.
    f.db.execute("UPDATE item SET missing_since = 1 WHERE id = ?1", [id])
        .unwrap();
    assert_eq!(
        f.library
            .add_file(f.music_tab, &folder.join("01 Song.m4a"))
            .unwrap(),
        id
    );
    assert!(!f.library.item(id).unwrap().summary.missing);
}

#[test]
fn files_outside_their_tab_folder_or_not_files_are_refused() {
    let f = fixture("add-refused");
    let movies: i64 =
        f.db.query_row("SELECT id FROM category WHERE name = 'Movies'", [], |row| {
            row.get(0)
        })
        .unwrap();
    fs::write(f.music.join("song.m4a"), b"audio").unwrap();
    fs::write(f.media.join("loose.m4a"), b"audio").unwrap();
    fs::write(f.dir.join("outside.m4a"), b"audio").unwrap();
    std::os::unix::fs::symlink(f.dir.join("outside.m4a"), f.music.join("escape.m4a")).unwrap();
    fs::create_dir_all(f.music.join("a folder")).unwrap();
    for (tab, path, kind, what) in [
        (
            movies,
            f.music.join("song.m4a"),
            ProblemKind::Input,
            "in another tab's folder",
        ),
        (
            f.music_tab,
            f.media.join("loose.m4a"),
            ProblemKind::Input,
            "in the media folder, not a tab's",
        ),
        (
            f.music_tab,
            f.dir.join("outside.m4a"),
            ProblemKind::Input,
            "outside the media folder",
        ),
        (
            f.music_tab,
            f.music.join("escape.m4a"),
            ProblemKind::Input,
            "a link out of the media folder",
        ),
        (
            f.music_tab,
            f.music.join("a folder"),
            ProblemKind::Input,
            "a folder",
        ),
        (
            f.music_tab,
            f.music.join("missing.m4a"),
            ProblemKind::Unavailable,
            "missing",
        ),
        (
            9999,
            f.music.join("song.m4a"),
            ProblemKind::Unavailable,
            "an unknown tab",
        ),
    ] {
        let problem = f.library.add_file(tab, &path).unwrap_err();
        assert_eq!(
            (problem.kind, problem.retry),
            (kind, false),
            "{what}: {problem:?}"
        );
    }
    let items: i64 =
        f.db.query_row("SELECT count(*) FROM item", [], |row| row.get(0))
            .unwrap();
    assert_eq!(items, 0, "nothing was added");
}
