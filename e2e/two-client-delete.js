#!/usr/bin/env node
/*
 * Two-client DELETE-FOR-EVERYONE E2E (delete-for-everyone / E2EE redaction).
 *
 * two-client-dm-reply.js proves A<->B DM delivery. This proves the redaction
 * leg: after A's message has CONVERGED into B (B already holds a local copy), A
 * deletes its own message and B's already-delivered copy must flip to the
 * "[deleted]" tombstone — the whole point of "delete for everyone". A one-client
 * or send-then-delete-before-fetch test would NOT catch the bug this guards; the
 * message has to reach B FIRST, then be redacted.
 *
 * Same isolation + backend assumptions as two-client-dm-reply.js (two isolated
 * instances, one shared external backend from start-backend.sh, one Vite server;
 * no LiveKit — convergence is by re-opening the DM to re-fire the debounced
 * ingest pull).
 *
 * Choreography:
 *   1. A + B sign up.
 *   2. A DMs B by email; B accepts the request.
 *   3. A sends msg; both A (local) and B (converged) render it.
 *   4. A deletes msg via the message hover toolbar's delete button + confirm bar.
 *   5. A's OWN view flips to "[deleted]" (soft-delete tombstone, not a vanish).
 *   6. B — who ALREADY received it — polls until its copy is "[deleted]" and the
 *      original token is gone: the redaction reached an already-delivered device.
 *
 * On failure, dumps per-client A-FAIL.* / B-FAIL.* into e2e/artifacts/.
 */

const fs = require("fs");
const path = require("path");
const h = require("./lib/harness");

const ARTIFACTS = path.join(__dirname, "artifacts");
const shot = h.makeShot(ARTIFACTS);

const PIN = "1357";
const REQUEST_TIMEOUT_MS = 120_000;
const MESSAGE_TIMEOUT_MS = 180_000;
const DELETED_MARKER = "[deleted]";

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
  console.log(`[delete] ${tag}: signing up ${email}`);
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
  console.log(`[delete] ${tag}: reached app-ready`);
}

async function startDmTo(browser, targetEmail) {
  await h.clickTestId(browser, "sidebar-row-dms");
  await h.clickTestId(browser, "menu-item-new-dm");
  await h.waitTestId(browser, "start-dm-page", 20000);
  await h.setSelectorValue(browser, "#dm-identifier", targetEmail);
  await h.clickTestId(browser, "start-dm-submit-button");
  await h.waitTestId(browser, "message-form", 30000);
}

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
    console.log(`[delete] B: no DM request yet (attempt ${attempt}), waiting…`);
    await h.sleep(Math.min(remaining > 0 ? remaining : 0, attempt === 1 ? 4000 : 32000));
  }
  throw new Error("B: DM request never appeared");
}

// Does some message-content node contain `token`?
async function messageVisible(browser, token) {
  return browser.execute((tok) => {
    const nodes = document.querySelectorAll('[data-testid="message-content"]');
    for (const n of nodes) {
      if ((n.textContent || "").includes(tok)) { return true; }
    }
    return false;
  }, token);
}

// Open the (only) DM conversation on this client — Home -> DMs -> first option.
async function openDm(browser) {
  await h.clickTestId(browser, "sidebar-row-dms").catch(() => {});
  await h.sleep(1000);
  if (await h.presentSelector(browser, '[data-testid^="dm-option-"]')) {
    await h.clickSelector(browser, '[data-testid^="dm-option-"]');
    await h.waitTestId(browser, "message-form", 15000).catch(() => {});
  }
}

// Poll until `token` renders, re-opening the DM each round to re-fire the
// 5s-debounced ingest_dm_envelopes pull.
async function waitForMessage(browser, token, timeoutMs) {
  const end = Date.now() + timeoutMs;
  while (Date.now() < end) {
    if (await messageVisible(browser, token)) {
      return;
    }
    await openDm(browser);
    await h.sleep(6000);
  }
  throw new Error(`message "${token}" never converged`);
}

async function sendMessage(browser, text) {
  await h.setTestIdValue(browser, "message-input", text);
  await h.clickTestId(browser, "message-send-button");
}

// Delete the message currently carrying `token` from this client (the sender).
// Drives the real UI: the hover toolbar's delete button opens the confirm bar
// that replaces the composer (NO MODALS), then the confirm button commits.
async function deleteMessage(browser) {
  await h.clickTestId(browser, "delete-button");
  await h.waitTestId(browser, "delete-message-bar", 15000);
  await h.clickTestId(browser, "delete-message-confirm");
  // The confirm bar closes and the composer returns once the delete resolves.
  await h.waitTestId(browser, "message-form", 30000);
}

// True once SOME message-content shows the "[deleted]" tombstone.
async function deletedMarkerVisible(browser) {
  return browser.execute((marker) => {
    const nodes = document.querySelectorAll('[data-testid="message-content"]');
    for (const n of nodes) {
      if ((n.textContent || "").includes(marker)) { return true; }
    }
    return false;
  }, DELETED_MARKER);
}

// Poll until the message is redacted here: the "[deleted]" tombstone renders AND
// the original token is gone. Re-opens the DM each round to re-fire ingest, so
// an already-delivered recipient pulls the redaction envelope.
async function waitForRedacted(browser, token, timeoutMs) {
  const end = Date.now() + timeoutMs;
  while (Date.now() < end) {
    const deleted = await deletedMarkerVisible(browser);
    const stillThere = await messageVisible(browser, token);
    if (deleted && !stillThere) {
      return;
    }
    await openDm(browser);
    await h.sleep(6000);
  }
  throw new Error(`message "${token}" was never redacted (no "${DELETED_MARKER}" / token still present)`);
}

async function main() {
  h.reap();
  const devEnv = h.readEnvFile(".env.development");
  const turso = h.tursoEnv();
  const deliveryUrl = process.env.POLLIS_DELIVERY_URL;
  if (!deliveryUrl) {
    throw new Error("POLLIS_DELIVERY_URL is not set — run e2e/scripts/start-backend.sh first.");
  }

  const children = [];
  const clients = [];
  const stop = (c) => { try { c && c.kill("SIGKILL"); } catch (_) {} };

  const vite = h.spawnVite(devEnv);
  children.push(vite);

  const stamp = Date.now();
  const emailA = `e2e_del_a_${stamp}@pollis.test`;
  const emailB = `e2e_del_b_${stamp}@pollis.test`;
  const token = `secret-${stamp}`;

  let code = 1;
  let A;
  let B;
  try {
    await h.waitViteReady();
    console.log(`[delete] using external delivery service at ${deliveryUrl}`);

    A = await h.startClient({
      index: 0, label: "A",
      appEnv: appEnvFor(devEnv, turso, deliveryUrl, path.join(__dirname, ".tmp-data-del-a")),
    });
    clients.push(A);
    B = await h.startClient({
      index: 1, label: "B",
      appEnv: appEnvFor(devEnv, turso, deliveryUrl, path.join(__dirname, ".tmp-data-del-b")),
    });
    clients.push(B);

    await signUp(A.browser, emailA, "A");
    await signUp(B.browser, emailB, "B");

    console.log(`[delete] A: starting DM to ${emailB}`);
    await startDmTo(A.browser, emailB);
    console.log("[delete] B: accepting the DM request…");
    await acceptIncomingDm(B.browser, REQUEST_TIMEOUT_MS);

    // Deliver A -> B and confirm B actually HAS it before we delete.
    console.log(`[delete] A: sending msg (${token})`);
    await sendMessage(A.browser, `top secret ${token}`);
    await waitForMessage(A.browser, token, 30000);
    await waitForMessage(B.browser, token, MESSAGE_TIMEOUT_MS);
    console.log("[delete] message converged into B (already delivered)");
    await shot(B.browser, "delete-B-got-message.png");

    // A deletes its own message — delete for everyone.
    console.log("[delete] A: deleting the message for everyone");
    await openDm(A.browser);
    await deleteMessage(A.browser);

    // A's own view is a tombstone (soft-delete, not a silent vanish).
    await waitForRedacted(A.browser, token, 30000);
    console.log("[delete] A's own view shows the tombstone");
    await shot(A.browser, "delete-A-redacted.png");

    // The real assertion: B, who ALREADY received it, sees it redacted too.
    await waitForRedacted(B.browser, token, MESSAGE_TIMEOUT_MS);
    console.log(`[delete] SUCCESS: B's already-delivered copy was redacted ("${DELETED_MARKER}")`);
    await shot(B.browser, "delete-B-redacted.png");
    code = 0;
  } catch (err) {
    console.error("[delete] FAILED:", err.message);
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
  console.error(`[delete] ${tag} on-screen testids:`, [...new Set(ids)].join(", "));
}

main().catch((e) => { console.error("[delete] fatal:", e); h.reap(); process.exit(1); });
