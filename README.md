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

## Share it

GitHub builds the app for you (`.github/workflows/build.yml`):

- Every push to `main` builds a universal (Apple Silicon + Intel) `TrackMap.app`, downloadable
  from that run's artifacts on the Actions tab.
- Pushing a version tag publishes a GitHub Release with the zipped app attached:

      git tag v0.1.0 && git push origin v0.1.0

To build it locally instead (needs several GB of free disk):

    scripts/fetch-gpui.sh
    scripts/bundle-macos.sh            # this Mac's architecture
    scripts/bundle-macos.sh --universal   # needs: rustup target add x86_64-apple-darwin aarch64-apple-darwin

The app is ad-hoc signed but not notarized (that needs a paid Apple Developer account), so the
first time it's opened: **right-click TrackMap → Open → Open**, or System Settings → Privacy &
Security → **Open Anyway**.
