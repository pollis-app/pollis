#!/usr/bin/env node
// Voice frame keys after a mid-call removal (#1850, #1097), three clients on a real LiveKit SFU.
//
// A (admin), B and C share a voice call. A removes C from the group mid-call. Then:
//   1. A and B rotate: both send under the frame key exported from the removal epoch.
//   2. C's key ring (the provider of its join, kept after its session ends) holds no
//      slot with that key.
//   3. C comes back as an adversary: it re-dials the SFU with the token and key
//      provider it held at join (the token is still valid; membership is checked only
//      at mint, #18), so the DS's eviction (#1102) is out of the picture and only the
//      crypto stands between C and the call. Every frame-cryptor verdict C gets for
//      A's and B's tracks must be a failure (MissingKey / DecryptionFailed), never Ok.
//
// Positive controls keep the assertions honest: before the removal C must decrypt A
// and B (verdict Ok) and its ring must hold A's live key, so "no Ok after" and "no
// matching key after" cannot pass merely because nothing was observed.
//
// Needs the app built with `--features pollis/e2e-hooks` (test-only `e2e_voice_*`
// commands, debug builds only), plus start-audio.sh, start-livekit.sh, start-backend.sh.

const path = require("path");
const h = require("./lib/harness");
const v = require("./lib/voice-flow");

const ARTIFACTS = path.join(__dirname, "artifacts");
const shot = h.makeShot(ARTIFACTS);

const INVITE_TIMEOUT_MS = 120_000;
const GROUP_VISIBLE_TIMEOUT_MS = 90_000;
const JOIN_TIMEOUT_MS = 60_000;
const CONVERGE_TIMEOUT_MS = 120_000;
const FRAMES_TIMEOUT_MS = 60_000;
const ROTATION_TIMEOUT_MS = 60_000;
const ADVERSARY_TIMEOUT_MS = 60_000;
// After the adversary's first failures, keep listening this long for a late Ok.
const ADVERSARY_SOAK_MS = 15_000;
const TAG = "voice-removal";

const FAILURE_STATES = new Set(["MissingKey", "DecryptionFailed"]);

// Call a Tauri command from the page (the app's own IPC, as the renderer would).
async function invokeApp(browser, cmd, args = {}) {
  const res = await browser.executeAsync((c, a, done) => {
    window.__TAURI_INTERNALS__.invoke(c, a).then(
      (r) => done({ ok: true, r }),
      (e) => done({ ok: false, e: typeof e === "string" ? e : JSON.stringify(e) }),
    );
  }, cmd, args);
  if (!res.ok) {
    throw new Error(`${cmd}: ${res.e}`);
  }
  return res.r;
}

const snapshot = (browser) => invokeApp(browser, "e2e_voice_e2ee_snapshot");

// Poll `probe` until it returns a truthy value; null on timeout.
async function waitFor(label, timeoutMs, probe) {
  const end = Date.now() + timeoutMs;
  while (Date.now() < end) {
    const r = await probe();
    if (r) {
      return r;
    }
    await h.sleep(2000);
  }
  console.log(`[${TAG}] timed out waiting for ${label}`);
  return null;
}

// Distinct remote identities per verdict, within one scope.
function verdicts(snap, scope) {
  const byState = {};
  for (const r of snap.frame_states.filter((x) => x.scope === scope)) {
    (byState[r.state] = byState[r.state] || new Set()).add(r.identity);
  }
  return byState;
}

function summarize(snap, scope) {
  const out = {};
  for (const [state, ids] of Object.entries(verdicts(snap, scope))) {
    out[state] = ids.size;
  }
  return JSON.stringify(out);
}

function keyLine(name, snap) {
  return `${name}: in_call=${snap.in_call} voice_epoch=${snap.voice_epoch} slot=${snap.sender_key_index} ` +
    `sender_key=${snap.sender_key_fp} mls_epoch=${snap.mls_epoch} mls_key=${snap.mls_epoch_key_fp}`;
}

async function kickMember(browser, groupName, userId) {
  await v.openGroup(browser, groupName, GROUP_VISIBLE_TIMEOUT_MS, TAG);
  await h.clickTestId(browser, "menu-item-members");
  await h.waitTestId(browser, `member-kick-${userId}`, 30000);
  await h.clickTestId(browser, `member-kick-${userId}`);
  await h.waitTestId(browser, "kick-member-confirm", 20000);
  await h.clickTestId(browser, "kick-member-confirm");
  const gone = await waitFor("the kicked member's row to go", 30000, async () =>
    !(await h.present(browser, `member-kick-${userId}`)));
  if (!gone) {
    throw new Error("A: the kicked member is still listed");
  }
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
  const email = (who) => `e2e_vr_${who}_${stamp}@pollis.test`;
  const groupName = `vrgrp${stamp}`;
  const channelName = `vrchan${stamp}`;

  let code = 1;
  const named = {};
  try {
    await h.waitViteReady();
    console.log(`[${TAG}] delivery ${deliveryUrl}, livekit ${process.env.LIVEKIT_URL}`);

    for (const [index, who] of ["A", "B", "C"].entries()) {
      const c = await h.startClient({
        index, label: who,
        appEnv: v.appEnvFor(devEnv, turso, deliveryUrl, path.join(__dirname, `.tmp-data-vr-${who.toLowerCase()}`)),
      });
      clients.push(c);
      named[who] = c.browser;
    }
    const { A, B, C } = named;

    // The hooks must be in this build; fail loudly before any setup if not.
    await snapshot(A);

    await v.signUp(A, email("a"), "A", TAG);
    await v.signUp(B, email("b"), "B", TAG);
    await v.signUp(C, email("c"), "C", TAG);

    console.log(`[${TAG}] A: creating group "${groupName}", inviting B and C, creating voice channel`);
    await v.createGroup(A, groupName);
    await v.inviteMember(A, email("b"));
    await v.inviteMember(A, email("c"));
    await v.createVoiceChannel(A, channelName);
    await v.acceptInvite(B, INVITE_TIMEOUT_MS, "B", TAG);
    await v.acceptInvite(C, INVITE_TIMEOUT_MS, "C", TAG);

    for (const [who, browser] of Object.entries(named)) {
      await v.openVoiceChannel(browser, groupName, channelName, GROUP_VISIBLE_TIMEOUT_MS, TAG);
      await v.joinVoice(browser, JOIN_TIMEOUT_MS, who, TAG);
    }
    for (const [who, browser] of Object.entries(named)) {
      await v.waitForParticipantCount(browser, 3, CONVERGE_TIMEOUT_MS, who, TAG);
    }
    await shot(C, "voice-removal-C-in-call.png");

    // Positive control 1: C decrypts both peers' frames before the removal.
    const cDecrypts = await waitFor("C to decrypt A and B", FRAMES_TIMEOUT_MS, async () => {
      const s = await snapshot(C);
      const ok = verdicts(s, "session").Ok;
      return ok && ok.size >= 2 ? s : null;
    });
    if (!cDecrypts) {
      throw new Error(`C never decrypted both peers before the removal: ${summarize(await snapshot(C), "session")}`);
    }
    console.log(`[${TAG}] control: C decrypts its peers before the removal ${summarize(cDecrypts, "session")}`);

    // Positive control 2: C's ring holds A's live key, so the later "no match" means something.
    const a0 = await snapshot(A);
    const c0 = await snapshot(C);
    console.log(`[${TAG}] before removal: ${keyLine("A", a0)}`);
    if (!a0.sender_key_fp || !c0.last_join_ring.includes(a0.sender_key_fp)) {
      throw new Error("control: C's ring does not hold A's live frame key before the removal");
    }
    const epoch0 = a0.voice_epoch;
    const cUserId = c0.user_id;
    if (!cUserId) {
      throw new Error("C's user id is unknown");
    }

    console.log(`[${TAG}] A: removing C (${cUserId}) from the group mid-call`);
    await kickMember(A, groupName, cUserId);

    // The DS evicts C from the SFU (#1102); its session ends.
    const cOut = await waitFor("C's voice session to end", ROTATION_TIMEOUT_MS, async () =>
      !(await snapshot(C)).in_call);
    if (!cOut) {
      throw new Error("C's voice session never ended after the removal");
    }
    await shot(C, "voice-removal-C-removed.png");

    // The removal commit lands on A and B.
    const committed = await waitFor("A and B to apply the removal commit", ROTATION_TIMEOUT_MS, async () => {
      const [a, b] = [await snapshot(A), await snapshot(B)];
      return a.mls_epoch > epoch0 && b.mls_epoch === a.mls_epoch ? [a, b] : null;
    });
    if (!committed) {
      throw new Error("A and B never reached the removal epoch");
    }
    // Rotation follows the commit; give it a moment to settle, then judge it below.
    await waitFor("A and B to send under the removal epoch", 20000, async () => {
      const [a, b] = [await snapshot(A), await snapshot(B)];
      return a.voice_epoch === a.mls_epoch && b.voice_epoch === b.mls_epoch;
    });

    const failures = [];
    const a1 = await snapshot(A);
    const b1 = await snapshot(B);
    const c1 = await snapshot(C);
    console.log(`[${TAG}] after removal: ${keyLine("A", a1)}`);
    console.log(`[${TAG}] after removal: ${keyLine("B", b1)}`);
    console.log(`[${TAG}] after removal: C ring = ${JSON.stringify(c1.last_join_ring)}`);

    // 1. A and B rotated to the removal epoch's key.
    if (!a1.in_call || !b1.in_call) {
      failures.push("A or B is no longer in the call");
    }
    if (a1.sender_key_fp !== a1.mls_epoch_key_fp) {
      failures.push(`A does not send under the removal epoch's key (sender ${a1.sender_key_fp}, epoch ${a1.mls_epoch} key ${a1.mls_epoch_key_fp})`);
    }
    if (b1.sender_key_fp !== a1.sender_key_fp) {
      failures.push(`A and B send under different keys (${a1.sender_key_fp} vs ${b1.sender_key_fp})`);
    }
    // 2. C holds no current frame key.
    for (const fp of new Set([a1.sender_key_fp, b1.sender_key_fp, a1.mls_epoch_key_fp])) {
      const slot = c1.last_join_ring.indexOf(fp);
      if (fp && slot !== -1) {
        failures.push(`removed member C still holds a current frame key (${fp}) in ring slot ${slot}`);
      }
    }

    // 3. C re-enters with its join-time token and keys and must not decrypt anything.
    console.log(`[${TAG}] C: re-entering the room as an adversary with its join-time token and keys`);
    await invokeApp(C, "e2e_voice_adversary_reenter");
    const sawFailures = await waitFor("the adversary's frame verdicts", ADVERSARY_TIMEOUT_MS, async () => {
      const s = await snapshot(C);
      const by = verdicts(s, "adversary");
      if (by.Ok || by.KeyRatcheted) {
        return s;
      }
      const failed = new Set();
      for (const st of FAILURE_STATES) {
        for (const id of by[st] || []) {
          failed.add(id);
        }
      }
      return failed.size >= 2 ? s : null;
    });
    if (sawFailures) {
      await h.sleep(ADVERSARY_SOAK_MS);
    }
    const cAdv = await snapshot(C);
    const adv = verdicts(cAdv, "adversary");
    console.log(`[${TAG}] adversary verdicts (distinct peers per state): ${summarize(cAdv, "adversary")}`);
    if (adv.Ok || adv.KeyRatcheted) {
      failures.push(`removed member C DECRYPTED frames sent after the removal: ${summarize(cAdv, "adversary")}`);
    }
    const failedPeers = new Set();
    for (const st of FAILURE_STATES) {
      for (const id of adv[st] || []) {
        failedPeers.add(id);
      }
    }
    if (!adv.Ok && !adv.KeyRatcheted && failedPeers.size < 2) {
      failures.push(`the adversary saw a failure verdict for ${failedPeers.size} of 2 peers — frames never reached it, so nothing was proven`);
    }
    await invokeApp(C, "e2e_voice_adversary_leave").catch(() => {});

    if (failures.length) {
      throw new Error(`frame-key checks failed:\n  - ${failures.join("\n  - ")}`);
    }
    console.log(`[${TAG}] SUCCESS: A and B rotated; removed C holds no current key and decrypts nothing after the removal`);
    code = 0;
  } catch (err) {
    console.error(`[${TAG}] FAILED:`, err.message);
    for (const [who, browser] of Object.entries(named)) {
      await v.dumpClient(browser, ARTIFACTS, shot, who, TAG);
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

main().catch((e) => { console.error(`[${TAG}] fatal:`, e); h.reap(); process.exit(1); });
