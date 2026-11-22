//! Prints detected BPM (and the tagged BPM, if any) for audio files, with timing.
//!
//!     cargo run -p trackmap-core --example detect_bpm -- file1.mp3 file2.wav …

use std::time::Instant;

fn main() {
    for arg in std::env::args().skip(1) {
        let path = std::path::Path::new(&arg);
        let tagged = trackmap_core::scan::read_track(path).bpm;
        let t = Instant::now();
        let bpm = trackmap_core::tempo::detect_file(path);
        println!(
            "{:>7} detected  {:>7} tagged  {:>5} ms  {}",
            bpm.map(|b| format!("{b:.1}")).unwrap_or("-".into()),
            tagged.map(|b| format!("{b:.1}")).unwrap_or("-".into()),
            t.elapsed().as_millis(),
            path.file_name().unwrap().to_string_lossy()
        );
    }
}
