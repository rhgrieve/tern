#!/bin/sh
# Makes dist/tern-VERSION-x86_64.AppImage from what archive.sh put together: the AppImage project's
# runtime, followed by a squashfs.
set -eu
cd "$(dirname "$0")/.."
runtime=https://github.com/AppImage/type2-runtime/releases/download/20251108/runtime-x86_64
sum=2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d
id=io.github.rhgrieve.Tern
from=$(echo dist/tern-*-x86_64)
work=dist/appimage
rm -rf "$work"
mkdir -p "$work"
cp -r "$from" "$work/AppDir"
install -m755 packaging/AppRun "$work/AppDir/AppRun"
# D-Bus does not know an image.
sed '/^DBusActivatable/d' "$from/share/desktop/$id.desktop" >"$work/AppDir/$id.desktop"
cp "$from/share/desktop/$id.svg" "$work/AppDir/"
ln -s "$id.svg" "$work/AppDir/.DirIcon"

[ "${1:-}" = --unpacked ] && exit 0
curl -fsSL -o "$work/runtime" "$runtime"
echo "$sum  $work/runtime" | sha256sum -c --quiet
mksquashfs "$work/AppDir" "$work/squashfs" -root-owned -noappend -comp zstd -quiet -no-progress
name=$(basename "$from").AppImage
cat "$work/runtime" "$work/squashfs" >"dist/$name"
chmod 755 "dist/$name"
cd dist
sha256sum "$name" >"$name.sha256"
echo "dist/$name"
