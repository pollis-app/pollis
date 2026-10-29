#!/usr/bin/env node
// Delivery + read receipts (#857), refined skin: A's row shows delivered before B opens the DM, then read-all.

const fs = require("fs");
const path = require("path");
const h = require("./lib/harness");

const ARTIFACTS = path.join(__dirname, "artifacts");
const shot = h.makeShot(ARTIFACTS);

const PIN = "1357";
const REQUEST_TIMEOUT_MS = 120_000;
const MESSAGE_TIMEOUT_MS = 180_000;
const RECEIPT_TIMEOUT_MS = 180_000;

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
  console.log(`[receipts] ${tag}: signing up ${email}`);
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
  console.log(`[receipts] ${tag}: reached app-ready`);
}

/** Switch skin via Preferences; receipts render only in the refined skin. */
async function setSkin(browser, skin, tag) {
  console.log(`[receipts] ${tag}: switching to ${skin} skin`);
  await browser.execute(() => { window.history.pushState({}, "", "/preferences"); window.dispatchEvent(new PopStateEvent("popstate")); });
  await h.waitTestId(browser, `pref-skin-${skin}`, 30000);
  await h.clickTestId(browser, `pref-skin-${skin}`);
  await h.sleep(1000);
  await browser.execute(() => { window.history.pushState({}, "", "/"); window.dispatchEvent(new PopStateEvent("popstate")); });
  await h.sleep(500);
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
    console.log(`[receipts] B: no DM request yet (attempt ${attempt}), waiting…`);
    await h.sleep(Math.min(remaining > 0 ? remaining : 0, attempt === 1 ? 4000 : 32000));
  }
  throw new Error("B: DM request never appeared");
}

async function messageVisible(browser, token) {
  return browser.execute((tok) => {
    const nodes = document.querySelectorAll('[data-testid="message-content"]');
    for (const n of nodes) {
      if ((n.textContent || "").includes(tok)) { return true; }
    }
    return false;
  }, token);
}

async function openDm(browser) {
  await h.clickTestId(browser, "sidebar-row-dms").catch(() => {});
  await h.sleep(1000);
  if (await h.presentSelector(browser, '[data-testid^="dm-option-"]')) {
    await h.clickSelector(browser, '[data-testid^="dm-option-"]');
    await h.waitTestId(browser, "message-form", 15000).catch(() => {});
  }
}

// Park B off the conversation: background ingest sends DELIVERED while nothing is on screen.
async function leaveDm(browser) {
  await h.clickTestId(browser, "sidebar-row-account").catch(() => {});
  await h.sleep(500);
}

async function sendMessage(browser, text) {
  await h.setComposerText(browser, text);
  await h.clickTestId(browser, "message-send-button");
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

// The receipt state currently rendered on the row whose body contains `token`.
// Returns null when the message is not on screen or carries no indicator.
async function receiptState(browser, token) {
  return browser.execute((tok) => {
    const rows = document.querySelectorAll('[data-testid^="message-"]');
    for (const row of rows) {
      if (!(row.textContent || "").includes(tok)) { continue; }
      const badge = row.querySelector("[data-receipt-state]");
      return badge ? badge.getAttribute("data-receipt-state") : null;
    }
    return null;
  }, token);
}

// Wait for one of `wanted` on A's row; `forbidden` states fail fast (delivered must not mean read).
async function waitForReceiptState(browser, token, wanted, forbidden, timeoutMs) {
  const end = Date.now() + timeoutMs;
  let last = null;
  while (Date.now() < end) {
    const state = await receiptState(browser, token);
    if (state !== last) {
      console.log(`[receipts] A: receipt state for "${token}" is now ${state}`);
      last = state;
    }
    if (state && forbidden.includes(state)) {
      throw new Error(
        `receipt state "${state}" appeared while waiting for ${wanted.join("/")} — ` +
        `delivered and read must be distinct signals`,
      );
    }
    if (state && wanted.includes(state)) {
      return state;
    }
    // Re-open A's DM to re-fire its debounced ingest, which is what pulls B's
    // receipt envelope in.
    await openDm(browser);
    await h.sleep(5000);
  }
  throw new Error(
    `receipt state never reached ${wanted.join("/")} for "${token}" (last saw: ${last})`,
  );
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
  const emailA = `e2e_rcpt_a_${stamp}@pollis.test`;
  const emailB = `e2e_rcpt_b_${stamp}@pollis.test`;
  const token = `rcpt-${stamp}`;

  let code = 1;
  let A;
  let B;
  try {
    await h.waitViteReady();
    console.log(`[receipts] using external delivery service at ${deliveryUrl}`);

    A = await h.startClient({
      index: 0, label: "A",
      appEnv: appEnvFor(devEnv, turso, deliveryUrl, path.join(__dirname, ".tmp-data-rcpt-a")),
    });
    clients.push(A);
    B = await h.startClient({
      index: 1, label: "B",
      appEnv: appEnvFor(devEnv, turso, deliveryUrl, path.join(__dirname, ".tmp-data-rcpt-b")),
    });
    clients.push(B);

    await signUp(A.browser, emailA, "A");
    await signUp(B.browser, emailB, "B");
    await setSkin(A.browser, "refined", "A");

    console.log(`[receipts] A: starting DM to ${emailB}`);
    await startDmTo(A.browser, emailB);
    console.log("[receipts] B: accepting the DM request…");
    await acceptIncomingDm(B.browser, REQUEST_TIMEOUT_MS);

    // B goes elsewhere BEFORE the message is sent, so nothing B does can be
    // mistaken for reading it.
    console.log("[receipts] B: leaving the DM so the message is never on screen");
    await leaveDm(B.browser);

    console.log(`[receipts] A: sending the message (${token})`);
    await openDm(A.browser);
    await sendMessage(A.browser, `receipt probe ${token}`);
    await waitForMessage(A.browser, token, 30000);

    // Step 3 — DELIVERED, and explicitly NOT read. B's device fetched and
    // decrypted the envelope in the background; no human saw it.
    console.log("[receipts] A: waiting for the DELIVERED tick (B must not have read it)");
    await waitForReceiptState(
      A.browser,
      token,
      ["delivered"],
      ["read-all"],
      RECEIPT_TIMEOUT_MS,
    );
    console.log("[receipts] delivered receipt converged (B decrypted, B did not read)");
    await shot(A.browser, "receipts-A-delivered.png");

    // Step 4 — B actually looks at the conversation.
    console.log("[receipts] B: opening the DM and dwelling on the message");
    await openDm(B.browser);
    await waitForMessage(B.browser, token, MESSAGE_TIMEOUT_MS);
    // Comfortably past the hook's DWELL_MS so the read receipt is emitted.
    await h.sleep(5000);

    // Step 5 — READ.
    console.log("[receipts] A: waiting for the READ tick");
    await waitForReceiptState(A.browser, token, ["read-all"], [], RECEIPT_TIMEOUT_MS);
    console.log(`[receipts] SUCCESS: delivered -> read converged for "${token}"`);
    await shot(A.browser, "receipts-A-read.png");
    code = 0;
  } catch (err) {
    console.error("[receipts] FAILED:", err.message);
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
  console.error(`[receipts] ${tag} on-screen testids:`, [...new Set(ids)].join(", "));
}

main().catch((err) => {
  console.error("[receipts] fatal:", err);
  process.exit(1);
});
