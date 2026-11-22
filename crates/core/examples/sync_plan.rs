//! Dry run: prints what syncing a library to a drive would do, without writing anything.
//!
//!     cargo run -p trackmap-core --example sync_plan -- <library> <drive> [playlists]

use trackmap_core::sync::{self, Action, Scope};

fn main() {
    let mut args = std::env::args().skip(1);
    let lib = trackmap_core::open(std::path::Path::new(&args.next().expect("library"))).expect("open library").library;
    let drive = std::path::PathBuf::from(args.next().expect("drive"));
    let scope = if args.next().as_deref() == Some("playlists") { Scope::PlaylistsOnly } else { Scope::WholeLibrary };
    let pls = trackmap_core::playlists::list(&lib.root).unwrap_or_default();
    let plan = sync::plan(&lib, &pls, scope, &drive, false);
    println!(
        "copy {} · update {} · up to date {} · reused {} · renamed {} · playlists {} · skipped {} · {} MB · free {:?} MB · fits {}",
        plan.copies(), plan.updates(), plan.up_to_date, plan.reused, plan.renamed, plan.playlists(),
        plan.skipped.len(), plan.bytes / 1_000_000, plan.free.map(|f| f / 1_000_000), plan.fits()
    );
    for a in plan.actions.iter().take(6) {
        if let Action::Copy { dest, .. } | Action::Playlist { dest, .. } = a {
            println!("  {dest}");
        }
    }
}
