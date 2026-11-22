//! The "Sync to USB" dialog: pick a drive, preview what will happen, then copy.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

use gpui::{div, prelude::*, px, AnyElement, Context, PathPromptOptions, SharedString};
use trackmap_core::settings::Settings;
use trackmap_core::sync::{self, Plan, Report, Scope};

use crate::theme::Theme;
use crate::{settings_path, LibraryState, Workspace};

pub struct SyncDialog {
    pub drive: Option<PathBuf>,
    pub scope: Scope,
    pub remove: bool,
    pub plan: Option<Arc<Plan>>,
    pub planning: bool,
    pub running: Option<Running>,
    pub report: Option<Report>,
    /// Bumped whenever the options change, so an outdated plan is ignored.
    pub version: u64,
}

pub struct Running {
    pub done: Arc<AtomicUsize>,
    pub total: usize,
    pub bytes: Arc<AtomicU64>,
    pub total_bytes: u64,
    pub cancel: Arc<AtomicBool>,
}

/// "1.8 GB", "340 MB", "12 KB".
pub fn human_bytes(b: u64) -> String {
    let b = b as f64;
    if b >= 1e9 {
        format!("{:.1} GB", b / 1e9)
    } else if b >= 1e6 {
        format!("{:.0} MB", b / 1e6)
    } else {
        format!("{:.0} KB", (b / 1e3).max(1.0))
    }
}

impl Workspace {
    pub fn open_sync(&mut self, cx: &mut Context<Self>) {
        let LibraryState::Loaded(lib) = &self.library else { return };
        let settings = settings_path().map(|p| Settings::load(&p)).unwrap_or_default();
        let drive = settings.sync_targets.get(&lib.root).cloned().filter(|d| d.is_dir());
        self.sync = Some(SyncDialog {
            drive,
            scope: settings.sync_scope,
            remove: false,
            plan: None,
            planning: false,
            running: None,
            report: None,
            version: 0,
        });
        self.replan(cx);
    }

    fn remember_sync(&self) {
        let (Some(d), LibraryState::Loaded(lib), Some(path)) = (&self.sync, &self.library, settings_path()) else {
            return;
        };
        let mut s = Settings::load(&path);
        if let Some(drive) = &d.drive {
            s.sync_targets.insert(lib.root.clone(), drive.clone());
        }
        s.sync_scope = d.scope;
        let _ = s.save(&path);
    }

    /// Recomputes the preview in the background.
    pub fn replan(&mut self, cx: &mut Context<Self>) {
        let (Some(d), LibraryState::Loaded(lib)) = (&mut self.sync, &self.library) else { return };
        d.version += 1;
        d.plan = None;
        let Some(drive) = d.drive.clone() else { return };
        d.planning = true;
        let (version, scope, remove) = (d.version, d.scope, d.remove);
        let (lib, pls) = (lib.clone(), self.playlists.clone());
        cx.notify();
        cx.spawn(async move |this, cx| {
            let plan = cx
                .background_executor()
                .spawn(async move { sync::plan(&lib, &pls, scope, &drive, remove) })
                .await;
            this.update(cx, |ws, cx| {
                if let Some(d) = &mut ws.sync {
                    if d.version == version {
                        d.plan = Some(Arc::new(plan));
                        d.planning = false;
                        cx.notify();
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    fn choose_drive(&mut self, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Sync to this drive".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(mut paths))) = picked.await else { return };
            let Some(drive) = paths.pop() else { return };
            this.update(cx, |ws, cx| {
                if let Some(d) = &mut ws.sync {
                    d.drive = Some(drive);
                    d.report = None;
                }
                ws.remember_sync();
                ws.replan(cx);
            })
            .ok();
        })
        .detach();
    }

    fn run_sync(&mut self, cx: &mut Context<Self>) {
        let Some(d) = &mut self.sync else { return };
        let (Some(plan), Some(drive)) = (d.plan.clone(), d.drive.clone()) else { return };
        if d.running.is_some() || plan.actions.is_empty() {
            return;
        }
        let run = Running {
            done: Arc::new(AtomicUsize::new(0)),
            total: plan.actions.len(),
            bytes: Arc::new(AtomicU64::new(0)),
            total_bytes: plan.bytes,
            cancel: Arc::new(AtomicBool::new(false)),
        };
        let (done, bytes, cancel) = (run.done.clone(), run.bytes.clone(), run.cancel.clone());
        d.running = Some(run);
        d.report = None;
        self.remember_sync();
        cx.notify();

        // Refresh the progress display while copying.
        cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(std::time::Duration::from_millis(200)).await;
            let running = this.update(cx, |ws, cx| {
                cx.notify();
                ws.sync.as_ref().is_some_and(|d| d.running.is_some())
            });
            if !matches!(running, Ok(true)) {
                break;
            }
        })
        .detach();

        cx.spawn(async move |this, cx| {
            let report = cx
                .background_executor()
                .spawn(async move {
                    sync::execute(&plan, &drive, &cancel, &mut |n, _, b| {
                        done.store(n, Ordering::Relaxed);
                        bytes.store(b, Ordering::Relaxed);
                    })
                })
                .await;
            this.update(cx, |ws, cx| {
                if let Some(d) = &mut ws.sync {
                    d.running = None;
                    d.report = Some(report);
                }
                ws.replan(cx);
            })
            .ok();
        })
        .detach();
    }

    pub fn sync_view(&self, t: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let d = self.sync.as_ref()?;
        let t = *t;
        let line = |text: String| div().text_color(t.text).child(text);
        let muted = |text: String| div().text_xs().text_color(t.text_muted).child(text);
        let button = |id: &'static str, label: SharedString, primary: bool| {
            div()
                .id(id)
                .px_3()
                .py_1()
                .rounded_md()
                .cursor_pointer()
                .when(primary, |b| b.bg(t.accent).text_color(t.bg_app).hover(|s| s.bg(t.accent_hover)))
                .when(!primary, |b| b.bg(t.bg_panel).text_color(t.text).hover(|s| s.bg(t.border)))
                .child(label)
        };
        let chip = |id: &'static str, label: &'static str, on: bool| {
            div()
                .id(id)
                .px_2()
                .py_0p5()
                .rounded_md()
                .cursor_pointer()
                .when(on, |b| b.bg(t.accent).text_color(t.bg_app))
                .when(!on, |b| b.bg(t.bg_panel).text_color(t.text_muted).hover(|s| s.text_color(t.text)))
                .child(label)
        };
        let busy = d.running.is_some();

        let mut body: Vec<AnyElement> = Vec::new();
        // Drive
        body.push(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(div().flex_1().overflow_hidden().child(match &d.drive {
                    Some(p) => line(p.display().to_string()).into_any_element(),
                    None => muted("No drive chosen".into()).into_any_element(),
                }))
                .when(!busy, |r| {
                    r.child(
                        button("choose-drive", if d.drive.is_some() { "Change…".into() } else { "Choose drive…".into() }, d.drive.is_none())
                            .on_click(cx.listener(|this, _, _, cx| this.choose_drive(cx))),
                    )
                })
                .into_any_element(),
        );
        // Options
        if !busy {
            body.push(
                div()
                    .flex()
                    .gap_2()
                    .child(chip("scope-all", "Whole library", d.scope == Scope::WholeLibrary).on_click(cx.listener(
                        |this, _, _, cx| {
                            if let Some(d) = &mut this.sync {
                                d.scope = Scope::WholeLibrary;
                            }
                            this.replan(cx);
                        },
                    )))
                    .child(chip("scope-pl", "Only tracks in playlists", d.scope == Scope::PlaylistsOnly).on_click(
                        cx.listener(|this, _, _, cx| {
                            if let Some(d) = &mut this.sync {
                                d.scope = Scope::PlaylistsOnly;
                            }
                            this.replan(cx);
                        }),
                    ))
                    .into_any_element(),
            );
            body.push(
                div()
                    .id("sync-remove")
                    .flex()
                    .gap_2()
                    .cursor_pointer()
                    .text_color(t.text_muted)
                    .hover(|s| s.text_color(t.text))
                    .child(div().w(px(14.)).text_color(t.accent).child(if d.remove { "☑" } else { "☐" }))
                    .child("Also remove tracks that are no longer in the library (only ones TrackMap copied)")
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(d) = &mut this.sync {
                            d.remove = !d.remove;
                        }
                        this.replan(cx);
                    }))
                    .into_any_element(),
            );
        }

        // Progress / result / preview
        if let Some(r) = &d.running {
            let (done, bytes) = (r.done.load(Ordering::Relaxed), r.bytes.load(Ordering::Relaxed));
            let frac = if r.total_bytes > 0 { bytes as f32 / r.total_bytes as f32 } else { done as f32 / r.total.max(1) as f32 };
            body.push(line(format!("Syncing {} of {}  ·  {} of {}", done, r.total, human_bytes(bytes), human_bytes(r.total_bytes))).into_any_element());
            body.push(
                div()
                    .h(px(6.))
                    .w_full()
                    .rounded_full()
                    .bg(t.bg_panel)
                    .child(div().h_full().w(gpui::relative(frac.clamp(0.0, 1.0))).rounded_full().bg(t.accent))
                    .into_any_element(),
            );
        } else if let Some(rep) = &d.report {
            let mut s = format!("Synced: {} copied, {} updated, {} playlists", rep.copied, rep.updated, rep.playlists);
            if rep.removed > 0 {
                s += &format!(", {} removed", rep.removed);
            }
            if rep.cancelled {
                s = format!("Stopped. {s}. Sync again to finish.");
            }
            body.push(line(s).into_any_element());
            for (dest, e) in rep.errors.iter().take(4) {
                body.push(muted(format!("Couldn't write {dest}: {e}")).into_any_element());
            }
            if rep.errors.is_empty() && !rep.cancelled {
                body.push(muted("Done. Eject the drive in Finder before unplugging it.".into()).into_any_element());
            }
        }
        if !busy {
            match (&d.plan, d.planning, &d.drive) {
                (_, true, _) => body.push(muted("Checking the drive…".into()).into_any_element()),
                (Some(p), _, Some(_)) => {
                    let mut lines = Vec::new();
                    if p.copies() > 0 {
                        lines.push(format!("Copy {} new tracks", p.copies()));
                    }
                    if p.updates() > 0 {
                        lines.push(format!("Update {} changed tracks", p.updates()));
                    }
                    if p.bytes > 0 {
                        lines.push(format!("{} to copy", human_bytes(p.bytes)));
                    }
                    let mut quiet = Vec::new();
                    if p.up_to_date > 0 {
                        quiet.push(format!("{} already up to date", p.up_to_date));
                    }
                    if p.reused > 0 {
                        quiet.push(format!("{} already on the drive (reused)", p.reused));
                    }
                    if p.renamed > 0 {
                        quiet.push(format!("{} renamed because a different file had the same name", p.renamed));
                    }
                    if p.playlists() > 0 {
                        lines.push(format!("Write {} playlists", p.playlists()));
                    }
                    if p.removals() > 0 {
                        lines.push(format!("Remove {} tracks", p.removals()));
                    }
                    if lines.is_empty() {
                        lines.push("Everything is already on the drive".into());
                    }
                    body.push(line(lines.join("  ·  ")).into_any_element());
                    if !quiet.is_empty() {
                        body.push(muted(quiet.join("  ·  ")).into_any_element());
                    }
                    for (path, why) in p.skipped.iter().take(3) {
                        let name = path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
                        body.push(muted(format!("Skipping {name}: {why}")).into_any_element());
                    }
                    if let Some(free) = p.free {
                        let after = free.saturating_sub(p.bytes);
                        body.push(
                            muted(if p.fits() {
                                format!("{} free on the drive, {} after syncing", human_bytes(free), human_bytes(after))
                            } else {
                                format!("Not enough space: {} needed, {} free", human_bytes(p.bytes), human_bytes(free))
                            })
                            .into_any_element(),
                        );
                    }
                    body.push(
                        muted("Nothing already on the drive is changed or deleted, only files TrackMap copied there.".into())
                            .into_any_element(),
                    );
                }
                _ => {}
            }
        }

        // Buttons
        let can_sync = !busy
            && d.plan.as_ref().is_some_and(|p| !p.actions.is_empty() && p.fits())
            && d.drive.is_some();
        let buttons = div()
            .flex()
            .justify_end()
            .gap_2()
            .when(busy, |r| {
                r.child(button("sync-stop", "Stop".into(), false).on_click(cx.listener(|this, _, _, cx| {
                    if let Some(run) = this.sync.as_ref().and_then(|d| d.running.as_ref()) {
                        run.cancel.store(true, Ordering::Relaxed);
                    }
                    cx.notify();
                })))
            })
            .when(!busy, |r| {
                r.child(button("sync-close", "Close".into(), false).on_click(cx.listener(|this, _, _, cx| {
                    this.sync = None;
                    cx.notify();
                })))
                .when(can_sync, |r| {
                    r.child(button("sync-go", "Sync".into(), true).on_click(cx.listener(|this, _, _, cx| this.run_sync(cx))))
                })
            });

        Some(
            div()
                .absolute()
                .size_full()
                .flex()
                .justify_center()
                .pt(px(70.))
                .child(
                    div()
                        .id("sync-dialog")
                        .w(px(560.))
                        .p_4()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .rounded_lg()
                        .bg(t.bg_elevated)
                        .border_1()
                        .border_color(t.border)
                        .shadow_lg()
                        .occlude()
                        .child(div().text_lg().text_color(t.text).child("Sync to USB"))
                        .children(body)
                        .child(buttons),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_are_human() {
        assert_eq!(human_bytes(1_800_000_000), "1.8 GB");
        assert_eq!(human_bytes(340_000_000), "340 MB");
        assert_eq!(human_bytes(12_000), "12 KB");
        assert_eq!(human_bytes(0), "1 KB");
    }
}
