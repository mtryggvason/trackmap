//! Graph model and layout. Pure data, no rendering.

mod model;

pub use model::{Graph, NodeId, NodeKey};
use trackmap_core::Camelot;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

/// Tempo-first map. x = BPM (slow left, fast right) is the main axis; y = key in Camelot order
/// with A/B pairs adjacent (1A, 1B, 2A, …) so compatible keys stay close. Songs without a key
/// get a row below the keys, songs without a BPM a column left of the slowest tempo.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MapLayout {
    pub bpm_min: f32,
    pub bpm_max: f32,
    /// World x spans [-half_width, half_width], y spans [-half_height, half_height].
    pub half_width: f32,
    pub half_height: f32,
}

/// 24 key rows plus one for unknown keys.
const ROWS: f32 = 25.0;

impl Default for MapLayout {
    fn default() -> Self {
        Self { bpm_min: 70.0, bpm_max: 180.0, half_width: 900.0, half_height: 500.0 }
    }
}

impl MapLayout {
    /// Fits the BPM axis to the library: the range of its tempos, rounded out to 5 BPM.
    /// With 20+ tracks, the slowest and fastest 2% are ignored (they sit at the edges), so a
    /// couple of outliers don't squeeze everything else together.
    pub fn fit(bpms: impl IntoIterator<Item = f32>) -> Self {
        let mut v: Vec<f32> = bpms.into_iter().filter(|b| b.is_finite() && *b > 0.0).collect();
        if v.is_empty() {
            return Self::default();
        }
        v.sort_by(f32::total_cmp);
        let trim = if v.len() >= 20 { v.len() / 50 } else { 0 };
        let (lo, hi) = (v[trim], v[v.len() - 1 - trim]);
        let lo = (lo / 5.0).floor() * 5.0;
        let mut hi = (hi / 5.0).ceil() * 5.0;
        if hi - lo < 20.0 {
            hi = lo + 20.0; // keep a sensible spread for tiny or uniform libraries
        }
        Self { bpm_min: lo, bpm_max: hi, ..Self::default() }
    }

    /// x for a BPM. Values outside the range are clamped to its edges.
    pub fn bpm_x(&self, bpm: f32) -> f32 {
        let t = ((bpm - self.bpm_min) / (self.bpm_max - self.bpm_min)).clamp(0.0, 1.0);
        -self.half_width + t * 2.0 * self.half_width
    }

    /// x of the column for songs with no BPM, just left of the axis.
    pub fn no_bpm_x(&self) -> f32 {
        -self.half_width - 80.0
    }

    /// y of a key's row: 1A at the top, 12B second to last, unknown keys at the bottom.
    pub fn key_y(&self, key: Option<Camelot>) -> f32 {
        let row = match key {
            Some(k) => ((k.number - 1) * 2 + u8::from(!k.minor)) as f32,
            None => ROWS - 1.0,
        };
        -self.half_height + (row + 0.5) / ROWS * 2.0 * self.half_height
    }

    /// Height of one key row, used for jitter.
    pub fn row_height(&self) -> f32 {
        2.0 * self.half_height / ROWS
    }

    pub fn position(&self, key: Option<Camelot>, bpm: Option<f32>) -> Point {
        let x = match bpm.filter(|b| *b > 0.0) {
            Some(b) => self.bpm_x(b),
            None => self.no_bpm_x(),
        };
        Point { x, y: self.key_y(key) }
    }

    /// BPM values to label along the axis: every 5 BPM for narrow ranges, otherwise every 10.
    pub fn bpm_ticks(&self) -> Vec<f32> {
        let step = if self.bpm_max - self.bpm_min <= 40.0 { 5.0 } else { 10.0 };
        let mut v = (self.bpm_min / step).ceil() * step;
        let mut out = Vec::new();
        while v <= self.bpm_max + 0.01 {
            out.push(v);
            v += step;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(s: &str) -> Option<Camelot> {
        Some(s.parse().unwrap())
    }

    #[test]
    fn tempo_is_the_main_axis() {
        let l = MapLayout::default();
        let a = l.position(k("8A"), Some(120.0));
        let b = l.position(k("8A"), Some(130.0));
        let c = l.position(k("8A"), Some(160.0));
        assert!(a.x < b.x && b.x < c.x);
        assert_eq!(a.y, b.y);
        // A whole-library spread in BPM spans more distance than the whole key range.
        assert!(2.0 * l.half_width > 2.0 * l.half_height);
    }

    #[test]
    fn compatible_keys_are_adjacent_rows() {
        let l = MapLayout::default();
        let row = l.row_height();
        let y = |s: &str| l.key_y(k(s));
        assert!((y("8B") - y("8A") - row).abs() < 0.01, "relative major right below minor");
        assert!((y("9A") - y("8A") - 2.0 * row).abs() < 0.01);
        assert!(y("2A") < y("8A"));
        assert!(l.key_y(None) > y("12B"), "unknown keys at the bottom");
    }

    #[test]
    fn fit_uses_the_library_range() {
        let l = MapLayout::fit([122.0, 126.5, 131.0]);
        assert_eq!((l.bpm_min, l.bpm_max), (120.0, 140.0));
        assert_eq!(l.bpm_x(120.0), -l.half_width);
        assert_eq!(l.bpm_x(140.0), l.half_width);
        assert_eq!(l.bpm_x(200.0), l.half_width, "clamped");
        assert_eq!(l.bpm_ticks(), [120.0, 125.0, 130.0, 135.0, 140.0]);

        assert_eq!(MapLayout::fit([]), MapLayout::default());
        let wide = MapLayout::fit([88.0, 174.0]);
        assert_eq!((wide.bpm_min, wide.bpm_max), (85.0, 175.0));
        assert_eq!(wide.bpm_ticks().first(), Some(&90.0));

        // One 70 BPM interlude among 99 house tracks doesn't stretch the axis.
        let mut many = vec![124.0; 99];
        many.push(70.0);
        let l = MapLayout::fit(many);
        assert_eq!((l.bpm_min, l.bpm_max), (120.0, 140.0));
    }

    #[test]
    fn missing_bpm_gets_its_own_column() {
        let l = MapLayout::default();
        assert!(l.position(k("8A"), None).x < l.bpm_x(l.bpm_min));
        assert_eq!(l.position(k("8A"), Some(0.0)).x, l.no_bpm_x());
    }
}
