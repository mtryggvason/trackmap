//! Track artwork: the cover embedded in the file's tags, or a cover image in the track's folder.
//!
//! Covers are stored as small square JPEG thumbnails in `<root>/.trackmap/art/`, named by a
//! hash of the source image, so an album's tracks share one file.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use lofty::picture::PictureType;
use lofty::prelude::*;
use serde::{Deserialize, Serialize};

use crate::cache::CACHE_DIR;
use crate::Track;

/// Thumbnail edge length in pixels.
pub const THUMB_SIZE: u32 = 128;

/// Folder images used when a file has no embedded cover (matched case-insensitively).
const FOLDER_NAMES: &[&str] = &["cover", "folder", "front", "album", "artwork"];
const FOLDER_EXTS: &[&str] = &["jpg", "jpeg", "png"];

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Art {
    /// Not looked for yet.
    #[default]
    Unchecked,
    /// Looked for and none found (or unreadable).
    None,
    /// Thumbnail file name inside the art folder.
    File(String),
}

pub fn art_dir(root: &Path) -> PathBuf {
    root.join(CACHE_DIR).join("art")
}

/// Finds the cover for one track and makes sure its thumbnail exists.
pub fn find(root: &Path, track: &Path) -> Art {
    let Some(bytes) = embedded(track).or_else(|| folder_image(track)) else { return Art::None };
    let name = format!("{:016x}.jpg", fnv1a(&bytes));
    let out = art_dir(root).join(&name);
    if out.exists() || write_thumbnail(&bytes, &out).is_ok() {
        Art::File(name)
    } else {
        Art::None
    }
}

/// Looks up art for every track that hasn't been checked yet.
pub fn fill(root: &Path, tracks: &[PathBuf]) -> Vec<(PathBuf, Art)> {
    tracks.iter().map(|t| (t.clone(), find(root, t))).collect()
}

/// Tracks whose art hasn't been looked for yet.
pub fn unchecked(tracks: &[Track]) -> Vec<PathBuf> {
    tracks.iter().filter(|t| t.art == Art::Unchecked).map(|t| t.path.clone()).collect()
}

/// The front cover if tagged as such, otherwise the first picture in any tag.
fn embedded(track: &Path) -> Option<Vec<u8>> {
    let file = lofty::read_from_path(track).ok()?;
    let pictures: Vec<_> = file.tags().iter().flat_map(|t| t.pictures()).collect();
    pictures
        .iter()
        .find(|p| p.pic_type() == PictureType::CoverFront)
        .or_else(|| pictures.first())
        .map(|p| p.data().to_vec())
}

fn folder_image(track: &Path) -> Option<Vec<u8>> {
    let dir = track.parent()?;
    let mut found: Vec<PathBuf> = fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            let stem = p.file_stem().map(|s| s.to_string_lossy().to_lowercase());
            let ext = p.extension().map(|s| s.to_string_lossy().to_lowercase());
            matches!((stem, ext), (Some(s), Some(e))
                if FOLDER_NAMES.contains(&s.as_str()) && FOLDER_EXTS.contains(&e.as_str()))
        })
        .collect();
    // Prefer names in FOLDER_NAMES order (cover before folder before …).
    found.sort_by_key(|p| {
        let stem = p.file_stem().unwrap_or_default().to_string_lossy().to_lowercase();
        FOLDER_NAMES.iter().position(|n| *n == stem)
    });
    fs::read(found.first()?).ok()
}

fn write_thumbnail(bytes: &[u8], out: &Path) -> io::Result<()> {
    let img = image::load_from_memory(bytes).map_err(io::Error::other)?;
    let thumb = img
        .resize_to_fill(THUMB_SIZE, THUMB_SIZE, image::imageops::FilterType::Triangle)
        .to_rgb8();
    fs::create_dir_all(out.parent().unwrap())?;
    let tmp = out.with_extension("jpg.tmp");
    {
        let mut f = io::BufWriter::new(fs::File::create(&tmp)?);
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut f, 85)
            .encode_image(&thumb)
            .map_err(io::Error::other)?;
    }
    fs::rename(tmp, out)
}

/// Stable 64-bit FNV-1a hash, so thumbnail names don't change between builds.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ *b as u64).wrapping_mul(0x0100_0000_01b3))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::tests::write_tagged_wav;
    use lofty::config::WriteOptions;
    use lofty::picture::{MimeType, Picture};

    fn png(color: [u8; 3], w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbImage::from_pixel(w, h, image::Rgb(color));
        let mut out = io::Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }

    fn embed(track: &Path, data: Vec<u8>, kind: PictureType) {
        let mut file = lofty::read_from_path(track).unwrap();
        let tag = file.primary_tag_mut().unwrap();
        tag.push_picture(
            Picture::unchecked(data).pic_type(kind).mime_type(MimeType::Png).build(),
        );
        file.save_to_path(track, WriteOptions::default()).unwrap();
    }

    fn thumb_color(root: &Path, art: &Art) -> [u8; 3] {
        let Art::File(name) = art else { panic!("no art: {art:?}") };
        let img = image::open(art_dir(root).join(name)).unwrap().to_rgb8();
        assert_eq!(img.dimensions(), (THUMB_SIZE, THUMB_SIZE));
        img.get_pixel(64, 64).0
    }

    fn close(a: [u8; 3], b: [u8; 3]) -> bool {
        a.iter().zip(b).all(|(x, y)| x.abs_diff(y) < 12)
    }

    #[test]
    fn embedded_front_cover_wins() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let track = root.join("a.wav");
        write_tagged_wav(&track, "A");
        embed(&track, png([0, 0, 255], 300, 200), PictureType::Other);
        embed(&track, png([255, 0, 0], 600, 600), PictureType::CoverFront);
        // A folder image is ignored when the file has its own cover.
        fs::write(root.join("cover.png"), png([0, 255, 0], 50, 50)).unwrap();

        let art = find(root, &track);
        assert!(close(thumb_color(root, &art), [255, 0, 0]));
    }

    #[test]
    fn folder_image_fallback_and_sharing() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("Album")).unwrap();
        let (a, b) = (root.join("Album/a.wav"), root.join("Album/b.wav"));
        write_tagged_wav(&a, "A");
        write_tagged_wav(&b, "B");
        fs::write(root.join("Album/Folder.PNG"), png([0, 0, 255], 40, 40)).unwrap();
        fs::write(root.join("Album/Cover.jpg"), png([0, 255, 0], 40, 40)).unwrap();

        let results = fill(root, &[a, b]);
        assert_eq!(results[0].1, results[1].1, "album tracks share one thumbnail");
        assert!(close(thumb_color(root, &results[0].1), [0, 255, 0]), "cover before folder");
        assert_eq!(fs::read_dir(art_dir(root)).unwrap().count(), 1);
    }

    #[test]
    fn no_art_and_broken_images() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let track = root.join("a.wav");
        write_tagged_wav(&track, "A");
        assert_eq!(find(root, &track), Art::None);

        fs::write(root.join("cover.jpg"), b"not an image").unwrap();
        assert_eq!(find(root, &track), Art::None);
    }

    #[test]
    fn unchecked_lists_only_unchecked() {
        let tracks = vec![
            Track { path: "/a".into(), ..Default::default() },
            Track { path: "/b".into(), art: Art::None, ..Default::default() },
            Track { path: "/c".into(), art: Art::File("x.jpg".into()), ..Default::default() },
        ];
        assert_eq!(unchecked(&tracks), [PathBuf::from("/a")]);
    }
}
