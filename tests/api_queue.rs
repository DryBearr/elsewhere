//! Tests for the playlist player as the apps get it (src/api/queue.rs): a real core, a
//! playlist of real media saved through yt-dlp from the local test server, and the media
//! engine with null outputs. Needs libmpv (ELSEWHERE_LIBMPV on a desktop without one).

mod common;

use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use common::{api_folders, kind_of, url};
use elsewhere::api::{
    self, CollectionSummary, CoreQueue, JobStatus, MediaFormat, PlaylistOptions, ProblemKind,
    QueueListener, QueueState, RepeatMode,
};

fn folders() -> &'static api::Folders {
    static FOLDERS: OnceLock<api::Folders> = OnceLock::new();
    FOLDERS.get_or_init(|| api_folders("api-queue"))
}

/// Starts the core once and saves the real playlist (4 entries: song.m4a, song.opus,
/// long.m4a, clip.webm) into the library.
fn playlist() -> &'static CollectionSummary {
    static LIST: OnceLock<CollectionSummary> = OnceLock::new();
    LIST.get_or_init(|| {
        elsewhere::player::set_null_output(true);
        api::start(folders().clone(), None).unwrap();
        let id = api::save_playlist(
            url("/real-playlist.html"),
            PlaylistOptions {
                format: MediaFormat::Video { max_height: None },
                lyrics: false,
                limit: None,
            },
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(120);
        let job = loop {
            let job = api::job(id).unwrap().unwrap();
            if matches!(
                job.status,
                JobStatus::Done | JobStatus::Failed | JobStatus::Cancelled
            ) {
                break job;
            }
            assert!(
                Instant::now() < deadline,
                "the save did not finish: {job:?}"
            );
            thread::sleep(Duration::from_millis(20));
        };
        assert_eq!(
            (job.status, job.done, job.failed),
            (JobStatus::Done, 4, 0),
            "{job:?}"
        );
        let videos = api::categories()
            .unwrap()
            .into_iter()
            .find(|tab| tab.name == "Videos")
            .unwrap();
        api::collections(videos.id)
            .unwrap()
            .into_iter()
            .find(|list| list.name == "Real Playlist")
            .unwrap()
    })
}

fn entries(list: &CollectionSummary) -> Vec<api::CollectionEntry> {
    api::collection_items(list.id, None, 0).unwrap().entries
}

type Seen = Arc<Mutex<Vec<QueueState>>>;

struct Recorder(Seen);

impl QueueListener for Recorder {
    fn on_state(&self, state: QueueState) {
        self.0.lock().unwrap().push(state);
    }
}

fn queue() -> (Arc<CoreQueue>, Seen) {
    playlist();
    let queue = CoreQueue::new().expect("libmpv loads");
    let seen = Seen::default();
    queue.set_listener(Box::new(Recorder(Arc::clone(&seen))));
    (queue, seen)
}

fn wait(queue: &CoreQueue, what: &str, check: impl Fn(&QueueState) -> bool) -> QueueState {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let state = queue.state();
        if check(&state) {
            return state;
        }
        assert!(Instant::now() < deadline, "never {what}: {state:?}");
        thread::sleep(Duration::from_millis(5));
    }
}

/// The current entry's item, from the queue's entries as they were for `state`.
fn current_item(queue: &CoreQueue, state: &QueueState) -> Option<i64> {
    let entries = queue.entries();
    assert_eq!(
        entries.revision, state.entries_revision,
        "the entries changed since"
    );
    let current = state.current?;
    let entry = entries.entries[current as usize];
    assert_eq!(Some(entry.key), state.current_key);
    Some(entry.item_id)
}

fn plays(item: i64) -> i64 {
    api::item(item)
        .unwrap()
        .summary
        .progress
        .map_or(0, |progress| progress.plays)
}

/// The apps' UI tests save this page (scripts/selftest_server.py) as songs and play it from
/// the Music tab: the same here. This test server says video/mp4 for every file, so yt-dlp
/// sees no audio-only format and takes the best single file (the UI tests' server says
/// audio/mp4: there it is the audio-only one).
#[test]
fn a_page_of_songs_saves_into_music_and_plays() {
    playlist();
    let id = api::save_playlist(
        url("/music-playlist.html"),
        PlaylistOptions {
            format: MediaFormat::Audio,
            lyrics: false,
            limit: None,
        },
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(120);
    let job = loop {
        let job = api::job(id).unwrap().unwrap();
        if matches!(
            job.status,
            JobStatus::Done | JobStatus::Failed | JobStatus::Cancelled
        ) {
            break job;
        }
        assert!(
            Instant::now() < deadline,
            "the save did not finish: {job:?}"
        );
        thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(
        (job.status, job.done, job.failed),
        (JobStatus::Done, 2, 0),
        "{job:?}"
    );
    let music = api::categories()
        .unwrap()
        .into_iter()
        .find(|tab| tab.name == "Music")
        .unwrap();
    let list = api::collections(music.id)
        .unwrap()
        .into_iter()
        .find(|list| list.name == "Music Playlist")
        .expect("the playlist is in the Music tab");
    assert_eq!(list.entries, 2);
    let songs = |list: &CollectionSummary| -> Vec<_> {
        entries(list)
            .iter()
            .map(|row| {
                let item = &row.item;
                let seconds = item.duration_ms.map(|ms| (ms + 500) / 1000);
                (item.title.clone(), item.media, seconds)
            })
            .collect()
    };
    let saved = songs(&list);
    assert_eq!(
        saved
            .iter()
            .map(|(title, media, _)| (title.as_str(), *media))
            .collect::<Vec<_>>(),
        [
            ("long", api::MediaKind::Audio),
            ("song", api::MediaKind::Audio)
        ],
        "{saved:?}"
    );
    let (queue, _) = queue();
    queue.play_collection(list.id, None, true).unwrap();
    let end = wait(&queue, "the end", |state| state.finished);
    assert_eq!(end.current, Some(1));
    assert!(end.skipped.is_empty(), "{:?}", end.skipped);
    // Plain files carry no length; the library learns it from playing them.
    let learned: Vec<_> = songs(&list)
        .into_iter()
        .map(|(_, _, seconds)| seconds)
        .collect();
    assert_eq!(learned, [Some(6), Some(2)]);
}

#[test]
fn a_saved_playlist_plays_to_its_end() {
    let list = playlist();
    let items: Vec<i64> = entries(list).iter().map(|entry| entry.item.id).collect();
    let before: Vec<i64> = items.iter().map(|&item| plays(item)).collect();
    let (queue, seen) = queue();
    queue.set_speed(4.0).unwrap();
    queue.play_collection(list.id, None, true).unwrap();
    let state = queue.state();
    assert_eq!(state.entry_count, 4);
    assert_eq!(
        queue
            .entries()
            .entries
            .iter()
            .map(|entry| entry.item_id)
            .collect::<Vec<_>>(),
        items
    );
    assert_eq!(state.current, Some(0), "set as soon as play returns");
    let end = wait(&queue, "finished", |state| state.finished);
    assert_eq!(
        (end.current, end.problem.clone(), end.skipped.len()),
        (Some(3), None, 0),
        "{end:?}"
    );
    for (item, before) in items.iter().zip(before) {
        // Other tests play the same items at the same time: exactly once is proven in
        // tests/queue.rs and the library's tests, which have items of their own.
        assert!(plays(*item) > before, "item {item} counted");
    }
    let revisions: Vec<i64> = seen
        .lock()
        .unwrap()
        .iter()
        .map(|state| state.revision)
        .collect();
    assert!(
        revisions.windows(2).all(|pair| pair[0] < pair[1]),
        "{revisions:?}"
    );
}

#[test]
fn a_playlist_starts_from_any_of_its_entries() {
    let list = playlist();
    let all = entries(list);
    let (queue, _) = queue();
    queue
        .play_collection(list.id, Some(all[2].entry_id), false)
        .unwrap();
    let state = queue.state();
    assert_eq!(
        (state.current, current_item(&queue, &state)),
        (Some(2), Some(all[2].item.id))
    );
    let paused = wait(&queue, "paused at the start", |state| {
        state.playback.status == api::PlayerStatus::Paused
    });
    assert_eq!(current_item(&queue, &paused), Some(all[2].item.id));
}

#[test]
fn wrong_playlists_entries_and_keys_are_refused() {
    let list = playlist();
    let (queue, _) = queue();
    assert_eq!(
        kind_of(&queue.play_collection(i64::MAX, None, true)),
        Some(ProblemKind::Unavailable)
    );
    assert_eq!(
        kind_of(&queue.play_collection(list.id, Some(-7), true)),
        Some(ProblemKind::Input)
    );
    assert_eq!(
        kind_of(&queue.play(vec![], 0, true)),
        Some(ProblemKind::Input)
    );
    assert_eq!(
        kind_of(&queue.play(vec![entries(list)[0].item.id], 5, true)),
        Some(ProblemKind::Input)
    );
    for result in [
        queue.next(),
        queue.previous(),
        queue.resume(),
        queue.seek(1000),
        queue.jump(1),
    ] {
        assert_eq!(kind_of(&result), Some(ProblemKind::Input));
    }
    queue.play_collection(list.id, None, false).unwrap();
    for result in [
        queue.jump(-1),
        queue.remove(-1),
        queue.move_entry(-1, 0),
        queue.jump(999_999),
        queue.remove(999_999),
    ] {
        assert_eq!(kind_of(&result), Some(ProblemKind::Input));
    }
    assert_eq!(
        kind_of(&queue.set_speed(f64::NAN)),
        Some(ProblemKind::Input)
    );
    assert_eq!(queue.state().entry_count, 4, "refusals change nothing");

    // An empty playlist and a smart one, made in the database directly.
    let db = rusqlite::Connection::open(std::path::Path::new(&folders().data).join("elsewhere.db"))
        .unwrap();
    let empty: i64 = db
        .query_row(
            "INSERT INTO collection (category_id, name) VALUES (?1, 'Empty') RETURNING id",
            [list.category_id],
            |row| row.get(0),
        )
        .unwrap();
    let smart: i64 = db
        .query_row(
            "INSERT INTO collection (category_id, name, query) VALUES (?1, 'Smart', '{}') RETURNING id",
            [list.category_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        kind_of(&queue.play_collection(empty, None, true)),
        Some(ProblemKind::Input)
    );
    assert_eq!(
        kind_of(&queue.play_collection(smart, None, true)),
        Some(ProblemKind::Input)
    );
    assert_eq!(
        queue.state().entry_count,
        4,
        "the queue playing before stays"
    );
}

#[test]
fn every_command_shows_in_the_state_when_it_returns() {
    let list = playlist();
    let all = entries(list);
    let (queue, _) = queue();
    queue.play_collection(list.id, None, false).unwrap();
    let keys: Vec<i64> = queue
        .entries()
        .entries
        .iter()
        .map(|entry| entry.key)
        .collect();
    queue.next().unwrap();
    assert_eq!(queue.state().current, Some(1));
    queue.jump(keys[3]).unwrap();
    assert_eq!(queue.state().current, Some(3));
    queue.previous().unwrap();
    assert_eq!(queue.state().current, Some(2));
    queue.set_repeat(RepeatMode::One).unwrap();
    assert_eq!(queue.state().repeat, RepeatMode::One);
    queue.set_shuffle(true).unwrap();
    let shuffled = queue.state();
    assert!(shuffled.shuffle);
    assert_eq!(
        current_item(&queue, &shuffled),
        Some(all[2].item.id),
        "shuffle keeps the current one"
    );
    queue.set_shuffle(false).unwrap();
    queue.add(vec![all[0].item.id]).unwrap();
    let added = queue.entries();
    assert_eq!(queue.state().entry_count, 5);
    assert_eq!(added.revision, queue.state().entries_revision);
    let new_key = added.entries[4].key;
    assert!(!keys.contains(&new_key), "a new key for the same item");
    queue.move_entry(new_key, 0).unwrap();
    assert_eq!(queue.entries().entries[0].key, new_key);
    assert_eq!(
        current_item(&queue, &queue.state()),
        Some(all[2].item.id),
        "moving keeps what plays"
    );
    // Play next: right after what plays, in the list too.
    queue.play_next(new_key).unwrap();
    let listed = queue.entries();
    let current = queue.state().current.unwrap() as usize;
    assert_eq!(listed.entries[current + 1].key, new_key);
    assert_eq!(
        kind_of(&queue.play_next(i64::MAX)),
        Some(ProblemKind::Input),
        "an unknown entry"
    );
    assert_eq!(kind_of(&queue.play_next(-1)), Some(ProblemKind::Input));
    queue.remove(new_key).unwrap();
    assert_eq!(queue.state().entry_count, 4);
    queue.stop().unwrap();
    let stopped = queue.state();
    assert_eq!(
        (stopped.entry_count, stopped.current, stopped.current_key),
        (0, None, None)
    );
    assert!(queue.entries().entries.is_empty());
}

#[test]
fn a_listener_that_calls_the_queue_is_answered_at_once() {
    struct Calling(
        Mutex<Option<std::sync::Weak<CoreQueue>>>,
        Arc<Mutex<Vec<(bool, Duration)>>>,
    );
    impl QueueListener for Calling {
        fn on_state(&self, _: QueueState) {
            let queue = self
                .0
                .lock()
                .unwrap()
                .as_ref()
                .and_then(|weak| weak.upgrade());
            if let Some(queue) = queue {
                let started = Instant::now();
                let refused = queue.set_repeat(RepeatMode::Off).is_err();
                self.1.lock().unwrap().push((refused, started.elapsed()));
            }
        }
    }
    let list = playlist();
    let queue = CoreQueue::new().unwrap();
    let answers = Arc::new(Mutex::new(Vec::new()));
    let listener = Calling(
        Mutex::new(Some(Arc::downgrade(&queue))),
        Arc::clone(&answers),
    );
    queue.set_listener(Box::new(listener));
    queue.play_collection(list.id, None, true).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while answers.lock().unwrap().len() < 3 {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    }
    // The first call comes from set_listener on this thread and works; the ones from the
    // queue's thread are refused without waiting.
    for (refused, took) in answers.lock().unwrap().iter().skip(1) {
        assert!(*refused);
        assert!(*took < Duration::from_millis(100), "{took:?}");
    }
}

#[test]
fn closing_a_playing_queue_saves_where_it_was() {
    let list = playlist();
    let long = entries(list)[2].item.id;
    let (queue, _) = queue();
    queue.play(vec![long], 0, true).unwrap();
    wait(&queue, "past 1.5 s", |state| {
        state.playback.position_ms > 1_500
    });
    let started = Instant::now();
    drop(queue);
    assert!(started.elapsed() < Duration::from_secs(3));
    let progress = api::item(long).unwrap().summary.progress.unwrap();
    assert!(progress.percent > 0.2, "{progress:?}");
}

#[test]
fn a_file_gone_since_it_was_saved_is_skipped_with_its_reason() {
    let list = playlist();
    let all = entries(list);
    // A copy of the playlist's last item, whose file then goes away.
    let gone = all[3].item.id;
    let file = api::item_file(gone).unwrap();
    let kept = std::fs::read(&file).unwrap();
    std::fs::remove_file(&file).unwrap();
    let (queue, _) = queue();
    queue.set_speed(4.0).unwrap();
    queue.play(vec![gone, all[0].item.id], 0, true).unwrap();
    let end = wait(&queue, "finished", |state| state.finished);
    std::fs::write(&file, kept).unwrap();
    assert_eq!(end.skipped.len(), 1, "{end:?}");
    assert_eq!(
        (end.skipped[0].item_id, end.skipped[0].problem.kind),
        (gone, ProblemKind::Unavailable)
    );
    assert_eq!(
        current_item(&queue, &end),
        Some(all[0].item.id),
        "the next one played"
    );
}

#[test]
fn two_queues_play_side_by_side() {
    let list = playlist();
    let all = entries(list);
    let (first, _) = queue();
    let (second, _) = queue();
    first.set_speed(4.0).unwrap();
    second.set_speed(4.0).unwrap();
    first.play(vec![all[0].item.id], 0, true).unwrap();
    second
        .play(vec![all[1].item.id, all[3].item.id], 0, true)
        .unwrap();
    let a = wait(&first, "first finished", |state| state.finished);
    let b = wait(&second, "second finished", |state| state.finished);
    assert_eq!((a.entry_count, b.entry_count), (1, 2));
    assert!(a.problem.is_none() && b.problem.is_none());
}

#[test]
fn the_lock_screen_reads_now_playing_and_wants_play() {
    let list = playlist();
    let all = entries(list);
    let (queue, _) = queue();
    queue
        .play_collection(list.id, Some(all[1].entry_id), true)
        .unwrap();
    let state = queue.state();
    let now = state.now_playing.clone().unwrap();
    assert_eq!(
        (now.item_id, now.title.as_str()),
        (all[1].item.id, all[1].item.title.as_str())
    );
    assert_eq!(
        now.media,
        api::MediaKind::Video,
        "saved into the Videos tab"
    );
    assert!(state.wants_play);
    queue.pause().unwrap();
    assert!(!queue.state().wants_play);
    queue.set_volume(30.0).unwrap();
    assert_eq!(
        kind_of(&queue.set_volume(f64::INFINITY)),
        Some(ProblemKind::Input)
    );
    queue.stop().unwrap();
    assert_eq!(queue.state().now_playing, None);
}

#[test]
fn video_needs_the_platform_s_own_output() {
    let (first, _) = queue();
    // Surfaces are Android's (these tests run elsewhere: desktop, simulator).
    if !cfg!(target_os = "android") {
        assert_eq!(kind_of(&first.attach_surface(1)), Some(ProblemKind::Bug));
    }
    assert_eq!(
        kind_of(&first.resize_surface(0, 10)),
        Some(ProblemKind::Input)
    );
    let described = first.describe_video();
    assert!(
        described.contains("vo-configured=no"),
        "nothing shows: {described}"
    );
    // The core's start turns on the frames output on iOS only (Android draws into a
    // Surface): elsewhere attaching frames is refused. Drawing before any frame came draws
    // nothing; detaching is fine.
    struct Count;
    impl api::FrameListener for Count {
        fn on_frame(&self) {}
    }
    let attached = first.attach_frames(Box::new(Count));
    if cfg!(target_os = "ios") {
        attached.unwrap();
        first.detach_video().unwrap();
    } else {
        assert_eq!(kind_of(&attached), Some(ProblemKind::Bug));
    }
    let mut pixels = vec![0u8; 16 * 16 * 4];
    assert!(
        !first
            .draw_frame(16, 16, 64, pixels.as_mut_ptr() as u64)
            .unwrap()
    );
    assert_eq!(
        kind_of(&first.draw_frame(0, 16, 64, pixels.as_mut_ptr() as u64)),
        Some(ProblemKind::Input)
    );
    assert_eq!(
        kind_of(&first.draw_frame(16, 16, 64, 0)),
        Some(ProblemKind::Input)
    );
    first.detach_video().unwrap();
}

#[test]
fn own_playlists_browsing_and_search_through_the_api() {
    let list = playlist();
    let all = entries(list);
    let videos = list.category_id;
    // A playlist of one's own from the saved one's items, edited.
    let mine = api::create_playlist(videos, " Mine ".to_owned()).unwrap();
    let ids: Vec<i64> = all.iter().map(|entry| entry.item.id).collect();
    let added = api::add_to_playlist(mine, ids.clone(), None).unwrap();
    assert_eq!(added.len(), ids.len());
    api::move_in_playlist(mine, added[3], Some(added[0])).unwrap();
    api::remove_from_playlist(mine, vec![added[1]]).unwrap();
    let rows: Vec<i64> = api::collection_items(mine, None, 0)
        .unwrap()
        .entries
        .iter()
        .map(|entry| entry.item.id)
        .collect();
    assert_eq!(rows, [ids[3], ids[0], ids[2]]);
    assert_eq!(api::collection(mine).unwrap().name, "Mine");
    // The saved playlist itself follows its link.
    assert_eq!(
        kind_of(&api::add_to_playlist(list.id, vec![ids[0]], None)),
        Some(ProblemKind::Input)
    );
    let copy = api::copy_playlist(list.id, "Copy".to_owned()).unwrap();
    api::add_to_playlist(copy, vec![ids[0]], None).unwrap();
    api::rename_playlist(copy, "Copy 2".to_owned()).unwrap();
    // Playing one's own playlist, then deleting it (the items stay).
    let (queue, _) = queue();
    queue.play_collection(mine, None, false).unwrap();
    assert_eq!(queue.state().entry_count, 3);
    api::delete_playlist(mine).unwrap();
    api::delete_playlist(copy).unwrap();
    assert!(api::item(ids[0]).is_ok());
    assert_eq!(
        kind_of(&api::collection(mine)),
        Some(ProblemKind::Unavailable)
    );
    // Browsing and search.
    let page = api::tab_items(videos, api::ItemOrder::Title, None, 0).unwrap();
    assert!(
        ids.iter()
            .all(|id| page.items.iter().any(|item| item.id == *id))
    );
    let found = api::search(Some(videos), "SONG".to_owned()).unwrap();
    assert!(
        found.items.iter().any(|item| item.id == ids[0]),
        "{found:?}"
    );
    assert!(
        api::search(None, "Real Playlist".to_owned())
            .unwrap()
            .playlists
            .iter()
            .any(|found| found.id == list.id)
    );
    assert!(api::artists(videos).is_ok() && api::albums(videos).is_ok());
    assert_eq!(
        kind_of(&api::tab_items(
            videos,
            api::ItemOrder::Title,
            Some("nonsense".to_owned()),
            0
        )),
        Some(ProblemKind::Input)
    );
}
