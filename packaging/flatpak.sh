#!/bin/sh
# Makes dist/tern-VERSION-x86_64.flatpak from what archive.sh put together. The programs are taken as
# they are, so this needs flatpak and the runtime below, but no builder.
set -eu
cd "$(dirname "$0")/.."
id=io.github.rhgrieve.Tern
runtime=org.gnome.Platform
branch=51
from=$(ls -dt dist/tern-*-x86_64 | head -n 1)
work=dist/flatpak
rm -rf "$work"
mkdir -p "$work"

flatpak build-init "$work/app" "$id" "$runtime" "$runtime" "$branch"
files=$work/app/files
mkdir -p "$files/bin" "$files/share/applications" "$files/share/metainfo" "$files/share/licenses/$id" \
    "$files/share/icons/hicolor/scalable/apps" "$files/share/dbus-1/services"
cp "$from"/bin/* "$files/bin/"
cp "$from/share/desktop/$id.desktop" "$from/share/desktop/$id.Chat.desktop" "$files/share/applications/"
cp "$from/share/desktop/$id.svg" "$files/share/icons/hicolor/scalable/apps/"
cp "$from/share/desktop/$id.metainfo.xml" "$files/share/metainfo/"
cp "$from"/LICENSE* "$files/share/licenses/$id/"
sed 's|^Exec=.*/tern |Exec=/app/bin/tern |' "$from/share/dbus/$id.service" >"$files/share/dbus-1/services/$id.service"

# The network, a window, the keyring (by a portal), and of the desktop only: notifications, the tray,
# whether anybody is there, and whether the computer locks, sleeps or goes online. No files: those
# are picked by the user.
flatpak build-finish "$work/app" \
    --command=tern \
    --share=network --share=ipc --socket=wayland --socket=fallback-x11 --device=dri \
    --talk-name=org.freedesktop.Notifications \
    --talk-name=org.kde.StatusNotifierWatcher \
    --talk-name=org.gnome.Mutter.IdleMonitor \
    --system-talk-name=org.freedesktop.login1 \
    --system-talk-name=org.freedesktop.NetworkManager
flatpak build-export --disable-sandbox "$work/repo" "$work/app" stable
name=$(basename "$from").flatpak
flatpak build-bundle --runtime-repo=https://dl.flathub.org/repo/flathub.flatpakrepo \
    "$work/repo" "dist/$name" "$id" stable
cd dist
sha256sum "$name" >"$name.sha256"
echo "dist/$name"
