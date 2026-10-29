#!/usr/bin/env node
// Two-client screenshare (#570 M3c): B sees A's remote screenshare tile; on Xvfb capture is the X11 root grab.

const fs = require("fs");
const path = require("path");
const h = require("./lib/harness");

const ARTIFACTS = path.join(__dirname, "artifacts");
const shot = h.makeShot(ARTIFACTS);

const PIN = "1357";

// Local self-preview keys (screenshare and camera), never a remote feed.
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
// B needs a full SFU publish/subscribe round trip plus the first frame.
const REMOTE_SHARE_TIMEOUT_MS = 120_000;
// Defensive: how long to wait for an in-app picker (Linux enumerates empty, so none).
const PICKER_PROBE_MS = 6_000;

// App env as in two-client-call.js, plus GDK_BACKEND / XDG_SESSION_TYPE=x11 to pin the X11 capture path.
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
    XDG_SESSION_TYPE: "x11",
  };
}

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

// A: DM the target by email, landing on the DM page.
async function startDmTo(browser, targetEmail) {
  await h.clickTestId(browser, "sidebar-row-dms");
  await h.clickTestId(browser, "menu-item-new-dm");
  await h.waitTestId(browser, "start-dm-page", 20000);
  await h.setSelectorValue(browser, "#dm-identifier", targetEmail);
  await h.clickTestId(browser, "start-dm-submit-button");
  await h.waitTestId(browser, "message-form", 30000);
}

// B: poll the DMs list for the request and accept it.
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

// A: re-open the DM so it refetches B's acceptance past the staleTime.
async function reopenDm(browser) {
  await h.clickTestId(browser, "sidebar-row-dms").catch(() => {});
  await h.sleep(1000);
  if (await h.presentSelector(browser, '[data-testid^="dm-option-"]')) {
    await h.clickSelector(browser, '[data-testid^="dm-option-"]');
    await h.waitTestId(browser, "message-form", 15000).catch(() => {});
  }
}

// A: poll until the call button appears (1:1, B accepted and online).
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

// Distinct call participants by user id (mirrors userIdFromVoiceIdentity).
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

// Poll until this client's call stage shows two distinct participants.
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

// Click the VoiceBar screenshare pill, else the stage tray's.
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

// A: start the share; Linux starts directly, but if a picker appears pick the first display.
async function startScreenShare(browser, tag) {
  await clickScreenShareToggle(browser, tag);

  const end = Date.now() + PICKER_PROBE_MS;
  while (Date.now() < end) {
    if (await h.present(browser, "screen-share-picker")) {
      console.log(
        `[two-client-screenshare] ${tag}: a screen-share picker appeared ` +
          "(unexpected on Linux/X11) — selecting the first display source"
      );
      // The picker's source cards have no testid; the first grid button is a display (default tab).
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

// A: poll for A's own share self-preview; fail loudly here on an A-side capture failure.
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

// Remote screenshare tiles: `vs-tile` roots keyed `:screen` (not `:cam`) holding a non-local remote-video-tile.
async function remoteScreenshareTiles(browser) {
  return browser.execute((localKeys) => {
    const out = [];
    for (const tile of document.querySelectorAll('[data-testid^="voice-tile-voice-"]')) {
      // Only participant tile ROOTS (sub-elements are voice-tile-avatar-/quality-/…).
      if (!tile.classList.contains("vs-tile")) {
        continue;
      }
      const tileTestId = tile.getAttribute("data-testid") || "";
      if (!tileTestId.endsWith(":screen")) {
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

    await signUp(A.browser, emailA, "A");
    await shot(A.browser, "two-client-screenshare-A-ready.png");
    await signUp(B.browser, emailB, "B");
    await shot(B.browser, "two-client-screenshare-B-ready.png");

    console.log(`[two-client-screenshare] A: starting DM to ${emailB}`);
    await startDmTo(A.browser, emailB);
    console.log("[two-client-screenshare] B: waiting for the DM request…");
    await acceptIncomingDm(B.browser, REQUEST_TIMEOUT_MS);
    console.log("[two-client-screenshare] B: DM request accepted");

    console.log("[two-client-screenshare] A: waiting to see B online + accepted…");
    await waitForCallButton(A.browser, CALL_BUTTON_TIMEOUT_MS);

    console.log("[two-client-screenshare] A: placing the call");
    await h.clickTestId(A.browser, "dm-header-call");
    await h.waitTestId(A.browser, "call-hang-up", 30000);

    console.log("[two-client-screenshare] B: waiting for the incoming-call alert…");
    await h.waitTestId(B.browser, "status-bar-incoming-call-accept", INCOMING_CALL_TIMEOUT_MS);
    await h.clickTestId(B.browser, "status-bar-incoming-call-accept");
    await h.waitTestId(B.browser, "call-hang-up", 30000);

    console.log("[two-client-screenshare] waiting for both sides to converge in the call…");
    await waitForRemoteParticipant(A.browser, "A", CONVERGE_TIMEOUT_MS);
    await waitForRemoteParticipant(B.browser, "B", CONVERGE_TIMEOUT_MS);
    await shot(A.browser, "two-client-screenshare-A-in-call.png");
    await shot(B.browser, "two-client-screenshare-B-in-call.png");
    console.log("[two-client-screenshare] both clients are in the call");

    console.log("[two-client-screenshare] A: starting a screen share");
    await startScreenShare(A.browser, "A");

    // Sanity: A's local self-preview mounts → capture + publish engaged.
    await waitForLocalSharePreview(A.browser, "A", LOCAL_SHARE_TIMEOUT_MS);
    await shot(A.browser, "two-client-screenshare-A-sharing.png");

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
