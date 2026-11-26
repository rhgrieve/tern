# Review of the `calling` crate

rusty-teams at `414be8e` (MIT), reviewed 2026-11-25 for calls in Tern. About 34,600 lines of Rust, on a patched LiveKit
WebRTC. Three reviewers read by area; nothing was called and no account was touched.

**Checked** means read again in the source afterwards. **Reported** means one reviewer traced it and it was not read again.
**Suspected** depends on something not seen, such as the prebuilt WebRTC or Teams' own servers.

## Verdict

Worth building on, not worth taking whole. The hard parts are there and tested: Teams' session descriptions, the signaling
messages, the push formats. The call loop is not: one function of about 2,550 lines that mixes video into every branch, and
most of the lifecycle bugs are in it. Take the protocol, write a small audio-only call loop, and run every call in a process
of its own.

## What was found by building it

| | |
|---|---|
| Compiles | Unchanged, against Tern's patched engine, in about 30 s (12 cores) |
| Tests | 477 unit tests, 5 + 5 session description tests and 1 video loopback pass; 3 ignored |
| Needs | `clang` (a reviewer reports version 21 or newer), and a prebuilt WebRTC of 149 MB packed, 692 MB unpacked |
| Links | `libasound`, GLib and GIO; the rest is loaded when needed |
| Size here | 15 MB of crate (13 MB is a model for background blur) and 15 MB of native sources (8 MB is a noise model) |

## Stops a call from working for us

1. **The relay request is refused by our own engine (checked).** Every call first reads
   `teams.microsoft.com/trap/tokens` (`relay.rs:10`), which `session/src/native.rs:255` left to a web page.
   **Solved 2026-11-25:** with that one path let through, Teams answered HTTP 200 to a token alone, with one relay
   credential for a week (`tern-cli probe-relay`). The credential was not shown or kept, and no relay was used yet.
2. **Europe is built in (checked).** `signaling.rs:18,20,122,1038`, `relay.rs:11`, `host.rs:6`, `meeting_options.rs:177`.
   Our account is in `amer`. The likeliest result is refusals or media by way of Europe.
   **Solved 2026-11-26:** `TenantRouting::for_account` takes region, partition and the conversation host from the authz
   answer (`tern-cli probe-regions`: `amer`, `amer01`, `api-noam.flightproxy.teams.microsoft.com`), and the relay is
   `gateway-us.az.relay.teams.cloud.microsoft` for `amer`. Chat addresses were already moved by our engine's
   `with_region`. Left in Europe: meeting options (`teams.microsoft.com/api/mt/emea`), which Tern leaves out.
3. **Two things that ring.** The engine registers an endpoint that rings; the ring notice of phase 4 registers another.
   Both would show the call. The daemon has to hand over: it learns of calls, the call process answers them.

## Security

| Finding | State | For Tern |
|---|---|---|
| The check of where links lead can be passed with `https://evil.example#.teams.microsoft.com/` and similar (`signaling.rs:146`) | Checked in the source | **Fixed:** addresses are parsed; user names, passwords and ports refused |
| A forged push or callback can name any address our token rule allows, the chat service included, and a POST or DELETE goes there without the user doing anything (`engine.rs:570`, `callLeg` links) | The POST checked, the rest reported | **Fixed:** only Teams' call proxy and the call services `cc`, `conv`, `mc`, `broker`, `css` |
| The caller decides how much memory is used: `x-multi-stream` in an offer is a count without a limit (`sdp.rs:1311`) | Checked. A test of it used up this computer's memory | **Fixed:** at most 64 lines in all, refused before anything is made |
| With noise suppression on Low, a meeting begins shown as muted while the microphone is sent (`mod.rs:878` mutes one track, `device_track` publishes the other) | Checked in the source, not heard | **Fixed:** both muted |
| Candidates of 1:1 calls tell the other side every local, VPN and IPv6 address | Reported | Offer a relay-only setting |
| `CALLING_AUDIO_DUMP` writes microphone and far end to disk, in releases too; `CALLING_TRACE` prints names | Reported | Remove them; clear the environment |
| Captions and recording send a Skype token to an address another participant can name | Reported | Leave both out |
| The prebuilt WebRTC is downloaded without a checksum, and cargo picked a newer helper (0.3.21) than upstream locks (0.3.19), which names **another** WebRTC build | Checked | **Fixed:** helper pinned to 0.3.19 (`webrtc-89d790b`); the build uses our checked copy, which `scripts/webrtc.sh` fetches |

No telemetry, automatic answering, recording or admitting was found. The published WebRTC crates are authentic: the
originals match the registry's checksums, and what upstream changed on Linux reads as honest. `desktop_capturer.cpp` was
only read by a reviewer. Upstream's patch notes list five changes and leave out several more; `libwebrtc` has no notes.

## Ends of calls

| Finding | State |
|---|---|
| Dropping the handle does not hang up: the call keeps a sender to itself (`control.rs:217`) | Checked |
| Hanging up while a call is being placed sends nothing: what would be told is stored only afterwards (`mod.rs:729`). The other side may keep ringing | Checked |
| A crash in the call leaves microphone and polling on: nothing releases them by itself | Checked that no `Drop` does |
| `stop()` ends neither calls nor rings; nothing ends a ring after a time | Reported |
| Hanging up waits behind other requests, in the worst case for minutes | Reported |
| Accepting a second call queues it, and it answers itself when the first one ends | Reported |
| No restart after a network change, and nothing for sleep or lock | Reported |

A process for each call, killed when it ends or the window goes, removes most of these. The rest belong into our loop.

## Sound, screen and camera on Linux

| Finding | State |
|---|---|
| A microphone that did not start counts as started (`devices.rs:303`); Windows retries | Checked |
| Devices are told apart by name only, and looked at once a second instead of watched | Reported |
| "Off" and "Music" take another path (ALSA), which needs `pipewire-alsa` | Reported |
| Sharing sound plays the others back to themselves | Reported |
| No microphone at all, or PipeWire starting again: may hang or stay silent | Suspected |
| Received video is made small (640 pixels), and rotation is ignored | Reported |
| Closing while the screen picker answers may use freed memory | Suspected |
| The screen picker takes GLib's main context on a thread of its own: never in the window's process | Checked |
| Camera is opened directly, not through the portal: the Flatpak would need all devices | Reported |
| Decoding is done by the processor; VA-API only encodes | Reported |

## Strange things

- `next_call_dump_dir` deletes whole folders, whatever they are, in the folder it is given. Checked. Never give it one.
- Says it is Chrome 155 on Windows, with fixed version numbers, which will age. Checked.
- Keeps the computer awake as "Rusty Teams". Checked.
- `examples/` act on the real account: one deletes recordings from OneDrive, one creates meetings. Leave them out.
- A comment that belongs to declining a call is on choosing the camera.

## What was never tried, by upstream's own account

Incoming calls and 1:1 calls with a second person, 1:1 video, hold and transfer, breakout rooms, bad networks. Meetings with
up to nine guests worked, mostly on Windows. On Linux only the automatic noise suppression is said to have been in a real
call. The formats of pushes and rings come from reading Teams' web code.

## Upstream

902 of about 1,000 commits are by one person, mostly with AI, which upstream says itself. It moves fast: 23 commits since
our pin, changing at least three files in `calling` (GitHub's list of changes was cut off). Pull requests from others are taken; they must name the model used, stay
small, and say what was really run. Worth offering, smallest first:

1. A limit on `x-multi-stream`
2. Muting both tracks at the start
3. Parsing addresses before trusting them
4. A microphone that did not start
5. Hanging up when the handle is gone
6. Regions as settings instead of Europe

The first three are safety problems: ask for a private way to tell them first.

## What to do

| Step | Proves | Touches |
|---|---|---|
| 1. Read `trap/tokens` once: **done, works** | The first obstacle is gone | Read one relay credential |
| 2. Region settings, the fixes above, `tern-call` without a window: **done**. `tern-call echo` places one call in a process of its own, sound only, through a guard that refuses everything but the call's own requests | It builds small | Nothing |
| 3. Call the Echo test: **done 2026-11-26, works** with a tone: media after 1.5 s, direct, 994 packets out, 427 in, 9 s of sound back, hung up cleanly, nothing refused; then with the microphone: the greeting and the user's own voice were heard back, 23 s of sound, ended by the bot as usual | Signaling, relay, sound both ways | Makes a chat with the Echo bot; perhaps a line in the call list |
| 4. Call a colleague, then be called | What upstream never tried | A real call |
| 5. Join a meeting for sound | Meetings | A real meeting |

Step 3 decides whether phase 5 goes on. It is `tern-call echo` (add `--tone` to send a tone instead of the
microphone); a request the guard refuses is printed as `refused:` and ends the call, which is how a missing one shows.
For now `tern-call` still runs upstream's call loop; whether to write our own follows from what step 3 shows. Leave out: noise model, background blur, camera, screen, captions, recording,
whiteboard, breakout rooms, transfer. That drops ONNX, tract, nokhwa and cpal.

## Not read

Bodies of `sdp.rs`, video sending, effects, most of the middle of `call/mod.rs`, WebRTC's own C++ sources, and the parts of
upstream's app around the crate.
