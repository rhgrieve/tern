# Tern

## The Flatpak does all that the other two do

Tern ships as an archive (`install.sh`), an AppImage and a Flatpak. Every feature works the same in
all three. Never turn a feature off, hide it, or say "not in the Flatpak" because the sandbox is in
the way: make it work there.

- A program of the user's computer (an editor, a browser, a player, `systemd-inhibit`, anything in
  their `$PATH`) is started through `tern_ipc::host`: `host::command` for what outlives Tern,
  `host::watched` for what ends with it, `host::script` to ask the computer something. Outside the
  Flatpak these are the plain command.
- The sandbox does not see the computer's `/usr`, `$PATH`, `~/.local/share` or session variables.
  Look for them with `host::script`, by the same rules as outside, and give the computer only paths
  both see alike: the home folder, `$XDG_RUNTIME_DIR/app/<id>`.
- A new D-Bus name, device, socket or portal goes into `packaging/flatpak.sh`, and into the
  permissions the `flatpak` job in `.github/workflows/build.yml` checks.
- Before calling a change done, try it in the Flatpak (`packaging/archive.sh && packaging/flatpak.sh`,
  then `flatpak install --user ./dist/tern-*.flatpak`), not only in the build.
