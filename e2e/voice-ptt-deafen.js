#!/usr/bin/env node
/*
 * Single-client PUSH-TO-TALK + SELF-DEAFEN E2E (#849).
 *
 * Covers the two behaviours a unit test cannot see: that the controls are
 * actually on screen, and that each distinct gate state is *visibly*
 * distinct rather than collapsing into "muted".
 *
 * The interesting assertion is the deafen round-trip. Deafen implies
 * self-mute, so undeafening has to put back the mute state it displaced —
 * blindly unmuting is the classic bug, and it is exactly the one that
 * pushes a live mic at a room that thought it was muted. We drive it both
 * ways here: deafen-from-unmuted must come back unmuted, and
 * deafen-from-muted must come back MUTED.
 *
 * One client is enough — a solo group is its own MLS group, so the voice
 * E2EE key derives fine and the join is self-contained. No second party is
 * needed to observe local gate state.
 *
 * Choreography:
 *   1. A signs up.
 *   2. A creates a group + a VOICE channel, and joins it.
 *   3. ASSERT: mic reads `live`, deafen reads off.
 *   4. Deafen → mic reads `deafened` (NOT plain `muted`), deafen reads on.
 *   5. Undeafen → mic is back to `live`. (Restored the prior UNMUTED state.)
 *   6. Mute → `muted`. Deafen → `deafened`. Undeafen → still `muted`.
 *      (Restored the prior MUTED state — did not blindly unmute.)
 *   7. Unmute, then switch Input Mode to Push to Talk in voice settings.
 *   8. ASSERT: mic reads `ptt-idle` — armed, not muted, and visibly its own
 *      state.
 *
 * On failure, dumps A-FAIL.* into e2e/artifacts/.
 */

const fs = require("fs");
const path = require("path");
const h = require("./lib/harness");

const ARTIFACTS = path.join(__dirname, "artifacts");
const shot = h.makeShot(ARTIFACTS);

const PIN = "2468";
const GROUP_VISIBLE_TIMEOUT_MS = 90_000;
const JOIN_TIMEOUT_MS = 60_000;
const STATE_TIMEOUT_MS = 15_000;

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

async function signUp(browser, email, tag) {
  console.log(`[ptt] ${tag}: signing up ${email}`);
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
  console.log(`[ptt] ${tag}: reached app-ready`);
}

async function clickByPrefixText(browser, prefix, text) {
  const ok = await browser.execute((pfx, needle) => {
    for (const el of document.querySelectorAll(`[data-testid^="${pfx}"]`)) {
      if ((el.textContent || "").includes(needle)) { el.click(); return true; }
    }
    return false;
  }, prefix, text);
  if (!ok) {
    throw new Error(`clickByPrefixText: no ${prefix}* containing "${text}"`);
  }
}

async function prefixTextPresent(browser, prefix, text) {
  return browser.execute((pfx, needle) => {
    for (const el of document.querySelectorAll(`[data-testid^="${pfx}"]`)) {
      if ((el.textContent || "").includes(needle)) { return true; }
    }
    return false;
  }, prefix, text);
}

async function goHome(browser) {
  const clicked = await browser.execute(() => {
    const trail = document.querySelector('[data-testid="breadcrumb-trail"]');
    const btn = trail && trail.querySelector("button");
    if (btn) { btn.click(); return true; }
    return false;
  });
  if (clicked) {
    await h.waitTestId(browser, "menu-item-groups", 15000);
  }
}

async function createGroup(browser, groupName) {
  await h.clickTestId(browser, "menu-item-groups");
  await h.clickTestId(browser, "menu-item-create-group");
  await h.waitTestId(browser, "create-group-page", 20000);
  await h.setSelectorValue(browser, "#create-group-name", groupName);
  await h.clickTestId(browser, "create-group-submit-button");
  await h.waitTestId(browser, "menu-item-create-channel", 30000);
}

async function createVoiceChannel(browser, channelName) {
  await h.clickTestId(browser, "menu-item-create-channel");
  await h.waitTestId(browser, "create-channel-page", 20000);
  await h.setSelectorValue(browser, "#create-channel-name", channelName);
  await h.clickSelector(browser, "#create-channel-type");
  await h.clickTestId(browser, "create-channel-submit-button");
  await h.waitTestId(browser, "menu-item-create-channel", 30000);
}

async function openVoiceChannel(browser, groupName, channelName, groupTimeoutMs) {
  const end = Date.now() + groupTimeoutMs;
  while (Date.now() < end) {
    await goHome(browser);
    await h.clickTestId(browser, "menu-item-groups");
    await h.waitTestId(browser, "menu-item-create-group", 15000);
    if (await prefixTextPresent(browser, "group-option-", groupName)) {
      await clickByPrefixText(browser, "group-option-", groupName);
      await h.waitSelector(browser, '[data-testid^="channel-option-"]', 20000, "a channel row");
      await clickByPrefixText(browser, "channel-option-", channelName);
      await h.waitTestId(browser, "voice-join-cta", 20000);
      return;
    }
    console.log(`[ptt] group "${groupName}" not visible yet, waiting…`);
    await h.sleep(6000);
  }
  throw new Error(`group "${groupName}" never appeared`);
}

// ── Gate-state helpers ─────────────────────────────────────────────────────
//
// The tray publishes the resolved indicator on `data-mic-state`
// (live | muted | deafened | ptt-idle) and `data-deafened`. Asserting on
// those attributes is what makes "PTT-idle is DISTINCT from muted" a real
// assertion rather than a screenshot someone has to eyeball.

async function readAttr(browser, testId, attr) {
  return browser.execute((id, a) => {
    const el = document.querySelector(`[data-testid="${id}"]`);
    return el ? el.getAttribute(a) : null;
  }, testId, attr);
}

async function waitAttr(browser, testId, attr, expected, timeoutMs = STATE_TIMEOUT_MS) {
  const end = Date.now() + timeoutMs;
  let last = null;
  while (Date.now() < end) {
    last = await readAttr(browser, testId, attr);
    if (last === expected) {
      return;
    }
    await h.sleep(300);
  }
  throw new Error(
    `${testId}[${attr}] never became "${expected}" (last saw "${last}")`
  );
}

async function expectMic(browser, expected) {
  await waitAttr(browser, "voice-tray-mute", "data-mic-state", expected);
  console.log(`[ptt] mic state = ${expected}`);
}

async function expectDeafened(browser, expected) {
  await waitAttr(browser, "voice-tray-deafen", "data-deafened", String(expected));
  console.log(`[ptt] deafened = ${expected}`);
}

async function main() {
  h.reap();
  const devEnv = h.readEnvFile(".env.development");
  const turso = h.tursoEnv();
  const deliveryUrl = process.env.POLLIS_DELIVERY_URL;
  if (!deliveryUrl) {
    throw new Error("POLLIS_DELIVERY_URL is not set — run e2e/scripts/start-backend.sh first.");
  }
  if (!process.env.LIVEKIT_URL) {
    throw new Error("LIVEKIT_URL is not set — run e2e/scripts/start-livekit.sh first.");
  }

  const children = [];
  const clients = [];
  const stop = (c) => { try { c && c.kill("SIGKILL"); } catch (_) {} };

  const vite = h.spawnVite(devEnv);
  children.push(vite);

  const stamp = Date.now();
  const emailA = `e2e_ptt_a_${stamp}@pollis.test`;
  const groupName = `pttgrp${stamp}`;
  const channelName = `pttchan${stamp}`;

  let code = 1;
  let A;
  try {
    await h.waitViteReady();
    console.log(`[ptt] delivery ${deliveryUrl}, livekit ${process.env.LIVEKIT_URL}`);

    A = await h.startClient({
      index: 0, label: "A",
      appEnv: appEnvFor(devEnv, turso, deliveryUrl, path.join(__dirname, ".tmp-data-ptt-a")),
    });
    clients.push(A);

    await signUp(A.browser, emailA, "A");

    console.log(`[ptt] A: creating group "${groupName}"`);
    await createGroup(A.browser, groupName);
    console.log(`[ptt] A: creating voice channel "${channelName}"`);
    await createVoiceChannel(A.browser, channelName);

    console.log("[ptt] A: opening + joining the voice channel");
    await openVoiceChannel(A.browser, groupName, channelName, GROUP_VISIBLE_TIMEOUT_MS);
    await h.clickTestId(A.browser, "voice-join-cta");
    await h.waitTestId(A.browser, "voice-channel-view", JOIN_TIMEOUT_MS);

    // This scenario needs a real capture device: with none, the join goes
    // listen-only and the mute toggle is replaced by a static indicator
    // (that path is covered by voice-channel-no-mic.js). Fail loudly rather
    // than silently skipping the assertions.
    const listenOnly = await h.presentSelector(A.browser, '[data-testid="voice-tray-listen-only"]');
    if (listenOnly) {
      throw new Error(
        "joined listen-only — this scenario needs a capture device; run e2e/scripts/start-audio.sh"
      );
    }
    await h.waitTestId(A.browser, "voice-tray-mute", STATE_TIMEOUT_MS);

    // ── 3. Baseline ────────────────────────────────────────────────────────
    await expectMic(A.browser, "live");
    await expectDeafened(A.browser, false);
    await shot(A.browser, "ptt-01-joined-live.png");

    // ── 4. Deafen from UNMUTED ─────────────────────────────────────────────
    console.log("[ptt] A: deafening (from unmuted)");
    await h.clickTestId(A.browser, "voice-tray-deafen");
    await expectDeafened(A.browser, true);
    // Deafened must NOT read as a plain mute — that distinction is the
    // whole point of the four-state indicator.
    await expectMic(A.browser, "deafened");
    await shot(A.browser, "ptt-02-deafened.png");

    // ── 5. Undeafen restores UNMUTED ───────────────────────────────────────
    console.log("[ptt] A: undeafening — must restore the prior UNMUTED state");
    await h.clickTestId(A.browser, "voice-tray-deafen");
    await expectDeafened(A.browser, false);
    await expectMic(A.browser, "live");
    await shot(A.browser, "ptt-03-undeafened-live.png");

    // ── 6. Deafen from MUTED must come back MUTED ──────────────────────────
    console.log("[ptt] A: muting");
    await h.clickTestId(A.browser, "voice-tray-mute");
    await expectMic(A.browser, "muted");
    await shot(A.browser, "ptt-04-muted.png");

    console.log("[ptt] A: deafening (from muted)");
    await h.clickTestId(A.browser, "voice-tray-deafen");
    await expectDeafened(A.browser, true);
    await expectMic(A.browser, "deafened");

    console.log("[ptt] A: undeafening — must stay MUTED, not blindly unmute");
    await h.clickTestId(A.browser, "voice-tray-deafen");
    await expectDeafened(A.browser, false);
    await expectMic(A.browser, "muted");
    await shot(A.browser, "ptt-05-undeafened-still-muted.png");

    // ── 7. Back to unmuted, then switch to push-to-talk ────────────────────
    console.log("[ptt] A: unmuting");
    await h.clickTestId(A.browser, "voice-tray-mute");
    await expectMic(A.browser, "live");

    console.log("[ptt] A: switching Input Mode to Push to Talk");
    await h.clickTestId(A.browser, "voice-settings-link");
    await h.waitTestId(A.browser, "voice-input-mode", 20000);
    await shot(A.browser, "ptt-06-input-mode-setting.png");
    await h.clickTestId(A.browser, "voice-input-mode-push_to_talk");

    // ── 8. The tray now shows PTT armed-and-idle ───────────────────────────
    // Back to the channel; the session is still joined, so the tray is live.
    await h.clickTestId(A.browser, "voice-bar-channel-name");
    await h.waitTestId(A.browser, "voice-tray-mute", 20000);
    await expectMic(A.browser, "ptt-idle");
    await expectDeafened(A.browser, false);
    await shot(A.browser, "ptt-07-push-to-talk-idle.png");

    console.log("[ptt] A: leaving the voice channel");
    await h.clickTestId(A.browser, "voice-tray-leave");
    await h.waitTestId(A.browser, "voice-join-cta", 30000);

    console.log("[ptt] SUCCESS: deafen round-trips restored the prior mute state; PTT-idle is its own state");
    code = 0;
  } catch (err) {
    console.error("[ptt] FAILED:", err.message);
    if (A) {
      await h.dumpFailure(A.browser, ARTIFACTS, shot).catch(() => {});
    }
  } finally {
    for (const c of clients) {
      try { await c.browser.deleteSession(); } catch (_) {}
      stop(c.tauriDriver);
    }
    for (const c of children) { stop(c); }
    h.reap();
  }
  process.exit(code);
}

main();
