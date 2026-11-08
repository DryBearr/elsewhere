# elsewhere

A Rust core with a native app on each phone:

```
src/                Rust core, shared by both apps
  api.rs            everything the apps can call (UniFFI makes the Kotlin and Swift bindings)
  ytdlp/            yt-dlp through embedded Python (feature `ytdlp`)
  playlists/        save YouTube / YouTube Music playlists into the library (feature `playlists`)
  jobs/             runs long work in the background and reports exact status to the app
  db.rs             the library database (SQLite)
  phone.rs          what phones need around yt-dlp: unpack Python, link ffmpeg, environment
android/            Kotlin + Jetpack Compose app (Gradle)
ios/                Swift + SwiftUI app (XcodeGen)
scripts/            build the parts the apps bundle
```

Plugging a feature in or out: a module in `src/` (with a feature flag when it carries weight), its section in `api.rs`, and one screen file per app (`android/.../XScreen.kt`, `ios/Elsewhere/XView.swift`), listed with one line in `MainActivity.kt` and `ElsewhereApp.swift`.

## yt-dlp module

`src/ytdlp` wraps yt-dlp as plain Rust functions (`init`, `version`, `info`, `download`) through embedded Python. It sits behind the `ytdlp` Cargo feature, so the core builds without Python when the feature is off.

Desktop (the Rust core and its tests):

```sh
python3 -m pip install --target vendor/python -r scripts/requirements.txt   # yt-dlp for development and tests
cargo test --features playlists              # offline tests against a local test server
cargo test --features playlists -- --ignored # also tests against real websites
```

`scripts/requirements.txt` pins yt-dlp. Bump it there to update yt-dlp everywhere. PyO3 links against Homebrew's Python on desktop (see `.cargo/config.toml`); set `PYO3_PYTHON` to use another Python that ships `libpython3.x.so`.

Phones: both carry their own Python (the official CPython 3.15 builds from python.org) with yt-dlp in its site-packages, prepared by `scripts/mobile_python.py`.

- Android: the APK holds the standard library and yt-dlp as `assets/python.tar`; the app hands it to the core, which unpacks it on the first start after each install or update. It also carries a small ffmpeg and ffprobe (`scripts/android_ffmpeg.sh`, copy-only, LGPL) so yt-dlp can merge YouTube's separate video and audio. Needs the Android NDK, a JDK 17 and Gradle:

  ```sh
  python3 scripts/mobile_python.py android
  scripts/android_ffmpeg.sh
  scripts/android_rust.sh
  gradle -p android assembleRelease
  ```

- iOS: the Xcode build copies Python into `Elsewhere.app/python` and turns each C module into a framework, as iOS requires. iOS apps cannot run other programs, so there is no ffmpeg; yt-dlp picks formats that need no merging. Needs a Mac:

  ```sh
  python3 scripts/mobile_python.py ios
  scripts/ios_rust.sh
  cd ios && xcodegen
  ```

## Jobs module

`jobs` runs long work (playlist saves now; scans, NAS sync, conversions later) and tells the app what is happening:

- **State:** every change updates the job's `JobState` (status, item i of n, bytes, speed, ETA, last error) and bumps its `revision`. The listener gets the whole state of that job and the app just replaces what it shows. One delivery thread calls the listener, so states arrive in order and work never waits for the app. Byte progress is sent at most 4 times a second; everything else at once.
- **Log:** info, warnings and errors are numbered and never dropped; the app reads new lines with `logs(job, since)`.
- **End:** every job ends exactly once (Done, Failed or Cancelled); a panicking job fails instead of crashing the app, and a panicking listener gets the state again.
- At most 2 jobs run at a time; `cancel` stops queued jobs at once and running ones through their cancel hooks.

- **Sudden close:** `Jobs::open(db)` keeps jobs in the library database (migration 0003). The delivery thread saves each change and new log line in one transaction before it tells the app. On the next start, finished jobs come back as history (the newest 200) and unfinished ones as **Interrupted**: the app offers `resume(id)` (runs it again from the start with the same params) or `discard(id)` (ends it as Cancelled). Failed jobs can be resumed too, as a retry.

A feature plugs in with one line, `jobs.register(kind, runner)`, then starts jobs with `jobs.start(kind, title, params)`. Params are text the runner understands (playlists use `playlists::job_params(link, options)` and `playlists::run_job`). A runner must be safe to run again with the same params, because resuming does exactly that; playlists skip saved items and yt-dlp continues partial files.

## Playlists module

`playlists::save(db, media, link, options, on_progress, cancel)` saves a YouTube or YouTube Music playlist (or one video) into the library, as m4a audio (Music tab) or mp4 video (Videos tab) up to a chosen height (`Format::Video { max_height: Some(720) }`; `None` for the best):

- one folder per playlist, `<media>/<Music|Videos>/<title> [<id>]/`, with the banner as `cover.jpg` and, per item, the media file, its image and its lyrics (`.lrc` synced, `.txt` plain, from lrclib.net when `lyrics` is on);
- in the database: one `item` per file (title, artist tags, year, length, description), and the playlist as a `collection` with its cover and order;
- saving the same link again downloads only the new items; a failing item is reported and the rest go on; `limit` saves only the first N.

YouTube Music links give proper song fields. For plain YouTube music videos the channel is taken as the artist and the title is cleaned to the song name ("Song (Official Video) - Artist" becomes "Song").

YouTube: yt-dlp has to solve JavaScript challenges. A QuickJS engine is compiled into the core (`third_party/quickjs`, `src/ytdlp/js.rs`), so this works on every platform without Deno or Node.

The Mobile workflow runs `tests/ytdlp.rs` on an Android emulator and an iOS simulator with that same Python, and starts the real app on both to check that yt-dlp starts.

## Releases

Push a tag that matches `version` in `Cargo.toml` (for example `v0.3.0`); both apps take their version from there. The Release workflow builds `Elsewhere.apk` and an unsigned `Elsewhere.ipa`, attaches both to a GitHub release, and the Pages workflow refreshes the download page.

To try a branch on a phone without releasing, run the Release workflow by hand (Actions → Release → Run workflow, or `gh workflow run release.yml --ref <branch>`). It builds both files and keeps them as workflow artifacts, without publishing anything. The APK is signed with the release key, so it installs over the Obtainium copy and keeps its data.

- Download page: https://drybearr.github.io/elsewhere/
- Android: add `https://github.com/DryBearr/elsewhere` to [Obtainium](https://github.com/ImranR98/Obtainium).
- iOS: add the source `https://drybearr.github.io/elsewhere/source.json` to SideStore or AltStore. They sign the app on the device.

One-time setup in the GitHub repo settings:

- Settings → Pages → Source: **GitHub Actions**.
- Settings → Secrets and variables → Actions: `ANDROID_KEYSTORE_BASE64` and `ANDROID_KEYSTORE_PASSWORD`. Android only installs updates signed with the same key, so never replace it.
