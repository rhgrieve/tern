# Vendored engine

From https://github.com/danieldaeschle/rusty-teams (MIT, see `LICENSE`), commit
`414be8e2046df79b7b42cd49ae0bb555f8a1ca18`. Crates: `session`, `chatsvc`, `graph`, `core`, `store`, plus the one asset
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
| `open_read_only` | `store/src/store.rs` | Readers beside the daemon |
| Recent conversation states between hourly full reads | `chatsvc/src/conversations.rs`, `core/src/{engine,remote,sidebar_sync}.rs` | Upstream reads every conversation on every refresh |
| Skype-token downloads without a page | `session/src/skype.rs`, `chatsvc/src/ams.rs` | Most inline pictures need it |

Known for later phases: pins and AMS image uploads still run in a page.
