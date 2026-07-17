#!/usr/bin/env node
/*
 * Two-client cross-client convergence E2E (issue #570, M2).
 *
 * Launches TWO isolated real Pollis desktop app instances against ONE shared
 * backend (the libsql + real pollis-delivery fixture from
 * e2e/scripts/start-backend.sh) and proves that a message sent by client A
 * appears in client B's UI — the first real MLS-through-the-real-renderer,
 * cross-client delivery test.
 *
 * Isolation: each client gets its own tauri-driver / WebKitWebDriver port pair
 * (A: 4444/4445, B: 4446/4447 — see harness.clientPorts) and its own
 * POLLIS_DATA_DIR (separate local SQLite + MLS state + keystore). ONE Vite dev
 * server serves both webviews.
 *
 * Conversation path — 1:1 DM request → accept (the fewest-step stable path):
 *   1. A + B both sign up through the real UI (reuses e2e.js's signup steps).
 *   2. A starts a DM to B by B's email (search_user_by_username matches email),
 *      landing on the DM page. This adds B to the DM's MLS group.
 *   3. B polls its DMs for the incoming request and accepts it, landing on the
 *      DM page (now a member of the MLS group).
 *   4. A sends a distinctive, random-token message.
 *   5. B polls its message list (re-opening the DM to re-fire the debounced
 *      envelope ingest — there's no LiveKit realtime hint in this fixture)
 *      until A's token text appears. Asserted, no fixed sleeps for correctness.
 *
 * Assumes the backend fixtures are already up (the workflow runs
 * start-backend.sh first, same as e2e-full). Both clients read
 * POLLIS_DELIVERY_URL / TURSO_URL / TURSO_TOKEN / R2_* from the env.
 *
 * On failure, dumps per-client screenshots (A-FAIL.png / B-FAIL.png) and page
 * source (A-FAIL.html / B-FAIL.html) into e2e/artifacts/ so CI is actionable.
 */

const fs = require("fs");
const path = require("path");
const h = require("./lib/harness");

const ARTIFACTS = path.join(__dirname, "artifacts");
const shot = h.makeShot(ARTIFACTS);

const PIN = "1357";

// How long to wait for eventual MLS/remote propagation between the two clients.
// Generous: cross-client delivery is eventual (remote metadata read for the DM
// request, MLS envelope fetch + decrypt for the message).
const REQUEST_TIMEOUT_MS = 120_000;
const MESSAGE_TIMEOUT_MS = 180_000;

// Build the app env for one client: shared backend creds from the process env
// (start-backend.sh) or .env.development, a per-client data dir, and the WebKit
// workaround vars every script needs on this setup.
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

// Full signup through the real UI — the exact step sequence e2e.js proves,
// factored so both clients run it. Leaves the client on [data-testid="app-ready"].
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

// A: open "New Message", DM the target by email, land on the DM page. The
// visible identifier field carries id="dm-identifier" (its dm-identifier-input
// testid is a hidden read-only mirror), so target the real input by id.
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

// B: poll the DMs list until the incoming request surfaces (a remote metadata
// read, not MLS-gated) and accept it. The accept button's testid embeds the
// channel id (unknown ahead of time), so match by prefix. Re-mounts DMsPage on
// each attempt to defeat React Query's 30s staleTime.
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

// Does any rendered message body contain the token?
async function messageVisible(browser, token) {
  return browser.execute((tok) => {
    const nodes = document.querySelectorAll('[data-testid="message-content"]');
    for (const n of nodes) {
      if ((n.textContent || "").includes(tok)) { return true; }
    }
    return false;
  }, token);
}

// B: poll the DM until A's token appears. Re-opens the conversation each round
// to re-fire the (5s-debounced) envelope ingest — the only pull path without a
// LiveKit realtime hint, which this fixture doesn't provide.
async function waitForMessage(browser, token, timeoutMs) {
  const end = Date.now() + timeoutMs;
  while (Date.now() < end) {
    if (await messageVisible(browser, token)) {
      return;
    }
    // Re-open the DM (Home → DMs → first conversation) to remount MainContent
    // and trigger a fresh ingest_dm_envelopes past the debounce window.
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
  // .env.development is optional (present locally for dev R2/LiveKit creds,
  // absent in CI where they come from the workflow env / start-backend.sh).
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

  // One shared Vite dev server for both webviews.
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

    // Bring up both isolated clients (distinct ports + data dirs).
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

    // A → DM B by email. Establishes the DM + adds B to its MLS group.
    console.log(`[two-client] A: starting DM to ${emailB}`);
    await startDmTo(A.browser, emailB);

    // B → accept the incoming request (becomes an MLS group member).
    console.log("[two-client] B: waiting for the DM request…");
    await acceptIncomingDm(B.browser, REQUEST_TIMEOUT_MS);
    console.log("[two-client] B: DM request accepted");

    // A → send the distinctive message and confirm it landed on A first.
    console.log(`[two-client] A: sending message with token ${token}`);
    await h.setTestIdValue(A.browser, "message-input", `hello from A ${token}`);
    await h.clickTestId(A.browser, "message-send-button");
    await waitForMessage(A.browser, token, 30000);
    console.log("[two-client] A: message shown locally");
    await shot(A.browser, "two-client-A-sent.png");

    // B → poll until A's message converges into B's UI.
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
