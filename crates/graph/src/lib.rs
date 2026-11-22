//! Graph model and layout. Pure data, no rendering.

mod model;

pub use model::{Graph, NodeId, NodeKey};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

/// Tempo map. x = BPM (slow left, fast right). Songs at the same (rounded) BPM stack into a
/// column, centred vertically, so the map reads like a histogram of the library's tempos.
/// Songs without a BPM get a column left of the slowest tempo.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MapLayout {
    pub bpm_min: f32,
    pub bpm_max: f32,
    /// World x spans [-half_width, half_width], y spans [-half_height, half_height].
    pub half_width: f32,
    pub half_height: f32,
}

/// Vertical gap between stacked songs, at most; tall columns are squeezed to fit.
const MAX_SPACING: f32 = 30.0;

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

    /// x of a song's column: its BPM rounded to a whole number, or the no-BPM column.
    pub fn column_x(&self, bpm: Option<f32>) -> f32 {
        match bpm.filter(|b| b.is_finite() && *b > 0.0) {
            Some(b) => self.bpm_x(b.round()),
            None => self.no_bpm_x(),
        }
    }

    /// Vertical spacing so the tallest column fits in the layout's height.
    pub fn stack_spacing(&self, tallest: usize) -> f32 {
        (2.0 * self.half_height / tallest.max(1) as f32).min(MAX_SPACING)
    }

    /// y of the `i`th of `n` songs in a column, centred on 0.
    pub fn stack_y(&self, i: usize, n: usize, spacing: f32) -> f32 {
        (i as f32 - (n as f32 - 1.0) / 2.0) * spacing
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

    #[test]
    fn columns_follow_rounded_bpm() {
        let l = MapLayout::default();
        assert!(l.column_x(Some(120.0)) < l.column_x(Some(130.0)));
        assert_eq!(l.column_x(Some(123.6)), l.column_x(Some(124.0)));
        assert_eq!(l.column_x(None), l.no_bpm_x());
        assert_eq!(l.column_x(Some(0.0)), l.no_bpm_x());
        assert!(l.no_bpm_x() < l.bpm_x(l.bpm_min));
    }

    #[test]
    fn stacks_are_centred_and_fit() {
        let l = MapLayout::default();
        assert_eq!(l.stack_spacing(3), MAX_SPACING);
        let tall = l.stack_spacing(200);
        assert!(tall * 199.0 <= 2.0 * l.half_height);
        let ys: Vec<f32> = (0..3).map(|i| l.stack_y(i, 3, 30.0)).collect();
        assert_eq!(ys, [-30.0, 0.0, 30.0]);
        assert_eq!(l.stack_y(0, 1, 30.0), 0.0);
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

}
