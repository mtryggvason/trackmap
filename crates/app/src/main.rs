mod theme;

use gpui::{
    actions, div, point, prelude::*, px, size, App, Application, Bounds, Context, FocusHandle,
    KeyBinding, SharedString, TitlebarOptions, Window, WindowBounds, WindowOptions,
};
use theme::Theme;

actions!(napworks, [QuickOpen, CommandPalette, Dismiss, Quit]);

#[derive(Clone, Copy, PartialEq)]
enum PaletteMode {
    QuickOpen,
    Commands,
}

struct Workspace {
    focus: FocusHandle,
    palette: Option<PaletteMode>,
}

impl Workspace {
    fn new(cx: &mut Context<Self>) -> Self {
        Self { focus: cx.focus_handle(), palette: None }
    }

    fn top_bar(&self, t: &Theme) -> impl IntoElement {
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
            .child(div().text_color(t.text).child("No library ▾"))
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
            .child(chip("New"))
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

    fn canvas_area(&self, t: &Theme) -> impl IntoElement {
        div()
            .flex_1()
            .flex()
            .items_center()
            .justify_center()
            .bg(t.bg_app)
            .text_color(t.text_muted)
            .child("Open a folder to start  ·  ⌘P to search")
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
            PaletteMode::Commands => "> New playlist · Sync to USB · Open folder…",
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
            .child(self.top_bar(&t))
            .child(self.canvas_area(&t))
            .child(self.preview_bar(&t))
            .children(self.palette.map(|m| self.palette(m, &t)))
    }
}

fn main() {
    Application::new().run(|cx: &mut App| {
        cx.set_global(Theme::dark());
        cx.bind_keys([
            KeyBinding::new("cmd-p", QuickOpen, None),
            KeyBinding::new("cmd-shift-p", CommandPalette, None),
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
                |_, cx| cx.new(Workspace::new),
            )
            .expect("failed to open window");

        window
            .update(cx, |ws, window, _| window.focus(&ws.focus))
            .expect("failed to focus window");
        cx.activate(true);
    });
}
