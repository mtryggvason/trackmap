//! The song map: pan/zoom state, painting and hit-testing. Only songs are drawn. The camera
//! fits whatever is shown, and covers grow to fill the space they have.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    hsla, point, px, quad, size, transparent_black, App, BorderStyle, Bounds, Hsla, PathBuilder,
    Pixels, Point, RenderImage, SharedString, TextRun, TouchPhase, Window,
};
use trackmap_core::Camelot;
use trackmap_graph::{Axis, Graph, MapLayout, NodeId, NodeKey, Point as WorldPoint, KEY_ROWS};

/// How long transitions take.
const TRANSITION: Duration = Duration::from_millis(320);

/// Ease-out cubic: fast start, gentle landing.
fn ease(t: f32) -> f32 {
    1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3)
}

fn lerp(a: WorldPoint, b: WorldPoint, t: f32) -> WorldPoint {
    WorldPoint { x: a.x + (b.x - a.x) * t, y: a.y + (b.y - a.y) * t }
}

/// Songs moving between layouts: where each started, and songs fading out where they were.
struct SongMotion {
    start: Instant,
    from: HashMap<usize, WorldPoint>,
    leaving: Vec<(usize, WorldPoint)>,
}

/// The camera easing from one pan/zoom to another.
struct ViewMotion {
    start: Instant,
    from: (Point<Pixels>, f32),
    to: (Point<Pixels>, f32),
}

/// Zoom limits. Pinching past them meets resistance and springs back on release.
pub const MIN_ZOOM: f32 = 0.3;
pub const MAX_ZOOM: f32 = 40.0;
/// How far past a limit you can stretch (as a factor of the limit).
const OVERSTRETCH: f32 = 1.6;
/// How quickly released momentum dies away (seconds); larger glides further.
const MOMENTUM_DECAY: f32 = 0.22;
/// How quickly an overstretched zoom springs back (seconds).
const SPRING: f32 = 0.10;

/// A pinch that's in progress: when the last event came and how fast zoom was changing
/// (natural log of zoom per second, smoothed).
struct Pinch {
    last: Instant,
    velocity: f32,
    anchor: Point<Pixels>,
}

/// Zoom still moving after the fingers lifted (momentum, then a spring back into the limits).
struct ZoomGlide {
    last: Instant,
    velocity: f32,
    anchor: Point<Pixels>,
}

/// What to draw this frame: every song's current position, how visible each shown song is
/// (fading in), and songs that are fading out.
pub struct Animated {
    pub positions: Vec<WorldPoint>,
    pub alpha: HashMap<usize, f32>,
    pub leaving: Vec<(usize, WorldPoint, f32)>,
    pub running: bool,
}

/// Which list the Artists / Labels / Playlists chip has open.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Picker {
    Artists,
    Labels,
    Playlists,
}

use crate::theme::Theme;

const HIT_RADIUS: f32 = 10.0;
/// Screen space kept free for the key labels (left), the BPM axis (bottom) and the status line.
const GUTTER_LEFT: f32 = 48.0;
const GUTTER_BOTTOM: f32 = 44.0;
const GUTTER_TOP: f32 = 30.0;
/// Covers never get bigger than this, however few songs are shown.
const MAX_COVER: f32 = 160.0;
const MIN_COVER: f32 = 10.0;

/// Cover size for the on-screen space a song has: a bit less than its slot width and row
/// height (leaving room for its title), within sensible limits.
pub fn cover_px(slot_px: f32, row_px: f32) -> f32 {
    (slot_px * 0.82).min(row_px * 0.62).clamp(MIN_COVER, MAX_COVER)
}

/// The world-space box the camera fits to the screen at zoom 1.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Fit {
    pub cx: f32,
    pub cy: f32,
    pub hw: f32,
    pub hh: f32,
}

impl Default for Fit {
    fn default() -> Self {
        Self { cx: 0.0, cy: 0.0, hw: 1000.0, hh: 560.0 }
    }
}

impl Fit {
    /// Box around these positions, with room for a cover of `pad_x`/`pad_y` world units on
    /// each side (plus its title below), never smaller than a minimum so a single song
    /// doesn't fill the whole window.
    pub fn around(points: impl IntoIterator<Item = WorldPoint>, pad_x: f32, pad_y: f32) -> Option<Self> {
        let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for p in points {
            (x0, y0, x1, y1) = (x0.min(p.x), y0.min(p.y), x1.max(p.x), y1.max(p.y));
        }
        if x0 > x1 {
            return None;
        }
        Some(Self {
            cx: (x0 + x1) / 2.0,
            cy: (y0 + y1) / 2.0 + pad_y * 0.25,
            hw: ((x1 - x0) / 2.0 + pad_x).max(pad_x * 2.0),
            hh: ((y1 - y0) / 2.0 + pad_y * 1.5).max(pad_y * 2.0),
        })
    }
}
pub struct MapState {
    /// Screen-space offset of the world origin from the canvas centre.
    pub pan: Point<Pixels>,
    pub zoom: f32,
    /// What the inspector shows: a song, or the artist/label/playlist being viewed.
    pub selected: Option<NodeKey>,
    /// An artist/label/playlist the map is narrowed to: only its songs are shown, and the
    /// layout is fitted to them.
    pub scope: Option<NodeKey>,
    /// Pan and zoom from before entering a scope, restored when leaving it.
    pub saved_view: Option<(Point<Pixels>, f32)>,
    /// Song index under the cursor.
    pub hovered: Option<usize>,
    pub drag_from: Option<Point<Pixels>>,
    pub dragged: bool,
    pub picker: Option<Picker>,
    /// Canvas bounds from the last paint, for hit-testing mouse events.
    pub bounds: Rc<Cell<Bounds<Pixels>>>,
    /// Songs drawn last frame, in paint order.
    pub drawn: Vec<(usize, WorldPoint)>,
    /// Song positions for a (bin width, graph, shown songs) combination, reused until it changes.
    pub positions: Option<(f32, Rc<Graph>, u64, Rc<Vec<WorldPoint>>)>,
    /// The same at normal zoom, which the camera frames.
    pub base_positions: Option<(f32, Rc<Graph>, u64, Rc<Vec<WorldPoint>>)>,
    /// The layout and shown songs last drawn, and where each shown song actually was.
    last_target: Option<Rc<Vec<WorldPoint>>>,
    last_shown: Vec<usize>,
    current: HashMap<usize, WorldPoint>,
    songs_motion: Option<SongMotion>,
    view_motion: Option<ViewMotion>,
    /// The box the camera currently fits (eased towards the shown songs' box).
    pub fit: Fit,
    fit_step: Option<Instant>,
    /// Cover size in screen pixels this frame (also the click target size).
    pub cover: f32,
    pinch: Option<Pinch>,
    glide: Option<ZoomGlide>,
}

impl Default for MapState {
    fn default() -> Self {
        Self {
            pan: point(px(0.), px(0.)),
            zoom: 1.0,
            selected: None,
            scope: None,
            saved_view: None,
            hovered: None,
            drag_from: None,
            dragged: false,
            picker: None,
            bounds: Rc::new(Cell::new(Bounds::default())),
            drawn: Vec::new(),
            positions: None,
            base_positions: None,
            last_target: None,
            last_shown: Vec::new(),
            current: HashMap::new(),
            songs_motion: None,
            view_motion: None,
            fit: Fit::default(),
            fit_step: None,
            cover: 16.0,
            pinch: None,
            glide: None,
        }
    }
}

#[derive(Clone, Copy)]
pub struct Transform {
    center: Point<Pixels>,
    scale: f32,
}

impl Transform {
    /// Fits `fit` into the canvas (minus the axis gutters), then applies pan and zoom.
    pub fn new(bounds: Bounds<Pixels>, pan: Point<Pixels>, zoom: f32, fit: Fit) -> Self {
        let w = (f32::from(bounds.size.width) - GUTTER_LEFT).max(1.0);
        let h = (f32::from(bounds.size.height) - GUTTER_BOTTOM - GUTTER_TOP).max(1.0);
        let scale = (w / (2.0 * fit.hw)).min(h / (2.0 * fit.hh)).max(0.0001) * zoom;
        let inner = point(
            bounds.origin.x + px(GUTTER_LEFT + w / 2.0),
            bounds.origin.y + px(GUTTER_TOP + h / 2.0),
        );
        let center = inner + pan - point(px(fit.cx * scale), px(fit.cy * scale));
        Self { center, scale }
    }

    /// Screen pixels per world unit.
    pub fn scale(&self) -> f32 {
        self.scale
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
        Transform::new(self.bounds.get(), self.pan, self.zoom, self.fit)
    }

    /// Eases the camera's fit box towards `target`. Returns whether it's still moving.
    pub fn step_fit(&mut self, target: Fit, now: Instant) -> bool {
        let dt = self.fit_step.map_or(1.0, |t| (now - t).as_secs_f32()).min(0.1);
        self.fit_step = Some(now);
        // First frame: jump straight there.
        if self.fit == Fit::default() {
            self.fit = target;
            return false;
        }
        let k = 1.0 - (-dt / 0.08).exp();
        let f = &mut self.fit;
        let mix = |a: &mut f32, b: f32| *a += (b - *a) * k;
        mix(&mut f.cx, target.cx);
        mix(&mut f.cy, target.cy);
        mix(&mut f.hw, target.hw);
        mix(&mut f.hh, target.hh);
        let close = |a: f32, b: f32| (a - b).abs() <= 0.002 * target.hw.max(target.hh);
        if close(f.cx, target.cx) && close(f.cy, target.cy) && close(f.hw, target.hw) && close(f.hh, target.hh) {
            self.fit = target;
            false
        } else {
            true
        }
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
                let r = HIT_RADIUS.max(self.cover / 2.0);
                d2 <= r * r
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(n, _)| n)
    }

    /// Eases the camera to a new pan/zoom instead of jumping there.
    fn move_view_to(&mut self, pan: Point<Pixels>, zoom: f32) {
        self.view_motion = Some(ViewMotion { start: Instant::now(), from: (self.pan, self.zoom), to: (pan, zoom) });
    }

    /// Stops any camera transition (the user is steering).
    pub fn stop_view_motion(&mut self) {
        self.view_motion = None;
    }

    /// Advances the camera transition. Returns whether it's still running.
    pub fn step_view(&mut self, now: Instant) -> bool {
        let Some(m) = &self.view_motion else { return false };
        let t = (now - m.start).as_secs_f32() / TRANSITION.as_secs_f32();
        let e = ease(t);
        let (fp, fz) = m.from;
        let (tp, tz) = m.to;
        self.pan = point(fp.x + (tp.x - fp.x) * e, fp.y + (tp.y - fp.y) * e);
        // Interpolate zoom geometrically so it feels even in and out.
        self.zoom = fz * (tz / fz).powf(e);
        if t >= 1.0 {
            self.view_motion = None;
            return false;
        }
        true
    }

    /// Positions to draw for `target` (the layout's positions) and `shown` songs, starting a
    /// transition whenever either changes. Songs glide from where they were; new ones fade in
    /// and songs no longer shown fade out in place.
    pub fn animate(&mut self, target: &Rc<Vec<WorldPoint>>, shown: &[usize], now: Instant) -> Animated {
        let changed = !self.last_target.as_ref().is_some_and(|t| Rc::ptr_eq(t, target))
            || self.last_shown != shown;
        if changed {
            if !self.current.is_empty() {
                let keep: HashSet<usize> = shown.iter().copied().collect();
                let leaving = self.current.iter().filter(|(s, _)| !keep.contains(s)).map(|(s, p)| (*s, *p)).collect();
                self.songs_motion = Some(SongMotion { start: now, from: self.current.clone(), leaving });
            }
            self.last_target = Some(target.clone());
            self.last_shown = shown.to_vec();
        }

        let mut positions = target.to_vec();
        let mut alpha = HashMap::new();
        let mut leaving = Vec::new();
        let mut running = false;
        if let Some(m) = &self.songs_motion {
            let t = (now - m.start).as_secs_f32() / TRANSITION.as_secs_f32();
            let e = ease(t);
            if t < 1.0 {
                running = true;
                for &s in shown {
                    match m.from.get(&s) {
                        Some(&from) => positions[s] = lerp(from, target[s], e),
                        None => {
                            alpha.insert(s, e);
                        }
                    }
                }
                leaving = m.leaving.iter().map(|&(s, p)| (s, p, 1.0 - e)).collect();
            } else {
                self.songs_motion = None;
            }
        }
        self.current = shown.iter().map(|&s| (s, positions[s])).collect();
        Animated { positions, alpha, leaving, running }
    }

    /// Zooms by `ln_factor` (natural log of the factor) around `anchor`, with resistance
    /// beyond the limits like a rubber band.
    fn stretch_zoom(&mut self, anchor: Point<Pixels>, ln_factor: f32) {
        let outward = (self.zoom <= MIN_ZOOM && ln_factor < 0.0) || (self.zoom >= MAX_ZOOM && ln_factor > 0.0);
        let l = if outward {
            // Resistance grows the further past the limit you are.
            let past = if self.zoom < MIN_ZOOM { MIN_ZOOM / self.zoom } else { self.zoom / MAX_ZOOM };
            ln_factor * (1.0 - past.ln() / OVERSTRETCH.ln()).clamp(0.0, 1.0) * 0.5
        } else {
            ln_factor
        };
        self.zoom_to(anchor, self.zoom * l.exp(), MIN_ZOOM / OVERSTRETCH, MAX_ZOOM * OVERSTRETCH);
    }

    /// Sets zoom (clamped to `lo..=hi`) keeping the world point under `anchor` fixed.
    fn zoom_to(&mut self, anchor: Point<Pixels>, zoom: f32, lo: f32, hi: f32) {
        self.view_motion = None;
        let world = self.transform().to_world(anchor);
        self.zoom = zoom.clamp(lo, hi);
        let moved = self.transform().to_screen(world);
        self.pan = self.pan + (anchor - moved);
    }

    /// Handles a trackpad pinch event. Speed is smoothed while pinching; on release the zoom
    /// keeps gliding and slows down, and springs back if it was stretched past a limit.
    pub fn pinch(&mut self, anchor: Point<Pixels>, delta: f32, phase: TouchPhase, now: Instant) {
        match phase {
            TouchPhase::Started => {
                self.glide = None;
                self.pinch = Some(Pinch { last: now, velocity: 0.0, anchor });
            }
            TouchPhase::Moved => {
                let l = (1.0 + delta).max(0.05).ln();
                self.stretch_zoom(anchor, l);
                let p = self.pinch.get_or_insert(Pinch { last: now, velocity: 0.0, anchor });
                let dt = (now - p.last).as_secs_f32().max(0.004);
                p.velocity = 0.6 * (l / dt) + 0.4 * p.velocity;
                p.last = now;
                p.anchor = anchor;
            }
            TouchPhase::Ended | TouchPhase::Cancelled => {
                if delta != 0.0 {
                    self.stretch_zoom(anchor, (1.0 + delta).max(0.05).ln());
                }
                let p = self.pinch.take();
                // Fingers that stopped before lifting don't fling.
                let velocity = p.as_ref().filter(|p| (now - p.last).as_secs_f32() < 0.08).map_or(0.0, |p| p.velocity);
                let anchor = p.map_or(anchor, |p| p.anchor);
                self.glide = Some(ZoomGlide { last: now, velocity: velocity.clamp(-12.0, 12.0), anchor });
            }
        }
    }

    /// Advances momentum and the spring back into the zoom limits. Returns whether zoom is
    /// still moving.
    pub fn step_glide(&mut self, now: Instant) -> bool {
        let Some(g) = &mut self.glide else { return false };
        let dt = (now - g.last).as_secs_f32().min(0.05);
        g.last = now;
        let (anchor, mut velocity) = (g.anchor, g.velocity);
        velocity *= (-dt / MOMENTUM_DECAY).exp();
        if velocity.abs() > 0.05 {
            self.stretch_zoom(anchor, velocity * dt);
        } else {
            velocity = 0.0;
        }
        // Spring back if past a limit (works together with any remaining momentum).
        let limit = self.zoom.clamp(MIN_ZOOM, MAX_ZOOM);
        let past = (self.zoom / limit).ln();
        if past.abs() > 1e-4 && velocity * past >= 0.0 {
            velocity = 0.0;
        }
        if past.abs() > 1e-4 {
            let back = past * (1.0 - (-dt / SPRING).exp());
            self.zoom_to(anchor, self.zoom * (-back).exp(), MIN_ZOOM / OVERSTRETCH, MAX_ZOOM * OVERSTRETCH);
        }
        let settled = velocity == 0.0 && (self.zoom / limit).ln().abs() <= 1e-3;
        if settled {
            if self.zoom != limit {
                self.zoom_to(anchor, limit, MIN_ZOOM, MAX_ZOOM);
            }
            self.glide = None;
            return false;
        }
        if let Some(g) = &mut self.glide {
            g.velocity = velocity;
        }
        true
    }

    /// Zooms by `factor`, keeping the world point under `anchor` fixed on screen.
    pub fn zoom_at(&mut self, anchor: Point<Pixels>, factor: f32) {
        self.view_motion = None;
        self.glide = None;
        let before = self.transform();
        let world = before.to_world(anchor);
        self.zoom = (self.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        let after = self.transform();
        let moved = after.to_screen(world);
        self.pan = self.pan + (anchor - moved);
    }

    /// Selects a song (highlighted in place), or narrows the map to an artist, label or
    /// playlist: other songs disappear and the view resets to fit the group.
    pub fn select(&mut self, key: NodeKey) {
        self.picker = None;
        if !matches!(key, NodeKey::Song(_)) {
            if self.saved_view.is_none() {
                self.saved_view = Some((self.pan, self.zoom));
            }
            self.move_view_to(point(px(0.), px(0.)), 1.0);
            self.scope = Some(key.clone());
        }
        self.selected = Some(key);
    }

    /// One step back: deselect a song inside a group, otherwise leave the group.
    pub fn back(&mut self) {
        if self.selected.is_some() && self.selected != self.scope {
            self.selected = self.scope.clone();
        } else {
            self.clear();
        }
    }

    /// Leaves any group and selection, restoring the view from before.
    pub fn clear(&mut self) {
        self.selected = None;
        self.picker = None;
        if self.scope.take().is_some() {
            if let Some((pan, zoom)) = self.saved_view.take() {
                self.move_view_to(pan, zoom);
            }
        }
    }
}

pub fn key_color(k: Camelot) -> Hsla {
    hsla((k.number - 1) as f32 / 12.0, 0.65, 0.6, 1.0)
}

struct DrawSong {
    pos: WorldPoint,
    /// 1 when fully shown; less while fading in or out.
    alpha: f32,
    /// Lower is more important when titles compete for space.
    label_priority: u8,
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
    /// The drawn BPM axis (compressed gaps), if any song has a tempo.
    axis: Option<Axis>,
    has_no_bpm: bool,
    /// Cover size in screen pixels.
    cover: f32,
    fit: Fit,
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
        layout: MapLayout,
        axis: Option<Axis>,
        has_no_bpm: bool,
        anim: &Animated,
        keys: &[Option<Camelot>],
        arts: &[Option<Arc<RenderImage>>],
        shown: &[usize],
        selected: Option<NodeId>,
        route_of: Option<NodeId>,
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

        // Titles are only drawn where they fit (see paint), so every song can ask for one;
        // very large views skip them to keep painting fast.
        let label_all = order.len() <= 1500;

        let positions = &anim.positions;
        let mut drawn = Vec::with_capacity(order.len());
        // Songs fading out are drawn first, underneath, and can't be clicked.
        let fading = anim.leaving.iter().map(|&(s, pos, a)| DrawSong {
            pos,
            alpha: a,
            label_priority: 4,
            art: arts.get(s).cloned().flatten(),
            dimmed: false,
            fill: hsla(0., 0., 0.55, 1.),
            ring: keys[s].map(key_color).unwrap_or(muted),
            big: false,
            label: None,
            label_color: muted,
        });
        let songs = fading.collect::<Vec<_>>().into_iter().chain(order
            .iter()
            .map(|&s| {
                let pos = positions[s];
                drawn.push((s, pos));
                let in_focus = focus.contains(&s);
                let is_hover = hovered == Some(s);
                let ring = keys[s].map(key_color).unwrap_or(muted);
                let (fill, ring) = match (has_focus, in_focus) {
                    (true, true) => (accent, ring),
                    (true, false) => (hsla(0., 0., 0.55, 0.2), ring.opacity(0.2)),
                    _ => (hsla(0., 0., 0.55, 1.), ring),
                };
                let labelled = is_hover || in_focus || label_all;
                DrawSong {
                    pos,
                    alpha: anim.alpha.get(&s).copied().unwrap_or(1.0),
                    label_priority: if is_hover {
                        0
                    } else if in_focus {
                        1
                    } else if has_focus {
                        3
                    } else {
                        2
                    },
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
            }))
            .collect();

        let route = match route_of {
            Some(p @ NodeId::Playlist(_)) => graph.songs_of(p).iter().map(|&s| anim.positions[s]).collect(),
            _ => Vec::new(),
        };

        let frame = Self {
            songs,
            route,
            layout,
            axis,
            has_no_bpm,
            cover: state.cover,
            fit: state.fit,
            pan: state.pan,
            zoom: state.zoom,
            theme: *theme,
        };
        (frame, drawn)
    }

    pub fn paint(self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
        let tf = Transform::new(bounds, self.pan, self.zoom, self.fit);
        let t = &self.theme;
        let font = window.text_style().font();

        let l = &self.layout;
        let muted: Hsla = t.text_muted.into();

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

        let cover = self.cover;
        let bg: Hsla = t.bg_app.into();
        let mut node_boxes = Vec::with_capacity(self.songs.len());
        for s in &self.songs {
            let c = tf.to_screen(s.pos);
            let d = if let Some(img) = &s.art {
                // Cover: image, a dimming veil when out of focus, then the key-coloured outline.
                let d = px(if s.big { (cover * 1.3).min(MAX_COVER * 1.15) } else { cover });
                let b = Bounds::new(point(c.x - d / 2., c.y - d / 2.), size(d, d));
                let radius = px(3.);
                let _ = window.paint_image(b, b, radius.into(), img.clone(), 0, s.dimmed);
                // Dimming and fading are a veil of background colour over the cover.
                let veil = 1.0 - (1.0 - if s.dimmed { 0.7 } else { 0.0 }) * s.alpha;
                if veil > 0.0 {
                    window.paint_quad(quad(b, radius, bg.opacity(veil), px(0.), bg, BorderStyle::Solid));
                }
                window.paint_quad(quad(b, radius, transparent_black(), px(1.5), s.ring.opacity(s.alpha), BorderStyle::Solid));
                d
            } else {
                let d = px(if s.big { 13. } else { 9. });
                window.paint_quad(quad(
                    Bounds::new(point(c.x - d / 2., c.y - d / 2.), size(d, d)),
                    d / 2.,
                    s.fill.opacity(s.alpha),
                    px(1.5),
                    s.ring.opacity(s.alpha),
                    BorderStyle::Solid,
                ));
                d
            };
            let (cx_, cy_, half) = (f32::from(c.x), f32::from(c.y), f32::from(d) / 2.0);
            node_boxes.push(Rect { x0: cx_ - half, y0: cy_ - half, x1: cx_ + half, y1: cy_ + half });
        }

        // Titles centred under their covers; where two would collide (or a title would cover
        // another song), only the more important one is drawn. Zooming in spreads songs out,
        // so more titles fit.
        const LABEL_SIZE: f32 = 11.0;
        const LABEL_MAX_W: f32 = 140.0;
        let view = Rect {
            x0: f32::from(bounds.origin.x),
            y0: f32::from(bounds.origin.y),
            x1: f32::from(bounds.origin.x + bounds.size.width),
            y1: f32::from(bounds.origin.y + bounds.size.height),
        };
        let mut candidates = Vec::new();
        for (i, s) in self.songs.iter().enumerate() {
            let Some(label) = &s.label else { continue };
            if s.alpha < 0.6 {
                continue; // no titles on songs still fading in
            }
            let nb = node_boxes[i];
            if !nb.overlaps(&view) {
                continue;
            }
            let line = shape_fitted(window, &font, label, LABEL_SIZE, s.label_color, LABEL_MAX_W);
            let w = f32::from(line.width);
            let cx_ = (nb.x0 + nb.x1) / 2.0;
            let rect = Rect { x0: cx_ - w / 2.0, y0: nb.y1 + 2.0, x1: cx_ + w / 2.0, y1: nb.y1 + 2.0 + LABEL_SIZE * 1.3 };
            candidates.push(LabelCandidate { owner: i, rect, priority: s.label_priority, line });
        }
        let keep = place_labels(
            &candidates.iter().map(|c| (c.owner, c.rect, c.priority)).collect::<Vec<_>>(),
            &node_boxes,
        );
        for (c, keep) in candidates.into_iter().zip(keep) {
            if keep {
                let origin = point(px(c.rect.x0), px(c.rect.y0));
                let _ = c.line.paint(origin, px(LABEL_SIZE * 1.3), gpui::TextAlign::Left, None, window, cx);
            }
        }

        // Axes, pinned to the canvas edges so they stay visible while zooming and panning:
        // BPM along the bottom (finer steps as you zoom in), keys down the left.
        let axis_y = bounds.origin.y + bounds.size.height - px(28.);
        let strip = Bounds::new(point(bounds.origin.x, axis_y - px(10.)), size(bounds.size.width, px(20.)));
        window.paint_quad(quad(strip, px(0.), bg.opacity(0.85), px(0.), bg, BorderStyle::Solid));
        let fmt = |bpm: f32, fine: bool| if fine || bpm.fract() != 0.0 { format!("{bpm:.1}") } else { format!("{bpm:.0}") };
        let no_bpm_world = match &self.axis {
            Some(axis) => {
                // Label the tempos that have songs, skipping any that would collide; compressed
                // empty stretches get a break mark.
                let fine = axis.bin < 1.0;
                let mut last = f32::MIN;
                for (bpm, x) in axis.labels() {
                    let sx = tf.to_screen(WorldPoint { x, y: 0. }).x;
                    if f32::from(sx) - last >= 34.0 {
                        paint_text(window, cx, &font, &fmt(bpm, fine), 11., muted, point(sx, axis_y), true);
                        last = f32::from(sx);
                    }
                }
                for g in &axis.gaps {
                    let sx = tf.to_screen(WorldPoint { x: *g, y: 0. }).x;
                    paint_text(window, cx, &font, "⋯", 11., muted.opacity(0.5), point(sx, axis_y), true);
                }
                axis.no_bpm_x()
            }
            None => l.no_bpm_x(),
        };
        let no_bpm = tf.to_screen(WorldPoint { x: no_bpm_world, y: 0. }).x;
        if self.has_no_bpm && no_bpm > bounds.origin.x {
            paint_text(window, cx, &font, "no BPM", 11., muted.opacity(0.6), point(no_bpm, axis_y), true);
        }
        let key_x = bounds.origin.x + px(22.);
        let gutter = Bounds::new(bounds.origin, size(px(44.), bounds.size.height));
        window.paint_quad(quad(gutter, px(0.), bg.opacity(0.85), px(0.), bg, BorderStyle::Solid));
        let row_px = l.row_height() * tf.scale();
        for row in 0..KEY_ROWS {
            // When rows are tight, label only the minor keys (1A, 2A, …) and the "?" row.
            if row_px < 14.0 && row % 2 == 1 && row != KEY_ROWS - 1 {
                continue;
            }
            let y = tf.to_screen(WorldPoint { x: 0., y: l.row_y(row) }).y;
            if y < bounds.origin.y || y > axis_y - px(12.) {
                continue;
            }
            let (text, color) = if row == KEY_ROWS - 1 {
                ("?".to_string(), muted.opacity(0.6))
            } else {
                let k = Camelot::new((row / 2) as u8 + 1, row % 2 == 0).unwrap();
                (k.to_string(), key_color(k).opacity(0.8))
            };
            paint_text(window, cx, &font, &text, 10., color, point(key_x, y), true);
        }

        // Play-order numbers on top of the route.
        for (i, p) in self.route.iter().enumerate() {
            let c = tf.to_screen(*p);
            paint_text(window, cx, &font, &(i + 1).to_string(), 10., t.bg_app.into(), c, true);
        }
    }
}

/// Screen-space rectangle for label placement.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Rect {
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
}

impl Rect {
    fn overlaps(&self, o: &Rect) -> bool {
        self.x0 < o.x1 && o.x0 < self.x1 && self.y0 < o.y1 && o.y0 < self.y1
    }
}

struct LabelCandidate {
    owner: usize,
    rect: Rect,
    priority: u8,
    line: gpui::ShapedLine,
}

/// Greedy label placement: in priority order (then paint order), keep a label if it overlaps
/// neither an already kept label nor any song other than its own.
fn place_labels(labels: &[(usize, Rect, u8)], nodes: &[Rect]) -> Vec<bool> {
    let mut order: Vec<usize> = (0..labels.len()).collect();
    order.sort_by_key(|&i| labels[i].2);
    let mut kept: Vec<Rect> = Vec::new();
    let mut keep = vec![false; labels.len()];
    for i in order {
        let (owner, rect, _) = labels[i];
        let hits_node = nodes.iter().enumerate().any(|(n, b)| n != owner && rect.overlaps(b));
        if !hits_node && !kept.iter().any(|k| rect.overlaps(k)) {
            kept.push(rect);
            keep[i] = true;
        }
    }
    keep
}

/// Shapes `text`, shortening it with "…" to fit `max_w` pixels.
fn shape_fitted(
    window: &mut Window,
    font: &gpui::Font,
    text: &str,
    size_px: f32,
    color: Hsla,
    max_w: f32,
) -> gpui::ShapedLine {
    let shape = |window: &mut Window, s: String| {
        let run = TextRun {
            len: s.len(),
            font: font.clone(),
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        window.text_system().shape_line(SharedString::from(s), px(size_px), &[run], None)
    };
    let line = shape(window, text.to_string());
    let w = f32::from(line.width);
    if w <= max_w {
        return line;
    }
    let chars: Vec<char> = text.chars().collect();
    let keep = ((chars.len() as f32 * max_w / w) as usize).saturating_sub(1).max(1);
    let short: String = chars[..keep.min(chars.len())].iter().collect::<String>().trim_end().to_string() + "…";
    shape(window, short)
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
    fn labels_never_overlap_and_priority_wins() {
        let r = |x0: f32, y0: f32, x1: f32, y1: f32| Rect { x0, y0, x1, y1 };
        let nodes = [r(0., 0., 10., 10.), r(30., 0., 40., 10.), r(0., 30., 10., 40.)];
        let labels = [
            (0, r(-20., 12., 30., 26.), 2), // under song 0
            (1, r(10., 12., 60., 26.), 0),  // under song 1, overlaps song 0's title, hovered
            (2, r(-20., 42., 30., 56.), 2), // under song 2, free
            (0, r(-5., 25., 15., 35.), 1),  // would cover song 2
        ];
        assert_eq!(place_labels(&labels, &nodes), [false, true, true, false]);
    }

    #[test]
    fn groups_narrow_the_view_and_back_restores_it() {
        let mut s = MapState::default();
        s.pan = point(px(40.), px(-10.));
        s.zoom = 3.0;
        let artist = NodeKey::Artist("Ann".into());
        let song = NodeKey::Song("/a".into());

        let settle = |s: &mut MapState| {
            s.step_view(Instant::now() + TRANSITION * 2);
        };
        s.select(artist.clone());
        settle(&mut s);
        assert_eq!((s.scope.as_ref(), s.zoom), (Some(&artist), 1.0), "fits the group");
        s.select(song.clone());
        assert_eq!(s.scope.as_ref(), Some(&artist), "picking a song stays in the group");
        assert_eq!(s.selected.as_ref(), Some(&song));

        s.back();
        assert_eq!(s.selected.as_ref(), Some(&artist), "first Esc deselects the song");
        s.back();
        settle(&mut s);
        assert_eq!((s.scope.as_ref(), s.selected.as_ref()), (None, None));
        assert_eq!((s.pan, s.zoom), (point(px(40.), px(-10.)), 3.0), "view restored");

        // Switching groups keeps the original view to return to.
        s.select(artist);
        s.zoom = 2.0;
        s.select(NodeKey::Label("Lab".into()));
        s.clear();
        settle(&mut s);
        assert_eq!(s.zoom, 3.0);
    }

    #[test]
    fn songs_glide_and_fade_between_layouts() {
        let mut s = MapState::default();
        let t0 = Instant::now();
        let a = Rc::new(vec![WorldPoint { x: 0., y: 0. }, WorldPoint { x: 100., y: 0. }, WorldPoint { x: 0., y: 50. }]);
        // First frame: nothing to animate from.
        let f = s.animate(&a, &[0, 1], t0);
        assert!(!f.running && f.positions[1].x == 100.);

        // New layout: song 1 moves, song 0 disappears, song 2 appears.
        let b = Rc::new(vec![WorldPoint { x: 0., y: 0. }, WorldPoint { x: 200., y: 0. }, WorldPoint { x: 0., y: 50. }]);
        let t1 = t0 + Duration::from_millis(500);
        let first = s.animate(&b, &[1, 2], t1);
        assert_eq!(first.positions[1].x, 100., "starts where it was");
        let mid = s.animate(&b, &[1, 2], t1 + TRANSITION / 2);
        assert!(mid.running);
        assert!(mid.positions[1].x > 100. && mid.positions[1].x < 200., "song 1 is on its way");
        assert!(mid.alpha[&2] > 0. && mid.alpha[&2] < 1., "song 2 fades in");
        assert_eq!(mid.leaving.len(), 1);
        assert_eq!(mid.leaving[0].0, 0, "song 0 fades out");

        let end = s.animate(&b, &[1, 2], t1 + TRANSITION * 2);
        assert!(!end.running && end.positions[1].x == 200. && end.leaving.is_empty() && end.alpha.is_empty());
    }

    #[test]
    fn camera_eases_into_a_group_and_back() {
        let mut s = MapState::default();
        s.zoom = 4.0;
        s.select(NodeKey::Artist("Ann".into()));
        assert_eq!(s.zoom, 4.0, "doesn't jump");
        let start = s.view_motion.as_ref().unwrap().start;
        assert!(s.step_view(start + TRANSITION / 2));
        assert!(s.zoom < 4.0 && s.zoom > 1.0);
        assert!(!s.step_view(start + TRANSITION * 2));
        assert_eq!(s.zoom, 1.0);
    }

    #[test]
    fn fit_frames_a_few_songs_big() {
        let pts = [WorldPoint { x: -50., y: 0. }, WorldPoint { x: 0., y: 0. }, WorldPoint { x: 50., y: 20. }];
        let f = Fit::around(pts, 40., 30.).unwrap();
        assert_eq!((f.cx, f.hw), (0., 90.));
        let mut s = MapState::default();
        s.bounds.set(Bounds::new(point(px(0.), px(0.)), size(px(1248.), px(874.))));
        s.fit = f;
        // Three songs 100 world units apart fill most of a 1200 px wide canvas.
        let tf = s.transform();
        let span = f32::from(tf.to_screen(pts[2]).x - tf.to_screen(pts[0]).x);
        assert!(span > 500.0, "span {span}");
        assert!(Fit::around([], 1., 1.).is_none());
    }

    #[test]
    fn covers_grow_with_space_up_to_a_limit() {
        assert_eq!(cover_px(20., 20.), 12.4);
        assert_eq!(cover_px(100., 400.), 82.);
        assert_eq!(cover_px(1000., 1000.), MAX_COVER);
        assert_eq!(cover_px(2., 2.), MIN_COVER);
    }

    #[test]
    fn fit_eases_towards_its_target() {
        let mut s = MapState::default();
        let t0 = Instant::now();
        let a = Fit { cx: 0., cy: 0., hw: 500., hh: 300. };
        assert!(!s.step_fit(a, t0), "first frame jumps");
        let b = Fit { cx: 100., cy: 0., hw: 100., hh: 60. };
        assert!(s.step_fit(b, t0 + Duration::from_millis(16)));
        assert!(s.fit.hw < 500. && s.fit.hw > 100.);
        let mut t = t0;
        for _ in 0..200 {
            t += Duration::from_millis(16);
            if !s.step_fit(b, t) {
                break;
            }
        }
        assert_eq!(s.fit, b);
    }

    fn pinch_frames(s: &mut MapState, start: Instant, deltas: &[f32]) -> Instant {
        let anchor = point(px(400.), px(300.));
        s.pinch(anchor, 0.0, TouchPhase::Started, start);
        let mut t = start;
        for d in deltas {
            t += Duration::from_millis(16);
            s.pinch(anchor, *d, TouchPhase::Moved, t);
        }
        t += Duration::from_millis(16);
        s.pinch(anchor, 0.0, TouchPhase::Ended, t);
        t
    }

    fn run_glide(s: &mut MapState, mut t: Instant) -> Instant {
        for _ in 0..600 {
            t += Duration::from_millis(16);
            if !s.step_glide(t) {
                break;
            }
        }
        t
    }

    #[test]
    fn a_quick_pinch_keeps_gliding_then_stops() {
        let mut s = MapState::default();
        s.bounds.set(Bounds::new(point(px(0.), px(0.)), size(px(800.), px(600.))));
        let t = pinch_frames(&mut s, Instant::now(), &[0.05; 6]);
        let released = s.zoom;
        assert!(released > 1.3);
        run_glide(&mut s, t);
        assert!(s.zoom > released * 1.05, "momentum carried it further: {} → {}", released, s.zoom);
        assert!(s.glide.is_none(), "and it came to rest");
    }

    #[test]
    fn a_pinch_held_still_before_release_does_not_fling() {
        let mut s = MapState::default();
        let anchor = point(px(400.), px(300.));
        let t0 = Instant::now();
        s.pinch(anchor, 0.0, TouchPhase::Started, t0);
        s.pinch(anchor, 0.1, TouchPhase::Moved, t0 + Duration::from_millis(16));
        let z = s.zoom;
        let t = t0 + Duration::from_millis(400);
        s.pinch(anchor, 0.0, TouchPhase::Ended, t);
        run_glide(&mut s, t);
        assert!((s.zoom - z).abs() < 1e-3);
    }

    #[test]
    fn pinching_past_the_limit_resists_and_springs_back() {
        let mut s = MapState::default();
        s.zoom = MAX_ZOOM;
        let t = pinch_frames(&mut s, Instant::now(), &[0.2; 10]);
        assert!(s.zoom > MAX_ZOOM, "it stretches a little");
        assert!(s.zoom < MAX_ZOOM * OVERSTRETCH, "but not far: {}", s.zoom);
        assert!(s.zoom < MAX_ZOOM * 1.2_f32.powi(10), "with resistance");
        run_glide(&mut s, t);
        assert!((s.zoom - MAX_ZOOM).abs() < 1e-3, "springs back to the limit: {}", s.zoom);
    }

    #[test]
    fn hit_finds_nearest_node() {
        let mut s = MapState::default();
        // Inner area 1000 × 500 after gutters; fit box 1000 × 500 world → scale 0.5.
        s.bounds.set(Bounds::new(
            point(px(0.), px(0.)),
            size(px(1000. + GUTTER_LEFT), px(500. + GUTTER_TOP + GUTTER_BOTTOM)),
        ));
        s.fit = Fit { cx: 0., cy: 0., hw: 1000., hh: 500. };
        let c = s.transform().to_screen(WorldPoint { x: 0., y: 0. });
        assert_eq!((c.x, c.y), (px(GUTTER_LEFT + 500.), px(GUTTER_TOP + 250.)));
        s.drawn = vec![(0, WorldPoint { x: 0., y: 0. }), (1, WorldPoint { x: 30., y: 0. })];
        assert_eq!(s.hit(c + point(px(2.), px(0.))), Some(0));
        assert_eq!(s.hit(c + point(px(14.), px(0.))), Some(1));
        assert_eq!(s.hit(c + point(px(200.), px(100.))), None);
    }
}
