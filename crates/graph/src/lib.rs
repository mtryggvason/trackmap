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
    /// Key rows shown, top to bottom (indices from [`MapLayout::key_row`]); the first
    /// `row_count` entries are used. A subset view only shows the keys it contains.
    rows: [u8; KEY_ROWS],
    row_count: u8,
}

/// Rows never get taller than this, so a view with two keys doesn't stretch absurdly.
const MAX_ROW_HEIGHT: f32 = 90.0;

const ALL_ROWS: [u8; KEY_ROWS] = {
    let mut r = [0u8; KEY_ROWS];
    let mut i = 0;
    while i < KEY_ROWS {
        r[i] = i as u8;
        i += 1;
    }
    r
};

/// 24 key rows plus one for unknown keys.
pub const KEY_ROWS: usize = 25;

/// BPM bin widths to choose from, finest first.
pub const BINS: [f32; 7] = [0.1, 0.2, 0.5, 1.0, 2.0, 5.0, 10.0];

/// World width given to the no-BPM column.
const NO_BPM_WIDTH: f32 = 60.0;

/// Empty stretches of the BPM axis longer than this many bins are compressed to it.
const MAX_GAP_BINS: f32 = 2.5;

/// World width of one occupied tempo bin on the drawn axis (about one key row high), so the
/// map's width depends on how many tempos are present, not on the BPM range.
pub const SLOT: f32 = 44.0;

/// Tempos are grouped by whole BPM on the drawn axis; zooming only magnifies.
pub const BPM_BIN: f32 = 1.0;

/// The BPM axis as actually drawn: one [`SLOT`] per bin, so neighbouring tempos keep their true
/// spacing while empty stretches are compressed to a short break.
#[derive(Debug, Clone, PartialEq)]
pub struct Axis {
    /// Width of one bin in BPM.
    pub bin: f32,
    /// World width of one bin ([`SLOT`]).
    pub cell: f32,
    /// Occupied bins (index = round(bpm / bin)), ascending, with their world x.
    pub knots: Vec<(i64, f32)>,
    /// World x of the middle of each compressed gap, for drawing a break mark.
    pub gaps: Vec<f32>,
}

impl Axis {
    /// Axis for these tempos at `bin` BPM per bin, centred on 0. `None` if there are no tempos.
    pub fn new(tempos: impl IntoIterator<Item = f32>, bin: f32) -> Option<Self> {
        let mut idx: Vec<i64> = tempos
            .into_iter()
            .filter(|b| b.is_finite() && *b > 0.0)
            .map(|b| (b / bin).round() as i64)
            .collect();
        idx.sort_unstable();
        idx.dedup();
        let first = *idx.first()?;
        let cell = SLOT;
        let (mut knots, mut gaps, mut x) = (vec![(first, 0.0)], Vec::new(), 0.0);
        for w in idx.windows(2) {
            let steps = (w[1] - w[0]) as f32;
            let width = steps.min(MAX_GAP_BINS) * cell;
            if steps > MAX_GAP_BINS {
                gaps.push(x + width / 2.0);
            }
            x += width;
            knots.push((w[1], x));
        }
        let shift = x / 2.0;
        for k in &mut knots {
            k.1 -= shift;
        }
        for g in &mut gaps {
            *g -= shift;
        }
        Some(Self { bin, cell, knots, gaps })
    }

    /// World x for a tempo: exact for occupied bins, interpolated in between.
    pub fn x(&self, bpm: f32) -> f32 {
        let i = (bpm / self.bin).round() as i64;
        match self.knots.binary_search_by_key(&i, |k| k.0) {
            Ok(k) => self.knots[k].1,
            Err(0) => self.knots[0].1,
            Err(k) if k >= self.knots.len() => self.knots[self.knots.len() - 1].1,
            Err(k) => {
                let (a, b) = (self.knots[k - 1], self.knots[k]);
                a.1 + (b.1 - a.1) * (i - a.0) as f32 / (b.0 - a.0) as f32
            }
        }
    }

    /// Tempo of each occupied bin with its x, for axis labels.
    pub fn labels(&self) -> impl Iterator<Item = (f32, f32)> + '_ {
        self.knots.iter().map(|(i, x)| (((*i as f32) * self.bin * 10.0).round() / 10.0, *x))
    }

    /// x of the column for songs with no BPM, left of the slowest tempo.
    pub fn no_bpm_x(&self) -> f32 {
        self.knots[0].1 - (self.cell * 3.0).max(120.0)
    }
}

impl Default for MapLayout {
    fn default() -> Self {
        Self {
            bpm_min: 70.0,
            bpm_max: 180.0,
            half_width: 900.0,
            half_height: 500.0,
            rows: ALL_ROWS,
            row_count: KEY_ROWS as u8,
        }
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
        let (mut lo, mut hi) = ((lo / 5.0).floor() * 5.0, (hi / 5.0).ceil() * 5.0);
        if hi - lo < 20.0 {
            // Keep a sensible spread for small or uniform sets, centred on their tempos.
            let mid = ((lo + hi) / 2.0 / 5.0).round() * 5.0;
            (lo, hi) = (mid - 10.0, mid + 10.0);
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

    /// Shows only these key rows (in Camelot order), e.g. the keys present in a selection.
    pub fn with_rows(mut self, used: impl IntoIterator<Item = usize>) -> Self {
        let mut present = [false; KEY_ROWS];
        for r in used {
            present[r.min(KEY_ROWS - 1)] = true;
        }
        let mut n = 0;
        for (r, p) in present.iter().enumerate() {
            if *p {
                self.rows[n] = r as u8;
                n += 1;
            }
        }
        if n == 0 {
            return Self { rows: ALL_ROWS, row_count: KEY_ROWS as u8, ..self };
        }
        self.row_count = n as u8;
        self
    }

    /// Key rows shown, top to bottom.
    pub fn rows(&self) -> &[u8] {
        &self.rows[..self.row_count as usize]
    }

    /// Height of one key row.
    pub fn row_height(&self) -> f32 {
        (2.0 * self.half_height / self.row_count as f32).min(MAX_ROW_HEIGHT)
    }

    /// y of a key row's centre. Rows are centred vertically; rows not shown go far below.
    pub fn row_y(&self, row: usize) -> f32 {
        let Some(slot) = self.rows().iter().position(|r| *r as usize == row) else {
            return self.half_height * 4.0;
        };
        let total = self.row_height() * self.row_count as f32;
        -total / 2.0 + (slot as f32 + 0.5) * self.row_height()
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
    fn axis_compresses_gaps_and_keeps_true_spacing_where_songs_are() {
        // 120, 121, 123 close together; 135 far away. 1 BPM bins, 10 world units per BPM.
        let a = Axis::new([135.0, 120.0, 121.0, 123.0, 123.2], 1.0).unwrap();
        let labels: Vec<f32> = a.labels().map(|(b, _)| b).collect();
        assert_eq!(labels, [120.0, 121.0, 123.0, 135.0]);
        assert_eq!(a.x(121.0) - a.x(120.0), SLOT, "neighbours keep true spacing");
        assert_eq!(a.x(123.0) - a.x(121.0), 2.0 * SLOT, "a small gap stays proportional");
        assert_eq!(a.x(135.0) - a.x(123.0), MAX_GAP_BINS * SLOT, "a big gap is compressed");
        assert_eq!(a.gaps.len(), 1);
        assert!((a.x(120.0) + a.x(135.0)).abs() < 1e-3, "centred");
        assert!(a.x(122.0) > a.x(121.0) && a.x(122.0) < a.x(123.0), "in-between values interpolate");
        assert!(a.no_bpm_x() < a.x(120.0));
        assert!(Axis::new([], 1.0).is_none());
    }

    #[test]
    fn zooming_in_splits_tempos_without_spreading_them_out() {
        let tempos = [124.0, 124.3, 131.0];
        let coarse = Axis::new(tempos, 1.0).unwrap();
        let fine = Axis::new(tempos, 0.1).unwrap(); // zoomed in 10×
        assert_eq!(coarse.labels().count(), 2, "124 and 124.3 share a bin");
        assert_eq!(fine.labels().count(), 3, "zoomed in they separate");
        // The 124 → 131 stretch is empty, so zoomed in it's a short break, not 67 slots.
        assert!(fine.x(131.0) - fine.x(124.3) <= MAX_GAP_BINS * fine.cell + 1e-3);
    }


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

        // A view with only two keys shows two rows, centred, capped in height.
        let sub = l.with_rows([14, 3]);
        assert_eq!(sub.rows(), [3, 14]);
        assert_eq!(sub.row_height(), MAX_ROW_HEIGHT);
        assert!((sub.row_y(3) + sub.row_y(14)).abs() < 1e-3);
        assert!(sub.row_y(5) > sub.half_height, "hidden rows are off the map");
        assert_eq!(l.with_rows([]).rows().len(), KEY_ROWS);
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
        let l = MapLayout::fit([122.0, 126.5, 138.0]);
        assert_eq!((l.bpm_min, l.bpm_max), (120.0, 140.0));
        // A narrow set is centred, not pushed to one edge.
        let narrow = MapLayout::fit([124.0, 124.0, 126.0]);
        assert_eq!((narrow.bpm_min, narrow.bpm_max), (115.0, 135.0));
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
        assert_eq!((l.bpm_min, l.bpm_max), (115.0, 135.0));
    }

}
