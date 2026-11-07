# elsewhere

## yt-dlp module

`src/ytdlp` wraps yt-dlp as plain Rust functions (`init`, `version`, `info`, `download`) through embedded Python. It sits behind the `ytdlp` Cargo feature, so the app builds without Python when the feature is off.

Desktop setup:

```sh
python3 -m pip install --target vendor/python yt-dlp   # yt-dlp for development and tests
cargo test --features ytdlp                             # offline tests against a local test server
cargo test --features ytdlp -- --ignored                # also tests against real websites
```

PyO3 links against Homebrew's Python (see `.cargo/config.toml`); set `PYO3_PYTHON` to use another Python that ships `libpython3.x.so`.
