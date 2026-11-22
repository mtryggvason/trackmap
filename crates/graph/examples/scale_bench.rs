//! Times the work redone whenever the library changes, for a large synthetic library.
use std::time::Instant;
use trackmap_core::{search::track_text, Track};
use trackmap_graph::{Graph, BPM_BIN};

fn main() {
    for n in [1_000usize, 5_000, 20_000] {
        let tracks: Vec<Track> = (0..n)
            .map(|i| Track {
                path: format!("/music/{}/{i}.mp3", i % 400).into(),
                title: Some(format!("Track {i}")),
                artist: Some(format!("Artist {}", i % (n / 20).max(1))),
                album: Some(format!("Album {}", i % (n / 8).max(1))),
                label: Some(format!("Label {}", i % 150)),
                bpm: Some(100.0 + (i * 37 % 600) as f32 / 10.0),
                key: format!("{}{}", 1 + i % 12, if i % 3 == 0 { "B" } else { "A" }).parse().ok(),
                ..Default::default()
            })
            .collect();
        let t = Instant::now();
        let g = Graph::build(&tracks, &[]);
        let build = t.elapsed();
        let all: Vec<usize> = (0..n).collect();
        let t = Instant::now();
        let axis = g.axis_for(&all, BPM_BIN);
        let pos = g.positions(&g.layout, axis.as_ref());
        let layout = t.elapsed();
        let t = Instant::now();
        let index: Vec<String> = tracks.iter().map(track_text).collect();
        let search = t.elapsed();
        println!(
            "{n:>6} tracks: graph {:>6.1} ms · layout {:>6.1} ms · search index {:>6.1} ms ({} positions, {} chars)",
            build.as_secs_f64() * 1e3,
            layout.as_secs_f64() * 1e3,
            search.as_secs_f64() * 1e3,
            pos.len(),
            index.iter().map(|s| s.len()).sum::<usize>()
        );
    }
}
