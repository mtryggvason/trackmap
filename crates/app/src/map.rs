//! The song map: pan/zoom state, painting and hit-testing. Only songs are drawn; selecting an
//! artist, label or playlist highlights its songs.

use std::cell::Cell;
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    hsla, point, px, quad, size, transparent_black, App, BorderStyle, Bounds, Hsla, PathBuilder,
    Pixels, Point, RenderImage, SharedString, TextRun, Window,
};
use trackmap_core::Camelot;
use trackmap_graph::{Graph, MapLayout, NodeId, NodeKey, Point as WorldPoint};

/// Which list the Artists / Labels / Playlists chip has open.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Picker {
    Artists,
    Labels,
    Playlists,
}

use crate::theme::Theme;

/// World extent that fits the screen at zoom 1: the layout plus room for the key labels on
/// the left, the no-BPM column, and the BPM axis below.
const WORLD_HALF_W: f32 = 1060.0;
const WORLD_HALF_H: f32 = 580.0;
const HIT_RADIUS: f32 = 10.0;

/// On-screen edge length of a cover at this zoom: grows gently as you zoom in.
pub fn cover_size(zoom: f32) -> f32 {
    (16.0 * zoom.sqrt()).clamp(14.0, 72.0)
}

pub struct MapState {
    /// Screen-space offset of the world origin from the canvas centre.
    pub pan: Point<Pixels>,
    pub zoom: f32,
    /// A song, or an artist/label/playlist whose songs are highlighted.
    pub selected: Option<NodeKey>,
    /// Song index under the cursor.
    pub hovered: Option<usize>,
    pub drag_from: Option<Point<Pixels>>,
    pub dragged: bool,
    pub picker: Option<Picker>,
    /// Canvas bounds from the last paint, for hit-testing mouse events.
    pub bounds: Rc<Cell<Bounds<Pixels>>>,
    /// Songs drawn last frame, in paint order.
    pub drawn: Vec<(usize, WorldPoint)>,
}

impl Default for MapState {
    fn default() -> Self {
        Self {
            pan: point(px(0.), px(0.)),
            zoom: 1.0,
            selected: None,
            hovered: None,
            drag_from: None,
            dragged: false,
            picker: None,
            bounds: Rc::new(Cell::new(Bounds::default())),
            drawn: Vec::new(),
        }
    }
}

#[derive(Clone, Copy)]
pub struct Transform {
    center: Point<Pixels>,
    scale: f32,
}

impl Transform {
    pub fn new(bounds: Bounds<Pixels>, pan: Point<Pixels>, zoom: f32) -> Self {
        let w = f32::from(bounds.size.width);
        let h = f32::from(bounds.size.height);
        let fit = (w / (2.0 * WORLD_HALF_W)).min(h / (2.0 * WORLD_HALF_H)).max(0.0001);
        Self { center: bounds.center() + pan, scale: fit * zoom }
    }

    pub fn to_screen(&self, p: WorldPoint) -> Point<Pixels> {
        point(self.center.x + px(p.x * self.scale), self.center.y + px(p.y * self.scale))
    }

    pub fn to_world(&self, s: Point<Pixels>) -> WorldPoint {
        WorldPoint {
            x: f32::from(s.x - self.center.x) / self.scale,
            y: f32::from(s.y - self.center.y) / self.scale,
        }
    }
}

impl MapState {
    pub fn transform(&self) -> Transform {
        Transform::new(self.bounds.get(), self.pan, self.zoom)
    }

    /// Topmost song under the cursor.
    pub fn hit(&self, pos: Point<Pixels>) -> Option<usize> {
        let tf = self.transform();
        self.drawn
            .iter()
            .rev()
            .map(|&(n, p)| {
                let s = tf.to_screen(p);
                let (dx, dy) = (f32::from(s.x - pos.x), f32::from(s.y - pos.y));
                (n, dx * dx + dy * dy)
            })
            .filter(|&(_, d2)| {
                let r = HIT_RADIUS.max(cover_size(self.zoom) / 2.0);
                d2 <= r * r
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(n, _)| n)
    }

    /// Zooms by `factor`, keeping the world point under `anchor` fixed on screen.
    pub fn zoom_at(&mut self, anchor: Point<Pixels>, factor: f32) {
        let before = self.transform();
        let world = before.to_world(anchor);
        self.zoom = (self.zoom * factor).clamp(0.3, 40.0);
        let after = self.transform();
        let moved = after.to_screen(world);
        self.pan = self.pan + (anchor - moved);
    }

    pub fn select(&mut self, key: NodeKey) {
        self.selected = Some(key);
        self.picker = None;
    }
}

pub fn key_color(k: Camelot) -> Hsla {
    hsla((k.number - 1) as f32 / 12.0, 0.65, 0.6, 1.0)
}

struct DrawSong {
    pos: WorldPoint,
    art: Option<Arc<RenderImage>>,
    dimmed: bool,
    fill: Hsla,
    ring: Hsla,
    big: bool,
    label: Option<SharedString>,
    label_color: Hsla,
}

/// Everything needed to paint one frame, owned so it can move into the canvas closure.
pub struct Frame {
    songs: Vec<DrawSong>,
    /// A selected playlist's songs in play order.
    route: Vec<WorldPoint>,
    layout: MapLayout,
    pan: Point<Pixels>,
    zoom: f32,
    theme: Theme,
}

impl Frame {
    /// `shown` are the songs passing the filters. A selection's songs are always shown.
    /// Returns the frame and the songs drawn, in paint order, for hit-testing.
    #[allow(clippy::too_many_arguments)]
    pub fn build(
        graph: &Graph,
        keys: &[Option<Camelot>],
        arts: &[Option<Arc<RenderImage>>],
        shown: &[usize],
        selected: Option<NodeId>,
        hovered: Option<usize>,
        state: &MapState,
        theme: &Theme,
    ) -> (Self, Vec<(usize, WorldPoint)>) {
        let accent: Hsla = theme.accent.into();
        let text: Hsla = theme.text.into();
        let muted: Hsla = theme.text_muted.into();

        let focus_list = selected.map(|n| graph.songs_of(n)).unwrap_or_default();
        let focus: HashSet<usize> = focus_list.iter().copied().collect();
        let has_focus = selected.is_some();

        // Focused songs are painted last so they sit on top of dimmed ones.
        let mut order: Vec<usize> = shown.iter().copied().filter(|s| !focus.contains(s)).collect();
        order.extend(focus_list.iter().copied());

        let crowded = shown.len() > 40;
        let song_labels_zoomed = state.zoom >= 3.0 && shown.len() <= 600;
        let label_focus = focus.len() <= 60;

        let mut drawn = Vec::with_capacity(order.len());
        let songs = order
            .iter()
            .map(|&s| {
                let pos = graph.song_position(s);
                drawn.push((s, pos));
                let in_focus = focus.contains(&s);
                let is_hover = hovered == Some(s);
                let ring = keys[s].map(key_color).unwrap_or(muted);
                let (fill, ring) = match (has_focus, in_focus) {
                    (true, true) => (accent, ring),
                    (true, false) => (hsla(0., 0., 0.55, 0.2), ring.opacity(0.2)),
                    _ => (hsla(0., 0., 0.55, 1.), ring),
                };
                let labelled =
                    is_hover || (in_focus && label_focus) || !crowded || song_labels_zoomed;
                DrawSong {
                    pos,
                    art: arts.get(s).cloned().flatten(),
                    dimmed: has_focus && !in_focus,
                    fill,
                    ring,
                    big: is_hover || (in_focus && focus.len() == 1),
                    label: labelled.then(|| SharedString::from(graph.song_titles[s].clone())),
                    label_color: if is_hover || in_focus {
                        text
                    } else if has_focus {
                        muted.opacity(0.3)
                    } else {
                        muted
                    },
                }
            })
            .collect();

        let route = match selected {
            Some(NodeId::Playlist(_)) => focus_list.iter().map(|&s| graph.song_position(s)).collect(),
            _ => Vec::new(),
        };

        let layout = graph.layout;
        (Self { songs, route, layout, pan: state.pan, zoom: state.zoom, theme: *theme }, drawn)
    }

    pub fn paint(self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
        let tf = Transform::new(bounds, self.pan, self.zoom);
        let t = &self.theme;
        let font = window.text_style().font();

        // BPM axis along the bottom, key rows down the left.
        let l = &self.layout;
        let muted: Hsla = t.text_muted.into();
        let axis_y = l.half_height + 36.;
        for bpm in l.bpm_ticks() {
            let s = tf.to_screen(WorldPoint { x: l.bpm_x(bpm), y: axis_y });
            paint_text(window, cx, &font, &format!("{bpm:.0}"), 11., muted, s, true);
        }
        let s = tf.to_screen(WorldPoint { x: l.no_bpm_x(), y: axis_y });
        paint_text(window, cx, &font, "no BPM", 11., muted.opacity(0.6), s, true);
        let label_x = l.no_bpm_x() - 70.;
        for n in 1..=12u8 {
            for minor in [true, false] {
                let k = Camelot::new(n, minor).unwrap();
                let s = tf.to_screen(WorldPoint { x: label_x, y: l.key_y(Some(k)) });
                paint_text(window, cx, &font, &k.to_string(), 10., key_color(k).opacity(0.6), s, true);
            }
        }
        let s = tf.to_screen(WorldPoint { x: label_x, y: l.key_y(None) });
        paint_text(window, cx, &font, "?", 10., muted.opacity(0.6), s, true);

        // A selected playlist's route, in play order.
        if self.route.len() > 1 {
            let mut path = PathBuilder::stroke(px(1.5));
            path.move_to(tf.to_screen(self.route[0]));
            for p in &self.route[1..] {
                path.line_to(tf.to_screen(*p));
            }
            if let Ok(p) = path.build() {
                window.paint_path(p, Hsla::from(t.accent).opacity(0.6));
            }
        }

        let cover = cover_size(self.zoom);
        let bg: Hsla = t.bg_app.into();
        for s in &self.songs {
            let c = tf.to_screen(s.pos);
            let d = if let Some(img) = &s.art {
                // Cover: image, a dimming veil when out of focus, then the key-coloured outline.
                let d = px(if s.big { cover * 1.5 } else { cover });
                let b = Bounds::new(point(c.x - d / 2., c.y - d / 2.), size(d, d));
                let radius = px(3.);
                let _ = window.paint_image(b, b, radius.into(), img.clone(), 0, s.dimmed);
                if s.dimmed {
                    window.paint_quad(quad(b, radius, bg.opacity(0.7), px(0.), bg, BorderStyle::Solid));
                }
                window.paint_quad(quad(b, radius, transparent_black(), px(1.5), s.ring, BorderStyle::Solid));
                d
            } else {
                let d = px(if s.big { 13. } else { 9. });
                window.paint_quad(quad(
                    Bounds::new(point(c.x - d / 2., c.y - d / 2.), size(d, d)),
                    d / 2.,
                    s.fill,
                    px(1.5),
                    s.ring,
                    BorderStyle::Solid,
                ));
                d
            };
            if let Some(label) = &s.label {
                let at = point(c.x + d / 2. + px(5.), c.y);
                paint_text(window, cx, &font, label, 12., s.label_color, at, false);
            }
        }

        // Play-order numbers on top of the route.
        for (i, p) in self.route.iter().enumerate() {
            let c = tf.to_screen(*p);
            paint_text(window, cx, &font, &(i + 1).to_string(), 10., t.bg_app.into(), c, true);
        }
    }
}

/// Paints one line of text vertically centred on `at` (and horizontally too if `centered`).
#[allow(clippy::too_many_arguments)]
fn paint_text(
    window: &mut Window,
    cx: &mut App,
    font: &gpui::Font,
    text: &str,
    size_px: f32,
    color: Hsla,
    at: Point<Pixels>,
    centered: bool,
) {
    let run = TextRun {
        len: text.len(),
        font: font.clone(),
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let line_height = px(size_px * 1.3);
    let shaped =
        window.text_system().shape_line(SharedString::from(text.to_string()), px(size_px), &[run], None);
    let x = if centered { at.x - shaped.width / 2. } else { at.x };
    let origin = point(x, at.y - line_height / 2.);
    let _ = shaped.paint(origin, line_height, gpui::TextAlign::Left, None, window, cx);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transform_round_trips_and_zoom_keeps_anchor() {
        let mut s = MapState::default();
        s.bounds.set(Bounds::new(point(px(10.), px(20.)), size(px(800.), px(600.))));
        let tf = s.transform();
        let w = WorldPoint { x: 123., y: -45. };
        let back = tf.to_world(tf.to_screen(w));
        assert!((back.x - w.x).abs() < 0.01 && (back.y - w.y).abs() < 0.01);

        let anchor = point(px(600.), px(100.));
        let world_before = s.transform().to_world(anchor);
        s.zoom_at(anchor, 2.5);
        let world_after = s.transform().to_world(anchor);
        assert!((world_before.x - world_after.x).abs() < 0.01);
        assert!((world_before.y - world_after.y).abs() < 0.01);
        assert_eq!(s.zoom, 2.5);
    }

    #[test]
    fn hit_finds_nearest_node() {
        let mut s = MapState::default();
        s.bounds.set(Bounds::new(point(px(0.), px(0.)), size(px(1060.), px(580.))));
        // scale = 0.5 on both axes, centre = (530, 290)
        s.drawn = vec![(0, WorldPoint { x: 0., y: 0. }), (1, WorldPoint { x: 30., y: 0. })];
        assert_eq!(s.hit(point(px(532.), px(290.))), Some(0));
        assert_eq!(s.hit(point(px(544.), px(290.))), Some(1));
        assert_eq!(s.hit(point(px(700.), px(400.))), None);
    }
}
