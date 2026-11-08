//! Shared by the integration tests: the setup for yt-dlp and a small HTTP server on
//! 127.0.0.1 with fake media, a fake playlist page and a fake lyrics service.

#![allow(dead_code)]

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::Duration;

use elsewhere::ytdlp::{self, Event, Setup};

pub const SIZE: usize = 200_000;
pub const SLOW_SIZE: usize = 1_000_000;

pub fn ready() {
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
pub fn folder(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("elsewhere-ytdlp-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

pub fn url(path: &str) -> String {
    format!("{}{path}", server())
}

/// Collects events; returns the list and a callback that fills it.
pub fn recorder() -> (
    Arc<Mutex<Vec<Event>>>,
    impl Fn(Event) + Send + Sync + 'static,
) {
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    (events, move |event| sink.lock().unwrap().push(event))
}

pub fn body(size: usize) -> Vec<u8> {
    (0..size).map(|i| (i % 251) as u8).collect()
}

/// Text answers: playlist pages (HTML5 <video> tags, which yt-dlp reads as a playlist)
/// and a fake lyrics service with lrclib.net's search answer format.
fn page(path: &str) -> Option<(&'static str, String)> {
    let playlist = |title: &str, videos: &[&str]| {
        let tags: String = videos
            .iter()
            .map(|video| format!("<video src=\"{video}\"></video>\n"))
            .collect();
        let html = format!("<html><head><title>{title}</title></head><body>\n{tags}</body></html>");
        Some(("text/html; charset=utf-8", html))
    };
    match path {
        "/playlist.html" => playlist(
            "Test Playlist",
            &["/Band%20-%20Hit.mp4", "/Band%20-%20Other.mp4"],
        ),
        "/broken-playlist.html" => {
            playlist("Broken Playlist", &["/Band%20-%20Hit.mp4", "/missing.mp4"])
        }
        "/slow-playlist.html" => playlist("Slow Playlist", &["/slow.mp4", "/Band%20-%20Hit.mp4"]),
        path if path.starts_with("/api/search?") => {
            let answer = if path.contains("track_name=Hit&") {
                r#"[{"trackName":"Hit","artistName":"Band","instrumental":false,"syncedLyrics":"[00:01.00] la la","plainLyrics":"la la"}]"#
            } else if path.contains("track_name=Other&") {
                r#"[{"trackName":"Other","artistName":"Band","instrumental":false,"syncedLyrics":null,"plainLyrics":"just words"}]"#
            } else {
                "[]"
            };
            Some(("application/json", answer.to_owned()))
        }
        _ => None,
    }
}

/// Base URL of the test server, started on first use.
///
/// - `/<name>.mp4`: SIZE bytes, with Range support
/// - `/slow.mp4`: SLOW_SIZE bytes, sent slowly
/// - `/broken.mp4`: promises SIZE bytes, sends half, then closes
/// - `/missing.mp4`: 404
/// - `/playlist.html`, `/broken-playlist.html`, `/slow-playlist.html`, `/api/search?...`: see `page`
pub fn server() -> &'static str {
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

    if let Some((content_type, text)) = page(path) {
        let _ = write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
            text.len()
        );
        return;
    }

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

/// Folders for `api::start` under a fresh test folder, with the test Python: the phone
/// Python when YTDLP_TEST_PYTHON_HOME is set (emulator, simulator), else vendor/python.
pub fn api_folders(name: &str) -> elsewhere::api::Folders {
    let dir = folder(name);
    let text = |path: PathBuf| path.to_string_lossy().into_owned();
    let python_home = std::env::var("YTDLP_TEST_PYTHON_HOME").ok();
    let python_paths = match python_home {
        Some(_) => Vec::new(),
        None => vec![text(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("vendor/python"),
        )],
    };
    elsewhere::api::Folders {
        data: text(dir.join("data")),
        cache: text(dir.join("cache")),
        temp: text(dir.join("tmp")),
        media: text(dir.join("media")),
        python_home,
        python_paths,
        programs: None,
    }
}
