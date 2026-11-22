//! Generates a demo library of short silent, tagged WAV files plus a few playlists.
//!
//!     cargo run -p trackmap-core --example demo_library -- /tmp/trackmap-demo [count]

use std::path::PathBuf;

use lofty::config::WriteOptions;
use lofty::picture::{MimeType, Picture, PictureType};
use lofty::prelude::*;
use lofty::tag::{Tag, TagType};
use trackmap_core::playlists;

const ARTISTS: &[&str] = &[
    "Mira Volt", "Kaspar Lune", "Odile Ferro", "Teo Brandt", "Night Index", "Sable & Rook",
    "Juniper Ash", "Lowfield", "Ayo Kessel", "Pale Harbour", "Rune Matsu", "Velvet Static",
];
const LABELS: &[&str] = &["Driftworks", "Concrete Bloom", "Salt Records", "Hollow Tone"];
const WORDS: &[&str] = &[
    "Night", "Drive", "Glass", "Signal", "Harbour", "Pulse", "Echo", "Velvet", "Static", "Bloom",
    "Orbit", "Tide", "Ember", "Lattice", "Mirage", "Cinder", "Arc", "Halo", "Drift", "Fold",
];

/// Small deterministic PRNG so the demo is the same every time.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn pick<'a>(&mut self, xs: &'a [&'a str]) -> &'a str {
        xs[(self.next() % xs.len() as u64) as usize]
    }
}

fn wav() -> Vec<u8> {
    let data = [0u8; 4410];
    let mut b = Vec::new();
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    for v in [16u32] {
        b.extend_from_slice(&v.to_le_bytes());
    }
    for v in [1u16, 1] {
        b.extend_from_slice(&v.to_le_bytes());
    }
    for v in [44_100u32, 88_200] {
        b.extend_from_slice(&v.to_le_bytes());
    }
    for v in [2u16, 16] {
        b.extend_from_slice(&v.to_le_bytes());
    }
    b.extend_from_slice(b"data");
    b.extend_from_slice(&(data.len() as u32).to_le_bytes());
    b.extend_from_slice(&data);
    b
}

/// A simple 300px cover: diagonal gradient between two colours derived from the artist index.
fn cover(index: usize) -> Vec<u8> {
    let hue = index as f32 / ARTISTS.len() as f32;
    let rgb = |h: f32, l: f32| {
        let f = |n: f32| {
            let k = (n + h * 12.0) % 12.0;
            let a = 0.7 * l.min(1.0 - l);
            ((l - a * (k - 3.0).min(9.0 - k).clamp(-1.0, 1.0)) * 255.0) as u8
        };
        [f(0.0), f(8.0), f(4.0)]
    };
    let (a, b) = (rgb(hue, 0.35), rgb((hue + 0.15) % 1.0, 0.65));
    let img = image::RgbImage::from_fn(300, 300, |x, y| {
        let t = (x + y) as f32 / 598.0;
        let mix = |i: usize| (a[i] as f32 * (1.0 - t) + b[i] as f32 * t) as u8;
        // A light disc so covers aren't just gradients.
        let (dx, dy) = (x as f32 - 150.0, y as f32 - 150.0);
        if (dx * dx + dy * dy).sqrt() < 60.0 { image::Rgb([235, 235, 235]) } else { image::Rgb([mix(0), mix(1), mix(2)]) }
    });
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).expect("encode cover");
    out.into_inner()
}

fn main() -> std::io::Result<()> {
    let mut args = std::env::args().skip(1);
    let root = PathBuf::from(args.next().expect("usage: demo_library <dir> [count]"));
    let count: usize = args.next().and_then(|c| c.parse().ok()).unwrap_or(150);
    std::fs::create_dir_all(&root)?;
    let root = root.canonicalize()?;
    let mut rng = Rng(0x5EED_CAFE);
    let mut paths = Vec::new();

    for i in 0..count {
        let artist = rng.pick(ARTISTS);
        // Each artist mostly releases on one label.
        let label = if rng.next() % 4 == 0 {
            rng.pick(LABELS)
        } else {
            LABELS[ARTISTS.iter().position(|a| *a == artist).unwrap() % LABELS.len()]
        };
        let title = format!("{} {}", rng.pick(WORDS), rng.pick(WORDS));
        // House/techno-ish tempos with a few slower and faster outliers.
        let bpm = match rng.next() % 10 {
            0 => 90 + rng.next() % 20,
            1 => 140 + rng.next() % 35,
            _ => 118 + rng.next() % 14,
        };
        let key = format!("{}{}", 1 + rng.next() % 12, if rng.next() % 3 == 0 { "B" } else { "A" });

        let artist_ix = ARTISTS.iter().position(|a| *a == artist).unwrap();
        let dir = root.join(artist);
        std::fs::create_dir_all(&dir)?;
        // The last artist keeps covers as a folder image instead of embedding them.
        let folder_art = artist_ix == ARTISTS.len() - 1;
        if folder_art && !dir.join("cover.png").exists() {
            std::fs::write(dir.join("cover.png"), cover(artist_ix))?;
        }
        let path = dir.join(format!("{i:03} {title}.wav"));
        std::fs::write(&path, wav())?;
        let mut tag = Tag::new(TagType::Id3v2);
        tag.set_title(title);
        tag.set_artist(artist.into());
        tag.insert_text(ItemKey::Label, label.into());
        tag.insert_text(ItemKey::IntegerBpm, bpm.to_string());
        // A few tracks without a key, as in real libraries.
        if rng.next() % 15 != 0 {
            tag.insert_text(ItemKey::InitialKey, key);
        }
        // Most tracks get the artist's cover; some have none, like real libraries.
        if !folder_art && rng.next() % 6 != 0 {
            tag.push_picture(
                Picture::unchecked(cover(artist_ix))
                    .pic_type(PictureType::CoverFront)
                    .mime_type(MimeType::Png)
                    .build(),
            );
        }
        tag.save_to_path(&path, WriteOptions::default()).map_err(std::io::Error::other)?;
        paths.push(path);
    }

    for (name, n) in [("Warmup", 12), ("Peak Time", 15), ("After Hours", 9)] {
        if playlists::create(&root, name).is_err() {
            continue; // already exists
        }
        for _ in 0..n {
            let p = &paths[(rng.next() % paths.len() as u64) as usize];
            playlists::add(&root, name, p)?;
        }
    }
    println!("Wrote {count} tracks and 3 playlists to {}", root.display());
    Ok(())
}
