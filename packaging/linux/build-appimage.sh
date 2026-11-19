#!/bin/sh
# Packs target/release/gitcrafted into an AppImage: one file that runs on most Linux distributions.
# Usage: packaging/linux/build-appimage.sh <output.AppImage>
# Needs appimagetool on PATH (the release workflow downloads it).
set -eu

out="$1"
stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT
app="$stage/gitcrafted.AppDir"
mkdir -p "$app/usr/bin" "$app/usr/share/applications" "$app/usr/share/icons"

cp target/release/gitcrafted "$app/usr/bin/gitcrafted"
strip "$app/usr/bin/gitcrafted"
cp packaging/gitcrafted.desktop "$app/usr/share/applications/gitcrafted.desktop"
cp -R assets/app-icon/hicolor "$app/usr/share/icons/hicolor"

# What appimagetool looks for at the top of the AppDir.
cp packaging/gitcrafted.desktop "$app/gitcrafted.desktop"
cp assets/app-icon/hicolor/256x256/apps/gitcrafted.png "$app/gitcrafted.png"
ln -s gitcrafted.png "$app/.DirIcon"
cat > "$app/AppRun" <<'EOF'
#!/bin/sh
here="$(dirname "$(readlink -f "$0")")"
exec "$here/usr/bin/gitcrafted" "$@"
EOF
chmod +x "$app/AppRun"

ARCH="${ARCH:-$(uname -m)}"
export ARCH
appimagetool "$app" "$out"
