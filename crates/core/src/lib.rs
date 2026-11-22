//! Library model shared by the graph and the app. No UI code lives here.

pub mod cache;
pub mod changes;
pub mod key;
pub mod scan;
pub mod settings;
pub mod watch;

use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

pub use changes::{changes_for, Change};
pub use key::Camelot;
pub use scan::{open, scan, Library, ScanReport};
pub use watch::{watch, Watcher};

/// File extensions the Devious Pocket can play (docs.nap.works). AAC/m4a is not supported.
pub const SUPPORTED_EXTENSIONS: &[&str] = &[
    "mp3", "flac", "fla", "wav", "aif", "aiff", "caf", "ogg", "oga",
];

/// Name of the folder under the library root whose subfolders are playlists.
pub const PLAYLISTS_DIR: &str = "Playlists";

/// Whether `path` is part of the library at `root`: inside it, not hidden (e.g. `.trackmap/`),
/// and not in the top-level `Playlists/` folder, whose entries link to tracks already counted.
pub fn is_library_path(root: &Path, path: &Path) -> bool {
    let Ok(rel) = path.strip_prefix(root) else { return false };
    if rel.as_os_str().is_empty() {
        return false; // the root itself
    }
    let mut comps = rel.components().peekable();
    if let Some(Component::Normal(first)) = comps.peek() {
        if *first == PLAYLISTS_DIR {
            return false;
        }
    }
    comps.all(|c| match c {
        Component::Normal(n) => !n.to_string_lossy().starts_with('.'),
        _ => false,
    })
}

pub fn is_supported_audio(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| SUPPORTED_EXTENSIONS.iter().any(|s| s.eq_ignore_ascii_case(e)))
        .unwrap_or(false)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TrackId(pub u64);

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
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
    /// File size and modification time (Unix seconds), used to skip re-reading unchanged files.
    pub size: u64,
    pub modified: Option<u64>,
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

    #[test]
    fn library_paths() {
        let root = Path::new("/lib");
        assert!(is_library_path(root, Path::new("/lib/House/a.mp3")));
        assert!(is_library_path(root, Path::new("/lib/House/Playlists/a.mp3")));
        assert!(!is_library_path(root, Path::new("/lib/Playlists/Warmup/a.mp3")));
        assert!(!is_library_path(root, Path::new("/lib/.trackmap/library.json")));
        assert!(!is_library_path(root, Path::new("/lib/House/.a.mp3")));
        assert!(!is_library_path(root, Path::new("/other/a.mp3")));
        assert!(!is_library_path(root, root));
    }
}
