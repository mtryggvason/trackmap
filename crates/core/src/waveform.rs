//! Waveform overviews: the loudness peaks across a whole track, for drawing.
//!
//! Stored as one byte per bin (0–255) in `<root>/.trackmap/waves/`, keyed by the file's path,
//! size and modification time so an edited file gets a fresh waveform.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::audio::AudioStream;
use crate::cache::CACHE_DIR;
use crate::Track;

/// Bins in a waveform: plenty for a full-width bar on a large screen.
pub const BINS: usize = 1200;

/// Peak level per bin across the whole file, scaled so the loudest bins reach 1.
pub fn compute(path: &Path) -> Option<Vec<f32>> {
    let mut stream = AudioStream::open(path)?;
    let channels = stream.channels().max(1) as usize;
    // Peaks over small blocks first (we may not know the length up front), then resampled.
    const BLOCK: usize = 256;
    let mut blocks = Vec::new();
    let (mut peak, mut n) = (0.0f32, 0usize);
    let mut frame_peak = 0.0f32;
    for (i, s) in stream.by_ref().enumerate() {
        frame_peak = frame_peak.max(s.abs());
        if (i + 1) % channels == 0 {
            peak = peak.max(frame_peak);
            frame_peak = 0.0;
            n += 1;
            if n == BLOCK {
                blocks.push(peak);
                (peak, n) = (0.0, 0);
            }
        }
    }
    if n > 0 {
        blocks.push(peak);
    }
    if blocks.is_empty() {
        return None;
    }
    let mut bins: Vec<f32> = (0..BINS)
        .map(|b| {
            let lo = b * blocks.len() / BINS;
            let hi = ((b + 1) * blocks.len() / BINS).max(lo + 1).min(blocks.len());
            blocks[lo.min(blocks.len() - 1)..hi].iter().cloned().fold(0.0, f32::max)
        })
        .collect();
    // Normalise to a high percentile rather than the single loudest spike.
    let mut sorted = bins.clone();
    sorted.sort_by(f32::total_cmp);
    let reference = sorted[(sorted.len() * 99 / 100).min(sorted.len() - 1)].max(1e-6);
    for v in &mut bins {
        *v = (*v / reference).min(1.0);
    }
    Some(bins)
}

fn wave_path(root: &Path, track: &Track) -> PathBuf {
    let rel = track.path.strip_prefix(root).unwrap_or(&track.path);
    let key = format!("{}|{}|{:?}", rel.display(), track.size, track.modified);
    let hash = key.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x0100_0000_01b3));
    root.join(CACHE_DIR).join("waves").join(format!("{hash:016x}.bin"))
}

/// The track's waveform from the cache, computing and caching it if needed.
pub fn load_or_compute(root: &Path, track: &Track) -> Option<Vec<f32>> {
    let path = wave_path(root, track);
    if let Ok(bytes) = fs::read(&path) {
        if bytes.len() == BINS {
            return Some(bytes.iter().map(|b| *b as f32 / 255.0).collect());
        }
    }
    let wave = compute(&track.path)?;
    let _ = save(&path, &wave);
    Some(wave)
}

fn save(path: &Path, wave: &[f32]) -> io::Result<()> {
    fs::create_dir_all(path.parent().unwrap())?;
    let bytes: Vec<u8> = wave.iter().map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8).collect();
    let tmp = path.with_extension("bin.tmp");
    fs::write(&tmp, bytes)?;
    fs::rename(tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::read_track;

    /// Mono 16-bit WAV: loud for the first half, quiet for the second.
    fn loud_then_quiet(path: &Path) {
        let rate = 8000u32;
        let mut data = Vec::new();
        for i in 0..rate * 4 {
            let amp = if i < rate * 2 { 20000.0 } else { 2000.0 };
            let v = (amp * (i as f32 * 0.3).sin()) as i16;
            data.extend_from_slice(&v.to_le_bytes());
        }
        let mut b = b"RIFF".to_vec();
        b.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
        b.extend_from_slice(b"WAVEfmt ");
        b.extend_from_slice(&16u32.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&rate.to_le_bytes());
        b.extend_from_slice(&(rate * 2).to_le_bytes());
        b.extend_from_slice(&2u16.to_le_bytes());
        b.extend_from_slice(&16u16.to_le_bytes());
        b.extend_from_slice(b"data");
        b.extend_from_slice(&(data.len() as u32).to_le_bytes());
        b.extend_from_slice(&data);
        fs::write(path, b).unwrap();
    }

    #[test]
    fn waveform_follows_loudness() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.wav");
        loud_then_quiet(&p);
        let w = compute(&p).unwrap();
        assert_eq!(w.len(), BINS);
        let first = w[..BINS / 2 - 10].iter().sum::<f32>() / (BINS / 2 - 10) as f32;
        let second = w[BINS / 2 + 10..].iter().sum::<f32>() / (BINS / 2 - 10) as f32;
        assert!(first > 0.9, "loud half near full scale: {first}");
        assert!(second < 0.2, "quiet half low: {second}");
    }

    #[test]
    fn cached_after_first_time() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let p = root.join("a.wav");
        loud_then_quiet(&p);
        let track = read_track(&p);
        let first = load_or_compute(&root, &track).unwrap();
        let cached = wave_path(&root, &track);
        assert!(cached.exists());
        // Delete the audio: the cached waveform still loads.
        fs::remove_file(&p).unwrap();
        let again = load_or_compute(&root, &track).unwrap();
        assert!(first.iter().zip(&again).all(|(a, b)| (a - b).abs() < 0.01));
    }

    #[test]
    fn unreadable_files_have_no_waveform() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("bad.mp3");
        fs::write(&p, b"nope").unwrap();
        assert!(compute(&p).is_none());
    }
}
