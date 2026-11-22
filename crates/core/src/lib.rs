//! Library model shared by the graph and the app. No UI code lives here.

pub mod key;
pub mod scan;

use std::path::{Path, PathBuf};

pub use key::Camelot;
pub use scan::{scan, Library};

/// File extensions the Devious Pocket can play (docs.nap.works). AAC/m4a is not supported.
pub const SUPPORTED_EXTENSIONS: &[&str] = &[
    "mp3", "flac", "fla", "wav", "aif", "aiff", "caf", "ogg", "oga",
];

/// Name of the folder under the library root whose subfolders are playlists.
pub const PLAYLISTS_DIR: &str = "Playlists";

pub fn is_supported_audio(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| SUPPORTED_EXTENSIONS.iter().any(|s| s.eq_ignore_ascii_case(e)))
        .unwrap_or(false)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TrackId(pub u64);

#[derive(Debug, Clone, Default)]
pub struct Track {
    pub path: PathBuf,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub label: Option<String>,
    pub genre: Option<String>,
    pub bpm: Option<f32>,
    pub key: Option<Camelot>,
    /// Seconds since the Unix epoch when the file first appeared in the library.
    pub added_at: Option<u64>,
}

#[derive(Debug, Clone, Default)]
pub struct Playlist {
    pub name: String,
    pub tracks: Vec<TrackId>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supported_extensions() {
        assert!(is_supported_audio(Path::new("a/b.MP3")));
        assert!(is_supported_audio(Path::new("x.aiff")));
        assert!(!is_supported_audio(Path::new("x.m4a")));
        assert!(!is_supported_audio(Path::new("noext")));
    }
}
