//! The core loads lazily: nothing works before `start`, `start` is cheap, and Python with
//! yt-dlp starts only for the first job that needs it. One test in its own process, so
//! nothing else starts Python first.

mod common;

use std::time::{Duration, Instant};

use common::{api_folders, server, url};
use elsewhere::api::{self, ApiError, DemoOptions, JobStatus, MediaFormat, PlaylistOptions};

fn wait_until_finished(id: i64) -> api::JobState {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let state = api::job(id).unwrap().unwrap();
        if matches!(
            state.status,
            JobStatus::Done | JobStatus::Failed | JobStatus::Cancelled
        ) {
            return state;
        }
        assert!(
            Instant::now() < deadline,
            "job {id} did not finish: {state:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn start_is_cheap_and_python_starts_on_first_use() {
    // Before start: everything says NotReady, nothing panics.
    assert!(!api::core_status().started);
    let not_ready = |result: Result<(), ApiError>| {
        assert!(
            matches!(result, Err(ApiError::NotReady { .. })),
            "{result:?}"
        )
    };
    not_ready(api::categories().map(drop));
    not_ready(api::jobs().map(drop));
    not_ready(api::job_logs(1, 0).map(drop));
    not_ready(api::cancel_job(1).map(drop));
    not_ready(api::ytdlp_version().map(drop));
    let demo = DemoOptions {
        items: 2,
        item_ms: 20,
        fail_at: None,
        warn_at: None,
    };
    not_ready(api::start_demo_job(demo.clone()).map(drop));

    // Folders must be absolute.
    let mut folders = api_folders("lazy");
    let mut relative = folders.clone();
    relative.media = "media".to_owned();
    assert!(matches!(
        api::start(relative, None),
        Err(ApiError::BadArgs { .. })
    ));
    assert!(!api::core_status().started);

    // Start: cheap, no Python.
    // SAFETY: before start, which is before any thread reads the environment here.
    unsafe { std::env::set_var("ELSEWHERE_LRCLIB_URL", server()) };
    let started = Instant::now();
    api::start(folders.clone(), None).unwrap();
    let status = api::core_status();
    assert!(
        status.started && status.ytdlp_built_in && !status.ytdlp_started,
        "{status:?}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "start took {:?}",
        started.elapsed()
    );
    api::start(folders.clone(), None).expect("starting again with the same folders is fine");
    folders.cache.push_str("-other");
    assert!(matches!(
        api::start(folders, None),
        Err(ApiError::BadArgs { .. })
    ));
    assert_eq!(api::categories().unwrap().len(), 9);

    // A job that needs no Python leaves Python down.
    let id = api::start_demo_job(demo).unwrap();
    assert_eq!(wait_until_finished(id).status, JobStatus::Done);
    assert!(
        !api::core_status().ytdlp_started,
        "the demo job must not start Python"
    );

    // The first playlist job starts it, and says so in its log.
    let options = PlaylistOptions {
        format: MediaFormat::Video {
            max_height: Some(720),
        },
        lyrics: false,
        limit: None,
    };
    let id = api::save_playlist(url("/playlist.html"), options).unwrap();
    let state = wait_until_finished(id);
    assert_eq!(
        (state.status, state.done),
        (JobStatus::Done, 2),
        "{state:?}"
    );
    assert!(api::core_status().ytdlp_started);
    let log = api::job_logs(id, 0).unwrap();
    assert_eq!(log[0].message, "Starting yt-dlp…", "{log:?}");
}
