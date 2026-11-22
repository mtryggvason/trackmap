//! The music graph. Only songs are drawn; artists, labels and playlists are groups of songs
//! that can be selected to highlight their members.

use std::collections::{BTreeMap, HashMap};

use std::path::PathBuf;

use trackmap_core::{Camelot, Playlist, Track};

use crate::{Axis, MapLayout, Point};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum NodeId {
    /// Index into the library's tracks.
    Song(usize),
    Artist(usize),
    Label(usize),
    Playlist(usize),
    Album(usize),
}

/// A node identity that survives rebuilding the graph (indices shift when tracks are added).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum NodeKey {
    Song(PathBuf),
    Artist(String),
    Label(String),
    Playlist(String),
    Album(String),
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
    pub albums: Vec<String>,

    song_artist: Vec<Option<usize>>,
    song_label: Vec<Option<usize>>,
    song_album: Vec<Option<usize>>,
    album_songs: Vec<Vec<usize>>,
    song_playlists: Vec<Vec<usize>>,
    artist_songs: Vec<Vec<usize>>,
    label_songs: Vec<Vec<usize>>,
    label_artists: Vec<Vec<usize>>,
    playlist_songs: Vec<Vec<usize>>,

    song_tempo: Vec<Option<f32>>,
    song_key: Vec<Option<Camelot>>,
    /// Each song's place in (artist, title, path) order, used to order songs within a cell.
    song_rank: Vec<usize>,
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
        let layout = &MapLayout::fit(tracks.iter().filter_map(|t| t.tempo()));
        let mut g = Graph { layout: *layout, ..Graph::default() };
        let (mut artist_ix, mut label_ix, mut album_ix) = (HashMap::new(), HashMap::new(), HashMap::new());

        for t in tracks {
            g.song_titles.push(t.title.clone().unwrap_or_default());
            g.song_paths.push(t.path.clone());
            let a = tag(&t.artist).map(|s| intern(&mut artist_ix, &mut g.artists, s));
            let l = tag(&t.label).map(|s| intern(&mut label_ix, &mut g.labels, s));
            let al = tag(&t.album).map(|s| intern(&mut album_ix, &mut g.albums, s));
            g.song_artist.push(a);
            g.song_label.push(l);
            g.song_album.push(al);
            g.song_playlists.push(Vec::new());
        }

        g.song_tempo = tracks.iter().map(|t| t.tempo()).collect();
        g.song_key = tracks.iter().map(|t| t.best_key()).collect();
        let lower = |s: &Option<String>| s.as_deref().unwrap_or("").to_lowercase();
        let mut order: Vec<usize> = (0..tracks.len()).collect();
        order.sort_by_key(|&i| (lower(&tracks[i].artist), lower(&tracks[i].title), tracks[i].path.clone()));
        g.song_rank = vec![0; tracks.len()];
        for (rank, i) in order.into_iter().enumerate() {
            g.song_rank[i] = rank;
        }

        g.artist_songs = vec![Vec::new(); g.artists.len()];
        g.label_songs = vec![Vec::new(); g.labels.len()];
        g.label_artists = vec![Vec::new(); g.labels.len()];
        g.album_songs = vec![Vec::new(); g.albums.len()];
        for s in 0..tracks.len() {
            if let Some(al) = g.song_album[s] {
                g.album_songs[al].push(s);
            }
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
            NodeId::Album(i) => NodeKey::Album(self.albums[i].clone()),
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
            NodeKey::Album(s) => pos(&self.albums, s).map(NodeId::Album),
        }
    }

    pub fn artist_of(&self, song: usize) -> Option<usize> {
        self.song_artist[song]
    }

    pub fn label_of(&self, song: usize) -> Option<usize> {
        self.song_label[song]
    }

    pub fn album_of(&self, song: usize) -> Option<usize> {
        self.song_album[song]
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
            NodeId::Album(i) => &self.album_songs[i],
        };
        v.to_vec()
    }

    /// The drawn BPM axis for these songs at `bin` BPM per bin.
    pub fn axis_for(&self, songs: &[usize], bin: f32) -> Option<Axis> {
        Axis::new(songs.iter().filter_map(|&s| self.song_tempo[s]), bin)
    }

    /// A song's tempo (tag or detected).
    pub fn tempo(&self, song: usize) -> Option<f32> {
        self.song_tempo[song]
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
            NodeId::Album(i) => &self.albums[i],
        }
    }

    /// A layout fitted to some songs: their tempo range and only the keys they use.
    pub fn layout_for(&self, songs: &[usize]) -> MapLayout {
        MapLayout::fit(songs.iter().filter_map(|&s| self.song_tempo[s]))
            .with_rows(songs.iter().map(|&s| MapLayout::key_row(self.song_key[s])))
    }

    /// Every song's position in `layout` with tempo grouped into bins of `bin` BPM: x is the
    /// bin, y the key row, and songs sharing a cell are spread evenly across it (by artist,
    /// then title).
    /// x comes from `axis` (which fixes the bin width and compresses empty stretches).
    pub fn positions(&self, layout: &MapLayout, axis: Option<&Axis>) -> Vec<Point> {
        let l = layout;
        let bin = axis.map_or(1.0, |a| a.bin);
        let mut cells: BTreeMap<(i64, usize), Vec<usize>> = BTreeMap::new();
        for s in 0..self.song_count() {
            let col = match self.song_tempo[s].filter(|b| b.is_finite() && *b > 0.0) {
                Some(b) => (b / bin).round() as i64,
                None => i64::MIN,
            };
            cells.entry((col, MapLayout::key_row(self.song_key[s]))).or_default().push(s);
        }
        let mut pos = vec![Point { x: 0.0, y: 0.0 }; self.song_count()];
        for ((_, row), mut members) in cells {
            members.sort_by_key(|&s| self.song_rank[s]);
            let n = members.len();
            let tempo = self.song_tempo[members[0]].filter(|b| b.is_finite() && *b > 0.0);
            let (cx, width) = match (axis, tempo) {
                (Some(a), Some(b)) => (a.x(b), a.cell),
                (Some(a), None) => (a.no_bpm_x(), a.cell.max(60.0)),
                (None, _) => l.cell_x(tempo, bin),
            };
            // Use up to 80% of the cell so neighbouring cells stay apart.
            let gap = if n > 1 { width * 0.8 / (n - 1) as f32 } else { 0.0 };
            for (i, &s) in members.iter().enumerate() {
                pos[s] = Point { x: cx + (i as f32 - (n as f32 - 1.0) / 2.0) * gap, y: l.row_y(row) };
            }
        }
        pos
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
            NodeId::Album(a) => self.album_songs[a].iter().map(|&s| NodeId::Song(s)).collect(),
        }
    }

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
    fn albums_group_songs_across_artists() {
        let mut a = track("/a", "Ann", "", "8A", 124.0);
        let mut b = track("/b", "Bob", "", "9A", 126.0);
        let c = track("/c", "Ann", "", "8A", 124.0);
        a.album = Some("Compilation".into());
        b.album = Some("Compilation".into());
        let g = Graph::build(&[a, b, c], &[]);
        assert_eq!(g.albums, ["Compilation"]);
        assert_eq!(g.songs_of(NodeId::Album(0)), [0, 1]);
        assert_eq!(g.album_of(2), None);
        assert_eq!(g.find(&NodeKey::Album("Compilation".into())), Some(NodeId::Album(0)));
        assert_eq!(g.neighbours(NodeId::Album(0)), [NodeId::Song(0), NodeId::Song(1)]);
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
    fn songs_go_by_tempo_bin_and_key_row() {
        let tracks = vec![
            track("/z", "Zed", "", "8A", 124.0),
            track("/a", "Ann", "", "8A", 124.3),
            track("/b", "Bob", "", "9A", 124.0),
            track("/n", "Nia", "", "", 0.0),
        ];
        let g = Graph::build(&tracks, &[]);

        // Whole-BPM bins: Zed and Ann share the 124/8A cell, side by side, Ann first.
        let p = g.positions(&g.layout, None);
        assert_eq!(p[0].y, p[1].y);
        assert!(p[1].x < p[0].x, "Ann before Zed");
        let centre = g.layout.bpm_x(124.0);
        assert!(((p[0].x + p[1].x) / 2.0 - centre).abs() < 1e-3, "cell is centred on its BPM");
        assert!(p[2].y > p[0].y, "9A is below 8A");
        assert_eq!(p[2].x, centre, "a lone song sits at its cell's centre");
        assert_eq!(p[3].x, g.layout.no_bpm_x());
        assert_eq!(p[3].y, g.layout.row_y(crate::KEY_ROWS - 1));

        // Zoomed in to 0.1 BPM bins, 124.0 and 124.3 separate and each sits at its own BPM.
        let fine = g.axis_for(&[0, 1, 2, 3], 0.1).unwrap();
        let p = g.positions(&g.layout, Some(&fine));
        let d = p[1].x - p[0].x;
        assert!(d > 0.0, "124.3 sits right of 124.0");
        assert!(d <= 3.0 * fine.cell + 1e-2, "and no further than 3 fine bins (empty bins compress)");

        // Songs without a BPM sit left of the axis.
        assert!(p[3].x < p[0].x);

        // Fitted to Zed and Bob only: one tempo, two keys, so just two rows.
        let sub = g.layout_for(&[0, 2]);
        assert_eq!(sub.rows(), [14, 16]);
        assert!(sub.bpm_min <= 124.0 && sub.bpm_max >= 124.0);
        assert!(sub.bpm_max - sub.bpm_min <= 20.0);
    }

}
