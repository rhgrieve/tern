# Tern

A native Microsoft Teams client for Linux, in progress. Unofficial; not affiliated with Microsoft.
It uses the undocumented services Teams itself uses, which can change at any time and which your
organization may not allow.

**Today:** a background service that keeps your Teams presence true to what you do at this
computer, in any app, also while you still use Teams on the web; keeps chats and channels in a
local cache you can read and search offline; queues messages until they can be sent; and shows
notifications you can answer. Recent pictures and faces are kept for offline use, files on request;
a tray icon shows your status and unread chats. The window shows chats and
channels with threads, sends, answers, edits and reacts, mentions people,
attaches pictures and files, and searches.
Conversations can have windows of their own (Ctrl+Shift+O, or a double click), known to window
managers as `io.github.tern.Tern.Chat`. Adaptive Cards are shown; answering one opens Teams.
Everything works with the keyboard (Ctrl+? lists the keys) and says what it is to screen readers.

| Program | What it does |
|---|---|
| `tern` | The window; `tern --demo` shows it with invented people |
| `ternd` | The background service |
| `tern-cli` | `login`, `logout`, `whoami`, `chats`, `read`, `search`, `files`, `get`, `send`, `status`, `tokens`, `tail`, `sensors`, `probe-sync` |
| `tern-login` | The sign-in window (WebKitGTK). It takes the redirect before the page loads; pasting that address from a browser is deliberately not supported, because that is how sign-in codes get phished |

- Away after 5 minutes without keyboard or pointer input anywhere (Wayland `ext-idle-notify-v1`;
  GNOME over D-Bus), at once on lock, and before sleep. A playing video does not count as activity.
- A status you set yourself in Teams always wins.
- The refresh token lives in the desktop keyring; access tokens only in memory. Tokens are only
  sent to the service they were issued for.

- The cache holds message text, in `~/.local/share/tern`, readable only by you. It is not encrypted
  by itself; `logout` keeps it. Pictures and files take at most 1 GB each.
- Only pictures Teams itself stores are fetched ahead. Anything else would tell its owner when you
  received a message. The same goes for pictures in cards.

To run a build beside the installed service: `TERN_INSTANCE=dev TERN_NO_PRESENCE=1 ternd`.

```sh
cargo test --workspace
cargo run --release -p tern -- --demo
cargo run --release -p tern --example scroll   # 10,000 messages, on a real display
cargo run --release -p tern --example opening  # what opening chats costs, needs a display
cargo run -p tern-cli -- sensors     # watch detection, no account needed
```

MIT, see `LICENSE`. `vendor/engine` is from rusty-teams (MIT), see `vendor/engine/PATCHES.md`.
