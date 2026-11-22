//! Writing analysis results back into audio files' tags, so the player can use them.

use std::io;
use std::path::{Path, PathBuf};

use lofty::config::WriteOptions;
use lofty::prelude::*;
use lofty::tag::Tag;

use crate::{Camelot, Track};

/// Detected values to write into one file's tags.
#[derive(Debug, Clone, PartialEq)]
pub struct TagWrite {
    pub path: PathBuf,
    pub bpm: Option<f32>,
    pub key: Option<Camelot>,
}

/// How a write went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Written {
    Ok,
    /// Written, but the file's store "ownership" (OWNE) frame had to be dropped: the tag
    /// library can read that frame but not write it back.
    DroppedOwnership,
}

/// Writes detected values into the file's main tag, creating the tag if the file has none.
/// BPM is rounded to a whole number (as the ID3 BPM field expects); the key is written in
/// musical notation ("Am", "F#m", "Db") as the ID3 key field specifies. Other fields are kept.
pub fn write(w: &TagWrite) -> io::Result<Written> {
    match write_generic(w) {
        Ok(()) => Ok(Written::Ok),
        Err(e) => match write_id3_without_ownership(w) {
            Ok(true) => Ok(Written::DroppedOwnership),
            // Not an MP3 with an OWNE frame: report the original problem.
            _ => Err(e),
        },
    }
}

fn write_generic(w: &TagWrite) -> io::Result<()> {
    let mut file = lofty::read_from_path(&w.path).map_err(io::Error::other)?;
    if file.primary_tag_mut().is_none() {
        let ty = file.primary_tag_type();
        file.insert_tag(Tag::new(ty));
    }
    let tag = file.primary_tag_mut().expect("tag was just inserted");
    if let Some(bpm) = w.bpm {
        let value = format!("{}", bpm.round() as u32);
        // ID3v2 calls it TBPM (IntegerBpm); Vorbis comments and others use a plain BPM field.
        if !tag.insert_text(ItemKey::IntegerBpm, value.clone()) {
            tag.insert_text(ItemKey::Bpm, value);
        }
    }
    if let Some(key) = w.key {
        tag.insert_text(ItemKey::InitialKey, key.musical_name());
    }
    file.save_to_path(&w.path, WriteOptions::default()).map_err(io::Error::other)
}

/// Fallback for MP3s whose ID3 tag has an OWNE frame: edits the ID3 tag directly, without that
/// frame. Returns `Ok(false)` (and changes nothing) if there is no OWNE frame to blame.
fn write_id3_without_ownership(w: &TagWrite) -> io::Result<bool> {
    use lofty::config::ParseOptions;
    use lofty::id3::v2::{Frame, FrameId, TextInformationFrame};
    use lofty::mpeg::MpegFile;
    use lofty::TextEncoding;

    let mut f = std::fs::File::open(&w.path)?;
    let mut mp = MpegFile::read_from(&mut f, ParseOptions::new()).map_err(io::Error::other)?;
    drop(f);
    let Some(tag) = mp.id3v2_mut() else { return Ok(false) };
    if tag.remove(&FrameId::Valid("OWNE".into())).count() == 0 {
        return Ok(false);
    }
    let mut set = |id: &'static str, value: String| {
        tag.insert(Frame::Text(TextInformationFrame::new(FrameId::Valid(id.into()), TextEncoding::UTF8, value)));
    };
    if let Some(bpm) = w.bpm {
        set("TBPM", format!("{}", bpm.round() as u32));
    }
    if let Some(key) = w.key {
        set("TKEY", key.musical_name());
    }
    tag.save_to_path(&w.path, WriteOptions::default()).map_err(io::Error::other)?;
    Ok(true)
}

/// Writes only a BPM.
pub fn write_bpm(path: &Path, bpm: f32) -> io::Result<Written> {
    write(&TagWrite { path: path.to_path_buf(), bpm: Some(bpm), key: None })
}

/// Detected BPMs and keys that aren't in their files' tags yet.
pub fn pending_writes(tracks: &[Track]) -> Vec<TagWrite> {
    tracks
        .iter()
        .filter_map(|t| {
            let bpm = t.detected_bpm.filter(|_| t.tempo_is_detected());
            let key = t.detected_key.filter(|_| t.key_is_detected());
            (bpm.is_some() || key.is_some()).then(|| TagWrite { path: t.path.clone(), bpm, key })
        })
        .collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::scan::read_track;
    use crate::scan::tests::write_tagged_wav;

    #[test]
    fn writes_bpm_and_keeps_other_tags() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.wav");
        write_tagged_wav(&p, "Night Drive");

        assert_eq!(write_bpm(&p, 127.6).unwrap(), Written::Ok);
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

    /// A short silent MP3 (MPEG-1 layer III, 128 kbps) behind an ID3v2.4 tag with the given
    /// frames and padding. Built by hand because the tag library can't write OWNE frames.
    pub(crate) fn mp3_with_id3(frames: &[(&str, Vec<u8>)], padding: usize) -> Vec<u8> {
        let mut body = Vec::new();
        for (id, data) in frames {
            body.extend_from_slice(id.as_bytes());
            let n = data.len() as u32;
            body.extend([(n >> 21) & 0x7f, (n >> 14) & 0x7f, (n >> 7) & 0x7f, n & 0x7f].map(|b| b as u8));
            body.extend([0, 0]);
            body.extend_from_slice(data);
        }
        body.extend(std::iter::repeat_n(0u8, padding));
        let n = body.len() as u32;
        let mut out = b"ID3\x04\x00\x00".to_vec();
        out.extend([(n >> 21) & 0x7f, (n >> 14) & 0x7f, (n >> 7) & 0x7f, n & 0x7f].map(|b| b as u8));
        out.extend(body);
        for _ in 0..400 {
            // 128 kbps @ 44.1 kHz, no padding: 417 bytes per frame.
            out.extend([0xFF, 0xFB, 0x90, 0x64]);
            out.extend(std::iter::repeat_n(0u8, 413));
        }
        out
    }

    fn text(s: &str) -> Vec<u8> {
        let mut v = vec![3u8]; // UTF-8
        v.extend_from_slice(s.as_bytes());
        v
    }

    #[test]
    fn mp3_with_ownership_frame_is_written_without_it() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("bought.mp3");
        let owne = b"\x00USD1.00\x0020240101Some Store".to_vec();
        std::fs::write(&p, mp3_with_id3(&[("TIT2", text("Bought Track")), ("OWNE", owne)], 64)).unwrap();
        // The plain path fails on this file, as it does in real libraries.
        assert!(write_generic(&TagWrite { path: p.clone(), bpm: Some(1.0), key: None }).is_err());

        let key: Camelot = "8A".parse().unwrap();
        let w = TagWrite { path: p.clone(), bpm: Some(126.4), key: Some(key) };
        assert_eq!(write(&w).unwrap(), Written::DroppedOwnership);
        let t = read_track(&p);
        assert_eq!((t.bpm, t.key), (Some(126.0), Some(key)));
        assert_eq!(t.title.as_deref(), Some("Bought Track"));
        let raw = std::fs::read(&p).unwrap();
        assert!(!raw.windows(4).any(|w| w == b"OWNE"));
    }

    #[test]
    fn writes_key_in_musical_notation() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.wav");
        write_tagged_wav(&p, "x");
        let key: Camelot = "11A".parse().unwrap();
        write(&TagWrite { path: p.clone(), bpm: Some(124.0), key: Some(key) }).unwrap();

        let t = read_track(&p);
        assert_eq!((t.bpm, t.key), (Some(124.0), Some(key)));
        let file = lofty::read_from_path(&p).unwrap();
        assert_eq!(file.primary_tag().unwrap().get_string(ItemKey::InitialKey), Some("F#m"));
    }

    #[test]
    fn only_detected_values_are_written() {
        let k = |s: &str| Some(s.parse::<Camelot>().unwrap());
        let tracks = vec![
            Track { path: "/a".into(), detected_bpm: Some(124.0), detected_key: k("8A"), ..Default::default() },
            Track { path: "/b".into(), bpm: Some(120.0), detected_bpm: Some(121.0), detected_key: k("2B"), ..Default::default() },
            Track { path: "/c".into(), key: k("1A"), detected_key: k("2A"), ..Default::default() },
            Track { path: "/d".into(), analyzed: true, key_analyzed: true, ..Default::default() },
        ];
        assert_eq!(
            pending_writes(&tracks),
            [
                TagWrite { path: "/a".into(), bpm: Some(124.0), key: k("8A") },
                TagWrite { path: "/b".into(), bpm: None, key: k("2B") },
            ]
        );
    }
}
