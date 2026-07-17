#!/usr/bin/env node
/*
 * Two-client 1:1 CALL E2E — the first MEDIA slice (issue #570, M3a).
 *
 * Builds on the M2 convergence test (two-client.js): TWO isolated real Pollis
 * desktop app instances against ONE shared backend, but now also against an
 * ephemeral LiveKit SFU (e2e/scripts/start-livekit.sh) with headless virtual
 * audio (e2e/scripts/start-audio.sh) so a real call can actually join and
 * publish a mic track.
 *
 * Choreography — a call is placed ON TOP of an established DM, so the signup +
 * DM-establish steps are reused verbatim from two-client.js:
 *   1. A + B both sign up through the real UI.
 *   2. A starts a DM to B by email (adds B to the DM's MLS group).
 *   3. B polls its DMs for the incoming request and accepts it.
 *   4. A waits until it sees B online AND B's acceptance (the `dm-header-call`
 *      button only renders when `canCall` = 1:1 && otherAccepted && otherOnline
 *      — DM.tsx). Presence flows from the shared DM LiveKit realtime room, so
 *      this doubles as proof the realtime plumbing is up. Re-mounts the DM each
 *      round to defeat React Query's staleTime on the acceptance metadata.
 *   5. A clicks the DM-header phone (`dm-header-call`) → navigates to the Call
 *      page, which auto-joins the `call-<id>` LiveKit room and publishes mic.
 *   6. B gets the incoming-call alert in its status bar (delivered over B's
 *      inbox realtime room) and accepts it (`status-bar-incoming-call-accept`)
 *      → B joins the same room.
 *   7. ASSERT CONVERGENCE: on BOTH clients, poll until the call stage shows two
 *      distinct participants — i.e. each side renders a `voice-tile-voice-<id>`
 *      for the OTHER user (StageTile.tsx). Generous eventual timeout; no fixed
 *      sleeps for correctness.
 *   8. A hangs up (`call-hang-up`).
 *
 * If audio devices or LiveKit are not up, the JOIN fails fast inside the app
 * (join_voice_channel returns an error if cpal can't open a device or
 * LIVEKIT_URL is empty) — this test then fails LOUDLY on the convergence poll
 * with the on-screen testids dumped, rather than hanging.
 *
 * Assumes the media + backend fixtures are already up (the workflow runs
 * start-audio.sh → start-livekit.sh → start-backend.sh first). Both clients read
 * POLLIS_DELIVERY_URL / TURSO_URL / TURSO_TOKEN / LIVEKIT_URL / PULSE_* from the
 * env.
 *
 * On failure, dumps per-client screenshots + page source into e2e/artifacts/.
 */

const fs = require("fs");
const path = require("path");
const h = require("./lib/harness");

const ARTIFACTS = path.join(__dirname, "artifacts");
const shot = h.makeShot(ARTIFACTS);

const PIN = "1357";

// Cross-client propagation is eventual (remote metadata reads + LiveKit
// realtime presence + the SFU join handshake), so every wait is generous.
const REQUEST_TIMEOUT_MS = 120_000;
// A must see B online + accepted before the call button appears.
const CALL_BUTTON_TIMEOUT_MS = 120_000;
// B's incoming-call alert arrives over its inbox realtime room after A joins.
const INCOMING_CALL_TIMEOUT_MS = 90_000;
// Both sides must render the other's participant tile after joining the room.
const CONVERGE_TIMEOUT_MS = 120_000;

// Build the app env for one client — identical to two-client.js, plus it lets
// the LiveKit + PulseAudio env (LIVEKIT_URL / PULSE_SERVER / PULSE_SINK /
// PULSE_SOURCE, injected by the fixtures) flow through via ...process.env.
function appEnvFor(devEnv, turso, deliveryUrl, dataDir) {
  fs.rmSync(dataDir, { recursive: true, force: true });
  fs.mkdirSync(dataDir, { recursive: true });
  return {
    ...process.env, ...devEnv,
    TURSO_URL: turso.TURSO_URL, TURSO_TOKEN: turso.TURSO_TOKEN,
    POLLIS_DELIVERY_URL: deliveryUrl,
    POLLIS_DATA_DIR: dataDir,
    WEBKIT_DISABLE_COMPOSITING_MODE: "1", GDK_BACKEND: "x11",
  };
}

// Full signup through the real UI — copied verbatim from two-client.js. Leaves
// the client on [data-testid="app-ready"].
async function signUp(browser, email, tag) {
  console.log(`[two-client-call] ${tag}: signing up ${email}`);
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
  console.log(`[two-client-call] ${tag}: reached app-ready`);
}

// A: open "New Message", DM the target by email, land on the DM page. Copied
// verbatim from two-client.js.
async function startDmTo(browser, targetEmail) {
  await h.clickTestId(browser, "sidebar-row-dms");
  await h.clickTestId(browser, "menu-item-new-dm");
  await h.waitTestId(browser, "start-dm-page", 20000);
  await h.setSelectorValue(browser, "#dm-identifier", targetEmail);
  await h.clickTestId(browser, "start-dm-submit-button");
  await h.waitTestId(browser, "message-form", 30000);
}

// B: poll the DMs list until the incoming request surfaces and accept it.
// Copied verbatim from two-client.js.
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
    console.log(`[two-client-call] B: no DM request yet (attempt ${attempt}), waiting…`);
    await h.sleep(Math.min(remaining > 0 ? remaining : 0, attempt === 1 ? 4000 : 32000));
  }
  throw new Error("B: DM request never appeared");
}

// A: re-open the DM (Home → DMs → first conversation) so DM.tsx remounts and
// React Query refetches the conversation — this is how A picks up B's
// acceptance metadata (`otherAcceptedAt`) past the 30s staleTime. Presence
// (`isOtherOnline`) already updates live via the MobX presence store.
async function reopenDm(browser) {
  await h.clickTestId(browser, "sidebar-row-dms").catch(() => {});
  await h.sleep(1000);
  if (await h.presentSelector(browser, '[data-testid^="dm-option-"]')) {
    await h.clickSelector(browser, '[data-testid^="dm-option-"]');
    await h.waitTestId(browser, "message-form", 15000).catch(() => {});
  }
}

// A: poll until the DM-header call button appears. It only renders when
// `canCall` is true — the DM is 1:1, B has accepted, AND B is seen online (via
// the shared DM realtime room). So this also proves the LiveKit realtime
// presence path is alive before we attempt the actual call.
async function waitForCallButton(browser, timeoutMs) {
  const end = Date.now() + timeoutMs;
  let attempt = 0;
  while (Date.now() < end) {
    attempt++;
    if (await h.present(browser, "dm-header-call")) {
      return;
    }
    console.log(`[two-client-call] A: call button not ready yet (attempt ${attempt}) — B not seen online/accepted; re-opening DM…`);
    await reopenDm(browser);
    // Give presence + the refetch a moment before the next remount.
    await h.sleep(4000);
  }
  throw new Error(
    "A: the DM-header call button never appeared — A never saw B both accepted " +
      "and online. Check LiveKit realtime presence on the shared DM room."
  );
}

// Distinct call participants visible on this client, by user id. A call tile's
// root testid is `voice-tile-${identity}` where identity is `voice-{userId}` or
// `voice-{userId}:{device}` — so the roots (and ONLY the roots; the sub-elements
// are `voice-tile-avatar-…`, `voice-tile-quality-…`, etc.) match the
// `voice-tile-voice-` prefix. Mirrors userIdFromVoiceIdentity.
async function callParticipantUserIds(browser) {
  return browser.execute(() => {
    const ids = new Set();
    for (const el of document.querySelectorAll('[data-testid^="voice-tile-voice-"]')) {
      const tid = el.getAttribute("data-testid") || "";
      // strip "voice-tile-" → "voice-{user}[:device]"
      let identity = tid.slice("voice-tile-".length);
      // strip "voice-" → "{user}[:device]"
      if (identity.startsWith("voice-")) {
        identity = identity.slice("voice-".length);
      }
      const colon = identity.indexOf(":");
      ids.add(colon === -1 ? identity : identity.slice(0, colon));
    }
    return Array.from(ids);
  });
}

// Poll until this client's call stage shows at least two distinct participants
// — i.e. the local user plus the remote peer's tile. A 1:1 call has exactly two
// people, so "2 distinct users" == "the other participant converged in".
async function waitForRemoteParticipant(browser, tag, timeoutMs) {
  const end = Date.now() + timeoutMs;
  while (Date.now() < end) {
    const ids = await callParticipantUserIds(browser).catch(() => []);
    if (ids.length >= 2) {
      console.log(`[two-client-call] ${tag}: sees ${ids.length} participants in the call`);
      return;
    }
    await h.sleep(2000);
  }
  throw new Error(`${tag}: the other call participant never appeared (no second voice tile)`);
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
  // The call cannot join without LiveKit configured for the app; fail early and
  // clearly rather than deep inside join_voice_channel.
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
  const emailA = `e2e_call_a_${stamp}@pollis.test`;
  const emailB = `e2e_call_b_${stamp}@pollis.test`;

  let code = 1;
  let A;
  let B;
  try {
    await h.waitViteReady();
    console.log(`[two-client-call] using external delivery service at ${deliveryUrl}`);
    console.log(`[two-client-call] using LiveKit at ${process.env.LIVEKIT_URL}`);

    A = await h.startClient({
      index: 0, label: "A",
      appEnv: appEnvFor(devEnv, turso, deliveryUrl, path.join(__dirname, ".tmp-data-call-a")),
    });
    clients.push(A);
    B = await h.startClient({
      index: 1, label: "B",
      appEnv: appEnvFor(devEnv, turso, deliveryUrl, path.join(__dirname, ".tmp-data-call-b")),
    });
    clients.push(B);

    // Both sign up.
    await signUp(A.browser, emailA, "A");
    await shot(A.browser, "two-client-call-A-ready.png");
    await signUp(B.browser, emailB, "B");
    await shot(B.browser, "two-client-call-B-ready.png");

    // A → DM B; B → accept. Establishes the DM + MLS group both directions.
    console.log(`[two-client-call] A: starting DM to ${emailB}`);
    await startDmTo(A.browser, emailB);
    console.log("[two-client-call] B: waiting for the DM request…");
    await acceptIncomingDm(B.browser, REQUEST_TIMEOUT_MS);
    console.log("[two-client-call] B: DM request accepted");

    // A → wait until B is seen online + accepted (call button renders).
    console.log("[two-client-call] A: waiting to see B online + accepted…");
    await waitForCallButton(A.browser, CALL_BUTTON_TIMEOUT_MS);
    await shot(A.browser, "two-client-call-A-can-call.png");

    // A → place the call. Navigates to the Call page, which auto-joins the
    // LiveKit room and publishes the mic.
    console.log("[two-client-call] A: placing the call");
    await h.clickTestId(A.browser, "dm-header-call");
    await h.waitTestId(A.browser, "call-hang-up", 30000);

    // B → accept the incoming-call alert (delivered over B's inbox realtime).
    console.log("[two-client-call] B: waiting for the incoming-call alert…");
    await h.waitTestId(B.browser, "status-bar-incoming-call-accept", INCOMING_CALL_TIMEOUT_MS);
    await shot(B.browser, "two-client-call-B-incoming.png");
    await h.clickTestId(B.browser, "status-bar-incoming-call-accept");
    await h.waitTestId(B.browser, "call-hang-up", 30000);

    // ASSERT: each side sees the other participant's tile in the call.
    console.log("[two-client-call] waiting for both sides to see each other…");
    await waitForRemoteParticipant(A.browser, "A", CONVERGE_TIMEOUT_MS);
    await shot(A.browser, "two-client-call-A-in-call.png");
    await waitForRemoteParticipant(B.browser, "B", CONVERGE_TIMEOUT_MS);
    await shot(B.browser, "two-client-call-B-in-call.png");
    console.log("[two-client-call] SUCCESS: both clients see each other in the call");

    // Hang up from A (best-effort — the run already succeeded).
    await h.clickTestId(A.browser, "call-hang-up").catch(() => {});
    code = 0;
  } catch (err) {
    console.error("[two-client-call] FAILED:", err.message);
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
  console.error(`[two-client-call] ${tag} on-screen testids:`, [...new Set(ids)].join(", "));
}

main().catch((e) => { console.error("[two-client-call] fatal:", e); h.reap(); process.exit(1); });
