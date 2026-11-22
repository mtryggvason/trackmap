# TrackMap

A fast, lightweight alternative to Rekordbox for preparing music for the
[nap.works](https://nap.works/) Devious Pocket.

Open a music folder and TrackMap lays your tracks out on a map: tempo from left to right, key
(Camelot) from top to bottom, with cover art. It detects BPM and key, plays tracks with a
waveform, keeps playlists as plain folders, and syncs everything to a Rekordbox-style USB drive
without touching what's already on it. The folder is watched, so new files show up on their own.

See `PLAN.md` for the design and what's next.

## Build

GPUI is built from Zed's repository at a pinned commit (the crates.io release has no trackpad
pinch). Check it out once, then build as usual:

    scripts/fetch-gpui.sh
    cargo run -p trackmap -- ~/Music        # or open a folder with ⌘O

Try it without your own music:

    cargo run -p trackmap-core --example demo_library -- /tmp/trackmap-demo
    cargo run -p trackmap -- /tmp/trackmap-demo

Build `TrackMap.app` (zipped into `dist/`):

    scripts/bundle-macos.sh

## Test

    cargo test --workspace
