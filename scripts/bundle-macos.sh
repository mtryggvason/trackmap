#!/bin/sh
# Builds TrackMap.app in release mode and zips it for sharing: dist/TrackMap-<version>-macos-<arch>.zip
#
#   scripts/bundle-macos.sh              # for this Mac's architecture
#   scripts/bundle-macos.sh --universal  # Apple Silicon + Intel in one app (needs both Rust targets)
#
# Run scripts/fetch-gpui.sh first. The app is ad-hoc signed, not notarized, so the first time
# it's opened macOS asks for confirmation (see README).
set -e
cd "$(dirname "$0")/.."

VERSION=${VERSION:-$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)}
APP=dist/TrackMap.app

if [ "$1" = "--universal" ]; then
  ARCH=universal
  for target in aarch64-apple-darwin x86_64-apple-darwin; do
    cargo build --release -p trackmap --target "$target"
  done
  mkdir -p target/universal
  lipo -create -output target/universal/trackmap \
    target/aarch64-apple-darwin/release/trackmap \
    target/x86_64-apple-darwin/release/trackmap
  BIN=target/universal/trackmap
else
  ARCH=$(uname -m)
  cargo build --release -p trackmap
  BIN=target/release/trackmap
fi

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/trackmap"
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>TrackMap</string>
  <key>CFBundleDisplayName</key><string>TrackMap</string>
  <key>CFBundleIdentifier</key><string>io.github.mtryggvason.trackmap</string>
  <key>CFBundleExecutable</key><string>trackmap</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>LSMinimumSystemVersion</key><string>12.0</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.music</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST

# Ad-hoc signature: required for Apple Silicon, and keeps the bundle consistent.
codesign --force --deep --sign - "$APP"

ZIP="dist/TrackMap-$VERSION-macos-$ARCH.zip"
rm -f "$ZIP"
ditto -c -k --keepParent "$APP" "$ZIP"
echo "Built $ZIP"
