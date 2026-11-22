# TrackMap

Organise a music folder for the nap.works Devious Pocket: a map of your tracks by tempo and key,
playlists as folders, and (soon) sync to a Rekordbox-style USB. See `PLAN.md`.

## Build

GPUI is built from Zed's repository at a pinned commit (the crates.io release has no trackpad
pinch). Check it out once, then build as usual:

    scripts/fetch-gpui.sh
    cargo run -p trackmap -- ~/Music        # or open a folder with ⌘O

Try it without your own music:

    cargo run -p trackmap-core --example demo_library -- /tmp/trackmap-demo
    cargo run -p trackmap -- /tmp/trackmap-demo

## Test

    cargo test --workspace
