//! Starting the core: folders, database and jobs. Cheap on purpose; Python and yt-dlp
//! start later, on first use (see ytdlp.rs).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use super::{ApiError, bad_args, failed};
use crate::jobs::{self, Jobs};

/// The app's folders, given to `start`.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct Folders {
    /// Private storage: the database, and Python's home on Android.
    /// Android: filesDir. iOS: Library/Application Support.
    pub data: String,
    /// For caches the system may clear. Android: cacheDir. iOS: Library/Caches.
    pub cache: String,
    /// For temporary files.
    pub temp: String,
    /// Where saved media goes (playlists, ...).
    pub media: String,
    /// iOS: Python's home inside the app bundle. Android and desktop: none.
    pub python_home: Option<String>,
    /// Extra folders for Python imports (desktop development: vendor/python).
    pub python_paths: Vec<String>,
    /// Android: the folder holding libffmpeg.so (nativeLibraryDir). Others: none.
    pub programs: Option<String>,
}

/// Android: hands the core the APK's python.tar when it first needs Python. `rewind`
/// (re)starts reading from the beginning and says whether that worked; `read` returns the
/// next bytes, an empty list at the end. (Not `open`: UniFFI escapes that Swift keyword
/// into the C header, which then does not compile.)
#[uniffi::export(callback_interface)]
pub trait PythonBundle: Send + Sync {
    fn rewind(&self) -> bool;
    fn read(&self, max: u32) -> Vec<u8>;
}

/// What the core keeps after `start`.
pub(super) struct Core {
    pub folders: Folders,
    pub db: PathBuf,
    pub jobs: &'static Jobs,
    /// Read when Python first starts (only builds with yt-dlp have Python).
    #[cfg_attr(not(feature = "ytdlp"), allow(dead_code))]
    pub bundle: Mutex<Option<Box<dyn PythonBundle>>>,
}

static CORE: OnceLock<Core> = OnceLock::new();
static STARTING: Mutex<()> = Mutex::new(());

/// The started core, or NotReady.
pub(super) fn core() -> Result<&'static Core, ApiError> {
    CORE.get().ok_or_else(|| ApiError::NotReady {
        message: "call start first".to_owned(),
    })
}

/// Starts the core: makes the folders, opens the database and the jobs (jobs cut short
/// by a sudden close come back Interrupted) and registers what jobs can run. Cheap: Python
/// and yt-dlp start on the first job or call that needs them.
///
/// Call it once, first thing at app start: it sets environment variables, which is safe
/// only before other work starts. Calling it again with the same folders does nothing.
#[uniffi::export]
pub fn start(
    folders: Folders,
    python_bundle: Option<Box<dyn PythonBundle>>,
) -> Result<(), ApiError> {
    let _starting = STARTING
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(core) = CORE.get() {
        return if core.folders == folders {
            Ok(())
        } else {
            Err(bad_args("the core already started with other folders"))
        };
    }
    for dir in [&folders.data, &folders.cache, &folders.temp, &folders.media] {
        if dir.is_empty() || !Path::new(dir).is_absolute() {
            return Err(bad_args(format!(
                "folders must be absolute paths, not {dir:?}"
            )));
        }
        fs::create_dir_all(dir).map_err(|err| failed(format!("cannot make {dir}: {err}")))?;
    }

    #[cfg(feature = "ytdlp")]
    super::ytdlp::prepare_env(&folders, python_bundle.is_some());

    let db = Path::new(&folders.data).join("elsewhere.db");
    let jobs = jobs::open_global(&db).map_err(|message| ApiError::NotReady { message })?;
    super::demo::register(jobs);
    #[cfg(feature = "playlists")]
    super::playlists::register(jobs, &db, Path::new(&folders.media));

    let core = Core {
        folders,
        db,
        jobs,
        bundle: Mutex::new(python_bundle),
    };
    let _ = CORE.set(core);
    Ok(())
}

/// What is loaded right now, for diagnostics and tests.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct CoreStatus {
    pub started: bool,
    /// Whether this build carries yt-dlp at all.
    pub ytdlp_built_in: bool,
    /// Whether Python and yt-dlp have started (they start on first use).
    pub ytdlp_started: bool,
    pub jobs: u32,
    pub jobs_running: u32,
}

#[uniffi::export]
pub fn core_status() -> CoreStatus {
    let states = CORE.get().map(|core| core.jobs.list()).unwrap_or_default();
    CoreStatus {
        started: CORE.get().is_some(),
        ytdlp_built_in: cfg!(feature = "ytdlp"),
        #[cfg(feature = "ytdlp")]
        ytdlp_started: crate::ytdlp::is_started(),
        #[cfg(not(feature = "ytdlp"))]
        ytdlp_started: false,
        jobs: states.len() as u32,
        jobs_running: states
            .iter()
            .filter(|state| state.status == jobs::Status::Running)
            .count() as u32,
    }
}
