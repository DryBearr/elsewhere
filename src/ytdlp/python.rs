//! The embedded Python side: start it once, load ytdlp.py, and turn Python values and
//! errors into Rust ones. Everything yt-dlp-specific lives in ytdlp.py.

use std::env;
use std::ffi::{CString, OsStr};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use pyo3::prelude::*;
use pyo3::types::{PyCFunction, PyDict, PyModule, PyTuple};

use super::{Cancel, Error, Event, Level, Setup};

const SOURCE: &str = include_str!("ytdlp.py");

/// The loaded ytdlp.py module, set once init() succeeds.
static BRIDGE: OnceLock<Py<PyModule>> = OnceLock::new();
/// Lets only one init() attempt run at a time.
static STARTING: Mutex<()> = Mutex::new(());

pub fn init(setup: &Setup) -> Result<String, Error> {
    let _starting = STARTING
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if BRIDGE.get().is_none() {
        let module = start(setup)?;
        let _ = BRIDGE.set(module);
    }
    version()
}

pub fn version() -> Result<String, Error> {
    let bridge = bridge()?;
    Python::attach(|py| {
        let module = bridge.bind(py);
        module
            .getattr("version")
            .and_then(|version| version.call0())
            .and_then(|version| version.extract::<String>())
            .map_err(|err| to_error(py, module, &err))
    })
}

/// Calls `function` (info or download) in ytdlp.py. Returns (final files, JSON).
pub fn run<F>(
    function: &str,
    url: &str,
    args: &[&str],
    on_event: F,
    cancel: &Cancel,
) -> Result<(Vec<PathBuf>, Option<String>), Error>
where
    F: Fn(Event) + Send + Sync + 'static,
{
    if cancel.is_cancelled() {
        return Err(Error::Cancelled);
    }
    let bridge = bridge()?;
    let cancel = cancel.clone();

    Python::attach(|py| {
        let module = bridge.bind(py);
        let call = || -> PyResult<(Vec<PathBuf>, Option<String>)> {
            let emit = PyCFunction::new_closure(
                py,
                None,
                None,
                move |args: &Bound<'_, PyTuple>, _: Option<&Bound<'_, PyDict>>| {
                    // A malformed event is dropped; it must never break a download.
                    if let Some(event) = to_event(args) {
                        // Release the Python lock while the caller's code runs.
                        args.py().detach(|| on_event(event));
                    }
                },
            )?;
            let cancelled = PyCFunction::new_closure(
                py,
                None,
                None,
                move |_: &Bound<'_, PyTuple>, _: Option<&Bound<'_, PyDict>>| cancel.is_cancelled(),
            )?;
            module
                .getattr(function)?
                .call1((url, args.to_vec(), emit, cancelled))?
                .extract()
        };
        call().map_err(|err| to_error(py, module, &err))
    })
}

fn bridge() -> Result<&'static Py<PyModule>, Error> {
    BRIDGE
        .get()
        .ok_or_else(|| Error::NotReady("call ytdlp::init first".to_owned()))
}

fn start(setup: &Setup) -> Result<Py<PyModule>, Error> {
    // SAFETY: Py_IsInitialized may be called at any time, even before Python starts.
    let running = unsafe { pyo3::ffi::Py_IsInitialized() } != 0;
    if !running {
        // A wrong home makes Python abort the whole process while starting, so check first.
        check_home(setup)?;
        let path = env::join_paths(&setup.python_paths)
            .map_err(|err| Error::NotReady(format!("python_paths: {err}")))?;
        // Python reads these once, while starting.
        if let Some(home) = &setup.python_home {
            set_env("PYTHONHOME", home.as_os_str());
        }
        if !setup.python_paths.is_empty() {
            set_env("PYTHONPATH", &path);
        }
        set_env("PYTHONUTF8", "1".as_ref());
        set_env("PYTHONNOUSERSITE", "1".as_ref());
        Python::initialize();
    }

    let source = CString::new(SOURCE)
        .map_err(|_| Error::NotReady("ytdlp.py contains a NUL byte".to_owned()))?;
    Python::attach(|py| {
        let load = || -> PyResult<Py<PyModule>> {
            let module = PyModule::from_code(py, &source, c"ytdlp.py", c"elsewhere_ytdlp")?;
            // YouTube's challenge solver runs in the built-in JavaScript engine (js.rs).
            let run_js = PyCFunction::new_closure(
                py,
                None,
                None,
                |args: &Bound<'_, PyTuple>, _: Option<&Bound<'_, PyDict>>| -> PyResult<String> {
                    let code: String = args.get_item(0)?.extract()?;
                    // Solving takes seconds; let other Python threads run meanwhile.
                    args.py()
                        .detach(|| super::js::run(&code))
                        .map_err(pyo3::exceptions::PyRuntimeError::new_err)
                },
            )?;
            module
                .getattr("setup")?
                .call1((setup.python_paths.clone(), run_js))?;
            Ok(module.unbind())
        };
        load().map_err(|err| {
            Error::NotReady(format!(
                "{}: {}",
                type_name(py, &err),
                message(err.value(py).as_any())
            ))
        })
    })
}

/// Sets an environment variable unless it already has this value, so an app that set
/// them all before starting threads (see the module docs) gets no writes here.
fn set_env(key: &str, value: &OsStr) {
    if env::var_os(key).as_deref() != Some(value) {
        // SAFETY: runs at most once per process, before Python starts. The module docs ask
        // callers to init, or to set these variables, before starting other threads.
        unsafe { env::set_var(key, value) };
    }
}

/// Checks that `python_home` exists and that a standard library can be found.
fn check_home(setup: &Setup) -> Result<(), Error> {
    let Some(home) = &setup.python_home else {
        return Ok(());
    };
    if !home.is_dir() {
        return Err(Error::NotReady(format!(
            "python_home {} is not a folder",
            home.display()
        )));
    }
    let in_home = std::fs::read_dir(home.join("lib"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("python"))
        });
    let mut places = setup.python_paths.iter().cloned().chain(in_home);
    if places.any(|place| has_standard_library(&place)) {
        Ok(())
    } else {
        Err(Error::NotReady(format!(
            "no Python standard library in {}/lib/python3.x or in python_paths",
            home.display()
        )))
    }
}

fn has_standard_library(place: &Path) -> bool {
    place.join("encodings").is_dir()
        || (place.is_file() && place.extension().is_some_and(|ext| ext == "zip"))
}

/// Turns an emit("kind", ...) call from ytdlp.py into an Event.
fn to_event(args: &Bound<'_, PyTuple>) -> Option<Event> {
    let text = |i: usize| -> Option<String> { Some(message(&args.get_item(i).ok()?)) };
    let file = |i: usize| -> Option<PathBuf> { args.get_item(i).ok()?.extract().ok() };
    let number =
        |i: usize| -> Option<u64> { args.get_item(i).ok()?.extract::<Option<u64>>().ok()? };

    Some(match text(0)?.as_str() {
        "log" => Event::Log {
            level: level(&text(1)?),
            message: text(2)?,
        },
        "output" => Event::Output(text(1)?),
        "progress" => Event::Progress {
            file: file(1)?,
            done: number(2).unwrap_or(0),
            total: number(3),
            speed: args.get_item(4).ok()?.extract::<Option<f64>>().ok()?,
            eta: number(5),
            fragment_index: number(6),
            fragment_count: number(7),
        },
        "downloaded" => Event::Downloaded { file: file(1)? },
        "postprocess" => Event::PostProcess {
            name: text(1)?,
            status: text(2)?,
        },
        _ => return None,
    })
}

fn level(name: &str) -> Level {
    match name {
        "debug" => Level::Debug,
        "warning" => Level::Warning,
        "error" => Level::Error,
        _ => Level::Info,
    }
}

/// Maps the three exceptions ytdlp.py raises to Error. Anything else is a yt-dlp failure.
fn to_error(py: Python<'_>, module: &Bound<'_, PyModule>, err: &PyErr) -> Error {
    let is = |class: &str| {
        module
            .getattr(class)
            .is_ok_and(|class| err.is_instance(py, &class))
    };
    let value = err.value(py);
    if is("Cancelled") {
        return Error::Cancelled;
    }
    if is("BadArgs") {
        return Error::BadArgs(message(value.as_any()));
    }
    if is("Failure")
        && let Ok((kind, message)) = value
            .getattr("args")
            .and_then(|args| args.extract::<(String, String)>())
    {
        return Error::YtDlp { kind, message };
    }
    Error::YtDlp {
        kind: type_name(py, err),
        message: message(value.as_any()),
    }
}

fn type_name(py: Python<'_>, err: &PyErr) -> String {
    err.get_type(py)
        .name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// str(value), with text that is not valid UTF-8 replaced instead of failing.
fn message(value: &Bound<'_, PyAny>) -> String {
    value
        .str()
        .map(|text| text.to_string_lossy().into_owned())
        .unwrap_or_default()
}
