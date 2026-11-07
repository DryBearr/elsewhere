# elsewhere

## yt-dlp module

`src/ytdlp` wraps yt-dlp as plain Rust functions (`init`, `version`, `info`, `download`) through embedded Python. It sits behind the `ytdlp` Cargo feature, so the app builds without Python when the feature is off.

Desktop setup:

```sh
python3 -m pip install --target vendor/python -r scripts/requirements.txt   # yt-dlp for development and tests
cargo test --features ytdlp                  # offline tests against a local test server
cargo test --features ytdlp -- --ignored     # also tests against real websites
cargo run --features ytdlp                   # the app, with the "Try yt-dlp" box working
```

`scripts/requirements.txt` pins yt-dlp. Bump it there to update yt-dlp everywhere.

Android: the APK carries its own Python (the official CPython build for Android) and yt-dlp. `scripts/android_python.py` downloads and prepares them under `target/android/`, and the app unpacks the standard library and yt-dlp into its private storage on the first start after each install or update (`src/android.rs`). The Release workflow does this; by hand it is:

```sh
python3 scripts/android_python.py
PYO3_CROSS_LIB_DIR=$PWD/target/android/prefix/lib cargo apk build --release --lib --features ytdlp
```

iOS builds do not include yt-dlp yet.

PyO3 links against Homebrew's Python (see `.cargo/config.toml`); set `PYO3_PYTHON` to use another Python that ships `libpython3.x.so`.

## Releases

Push a tag that matches `version` in `Cargo.toml` (for example `v0.1.0`). The Release workflow builds `Elsewhere.apk` and an unsigned `Elsewhere.ipa`, attaches both to a GitHub release, and the Pages workflow refreshes the download page.

To try a branch on a phone without releasing, run the Release workflow by hand (Actions → Release → Run workflow, or `gh workflow run release.yml --ref <branch>`). It builds both files and keeps them as workflow artifacts, without publishing anything. The APK is signed with the release key, so it installs over the Obtainium copy and keeps its data.

- Download page: https://drybearr.github.io/elsewhere/
- Android: add `https://github.com/DryBearr/elsewhere` to [Obtainium](https://github.com/ImranR98/Obtainium).
- iOS: add the source `https://drybearr.github.io/elsewhere/source.json` to SideStore or AltStore. They sign the app on the device.

One-time setup in the GitHub repo settings:

- Settings → Pages → Source: **GitHub Actions**.
- Settings → Secrets and variables → Actions: `ANDROID_KEYSTORE_BASE64` and `ANDROID_KEYSTORE_PASSWORD`. Android only installs updates signed with the same key, so never replace it.
