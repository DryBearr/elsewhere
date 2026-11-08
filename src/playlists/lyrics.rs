//! Lyrics from LRCLIB (lrclib.net): free, open, no account. It often has synced lyrics,
//! timed line by line for a player; those are saved as `.lrc` next to the media file, and
//! plain lyrics as `.txt`. Finding nothing is normal and just means no lyrics.
//!
//! Sends the item's artist and title to lrclib.net. ELSEWHERE_LRCLIB_URL points it at
//! another server (the tests use a local one).

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::{Meta, clean_title};
use crate::ytdlp;

const SERVICE: &str = "https://lrclib.net";

/// Finds lyrics for `meta` and writes them next to `file`. Returns the lyrics file.
pub(super) fn save(meta: &Meta, file: &Path) -> Option<PathBuf> {
    let (artist, track) = query(meta)?;
    let service = std::env::var("ELSEWHERE_LRCLIB_URL").unwrap_or_else(|_| SERVICE.to_owned());
    let url = format!(
        "{}/api/search?track_name={}&artist_name={}",
        service.trim_end_matches('/'),
        encode(&track),
        encode(&artist)
    );
    let answer: Value = serde_json::from_str(&ytdlp::fetch(&url).ok()?).ok()?;
    let found = answer
        .as_array()?
        .iter()
        .find(|result| fits(result, meta))?;

    let (lyrics, ext) = match (
        lyrics_text(found, "syncedLyrics"),
        lyrics_text(found, "plainLyrics"),
    ) {
        (Some(synced), _) => (synced, "lrc"),
        (None, Some(plain)) => (plain, "txt"),
        (None, None) => return None,
    };
    let path = file.with_extension(ext);
    fs::write(&path, lyrics).ok()?;
    Some(path)
}

/// Artist and track to search for: the item's own (see Meta), else an "Artist - Title"
/// video title split and cleaned.
fn query(meta: &Meta) -> Option<(String, String)> {
    if let Some(artist) = meta.artists.first() {
        return Some((artist.clone(), clean_title(&meta.title)));
    }
    let (artist, title) = meta.title.split_once(" - ")?;
    let (artist, title) = (artist.trim(), clean_title(title));
    (!artist.is_empty() && !title.is_empty()).then(|| (artist.to_owned(), title))
}

/// A result that is not instrumental and lasts about as long as ours.
fn fits(result: &Value, meta: &Meta) -> bool {
    if result["instrumental"] == true {
        return false;
    }
    // YouTube Music's songs last exactly as long as the track; music videos may add an
    // intro or outro.
    let tolerance = if meta.from_music_fields { 5.0 } else { 20.0 };
    match (meta.duration_ms, result["duration"].as_f64()) {
        (Some(ms), Some(seconds)) => (ms as f64 / 1000.0 - seconds).abs() <= tolerance,
        _ => true,
    }
}

fn lyrics_text(result: &Value, key: &str) -> Option<String> {
    let text = result[key].as_str()?.trim();
    (!text.is_empty()).then(|| format!("{text}\n"))
}

/// Percent-encodes everything but the unreserved URL characters.
fn encode(text: &str) -> String {
    text.bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn meta(title: &str, artists: &[&str], duration_ms: Option<i64>) -> Meta {
        Meta {
            title: title.to_owned(),
            artist: None,
            artists: artists.iter().map(|a| a.to_string()).collect(),
            album: None,
            uploader: None,
            year: None,
            duration_ms,
            description: None,
            from_music_fields: !artists.is_empty(),
        }
    }

    #[test]
    fn queries_prefer_music_fields() {
        assert_eq!(
            query(&meta("Song", &["Artist", "Guest"], None)),
            Some(("Artist".to_owned(), "Song".to_owned()))
        );
        assert_eq!(
            query(&meta("Band - Hit (Official Video)", &[], None)),
            Some(("Band".to_owned(), "Hit".to_owned()))
        );
        assert_eq!(query(&meta("Vlog day 3", &[], None)), None);
    }

    #[test]
    fn results_must_fit() {
        let ours = meta("Song", &["A"], Some(200_000));
        assert!(fits(&json!({"duration": 203.0}), &ours));
        assert!(!fits(&json!({"duration": 210.0}), &ours));
        assert!(!fits(&json!({"duration": 260.0}), &ours));
        let video = meta("Band - Song", &[], Some(200_000));
        assert!(
            fits(&json!({"duration": 185.0}), &video),
            "music videos may run longer"
        );
        assert!(!fits(
            &json!({"duration": 200.0, "instrumental": true}),
            &ours
        ));
        assert!(fits(&json!({}), &ours));
    }

    #[test]
    fn encodes_queries() {
        assert_eq!(encode("AC/DC & Co ✓"), "AC%2FDC%20%26%20Co%20%E2%9C%93");
    }
}
