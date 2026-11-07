fn main() {
    elsewhere::run(db_path());
}

/// Where the database lives. iOS apps may only write inside their own container,
/// which `HOME` points to. On desktop it stays in the current folder for now.
fn db_path() -> std::path::PathBuf {
    if cfg!(target_os = "ios") {
        let home = std::env::var_os("HOME").unwrap_or_default();
        let dir = std::path::PathBuf::from(home).join("Library/Application Support");
        let _ = std::fs::create_dir_all(&dir);
        dir.join("elsewhere.db")
    } else {
        "elsewhere.db".into()
    }
}
