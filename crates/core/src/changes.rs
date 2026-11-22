//! Turning file-system events into library updates.
//!
//! Split in two so the slow part (reading files) can run off the UI thread:
//! [`changes_for`] does the I/O, [`Library::apply`] is a pure in-memory merge.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use crate::scan::{audio_files, now, read_track};
use crate::{is_library_path, is_supported_audio, Library, Track};

#[derive(Debug, Clone)]
pub enum Change {
    /// A file was added or modified.
    Upsert(Track),
    /// Everything at or under this path is gone (a file or a whole folder).
    Remove(PathBuf),
    /// The contents of this folder are now exactly these tracks (folder moved in, or a rescan).
    ReplaceDir(PathBuf, Vec<Track>),
}

/// Inspects the given changed paths on disk and describes what happened to the library.
pub fn changes_for(root: &Path, paths: impl IntoIterator<Item = PathBuf>) -> Vec<Change> {
    // Parents sort before children, so a folder event covers the files under it.
    let paths: BTreeSet<PathBuf> = paths.into_iter().collect();
    let mut out: Vec<Change> = Vec::new();
    let mut covered: Vec<PathBuf> = Vec::new();

    for path in paths {
        if covered.iter().any(|d| path.starts_with(d)) {
            continue;
        }
        let is_root = path == root;
        if !is_root && !is_library_path(root, &path) {
            continue;
        }
        match std::fs::metadata(&path) {
            Err(_) => {
                covered.push(path.clone());
                out.push(Change::Remove(path));
            }
            Ok(m) if m.is_dir() => {
                let tracks = audio_files(root, &path).iter().map(|p| read_track(p)).collect();
                covered.push(path.clone());
                out.push(Change::ReplaceDir(path, tracks));
            }
            Ok(m) if m.is_file() && is_supported_audio(&path) => {
                out.push(Change::Upsert(read_track(&path)));
            }
            Ok(_) => {}
        }
    }
    out
}

impl Library {
    /// Applies changes and returns the paths of tracks that are new to the library.
    /// Tracks that already existed keep their `added_at`, so editing tags doesn't make them "new".
    pub fn apply(&mut self, changes: Vec<Change>) -> Vec<PathBuf> {
        self.apply_at(changes, now())
    }

    pub(crate) fn apply_at(&mut self, changes: Vec<Change>, now: u64) -> Vec<PathBuf> {
        let mut by_path: HashMap<PathBuf, Track> =
            self.tracks.drain(..).map(|t| (t.path.clone(), t)).collect();
        let mut added = Vec::new();

        let mut insert = |by_path: &mut HashMap<PathBuf, Track>,
                          removed: &HashMap<PathBuf, Track>,
                          mut t: Track| {
            match by_path.get(&t.path).or_else(|| removed.get(&t.path)) {
                Some(old) => t.added_at = old.added_at,
                None => {
                    t.added_at = Some(now);
                    added.push(t.path.clone());
                }
            }
            by_path.insert(t.path.clone(), t);
        };

        for change in changes {
            match change {
                Change::Upsert(t) => insert(&mut by_path, &HashMap::new(), t),
                Change::Remove(p) => by_path.retain(|k, _| !k.starts_with(&p)),
                Change::ReplaceDir(dir, tracks) => {
                    let (removed, kept) = by_path.drain().partition(|(k, _)| k.starts_with(&dir));
                    by_path = kept;
                    for t in tracks {
                        insert(&mut by_path, &removed, t);
                    }
                }
            }
        }

        self.tracks = by_path.into_values().collect();
        self.sort();
        added
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::tests::{rel, write_tagged_wav};
    use crate::{scan, PLAYLISTS_DIR};
    use std::fs;

    fn setup() -> (tempfile::TempDir, Library) {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("House")).unwrap();
        write_tagged_wav(&dir.path().join("House/a.wav"), "A");
        let lib = scan(dir.path()).unwrap();
        (dir, lib)
    }

    #[test]
    fn added_file() {
        let (_d, mut lib) = setup();
        let p = lib.root.join("House/b.wav");
        write_tagged_wav(&p, "B");

        let added = lib.apply_at(changes_for(&lib.root.clone(), [p.clone()]), 42);
        assert_eq!(added, [p]);
        assert_eq!(rel(&lib), ["House/a.wav", "House/b.wav"]);
        assert_eq!(lib.tracks[1].added_at, Some(42));
    }

    #[test]
    fn modified_file_keeps_added_at_and_is_not_new() {
        let (_d, mut lib) = setup();
        let p = lib.root.join("House/a.wav");
        let before = lib.tracks[0].added_at;
        write_tagged_wav(&p, "A (edit)");

        let added = lib.apply_at(changes_for(&lib.root.clone(), [p]), 42);
        assert!(added.is_empty());
        assert_eq!(lib.tracks[0].title.as_deref(), Some("A (edit)"));
        assert_eq!(lib.tracks[0].added_at, before);
    }

    #[test]
    fn deleted_file_and_folder() {
        let (_d, mut lib) = setup();
        let root = lib.root.clone();
        write_tagged_wav(&root.join("b.wav"), "B");
        lib.apply(changes_for(&root, [root.join("b.wav")]));
        assert_eq!(lib.tracks.len(), 2);

        fs::remove_file(root.join("b.wav")).unwrap();
        lib.apply(changes_for(&root, [root.join("b.wav")]));
        assert_eq!(rel(&lib), ["House/a.wav"]);

        fs::remove_dir_all(root.join("House")).unwrap();
        // Events for the folder and the file inside it arrive together.
        lib.apply(changes_for(&root, [root.join("House/a.wav"), root.join("House")]));
        assert!(lib.tracks.is_empty());
    }

    #[test]
    fn folder_moved_in_and_renamed() {
        let (dir, mut lib) = setup();
        let root = lib.root.clone();

        // Build a folder outside the library, then move it in.
        let outside = tempfile::tempdir().unwrap();
        write_tagged_wav(&outside.path().join("c.wav"), "C");
        fs::rename(outside.path(), root.join("Techno")).ok().or_else(|| {
            // Different volumes: fall back to copying.
            fs::create_dir_all(root.join("Techno")).unwrap();
            fs::copy(outside.path().join("c.wav"), root.join("Techno/c.wav")).ok().map(|_| ())
        });
        let added = lib.apply(changes_for(&root, [root.join("Techno")]));
        assert_eq!(added, [root.join("Techno/c.wav")]);

        // Rename House -> Deep House: one event for each side of the rename.
        fs::rename(root.join("House"), root.join("Deep House")).unwrap();
        lib.apply(changes_for(&root, [root.join("House"), root.join("Deep House")]));
        assert_eq!(rel(&lib), ["Deep House/a.wav", "Techno/c.wav"]);
        drop(dir);
    }

    #[test]
    fn root_rescan_picks_up_everything_and_drops_missing() {
        let (_d, mut lib) = setup();
        let root = lib.root.clone();
        write_tagged_wav(&root.join("new.wav"), "N");
        fs::remove_file(root.join("House/a.wav")).unwrap();

        let added = lib.apply(changes_for(&root, [root.clone()]));
        assert_eq!(added, [root.join("new.wav")]);
        assert_eq!(rel(&lib), ["new.wav"]);
    }

    #[test]
    fn ignores_hidden_playlists_and_unsupported() {
        let (_d, mut lib) = setup();
        let root = lib.root.clone();
        fs::create_dir_all(root.join(PLAYLISTS_DIR).join("Warmup")).unwrap();
        fs::write(root.join(PLAYLISTS_DIR).join("Warmup/x.mp3"), b"x").unwrap();
        fs::write(root.join("cover.jpg"), b"x").unwrap();

        let changes = changes_for(
            &root,
            [
                root.join(PLAYLISTS_DIR).join("Warmup/x.mp3"),
                root.join(".trackmap/library.json"),
                root.join("cover.jpg"),
                PathBuf::from("/elsewhere/x.mp3"),
            ],
        );
        assert!(changes.is_empty());
        assert!(lib.apply(changes).is_empty());
    }
}
