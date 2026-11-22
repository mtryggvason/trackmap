//! Writing analysis results back into audio files' tags, so the player can use them.

use std::io;
use std::path::{Path, PathBuf};

use lofty::config::WriteOptions;
use lofty::prelude::*;
use lofty::tag::Tag;

use crate::Track;

/// Writes `bpm` (rounded to a whole number, as the ID3 BPM field expects) into the file's main
/// tag, creating the tag if the file has none. Other tags and fields are left alone.
pub fn write_bpm(path: &Path, bpm: f32) -> io::Result<()> {
    let mut file = lofty::read_from_path(path).map_err(io::Error::other)?;
    if file.primary_tag_mut().is_none() {
        let ty = file.primary_tag_type();
        file.insert_tag(Tag::new(ty));
    }
    let tag = file.primary_tag_mut().expect("tag was just inserted");
    let value = format!("{}", bpm.round() as u32);
    // ID3v2 calls it TBPM (IntegerBpm); Vorbis comments and others use a plain BPM field.
    if !tag.insert_text(ItemKey::IntegerBpm, value.clone()) {
        tag.insert_text(ItemKey::Bpm, value);
    }
    file.save_to_path(path, WriteOptions::default()).map_err(io::Error::other)
}

/// Tracks whose BPM was detected but isn't in their tags yet, with the value to write.
pub fn bpm_to_write(tracks: &[Track]) -> Vec<(PathBuf, f32)> {
    tracks
        .iter()
        .filter(|t| t.tempo_is_detected())
        .filter_map(|t| Some((t.path.clone(), t.detected_bpm?)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::read_track;
    use crate::scan::tests::write_tagged_wav;

    #[test]
    fn writes_bpm_and_keeps_other_tags() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.wav");
        write_tagged_wav(&p, "Night Drive");

        write_bpm(&p, 127.6).unwrap();
        let t = read_track(&p);
        assert_eq!(t.bpm, Some(128.0));
        assert_eq!(t.title.as_deref(), Some("Night Drive"));
        assert_eq!(t.label.as_deref(), Some("Good Label"));
        assert_eq!(t.key, Some("8A".parse().unwrap()));
    }

    #[test]
    fn creates_a_tag_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let tagged = dir.path().join("t.wav");
        write_tagged_wav(&tagged, "x");
        // Same audio, no tags at all.
        let bare = dir.path().join("bare.wav");
        std::fs::copy(&tagged, &bare).unwrap();
        for ty in [lofty::tag::TagType::Id3v2, lofty::tag::TagType::RiffInfo] {
            let _ = ty.remove_from_path(&bare, WriteOptions::default());
        }
        assert_eq!(read_track(&bare).bpm, None);

        write_bpm(&bare, 122.0).unwrap();
        assert_eq!(read_track(&bare).bpm, Some(122.0));
    }

    #[test]
    fn unreadable_files_error() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("bad.mp3");
        std::fs::write(&p, b"nope").unwrap();
        assert!(write_bpm(&p, 120.0).is_err());
    }

    #[test]
    fn only_detected_tempos_are_written() {
        let tracks = vec![
            Track { path: "/a".into(), detected_bpm: Some(124.0), analyzed: true, ..Default::default() },
            Track { path: "/b".into(), bpm: Some(120.0), detected_bpm: Some(121.0), ..Default::default() },
            Track { path: "/c".into(), analyzed: true, ..Default::default() },
        ];
        assert_eq!(bpm_to_write(&tracks), [(PathBuf::from("/a"), 124.0)]);
    }
}
