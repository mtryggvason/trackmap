//! Search matching: every word of the query must appear (any order, any case, accents
//! ignored), so "jonfri andalusia" finds "Jónfrí – Andalusia RMX".

use unicode_normalization::UnicodeNormalization;

use crate::Track;

/// Lowercase with accents removed ("Jónfrí" → "jonfri").
pub fn fold(s: &str) -> String {
    s.nfd().filter(|c| !unicode_normalization::char::is_combining_mark(*c)).flat_map(char::to_lowercase).collect()
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Query {
    words: Vec<String>,
    whole: String,
}

impl Query {
    pub fn new(q: &str) -> Self {
        let whole = fold(q.trim());
        Self { words: whole.split_whitespace().map(str::to_string).collect(), whole }
    }

    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }

    /// Whether every word appears in `folded` (text already passed through [`fold`]).
    pub fn matches(&self, folded: &str) -> bool {
        self.words.iter().all(|w| folded.contains(w.as_str()))
    }

    /// How well a name matches, lower is better; `None` if it doesn't. A name that starts with
    /// the query beats one with a word starting with it, which beats a match anywhere.
    pub fn rank(&self, name: &str) -> Option<u8> {
        let f = fold(name);
        if !self.matches(&f) {
            return None;
        }
        Some(if f.starts_with(&self.whole) {
            0
        } else if f.split(|c: char| !c.is_alphanumeric()).any(|w| w.starts_with(&self.words[0])) {
            1
        } else {
            2
        })
    }
}

/// Everything a track can be found by, folded, for [`Query::matches`].
pub fn track_text(t: &Track) -> String {
    let file = t.path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
    let parts = [&t.title, &t.artist, &t.album, &t.label, &t.genre];
    let mut s: String = parts.iter().filter_map(|p| p.as_deref()).collect::<Vec<_>>().join(" ");
    s.push(' ');
    s.push_str(&file);
    fold(&s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folding_ignores_case_and_accents() {
        assert_eq!(fold("Jónfrí"), "jonfri");
        assert_eq!(fold("Ég Gef Þér"), "eg gef þer");
        assert_eq!(fold("CAFÉ"), "cafe");
    }

    #[test]
    fn every_word_must_appear_in_any_order() {
        let t = Track {
            path: "/m/Andalusia RMX.wav".into(),
            title: Some("Andalusia RMX".into()),
            artist: Some("Jónfrí".into()),
            label: Some("Good Label".into()),
            ..Default::default()
        };
        let text = track_text(&t);
        assert!(Query::new("jonfri andalusia").matches(&text));
        assert!(Query::new("ANDAL  jón").matches(&text));
        assert!(Query::new("good").matches(&text), "labels count");
        assert!(Query::new("rmx.wav").matches(&text), "file names count");
        assert!(!Query::new("jonfri techno").matches(&text));
        assert!(Query::new("   ").is_empty());
    }

    #[test]
    fn ranking_prefers_names_starting_with_the_query() {
        let q = Query::new("bull");
        assert_eq!(q.rank("Bullion"), Some(0));
        assert_eq!(q.rank("The Bullion Band"), Some(1));
        assert_eq!(q.rank("Rebullet"), Some(2));
        assert_eq!(q.rank("Calibre"), None);
    }
}
