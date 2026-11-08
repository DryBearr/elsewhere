# elsewhere

A Rust core with a native app on each phone:

```
src/                Rust core, shared by both apps
  api/              everything the apps can call (UniFFI makes the Kotlin and Swift bindings)
  ytdlp/            yt-dlp through embedded Python (feature `ytdlp`)
  playlists/        save YouTube / YouTube Music playlists into the library (feature `playlists`)
  jobs/             runs long work in the background and reports exact status to the app
  player/           the media engine: plays local files through libmpv (feature `player`)
  library/          the media library: tabs, playlists, items and their files (read side)
  problem.rs        what a failure means for people: kind, text, detail, retry
  health.rs         supervised parts that restart themselves; says when the app must restart
  logs.rs           the core's log, on the phone only
  db.rs             the library database (SQLite)
  phone.rs          what phones need around yt-dlp: unpack Python, link ffmpeg, environment
android/            Kotlin + Jetpack Compose app (Gradle)
ios/                Swift + SwiftUI app (XcodeGen)
scripts/            build the parts the apps bundle
```

How the apps use the core:

- **Start:** `start(folders, python_bundle)` runs first at app start (`ElsewhereApplication` / `ElsewhereApp.init`). It is cheap; Python and yt-dlp start on the first job or call that needs them (Android hands over `python.tar` through `PythonBundle` then). `core_status()` says what is loaded.
- **Jobs:** one `JobsStore` per app (`JobsStore.kt`, `JobsStore.swift`) is the job listener. It keeps the newest revision of each job and every screen reads it from there; calls into the core run off the UI thread. Screens: Downloads (job cards with Cancel / Resume / Discard / Retry / Remove, and each job's log), Add (save a playlist, run a demo job), Library.
- **Errors:** every failing call throws one `Problem` (Kotlin `ApiException.Failed`, Swift `ApiError.Failed`), and a job's `lastError` is the same `Problem`: `kind`, `text` for the screen, `detail` for the log and `retry` for a "Try again" button.
- **Restart:** `core_health()` says whether a part of the core broke for good. Android then restarts itself (`Health.kt`: checked every 30 s and after a job fails with a bug; at most once per 10 minutes). iOS cannot restart itself; `Core.needsRestart` tells the app to ask for it.
- **Self-test:** debug builds log every job state the store receives; `scripts/selftest_android.sh` and `scripts/selftest_ios.sh` kill a demo job halfway, check it comes back Interrupted, resume it to Done (store equal to core), discard another, and start yt-dlp lazily, on the emulator and simulator. On Android it also breaks the core on purpose and checks that the app comes back as a new process.

Plugging a feature in or out: a module in `src/` (with a feature flag when it carries weight), its file in `src/api/`, and one screen file per app (`android/.../XScreen.kt`, `ios/Elsewhere/XView.swift`), listed with one line in `MainActivity.kt` and `ElsewhereApp.swift`.

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

## Errors and recovery

Nothing leaves the phone: no crash reports, no telemetry. The core heals what it can by itself and keeps a log on the device.

- **One error model:** modules keep their own errors; at the edges (API, job, item) they become a `Problem` through `problem.rs`, so the same failure looks and behaves the same everywhere. Kinds: Input, Unavailable, Network, Storage, Setup, Cancelled, Bug.
- **Retries:** only Network problems are tried again by themselves, 3 more times after 2, 10 and 30 s (`problem::with_retries`). Playlists retry the listing and each item; the job log shows each new try.
- **Supervised parts:** the jobs delivery thread and the watchdog run under `health::supervise`. A crashed part restarts after 1, 5, 30 s; 5 crashes within 10 minutes make it Broken. After a restart, delivery sends and saves everything again, so nothing is lost.
- **Database:** a failed save reopens the connection and tries again next round; 5 failures in a row make the store Broken. The app keeps seeing job states meanwhile.
- **Watchdog:** a running job with no sign of life for 10 minutes is interrupted (playlists then try that item again); still nothing after 60 s, and the job fails as a Bug and the core asks for a restart. yt-dlp's chatter counts as life (`Job::keep_alive`). JavaScript challenges stop after 120 s.
- **Log:** `<data>/logs/elsewhere.log`, one JSON object per line (time, level, area, job, item, message, detail). It rotates at 512 KB into `elsewhere.1.log`, so it stays under about 1 MB. Every panic writes `crash-<time>.txt` with its backtrace. There is no log screen yet.
- **Restart:** the database is the truth and the app's start is the recovery path, so restarting (Android does it by itself) is always safe.

Tests break things on purpose: delivery crashes, failing saves, a stuck and a stalled job (`src/jobs/tests.rs`), a server that fails twice or always (`tests/playlists.rs`), log rotation and crash notes (`tests/recovery.rs`). The Mobile workflow runs them on the emulator and simulator too.

## Library module

`library` is the media library, built in steps (done: reading, and what the person marks; next: browsing, editing, change notices, search, folder scans). The apps get `categories()`, `collections(tab)`, `collection(id)`, `collection_items(id, after, limit)`, `item(id)` and `item_file(id)`, and the marks below.

- **Ids, never paths:** the apps hold item ids; `item_file` is the only way to a file, and it checks that the file exists, is a regular file and lies inside its library folder and the media folder (links resolved).
- **Bad data never fails a list:** a deleted file or cover, missing fields or odd values in `extra` leave the row in place, marked `missing` or without a cover.
- **Pages:** playlists come 100 entries at a time (at most 500), ordered by position and entry id, with an opaque cursor: rows added or removed meanwhile never repeat or vanish among the rest. Each row carries its progress (position, percent, done, plays).
- **One connection, healed:** reused for every call, reopened once when it breaks.
- **What the person marks (step 3):** `start_play(item)` says where to start and begins a play; `save_progress(item, position, duration)` keeps the resume point; `set_watched`, `set_favorite`, `set_rating`. The rules: songs (audio under 20 minutes) and clips (video under 5 minutes) always start over; longer things resume, but only from 30 s on; done at 95 %, or in the last 2 minutes of items 10 minutes and longer, and a done item starts over; a play counts once, however often the end is reached by seeking; the player's length corrects the stored one; a save for an item that is gone does nothing.
- **Moved folders:** iOS gives an updated app a new container path (and Android can move an app to other storage). A stored path that is gone but runs through a folder named like the media folder is found again inside today's media folder.

## Player module

`player` is the media engine that music, movies, TV shows and the rest will build on. It plays one local file per `Player` (several can play at once) and knows nothing about the library, queues or resume points.

- **libmpv underneath,** loaded when the first player is made (no build dependency; `ELSEWHERE_LIBMPV` points at it on a desktop). It plays mp4, webm, mkv, m4a, opus and more, with hardware decoding where the phone has it.
- **Local files only:** a path must be absolute, a regular file and, after resolving links, inside the folders `allow_folders` named (the app's media folder). mpv runs with no config files, no scripts, no yt-dlp, no following references inside files.
- **State like jobs:** `open(path, start_ms, autoplay)`, `play`, `pause`, `seek`, `set_speed` (0.25 to 4), `set_volume`, `stop`; every change bumps `revision` and the listener gets the whole `PlayerState` (Idle, Loading, Playing, Paused, Ended, Failed with a `Problem`; position at most 4 times a second).
- **Robust:** events of a replaced file are ignored, a file that does not open in 20 s fails, a broken file fails with a reason and the next one plays, the event thread runs supervised, and a panicking listener changes nothing.
- The apps get it as `CorePlayer`. Video output and the lock screen come next, from the apps' side; until then video tracks are off, and decoding is in software (hardware decoders crashed on Android when players were dropped while starting; they come back with the video surfaces).
- **libmpv on the phones is built here,** from the sources pinned in `scripts/mpv_sources.sh`: mpv 0.41 (LGPL), FFmpeg 9.0.2 without network code, dav1d, libass with FreeType, FriBidi and HarfBuzz, libplacebo. `scripts/android_mpv.sh` makes `libmpv.so` per ABI (about 15 MB, 4.5 MB compressed); `scripts/ios_mpv.sh` makes `mpv.xcframework`, a dynamic framework the app embeds (about 12 MB). Both export only `mpv_*` and need nothing beyond the system. Sound goes out through AAudio or OpenSL ES on Android and AudioUnit on iOS.

Tests: `src/player/machine.rs` covers the rules without libmpv; `tests/player.rs` plays the small files in `tests/media` through the real libmpv with null outputs (every format to the end, seeking, speed, refused paths, broken and cut-off files, replacing files, command storms from 4 threads, opens from 5 threads, 40 open/stop rounds, 8 players at once, quick drops, no leaked threads). The Mobile workflow runs them 4 times on the emulator and in the simulator, and the apps' self-tests play an audio and a video file through the real sound output in real time.

## Playlists module

`playlists::save(db, media, link, options, on_progress, cancel)` saves a YouTube or YouTube Music playlist (or one video) into the library, as m4a audio (Music tab) or mp4 video (Videos tab) up to a chosen height (`Format::Video { max_height: Some(720) }`; `None` for the best):

- one folder per playlist, `<media>/<Music|Videos>/<title> [<id>]/`, with the banner as `cover.jpg` and, per item, the media file, its image and its lyrics (`.lrc` synced, `.txt` plain, from lrclib.net when `lyrics` is on);
- in the database: one `item` per file (title, artist tags, year, length, description), and the playlist as a `collection` with its cover and order;
- saving the same link again downloads only the new items; a failing item is reported and the rest go on; `limit` saves only the first N.
- entries that cannot be saved are known while the playlist is read, and skipped without trying: removed or private videos (`[Deleted video]`, `[Private video]`), members-only and Premium-only ones, premieres not out yet, streams still live. The job says how many and which;
- a song that left the playlist leaves the saved playlist too, but its file and library entry stay (the job says so); a song still listed but gone from the site keeps the copy saved before;
- a limit counts only entries that can be saved;
- `preview_playlist(link)` reads what a link holds before saving: how many entries can be saved (the most a limit can be), which cannot and why, and whether YouTube made the playlist up (a Mix, which can go on and on: a limit of 50 is suggested); it reads at most 1000 entries;
- a link with nothing that can be saved changes nothing.

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
