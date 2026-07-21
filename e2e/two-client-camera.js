#!/usr/bin/env node
/*
 * Two-client 1:1 CAMERA E2E — the camera slice of the media milestone
 * (issue #570, M3b). Directly validates the #568 camera-parity work end to
 * end: one client turns its webcam on in a live call, the OTHER client sees
 * the remote camera video tile render.
 *
 * Then (#394) A ALSO starts a screen share, and B must see A as TWO tiles — a
 * camera tile AND a screenshare tile — proving simultaneous camera+screen
 * render as two tiles instead of the screenshare replacing (dropping) the
 * camera. The Xvfb display + LiveKit this camera fixture already stands up also
 * support X11 screenshare, so no extra fixture is needed.
 *
 * Builds on the M3a call test (two-client-call.js): the signup + DM-establish +
 * place/accept-call choreography is reused verbatim (copied, matching how M3a
 * itself copied from two-client.js — these e2e flow helpers are duplicated per
 * test rather than shared). The camera step happens AFTER the call is connected
 * (both sides see two participants).
 *
 * Extra fixture over M3a: a VIRTUAL CAMERA (e2e/scripts/start-camera.sh) — a
 * v4l2loopback /dev/video0 fed a moving 1280x720 YUYV test pattern by ffmpeg —
 * so the app's Linux V4L2 capture path (pollis-capture-linux/src/camera.rs)
 * opens a real, changing signal headless.
 *
 * Choreography:
 *   1..7. Identical to two-client-call.js: A + B sign up, A DMs B, B accepts,
 *         A places the call, B accepts it, both converge on two participants.
 *   8. A turns its CAMERA on (the camera toggle pill). With exactly one camera
 *      (/dev/video0 is the only node on the runner) toggleCamera() starts it
 *      directly — no picker — capturing the loopback device and publishing a
 *      TrackSource::Camera track into the call room.
 *   9. Sanity on A: A's own local self-preview tile
 *      (`remote-video-tile-__local_camera_preview__`) appears — proof A's
 *      capture+publish actually engaged (isolates an A-side capture failure
 *      from a B-side delivery failure).
 *  10. ASSERT on B: poll until A's REMOTE CAMERA tile renders — a
 *      `remote-video-tile-<trackKey>` element nested inside A's participant tile
 *      (`voice-tile-voice-<A>`), excluding the local-preview keys and excluding
 *      screenshare feeds (a screenshare tile also carries a
 *      `voice-tile-stream-stats-` badge; a camera never does). Generous
 *      eventual timeout; no fixed sleeps for correctness.
 *  11. A turns the camera off and hangs up.
 *
 * Why that signal: on B, a remote participant's webcam lands in
 * `appStore.cameraRemotes[identity]` (screenShareSession's `remote_started`
 * with source==="camera"), which VoiceStage maps to the tile's `cameraTrackKey`,
 * which StageTile renders through RemoteVideoTile as
 * `data-testid="remote-video-tile-<trackKey>"` (a <canvas> on the Tauri /
 * WebKitGTK path). So the tile's mere PRESENCE proves A's camera track was
 * published, subscribed by B, and mounted for render. Camera and screenshare
 * share that testid prefix, so we additionally exclude screenshare tiles by the
 * stream-stats badge to keep the assertion camera-specific (M3c adds
 * screenshare; this keeps the two unambiguous even then).
 *
 * Assumes the media + backend fixtures are already up (the workflow runs
 * start-camera.sh → start-audio.sh → start-livekit.sh → start-backend.sh first).
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
// tiles, never a remote camera — exclude them from the remote-camera assertion.
// Must match cameraSession.ts / screenShareSession.ts.
const LOCAL_CAMERA_PREVIEW_KEY = "__local_camera_preview__";
const LOCAL_PREVIEW_KEY = "__local_preview__";

// Cross-client propagation is eventual (remote metadata reads + LiveKit
// realtime presence + the SFU join handshake), so every wait is generous.
const REQUEST_TIMEOUT_MS = 120_000;
const CALL_BUTTON_TIMEOUT_MS = 120_000;
const INCOMING_CALL_TIMEOUT_MS = 90_000;
const CONVERGE_TIMEOUT_MS = 120_000;
// A's camera must capture (open /dev/video0, negotiate YUYV) + publish locally.
const LOCAL_CAMERA_TIMEOUT_MS = 60_000;
// B must subscribe to A's published camera track and mount the tile — a full
// SFU publish→subscribe round trip plus the first decoded frame, so generous.
const REMOTE_CAMERA_TIMEOUT_MS = 120_000;

// Build the app env for one client — identical to two-client-call.js. The
// virtual-camera fixture needs no per-client env (the app enumerates
// /dev/video* itself); PULSE_* / LIVEKIT_URL flow through via ...process.env.
function appEnvFor(devEnv, turso, deliveryUrl, dataDir) {
  fs.rmSync(dataDir, { recursive: true, force: true });
  fs.mkdirSync(dataDir, { recursive: true });
  return {
    ...devEnv, ...process.env,
    TURSO_URL: turso.TURSO_URL, TURSO_TOKEN: turso.TURSO_TOKEN,
    POLLIS_DELIVERY_URL: deliveryUrl,
    POLLIS_DATA_DIR: dataDir,
    LOG_DB_URL: "", LOG_DB_TOKEN: "", LOG_DB_ADMIN_TOKEN: "",
    WEBKIT_DISABLE_COMPOSITING_MODE: "1", GDK_BACKEND: "x11",
  };
}

// Full signup through the real UI — copied verbatim from two-client-call.js.
async function signUp(browser, email, tag) {
  console.log(`[two-client-camera] ${tag}: signing up ${email}`);
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
  console.log(`[two-client-camera] ${tag}: reached app-ready`);
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
    console.log(`[two-client-camera] B: no DM request yet (attempt ${attempt}), waiting…`);
    await h.sleep(Math.min(remaining > 0 ? remaining : 0, attempt === 1 ? 4000 : 32000));
  }
  throw new Error("B: DM request never appeared");
}

// A: re-open the DM so DM.tsx remounts and React Query refetches — how A picks
// up B's acceptance metadata past the staleTime. Copied verbatim from M3a.
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
    console.log(`[two-client-camera] A: call button not ready yet (attempt ${attempt}) — B not seen online/accepted; re-opening DM…`);
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
      console.log(`[two-client-camera] ${tag}: sees ${ids.length} participants in the call`);
      return;
    }
    await h.sleep(2000);
  }
  throw new Error(`${tag}: the other call participant never appeared (no second voice tile)`);
}

// Click A's camera toggle. The global VoiceBar pill (`voice-bar-camera-button`)
// is the primary control; on the full-screen call stage the footer tray offers
// the same action as `voice-tray-camera`. Both call toggleCamera() identically,
// so click whichever is present (prefer the bar pill, per the milestone spec).
async function clickCameraToggle(browser, tag) {
  if (await h.present(browser, "voice-bar-camera-button")) {
    console.log(`[two-client-camera] ${tag}: clicking voice-bar-camera-button`);
    await h.clickTestId(browser, "voice-bar-camera-button");
    return;
  }
  if (await h.present(browser, "voice-tray-camera")) {
    console.log(`[two-client-camera] ${tag}: clicking voice-tray-camera (bar pill absent)`);
    await h.clickTestId(browser, "voice-tray-camera");
    return;
  }
  throw new Error(`${tag}: no camera toggle (voice-bar-camera-button / voice-tray-camera) on screen`);
}

// A: poll until A's OWN local camera self-preview tile mounts — proof the
// capture (open /dev/video0, negotiate a format) + local publish engaged. If
// this never appears the failure is A-side (capture), not B-side (delivery).
async function waitForLocalCameraPreview(browser, tag, timeoutMs) {
  const end = Date.now() + timeoutMs;
  const testId = `remote-video-tile-${LOCAL_CAMERA_PREVIEW_KEY}`;
  while (Date.now() < end) {
    if (await h.present(browser, testId)) {
      console.log(`[two-client-camera] ${tag}: local camera self-preview is up`);
      return;
    }
    await h.sleep(1500);
  }
  throw new Error(
    `${tag}: local camera preview never appeared — the webcam capture/publish did not engage. ` +
      "Check the v4l2loopback device (e2e/scripts/start-camera.sh) and the app's V4L2 capture path."
  );
}

// Remote CAMERA tiles visible on this client: `remote-video-tile-<trackKey>`
// elements nested inside a participant CAMERA tile — a `voice-tile-voice-<id>`
// root (class `vs-tile`) whose key ends in `:cam` (a screenshare tile ends in
// `:screen`). Camera and screenshare tiles are otherwise identical (both carry
// the res·fps badge, both spotlightable), so the `:cam`/`:screen` suffix — not
// the badge — is what tells them apart. Excludes the local-preview keys.
// Returns the owning tiles' identities + track keys.
async function remoteCameraTiles(browser) {
  return browser.execute((localKeys) => {
    const out = [];
    for (const tile of document.querySelectorAll('[data-testid^="voice-tile-voice-"]')) {
      // Only participant tile ROOTS (sub-elements are voice-tile-avatar-/quality-/…).
      if (!tile.classList.contains("vs-tile")) {
        continue;
      }
      const tileTestId = tile.getAttribute("data-testid") || "";
      if (!tileTestId.endsWith(":cam")) {
        continue;
      }
      for (const v of tile.querySelectorAll('[data-testid^="remote-video-tile-"]')) {
        const key = (v.getAttribute("data-testid") || "").slice("remote-video-tile-".length);
        if (localKeys.includes(key)) {
          continue;
        }
        out.push({ tileTestId, trackKey: key });
      }
    }
    return out;
  }, [LOCAL_CAMERA_PREVIEW_KEY, LOCAL_PREVIEW_KEY]);
}

// B: poll until A's remote camera tile renders. This is the M3b assertion.
async function waitForRemoteCamera(browser, tag, timeoutMs) {
  const end = Date.now() + timeoutMs;
  let last = [];
  while (Date.now() < end) {
    last = await remoteCameraTiles(browser).catch(() => []);
    if (last.length >= 1) {
      console.log(
        `[two-client-camera] ${tag}: remote camera tile present — ` +
          last.map((t) => `${t.tileTestId} (${t.trackKey})`).join(", ")
      );
      return;
    }
    await h.sleep(2000);
  }
  throw new Error(
    `${tag}: the remote camera tile never appeared — A's camera track was not ` +
      "seen rendering on B (no non-local remote-video-tile inside a participant tile)."
  );
}

// A: start a screen share ALONGSIDE the camera. Linux/X11 enumerates empty, so
// the toggle starts capture directly with no picker (same as
// two-client-screenshare.js). Bar pill first, stage tray as fallback.
async function startScreenShare(browser, tag) {
  if (await h.present(browser, "voice-bar-screenshare-button")) {
    await h.clickTestId(browser, "voice-bar-screenshare-button");
    return;
  }
  if (await h.present(browser, "voice-tray-screenshare")) {
    await h.clickTestId(browser, "voice-tray-screenshare");
    return;
  }
  throw new Error(`${tag}: no screenshare toggle (voice-bar-screenshare-button / voice-tray-screenshare) on screen`);
}

// Which KINDS of remote video tile this client currently renders. A remote tile
// (`voice-tile-voice-<id>` root with a nested non-local `remote-video-tile-`) is
// a camera when its key ends `:cam`, a screenshare when it ends `:screen`. #394:
// a participant publishing both must show up as BOTH kinds — two distinct tiles.
async function remoteVideoTileKinds(browser) {
  return browser.execute((localKeys) => {
    let camera = false;
    let screenshare = false;
    for (const tile of document.querySelectorAll('[data-testid^="voice-tile-voice-"]')) {
      if (!tile.classList.contains("vs-tile")) {
        continue;
      }
      const tileTestId = tile.getAttribute("data-testid") || "";
      let hasRemoteVideo = false;
      for (const v of tile.querySelectorAll('[data-testid^="remote-video-tile-"]')) {
        const key = (v.getAttribute("data-testid") || "").slice("remote-video-tile-".length);
        if (!localKeys.includes(key)) {
          hasRemoteVideo = true;
        }
      }
      if (!hasRemoteVideo) {
        continue;
      }
      if (tileTestId.endsWith(":screen")) {
        screenshare = true;
      } else if (tileTestId.endsWith(":cam")) {
        camera = true;
      }
    }
    return { camera, screenshare };
  }, [LOCAL_CAMERA_PREVIEW_KEY, LOCAL_PREVIEW_KEY]);
}

// B: poll until it renders BOTH a remote camera tile AND a remote screenshare
// tile — i.e. A's simultaneous camera+screen surfaces as two tiles (#394). This
// is the regression guard for the old bug where a screenshare REPLACED the
// camera tile and the camera was silently dropped.
async function waitForBothRemoteVideo(browser, tag, timeoutMs) {
  const end = Date.now() + timeoutMs;
  let last = { camera: false, screenshare: false };
  while (Date.now() < end) {
    last = await remoteVideoTileKinds(browser).catch(() => ({ camera: false, screenshare: false }));
    if (last.camera && last.screenshare) {
      return;
    }
    await h.sleep(2000);
  }
  throw new Error(
    `${tag}: expected BOTH a remote camera tile and a screenshare tile for the ` +
      `participant sharing both (got camera=${last.camera}, screenshare=${last.screenshare}). ` +
      "A participant publishing camera + screen must render two tiles (#394)."
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
  const emailA = `e2e_cam_a_${stamp}@pollis.test`;
  const emailB = `e2e_cam_b_${stamp}@pollis.test`;

  let code = 1;
  let A;
  let B;
  try {
    await h.waitViteReady();
    console.log(`[two-client-camera] using external delivery service at ${deliveryUrl}`);
    console.log(`[two-client-camera] using LiveKit at ${process.env.LIVEKIT_URL}`);
    console.log(`[two-client-camera] virtual camera device: ${process.env.POLLIS_E2E_CAMERA_DEVICE || "/dev/video0 (default)"}`);

    A = await h.startClient({
      index: 0, label: "A",
      appEnv: appEnvFor(devEnv, turso, deliveryUrl, path.join(__dirname, ".tmp-data-camera-a")),
    });
    clients.push(A);
    B = await h.startClient({
      index: 1, label: "B",
      appEnv: appEnvFor(devEnv, turso, deliveryUrl, path.join(__dirname, ".tmp-data-camera-b")),
    });
    clients.push(B);

    // Both sign up.
    await signUp(A.browser, emailA, "A");
    await shot(A.browser, "two-client-camera-A-ready.png");
    await signUp(B.browser, emailB, "B");
    await shot(B.browser, "two-client-camera-B-ready.png");

    // A → DM B; B → accept. Establishes the DM + MLS group both directions.
    console.log(`[two-client-camera] A: starting DM to ${emailB}`);
    await startDmTo(A.browser, emailB);
    console.log("[two-client-camera] B: waiting for the DM request…");
    await acceptIncomingDm(B.browser, REQUEST_TIMEOUT_MS);
    console.log("[two-client-camera] B: DM request accepted");

    // A → wait until B is seen online + accepted (call button renders).
    console.log("[two-client-camera] A: waiting to see B online + accepted…");
    await waitForCallButton(A.browser, CALL_BUTTON_TIMEOUT_MS);

    // A → place the call.
    console.log("[two-client-camera] A: placing the call");
    await h.clickTestId(A.browser, "dm-header-call");
    await h.waitTestId(A.browser, "call-hang-up", 30000);

    // B → accept the incoming-call alert.
    console.log("[two-client-camera] B: waiting for the incoming-call alert…");
    await h.waitTestId(B.browser, "status-bar-incoming-call-accept", INCOMING_CALL_TIMEOUT_MS);
    await h.clickTestId(B.browser, "status-bar-incoming-call-accept");
    await h.waitTestId(B.browser, "call-hang-up", 30000);

    // Both sides see each other in the call (2 participants).
    console.log("[two-client-camera] waiting for both sides to converge in the call…");
    await waitForRemoteParticipant(A.browser, "A", CONVERGE_TIMEOUT_MS);
    await waitForRemoteParticipant(B.browser, "B", CONVERGE_TIMEOUT_MS);
    await shot(A.browser, "two-client-camera-A-in-call.png");
    await shot(B.browser, "two-client-camera-B-in-call.png");
    console.log("[two-client-camera] both clients are in the call");

    // ── M3b: A turns its camera ON ─────────────────────────────────────────
    console.log("[two-client-camera] A: turning the camera on");
    await clickCameraToggle(A.browser, "A");

    // Sanity: A's local self-preview mounts → capture + publish engaged.
    await waitForLocalCameraPreview(A.browser, "A", LOCAL_CAMERA_TIMEOUT_MS);
    await shot(A.browser, "two-client-camera-A-camera-on.png");

    // ASSERT: B renders A's REMOTE camera tile.
    console.log("[two-client-camera] B: waiting for A's remote camera tile…");
    await waitForRemoteCamera(B.browser, "B", REMOTE_CAMERA_TIMEOUT_MS);
    await shot(B.browser, "two-client-camera-B-sees-camera.png");
    console.log("[two-client-camera] SUCCESS: B sees A's remote camera tile");

    // ── #394: A ALSO shares its screen — camera + screenshare must render as
    // TWO tiles for A on B. Before the fix, the screenshare took over A's tile
    // and the camera was dropped; now each source gets its own tile. ──
    console.log("[two-client-camera] A: starting a screen share alongside the camera");
    await startScreenShare(A.browser, "A");
    console.log("[two-client-camera] B: waiting for BOTH A's camera tile AND screenshare tile…");
    await waitForBothRemoteVideo(B.browser, "B", REMOTE_CAMERA_TIMEOUT_MS);
    await shot(B.browser, "two-client-camera-B-sees-both.png");
    console.log("[two-client-camera] SUCCESS: B sees A's camera AND screenshare as two tiles");

    // Turn the camera off + hang up (best-effort — the run already succeeded).
    await clickCameraToggle(A.browser, "A").catch(() => {});
    await h.clickTestId(A.browser, "call-hang-up").catch(() => {});
    code = 0;
  } catch (err) {
    console.error("[two-client-camera] FAILED:", err.message);
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
  console.error(`[two-client-camera] ${tag} on-screen testids:`, [...new Set(ids)].join(", "));
}

main().catch((e) => { console.error("[two-client-camera] fatal:", e); h.reap(); process.exit(1); });
