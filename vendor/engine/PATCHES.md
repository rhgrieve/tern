# Vendored engine

From https://github.com/danieldaeschle/rusty-teams (MIT, see `LICENSE`), commit
`414be8e2046df79b7b42cd49ae0bb555f8a1ca18`. Crates: `session`, `chatsvc`, `graph`, `core`, `store`, `calling`, plus the one asset
`app/assets/emoticons.tsv`. Examples are left out. `tern.patch` is the full difference.

| Change | Where | Why |
|---|---|---|
| `TokenProvider`, `Session::with_provider`, `Session::access_token` | `session/src/provider.rs`, `session.rs`, `native.rs` | Tokens without a browser |
| A token is only sent to hosts of its audience | `session/src/provider.rs` | Upstream attaches it to any https URL |
| Page-only requests answer `Error::PageOnly` | `session.rs`, `page_script.rs` | Fail clearly instead of waiting for a page |
| Push socket takes its token from `access_token` | `chatsvc/src/realtime/native.rs` | Same |
| `/ups/` counts as a bearer path | `session/src/native.rs` | Upstream sends the regional presence service through a page |
| `PresenceService::at(base)` | `chatsvc/src/presence.rs` | Regional presence host |
| Teams addresses use the account's region; `with_region` | `session/src/provider.rs`, `session.rs` | Upstream is fixed to `emea` |
| `with_guard` refuses requests | `session/src/session.rs` | Trying things on a real account without writing |
| `with_client_message_id` | `chatsvc/src/messages.rs` | Every try of a queued message has the same id |
| `find_image_file` | `store/src/image_files.rs` | The window finds cached pictures without touching the cache |
| `open_read_only` | `store/src/store.rs` | Readers beside the daemon |
| An empty page of messages is followed to the next | `chatsvc/src/chat_listing.rs` | Upstream takes a quiet stretch for the start of a chat |
| Recent conversation states between hourly full reads | `chatsvc/src/conversations.rs`, `core/src/{engine,remote,sidebar_sync}.rs` | Upstream reads every conversation on every refresh |
| Skype-token downloads and uploads without a page, in the account's region | `session/src/skype.rs`, `chatsvc/src/ams.rs` | Inline pictures; upstream uploads to Germany |
| Pictures of linked pages without a page | `core/src/remote.rs` | Link previews; upstream asks a page for them |
| Reacting with an org's own emoji | `chatsvc/src/reactions.rs` | Upstream only knows the standard ones |
| `/trap/tokens` counts as a bearer path | `session/src/native.rs` | Calls read their relay credential there |
| Calls follow the account: `TenantRouting::for_account`, `relay_host_for` | `calling/src/signaling.rs`, `relay.rs` | Upstream is fixed to Europe |
| An offer may unfold into at most 64 lines | `calling/src/sdp.rs` | The other side could ask for any amount of memory |
| A call that starts muted mutes both microphone tracks | `calling/src/call/mod.rs` | With noise suppression on Low the unmuted one was sent |
| Links are parsed before their host is trusted; only call services are reached | `calling/src/signaling.rs` | `https://evil.example#.teams.microsoft.com/` passed, and a push could name any Teams host |
| `EngineConfig::trouter_host`; `run_call` runs one call with the caller's own options | `calling/src/engine.rs`, `call/mod.rs` | Finding the push host needs a web page; `run_test_call` only knows Europe |
| Messages' Markdown has italic, `~~strike~~`, `#` headings, `>` quotes, `1.` and `*` lists, pipe tables; `markdown_line`, `markdown_table`, `table_cells` and `italicize` are public | `core/src/markdown.rs`, `italic.rs`, `lib.rs`, `tests/markdown.rs` | Teams writes all of these; the message box styles what is sent by the same rules |
| WebRTC's build helper pinned to 0.3.19; the build uses our checked copy | `native/webrtc-sys/Cargo.toml`, `.cargo/config.toml` | 0.3.21 named another WebRTC build, downloaded without a check |

Known for later phases: pinned posts still run in a page.
