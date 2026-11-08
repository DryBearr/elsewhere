//! Everything the Android and iOS apps can call. UniFFI generates the Kotlin and Swift
//! bindings from this file (scripts/android_rust.sh, scripts/ios_rust.sh), so names here
//! are the names the apps see: `start_ytdlp` becomes `startYtdlp` in Kotlin and Swift.
//!
//! One section per module. A module plugs out with its feature flag, and its section goes
//! with it. All functions block: the apps call them from a background thread.

use std::fmt;
use std::path::Path;

/// Every error the apps can get. The apps receive the variant and its text (Display).
#[derive(Debug, uniffi::Error)]
#[uniffi(flat_error)]
pub enum ApiError {
    /// Not set up yet, or the setup is broken (for example Python is missing).
    NotReady { message: String },
    /// The arguments cannot be used.
    BadArgs { message: String },
    /// Stopped by a CancelToken.
    Cancelled,
    /// yt-dlp failed; `kind` is the yt-dlp error type, such as "DownloadError".
    YtDlp { kind: String, message: String },
    /// Anything else, such as a database or file error.
    Failed { message: String },
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ApiError::NotReady { message } => write!(f, "not ready: {message}"),
            ApiError::BadArgs { message } => write!(f, "bad arguments: {message}"),
            ApiError::Cancelled => f.write_str("cancelled"),
            ApiError::YtDlp { message, .. } => f.write_str(message),
            ApiError::Failed { message } => f.write_str(message),
        }
    }
}

impl std::error::Error for ApiError {}

fn failed(err: impl fmt::Display) -> ApiError {
    ApiError::Failed {
        message: err.to_string(),
    }
}

// --- Library ---------------------------------------------------------------------------

/// Category names, in display order. Opens (and creates or migrates) the database file.
#[uniffi::export]
pub fn categories(db_path: String) -> Result<Vec<String>, ApiError> {
    let db = crate::db::open(Path::new(&db_path)).map_err(failed)?;
    let mut stmt = db
        .prepare("SELECT name FROM category ORDER BY position")
        .map_err(failed)?;
    let names = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(failed)?;
    names.collect::<Result<_, _>>().map_err(failed)
}

// --- yt-dlp ----------------------------------------------------------------------------

#[cfg(feature = "ytdlp")]
pub use self::ytdlp_api::*;

#[cfg(feature = "ytdlp")]
mod ytdlp_api {
    use std::io::{self, Read};
    use std::path::PathBuf;
    use std::sync::Arc;

    use super::*;
    use crate::{phone, ytdlp};

    /// The app's folders, given to `start_ytdlp`.
    #[derive(uniffi::Record)]
    pub struct Folders {
        /// The app's private storage (Android: filesDir; iOS: Application Support).
        pub data: String,
        /// For caches the system may clear (Android: cacheDir; iOS: Library/Caches).
        pub cache: String,
        /// For temporary files.
        pub temp: String,
        /// Python's home: from `unpack_python` on Android, inside the app bundle on iOS.
        pub python_home: String,
        /// Android: the folder with libffmpeg.so (nativeLibraryDir). iOS: none.
        pub programs: Option<String>,
    }

    /// Starts Python and yt-dlp. Returns yt-dlp's version. Call it once, first thing at
    /// app start: it sets environment variables, which is safe only before other work.
    #[uniffi::export]
    pub fn start_ytdlp(folders: Folders) -> Result<String, ApiError> {
        let data = PathBuf::from(&folders.data);
        let home = PathBuf::from(&folders.python_home);
        let bin = folders
            .programs
            .as_deref()
            .and_then(|programs| phone::link_programs(Path::new(programs), &data));
        phone::set_env(
            Path::new(&folders.cache),
            Path::new(&folders.temp),
            Some(&home),
            bin.as_deref(),
        );
        let setup = ytdlp::Setup {
            python_home: Some(home),
            python_paths: Vec::new(),
        };
        ytdlp::init(&setup).map_err(ApiError::from)
    }

    /// A stream of bytes from the app, read until it returns an empty list. Android hands
    /// over the python.tar asset this way.
    #[uniffi::export(callback_interface)]
    pub trait ByteSource: Send + Sync {
        fn read(&self, max: u32) -> Vec<u8>;
    }

    /// Android: unpacks the python.tar asset into `<data>/python` the first time after an
    /// install or update (later calls only compare a stamp). Returns the Python home, for
    /// `Folders::python_home`.
    #[uniffi::export]
    pub fn unpack_python(source: Box<dyn ByteSource>, data: String) -> Result<String, ApiError> {
        let reader = SourceReader {
            source,
            chunk: Vec::new(),
            offset: 0,
        };
        let home = phone::unpack(reader, Path::new(&data)).map_err(|err| ApiError::NotReady {
            message: format!("could not unpack Python: {err}"),
        })?;
        Ok(home.to_string_lossy().into_owned())
    }

    struct SourceReader {
        source: Box<dyn ByteSource>,
        chunk: Vec<u8>,
        offset: usize,
    }

    impl Read for SourceReader {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            if self.offset == self.chunk.len() {
                self.chunk = self.source.read(256 * 1024);
                self.offset = 0;
            }
            let count = out.len().min(self.chunk.len() - self.offset);
            out[..count].copy_from_slice(&self.chunk[self.offset..self.offset + count]);
            self.offset += count;
            Ok(count)
        }
    }

    /// yt-dlp's version, once started.
    #[uniffi::export]
    pub fn ytdlp_version() -> Result<String, ApiError> {
        ytdlp::version().map_err(ApiError::from)
    }

    /// Receives what yt-dlp logs, prints and reports while it works. Runs on the worker
    /// thread that called `ytdlp_info` or `ytdlp_download`.
    #[uniffi::export(callback_interface)]
    pub trait EventListener: Send + Sync {
        fn on_event(&self, event: YtdlpEvent);
    }

    #[derive(uniffi::Enum)]
    pub enum LogLevel {
        Debug,
        Info,
        Warning,
        Error,
    }

    #[derive(uniffi::Enum)]
    pub enum YtdlpEvent {
        Log {
            level: LogLevel,
            message: String,
        },
        /// A line yt-dlp printed (--print, -j, -F, ...).
        Output {
            line: String,
        },
        Progress {
            file: String,
            done: u64,
            total: Option<u64>,
            speed: Option<f64>,
            eta: Option<u64>,
            fragment_index: Option<u64>,
            fragment_count: Option<u64>,
        },
        Downloaded {
            file: String,
        },
        PostProcess {
            name: String,
            status: String,
        },
    }

    /// Stops a running `ytdlp_info` or `ytdlp_download` from any thread.
    #[derive(uniffi::Object, Default)]
    pub struct CancelToken(ytdlp::Cancel);

    #[uniffi::export]
    impl CancelToken {
        #[uniffi::constructor]
        pub fn new() -> Self {
            Self::default()
        }

        pub fn cancel(&self) {
            self.0.cancel();
        }

        pub fn is_cancelled(&self) -> bool {
            self.0.is_cancelled()
        }
    }

    /// What a download produced.
    #[derive(uniffi::Record)]
    pub struct Downloaded {
        /// Final files, after merging and post-processing.
        pub files: Vec<String>,
        /// yt-dlp's info JSON, or none when it stopped early.
        pub json: Option<String>,
    }

    /// Information about `url` as yt-dlp's JSON (title, formats, ...), without downloading.
    /// `args` are yt-dlp's command-line flags.
    #[uniffi::export]
    pub fn ytdlp_info(
        url: String,
        args: Vec<String>,
        listener: Box<dyn EventListener>,
        cancel: Arc<CancelToken>,
    ) -> Result<String, ApiError> {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        ytdlp::info(&url, &args, events(listener), &cancel.0).map_err(ApiError::from)
    }

    /// Downloads `url`. `args` must say where: `-P <absolute folder>` or an absolute `-o`.
    #[uniffi::export]
    pub fn ytdlp_download(
        url: String,
        args: Vec<String>,
        listener: Box<dyn EventListener>,
        cancel: Arc<CancelToken>,
    ) -> Result<Downloaded, ApiError> {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let done = ytdlp::download(&url, &args, events(listener), &cancel.0)?;
        Ok(Downloaded {
            files: done.files.iter().map(|file| text(file)).collect(),
            json: done.json,
        })
    }

    /// Checks YouTube support on this device: solves one of YouTube's JavaScript challenges
    /// with the built-in engine. See ytdlp::solve_youtube_challenge.
    #[uniffi::export]
    pub fn solve_youtube_challenge(
        player_url: String,
        kind: String,
        challenge: String,
    ) -> Result<String, ApiError> {
        ytdlp::solve_youtube_challenge(&player_url, &kind, &challenge).map_err(ApiError::from)
    }

    fn events(listener: Box<dyn EventListener>) -> impl Fn(ytdlp::Event) + Send + Sync + 'static {
        move |event| listener.on_event(event.into())
    }

    fn text(path: &Path) -> String {
        path.to_string_lossy().into_owned()
    }

    impl From<ytdlp::Event> for YtdlpEvent {
        fn from(event: ytdlp::Event) -> Self {
            match event {
                ytdlp::Event::Log { level, message } => YtdlpEvent::Log {
                    level: match level {
                        ytdlp::Level::Debug => LogLevel::Debug,
                        ytdlp::Level::Info => LogLevel::Info,
                        ytdlp::Level::Warning => LogLevel::Warning,
                        ytdlp::Level::Error => LogLevel::Error,
                    },
                    message,
                },
                ytdlp::Event::Output(line) => YtdlpEvent::Output { line },
                ytdlp::Event::Progress {
                    file,
                    done,
                    total,
                    speed,
                    eta,
                    fragment_index,
                    fragment_count,
                } => YtdlpEvent::Progress {
                    file: text(&file),
                    done,
                    total,
                    speed,
                    eta,
                    fragment_index,
                    fragment_count,
                },
                ytdlp::Event::Downloaded { file } => YtdlpEvent::Downloaded { file: text(&file) },
                ytdlp::Event::PostProcess { name, status } => {
                    YtdlpEvent::PostProcess { name, status }
                }
            }
        }
    }

    impl From<ytdlp::Error> for ApiError {
        fn from(err: ytdlp::Error) -> Self {
            match err {
                ytdlp::Error::NotReady(message) => ApiError::NotReady { message },
                ytdlp::Error::BadArgs(message) => ApiError::BadArgs { message },
                ytdlp::Error::Cancelled => ApiError::Cancelled,
                ytdlp::Error::YtDlp { kind, message } => ApiError::YtDlp { kind, message },
            }
        }
    }
}
