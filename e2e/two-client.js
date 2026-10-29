#!/usr/bin/env node
// Two-client DM convergence (#570, M2): A's message appears in B's UI; two isolated apps, one backend.

const fs = require("fs");
const path = require("path");
const h = require("./lib/harness");

const ARTIFACTS = path.join(__dirname, "artifacts");
const shot = h.makeShot(ARTIFACTS);

const PIN = "1357";

// Cross-client delivery is eventual, so waits are generous.
const REQUEST_TIMEOUT_MS = 120_000;
const MESSAGE_TIMEOUT_MS = 180_000;

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

// Full signup through the real UI; leaves the client on app-ready.
async function signUp(browser, email, tag) {
  console.log(`[two-client] ${tag}: signing up ${email}`);

  // 1. Auth screen.
  await h.waitTestId(browser, "auth-screen", 30000);

  // 2. Email → request OTP.
  await h.setTestIdValue(browser, "email-input", email);
  await h.clickTestId(browser, "send-otp-button");
  await h.waitTestId(browser, "otp-form-container", 20000);

  // 3. Dev OTP 000000 (auto-submits) → new-account secret-key flow.
  await h.typeCode(browser, "000000");
  await h.waitTestId(browser, "save-secret-key-warning-screen", 45000);

  // 4. Save-secret-key dance.
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

  // 5. Create PIN (enter + confirm, each auto-advances/submits).
  await h.waitTestId(browser, "pin-create-screen");
  await h.typeCode(browser, PIN);
  await h.typeCode(browser, PIN);

  // 6. Ready app.
  await h.waitTestId(browser, "app-ready", 60000);
  console.log(`[two-client] ${tag}: reached app-ready`);
}

// A: DM the target by email; target the real input by id (its testid is a hidden mirror).
async function startDmTo(browser, targetEmail) {
  await h.clickTestId(browser, "sidebar-row-dms");
  await h.clickTestId(browser, "menu-item-new-dm");
  await h.waitTestId(browser, "start-dm-page", 20000);
  await h.setSelectorValue(browser, "#dm-identifier", targetEmail);
  await h.clickTestId(browser, "start-dm-submit-button");
  // The DM page mounts its composer (message-form) once the conversation is
  // selected; that's our "arrived" marker.
  await h.waitTestId(browser, "message-form", 30000);
}

// B: poll DMs for the request and accept it, remounting DMsPage to beat the 30s staleTime.
async function acceptIncomingDm(browser, timeoutMs) {
  const end = Date.now() + timeoutMs;
  let attempt = 0;
  while (Date.now() < end) {
    attempt++;
    // Bounce Home → DMs so DMsPage remounts and refetches the requests list.
    await h.clickTestId(browser, "sidebar-row-account").catch(() => {});
    await h.sleep(500);
    await h.clickTestId(browser, "sidebar-row-dms");
    await h.sleep(1500);
    if (await h.presentSelector(browser, '[data-testid="menu-item-dm-requests"]')) {
      await h.clickTestId(browser, "menu-item-dm-requests");
      await h.waitTestId(browser, "requests-page", 15000);
      await h.waitSelector(browser, '[data-testid^="accept-request-"]', 15000, "a DM request accept button");
      await h.clickSelector(browser, '[data-testid^="accept-request-"]');
      // Accepting navigates to the DM page → composer mounts.
      await h.waitTestId(browser, "message-form", 30000);
      return;
    }
    // Later attempts wait out the staleTime so the next remount really refetches.
    const remaining = end - Date.now();
    console.log(`[two-client] B: no DM request yet (attempt ${attempt}), waiting…`);
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

// B: poll the DM, re-opening it each round to re-fire the 5s-debounced ingest.
async function waitForMessage(browser, token, timeoutMs) {
  const end = Date.now() + timeoutMs;
  while (Date.now() < end) {
    if (await messageVisible(browser, token)) {
      return;
    }
    await h.clickTestId(browser, "sidebar-row-dms").catch(() => {});
    await h.sleep(1000);
    if (await h.presentSelector(browser, '[data-testid^="dm-option-"]')) {
      await h.clickSelector(browser, '[data-testid^="dm-option-"]');
      await h.waitTestId(browser, "message-form", 15000).catch(() => {});
    }
    // > INGEST_DEBOUNCE_MS (5s) so the next remount actually re-ingests.
    await h.sleep(6000);
  }
  throw new Error(`B: message "${token}" never converged`);
}

async function main() {
  h.reap();
  // .env.development is optional (absent in CI).
  const devEnv = h.readEnvFile(".env.development");
  const turso = h.tursoEnv();

  // This test assumes an EXTERNAL delivery service (start-backend.sh) — it does
  // NOT self-spawn one, since two clients must share exactly one backend.
  const deliveryUrl = process.env.POLLIS_DELIVERY_URL;
  if (!deliveryUrl) {
    throw new Error(
      "POLLIS_DELIVERY_URL is not set — run e2e/scripts/start-backend.sh first " +
        "(both clients must share one backend). See e2e/README.md."
    );
  }

  const children = [];
  const clients = [];
  const stop = (c) => { try { c && c.kill("SIGKILL"); } catch (_) {} };

  const vite = h.spawnVite(devEnv);
  children.push(vite);

  const stamp = Date.now();
  const emailA = `e2e_a_${stamp}@pollis.test`;
  const emailB = `e2e_b_${stamp}@pollis.test`;
  const token = `converge-${stamp}-${Math.floor((stamp % 100000))}`;

  let code = 1;
  let A;
  let B;
  try {
    await h.waitViteReady();
    console.log(`[two-client] using external delivery service at ${deliveryUrl}`);

    A = await h.startClient({
      index: 0, label: "A",
      appEnv: appEnvFor(devEnv, turso, deliveryUrl, path.join(__dirname, ".tmp-data-a")),
    });
    clients.push(A);
    B = await h.startClient({
      index: 1, label: "B",
      appEnv: appEnvFor(devEnv, turso, deliveryUrl, path.join(__dirname, ".tmp-data-b")),
    });
    clients.push(B);

    // Both sign up (sequential — signup is chatty; both apps stay running).
    await signUp(A.browser, emailA, "A");
    await shot(A.browser, "two-client-A-ready.png");
    await signUp(B.browser, emailB, "B");
    await shot(B.browser, "two-client-B-ready.png");

    console.log(`[two-client] A: starting DM to ${emailB}`);
    await startDmTo(A.browser, emailB);

    console.log("[two-client] B: waiting for the DM request…");
    await acceptIncomingDm(B.browser, REQUEST_TIMEOUT_MS);
    console.log("[two-client] B: DM request accepted");

    console.log(`[two-client] A: sending message with token ${token}`);
    await h.setComposerText(A.browser, `hello from A ${token}`);
    await h.clickTestId(A.browser, "message-send-button");
    await waitForMessage(A.browser, token, 30000);
    console.log("[two-client] A: message shown locally");
    await shot(A.browser, "two-client-A-sent.png");

    console.log("[two-client] B: waiting for convergence…");
    await waitForMessage(B.browser, token, MESSAGE_TIMEOUT_MS);
    await shot(B.browser, "two-client-B-received.png");
    console.log(`[two-client] SUCCESS: B received A's message ("${token}")`);
    code = 0;
  } catch (err) {
    console.error("[two-client] FAILED:", err.message);
    // Per-client failure artifacts so CI can see BOTH sides.
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

// Mirror e2e.js's dumpFailure, but per-client-prefixed so A and B don't clobber.
async function dumpClient(browser, tag) {
  await shot(browser, `${tag}-FAIL.png`).catch(() => {});
  const src = await browser.getPageSource().catch(() => "");
  fs.mkdirSync(ARTIFACTS, { recursive: true });
  fs.writeFileSync(path.join(ARTIFACTS, `${tag}-FAIL.html`), src);
  const ids = [...src.matchAll(/data-testid="([^"]+)"/g)].map((m) => m[1]);
  console.error(`[two-client] ${tag} on-screen testids:`, [...new Set(ids)].join(", "));
}

main().catch((e) => { console.error("[two-client] fatal:", e); h.reap(); process.exit(1); });
