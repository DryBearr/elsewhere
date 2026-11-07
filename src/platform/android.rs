//! Android: the APK carries Python and yt-dlp as assets/python.tar (made by
//! scripts/mobile_python.py). The first start after an install or update unpacks it into
//! the app's private storage; later starts find it there. ffmpeg and ffprobe ship as
//! native libraries (scripts/android_ffmpeg.sh) and are put on PATH for yt-dlp.

use std::ffi::{CString, c_char, c_int};
use std::fs::{self, File};
use std::io::{self, BufReader, ErrorKind, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

use slint::android::AndroidApp;

use crate::ytdlp::Setup;

static APP: OnceLock<AndroidApp> = OnceLock::new();

/// Android only: keeps the app handle, which set_env and python_setup need. Call it first
/// thing in android_main.
pub fn keep_app(app: &AndroidApp) {
    let _ = APP.set(app.clone());
}

/// Sets the environment Python and yt-dlp need. Android sets neither TMPDIR (before
/// Android 13) nor a usable HOME, so temporary files and caches go into the app's storage.
/// The PYTHON* values are the ones ytdlp::init would set; with them in place, init (which
/// runs later, on a worker thread) does not touch the environment.
pub fn set_env() {
    let Some(data) = APP.get().and_then(|app| app.internal_data_path()) else {
        return;
    };
    let tmp = data.join("tmp");
    let cache = data.join("cache");
    let _ = fs::create_dir_all(&tmp);
    let _ = fs::create_dir_all(&cache);
    let path = match (link_programs(&data), std::env::var_os("PATH")) {
        (Some(bin), Some(path)) => std::env::join_paths([bin, PathBuf::from(path)]).ok(),
        (Some(bin), None) => Some(bin.into_os_string()),
        (None, _) => None,
    };
    // SAFETY: runs at the very start of android_main, before Slint or anything else
    // in this process starts threads that could read the environment.
    unsafe {
        std::env::set_var("HOME", &data);
        std::env::set_var("TMPDIR", &tmp);
        std::env::set_var("XDG_CACHE_HOME", &cache);
        if let Some(path) = path {
            std::env::set_var("PATH", path);
        }
        std::env::set_var("PYTHONHOME", python_dir(&data));
        std::env::set_var("PYTHONUTF8", "1");
        std::env::set_var("PYTHONNOUSERSITE", "1");
    }
}

/// Android runs app programs only from the folder it unpacks native libraries into, so
/// ffmpeg and ffprobe ship there as libffmpeg.so and libffprobe.so. This makes links
/// named ffmpeg and ffprobe in `<data>/bin` and returns that folder, or None when the
/// programs are missing.
fn link_programs(data: &Path) -> Option<PathBuf> {
    let libs = native_lib_dir()?;
    let bin = data.join("bin");
    fs::create_dir_all(&bin).ok()?;
    let mut linked = false;
    for name in ["ffmpeg", "ffprobe"] {
        let program = libs.join(format!("lib{name}.so"));
        let link = bin.join(name);
        // The folder moves with every app update, so always make the link again.
        let _ = fs::remove_file(&link);
        if program.is_file() && std::os::unix::fs::symlink(&program, &link).is_ok() {
            linked = true;
        }
    }
    linked.then_some(bin)
}

/// The folder holding this app's native libraries: the one libelsewhere.so was loaded
/// from, as listed in /proc/self/maps.
fn native_lib_dir() -> Option<PathBuf> {
    let maps = fs::read_to_string("/proc/self/maps").ok()?;
    let library = maps
        .lines()
        .filter_map(|line| line.split_whitespace().nth(5))
        .find(|path| path.ends_with("/libelsewhere.so"))?;
    Some(Path::new(library).parent()?.to_path_buf())
}

#[link(name = "log")]
unsafe extern "C" {
    fn __android_log_write(priority: c_int, tag: *const c_char, text: *const c_char) -> c_int;
}

/// Writes a line to logcat with the tag Elsewhere (`adb logcat -s Elsewhere`).
pub fn log(message: &str) {
    const INFO: c_int = 4;
    let text = CString::new(message.replace('\0', " ")).unwrap_or_default();
    // SAFETY: both strings are NUL-terminated and live through the call.
    unsafe { __android_log_write(INFO, c"Elsewhere".as_ptr(), text.as_ptr()) };
}

/// Unpacks python.tar from the APK when this version is not unpacked yet, and returns
/// where Python is. The archive starts with a stamp; a matching stamp on disk means
/// nothing to do.
pub fn python_setup() -> Result<Setup, String> {
    let app = APP.get().ok_or("keep_app was not called")?;
    let data = app
        .internal_data_path()
        .ok_or("Android gave no app storage folder")?;
    let asset = app
        .asset_manager()
        .open(c"python.tar")
        .ok_or("python.tar is missing from the APK")?;
    let home = unpack(BufReader::new(asset), &data)
        .map_err(|err| format!("could not unpack Python: {err}"))?;
    Ok(Setup {
        python_home: Some(home),
        python_paths: Vec::new(),
    })
}

/// Where the Python home goes inside the app's storage.
fn python_dir(data: &Path) -> PathBuf {
    data.join("python")
}

/// Unpacks a python.tar stream into `<data>/python` unless the same stamp is already there.
fn unpack(mut tar: impl Read, data: &Path) -> io::Result<PathBuf> {
    let home = python_dir(data);
    let stamp = match next_entry(&mut tar)? {
        Some(entry) if entry.path == Path::new("stamp") => {
            let mut stamp = Vec::new();
            read_data(&mut tar, entry.size, &mut stamp)?;
            stamp
        }
        _ => return Err(invalid("python.tar does not start with a stamp")),
    };
    if fs::read(home.join("stamp")).is_ok_and(|old| old == stamp) {
        return Ok(home);
    }

    // Unpack beside the old copy and swap at the end, so an unpack cut short (app killed,
    // disk full) never leaves a half-filled home behind: the stamp is written last.
    let fresh = data.join("python.new");
    remove_dir(&fresh)?;
    fs::create_dir_all(&fresh)?;
    while let Some(entry) = next_entry(&mut tar)? {
        let path = fresh.join(&entry.path);
        match entry.kind {
            b'0' | 0 => {
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent)?;
                }
                read_data(&mut tar, entry.size, &mut File::create(&path)?)?;
            }
            b'5' => {
                fs::create_dir_all(&path)?;
                read_data(&mut tar, entry.size, &mut io::sink())?;
            }
            // Links and other special entries: the archive has none, so skip them.
            _ => read_data(&mut tar, entry.size, &mut io::sink())?,
        }
    }
    fs::write(fresh.join("stamp"), &stamp)?;
    remove_dir(&home)?;
    fs::rename(&fresh, &home)?;
    Ok(home)
}

struct Entry {
    path: PathBuf,
    size: u64,
    kind: u8,
}

/// Reads the next ustar header. Returns None at the end of the archive.
fn next_entry(tar: &mut impl Read) -> io::Result<Option<Entry>> {
    let mut header = [0u8; 512];
    tar.read_exact(&mut header)?;
    if header.iter().all(|&byte| byte == 0) {
        return Ok(None);
    }

    let text = |start: usize, end: usize| -> io::Result<&str> {
        let field = &header[start..end];
        let len = field
            .iter()
            .position(|&byte| byte == 0)
            .unwrap_or(field.len());
        std::str::from_utf8(&field[..len])
            .map_err(|_| invalid("python.tar has a name that is not UTF-8"))
    };
    let name = text(0, 100)?;
    let prefix = text(345, 500)?;
    let path = if prefix.is_empty() {
        PathBuf::from(name)
    } else {
        Path::new(prefix).join(name)
    };
    // Only plain relative paths, so nothing can land outside the Python home.
    let safe = path.components().next().is_some()
        && path
            .components()
            .all(|part| matches!(part, Component::Normal(_)));
    if !safe {
        return Err(invalid("python.tar has an unsafe path"));
    }

    let size = text(124, 136)?.trim();
    let size = u64::from_str_radix(size, 8).map_err(|_| invalid("python.tar has a bad size"))?;
    Ok(Some(Entry {
        path,
        size,
        kind: header[156],
    }))
}

/// Copies an entry's data to `to`, then skips the padding up to the next 512-byte block.
fn read_data(tar: &mut impl Read, size: u64, to: &mut impl Write) -> io::Result<()> {
    let padding = (512 - size % 512) % 512;
    if io::copy(&mut tar.take(size), to)? != size
        || io::copy(&mut tar.take(padding), &mut io::sink())? != padding
    {
        return Err(io::Error::new(
            ErrorKind::UnexpectedEof,
            "python.tar is cut short",
        ));
    }
    Ok(())
}

fn remove_dir(path: &Path) -> io::Result<()> {
    match fs::remove_dir_all(path) {
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, message.to_owned())
}
