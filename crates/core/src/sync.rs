//! Copying a library to a USB drive for the player, without disturbing anything else on it.
//!
//! Rules:
//! - TrackMap records every file it writes in `<drive>/.trackmap-sync.json` (the manifest).
//!   Only those files are ever updated or removed. Anything else on the drive (an existing
//!   Rekordbox export, files copied by hand) is never modified or deleted.
//! - Tracks go to `Contents/<Artist>/<Album>/<file>`, the Rekordbox layout the player expects.
//!   If an identical file is already there (e.g. Rekordbox exported the same track), it is
//!   reused and left unowned. If a *different* file is in the way, the track gets another name.
//! - Files are copied to a temporary name and renamed when complete, and the manifest is saved
//!   after each file, so an unplugged drive never ends up with half-written tracks and a sync
//!   can resume.
//! - Removing tracks that left the library is opt-in, and only touches owned files.
//! - Bytes are copied without extended attributes, so macOS doesn't litter FAT/exFAT drives
//!   with `._` files.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};

use crate::{Library, Playlist, Track};

pub const MANIFEST: &str = ".trackmap-sync.json";
const TMP_SUFFIX: &str = ".trackmap-tmp";
/// FAT32 can't hold files of 4 GiB or more.
const FAT32_MAX: u64 = 4 * 1024 * 1024 * 1024 - 1;

/// Which tracks to put on the drive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Scope {
    #[default]
    WholeLibrary,
    PlaylistsOnly,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Owned {
    /// Source track, relative to the library root.
    pub source: String,
    /// Size of the copy on the drive.
    pub size: u64,
    /// The source's size and modification time when copied, to spot changes.
    pub source_size: u64,
    pub source_modified: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub version: u32,
    /// Files TrackMap wrote, by path relative to the drive root (with `/` separators).
    pub files: BTreeMap<String, Owned>,
}

impl Manifest {
    pub fn load(drive: &Path) -> Self {
        fs::read(drive.join(MANIFEST))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or(Self { version: 1, files: BTreeMap::new() })
    }

    pub fn save(&self, drive: &Path) -> io::Result<()> {
        let path = drive.join(MANIFEST);
        let tmp = drive.join(format!("{MANIFEST}{TMP_SUFFIX}"));
        let mut f = fs::File::create(&tmp)?;
        f.write_all(&serde_json::to_vec_pretty(self)?)?;
        f.sync_all()?;
        fs::rename(tmp, path)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Copy a track (new, or an owned copy whose source changed).
    /// `rel` is the source relative to the library root, as recorded in the manifest.
    Copy { source: PathBuf, rel: String, dest: String, size: u64, update: bool },
    /// Write a playlist file.
    Playlist { name: String, dest: String, contents: Vec<u8> },
    /// Delete an owned file whose track is no longer wanted (only if the user opts in).
    Remove { dest: String },
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Plan {
    pub actions: Vec<Action>,
    /// Owned copies that are already up to date.
    pub up_to_date: usize,
    /// Tracks already on the drive as identical files TrackMap didn't write (e.g. Rekordbox's).
    pub reused: usize,
    /// Tracks given another name because a different file was in the way.
    pub renamed: usize,
    /// Tracks that can't be synced, with the reason.
    pub skipped: Vec<(PathBuf, String)>,
    /// Bytes to copy (tracks only).
    pub bytes: u64,
    /// Free space on the drive, if known.
    pub free: Option<u64>,
}

impl Plan {
    pub fn copies(&self) -> usize {
        self.actions.iter().filter(|a| matches!(a, Action::Copy { update: false, .. })).count()
    }
    pub fn updates(&self) -> usize {
        self.actions.iter().filter(|a| matches!(a, Action::Copy { update: true, .. })).count()
    }
    pub fn removals(&self) -> usize {
        self.actions.iter().filter(|a| matches!(a, Action::Remove { .. })).count()
    }
    pub fn playlists(&self) -> usize {
        self.actions.iter().filter(|a| matches!(a, Action::Playlist { .. })).count()
    }
    /// Whether there's room for the copies (allowing for files being replaced).
    pub fn fits(&self) -> bool {
        self.free.is_none_or(|f| f > self.bytes + 16 * 1024 * 1024)
    }
}

/// A path component made safe for FAT/exFAT: no reserved characters, no trailing dots or
/// spaces, not empty, and not too long.
pub fn sanitize(name: &str) -> String {
    let mut s: String = name
        .chars()
        .map(|c| if c.is_control() || r#""*/:<>?\|"#.contains(c) { '_' } else { c })
        .collect();
    while s.ends_with('.') || s.ends_with(' ') {
        s.pop();
    }
    let s = s.trim_start().to_string();
    let mut s = if s.is_empty() { "_".to_string() } else { s };
    if s.chars().count() > 120 {
        s = s.chars().take(120).collect::<String>().trim_end().to_string();
    }
    s
}

/// Where a track goes on the drive: `Contents/<Artist>/<Album>/<file>`.
pub fn dest_for(track: &Track) -> String {
    let artist = track.artist.as_deref().map(str::trim).filter(|s| !s.is_empty()).unwrap_or("UnknownArtist");
    let album = track.album.as_deref().map(str::trim).filter(|s| !s.is_empty()).unwrap_or("UnknownAlbum");
    let file = track.path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
    format!("Contents/{}/{}/{}", sanitize(artist), sanitize(album), sanitize(&file))
}

/// `name (2).ext`, `name (3).ext`, … for a path that's taken.
fn numbered(dest: &str, n: usize) -> String {
    let (dir, file) = dest.rsplit_once('/').unwrap_or(("", dest));
    let (stem, ext) = match file.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s, format!(".{e}")),
        _ => (file, String::new()),
    };
    let name = format!("{stem} ({n}){ext}");
    if dir.is_empty() { name } else { format!("{dir}/{name}") }
}

fn on_drive(drive: &Path, rel: &str) -> PathBuf {
    rel.split('/').fold(drive.to_path_buf(), |p, c| p.join(c))
}

/// Whether two files are the same: equal size and equal first and last 64 KiB.
fn same_file(a: &Path, b: &Path) -> bool {
    let (Ok(ma), Ok(mb)) = (fs::metadata(a), fs::metadata(b)) else { return false };
    if ma.len() != mb.len() {
        return false;
    }
    let read = |p: &Path, from_end: bool| -> Option<Vec<u8>> {
        use std::io::{Seek, SeekFrom};
        let mut f = fs::File::open(p).ok()?;
        let len = f.metadata().ok()?.len();
        let n = len.min(64 * 1024);
        if from_end {
            f.seek(SeekFrom::Start(len - n)).ok()?;
        }
        let mut buf = vec![0; n as usize];
        f.read_exact(&mut buf).ok()?;
        Some(buf)
    };
    read(a, false).is_some_and(|x| read(b, false) == Some(x)) && read(a, true).is_some_and(|x| read(b, true) == Some(x))
}

/// Works out what a sync would do. Reads the drive but changes nothing.
pub fn plan(lib: &Library, playlists: &[Playlist], scope: Scope, drive: &Path, remove_unwanted: bool) -> Plan {
    let manifest = Manifest::load(drive);
    let fat32 = is_fat32(drive);
    let mut plan = Plan { free: free_space(drive), ..Default::default() };

    let in_playlists: HashSet<&PathBuf> = playlists.iter().flat_map(|p| p.entries.iter().map(|e| &e.target)).collect();
    let wanted: Vec<&Track> = lib
        .tracks
        .iter()
        .filter(|t| scope == Scope::WholeLibrary || in_playlists.contains(&t.path))
        .collect();

    // Owned files by source, so a track keeps its spot (even if it was renamed earlier).
    let owned_by_source: HashMap<&str, &str> =
        manifest.files.iter().map(|(dest, o)| (o.source.as_str(), dest.as_str())).collect();
    // Destinations claimed in this plan, compared case-insensitively (FAT and exFAT are).
    let mut claimed: HashSet<String> = HashSet::new();
    let mut kept_owned: HashSet<String> = HashSet::new();
    let mut synced_dest: HashMap<PathBuf, String> = HashMap::new();

    for t in wanted {
        let rel = t.path.strip_prefix(&lib.root).unwrap_or(&t.path).to_string_lossy().replace('\\', "/");
        if fat32 && t.size > FAT32_MAX {
            plan.skipped.push((t.path.clone(), "larger than 4 GB (FAT32 limit)".into()));
            continue;
        }
        // Already ours from an earlier sync?
        if let Some(&dest) = owned_by_source.get(rel.as_str()) {
            let o = &manifest.files[dest];
            claimed.insert(dest.to_lowercase());
            kept_owned.insert(dest.to_string());
            synced_dest.insert(t.path.clone(), dest.to_string());
            let on = on_drive(drive, dest);
            let current = fs::metadata(&on).map(|m| m.len()).ok() == Some(o.size)
                && o.source_size == t.size
                && o.source_modified == t.modified;
            if current {
                plan.up_to_date += 1;
            } else {
                plan.bytes += t.size;
                plan.actions.push(Action::Copy {
                    source: t.path.clone(),
                    rel: rel.clone(),
                    dest: dest.to_string(),
                    size: t.size,
                    update: true,
                });
            }
            continue;
        }
        // Find a free spot, reusing an identical file someone else put there.
        let base = dest_for(t);
        let mut n = 1;
        loop {
            let dest = if n == 1 { base.clone() } else { numbered(&base, n) };
            n += 1;
            if claimed.contains(&dest.to_lowercase()) {
                continue;
            }
            let on = on_drive(drive, &dest);
            let owned = manifest.files.contains_key(&dest);
            if on.exists() && !owned {
                if same_file(&t.path, &on) {
                    claimed.insert(dest.to_lowercase());
                    synced_dest.insert(t.path.clone(), dest);
                    plan.reused += 1;
                    break;
                }
                continue; // someone else's different file: never touch it
            }
            if owned {
                continue; // owned by another source; leave its slot alone
            }
            if n > 2 {
                plan.renamed += 1;
            }
            claimed.insert(dest.to_lowercase());
            synced_dest.insert(t.path.clone(), dest.clone());
            plan.bytes += t.size;
            plan.actions.push(Action::Copy { source: t.path.clone(), rel: rel.clone(), dest, size: t.size, update: false });
            break;
        }
    }

    // Playlists: one text file each, owned like tracks.
    let tracks_by_path: HashMap<&PathBuf, &Track> = lib.tracks.iter().map(|t| (&t.path, t)).collect();
    for pl in playlists {
        let rows: Vec<&Track> = pl
            .entries
            .iter()
            .filter(|e| synced_dest.contains_key(&e.target))
            .filter_map(|e| tracks_by_path.get(&e.target).copied())
            .collect();
        let base = format!("Playlists/{}.txt", sanitize(&pl.name));
        let mut n = 1;
        let dest = loop {
            let d = if n == 1 { base.clone() } else { numbered(&base, n) };
            n += 1;
            let on = on_drive(drive, &d);
            if (!on.exists() || manifest.files.contains_key(&d)) && !claimed.contains(&d.to_lowercase()) {
                break d;
            }
        };
        claimed.insert(dest.to_lowercase());
        kept_owned.insert(dest.clone());
        plan.actions.push(Action::Playlist { name: pl.name.clone(), dest, contents: rekordbox_txt(&rows) });
    }

    if remove_unwanted {
        for dest in manifest.files.keys() {
            if !kept_owned.contains(dest) {
                plan.actions.push(Action::Remove { dest: dest.clone() });
            }
        }
    }
    plan
}

/// A playlist in the text format Rekordbox exports ("Export a playlist to a file"): UTF-16LE
/// with a byte-order mark, tab-separated, one header row.
///
/// NOTE: the exact columns the player needs are unverified until we compare with a real export;
/// this is the only place to change when we do.
pub fn rekordbox_txt(tracks: &[&Track]) -> Vec<u8> {
    let mut text = String::from("#\tTrack Title\tArtist\tAlbum\tGenre\tBPM\tKey\r\n");
    for (i, t) in tracks.iter().enumerate() {
        let clean = |s: &Option<String>| s.as_deref().unwrap_or("").replace(['\t', '\r', '\n'], " ");
        text += &format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\r\n",
            i + 1,
            clean(&t.title),
            clean(&t.artist),
            clean(&t.album),
            clean(&t.genre),
            t.tempo().map(|b| format!("{b:.2}")).unwrap_or_default(),
            t.best_key().map(|k| k.musical_name()).unwrap_or_default(),
        );
    }
    let mut out = vec![0xFF, 0xFE];
    for u in text.encode_utf16() {
        out.extend_from_slice(&u.to_le_bytes());
    }
    out
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Report {
    pub copied: usize,
    pub updated: usize,
    pub playlists: usize,
    pub removed: usize,
    pub bytes: u64,
    pub errors: Vec<(String, String)>,
    pub cancelled: bool,
}

/// Progress: (actions done, actions total, bytes done).
pub type Progress<'a> = &'a mut dyn FnMut(usize, usize, u64);

/// Carries out a plan. Stops cleanly between files when `cancel` is set.
pub fn execute(plan: &Plan, drive: &Path, cancel: &AtomicBool, progress: Progress<'_>) -> Report {
    let mut manifest = Manifest::load(drive);
    let mut report = Report::default();
    let total = plan.actions.len();
    for (i, action) in plan.actions.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            report.cancelled = true;
            break;
        }
        let result = match action {
            Action::Copy { source, rel, dest, update, .. } => copy_file(source, &on_drive(drive, dest)).map(|size| {
                let meta = fs::metadata(source).ok();
                manifest.files.insert(
                    dest.clone(),
                    Owned {
                        source: rel.clone(),
                        size,
                        source_size: meta.as_ref().map_or(0, |m| m.len()),
                        source_modified: meta.and_then(|m| m.modified().ok()).and_then(|t| {
                            t.duration_since(std::time::UNIX_EPOCH).ok().map(|d| d.as_secs())
                        }),
                    },
                );
                report.bytes += size;
                if *update { report.updated += 1 } else { report.copied += 1 }
            }),
            Action::Playlist { dest, contents, .. } => write_atomic(&on_drive(drive, dest), contents).map(|_| {
                manifest.files.insert(
                    dest.clone(),
                    Owned { source: String::new(), size: contents.len() as u64, ..Default::default() },
                );
                report.playlists += 1;
            }),
            Action::Remove { dest } => {
                // Only ever files we wrote.
                if manifest.files.contains_key(dest) {
                    let p = on_drive(drive, dest);
                    let r = match fs::remove_file(&p) {
                        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
                        _ => Ok(()),
                    };
                    r.map(|_| {
                        manifest.files.remove(dest);
                        remove_empty_dirs(drive, p.parent());
                        report.removed += 1;
                    })
                } else {
                    Ok(())
                }
            }
        };
        if let Err(e) = result {
            let dest = match action {
                Action::Copy { dest, .. } | Action::Playlist { dest, .. } | Action::Remove { dest } => dest.clone(),
            };
            report.errors.push((dest, e.to_string()));
        }
        // Save after every file so an interrupted sync knows what's on the drive.
        if let Err(e) = manifest.save(drive) {
            report.errors.push((MANIFEST.into(), e.to_string()));
        }
        progress(i + 1, total, report.bytes);
    }
    report
}

/// Copies bytes only (no extended attributes, so no `._` files on FAT/exFAT) to a temporary
/// file, syncs it, and renames it into place. Returns the size written.
fn copy_file(from: &Path, to: &Path) -> io::Result<u64> {
    if let Some(dir) = to.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = tmp_path(to);
    let result = (|| {
        let mut src = fs::File::open(from)?;
        let mut dst = fs::File::create(&tmp)?;
        let n = io::copy(&mut src, &mut dst)?;
        dst.sync_all()?;
        drop(dst);
        fs::rename(&tmp, to)?;
        Ok(n)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

fn write_atomic(to: &Path, contents: &[u8]) -> io::Result<()> {
    if let Some(dir) = to.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = tmp_path(to);
    let mut f = fs::File::create(&tmp)?;
    f.write_all(contents)?;
    f.sync_all()?;
    drop(f);
    fs::rename(tmp, to)
}

fn tmp_path(to: &Path) -> PathBuf {
    let mut name = to.file_name().unwrap_or_default().to_os_string();
    name.push(TMP_SUFFIX);
    to.with_file_name(name)
}

/// Removes now-empty folders up to (not including) the drive root.
fn remove_empty_dirs(drive: &Path, mut dir: Option<&Path>) {
    while let Some(d) = dir {
        if d == drive || !d.starts_with(drive) || fs::remove_dir(d).is_err() {
            break;
        }
        dir = d.parent();
    }
}

/// Free bytes on the volume holding `path`.
pub fn free_space(path: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c` is a valid C string and `s` a writable statvfs.
    if unsafe { libc::statvfs(c.as_ptr(), &mut s) } != 0 {
        return None;
    }
    Some(s.f_bavail as u64 * s.f_frsize as u64)
}

/// Whether the volume holding `path` is FAT (FAT32's 4 GB file limit applies).
#[cfg(target_os = "macos")]
pub fn is_fat32(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c) = std::ffi::CString::new(path.as_os_str().as_bytes()) else { return false };
    let mut s: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c` is a valid C string and `s` a writable statfs.
    if unsafe { libc::statfs(c.as_ptr(), &mut s) } != 0 {
        return false;
    }
    let name: Vec<u8> = s.f_fstypename.iter().take_while(|c| **c != 0).map(|c| *c as u8).collect();
    name == b"msdos"
}

#[cfg(not(target_os = "macos"))]
pub fn is_fat32(_path: &Path) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::playlists::Entry;

    struct Setup {
        _dirs: (tempfile::TempDir, tempfile::TempDir),
        lib: Library,
        drive: PathBuf,
    }

    fn track(root: &Path, rel: &str, artist: &str, album: &str, bytes: &[u8]) -> Track {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, bytes).unwrap();
        let m = fs::metadata(&p).unwrap();
        Track {
            path: p,
            title: Some(rel.into()),
            artist: Some(artist.into()).filter(|s: &String| !s.is_empty()),
            album: Some(album.into()).filter(|s: &String| !s.is_empty()),
            size: m.len(),
            modified: m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs()),
            ..Default::default()
        }
    }

    fn setup() -> Setup {
        let (l, d) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let root = l.path().canonicalize().unwrap();
        let tracks = vec![
            track(&root, "a.mp3", "Ann", "EP", b"aaaa"),
            track(&root, "House/b.mp3", "Bob", "", b"bbbbbb"),
            track(&root, "c.mp3", "", "", b"cc"),
        ];
        let drive = d.path().canonicalize().unwrap();
        Setup { lib: Library { root, tracks }, drive, _dirs: (l, d) }
    }

    fn run(s: &Setup, pls: &[Playlist], scope: Scope, remove: bool) -> (Plan, Report) {
        let plan = plan(&s.lib, pls, scope, &s.drive, remove);
        let report = execute(&plan, &s.drive, &AtomicBool::new(false), &mut |_, _, _| {});
        (plan, report)
    }

    fn files_on(drive: &Path) -> Vec<String> {
        let mut v: Vec<String> = walkdir::WalkDir::new(drive)
            .into_iter()
            .flatten()
            .filter(|e| e.file_type().is_file())
            .map(|e| e.path().strip_prefix(drive).unwrap().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    #[test]
    fn first_sync_copies_into_rekordbox_layout_and_second_does_nothing() {
        let s = setup();
        let (plan, report) = run(&s, &[], Scope::WholeLibrary, false);
        assert_eq!((plan.copies(), report.copied, report.errors.len()), (3, 3, 0));
        assert_eq!(
            files_on(&s.drive),
            [
                MANIFEST,
                "Contents/Ann/EP/a.mp3",
                "Contents/Bob/UnknownAlbum/b.mp3",
                "Contents/UnknownArtist/UnknownAlbum/c.mp3",
            ]
        );
        assert_eq!(fs::read(s.drive.join("Contents/Ann/EP/a.mp3")).unwrap(), b"aaaa");

        let (again, _) = run(&s, &[], Scope::WholeLibrary, false);
        assert_eq!((again.actions.len(), again.up_to_date), (0, 3));
    }

    #[test]
    fn never_touches_other_files_on_the_drive() {
        let s = setup();
        // An existing Rekordbox export: the same a.mp3, a different file where b.mp3 would go,
        // and unrelated stuff.
        let put = |rel: &str, data: &[u8]| {
            let p = s.drive.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, data).unwrap();
        };
        put("Contents/Ann/EP/a.mp3", b"aaaa");
        put("Contents/Bob/UnknownAlbum/b.mp3", b"someone else's");
        put("PIONEER/rekordbox/export.pdb", b"db");

        let (plan, report) = run(&s, &[], Scope::WholeLibrary, true);
        assert_eq!(plan.reused, 1, "identical a.mp3 reused");
        assert_eq!(plan.renamed, 1, "b.mp3 goes beside the other file");
        assert!(report.errors.is_empty());
        assert_eq!(fs::read(s.drive.join("Contents/Bob/UnknownAlbum/b.mp3")).unwrap(), b"someone else's");
        assert_eq!(fs::read(s.drive.join("Contents/Bob/UnknownAlbum/b (2).mp3")).unwrap(), b"bbbbbb");
        assert_eq!(fs::read(s.drive.join("PIONEER/rekordbox/export.pdb")).unwrap(), b"db");

        // Even with removal on and the library emptied, foreign files stay.
        let mut empty = Setup { lib: Library { root: s.lib.root.clone(), tracks: vec![] }, drive: s.drive.clone(), _dirs: (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap()) };
        empty._dirs = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let (plan, report) = run(&empty, &[], Scope::WholeLibrary, true);
        assert_eq!((plan.removals(), report.removed), (2, 2), "only TrackMap's own copies (b and c)");
        assert!(s.drive.join("Contents/Ann/EP/a.mp3").exists(), "reused file is not ours to delete");
        assert!(s.drive.join("Contents/Bob/UnknownAlbum/b.mp3").exists());
        assert!(s.drive.join("PIONEER/rekordbox/export.pdb").exists());
        assert!(!s.drive.join("Contents/UnknownArtist").exists(), "emptied folders are tidied");
    }

    #[test]
    fn changed_sources_are_updated_and_removal_is_opt_in() {
        let mut s = setup();
        run(&s, &[], Scope::WholeLibrary, false);
        // Change a.mp3 and drop c.mp3 from the library.
        s.lib.tracks[0] = track(&s.lib.root, "a.mp3", "Ann", "EP", b"aaaa-new-master");
        s.lib.tracks[0].modified = s.lib.tracks[0].modified.map(|m| m + 10);
        s.lib.tracks.pop();

        let plan = plan(&s.lib, &[], Scope::WholeLibrary, &s.drive, false);
        assert_eq!((plan.updates(), plan.removals()), (1, 0));
        execute(&plan, &s.drive, &AtomicBool::new(false), &mut |_, _, _| {});
        assert_eq!(fs::read(s.drive.join("Contents/Ann/EP/a.mp3")).unwrap(), b"aaaa-new-master");
        assert!(s.drive.join("Contents/UnknownArtist/UnknownAlbum/c.mp3").exists(), "kept without opt-in");

        let (plan, _) = run(&s, &[], Scope::WholeLibrary, true);
        assert_eq!(plan.removals(), 1);
        assert!(!s.drive.join("Contents/UnknownArtist/UnknownAlbum/c.mp3").exists());
    }

    #[test]
    fn playlists_scope_and_playlist_files() {
        let s = setup();
        let b = s.lib.tracks[1].path.clone();
        let pls = vec![Playlist {
            name: "Peak: Time".into(),
            entries: vec![Entry { link: "x".into(), target: b.clone(), missing: false }],
        }];
        let (plan, report) = run(&s, &pls, Scope::PlaylistsOnly, false);
        assert_eq!((plan.copies(), report.playlists), (1, 1));
        assert!(s.drive.join("Contents/Bob/UnknownAlbum/b.mp3").exists());
        assert!(!s.drive.join("Contents/Ann").exists(), "not in a playlist");

        let txt = fs::read(s.drive.join("Playlists/Peak_ Time.txt")).unwrap();
        assert_eq!(&txt[..2], &[0xFF, 0xFE], "UTF-16LE with BOM");
        let words: Vec<u16> = txt[2..].chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        let text = String::from_utf16(&words).unwrap();
        assert!(text.starts_with("#\tTrack Title\tArtist"));
        assert!(text.contains("1\tHouse/b.mp3\tBob"));

        // A hand-made playlist file with the same name is left alone.
        let s2 = setup();
        fs::create_dir_all(s2.drive.join("Playlists")).unwrap();
        fs::write(s2.drive.join("Playlists/Peak_ Time.txt"), b"mine").unwrap();
        let (plan, _) = run(&s2, &pls, Scope::WholeLibrary, false);
        assert!(plan.actions.iter().any(|a| matches!(a, Action::Playlist { dest, .. } if dest == "Playlists/Peak_ Time (2).txt")));
        assert_eq!(fs::read(s2.drive.join("Playlists/Peak_ Time.txt")).unwrap(), b"mine");
    }

    #[test]
    fn cancelling_leaves_a_consistent_drive() {
        let s = setup();
        let plan = plan(&s.lib, &[], Scope::WholeLibrary, &s.drive, false);
        let cancel = AtomicBool::new(false);
        let report = execute(&plan, &s.drive, &cancel, &mut |done, _, _| {
            if done == 1 {
                cancel.store(true, Ordering::Relaxed);
            }
        });
        assert!(report.cancelled);
        assert_eq!(report.copied, 1);
        assert_eq!(Manifest::load(&s.drive).files.len(), 1, "manifest matches what was copied");
        assert!(!files_on(&s.drive).iter().any(|f| f.ends_with(TMP_SUFFIX)));
        // Resuming copies only the rest.
        let rest = super::plan(&s.lib, &[], Scope::WholeLibrary, &s.drive, false);
        assert_eq!((rest.copies(), rest.up_to_date), (2, 1));
    }

    #[test]
    fn names_are_safe_for_fat_and_case_collisions_are_avoided() {
        assert_eq!(sanitize("AC/DC: Live?"), "AC_DC_ Live_");
        assert_eq!(sanitize("trailing. "), "trailing");
        assert_eq!(sanitize(""), "_");
        assert_eq!(numbered("Contents/A/B/x.mp3", 2), "Contents/A/B/x (2).mp3");

        let (l, d) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let root = l.path().canonicalize().unwrap();
        let tracks = vec![
            track(&root, "1/Song.mp3", "Ann", "EP", b"1"),
            track(&root, "2/song.mp3", "ann", "ep", b"2"),
        ];
        let lib = Library { root, tracks };
        let plan = plan(&lib, &[], Scope::WholeLibrary, d.path(), false);
        let dests: Vec<&str> = plan.actions.iter().filter_map(|a| match a {
            Action::Copy { dest, .. } => Some(dest.as_str()),
            _ => None,
        }).collect();
        assert_eq!(dests.len(), 2);
        assert_ne!(dests[0].to_lowercase(), dests[1].to_lowercase());
    }

    #[test]
    fn reports_free_space() {
        let d = tempfile::tempdir().unwrap();
        assert!(free_space(d.path()).unwrap() > 0);
    }
}
