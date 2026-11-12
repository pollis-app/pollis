#!/usr/bin/env node
// Two-client voice channel join/leave (#570): both see 2 participants, then 1 after A leaves.

const path = require("path");
const h = require("./lib/harness");
const v = require("./lib/voice-flow");

const ARTIFACTS = path.join(__dirname, "artifacts");
const shot = h.makeShot(ARTIFACTS);

const INVITE_TIMEOUT_MS = 120_000;
const GROUP_VISIBLE_TIMEOUT_MS = 90_000;
const JOIN_TIMEOUT_MS = 60_000;
const CONVERGE_TIMEOUT_MS = 120_000;
const TAG = "voice-channel";

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
      appEnv: v.appEnvFor(devEnv, turso, deliveryUrl, path.join(__dirname, ".tmp-data-vc-a")),
    });
    clients.push(A);
    B = await h.startClient({
      index: 1, label: "B",
      appEnv: v.appEnvFor(devEnv, turso, deliveryUrl, path.join(__dirname, ".tmp-data-vc-b")),
    });
    clients.push(B);

    await v.signUp(A.browser, emailA, "A", TAG);
    await v.signUp(B.browser, emailB, "B", TAG);

    console.log(`[voice-channel] A: creating group "${groupName}"`);
    await v.createGroup(A.browser, groupName);
    await v.inviteMember(A.browser, emailB);
    console.log(`[voice-channel] A: creating voice channel "${channelName}"`);
    await v.createVoiceChannel(A.browser, channelName);

    console.log("[voice-channel] B: accepting invite…");
    await v.acceptInvite(B.browser, INVITE_TIMEOUT_MS, "B", TAG);

    console.log("[voice-channel] A: opening + joining the voice channel");
    await v.openVoiceChannel(A.browser, groupName, channelName, GROUP_VISIBLE_TIMEOUT_MS, TAG);
    await v.joinVoice(A.browser, JOIN_TIMEOUT_MS, "A", TAG);
    await shot(A.browser, "voice-channel-A-joined.png");

    console.log("[voice-channel] B: opening + joining the voice channel");
    await v.openVoiceChannel(B.browser, groupName, channelName, GROUP_VISIBLE_TIMEOUT_MS, TAG);
    await v.joinVoice(B.browser, JOIN_TIMEOUT_MS, "B", TAG);
    await shot(B.browser, "voice-channel-B-joined.png");

    console.log("[voice-channel] waiting for both to see 2 participants…");
    await v.waitForParticipantCount(A.browser, 2, CONVERGE_TIMEOUT_MS, "A", TAG);
    await v.waitForParticipantCount(B.browser, 2, CONVERGE_TIMEOUT_MS, "B", TAG);
    await shot(A.browser, "voice-channel-A-two.png");
    await shot(B.browser, "voice-channel-B-two.png");
    console.log("[voice-channel] both clients see 2 participants");

    console.log("[voice-channel] A: leaving the voice channel");
    await h.clickTestId(A.browser, "voice-tray-leave");
    await h.waitTestId(A.browser, "voice-join-cta", 30000);
    console.log("[voice-channel] A: left (join CTA reappeared)");
    await v.waitForParticipantCount(B.browser, 1, CONVERGE_TIMEOUT_MS, "B", TAG);
    await shot(B.browser, "voice-channel-B-one.png");
    console.log("[voice-channel] B saw A leave (back to 1 participant)");

    // A rejoins straight after leaving: the previous room's close runs in the background and the
    // SFU evicts the stale participant, so the rejoin must neither wait on it nor be refused.
    console.log("[voice-channel] A: rejoining immediately");
    await v.joinVoice(A.browser, JOIN_TIMEOUT_MS, "A", TAG);
    await v.waitForParticipantCount(A.browser, 2, CONVERGE_TIMEOUT_MS, "A", TAG);
    await v.waitForParticipantCount(B.browser, 2, CONVERGE_TIMEOUT_MS, "B", TAG);
    await h.clickTestId(A.browser, "voice-tray-leave");
    await h.waitTestId(A.browser, "voice-join-cta", 30000);
    await v.waitForParticipantCount(B.browser, 1, CONVERGE_TIMEOUT_MS, "B", TAG);
    console.log("[voice-channel] SUCCESS: A's immediate rejoin converged on both sides, then left again");
    code = 0;
  } catch (err) {
    console.error("[voice-channel] FAILED:", err.message);
    if (A && A.browser) {
      await v.dumpClient(A.browser, ARTIFACTS, shot, "A", TAG);
    }
    if (B && B.browser) {
      await v.dumpClient(B.browser, ARTIFACTS, shot, "B", TAG);
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

main().catch((e) => { console.error("[voice-channel] fatal:", e); h.reap(); process.exit(1); });
