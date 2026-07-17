#!/usr/bin/env node
/*
 * Two-client VOICE CHANNEL join/leave E2E (issue #570, extends M3).
 *
 * The M3a call test covers 1:1 DM calls. This covers the other real-time voice
 * surface — a Slack/Discord-style persistent GROUP voice channel: A + B are both
 * members of a group, both JOIN the same voice channel, each sees the other as a
 * participant, then A LEAVES and B sees the participant count drop back to one.
 *
 * Needs the same media stack as the call test (LiveKit + audio) PLUS group
 * membership (join_voice_channel derives an MLS-based E2EE voice key and fails
 * closed if the joiner isn't in the group's MLS state).
 *
 * Choreography:
 *   1. A + B sign up.
 *   2. A creates a group, invites B, B accepts (MLS welcome processed).
 *   3. A creates a VOICE channel (toggles the create-channel type switch).
 *   4. A opens the voice channel and clicks Join (voice-join-cta); waits until
 *      joined (voice-channel-view present).
 *   5. B opens the same voice channel and joins.
 *   6. ASSERT: on the joined side, the live participant roster shows 2 distinct
 *      users (both A and B). LiveKit presence is live, no polling of remote DB.
 *   7. A leaves (voice-tray-leave); B's participant count drops back to 1.
 *
 * On failure, dumps per-client A-FAIL.* / B-FAIL.* into e2e/artifacts/.
 */

const fs = require("fs");
const path = require("path");
const h = require("./lib/harness");

const ARTIFACTS = path.join(__dirname, "artifacts");
const shot = h.makeShot(ARTIFACTS);

const PIN = "1357";
const INVITE_TIMEOUT_MS = 120_000;
const GROUP_VISIBLE_TIMEOUT_MS = 90_000;
const JOIN_TIMEOUT_MS = 60_000;
const CONVERGE_TIMEOUT_MS = 120_000;

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
  console.log(`[voice-channel] ${tag}: signing up ${email}`);
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
  console.log(`[voice-channel] ${tag}: reached app-ready`);
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

async function inviteMember(browser, email) {
  await h.clickTestId(browser, "menu-item-invite-member");
  await h.waitTestId(browser, "invite-member-page", 20000);
  await h.setSelectorValue(browser, "#invite-username", email);
  await h.clickTestId(browser, "send-invite-button");
  await h.waitTestId(browser, "invite-sent-confirmation", 20000);
  await h.clickTestId(browser, "breadcrumb-back-button");
  await h.waitTestId(browser, "menu-item-create-channel", 20000);
}

// Create a VOICE channel: toggle the type switch (#create-channel-type, default
// "text") to voice before submitting. A voice channel returns to the group page.
async function createVoiceChannel(browser, channelName) {
  await h.clickTestId(browser, "menu-item-create-channel");
  await h.waitTestId(browser, "create-channel-page", 20000);
  await h.setSelectorValue(browser, "#create-channel-name", channelName);
  await h.clickSelector(browser, "#create-channel-type");
  await h.clickTestId(browser, "create-channel-submit-button");
  // Returns to the group page; the new voice channel-option should appear.
  await h.waitTestId(browser, "menu-item-create-channel", 30000);
}

async function acceptInvite(browser, timeoutMs) {
  const end = Date.now() + timeoutMs;
  let attempt = 0;
  while (Date.now() < end) {
    attempt++;
    await goHome(browser);
    await h.clickTestId(browser, "menu-item-invites");
    await h.waitTestId(browser, "invites-page", 15000);
    if (await h.presentSelector(browser, '[data-testid^="accept-invite-"]')) {
      await h.clickSelector(browser, '[data-testid^="accept-invite-"]');
      await h.sleep(3000);
      return;
    }
    const remaining = end - Date.now();
    console.log(`[voice-channel] B: no invite yet (attempt ${attempt}), waiting…`);
    await h.sleep(Math.min(remaining > 0 ? remaining : 0, attempt === 1 ? 4000 : 15000));
  }
  throw new Error("B: group invite never appeared");
}

// Navigate into the group and click the voice channel-option (by name), landing
// on the not-joined VoiceStage (voice-channel-observers + voice-join-cta).
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
    console.log(`[voice-channel] group "${groupName}" not visible yet, waiting…`);
    await h.sleep(6000);
  }
  throw new Error(`group "${groupName}" never appeared`);
}

// Click Join and wait until joined (the joined spotlight/grid mounts).
async function joinVoice(browser, tag) {
  await h.clickTestId(browser, "voice-join-cta");
  await h.waitTestId(browser, "voice-channel-view", JOIN_TIMEOUT_MS);
  console.log(`[voice-channel] ${tag}: joined the voice channel`);
}

// Distinct participant user IDs from the live roster. Root tiles carry class
// vs-tile and testid voice-tile-<identity> (identity = voice-{user}[:device]);
// mirrors userIdFromVoiceIdentity. Constrain to .vs-tile so the many
// voice-tile-<sub>-* elements don't over-match.
async function participantUserIds(browser) {
  return browser.execute(() => {
    const ids = new Set();
    for (const el of document.querySelectorAll('.vs-tile[data-testid^="voice-tile-"]')) {
      let id = (el.getAttribute("data-testid") || "").slice("voice-tile-".length);
      if (id.startsWith("voice-")) { id = id.slice("voice-".length); }
      const c = id.indexOf(":");
      ids.add(c === -1 ? id : id.slice(0, c));
    }
    return Array.from(ids);
  });
}

async function waitForParticipantCount(browser, tag, n, timeoutMs) {
  const end = Date.now() + timeoutMs;
  let last = -1;
  while (Date.now() < end) {
    const ids = await participantUserIds(browser).catch(() => []);
    if (ids.length !== last) {
      console.log(`[voice-channel] ${tag}: sees ${ids.length} participant(s)`);
      last = ids.length;
    }
    if (ids.length === n) {
      return;
    }
    await h.sleep(2000);
  }
  throw new Error(`${tag}: participant count never reached ${n} (last ${last})`);
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
  const emailA = `e2e_vc_a_${stamp}@pollis.test`;
  const emailB = `e2e_vc_b_${stamp}@pollis.test`;
  const groupName = `vcgrp${stamp}`;
  const channelName = `vcchan${stamp}`;

  let code = 1;
  let A;
  let B;
  try {
    await h.waitViteReady();
    console.log(`[voice-channel] delivery ${deliveryUrl}, livekit ${process.env.LIVEKIT_URL}`);

    A = await h.startClient({
      index: 0, label: "A",
      appEnv: appEnvFor(devEnv, turso, deliveryUrl, path.join(__dirname, ".tmp-data-vc-a")),
    });
    clients.push(A);
    B = await h.startClient({
      index: 1, label: "B",
      appEnv: appEnvFor(devEnv, turso, deliveryUrl, path.join(__dirname, ".tmp-data-vc-b")),
    });
    clients.push(B);

    await signUp(A.browser, emailA, "A");
    await signUp(B.browser, emailB, "B");

    // A: group + invite + voice channel.
    console.log(`[voice-channel] A: creating group "${groupName}"`);
    await createGroup(A.browser, groupName);
    await inviteMember(A.browser, emailB);
    console.log(`[voice-channel] A: creating voice channel "${channelName}"`);
    await createVoiceChannel(A.browser, channelName);

    // B: accept invite.
    console.log("[voice-channel] B: accepting invite…");
    await acceptInvite(B.browser, INVITE_TIMEOUT_MS);

    // A joins the voice channel.
    console.log("[voice-channel] A: opening + joining the voice channel");
    await openVoiceChannel(A.browser, groupName, channelName, GROUP_VISIBLE_TIMEOUT_MS);
    await joinVoice(A.browser, "A");
    await shot(A.browser, "voice-channel-A-joined.png");

    // B joins the same voice channel.
    console.log("[voice-channel] B: opening + joining the voice channel");
    await openVoiceChannel(B.browser, groupName, channelName, GROUP_VISIBLE_TIMEOUT_MS);
    await joinVoice(B.browser, "B");
    await shot(B.browser, "voice-channel-B-joined.png");

    // ASSERT: both see 2 participants.
    console.log("[voice-channel] waiting for both to see 2 participants…");
    await waitForParticipantCount(A.browser, "A", 2, CONVERGE_TIMEOUT_MS);
    await waitForParticipantCount(B.browser, "B", 2, CONVERGE_TIMEOUT_MS);
    await shot(A.browser, "voice-channel-A-two.png");
    await shot(B.browser, "voice-channel-B-two.png");
    console.log("[voice-channel] both clients see 2 participants");

    // A leaves; B should drop back to 1.
    console.log("[voice-channel] A: leaving the voice channel");
    await h.clickTestId(A.browser, "voice-tray-leave");
    await h.waitTestId(A.browser, "voice-join-cta", 30000);
    console.log("[voice-channel] A: left (join CTA reappeared)");
    await waitForParticipantCount(B.browser, "B", 1, CONVERGE_TIMEOUT_MS);
    await shot(B.browser, "voice-channel-B-one.png");
    console.log("[voice-channel] SUCCESS: B saw A leave (back to 1 participant)");
    code = 0;
  } catch (err) {
    console.error("[voice-channel] FAILED:", err.message);
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

async function dumpClient(browser, tag) {
  await shot(browser, `${tag}-FAIL.png`).catch(() => {});
  const src = await browser.getPageSource().catch(() => "");
  fs.mkdirSync(ARTIFACTS, { recursive: true });
  fs.writeFileSync(path.join(ARTIFACTS, `${tag}-FAIL.html`), src);
  const ids = [...src.matchAll(/data-testid="([^"]+)"/g)].map((m) => m[1]);
  console.error(`[voice-channel] ${tag} on-screen testids:`, [...new Set(ids)].join(", "));
}

main().catch((e) => { console.error("[voice-channel] fatal:", e); h.reap(); process.exit(1); });
