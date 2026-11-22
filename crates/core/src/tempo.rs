//! Tempo (BPM) detection from audio.
//!
//! 1. Decode up to ~2.5 minutes, mix to mono, skip the intro.
//! 2. Onset strength: how much the (log-magnitude) spectrum rises from frame to frame.
//! 3. Autocorrelate that curve and score each candidate tempo by how well beats repeat at
//!    1–4 beat intervals, with a mild preference for 100–140 BPM to settle half/double tempo.

use std::path::{Path, PathBuf};

use rustfft::num_complex::Complex;
use rustfft::FftPlanner;
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::TrackType;

use crate::Track;

/// Tempo range considered. Anything outside is folded in by the octave logic.
pub const MIN_BPM: f32 = 70.0;
pub const MAX_BPM: f32 = 180.0;

const ANALYSIS_RATE: f32 = 11_025.0;
const WINDOW: usize = 1024;
const HOP: usize = 64;
/// Skip this much of the start (intros are often beatless), when the track is long enough.
const SKIP_SECS: f32 = 30.0;
/// Analyse at most this much audio.
const MAX_SECS: f32 = 150.0;
/// Need at least this much audio to say anything.
const MIN_SECS: f32 = 8.0;

/// Tracks with no BPM from their tags that haven't been analysed yet.
pub fn pending(tracks: &[Track]) -> Vec<PathBuf> {
    tracks
        .iter()
        .filter(|t| t.bpm.is_none() && !t.analyzed)
        .map(|t| t.path.clone())
        .collect()
}

/// Detects the tempo of an audio file. `None` if it can't be decoded or has no clear beat.
pub fn detect_file(path: &Path) -> Option<f32> {
    let (samples, rate) = decode_mono(path, MAX_SECS + SKIP_SECS)?;
    let rate_f = rate as f32;
    let len_secs = samples.len() as f32 / rate_f;
    let start = if len_secs > SKIP_SECS + 45.0 { (SKIP_SECS * rate_f) as usize } else { 0 };
    detect(&samples[start..], rate)
}

/// Decodes up to `max_secs` of audio as mono f32.
pub fn decode_mono(path: &Path, max_secs: f32) -> Option<(Vec<f32>, u32)> {
    let (mut format, _) = crate::audio::open_format(path)?;
    let track = format.default_track(TrackType::Audio)?;
    let track_id = track.id;
    let params = track.codec_params.as_ref()?.audio()?.clone();
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(&params, &AudioDecoderOptions::default())
        .ok()?;

    let mut mono = Vec::new();
    let mut rate = 0u32;
    let mut interleaved: Vec<f32> = Vec::new();
    loop {
        let packet = match format.next_packet() {
            Ok(Some(p)) => p,
            _ => break,
        };
        if packet.track_id != track_id {
            continue;
        }
        let buf = match decoder.decode(&packet) {
            Ok(b) => b,
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(_) => break,
        };
        let channels = buf.spec().channels().count().max(1);
        rate = buf.spec().rate();
        interleaved.resize(buf.samples_interleaved(), 0.0);
        buf.copy_to_slice_interleaved(&mut interleaved);
        mono.extend(
            interleaved.chunks(channels).map(|f| f.iter().sum::<f32>() / channels as f32),
        );
        if rate > 0 && mono.len() as f32 >= max_secs * rate as f32 {
            break;
        }
    }
    (rate > 0 && !mono.is_empty()).then_some((mono, rate))
}

/// Detects the tempo of mono samples at `rate` Hz.
pub fn detect(samples: &[f32], rate: u32) -> Option<f32> {
    let (env, fps) = onset_envelope(samples, rate)?;
    let lag = |bpm: f32| 60.0 * fps / bpm;
    let max_lag = (4.0 * lag(MIN_BPM)).ceil() as usize + 2;
    if env.len() < max_lag * 2 {
        return None;
    }
    let ac = autocorrelation(&env, max_lag);
    if ac[0] <= f32::EPSILON {
        return None; // silence
    }

    let score = |bpm: f32| {
        let l = lag(bpm);
        let comb: f32 = [1.0, 0.5, 0.33, 0.25]
            .iter()
            .enumerate()
            .map(|(m, w)| w * interp(&ac, l * (m + 1) as f32))
            .sum();
        // Mild preference for dance tempos; ~0.6 octave wide.
        let prior = (-0.5 * ((bpm / 125.0).log2() / 0.6).powi(2)).exp();
        comb * prior
    };

    let (mut best, mut best_score) = (0.0, f32::MIN);
    let mut bpm = MIN_BPM;
    while bpm <= MAX_BPM {
        let s = score(bpm);
        if s > best_score {
            (best, best_score) = (bpm, s);
        }
        bpm += 0.05;
    }
    if best_score <= 0.0 {
        return None;
    }
    // Most electronic music sits on whole numbers; snap when we're close.
    let rounded = best.round();
    Some(if (best - rounded).abs() < 0.25 { rounded } else { (best * 10.0).round() / 10.0 })
}

/// Spectral-flux onset strength, locally normalised. Returns the curve and its frame rate.
fn onset_envelope(samples: &[f32], rate: u32) -> Option<(Vec<f32>, f32)> {
    // Cheap downsample by averaging blocks (also acts as a low-pass filter).
    let factor = ((rate as f32 / ANALYSIS_RATE).round() as usize).max(1);
    let sr = rate as f32 / factor as f32;
    let x: Vec<f32> =
        samples.chunks_exact(factor).map(|c| c.iter().sum::<f32>() / factor as f32).collect();
    if (x.len() as f32) < MIN_SECS * sr || x.len() < WINDOW * 2 {
        return None;
    }

    let fft = FftPlanner::<f32>::new().plan_fft_forward(WINDOW);
    let window: Vec<f32> = (0..WINDOW)
        .map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / WINDOW as f32).cos())
        .collect();
    let bins = WINDOW / 2;
    let mut prev = vec![0.0f32; bins];
    let mut buf = vec![Complex::new(0.0, 0.0); WINDOW];
    let mut flux = Vec::with_capacity(x.len() / HOP);

    for start in (0..x.len() - WINDOW).step_by(HOP) {
        for (i, b) in buf.iter_mut().enumerate() {
            *b = Complex::new(x[start + i] * window[i], 0.0);
        }
        fft.process(&mut buf);
        let mut f = 0.0;
        for (k, p) in prev.iter_mut().enumerate() {
            let mag = (1.0 + 100.0 * buf[k].norm()).ln();
            f += (mag - *p).max(0.0);
            *p = mag;
        }
        flux.push(f);
    }
    flux[0] = 0.0; // the first frame "rises" from nothing

    // Subtract a ~1 s moving average so slow loudness changes don't count as beats.
    let fps = sr / HOP as f32;
    let half = (fps / 2.0) as usize;
    let mut prefix = vec![0.0f64; flux.len() + 1];
    for (i, v) in flux.iter().enumerate() {
        prefix[i + 1] = prefix[i] + *v as f64;
    }
    let env: Vec<f32> = (0..flux.len())
        .map(|i| {
            let (a, b) = (i.saturating_sub(half), (i + half + 1).min(flux.len()));
            let mean = ((prefix[b] - prefix[a]) / (b - a) as f64) as f32;
            (flux[i] - mean).max(0.0)
        })
        .collect();
    Some((env, fps))
}

/// Normalised autocorrelation for lags 0..=max_lag.
fn autocorrelation(env: &[f32], max_lag: usize) -> Vec<f32> {
    let n = env.len();
    (0..=max_lag)
        .map(|lag| {
            let s: f32 = env[..n - lag].iter().zip(&env[lag..]).map(|(a, b)| a * b).sum();
            s / (n - lag) as f32
        })
        .collect()
}

/// Linear interpolation into `v` at a fractional index (0 beyond the end).
fn interp(v: &[f32], x: f32) -> f32 {
    let i = x.floor() as usize;
    if i + 1 >= v.len() {
        return 0.0;
    }
    let t = x - i as f32;
    v[i] * (1.0 - t) + v[i + 1] * t
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 44_100;

    /// A drum pattern: kick on every beat, hi-hat on the off-beats, a little noise floor.
    fn pattern(bpm: f32, secs: f32) -> Vec<f32> {
        let n = (secs * RATE as f32) as usize;
        let beat = 60.0 / bpm * RATE as f32;
        let mut seed = 0x1234_5678u32;
        let mut noise = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            (seed as f32 / u32::MAX as f32) * 2.0 - 1.0
        };
        let mut out = vec![0.0f32; n];
        for (i, s) in out.iter_mut().enumerate() {
            let pos = i as f32 % beat;
            let t = pos / RATE as f32;
            let kick = (std::f32::consts::TAU * 55.0 * t).sin() * (-t * 25.0).exp();
            let off = (i as f32 + beat / 2.0) % beat / RATE as f32;
            let hat = noise() * 0.3 * (-off * 120.0).exp();
            *s = 0.8 * kick + hat + noise() * 0.01;
        }
        out
    }

    #[test]
    fn detects_common_dance_tempos() {
        for bpm in [90.0, 118.0, 122.0, 124.0, 128.0, 133.0, 140.0, 150.0, 172.0] {
            let got = detect(&pattern(bpm, 40.0), RATE).expect("a tempo");
            assert!((got - bpm).abs() <= 0.5, "expected {bpm}, got {got}");
        }
    }

    #[test]
    fn keeps_fractional_tempos() {
        let got = detect(&pattern(123.5, 40.0), RATE).unwrap();
        assert!((got - 123.5).abs() <= 0.3, "got {got}");
    }

    #[test]
    fn silence_and_short_clips_give_nothing() {
        assert_eq!(detect(&vec![0.0; RATE as usize * 30], RATE), None);
        assert_eq!(detect(&pattern(124.0, 3.0), RATE), None);
    }

    #[test]
    fn works_at_other_sample_rates() {
        // 48 kHz audio: resample the 44.1 kHz pattern crudely by stretching.
        let src = pattern(126.0, 40.0);
        let ratio = 48_000.0 / RATE as f32;
        let out: Vec<f32> =
            (0..(src.len() as f32 * ratio) as usize).map(|i| src[(i as f32 / ratio) as usize]).collect();
        let got = detect(&out, 48_000).unwrap();
        assert!((got - 126.0).abs() <= 0.5, "got {got}");
    }

    #[test]
    fn decodes_mp3_behind_a_huge_id3_tag() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("big-art.mp3");
        // 3 MB of tag padding, like a track with large embedded artwork.
        std::fs::write(&p, crate::tags::tests::mp3_with_id3(&[], 3_000_000)).unwrap();
        let (samples, rate) = decode_mono(&p, 60.0).expect("decodes past the tag");
        assert_eq!(rate, 44_100);
        assert!(samples.len() > 100_000);
    }

    #[test]
    fn pending_skips_tagged_and_analysed() {
        let tracks = vec![
            Track { path: "/a".into(), ..Default::default() },
            Track { path: "/b".into(), bpm: Some(124.0), ..Default::default() },
            Track { path: "/c".into(), analyzed: true, ..Default::default() },
        ];
        assert_eq!(pending(&tracks), [PathBuf::from("/a")]);
    }
}
