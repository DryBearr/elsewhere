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

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use elsewhere::ytdlp::{self, Cancel, Error, Event, Level, Setup};

const SIZE: usize = 200_000;
const SLOW_SIZE: usize = 1_000_000;

fn ready() {
    let setup = match std::env::var_os("YTDLP_TEST_PYTHON_HOME") {
        Some(home) => Setup {
            python_home: Some(PathBuf::from(home)),
            python_paths: Vec::new(),
        },
        None => Setup {
            python_home: None,
            python_paths: vec![PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("vendor/python")],
        },
    };
    if let Err(err) = ytdlp::init(&setup) {
        panic!(
            "yt-dlp did not start ({err}); run: python3 -m pip install --target vendor/python -r scripts/requirements.txt"
        );
    }
}

/// A fresh, empty folder for one test.
fn folder(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("elsewhere-ytdlp-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn url(path: &str) -> String {
    format!("{}{path}", server())
}

/// Collects events; returns the list and a callback that fills it.
fn recorder() -> (
    Arc<Mutex<Vec<Event>>>,
    impl Fn(Event) + Send + Sync + 'static,
) {
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    (events, move |event| sink.lock().unwrap().push(event))
}

fn body(size: usize) -> Vec<u8> {
    (0..size).map(|i| (i % 251) as u8).collect()
}

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

/// Base URL of the test server, started on first use.
///
/// - `/<name>.mp4`: SIZE bytes, with Range support
/// - `/slow.mp4`: SLOW_SIZE bytes, sent slowly
/// - `/broken.mp4`: promises SIZE bytes, sends half, then closes
/// - `/missing.mp4`: 404
fn server() -> &'static str {
    static URL: OnceLock<String> = OnceLock::new();
    URL.get_or_init(|| {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                thread::spawn(move || serve(stream));
            }
        });
        url
    })
}

fn serve(mut stream: TcpStream) {
    let Ok(clone) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(clone);
    let mut request = String::new();
    if reader.read_line(&mut request).is_err() {
        return;
    }
    let mut range: Option<(usize, Option<usize>)> = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
            break;
        }
        if let Some(value) = line.to_ascii_lowercase().strip_prefix("range: bytes=") {
            let (start, end) = value.trim().split_once('-').unwrap_or(("0", ""));
            range = Some((start.parse().unwrap_or(0), end.parse().ok()));
        }
    }
    let mut parts = request.split_whitespace();
    let (method, path) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));

    let (size, slow, broken) = match path {
        "/slow.mp4" => (SLOW_SIZE, true, false),
        "/broken.mp4" => (SIZE, false, true),
        "/missing.mp4" => (0, false, false),
        path if path.ends_with(".mp4") => (SIZE, false, false),
        _ => (0, false, false),
    };
    if size == 0 {
        let _ = stream
            .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        return;
    }

    let data = body(size);
    let (start, end) = match range {
        Some((start, end)) => (start, end.unwrap_or(size - 1).min(size - 1)),
        None => (0, size - 1),
    };
    if start >= size {
        let _ = write!(
            stream,
            "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{size}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        return;
    }
    let part = &data[start..=end];
    let status = if range.is_some() {
        "206 Partial Content"
    } else {
        "200 OK"
    };
    let mut head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: video/mp4\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n",
        part.len()
    );
    if range.is_some() {
        head += &format!("Content-Range: bytes {start}-{end}/{size}\r\n");
    }
    head += "\r\n";
    if stream.write_all(head.as_bytes()).is_err() || method == "HEAD" {
        return;
    }

    if broken {
        let _ = stream.write_all(&part[..part.len() / 2]);
    } else if slow {
        for chunk in part.chunks(16 * 1024) {
            if stream.write_all(chunk).is_err() {
                return;
            }
            thread::sleep(Duration::from_millis(15));
        }
    } else {
        let _ = stream.write_all(part);
    }
}
