//! iOS: Python and yt-dlp sit inside the app bundle, in Elsewhere.app/python, copied there
//! by the Xcode build (ios/project.yml) from target/ios/Python.xcframework. The bundle is
//! read-only, so Python keeps its compiled-file cache in the app's Library/Caches.

use std::fs;
use std::path::PathBuf;

use crate::ytdlp::Setup;

/// Sets the environment Python and yt-dlp need. The PYTHON* values match what ytdlp::init
/// would set, so init (which runs later, on a worker thread) does not touch the
/// environment.
pub fn set_env() {
    let Some(home) = python_dir() else {
        return;
    };
    let caches = PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join("Library/Caches");
    let pycache = caches.join("python");
    let _ = fs::create_dir_all(&pycache);
    // SAFETY: runs at the start of run(), before Slint or anything else in this process
    // starts threads that could read the environment.
    unsafe {
        std::env::set_var("XDG_CACHE_HOME", &caches);
        std::env::set_var("PYTHONPYCACHEPREFIX", &pycache);
        std::env::set_var("PYTHONHOME", home);
        std::env::set_var("PYTHONUTF8", "1");
        std::env::set_var("PYTHONNOUSERSITE", "1");
    }
}

/// Python's home inside the app bundle. Nothing to unpack on iOS.
pub fn python_setup() -> Result<Setup, String> {
    let home = python_dir().ok_or("cannot find the app bundle")?;
    Ok(Setup {
        python_home: Some(home),
        python_paths: Vec::new(),
    })
}

/// The app's executable sits at the top of the bundle, next to the python folder.
fn python_dir() -> Option<PathBuf> {
    Some(std::env::current_exe().ok()?.parent()?.join("python"))
}

/// Writes a line to stderr (the Xcode console, or `simctl launch --console-pty`).
pub fn log(message: &str) {
    eprintln!("Elsewhere: {message}");
}
