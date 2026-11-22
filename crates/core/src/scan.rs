//! Walks a library root and reads tags from every playable file.

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use lofty::prelude::*;
use walkdir::{DirEntry, WalkDir};

use crate::{is_supported_audio, Camelot, Track, PLAYLISTS_DIR};

#[derive(Debug, Clone, Default)]
pub struct Library {
    pub root: PathBuf,
    pub tracks: Vec<Track>,
}

impl Library {
    pub fn artist_count(&self) -> usize {
        distinct(self.tracks.iter().filter_map(|t| t.artist.as_deref()))
    }

    pub fn label_count(&self) -> usize {
        distinct(self.tracks.iter().filter_map(|t| t.label.as_deref()))
    }
}

fn distinct<'a>(it: impl Iterator<Item = &'a str>) -> usize {
    it.collect::<HashSet<_>>().len()
}

/// Scans `root` recursively. Skips hidden entries (such as `.napworks/`) and the top-level
/// `Playlists/` folder, whose entries are links to tracks that are already in the library.
/// Files with unreadable tags are still included, using the file name as the title.
pub fn scan(root: &Path) -> io::Result<Library> {
    let root = root.canonicalize()?;
    let playlists = root.join(PLAYLISTS_DIR);
    let mut tracks = Vec::new();

    let walker = WalkDir::new(&root)
        .sort_by_file_name()
        .into_iter()
        .filter_entry(|e| e.depth() == 0 || (!is_hidden(e) && e.path() != playlists));

    for entry in walker {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue, // unreadable directory: skip, keep scanning the rest
        };
        if entry.file_type().is_file() && is_supported_audio(entry.path()) {
            tracks.push(read_track(entry.path()));
        }
    }

    Ok(Library { root, tracks })
}

fn is_hidden(e: &DirEntry) -> bool {
    e.file_name().to_str().is_some_and(|s| s.starts_with('.'))
}

/// Reads tags from one file. Never fails: missing tags just leave fields empty.
pub fn read_track(path: &Path) -> Track {
    let mut track = Track {
        path: path.to_path_buf(),
        added_at: added_at(path),
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

fn added_at(path: &Path) -> Option<u64> {
    let meta = std::fs::metadata(path).ok()?;
    let t = meta.created().or_else(|_| meta.modified()).ok()?;
    t.duration_since(UNIX_EPOCH).ok().map(|d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;
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

    fn write_tagged_wav(path: &Path) {
        fs::write(path, wav_bytes()).unwrap();
        let mut tag = Tag::new(TagType::Id3v2);
        tag.set_title("Night Drive".into());
        tag.set_artist("Some Artist".into());
        tag.set_album("Some EP".into());
        tag.insert_text(ItemKey::Label, "Good Label".into());
        tag.insert_text(ItemKey::IntegerBpm, "124".into());
        tag.insert_text(ItemKey::InitialKey, "Am".into());
        tag.save_to_path(path, WriteOptions::default()).unwrap();
    }

    #[test]
    fn reads_tags() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("track.wav");
        write_tagged_wav(&p);

        let t = read_track(&p);
        assert_eq!(t.title.as_deref(), Some("Night Drive"));
        assert_eq!(t.artist.as_deref(), Some("Some Artist"));
        assert_eq!(t.album.as_deref(), Some("Some EP"));
        assert_eq!(t.label.as_deref(), Some("Good Label"));
        assert_eq!(t.bpm, Some(124.0));
        assert_eq!(t.key, Some("8A".parse().unwrap()));
        assert!(t.added_at.is_some());
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
        fs::create_dir_all(root.join(".napworks")).unwrap();

        write_tagged_wav(&root.join("House/Deep/a.wav"));
        fs::write(root.join("House/b.mp3"), b"x").unwrap();
        fs::write(root.join("House/cover.jpg"), b"x").unwrap();
        fs::write(root.join("House/c.m4a"), b"x").unwrap();
        fs::write(root.join(PLAYLISTS_DIR).join("Warmup/a.wav"), b"x").unwrap();
        fs::write(root.join(".napworks/hidden.mp3"), b"x").unwrap();
        fs::write(root.join("House/.hidden.mp3"), b"x").unwrap();

        let lib = scan(root).unwrap();
        let mut names: Vec<_> = lib
            .tracks
            .iter()
            .map(|t| t.path.strip_prefix(&lib.root).unwrap().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, ["House/Deep/a.wav", "House/b.mp3"]);
        assert_eq!(lib.artist_count(), 1);
        assert_eq!(lib.label_count(), 1);
    }

    #[test]
    fn scan_missing_root_is_an_error() {
        assert!(scan(Path::new("/definitely/not/here")).is_err());
    }
}
