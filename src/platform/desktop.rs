//! Desktop (development): the Python the app links against, with yt-dlp from vendor/python
//! when that folder exists (see the README).

use std::path::PathBuf;

use crate::ytdlp::Setup;

/// Nothing to set: the desktop Python finds its own home.
pub fn set_env() {}

pub fn python_setup() -> Result<Setup, String> {
    let vendor = PathBuf::from("vendor/python");
    Ok(Setup {
        python_home: None,
        python_paths: if vendor.is_dir() {
            vec![vendor]
        } else {
            Vec::new()
        },
    })
}
