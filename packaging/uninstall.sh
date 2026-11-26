#!/bin/sh
# Removes Tern for the current user. Signing out and the message cache are separate; see the end.
set -eu
id=io.github.rhgrieve.Tern
bin=$HOME/.local/bin
data=${XDG_DATA_HOME:-$HOME/.local/share}
units=${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user
signed_in=
if [ -x "$bin/tern-cli" ] && [ "${1:-}" != --no-service ]; then
    "$bin/tern-cli" logout || signed_in=yes
fi
[ "${1:-}" = --no-service ] || systemctl --user disable -q --now ternd.service || true
for name in ternd tern tern-cli tern-login tern-call; do rm -f "$bin/$name"; done
rm -f "$data/applications/$id.desktop" "$data/applications/$id.Chat.desktop" \
    "$data/dbus-1/services/$id.service" "$data/icons/hicolor/scalable/apps/$id.svg" \
    "$units/ternd.service"
[ "${1:-}" = --no-service ] || systemctl --user daemon-reload
[ -z "$signed_in" ] || echo "Signing out did not work: the login may still be in your keyring (look for Tern)."
echo "Tern is removed. Your message cache and drafts are still in $data/tern;"
echo "delete that folder if you no longer want them."
