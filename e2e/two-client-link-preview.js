#!/usr/bin/env node
// Link previews (#1987) through two real app instances: A sends a link, its own
// core builds the preview (a real fetch from this machine), B's app draws the
// card from the message alone. A edits to another link: both windows show the
// new card. A deletes for everyone: the card goes in both windows. Then the
// reverse: B sends a preview, A sees it, B deletes it, A sees it go.
// Pages with no image (example.com/.org/.net): the CI backend has no object
// storage, so a thumbnail could not upload. Thumbnails, their release on edit
// and on delete run against the dev DS in mobile/scripts/maestro-link-previews.sh.

const fs = require("fs");
const path = require("path");
const h = require("./lib/harness");

const ARTIFACTS = path.join(__dirname, "artifacts");
const shot = h.makeShot(ARTIFACTS);

const PIN = "1357";
const REQUEST_TIMEOUT_MS = 120_000;
const MESSAGE_TIMEOUT_MS = 180_000;
const DELETED_MARKER = "Message deleted";

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
  console.log(`[link-preview] ${tag}: signing up ${email}`);
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
  await h.waitAppReadyAfterSignUp(browser, 60000);
  console.log(`[link-preview] ${tag}: reached app-ready`);
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
    console.log(`[link-preview] B: no DM request yet (attempt ${attempt}), waiting…`);
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
  await h.setComposerText(browser, text);
  await h.clickTestId(browser, "message-send-button");
}

// Click `testId` inside the row whose text holds `token` (each row carries its own action bar).
async function clickInRow(browser, token, testId) {
  const ok = await browser.execute((tok, id) => {
    for (const n of document.querySelectorAll('[data-testid="message-content"]')) {
      if (!(n.textContent || "").includes(tok)) { continue; }
      const row = n.closest('[data-testid^="message-"][tabindex="-1"]');
      const el = row && row.querySelector(`[data-testid="${id}"]`);
      if (el) { el.click(); return true; }
    }
    return false;
  }, token, testId);
  if (!ok) {
    throw new Error(`no ${testId} in the row holding "${token}"`);
  }
}

// Delete via the row's "more" menu; the confirm bar replaces the composer.
async function deleteMessage(browser, token) {
  await clickInRow(browser, token, "message-actions-more");
  await h.clickTestId(browser, "delete-button");
  await h.waitTestId(browser, "delete-message-bar", 15000);
  await h.clickTestId(browser, "delete-message-confirm");
  // The confirm bar closes and the composer returns once the delete resolves.
  await h.waitTestId(browser, "message-form", 30000);
}

// The card in the row holding `token`: { host, title } or null.
async function cardIn(browser, token) {
  return browser.execute((tok) => {
    for (const n of document.querySelectorAll('[data-testid="message-content"]')) {
      if (!(n.textContent || "").includes(tok)) { continue; }
      // The row is MessageItem's root: `message-<id>`, focusable for arrow-key navigation.
      const row = n.closest('[data-testid^="message-"][tabindex="-1"]');
      const card = row && row.querySelector('[data-testid="message-link-preview"]');
      if (card) { return { text: card.textContent || "", href: card.getAttribute("href") || "" }; }
    }
    return null;
  }, token);
}

// Poll until the row holding `token` shows a card whose text has `title` and whose link is `href`.
async function waitForCard(browser, token, title, href, timeoutMs) {
  const end = Date.now() + timeoutMs;
  let last = null;
  while (Date.now() < end) {
    last = await cardIn(browser, token);
    if (last && last.text.includes(title) && (!href || last.href === href)) {
      return last;
    }
    await openDm(browser);
    await h.sleep(5000);
  }
  throw new Error(`no card "${title}" ${href} on "${token}" (last: ${JSON.stringify(last)})`);
}

async function anyCard(browser) {
  return h.presentSelector(browser, '[data-testid="message-link-preview"]');
}

async function deletedMarkerVisible(browser) {
  return browser.execute((marker) => {
    const nodes = document.querySelectorAll('[data-testid="message-content"]');
    for (const n of nodes) {
      if ((n.textContent || "").includes(marker)) { return true; }
    }
    return false;
  }, DELETED_MARKER);
}

// Poll until the row holding `token` is a tombstone and no card is left anywhere.
async function waitForDeleted(browser, token, timeoutMs) {
  const end = Date.now() + timeoutMs;
  while (Date.now() < end) {
    const gone = !(await messageVisible(browser, token)) && (await deletedMarkerVisible(browser));
    if (gone && !(await anyCard(browser))) {
      return;
    }
    await openDm(browser);
    await h.sleep(5000);
  }
  throw new Error(`"${token}" never became a tombstone without a card`);
}

// Type a message with a link, wait for the composer's card to be built, send.
async function sendWithPreview(browser, text, title) {
  await h.setComposerText(browser, text);
  await waitComposerCard(browser, title);
  await h.clickTestId(browser, "message-send-button");
}

async function waitComposerCard(browser, title) {
  const end = Date.now() + 30000;
  while (Date.now() < end) {
    const text = await browser.execute(() => {
      const el = document.querySelector('[data-testid="composer-link-preview"]');
      return el ? el.textContent || "" : null;
    });
    if (text && text.includes(title)) {
      return;
    }
    await h.sleep(400);
  }
  throw new Error(`the composer never showed a card "${title}"`);
}

async function editTo(browser, token, text, title) {
  await clickInRow(browser, token, "edit-button");
  await h.waitTestId(browser, "edit-message-bar", 15000);
  // WebKitWebDriver's typing does not reach React's onChange on this textarea:
  // set the value through the native setter and fire the input event React listens to.
  await browser.execute((t) => {
    const el = document.querySelector('[data-testid="edit-message-bar-input"]');
    const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value").set;
    setter.call(el, t);
    el.dispatchEvent(new Event("input", { bubbles: true }));
    el.focus();
  }, text);
  await waitComposerCard(browser, title);
  await browser.keys("Enter");
  await h.waitTestId(browser, "message-form", 30000);
}

const SITE_A = "https://example.com/";
const SITE_B = "https://example.org/";
const SITE_C = "https://example.net/";
const TITLE = "Example Domain";

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
  const emailA = `e2e_lp_a_${stamp}@pollis.test`;
  const emailB = `e2e_lp_b_${stamp}@pollis.test`;
  const sent = `link-${stamp}`;
  const edited = `moved-${stamp}`;
  const back = `back-${stamp}`;

  let code = 1;
  let A;
  let B;
  try {
    await h.waitViteReady();
    console.log(`[link-preview] using external delivery service at ${deliveryUrl}`);

    A = await h.startClient({
      index: 0, label: "A",
      appEnv: appEnvFor(devEnv, turso, deliveryUrl, path.join(__dirname, ".tmp-data-lp-a")),
    });
    clients.push(A);
    B = await h.startClient({
      index: 1, label: "B",
      appEnv: appEnvFor(devEnv, turso, deliveryUrl, path.join(__dirname, ".tmp-data-lp-b")),
    });
    clients.push(B);

    await signUp(A.browser, emailA, "A");
    await signUp(B.browser, emailB, "B");

    console.log(`[link-preview] A: starting DM to ${emailB}`);
    await startDmTo(A.browser, emailB);
    console.log("[link-preview] B: accepting the DM request…");
    await acceptIncomingDm(B.browser, REQUEST_TIMEOUT_MS);
    await openDm(A.browser);

    // 1. A sends a link; A's core builds the preview; B draws the card.
    console.log("[link-preview] A: sending a link with its preview");
    await sendWithPreview(A.browser, `${sent} ${SITE_A}`, TITLE);
    await waitForCard(A.browser, sent, TITLE, SITE_A, 30000);
    await shot(A.browser, "lp-A-sent.png");
    await waitForCard(B.browser, sent, TITLE, SITE_A, MESSAGE_TIMEOUT_MS);
    console.log("[link-preview] B shows A's card");
    await shot(B.browser, "lp-B-received.png");

    // 2. A edits to another link: both windows show the new card.
    console.log("[link-preview] A: editing to another link");
    await editTo(A.browser, sent, `${edited} ${SITE_B}`, TITLE);
    await waitForCard(A.browser, edited, TITLE, SITE_B, 30000);
    await waitForCard(B.browser, edited, TITLE, SITE_B, MESSAGE_TIMEOUT_MS);
    if (await messageVisible(B.browser, sent)) {
      throw new Error("B still shows the pre-edit text");
    }
    console.log("[link-preview] B shows the edited card");
    await shot(B.browser, "lp-B-edited.png");

    // 3. A deletes for everyone: the card goes in both windows.
    console.log("[link-preview] A: deleting for everyone");
    await openDm(A.browser);
    await deleteMessage(A.browser, edited);
    await waitForDeleted(A.browser, edited, 30000);
    await waitForDeleted(B.browser, edited, MESSAGE_TIMEOUT_MS);
    console.log("[link-preview] the card is gone on both sides");
    await shot(B.browser, "lp-B-deleted.png");

    // 4. The reverse: B sends a preview, A sees it; B deletes it, A sees it go.
    console.log("[link-preview] B: sending a link with its preview");
    await sendWithPreview(B.browser, `${back} ${SITE_C}`, TITLE);
    await waitForCard(B.browser, back, TITLE, SITE_C, 30000);
    await waitForCard(A.browser, back, TITLE, SITE_C, MESSAGE_TIMEOUT_MS);
    await shot(A.browser, "lp-A-received.png");
    await deleteMessage(B.browser, back);
    await waitForDeleted(A.browser, back, MESSAGE_TIMEOUT_MS);
    console.log("[link-preview] SUCCESS: send, edit and delete converge with their cards in both windows");
    await shot(A.browser, "lp-A-deleted.png");
    code = 0;
  } catch (err) {
    console.error("[link-preview] FAILED:", err.message);
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
  await shot(browser, `lp-${tag}-FAIL.png`).catch(() => {});
  const src = await browser.getPageSource().catch(() => "");
  fs.mkdirSync(ARTIFACTS, { recursive: true });
  fs.writeFileSync(path.join(ARTIFACTS, `lp-${tag}-FAIL.html`), src);
  const ids = [...src.matchAll(/data-testid="([^"]+)"/g)].map((m) => m[1]);
  console.error(`[link-preview] ${tag} on-screen testids:`, [...new Set(ids)].join(", "));
}

main().catch((e) => { console.error("[link-preview] fatal:", e); h.reap(); process.exit(1); });
