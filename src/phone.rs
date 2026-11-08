//! What the phone apps need around yt-dlp, the same code on Android and iOS:
//!
//! - `set_env` points Python, yt-dlp's cache and temporary files into the app's folders.
//! - `link_programs` (Android): ffmpeg and ffprobe ship as libffmpeg.so and libffprobe.so,
//!   the only place Android lets apps run programs from; this links them as ffmpeg and
//!   ffprobe in a folder that goes on PATH, where yt-dlp finds them.
//! - `unpack` (Android): the APK carries Python and yt-dlp as assets/python.tar (made by
//!   scripts/mobile_python.py); the first start after an install or update unpacks it into
//!   the app's storage. iOS needs nothing like it: Python sits in the app bundle.

use std::fs::{self, File};
use std::io::{self, ErrorKind, Read, Write};
use std::path::{Component, Path, PathBuf};

/// Sets the environment Python and yt-dlp read. The PYTHON* values are the ones
/// ytdlp::init would set, so init itself does not touch the environment afterwards.
///
/// Changing the environment is only safe while no other thread reads it, so the apps call
/// this (through api::start_ytdlp) first thing at startup, before starting other work.
pub fn set_env(
    cache: &Path,
    temp: &Path,
    python_home: Option<&Path>,
    python_paths: &[PathBuf],
    bin: Option<&Path>,
) {
    let pycache = cache.join("python");
    for dir in [cache, temp, &pycache] {
        let _ = fs::create_dir_all(dir);
    }
    let path = bin.and_then(|bin| {
        let rest = std::env::var_os("PATH").unwrap_or_default();
        std::env::join_paths(
            [bin.to_path_buf()]
                .into_iter()
                .chain(std::env::split_paths(&rest)),
        )
        .ok()
    });
    // SAFETY: see above; the apps call this before other threads use the environment.
    unsafe {
        std::env::set_var("TMPDIR", temp);
        std::env::set_var("XDG_CACHE_HOME", cache);
        // Python writes its compiled files here: the iOS app bundle is read-only.
        std::env::set_var("PYTHONPYCACHEPREFIX", &pycache);
        if let Some(path) = path {
            std::env::set_var("PATH", path);
        }
        if let Some(home) = python_home {
            std::env::set_var("PYTHONHOME", home);
        }
        if let Ok(paths) = std::env::join_paths(python_paths)
            && !python_paths.is_empty()
        {
            std::env::set_var("PYTHONPATH", paths);
        }
        std::env::set_var("PYTHONUTF8", "1");
        std::env::set_var("PYTHONNOUSERSITE", "1");
    }
}

/// Links `<programs>/libffmpeg.so` and `libffprobe.so` as ffmpeg and ffprobe in
/// `<data>/bin`, and returns that folder, or None when neither program is there.
pub fn link_programs(programs: &Path, data: &Path) -> Option<PathBuf> {
    let bin = data.join("bin");
    fs::create_dir_all(&bin).ok()?;
    let mut linked = false;
    for name in ["ffmpeg", "ffprobe"] {
        let program = programs.join(format!("lib{name}.so"));
        let link = bin.join(name);
        // The programs folder moves with every app update, so always make the link again.
        let _ = fs::remove_file(&link);
        if program.is_file() && std::os::unix::fs::symlink(&program, &link).is_ok() {
            linked = true;
        }
    }
    linked.then_some(bin)
}

/// Unpacks a python.tar stream into `<data>/python`, unless that folder already holds the
/// same version: the archive starts with a stamp, and a matching stamp on disk means
/// nothing to do. Returns the Python home.
/// Where `unpack` puts Python's home inside `data`.
pub fn python_dir(data: &Path) -> PathBuf {
    data.join("python")
}

pub fn unpack(mut tar: impl Read, data: &Path) -> io::Result<PathBuf> {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A ustar archive, like scripts/mobile_python.py writes: entries in order, then the end.
    fn archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut tar = Vec::new();
        for (name, data) in entries {
            let mut header = [0u8; 512];
            header[..name.len()].copy_from_slice(name.as_bytes());
            let size = format!("{:011o}\0", data.len());
            header[124..136].copy_from_slice(size.as_bytes());
            header[156] = b'0';
            tar.extend_from_slice(&header);
            tar.extend_from_slice(data);
            tar.resize(tar.len().next_multiple_of(512), 0);
        }
        tar.extend_from_slice(&[0u8; 1024]);
        tar
    }

    fn folder(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("elsewhere-phone-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn unpacks_once_per_stamp() {
        let data = folder("stamp");
        let v1 = archive(&[
            ("stamp", b"v1"),
            ("lib/python3.15/os.py", b"# os"),
            ("lib/a/b/c.txt", b"deep"),
        ]);
        let home = unpack(v1.as_slice(), &data).unwrap();
        assert_eq!(home, data.join("python"));
        assert_eq!(
            fs::read(home.join("lib/python3.15/os.py")).unwrap(),
            b"# os"
        );
        assert_eq!(fs::read(home.join("lib/a/b/c.txt")).unwrap(), b"deep");

        // Same stamp: nothing is read past the stamp, so even a broken rest is fine.
        let same = archive(&[("stamp", b"v1")]);
        unpack(&same[..1024], &data).unwrap();

        // New stamp: the old files go, the new ones come.
        let v2 = archive(&[("stamp", b"v2"), ("lib/new.py", b"new")]);
        unpack(v2.as_slice(), &data).unwrap();
        assert!(!home.join("lib/python3.15/os.py").exists());
        assert_eq!(fs::read(home.join("lib/new.py")).unwrap(), b"new");
        assert_eq!(fs::read(home.join("stamp")).unwrap(), b"v2");
    }

    #[test]
    fn a_cut_short_archive_keeps_the_old_home() {
        let data = folder("short");
        unpack(
            archive(&[("stamp", b"v1"), ("x.py", b"x")]).as_slice(),
            &data,
        )
        .unwrap();
        let v2 = archive(&[("stamp", b"v2"), ("y.py", &[7u8; 5000])]);
        let err = unpack(&v2[..2048], &data).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::UnexpectedEof);
        assert_eq!(fs::read(data.join("python/stamp")).unwrap(), b"v1");
        assert!(data.join("python/x.py").exists());
    }

    #[test]
    fn rejects_paths_outside_the_home_and_a_missing_stamp() {
        let data = folder("unsafe");
        let evil = archive(&[("stamp", b"v1"), ("../evil.py", b"x")]);
        assert_eq!(
            unpack(evil.as_slice(), &data).unwrap_err().kind(),
            ErrorKind::InvalidData
        );
        assert!(!data.join("evil.py").exists());
        let no_stamp = archive(&[("x.py", b"x")]);
        assert_eq!(
            unpack(no_stamp.as_slice(), &data).unwrap_err().kind(),
            ErrorKind::InvalidData
        );
    }

    #[test]
    fn links_programs_when_they_exist() {
        let data = folder("programs");
        let programs = data.join("lib");
        fs::create_dir_all(&programs).unwrap();
        assert_eq!(link_programs(&programs, &data), None);
        fs::write(programs.join("libffmpeg.so"), b"").unwrap();
        let bin = link_programs(&programs, &data).unwrap();
        assert_eq!(
            fs::read_link(bin.join("ffmpeg")).unwrap(),
            programs.join("libffmpeg.so")
        );
        assert!(!bin.join("ffprobe").exists());
    }
}
