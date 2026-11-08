//! yt-dlp through the core's Python. Python and yt-dlp start lazily: `start` only sets
//! their environment, and the first call here (or the first job that needs them) starts
//! them, after unpacking the Android bundle when needed.

use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use super::{ApiError, Folders, LogLevel, core};
use crate::{phone, ytdlp};

/// At `start`: sets the environment Python will read. Cheap; Python itself starts later.
pub(super) fn prepare_env(folders: &Folders, bundled: bool) {
    let data = Path::new(&folders.data);
    let bin = folders
        .programs
        .as_deref()
        .and_then(|programs| phone::link_programs(Path::new(programs), data));
    let home = if bundled {
        Some(phone::python_dir(data))
    } else {
        folders.python_home.as_ref().map(PathBuf::from)
    };
    let paths: Vec<PathBuf> = folders.python_paths.iter().map(PathBuf::from).collect();
    phone::set_env(
        Path::new(&folders.cache),
        Path::new(&folders.temp),
        home.as_deref(),
        &paths,
        bin.as_deref(),
    );
}

static STARTING: Mutex<()> = Mutex::new(());

/// Starts Python and yt-dlp if they are not up yet (Android: unpacks the bundle first,
/// which only takes time after an install or update). Quick once they are up. Callers
/// that start at the same moment share one start.
pub(crate) fn ensure() -> Result<(), ApiError> {
    if ytdlp::is_started() {
        return Ok(());
    }
    let core = core()?;
    let _starting = STARTING
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if ytdlp::is_started() {
        return Ok(());
    }
    let mut home = core.folders.python_home.as_ref().map(PathBuf::from);
    let bundle = core
        .bundle
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(bundle) = bundle.as_deref() {
        if !bundle.rewind() {
            return Err(super::not_ready("cannot open the Python bundle"));
        }
        let reader = BundleReader {
            bundle,
            chunk: Vec::new(),
            offset: 0,
        };
        let unpacked = phone::unpack(reader, Path::new(&core.folders.data))
            .map_err(|err| super::not_ready(format!("could not unpack Python: {err}")))?;
        home = Some(unpacked);
    }
    let setup = ytdlp::Setup {
        python_home: home,
        python_paths: core
            .folders
            .python_paths
            .iter()
            .map(PathBuf::from)
            .collect(),
    };
    ytdlp::init(&setup).map(|_| ()).map_err(ApiError::from)
}

/// Reads the bundle through the app's callback, a chunk at a time.
struct BundleReader<'a> {
    bundle: &'a dyn super::PythonBundle,
    chunk: Vec<u8>,
    offset: usize,
}

impl Read for BundleReader<'_> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.offset == self.chunk.len() {
            self.chunk = self.bundle.read(256 * 1024);
            self.offset = 0;
        }
        let count = out.len().min(self.chunk.len() - self.offset);
        out[..count].copy_from_slice(&self.chunk[self.offset..self.offset + count]);
        self.offset += count;
        Ok(count)
    }
}

/// yt-dlp's version. Starts yt-dlp if needed.
#[uniffi::export]
pub fn ytdlp_version() -> Result<String, ApiError> {
    ensure()?;
    ytdlp::version().map_err(ApiError::from)
}

/// Receives what yt-dlp logs, prints and reports while it works. Runs on the worker
/// thread that called `ytdlp_info` or `ytdlp_download`.
#[uniffi::export(callback_interface)]
pub trait EventListener: Send + Sync {
    fn on_event(&self, event: YtdlpEvent);
}

#[derive(Debug, Clone, PartialEq, uniffi::Enum)]
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
        done: i64,
        total: Option<i64>,
        speed: Option<f64>,
        eta: Option<i64>,
        fragment_index: Option<i64>,
        fragment_count: Option<i64>,
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
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct Downloaded {
    /// Final files, after merging and post-processing.
    pub files: Vec<String>,
    /// yt-dlp's info JSON, or none when it stopped early.
    pub json: Option<String>,
}

/// Information about `url` as yt-dlp's JSON (title, formats, ...), without downloading.
/// `args` are yt-dlp's command-line flags. Starts yt-dlp if needed.
#[uniffi::export]
pub fn ytdlp_info(
    url: String,
    args: Vec<String>,
    listener: Box<dyn EventListener>,
    cancel: Arc<CancelToken>,
) -> Result<String, ApiError> {
    ensure()?;
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    ytdlp::info(&url, &args, events(listener), &cancel.0).map_err(ApiError::from)
}

/// Downloads `url`. `args` must say where: `-P <absolute folder>` or an absolute `-o`.
/// Starts yt-dlp if needed.
#[uniffi::export]
pub fn ytdlp_download(
    url: String,
    args: Vec<String>,
    listener: Box<dyn EventListener>,
    cancel: Arc<CancelToken>,
) -> Result<Downloaded, ApiError> {
    ensure()?;
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
    ensure()?;
    ytdlp::solve_youtube_challenge(&player_url, &kind, &challenge).map_err(ApiError::from)
}

fn events(listener: Box<dyn EventListener>) -> impl Fn(ytdlp::Event) + Send + Sync + 'static {
    move |event| listener.on_event(event.into())
}

fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn signed(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
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
                done: signed(done),
                total: total.map(signed),
                speed,
                eta: eta.map(signed),
                fragment_index: fragment_index.map(signed),
                fragment_count: fragment_count.map(signed),
            },
            ytdlp::Event::Downloaded { file } => YtdlpEvent::Downloaded { file: text(&file) },
            ytdlp::Event::PostProcess { name, status } => YtdlpEvent::PostProcess { name, status },
        }
    }
}

impl From<ytdlp::Error> for ApiError {
    fn from(err: ytdlp::Error) -> Self {
        crate::problem::from_ytdlp(&err).into()
    }
}
