//! The music graph. Only songs are drawn; artists, labels and playlists are groups of songs
//! that can be selected to highlight their members.

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;

use trackmap_core::{Playlist, Track};

use crate::{MapLayout, Point};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum NodeId {
    /// Index into the library's tracks.
    Song(usize),
    Artist(usize),
    Label(usize),
    Playlist(usize),
}

/// A node identity that survives rebuilding the graph (indices shift when tracks are added).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum NodeKey {
    Song(PathBuf),
    Artist(String),
    Label(String),
    Playlist(String),
}

#[derive(Debug, Default)]
pub struct Graph {
    /// The layout songs were placed with (BPM axis fitted to the library).
    pub layout: MapLayout,
    pub song_titles: Vec<String>,
    pub song_paths: Vec<PathBuf>,
    pub artists: Vec<String>,
    pub labels: Vec<String>,
    pub playlists: Vec<String>,

    song_artist: Vec<Option<usize>>,
    song_label: Vec<Option<usize>>,
    song_playlists: Vec<Vec<usize>>,
    artist_songs: Vec<Vec<usize>>,
    label_songs: Vec<Vec<usize>>,
    label_artists: Vec<Vec<usize>>,
    playlist_songs: Vec<Vec<usize>>,

    song_pos: Vec<Point>,
}

/// Interns strings into indices, keeping first-seen order.
fn intern(map: &mut HashMap<String, usize>, names: &mut Vec<String>, s: &str) -> usize {
    *map.entry(s.to_string()).or_insert_with(|| {
        names.push(s.to_string());
        names.len() - 1
    })
}

fn tag(s: &Option<String>) -> Option<&str> {
    s.as_deref().map(str::trim).filter(|s| !s.is_empty())
}

impl Graph {
    pub fn build(tracks: &[Track], playlists: &[Playlist]) -> Self {
        let layout = &MapLayout::fit(tracks.iter().filter_map(|t| t.bpm));
        let mut g = Graph { layout: *layout, ..Graph::default() };
        let (mut artist_ix, mut label_ix) = (HashMap::new(), HashMap::new());

        for t in tracks {
            g.song_titles.push(t.title.clone().unwrap_or_default());
            g.song_paths.push(t.path.clone());
            let a = tag(&t.artist).map(|s| intern(&mut artist_ix, &mut g.artists, s));
            let l = tag(&t.label).map(|s| intern(&mut label_ix, &mut g.labels, s));
            g.song_artist.push(a);
            g.song_label.push(l);
            g.song_playlists.push(Vec::new());
            g.song_pos.push(song_position(t, layout));
        }

        g.artist_songs = vec![Vec::new(); g.artists.len()];
        g.label_songs = vec![Vec::new(); g.labels.len()];
        g.label_artists = vec![Vec::new(); g.labels.len()];
        for s in 0..tracks.len() {
            if let Some(a) = g.song_artist[s] {
                g.artist_songs[a].push(s);
            }
            if let Some(l) = g.song_label[s] {
                g.label_songs[l].push(s);
                if let Some(a) = g.song_artist[s] {
                    if !g.label_artists[l].contains(&a) {
                        g.label_artists[l].push(a);
                    }
                }
            }
        }

        let by_path: HashMap<&PathBuf, usize> =
            tracks.iter().enumerate().map(|(i, t)| (&t.path, i)).collect();
        for (p, pl) in playlists.iter().enumerate() {
            g.playlists.push(pl.name.clone());
            let songs: Vec<usize> =
                pl.entries.iter().filter_map(|e| by_path.get(&e.target).copied()).collect();
            for &s in &songs {
                g.song_playlists[s].push(p);
            }
            g.playlist_songs.push(songs);
        }
        g
    }

    pub fn key(&self, n: NodeId) -> NodeKey {
        match n {
            NodeId::Song(i) => NodeKey::Song(self.song_paths[i].clone()),
            NodeId::Artist(i) => NodeKey::Artist(self.artists[i].clone()),
            NodeId::Label(i) => NodeKey::Label(self.labels[i].clone()),
            NodeId::Playlist(i) => NodeKey::Playlist(self.playlists[i].clone()),
        }
    }

    /// Finds a node by key; `None` if it no longer exists.
    pub fn find(&self, key: &NodeKey) -> Option<NodeId> {
        let pos = |v: &[String], s: &str| v.iter().position(|x| x == s);
        match key {
            NodeKey::Song(p) => self.song_paths.iter().position(|x| x == p).map(NodeId::Song),
            NodeKey::Artist(s) => pos(&self.artists, s).map(NodeId::Artist),
            NodeKey::Label(s) => pos(&self.labels, s).map(NodeId::Label),
            NodeKey::Playlist(s) => pos(&self.playlists, s).map(NodeId::Playlist),
        }
    }

    pub fn artist_of(&self, song: usize) -> Option<usize> {
        self.song_artist[song]
    }

    pub fn label_of(&self, song: usize) -> Option<usize> {
        self.song_label[song]
    }

    pub fn playlists_of(&self, song: usize) -> &[usize] {
        &self.song_playlists[song]
    }

    /// The songs a selection highlights: the song itself, or a group's songs
    /// (a playlist's in play order).
    pub fn songs_of(&self, n: NodeId) -> Vec<usize> {
        let v: &[usize] = match n {
            NodeId::Song(i) => return vec![i],
            NodeId::Artist(i) => &self.artist_songs[i],
            NodeId::Label(i) => &self.label_songs[i],
            NodeId::Playlist(i) => &self.playlist_songs[i],
        };
        v.to_vec()
    }

    pub fn song_count(&self) -> usize {
        self.song_titles.len()
    }

    pub fn name(&self, n: NodeId) -> &str {
        match n {
            NodeId::Song(i) => &self.song_titles[i],
            NodeId::Artist(i) => &self.artists[i],
            NodeId::Label(i) => &self.labels[i],
            NodeId::Playlist(i) => &self.playlists[i],
        }
    }

    pub fn song_position(&self, song: usize) -> Point {
        self.song_pos[song]
    }

    /// What the inspector lists for a selection.
    /// Song: its artist, label and playlists. Artist: its songs. Label: its artists.
    /// Playlist: its songs.
    pub fn neighbours(&self, n: NodeId) -> Vec<NodeId> {
        match n {
            NodeId::Song(s) => self.song_artist[s]
                .map(NodeId::Artist)
                .into_iter()
                .chain(self.song_label[s].map(NodeId::Label))
                .chain(self.song_playlists[s].iter().map(|&p| NodeId::Playlist(p)))
                .collect(),
            NodeId::Artist(a) => self.artist_songs[a].iter().map(|&s| NodeId::Song(s)).collect(),
            NodeId::Label(l) => self.label_artists[l].iter().map(|&a| NodeId::Artist(a)).collect(),
            NodeId::Playlist(p) => {
                self.playlist_songs[p].iter().map(|&s| NodeId::Song(s)).collect()
            }
        }
    }

}

/// Layout position plus a small stable jitter, so songs with the same key and BPM don't
/// sit exactly on top of each other.
fn song_position(t: &Track, layout: &MapLayout) -> Point {
    let mut h = DefaultHasher::new();
    t.path.hash(&mut h);
    let bits = h.finish();
    let j1 = (bits & 0xFFFF) as f32 / 65535.0 * 2.0 - 1.0;
    let j2 = ((bits >> 16) & 0xFFFF) as f32 / 65535.0 * 2.0 - 1.0;

    let p = layout.position(t.key, t.bpm);
    // Stay well inside the key row; a few world units sideways is a fraction of a BPM.
    Point { x: p.x + j1 * 6.0, y: p.y + j2 * layout.row_height() * 0.35 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use trackmap_core::playlists::Entry;

    fn track(path: &str, artist: &str, label: &str, key: &str, bpm: f32) -> Track {
        Track {
            path: path.into(),
            title: Some(path.into()),
            artist: Some(artist.into()).filter(|s: &String| !s.is_empty()),
            label: Some(label.into()).filter(|s: &String| !s.is_empty()),
            key: key.parse().ok(),
            bpm: Some(bpm),
            ..Default::default()
        }
    }

    fn sample() -> Graph {
        let tracks = vec![
            track("/a", "Ann", "Lab1", "8A", 124.0),
            track("/b", "Ann", "Lab2", "9A", 126.0),
            track("/c", "Bob", "Lab1", "2B", 140.0),
            track("/d", "", "", "", 0.0),
        ];
        let playlists = vec![Playlist {
            name: "Warmup".into(),
            entries: ["/c", "/a", "/missing"]
                .iter()
                .map(|p| Entry { link: p.into(), target: p.into(), missing: false })
                .collect(),
        }];
        Graph::build(&tracks, &playlists)
    }

    #[test]
    fn keys_survive_rebuilds() {
        let g = sample();
        for n in [NodeId::Song(2), NodeId::Artist(1), NodeId::Label(1), NodeId::Playlist(0)] {
            assert_eq!(g.find(&g.key(n)), Some(n));
        }
        // A rebuild with an extra track first shifts song indices, but keys still resolve.
        let mut tracks = vec![track("/0", "Zed", "", "1A", 120.0)];
        tracks.extend(["/a", "/b", "/c"].map(|p| track(p, "Ann", "Lab1", "8A", 124.0)));
        let g2 = Graph::build(&tracks, &[]);
        assert_eq!(g2.find(&g.key(NodeId::Song(0))), Some(NodeId::Song(1)));
        assert_eq!(g2.find(&NodeKey::Playlist("Warmup".into())), None);
        assert_eq!(g2.songs_of(NodeId::Artist(1)), [1, 2, 3]);
        assert_eq!(g2.songs_of(NodeId::Song(0)), [0]);
    }

    #[test]
    fn interns_entities() {
        let g = sample();
        assert_eq!(g.artists, ["Ann", "Bob"]);
        assert_eq!(g.labels, ["Lab1", "Lab2"]);
        assert_eq!(g.playlists, ["Warmup"]);
    }

    #[test]
    fn neighbours_follow_the_click_rules() {
        use NodeId::*;
        let g = sample();
        assert_eq!(g.neighbours(Song(0)), [Artist(0), Label(0), Playlist(0)]);
        assert_eq!(g.neighbours(Song(3)), []);
        assert_eq!(g.neighbours(Artist(0)), [Song(0), Song(1)]);
        assert_eq!(g.neighbours(Label(0)), [Artist(0), Artist(1)]);
        assert_eq!(g.neighbours(Playlist(0)), [Song(2), Song(0)], "missing tracks are skipped");
    }

    #[test]
    fn selections_highlight_songs() {
        use NodeId::*;
        let g = sample();
        assert_eq!(g.songs_of(Song(3)), [3]);
        assert_eq!(g.songs_of(Artist(0)), [0, 1]);
        assert_eq!(g.songs_of(Label(0)), [0, 2]);
        assert_eq!(g.songs_of(Playlist(0)), [2, 0], "playlists keep play order");
    }

    #[test]
    fn positions_are_stable_and_tempo_dominates() {
        let (a, b) = (sample(), sample());
        let d = |p: Point, q: Point| ((p.x - q.x).powi(2) + (p.y - q.y).powi(2)).sqrt();
        assert_eq!(a.song_position(0), b.song_position(0));
        // 8A and 9A are neighbours on the wheel; 2B is across it.
        let (s0, s1, s2) = (a.song_position(0), a.song_position(1), a.song_position(2));
        assert!(d(s0, s1) < d(s0, s2));
    }
}
