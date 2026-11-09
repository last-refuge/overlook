#!/usr/bin/env bash
# Builds the generic Linux tarball used by the in-app updater and by install.sh on
# non-Arch systems. Expects a release binary at target/release/overlook.
#   scripts/make-release-tarball.sh <version> <out-dir>
set -Eeuo pipefail
VERSION=${1:?version}; OUT=${2:?out dir}
ROOT=$(cd "$(dirname "$0")/.." && pwd)
NAME="overlook-$VERSION-x86_64-linux"
STAGE=$(mktemp -d)
mkdir -p "$STAGE/$NAME/packaging" "$STAGE/$NAME/icons" "$OUT"
install -m755 "$ROOT/target/release/overlook" "$STAGE/$NAME/overlook"
cp "$ROOT/packaging/ai.lastrefuge.Overlook.desktop" "$ROOT/packaging/ai.lastrefuge.Overlook.metainfo.xml" "$STAGE/$NAME/packaging/"
cp "$ROOT/branding/assets/overlook-app-icon.svg" "$ROOT"/branding/assets/overlook-icon-*.png "$STAGE/$NAME/icons/"
cp "$ROOT/install.sh" "$ROOT/README.md" "$ROOT/LICENSE" "$STAGE/$NAME/"
tar -C "$STAGE" -czf "$OUT/$NAME.tar.gz" "$NAME"
rm -rf "$STAGE"
echo "$OUT/$NAME.tar.gz"
