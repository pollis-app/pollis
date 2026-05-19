# Screen-Capture Helper Split

How Pollis captures the local screen for screen-share, and why capture
runs in a **per-platform helper subprocess** on Linux and macOS but
in-process on Windows.

Covers issues **#281** (Linux: no portal backend on Cinnamon/MATE/XFCE)
and **#283** (macOS: uncatchable Objective-C throw from ScreenCaptureKit).

## TL;DR

| Platform | Where capture runs | Mechanism | Why |
|----------|--------------------|-----------|-----|
| Linux (Wayland + portal) | `pollis-capture-linux` subprocess | xdg-desktop-portal ScreenCast + PipeWire | libpipewire can't co-link with libwebrtc+cpal+webkit2gtk |
| Linux (X11 session) | `pollis-capture-linux` subprocess | xcb + MIT-SHM + RandR | No ScreenCast portal backend exists on many DEs |
| Linux (Wayland, no portal backend) | n/a — structured error | — | Genuinely unsupported; surfaced distinctly from "denied" |
| macOS | `pollis-capture-macos` subprocess | ScreenCaptureKit (SCContentSharingPicker + SCStream) | An ObjC `@throw` from SCK is uncatchable from Rust and hard-kills the host process |
| Windows | in-process | `windows-capture` (WGC) | Clean in-proc linkage, no analogous hazard |

All three capture helpers / paths emit frames over **one shared wire
protocol** (`pollis-capture-proto`). The parent-process pipeline
(socket reader, FPS cap, libyuv ARGB→I420, LiveKit publish, 2 s stall
heartbeat) is identical regardless of where frames originate.

## The shared protocol — `pollis-capture-proto`

A tiny, platform-free workspace crate: `pollis-capture-proto/src/lib.rs`.
It is the single definition of the Unix-socket frame protocol.

```
message := [ u8 type ][ u32 LE payload_len ][ payload ]

0x01 Format   payload = [u32 LE width][u32 LE height]
0x02 Frame    payload = [u32 w][u32 h][u32 stride][i64 ts_us][BGRx...]
0xFF Error    payload = utf-8 message
```

- Encoders: `encode_format`, `encode_frame_header`, `encode_error`,
  `write_msg`. Used by `pollis-capture-linux` and `pollis-capture-macos`.
- Decoder: `read_msg` — used by the parent in
  `pollis-core/src/commands/screenshare.rs` (both the initial Format
  read and the streaming reader task).
- Wire bytes are **unchanged** from the original hand-rolled
  encode/decode that lived separately in `pollis-capture-linux` and
  `screenshare.rs`; the refactor only centralized them. Round-trip
  tests pin the byte layout and opcodes.

Reused by: `pollis-capture-linux`, `pollis-capture-macos`, `pollis-core`.

## #281 — Linux: two backends, routed by session type

### Root cause

`pollis-capture-linux` used `ashpd` → xdg-desktop-portal's `ScreenCast`
interface. That interface needs a **DE-specific portal backend**.
GNOME/KDE/wlroots ship one; **Cinnamon/MATE/XFCE do not** —
`xdg-desktop-portal-gtk` does NOT implement ScreenCast. On Mint/Cinnamon
the portal call errors *before any picker UI*, and the old helper
collapsed "no backend / portal error" into the same path as "user
denied" → looked like "denied without a prompt". Kernel version is
irrelevant.

### Why not X11 grab everywhere

Under Wayland, XWayland gives X11 clients a **private root window**, not
the real composited screen. `XShm`/`XGetImage` against it returns black.
So this must be a **two-backend, session-type-routed** design — not a
DE-name switch (GNOME and KDE also ship X11 sessions, which a DE-name
switch would mis-route).

### Routing probe

Decided once at capture start, in `pollis-capture-linux/src/linux.rs`
(`probe_backend`):

1. **Session type**: `$XDG_SESSION_TYPE` (`x11` / `wayland`), with
   `$WAYLAND_DISPLAY` / `$DISPLAY` corroborating.
2. **Portal availability** (Wayland only): is
   `org.freedesktop.portal.ScreenCast` actually present (probed via
   `Screencast::available_source_types`)?

| Session | Portal ScreenCast | Backend |
|---------|-------------------|---------|
| Wayland | present | `Portal` — ashpd + PipeWire (unchanged) |
| X11 | (not probed) | `X11` — xcb/SHM/RandR |
| Wayland | absent | `Unsupported` — structured `0xFF` error |

The `Unsupported` case sends a `0xFF` with an `unsupported:` prefix; the
parent maps that to a new `ScreenShareEvent::LocalUnsupported` (distinct
from `LocalError`), and the frontend shows "your desktop environment has
no screen-sharing backend" — NOT "grant permission". The portal path's
deny-vs-error collapse was also split (cancel / unsupported / genuine
failure are now distinguished in `screenshare.rs`).

### v1 X11 backend (`pollis-capture-linux/src/x11.rs`)

Shippable, deliberately minimal:

- **xcb + MIT-SHM** (SHM is non-negotiable — plain `XGetImage` is
  unusably slow at 1080p).
- **RandR** enumeration: capture one monitor (RandR primary, else first
  active CRTC, else whole root), not the spanned root.
- No per-window consent picker — X11 has no consent model
  (monitor/full-screen only).
- v1 = **full-framebuffer SHM copy per tick** (correct; heavier on weak
  CPUs).
- Emits the exact shared protocol; the parent reader / FPS cap / libyuv
  / LiveKit path is untouched.

Pixel format: a 24/32-bpp TrueColor `ZPixmap` on a little-endian X
server is byte-order BGRX — exactly what the parent's `argb_to_i420`
expects. The backend rejects big-endian / non-24/32-bpp servers loudly
rather than ship miscolored frames.

#### X11 follow-up phases (OUT of v1, documented TODOs — not blockers)

- **Phase 2**: XDamage — copy only changed regions.
- **Phase 3**: cursor compositing via XFixes `GetCursorImage`.
- **Phase 4**: HiDPI / fractional scaling; multi-monitor edge geometry.

## #283 — macOS: SCK in a helper subprocess (Phase 2 only)

### Root cause

`screencapturekit` 2.x can `@throw` an Objective-C
`NSUnknownKeyException` from inside `SCContentSharingPicker`'s selection
delegate (`valueForKey:` on a window whose owning app/bundle lacks the
key), dispatched on **replayd's XPC queue**. Rust `catch_unwind` does
**not** catch an ObjC `@throw` — it reaches `std::terminate` →
`abort()`, killing the whole app. Upstream has no known fix (checked
2026-05-19).

### Scope here: Phase 2 only

- **Phase 0** (symbolicate the crash with a CI dSYM) and **Phase 1**
  (`[patch.crates-io]` fork of `screencapturekit`, upstream PR) require
  external artifacts / a crate fork and were **NOT done**. They are
  still owed (see TODOs).
- **Phase 2 (done)**: durable resilience — isolate SCK in a subprocess
  exactly like Linux. An ObjC terminate then kills only the helper; the
  parent observes the socket close / non-zero exit (the existing 2 s
  stall heartbeat covers mid-stream death) and surfaces a structured
  error. This retroactively de-risks **every** SCK call.

### Layout

`pollis-capture-macos/` mirrors `pollis-capture-linux/`:

- `src/main.rs` — non-macOS stub + `mod macos`.
- `src/macos.rs` — connects to the parent socket, drives
  `SCContentSharingPicker` + `SCStream`, and the `SCStreamOutputTrait`
  frame handler packs BGRA (== little-endian ARGB == BGRx) into the
  shared protocol. This is the picker/stream/handler logic **extracted
  from** `pollis-core/src/commands/screenshare.rs` (the in-process SCK
  path is retained only as never-compiled reference behind the
  `legacy_inproc_sck` feature; `screencapturekit` was removed from
  `pollis-core`'s dependencies).
- Parent death watch: macOS has no `PR_SET_PDEATHSIG`; the helper polls
  `getppid()` and exits if reparented to launchd.

### Packaging

- `src-tauri/tauri.macos.conf.json`: `externalBin`
  `binaries/pollis-capture-macos`, Developer-ID signed, **same team
  9JF7WWYMU2**.
- `scripts/build-capture-helper.sh` is now platform-aware (Linux →
  linux helper, macOS → macos helper) and the macOS CI job stages the
  helper before `tauri-action` so it is signed + bundled.

### OPEN RISK (not silently assumed)

`SCContentSharingPicker` must be driven from a process with a
**window-server connection**. Whether the system picker presents
correctly from a **helper process** (vs. the main app, which has the
foreground GUI activation) is **UNVERIFIED** — it was slated to be the
Phase 0 spike, which is out of scope. This is flagged prominently in
`pollis-capture-macos/src/main.rs` and `src/macos.rs`. If the picker
does NOT appear from the helper, the fallback is one of:

1. The **parent** drives the picker and hands the helper an
   already-selected `SCContentFilter` (the helper then only owns
   `SCStream` + the frame handler — still isolates the streaming SCK
   surface, but the picker selection delegate would run in-process,
   only partially de-risked).
2. Revert to in-process SCK with the Phase 1 `[patch.crates-io]` fork.

Do not treat the macOS split as proven end-to-end until this is
verified on a real macOS host.

## Parent-side pipeline (unchanged, shared by all paths)

`pollis-core/src/commands/screenshare.rs`:

- `start_screen_share` (`#[cfg(any(target_os = "linux",
  target_os = "macos"))]`) — binds a Unix socket, spawns the
  per-platform helper (`capture_helper_name()` →
  `locate_capture_helper()`), reads the `Format` via
  `pollis_capture_proto::read_msg`, creates the LiveKit
  `NativeVideoSource` + track, publishes, then spawns the reader task.
- Reader task — `read_msg` loop: FPS cap, `convert_and_cap`
  (libyuv ARGB→I420 + 1080p downscale), `source.capture_frame`,
  self-preview, 2 s stall heartbeat.
- `stop_screen_share` — Linux + macOS share one teardown: abort the
  reader task, kill the helper (killing the macOS helper IS the SCK
  stop + picker deactivate, since SCK now lives entirely in it).

## Follow-up TODOs

- **#283 Phase 0**: symbolicate the SCK crash with a CI-produced dSYM.
- **#283 Phase 1**: `[patch.crates-io]` fork of `screencapturekit`;
  upstream PR for the `valueForKey:` `NSUnknownKeyException`.
- **#283**: verify `SCContentSharingPicker` presents from the helper
  process (the OPEN RISK above) before treating the split as proven.
- **#281 Phase 2**: X11 XDamage (changed-region capture).
- **#281 Phase 3**: X11 cursor via XFixes `GetCursorImage`.
- **#281 Phase 4**: X11 HiDPI / fractional scaling; multi-monitor edge
  geometry.

## Key files

- `pollis-capture-proto/src/lib.rs` — shared wire protocol.
- `pollis-capture-linux/src/linux.rs` — session probe + Portal/X11
  dispatch.
- `pollis-capture-linux/src/x11.rs` — v1 xcb/SHM/RandR backend.
- `pollis-capture-macos/src/macos.rs` — SCK picker/stream/handler.
- `pollis-core/src/commands/screenshare.rs` — shared parent pipeline,
  deny-vs-unsupported split.
- `frontend/src/screenshare/screenShareSession.ts` —
  `local_unsupported` event + distinct error message.
- `src-tauri/tauri.linux.conf.json`, `src-tauri/tauri.macos.conf.json`
  — sidecar packaging.
- `scripts/build-capture-helper.sh` — platform-aware helper build.
