//! Musical key detection from audio.
//!
//! 1. Decode up to ~2.5 minutes (skipping the intro), mix to mono, downsample.
//! 2. Chroma: per frame, the strongest spectral peak in each semitone over exactly five octaves
//!    (A2–A7), log-compressed, minus the frame's median (so broadband drum noise cancels out),
//!    folded onto the 12 pitch classes. Every note gets one value per octave, so neither FFT bin
//!    density nor a partial octave biases the result, and kick drums (below A2) are ignored.
//! 3. Correlate that profile with the Krumhansl–Kessler major and minor profiles in all 12
//!    transpositions; the best match is the key.

use std::path::{Path, PathBuf};

use rustfft::num_complex::Complex;
use rustfft::FftPlanner;

use crate::tempo::decode_mono;
use crate::{Camelot, Track};

const ANALYSIS_RATE: f32 = 11_025.0;
/// Long window for fine frequency resolution (~2.7 Hz per bin at 11 kHz).
const WINDOW: usize = 4096;
const HOP: usize = 2048;
const SKIP_SECS: f32 = 30.0;
const MAX_SECS: f32 = 150.0;
const MIN_SECS: f32 = 8.0;
/// A2 (110 Hz) up to, not including, A7: five whole octaves.
const LOW_MIDI: i32 = 45;
const SEMITONES: usize = 60;

/// Krumhansl–Kessler key profiles, starting from the tonic. (Temperley's profiles were tried
/// and did worse on real dance music.)
const MAJOR: [f32; 12] = [6.35, 2.23, 3.48, 2.33, 4.38, 4.09, 2.52, 5.19, 2.39, 3.66, 2.29, 2.88];
const MINOR: [f32; 12] = [6.33, 2.68, 3.52, 5.38, 2.60, 3.53, 2.54, 4.75, 3.98, 2.69, 3.34, 3.17];

/// Tracks with no key in their tags that haven't been analysed yet.
pub fn pending(tracks: &[Track]) -> Vec<PathBuf> {
    tracks
        .iter()
        .filter(|t| t.key.is_none() && !t.key_analyzed)
        .map(|t| t.path.clone())
        .collect()
}

/// Detects the key of an audio file. `None` if it can't be decoded or is silent.
pub fn detect_file(path: &Path) -> Option<Camelot> {
    let (samples, rate) = decode_mono(path, MAX_SECS + SKIP_SECS)?;
    let rate_f = rate as f32;
    let len_secs = samples.len() as f32 / rate_f;
    let start = if len_secs > SKIP_SECS + 45.0 { (SKIP_SECS * rate_f) as usize } else { 0 };
    detect(&samples[start..], rate)
}

/// The measured pitch-class profile of a file (C first), for inspecting what the detector hears.
pub fn file_profile(path: &Path) -> Option<[f32; 12]> {
    let (samples, rate) = decode_mono(path, MAX_SECS + SKIP_SECS)?;
    let start = if samples.len() as f32 / rate as f32 > SKIP_SECS + 45.0 {
        (SKIP_SECS * rate as f32) as usize
    } else {
        0
    };
    chroma(&samples[start..], rate)
}

/// Detects the key of mono samples at `rate` Hz.
pub fn detect(samples: &[f32], rate: u32) -> Option<Camelot> {
    let chroma = chroma(samples, rate)?;
    let mut best = (f32::MIN, Camelot::from_pitch_class(0, false));
    for tonic in 0..12u8 {
        for (minor, profile) in [(false, &MAJOR), (true, &MINOR)] {
            // Rotate the profile so index 0 lines up with pitch class `tonic`.
            let rotated: Vec<f32> = (0..12).map(|pc| profile[(pc + 12 - tonic as usize) % 12]).collect();
            let r = pearson(&chroma, &rotated);
            if r > best.0 {
                best = (r, Camelot::from_pitch_class(tonic, minor));
            }
        }
    }
    Some(best.1)
}

/// Summed chroma vector over the clip. `None` if too short or silent.
fn chroma(samples: &[f32], rate: u32) -> Option<[f32; 12]> {
    let factor = ((rate as f32 / ANALYSIS_RATE).round() as usize).max(1);
    let sr = rate as f32 / factor as f32;
    let x: Vec<f32> =
        samples.chunks_exact(factor).map(|c| c.iter().sum::<f32>() / factor as f32).collect();
    if (x.len() as f32) < MIN_SECS * sr || x.len() < WINDOW * 2 {
        return None;
    }

    // FFT bins in each semitone's band (±half a semitone around its centre).
    let bin_hz = sr / WINDOW as f32;
    let bands: Vec<std::ops::RangeInclusive<usize>> = (0..SEMITONES)
        .map(|j| {
            let midi = (LOW_MIDI + j as i32) as f32;
            let hz = |m: f32| 440.0 * 2f32.powf((m - 69.0) / 12.0);
            let lo = (hz(midi - 0.5) / bin_hz).ceil() as usize;
            let hi = ((hz(midi + 0.5) / bin_hz).floor() as usize).max(lo);
            lo..=hi
        })
        .collect();

    let fft = FftPlanner::<f32>::new().plan_fft_forward(WINDOW);
    let window: Vec<f32> = (0..WINDOW)
        .map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / WINDOW as f32).cos())
        .collect();
    let mut buf = vec![Complex::new(0.0, 0.0); WINDOW];
    let mut total = [0.0f32; 12];

    for start in (0..x.len() - WINDOW).step_by(HOP) {
        for (i, b) in buf.iter_mut().enumerate() {
            *b = Complex::new(x[start + i] * window[i], 0.0);
        }
        fft.process(&mut buf);
        let mut sem: Vec<f32> = bands
            .iter()
            .map(|band| {
                let peak = band.clone().map(|k| buf[k].norm()).fold(0.0f32, f32::max);
                (1.0 + 100.0 * peak).ln()
            })
            .collect();
        let mut sorted = sem.clone();
        sorted.sort_by(f32::total_cmp);
        let median = sorted[SEMITONES / 2];
        let mut frame = [0.0f32; 12];
        for (j, v) in sem.iter_mut().enumerate() {
            let pc = (LOW_MIDI as usize + j) % 12;
            frame[pc] += (*v - median).max(0.0);
        }
        // Normalise per frame so loud sections don't dominate.
        let sum: f32 = frame.iter().sum();
        if sum > 1e-6 {
            for (t, f) in total.iter_mut().zip(frame) {
                *t += f / sum;
            }
        }
    }
    (total.iter().sum::<f32>() > 1e-6).then_some(total)
}

fn pearson(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len() as f32;
    let (ma, mb) = (a.iter().sum::<f32>() / n, b.iter().sum::<f32>() / n);
    let (mut cov, mut va, mut vb) = (0.0, 0.0, 0.0);
    for (x, y) in a.iter().zip(b) {
        cov += (x - ma) * (y - mb);
        va += (x - ma).powi(2);
        vb += (y - mb).powi(2);
    }
    if va <= 0.0 || vb <= 0.0 { 0.0 } else { cov / (va.sqrt() * vb.sqrt()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 44_100;

    fn freq(midi: i32) -> f32 {
        440.0 * 2f32.powf((midi - 69) as f32 / 12.0)
    }

    /// Plays a chord progression (two seconds per chord, with bass and octave doubling).
    fn progression(chords: &[&[i32]], secs: f32) -> Vec<f32> {
        let n = (secs * RATE as f32) as usize;
        let per_chord = 2.0 * RATE as f32;
        (0..n)
            .map(|i| {
                let t = i as f32 / RATE as f32;
                let chord = chords[(i as f32 / per_chord) as usize % chords.len()];
                let mut s = 0.0;
                for (j, &m) in chord.iter().enumerate() {
                    let amp = if j == 0 { 0.5 } else { 0.25 };
                    s += amp * (std::f32::consts::TAU * freq(m) * t).sin();
                    s += amp * 0.3 * (std::f32::consts::TAU * freq(m + 12) * t).sin();
                }
                s * 0.3
            })
            .collect()
    }

    /// i–iv–v–i in a minor key, bass on the root (MIDI note numbers).
    fn minor_prog(root: i32) -> Vec<f32> {
        let tri = |r: i32| vec![r - 12, r, r + 3, r + 7];
        let (i, iv, v) = (tri(root), tri(root + 5), tri(root + 7));
        progression(&[&i, &iv, &v, &i], 24.0)
    }

    /// I–IV–V–I in a major key.
    fn major_prog(root: i32) -> Vec<f32> {
        let tri = |r: i32| vec![r - 12, r, r + 4, r + 7];
        let (i, iv, v) = (tri(root), tri(root + 5), tri(root + 7));
        progression(&[&i, &iv, &v, &i], 24.0)
    }

    fn k(s: &str) -> Camelot {
        s.parse().unwrap()
    }

    #[test]
    fn detects_minor_keys() {
        // A minor (8A), F# minor (11A), D minor (7A), G minor (6A)
        for (root, want) in [(57, "8A"), (54, "11A"), (50, "7A"), (55, "6A")] {
            assert_eq!(detect(&minor_prog(root), RATE), Some(k(want)), "root {root}");
        }
    }

    #[test]
    fn detects_major_keys() {
        // C major (8B), Eb major (5B), G major (9B), E major (12B)
        for (root, want) in [(60, "8B"), (51, "5B"), (55, "9B"), (52, "12B")] {
            assert_eq!(detect(&major_prog(root), RATE), Some(k(want)), "root {root}");
        }
    }

    #[test]
    fn silence_and_short_clips_give_nothing() {
        assert_eq!(detect(&vec![0.0; RATE as usize * 20], RATE), None);
        assert_eq!(detect(&minor_prog(57)[..RATE as usize * 3], RATE), None);
    }

    #[test]
    fn pending_skips_tagged_and_analysed() {
        let tracks = vec![
            Track { path: "/a".into(), ..Default::default() },
            Track { path: "/b".into(), key: Some(k("8A")), ..Default::default() },
            Track { path: "/c".into(), key_analyzed: true, ..Default::default() },
        ];
        assert_eq!(pending(&tracks), [PathBuf::from("/a")]);
    }
}
