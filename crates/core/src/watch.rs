//! Watches a library root and reports changed paths in debounced batches.

use std::path::{Path, PathBuf};
use std::time::Duration;

use notify::RecursiveMode;
use notify_debouncer_full::{new_debouncer, DebounceEventResult, Debouncer, RecommendedCache};

/// Keeps watching until dropped.
pub struct Watcher {
    _debouncer: Debouncer<notify::RecommendedWatcher, RecommendedCache>,
}

/// Calls `on_change` with the paths touched in each batch of events (after 500ms of quiet,
/// so copying a folder of tracks arrives as one batch). If the OS reports that events were
/// lost, the batch contains `root` itself, which means "rescan everything".
/// Feed the batch to [`crate::changes_for`].
pub fn watch(
    root: &Path,
    mut on_change: impl FnMut(Vec<PathBuf>) + Send + 'static,
) -> notify::Result<Watcher> {
    let root_owned = root.to_path_buf();
    let mut debouncer = new_debouncer(
        Duration::from_millis(500),
        None,
        move |res: DebounceEventResult| {
            let paths = match res {
                Ok(events) => {
                    if events.iter().any(|e| e.need_rescan()) {
                        vec![root_owned.clone()]
                    } else {
                        events.into_iter().flat_map(|e| e.event.paths).collect()
                    }
                }
                // Watcher errors: be safe and rescan.
                Err(_) => vec![root_owned.clone()],
            };
            if !paths.is_empty() {
                on_change(paths);
            }
        },
    )?;
    debouncer.watch(root, RecursiveMode::Recursive)?;
    Ok(Watcher { _debouncer: debouncer })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn reports_new_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let (tx, rx) = mpsc::channel();
        let _w = watch(&root, move |paths| {
            let _ = tx.send(paths);
        })
        .unwrap();

        // Give FSEvents a moment to start before touching the folder.
        std::thread::sleep(Duration::from_millis(300));
        let file = root.join("new.mp3");
        std::fs::write(&file, b"x").unwrap();

        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            let paths = rx.recv_timeout(left).expect("no watch event within 10s");
            if paths.iter().any(|p| p == &file || p == &root) {
                break;
            }
        }
    }
}
