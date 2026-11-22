//! Generates a demo library of short silent, tagged WAV files plus a few playlists.
//!
//!     cargo run -p trackmap-core --example demo_library -- /tmp/trackmap-demo [count]

use std::path::PathBuf;

use lofty::config::WriteOptions;
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

        let dir = root.join(artist);
        std::fs::create_dir_all(&dir)?;
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
