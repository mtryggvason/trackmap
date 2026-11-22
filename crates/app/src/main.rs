mod map;
mod theme;

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::ops::Range;
use std::path::PathBuf;
use std::rc::Rc;

use futures::StreamExt;

use gpui::{
    actions, canvas, div, point, prelude::*, px, size, uniform_list, AnyElement, App,
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

actions!(trackmap, [QuickOpen, CommandPalette, OpenFolder, Dismiss, Quit]);

enum LibraryState {
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

struct Workspace {
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

fn settings_path() -> Option<PathBuf> {
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
        }
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
            .when(self.map.selected.is_some(), |d| {
                d.child(toggle("clear", "Clear ✕".into(), false).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.map.selected = None;
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
                    .px_3()
                    .py_0p5()
                    .rounded_md()
                    .bg(t.accent)
                    .hover(|s| s.bg(t.accent_hover))
                    .text_color(t.bg_app)
                    .child("Sync ⇪"),
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

    fn canvas_area(&mut self, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
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
                return div()
                    .flex_1()
                    .flex()
                    .bg(t.bg_app)
                    .child(self.map_view(t, cx))
                    .children(self.inspector(t, cx))
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
                    cx.processor(move |this, range: Range<usize>, _, _| {
                        let LibraryState::Loaded(lib) = &this.library else { return Vec::new() };
                        range
                            .filter_map(|row| this.visible.get(row).map(|&i| (row, i)))
                            .map(|(row, i)| {
                                let tr = &lib.tracks[i];
                                let is_new = this.new_tracks.contains(&tr.path);
                                let dash = || "—".to_string();
                                div()
                                    .id(row)
                                    .flex()
                                    .gap_3()
                                    .px_3()
                                    .h(px(26.))
                                    .items_center()
                                    .hover(|s| s.bg(t.bg_elevated))
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

    fn map_view(&mut self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
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
        let (frame, drawn) = Frame::build(
            &graph,
            &keys,
            &arts,
            &self.visible,
            selected,
            self.map.hovered,
            &self.map,
            t,
        );
        self.map.drawn = drawn;
        let picker = self.map.picker.map(|kind| self.picker(kind, &graph, t, cx));

        let songs = self.visible.len();
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
                        }
                        _ => {
                            this.map.selected = None;
                            this.map.picker = None;
                        }
                    }
                    cx.notify();
                }),
            )
            .on_pinch(cx.listener(|this, e: &PinchEvent, _, cx| {
                this.map.zoom_at(e.position, (1.0 + e.delta).clamp(0.5, 2.0));
                cx.notify();
            }))
            .on_scroll(cx.listener(|this, e: &ScrollEvent, _, cx| {
                let d = e.delta.pixel_delta(px(16.));
                if e.modifiers.platform || e.modifiers.control {
                    let factor = (1.0 - f32::from(d.y) * 0.005).clamp(0.5, 2.0);
                    this.map.zoom_at(e.position, factor);
                } else {
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
                        "{songs} songs shown  ·  {}{}",
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
                let rel = tr.path.strip_prefix(&lib.root).unwrap_or(&tr.path);
                rows.push(heading("File").into_any_element());
                rows.push(
                    div().text_xs().text_color(t.text_muted).child(rel.display().to_string())
                        .into_any_element(),
                );
                ("Song", rows)
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
                .children(body),
        )
    }

    fn preview_bar(&self, t: &Theme) -> impl IntoElement {
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
            .child(div().text_color(t.accent).child("▶"))
            .child("Nothing playing")
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
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
            .on_action(cx.listener(|this, _: &Dismiss, _, cx| {
                // Esc peels back one layer: palette, then picker, then selection.
                if this.palette.is_some() {
                    this.palette = None;
                } else if this.map.picker.is_some() {
                    this.map.picker = None;
                } else {
                    this.map.selected = None;
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
            .child(self.canvas_area(&t, cx))
            .child(self.preview_bar(&t))
            .children(self.palette.map(|m| self.palette(m, &t)))
    }
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
            KeyBinding::new("cmd-q", Quit, None),
        ]);
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
