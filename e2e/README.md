# E2E tests (real Tauri app, driven via WebDriver)

Drives the **actual native desktop app** — the real WebKitGTK WebView inside the
Tauri shell, talking to the real Rust core over Tauri IPC — not the browser
build of the frontend.

Three scripts, sharing `lib/harness.js` for the tauri-driver/WebKitWebDriver
plumbing:

| script | what it proves | needs DS? | needs Turso? |
|---|---|---|---|
| `smoke.js` | app launches, login screen renders | no | no |
| `e2e.js` | full signup: email → OTP → secret key → PIN → app-ready | yes | yes (writable) |
| `invalid-otp.js` | a wrong OTP code is rejected with an inline error, doesn't advance | yes | yes (writable) |
| `two-client.js` | two isolated app instances; a message from A converges into B's UI | yes | yes (writable) |
| `two-client-dm-reply.js` | bidirectional 1:1 DM — B replies and A sees it (reverse leg of two-client) | yes | yes (writable) |
| `two-client-channel.js` | A creates a group + text channel, invites B, B accepts, A posts, B receives | yes | yes (writable) |
| `two-client-voice-channel.js` | A + B join a group voice channel, both see 2 participants, A leaves, B sees the drop | yes | yes (writable) + LiveKit + audio |
| `two-client-call.js` | two instances place + accept a real 1:1 call; each sees the other in the call | yes | yes (writable) + LiveKit + audio |
| `two-client-camera.js` | two instances in a call; A turns its webcam on, B sees A's remote camera tile | yes | yes (writable) + LiveKit + audio + virtual camera |
| `two-client-screenshare.js` | two instances in a call; A shares its screen (X11/Xvfb capture), B sees A's remote screenshare tile | yes | yes (writable) + LiveKit + audio |

```bash
pnpm --filter @pollis/e2e smoke        # or: node e2e/smoke.js       (fast, no deps)
pnpm --filter @pollis/e2e test         # or: node e2e/e2e.js         (full signup flow)
pnpm --filter @pollis/e2e invalid-otp  # or: node e2e/invalid-otp.js
pnpm --filter @pollis/e2e two-client   # or: node e2e/two-client.js  (needs backend up first)
pnpm --filter @pollis/e2e two-client-dm-reply       # bidirectional DM (needs backend)
pnpm --filter @pollis/e2e two-client-channel        # group text-channel convergence (needs backend)
pnpm --filter @pollis/e2e two-client-voice-channel  # group voice join/leave (needs backend + LiveKit + audio)
pnpm --filter @pollis/e2e two-client-call  # or: node e2e/two-client-call.js  (needs backend + LiveKit + audio up first)
pnpm --filter @pollis/e2e two-client-camera  # or: node e2e/two-client-camera.js  (needs backend + LiveKit + audio + virtual camera up first)
pnpm --filter @pollis/e2e two-client-screenshare  # or: node e2e/two-client-screenshare.js  (needs backend + LiveKit + audio up first)
```

`smoke.js` is the one to reach for in CI or as a quick "did I break launch"
check — `checkStoredSession()` (`frontend/src/App.tsx`) resolves the
logged-out path entirely from local Tauri commands (`getSession` /
`listKnownAccounts`), so it never touches the delivery service or Turso. The
other two need the local delivery service + a writable disposable Turso DB
(see Prerequisites below).

Proof screenshots land in `e2e/artifacts/` (`smoke-auth-screen.png`,
`01-auth-screen.png`, `99-app-ready.png`, `invalid-otp-error.png`); on failure
any script writes `FAIL.png` + `FAIL.html` and prints the testids that were
actually on screen.

## How it works

Each script stands up its own local stack (`smoke.js` skips the delivery
service entirely), then drives the app with raw `webdriverio` `remote()`
calls via helpers in `lib/harness.js` (`reap`, `waitPort`, `waitTestId`,
`clickTestId`, `typeCode`, `makeShot`, ...):

1. **Vite dev server** on `:5173`. The debug Tauri binary loads its UI from
   `devUrl` (Tauri only embeds the frontend in release builds — and even this
   repo's release profile keeps `devUrl`). Running the real dev server also sets
   `import.meta.env.DEV`, which skips the launch-time auto-updater gate in
   `App.tsx`. The script pre-warms Vite's lazy module transforms with `curl`
   (a plain Node request gets 404 from Vite 3) so the app's one-shot page load
   never hits a cold server.
2. **Local delivery service** (`pollis-delivery`) on `:8788` with
   `DEV_OTP=000000` and no `RESEND_API_KEY` — OTP email is skipped and the
   fixed code `000000` always verifies. Writes go to the disposable test DB.
3. **tauri-driver** (`:4444`) → **WebKitWebDriver** (`:4445`) → launches
   `target/debug/pollis` with: dev creds for R2/LiveKit, the **writable test
   DB** (`.env.test`) for Turso, `POLLIS_DELIVERY_URL` → the local DS, and —
   critically — `WEBKIT_DISABLE_COMPOSITING_MODE=1 GDK_BACKEND=x11`, same as
   `pnpm dev`. Without those, WebKitGTK compositing wedges the WebView on this
   setup: pages half-render and the screenshot endpoint hangs forever.

Nothing talks to prod. No OTP email is ever sent.

## Prerequisites (one-time)

- **System**: `WebKitWebDriver` (ships with webkit2gtk), a display, and
  `cargo install tauri-driver`.
- **Node**: `pnpm install` at the repo root. (webdriverio v8 — v9's undici
  client can't talk to tauri-driver.)
- **Binaries, built with a CLEAN environment**:

  ```bash
  env -u POLLIS_DELIVERY_URL -u TURSO_URL -u TURSO_TOKEN cargo build -p pollis -p pollis-delivery
  ```

  `smoke.js` only launches `target/debug/pollis` (no DS calls before the
  login screen renders), so `-p pollis` alone is enough for it. `e2e.js` and
  `invalid-otp.js` need `pollis-delivery` too.

  This matters: `pollis-core/src/config.rs` reads env with `option_env!`,
  which **bakes values in at compile time** and beats the runtime environment.
  A binary built with `.env.development` sourced has `api-dev.pollis.com`
  baked in and will silently ignore the local-DS override.

- **`.env.test`** — only needed for `e2e.js` / `invalid-otp.js` (`smoke.js`
  reads neither TURSO_URL nor POLLIS_DELIVERY_URL). Must point at a
  **writable, disposable** Turso DB with the
  schema applied. `.env.development`'s Turso token is read-only — signup fails
  on it — which is why all Turso access is redirected to the test DB.

  > **Schema bootstrap is a manual, one-time step** (see "Known limitation"
  > below). Note the Rust `flows` integration harness does **not** help here
  > as of `8b41317` (2026-05-05) — it runs against an embedded local libsql
  > file, not `.env.test`'s real Turso, so running it does nothing for this
  > DB's schema. Use the CI migration runner directly instead:
  >
  > ```bash
  > set -a; . .env.test; set +a
  > bash scripts/db-apply.sh
  > ```
  >
  > It's idempotent (tracks applied versions in `schema_migrations`) and is
  > exactly what CI runs before tests. If a run fails with "duplicate column"
  > or similar on a migration `db-apply.sh` thinks is pending, the DB's
  > `schema_migrations` bookkeeping has drifted from its actual schema
  > (columns/tables applied by hand outside the script at some point) —
  > diagnose with a manual `SELECT sql FROM sqlite_master WHERE name = '…'`
  > against the DB before assuming the migration file itself is wrong.

The scripts still fall back to `.env.test` for a purely local run (see
Prerequisites), but the backend fixtures below are the supported path — they
apply the schema for you and run the DS, so you don't hand-provision anything.

## Automatic backend fixtures (`start-backend.sh` / `stop-backend.sh`)

`e2e/scripts/start-backend.sh` (M1 of #570) stands up the whole backend the
authenticated scripts need — no hand-provisioned Turso, no manual schema step:

1. a real **libsql server** (Turso `libsql-server` / sqld) on `127.0.0.1:8080`,
   in no-auth local mode (so `TURSO_TOKEN` is an ignored placeholder);
2. the **schema**, applied by the repo's real migration runner
   (`scripts/db-apply.sh` over `/v2/pipeline`) — the same thing prod CI runs, so
   there's no second copy of the migration logic to drift;
3. the **real `pollis-delivery` binary** on `127.0.0.1:8788` with
   `DEV_OTP=000000` and Resend disabled (same knobs as the in-process DS in
   `src-tauri/tests/flows/harness.rs`, just the shipped binary).

It then exports `TURSO_URL` / `TURSO_TOKEN` / `POLLIS_DELIVERY_URL` /
`R2_S3_ENDPOINT` / `R2_PUBLIC_URL` / `DEV_OTP` — to `$GITHUB_ENV` in CI and as
`export …` lines on stdout locally. When `POLLIS_DELIVERY_URL` is already set,
`e2e.js` / `invalid-otp.js` use that external DS instead of spawning their own.

Needs Docker (for the libsql image) + `jq`/`curl` (in `scripts/db-apply.sh`).
The R2 values are unreachable **placeholders**: signup never dials R2, but
`Config::from_env()` requires the vars to be present. Real object storage
(MinIO/R2) and LiveKit/media come in later milestones (M3), when a
media/attachment test actually needs them.

### Run the full authenticated flow locally

```bash
# Build both binaries with a CLEAN env (see Prerequisites — option_env! bakes
# compile-time values that would beat the runtime overrides otherwise):
env -u POLLIS_DELIVERY_URL -u TURSO_URL -u TURSO_TOKEN cargo build -p pollis -p pollis-delivery

# Bring up libsql + schema + the real DS, and pull its exported env into your shell:
eval "$(e2e/scripts/start-backend.sh)"

# Drive the app against that backend:
node e2e/e2e.js          # full signup -> app-ready
node e2e/invalid-otp.js  # wrong OTP is rejected

# Tear it all down (idempotent):
e2e/scripts/stop-backend.sh
```

## Two-client convergence (M2)

`two-client.js` (issue #570, M2) is the first **cross-client** test: it launches
**two** isolated real app instances against the **same** backend and proves a
message sent by client A shows up in client B's UI — MLS delivery through the
real renderer, not a unit test of the core.

Isolation is per client: each gets its own `tauri-driver` / `WebKitWebDriver`
port pair (A: `4444`/`4445`, B: `4446`/`4447` — `harness.clientPorts()`) and its
own `POLLIS_DATA_DIR` (separate local SQLite + MLS state + keystore). **One**
shared Vite dev server serves both webviews. The generalized
`harness.startClient({ index, appEnv })` spawns a client's driver + WebDriver
session; `harness.reap()` already pkills every `tauri-driver` / `WebKitWebDriver`
/ `pollis` regardless of port, so both clients are cleaned up.

Conversation path — **1:1 DM request → accept** (the fewest-step stable path):

1. A and B each sign up through the real UI (the same steps `e2e.js` proves).
2. A opens **New Message** and DMs B **by B's email** (`search_user_by_username`
   matches `username OR email`, and signup sets no username) → lands on the DM
   page, adding B to the DM's MLS group.
3. B polls its DMs list for the incoming request (a remote metadata read, not
   MLS-gated) and accepts it → lands on the DM page as a group member.
4. A sends a message carrying a distinctive random token.
5. B polls its message list until A's token text appears, re-opening the DM each
   round to re-fire the (5s-debounced) `ingest_dm_envelopes` pull — there's no
   LiveKit realtime hint in this fixture. Asserted; no fixed sleeps for
   correctness.

Unlike the single-client scripts, `two-client.js` **requires an external
delivery service** (`POLLIS_DELIVERY_URL` must be set) — it never self-spawns
one, because both clients must share exactly one backend. So bring the fixtures
up first:

```bash
# Build both binaries with a CLEAN env (option_env! bakes compile-time values):
env -u POLLIS_DELIVERY_URL -u TURSO_URL -u TURSO_TOKEN cargo build -p pollis -p pollis-delivery

# Bring up libsql + schema + the real DS, and pull its exported env into your shell:
eval "$(e2e/scripts/start-backend.sh)"

# Drive the two clients against that shared backend:
node e2e/two-client.js

# Tear it all down (idempotent):
e2e/scripts/stop-backend.sh
```

Proof screenshots: `two-client-A-ready.png`, `two-client-B-ready.png`,
`two-client-A-sent.png`, `two-client-B-received.png`. On failure it dumps
per-client `A-FAIL.png` / `A-FAIL.html` **and** `B-FAIL.png` / `B-FAIL.html`
(plus each side's on-screen testids) so CI artifacts show both clients.

CI: `.github/workflows/e2e-two-client.yml` (`workflow_dispatch`) wires it end to
end through the same M0/M1 pieces as `e2e-full.yml` — install deps → build
`pollis` + `pollis-delivery` → `start-backend.sh` → `pnpm two-client` via the
`desktop-e2e` composite action → `stop-backend.sh` (`if: always()`).

## Two-client call — audio + LiveKit (M3a)

`two-client-call.js` (issue #570, M3a) is the first **media** test: it places a
real 1:1 **call** on top of an established DM and asserts both clients see each
other in the call. It reuses `two-client.js`'s signup + DM-establish
choreography verbatim, then:

1. A waits until it sees B online **and** accepted — the DM-header phone button
   (`dm-header-call`) only renders when `canCall` (1:1 && `otherAcceptedAt` &&
   `isOtherOnline`) is true. Presence flows from the shared DM LiveKit realtime
   room, so this doubles as proof the realtime path is up.
2. A clicks the phone → the Call page auto-joins the `call-<id>` LiveKit room and
   publishes the mic.
3. B accepts the incoming-call alert in its status bar
   (`status-bar-incoming-call-accept`, delivered over B's inbox realtime room).
4. Each side polls until it renders a `voice-tile-voice-<id>` for the **other**
   user (StageTile.tsx) — i.e. two distinct participants in a 1:1 call. Then A
   hangs up (`call-hang-up`).

Two extra fixtures make a real join possible headless, both idempotent and torn
down by `stop-backend.sh`:

- **`start-audio.sh`** — a headless PulseAudio daemon with a null sink (fake
  speaker) + a virtual source (fake mic), and an `/etc/asound.conf` that points
  ALSA's default PCM at PulseAudio so **cpal** (the Linux ALSA host) opens the
  virtual devices. Without it `join_voice_channel` fails opening the mic. It
  exports `PULSE_SERVER` / `PULSE_SINK` / `PULSE_SOURCE`. Needs `pulseaudio`,
  `pulseaudio-utils`, `libasound2-plugins` (in `install-system-deps.sh`).
- **`start-livekit.sh`** — an ephemeral `livekit/livekit-server:v1.10.0` in
  `--dev` mode (dev key `devkey` / secret `secret`) on loopback under Docker
  `--network host`. It exports `LIVEKIT_URL` (the app dials it —
  `pollis-core/src/config.rs`) and `LIVEKIT_API_KEY` / `LIVEKIT_API_SECRET`
  (the DS mints room tokens with them — `pollis-delivery/src/broker.rs`).
  `start-backend.sh` forwards those to the `pollis-delivery` process.

Run it locally (build binaries with a **clean** env — see Prerequisites):

```bash
# Bring up virtual audio, LiveKit, and the backend, pulling their env into your shell:
eval "$(e2e/scripts/start-audio.sh)"
eval "$(e2e/scripts/start-livekit.sh)"
eval "$(e2e/scripts/start-backend.sh)"

# Drive the two clients placing + accepting a call:
node e2e/two-client-call.js

# Tear it all down (idempotent — stops the DS, both containers, and PulseAudio):
e2e/scripts/stop-backend.sh
```

Proof screenshots: `two-client-call-A-ready.png`, `two-client-call-B-ready.png`,
`two-client-call-A-can-call.png`, `two-client-call-B-incoming.png`,
`two-client-call-A-in-call.png`, `two-client-call-B-in-call.png`. On failure it
dumps per-client `A-FAIL.*` / `B-FAIL.*` (plus on-screen testids), same as
`two-client.js`.

CI: `.github/workflows/e2e-two-client-call.yml` (`workflow_dispatch`) — install
deps → build → `start-audio.sh` → `start-livekit.sh` → `start-backend.sh` →
`pnpm two-client-call` via the `desktop-e2e` action → `stop-backend.sh`
(`if: always()`).

## Two-client camera — virtual webcam (M3b)

`two-client-camera.js` (issue #570, M3b) is the camera slice of the media
milestone, and directly validates the #568 camera-parity work end to end: it
reuses the M3a call choreography verbatim to get A + B into a connected call,
then A turns its **webcam** on and B asserts A's **remote camera tile** renders.

1. A + B sign up, A DMs B, B accepts, A places the call, B accepts — identical
   to `two-client-call.js`; both converge on two participants.
2. A clicks the camera toggle (`voice-bar-camera-button`, or the stage tray's
   `voice-tray-camera`). With exactly one camera on the runner (`/dev/video0` is
   the only node), `toggleCamera()` starts it directly — no picker — capturing
   the loopback device and publishing a `TrackSource::Camera` track into the
   call room.
3. Sanity on A: A's own local self-preview tile
   (`remote-video-tile-__local_camera_preview__`) mounts — proof A's
   capture+publish engaged, isolating an A-side capture failure from a B-side
   delivery one.
4. **ASSERT on B**: poll until A's remote camera tile renders — a
   `remote-video-tile-<trackKey>` element nested inside A's participant tile
   (`voice-tile-voice-<A>`), excluding the local-preview keys and excluding
   screenshare feeds (a screenshare tile also carries a `voice-tile-stream-stats-`
   badge; a camera face never does). On B a remote webcam lands in
   `appStore.cameraRemotes[identity]` → VoiceStage's `cameraTrackKey` →
   StageTile's `RemoteVideoTile` (`remote-video-tile-<trackKey>`, a `<canvas>` on
   the Tauri/WebKitGTK path), so the tile's presence proves A's camera track was
   published, subscribed by B, and mounted. Generous eventual timeout; no fixed
   sleeps for correctness. Then A turns the camera off and hangs up.

One extra fixture over M3a, idempotent and torn down by `stop-backend.sh`:

- **`start-camera.sh`** — loads the `v4l2loopback` kernel module to create a
  fixed capture node (`/dev/video0`) and feeds it a **moving** 1280x720 YUYV422
  test pattern (`ffmpeg`'s `testsrc`) at 30fps — a real, changing signal in a
  format the app's V4L2 path accepts directly (feeding YUYV means the loopback
  offers only YUYV, so the app deterministically takes its YUYV branch). It
  **verifies** `/dev/video0` exists after `modprobe` and that the capture side
  advertises a format, and **fails loudly** with the modprobe/dmesg error
  otherwise. Needs `v4l-utils` + `ffmpeg` (userspace) and the `v4l2loopback-dkms`
  module + kernel headers (all in `install-system-deps.sh`; the kernel-specific
  bits are best-effort there and retried at runtime by `start-camera.sh`).

  > **Known risk — v4l2loopback on hosted runners.** Loading a kernel module
  > (`modprobe`) requires the DKMS build to find `linux-headers-$(uname -r)` and
  > the runner to permit module loading. A GitHub-hosted `ubuntu-24.04` runner
  > **may not** allow this. When it can't, `start-camera.sh` fails at the
  > `modprobe`/`/dev/video0` check with a clear message rather than hanging — a
  > legitimate signal that this flow needs a **self-hosted runner**.

Run it locally (build binaries with a **clean** env — see Prerequisites):

```bash
# Bring up the virtual camera, virtual audio, LiveKit, and the backend:
eval "$(e2e/scripts/start-camera.sh)"
eval "$(e2e/scripts/start-audio.sh)"
eval "$(e2e/scripts/start-livekit.sh)"
eval "$(e2e/scripts/start-backend.sh)"

# Drive the two clients placing a call + turning the camera on:
node e2e/two-client-camera.js

# Tear it all down (idempotent — also kills the ffmpeg feeder + unloads v4l2loopback):
e2e/scripts/stop-backend.sh
```

Proof screenshots: `two-client-camera-A-ready.png`,
`two-client-camera-B-ready.png`, `two-client-camera-A-in-call.png`,
`two-client-camera-B-in-call.png`, `two-client-camera-A-camera-on.png`,
`two-client-camera-B-sees-camera.png`. On failure it dumps per-client `A-FAIL.*`
/ `B-FAIL.*` (plus on-screen testids), same as the other two-client scripts.

CI: `.github/workflows/e2e-two-client-camera.yml` — install deps → build →
`start-camera.sh` → `start-audio.sh` → `start-livekit.sh` → `start-backend.sh` →
`pnpm two-client-camera` via the `desktop-e2e` action → `stop-backend.sh`
(`if: always()`). It carries a **temporary** `push:` trigger on the
`auto/e2e-two-client-camera` branch (marked REMOVE-before-merge) so the module
load can be validated in CI, since v4l2loopback can't run in the dev sandbox.

## Two-client screenshare — X11/Xvfb capture (M3c)

`two-client-screenshare.js` (issue #570, M3c) is the screenshare slice of the
media milestone — the **last** media slice, completing the #568
voice/video/screenshare surface. It reuses the M3a call choreography verbatim to
get A + B into a connected call, then A shares its **screen** and B asserts A's
**remote screenshare tile** renders.

1. A + B sign up, A DMs B, B accepts, A places the call, B accepts — identical
   to `two-client-call.js`; both converge on two participants.
2. A clicks the screenshare toggle (`voice-bar-screenshare-button`, or the stage
   tray's `voice-tray-screenshare`). On Linux `enumerate_screen_sources` returns
   an **empty** list (`start_unix.rs`), so the frontend's `toggleScreenShare`
   skips its in-app picker and calls `screenShareSession.start()` directly — the
   capture helper spawns, probes the session, and starts streaming.
   (The test keeps a defensive branch that selects the first display source if a
   picker ever does appear, but on Linux/X11 it never does.)
3. Sanity on A: A's own local self-preview tile
   (`remote-video-tile-__local_preview__`) mounts — proof A's capture+publish
   engaged, isolating an A-side capture failure from a B-side delivery one. This
   is where the test **fails loudly** if the share never starts.
4. **ASSERT on B**: poll until A's remote screenshare tile renders — a
   `remote-video-tile-<trackKey>` element nested inside A's participant tile
   (`voice-tile-voice-<A>`) that **also** carries a `voice-tile-stream-stats-<A>`
   res·fps badge. That badge is present on a **screenshare** tile
   (`StageTile.tsx`, `hasFeed && !preview`) and **absent** on a camera tile — the
   exact mirror-inverse of the M3b camera assertion (which excludes tiles with
   that badge), so the two stay unambiguous even now that both exist. On B a
   remote screenshare lands as `screenshareOf(p.video)` → VoiceStage's
   `streamTrackKey` → StageTile's `RemoteVideoTile`
   (`remote-video-tile-<trackKey>`, a `<canvas>` on the Tauri/WebKitGTK path), so
   the tile's presence with the badge proves A's screenshare track was published,
   subscribed by B, and mounted. Generous eventual timeout; no fixed sleeps for
   correctness. Then A stops the share and hangs up.

**No extra fixture over M3a.** Unlike the camera slice (which needs a
v4l2loopback `/dev/video0`), screenshare needs nothing but the Xvfb display the
`desktop-e2e` composite action already provides:

> **Linux screen capture is X11, not Wayland.** `pollis-capture-linux` has two
> backends (`src/linux.rs` `probe_backend`): **Wayland** →
> xdg-desktop-portal ScreenCast + PipeWire (needs a live compositor + portal
> backend — **not feasible headless**), and **X11** → xcb + MIT-SHM `XGetImage`
> on the **root window** (`src/x11.rs`, works under Xvfb). The probe routes on
> **session type**: an X11 session (or, headless, `$DISPLAY` set with no
> `$WAYLAND_DISPLAY`) selects `Backend::X11` and never touches the portal or the
> D-Bus session bus. Under `xvfb-run` there's a real X server and no Wayland, so
> the X11 branch is taken; the test additionally pins `XDG_SESSION_TYPE=x11` +
> `GDK_BACKEND=x11` on each app process (`appEnvFor`) to make that explicit. The
> app's own WebKitGTK window sits on that same Xvfb display, so the root grab is
> non-blank — there's real content to publish. The xcb libs (`libxcb1-dev`,
> `libxcb-shm0-dev`, `libxcb-randr0-dev`) are already in `install-system-deps.sh`
> for exactly this capture path.

Run it locally (build binaries with a **clean** env — see Prerequisites). Note a
local X11 desktop takes the same X11 root-capture path; on a Wayland desktop the
app would use the portal instead (a picker dialog appears), which the test's
defensive picker branch handles:

```bash
# Bring up virtual audio, LiveKit, and the backend (no camera fixture needed):
eval "$(e2e/scripts/start-audio.sh)"
eval "$(e2e/scripts/start-livekit.sh)"
eval "$(e2e/scripts/start-backend.sh)"

# Drive the two clients placing a call + A sharing its screen:
node e2e/two-client-screenshare.js

# Tear it all down (idempotent — stops the DS, both containers, and PulseAudio):
e2e/scripts/stop-backend.sh
```

Proof screenshots: `two-client-screenshare-A-ready.png`,
`two-client-screenshare-B-ready.png`, `two-client-screenshare-A-in-call.png`,
`two-client-screenshare-B-in-call.png`, `two-client-screenshare-A-sharing.png`,
`two-client-screenshare-B-sees-share.png`. On failure it dumps per-client
`A-FAIL.*` / `B-FAIL.*` (plus on-screen testids), same as the other two-client
scripts.

CI: `.github/workflows/e2e-two-client-screenshare.yml` — install deps → build →
`start-audio.sh` → `start-livekit.sh` → `start-backend.sh` →
`pnpm two-client-screenshare` via the `desktop-e2e` action → `stop-backend.sh`
(`if: always()`). It carries a **temporary** `push:` trigger on the
`auto/e2e-two-client-screenshare` branch (marked REMOVE-before-merge) so the
headless X11 capture path can be validated in CI, since it can't run in the dev
sandbox.

## CI history — schema bootstrap used to be manual

Before M1, `e2e.js` / `invalid-otp.js` did **not** bring the test DB's schema up
themselves — they assumed a hand-applied schema, which is why only `smoke.js`
(no schema dependency) ran in CI. `start-backend.sh` closes that gap by running
`scripts/db-apply.sh` against a fresh libsql server, so both scripts now run in
CI via `.github/workflows/e2e-full.yml` (below). `smoke.js` remains the fast,
backend-free launch check.

## Gotchas encoded in these scripts (learned the hard way)

- **`.env.development` must not override the local fixture (local-only bug).**
  `appEnvFor` builds the app's env as `{ ...devEnv, ...process.env, <explicit
  overrides> }`. It **must** be that order (fixture wins) and it explicitly
  clears `LOG_DB_*`. `.env.development` sets `LIVEKIT_URL=wss://rtc.pollis.com`
  and a prod `LOG_DB_URL`; if those leak into the app it dials **prod** LiveKit
  (401 → realtime dead → the membership hint that triggers the Welcome poll never
  arrives) and reads Welcomes from the **prod** log DB (empty) — so nothing
  converges. This never bites CI (no `.env.development` there), which is why the
  two-client suite passed in CI but hung locally. If you add a new two-client
  script, copy `appEnvFor` verbatim.
- **The DS shares one libsql DB here** (`LOG_DB_*` unset → single-DB fallback),
  so `start-backend.sh` also applies `migrations-log/` (via
  `apply-log-migrations.py`) — without it the DS's Welcome upsert fails on a
  missing `ON CONFLICT` target and Welcomes silently never persist.

- **Clicks**: WebKitWebDriver's native `element.click()` doesn't reliably fire
  React handlers/form submits here; `clickTestId()` dispatches a DOM click via
  `execute()`. Text inputs work fine with `setValue` (real keystrokes).
- **OTP / PIN**: N separate `<input maxlength=1>` boxes with
  `aria-label="OTP digit K"`; the widget auto-advances and auto-submits.
- **Settle before first command**: the script pauses ~6s after session create
  so the initial Vite page load finishes before the first WebDriver query.
- **Screenshots are time-boxed** (25s race) — a wedged compositor hangs the
  endpoint; the run continues without the shot rather than dying.
- **Orphan reaping**: a run killed mid-session leaves tauri-driver /
  WebKitWebDriver / `pollis` / `pollis-delivery` / vite behind; the next
  session then fails with "Maximum number of active sessions" or a 4445 bind
  error. `e2e.js` reaps all of these on start and on teardown.
- Vite 3 binds IPv6 loopback only (`[::1]:5173`); port checks probe both
  families.
- **`smoke.js` still needs placeholder env vars**: `Config::from_env()`
  (`pollis-core/src/config.rs`) hard-requires `TURSO_URL` / `TURSO_TOKEN` /
  `R2_S3_ENDPOINT` / `R2_PUBLIC_URL` to be present — baked in at compile time
  or set at runtime — or the app panics in its Tauri setup hook before any
  window opens. `smoke.js` supplies unreachable placeholders for these
  (`REQUIRED_PLACEHOLDERS`), since the login screen never dials any of them.
  If `Config::from_env()` grows a new required field, `smoke.js` needs a
  matching placeholder or it starts failing for an unrelated reason.

## Run the environment locally

The whole system environment this suite needs (WebKitGTK, the media/audio libs
pollis-core links, the pinned Rust toolchain, `tauri-driver`, `meson`, `xvfb`)
is packaged as a reusable Docker image — `e2e/Dockerfile` — so you can run the
smoke on any plain Docker host without hand-installing WebKitGTK. This is the
**single definition of the environment**; CI (`e2e-smoke.yml`) and later
milestones (backend fixtures, two-client, media) extend the same image and the
same shared setup rather than redefining it.

It is a **build-environment** image: the source is *not* baked in — you mount
your checkout at run time, so one image works across branches without a rebuild.

```bash
# Build the image once (from the repo root):
docker build -f e2e/Dockerfile -t pollis-e2e .

# Run the smoke inside it, mounting the current checkout:
docker run --rm -v "$PWD":/work -w /work pollis-e2e bash -lc '
  pnpm install --frozen-lockfile &&
  cargo build -p pollis &&
  dbus-run-session -- xvfb-run --auto-servernum pnpm --filter @pollis/e2e smoke
'
```

The image bakes the WebKit env gotchas (`WEBKIT_DISABLE_COMPOSITING_MODE=1`,
`GDK_BACKEND=x11`, `XDG_SESSION_TYPE=x11`), so a bare `docker run` inherits
them. The `dbus-run-session -- xvfb-run --auto-servernum …` prefix mirrors
exactly what CI does — it's the same wrapping the `desktop-e2e` composite
action applies (see below), so local and CI runs are byte-for-byte the same
environment.

Where the pieces live (all shared between the image and CI, so nothing is
duplicated):

| piece | file | used by |
|---|---|---|
| system deps (the apt list) | `e2e/scripts/install-system-deps.sh` | `e2e/Dockerfile` **and** the CI workflow |
| the whole build environment | `e2e/Dockerfile` | local Docker runs (and later milestones) |
| run-time wrapping (dbus + xvfb + orphan reaping) | `.github/actions/desktop-e2e/action.yml` | `e2e-smoke.yml` |

## CI

`.github/workflows/e2e-smoke.yml` runs `smoke.js` on Linux, triggered
manually (`workflow_dispatch`) — not on every push, since it needs a real
(virtual) display and a full `cargo build`, both slower than the rest of CI.
Trigger it from the Actions tab when you want a smoke check that the app
still launches. It installs the system deps via the shared
`e2e/scripts/install-system-deps.sh` (`webkit2gtk` + `webkit2gtk-driver` and
the media libs) and `tauri-driver`, then runs the smoke through the
`.github/actions/desktop-e2e` composite action, which wraps it in a dbus
session + `xvfb-run` and reaps orphan processes before and after.

`.github/workflows/e2e-full.yml` (issue #570, M1) runs the authenticated
scripts — `e2e.js` and `invalid-otp.js` — also `workflow_dispatch`-only. It
builds `pollis` **and** `pollis-delivery`, brings the backend up with
`e2e/scripts/start-backend.sh` (libsql + schema + real DS), runs both scripts
through the same `desktop-e2e` composite action, and tears the backend down in
an `if: always()` step. No nightly schedule (cost); trigger it from the Actions
tab when you want the full signup path exercised end-to-end.
