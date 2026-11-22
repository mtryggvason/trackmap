//! Graph model and layout. Pure data, no rendering.

use napworks_core::Camelot;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

/// Parameters for the harmonic map: angle = Camelot position, radius = BPM.
#[derive(Debug, Clone, Copy)]
pub struct HarmonicLayout {
    /// Inner ring (minor keys) spans [inner, inner + ring_width]; the major ring follows after `gap`.
    pub inner: f32,
    pub ring_width: f32,
    pub gap: f32,
    pub bpm_min: f32,
    pub bpm_max: f32,
}

impl Default for HarmonicLayout {
    fn default() -> Self {
        Self { inner: 200.0, ring_width: 300.0, gap: 60.0, bpm_min: 70.0, bpm_max: 180.0 }
    }
}

impl HarmonicLayout {
    /// Position of a song with this key and BPM. Songs with no key go to the centre,
    /// spread out by BPM.
    pub fn position(&self, key: Option<Camelot>, bpm: Option<f32>) -> Point {
        let t = bpm.map(|b| self.bpm_t(b)).unwrap_or(0.5);
        let Some(key) = key else {
            return Point { x: 0.0, y: (t - 0.5) * self.inner };
        };
        let base = if key.minor { self.inner } else { self.inner + self.ring_width + self.gap };
        let r = base + t * self.ring_width;
        let a = key.angle();
        Point { x: r * a.cos(), y: r * a.sin() }
    }

    /// BPM mapped to 0..=1 on a log scale. Half/double tempo is folded into the range.
    fn bpm_t(&self, mut bpm: f32) -> f32 {
        if bpm <= 0.0 {
            return 0.5;
        }
        while bpm < self.bpm_min {
            bpm *= 2.0;
        }
        while bpm > self.bpm_max {
            bpm /= 2.0;
        }
        let (lo, hi) = (self.bpm_min.ln(), self.bpm_max.ln());
        ((bpm.ln() - lo) / (hi - lo)).clamp(0.0, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dist(a: Point, b: Point) -> f32 {
        ((a.x - b.x).powi(2) + (a.y - b.y).powi(2)).sqrt()
    }

    #[test]
    fn compatible_keys_are_closer_than_distant_ones() {
        let l = HarmonicLayout::default();
        let p = |k: &str| l.position(Some(k.parse().unwrap()), Some(124.0));
        assert!(dist(p("8A"), p("9A")) < dist(p("8A"), p("2A")));
    }

    #[test]
    fn similar_tempo_is_closer() {
        let l = HarmonicLayout::default();
        let k = Some("8A".parse().unwrap());
        let a = l.position(k, Some(124.0));
        assert!(dist(a, l.position(k, Some(126.0))) < dist(a, l.position(k, Some(150.0))));
    }

    #[test]
    fn minor_ring_inside_major_ring() {
        let l = HarmonicLayout::default();
        let o = Point { x: 0.0, y: 0.0 };
        let minor = l.position(Some("8A".parse().unwrap()), Some(180.0));
        let major = l.position(Some("8B".parse().unwrap()), Some(70.0));
        assert!(dist(o, minor) < dist(o, major));
    }
}
