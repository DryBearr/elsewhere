//! Tests for src/queue: the playlist player with the real engine (null outputs) and a real
//! library on a fresh database. The media are the small files in tests/media (2 s, and
//! long.m4a with 6 s). Needs libmpv like tests/player.rs (ELSEWHERE_LIBMPV on a desktop
//! without one; PLAYER_TEST_MEDIA on an emulator or simulator).

use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, Once};
use std::thread;
use std::time::{Duration, Instant};

use elsewhere::library::Library;
use elsewhere::player::{self, Status};
use elsewhere::problem::Kind;
use elsewhere::queue::{Command, QueuePlayer, QueueState, Repeat};
use rusqlite::{Connection, params};

fn test_media() -> PathBuf {
    std::env::var_os("PLAYER_TEST_MEDIA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/media"))
}

/// One media folder for the whole program (players may only open files inside it).
fn media_root() -> PathBuf {
    let root = std::env::temp_dir()
        .join(format!("elsewhere-queue-{}", std::process::id()))
        .join("media");
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = fs::remove_dir_all(root.parent().unwrap());
        fs::create_dir_all(&root).unwrap();
        player::set_null_output(true);
        player::allow_folders(std::slice::from_ref(&root));
    });
    root
}

/// A library of its own for one test, with a Music folder under the shared media folder.
struct Shelf {
    library: &'static Library,
    db: Connection,
    folder: PathBuf,
    tab: i64,
    library_folder: i64,
}

fn shelf(name: &str) -> Shelf {
    let media = media_root();
    let folder = media.join(name).join("Music");
    fs::create_dir_all(&folder).unwrap();
    let path = media.parent().unwrap().join(format!("{name}.db"));
    let db = Connection::open(&path).unwrap();
    // The library makes the tables on first open.
    let library: &'static Library = Box::leak(Box::new(Library::open(&path, &media).unwrap()));
    let tab: i64 = db
        .query_row("SELECT id FROM category WHERE name = 'Music'", [], |row| {
            row.get(0)
        })
        .unwrap();
    let library_folder: i64 = db
        .query_row(
            "INSERT INTO library (category_id, root) VALUES (?1, ?2) RETURNING id",
            params![tab, folder.to_str().unwrap()],
            |row| row.get(0),
        )
        .unwrap();
    Shelf {
        library,
        db,
        folder,
        tab,
        library_folder,
    }
}

impl Shelf {
    /// An item playing a copy of `source` from tests/media (None: its file is missing).
    fn item(&self, title: &str, source: Option<&str>) -> i64 {
        let file = format!(
            "{title}.{}",
            source
                .and_then(|name| name.rsplit('.').next())
                .unwrap_or("m4a")
        );
        if let Some(source) = source {
            fs::copy(test_media().join(source), self.folder.join(&file)).unwrap();
        }
        self.db
            .query_row(
                "INSERT INTO item (category_id, library_id, kind, path, title, sort_title)
                 VALUES (?1, ?2, 'entry', ?3, ?4, lower(?4)) RETURNING id",
                params![self.tab, self.library_folder, file, title],
                |row| row.get(0),
            )
            .unwrap()
    }

    fn broken(&self, title: &str) -> i64 {
        let id = self.item(title, None);
        fs::write(self.folder.join(format!("{title}.m4a")), vec![7u8; 4000]).unwrap();
        id
    }

    fn plays(&self, id: i64) -> i64 {
        self.library
            .item(id)
            .unwrap()
            .summary
            .progress
            .map_or(0, |progress| progress.plays)
    }
}

type Seen = Arc<Mutex<Vec<QueueState>>>;

fn queue(shelf: &Shelf) -> (QueuePlayer, Seen) {
    let queue = QueuePlayer::new(shelf.library).expect("libmpv loads");
    let seen = Seen::default();
    let sink = Arc::clone(&seen);
    queue.set_listener(move |state| sink.lock().unwrap().push(state));
    (queue, seen)
}

fn wait(queue: &QueuePlayer, what: &str, check: impl Fn(&QueueState) -> bool) -> QueueState {
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

fn current_item(state: &QueueState) -> Option<i64> {
    state.current.map(|index| state.entries[index].item_id)
}

fn play(queue: &QueuePlayer, items: &[i64], start: usize) {
    queue
        .command(Command::Play {
            items: items.to_vec(),
            start,
            autoplay: true,
        })
        .unwrap();
}

#[test]
fn a_queue_plays_in_order_to_its_end() {
    let shelf = shelf("in-order");
    let items = [
        shelf.item("One", Some("song.m4a")),
        shelf.item("Two", Some("song.opus")),
        shelf.item("Three", Some("clip.webm")),
    ];
    let (queue, seen) = queue(&shelf);
    queue.command(Command::SetSpeed(4.0)).unwrap();
    play(&queue, &items, 0);
    let end = wait(&queue, "finished", |state| state.finished);
    assert_eq!(
        (end.current, end.problem.clone()),
        (Some(2), None),
        "{end:?}"
    );
    let order: Vec<i64> = {
        let mut order: Vec<i64> = seen
            .lock()
            .unwrap()
            .iter()
            .filter_map(current_item)
            .collect();
        order.dedup();
        order
    };
    assert_eq!(order, items, "each in turn");
    for id in items {
        assert_eq!(shelf.plays(id), 1, "item {id} counted once");
    }
    let revisions: Vec<u64> = seen
        .lock()
        .unwrap()
        .iter()
        .map(|state| state.revision)
        .collect();
    assert!(revisions.windows(2).all(|pair| pair[0] < pair[1]));
}

#[test]
fn what_cannot_be_played_is_skipped_with_its_reason() {
    let shelf = shelf("skips");
    let first = shelf.item("First", Some("song.m4a"));
    let missing = shelf.item("Missing", None);
    let broken = shelf.broken("Broken");
    let last = shelf.item("Last", Some("song.opus"));
    let (queue, seen) = queue(&shelf);
    queue.command(Command::SetSpeed(4.0)).unwrap();
    play(&queue, &[first, missing, broken, last], 0);
    let end = wait(&queue, "finished", |state| state.finished);
    assert_eq!(current_item(&end), Some(last));
    let played: Vec<i64> = seen
        .lock()
        .unwrap()
        .iter()
        .filter(|state| state.playback.status == Status::Playing)
        .filter_map(current_item)
        .collect();
    assert!(
        played.contains(&first) && played.contains(&last),
        "{played:?}"
    );
    assert!(
        !played.contains(&missing) && !played.contains(&broken),
        "{played:?}"
    );
    let reasons: Vec<Kind> = seen
        .lock()
        .unwrap()
        .iter()
        .filter_map(|state| state.problem.as_ref().map(|p| p.kind))
        .collect();
    assert!(
        reasons.contains(&Kind::Unavailable),
        "the skips were explained: {reasons:?}"
    );
    assert_eq!((shelf.plays(first), shelf.plays(last)), (1, 1));
}

#[test]
fn a_queue_where_nothing_plays_stops_with_a_problem() {
    let shelf = shelf("nothing");
    let items = [
        shelf.item("Gone", None),
        shelf.broken("Bad"),
        shelf.item("Also gone", None),
    ];
    let (queue, _) = queue(&shelf);
    play(&queue, &items, 0);
    let end = wait(&queue, "stopped", |state| state.finished);
    let problem = end.problem.clone().unwrap();
    assert_eq!(problem.kind, Kind::Unavailable);
    assert!(
        problem.text.starts_with("Nothing in this queue"),
        "{problem:?}"
    );
    assert_ne!(end.playback.status, Status::Playing);
    // Thousands of missing files: no pile-up, just the same answer.
    let many: Vec<i64> = (0..3000)
        .map(|n| shelf.item(&format!("Gone {n}"), None))
        .collect();
    let started = Instant::now();
    play(&queue, &many, 0);
    let end = wait(&queue, "stopped again", |state| {
        state.finished && state.entries.len() == 3000
    });
    assert!(
        end.problem
            .unwrap()
            .text
            .starts_with("Nothing in this queue")
    );
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "{:?}",
        started.elapsed()
    );
}

#[test]
fn next_previous_and_starting_over() {
    let shelf = shelf("next-previous");
    let items = [
        shelf.item("A", Some("long.m4a")),
        shelf.item("B", Some("long.m4a")),
        shelf.item("C", Some("long.m4a")),
    ];
    let (queue, _) = queue(&shelf);
    play(&queue, &items, 0);
    wait(&queue, "A playing", |state| {
        state.playback.status == Status::Playing && current_item(state) == Some(items[0])
    });
    queue.command(Command::Next).unwrap();
    wait(&queue, "B playing", |state| {
        state.playback.status == Status::Playing && current_item(state) == Some(items[1])
    });
    // Early in B: back to A.
    queue.command(Command::Previous).unwrap();
    wait(&queue, "A again", |state| {
        state.playback.status == Status::Playing && current_item(state) == Some(items[0])
    });
    // Past 3 s: previous starts A over.
    wait(&queue, "A past 3 s", |state| {
        state.playback.position_ms > 3_200
    });
    queue.command(Command::Previous).unwrap();
    let over = wait(&queue, "A from the top", |state| {
        state.playback.position_ms < 2_000
    });
    assert_eq!(current_item(&over), Some(items[0]));
    // At the first entry, previous stays there.
    queue.command(Command::Previous).unwrap();
    assert_eq!(current_item(&queue.state()), Some(items[0]));
    // Jump to C, and next after the last ends the queue.
    let key = queue.state().entries[2].key;
    queue.command(Command::Jump(key)).unwrap();
    wait(&queue, "C playing", |state| {
        current_item(state) == Some(items[2]) && state.playback.status == Status::Playing
    });
    queue.command(Command::Next).unwrap();
    assert!(wait(&queue, "over", |state| state.finished).finished);
    // Resume after the end starts from the top.
    queue.command(Command::Resume).unwrap();
    wait(&queue, "A from the start", |state| {
        !state.finished && current_item(state) == Some(items[0])
    });
}

#[test]
fn repeat_one_and_repeat_all() {
    let shelf = shelf("repeat");
    let song = shelf.item("Song", Some("song.m4a"));
    let other = shelf.item("Other", Some("song.opus"));
    let (queue, _) = queue(&shelf);
    queue.command(Command::SetSpeed(4.0)).unwrap();
    queue.command(Command::SetRepeat(Repeat::One)).unwrap();
    play(&queue, &[song, other], 0);
    let deadline = Instant::now() + Duration::from_secs(30);
    while shelf.plays(song) < 3 {
        assert!(
            Instant::now() < deadline,
            "song played {} times",
            shelf.plays(song)
        );
        assert_eq!(
            current_item(&queue.state()),
            Some(song),
            "repeat one stays on it"
        );
        thread::sleep(Duration::from_millis(20));
    }
    queue.command(Command::SetRepeat(Repeat::All)).unwrap();
    // Round and round: both get played again and again, and it never ends.
    let deadline = Instant::now() + Duration::from_secs(30);
    while shelf.plays(other) < 2 {
        assert!(Instant::now() < deadline);
        assert!(!queue.state().finished);
        thread::sleep(Duration::from_millis(20));
    }
    queue.command(Command::SetRepeat(Repeat::Off)).unwrap();
    wait(&queue, "finished once repeat is off", |state| {
        state.finished
    });
}

#[test]
fn shuffle_plays_every_entry_once() {
    let shelf = shelf("shuffle");
    let items: Vec<i64> = (0..6)
        .map(|n| shelf.item(&format!("Song {n}"), Some("song.m4a")))
        .collect();
    let (queue, seen) = queue(&shelf);
    queue.command(Command::SetSpeed(4.0)).unwrap();
    queue.command(Command::SetShuffle(true)).unwrap();
    play(&queue, &items, 2);
    let end = wait(&queue, "finished", |state| state.finished);
    assert!(end.shuffle);
    let mut order: Vec<i64> = seen
        .lock()
        .unwrap()
        .iter()
        .filter_map(current_item)
        .collect();
    order.dedup();
    assert_eq!(order[0], items[2], "the chosen one first");
    let mut sorted = order.clone();
    sorted.sort_unstable();
    assert_eq!(sorted, items, "every one once: {order:?}");
    for id in items {
        assert_eq!(shelf.plays(id), 1);
    }
}

#[test]
fn editing_the_queue_while_it_plays() {
    let shelf = shelf("editing");
    let a = shelf.item("A", Some("long.m4a"));
    let b = shelf.item("B", Some("long.m4a"));
    let c = shelf.item("C", Some("song.m4a"));
    let (queue, _) = queue(&shelf);
    play(&queue, &[a, b], 0);
    wait(&queue, "A playing", |state| {
        state.playback.status == Status::Playing
    });
    // Removing what plays: the next one plays.
    let key = queue.state().entries[0].key;
    queue.command(Command::Remove(key)).unwrap();
    let now = wait(&queue, "B playing", |state| {
        current_item(state) == Some(b) && state.playback.status == Status::Playing
    });
    assert_eq!(now.entries.len(), 1);
    // Added entries come after.
    queue.command(Command::Add(vec![c, a])).unwrap();
    let state = queue.state();
    assert_eq!(
        state
            .entries
            .iter()
            .map(|slot| slot.item_id)
            .collect::<Vec<_>>(),
        [b, c, a]
    );
    let last = state.entries[2].key;
    queue.command(Command::Move { key: last, to: 0 }).unwrap();
    let moved = queue.state();
    assert_eq!(
        moved
            .entries
            .iter()
            .map(|slot| slot.item_id)
            .collect::<Vec<_>>(),
        [a, b, c]
    );
    assert_eq!(
        current_item(&moved),
        Some(b),
        "moving does not change what plays"
    );
    // Removing the only and current entry of a one-entry queue ends it.
    queue.command(Command::Stop).unwrap();
    play(&queue, &[c], 0);
    let key = queue.state().entries[0].key;
    queue.command(Command::Remove(key)).unwrap();
    let empty = wait(&queue, "empty and over", |state| {
        state.entries.is_empty() && state.finished
    });
    assert_ne!(empty.playback.status, Status::Playing);
    // Adding to a queue that ended plays the new entry.
    queue.command(Command::Add(vec![c])).unwrap();
    wait(&queue, "C playing", |state| {
        current_item(state) == Some(c) && state.playback.status == Status::Playing
    });
}

#[test]
fn pausing_saves_where_playback_is() {
    let shelf = shelf("pause");
    let song = shelf.item("Song", Some("long.m4a"));
    let (queue, _) = queue(&shelf);
    play(&queue, &[song], 0);
    wait(&queue, "past 1.5 s", |state| {
        state.playback.position_ms > 1_500
    });
    queue.command(Command::Pause).unwrap();
    let paused = wait(&queue, "paused", |state| {
        state.playback.status == Status::Paused
    });
    let progress = shelf.library.item(song).unwrap().summary.progress.unwrap();
    assert!(progress.percent > 0.2 && !progress.done, "{progress:?}");
    // And next/previous keep it paused.
    queue.command(Command::Previous).unwrap();
    thread::sleep(Duration::from_millis(300));
    assert_ne!(queue.state().playback.status, Status::Playing, "{paused:?}");
    queue.command(Command::Resume).unwrap();
    wait(&queue, "playing again", |state| {
        state.playback.status == Status::Playing
    });
}

#[test]
fn wrong_commands_are_refused_and_change_nothing() {
    let shelf = shelf("refusals");
    let song = shelf.item("Song", Some("song.m4a"));
    let (queue, _) = queue(&shelf);
    for command in [
        Command::Next,
        Command::Previous,
        Command::Resume,
        Command::Seek(10),
        Command::Jump(1),
    ] {
        let problem = queue.command(command.clone()).unwrap_err();
        assert_eq!(problem.kind, Kind::Input, "{command:?}");
    }
    let empty = queue
        .command(Command::Play {
            items: vec![],
            start: 0,
            autoplay: true,
        })
        .unwrap_err();
    assert_eq!(empty.kind, Kind::Input);
    let past = queue
        .command(Command::Play {
            items: vec![song],
            start: 1,
            autoplay: true,
        })
        .unwrap_err();
    assert_eq!(past.kind, Kind::Input);
    assert!(queue.state().entries.is_empty());
    play(&queue, &[song], 0);
    assert_eq!(
        queue.command(Command::Remove(9999)).unwrap_err().kind,
        Kind::Input
    );
    assert_eq!(
        queue
            .command(Command::Move { key: 9999, to: 0 })
            .unwrap_err()
            .kind,
        Kind::Input
    );
    assert_eq!(
        queue.command(Command::SetSpeed(f64::NAN)).unwrap_err().kind,
        Kind::Input
    );
    assert_eq!(queue.state().entries.len(), 1);
}

#[test]
fn commands_from_many_threads_leave_a_sane_queue() {
    let shelf = shelf("storm");
    let items: Vec<i64> = (0..5)
        .map(|n| shelf.item(&format!("Song {n}"), Some("long.m4a")))
        .collect();
    let (queue, seen) = queue(&shelf);
    let queue = Arc::new(queue);
    play(&queue, &items, 0);
    let workers: Vec<_> = (0..4u64)
        .map(|worker| {
            let queue = Arc::clone(&queue);
            thread::spawn(move || {
                for step in 0..60u64 {
                    // Refusals are fine; panics, hangs and broken states are not.
                    let _ = queue.command(match (worker + step) % 7 {
                        0 => Command::Next,
                        1 => Command::Previous,
                        2 => Command::Pause,
                        3 => Command::Resume,
                        4 => Command::Seek((step * 300) as i64),
                        5 => Command::SetShuffle(step % 2 == 0),
                        _ => Command::SetRepeat(if step % 3 == 0 {
                            Repeat::All
                        } else {
                            Repeat::Off
                        }),
                    });
                    let state = queue.state();
                    assert!(
                        state
                            .current
                            .is_none_or(|index| index < state.entries.len()),
                        "{state:?}"
                    );
                }
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }
    queue.command(Command::Stop).unwrap();
    let stopped = wait(&queue, "stopped", |state| state.entries.is_empty());
    assert_eq!(stopped.current, None);
    let revisions: Vec<u64> = seen
        .lock()
        .unwrap()
        .iter()
        .map(|state| state.revision)
        .collect();
    assert!(revisions.windows(2).all(|pair| pair[0] < pair[1]));
}

#[test]
fn dropping_a_playing_queue_is_quick() {
    let shelf = shelf("drop");
    let song = shelf.item("Song", Some("long.m4a"));
    for _ in 0..5 {
        let (queue, _) = queue(&shelf);
        play(&queue, &[song], 0);
        thread::sleep(Duration::from_millis(50));
        let started = Instant::now();
        drop(queue);
        assert!(started.elapsed() < Duration::from_millis(500));
    }
}

#[test]
fn after_a_command_returns_its_result_is_in_the_state() {
    let shelf = shelf("read-your-writes");
    let items: Vec<i64> = (0..3)
        .map(|n| shelf.item(&format!("Song {n}"), Some("long.m4a")))
        .collect();
    let (queue, _) = queue(&shelf);
    for round in 0..50 {
        play(&queue, &items, round % 3);
        let state = queue.state();
        assert_eq!(
            (state.entries.len(), state.current),
            (3, Some(round % 3)),
            "round {round}"
        );
        queue.command(Command::Add(vec![items[0]])).unwrap();
        assert_eq!(queue.state().entries.len(), 4, "round {round}");
        queue.command(Command::SetRepeat(Repeat::All)).unwrap();
        assert_eq!(queue.state().repeat, Repeat::All);
        queue.command(Command::SetShuffle(round % 2 == 0)).unwrap();
        assert_eq!(queue.state().shuffle, round % 2 == 0);
        queue.command(Command::Stop).unwrap();
        assert!(queue.state().entries.is_empty(), "round {round}");
        queue.command(Command::SetRepeat(Repeat::Off)).unwrap();
    }
}

// --- Edge cases ------------------------------------------------------------------------

#[test]
fn leaving_an_entry_and_dropping_the_queue_save_where_it_was() {
    let shelf = shelf("leaving");
    let first = shelf.item("First", Some("long.m4a"));
    let second = shelf.item("Second", Some("long.m4a"));
    let third = shelf.item("Third", Some("long.m4a"));
    let (queue, _) = queue(&shelf);
    play(&queue, &[first], 0);
    wait(&queue, "past 1.5 s", |state| {
        state.playback.position_ms > 1_500
    });
    // A new queue replaces it: the first one's progress is kept.
    play(&queue, &[second, third], 0);
    let progress = shelf.library.item(first).unwrap().summary.progress.unwrap();
    assert!(progress.percent > 0.2, "{progress:?}");
    wait(&queue, "second past 1.5 s", |state| {
        current_item(state) == Some(second) && state.playback.position_ms > 1_500
    });
    drop(queue);
    let progress = shelf
        .library
        .item(second)
        .unwrap()
        .summary
        .progress
        .unwrap();
    assert!(progress.percent > 0.2, "dropping saves too: {progress:?}");
}

#[test]
fn progress_is_saved_while_playing_without_pauses() {
    let shelf = shelf("periodic");
    let song = shelf.item("Song", Some("long.m4a"));
    elsewhere::queue::set_save_interval(Duration::from_millis(200));
    let (queue, _) = queue(&shelf);
    play(&queue, &[song], 0);
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let saved = shelf
            .library
            .item(song)
            .unwrap()
            .summary
            .progress
            .map_or(0.0, |p| p.percent);
        if saved > 0.2 {
            break;
        }
        assert!(Instant::now() < deadline, "never saved while playing");
        thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(
        queue.state().playback.status,
        Status::Playing,
        "saved without pausing"
    );
}

#[test]
fn skipping_past_the_last_entry_stops_its_sound() {
    let shelf = shelf("past-the-end");
    let song = shelf.item("Song", Some("long.m4a"));
    let (queue, _) = queue(&shelf);
    play(&queue, &[song], 0);
    wait(&queue, "playing", |state| {
        state.playback.status == Status::Playing
    });
    queue.command(Command::Next).unwrap();
    let state = queue.state();
    assert!(state.finished);
    thread::sleep(Duration::from_millis(300));
    assert_ne!(
        queue.state().playback.status,
        Status::Playing,
        "the file does not play on"
    );
}

#[test]
fn groups_books_and_deleted_items_are_skipped() {
    let shelf = shelf("odd-items");
    let song = shelf.item("Song", Some("song.m4a"));
    let album: i64 = shelf
        .db
        .query_row(
            "INSERT INTO item (category_id, library_id, kind, path, title, sort_title)
             VALUES (?1, ?2, 'group', 'Album', 'Album', 'album') RETURNING id",
            params![shelf.tab, shelf.library_folder],
            |row| row.get(0),
        )
        .unwrap();
    let books: i64 = shelf
        .db
        .query_row("SELECT id FROM category WHERE name = 'Books'", [], |row| {
            row.get(0)
        })
        .unwrap();
    let book = shelf.item("Book", Some("song.m4a"));
    shelf
        .db
        .execute(
            "UPDATE item SET category_id = ?2 WHERE id = ?1",
            params![book, books],
        )
        .unwrap();
    let deleted = shelf.item("Deleted", Some("song.m4a"));
    shelf
        .db
        .execute("DELETE FROM item WHERE id = ?1", [deleted])
        .unwrap();
    let (queue, seen) = queue(&shelf);
    queue.command(Command::SetSpeed(4.0)).unwrap();
    play(&queue, &[album, book, deleted, song], 0);
    let end = wait(&queue, "finished", |state| state.finished);
    assert_eq!(current_item(&end), Some(song), "only the song played");
    let skipped: Vec<(i64, Kind)> = end
        .skipped
        .iter()
        .map(|skip| (skip.item_id, skip.problem.kind))
        .collect();
    assert_eq!(
        skipped,
        [
            (album, Kind::Input),
            (book, Kind::Input),
            (deleted, Kind::Unavailable)
        ],
        "each skip with its own reason"
    );
    let _ = &seen;
    assert_eq!(shelf.plays(song), 1);
}

#[test]
fn an_item_deleted_while_it_plays_does_not_stop_the_queue() {
    let shelf = shelf("deleted-playing");
    let first = shelf.item("First", Some("song.m4a"));
    let second = shelf.item("Second", Some("song.opus"));
    let (queue, _) = queue(&shelf);
    play(&queue, &[first, second], 0);
    wait(&queue, "first playing", |state| {
        state.playback.status == Status::Playing
    });
    shelf
        .db
        .execute("DELETE FROM item WHERE id = ?1", [first])
        .unwrap();
    queue.command(Command::SetSpeed(4.0)).unwrap();
    let end = wait(&queue, "finished", |state| state.finished);
    assert_eq!(current_item(&end), Some(second));
    assert_eq!(shelf.plays(second), 1);
}

#[test]
fn repeat_all_over_only_broken_entries_stops_instead_of_spinning() {
    let shelf = shelf("repeat-broken");
    let items = [
        shelf.broken("Bad"),
        shelf.item("Gone", None),
        shelf.broken("Worse"),
    ];
    let (queue, _) = queue(&shelf);
    queue.command(Command::SetRepeat(Repeat::All)).unwrap();
    play(&queue, &items, 0);
    let end = wait(&queue, "stopped", |state| state.finished);
    assert!(
        end.problem
            .unwrap()
            .text
            .starts_with("Nothing in this queue")
    );
    thread::sleep(Duration::from_millis(300));
    assert!(queue.state().finished, "and it stays stopped");
    // Repeat one over a single broken entry: the same.
    queue.command(Command::SetRepeat(Repeat::One)).unwrap();
    play(&queue, &[items[0]], 0);
    assert!(
        wait(&queue, "stopped again", |state| state.finished)
            .problem
            .is_some()
    );
}

#[test]
fn a_paused_queue_skips_broken_entries_without_giving_up_too_early() {
    let shelf = shelf("paused-broken");
    let bad = shelf.broken("Bad");
    let good = shelf.item("Good", Some("long.m4a"));
    let (queue, _) = queue(&shelf);
    queue.command(Command::SetRepeat(Repeat::All)).unwrap();
    queue
        .command(Command::Play {
            items: vec![bad, good],
            start: 0,
            autoplay: false,
        })
        .unwrap();
    wait(&queue, "paused on the good one", |state| {
        current_item(state) == Some(good) && state.playback.status == Status::Paused
    });
    // Next goes round to the broken one, skips it, and lands on the good one again: the
    // good one opening (paused) counts, so this is not "nothing can be played".
    queue.command(Command::Next).unwrap();
    let state = wait(&queue, "paused on the good one again", |state| {
        current_item(state) == Some(good) && state.playback.status == Status::Paused
    });
    assert!(!state.finished, "{state:?}");
}

#[test]
fn removing_another_entry_does_not_restart_what_plays() {
    let shelf = shelf("remove-other");
    let a = shelf.item("A", Some("long.m4a"));
    let b = shelf.item("B", Some("long.m4a"));
    let c = shelf.item("C", Some("long.m4a"));
    let (queue, _) = queue(&shelf);
    play(&queue, &[a, b, c], 1);
    let before = wait(&queue, "B past 1 s", |state| {
        state.playback.position_ms > 1_000
    });
    let first = before.entries[0].key;
    let last = before.entries[2].key;
    queue.command(Command::Remove(first)).unwrap();
    queue.command(Command::Remove(last)).unwrap();
    let after = queue.state();
    assert_eq!(current_item(&after), Some(b));
    assert!(
        after.playback.position_ms >= before.playback.position_ms,
        "not started over"
    );
    assert_eq!(after.entries.len(), 1);
}

#[test]
fn a_listener_that_sends_commands_gets_an_answer_at_once() {
    let shelf = shelf("listener-commands");
    let song = shelf.item("Song", Some("song.m4a"));
    let queue = Arc::new(QueuePlayer::new(shelf.library).unwrap());
    let answers = Arc::new(Mutex::new(Vec::new()));
    let (inner, sink) = (Arc::downgrade(&queue), Arc::clone(&answers));
    queue.set_listener(move |_| {
        if let Some(queue) = inner.upgrade() {
            let started = Instant::now();
            let result = queue.command(Command::SetRepeat(Repeat::Off));
            sink.lock()
                .unwrap()
                .push((result.is_err(), started.elapsed()));
        }
    });
    play(&queue, &[song], 0);
    let deadline = Instant::now() + Duration::from_secs(10);
    while answers.lock().unwrap().len() < 2 {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    }
    // From inside the listener: refused at once, never a 30 s wait for itself.
    for (refused, took) in answers.lock().unwrap().iter().skip(1) {
        assert!(*refused);
        assert!(*took < Duration::from_millis(100), "{took:?}");
    }
}

#[test]
fn a_panicking_listener_does_not_stop_the_queue() {
    let shelf = shelf("bad-listener");
    let items = [
        shelf.item("One", Some("song.m4a")),
        shelf.item("Two", Some("song.opus")),
    ];
    let queue = QueuePlayer::new(shelf.library).unwrap();
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = Arc::clone(&calls);
    queue.set_listener(move |_| {
        if counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst) % 2 == 1 {
            panic!("listener bug");
        }
    });
    queue.command(Command::SetSpeed(4.0)).unwrap();
    play(&queue, &items, 0);
    wait(&queue, "finished", |state| state.finished);
    assert_eq!((shelf.plays(items[0]), shelf.plays(items[1])), (1, 1));
}

#[test]
fn seeking_while_a_file_opens_answers_and_changes_nothing() {
    let shelf = shelf("seek-loading");
    let song = shelf.item("Song", Some("long.m4a"));
    let (queue, _) = queue(&shelf);
    play(&queue, &[song], 0);
    // Right after Play the file may still be opening: either a seek works or it is
    // refused, never a hang or a broken queue.
    let _ = queue.command(Command::Seek(2_000));
    wait(&queue, "playing", |state| {
        state.playback.status == Status::Playing
    });
    queue.command(Command::Seek(4_000)).unwrap();
    wait(&queue, "past 4 s", |state| {
        state.playback.position_ms >= 3_900
    });
}
