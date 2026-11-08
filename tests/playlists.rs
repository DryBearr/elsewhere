//! Tests for src/playlists, fully offline: the local test server (tests/common) serves a
//! playlist page with two videos and stands in for the lyrics service.
//! Saving a real YouTube playlist is marked #[ignore]: `cargo test --features playlists -- --ignored`.

mod common;

use std::path::Path;
use std::sync::{Arc, Mutex, Once};
use std::time::Duration;

use common::{folder, ready, url};
use elsewhere::jobs::{JobState, Jobs, Level, Status};
use elsewhere::playlists::{self, Format, Options, Progress};
use elsewhere::problem::{self, Kind};
use elsewhere::ytdlp::Cancel;
use rusqlite::Connection;

/// yt-dlp is ready and lyrics come from the test server.
fn setup() {
    static LYRICS: Once = Once::new();
    ready();
    LYRICS.call_once(|| {
        // SAFETY: set once, before any test reads it; nothing else here changes the environment.
        unsafe { std::env::set_var("ELSEWHERE_LRCLIB_URL", common::server()) };
        // Connection trouble is tried again after 20 ms here, not after seconds.
        problem::set_retry_delays(vec![Duration::from_millis(20); 3]);
    });
}

fn options() -> Options {
    // Video: the test media are plain mp4 files, which have no separate audio stream.
    Options {
        format: Format::Video {
            max_height: Some(720),
        },
        lyrics: true,
        limit: None,
    }
}

fn progress() -> (
    Arc<Mutex<Vec<Progress>>>,
    impl Fn(Progress) + Send + Sync + 'static,
) {
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    // Working is only a sign of life for the watchdog; the tests look at the rest.
    (events, move |event| {
        if !matches!(event, Progress::Working) {
            sink.lock().unwrap().push(event)
        }
    })
}

fn collection_items(db: &Path, collection_id: i64) -> Vec<(i64, String)> {
    let db = Connection::open(db).unwrap();
    let mut stmt = db
        .prepare(
            "SELECT item.id, item.title FROM collection_item
             JOIN item ON item.id = collection_item.item_id
             WHERE collection_id = ?1 ORDER BY collection_item.position",
        )
        .unwrap();
    stmt.query_map([collection_id], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

#[test]
fn saves_a_playlist_with_lyrics_then_only_new_items() {
    setup();
    let dir = folder("playlist");
    let db = dir.join("library.db");
    let (events, on_progress) = progress();

    let saved = playlists::save(
        &db,
        &dir,
        &url("/playlist.html"),
        &options(),
        on_progress,
        &Cancel::new(),
    )
    .unwrap();
    assert_eq!(saved.title, "Test Playlist");
    assert!(saved.failed.is_empty(), "{:?}", saved.failed);
    assert_eq!(saved.items.len(), 2);
    assert!(
        saved.folder.starts_with(dir.join("Videos")),
        "{}",
        saved.folder.display()
    );
    for (index, item) in saved.items.iter().enumerate() {
        assert_eq!(item.position, index + 1);
        assert!(item.file.is_file(), "{}", item.file.display());
        assert!(
            item.file
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(&format!("{:03} - ", index + 1))
        );
    }

    // Lyrics: synced for "Band - Hit" (.lrc), plain only for "Band - Other" (.txt).
    let hit = saved.items[0]
        .lyrics
        .as_ref()
        .expect("synced lyrics for Hit");
    assert_eq!(hit.extension().unwrap(), "lrc");
    assert!(
        std::fs::read_to_string(hit)
            .unwrap()
            .contains("[00:01.00] la la")
    );
    let other = saved.items[1]
        .lyrics
        .as_ref()
        .expect("plain lyrics for Other");
    assert_eq!(other.extension().unwrap(), "txt");

    // The library: a playlist with both items in order.
    let collection_id = saved
        .collection_id
        .expect("a playlist link makes a playlist");
    let titles: Vec<String> = collection_items(&db, collection_id)
        .into_iter()
        .map(|(_, title)| title)
        .collect();
    assert_eq!(titles, ["Band - Hit", "Band - Other"]);
    {
        let events = events.lock().unwrap();
        assert!(
            matches!(events[0], Progress::Listed { total: 2, .. }),
            "{events:?}"
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event, Progress::Downloading { .. }))
        );
    }

    // Again: nothing new to download, same items, same playlist.
    let (events, on_progress) = progress();
    let again = playlists::save(
        &db,
        &dir,
        &url("/playlist.html"),
        &options(),
        on_progress,
        &Cancel::new(),
    )
    .unwrap();
    assert_eq!(again.collection_id, Some(collection_id));
    let ids: Vec<i64> = again.items.iter().map(|item| item.item_id).collect();
    assert_eq!(
        ids,
        saved
            .items
            .iter()
            .map(|item| item.item_id)
            .collect::<Vec<_>>()
    );
    let events = events.lock().unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, Progress::Skipped { .. }))
            .count(),
        2,
        "{events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, Progress::Downloading { .. }))
    );
}

#[test]
fn a_failing_item_is_reported_and_the_rest_saved() {
    setup();
    let dir = folder("broken-playlist");
    let db = dir.join("library.db");
    let saved = playlists::save(
        &db,
        &dir,
        &url("/broken-playlist.html"),
        &options(),
        |_| {},
        &Cancel::new(),
    )
    .unwrap();
    assert_eq!(saved.items.len(), 1);
    assert_eq!(saved.failed.len(), 1);
    assert_eq!(saved.failed[0].position, 2);
    assert!(
        saved.failed[0].problem.detail.contains("404"),
        "{:?}",
        saved.failed[0].problem
    );
    assert_eq!(collection_items(&db, saved.collection_id.unwrap()).len(), 1);
}

#[test]
fn a_flaky_server_is_tried_again_until_it_works() {
    setup();
    let dir = folder("flaky");
    let (events, on_progress) = progress();
    let saved = playlists::save(
        &dir.join("library.db"),
        &dir,
        &url("/flaky-playlist-save.html"),
        &options(),
        on_progress,
        &Cancel::new(),
    )
    .unwrap();
    assert_eq!((saved.items.len(), saved.failed.len()), (1, 0), "{saved:?}");
    let events = events.lock().unwrap();
    let tries: Vec<(usize, usize)> = events
        .iter()
        .filter_map(|event| match event {
            Progress::Retrying {
                position: Some(1),
                attempt,
                of,
                problem,
            } => {
                assert_eq!(problem.kind, Kind::Network, "{problem:?}");
                Some((*attempt, *of))
            }
            _ => None,
        })
        .collect();
    // yt-dlp tries 11 times itself; the server fails 22 requests, so about 2 of ours.
    assert!(
        !tries.is_empty() && tries.iter().all(|&(attempt, of)| attempt >= 2 && of == 4),
        "{events:?}"
    );
    assert!(common::hits("/flaky-save.mp4") > 22);
}

#[test]
fn a_server_that_stays_down_fails_the_item_after_four_tries() {
    setup();
    let dir = folder("down");
    let (events, on_progress) = progress();
    let saved = playlists::save(
        &dir.join("library.db"),
        &dir,
        &url("/down-playlist.html"),
        &options(),
        on_progress,
        &Cancel::new(),
    )
    .unwrap();
    assert_eq!((saved.items.len(), saved.failed.len()), (0, 1));
    let problem = &saved.failed[0].problem;
    assert_eq!(
        (problem.kind, problem.retry),
        (Kind::Network, true),
        "{problem:?}"
    );
    assert!(problem.detail.contains("503"), "{problem:?}");
    let retries = events
        .lock()
        .unwrap()
        .iter()
        .filter(|event| matches!(event, Progress::Retrying { .. }))
        .count();
    assert_eq!(retries, 3);
    assert!(common::hits("/down.mp4") >= 4);
}

#[test]
fn a_single_video_link_saves_one_item_without_a_playlist() {
    setup();
    let dir = folder("single");
    let db = dir.join("library.db");
    let saved = playlists::save(
        &db,
        &dir,
        &url("/Solo%20Clip.mp4"),
        &options(),
        |_| {},
        &Cancel::new(),
    )
    .unwrap();
    assert_eq!(saved.collection_id, None);
    assert_eq!(saved.items.len(), 1);
    assert_eq!(saved.items[0].title, "Solo Clip");
    assert!(
        saved.items[0].lyrics.is_none(),
        "no artist, so no lyrics search"
    );
}

#[test]
fn limit_saves_only_the_first_items() {
    setup();
    let dir = folder("limit");
    let db = dir.join("library.db");
    let options = Options {
        limit: Some(1),
        ..options()
    };
    let saved = playlists::save(
        &db,
        &dir,
        &url("/playlist.html"),
        &options,
        |_| {},
        &Cancel::new(),
    )
    .unwrap();
    assert_eq!(saved.items.len(), 1);
    assert_eq!(saved.items[0].title, "Band - Hit");
}

#[test]
fn cancelled_before_it_starts() {
    setup();
    let dir = folder("cancelled");
    let cancel = Cancel::new();
    cancel.cancel();
    let result = playlists::save(
        &dir.join("library.db"),
        &dir,
        &url("/playlist.html"),
        &options(),
        |_| {},
        &cancel,
    );
    assert!(
        matches!(
            result,
            Err(ref problem) if problem.is_cancelled()
        ),
        "{result:?}"
    );
}

#[test]
#[ignore = "reaches youtube.com"]
fn real_youtube_music_playlist() {
    setup();
    // SAFETY: as in setup(); this test runs on its own with --ignored.
    unsafe { std::env::remove_var("ELSEWHERE_LRCLIB_URL") };
    let dir = folder("youtube-playlist");
    let db = dir.join("library.db");
    let options = Options {
        format: Format::Audio,
        lyrics: true,
        limit: Some(2),
    };
    // YouTube's "Popular Music Videos" playlist; only its first two songs.
    let link = "https://www.youtube.com/playlist?list=PLFgquLnL59alCl_2TQvOiD5Vgm1hCaGSI";
    let saved = playlists::save(
        &db,
        &dir,
        link,
        &options,
        |event| println!("{event:?}"),
        &Cancel::new(),
    )
    .unwrap();
    println!("{saved:#?}");
    assert_eq!(saved.items.len(), 2, "{:?}", saved.failed);
    assert!(saved.cover.is_some(), "the playlist banner");
    assert!(
        saved.items.iter().all(|item| item.image.is_some()),
        "every item's image"
    );
}

#[test]
#[ignore = "reaches youtube.com"]
fn real_youtube_video_quality() {
    setup();
    let dir = folder("youtube-quality");
    let options = Options {
        format: Format::Video {
            max_height: Some(480),
        },
        lyrics: false,
        limit: Some(1),
    };
    let link = "https://www.youtube.com/playlist?list=PLFgquLnL59alCl_2TQvOiD5Vgm1hCaGSI";
    let saved = playlists::save(
        &dir.join("library.db"),
        &dir,
        link,
        &options,
        |_| {},
        &Cancel::new(),
    )
    .unwrap();
    assert_eq!(saved.items.len(), 1, "{:?}", saved.failed);
    let file = &saved.items[0].file;
    assert_eq!(file.extension().unwrap(), "mp4");
    println!("{}", file.display());
}

/// Runs a playlist save as a job on its own Jobs, the way the app will: the runner is
/// registered with the folders, the job started with params. Records every state.
fn run_as_job(link: &str, name: &str) -> (Jobs, Arc<Mutex<Vec<JobState>>>, u64) {
    setup();
    let dir = folder(name);
    let jobs = Jobs::new(2);
    jobs.register("playlist", move |job, params| {
        playlists::run_job(job, &dir.join("library.db"), &dir, params)
    });
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = seen.clone();
    jobs.set_listener(move |state| sink.lock().unwrap().push(state));
    let id = jobs
        .start("playlist", link, &playlists::job_params(link, &options()))
        .unwrap();
    (jobs, seen, id)
}

fn finished(jobs: &Jobs, seen: &Mutex<Vec<JobState>>, id: u64) -> JobState {
    for _ in 0..3000 {
        let state = jobs.get(id).unwrap();
        let delivered = seen
            .lock()
            .unwrap()
            .iter()
            .any(|seen| seen.revision == state.revision);
        if state.status.is_finished() && delivered {
            return state;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    panic!("job did not finish: {:?}", jobs.get(id));
}

#[test]
fn a_playlist_job_reports_items_and_ends_done() {
    let (jobs, seen, id) = run_as_job(&url("/playlist.html"), "job");
    let last = finished(&jobs, &seen, id);
    assert_eq!(last.status, Status::Done, "{last:?}");
    assert_eq!(
        (last.title.as_str(), last.done, last.failed, last.total),
        ("Test Playlist", 2, 0, Some(2))
    );
    let states = seen.lock().unwrap().clone();
    assert!(
        states
            .windows(2)
            .all(|pair| pair[0].revision < pair[1].revision)
    );
    let log = jobs.logs(id, 0);
    assert!(
        log.last().unwrap().message.starts_with("Saved 2 of 2"),
        "{log:?}"
    );
}

#[test]
fn a_playlist_job_with_a_broken_item_is_done_with_the_error_shown() {
    let (jobs, seen, id) = run_as_job(&url("/broken-playlist.html"), "job-broken");
    let last = finished(&jobs, &seen, id);
    assert_eq!(
        (last.status, last.done, last.failed),
        (Status::Done, 1, 1),
        "{last:?}"
    );
    assert!(
        last.last_error.as_ref().unwrap().text.starts_with("#2"),
        "{last:?}"
    );
    assert!(
        jobs.logs(id, 0)
            .iter()
            .any(|line| line.level == Level::Error && line.message.contains("404"))
    );
}

#[test]
fn a_playlist_job_logs_each_new_try() {
    let (jobs, seen, id) = run_as_job(&url("/flaky-playlist-job.html"), "job-flaky");
    let last = finished(&jobs, &seen, id);
    assert_eq!(
        (last.status, last.done, last.failed),
        (Status::Done, 1, 0),
        "{last:?}"
    );
    let log = jobs.logs(id, 0);
    assert!(
        log.iter().any(|line| line.level == Level::Warning
            && line.message.starts_with("#1: ")
            && line.message.ends_with("(try 2 of 4)")),
        "{log:?}"
    );
}

#[test]
fn cancelling_a_playlist_job_stops_the_download() {
    // The first item downloads slowly (about a second), so a cancel soon after the job
    // starts lands while it reads the playlist or downloads that item, however fast the
    // machine is. Either way the job must end Cancelled with nothing saved.
    let (jobs, seen, id) = run_as_job(&url("/slow-playlist.html"), "job-cancel");
    for _ in 0..6000 {
        if jobs.get(id).unwrap().status == Status::Running {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert!(jobs.cancel(id), "{:?}", jobs.get(id));
    let last = finished(&jobs, &seen, id);
    assert_eq!(last.status, Status::Cancelled, "{last:?}");
    assert_eq!(
        last.done, 0,
        "cancelled before the slow first item finished: {last:?}"
    );
}

#[test]
fn two_saves_of_the_same_playlist_at_once_both_succeed() {
    setup();
    let dir = folder("twice");
    let db = dir.join("library.db");
    // The slow file takes about a second, so the two saves overlap: the second must wait
    // for the first instead of writing the same files at the same time.
    let saves: Vec<_> = (0..2)
        .map(|_| {
            let (dir, db) = (dir.clone(), db.clone());
            let (events, on_progress) = progress();
            let save = std::thread::spawn(move || {
                playlists::save(
                    &db,
                    &dir,
                    &url("/slow-playlist.html"),
                    &options(),
                    on_progress,
                    &Cancel::new(),
                )
            });
            (save, events)
        })
        .collect();
    let mut waited = 0;
    for (save, events) in saves {
        let saved = save.join().unwrap().unwrap();
        assert_eq!(
            (saved.items.len(), saved.failed.len()),
            (2, 0),
            "{:?}",
            saved.failed
        );
        waited += events.lock().unwrap().iter().filter(|event| {
            matches!(event, Progress::Log { message, .. } if message.starts_with("Waiting for another save"))
        })
        .count();
    }
    assert_eq!(waited, 1, "exactly one save waited for the other");
    let collections: i64 = Connection::open(&db)
        .unwrap()
        .query_row("SELECT count(*) FROM collection", [], |row| row.get(0))
        .unwrap();
    assert_eq!(collections, 1, "one playlist, not two");
}

fn collection_titles(db: &Path, source_like: &str) -> Vec<String> {
    let db = Connection::open(db).unwrap();
    let id: i64 = db
        .query_row(
            "SELECT id FROM collection WHERE name = ?1",
            [source_like],
            |row| row.get(0),
        )
        .unwrap();
    collection_items(&dbpath(&db), id)
        .into_iter()
        .map(|(_, title)| title)
        .collect()
}

fn dbpath(db: &Connection) -> std::path::PathBuf {
    std::path::PathBuf::from(db.path().unwrap())
}

#[test]
fn a_playlist_that_changed_since_it_was_saved() {
    setup();
    let dir = folder("changing");
    let db = dir.join("library.db");
    let save = || {
        let (events, on_progress) = progress();
        let saved = playlists::save(
            &db,
            &dir,
            &url("/changing-playlist.html"),
            &options(),
            on_progress,
            &Cancel::new(),
        )
        .unwrap();
        let notes: Vec<String> = events
            .lock()
            .unwrap()
            .iter()
            .filter_map(|event| match event {
                Progress::Note { message } => Some(message.clone()),
                _ => None,
            })
            .collect();
        (saved, notes)
    };

    common::set_changing_playlist(1);
    let (first, notes) = save();
    assert_eq!((first.items.len(), first.dropped), (3, 0));
    assert!(notes.is_empty(), "{notes:?}");
    let two = first
        .items
        .iter()
        .find(|item| item.title == "Ch - Two")
        .unwrap()
        .file
        .clone();

    // "Two" left the playlist and the rest moved around: it leaves the playlist, its file
    // and its library entry stay, and the person is told.
    common::set_changing_playlist(2);
    let (second, notes) = save();
    assert_eq!(
        (second.items.len(), second.failed.len(), second.dropped),
        (2, 0, 1)
    );
    assert_eq!(
        collection_titles(&db, "Changing Playlist"),
        ["Ch - Three", "Ch - One"]
    );
    assert!(two.is_file(), "the file of the song that left stays");
    let still: i64 = Connection::open(&db)
        .unwrap()
        .query_row(
            "SELECT count(*) FROM item WHERE title = 'Ch - Two'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(still, 1, "and so does its library entry");
    assert!(
        notes
            .iter()
            .any(|note| note.starts_with("1 item is no longer in this playlist")),
        "{notes:?}"
    );

    // "Two" is listed again, but the site removed it: the copy saved before is kept.
    common::set_changing_playlist(3);
    let (third, _) = save();
    assert_eq!(
        (third.items.len(), third.failed.len(), third.dropped),
        (3, 0, 0),
        "{:?}",
        third.failed
    );
    assert_eq!(
        collection_titles(&db, "Changing Playlist"),
        ["Ch - One", "Ch - Two", "Ch - Three"]
    );
    common::set_changing_playlist(1);
}

#[test]
fn a_preview_says_what_a_link_holds() {
    setup();
    let preview = playlists::preview(&url("/playlist.html"), &Cancel::new()).unwrap();
    assert_eq!(
        (
            preview.title.as_str(),
            preview.is_playlist,
            preview.available,
            preview.complete,
            preview.generated
        ),
        ("Test Playlist", true, 2, true, false)
    );
    assert!(preview.unavailable.is_empty());
    assert_eq!(
        preview.suggested_limit, None,
        "only generated playlists get one"
    );
    let single = playlists::preview(&url("/Band%20-%20Hit.mp4"), &Cancel::new()).unwrap();
    assert_eq!((single.is_playlist, single.available), (false, 1));
    let problem = playlists::preview(&url("/missing.mp4"), &Cancel::new()).unwrap_err();
    assert_eq!(problem.kind, Kind::Unavailable, "{problem:?}");
    let cancel = Cancel::new();
    cancel.cancel();
    assert!(
        playlists::preview(&url("/playlist.html"), &cancel)
            .unwrap_err()
            .is_cancelled()
    );
}

#[test]
fn a_limit_past_the_end_saves_everything_once() {
    setup();
    let dir = folder("limit-past-end");
    let mut opts = options();
    opts.limit = Some(500);
    let saved = playlists::save(
        &dir.join("library.db"),
        &dir,
        &url("/playlist.html"),
        &opts,
        |_| {},
        &Cancel::new(),
    )
    .unwrap();
    assert_eq!((saved.items.len(), saved.unavailable.len()), (2, 0));
}

#[test]
fn a_page_with_nothing_to_save_changes_nothing() {
    setup();
    let dir = folder("nothing-to-save");
    let db = dir.join("library.db");
    let problem = playlists::save(
        &db,
        &dir,
        &url("/empty-page.html"),
        &options(),
        |_| {},
        &Cancel::new(),
    )
    .unwrap_err();
    assert!(
        matches!(problem.kind, Kind::Input | Kind::Unavailable),
        "{problem:?}"
    );
    assert!(!problem.retry, "{problem:?}");
    assert!(!dir.join("Videos").exists(), "no folder made");
    if db.exists() {
        let lists: i64 = Connection::open(&db)
            .unwrap()
            .query_row("SELECT count(*) FROM collection", [], |row| row.get(0))
            .unwrap();
        assert_eq!(lists, 0, "no playlist made");
    }
}
