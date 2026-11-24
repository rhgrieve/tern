# Vendored engine

From https://github.com/danieldaeschle/rusty-teams (MIT, see `LICENSE`), commit
`414be8e2046df79b7b42cd49ae0bb555f8a1ca18`. Crates: `session`, `chatsvc`, plus the one asset
`app/assets/emoticons.tsv`. Examples are left out. `tern.patch` is the full difference.

| Change | Where | Why |
|---|---|---|
| `TokenProvider`, `Session::with_provider`, `Session::access_token` | `session/src/provider.rs`, `session.rs`, `native.rs` | Tokens without a browser |
| A token is only sent to hosts of its audience | `session/src/provider.rs` | Upstream attaches it to any https URL |
| Page-only requests answer `Error::PageOnly` | `session.rs`, `page_script.rs` | Fail clearly instead of waiting for a page |
| Push socket takes its token from `access_token` | `chatsvc/src/realtime/native.rs` | Same |
| `/ups/` counts as a bearer path | `session/src/native.rs` | Upstream sends the regional presence service through a page |
| `PresenceService::at(base)` | `chatsvc/src/presence.rs` | Regional presence host |

Known for later phases: parts of `chatsvc` assume an EU region (`DEFAULT_REGION`), and pins, AMS
uploads and downloads still run in a page.
