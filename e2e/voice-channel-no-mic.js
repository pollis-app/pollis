#!/usr/bin/env node
/*
 * Single-client LISTEN-ONLY voice join E2E.
 *
 * Regression cover for the no-mic join bug: on Wayland/Linux with no capture
 * device, `join_voice_channel` used to hard-fail on `build mic stream` (ALSA
 * "No such file or directory"), so you couldn't even listen in. The mic is now
 * best-effort — a failed capture device joins the room *listen-only* (connected,
 * receiving, not publishing) instead of aborting the whole join.
 *
 * We force that path deterministically with POLLIS_DISABLE_MIC=1 (the same seam
 * the backend uses to skip cpal capture) rather than trying to unplug a virtual
 * device mid-run.
 *
 * Choreography (one client is enough — a solo group is its own MLS group, so the
 * voice E2EE key derives fine and the join is self-contained):
 *   1. A signs up.
 *   2. A creates a group + a VOICE channel.
 *   3. A opens the voice channel and clicks Join — with the mic force-disabled.
 *   4. ASSERT: the join SUCCEEDS anyway (voice-channel-view mounts), and the
 *      tray shows the "listening only" indicator (voice-tray-listen-only)
 *      instead of a live mute toggle (voice-tray-mute).
 *   5. A leaves; the join CTA reappears.
 *
 * On failure, dumps A-FAIL.* into e2e/artifacts/.
 */

const fs = require("fs");
const path = require("path");
const h = require("./lib/harness");

const ARTIFACTS = path.join(__dirname, "artifacts");
const shot = h.makeShot(ARTIFACTS);

const PIN = "1357";
const GROUP_VISIBLE_TIMEOUT_MS = 90_000;
const JOIN_TIMEOUT_MS = 60_000;

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
    // The whole point of this test: force the listen-only path.
    POLLIS_DISABLE_MIC: "1",
  };
}

async function signUp(browser, email, tag) {
  console.log(`[no-mic] ${tag}: signing up ${email}`);
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
  console.log(`[no-mic] ${tag}: reached app-ready`);
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

// Create a VOICE channel: toggle the type switch (#create-channel-type, default
// "text") to voice before submitting. A voice channel returns to the group page.
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
    console.log(`[no-mic] group "${groupName}" not visible yet, waiting…`);
    await h.sleep(6000);
  }
  throw new Error(`group "${groupName}" never appeared`);
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
  const emailA = `e2e_nomic_a_${stamp}@pollis.test`;
  const groupName = `nomicgrp${stamp}`;
  const channelName = `nomicchan${stamp}`;

  let code = 1;
  let A;
  try {
    await h.waitViteReady();
    console.log(`[no-mic] delivery ${deliveryUrl}, livekit ${process.env.LIVEKIT_URL}`);

    A = await h.startClient({
      index: 0, label: "A",
      appEnv: appEnvFor(devEnv, turso, deliveryUrl, path.join(__dirname, ".tmp-data-nomic-a")),
    });
    clients.push(A);

    await signUp(A.browser, emailA, "A");

    console.log(`[no-mic] A: creating group "${groupName}"`);
    await createGroup(A.browser, groupName);
    console.log(`[no-mic] A: creating voice channel "${channelName}"`);
    await createVoiceChannel(A.browser, channelName);

    console.log("[no-mic] A: opening + joining the voice channel (mic disabled)");
    await openVoiceChannel(A.browser, groupName, channelName, GROUP_VISIBLE_TIMEOUT_MS);
    await h.clickTestId(A.browser, "voice-join-cta");

    // ASSERT 1: the join succeeds despite no mic — the joined stage mounts.
    await h.waitTestId(A.browser, "voice-channel-view", JOIN_TIMEOUT_MS);
    console.log("[no-mic] A: joined the voice channel (listen-only)");
    await shot(A.browser, "no-mic-joined.png");

    // ASSERT 2: the tray shows the listen-only indicator, NOT a live mute
    // toggle. The MicAvailability event lands just after join, so wait for it.
    await h.waitTestId(A.browser, "voice-tray-listen-only", 15000);
    const hasMute = await h.presentSelector(A.browser, '[data-testid="voice-tray-mute"]');
    if (hasMute) {
      throw new Error("expected listen-only indicator, but a live mute toggle is present");
    }
    console.log("[no-mic] A: listen-only indicator present, no mute toggle");
    await shot(A.browser, "no-mic-listen-only.png");

    // A leaves; the join CTA reappears.
    console.log("[no-mic] A: leaving the voice channel");
    await h.clickTestId(A.browser, "voice-tray-leave");
    await h.waitTestId(A.browser, "voice-join-cta", 30000);
    console.log("[no-mic] SUCCESS: listen-only join + leave round-tripped");
    code = 0;
  } catch (err) {
    console.error("[no-mic] FAILED:", err.message);
    if (A && A.browser) {
      await dumpClient(A.browser, "A");
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

async function dumpClient(browser, tag) {
  await shot(browser, `${tag}-FAIL.png`).catch(() => {});
  const src = await browser.getPageSource().catch(() => "");
  fs.mkdirSync(ARTIFACTS, { recursive: true });
  fs.writeFileSync(path.join(ARTIFACTS, `${tag}-FAIL.html`), src);
  const ids = [...src.matchAll(/data-testid="([^"]+)"/g)].map((m) => m[1]);
  console.error(`[no-mic] ${tag} on-screen testids:`, [...new Set(ids)].join(", "));
}

main().catch((e) => { console.error("[no-mic] fatal:", e); h.reap(); process.exit(1); });
