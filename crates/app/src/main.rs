mod theme;

use std::collections::HashSet;
use std::ops::Range;
use std::path::PathBuf;

use futures::StreamExt;

use gpui::{
    actions, div, point, prelude::*, px, size, uniform_list, App, Application, Bounds, Context,
    FocusHandle, KeyBinding, PathPromptOptions, SharedString, TitlebarOptions, Window,
    WindowBounds, WindowOptions,
};
use napworks_core::settings::Settings;
use napworks_core::{Library, Watcher};
use theme::Theme;

actions!(napworks, [QuickOpen, CommandPalette, OpenFolder, Dismiss, Quit]);

enum LibraryState {
    Empty,
    Scanning(PathBuf),
    Loaded(Library),
    Failed(PathBuf, String),
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
}

fn settings_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("napworks").join("settings.json"))
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
        }
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
        self.library = LibraryState::Scanning(root.clone());
        cx.notify();

        cx.spawn(async move |this, cx| {
            let scan_root = root.clone();
            let result = cx
                .background_executor()
                .spawn(async move { napworks_core::open(&scan_root) })
                .await;

            this.update(cx, |ws, cx| {
                if ws.generation != generation {
                    return;
                }
                match result {
                    Ok(report) => {
                        remember_library(&report.library.root);
                        ws.new_tracks = report.new.into_iter().collect();
                        ws.library = LibraryState::Loaded(report.library);
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
        match napworks_core::watch(&root, move |paths| {
            let _ = tx.unbounded_send(paths);
        }) {
            Ok(w) => self.watcher = Some(w),
            Err(e) => {
                eprintln!("napworks: not watching {}: {e}", root.display());
                return;
            }
        }

        cx.spawn(async move |this, cx| {
            // Ends when the watcher (and with it the sender) is dropped.
            while let Some(mut paths) = rx.next().await {
                while let Ok(more) = rx.try_recv() {
                    paths.extend(more);
                }
                let changes_root = root.clone();
                let changes = cx
                    .background_executor()
                    .spawn(async move { napworks_core::changes_for(&changes_root, paths) })
                    .await;
                if changes.is_empty() {
                    continue;
                }

                let snapshot = this.update(cx, |ws, cx| {
                    if ws.generation != generation {
                        return None;
                    }
                    let LibraryState::Loaded(lib) = &mut ws.library else { return None };
                    let added = lib.apply(changes);
                    ws.new_tracks.extend(added);
                    cx.notify();
                    Some(lib.clone())
                });
                let Ok(Some(lib)) = snapshot else { break };
                cx.background_executor()
                    .spawn(async move {
                        let _ = napworks_core::cache::save(&lib);
                    })
                    .await;
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
                div()
                    .id("new-filter")
                    .px_2()
                    .py_0p5()
                    .rounded_md()
                    .cursor_pointer()
                    .when(self.only_new, |d| d.bg(t.accent).text_color(t.bg_app))
                    .when(!self.only_new, |d| {
                        d.bg(t.bg_elevated).text_color(t.text_muted).hover(|s| s.text_color(t.text))
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.only_new = !this.only_new;
                        cx.notify();
                    }))
                    .child(match self.new_tracks.len() {
                        0 => "New".to_string(),
                        n => format!("New {n}"),
                    }),
            )
            .child(chip("Playlists"))
            .child(chip("Labels"))
            .child(div().flex_1())
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

    fn canvas_area(&self, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
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
            LibraryState::Loaded(lib) => self.track_list(lib, t, cx).into_any_element(),
        };
        div().flex_1().flex().flex_col().bg(t.bg_app).child(body)
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
                this.palette = None;
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
        eprintln!("napworks: could not save settings: {e}");
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
                        title: Some("napworks".into()),
                        appears_transparent: true,
                        traffic_light_position: Some(point(px(12.), px(13.))),
                    }),
                    ..Default::default()
                },
                |_, cx| {
                    cx.new(|cx| {
                        let mut ws = Workspace::new(cx);
                        // `napworks <folder>` opens that folder; otherwise reopen the last one.
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
