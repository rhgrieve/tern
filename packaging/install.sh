#!/bin/sh
# Installs Tern for the current user, from an unpacked download or from a build of this repository.
# Running it again updates. Does not need root.
set -eu

service=yes
case "${1:-}" in
"") ;;
--no-service) service=no ;;
*) echo "usage: $0 [--no-service]" >&2; exit 2 ;;
esac
if [ "$(id -u)" = 0 ] && [ "$service" = yes ]; then
    echo "Tern is installed for one user: run this without sudo." >&2
    exit 1
fi

here=$(cd "$(dirname "$0")" && pwd)
if [ -d "$here/bin" ]; then
    programs=$here/bin
    share=$here/share
else
    programs=$here/../target/release
    share=$here
fi
id=io.github.rhgrieve.Tern
bin=$HOME/.local/bin
data=${XDG_DATA_HOME:-$HOME/.local/share}
units=${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user

for name in ternd tern tern-cli tern-login; do
    if [ ! -x "$programs/$name" ]; then
        echo "$programs/$name is missing. In a checkout, build first: cargo build --release --workspace" >&2
        exit 1
    fi
    wrong=$(ldd "$programs/$name" 2>&1 | grep -E 'not found|not a dynamic' || true)
    if [ -n "$wrong" ]; then
        cat >&2 <<END
$name cannot run here:
$wrong

Tern needs GTK 4.16, libadwaita 1.8 and WebKitGTK 6.0 or newer (and a 64-bit Intel or AMD computer):
  Fedora   sudo dnf install gtk4 libadwaita webkitgtk6.0
  Ubuntu   sudo apt install libgtk-4-1 libadwaita-1-0 libwebkitgtk-6.0-4
  Arch     sudo pacman -S --needed gtk4 libadwaita webkitgtk-6.0
Older systems (Ubuntu 25.04, Debian 13, Fedora 42 and before) do not have them.
END
        exit 1
    fi
done

mkdir -p "$bin" "$data/applications" "$data/dbus-1/services" \
    "$data/icons/hicolor/scalable/apps" "$units"
for name in ternd tern tern-cli tern-login; do
    # Beside and then over it, so that a running one is not disturbed.
    cp "$programs/$name" "$bin/.$name.new"
    chmod 755 "$bin/.$name.new"
    mv -f "$bin/.$name.new" "$bin/$name"
done
# Builds before the application had its present name.
rm -f "$data"/applications/io.github.tern.Tern.desktop \
    "$data"/applications/io.github.tern.Tern.Chat.desktop \
    "$data"/dbus-1/services/io.github.tern.Tern.service \
    "$data"/icons/hicolor/scalable/apps/io.github.tern.Tern.svg
for name in "$id" "$id.Chat"; do
    sed "s|^Exec=tern|Exec=$bin/tern|" "$share/desktop/$name.desktop" >"$data/applications/$name.desktop"
done
cp "$share/desktop/$id.svg" "$data/icons/hicolor/scalable/apps/"
sed "s|^Exec=.*/tern |Exec=$bin/tern |" "$share/dbus/$id.service" >"$data/dbus-1/services/$id.service"
cp "$share/systemd/ternd.service" "$units/"
update-desktop-database -q "$data/applications" 2>/dev/null || true
gtk4-update-icon-cache -qtf "$data/icons/hicolor" 2>/dev/null || true

if [ "$service" = yes ]; then
    systemctl --user daemon-reload
    systemctl --user enable -q ternd.service
    systemctl --user restart ternd.service
    busctl --user call org.freedesktop.DBus /org/freedesktop/DBus org.freedesktop.DBus ReloadConfig \
        >/dev/null 2>&1 || true
fi
cat <<END
Tern is installed in $bin.

First time:  $bin/tern-cli login
Then open Tern from your applications, or run: $bin/tern
Is it well?  $bin/tern-cli status
END
