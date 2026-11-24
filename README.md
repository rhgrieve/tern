# Tern

A native Microsoft Teams client for Linux, in progress. Unofficial; not affiliated with Microsoft.
It uses the undocumented services Teams itself uses, which can change at any time and which your
organization may not allow.

**Today:** a background service that keeps your Teams presence true to what you do at this
computer, in any app, also while you still use Teams on the web.

| Program | What it does |
|---|---|
| `ternd` | The background service |
| `tern-cli` | `login`, `logout`, `whoami`, `tokens`, `tail`, `status`, `sensors` |
| `tern-login` | The sign-in window (WebKitGTK). It takes the redirect before the page loads; pasting that address from a browser is deliberately not supported, because that is how sign-in codes get phished |

- Away after 5 minutes without keyboard or pointer input anywhere (Wayland `ext-idle-notify-v1`;
  GNOME over D-Bus), at once on lock, and before sleep. A playing video does not count as activity.
- A status you set yourself in Teams always wins.
- The refresh token lives in the desktop keyring; access tokens only in memory. Tokens are only
  sent to the service they were issued for.

```sh
cargo test --workspace
cargo run -p tern-cli -- sensors     # watch detection, no account needed
```

MIT, see `LICENSE`. `vendor/engine` is from rusty-teams (MIT), see `vendor/engine/PATCHES.md`.
