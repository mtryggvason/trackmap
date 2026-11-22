//! Prints detected BPM and key (and the tagged values, if any) for audio files, with timing,
//! and how long a waveform takes.
//!
//!     cargo run -p trackmap-core --example analyze -- file1.mp3 file2.wav …

use std::time::Instant;

fn main() {
    for arg in std::env::args().skip(1) {
        let path = std::path::Path::new(&arg);
        let tagged = trackmap_core::scan::read_track(path);
        let t = Instant::now();
        let bpm = trackmap_core::tempo::detect_file(path);
        let bpm_ms = t.elapsed().as_millis();
        let t = Instant::now();
        let key = trackmap_core::key_detect::detect_file(path);
        let key_ms = t.elapsed().as_millis();
        let t = Instant::now();
        let wave = trackmap_core::waveform::compute(path);
        eprintln!("waveform: {} bins in {} ms", wave.map_or(0, |w| w.len()), t.elapsed().as_millis());
        let show = |v: Option<String>| v.unwrap_or("-".into());
        println!(
            "BPM {:>6} (tag {:>6}) {:>4} ms | key {:>4} (tag {:>4}) {:>4} ms | {}",
            show(bpm.map(|b| format!("{b:.1}"))),
            show(tagged.bpm.map(|b| format!("{b:.1}"))),
            bpm_ms,
            show(key.map(|k| k.to_string())),
            show(tagged.key.map(|k| k.to_string())),
            key_ms,
            path.file_name().unwrap().to_string_lossy()
        );
    }
}
