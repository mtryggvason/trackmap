mod input;
mod map;
mod player;
mod sync_ui;
mod theme;

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::ops::Range;
use std::path::PathBuf;
use std::rc::Rc;

use futures::StreamExt;

use gpui::{
    actions, canvas, div, Focusable, point, prelude::*, px, size, uniform_list, AnyElement, App,
    Bounds, Context, FocusHandle, KeyBinding, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, PathPromptOptions, PinchEvent, RenderImage, ScrollEvent,
    SharedString,
    TitlebarOptions, Window, WindowBounds, WindowOptions,
};
use map::{Frame, MapState, Picker};
use theme::Theme;
use trackmap_core::settings::Settings;
use trackmap_core::{
    artwork, key_detect, playlists, tags, tempo, Art, Camelot, Library, Playlist, Watcher,
    PLAYLISTS_DIR,
};
use trackmap_graph::{Graph, NodeId};

actions!(trackmap, [QuickOpen, CommandPalette, OpenFolder, Dismiss, Quit, TogglePlay, SkipBack, SkipForward, Confirm]);

/// What a name prompt is for.
#[derive(Clone)]
enum PromptKind {
    /// Create a playlist, adding this song to it if set.
    NewPlaylist { add: Option<PathBuf> },
    /// Rename this playlist.
    Rename(String),
}

struct NamePrompt {
    kind: PromptKind,
    input: gpui::Entity<input::TextInput>,
}

pub(crate) enum LibraryState {
    Empty,
    Scanning(PathBuf),
    Loaded(Library),
    Failed(PathBuf, String),
}

#[derive(Clone, Copy, PartialEq)]
enum ViewMode {
    Map,
    List,
}

#[derive(Clone, Copy, PartialEq)]
enum PaletteMode {
    QuickOpen,
    Commands,
}

pub(crate) struct Workspace {
    focus: FocusHandle,
    palette: Option<PaletteMode>,
    library: LibraryState,
    /// Tracks added since the last time this library was open, or while it is open.
    new_tracks: HashSet<PathBuf>,
    only_new: bool,
    /// Indices into `library.tracks` currently shown, after filters.
    visible: Vec<usize>,
    watcher: Option<Watcher>,
    /// Bumped on every load so work for a previous library is discarded.
    generation: u64,
    playlists: Vec<Playlist>,
    graph: Option<Rc<Graph>>,
    map: MapState,
    view: ViewMode,
    /// Decoded cover thumbnails by file name.
    art_images: HashMap<String, Arc<RenderImage>>,
    art_running: bool,
    analysis: Option<AnalysisJob>,
    /// Writing detected BPMs/keys into files' tags.
    writing_tags: bool,
    /// Result of the last tag write, shown on the map.
    notice: Option<SharedString>,
    player: player::Player,
    /// Whether a timer is refreshing the preview bar while audio plays.
    ticking: bool,
    /// Preview bar's progress track, for turning clicks into seek positions.
    progress_bounds: Rc<std::cell::Cell<Bounds<gpui::Pixels>>>,
    /// Waveforms of played tracks, and tracks whose waveform is being computed.
    waves: HashMap<PathBuf, Arc<Vec<f32>>>,
    wave_loading: HashSet<PathBuf>,
    /// The name prompt (new / rename playlist), when open.
    prompt: Option<NamePrompt>,
    /// The song inspector's "Add to playlist" list is open.
    adding_to_playlist: bool,
    /// A playlist whose Delete button was clicked once and awaits confirmation.
    confirm_delete: Option<String>,
    /// The Sync to USB dialog, when open.
    sync: Option<sync_ui::SyncDialog>,
}

/// What a "Process …" button analyses.
#[derive(Clone, Copy, PartialEq)]
enum Analysis {
    Bpm,
    Key,
}

impl Analysis {
    fn label(self) -> &'static str {
        match self {
            Analysis::Bpm => "BPM",
            Analysis::Key => "Key",
        }
    }

    fn pending(self, lib: &Library) -> Vec<PathBuf> {
        match self {
            Analysis::Bpm => tempo::pending(&lib.tracks),
            Analysis::Key => key_detect::pending(&lib.tracks),
        }
    }
}

/// A running analysis pass.
struct AnalysisJob {
    kind: Analysis,
    done: usize,
    total: usize,
    cancel: Arc<AtomicBool>,
}

enum Detected {
    Bpm(Option<f32>),
    Key(Option<Camelot>),
}

fn analysis_threads() -> usize {
    std::thread::available_parallelism().map_or(2, |n| n.get()).clamp(1, 6)
}

/// Analyses tracks in parallel on a few threads.
fn detect_batch(kind: Analysis, paths: Vec<PathBuf>) -> Vec<(PathBuf, Detected)> {
    let chunk = paths.len().div_ceil(analysis_threads()).max(1);
    let run = move |p: &PathBuf| match kind {
        Analysis::Bpm => Detected::Bpm(tempo::detect_file(p)),
        Analysis::Key => Detected::Key(key_detect::detect_file(p)),
    };
    std::thread::scope(|s| {
        let handles: Vec<_> = paths
            .chunks(chunk)
            .map(|c| s.spawn(move || c.iter().map(|p| (p.clone(), run(p))).collect::<Vec<_>>()))
            .collect();
        handles.into_iter().flat_map(|h| h.join().unwrap_or_default()).collect()
    })
}

/// Covers looked up per background step, so they appear progressively.
const ART_BATCH: usize = 24;

/// Decodes a thumbnail into the BGRA frame GPUI draws.
fn decode_thumb(path: &std::path::Path) -> Option<Arc<RenderImage>> {
    let mut rgba = image::open(path).ok()?.to_rgba8();
    for px in rgba.pixels_mut() {
        px.0.swap(0, 2);
    }
    Some(Arc::new(RenderImage::new(vec![image::Frame::new(rgba)])))
}

pub(crate) fn settings_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("TrackMap").join("settings.json"))
}

impl Workspace {
    fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus: cx.focus_handle(),
            palette: None,
            library: LibraryState::Empty,
            new_tracks: HashSet::new(),
            only_new: false,
            visible: Vec::new(),
            watcher: None,
            generation: 0,
            playlists: Vec::new(),
            graph: None,
            map: MapState::default(),
            view: ViewMode::Map,
            art_images: HashMap::new(),
            art_running: false,
            analysis: None,
            writing_tags: false,
            notice: None,
            player: player::Player::default(),
            ticking: false,
            progress_bounds: Rc::new(std::cell::Cell::new(Bounds::default())),
            waves: HashMap::new(),
            wave_loading: HashSet::new(),
            prompt: None,
            adding_to_playlist: false,
            confirm_delete: None,
            sync: None,
        }
    }

    fn library_root(&self) -> Option<PathBuf> {
        match &self.library {
            LibraryState::Loaded(lib) => Some(lib.root.clone()),
            _ => None,
        }
    }

    /// Re-reads the playlists from disk right away (the watcher also does, a moment later).
    fn reload_playlists(&mut self) {
        if let Some(root) = self.library_root() {
            self.playlists = playlists::list(&root).unwrap_or_default();
            self.rebuild_graph();
        }
    }

    /// Runs a playlist change, then shows any error and refreshes.
    fn playlist_op(&mut self, op: impl FnOnce(&std::path::Path) -> std::io::Result<()>, cx: &mut Context<Self>) {
        let Some(root) = self.library_root() else { return };
        if let Err(e) = op(&root) {
            self.notice = Some(format!("Playlist: {e}").into());
        }
        self.reload_playlists();
        cx.notify();
    }

    fn open_prompt(&mut self, kind: PromptKind, window: &mut Window, cx: &mut Context<Self>) {
        let (placeholder, text) = match &kind {
            PromptKind::NewPlaylist { .. } => ("Playlist name", String::new()),
            PromptKind::Rename(name) => ("New name", name.clone()),
        };
        let input = cx.new(|cx| input::TextInput::new(placeholder, &text, cx));
        let handle = input.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
        self.map.picker = None;
        self.prompt = Some(NamePrompt { kind, input });
        cx.notify();
    }

    fn close_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.prompt = None;
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// Enter in the name prompt: create or rename the playlist.
    fn confirm_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(prompt) = &self.prompt else { return };
        let name = prompt.input.read(cx).text().trim().to_string();
        let kind = prompt.kind.clone();
        let Some(root) = self.library_root() else { return };
        let result = match &kind {
            PromptKind::NewPlaylist { add } => playlists::create(&root, &name).and_then(|_| match add {
                Some(song) => playlists::add(&root, &name, song).map(|_| ()),
                None => Ok(()),
            }),
            PromptKind::Rename(old) if *old == name => Ok(()),
            PromptKind::Rename(old) => playlists::rename(&root, old, &name),
        };
        match result {
            Ok(()) => {
                self.close_prompt(window, cx);
                self.reload_playlists();
                let key = trackmap_graph::NodeKey::Playlist(name.clone());
                match kind {
                    // Keep viewing the playlist under its new name.
                    PromptKind::Rename(old) => {
                        let was = Some(trackmap_graph::NodeKey::Playlist(old));
                        if self.map.scope == was {
                            self.map.scope = Some(key.clone());
                        }
                        if self.map.selected == was {
                            self.map.selected = Some(key);
                        }
                    }
                    PromptKind::NewPlaylist { .. } => self.notice = Some(format!("Created playlist {name}").into()),
                }
            }
            // Keep the prompt open so the name can be fixed.
            Err(e) => self.notice = Some(format!("Playlist: {e}").into()),
        }
        cx.notify();
    }

    /// Moves a song up (-1) or down (+1) in a playlist.
    fn move_in_playlist(&mut self, name: &str, song: &std::path::Path, delta: i32, cx: &mut Context<Self>) {
        let Some(pl) = self.playlists.iter().find(|p| p.name == name) else { return };
        let mut order: Vec<PathBuf> = pl.entries.iter().map(|e| e.target.clone()).collect();
        let Some(i) = order.iter().position(|p| p == song) else { return };
        let j = i as i32 + delta;
        if j < 0 || j as usize >= order.len() {
            return;
        }
        order.swap(i, j as usize);
        let name = name.to_string();
        self.playlist_op(move |root| playlists::reorder(root, &name, &order), cx);
    }

    fn prompt_view(&self, prompt: &NamePrompt, t: &Theme) -> impl IntoElement {
        let title = match &prompt.kind {
            PromptKind::NewPlaylist { add: Some(_) } => "New playlist with the selected song",
            PromptKind::NewPlaylist { add: None } => "New playlist",
            PromptKind::Rename(_) => "Rename playlist",
        };
        div().absolute().size_full().flex().justify_center().pt(px(80.)).child(
            div()
                .w(px(420.))
                .p_3()
                .flex()
                .flex_col()
                .gap_2()
                .rounded_lg()
                .bg(t.bg_elevated)
                .border_1()
                .border_color(t.accent)
                .shadow_lg()
                .child(div().text_xs().text_color(t.text_muted).child(title))
                .child(
                    div()
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .bg(t.bg_app)
                        .border_1()
                        .border_color(t.border)
                        .child(prompt.input.clone()),
                )
                .child(div().text_xs().text_color(t.text_muted).child("Enter to save  ·  Esc to cancel")),
        )
    }

    /// The song picked on the map, if any.
    fn selected_song(&self) -> Option<PathBuf> {
        match &self.map.selected {
            Some(trackmap_graph::NodeKey::Song(p)) => Some(p.clone()),
            _ => None,
        }
    }

    /// Space: plays the selected song, or pauses/resumes what's playing.
    fn toggle_play(&mut self, cx: &mut Context<Self>) {
        let selected = self.selected_song();
        match (&selected, &self.player.track) {
            (Some(s), current) if current.as_ref() != Some(s) => self.start_playback(s, cx),
            (_, Some(current)) if self.player.finished() => {
                let current = current.clone();
                self.start_playback(&current, cx);
            }
            (_, Some(_)) => self.player.toggle(),
            _ => {}
        }
        self.start_ticking(cx);
        cx.notify();
    }

    /// Starts a track and fetches its waveform (cached, or computed in the background).
    fn start_playback(&mut self, path: &std::path::Path, cx: &mut Context<Self>) {
        self.player.play(path);
        if self.waves.contains_key(path) || !self.wave_loading.insert(path.to_path_buf()) {
            return;
        }
        let LibraryState::Loaded(lib) = &self.library else { return };
        let Some(track) = lib.tracks.iter().find(|t| t.path == path).cloned() else { return };
        let root = lib.root.clone();
        cx.spawn(async move |this, cx| {
            let wave = cx
                .background_executor()
                .spawn(async move { trackmap_core::waveform::load_or_compute(&root, &track).map(|w| (track.path, w)) })
                .await;
            this.update(cx, |ws, cx| {
                if let Some((path, w)) = wave {
                    ws.wave_loading.remove(&path);
                    ws.waves.insert(path, Arc::new(w));
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn play_song(&mut self, path: &std::path::Path, cx: &mut Context<Self>) {
        self.start_playback(path, cx);
        self.start_ticking(cx);
        cx.notify();
    }

    /// Refreshes the preview bar (time, progress) a few times a second while audio plays.
    fn start_ticking(&mut self, cx: &mut Context<Self>) {
        if self.ticking || !self.player.is_playing() {
            return;
        }
        self.ticking = true;
        cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(std::time::Duration::from_millis(100)).await;
            let keep = this.update(cx, |ws, cx| {
                cx.notify();
                if !ws.player.is_playing() {
                    ws.ticking = false;
                    return false;
                }
                true
            });
            if !matches!(keep, Ok(true)) {
                break;
            }
        })
        .detach();
    }

    /// Writes every detected BPM and key into its file's tags so the Pocket can sort by them.
    fn write_tags(&mut self, cx: &mut Context<Self>) {
        if self.writing_tags {
            return;
        }
        let LibraryState::Loaded(lib) = &self.library else { return };
        let todo = tags::pending_writes(&lib.tracks);
        if todo.is_empty() {
            return;
        }
        self.writing_tags = true;
        self.notice = Some(format!("Writing tags to {} files…", todo.len()).into());
        let generation = self.generation;
        cx.notify();

        cx.spawn(async move |this, cx| {
            let results = cx
                .background_executor()
                .spawn(async move {
                    todo.into_iter()
                        .map(|w| {
                            let r = tags::write(&w).map_err(|e| e.to_string());
                            (w, r)
                        })
                        .collect::<Vec<_>>()
                })
                .await;
            let snapshot = this.update(cx, |ws, cx| {
                ws.writing_tags = false;
                if ws.generation != generation {
                    return None;
                }
                let LibraryState::Loaded(lib) = &mut ws.library else { return None };
                let written: HashMap<&PathBuf, &tags::TagWrite> =
                    results.iter().filter(|r| r.1.is_ok()).map(|(w, _)| (&w.path, w)).collect();
                // Reflect the new tags right away; the watcher will re-read the files too.
                for t in &mut lib.tracks {
                    if let Some(w) = written.get(&t.path) {
                        if let Some(bpm) = w.bpm {
                            t.bpm = Some(bpm.round());
                        }
                        if let Some(key) = w.key {
                            t.key = Some(key);
                        }
                    }
                }
                let failed = results.len() - written.len();
                let dropped = results
                    .iter()
                    .filter(|r| matches!(r.1, Ok(tags::Written::DroppedOwnership)))
                    .count();
                for (w, r) in results.iter().filter(|r| r.1.is_err()) {
                    eprintln!("trackmap: could not write tags to {}: {}", w.path.display(), r.as_ref().unwrap_err());
                }
                let mut notice = format!("Wrote tags to {} files", written.len());
                if dropped > 0 {
                    notice += &format!(" ({dropped} had their store receipt (OWNE) removed to allow it)");
                }
                if failed > 0 {
                    notice += &format!(", {failed} failed");
                }
                ws.notice = Some(notice.into());
                let snapshot = lib.clone();
                ws.rebuild_graph();
                cx.notify();
                Some(snapshot)
            });
            if let Ok(Some(lib)) = snapshot {
                cx.background_executor()
                    .spawn(async move {
                        let _ = trackmap_core::cache::save(&lib);
                    })
                    .await;
            }
        })
        .detach();
    }

    /// Analyses every track missing a BPM or key, a few at a time, updating the map as it goes.
    /// One pass runs at a time.
    fn start_analysis(&mut self, kind: Analysis, cx: &mut Context<Self>) {
        if self.analysis.is_some() {
            return;
        }
        let LibraryState::Loaded(lib) = &self.library else { return };
        let todo = kind.pending(lib);
        if todo.is_empty() {
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        self.analysis =
            Some(AnalysisJob { kind, done: 0, total: todo.len(), cancel: cancel.clone() });
        let generation = self.generation;
        cx.notify();

        cx.spawn(async move |this, cx| {
            for chunk in todo.chunks(analysis_threads() * 2) {
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
                let paths = chunk.to_vec();
                let results =
                    cx.background_executor().spawn(async move { detect_batch(kind, paths) }).await;
                let keep_going = this.update(cx, |ws, cx| {
                    if ws.generation != generation {
                        return false;
                    }
                    let LibraryState::Loaded(lib) = &mut ws.library else { return false };
                    let n = results.len();
                    let (mut bpms, mut keys) = (Vec::new(), Vec::new());
                    for (p, d) in results {
                        match d {
                            Detected::Bpm(b) => bpms.push((p, b)),
                            Detected::Key(k) => keys.push((p, k)),
                        }
                    }
                    lib.set_tempo(bpms);
                    lib.set_key(keys);
                    if let Some(job) = &mut ws.analysis {
                        job.done += n;
                    }
                    ws.rebuild_graph();
                    cx.notify();
                    true
                });
                if !matches!(keep_going, Ok(true)) {
                    return;
                }
            }
            let snapshot = this.update(cx, |ws, cx| {
                ws.analysis = None;
                cx.notify();
                match &ws.library {
                    LibraryState::Loaded(lib) if ws.generation == generation => Some(lib.clone()),
                    _ => None,
                }
            });
            if let Ok(Some(lib)) = snapshot {
                cx.background_executor()
                    .spawn(async move {
                        let _ = trackmap_core::cache::save(&lib);
                    })
                    .await;
            }
        })
        .detach();
    }

    fn stop_analysis(&mut self) {
        if let Some(job) = self.analysis.take() {
            job.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Finds covers for tracks not checked yet and decodes thumbnails not loaded yet, in
    /// batches on a background thread. Safe to call often; only one pass runs at a time.
    fn fill_artwork(&mut self, cx: &mut Context<Self>) {
        if self.art_running {
            return;
        }
        let LibraryState::Loaded(lib) = &self.library else { return };
        self.art_running = true;
        let root = lib.root.clone();
        let generation = self.generation;

        cx.spawn(async move |this, cx| {
            let mut changed = false;
            loop {
                // What's left: tracks to look up, and thumbnails not decoded yet.
                let Ok(Some((todo, missing))) = this.update(cx, |ws, _| {
                    if ws.generation != generation {
                        return None;
                    }
                    let LibraryState::Loaded(lib) = &ws.library else { return None };
                    let mut todo = artwork::unchecked(&lib.tracks);
                    todo.truncate(ART_BATCH);
                    let missing: HashSet<String> = lib
                        .tracks
                        .iter()
                        .filter_map(|t| match &t.art {
                            Art::File(n) if !ws.art_images.contains_key(n) => Some(n.clone()),
                            _ => None,
                        })
                        .collect();
                    Some((todo, missing))
                }) else {
                    break;
                };
                if todo.is_empty() && missing.is_empty() {
                    break;
                }

                let art_root = root.clone();
                let (results, images) = cx
                    .background_executor()
                    .spawn(async move {
                        let results = artwork::fill(&art_root, &todo);
                        let dir = artwork::art_dir(&art_root);
                        let mut names = missing;
                        names.extend(results.iter().filter_map(|(_, a)| match a {
                            Art::File(n) => Some(n.clone()),
                            _ => None,
                        }));
                        let images: Vec<(String, Option<Arc<RenderImage>>)> = names
                            .into_iter()
                            .map(|n| {
                                let img = decode_thumb(&dir.join(&n));
                                (n, img)
                            })
                            .collect();
                        (results, images)
                    })
                    .await;
                changed |= !results.is_empty();

                let ok = this.update(cx, |ws, cx| {
                    if ws.generation != generation {
                        return false;
                    }
                    let LibraryState::Loaded(lib) = &mut ws.library else { return false };
                    // A thumbnail that can't be decoded counts as no art, so it isn't retried forever.
                    let broken: HashSet<String> =
                        images.iter().filter(|(_, i)| i.is_none()).map(|(n, _)| n.clone()).collect();
                    lib.set_art(results);
                    for t in &mut lib.tracks {
                        if matches!(&t.art, Art::File(n) if broken.contains(n)) {
                            t.art = Art::None;
                        }
                    }
                    ws.art_images.extend(images.into_iter().filter_map(|(n, i)| Some((n, i?))));
                    cx.notify();
                    true
                });
                if !matches!(ok, Ok(true)) {
                    break;
                }
            }

            let snapshot = this.update(cx, |ws, _| {
                ws.art_running = false;
                match &ws.library {
                    LibraryState::Loaded(lib) if changed && ws.generation == generation => {
                        Some(lib.clone())
                    }
                    _ => None,
                }
            });
            if let Ok(Some(lib)) = snapshot {
                cx.background_executor()
                    .spawn(async move {
                        let _ = trackmap_core::cache::save(&lib);
                    })
                    .await;
            }
        })
        .detach();
    }

    /// Rebuilds the graph after the library or playlists change.
    fn rebuild_graph(&mut self) {
        self.graph = match &self.library {
            LibraryState::Loaded(lib) => Some(Rc::new(Graph::build(&lib.tracks, &self.playlists))),
            _ => None,
        };
    }

    fn open_folder(&mut self, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Open library".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(mut paths))) = picked.await else { return };
            let Some(root) = paths.pop() else { return };
            this.update(cx, |ws, cx| ws.load(root, cx)).ok();
        })
        .detach();
    }

    /// Opens `root` on a background thread (using its cache), then watches it for changes.
    fn load(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        self.generation += 1;
        let generation = self.generation;
        self.watcher = None; // stops the previous watcher and its update loop
        self.new_tracks.clear();
        self.playlists.clear();
        self.graph = None;
        self.map = MapState::default();
        self.art_images.clear();
        self.art_running = false;
        self.stop_analysis();
        self.library = LibraryState::Scanning(root.clone());
        cx.notify();

        cx.spawn(async move |this, cx| {
            let scan_root = root.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    let report = trackmap_core::open(&scan_root);
                    let pls = playlists::list(&scan_root).unwrap_or_default();
                    report.map(|r| (r, pls))
                })
                .await;

            this.update(cx, |ws, cx| {
                if ws.generation != generation {
                    return;
                }
                match result {
                    Ok((report, pls)) => {
                        remember_library(&report.library.root);
                        ws.new_tracks = report.new.into_iter().collect();
                        ws.library = LibraryState::Loaded(report.library);
                        ws.playlists = pls;
                        ws.rebuild_graph();
                        ws.start_watching(generation, cx);
                        ws.fill_artwork(cx);
                    }
                    Err(e) => ws.library = LibraryState::Failed(root, e.to_string()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Applies file-system changes to the open library as they happen.
    fn start_watching(&mut self, generation: u64, cx: &mut Context<Self>) {
        let LibraryState::Loaded(lib) = &self.library else { return };
        let root = lib.root.clone();
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<Vec<PathBuf>>();
        match trackmap_core::watch(&root, move |paths| {
            let _ = tx.unbounded_send(paths);
        }) {
            Ok(w) => self.watcher = Some(w),
            Err(e) => {
                eprintln!("trackmap: not watching {}: {e}", root.display());
                return;
            }
        }

        cx.spawn(async move |this, cx| {
            // Ends when the watcher (and with it the sender) is dropped.
            while let Some(mut paths) = rx.next().await {
                while let Ok(more) = rx.try_recv() {
                    paths.extend(more);
                }
                let playlists_dir = root.join(PLAYLISTS_DIR);
                let playlists_touched = paths.iter().any(|p| p.starts_with(&playlists_dir));
                let changes_root = root.clone();
                let (changes, pls) = cx
                    .background_executor()
                    .spawn(async move {
                        let pls = playlists_touched
                            .then(|| playlists::list(&changes_root).unwrap_or_default());
                        (trackmap_core::changes_for(&changes_root, paths), pls)
                    })
                    .await;
                if changes.is_empty() && pls.is_none() {
                    continue;
                }

                let snapshot = this.update(cx, |ws, cx| {
                    if ws.generation != generation {
                        return Err(());
                    }
                    let LibraryState::Loaded(lib) = &mut ws.library else { return Err(()) };
                    let snapshot = (!changes.is_empty()).then(|| {
                        let added = lib.apply(changes);
                        ws.new_tracks.extend(added);
                        lib.clone()
                    });
                    if let Some(pls) = pls {
                        ws.playlists = pls;
                    }
                    ws.rebuild_graph();
                    ws.fill_artwork(cx);
                    cx.notify();
                    Ok(snapshot)
                });
                let Ok(Ok(snapshot)) = snapshot else { break };
                if let Some(lib) = snapshot {
                    cx.background_executor()
                        .spawn(async move {
                            let _ = trackmap_core::cache::save(&lib);
                        })
                        .await;
                }
            }
        })
        .detach();
    }

    fn library_name(&self) -> SharedString {
        let path = match &self.library {
            LibraryState::Empty => return "Open folder… ▾".into(),
            LibraryState::Scanning(p) | LibraryState::Failed(p, _) => p,
            LibraryState::Loaded(lib) => &lib.root,
        };
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
        format!("{} ▾", name.unwrap_or_else(|| path.display().to_string())).into()
    }

    fn top_bar(&self, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let chip = |label: &'static str| {
            div()
                .px_2()
                .py_0p5()
                .rounded_md()
                .bg(t.bg_elevated)
                .text_color(t.text_muted)
                .child(label)
        };
        let t2 = *t;
        let toggle = move |id: &'static str, label: SharedString, on: bool| {
            div()
                .id(id)
                .px_2()
                .py_0p5()
                .rounded_md()
                .cursor_pointer()
                .when(on, |d| d.bg(t2.accent).text_color(t2.bg_app))
                .when(!on, |d| {
                    d.bg(t2.bg_elevated).text_color(t2.text_muted).hover(|s| s.text_color(t2.text))
                })
                .child(label)
        };
        div()
            .flex()
            .items_center()
            .gap_2()
            .h(px(40.))
            // Leave room for the macOS traffic lights.
            .pl(px(80.))
            .pr_3()
            .bg(t.bg_panel)
            .border_b_1()
            .border_color(t.border)
            .child(
                div()
                    .id("library")
                    .px_2()
                    .py_0p5()
                    .rounded_md()
                    .cursor_pointer()
                    .text_color(t.text)
                    .hover(|s| s.bg(t.bg_elevated))
                    .on_click(cx.listener(|this, _, _, cx| this.open_folder(cx)))
                    .child(self.library_name()),
            )
            .child(
                div()
                    .w(px(280.))
                    .px_2()
                    .py_0p5()
                    .rounded_md()
                    .bg(t.bg_elevated)
                    .border_1()
                    .border_color(t.border)
                    .text_color(t.text_muted)
                    .child("Search…  ⌘P"),
            )
            .child(chip("BPM"))
            .child(chip("Key"))
            .child(
                toggle(
                    "new-filter",
                    match self.new_tracks.len() {
                        0 => "New".into(),
                        n => format!("New {n}").into(),
                    },
                    self.only_new,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.only_new = !this.only_new;
                    cx.notify();
                })),
            )
            .children([
                (Picker::Artists, "artists", "Artists"),
                (Picker::Labels, "labels", "Labels"),
                (Picker::Playlists, "playlists", "Playlists"),
            ]
            .map(|(kind, id, label)| {
                toggle(id, format!("{label} ▾").into(), self.map.picker == Some(kind)).on_click(
                    cx.listener(move |this, _, _, cx| {
                        this.map.picker =
                            if this.map.picker == Some(kind) { None } else { Some(kind) };
                        cx.notify();
                    }),
                )
            }))
            .when(self.map.selected.is_some() || self.map.scope.is_some(), |d| {
                d.child(toggle("clear", "Clear ✕".into(), false).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.map.clear();
                        cx.notify();
                    },
                )))
            })
            .child(div().flex_1())
            .child(
                div()
                    .flex()
                    .rounded_md()
                    .bg(t.bg_elevated)
                    .child(toggle("view-map", "Map".into(), self.view == ViewMode::Map).on_click(
                        cx.listener(|this, _, _, cx| {
                            this.view = ViewMode::Map;
                            cx.notify();
                        }),
                    ))
                    .child(
                        toggle("view-list", "List".into(), self.view == ViewMode::List).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.view = ViewMode::List;
                                cx.notify();
                            }),
                        ),
                    ),
            )
            .children(self.process_buttons(t, cx))
            .child(
                div()
                    .id("sync-open")
                    .px_3()
                    .py_0p5()
                    .rounded_md()
                    .cursor_pointer()
                    .bg(t.accent)
                    .hover(|s| s.bg(t.accent_hover))
                    .text_color(t.bg_app)
                    .child("Sync ⇪")
                    .on_click(cx.listener(|this, _, _, cx| this.open_sync(cx))),
            )
    }

    /// "Process BPM (n)" / "Process Key (n)" while tracks lack them, progress (click to stop)
    /// while one runs, and "Write to tags (n)" once there are detected values to save.
    fn process_buttons(&self, t: &Theme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let t = *t;
        let base = move |id: &'static str| {
            div()
                .id(id)
                .px_3()
                .py_0p5()
                .rounded_md()
                .cursor_pointer()
                .border_1()
                .border_color(t.accent)
        };
        let LibraryState::Loaded(lib) = &self.library else { return Vec::new() };
        let mut out = Vec::new();
        // BPM and Key always sit side by side: progress on the running one, the other greyed
        // out while it runs, and a tick once there's nothing left to process.
        for (kind, id) in [(Analysis::Bpm, "process-bpm"), (Analysis::Key, "process-key")] {
            let button = match &self.analysis {
                Some(job) if job.kind == kind => base(id)
                    .text_color(t.accent)
                    .child(format!("Processing {} {}/{}  ✕", kind.label(), job.done, job.total))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.stop_analysis();
                        cx.notify();
                    })),
                _ => {
                    let n = kind.pending(lib).len();
                    if n == 0 {
                        base(id)
                            .cursor_default()
                            .border_color(t.border)
                            .text_color(t.text_muted)
                            .child(format!("{} ✓", kind.label()))
                    } else if self.analysis.is_some() {
                        base(id)
                            .cursor_default()
                            .border_color(t.border)
                            .text_color(t.text_muted)
                            .child(format!("Process {} ({n})", kind.label()))
                    } else {
                        base(id)
                            .text_color(t.text)
                            .hover(|s| s.bg(t.bg_elevated))
                            .child(format!("Process {} ({n})", kind.label()))
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.start_analysis(kind, cx)),
                            )
                    }
                }
            };
            out.push(button.into_any_element());
        }
        if self.writing_tags {
            out.push(base("write-tags").text_color(t.text_muted).child("Writing tags…").into_any_element());
        } else {
            let w = tags::pending_writes(&lib.tracks).len();
            if w > 0 {
                out.push(
                    base("write-tags")
                        .text_color(t.text)
                        .hover(|s| s.bg(t.bg_elevated))
                        .child(format!("Write to tags ({w})"))
                        .on_click(cx.listener(|this, _, _, cx| this.write_tags(cx)))
                        .into_any_element(),
                );
            }
        }
        out
    }

    fn canvas_area(&mut self, t: &Theme, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let centered = |text: SharedString| {
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_color(t.text_muted)
                .child(text)
        };
        let body = match &self.library {
            LibraryState::Empty => {
                centered("Open a folder to start (⌘O)  ·  ⌘P to search".into()).into_any_element()
            }
            LibraryState::Scanning(p) => {
                centered(format!("Scanning {}…", p.display()).into()).into_any_element()
            }
            LibraryState::Failed(p, e) => {
                centered(format!("Could not open {}: {e}", p.display()).into()).into_any_element()
            }
            LibraryState::Loaded(_) if self.view == ViewMode::Map => {
                // The inspector floats over the map, so opening it doesn't resize the map and
                // nothing under it moves.
                return div()
                    .flex_1()
                    .flex()
                    .relative()
                    .bg(t.bg_app)
                    .child(self.map_view(t, window, cx))
                    .children(self.inspector(t, cx).map(|panel| {
                        div().absolute().top_0().right_0().bottom_0().child(panel)
                    }))
                    .into_any_element();
            }
            LibraryState::Loaded(lib) => self.track_list(lib, t, cx).into_any_element(),
        };
        div().flex_1().flex().flex_col().bg(t.bg_app).child(body).into_any_element()
    }

    /// Temporary flat list until the graph canvas lands (milestone 5).
    fn track_list(&self, lib: &Library, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let t = *t;
        let mut summary = format!(
            "{} tracks  ·  {} artists  ·  {} labels",
            lib.tracks.len(),
            lib.artist_count(),
            lib.label_count()
        );
        if self.only_new {
            summary = format!("Showing {} new  ·  {summary}", self.visible.len());
        }
        let status = if self.watcher.is_some() { "● watching" } else { "not watching" };
        div()
            .flex_1()
            .flex()
            .flex_col()
            .child(
                div()
                    .px_3()
                    .py_1()
                    .text_color(t.text_muted)
                    .border_b_1()
                    .border_color(t.border)
                    .flex()
                    .justify_between()
                    .child(summary)
                    .child(status),
            )
            .child(
                uniform_list(
                    "tracks",
                    self.visible.len(),
                    cx.processor(move |this, range: Range<usize>, _, cx| {
                        let LibraryState::Loaded(lib) = &this.library else { return Vec::new() };
                        range
                            .filter_map(|row| this.visible.get(row).map(|&i| (row, i)))
                            .map(|(row, i)| {
                                let tr = &lib.tracks[i];
                                let is_new = this.new_tracks.contains(&tr.path);
                                let dash = || "—".to_string();
                                let path = tr.path.clone();
                                let is_selected = this.selected_song().as_ref() == Some(&tr.path);
                                div()
                                    .id(row)
                                    .flex()
                                    .gap_3()
                                    .px_3()
                                    .h(px(26.))
                                    .items_center()
                                    .cursor_pointer()
                                    .when(is_selected, |d| d.bg(t.bg_elevated))
                                    .hover(|s| s.bg(t.bg_elevated))
                                    // Click selects (Space then plays); double-click plays.
                                    .on_click(cx.listener(move |this, e: &gpui::ClickEvent, _, cx| {
                                        this.map.selected = Some(trackmap_graph::NodeKey::Song(path.clone()));
                                        if e.click_count() >= 2 {
                                            this.play_song(&path, cx);
                                        }
                                        cx.notify();
                                    }))
                                    .child(
                                        div()
                                            .w(px(36.))
                                            .text_xs()
                                            .text_color(t.accent)
                                            .child(if is_new { "NEW" } else { "" }),
                                    )
                                    .child(
                                        div()
                                            .w(px(360.))
                                            .overflow_hidden()
                                            .text_color(t.text)
                                            .child(tr.title.clone().unwrap_or_else(dash)),
                                    )
                                    .child(
                                        div()
                                            .w(px(220.))
                                            .overflow_hidden()
                                            .text_color(t.text_muted)
                                            .child(tr.artist.clone().unwrap_or_else(dash)),
                                    )
                                    .child(
                                        div()
                                            .w(px(180.))
                                            .overflow_hidden()
                                            .text_color(t.text_muted)
                                            .child(tr.label.clone().unwrap_or_else(dash)),
                                    )
                                    .child(div().w(px(60.)).text_color(t.text_muted).child(
                                        tr.tempo().map(|b| format!("{b:.0}")).unwrap_or_else(dash),
                                    ))
                                    .child(div().w(px(40.)).text_color(t.accent).child(
                                        tr.best_key().map(|k| k.to_string()).unwrap_or_default(),
                                    ))
                            })
                            .collect()
                    }),
                )
                .flex_1(),
            )
    }

    fn map_view(&mut self, t: &Theme, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let now = std::time::Instant::now();
        let mut animating = self.map.step_view(now);
        animating |= self.map.step_glide(now);
        let (Some(graph), LibraryState::Loaded(lib)) = (self.graph.clone(), &self.library) else {
            return div().into_any_element();
        };
        let keys: Vec<_> = lib.tracks.iter().map(|tr| tr.best_key()).collect();
        let arts: Vec<_> = lib
            .tracks
            .iter()
            .map(|tr| match &tr.art {
                Art::File(n) => self.art_images.get(n).cloned(),
                _ => None,
            })
            .collect();
        let selected = self.map.selected.as_ref().and_then(|k| graph.find(k));
        let scope = self.map.scope.as_ref().and_then(|k| graph.find(k));
        // Inside an artist/label/playlist only its songs are shown; otherwise the filters decide.
        let shown: Vec<usize> = match scope {
            Some(group) => graph.songs_of(group),
            None => self.visible.clone(),
        };
        // A subset (group or filter) gets a layout fitted to it; the whole library uses the
        // library's layout.
        let subset = shown.len() < graph.song_count();
        let layout = if subset && !shown.is_empty() { graph.layout_for(&shown) } else { graph.layout };
        let signature = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            shown.hash(&mut h);
            h.finish()
        };
        // Positions (and the drawn axis) for a tempo bin width, cached per (bin, graph, shown).
        let place = |bin: f32, cache: &mut Option<(f32, Rc<Graph>, u64, Rc<Vec<trackmap_graph::Point>>)>| {
            match cache {
                Some((b, g, sig, p)) if *b == bin && Rc::ptr_eq(g, &graph) && *sig == signature => p.clone(),
                _ => {
                    let axis = graph.axis_for(&shown, bin);
                    let p = Rc::new(graph.positions(&layout, axis.as_ref()));
                    *cache = Some((bin, graph.clone(), signature, p.clone()));
                    p
                }
            }
        };
        // Whole-BPM groups at every zoom level: zooming only magnifies, so nothing moves under
        // your fingers. The camera frames the shown songs.
        let bin = trackmap_graph::BPM_BIN;
        let positions = place(bin, &mut self.map.positions);
        let axis = graph.axis_for(&shown, bin);
        let row = layout.row_height();
        let slot = trackmap_graph::SLOT;
        if let Some(target) = map::Fit::around(shown.iter().map(|&s| positions[s]), slot * 0.7, row * 0.6) {
            animating |= self.map.step_fit(target, now);
        }
        // Covers fill the space each song has on screen (a slot wide, a row high).
        let scale = self.map.transform().scale();
        self.map.cover = map::cover_px(slot * scale, row * scale);
        // Highlighting only applies to a song picked inside the view, not to the group itself.
        let highlight = selected.filter(|_| self.map.selected != self.map.scope);
        let route_of = selected.filter(|n| matches!(n, NodeId::Playlist(_))).or(scope);
        let anim = self.map.animate(&positions, &shown, now);
        animating |= anim.running;
        if animating {
            window.request_animation_frame();
        }
        let has_no_bpm = shown.iter().any(|&s| graph.tempo(s).is_none());
        let playing = self
            .player
            .track
            .as_ref()
            .and_then(|p| graph.find(&trackmap_graph::NodeKey::Song(p.clone())))
            .and_then(|n| match n {
                NodeId::Song(i) => Some(i),
                _ => None,
            });
        let (frame, drawn) = Frame::build(
            &graph,
            layout,
            axis,
            has_no_bpm,
            &anim,
            &keys,
            &arts,
            &shown,
            highlight,
            route_of,
            playing,
            self.map.hovered,
            &self.map,
            t,
        );
        let scope_label = scope.map(|n| {
            let kind = match n {
                NodeId::Artist(_) => "Artist",
                NodeId::Label(_) => "Label",
                NodeId::Playlist(_) => "Playlist",
                NodeId::Song(_) => "Song",
            };
            format!("{kind}: {}  ·  ", graph.name(n))
        });
        self.map.drawn = drawn;
        let picker = self.map.picker.map(|kind| self.picker(kind, &graph, t, cx));

        let songs = shown.len();
        let bounds_cell = self.map.bounds.clone();
        div()
            .id("map")
            .relative()
            .flex_1()
            .h_full()
            .overflow_hidden()
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, e: &MouseDownEvent, _, _| {
                    this.map.drag_from = Some(e.position);
                    this.map.dragged = false;
                }),
            )
            .on_mouse_move(cx.listener(|this, e: &MouseMoveEvent, _, cx| {
                match (this.map.drag_from, e.pressed_button) {
                    (Some(from), Some(MouseButton::Left)) => {
                        let delta = e.position - from;
                        if this.map.dragged || f32::from(delta.x.abs() + delta.y.abs()) > 3.0 {
                            this.map.dragged = true;
                            this.map.stop_view_motion();
                            this.map.pan = this.map.pan + delta;
                            this.map.drag_from = Some(e.position);
                            cx.notify();
                        }
                    }
                    _ => {
                        this.map.drag_from = None;
                        let hovered = this.map.hit(e.position);
                        if hovered != this.map.hovered {
                            this.map.hovered = hovered;
                            cx.notify();
                        }
                    }
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, e: &MouseUpEvent, _, cx| {
                    let was_drag = this.map.dragged;
                    this.map.drag_from = None;
                    this.map.dragged = false;
                    if was_drag {
                        return;
                    }
                    match (this.map.hit(e.position), &this.graph) {
                        (Some(s), Some(g)) => {
                            let key = g.key(NodeId::Song(s));
                            this.map.select(key);
                            // Double-click plays it.
                            if e.click_count >= 2 {
                                let path = g.song_paths[s].clone();
                                this.play_song(&path, cx);
                            }
                        }
                        // Empty space: drop the song selection but stay in the group.
                        _ if this.map.picker.is_some() => this.map.picker = None,
                        _ => {
                            if this.map.selected != this.map.scope {
                                this.map.back();
                            }
                        }
                    }
                    cx.notify();
                }),
            )
            .on_pinch(cx.listener(|this, e: &PinchEvent, _, cx| {
                this.map.pinch(e.position, e.delta, e.phase, std::time::Instant::now());
                cx.notify();
            }))
            .on_scroll(cx.listener(|this, e: &ScrollEvent, _, cx| {
                let d = e.delta.pixel_delta(px(16.));
                if e.modifiers.platform || e.modifiers.control {
                    let factor = (1.0 - f32::from(d.y) * 0.005).clamp(0.5, 2.0);
                    this.map.zoom_at(e.position, factor);
                } else {
                    this.map.stop_view_motion();
                    this.map.pan = this.map.pan + d;
                }
                cx.notify();
            }))
            .child(
                canvas(move |bounds, _, _| bounds_cell.set(bounds), move |bounds, _, window, cx| {
                    frame.paint(bounds, window, cx)
                })
                .size_full(),
            )
            .child(
                div()
                    .absolute()
                    .top_2()
                    .left_3()
                    .text_xs()
                    .text_color(t.text_muted)
                    .child(format!(
                        "{}{songs} songs shown  ·  {}{}",
                        scope_label.clone().unwrap_or_default(),
                        if self.watcher.is_some() { "● watching" } else { "not watching" },
                        self.notice.as_ref().map(|n| format!("  ·  {n}")).unwrap_or_default()
                    )),
            )
            .child(
                div()
                    .absolute()
                    .bottom_2()
                    .left_3()
                    .text_xs()
                    .text_color(t.text_muted)
                    .child("Click a song  ·  drag or scroll to pan  ·  pinch or ⌘ + scroll to zoom  ·  Esc to deselect"),
            )
            .children(picker)
            .into_any_element()
    }

    /// List of artists, labels or playlists to pick from; picking one highlights its songs.
    fn picker(&self, kind: Picker, graph: &Graph, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let t = *t;
        let (names, make): (&[String], fn(usize) -> NodeId) = match kind {
            Picker::Artists => (&graph.artists, NodeId::Artist),
            Picker::Labels => (&graph.labels, NodeId::Label),
            Picker::Playlists => (&graph.playlists, NodeId::Playlist),
        };
        let mut items: Vec<(String, NodeId, usize)> = names
            .iter()
            .enumerate()
            .map(|(i, n)| (n.clone(), make(i), graph.songs_of(make(i)).len()))
            .collect();
        items.sort_by_key(|(n, _, _)| n.to_lowercase());
        let empty = items.is_empty();
        div()
            .id("picker")
            .absolute()
            .top_8()
            .left_3()
            .w(px(260.))
            .max_h(px(420.))
            .overflow_y_scroll()
            .py_1()
            .rounded_lg()
            .bg(t.bg_elevated)
            .border_1()
            .border_color(t.border)
            .shadow_lg()
            // Clicks inside the list must not reach the map underneath.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_up(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .when(kind == Picker::Playlists, |d| {
                d.child(
                    div()
                        .id("new-playlist")
                        .px_3()
                        .py_1()
                        .cursor_pointer()
                        .text_color(t.accent)
                        .hover(|s| s.bg(t.bg_panel))
                        .child("+ New playlist…")
                        .on_click(cx.listener(|this, _, window, cx| {
                            let add = this.selected_song();
                            this.open_prompt(PromptKind::NewPlaylist { add }, window, cx);
                        })),
                )
            })
            .when(empty, |d| {
                d.child(div().px_3().py_1().text_color(t.text_muted).child("None yet"))
            })
            .children(items.into_iter().enumerate().map(|(row, (name, node, count))| {
                let key = graph.key(node);
                div()
                    .id(("pick", row))
                    .flex()
                    .justify_between()
                    .px_3()
                    .py_1()
                    .cursor_pointer()
                    .text_color(t.text)
                    .hover(|s| s.bg(t.bg_panel).text_color(t.accent))
                    .child(name)
                    .child(div().text_color(t.text_muted).child(count.to_string()))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.map.select(key.clone());
                        cx.notify();
                    }))
            }))
            .into_any_element()
    }

    /// Details for the selected node, with links to its neighbours.
    fn inspector(&self, t: &Theme, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let graph = self.graph.as_ref()?;
        let LibraryState::Loaded(lib) = &self.library else { return None };
        let sel = graph.find(self.map.selected.as_ref()?)?;
        let t = *t;

        let heading = |s: &str| div().mt_3().text_xs().text_color(t.text_muted).child(s.to_string());
        let link = |id: usize, n: NodeId, cx: &mut Context<Self>| {
            let key = graph.key(n);
            div()
                .id(("link", id))
                .py_0p5()
                .cursor_pointer()
                .text_color(t.text)
                .hover(|s| s.text_color(t.accent))
                .child(graph.name(n).to_string())
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.map.select(key.clone());
                    cx.notify();
                }))
        };

        let (kind, mut body) = match sel {
            NodeId::Song(i) => {
                let tr = &lib.tracks[i];
                let mut rows = vec![div()
                    .text_color(t.text_muted)
                    .child(format!(
                        "{} BPM{}  ·  {}",
                        tr.tempo().map(|b| format!("{b:.0}")).unwrap_or_else(|| "—".into()),
                        if tr.tempo_is_detected() { " (detected)" } else { "" },
                        match tr.best_key() {
                            Some(k) if tr.key_is_detected() => format!("{k} (detected)"),
                            Some(k) => k.to_string(),
                            None => "no key".into(),
                        }
                    ))
                    .into_any_element()];
                let path = tr.path.clone();
                let playing_this = self.player.track.as_ref() == Some(&tr.path) && self.player.is_playing();
                rows.push(
                    div()
                        .id("inspector-play")
                        .mt_2()
                        .px_3()
                        .py_1()
                        .rounded_md()
                        .cursor_pointer()
                        .bg(t.accent)
                        .hover(|s| s.bg(t.accent_hover))
                        .text_color(t.bg_app)
                        .child(if playing_this { "❚❚  Pause" } else { "▶  Play" })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if this.player.track.as_ref() == Some(&path) && !this.player.finished() {
                                this.player.toggle();
                                this.start_ticking(cx);
                                cx.notify();
                            } else {
                                this.play_song(&path, cx);
                            }
                        }))
                        .into_any_element(),
                );
                if let Some(a) = graph.artist_of(i) {
                    rows.push(heading("Artist").into_any_element());
                    rows.push(link(0, NodeId::Artist(a), cx).into_any_element());
                }
                if let Some(l) = graph.label_of(i) {
                    rows.push(heading("Label").into_any_element());
                    rows.push(link(1, NodeId::Label(l), cx).into_any_element());
                }
                rows.push(heading("Playlists").into_any_element());
                if graph.playlists_of(i).is_empty() {
                    rows.push(div().text_color(t.text_muted).child("None").into_any_element());
                }
                for (j, &p) in graph.playlists_of(i).iter().enumerate() {
                    rows.push(link(10 + j, NodeId::Playlist(p), cx).into_any_element());
                }
                rows.push(
                    div()
                        .id("add-to-playlist")
                        .mt_1()
                        .cursor_pointer()
                        .text_color(t.accent)
                        .child(if self.adding_to_playlist { "− Add to playlist" } else { "+ Add to playlist" })
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.adding_to_playlist = !this.adding_to_playlist;
                            cx.notify();
                        }))
                        .into_any_element(),
                );
                if self.adding_to_playlist {
                    let member: HashSet<usize> = graph.playlists_of(i).iter().copied().collect();
                    let mut names: Vec<(usize, String)> = graph.playlists.iter().cloned().enumerate().collect();
                    names.sort_by_key(|(_, n)| n.to_lowercase());
                    for (j, (p, name)) in names.into_iter().enumerate() {
                        let is_member = member.contains(&p);
                        let song = tr.path.clone();
                        rows.push(
                            div()
                                .id(("toggle-pl", j))
                                .flex()
                                .gap_2()
                                .pl_2()
                                .py_0p5()
                                .cursor_pointer()
                                .text_color(if is_member { t.text } else { t.text_muted })
                                .hover(|s| s.text_color(t.accent))
                                .child(div().w(px(14.)).text_color(t.accent).child(if is_member { "✓" } else { "" }))
                                .child(name.clone())
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    let (name, song) = (name.clone(), song.clone());
                                    this.playlist_op(
                                        move |root| {
                                            if is_member {
                                                playlists::remove(root, &name, &song).map(|_| ())
                                            } else {
                                                playlists::add(root, &name, &song).map(|_| ())
                                            }
                                        },
                                        cx,
                                    );
                                }))
                                .into_any_element(),
                        );
                    }
                    let song = tr.path.clone();
                    rows.push(
                        div()
                            .id("new-pl-with-song")
                            .pl_2()
                            .py_0p5()
                            .cursor_pointer()
                            .text_color(t.text_muted)
                            .hover(|s| s.text_color(t.accent))
                            .child("New playlist…")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_prompt(PromptKind::NewPlaylist { add: Some(song.clone()) }, window, cx);
                            }))
                            .into_any_element(),
                    );
                }
                let rel = tr.path.strip_prefix(&lib.root).unwrap_or(&tr.path);
                rows.push(heading("File").into_any_element());
                rows.push(
                    div().text_xs().text_color(t.text_muted).child(rel.display().to_string())
                        .into_any_element(),
                );
                ("Song", rows)
            }
            NodeId::Playlist(p) => {
                let name = graph.playlists[p].clone();
                let songs = graph.songs_of(sel);
                let button = |id: &'static str, label: String| {
                    div()
                        .id(id)
                        .px_2()
                        .py_0p5()
                        .rounded_md()
                        .cursor_pointer()
                        .bg(t.bg_elevated)
                        .hover(|s| s.bg(t.border))
                        .child(label)
                };
                let confirming = self.confirm_delete.as_deref() == Some(name.as_str());
                let (n1, n2) = (name.clone(), name.clone());
                let mut rows = vec![div()
                    .mt_2()
                    .flex()
                    .gap_2()
                    .child(button("rename-pl", "Rename".into()).text_color(t.text).on_click(cx.listener(
                        move |this, _, window, cx| this.open_prompt(PromptKind::Rename(n1.clone()), window, cx),
                    )))
                    .child(
                        button("delete-pl", if confirming { "Really delete?".into() } else { "Delete".into() })
                            .text_color(if confirming { t.accent } else { t.text })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if this.confirm_delete.as_deref() == Some(n2.as_str()) {
                                    this.confirm_delete = None;
                                    let name = n2.clone();
                                    this.playlist_op(move |root| playlists::delete(root, &name), cx);
                                    this.map.clear();
                                } else {
                                    this.confirm_delete = Some(n2.clone());
                                    cx.notify();
                                }
                            })),
                    )
                    .into_any_element()];
                rows.push(heading(&format!("Songs ({}) — in play order", songs.len())).into_any_element());
                let last = songs.len().saturating_sub(1);
                for (j, &s) in songs.iter().enumerate() {
                    let path = graph.song_paths[s].clone();
                    let (up, down, del) = (path.clone(), path.clone(), path.clone());
                    let (nu, nd, nx) = (name.clone(), name.clone(), name.clone());
                    let small = |id: (&'static str, usize), label: &'static str| {
                        div()
                            .id(id)
                            .w(px(18.))
                            .text_center()
                            .cursor_pointer()
                            .text_color(t.text_muted)
                            .hover(|s| s.text_color(t.accent))
                            .child(label)
                    };
                    rows.push(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(div().w(px(22.)).text_xs().text_color(t.text_muted).child(format!("{}", j + 1)))
                            .child(div().flex_1().overflow_hidden().child(link(100 + j, NodeId::Song(s), cx)))
                            .child(small(("pl-up", j), if j > 0 { "↑" } else { "" }).on_click(cx.listener(
                                move |this, _, _, cx| this.move_in_playlist(&nu, &up, -1, cx),
                            )))
                            .child(small(("pl-down", j), if j < last { "↓" } else { "" }).on_click(cx.listener(
                                move |this, _, _, cx| this.move_in_playlist(&nd, &down, 1, cx),
                            )))
                            .child(small(("pl-del", j), "×").on_click(cx.listener(move |this, _, _, cx| {
                                let (name, song) = (nx.clone(), del.clone());
                                this.playlist_op(move |root| playlists::remove(root, &name, &song).map(|_| ()), cx);
                            })))
                            .into_any_element(),
                    );
                }
                if songs.is_empty() {
                    rows.push(
                        div()
                            .text_color(t.text_muted)
                            .child("Empty. Select a song and use “Add to playlist”.")
                            .into_any_element(),
                    );
                }
                ("Playlist", rows)
            }
            n => {
                let (kind, what) = match n {
                    NodeId::Artist(_) => ("Artist", "Songs"),
                    NodeId::Label(_) => ("Label", "Artists"),
                    _ => ("Playlist", "Songs"),
                };
                let items = graph.neighbours(n);
                let mut rows = vec![heading(&format!("{what} ({})", items.len())).into_any_element()];
                for (j, m) in items.into_iter().enumerate().take(200) {
                    rows.push(link(100 + j, m, cx).into_any_element());
                }
                (kind, rows)
            }
        };
        body.insert(
            0,
            div().text_lg().text_color(t.text).child(graph.name(sel).to_string()).into_any_element(),
        );
        body.insert(0, div().text_xs().text_color(t.accent).child(kind).into_any_element());

        Some(
            div()
                .id("inspector")
                .w(px(280.))
                .h_full()
                .flex_none()
                .overflow_y_scroll()
                .p_3()
                .bg(t.bg_panel)
                .border_l_1()
                .border_color(t.border)
                .shadow_lg()
                // Clicks and scrolling in the panel must not reach the map underneath.
                .occlude()
                .children(body),
        )
    }

    /// Play/pause, what's playing, time, and a progress bar you can click to seek.
    fn preview_bar(&self, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let t = *t;
        let p = &self.player;
        let info = p.track.as_ref().and_then(|path| match &self.library {
            LibraryState::Loaded(lib) => lib.tracks.iter().find(|tr| &tr.path == path),
            _ => None,
        });
        let (title, artist) = match info {
            Some(tr) => (tr.title.clone().unwrap_or_default(), tr.artist.clone().unwrap_or_default()),
            None if self.selected_song().is_some() => ("Press Space to play the selected song".into(), String::new()),
            None => ("Nothing playing".into(), String::new()),
        };
        let pos = p.position();
        let frac = p.duration.map_or(0.0, |d| (pos.as_secs_f32() / d.as_secs_f32().max(0.001)).clamp(0.0, 1.0));
        let time = match p.duration {
            _ if p.track.is_none() => String::new(),
            Some(d) => format!("{} / {}", player::clock(pos), player::clock(d)),
            None => player::clock(pos),
        };
        let bounds_cell = self.progress_bounds.clone();
        let wave = p.track.as_ref().and_then(|path| self.waves.get(path).cloned());
        div()
            .flex()
            .items_center()
            .gap_3()
            .h(px(44.))
            .px_3()
            .bg(t.bg_panel)
            .border_t_1()
            .border_color(t.border)
            .text_color(t.text_muted)
            .child(
                div()
                    .id("play")
                    .w(px(28.))
                    .h(px(28.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .cursor_pointer()
                    .bg(t.bg_elevated)
                    .hover(|s| s.bg(t.border))
                    .text_color(t.accent)
                    .child(if p.is_playing() { "❚❚" } else { "▶" })
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_play(cx))),
            )
            .child(
                div()
                    .w(px(260.))
                    .flex()
                    .flex_col()
                    .overflow_hidden()
                    .child(div().text_color(t.text).truncate().child(title))
                    .child(div().text_xs().truncate().child(artist)),
            )
            .child(
                div()
                    .id("progress")
                    .flex_1()
                    .h(px(34.))
                    .cursor_pointer()
                    .child(
                        canvas(move |b, _, _| bounds_cell.set(b), move |b, _, window, _| {
                            paint_waveform(window, b, wave.as_deref().map(|w| w.as_slice()), frac, &t)
                        })
                        .size_full(),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, e: &MouseDownEvent, _, cx| {
                            let b = this.progress_bounds.get();
                            let w = f32::from(b.size.width);
                            if let (Some(d), true) = (this.player.duration, w > 0.0) {
                                let f = (f32::from(e.position.x - b.origin.x) / w).clamp(0.0, 1.0);
                                this.player.seek(d.mul_f32(f));
                                cx.notify();
                            }
                        }),
                    ),
            )
            .child(div().w(px(90.)).text_xs().child(time))
            .children(p.error.clone().map(|e| div().text_xs().text_color(t.accent).child(e)))
    }

    fn palette(&self, mode: PaletteMode, t: &Theme) -> impl IntoElement {
        let placeholder: SharedString = match mode {
            PaletteMode::QuickOpen => "Search songs, artists, labels, playlists…  (type > for commands)",
            PaletteMode::Commands => "> New playlist · Sync to USB · Open folder… (⌘O)",
        }
        .into();
        // Full-window overlay with the palette anchored near the top, like VS Code.
        div()
            .absolute()
            .size_full()
            .flex()
            .justify_center()
            .pt(px(60.))
            .child(
                div()
                    .w(px(560.))
                    .h(px(36.))
                    .px_3()
                    .flex()
                    .items_center()
                    .rounded_lg()
                    .bg(t.bg_elevated)
                    .border_1()
                    .border_color(t.accent)
                    .shadow_lg()
                    .text_color(t.text_muted)
                    .child(placeholder),
            )
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = *Theme::get(cx);
        self.visible = match &self.library {
            LibraryState::Loaded(lib) => (0..lib.tracks.len())
                .filter(|&i| !self.only_new || self.new_tracks.contains(&lib.tracks[i].path))
                .collect(),
            _ => Vec::new(),
        };
        div()
            .track_focus(&self.focus)
            .key_context("Workspace")
            .on_action(cx.listener(|this, _: &QuickOpen, _, cx| {
                this.palette = Some(PaletteMode::QuickOpen);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &CommandPalette, _, cx| {
                this.palette = Some(PaletteMode::Commands);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &OpenFolder, _, cx| this.open_folder(cx)))
            .on_action(cx.listener(|this, _: &TogglePlay, _, cx| this.toggle_play(cx)))
            .on_action(cx.listener(|this, _: &SkipBack, _, cx| {
                this.player.skip(-10.0);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &SkipForward, _, cx| {
                this.player.skip(10.0);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &Confirm, window, cx| this.confirm_prompt(window, cx)))
            .on_action(cx.listener(|this, _: &Dismiss, window, cx| {
                // Esc peels back one layer: prompt, palette, picker, song selection, then group.
                if this.prompt.is_some() {
                    this.close_prompt(window, cx);
                    return;
                }
                if this.sync.as_ref().is_some_and(|d| d.running.is_none()) {
                    this.sync = None;
                    cx.notify();
                    return;
                }
                if this.palette.is_some() {
                    this.palette = None;
                } else if this.map.picker.is_some() {
                    this.map.picker = None;
                } else {
                    this.map.back();
                }
                cx.notify();
            }))
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(t.bg_app)
            .text_sm()
            .child(self.top_bar(&t, cx))
            .child(self.canvas_area(&t, window, cx))
            .child(self.preview_bar(&t, cx))
            .children(self.palette.map(|m| self.palette(m, &t)))
            .children(self.prompt.as_ref().map(|p| self.prompt_view(p, &t)))
            .children(self.sync_view(&t, cx))
    }
}

/// Draws a track's waveform as mirrored bars (played part in the accent colour) with a playhead.
/// Without a waveform yet, draws a thin progress line.
fn paint_waveform(window: &mut Window, b: Bounds<gpui::Pixels>, wave: Option<&[f32]>, frac: f32, t: &Theme) {
    use gpui::{fill, point, Hsla};
    let (w, h) = (f32::from(b.size.width), f32::from(b.size.height));
    let accent: Hsla = t.accent.into();
    let muted: Hsla = Hsla::from(t.text_muted).opacity(0.45);
    let Some(wave) = wave.filter(|w| !w.is_empty() && w.len() >= 2) else {
        let line = |x0: f32, x1: f32, c: Hsla| {
            fill(Bounds::new(point(b.origin.x + px(x0), b.center().y - px(1.5)), size(px((x1 - x0).max(0.)), px(3.))), c)
        };
        window.paint_quad(line(0., w, t.bg_elevated.into()));
        window.paint_quad(line(0., w * frac, accent));
        return;
    };
    const BAR: f32 = 2.0;
    const GAP: f32 = 1.0;
    let n = ((w / (BAR + GAP)) as usize).max(1);
    for k in 0..n {
        let lo = k * wave.len() / n;
        let hi = ((k + 1) * wave.len() / n).max(lo + 1).min(wave.len());
        let v = wave[lo..hi].iter().cloned().fold(0.0f32, f32::max);
        let bar_h = (v * h).max(1.5);
        let x = b.origin.x + px(k as f32 * (BAR + GAP));
        let played = (k as f32 + 0.5) / n as f32 <= frac;
        window.paint_quad(fill(
            Bounds::new(point(x, b.center().y - px(bar_h / 2.)), size(px(BAR), px(bar_h))),
            if played { accent } else { muted },
        ));
    }
    let head = b.origin.x + px(w * frac);
    window.paint_quad(fill(Bounds::new(point(head - px(0.75), b.origin.y), size(px(1.5), b.size.height)), Hsla::from(t.text)));
}

fn remember_library(root: &std::path::Path) {
    let Some(path) = settings_path() else { return };
    let mut settings = Settings::load(&path);
    settings.last_library = Some(root.to_path_buf());
    if let Err(e) = settings.save(&path) {
        eprintln!("trackmap: could not save settings: {e}");
    }
}

fn main() {
    gpui_platform::application().run(|cx: &mut App| {
        cx.set_global(Theme::dark());
        cx.bind_keys([
            KeyBinding::new("cmd-p", QuickOpen, None),
            KeyBinding::new("cmd-shift-p", CommandPalette, None),
            KeyBinding::new("cmd-o", OpenFolder, None),
            KeyBinding::new("escape", Dismiss, None),
            KeyBinding::new("enter", Confirm, Some("TextInput")),
            KeyBinding::new("space", TogglePlay, Some("Workspace")),
            KeyBinding::new("left", SkipBack, Some("Workspace")),
            KeyBinding::new("right", SkipForward, Some("Workspace")),
            KeyBinding::new("cmd-q", Quit, None),
        ]);
        input::bind_keys(cx);
        cx.on_action(|_: &Quit, cx| cx.quit());

        let bounds = Bounds::centered(None, size(px(1280.), px(800.)), cx);
        let window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(TitlebarOptions {
                        title: Some("TrackMap".into()),
                        appears_transparent: true,
                        traffic_light_position: Some(point(px(12.), px(13.))),
                    }),
                    ..Default::default()
                },
                |_, cx| {
                    cx.new(|cx| {
                        let mut ws = Workspace::new(cx);
                        // `trackmap <folder>` opens that folder; otherwise reopen the last one.
                        let root = std::env::args_os().nth(1).map(PathBuf::from).or_else(|| {
                            let s = Settings::load(&settings_path()?);
                            s.last_library.filter(|p| p.is_dir())
                        });
                        if let Some(root) = root {
                            ws.load(root, cx);
                        }
                        ws
                    })
                },
            )
            .expect("failed to open window");

        window
            .update(cx, |ws, window, cx| window.focus(&ws.focus, cx))
            .expect("failed to focus window");
        cx.activate(true);
    });
}
