#!/bin/sh
# Renders assets/icon.svg into assets/AppIcon.icns (all sizes macOS uses), with built-in tools:
# Quick Look renders the SVG, sips resizes, iconutil packs the .icns.
set -e
cd "$(dirname "$0")/.."
WORK=$(mktemp -d)
qlmanage -t -s 1024 -o "$WORK" assets/icon.svg >/dev/null
SRC="$WORK/icon.svg.png"
SET="$WORK/AppIcon.iconset"
mkdir -p "$SET"
for size in 16 32 128 256 512; do
  sips -z $size $size "$SRC" --out "$SET/icon_${size}x${size}.png" >/dev/null
  double=$((size * 2))
  sips -z $double $double "$SRC" --out "$SET/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$SET" -o assets/AppIcon.icns
rm -rf "$WORK"
echo "Wrote assets/AppIcon.icns"
