"""Prepares Python and yt-dlp for the phone builds. Both phones get the official CPython
build from python.org, the same trimmed standard library, and the same packages from
scripts/requirements.txt.

    python3 scripts/mobile_python.py android           # before `cargo apk build`
    python3 scripts/mobile_python.py android x86_64    # for the Android emulator tests
    python3 scripts/mobile_python.py ios               # before xcodegen / xcodebuild

Android, under target/android/:
  prefix/lib           the Python build; PyO3 links against it (PYO3_CROSS_LIB_DIR)
  libs/<abi>/          libpython and the libraries its modules load; cargo-apk adds them
                       to the APK (runtime_libs in Cargo.toml)
  assets/python.tar    the standard library and the packages; the app unpacks it into its
                       storage on first start (src/platform/android.rs). The first entry,
                       `stamp`, is a hash of the rest, so the app unpacks again only when
                       it changes.

iOS, under target/ios/:
  Python.xcframework   the Python build, with the standard library trimmed and the packages
                       added to its site-packages. The Xcode build links it and copies it
                       into the app (ios/project.yml).
"""

import hashlib
import io
import shutil
import subprocess
import sys
import tarfile
import urllib.request
from pathlib import Path

PYTHON_VERSION = '3.15.2'
SHORT_VERSION = '3.15'
SHA256 = {
    'python-3.15.2-aarch64-linux-android.tar.gz': 'cfd4f0fdaff465ba62ebf3c3df2c8c410d0802fef3f98897082460df704cfef6',
    'python-3.15.2-x86_64-linux-android.tar.gz': 'e5b46c72295e558991bbe98415ac29bc1448aca35a8e660ddfe017e20305cbf8',
    'python-3.15.2-iOS-XCframework.tar.gz': '0f9fc7f61ceb8278a69fc7ec6089aad81789a96fa42f31cfe6d2f5f860d20a62',
}
ANDROID_ABIS = {'aarch64': 'arm64-v8a', 'x86_64': 'x86_64'}

ROOT = Path(__file__).resolve().parent.parent
TARGET = ROOT / 'target'
REQUIREMENTS = ROOT / 'scripts' / 'requirements.txt'

# Parts of the standard library the app never uses: tests, GUI and help tools, the pip
# installer, and the build configuration.
SKIP_STDLIB = {'test', 'idlelib', 'tkinter', 'turtledemo', 'turtle.py', 'ensurepip', 'pydoc_data'}
# C modules that only exist for CPython's own tests.
SKIP_MODULES = ('_test', '_ctypes_test', '_xxtestfuzz', 'xx')


def main():
    args = sys.argv[1:]
    if args[:1] == ['android'] and len(args) <= 2 and (args[1:] or ['aarch64'])[0] in ANDROID_ABIS:
        android((args[1:] or ['aarch64'])[0])
    elif args == ['ios']:
        ios()
    else:
        sys.exit(__doc__)


def android(arch):
    out = TARGET / 'android'
    shutil.rmtree(out, ignore_errors=True)
    tarball = download(f'python-{PYTHON_VERSION}-{arch}-linux-android.tar.gz')
    unpack(tarball, 'prefix/', out)
    prefix = out / 'prefix'
    stdlib = prefix / 'lib' / f'python{SHORT_VERSION}'
    trim(stdlib)

    libs = out / 'libs' / ANDROID_ABIS[arch]
    libs.mkdir(parents=True)
    lib = prefix / 'lib'
    names = [f'libpython{SHORT_VERSION}.so'] + sorted(path.name for path in lib.glob('lib*_python.so'))
    for name in names:
        shutil.copyfile(lib / name, libs / name)
    print(f'Native libraries: {", ".join(names)}')

    site = out / 'site-packages'
    install_packages(site)
    write_archive(stdlib, site, out / 'assets' / 'python.tar')


def ios():
    out = TARGET / 'ios'
    shutil.rmtree(out, ignore_errors=True)
    tarball = download(f'python-{PYTHON_VERSION}-iOS-XCframework.tar.gz')
    unpack(tarball, 'Python.xcframework/', out)
    framework = out / 'Python.xcframework'
    # The pure-Python part is shared by all slices; each slice has its own C modules.
    trim(framework / 'lib' / f'python{SHORT_VERSION}')
    for slice_lib in sorted(framework.glob(f'ios-*/lib-*/python{SHORT_VERSION}')):
        trim(slice_lib)
    install_packages(framework / 'lib' / f'python{SHORT_VERSION}' / 'site-packages')
    print(f'{framework.relative_to(ROOT)}: {size_of(framework):.1f} MB')


def download(name):
    folder = TARGET / 'python-downloads'
    folder.mkdir(parents=True, exist_ok=True)
    path = folder / name
    if not path.exists() or sha256(path) != SHA256[name]:
        url = f'https://www.python.org/ftp/python/{PYTHON_VERSION}/{name}'
        print(f'Downloading {url}')
        part = folder / (name + '.part')
        with urllib.request.urlopen(url, timeout=60) as response, open(part, 'wb') as file:
            shutil.copyfileobj(response, file)
        part.replace(path)
    if sha256(path) != SHA256[name]:
        sys.exit(f'{path} does not have the expected SHA-256 {SHA256[name]}')
    return path


def unpack(tarball, top, out):
    """Extracts the members under `top` (for example 'prefix/') into `out`."""
    with tarfile.open(tarball) as archive:
        members = [
            member for member in archive.getmembers()
            if member.name.removeprefix('./').startswith(top)
        ]
        archive.extractall(out, members=members, filter='data')


def trim(stdlib):
    """Deletes the parts of a standard library folder the app never uses."""
    for path in sorted(stdlib.rglob('__pycache__'), reverse=True):
        shutil.rmtree(path)
    for path in list(stdlib.iterdir()):
        if path.name in SKIP_STDLIB or path.name.startswith('config-'):
            remove(path)
    dynload = stdlib / 'lib-dynload'
    if dynload.is_dir():
        for path in list(dynload.iterdir()):
            if path.name.startswith(SKIP_MODULES):
                remove(path)


def install_packages(site):
    # Pure-Python wheels only, so the build machine's Python can install them for a phone.
    subprocess.run(
        [sys.executable, '-m', 'pip', 'install', '--quiet', '--disable-pip-version-check',
         '--no-deps', '--no-compile', '--only-binary=:all:', '--upgrade',
         '--target', str(site), '--requirement', str(REQUIREMENTS)],
        check=True,
    )
    # The yt-dlp command and its man pages; the app only imports the package.
    for name in ('bin', 'share'):
        shutil.rmtree(site / name, ignore_errors=True)


def write_archive(stdlib, site, archive_path):
    base = f'lib/python{SHORT_VERSION}'
    files = [(f'{base}/{path.relative_to(stdlib).as_posix()}', path)
             for path in sorted(stdlib.rglob('*')) if path.is_file()]
    files += [(f'{base}/site-packages/{path.relative_to(site).as_posix()}', path)
              for path in sorted(site.rglob('*')) if path.is_file()]

    digest = hashlib.sha256()
    for name, path in files:
        data = path.read_bytes()
        digest.update(f'{name}\0{len(data)}\0'.encode())
        digest.update(data)
    stamp = f'python {PYTHON_VERSION} {digest.hexdigest()}\n'.encode()

    archive_path.parent.mkdir(parents=True, exist_ok=True)
    # ustar keeps the reader in src/platform/android.rs simple: no long-name extensions.
    with tarfile.open(archive_path, 'w', format=tarfile.USTAR_FORMAT) as archive:
        add(archive, 'stamp', stamp)
        for name, path in files:
            add(archive, name, path.read_bytes())
    size = archive_path.stat().st_size / 1e6
    print(f'{archive_path.relative_to(ROOT)}: {len(files)} files, {size:.1f} MB')


def add(archive, name, data):
    info = tarfile.TarInfo(name)
    info.size = len(data)
    info.mode = 0o644
    info.mtime = 0
    archive.addfile(info, io.BytesIO(data))


def remove(path):
    if path.is_dir() and not path.is_symlink():
        shutil.rmtree(path)
    else:
        path.unlink()


def size_of(folder):
    return sum(path.stat().st_size for path in folder.rglob('*') if path.is_file() and not path.is_symlink()) / 1e6


def sha256(path):
    with open(path, 'rb') as file:
        return hashlib.file_digest(file, 'sha256').hexdigest()


if __name__ == '__main__':
    main()
