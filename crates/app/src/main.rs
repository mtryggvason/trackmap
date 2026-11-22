mod map;
mod theme;

use std::collections::HashSet;
use std::ops::Range;
use std::path::PathBuf;
use std::rc::Rc;

use futures::StreamExt;

use gpui::{
    actions, canvas, div, point, prelude::*, px, size, uniform_list, AnyElement, App,
    Application, Bounds, Context, FocusHandle, KeyBinding, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, PathPromptOptions, ScrollWheelEvent, SharedString,
    TitlebarOptions, Window, WindowBounds, WindowOptions,
};
use map::{Frame, MapState};
use theme::Theme;
use trackmap_core::settings::Settings;
use trackmap_core::{playlists, Library, Playlist, Watcher, PLAYLISTS_DIR};
use trackmap_graph::{Graph, HarmonicLayout, NodeId};

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
        }
    }

    /// Rebuilds the graph after the library or playlists change.
    fn rebuild_graph(&mut self) {
        self.graph = match &self.library {
            LibraryState::Loaded(lib) => Some(Rc::new(Graph::build(
                &lib.tracks,
                &self.playlists,
                &HarmonicLayout::default(),
            ))),
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
            .child(
                toggle("playlists", "Playlists".into(), self.map.show_playlists).on_click(
                    cx.listener(|this, _, _, cx| {
                        this.map.show_playlists = !this.map.show_playlists;
                        cx.notify();
                    }),
                ),
            )
            .child(toggle("labels", "Labels".into(), self.map.show_labels).on_click(cx.listener(
                |this, _, _, cx| {
                    this.map.show_labels = !this.map.show_labels;
                    cx.notify();
                },
            )))
            .when(!self.map.expanded.is_empty(), |d| {
                d.child(toggle("clear", "Clear ✕".into(), false).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.map.expanded.clear();
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
                                        tr.bpm.map(|b| format!("{b:.0}")).unwrap_or_else(dash),
                                    ))
                                    .child(div().w(px(40.)).text_color(t.accent).child(
                                        tr.key.map(|k| k.to_string()).unwrap_or_default(),
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
        let keys: Vec<_> = lib.tracks.iter().map(|tr| tr.key).collect();
        let expanded: Vec<NodeId> =
            self.map.expanded.iter().filter_map(|k| graph.find(k)).collect();
        let selected = self.map.selected.as_ref().and_then(|k| graph.find(k));
        let visible = graph.visible(
            self.visible.iter().copied(),
            self.map.show_playlists,
            self.map.show_labels,
            &expanded,
        );
        let edges = graph.edges(&visible);
        let frame = Frame::build(
            &graph,
            &keys,
            &visible,
            &edges,
            selected,
            self.map.hovered,
            &self.map,
            t,
        );
        let mut drawn: Vec<_> = visible.iter().map(|&n| (n, graph.position(n))).collect();
        drawn.sort_by_key(|(n, _)| !matches!(n, NodeId::Song(_)));
        self.map.drawn = drawn;

        let songs = visible.iter().filter(|n| matches!(n, NodeId::Song(_))).count();
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
                        (Some(n), Some(g)) => {
                            let key = g.key(n);
                            this.map.select(key);
                        }
                        _ => this.map.selected = None,
                    }
                    cx.notify();
                }),
            )
            .on_scroll_wheel(cx.listener(|this, e: &ScrollWheelEvent, _, cx| {
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
                        "{songs} songs shown  ·  {}",
                        if self.watcher.is_some() { "● watching" } else { "not watching" }
                    )),
            )
            .child(
                div()
                    .absolute()
                    .bottom_2()
                    .left_3()
                    .text_xs()
                    .text_color(t.text_muted)
                    .child("Click to expand  ·  drag or scroll to pan  ·  ⌘ + scroll to zoom  ·  Esc to deselect"),
            )
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
                        "{} BPM  ·  {}",
                        tr.bpm.map(|b| format!("{b:.0}")).unwrap_or_else(|| "—".into()),
                        tr.key.map(|k| k.to_string()).unwrap_or_else(|| "no key".into())
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
                // Esc peels back one layer: palette, then selection, then expanded nodes.
                if this.palette.is_some() {
                    this.palette = None;
                } else if this.map.selected.is_some() {
                    this.map.selected = None;
                } else {
                    this.map.expanded.clear();
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
    Application::new().run(|cx: &mut App| {
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
            .update(cx, |ws, window, _| window.focus(&ws.focus))
            .expect("failed to focus window");
        cx.activate(true);
    });
}
