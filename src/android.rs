//! Android: unpacks the Python that ships inside the APK (assets/python.tar, made by
//! scripts/android_python.py) into the app's private storage, where yt-dlp can run it.

use std::fs::{self, File};
use std::io::{self, BufReader, ErrorKind, Read, Write};
use std::path::{Component, Path, PathBuf};

use slint::android::AndroidApp;

/// Sets the environment Python and yt-dlp need. Android sets neither TMPDIR (before
/// Android 13) nor a usable HOME, so temporary files and caches go into the app's storage.
/// The PYTHON* values are the ones ytdlp::init would set; with them in place, init (which
/// runs later, on a worker thread) does not touch the environment.
/// Call it first thing in android_main, before other threads start.
pub fn set_env(data: &Path) {
    let tmp = data.join("tmp");
    let cache = data.join("cache");
    let _ = fs::create_dir_all(&tmp);
    let _ = fs::create_dir_all(&cache);
    // SAFETY: runs at the very start of android_main, before Slint or anything else
    // in this process starts threads that could read the environment.
    unsafe {
        std::env::set_var("HOME", data);
        std::env::set_var("TMPDIR", &tmp);
        std::env::set_var("XDG_CACHE_HOME", &cache);
        std::env::set_var("PYTHONHOME", python_dir(data));
        std::env::set_var("PYTHONUTF8", "1");
        std::env::set_var("PYTHONNOUSERSITE", "1");
    }
}

/// Where the Python home goes inside the app's storage.
fn python_dir(data: &Path) -> PathBuf {
    data.join("python")
}

/// Returns the Python home, unpacking python.tar from the APK first when it is not there
/// yet. The archive starts with a stamp; a matching stamp on disk means nothing to do, so
/// only the first start after an install or update pays for unpacking.
pub fn python_home(app: &AndroidApp) -> io::Result<PathBuf> {
    let data = app
        .internal_data_path()
        .ok_or_else(|| io::Error::other("Android gave no app storage folder"))?;
    let asset = app
        .asset_manager()
        .open(c"python.tar")
        .ok_or_else(|| io::Error::new(ErrorKind::NotFound, "python.tar is missing from the APK"))?;
    unpack(BufReader::new(asset), &data)
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
