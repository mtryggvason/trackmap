//! Opening and streaming audio files: shared by analysis (decode to mono) and playback.

use std::fs::File;
use std::io::{Seek, SeekFrom};
use std::path::Path;

use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType};
use symphonia::core::io::{MediaSourceStream, ReadOnlySource};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::Time;

/// Size of a leading ID3v2 tag (header and footer included), if the file starts with one.
fn id3v2_len(path: &Path) -> Option<u64> {
    use std::io::Read;
    let mut h = [0u8; 10];
    File::open(path).ok()?.read_exact(&mut h).ok()?;
    if &h[..3] != b"ID3" {
        return None;
    }
    let size = h[6..10].iter().fold(0u64, |acc, b| (acc << 7) | (*b & 0x7f) as u64);
    let footer = if h[5] & 0x10 != 0 { 10 } else { 0 };
    Some(10 + size + footer)
}

/// Opens an audio file's container. Returns the reader and whether it can seek (it can't when
/// we had to skip a huge ID3 tag ourselves and read the rest as a plain stream).
pub fn open_format(path: &Path) -> Option<(Box<dyn FormatReader>, bool)> {
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let probe = |mss: MediaSourceStream<'static>| {
        symphonia::default::get_probe()
            .probe(&hint, mss, FormatOptions::default(), MetadataOptions::default())
            .ok()
    };
    let file = File::open(path).ok()?;
    if let Some(f) = probe(MediaSourceStream::new(Box::new(file), Default::default())) {
        return Some((f, true));
    }
    // The decoder can give up on MP3s with very large ID3 tags (big embedded artwork) before
    // reaching the audio. Skip the tag ourselves and try again.
    let skip = id3v2_len(path)?;
    let mut file = File::open(path).ok()?;
    file.seek(SeekFrom::Start(skip)).ok()?;
    let source = ReadOnlySource::new(file);
    probe(MediaSourceStream::new(Box::new(source), Default::default())).map(|f| (f, false))
}

/// A decoded audio file as a stream of interleaved f32 samples, with seeking.
pub struct AudioStream {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    track_id: u32,
    channels: u16,
    rate: u32,
    duration: Option<f64>,
    seekable: bool,
    buf: Vec<f32>,
    pos: usize,
}

impl AudioStream {
    pub fn open(path: &Path) -> Option<Self> {
        let (format, seekable) = open_format(path)?;
        let track = format.default_track(TrackType::Audio)?;
        let track_id = track.id;
        let params = track.codec_params.as_ref()?.audio()?.clone();
        let duration = match (track.num_frames, params.sample_rate) {
            (Some(n), Some(r)) if r > 0 => Some(n as f64 / r as f64),
            _ => None,
        };
        let decoder = symphonia::default::get_codecs()
            .make_audio_decoder(&params, &AudioDecoderOptions::default())
            .ok()?;
        let mut s = Self {
            format,
            decoder,
            track_id,
            channels: params.channels.as_ref().map_or(0, |c| c.count() as u16),
            rate: params.sample_rate.unwrap_or(0),
            duration,
            seekable,
            buf: Vec::new(),
            pos: 0,
        };
        // Some containers only reveal the format once a packet is decoded.
        if s.channels == 0 || s.rate == 0 {
            s.refill();
        }
        (s.channels > 0 && s.rate > 0).then_some(s)
    }

    pub fn channels(&self) -> u16 {
        self.channels
    }

    pub fn sample_rate(&self) -> u32 {
        self.rate
    }

    /// Length in seconds, if the file says.
    pub fn duration(&self) -> Option<f64> {
        self.duration
    }

    /// Decodes the next packet into the buffer. Returns false at the end of the stream.
    fn refill(&mut self) -> bool {
        loop {
            let packet = match self.format.next_packet() {
                Ok(Some(p)) => p,
                _ => return false,
            };
            if packet.track_id != self.track_id {
                continue;
            }
            match self.decoder.decode(&packet) {
                Ok(audio) => {
                    let spec = audio.spec();
                    self.channels = spec.channels().count() as u16;
                    self.rate = spec.rate();
                    self.buf.resize(audio.samples_interleaved(), 0.0);
                    audio.copy_to_slice_interleaved(&mut self.buf);
                    self.pos = 0;
                    if !self.buf.is_empty() {
                        return true;
                    }
                }
                Err(SymphoniaError::DecodeError(_)) => continue,
                Err(_) => return false,
            }
        }
    }

    /// Jumps to `secs` from the start. Returns false if the file can't seek there.
    pub fn seek(&mut self, secs: f64) -> bool {
        if !self.seekable {
            return false;
        }
        let secs = secs.max(0.0);
        let Some(time) = Time::try_new(secs.trunc() as i64, (secs.fract() * 1e9) as u32) else {
            return false;
        };
        let to = SeekTo::Time { time, track_id: Some(self.track_id) };
        if self.format.seek(SeekMode::Accurate, to).is_err() {
            return false;
        }
        self.decoder.reset();
        self.buf.clear();
        self.pos = 0;
        true
    }
}

impl Iterator for AudioStream {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        if self.pos >= self.buf.len() && !self.refill() {
            return None;
        }
        let s = self.buf[self.pos];
        self.pos += 1;
        Some(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stereo 16-bit WAV: left channel counts up, right is silent.
    fn ramp_wav(path: &Path, secs: u32, rate: u32) {
        let frames = secs * rate;
        let mut data = Vec::with_capacity(frames as usize * 4);
        for i in 0..frames {
            let v = ((i % 1000) as i16) * 10;
            data.extend_from_slice(&v.to_le_bytes());
            data.extend_from_slice(&0i16.to_le_bytes());
        }
        let mut b = Vec::new();
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
        b.extend_from_slice(b"WAVEfmt ");
        b.extend_from_slice(&16u32.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&2u16.to_le_bytes());
        b.extend_from_slice(&rate.to_le_bytes());
        b.extend_from_slice(&(rate * 4).to_le_bytes());
        b.extend_from_slice(&4u16.to_le_bytes());
        b.extend_from_slice(&16u16.to_le_bytes());
        b.extend_from_slice(b"data");
        b.extend_from_slice(&(data.len() as u32).to_le_bytes());
        b.extend_from_slice(&data);
        std::fs::write(path, b).unwrap();
    }

    #[test]
    fn streams_all_samples_with_format_and_duration() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("ramp.wav");
        ramp_wav(&p, 3, 8000);
        let s = AudioStream::open(&p).unwrap();
        assert_eq!((s.channels(), s.sample_rate()), (2, 8000));
        assert!((s.duration().unwrap() - 3.0).abs() < 0.01);
        let samples: Vec<f32> = s.collect();
        assert_eq!(samples.len(), 3 * 8000 * 2);
        assert!(samples.iter().skip(1).step_by(2).all(|v| *v == 0.0), "right channel silent");
    }

    #[test]
    fn seeks_to_a_time() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("ramp.wav");
        ramp_wav(&p, 4, 8000);
        let mut s = AudioStream::open(&p).unwrap();
        assert!(s.seek(2.0));
        let rest = s.count();
        // About two seconds of stereo samples remain.
        assert!((rest as i64 - 2 * 8000 * 2).abs() < 8000, "remaining {rest}");
    }

    #[test]
    fn missing_or_bad_files_dont_open() {
        let dir = tempfile::tempdir().unwrap();
        assert!(AudioStream::open(&dir.path().join("nope.mp3")).is_none());
        let bad = dir.path().join("bad.mp3");
        std::fs::write(&bad, b"not audio").unwrap();
        assert!(AudioStream::open(&bad).is_none());
    }
}
