//! App-wide settings, stored as JSON. The app decides where the file lives.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// The library opened most recently; reopened on launch.
    pub last_library: Option<PathBuf>,
    /// The drive each library was last synced to, by library root.
    pub sync_targets: BTreeMap<PathBuf, PathBuf>,
    /// Whether the last sync was whole-library or playlists-only.
    pub sync_scope: crate::sync::Scope,
}

impl Settings {
    /// Missing or unreadable settings give the defaults.
    pub fn load(path: &Path) -> Self {
        fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        fs::rename(tmp, path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/settings.json");
        assert_eq!(Settings::load(&path), Settings::default());

        let mut s = Settings { last_library: Some("/Music".into()), ..Default::default() };
        s.sync_targets.insert("/Music".into(), "/Volumes/USB".into());
        s.sync_scope = crate::sync::Scope::PlaylistsOnly;
        s.save(&path).unwrap();
        assert_eq!(Settings::load(&path), s);

        fs::write(&path, b"garbage").unwrap();
        assert_eq!(Settings::load(&path), Settings::default());

        // Settings files from before sync existed still load.
        fs::write(&path, br#"{"last_library":"/Old"}"#).unwrap();
        assert_eq!(Settings::load(&path).last_library, Some("/Old".into()));
    }
}
