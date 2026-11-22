//! The graph canvas: pan/zoom state, painting and hit-testing.

use std::cell::Cell;
use std::collections::HashSet;
use std::rc::Rc;

use gpui::{
    hsla, point, px, quad, size, transparent_black, App, BorderStyle, Bounds, Hsla, PathBuilder,
    Pixels, Point, SharedString, TextRun, Window,
};
use trackmap_core::Camelot;
use trackmap_graph::{Graph, HarmonicLayout, NodeId, NodeKey, Point as WorldPoint};

use crate::theme::Theme;

/// World radius that fits the screen at zoom 1 (outer edge of the major-key ring plus margin).
const WORLD_RADIUS: f32 = 920.0;
const HIT_RADIUS: f32 = 10.0;

pub struct MapState {
    /// Screen-space offset of the world origin from the canvas centre.
    pub pan: Point<Pixels>,
    pub zoom: f32,
    pub expanded: Vec<NodeKey>,
    pub selected: Option<NodeKey>,
    pub hovered: Option<NodeId>,
    pub drag_from: Option<Point<Pixels>>,
    pub dragged: bool,
    pub show_playlists: bool,
    pub show_labels: bool,
    /// Canvas bounds from the last paint, for hit-testing mouse events.
    pub bounds: Rc<Cell<Bounds<Pixels>>>,
    /// Nodes drawn last frame, in paint order.
    pub drawn: Vec<(NodeId, WorldPoint)>,
}

impl Default for MapState {
    fn default() -> Self {
        Self {
            pan: point(px(0.), px(0.)),
            zoom: 1.0,
            expanded: Vec::new(),
            selected: None,
            hovered: None,
            drag_from: None,
            dragged: false,
            show_playlists: false,
            show_labels: false,
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
        Self { center: bounds.center() + pan, scale: w.min(h).max(1.0) / 2.0 / WORLD_RADIUS * zoom }
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

    /// Topmost node under the cursor.
    pub fn hit(&self, pos: Point<Pixels>) -> Option<NodeId> {
        let tf = self.transform();
        self.drawn
            .iter()
            .rev()
            .map(|&(n, p)| {
                let s = tf.to_screen(p);
                let (dx, dy) = (f32::from(s.x - pos.x), f32::from(s.y - pos.y));
                (n, dx * dx + dy * dy)
            })
            .filter(|&(_, d2)| d2 <= HIT_RADIUS * HIT_RADIUS)
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
        if !self.expanded.contains(&key) {
            self.expanded.push(key.clone());
        }
        self.selected = Some(key);
    }
}

pub fn key_color(k: Camelot) -> Hsla {
    hsla((k.number - 1) as f32 / 12.0, 0.65, 0.6, 1.0)
}

struct DrawNode {
    id: NodeId,
    pos: WorldPoint,
    fill: Hsla,
    border: Hsla,
    label: Option<SharedString>,
    label_color: Hsla,
}

/// Everything needed to paint one frame, owned so it can move into the canvas closure.
pub struct Frame {
    nodes: Vec<DrawNode>,
    edges: Vec<(WorldPoint, WorldPoint)>,
    edges_hi: Vec<(WorldPoint, WorldPoint)>,
    pan: Point<Pixels>,
    zoom: f32,
    theme: Theme,
}

impl Frame {
    #[allow(clippy::too_many_arguments)]
    pub fn build(
        graph: &Graph,
        keys: &[Option<Camelot>],
        visible: &[NodeId],
        edges: &[(NodeId, NodeId)],
        selected: Option<NodeId>,
        hovered: Option<NodeId>,
        state: &MapState,
        theme: &Theme,
    ) -> Self {
        let accent: Hsla = theme.accent.into();
        let text: Hsla = theme.text.into();
        let muted: Hsla = theme.text_muted.into();

        let focus: HashSet<NodeId> = match selected {
            Some(s) => std::iter::once(s).chain(graph.neighbours(s)).collect(),
            None => HashSet::new(),
        };
        let dim = |n: NodeId, c: Hsla| {
            if selected.is_some() && !focus.contains(&n) { c.opacity(0.25) } else { c }
        };

        let songs_shown = visible.iter().filter(|n| matches!(n, NodeId::Song(_))).count();
        // Song titles get busy; show them when zoomed in on a modest number of songs.
        let song_labels = state.zoom >= 3.0 && songs_shown <= 600 || songs_shown <= 40;

        // Paint songs first so group nodes sit on top.
        let mut order: Vec<NodeId> = visible.to_vec();
        order.sort_by_key(|n| !matches!(n, NodeId::Song(_)));

        let nodes = order
            .into_iter()
            .map(|n| {
                let is_sel = Some(n) == selected;
                let is_hover = Some(n) == hovered;
                let (fill, border) = match n {
                    NodeId::Song(i) => {
                        let ring = keys[i].map(key_color).unwrap_or(muted);
                        (if is_sel { accent } else { hsla(0., 0., 0.55, 1.) }, ring)
                    }
                    NodeId::Artist(_) => (if is_sel { accent } else { hsla(0., 0., 0.85, 1.) }, text),
                    NodeId::Label(_) => (if is_sel { accent } else { hsla(0., 0., 0.4, 1.) }, text),
                    NodeId::Playlist(_) => {
                        (if is_sel { accent } else { theme.bg_elevated.into() }, accent)
                    }
                };
                let labelled = !matches!(n, NodeId::Song(_))
                    || song_labels
                    || is_sel
                    || is_hover
                    || (selected.is_some() && focus.contains(&n));
                DrawNode {
                    id: n,
                    pos: graph.position(n),
                    fill: dim(n, fill),
                    border: dim(n, border),
                    label: labelled.then(|| SharedString::from(graph.name(n).to_string())),
                    label_color: dim(n, if is_sel || is_hover { text } else { muted }),
                }
            })
            .collect();

        let mut lines = Vec::new();
        let mut lines_hi = Vec::new();
        for &(a, b) in edges {
            let seg = (graph.position(a), graph.position(b));
            if selected.is_some_and(|s| s == a || s == b) {
                lines_hi.push(seg);
            } else {
                lines.push(seg);
            }
        }

        Self {
            nodes,
            edges: lines,
            edges_hi: lines_hi,
            pan: state.pan,
            zoom: state.zoom,
            theme: *theme,
        }
    }

    pub fn paint(self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
        let tf = Transform::new(bounds, self.pan, self.zoom);
        let t = &self.theme;
        let muted: Hsla = t.text_muted.into();
        let font = window.text_style().font();

        // Key wheel guides: ring boundaries and key names.
        let layout = HarmonicLayout::default();
        let minor_out = layout.inner + layout.ring_width;
        let major_in = minor_out + layout.gap;
        let major_out = major_in + layout.ring_width;
        let guide: Hsla = Hsla::from(t.border).opacity(0.8);
        for r in [layout.inner, minor_out, major_in, major_out] {
            let c = tf.to_screen(WorldPoint { x: 0., y: 0. });
            let rs = px(r * tf.scale);
            window.paint_quad(quad(
                Bounds::new(point(c.x - rs, c.y - rs), size(rs * 2., rs * 2.)),
                rs,
                transparent_black(),
                px(1.),
                guide,
                BorderStyle::Solid,
            ));
        }
        for n in 1..=12u8 {
            for (minor, r) in [(true, layout.inner - 28.), (false, major_out + 28.)] {
                let k = Camelot::new(n, minor).unwrap();
                let a = k.angle();
                let s = tf.to_screen(WorldPoint { x: r * a.cos(), y: r * a.sin() });
                paint_text(window, cx, &font, &k.to_string(), 11., key_color(k).opacity(0.7), s, true);
            }
        }

        for (segs, color) in [(&self.edges, muted.opacity(0.35)), (&self.edges_hi, t.accent.into())] {
            if segs.is_empty() {
                continue;
            }
            let mut path = PathBuilder::stroke(px(1.));
            for (a, b) in segs.iter() {
                path.move_to(tf.to_screen(*a));
                path.line_to(tf.to_screen(*b));
            }
            if let Ok(p) = path.build() {
                window.paint_path(p, color);
            }
        }

        for n in &self.nodes {
            let c = tf.to_screen(n.pos);
            let (w, h, radius, border) = match n.id {
                NodeId::Song(_) => (9., 9., 4.5, 1.5),
                NodeId::Artist(_) => (14., 14., 7., 1.),
                NodeId::Label(_) => (14., 14., 3., 1.),
                NodeId::Playlist(_) => (20., 13., 4., 1.5),
            };
            let (w, h) = (px(w), px(h));
            window.paint_quad(quad(
                Bounds::new(point(c.x - w / 2., c.y - h / 2.), size(w, h)),
                px(radius),
                n.fill,
                px(border),
                n.border,
                BorderStyle::Solid,
            ));
            if let Some(label) = &n.label {
                let at = point(c.x + w / 2. + px(5.), c.y);
                paint_text(window, cx, &font, label, 12., n.label_color, at, false);
            }
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
    let _ = shaped.paint(point(x, at.y - line_height / 2.), line_height, window, cx);
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
        s.bounds.set(Bounds::new(point(px(0.), px(0.)), size(px(920.), px(920.))));
        // scale = 460 / 920 = 0.5, centre = (460, 460)
        s.drawn = vec![
            (NodeId::Song(0), WorldPoint { x: 0., y: 0. }),
            (NodeId::Song(1), WorldPoint { x: 30., y: 0. }),
        ];
        assert_eq!(s.hit(point(px(462.), px(460.))), Some(NodeId::Song(0)));
        assert_eq!(s.hit(point(px(474.), px(460.))), Some(NodeId::Song(1)));
        assert_eq!(s.hit(point(px(600.), px(600.))), None);
    }
}
