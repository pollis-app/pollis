#!/usr/bin/env node
/*
 * Two-client 1:1 SCREENSHARE E2E — the screenshare slice of the media
 * milestone (issue #570, M3c) and the LAST media slice, completing the #568
 * voice/video/screenshare surface. One client shares its SCREEN in a live call,
 * the OTHER client sees the remote screenshare tile render.
 *
 * Builds on the M3a call test (two-client-call.js) + the M3b camera test
 * (two-client-camera.js): the signup + DM-establish + place/accept-call
 * choreography is reused verbatim (copied, matching how M3a/M3b themselves
 * copied from two-client.js — these e2e flow helpers are duplicated per test
 * rather than shared). The screenshare step happens AFTER the call is connected
 * (both sides see two participants).
 *
 * NO extra fixture over M3a: unlike the camera slice (which needs a
 * v4l2loopback /dev/video0), screenshare needs nothing but the Xvfb display the
 * desktop-e2e composite action already provides. On Linux the app captures the
 * X11 ROOT window via xcb + MIT-SHM `XGetImage` (pollis-capture-linux/src/x11.rs),
 * driven by the backend probe in src/linux.rs. Under xvfb-run there is a real
 * X server ($DISPLAY set) and NO Wayland ($WAYLAND_DISPLAY unset), so the probe
 * selects `Backend::X11` — NO xdg-desktop-portal / PipeWire session is needed or
 * used. appEnvFor also pins GDK_BACKEND=x11 + XDG_SESSION_TYPE=x11 to make the
 * X11 branch explicit and bulletproof. The app's own WebKitGTK window is on that
 * same Xvfb display, so the root grab is non-blank — there's real content to send.
 *
 * On Linux `enumerate_screen_sources` returns an EMPTY list (start_unix.rs), so
 * the frontend's toggleScreenShare skips its in-app picker and goes straight to
 * `screenShareSession.start()` (the OS portal dialog IS the picker on Linux, and
 * there is none under X11/Xvfb) — the helper subprocess spawns, probes X11, and
 * streams frames. So clicking the screenshare button is the whole trigger; no
 * picker UI appears. (If a picker ever DOES appear — e.g. a future backend
 * change — we handle it defensively by selecting the first display source.)
 *
 * Choreography:
 *   1..7. Identical to two-client-call.js / two-client-camera.js: A + B sign up,
 *         A DMs B, B accepts, A places the call, B accepts it, both converge on
 *         two participants.
 *   8. A starts a SCREEN SHARE (the screenshare toggle — the VoiceBar pill
 *      `voice-bar-screenshare-button`, or the stage tray's
 *      `voice-tray-screenshare`). On Linux this enumerates empty → no picker →
 *      start() directly, spawning the capture helper (X11/SHM root grab) and
 *      publishing a `TrackSource::Screenshare` track into the call room.
 *   9. Sanity on A: A's own local self-preview tile
 *      (`remote-video-tile-__local_preview__`) mounts — proof A's capture+publish
 *      engaged (isolates an A-side capture failure from a B-side delivery one).
 *  10. ASSERT on B: poll until A's REMOTE SCREENSHARE tile renders — a
 *      `remote-video-tile-<trackKey>` element nested inside A's participant tile
 *      (`voice-tile-voice-<A>`) that ALSO carries a `voice-tile-stream-stats-<A>`
 *      badge. That badge (a res·fps label) is present on a SCREENSHARE tile and
 *      ABSENT on a camera tile — this is the exact mirror-inverse of M3b's camera
 *      assertion (which EXCLUDES screenshare via that same badge), so the two
 *      stay unambiguous. Generous eventual timeout; no fixed sleeps.
 *  11. A stops the share + hangs up.
 *
 * Why that signal: on B, a remote participant's screenshare lands as
 * `screenshareOf(p.video)` (VoiceStage.tsx) → the tile's `streamTrackKey`, which
 * StageTile renders through RemoteVideoTile as
 * `data-testid="remote-video-tile-<trackKey>"` (a <canvas> on the Tauri /
 * WebKitGTK path) AND — because it `hasFeed` and is in-call — a
 * `voice-tile-stream-stats-<identity>` res·fps badge. So the tile's presence WITH
 * that badge proves A's screenshare track was published, subscribed by B, and
 * mounted for render. A camera tile renders the same `remote-video-tile-` prefix
 * but never the stream-stats badge, so requiring the badge makes the assertion
 * screenshare-specific.
 *
 * Assumes the media + backend fixtures are already up (the workflow runs
 * start-audio.sh → start-livekit.sh → start-backend.sh first). Screenshare needs
 * audio + LiveKit like the call; it needs NO camera fixture.
 *
 * FAILS LOUDLY if the share never starts (e.g. the app took a portal path and
 * errored, or capture never produced a frame): the A-side local-preview poll
 * throws with a clear message before B is ever consulted.
 *
 * On failure, dumps per-client screenshots + page source into e2e/artifacts/.
 */

const fs = require("fs");
const path = require("path");
const h = require("./lib/harness");

const ARTIFACTS = path.join(__dirname, "artifacts");
const shot = h.makeShot(ARTIFACTS);

const PIN = "1357";

// Reserved local-preview frame-WS keys the backend mirrors the SHARER's own
// outgoing video under (for their self-view). These are the local user's own
// tiles, never a remote feed — exclude them from the remote-screenshare
// assertion. `__local_preview__` is the SCREENSHARE self-preview key
// (screenShareSession.ts / pollis-core screenshare LOCAL_PREVIEW_KEY);
// `__local_camera_preview__` is the camera one (cameraSession.ts).
const LOCAL_PREVIEW_KEY = "__local_preview__";
const LOCAL_CAMERA_PREVIEW_KEY = "__local_camera_preview__";

// Cross-client propagation is eventual (remote metadata reads + LiveKit
// realtime presence + the SFU join handshake), so every wait is generous.
const REQUEST_TIMEOUT_MS = 120_000;
const CALL_BUTTON_TIMEOUT_MS = 120_000;
const INCOMING_CALL_TIMEOUT_MS = 90_000;
const CONVERGE_TIMEOUT_MS = 120_000;
// A's capture helper must spawn, probe X11, grab the root window, and the app
// must publish the track locally (local self-preview tile mounts).
const LOCAL_SHARE_TIMEOUT_MS = 60_000;
// B must subscribe to A's published screenshare track and mount the tile — a
// full SFU publish→subscribe round trip plus the first decoded frame, so
// generous.
const REMOTE_SHARE_TIMEOUT_MS = 120_000;
// How long we give a (Linux-unexpected) in-app picker to appear before deciding
// the app auto-started without one. Short — on Linux enumerate returns empty so
// no picker ever renders; this only covers a defensive future-proofing branch.
const PICKER_PROBE_MS = 6_000;

// Build the app env for one client — identical to two-client-call.js /
// two-client-camera.js, plus it makes the LINUX X11 CAPTURE PATH explicit:
// GDK_BACKEND=x11 keeps WebKitGTK on X11, and XDG_SESSION_TYPE=x11 forces the
// capture helper's backend probe (pollis-capture-linux/src/linux.rs) down the
// X11/xcb/SHM branch rather than trying an xdg-desktop-portal ScreenCast session
// (which is not feasible headless). Under xvfb-run the probe would already pick
// X11 (DISPLAY set, no WAYLAND_DISPLAY); pinning it removes all ambiguity. The
// LiveKit + PulseAudio env (LIVEKIT_URL / PULSE_*) flows through via
// ...process.env.
function appEnvFor(devEnv, turso, deliveryUrl, dataDir) {
  fs.rmSync(dataDir, { recursive: true, force: true });
  fs.mkdirSync(dataDir, { recursive: true });
  return {
    ...process.env, ...devEnv,
    TURSO_URL: turso.TURSO_URL, TURSO_TOKEN: turso.TURSO_TOKEN,
    POLLIS_DELIVERY_URL: deliveryUrl,
    POLLIS_DATA_DIR: dataDir,
    WEBKIT_DISABLE_COMPOSITING_MODE: "1", GDK_BACKEND: "x11",
    XDG_SESSION_TYPE: "x11",
  };
}

// Full signup through the real UI — copied verbatim from two-client-call.js.
async function signUp(browser, email, tag) {
  console.log(`[two-client-screenshare] ${tag}: signing up ${email}`);
  await h.waitTestId(browser, "auth-screen", 30000);
  await h.setTestIdValue(browser, "email-input", email);
  await h.clickTestId(browser, "send-otp-button");
  await h.waitTestId(browser, "otp-form-container", 20000);
  await h.typeCode(browser, "000000");
  await h.waitTestId(browser, "save-secret-key-warning-screen", 45000);
  await h.clickTestId(browser, "save-secret-key-acknowledge-button");
  await h.waitTestId(browser, "save-secret-key-screen");
  const secretKey = (await (await browser.$('[data-testid="secret-key-display"]')).getText()).trim();
  if (!secretKey) {
    throw new Error(`${tag}: secret key display was empty`);
  }
  await h.clickTestId(browser, "secret-key-saved-button");
  await h.waitTestId(browser, "save-secret-key-confirm-screen");
  await h.setTestIdValue(browser, "secret-key-confirm-input", secretKey);
  await h.clickTestId(browser, "confirm-secret-key-button");
  await h.waitTestId(browser, "pin-create-screen");
  await h.typeCode(browser, PIN);
  await h.typeCode(browser, PIN);
  await h.waitTestId(browser, "app-ready", 60000);
  console.log(`[two-client-screenshare] ${tag}: reached app-ready`);
}

// A: open "New Message", DM the target by email, land on the DM page. Copied
// verbatim from two-client-call.js.
async function startDmTo(browser, targetEmail) {
  await h.clickTestId(browser, "sidebar-row-dms");
  await h.clickTestId(browser, "menu-item-new-dm");
  await h.waitTestId(browser, "start-dm-page", 20000);
  await h.setSelectorValue(browser, "#dm-identifier", targetEmail);
  await h.clickTestId(browser, "start-dm-submit-button");
  await h.waitTestId(browser, "message-form", 30000);
}

// B: poll the DMs list until the incoming request surfaces and accept it.
// Copied verbatim from two-client-call.js.
async function acceptIncomingDm(browser, timeoutMs) {
  const end = Date.now() + timeoutMs;
  let attempt = 0;
  while (Date.now() < end) {
    attempt++;
    await h.clickTestId(browser, "sidebar-row-account").catch(() => {});
    await h.sleep(500);
    await h.clickTestId(browser, "sidebar-row-dms");
    await h.sleep(1500);
    if (await h.presentSelector(browser, '[data-testid="menu-item-dm-requests"]')) {
      await h.clickTestId(browser, "menu-item-dm-requests");
      await h.waitTestId(browser, "requests-page", 15000);
      await h.waitSelector(browser, '[data-testid^="accept-request-"]', 15000, "a DM request accept button");
      await h.clickSelector(browser, '[data-testid^="accept-request-"]');
      await h.waitTestId(browser, "message-form", 30000);
      return;
    }
    const remaining = end - Date.now();
    console.log(`[two-client-screenshare] B: no DM request yet (attempt ${attempt}), waiting…`);
    await h.sleep(Math.min(remaining > 0 ? remaining : 0, attempt === 1 ? 4000 : 32000));
  }
  throw new Error("B: DM request never appeared");
}

// A: re-open the DM so DM.tsx remounts and React Query refetches — how A picks
// up B's acceptance metadata past the staleTime. Copied verbatim from M3a/M3b.
async function reopenDm(browser) {
  await h.clickTestId(browser, "sidebar-row-dms").catch(() => {});
  await h.sleep(1000);
  if (await h.presentSelector(browser, '[data-testid^="dm-option-"]')) {
    await h.clickSelector(browser, '[data-testid^="dm-option-"]');
    await h.waitTestId(browser, "message-form", 15000).catch(() => {});
  }
}

// A: poll until the DM-header call button appears (canCall = 1:1 && B accepted
// && B online). Copied verbatim from two-client-call.js.
async function waitForCallButton(browser, timeoutMs) {
  const end = Date.now() + timeoutMs;
  let attempt = 0;
  while (Date.now() < end) {
    attempt++;
    if (await h.present(browser, "dm-header-call")) {
      return;
    }
    console.log(`[two-client-screenshare] A: call button not ready yet (attempt ${attempt}) — B not seen online/accepted; re-opening DM…`);
    await reopenDm(browser);
    await h.sleep(4000);
  }
  throw new Error(
    "A: the DM-header call button never appeared — A never saw B both accepted " +
      "and online. Check LiveKit realtime presence on the shared DM room."
  );
}

// Distinct call participants visible on this client, by user id. Copied
// verbatim from two-client-call.js (mirrors userIdFromVoiceIdentity).
async function callParticipantUserIds(browser) {
  return browser.execute(() => {
    const ids = new Set();
    for (const el of document.querySelectorAll('[data-testid^="voice-tile-voice-"]')) {
      const tid = el.getAttribute("data-testid") || "";
      let identity = tid.slice("voice-tile-".length);
      if (identity.startsWith("voice-")) {
        identity = identity.slice("voice-".length);
      }
      const colon = identity.indexOf(":");
      ids.add(colon === -1 ? identity : identity.slice(0, colon));
    }
    return Array.from(ids);
  });
}

// Poll until this client's call stage shows at least two distinct participants.
// Copied verbatim from two-client-call.js.
async function waitForRemoteParticipant(browser, tag, timeoutMs) {
  const end = Date.now() + timeoutMs;
  while (Date.now() < end) {
    const ids = await callParticipantUserIds(browser).catch(() => []);
    if (ids.length >= 2) {
      console.log(`[two-client-screenshare] ${tag}: sees ${ids.length} participants in the call`);
      return;
    }
    await h.sleep(2000);
  }
  throw new Error(`${tag}: the other call participant never appeared (no second voice tile)`);
}

// Click A's screenshare toggle. The global VoiceBar pill
// (`voice-bar-screenshare-button`) is the primary control; on the full-screen
// call stage the footer tray offers the same action as `voice-tray-screenshare`.
// Both call toggleScreenShare() identically, so click whichever is present
// (prefer the bar pill, per the milestone spec).
async function clickScreenShareToggle(browser, tag) {
  if (await h.present(browser, "voice-bar-screenshare-button")) {
    console.log(`[two-client-screenshare] ${tag}: clicking voice-bar-screenshare-button`);
    await h.clickTestId(browser, "voice-bar-screenshare-button");
    return;
  }
  if (await h.present(browser, "voice-tray-screenshare")) {
    console.log(`[two-client-screenshare] ${tag}: clicking voice-tray-screenshare (bar pill absent)`);
    await h.clickTestId(browser, "voice-tray-screenshare");
    return;
  }
  throw new Error(`${tag}: no screenshare toggle (voice-bar-screenshare-button / voice-tray-screenshare) on screen`);
}

// A: start the screen share. Click the toggle, then handle the two possible
// outcomes:
//   - Linux X11 (the expected path): enumerate returns empty, so
//     toggleScreenShare skips the in-app picker and calls start() directly — no
//     `screen-share-picker` ever renders; capture begins immediately.
//   - A picker DID appear (defensive, non-Linux / future backend change): the
//     ScreenSharePicker took over the stage. Select the first DISPLAY source
//     (whole-monitor share — its default tab) so the share still starts.
async function startScreenShare(browser, tag) {
  await clickScreenShareToggle(browser, tag);

  // Give a picker a brief chance to appear. On Linux it won't (empty enumerate
  // → direct start), so this just times out and we fall through to the
  // local-preview poll.
  const end = Date.now() + PICKER_PROBE_MS;
  while (Date.now() < end) {
    if (await h.present(browser, "screen-share-picker")) {
      console.log(
        `[two-client-screenshare] ${tag}: a screen-share picker appeared ` +
          "(unexpected on Linux/X11) — selecting the first display source"
      );
      // ScreenSharePicker.tsx exposes no per-source testid; its source cards are
      // plain <button>s inside the scrollable grid (`.grid > button`). The
      // Displays tab is the default, so the first grid button is a display.
      const picked = await browser.execute(() => {
        const picker = document.querySelector('[data-testid="screen-share-picker"]');
        if (!picker) {
          return false;
        }
        const card = picker.querySelector(".grid button");
        if (card) {
          card.click();
          return true;
        }
        return false;
      });
      if (!picked) {
        throw new Error(
          `${tag}: screen-share picker was open but had no selectable source card`
        );
      }
      return;
    }
    // Local preview already up ⇒ auto-started with no picker; nothing to pick.
    if (await h.present(browser, `remote-video-tile-${LOCAL_PREVIEW_KEY}`)) {
      return;
    }
    await h.sleep(500);
  }
  console.log(
    `[two-client-screenshare] ${tag}: no picker appeared — Linux X11 auto-start path`
  );
}

// A: poll until A's OWN local screenshare self-preview tile mounts — proof the
// capture (spawn helper, probe X11, grab the root window) + local publish
// engaged. If this never appears the failure is A-side (capture/publish), not
// B-side (delivery) — so we fail LOUDLY here before ever consulting B.
async function waitForLocalSharePreview(browser, tag, timeoutMs) {
  const end = Date.now() + timeoutMs;
  const testId = `remote-video-tile-${LOCAL_PREVIEW_KEY}`;
  while (Date.now() < end) {
    if (await h.present(browser, testId)) {
      console.log(`[two-client-screenshare] ${tag}: local screenshare self-preview is up`);
      return;
    }
    await h.sleep(1500);
  }
  throw new Error(
    `${tag}: local screenshare preview never appeared — the screen capture/publish did not engage. ` +
      "On Linux the app should take the X11/xcb+SHM root-capture path under Xvfb " +
      "(pollis-capture-linux/src/linux.rs probe → Backend::X11); if it errored it likely tried an " +
      "xdg-desktop-portal path headless (no ScreenCast backend), or capture produced no frame."
  );
}

// Remote SCREENSHARE tiles visible on this client: a participant tile
// (`voice-tile-voice-<id>` root, class `vs-tile`) that BOTH carries a
// `voice-tile-stream-stats-` res·fps badge (screenshare-specific — a camera tile
// never has it) AND contains a non-local `remote-video-tile-<trackKey>`. Returns
// the owning tiles' identities + track keys. This is the mirror-INVERSE of the
// M3b camera assertion (two-client-camera.js `remoteCameraTiles`, which EXCLUDES
// tiles with that badge).
async function remoteScreenshareTiles(browser) {
  return browser.execute((localKeys) => {
    const out = [];
    for (const tile of document.querySelectorAll('[data-testid^="voice-tile-voice-"]')) {
      // Only participant tile ROOTS (sub-elements are voice-tile-avatar-/quality-/…).
      if (!tile.classList.contains("vs-tile")) {
        continue;
      }
      const tileTestId = tile.getAttribute("data-testid") || "";
      // A SCREENSHARE feed in this tile shows a res·fps badge; a camera doesn't.
      const isScreenshare = !!tile.querySelector('[data-testid^="voice-tile-stream-stats-"]');
      if (!isScreenshare) {
        continue;
      }
      for (const v of tile.querySelectorAll('[data-testid^="remote-video-tile-"]')) {
        const key = (v.getAttribute("data-testid") || "").slice("remote-video-tile-".length);
        // Never count the local self-preview keys as a remote feed.
        if (localKeys.includes(key)) {
          continue;
        }
        out.push({ tileTestId, trackKey: key });
      }
    }
    return out;
  }, [LOCAL_PREVIEW_KEY, LOCAL_CAMERA_PREVIEW_KEY]);
}

// B: poll until A's remote screenshare tile renders. This is the M3c assertion.
async function waitForRemoteShare(browser, tag, timeoutMs) {
  const end = Date.now() + timeoutMs;
  let last = [];
  while (Date.now() < end) {
    last = await remoteScreenshareTiles(browser).catch(() => []);
    if (last.length >= 1) {
      console.log(
        `[two-client-screenshare] ${tag}: remote screenshare tile present — ` +
          last.map((t) => `${t.tileTestId} (${t.trackKey})`).join(", ")
      );
      return;
    }
    await h.sleep(2000);
  }
  throw new Error(
    `${tag}: the remote screenshare tile never appeared — A's screenshare track was not ` +
      "seen rendering on B (no non-local remote-video-tile WITH a voice-tile-stream-stats- " +
      "badge inside a participant tile)."
  );
}

async function main() {
  h.reap();
  const devEnv = h.readEnvFile(".env.development");
  const turso = h.tursoEnv();

  const deliveryUrl = process.env.POLLIS_DELIVERY_URL;
  if (!deliveryUrl) {
    throw new Error(
      "POLLIS_DELIVERY_URL is not set — run e2e/scripts/start-backend.sh first " +
        "(both clients must share one backend). See e2e/README.md."
    );
  }
  if (!process.env.LIVEKIT_URL) {
    throw new Error(
      "LIVEKIT_URL is not set — run e2e/scripts/start-livekit.sh before this test " +
        "(the app dials it to join the call room). See e2e/README.md."
    );
  }

  const children = [];
  const clients = [];
  const stop = (c) => { try { c && c.kill("SIGKILL"); } catch (_) {} };

  const vite = h.spawnVite(devEnv);
  children.push(vite);

  const stamp = Date.now();
  const emailA = `e2e_share_a_${stamp}@pollis.test`;
  const emailB = `e2e_share_b_${stamp}@pollis.test`;

  let code = 1;
  let A;
  let B;
  try {
    await h.waitViteReady();
    console.log(`[two-client-screenshare] using external delivery service at ${deliveryUrl}`);
    console.log(`[two-client-screenshare] using LiveKit at ${process.env.LIVEKIT_URL}`);

    A = await h.startClient({
      index: 0, label: "A",
      appEnv: appEnvFor(devEnv, turso, deliveryUrl, path.join(__dirname, ".tmp-data-share-a")),
    });
    clients.push(A);
    B = await h.startClient({
      index: 1, label: "B",
      appEnv: appEnvFor(devEnv, turso, deliveryUrl, path.join(__dirname, ".tmp-data-share-b")),
    });
    clients.push(B);

    // Both sign up.
    await signUp(A.browser, emailA, "A");
    await shot(A.browser, "two-client-screenshare-A-ready.png");
    await signUp(B.browser, emailB, "B");
    await shot(B.browser, "two-client-screenshare-B-ready.png");

    // A → DM B; B → accept. Establishes the DM + MLS group both directions.
    console.log(`[two-client-screenshare] A: starting DM to ${emailB}`);
    await startDmTo(A.browser, emailB);
    console.log("[two-client-screenshare] B: waiting for the DM request…");
    await acceptIncomingDm(B.browser, REQUEST_TIMEOUT_MS);
    console.log("[two-client-screenshare] B: DM request accepted");

    // A → wait until B is seen online + accepted (call button renders).
    console.log("[two-client-screenshare] A: waiting to see B online + accepted…");
    await waitForCallButton(A.browser, CALL_BUTTON_TIMEOUT_MS);

    // A → place the call.
    console.log("[two-client-screenshare] A: placing the call");
    await h.clickTestId(A.browser, "dm-header-call");
    await h.waitTestId(A.browser, "call-hang-up", 30000);

    // B → accept the incoming-call alert.
    console.log("[two-client-screenshare] B: waiting for the incoming-call alert…");
    await h.waitTestId(B.browser, "status-bar-incoming-call-accept", INCOMING_CALL_TIMEOUT_MS);
    await h.clickTestId(B.browser, "status-bar-incoming-call-accept");
    await h.waitTestId(B.browser, "call-hang-up", 30000);

    // Both sides see each other in the call (2 participants).
    console.log("[two-client-screenshare] waiting for both sides to converge in the call…");
    await waitForRemoteParticipant(A.browser, "A", CONVERGE_TIMEOUT_MS);
    await waitForRemoteParticipant(B.browser, "B", CONVERGE_TIMEOUT_MS);
    await shot(A.browser, "two-client-screenshare-A-in-call.png");
    await shot(B.browser, "two-client-screenshare-B-in-call.png");
    console.log("[two-client-screenshare] both clients are in the call");

    // ── M3c: A starts a SCREEN SHARE ───────────────────────────────────────
    console.log("[two-client-screenshare] A: starting a screen share");
    await startScreenShare(A.browser, "A");

    // Sanity: A's local self-preview mounts → capture + publish engaged.
    await waitForLocalSharePreview(A.browser, "A", LOCAL_SHARE_TIMEOUT_MS);
    await shot(A.browser, "two-client-screenshare-A-sharing.png");

    // ASSERT: B renders A's REMOTE screenshare tile (video tile + stats badge).
    console.log("[two-client-screenshare] B: waiting for A's remote screenshare tile…");
    await waitForRemoteShare(B.browser, "B", REMOTE_SHARE_TIMEOUT_MS);
    await shot(B.browser, "two-client-screenshare-B-sees-share.png");
    console.log("[two-client-screenshare] SUCCESS: B sees A's remote screenshare tile");

    // Stop the share + hang up (best-effort — the run already succeeded).
    await clickScreenShareToggle(A.browser, "A").catch(() => {});
    await h.clickTestId(A.browser, "call-hang-up").catch(() => {});
    code = 0;
  } catch (err) {
    console.error("[two-client-screenshare] FAILED:", err.message);
    if (A && A.browser) {
      await dumpClient(A.browser, "A");
    }
    if (B && B.browser) {
      await dumpClient(B.browser, "B");
    }
  } finally {
    for (const c of clients) {
      if (c && c.browser) {
        await c.browser.deleteSession().catch(() => {});
      }
    }
    for (const c of clients) {
      stop(c && c.tauriDriver);
    }
    for (const c of children) {
      stop(c);
    }
    h.reap();
  }
  process.exit(code);
}

// Per-client failure artifacts (prefixed so A and B don't clobber).
async function dumpClient(browser, tag) {
  await shot(browser, `${tag}-FAIL.png`).catch(() => {});
  const src = await browser.getPageSource().catch(() => "");
  fs.mkdirSync(ARTIFACTS, { recursive: true });
  fs.writeFileSync(path.join(ARTIFACTS, `${tag}-FAIL.html`), src);
  const ids = [...src.matchAll(/data-testid="([^"]+)"/g)].map((m) => m[1]);
  console.error(`[two-client-screenshare] ${tag} on-screen testids:`, [...new Set(ids)].join(", "));
}

main().catch((e) => { console.error("[two-client-screenshare] fatal:", e); h.reap(); process.exit(1); });
