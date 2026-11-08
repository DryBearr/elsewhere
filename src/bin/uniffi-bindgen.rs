//! Writes the Kotlin and Swift bindings for src/api.rs (used by scripts/android_rust.sh and
//! scripts/ios_rust.sh). Build machines only: `--features bindgen`.
fn main() {
    uniffi::uniffi_bindgen_main()
}
