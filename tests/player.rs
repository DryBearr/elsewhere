//! Tests for src/player with the real libmpv, without a window or sound (null outputs: video
//! is still decoded). The media in tests/media are 2 s long and tiny. On a desktop without
//! a system libmpv, point ELSEWHERE_LIBMPV at one (for example Homebrew's libmpv.so.2).
//! On an emulator or simulator, PLAYER_TEST_MEDIA names the copy of tests/media.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Once};
use std::thread;
use std::time::{Duration, Instant};

use elsewhere::player::{self, Player, PlayerState, Status};
use elsewhere::problem::Kind;

fn media_dir() -> PathBuf {
    std::env::var_os("PLAYER_TEST_MEDIA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/media"))
}

/// A folder for files made by the tests (broken files, links).
fn scratch() -> PathBuf {
    std::env::temp_dir().join(format!("elsewhere-player-{}", std::process::id()))
}

/// A real media file outside the allowed folders.
fn outside() -> PathBuf {
    std::env::temp_dir()
        .join(format!("elsewhere-outside-{}", std::process::id()))
        .join("secret.mp4")
}

fn setup() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = fs::remove_dir_all(scratch());
        fs::create_dir_all(scratch()).unwrap();
        let secret = outside();
        fs::create_dir_all(secret.parent().unwrap()).unwrap();
        fs::copy(media_dir().join("clip.mp4"), &secret).unwrap();
        player::set_null_output(true);
        player::allow_folders(&[media_dir(), scratch()]);
    });
}

fn media(name: &str) -> String {
    media_dir().join(name).to_str().unwrap().to_owned()
}

type Seen = Arc<Mutex<Vec<PlayerState>>>;

fn new_player() -> (Player, Seen) {
    setup();
    let player = Player::new().expect("libmpv loads (set ELSEWHERE_LIBMPV if it is not installed)");
    let seen = Seen::default();
    let sink = Arc::clone(&seen);
    player.set_listener(move |state| sink.lock().unwrap().push(state));
    (player, seen)
}

/// Waits until the state matches and the listener has seen that revision.
fn wait_for(
    player: &Player,
    seen: &Seen,
    what: &str,
    check: impl Fn(&PlayerState) -> bool,
) -> PlayerState {
    let deadline = Instant::now() + Duration::from_secs(25);
    loop {
        let state = player.state();
        let delivered = seen
            .lock()
            .unwrap()
            .iter()
            .any(|seen| seen.revision == state.revision);
        if check(&state) && delivered {
            return state;
        }
        assert!(Instant::now() < deadline, "never {what}: {state:?}");
        thread::sleep(Duration::from_millis(5));
    }
}

fn status_is(status: Status) -> impl Fn(&PlayerState) -> bool {
    move |state| state.status == status
}

fn final_state(player: &Player, seen: &Seen) -> PlayerState {
    wait_for(player, seen, "a final state", |state| {
        matches!(state.status, Status::Ended | Status::Failed)
    })
}

#[test]
fn every_kind_of_file_plays_to_the_end() {
    let (player, seen) = new_player();
    player.set_speed(4.0).unwrap();
    for (name, video, audio) in [
        ("clip.mp4", true, true),
        ("clip.webm", true, true),
        ("clip.mkv", true, true),
        ("song.m4a", false, true),
        ("song.opus", false, true),
        ("video-only.mp4", true, false),
    ] {
        seen.lock().unwrap().clear();
        player.open(&media(name), 0, true).unwrap();
        let end = final_state(&player, &seen);
        assert_eq!(end.status, Status::Ended, "{name}: {end:?}");
        assert_eq!(end.file.as_deref(), Some(media_dir().join(name).as_path()));
        assert_eq!((end.has_video, end.has_audio), (video, audio), "{name}");
        let duration = end
            .duration_ms
            .unwrap_or_else(|| panic!("{name} has a duration"));
        assert!((1900..=2100).contains(&duration), "{name}: {duration} ms");
        assert!(
            end.position_ms + 150 >= duration,
            "{name}: ended at {}",
            end.position_ms
        );

        let states = seen.lock().unwrap().clone();
        assert!(
            states
                .windows(2)
                .all(|pair| pair[0].revision < pair[1].revision),
            "{name}: revisions only go up"
        );
        let playing = states
            .iter()
            .position(|state| state.status == Status::Playing);
        let ended = states
            .iter()
            .position(|state| state.status == Status::Ended);
        assert!(
            playing.is_some() && playing < ended,
            "{name}: Playing before Ended: {states:?}"
        );
        assert!(states.iter().all(|state| state.problem.is_none()), "{name}");
    }
}

#[test]
fn a_paused_open_seeks_plays_and_starts_over_at_the_end() {
    let (player, seen) = new_player();
    player.open(&media("clip.mp4"), 500, false).unwrap();
    let paused = wait_for(&player, &seen, "paused", status_is(Status::Paused));
    assert!((400..=600).contains(&paused.position_ms), "{paused:?}");

    player.seek(1500).unwrap();
    wait_for(&player, &seen, "at 1.5 s", |state| {
        (1400..=1600).contains(&state.position_ms)
    });
    player.seek(-5).unwrap();
    wait_for(&player, &seen, "at the start", |state| {
        state.position_ms <= 100
    });
    assert_eq!(
        player.state().status,
        Status::Paused,
        "seeking does not start playing"
    );

    // Past the end is the end.
    player.seek(99_000).unwrap();
    let end = wait_for(&player, &seen, "at the end", |state| {
        state.position_ms >= 1800
    });
    assert!(end.position_ms <= end.duration_ms.unwrap(), "{end:?}");

    player.play().unwrap();
    let again = wait_for(&player, &seen, "playing again", |state| {
        state.status == Status::Playing && state.position_ms < 1500
    });
    assert_eq!(again.problem, None);
    player.pause().unwrap();
    let paused = wait_for(&player, &seen, "paused again", status_is(Status::Paused));
    thread::sleep(Duration::from_millis(300));
    let later = player.state().position_ms;
    assert!(
        later.abs_diff(paused.position_ms) <= 50,
        "a paused player stays put"
    );
}

#[test]
fn speed_and_volume_stay_in_range() {
    let (player, seen) = new_player();
    player.set_speed(10.0).unwrap();
    wait_for(&player, &seen, "speed 4", |state| {
        state.speed == player::MAX_SPEED
    });
    player.set_speed(0.01).unwrap();
    wait_for(&player, &seen, "speed 0.25", |state| {
        state.speed == player::MIN_SPEED
    });
    assert_eq!(player.set_speed(f64::NAN).unwrap_err().kind, Kind::Input);
    player.set_volume(150.0).unwrap();
    wait_for(&player, &seen, "volume 100", |state| state.volume == 100.0);
    player.set_volume(-3.0).unwrap();
    wait_for(&player, &seen, "volume 0", |state| state.volume == 0.0);
    assert_eq!(
        player.set_volume(f64::INFINITY).unwrap_err().kind,
        Kind::Input
    );

    // At 4x, the 2 s clip takes about half a second.
    player.set_speed(4.0).unwrap();
    let started = Instant::now();
    player.open(&media("song.m4a"), 0, true).unwrap();
    assert_eq!(final_state(&player, &seen).status, Status::Ended);
    assert!(
        started.elapsed() < Duration::from_millis(1600),
        "{:?}",
        started.elapsed()
    );
}

#[test]
fn only_local_files_in_the_allowed_folders_open() {
    let (player, _seen) = new_player();
    let link_out = scratch().join("outside-link.mp4");
    let _ = fs::remove_file(&link_out);
    std::os::unix::fs::symlink(outside(), &link_out).unwrap();
    let secret = outside();
    let dir_name = secret
        .parent()
        .unwrap()
        .file_name()
        .unwrap()
        .to_str()
        .unwrap();
    let inside = scratch().join("inside-link.mp4");
    let _ = fs::remove_file(&inside);
    std::os::unix::fs::symlink(media_dir().join("clip.mp4"), &inside).unwrap();

    for (path, kind) in [
        ("https://example.com/video.mp4".to_owned(), Kind::Input),
        (format!("file://{}", secret.display()), Kind::Input),
        ("clip.mp4".to_owned(), Kind::Input),
        (secret.to_str().unwrap().to_owned(), Kind::Input),
        (
            format!("{}/../{dir_name}/secret.mp4", scratch().display()),
            Kind::Input,
        ),
        (link_out.to_str().unwrap().to_owned(), Kind::Input),
        (media_dir().to_str().unwrap().to_owned(), Kind::Input),
        (media("missing.mp4"), Kind::Unavailable),
    ] {
        let problem = player.open(&path, 0, true).unwrap_err();
        assert_eq!(problem.kind, kind, "{path}: {problem:?}");
        assert!(!problem.retry, "{path}");
        assert_eq!(
            player.state().status,
            Status::Idle,
            "{path} never reached mpv"
        );
    }
    // A link to an allowed file is fine: what counts is the real file.
    player.open(inside.to_str().unwrap(), 0, false).unwrap();
}

#[test]
fn broken_files_fail_and_the_player_goes_on() {
    let (player, seen) = new_player();
    let garbage = scratch().join("garbage.mp4");
    fs::write(
        &garbage,
        (0..50_000u32)
            .map(|i| (i * 7919 % 251) as u8)
            .collect::<Vec<_>>(),
    )
    .unwrap();
    let empty = scratch().join("empty.mkv");
    fs::write(&empty, b"").unwrap();
    let clip = fs::read(media_dir().join("clip.webm")).unwrap();
    let truncated = scratch().join("truncated.webm");
    fs::write(&truncated, &clip[..clip.len() / 3]).unwrap();

    for file in [&garbage, &empty] {
        player.open(file.to_str().unwrap(), 0, true).unwrap();
        let end = final_state(&player, &seen);
        assert_eq!(end.status, Status::Failed, "{}: {end:?}", file.display());
        let problem = end.problem.unwrap();
        assert_eq!(problem.kind, Kind::Unavailable, "{problem:?}");
        assert!(problem.detail.starts_with("mpv error"), "{problem:?}");
    }
    // Cut off in the middle: it plays what is there, or fails, but never hangs.
    player.set_speed(4.0).unwrap();
    player.open(truncated.to_str().unwrap(), 0, true).unwrap();
    final_state(&player, &seen);

    // And the next good file plays as if nothing happened.
    player.open(&media("clip.mp4"), 0, true).unwrap();
    let end = final_state(&player, &seen);
    assert_eq!((end.status, end.problem), (Status::Ended, None));
}

#[test]
fn a_new_file_replaces_the_old_one_cleanly() {
    let (player, seen) = new_player();
    player.open(&media("clip.mp4"), 0, true).unwrap();
    wait_for(&player, &seen, "playing", status_is(Status::Playing));
    seen.lock().unwrap().clear();
    player.open(&media("song.m4a"), 0, true).unwrap();
    wait_for(&player, &seen, "the song playing", |state| {
        state.status == Status::Playing
            && state.file.as_deref() == Some(media_dir().join("song.m4a").as_path())
    });
    let states = seen.lock().unwrap().clone();
    assert!(
        states
            .iter()
            .all(|state| state.file.as_deref() == Some(media_dir().join("song.m4a").as_path())),
        "nothing about the old file after the new one: {states:?}"
    );

    // Opens in a row: the last one wins, nothing fails.
    for name in ["clip.webm", "clip.mkv", "song.opus"] {
        player.open(&media(name), 0, true).unwrap();
    }
    player.set_speed(4.0).unwrap();
    let end = final_state(&player, &seen);
    assert_eq!(end.status, Status::Ended, "{end:?}");
    assert_eq!(
        end.file.as_deref(),
        Some(media_dir().join("song.opus").as_path())
    );
}

#[test]
fn stop_closes_the_file() {
    let (player, seen) = new_player();
    assert_eq!(
        player.play().unwrap_err().kind,
        Kind::Input,
        "nothing open yet"
    );
    player.open(&media("clip.mkv"), 0, true).unwrap();
    wait_for(&player, &seen, "playing", status_is(Status::Playing));
    player.stop().unwrap();
    let idle = wait_for(&player, &seen, "idle", status_is(Status::Idle));
    assert_eq!(
        (idle.file, idle.position_ms, idle.duration_ms),
        (None, 0, None)
    );
    thread::sleep(Duration::from_millis(200));
    assert_eq!(
        player.state().status,
        Status::Idle,
        "the old file's events change nothing"
    );
    for result in [player.play(), player.pause(), player.seek(100)] {
        assert_eq!(result.unwrap_err().kind, Kind::Input);
    }
}

#[test]
fn two_players_play_at_the_same_time() {
    let (music, music_seen) = new_player();
    let (video, video_seen) = new_player();
    music.open(&media("song.m4a"), 0, true).unwrap();
    video.open(&media("clip.webm"), 0, true).unwrap();
    let a = final_state(&music, &music_seen);
    let b = final_state(&video, &video_seen);
    assert_eq!((a.status, a.has_video), (Status::Ended, false));
    assert_eq!((b.status, b.has_video), (Status::Ended, true));
}

#[test]
fn players_come_and_go_quickly_and_a_bad_listener_breaks_nothing() {
    setup();
    for _ in 0..10 {
        let player = Player::new().unwrap();
        player.open(&media("clip.mp4"), 0, true).unwrap();
        thread::sleep(Duration::from_millis(30));
        let started = Instant::now();
        drop(player);
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "dropping took {:?}",
            started.elapsed()
        );
    }

    let player = Player::new().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = Seen::default();
    let (counted, sink) = (Arc::clone(&calls), Arc::clone(&seen));
    player.set_listener(move |state| {
        if counted.fetch_add(1, Ordering::SeqCst) % 2 == 0 {
            panic!("listener bug");
        }
        sink.lock().unwrap().push(state);
    });
    player.set_speed(4.0).unwrap();
    player.open(&media("song.opus"), 0, true).unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    while player.state().status != Status::Ended {
        assert!(Instant::now() < deadline, "{:?}", player.state());
        thread::sleep(Duration::from_millis(5));
    }
    assert!(
        calls.load(Ordering::SeqCst) >= 2,
        "it kept calling after a panic"
    );
}

#[test]
#[cfg(any(target_os = "linux", target_os = "android"))]
#[ignore = "counts the process's threads, so it must run alone: --ignored --test-threads=1"]
fn dropped_players_leave_no_threads_behind() {
    setup();
    let threads = || fs::read_dir("/proc/self/task").unwrap().count();
    // The first player loads libmpv, which may keep a few threads of its own.
    drop(Player::new().unwrap());
    thread::sleep(Duration::from_millis(300));
    let before = threads();
    for _ in 0..30 {
        let player = Player::new().unwrap();
        player.open(&media("clip.webm"), 0, true).unwrap();
        thread::sleep(Duration::from_millis(20));
    }
    thread::sleep(Duration::from_millis(1000));
    let after = threads();
    assert!(
        after <= before + 2,
        "{before} threads before, {after} after"
    );
}

#[test]
fn a_storm_of_commands_from_many_threads_leaves_a_sane_player() {
    let (player, seen) = new_player();
    let player = Arc::new(player);
    player.open(&media("clip.webm"), 0, true).unwrap();
    wait_for(&player, &seen, "playing", status_is(Status::Playing));
    let workers: Vec<_> = (0..4u64)
        .map(|worker| {
            let player = Arc::clone(&player);
            thread::spawn(move || {
                for step in 0..150u64 {
                    // Errors are fine (for example while a file opens); panics and hangs are not.
                    let _ = match (worker + step) % 6 {
                        0 => player.seek(((step * 137) % 2200) as i64),
                        1 => player.pause(),
                        2 => player.play(),
                        3 => player.set_speed(0.5 + (step % 7) as f64 * 0.5),
                        4 => player.set_volume((step % 101) as f64),
                        _ => player.seek(-1),
                    };
                    let state = player.state();
                    assert!(state.position_ms <= 2100, "{state:?}");
                }
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }
    player.set_speed(4.0).unwrap();
    player.play().unwrap();
    let end = final_state(&player, &seen);
    assert_eq!(
        (end.status, end.problem.clone()),
        (Status::Ended, None),
        "{end:?}"
    );
    let states = seen.lock().unwrap().clone();
    assert!(
        states
            .windows(2)
            .all(|pair| pair[0].revision < pair[1].revision)
    );
}

#[test]
fn opens_from_many_threads_end_with_one_of_them_open() {
    let (player, seen) = new_player();
    let player = Arc::new(player);
    let names = ["clip.mp4", "clip.webm", "clip.mkv", "song.m4a", "song.opus"];
    let workers: Vec<_> = (0..5)
        .map(|worker| {
            let player = Arc::clone(&player);
            thread::spawn(move || {
                for round in 0..10 {
                    player
                        .open(&media(names[(worker + round) % names.len()]), 0, false)
                        .unwrap();
                }
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }
    let open = wait_for(
        &player,
        &seen,
        "paused on a file",
        status_is(Status::Paused),
    );
    let file = open.file.clone().unwrap();
    assert!(names.iter().any(|name| file.ends_with(name)), "{open:?}");
    assert_eq!(open.problem, None);
    // And it plays from there.
    player.set_speed(4.0).unwrap();
    player.play().unwrap();
    assert_eq!(final_state(&player, &seen).status, Status::Ended);
}

#[test]
fn open_and_stop_again_and_again() {
    let (player, seen) = new_player();
    for round in 0..40 {
        player
            .open(
                &media(if round % 2 == 0 {
                    "song.m4a"
                } else {
                    "clip.mkv"
                }),
                0,
                true,
            )
            .unwrap();
        if round % 3 == 0 {
            wait_for(&player, &seen, "playing", status_is(Status::Playing));
        }
        player.stop().unwrap();
        assert_eq!(player.state().status, Status::Idle);
    }
    thread::sleep(Duration::from_millis(300));
    assert_eq!(
        player.state().status,
        Status::Idle,
        "late events of stopped files change nothing"
    );
    player.open(&media("song.opus"), 0, true).unwrap();
    player.set_speed(4.0).unwrap();
    assert_eq!(final_state(&player, &seen).status, Status::Ended);
}

#[test]
fn eight_players_play_at_once() {
    let players: Vec<(Player, Seen)> = (0..8).map(|_| new_player()).collect();
    let names = [
        "clip.mp4",
        "clip.webm",
        "clip.mkv",
        "song.m4a",
        "song.opus",
        "video-only.mp4",
    ];
    for (index, (player, _)) in players.iter().enumerate() {
        player
            .open(&media(names[index % names.len()]), 0, true)
            .unwrap();
    }
    for (index, (player, seen)) in players.iter().enumerate() {
        let end = final_state(player, seen);
        assert_eq!(end.status, Status::Ended, "player {index}: {end:?}");
    }
}
