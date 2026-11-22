//! Per-library cache at `<root>/.trackmap/library.json`.
//!
//! Paths are stored relative to the root, so the cache survives the library moving
//! (e.g. a drive mounted under a different name).

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{Library, Track};

pub const CACHE_DIR: &str = ".trackmap";
const CACHE_FILE: &str = "library.json";
// Bump when cached analysis should be redone (e.g. the decoder learned new formats).
const VERSION: u32 = 2;

#[derive(Serialize, Deserialize)]
struct CacheFile {
    version: u32,
    tracks: Vec<Track>,
}

pub fn cache_path(root: &Path) -> PathBuf {
    root.join(CACHE_DIR).join(CACHE_FILE)
}

/// Loads the cache keyed by absolute path. `None` if missing, unreadable or an old version.
pub fn load(root: &Path) -> Option<HashMap<PathBuf, Track>> {
    let bytes = fs::read(cache_path(root)).ok()?;
    let file: CacheFile = serde_json::from_slice(&bytes).ok()?;
    if file.version != VERSION {
        return None;
    }
    Some(
        file.tracks
            .into_iter()
            .map(|mut t| {
                t.path = root.join(&t.path);
                (t.path.clone(), t)
            })
            .collect(),
    )
}

/// Writes the cache atomically (temp file + rename), so a crash never leaves it half-written.
pub fn save(lib: &Library) -> io::Result<()> {
    let tracks = lib
        .tracks
        .iter()
        .filter_map(|t| {
            let rel = t.path.strip_prefix(&lib.root).ok()?;
            Some(Track { path: rel.to_path_buf(), ..t.clone() })
        })
        .collect();
    let json = serde_json::to_vec(&CacheFile { version: VERSION, tracks })?;

    let path = cache_path(&lib.root);
    fs::create_dir_all(path.parent().unwrap())?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, json)?;
    fs::rename(tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_uses_relative_paths() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let track = Track {
            path: root.join("House/a.mp3"),
            title: Some("A".into()),
            key: Some("8A".parse().unwrap()),
            bpm: Some(124.0),
            ..Default::default()
        };
        save(&Library { root: root.clone(), tracks: vec![track.clone()] }).unwrap();

        let raw = fs::read_to_string(cache_path(&root)).unwrap();
        assert!(raw.contains("House/a.mp3") && !raw.contains(&*root.to_string_lossy()));

        let loaded = load(&root).unwrap();
        assert_eq!(loaded.get(&track.path), Some(&track));
    }

    #[test]
    fn corrupt_or_missing_cache_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load(dir.path()).is_none());
        fs::create_dir_all(dir.path().join(CACHE_DIR)).unwrap();
        fs::write(cache_path(dir.path()), b"{not json").unwrap();
        assert!(load(dir.path()).is_none());
    }
}
