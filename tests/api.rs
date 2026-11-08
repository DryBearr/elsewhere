//! The apps' API (src/api) end to end, the way Kotlin and Swift call it: one core started
//! once, one job listener, jobs started by id. Offline (local test server).

mod common;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use common::{api_folders, kind_of, server, url};
use elsewhere::api::{
    self, ApiError, CancelToken, DemoOptions, EventListener, JobListener, JobState, JobStatus,
    LogLevel, MediaFormat, PlaylistOptions, ProblemKind, YtdlpEvent,
};

/// Every state the one listener got, by job, in arrival order.
type Seen = Arc<Mutex<HashMap<i64, Vec<JobState>>>>;

struct Recorder(Seen);

impl JobListener for Recorder {
    fn on_job(&self, job: JobState) {
        self.0.lock().unwrap().entry(job.id).or_default().push(job);
    }
}

/// The folders the core started with (made once: making them again would empty them).
fn folders() -> &'static api::Folders {
    static FOLDERS: OnceLock<api::Folders> = OnceLock::new();
    FOLDERS.get_or_init(|| api_folders("api"))
}

/// Starts the core once for this test program and installs the recording listener.
fn core() -> &'static Seen {
    static SEEN: OnceLock<Seen> = OnceLock::new();
    SEEN.get_or_init(|| {
        // SAFETY: before start, which is before any thread reads the environment here.
        unsafe { std::env::set_var("ELSEWHERE_LRCLIB_URL", server()) };
        api::start(folders().clone(), None).unwrap();
        let seen = Seen::default();
        api::set_job_listener(Box::new(Recorder(seen.clone()))).unwrap();
        seen
    })
}

fn finished(status: JobStatus) -> bool {
    matches!(
        status,
        JobStatus::Done | JobStatus::Failed | JobStatus::Cancelled
    )
}

/// Waits until the job matches `wanted` and the listener has its latest revision.
fn wait_for(id: i64, wanted: fn(JobStatus) -> bool) -> JobState {
    let seen = core();
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let state = api::job(id).unwrap().expect("the job exists");
        let delivered = seen
            .lock()
            .unwrap()
            .get(&id)
            .and_then(|states| states.last())
            .is_some_and(|last| last.revision == state.revision);
        if wanted(state.status) && delivered {
            return state;
        }
        assert!(
            Instant::now() < deadline,
            "job {id} did not get there: {state:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn demo(items: u32, item_ms: u32, fail_at: Option<u32>, warn_at: Option<u32>) -> i64 {
    core();
    api::start_demo_job(DemoOptions {
        items,
        item_ms,
        fail_at,
        warn_at,
    })
    .unwrap()
}

fn delivered(id: i64) -> Vec<JobState> {
    core().lock().unwrap().get(&id).cloned().unwrap_or_default()
}

#[test]
fn a_demo_job_reaches_the_listener_exactly() {
    let id = demo(5, 40, Some(3), Some(2));
    let last = wait_for(id, finished);
    assert_eq!(
        (
            last.status,
            last.done,
            last.failed,
            last.total,
            last.kind.as_str()
        ),
        (JobStatus::Done, 4, 1, Some(5), "demo")
    );
    assert_eq!(last.current, None);
    assert!(
        last.last_error
            .as_ref()
            .unwrap()
            .text
            .contains("Demo item 3")
    );

    // What the screen got: revisions only go up, one final state, and it is the last one
    // and equal to what the core has.
    let states = delivered(id);
    assert!(
        states
            .windows(2)
            .all(|pair| pair[0].revision < pair[1].revision),
        "{states:?}"
    );
    assert_eq!(
        states.iter().filter(|state| finished(state.status)).count(),
        1
    );
    assert_eq!(states.last(), Some(&last));
    assert_eq!(
        api::jobs().unwrap().into_iter().find(|job| job.id == id),
        Some(last.clone())
    );
    // The log: the warning, the failed item, the end; numbered from 1, nothing missing.
    let log = api::job_logs(id, 0).unwrap();
    assert_eq!(log.len() as i64, last.log_lines);
    assert!(
        log.iter()
            .enumerate()
            .all(|(index, line)| line.number == index as i64 + 1)
    );
    let levels: Vec<LogLevel> = log.iter().map(|line| line.level).collect();
    assert_eq!(
        levels,
        [LogLevel::Warning, LogLevel::Error, LogLevel::Info],
        "{log:?}"
    );
    assert_eq!(api::job_logs(id, 2).unwrap().len(), 1);
    assert_eq!(
        api::job_logs(id, -10).unwrap().len(),
        3,
        "a negative since means all"
    );
    assert!(api::job_logs(id, 99).unwrap().is_empty());
}

#[test]
fn byte_progress_reaches_the_screen_while_an_item_runs() {
    // One slow item: throttled progress gets out several times while it runs. (With fast
    // items the screen sees only start and done, which is right.)
    let id = demo(1, 1500, None, None);
    wait_for(id, finished);
    let bytes: Vec<i64> = delivered(id)
        .iter()
        .filter_map(|state| state.current.as_ref())
        .map(|item| item.bytes)
        .filter(|bytes| *bytes > 0)
        .collect();
    assert!(bytes.len() >= 2, "{bytes:?}");
    assert!(bytes.windows(2).all(|pair| pair[0] <= pair[1]), "{bytes:?}");
    assert!(
        bytes.len() <= 8,
        "throttled to about 4 per second: {bytes:?}"
    );
}

#[test]
fn an_empty_demo_job_is_done_at_once() {
    let id = demo(0, 0, None, None);
    let last = wait_for(id, finished);
    assert_eq!(
        (last.status, last.done, last.total),
        (JobStatus::Done, 0, Some(0))
    );
}

#[test]
fn cancel_stops_a_running_job_once() {
    let id = demo(50, 100, None, None);
    wait_for(id, |status| status == JobStatus::Running);
    assert!(api::cancel_job(id).unwrap());
    let last = wait_for(id, finished);
    assert_eq!(last.status, JobStatus::Cancelled);
    assert!(last.done < 50);
    assert!(!api::cancel_job(id).unwrap(), "already finished");
    assert!(
        api::resume_job(id).is_err(),
        "a cancelled job is not resumed"
    );
}

#[test]
fn finished_jobs_can_be_forgotten_but_not_running_ones() {
    let running = demo(50, 100, None, None);
    wait_for(running, |status| status == JobStatus::Running);
    assert!(!api::forget_job(running).unwrap());
    api::cancel_job(running).unwrap();
    wait_for(running, finished);
    assert!(api::forget_job(running).unwrap());
    assert_eq!(api::job(running).unwrap(), None);
    assert!(api::job_logs(running, 0).unwrap().is_empty());
    assert!(!api::forget_job(running).unwrap(), "already gone");
}

#[test]
fn unknown_and_impossible_requests_are_refused_without_panicking() {
    core();
    for id in [-1, 0, i64::MAX, 987_654_321] {
        assert_eq!(api::job(id).unwrap(), None);
        assert!(api::job_logs(id, 0).unwrap().is_empty());
        assert!(!api::cancel_job(id).unwrap());
        assert!(!api::discard_job(id).unwrap());
        assert!(!api::forget_job(id).unwrap());
        assert!(kind_of(&api::resume_job(id)) == Some(ProblemKind::Input));
    }
    let done = demo(1, 0, None, None);
    wait_for(done, finished);
    assert!(
        kind_of(&api::resume_job(done)) == Some(ProblemKind::Input),
        "a Done job is not resumed"
    );
    assert!(
        !api::discard_job(done).unwrap(),
        "only Interrupted jobs are discarded"
    );

    let bad_demo = DemoOptions {
        items: 1001,
        item_ms: 0,
        fail_at: None,
        warn_at: None,
    };
    assert!(kind_of(&api::start_demo_job(bad_demo)) == Some(ProblemKind::Input));
}

#[test]
fn bad_playlist_requests_are_refused_before_a_job_starts() {
    core();
    let before = api::jobs().unwrap().len();
    let ok = PlaylistOptions {
        format: MediaFormat::Audio,
        lyrics: false,
        limit: None,
    };
    for link in [
        "",
        "   ",
        "youtube.com/playlist?list=x",
        "ftp://x/y",
        "https://",
        "https://a b/c",
    ] {
        assert!(
            kind_of(&api::save_playlist(link.to_owned(), ok)) == Some(ProblemKind::Input),
            "{link:?}"
        );
    }
    let zero_height = PlaylistOptions {
        format: MediaFormat::Video {
            max_height: Some(0),
        },
        ..ok
    };
    let zero_limit = PlaylistOptions {
        limit: Some(0),
        ..ok
    };
    for options in [zero_height, zero_limit] {
        assert!(
            kind_of(&api::save_playlist(url("/playlist.html"), options))
                == Some(ProblemKind::Input)
        );
    }
    // Refused requests leave no job behind (other tests may add their own meanwhile).
    let refused_titles = api::jobs()
        .unwrap()
        .into_iter()
        .skip(before)
        .filter(|job| job.kind == "playlist" && job.title.contains("ftp"))
        .count();
    assert_eq!(refused_titles, 0);
}

#[test]
fn a_playlist_job_saves_files_with_lyrics() {
    core();
    let options = PlaylistOptions {
        format: MediaFormat::Video {
            max_height: Some(720),
        },
        lyrics: true,
        limit: None,
    };
    let id = api::save_playlist(format!("  {}  ", url("/playlist.html")), options).unwrap();
    let last = wait_for(id, finished);
    assert_eq!(
        (last.status, last.done, last.failed, last.title.as_str()),
        (JobStatus::Done, 2, 0, "Test Playlist"),
        "{last:?}"
    );
    let log = api::job_logs(id, 0).unwrap();
    assert!(
        log.last().unwrap().message.starts_with("Saved 2 of 2"),
        "{log:?}"
    );
    let media = std::path::PathBuf::from(&folders().media).join("Videos");
    let files = std::fs::read_dir(media).unwrap().count();
    assert!(files >= 1, "the playlist folder exists");
}

#[test]
fn a_failed_job_can_be_retried_and_fails_the_same_way() {
    core();
    let options = PlaylistOptions {
        format: MediaFormat::Video { max_height: None },
        lyrics: false,
        limit: None,
    };
    let id = api::save_playlist(url("/missing.mp4"), options).unwrap();
    let first = wait_for(id, finished);
    assert_eq!(first.status, JobStatus::Failed, "{first:?}");
    assert!(
        first.last_error.as_ref().unwrap().detail.contains("404"),
        "{first:?}"
    );

    api::resume_job(id).unwrap();
    let again = wait_for(id, |status| status == JobStatus::Failed);
    assert!(again.revision > first.revision);
    let log = api::job_logs(id, 0).unwrap();
    assert!(log.iter().any(|line| line.message == "Resumed."), "{log:?}");
    assert_eq!(
        log.iter()
            .filter(|line| line.level == LogLevel::Error)
            .count(),
        2,
        "{log:?}"
    );
}

#[test]
fn at_most_two_jobs_run_at_once() {
    let ids: Vec<i64> = (0..6).map(|_| demo(3, 60, None, None)).collect();
    let mut most = 0;
    while !ids
        .iter()
        .all(|id| finished(api::job(*id).unwrap().unwrap().status))
    {
        most = most.max(api::core_status().jobs_running);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(most <= 2, "{most} ran at once");
    for id in ids {
        assert_eq!(wait_for(id, finished).status, JobStatus::Done);
    }
}

#[test]
fn ytdlp_calls_start_ytdlp_when_needed() {
    core();
    struct Lines(Mutex<Vec<String>>);
    impl EventListener for Lines {
        fn on_event(&self, event: YtdlpEvent) {
            if let YtdlpEvent::Output { line } = event {
                self.0.lock().unwrap().push(line);
            }
        }
    }
    let version = api::ytdlp_version().unwrap();
    assert!(version.contains('.'), "{version}");
    let json = api::ytdlp_info(
        url("/clip.mp4"),
        Vec::new(),
        Box::new(Lines(Mutex::default())),
        Arc::new(CancelToken::new()),
    )
    .unwrap();
    assert!(json.contains(r#""id": "clip""#), "{json}");

    let cancel = Arc::new(CancelToken::new());
    cancel.cancel();
    assert!(cancel.is_cancelled());
    let result = api::ytdlp_info(
        url("/clip.mp4"),
        Vec::new(),
        Box::new(Lines(Mutex::default())),
        cancel,
    );
    assert!(
        kind_of(&result) == Some(ProblemKind::Cancelled),
        "{result:?}"
    );
    let bad = api::ytdlp_info(
        url("/clip.mp4"),
        vec!["--no-such-flag".to_owned()],
        Box::new(Lines(Mutex::default())),
        Arc::new(CancelToken::new()),
    );
    assert!(kind_of(&bad) == Some(ProblemKind::Input), "{bad:?}");
}

#[test]
fn many_jobs_from_many_threads() {
    core();
    let threads: Vec<_> = (0..8)
        .map(|_| {
            std::thread::spawn(|| {
                (0..5)
                    .map(|_| demo(4, 5, Some(2), None))
                    .collect::<Vec<_>>()
            })
        })
        .collect();
    let ids: Vec<i64> = threads
        .into_iter()
        .flat_map(|thread| thread.join().unwrap())
        .collect();
    let mut unique = ids.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), 40, "every job got its own id");
    for id in ids {
        let last = wait_for(id, finished);
        assert_eq!(
            (last.status, last.done, last.failed),
            (JobStatus::Done, 3, 1)
        );
        let states = delivered(id);
        assert!(
            states
                .windows(2)
                .all(|pair| pair[0].revision < pair[1].revision)
        );
    }
}

#[test]
fn the_core_keeps_its_log_on_the_device() {
    core();
    let log =
        std::fs::read_to_string(std::path::Path::new(&folders().data).join("logs/elsewhere.log"))
            .unwrap();
    assert!(log.contains("Starting Elsewhere"), "{log}");
    assert!(
        log.lines()
            .all(|line| line.starts_with('{') && line.ends_with('}'))
    );
}

#[test]
fn errors_carry_a_problem_for_the_screen() {
    core();
    let Err(ApiError::Failed { problem }) = api::resume_job(999_999) else {
        panic!("an unknown job cannot be resumed");
    };
    assert_eq!(problem.kind, ProblemKind::Input);
    assert!(!problem.text.is_empty() && !problem.retry, "{problem:?}");
}

#[test]
fn a_saved_playlist_reads_back_from_the_library() {
    core();
    // The test server's files are videos: an audio-only save finds no audio stream there.
    let options = PlaylistOptions {
        format: MediaFormat::Video { max_height: None },
        lyrics: false,
        limit: None,
    };
    let id = api::save_playlist(url("/library-playlist.html"), options).unwrap();
    let job = wait_for(id, finished);
    assert_eq!((job.status, job.done), (JobStatus::Done, 2), "{job:?}");

    let tabs = api::categories().unwrap();
    let videos = tabs.iter().find(|tab| tab.name == "Videos").unwrap();
    assert_eq!(videos.media, api::MediaKind::Video);
    let lists = api::collections(videos.id).unwrap();
    let list = lists
        .iter()
        .find(|list| list.name == "Library Playlist")
        .expect("the saved playlist");
    assert_eq!((list.entries, list.smart), (2, false));
    assert!(list.source.is_some());
    assert_eq!(api::collection(list.id).unwrap(), *list);

    let first = api::collection_items(list.id, None, 1).unwrap();
    assert_eq!(first.entries.len(), 1);
    let second = api::collection_items(list.id, first.next.clone(), 1).unwrap();
    assert_eq!((second.entries.len(), second.next.clone()), (1, None));
    let titles: Vec<String> = first
        .entries
        .iter()
        .chain(&second.entries)
        .map(|entry| entry.item.title.clone())
        .collect();
    assert_eq!(titles, ["Lib - One", "Lib - Two"]);
    for entry in first.entries.iter().chain(&second.entries) {
        let item = &entry.item;
        assert_eq!(
            (item.kind, item.media, item.missing),
            (api::ItemKind::Entry, api::MediaKind::Video, false)
        );
        let file = std::path::PathBuf::from(api::item_file(item.id).unwrap());
        assert!(file.is_file(), "{}", file.display());
        let media = std::fs::canonicalize(&folders().media).unwrap();
        assert!(
            file.starts_with(&media),
            "{} is in {}",
            file.display(),
            media.display()
        );
        let detail = api::item(item.id).unwrap();
        assert_eq!(detail.summary.id, item.id);
        assert!(detail.source_url.is_some(), "{detail:?}");
    }

    assert_eq!(
        kind_of(&api::collection_items(
            list.id,
            Some("not a cursor".to_owned()),
            1
        )),
        Some(ProblemKind::Input)
    );
    assert_eq!(
        kind_of(&api::item(i64::MAX)),
        Some(ProblemKind::Unavailable)
    );
    assert_eq!(kind_of(&api::item_file(-1)), Some(ProblemKind::Unavailable));
    assert_eq!(
        kind_of(&api::collections(-5)),
        Some(ProblemKind::Unavailable)
    );
}

#[test]
fn a_preview_before_saving() {
    core();
    let cancel = Arc::new(CancelToken::new());
    let preview = api::preview_playlist(
        format!("  {}  ", url("/playlist.html")),
        Arc::clone(&cancel),
    )
    .unwrap();
    assert_eq!(
        (
            preview.title.as_str(),
            preview.is_playlist,
            preview.available,
            preview.generated,
            preview.complete
        ),
        ("Test Playlist", true, 2, false, true)
    );
    assert!(preview.unavailable.is_empty() && preview.suggested_limit.is_none());
    assert_eq!(
        kind_of(&api::preview_playlist(
            "not a link".to_owned(),
            Arc::clone(&cancel)
        )),
        Some(ProblemKind::Input)
    );
    let stopped = Arc::new(CancelToken::new());
    stopped.cancel();
    assert_eq!(
        kind_of(&api::preview_playlist(url("/playlist.html"), stopped)),
        Some(ProblemKind::Cancelled)
    );
}
