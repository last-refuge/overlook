#!/usr/bin/env bash
# Packages the native release binary as a Mac application for the build architecture.
set -euo pipefail

version=${1:?version}
output=${2:?output directory}
binary=${3:-target/release/overlook}
root=$(cd "$(dirname "$0")/.." && pwd)
arch=$(uname -m)
[[ "$(uname -s)" == Darwin ]]
lipo "$binary" -verify_arch "$arch"
[[ "$("$binary" --version)" == "overlook $version" ]]

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
app="$stage/Overlook.app"
resources="$app/Contents/Resources"
iconset="$stage/Overlook.iconset"
mkdir -p "$app/Contents/MacOS" "$resources" "$iconset" "$output"
install -m755 "$binary" "$app/Contents/MacOS/overlook"
install -m644 "$root/crates/overlook-app/Info.plist" "$app/Contents/Info.plist"
cp "$root/LICENSE" "$root/branding/fonts/Inter-LICENSE.txt" \
  "$root/branding/fonts/SourceSerif-LICENSE.md" "$resources/"

for size in 16 32 128 256; do
  cp "$root/branding/assets/overlook-icon-$size.png" "$iconset/icon_${size}x${size}.png"
  cp "$root/branding/assets/overlook-icon-$((size * 2)).png" "$iconset/icon_${size}x${size}@2x.png"
done
cp "$root/branding/assets/overlook-icon-512.png" "$iconset/icon_512x512.png"
iconutil -c icns "$iconset" -o "$resources/Overlook.icns"

/usr/libexec/PlistBuddy -c "Add :CFBundleShortVersionString string $version" "$app/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Add :CFBundleVersion string $version" "$app/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Add :CFBundleIconFile string Overlook.icns" "$app/Contents/Info.plist"
plutil -lint "$app/Contents/Info.plist"
codesign --force --sign - "$app"
codesign --verify --strict "$app"

archive="$output/overlook-$version-$arch-macos.zip"
ditto -c -k --keepParent "$app" "$archive"
printf '%s\n' "$archive"
