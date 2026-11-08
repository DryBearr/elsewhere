//! Tests for src/ytdlp against the real yt-dlp, fully offline: a small HTTP server on
//! 127.0.0.1 serves fake media files. Needs yt-dlp in vendor/python:
//!
//!     python3 -m pip install --target vendor/python -r scripts/requirements.txt
//!
//! Tests that reach real websites are marked #[ignore]: `cargo test --features ytdlp -- --ignored`.
//!
//! On the Android emulator and iOS simulator (.github/workflows/mobile.yml), the test
//! binary runs with YTDLP_TEST_PYTHON_HOME set to the phone Python, which has yt-dlp in
//! its site-packages. YTDLP_TEST_FFMPEG=1 says ffmpeg is on PATH, so the ffmpeg tests run.

mod common;

use std::fs;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use common::{SIZE, SLOW_SIZE, body, folder, ready, recorder, url};
use elsewhere::ytdlp::{self, Cancel, Error, Event, Level, Setup};

#[test]
fn init_returns_version() {
    ready();
    let version = ytdlp::version().unwrap();
    assert!(version.contains('.'), "{version}");
    assert_eq!(
        ytdlp::init(&Setup::default()).unwrap(),
        version,
        "second init only returns the version"
    );
}

#[test]
fn info_returns_json_and_logs() {
    ready();
    let (events, on_event) = recorder();
    let json = ytdlp::info(&url("/video.mp4"), &[], on_event, &Cancel::new()).unwrap();
    assert!(json.contains(r#""id": "video""#), "{json}");
    assert!(json.contains(r#""ext": "mp4""#), "{json}");
    let events = events.lock().unwrap();
    assert!(
        events.iter().any(|e| matches!(
            e,
            Event::Log {
                level: Level::Info,
                ..
            }
        )),
        "{events:?}"
    );
}

#[test]
fn download_writes_file_and_reports_progress() {
    ready();
    let dir = folder("download");
    let (events, on_event) = recorder();
    let done = ytdlp::download(
        &url("/video.mp4"),
        &["-P", dir.to_str().unwrap(), "-o", "%(id)s.%(ext)s"],
        on_event,
        &Cancel::new(),
    )
    .unwrap();

    let file = dir.join("video.mp4");
    assert_eq!(done.files, vec![file.clone()]);
    assert_eq!(fs::read(&file).unwrap(), body(SIZE));
    assert!(done.json.unwrap().contains(r#""id": "video""#));

    let events = events.lock().unwrap();
    let last = events.iter().rev().find_map(|e| match e {
        Event::Progress { done, total, .. } => Some((*done, *total)),
        _ => None,
    });
    assert_eq!(last, Some((SIZE as u64, Some(SIZE as u64))), "{events:?}");
    assert!(
        events.iter().any(|e| matches!(e, Event::Downloaded { .. })),
        "{events:?}"
    );
}

#[test]
fn second_download_returns_existing_file() {
    ready();
    let dir = folder("existing");
    let args = ["-P", dir.to_str().unwrap(), "-o", "%(id)s.%(ext)s"];
    let first = ytdlp::download(&url("/video.mp4"), &args, |_| {}, &Cancel::new()).unwrap();
    let (events, on_event) = recorder();
    let second = ytdlp::download(&url("/video.mp4"), &args, on_event, &Cancel::new()).unwrap();
    assert_eq!(first.files, second.files);
    assert!(
        !events
            .lock()
            .unwrap()
            .iter()
            .any(|e| matches!(e, Event::Progress { .. })),
        "nothing downloaded again"
    );
}

#[test]
fn cancel_during_download_then_resume() {
    ready();
    let dir = folder("cancel");
    let args = ["-P", dir.to_str().unwrap(), "-o", "%(id)s.%(ext)s"];

    let cancel = Cancel::new();
    let stop = cancel.clone();
    let on_event = move |event| {
        if let Event::Progress { done, .. } = event
            && done > 0
        {
            stop.cancel();
        }
    };
    let started = Instant::now();
    assert_eq!(
        ytdlp::download(&url("/slow.mp4"), &args, on_event, &cancel),
        Err(Error::Cancelled)
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "stopped at the next step, not at the end"
    );
    assert!(
        dir.join("slow.mp4.part").exists(),
        "partial file kept for resuming"
    );

    let done = ytdlp::download(&url("/slow.mp4"), &args, |_| {}, &Cancel::new()).unwrap();
    assert_eq!(
        fs::read(&done.files[0]).unwrap(),
        body(SLOW_SIZE),
        "resumed file is complete and correct"
    );
}

#[test]
fn cancel_from_another_thread() {
    ready();
    let dir = folder("cancel-thread");
    let cancel = Cancel::new();
    let stop = cancel.clone();
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(300));
        stop.cancel();
    });
    let result = ytdlp::download(
        &url("/slow.mp4"),
        &["-P", dir.to_str().unwrap()],
        |_| {},
        &cancel,
    );
    assert_eq!(result, Err(Error::Cancelled));
}

#[test]
fn bad_flags_are_bad_args() {
    ready();
    let check = |args: &[&str], expected: &str| match ytdlp::info(
        &url("/video.mp4"),
        args,
        |_| {},
        &Cancel::new(),
    ) {
        Err(Error::BadArgs(message)) => assert!(message.contains(expected), "{args:?}: {message}"),
        other => panic!("{args:?}: {other:?}"),
    };
    check(&["--no-such-flag"], "no such option");
    check(&["--limit-rate", "fast"], "rate");
    check(&["--help"], "command-line program");
    check(&["--version"], "command-line program");
    check(&["-U"], "command-line program");
    check(&["--list-extractors"], "command-line program");
    check(&["--rm-cache-dir"], "command-line program");
    check(&["--no-plugin-dirs"], "command-line program");
    check(&["https://example.com/other"], "url argument");
}

#[test]
fn download_needs_absolute_output() {
    ready();
    let check =
        |args: &[&str]| match ytdlp::download(&url("/video.mp4"), args, |_| {}, &Cancel::new()) {
            Err(Error::BadArgs(_)) => {}
            other => panic!("{args:?}: {other:?}"),
        };
    check(&[]);
    check(&["-P", "relative/folder"]);
    check(&["-o", "-"]);
    check(&["-P", "/tmp", "-o", "-"]);
}

#[test]
fn print_flag_gives_output_events() {
    ready();
    let dir = folder("print");
    let (events, on_event) = recorder();
    let done = ytdlp::download(
        &url("/video.mp4"),
        &["-P", dir.to_str().unwrap(), "--print", "id"],
        on_event,
        &Cancel::new(),
    )
    .unwrap();
    assert!(done.files.is_empty(), "--print only simulates");
    assert!(
        events
            .lock()
            .unwrap()
            .contains(&Event::Output("video".to_owned()))
    );
}

#[test]
fn http_errors_are_ytdlp_errors() {
    ready();
    let dir = folder("errors");
    let args = [
        "-P",
        dir.to_str().unwrap(),
        "-R",
        "0",
        "--fragment-retries",
        "0",
    ];

    match ytdlp::download(&url("/missing.mp4"), &args, |_| {}, &Cancel::new()) {
        Err(Error::YtDlp { message, .. }) => assert!(message.contains("404"), "{message}"),
        other => panic!("{other:?}"),
    }
    match ytdlp::download(&url("/broken.mp4"), &args, |_| {}, &Cancel::new()) {
        Err(Error::YtDlp { kind, message }) => assert!(!kind.is_empty() && !message.is_empty()),
        other => panic!("{other:?}"),
    }
    match ytdlp::info("not a url", &[], |_| {}, &Cancel::new()) {
        Err(Error::YtDlp { message, .. }) => assert!(message.contains("not a url"), "{message}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn non_ascii_file_names() {
    ready();
    let dir = folder("names");
    let done = ytdlp::download(
        &url("/video.mp4"),
        &["-P", dir.to_str().unwrap(), "-o", "ვიდეო.%(ext)s"],
        |_| {},
        &Cancel::new(),
    )
    .unwrap();
    assert_eq!(done.files, vec![dir.join("ვიდეო.mp4")]);
    assert!(done.files[0].exists());
}

#[test]
fn parallel_downloads() {
    ready();
    let workers: Vec<_> = ["one", "two", "three"]
        .into_iter()
        .map(|name| {
            thread::spawn(move || {
                let dir = folder(&format!("parallel-{name}"));
                let args = ["-P", dir.to_str().unwrap(), "-o", "%(id)s.%(ext)s"];
                let done =
                    ytdlp::download(&url(&format!("/{name}.mp4")), &args, |_| {}, &Cancel::new())
                        .unwrap();
                assert_eq!(done.files, vec![dir.join(format!("{name}.mp4"))]);
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }
}

#[test]
fn on_event_may_call_back_into_the_module() {
    ready();
    let calls = Arc::new(Mutex::new(0));
    let count = calls.clone();
    ytdlp::info(
        &url("/video.mp4"),
        &[],
        move |_| {
            ytdlp::version().unwrap();
            *count.lock().unwrap() += 1;
        },
        &Cancel::new(),
    )
    .unwrap();
    assert!(*calls.lock().unwrap() > 0);
}

#[test]
fn planned_stop_is_not_an_error() {
    ready();
    let dir = folder("max");
    let done = ytdlp::download(
        &url("/video.mp4"),
        &["-P", dir.to_str().unwrap(), "--max-downloads", "1"],
        |_| {},
        &Cancel::new(),
    )
    .unwrap();
    assert_eq!(done.files.len(), 1);
}

#[test]
#[ignore = "reaches youtube.com"]
fn real_site_info() {
    ready();
    let (events, on_event) = recorder();
    let json = ytdlp::info(
        "https://www.youtube.com/watch?v=jNQXAC9IVRw",
        &[],
        on_event,
        &Cancel::new(),
    )
    .unwrap();
    assert!(json.contains("jNQXAC9IVRw"));

    // YouTube's JavaScript challenges were solved by the engine built into the app.
    let logs: Vec<String> = events
        .lock()
        .unwrap()
        .iter()
        .filter_map(|event| match event {
            Event::Log { message, .. } => Some(message.clone()),
            _ => None,
        })
        .collect();
    let all = logs.join("\n");
    assert!(all.contains("built-in QuickJS"), "{all}");
    assert!(!all.contains("challenge solving failed"), "{all}");
    assert!(!all.contains("formats may be missing"), "{all}");
}

#[test]
#[ignore = "reaches youtube.com"]
fn real_site_download() {
    ready();
    let dir = folder("youtube");
    // A single-file format, so no ffmpeg is needed (phones have none).
    let done = ytdlp::download(
        "https://www.youtube.com/watch?v=jNQXAC9IVRw",
        &["-P", dir.to_str().unwrap(), "-f", "b"],
        |_| {},
        &Cancel::new(),
    )
    .unwrap();
    assert_eq!(done.files.len(), 1, "{:?}", done.files);
    let size = fs::metadata(&done.files[0]).unwrap().len();
    assert!(size > 100_000, "{size} bytes");
}

#[test]
#[ignore = "reaches youtube.com"]
fn youtube_challenge_is_solved() {
    ready();
    // A test case from yt-dlp's own suite (test/test_jsc/test_ejs_integration.py). The
    // player script is a static file, which YouTube serves even where it blocks videos.
    let player = "https://www.youtube.com/s/player/631d3938/player_ias.vflset/en_US/base.js";
    let started = Instant::now();
    let answer = ytdlp::solve_youtube_challenge(player, "n", "KBx1qz7jMhxELa8c").unwrap();
    assert_eq!(answer, "ttPvh7WIptsgSw");
    println!("solved in {:?}", started.elapsed());
    assert!(matches!(
        ytdlp::solve_youtube_challenge(player, "x", "y"),
        Err(Error::BadArgs(_))
    ));
}

/// True when the runner put ffmpeg on PATH (the Android emulator run does).
fn has_ffmpeg() -> bool {
    std::env::var_os("YTDLP_TEST_FFMPEG").is_some_and(|value| value == "1")
}

#[test]
fn ffmpeg_is_found_and_runs() {
    if !has_ffmpeg() {
        return;
    }
    ready();
    let (events, on_event) = recorder();
    ytdlp::info(&url("/video.mp4"), &["-v"], on_event, &Cancel::new()).unwrap();
    let events = events.lock().unwrap();
    let versions = events.iter().find_map(|event| match event {
        Event::Log { message, .. } if message.contains("exe versions") => Some(message.clone()),
        _ => None,
    });
    let versions = versions.expect("yt-dlp -v always logs the programs it found");
    assert!(
        versions.contains("ffmpeg") && versions.contains("ffprobe"),
        "{versions}"
    );
}

#[test]
#[ignore = "reaches youtube.com"]
fn real_site_merge() {
    if !has_ffmpeg() {
        return;
    }
    ready();
    let dir = folder("youtube-merge");
    // Separate video and audio streams, merged by ffmpeg into one file.
    let done = ytdlp::download(
        "https://www.youtube.com/watch?v=jNQXAC9IVRw",
        &["-P", dir.to_str().unwrap(), "-f", "bv*[height<=240]+ba"],
        |_| {},
        &Cancel::new(),
    )
    .unwrap();
    assert_eq!(done.files.len(), 1, "{:?}", done.files);
    let size = fs::metadata(&done.files[0]).unwrap().len();
    assert!(size > 100_000, "{size} bytes");
}
