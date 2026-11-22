#!/bin/sh
# Checks out Zed's repository at the commit TrackMap builds GPUI from, into vendor/zed.
set -e
REV=d31d1074a07b7bbaa43a52fef7847a00dd491baf
cd "$(dirname "$0")/.."
if [ ! -d vendor/zed/.git ]; then
  git init -q vendor/zed
  git -C vendor/zed remote add origin https://github.com/zed-industries/zed
fi
if [ "$(git -C vendor/zed rev-parse HEAD 2>/dev/null)" != "$REV" ]; then
  git -C vendor/zed fetch --depth 1 origin "$REV"
  git -C vendor/zed checkout -q "$REV"
fi
echo "vendor/zed is at $REV"
