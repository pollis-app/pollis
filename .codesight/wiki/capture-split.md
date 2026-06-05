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

`screencapturekit` 2.x ships a buggy `PickerResult.init(filter:)` Swift
bridge that does `[filter valueForKey:@"includedDisplays"]` on
`SCContentFilter`, a class without that key. Every selection from the
system `SCContentSharingPicker` throws `NSUnknownKeyException` on
replayd's XPC queue. Rust `catch_unwind` does **not** catch an ObjC
`@throw` — it reaches `std::terminate` → `abort()`. Confirmed on macOS
14.7. **No system picker is used.** Pollis enumerates with
`SCShareableContent.current()` and renders its own picker — the
industry-standard path used by Slack, Discord, Zoom and OBS — which
never goes through the broken code.

The helper subprocess is still load-bearing as defense-in-depth: SCK
has shown it'll throw and any future throw site stays isolated to the
helper, never killing the host app.

### Layout

`pollis-capture-macos/` mirrors `pollis-capture-linux/`:

- `src/main.rs` — non-macOS stub + `mod macos`.
- `src/macos.rs` — connects to the parent socket, enumerates available
  displays + windows via `SCShareableContent`, sends the list back to
  the parent (`MSG_SOURCES`), waits for the parent's `MSG_SELECT`,
  builds an `SCContentFilter` from the chosen display/window, and runs
  the `SCStream`. The `SCStreamOutputTrait` frame handler packs BGRA
  (== little-endian ARGB == BGRx) into the shared protocol.
- **No `SCContentSharingPicker`.** The system picker's
  `PickerResult.init(filter:)` Swift bridge does
  `[filter valueForKey:@"includedDisplays"]` on a key
  `SCContentFilter` doesn't expose, throws `NSUnknownKeyException`,
  and kills the helper on **every** selection — confirmed on macOS
  14.7. The industry-standard answer (used by Slack, Discord, Zoom,
  OBS): enumerate via `SCShareableContent.current` and render an
  in-app picker. That's what Pollis does. Less Apple gloss, but
  works.
- Parent death watch: macOS has no `PR_SET_PDEATHSIG`; the helper polls
  `getppid()` and exits if reparented to launchd.

### Packaging

The helper sidecar packaging story is in transition: the legacy `src-tauri/` build pipeline ships a working sidecar today, while the active Electron pipeline's strategy (extraResource via electron-builder, or fold-in to `pollis-node`) is still being decided. Both paths are documented here so the helper can be wired into the Electron build without re-discovering the constraints.

**Legacy Tauri shell (still produces a runnable helper):**
- `src-tauri/tauri.macos.conf.json`: `externalBin`
  `binaries/pollis-capture-macos`, Developer-ID signed, **same team
  9JF7WWYMU2**.
- `src-tauri/build.rs` builds the per-OS helper crate and stages it at
  `src-tauri/binaries/<helper>-<triple>` automatically on every cargo
  build of the `pollis` crate. Skips when the file is already present so
  CI's pre-built Linux artifact (from ubuntu-24.04, PipeWire 1.0) is
  reused on the app job (ubuntu-22.04). No shell script wrapper — runs
  uniformly for `cargo check`, `tauri dev`, and `tauri build` on macOS
  and Linux. Windows is skipped (WGC is in-process).

**Electron shell (active path; helper packaging is a TODO in `electron/build/electron-builder.yml`):**
- The decision pending in the config TODO is whether to ship the helper as an `extraResources` entry next to `pollis-node`, embed it inside `pollis-node`, or drop the separate binary entirely if `pollis-node` performs the capture itself. Until that lands, screenshare under the Electron build will not have a packaged helper.

### Picker UX

On macOS the picker is a Pollis component (`ScreenSharePicker.tsx`),
not the macOS system picker. It opens in-place inside the voice
channel view (no modal — project rule), showing a tabbed grid of
displays and windows. The user picks one, the frontend sends
`Selection` to the parked helper via `start_screen_share`, the helper
builds the `SCContentFilter` and starts the `SCStream`. Cancel returns
to the participant grid.

On Linux the system portal (`xdg-desktop-portal`) is the consent gate
and **is** the picker; on Windows the WGC picker plays the same role.
The frontend calls `enumerate_screen_sources` first and, if the
returned list is empty (the backend's signal that this platform
handles selection itself), goes straight to `start_screen_share(null)`.

### Wire protocol (macOS extension)

`pollis-capture-proto` carries two extra message types just for the
macOS picker handshake:

- `MSG_SOURCES (0x03)` helper → parent: JSON `SourceList` of the
  enumerated displays + windows.
- `MSG_SELECT (0x04)` parent → helper: JSON `Selection` —
  `{kind: "display" | "window", id: <CGDirectDisplayID | CGWindowID>}`.

Linux helpers never send `MSG_SOURCES` and never read `MSG_SELECT`.
The same opcodes are reserved in the proto crate so both helpers
share one wire format definition.

## Electron publish-path codec policy

Under Electron, capture + encode + publish all happen in Chromium
(`screenShareSession.ts` → `livekitView.publishScreenShare`), bypassing
the Rust helper pipeline above. The codec is chosen **per-machine at
publish time** by `frontend/src/screenshare/codecPolicy.ts`:

- **Hardware H.264 when present.** `pickScreenShareCodec()` scans
  `RTCRtpSender.getCapabilities('video')` for an H.264 entry whose
  `profile-level-id` does **not** end in `1f` (level > 3.1). Software
  OpenH264 only advertises baseline Level 3.1, so any higher-level entry
  (e.g. High/5.2 `640034`) is itself proof a hardware encoder
  (VideoToolbox / Media Foundation / VAAPI) is registered. When found,
  that exact capability is pinned first via `setPreferredVideoCodec()` in
  `sdpMunger.ts`, so SDP negotiation offers it ahead of baseline 3.1 and
  Chromium engages the hardware encoder at high resolution/framerate
  ("uncap the negotiation").
- **Software VP8 fallback.** If only baseline `…1f` H.264 entries exist
  (typical on GPU-less Windows/VMs and most Linux), publish VP8 — it has
  no level cap and we control its bitrate/framerate, a better fallback
  than baseline-3.1 H.264 (which can't do 1080p).

Cross-platform: macOS always gets HW H.264 (VideoToolbox); Windows
usually does (any GPU machine); Linux often falls back to VP8. Decode is
never a problem — every Pollis client is the same Chromium with H.264
bundled, so any client decodes any other's stream regardless of platform.

The pin reorders **within** the AV1-stripped codec list `sdpMunger.ts`
already enforces, so the PT=35 BUNDLE collision that originally forced
VP8 stays closed. `VITE_POLLIS_SCREENSHARE_CODEC` = `h264` | `vp8`
overrides the auto-detection for A/B testing. See issue #364.

## Parent-side pipeline (unchanged, shared by all paths)

`pollis-core/src/commands/screenshare.rs`:

- `enumerate_screen_sources` (macOS) — binds a Unix socket, spawns the
  helper, reads the `MSG_SOURCES` list, parks the helper in
  `picker_session` waiting for the upcoming `Select`, returns the
  list to the frontend.
- `cancel_screen_share_picker` — kills a parked picker helper when the
  user backs out of the in-app picker without selecting.
- `start_screen_share(selection)` — reuses the parked picker helper if
  present (macOS) or spawns a fresh helper (Linux portal path). On
  macOS sends `MSG_SELECT` with the user's pick, then reads `Format`
  from the same helper. Linux skips the Select (no such message). On
  both, creates the LiveKit `NativeVideoSource` + track, publishes,
  spawns the reader task.
- Reader task — `read_msg` loop: FPS cap, `convert_and_cap`
  (libyuv ARGB→I420 + 1080p downscale), `source.capture_frame`,
  self-preview, 2 s stall heartbeat.
- `stop_screen_share` — Linux + macOS share one teardown: abort the
  reader task, kill the helper (killing the macOS helper IS the SCK
  stop + picker deactivate, since SCK now lives entirely in it).

## Follow-up TODOs

- **#281 Phase 2**: X11 XDamage (changed-region capture).
- **#281 Phase 3**: X11 cursor via XFixes `GetCursorImage`.
- **#281 Phase 4**: X11 HiDPI / fractional scaling; multi-monitor edge
  geometry.

## Key files

- `pollis-capture-proto/src/lib.rs` — shared wire protocol.
- `pollis-capture-linux/src/linux.rs` — session probe + Portal/X11
  dispatch.
- `pollis-capture-linux/src/x11.rs` — v1 xcb/SHM/RandR backend.
- `pollis-capture-macos/src/macos.rs` — SCShareableContent enumeration
  + SCContentFilter + SCStream/handler.
- `frontend/src/components/Voice/ScreenSharePicker.tsx` — in-app picker
  UI (macOS path).
- `pollis-core/src/commands/screenshare.rs` — shared parent pipeline,
  deny-vs-unsupported split.
- `frontend/src/screenshare/screenShareSession.ts` —
  `local_unsupported` event + distinct error message.
- `src-tauri/tauri.linux.conf.json`, `src-tauri/tauri.macos.conf.json`
  — sidecar packaging in the legacy Tauri build.
- `src-tauri/build.rs` — auto-builds + stages the per-OS helper sidecar
  during the legacy Tauri shell's cargo build.
- `electron/build/electron-builder.yml` — active build config; helper
  packaging strategy is the open TODO described in "Packaging" above.
