//! Times search over a large synthetic library: building the index, then a query per keystroke.
use std::time::Instant;
use trackmap_core::search::{track_text, Query};
use trackmap_core::Track;

fn main() {
    let words = ["Night", "Drive", "Glass", "Signal", "Harbour", "Pulse", "Echo", "Velvet", "Jónfrí", "Bloom", "Orbit", "Tide"];
    let tracks: Vec<Track> = (0..20_000)
        .map(|i| Track {
            path: format!("/music/{i}/{} {}.mp3", words[i % 12], words[(i / 12) % 12]).into(),
            title: Some(format!("{} {} ({} Remix)", words[i % 12], words[(i * 7) % 12], words[(i * 3) % 12])),
            artist: Some(format!("Artist {}", i % 900)),
            album: Some(format!("Album {}", i % 2500)),
            label: Some(format!("Label {}", i % 120)),
            genre: Some("House".into()),
            ..Default::default()
        })
        .collect();
    let t = Instant::now();
    let index: Vec<String> = tracks.iter().map(track_text).collect();
    println!("index 20,000 tracks: {:.1} ms (once per library change)", t.elapsed().as_secs_f64() * 1e3);
    for q in ["n", "ni", "nig", "night", "night dr", "jonfri", "artist 42", "zzz"] {
        let t = Instant::now();
        let query = Query::new(q);
        let hits = index.iter().filter(|s| query.matches(s)).count();
        println!("query {:<10} {:>6} hits  {:.2} ms", format!("{q:?}"), hits, t.elapsed().as_secs_f64() * 1e3);
    }
}
