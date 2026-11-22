//! Playlists are folders in `<root>/Playlists/`, containing relative symlinks to library tracks.
//!
//! Folders have no order, so link names carry a position prefix: `001 - Night Drive.wav`.
//! That keeps the order visible in Finder too. Links added by hand without a prefix sort after.
//! Plain files dropped into a playlist folder are listed as entries as well.

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::os::unix::fs::symlink;
use std::path::{Component, Path, PathBuf};

use crate::{is_supported_audio, PLAYLISTS_DIR};

#[derive(Debug, Clone, PartialEq)]
pub struct Playlist {
    pub name: String,
    pub entries: Vec<Entry>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    /// The link (or file) inside the playlist folder.
    pub link: PathBuf,
    /// The track it points to, as an absolute path.
    pub target: PathBuf,
    /// The target no longer exists (track moved or deleted).
    pub missing: bool,
}

impl Playlist {
    pub fn contains(&self, track: &Path) -> bool {
        self.entries.iter().any(|e| e.target == track)
    }
}

pub fn playlists_dir(root: &Path) -> PathBuf {
    root.join(PLAYLISTS_DIR)
}

fn dir(root: &Path, name: &str) -> io::Result<PathBuf> {
    validate_name(name)?;
    Ok(playlists_dir(root).join(name))
}

fn invalid(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, msg.to_string())
}

/// Playlist names become folder names, so they must be valid single path components.
pub fn validate_name(name: &str) -> io::Result<()> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(invalid("playlist name is empty"));
    }
    if trimmed != name {
        return Err(invalid("playlist name has leading or trailing spaces"));
    }
    if name.starts_with('.') || name.contains(['/', ':', '\0']) {
        return Err(invalid("playlist name can't start with '.' or contain '/' or ':'"));
    }
    Ok(())
}

/// All playlists, sorted by name. A missing `Playlists/` folder means no playlists.
pub fn list(root: &Path) -> io::Result<Vec<Playlist>> {
    let base = playlists_dir(root);
    let read = match fs::read_dir(&base) {
        Ok(r) => r,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut out = Vec::new();
    for entry in read.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        out.push(Playlist { entries: entries(&entry.path())?, name });
    }
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    Ok(out)
}

pub fn get(root: &Path, name: &str) -> io::Result<Playlist> {
    Ok(Playlist { name: name.to_string(), entries: entries(&dir(root, name)?)? })
}

fn entries(dir: &Path) -> io::Result<Vec<Entry>> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir)?.flatten() {
        let link = entry.path();
        if entry.file_name().to_string_lossy().starts_with('.') || !is_supported_audio(&link) {
            continue;
        }
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_symlink() {
            let Ok(raw) = fs::read_link(&link) else { continue };
            let target = resolve(dir, &raw);
            out.push(Entry { missing: !target.exists(), link, target });
        } else if ft.is_file() {
            out.push(Entry { target: link.clone(), link, missing: false });
        }
    }
    out.sort_by(|a, b| a.link.file_name().cmp(&b.link.file_name()));
    Ok(out)
}

/// Absolute target of a link in `dir`: canonical if it exists, otherwise lexically normalised.
fn resolve(dir: &Path, raw: &Path) -> PathBuf {
    let joined = dir.join(raw);
    joined.canonicalize().unwrap_or_else(|_| normalise(&joined))
}

fn normalise(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

pub fn create(root: &Path, name: &str) -> io::Result<()> {
    let d = dir(root, name)?;
    if d.exists() {
        return Err(io::Error::new(io::ErrorKind::AlreadyExists, "playlist already exists"));
    }
    fs::create_dir_all(d)
}

pub fn rename(root: &Path, from: &str, to: &str) -> io::Result<()> {
    let (a, b) = (dir(root, from)?, dir(root, to)?);
    if b.exists() {
        return Err(io::Error::new(io::ErrorKind::AlreadyExists, "playlist already exists"));
    }
    // Links are relative to the playlist folder's depth, which doesn't change, so they keep working.
    fs::rename(a, b)
}

/// Deletes a playlist. Only links are removed, never tracks. Refuses if the folder holds real
/// audio files, since deleting those would delete music.
pub fn delete(root: &Path, name: &str) -> io::Result<()> {
    let d = dir(root, name)?;
    for entry in fs::read_dir(&d)?.flatten() {
        let ft = entry.file_type()?;
        if ft.is_file() && is_supported_audio(&entry.path()) {
            return Err(io::Error::other(format!(
                "playlist folder contains real files (e.g. {}); move them out first",
                entry.file_name().to_string_lossy()
            )));
        }
    }
    for entry in fs::read_dir(&d)?.flatten() {
        if entry.file_type()?.is_symlink() || entry.file_name() == ".DS_Store" {
            fs::remove_file(entry.path())?;
        }
    }
    fs::remove_dir(d)
}

/// Appends `track` to the playlist. Returns `false` if it was already in it.
pub fn add(root: &Path, name: &str, track: &Path) -> io::Result<bool> {
    let pl = get(root, name)?;
    if pl.contains(track) {
        return Ok(false);
    }
    let d = dir(root, name)?;
    let link = d.join(link_name(pl.entries.len() + 1, track));
    symlink(relative_target(root, track)?, link)?;
    Ok(true)
}

/// Removes `track` from the playlist and renumbers the rest. Returns `false` if it wasn't in it.
pub fn remove(root: &Path, name: &str, track: &Path) -> io::Result<bool> {
    let pl = get(root, name)?;
    let Some(entry) = pl.entries.iter().find(|e| e.target == track) else { return Ok(false) };
    if !entry.link.is_symlink() {
        return Err(io::Error::other("entry is a real file, not a link; move it out in Finder"));
    }
    fs::remove_file(&entry.link)?;
    renumber(root, name)?;
    Ok(true)
}

/// Puts the playlist in the given order of tracks. Tracks not mentioned keep their relative
/// order after the ones that are.
pub fn reorder(root: &Path, name: &str, order: &[PathBuf]) -> io::Result<()> {
    let pl = get(root, name)?;
    let mut entries = pl.entries;
    entries.sort_by_key(|e| order.iter().position(|t| *t == e.target).unwrap_or(usize::MAX));
    write_order(root, name, &entries)
}

fn renumber(root: &Path, name: &str) -> io::Result<()> {
    let pl = get(root, name)?;
    write_order(root, name, &pl.entries)
}

/// Renames links to `NNN - <file name>` in the given order. Two passes, so a new name never
/// collides with an old one that hasn't been moved yet.
fn write_order(root: &Path, name: &str, entries: &[Entry]) -> io::Result<()> {
    let d = dir(root, name)?;
    let links: Vec<&Entry> = entries.iter().filter(|e| e.link.is_symlink()).collect();
    let mut temps = Vec::with_capacity(links.len());
    for (i, e) in links.iter().enumerate() {
        let tmp = d.join(format!(".reorder-{i}"));
        fs::rename(&e.link, &tmp)?;
        temps.push(tmp);
    }
    for (i, (tmp, e)) in temps.iter().zip(&links).enumerate() {
        fs::rename(tmp, d.join(link_name(i + 1, &e.target)))?;
    }
    Ok(())
}

fn link_name(position: usize, track: &Path) -> String {
    let file = track.file_name().unwrap_or(OsStr::new("track")).to_string_lossy();
    format!("{position:03} - {file}")
}

/// Path from `Playlists/<name>/` to the track, e.g. `../../House/a.wav`.
fn relative_target(root: &Path, track: &Path) -> io::Result<PathBuf> {
    let rel = track
        .strip_prefix(root)
        .map_err(|_| invalid("track is not inside the library"))?;
    Ok(Path::new("../..").join(rel))
}

/// Display title for a link name: strips the `NNN - ` prefix and the extension.
pub fn entry_title(link: &Path) -> String {
    let stem = link.file_stem().unwrap_or_default().to_string_lossy();
    match stem.split_once(" - ") {
        Some((n, rest)) if !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()) => rest.into(),
        _ => stem.into_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::tests::write_tagged_wav;

    fn setup() -> (tempfile::TempDir, PathBuf, Vec<PathBuf>) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        fs::create_dir_all(root.join("House")).unwrap();
        let tracks: Vec<PathBuf> = ["a", "b", "c"]
            .iter()
            .map(|n| {
                let p = root.join(format!("House/{n}.wav"));
                write_tagged_wav(&p, n);
                p
            })
            .collect();
        (dir, root, tracks)
    }

    fn targets(root: &Path, name: &str) -> Vec<PathBuf> {
        get(root, name).unwrap().entries.into_iter().map(|e| e.target).collect()
    }

    fn link_names(root: &Path, name: &str) -> Vec<String> {
        get(root, name)
            .unwrap()
            .entries
            .iter()
            .map(|e| e.link.file_name().unwrap().to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn create_add_list() {
        let (_d, root, t) = setup();
        assert!(list(&root).unwrap().is_empty());
        create(&root, "Warmup").unwrap();
        assert!(add(&root, "Warmup", &t[1]).unwrap());
        assert!(add(&root, "Warmup", &t[0]).unwrap());
        assert!(!add(&root, "Warmup", &t[0]).unwrap(), "duplicates are ignored");

        assert_eq!(targets(&root, "Warmup"), [t[1].clone(), t[0].clone()]);
        assert_eq!(link_names(&root, "Warmup"), ["001 - b.wav", "002 - a.wav"]);
        // Links are relative and point at the real file.
        let link = root.join("Playlists/Warmup/001 - b.wav");
        assert_eq!(fs::read_link(&link).unwrap(), Path::new("../../House/b.wav"));
        assert_eq!(fs::read(&link).unwrap(), fs::read(&t[1]).unwrap());

        let all = list(&root).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].name, "Warmup");
    }

    #[test]
    fn remove_renumbers() {
        let (_d, root, t) = setup();
        create(&root, "P").unwrap();
        for tr in &t {
            add(&root, "P", tr).unwrap();
        }
        assert!(remove(&root, "P", &t[0]).unwrap());
        assert!(!remove(&root, "P", &t[0]).unwrap());
        assert_eq!(link_names(&root, "P"), ["001 - b.wav", "002 - c.wav"]);
        assert!(t[0].exists(), "removing from a playlist never deletes the track");
    }

    #[test]
    fn reorder_playlist() {
        let (_d, root, t) = setup();
        create(&root, "P").unwrap();
        for tr in &t {
            add(&root, "P", tr).unwrap();
        }
        reorder(&root, "P", &[t[2].clone(), t[0].clone()]).unwrap();
        assert_eq!(targets(&root, "P"), [t[2].clone(), t[0].clone(), t[1].clone()]);
        assert_eq!(link_names(&root, "P"), ["001 - c.wav", "002 - a.wav", "003 - b.wav"]);
    }

    #[test]
    fn rename_keeps_links_working_and_delete_keeps_tracks() {
        let (_d, root, t) = setup();
        create(&root, "Old").unwrap();
        add(&root, "Old", &t[0]).unwrap();
        create(&root, "Taken").unwrap();
        assert!(rename(&root, "Old", "Taken").is_err());

        rename(&root, "Old", "New").unwrap();
        let pl = get(&root, "New").unwrap();
        assert!(!pl.entries[0].missing);

        delete(&root, "New").unwrap();
        assert!(!root.join("Playlists/New").exists());
        assert!(t[0].exists());
    }

    #[test]
    fn delete_refuses_real_files() {
        let (_d, root, t) = setup();
        create(&root, "P").unwrap();
        fs::copy(&t[0], root.join("Playlists/P/copy.wav")).unwrap();
        assert!(delete(&root, "P").is_err());
        assert!(root.join("Playlists/P/copy.wav").exists());
    }

    #[test]
    fn missing_targets_and_hand_made_links() {
        let (_d, root, t) = setup();
        create(&root, "P").unwrap();
        add(&root, "P", &t[0]).unwrap();
        // A link made in Finder/terminal without a number prefix sorts after numbered ones.
        symlink("../../House/c.wav", root.join("Playlists/P/c.wav")).unwrap();
        fs::remove_file(&t[0]).unwrap();

        let pl = get(&root, "P").unwrap();
        assert_eq!(pl.entries.len(), 2);
        assert!(pl.entries[0].missing);
        assert_eq!(pl.entries[0].target, t[0]);
        assert!(!pl.entries[1].missing);
        assert_eq!(pl.entries[1].target, t[2]);
    }

    #[test]
    fn names_are_validated() {
        let (_d, root, _t) = setup();
        for bad in ["", " ", ".hidden", "a/b", "a:b", " pad"] {
            assert!(create(&root, bad).is_err(), "{bad:?} should be rejected");
        }
        create(&root, "Peak Time 2026").unwrap();
        assert!(create(&root, "Peak Time 2026").is_err());
    }

    #[test]
    fn titles() {
        assert_eq!(entry_title(Path::new("001 - Night Drive.wav")), "Night Drive");
        assert_eq!(entry_title(Path::new("Artist - Title.mp3")), "Artist - Title");
        assert_eq!(entry_title(Path::new("plain.mp3")), "plain");
    }
}
