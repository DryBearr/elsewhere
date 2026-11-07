#[cfg(feature = "ytdlp")]
pub mod ytdlp;

mod db;
#[cfg(feature = "ytdlp")]
mod platform;

use std::path::PathBuf;

use rusqlite::Connection;
use slint::{ModelRc, SharedString, VecModel};

slint::include_modules!();

pub fn run(db_path: PathBuf) {
    // Android sets it in android_main instead, before Slint starts its threads.
    #[cfg(all(feature = "ytdlp", not(target_os = "android")))]
    platform::set_env();

    let db = db::open(&db_path).unwrap();

    let app = App::new().unwrap();
    app.set_categories(ModelRc::new(VecModel::from(load_categories(&db))));
    start_ytdlp(&app);
    app.run().unwrap();
}

fn load_categories(db: &Connection) -> Vec<SharedString> {
    let mut stmt = db
        .prepare("SELECT name FROM category ORDER BY position")
        .unwrap();
    stmt.query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .map(|row| row.unwrap().into())
        .collect()
}

/// Starts yt-dlp on a worker thread and wires up the "Try yt-dlp" box.
#[cfg(feature = "ytdlp")]
fn start_ytdlp(app: &App) {
    app.set_ytdlp_status("Starting yt-dlp…".into());
    let weak = app.as_weak();
    std::thread::spawn(move || {
        let started = platform::python_setup()
            .and_then(|setup| ytdlp::init(&setup).map_err(|err| err.to_string()));
        let (status, ready) = match started {
            Ok(version) => (format!("yt-dlp {version} is ready."), true),
            Err(err) => (format!("yt-dlp could not start: {err}"), false),
        };
        // The emulator and simulator checks in .github/workflows/mobile.yml look for this.
        platform::log(&status);
        let _ = weak.upgrade_in_event_loop(move |app| {
            app.set_ytdlp_status(status.into());
            app.set_ytdlp_ready(ready);
        });
    });

    let weak = app.as_weak();
    app.on_ytdlp_info(move |link| {
        let Some(app) = weak.upgrade() else { return };
        app.set_ytdlp_busy(true);
        app.set_ytdlp_result("Looking it up…".into());
        let weak = weak.clone();
        let link = link.trim().to_owned();
        std::thread::spawn(move || {
            let result = lookup(&link);
            let _ = weak.upgrade_in_event_loop(move |app| {
                app.set_ytdlp_result(result.into());
                app.set_ytdlp_busy(false);
            });
        });
    });
}

#[cfg(not(feature = "ytdlp"))]
fn start_ytdlp(app: &App) {
    app.set_ytdlp_status("This build has no yt-dlp.".into());
}

/// Title, uploader and length of a link (the first three for a playlist), or the error.
#[cfg(feature = "ytdlp")]
fn lookup(link: &str) -> String {
    use std::sync::{Arc, Mutex};

    let lines = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&lines);
    // --print only simulates, so nothing is saved; -P is still required by download().
    let tmp = std::env::temp_dir();
    let tmp = tmp.to_string_lossy();
    let args = [
        "-P",
        &tmp,
        "--playlist-items",
        "1-3",
        "--print",
        "%(title)s",
        "--print",
        "%(uploader|unknown uploader)s · %(duration_string|unknown length)s",
    ];
    let on_event = move |event| {
        if let ytdlp::Event::Output(line) = event {
            sink.lock().unwrap().push(line);
        }
    };
    match ytdlp::download(link, &args, on_event, &ytdlp::Cancel::new()) {
        Ok(_) => lines.lock().unwrap().join("\n"),
        Err(err) => err.to_string(),
    }
}

#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
fn android_main(app: slint::android::AndroidApp) {
    let dir = app.internal_data_path().unwrap();
    #[cfg(feature = "ytdlp")]
    {
        platform::keep_app(&app);
        platform::set_env();
    }
    slint::android::init(app).unwrap();
    run(dir.join("elsewhere.db"));
}
