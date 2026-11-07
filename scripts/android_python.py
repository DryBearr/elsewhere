"""Prepares Python and yt-dlp for the Android build. Run it before `cargo apk build`:

    python3 scripts/android_python.py
    PYO3_CROSS_LIB_DIR=$PWD/target/android/prefix/lib cargo apk build --release --lib --features ytdlp

It writes, under target/android/:
  prefix/              the official Python build for Android; PyO3 links against prefix/lib
  libs/arm64-v8a/      libpython and the libraries its modules load; cargo-apk adds them
                       to the APK (runtime_libs in Cargo.toml)
  assets/python.tar    the standard library and the packages in requirements.txt; the app
                       unpacks it into its own storage on first start (src/android.rs)

The first entry of python.tar is `stamp`, a hash of everything after it. The app
unpacks the archive again only when the stamp changes.
"""

import hashlib
import io
import shutil
import subprocess
import sys
import tarfile
import urllib.request
from pathlib import Path

PYTHON_VERSION = '3.14.10'
PYTHON_SHA256 = '423c80b957330cf0ebb92b9b20f2294c292144a441bdb0ceb8c69d14928e569b'
TRIPLET = 'aarch64-linux-android'
ABI = 'arm64-v8a'

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / 'target' / 'android'
REQUIREMENTS = ROOT / 'scripts' / 'requirements.txt'
SHORT_VERSION = '.'.join(PYTHON_VERSION.split('.')[:2])

# Parts of the standard library the app never uses: tests, GUI and help tools,
# the pip installer, and the build configuration.
SKIP_STDLIB = {'test', 'idlelib', 'tkinter', 'turtledemo', 'turtle.py', 'ensurepip', 'pydoc_data'}
# C modules that only exist for CPython's own tests.
SKIP_MODULES = ('_test', '_ctypes_test', '_xxtestfuzz', 'xx')


def main():
    tarball = download()
    prefix = unpack(tarball)
    copy_libs(prefix)
    site = install_packages()
    write_archive(prefix, site)


def download():
    OUT.mkdir(parents=True, exist_ok=True)
    name = f'python-{PYTHON_VERSION}-{TRIPLET}.tar.gz'
    path = OUT / name
    if not path.exists() or sha256(path) != PYTHON_SHA256:
        url = f'https://www.python.org/ftp/python/{PYTHON_VERSION}/{name}'
        print(f'Downloading {url}')
        part = OUT / (name + '.part')
        with urllib.request.urlopen(url, timeout=60) as response, open(part, 'wb') as file:
            shutil.copyfileobj(response, file)
        part.replace(path)
    if sha256(path) != PYTHON_SHA256:
        sys.exit(f'{path} does not have the expected SHA-256 {PYTHON_SHA256}')
    return path


def unpack(tarball):
    prefix = OUT / 'prefix'
    shutil.rmtree(prefix, ignore_errors=True)
    with tarfile.open(tarball) as archive:
        members = [
            member for member in archive.getmembers()
            if member.name.removeprefix('./').startswith('prefix/')
        ]
        archive.extractall(OUT, members=members, filter='data')
    return prefix


def copy_libs(prefix):
    libs = OUT / 'libs' / ABI
    shutil.rmtree(libs.parent, ignore_errors=True)
    libs.mkdir(parents=True)
    lib = prefix / 'lib'
    names = [f'libpython{SHORT_VERSION}.so'] + sorted(path.name for path in lib.glob('lib*_python.so'))
    for name in names:
        shutil.copyfile(lib / name, libs / name)
    print(f'Native libraries: {", ".join(names)}')


def install_packages():
    site = OUT / 'site-packages'
    shutil.rmtree(site, ignore_errors=True)
    # Pure-Python wheels only, so the host Python can install them for the phone.
    subprocess.run(
        [sys.executable, '-m', 'pip', 'install', '--quiet', '--disable-pip-version-check',
         '--no-deps', '--no-compile', '--only-binary=:all:',
         '--target', str(site), '--requirement', str(REQUIREMENTS)],
        check=True,
    )
    return site


def write_archive(prefix, site):
    stdlib = prefix / 'lib' / f'python{SHORT_VERSION}'
    base = f'lib/python{SHORT_VERSION}'
    files = []
    for path in sorted(stdlib.rglob('*')):
        relative = path.relative_to(stdlib)
        if path.is_file() and not skip_stdlib(relative):
            files.append((f'{base}/{relative.as_posix()}', path))
    for path in sorted(site.rglob('*')):
        relative = path.relative_to(site)
        # bin/ and share/ hold the yt-dlp command and man pages, which the app does not use.
        if path.is_file() and relative.parts[0] not in ('bin', 'share') and '__pycache__' not in relative.parts:
            files.append((f'{base}/site-packages/{relative.as_posix()}', path))

    digest = hashlib.sha256()
    for name, path in files:
        data = path.read_bytes()
        digest.update(f'{name}\0{len(data)}\0'.encode())
        digest.update(data)
    stamp = f'python {PYTHON_VERSION} {digest.hexdigest()}\n'.encode()

    assets = OUT / 'assets'
    shutil.rmtree(assets, ignore_errors=True)
    assets.mkdir(parents=True)
    archive_path = assets / 'python.tar'
    # ustar keeps the reader in src/android.rs simple: no long-name extensions.
    with tarfile.open(archive_path, 'w', format=tarfile.USTAR_FORMAT) as archive:
        add(archive, 'stamp', stamp)
        for name, path in files:
            add(archive, name, path.read_bytes())
    size = archive_path.stat().st_size / 1e6
    print(f'{archive_path.relative_to(ROOT)}: {len(files)} files, {size:.1f} MB')


def skip_stdlib(relative):
    if '__pycache__' in relative.parts or relative.parts[0] in SKIP_STDLIB:
        return True
    if relative.parts[0].startswith('config-'):
        return True
    return relative.parts[0] == 'lib-dynload' and relative.name.startswith(SKIP_MODULES)


def add(archive, name, data):
    info = tarfile.TarInfo(name)
    info.size = len(data)
    info.mode = 0o644
    info.mtime = 0
    archive.addfile(info, io.BytesIO(data))


def sha256(path):
    with open(path, 'rb') as file:
        return hashlib.file_digest(file, 'sha256').hexdigest()


if __name__ == '__main__':
    main()
