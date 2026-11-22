//! Audio preview: plays one track at a time through the default output device.

use std::num::NonZero;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rodio::source::SeekError;
use rodio::{MixerDeviceSink, Source};
use trackmap_core::audio::AudioStream;

/// Feeds a decoded file to rodio.
struct StreamSource(AudioStream);

impl Iterator for StreamSource {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        self.0.next()
    }
}

impl Source for StreamSource {
    fn current_span_len(&self) -> Option<usize> {
        None
    }

    fn channels(&self) -> rodio::ChannelCount {
        NonZero::new(self.0.channels()).unwrap_or(NonZero::<u16>::MIN)
    }

    fn sample_rate(&self) -> rodio::SampleRate {
        NonZero::new(self.0.sample_rate()).unwrap_or(NonZero::<u32>::MIN)
    }

    fn total_duration(&self) -> Option<Duration> {
        self.0.duration().map(Duration::from_secs_f64)
    }

    fn try_seek(&mut self, pos: Duration) -> Result<(), SeekError> {
        if self.0.seek(pos.as_secs_f64()) {
            Ok(())
        } else {
            Err(SeekError::NotSupported { underlying_source: "this file" })
        }
    }
}

#[derive(Default)]
pub struct Player {
    /// The output device, opened on first use and kept open.
    output: Option<MixerDeviceSink>,
    current: Option<rodio::Player>,
    /// The loaded track and its length (if known).
    pub track: Option<PathBuf>,
    pub duration: Option<Duration>,
    /// Why the last play attempt failed, for showing in the UI.
    pub error: Option<String>,
}

impl Player {
    /// Starts playing `path` from the beginning, replacing whatever was playing.
    pub fn play(&mut self, path: &Path) {
        self.stop();
        self.error = None;
        if self.output.is_none() {
            match rodio::DeviceSinkBuilder::open_default_sink() {
                Ok(mut sink) => {
                    sink.log_on_drop(false);
                    self.output = Some(sink);
                }
                Err(e) => {
                    self.error = Some(format!("No audio output: {e}"));
                    return;
                }
            }
        }
        let Some(stream) = AudioStream::open(path) else {
            self.error = Some("Can't play this file".into());
            return;
        };
        let source = StreamSource(stream);
        self.duration = source.total_duration();
        let player = rodio::Player::connect_new(self.output.as_ref().unwrap().mixer());
        player.append(source);
        self.current = Some(player);
        self.track = Some(path.to_path_buf());
    }

    pub fn stop(&mut self) {
        if let Some(p) = self.current.take() {
            p.stop();
        }
        self.track = None;
        self.duration = None;
    }

    pub fn is_playing(&self) -> bool {
        self.current.as_ref().is_some_and(|p| !p.is_paused() && !p.empty())
    }

    /// Whether a track is loaded but has played to its end.
    pub fn finished(&self) -> bool {
        self.current.as_ref().is_some_and(|p| p.empty())
    }

    pub fn toggle(&mut self) {
        if let Some(p) = &self.current {
            if p.is_paused() {
                p.play();
            } else {
                p.pause();
            }
        }
    }

    pub fn position(&self) -> Duration {
        self.current.as_ref().map_or(Duration::ZERO, |p| p.get_pos())
    }

    /// Jumps to `pos` (clamped to the track). Some files can't seek; then nothing happens.
    pub fn seek(&mut self, pos: Duration) {
        let pos = match self.duration {
            Some(d) => pos.min(d.saturating_sub(Duration::from_millis(200))),
            None => pos,
        };
        if let Some(p) = &self.current {
            if p.try_seek(pos).is_err() {
                self.error = Some("Can't seek in this file".into());
            }
        }
    }

    /// Moves by `secs` (negative goes back).
    pub fn skip(&mut self, secs: f64) {
        let now = self.position().as_secs_f64();
        self.seek(Duration::from_secs_f64((now + secs).max(0.0)));
    }
}

/// "m:ss" for a duration.
pub fn clock(d: Duration) -> String {
    let s = d.as_secs();
    format!("{}:{:02}", s / 60, s % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_format() {
        assert_eq!(clock(Duration::from_secs(0)), "0:00");
        assert_eq!(clock(Duration::from_secs(83)), "1:23");
        assert_eq!(clock(Duration::from_secs(400)), "6:40");
    }

    /// One second of 16-bit stereo silence at 22.05 kHz.
    fn wav(path: &Path, secs: u32) {
        let rate = 22_050u32;
        let data = vec![0u8; (rate * secs * 4) as usize];
        let mut b = b"RIFF".to_vec();
        b.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
        b.extend_from_slice(b"WAVEfmt ");
        for v in [16u32] { b.extend_from_slice(&v.to_le_bytes()); }
        for v in [1u16, 2] { b.extend_from_slice(&v.to_le_bytes()); }
        for v in [rate, rate * 4] { b.extend_from_slice(&v.to_le_bytes()); }
        for v in [4u16, 16] { b.extend_from_slice(&v.to_le_bytes()); }
        b.extend_from_slice(b"data");
        b.extend_from_slice(&(data.len() as u32).to_le_bytes());
        b.extend_from_slice(&data);
        std::fs::write(path, b).unwrap();
    }

    #[test]
    fn the_source_rodio_plays_reports_format_and_seeks() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.wav");
        wav(&p, 3);
        let mut src = StreamSource(AudioStream::open(&p).unwrap());
        assert_eq!((src.channels().get(), src.sample_rate().get()), (2, 22_050));
        assert_eq!(src.total_duration().map(|d| d.as_secs()), Some(3));
        assert!(src.try_seek(Duration::from_secs(2)).is_ok());
        let rest = src.count();
        assert!((rest as i64 - 22_050 * 2).abs() < 22_050 / 2, "about one second left: {rest}");
    }

    #[test]
    fn nothing_loaded_is_quiet() {
        let mut p = Player::default();
        assert!(!p.is_playing() && !p.finished());
        p.toggle();
        p.skip(10.0);
        assert_eq!(p.position(), Duration::ZERO);
    }
}
