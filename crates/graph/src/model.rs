//! The music graph: songs, artists, labels and playlists, and which of them are visible.

use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeSet, HashMap};
use std::hash::{Hash, Hasher};
use std::path::PathBuf;

use trackmap_core::{Playlist, Track};

use crate::{HarmonicLayout, Point};

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
    artist_pos: Vec<Point>,
    label_pos: Vec<Point>,
    playlist_pos: Vec<Point>,
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
    pub fn build(tracks: &[Track], playlists: &[Playlist], layout: &HarmonicLayout) -> Self {
        let mut g = Graph::default();
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

        g.artist_pos = g.artist_songs.iter().map(|s| g.orbit(s, 70.0)).collect();
        g.label_pos = g.label_songs.iter().map(|s| g.orbit(s, 140.0)).collect();
        g.playlist_pos = g.playlist_songs.iter().map(|s| g.orbit(s, 210.0)).collect();
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

    /// Songs belonging to an artist, label or playlist (empty for a song).
    pub fn songs_of(&self, n: NodeId) -> &[usize] {
        match n {
            NodeId::Song(_) => &[],
            NodeId::Artist(i) => &self.artist_songs[i],
            NodeId::Label(i) => &self.label_songs[i],
            NodeId::Playlist(i) => &self.playlist_songs[i],
        }
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

    pub fn position(&self, n: NodeId) -> Point {
        match n {
            NodeId::Song(i) => self.song_pos[i],
            NodeId::Artist(i) => self.artist_pos[i],
            NodeId::Label(i) => self.label_pos[i],
            NodeId::Playlist(i) => self.playlist_pos[i],
        }
    }

    /// What clicking a node reveals.
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

    /// Edges among a set of visible nodes (sorted, as returned by [`Graph::visible`]).
    pub fn edges(&self, visible: &[NodeId]) -> Vec<(NodeId, NodeId)> {
        let is_visible = |n: &NodeId| visible.binary_search(n).is_ok();
        let mut out = Vec::new();
        for &n in visible {
            match n {
                NodeId::Song(s) => {
                    for other in self.neighbours(NodeId::Song(s)) {
                        if is_visible(&other) {
                            out.push((n, other));
                        }
                    }
                }
                NodeId::Label(l) => {
                    for &a in &self.label_artists[l] {
                        if is_visible(&NodeId::Artist(a)) {
                            out.push((n, NodeId::Artist(a)));
                        }
                    }
                }
                _ => {}
            }
        }
        out
    }

    /// Visible nodes: the filtered songs, optionally every playlist/label, plus every expanded
    /// node and what it reveals. Returned sorted and without duplicates.
    pub fn visible(
        &self,
        songs: impl IntoIterator<Item = usize>,
        all_playlists: bool,
        all_labels: bool,
        expanded: &[NodeId],
    ) -> Vec<NodeId> {
        let mut set: BTreeSet<NodeId> = songs.into_iter().map(NodeId::Song).collect();
        if all_playlists {
            set.extend((0..self.playlists.len()).map(NodeId::Playlist));
        }
        if all_labels {
            set.extend((0..self.labels.len()).map(NodeId::Label));
        }
        for &n in expanded {
            set.insert(n);
            set.extend(self.neighbours(n));
        }
        set.into_iter().collect()
    }

    /// Places a group node just outside the centroid of its songs, pushed away from the centre.
    fn orbit(&self, songs: &[usize], distance: f32) -> Point {
        if songs.is_empty() {
            return Point { x: 0.0, y: -distance };
        }
        let n = songs.len() as f32;
        let (sx, sy) = songs.iter().fold((0.0, 0.0), |(x, y), &s| {
            (x + self.song_pos[s].x, y + self.song_pos[s].y)
        });
        let (cx, cy) = (sx / n, sy / n);
        let len = (cx * cx + cy * cy).sqrt();
        let (dx, dy) = if len < 1.0 { (0.0, -1.0) } else { (cx / len, cy / len) };
        Point { x: cx + dx * distance, y: cy + dy * distance }
    }
}

/// Harmonic position plus a small stable jitter, so songs with the same key and BPM don't
/// sit exactly on top of each other.
fn song_position(t: &Track, layout: &HarmonicLayout) -> Point {
    let mut h = DefaultHasher::new();
    t.path.hash(&mut h);
    let bits = h.finish();
    let j1 = (bits & 0xFFFF) as f32 / 65535.0 * 2.0 - 1.0;
    let j2 = ((bits >> 16) & 0xFFFF) as f32 / 65535.0 * 2.0 - 1.0;

    let p = layout.position(t.key, t.bpm);
    if t.key.is_none() {
        // Unknown key: a disc in the middle.
        return Point { x: j1 * layout.inner * 0.6, y: p.y + j2 * 20.0 };
    }
    // Rotate up to ±0.2 rad (the key wedge is ±0.26) and nudge the radius a little.
    let (r, a) = ((p.x * p.x + p.y * p.y).sqrt() + j2 * 12.0, p.y.atan2(p.x) + j1 * 0.2);
    Point { x: r * a.cos(), y: r * a.sin() }
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
        Graph::build(&tracks, &playlists, &HarmonicLayout::default())
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
        let g2 = Graph::build(&tracks, &[], &HarmonicLayout::default());
        assert_eq!(g2.find(&g.key(NodeId::Song(0))), Some(NodeId::Song(1)));
        assert_eq!(g2.find(&NodeKey::Playlist("Warmup".into())), None);
        assert_eq!(g2.songs_of(NodeId::Artist(1)), [1, 2, 3]);
        assert!(g2.songs_of(NodeId::Song(0)).is_empty());
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
    fn visibility_and_edges() {
        use NodeId::*;
        let g = sample();
        // Only the "new" song 2 is in the filter; expanding Ann reveals her songs too.
        let v = g.visible([2], false, false, &[Artist(0)]);
        assert_eq!(v, [Song(0), Song(1), Song(2), Artist(0)]);
        assert_eq!(g.edges(&v), [(Song(0), Artist(0)), (Song(1), Artist(0))]);

        let v = g.visible([], true, true, &[]);
        assert_eq!(v, [Label(0), Label(1), Playlist(0)]);
        assert!(g.edges(&v).is_empty());

        // Label -> artist edges once both are visible.
        let v = g.visible([], false, false, &[Label(0)]);
        assert_eq!(g.edges(&v), [(Label(0), Artist(0)), (Label(0), Artist(1))]);
    }

    #[test]
    fn positions_are_stable_and_compatible_keys_cluster() {
        let (a, b) = (sample(), sample());
        let d = |p: Point, q: Point| ((p.x - q.x).powi(2) + (p.y - q.y).powi(2)).sqrt();
        assert_eq!(a.position(NodeId::Song(0)), b.position(NodeId::Song(0)));
        // 8A and 9A are neighbours on the wheel; 2B is across it.
        let (s0, s1, s2) = (
            a.position(NodeId::Song(0)),
            a.position(NodeId::Song(1)),
            a.position(NodeId::Song(2)),
        );
        assert!(d(s0, s1) < d(s0, s2));
        // Ann's node sits outside the middle of her songs.
        let ann = a.position(NodeId::Artist(0));
        let mid = Point { x: (s0.x + s1.x) / 2.0, y: (s0.y + s1.y) / 2.0 };
        assert!(d(ann, mid) > 60.0 && d(ann, mid) < 80.0);
    }
}
