//! Graph model and layout. Pure data, no rendering.

mod model;

pub use model::{Graph, NodeId, NodeKey};
use trackmap_core::Camelot;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

/// Tempo × key map. x = BPM (slow left, fast right), grouped into bins whose width depends on
/// the zoom (see [`MapLayout::bin_for`]). y = key in Camelot order with A/B pairs adjacent
/// (1A, 1B, 2A, …), unknown keys in a row at the bottom. Songs in the same bin and key sit side
/// by side. Songs without a BPM get a column left of the slowest tempo.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MapLayout {
    pub bpm_min: f32,
    pub bpm_max: f32,
    /// World x spans [-half_width, half_width], y spans [-half_height, half_height].
    pub half_width: f32,
    pub half_height: f32,
}

/// 24 key rows plus one for unknown keys.
pub const KEY_ROWS: usize = 25;

/// BPM bin widths to choose from, finest first.
pub const BINS: [f32; 7] = [0.1, 0.2, 0.5, 1.0, 2.0, 5.0, 10.0];

/// World width given to the no-BPM column.
const NO_BPM_WIDTH: f32 = 60.0;

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

    /// World units per BPM along x.
    pub fn world_per_bpm(&self) -> f32 {
        2.0 * self.half_width / (self.bpm_max - self.bpm_min)
    }

    /// The finest bin whose on-screen width is at least `min_px`, given how many screen pixels
    /// one world unit takes. Zooming in therefore splits songs into finer tempo groups.
    pub fn bin_for(&self, px_per_world: f32, min_px: f32) -> f32 {
        let px_per_bpm = self.world_per_bpm() * px_per_world;
        BINS.into_iter().find(|b| b * px_per_bpm >= min_px).unwrap_or(BINS[BINS.len() - 1])
    }

    /// Centre BPM of the bin `bpm` falls in.
    pub fn bin_center(bpm: f32, bin: f32) -> f32 {
        (bpm / bin).round() * bin
    }

    /// Row of a key: 1A, 1B, 2A, 2B, …, 12B, then unknown.
    pub fn key_row(key: Option<Camelot>) -> usize {
        match key {
            Some(k) => (k.number as usize - 1) * 2 + usize::from(!k.minor),
            None => KEY_ROWS - 1,
        }
    }

    /// Height of one key row.
    pub fn row_height(&self) -> f32 {
        2.0 * self.half_height / KEY_ROWS as f32
    }

    /// y of a key row's centre.
    pub fn row_y(&self, row: usize) -> f32 {
        -self.half_height + (row as f32 + 0.5) * self.row_height()
    }

    /// Cell for a song: (centre x, available width) of its tempo bin.
    pub fn cell_x(&self, bpm: Option<f32>, bin: f32) -> (f32, f32) {
        match bpm.filter(|b| b.is_finite() && *b > 0.0) {
            Some(b) => (self.bpm_x(Self::bin_center(b, bin)), bin * self.world_per_bpm()),
            None => (self.no_bpm_x(), NO_BPM_WIDTH),
        }
    }

    /// BPM labels between `from` and `to` (clamped to the axis), spaced at least `min_px` apart
    /// on screen. Returns the step and the values.
    pub fn bpm_ticks(&self, from: f32, to: f32, px_per_world: f32, min_px: f32) -> (f32, Vec<f32>) {
        let px_per_bpm = self.world_per_bpm() * px_per_world;
        let step = [0.1, 0.2, 0.5, 1.0, 2.0, 5.0, 10.0, 20.0, 50.0]
            .into_iter()
            .find(|s| s * px_per_bpm >= min_px)
            .unwrap_or(50.0);
        let (from, to) = (from.max(self.bpm_min), to.min(self.bpm_max));
        let mut out = Vec::new();
        let mut i = (from / step).ceil() as i64;
        while (i as f32) * step <= to + step * 0.01 {
            out.push(((i as f32) * step * 10.0).round() / 10.0);
            i += 1;
        }
        (step, out)
    }

    /// BPM at a world x (inverse of [`MapLayout::bpm_x`], unclamped).
    pub fn x_bpm(&self, x: f32) -> f32 {
        self.bpm_min + (x + self.half_width) / self.world_per_bpm()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zooming_in_picks_finer_bins() {
        let l = MapLayout::fit([120.0, 140.0]); // 20 BPM over 1800 world units = 90 per BPM
        assert_eq!(l.world_per_bpm(), 90.0);
        assert_eq!(l.bin_for(0.5, 20.0), 0.5); // 45 px per BPM
        assert_eq!(l.bin_for(0.2, 20.0), 2.0); // 18 px per BPM
        assert_eq!(l.bin_for(5.0, 20.0), 0.1); // 450 px per BPM
        assert_eq!(MapLayout::bin_center(124.3, 1.0), 124.0);
        assert!((MapLayout::bin_center(124.3, 0.5) - 124.5).abs() < 1e-4);
        assert!((MapLayout::bin_center(124.3, 0.1) - 124.3).abs() < 1e-4);
    }

    #[test]
    fn key_rows_pair_minor_and_major() {
        let k = |s: &str| Some(s.parse::<Camelot>().unwrap());
        assert_eq!(MapLayout::key_row(k("1A")), 0);
        assert_eq!(MapLayout::key_row(k("1B")), 1);
        assert_eq!(MapLayout::key_row(k("8A")), 14);
        assert_eq!(MapLayout::key_row(k("12B")), 23);
        assert_eq!(MapLayout::key_row(None), KEY_ROWS - 1);
        let l = MapLayout::default();
        assert!(l.row_y(0) < l.row_y(1));
        assert!((l.row_y(KEY_ROWS - 1) + l.row_height() / 2.0 - l.half_height).abs() < 1e-3);
    }

    #[test]
    fn ticks_follow_zoom_and_view() {
        let l = MapLayout::fit([120.0, 140.0]);
        let (step, ticks) = l.bpm_ticks(0.0, 1000.0, 0.5, 60.0);
        assert_eq!(step, 2.0);
        assert_eq!(ticks.first(), Some(&120.0));
        assert_eq!(ticks.last(), Some(&140.0));
        let (step, ticks) = l.bpm_ticks(124.0, 125.0, 10.0, 60.0);
        assert_eq!(step, 0.1);
        assert_eq!(ticks.len(), 11);
        assert!((l.x_bpm(l.bpm_x(131.5)) - 131.5).abs() < 1e-3);
    }

    #[test]
    fn fit_uses_the_library_range() {
        let l = MapLayout::fit([122.0, 126.5, 131.0]);
        assert_eq!((l.bpm_min, l.bpm_max), (120.0, 140.0));
        assert_eq!(l.bpm_x(120.0), -l.half_width);
        assert_eq!(l.bpm_x(140.0), l.half_width);
        assert_eq!(l.bpm_x(200.0), l.half_width, "clamped");

        assert_eq!(MapLayout::fit([]), MapLayout::default());
        let wide = MapLayout::fit([88.0, 174.0]);
        assert_eq!((wide.bpm_min, wide.bpm_max), (85.0, 175.0));
        assert_eq!(wide.bpm_ticks(0.0, 1000.0, 0.5, 60.0).1.first(), Some(&90.0));

        // One 70 BPM interlude among 99 house tracks doesn't stretch the axis.
        let mut many = vec![124.0; 99];
        many.push(70.0);
        let l = MapLayout::fit(many);
        assert_eq!((l.bpm_min, l.bpm_max), (120.0, 140.0));
    }

}
