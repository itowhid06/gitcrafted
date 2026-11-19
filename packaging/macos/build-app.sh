#!/bin/sh
# Packs the Apple silicon and Intel builds into one universal gitcrafted.app, then a .dmg (drag
# to Applications) and a .zip of it.
# Usage: packaging/macos/build-app.sh <version> <output name without extension>
# Needs both `cargo build --release --target aarch64-apple-darwin` and
# `--target x86_64-apple-darwin` done first. macOS only (lipo, iconutil, codesign, hdiutil).
set -eu

version="$1"
out="$2"
work="$(mktemp -d)"
app="$work/gitcrafted.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"

lipo -create -output "$app/Contents/MacOS/gitcrafted" \
  target/aarch64-apple-darwin/release/gitcrafted \
  target/x86_64-apple-darwin/release/gitcrafted
strip -x "$app/Contents/MacOS/gitcrafted"
sed "s/@VERSION@/$version/g" packaging/macos/Info.plist > "$app/Contents/Info.plist"
printf 'APPL????' > "$app/Contents/PkgInfo"

# The icon, every size scaled from the 1024 px master.
iconset="$work/gitcrafted.iconset"
mkdir "$iconset"
for entry in 16x16:16 16x16@2x:32 32x32:32 32x32@2x:64 128x128:128 128x128@2x:256 256x256:256 256x256@2x:512 512x512:512 512x512@2x:1024; do
  name="${entry%%:*}"
  px="${entry##*:}"
  sips -z "$px" "$px" assets/app-icon/gitcrafted-1024.png --out "$iconset/icon_$name.png" >/dev/null
done
iconutil -c icns -o "$app/Contents/Resources/gitcrafted.icns" "$iconset"

cp README.md ATTRIBUTION.md LICENSE-MIT LICENSE-APACHE "$app/Contents/Resources/"

# Not notarized: an ad-hoc signature, which Apple silicon needs to run the program at all.
codesign --force --deep --sign - "$app"
codesign --verify --strict "$app"

ditto -c -k --keepParent "$app" "$out.zip"

dmg="$work/dmg"
mkdir "$dmg"
cp -R "$app" "$dmg/"
ln -s /Applications "$dmg/Applications"
hdiutil create -volname gitcrafted -srcfolder "$dmg" -ov -format UDZO "$out.dmg"
