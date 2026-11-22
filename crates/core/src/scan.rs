//! Walks a library root and reads tags from every playable file.

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use lofty::prelude::*;
use walkdir::WalkDir;

use crate::{cache, is_library_path, is_supported_audio, Camelot, Track};

#[derive(Debug, Clone, Default)]
pub struct Library {
    pub root: PathBuf,
    /// Sorted by path.
    pub tracks: Vec<Track>,
}

impl Library {
    pub fn artist_count(&self) -> usize {
        distinct(self.tracks.iter().filter_map(|t| t.artist.as_deref()))
    }

    pub fn label_count(&self) -> usize {
        distinct(self.tracks.iter().filter_map(|t| t.label.as_deref()))
    }

    pub(crate) fn sort(&mut self) {
        self.tracks.sort_by(|a, b| a.path.cmp(&b.path));
    }
}

fn distinct<'a>(it: impl Iterator<Item = &'a str>) -> usize {
    it.collect::<HashSet<_>>().len()
}

/// Result of opening a library.
#[derive(Debug, Clone)]
pub struct ScanReport {
    pub library: Library,
    /// Tracks that were not in the cache, i.e. added while the app was closed.
    /// Empty the first time a folder is opened, since there is nothing to compare against.
    pub new: Vec<PathBuf>,
}

/// Opens a library: loads the cache, rescans (re-reading only changed files), saves the cache.
pub fn open(root: &Path) -> io::Result<ScanReport> {
    let root = root.canonicalize()?;
    let cached = cache::load(&root);
    let report = scan_with(&root, cached.as_ref(), now())?;
    // A read-only library still opens; it just won't remember anything.
    let _ = cache::save(&report.library);
    Ok(report)
}

/// Scans `root` from scratch, without a cache.
pub fn scan(root: &Path) -> io::Result<Library> {
    Ok(scan_with(&root.canonicalize()?, None, now())?.library)
}

pub(crate) fn scan_with(
    root: &Path,
    cached: Option<&HashMap<PathBuf, Track>>,
    now: u64,
) -> io::Result<ScanReport> {
    if !root.is_dir() {
        return Err(io::Error::new(io::ErrorKind::NotFound, "not a folder"));
    }
    let mut tracks = Vec::new();
    let mut new = Vec::new();

    for path in audio_files(root, root) {
        let prev = cached.and_then(|c| c.get(&path));
        let (size, modified) = file_stamp(&path);
        let track = match prev {
            Some(t) if t.size == size && t.modified == modified => t.clone(),
            Some(t) => Track { added_at: t.added_at, ..read_track(&path) },
            None if cached.is_some() => {
                new.push(path.clone());
                Track { added_at: Some(now), ..read_track(&path) }
            }
            // First time this folder is opened: fall back to the file's creation date.
            None => read_track(&path),
        };
        tracks.push(track);
    }

    let mut library = Library { root: root.to_path_buf(), tracks };
    library.sort();
    Ok(ScanReport { library, new })
}

/// All playable files under `dir` that belong to the library at `root`.
pub(crate) fn audio_files(root: &Path, dir: &Path) -> Vec<PathBuf> {
    WalkDir::new(dir)
        .into_iter()
        .filter_entry(|e| e.path() == root || is_library_path(root, e.path()))
        // Unreadable directories are skipped; the rest is still scanned.
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file() && is_supported_audio(e.path()))
        .map(|e| e.into_path())
        .collect()
}

/// Reads tags from one file. Never fails: missing tags just leave fields empty.
pub fn read_track(path: &Path) -> Track {
    let (size, modified) = file_stamp(path);
    let mut track = Track {
        path: path.to_path_buf(),
        added_at: created_at(path),
        size,
        modified,
        ..Default::default()
    };

    if let Ok(file) = lofty::read_from_path(path) {
        if let Some(tag) = file.primary_tag().or_else(|| file.first_tag()) {
            track.title = tag.title().map(|s| s.into_owned());
            track.artist = tag.artist().map(|s| s.into_owned());
            track.album = tag.album().map(|s| s.into_owned());
            track.genre = tag.genre().map(|s| s.into_owned());
            track.label = first_string(tag, &[ItemKey::Label, ItemKey::Publisher]);
            track.bpm = first_string(tag, &[ItemKey::Bpm, ItemKey::IntegerBpm])
                .and_then(|s| s.trim().parse::<f32>().ok())
                .filter(|b| *b > 0.0);
            track.key = tag
                .get_string(ItemKey::InitialKey)
                .and_then(|s| s.parse::<Camelot>().ok());
        }
    }

    if track.title.as_deref().is_none_or(str::is_empty) {
        track.title = path.file_stem().map(|s| s.to_string_lossy().into_owned());
    }
    track
}

fn first_string(tag: &lofty::tag::Tag, keys: &[ItemKey]) -> Option<String> {
    keys.iter()
        .find_map(|k| tag.get_string(k.clone()))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

pub(crate) fn now() -> u64 {
    unix(SystemTime::now()).unwrap_or(0)
}

fn unix(t: SystemTime) -> Option<u64> {
    t.duration_since(UNIX_EPOCH).ok().map(|d| d.as_secs())
}

fn file_stamp(path: &Path) -> (u64, Option<u64>) {
    match std::fs::metadata(path) {
        Ok(m) => (m.len(), m.modified().ok().and_then(unix)),
        Err(_) => (0, None),
    }
}

fn created_at(path: &Path) -> Option<u64> {
    let meta = std::fs::metadata(path).ok()?;
    meta.created().or_else(|_| meta.modified()).ok().and_then(unix)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::PLAYLISTS_DIR;
    use lofty::config::WriteOptions;
    use lofty::tag::{Tag, TagType};
    use std::fs;

    /// A valid, silent 16-bit mono WAV file.
    fn wav_bytes() -> Vec<u8> {
        let data = [0u8; 400];
        let mut b = Vec::new();
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
        b.extend_from_slice(b"WAVEfmt ");
        b.extend_from_slice(&16u32.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes()); // PCM
        b.extend_from_slice(&1u16.to_le_bytes()); // mono
        b.extend_from_slice(&44_100u32.to_le_bytes());
        b.extend_from_slice(&88_200u32.to_le_bytes());
        b.extend_from_slice(&2u16.to_le_bytes());
        b.extend_from_slice(&16u16.to_le_bytes());
        b.extend_from_slice(b"data");
        b.extend_from_slice(&(data.len() as u32).to_le_bytes());
        b.extend_from_slice(&data);
        b
    }

    pub(crate) fn write_tagged_wav(path: &Path, title: &str) {
        fs::write(path, wav_bytes()).unwrap();
        let mut tag = Tag::new(TagType::Id3v2);
        tag.set_title(title.into());
        tag.set_artist("Some Artist".into());
        tag.set_album("Some EP".into());
        tag.insert_text(ItemKey::Label, "Good Label".into());
        tag.insert_text(ItemKey::IntegerBpm, "124".into());
        tag.insert_text(ItemKey::InitialKey, "Am".into());
        tag.save_to_path(path, WriteOptions::default()).unwrap();
    }

    pub(crate) fn rel(lib: &Library) -> Vec<String> {
        lib.tracks
            .iter()
            .map(|t| t.path.strip_prefix(&lib.root).unwrap().to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn reads_tags() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("track.wav");
        write_tagged_wav(&p, "Night Drive");

        let t = read_track(&p);
        assert_eq!(t.title.as_deref(), Some("Night Drive"));
        assert_eq!(t.artist.as_deref(), Some("Some Artist"));
        assert_eq!(t.album.as_deref(), Some("Some EP"));
        assert_eq!(t.label.as_deref(), Some("Good Label"));
        assert_eq!(t.bpm, Some(124.0));
        assert_eq!(t.key, Some("8A".parse().unwrap()));
        assert!(t.added_at.is_some());
        assert!(t.size > 0 && t.modified.is_some());
    }

    #[test]
    fn untagged_or_broken_files_fall_back_to_file_name() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("not really audio.mp3");
        fs::write(&p, b"garbage").unwrap();

        let t = read_track(&p);
        assert_eq!(t.title.as_deref(), Some("not really audio"));
        assert_eq!(t.artist, None);
    }

    #[test]
    fn scan_skips_playlists_hidden_and_unsupported() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("House/Deep")).unwrap();
        fs::create_dir_all(root.join(PLAYLISTS_DIR).join("Warmup")).unwrap();
        fs::create_dir_all(root.join(".trackmap")).unwrap();

        write_tagged_wav(&root.join("House/Deep/a.wav"), "A");
        fs::write(root.join("House/b.mp3"), b"x").unwrap();
        fs::write(root.join("House/cover.jpg"), b"x").unwrap();
        fs::write(root.join("House/c.m4a"), b"x").unwrap();
        fs::write(root.join(PLAYLISTS_DIR).join("Warmup/a.wav"), b"x").unwrap();
        fs::write(root.join(".trackmap/hidden.mp3"), b"x").unwrap();
        fs::write(root.join("House/.hidden.mp3"), b"x").unwrap();

        let lib = scan(root).unwrap();
        assert_eq!(rel(&lib), ["House/Deep/a.wav", "House/b.mp3"]);
        assert_eq!(lib.artist_count(), 1);
        assert_eq!(lib.label_count(), 1);
    }

    #[test]
    fn scan_missing_root_is_an_error() {
        assert!(scan(Path::new("/definitely/not/here")).is_err());
    }

    #[test]
    fn open_reports_files_added_while_closed() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write_tagged_wav(&root.join("a.wav"), "A");

        // First open: nothing to compare against, so nothing is "new".
        let first = open(root).unwrap();
        assert!(first.new.is_empty());
        assert_eq!(first.library.tracks.len(), 1);

        // Add a file while "closed", then reopen.
        write_tagged_wav(&root.join("b.wav"), "B");
        let second = open(root).unwrap();
        assert_eq!(rel(&second.library), ["a.wav", "b.wav"]);
        assert_eq!(second.new, [second.library.root.join("b.wav")]);

        // Third open: b is no longer new, and a keeps its original added_at.
        let third = open(root).unwrap();
        assert!(third.new.is_empty());
        assert_eq!(third.library.tracks[0].added_at, first.library.tracks[0].added_at);
    }

    #[test]
    fn unchanged_files_come_from_cache() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        write_tagged_wav(&root.join("a.wav"), "Real Title");

        let mut cached = HashMap::new();
        let mut t = read_track(&root.join("a.wav"));
        t.title = Some("From Cache".into());
        cached.insert(t.path.clone(), t.clone());
        let lib = scan_with(&root, Some(&cached), 0).unwrap().library;
        assert_eq!(lib.tracks[0].title.as_deref(), Some("From Cache"));

        // A changed size forces a re-read.
        t.size += 1;
        cached.insert(t.path.clone(), t);
        let lib = scan_with(&root, Some(&cached), 0).unwrap().library;
        assert_eq!(lib.tracks[0].title.as_deref(), Some("Real Title"));
    }
}
