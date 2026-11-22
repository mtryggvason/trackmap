//! Musical keys in Camelot notation (1A–12B), used for harmonic layout and mixing.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// A Camelot wheel position. `number` is 1..=12; `minor` is the A ring, otherwise B.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Camelot {
    pub number: u8,
    pub minor: bool,
}

impl Camelot {
    pub fn new(number: u8, minor: bool) -> Option<Self> {
        (1..=12).contains(&number).then_some(Self { number, minor })
    }

    /// Keys that mix harmonically: same key, ±1 on the wheel, and the relative major/minor.
    pub fn is_compatible(self, other: Camelot) -> bool {
        let d = (self.number as i8 - other.number as i8).rem_euclid(12);
        if self.minor == other.minor {
            d == 0 || d == 1 || d == 11
        } else {
            d == 0
        }
    }

    /// Angle on the wheel in radians, with 12 at the top and going clockwise.
    pub fn angle(self) -> f32 {
        let step = std::f32::consts::TAU / 12.0;
        (self.number % 12) as f32 * step - std::f32::consts::FRAC_PI_2
    }
}

impl fmt::Display for Camelot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", self.number, if self.minor { 'A' } else { 'B' })
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct ParseKeyError;

/// Pitch class (C=0) → Camelot number, for major keys. Minor = relative major's number.
const MAJOR_CAMELOT: [u8; 12] = [8, 3, 10, 5, 12, 7, 2, 9, 4, 11, 6, 1];

impl FromStr for Camelot {
    type Err = ParseKeyError;

    /// Accepts Camelot ("8A", "12b") and musical notation ("Am", "A minor", "F#", "Bbm", "C#maj").
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        if let Some(c) = parse_camelot(s) {
            return Ok(c);
        }
        parse_musical(s).ok_or(ParseKeyError)
    }
}

fn parse_camelot(s: &str) -> Option<Camelot> {
    let (num, ring) = s.split_at(s.len().checked_sub(1)?);
    let number: u8 = num.parse().ok()?;
    let minor = match ring {
        "A" | "a" => true,
        "B" | "b" => false,
        _ => return None,
    };
    Camelot::new(number, minor)
}

fn parse_musical(s: &str) -> Option<Camelot> {
    let mut chars = s.chars();
    let mut pc: i8 = match chars.next()?.to_ascii_uppercase() {
        'C' => 0,
        'D' => 2,
        'E' => 4,
        'F' => 5,
        'G' => 7,
        'A' => 9,
        'B' => 11,
        _ => return None,
    };
    let mut rest: String = chars.collect();
    if let Some(r) = rest.strip_prefix(['#', '♯']) {
        pc += 1;
        rest = r.to_string();
    } else if let Some(r) = rest.strip_prefix(['b', '♭']) {
        pc -= 1;
        rest = r.to_string();
    }
    let rest = rest.trim().to_ascii_lowercase();
    let minor = match rest.as_str() {
        "" | "maj" | "major" => false,
        "m" | "min" | "minor" => true,
        _ => return None,
    };
    let pc = pc.rem_euclid(12) as usize;
    // A minor key shares its Camelot number with the major key 3 semitones up.
    let idx = if minor { (pc + 3) % 12 } else { pc };
    Camelot::new(MAJOR_CAMELOT[idx], minor)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(s: &str) -> Camelot {
        s.parse().unwrap()
    }

    #[test]
    fn parses_camelot_and_musical() {
        assert_eq!(k("8A"), k("Am"));
        assert_eq!(k("8B"), k("C"));
        assert_eq!(k("1B"), k("B"));
        assert_eq!(k("5A"), k("C minor"));
        assert_eq!(k("3B"), k("Db"));
        assert_eq!(k("3B"), k("C#"));
        assert_eq!(k("12a").to_string(), "12A");
        assert!("13A".parse::<Camelot>().is_err());
        assert!("H".parse::<Camelot>().is_err());
    }

    #[test]
    fn compatibility() {
        assert!(k("8A").is_compatible(k("9A")));
        assert!(k("12A").is_compatible(k("1A")));
        assert!(k("8A").is_compatible(k("8B")));
        assert!(!k("8A").is_compatible(k("9B")));
        assert!(!k("8A").is_compatible(k("10A")));
    }
}
