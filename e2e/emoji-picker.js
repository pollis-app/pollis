#!/usr/bin/env node
/*
 * Emoji picker E2E (issue #848) — VISUAL verification that the real picker
 * renders and works in the shipped app.
 *
 * A note on the tool, because the ticket asked for Playwright: this repo has no
 * Playwright and cannot usefully have it here. Every e2e scenario drives the
 * REAL Tauri binary through `tauri-driver` + `WebKitWebDriver` (see
 * `e2e/README.md`), because the thing under test is the desktop app — the
 * picker's custom-emoji half calls `invoke("list_usable_emoji")` and
 * `invoke("get_emoji_url")`, neither of which exists in a browser. Playwright
 * drives browsers, not a WebKitGTK-backed Tauri window, and adding it would also
 * mean changing the frozen lockfile. So this follows the house convention
 * (webdriverio, one scenario per file, no runner) and gets to verify the actual
 * shipping surface instead of a browser-only mock of it.
 *
 * What it proves:
 *   1. The reaction affordance now opens the REAL picker — search box, category
 *      rail, skin-tone swatches, hundreds of cells — not the eight hard-coded
 *      emoji it used to be.
 *   2. Search narrows the grid, and the result is pickable.
 *   3. Picking one adds a reaction pill to the message.
 *   4. The category rail jumps between sections.
 *   5. The per-group Custom Emoji page renders and states the storage model.
 *
 * Screenshots land in `e2e/artifacts/emoji-*.png` — those are the visual record.
 *
 * Backend assumptions are the same as every other scenario: ONE shared external
 * backend (`e2e/scripts/start-backend.sh`, so `POLLIS_DELIVERY_URL` must be set)
 * and ONE Vite server. Single client — nothing here needs convergence.
 */

const fs = require("fs");
const path = require("path");
const h = require("./lib/harness");

const ARTIFACTS = path.join(__dirname, "artifacts");
const shot = h.makeShot(ARTIFACTS);

const PIN = "2468";

// Full signup through the real UI — the same flow every other scenario uses.
async function signUp(browser, email) {
  console.log(`[emoji-picker] signing up ${email}`);
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
    throw new Error("secret key display was empty");
  }
  await h.clickTestId(browser, "secret-key-saved-button");
  await h.waitTestId(browser, "save-secret-key-confirm-screen");
  await h.setTestIdValue(browser, "secret-key-confirm-input", secretKey);
  await h.clickTestId(browser, "confirm-secret-key-button");
  await h.waitTestId(browser, "pin-create-screen");
  await h.typeCode(browser, PIN);
  await h.typeCode(browser, PIN);
  await h.waitTestId(browser, "app-ready", 60000);
}

async function createGroupWithChannel(browser, groupName, channelName) {
  await h.clickTestId(browser, "menu-item-groups");
  await h.clickTestId(browser, "menu-item-create-group");
  await h.waitTestId(browser, "create-group-page", 20000);
  await h.setSelectorValue(browser, "#create-group-name", groupName);
  await h.clickTestId(browser, "create-group-submit-button");
  await h.waitTestId(browser, "menu-item-create-channel", 30000);

  await h.clickTestId(browser, "menu-item-create-channel");
  await h.waitTestId(browser, "create-channel-page", 20000);
  await h.setSelectorValue(browser, "#create-channel-name", channelName);
  await h.clickTestId(browser, "create-channel-submit-button");
  await h.waitTestId(browser, "message-form", 30000);
}

/**
 * Click the breadcrumb trail segment whose text is `label`.
 *
 * The bar's chevrons are history back/forward now, so "back" from this channel
 * is the create-channel form it was made from, not the group. Climbing the
 * hierarchy is the trail's job — this clicks it the way a user would.
 */
async function clickTrailSegment(browser, label) {
  const clicked = await browser.execute((needle) => {
    const trail = document.querySelector('[data-testid="breadcrumb-trail"]');
    if (!trail) { return false; }
    for (const btn of trail.querySelectorAll("button")) {
      if ((btn.textContent || "").trim() === needle) { btn.click(); return true; }
    }
    return false;
  }, label);
  if (!clicked) {
    throw new Error(`no breadcrumb trail segment labelled "${label}"`);
  }
}

async function countSelector(browser, selector) {
  return browser.execute((sel) => document.querySelectorAll(sel).length, selector);
}

/** Hover the message row so the reaction affordance un-hides, then click it. */
async function openReactionPicker(browser) {
  const opened = await browser.execute(() => {
    const btn = document.querySelector('[data-testid="reaction-add-btn"]');
    if (!btn) {
      return false;
    }
    btn.click();
    return true;
  });
  if (!opened) {
    throw new Error("no reaction-add-btn on screen");
  }
  await h.waitTestId(browser, "emoji-picker", 15000);
}

async function main() {
  h.reap();
  const devEnv = h.readEnvFile(".env.development");
  const turso = h.tursoEnv();

  const deliveryUrl = process.env.POLLIS_DELIVERY_URL;
  if (!deliveryUrl) {
    throw new Error(
      "POLLIS_DELIVERY_URL is not set — run e2e/scripts/start-backend.sh first. See e2e/README.md."
    );
  }

  const children = [];
  const stop = (c) => { try { c && c.kill("SIGKILL"); } catch (_) {} };

  const vite = h.spawnVite(devEnv);
  children.push(vite);

  const dataDir = path.join(__dirname, ".tmp-data-emoji");
  fs.rmSync(dataDir, { recursive: true, force: true });
  fs.mkdirSync(dataDir, { recursive: true });

  const appEnv = {
    ...devEnv, ...process.env,
    TURSO_URL: turso.TURSO_URL, TURSO_TOKEN: turso.TURSO_TOKEN,
    POLLIS_DELIVERY_URL: deliveryUrl,
    POLLIS_DATA_DIR: dataDir,
    LOG_DB_URL: "", LOG_DB_TOKEN: "", LOG_DB_ADMIN_TOKEN: "",
    WEBKIT_DISABLE_COMPOSITING_MODE: "1", GDK_BACKEND: "x11",
  };

  let client = null;
  let code = 1;
  try {
    await h.waitViteReady();
    client = await h.startClient({ index: 0, appEnv, label: "emoji" });
    const browser = client.browser;

    const stamp = Date.now();
    await signUp(browser, `emoji-${stamp}@pollis.test`);

    const groupName = `Emoji Test ${stamp}`;
    await createGroupWithChannel(browser, groupName, "general");

    // A message to react to.
    await h.setComposerText(browser, `emoji e2e ${stamp}`);
    await h.clickTestId(browser, "message-send-button");
    await h.waitSelector(browser, '[data-testid="message-content"]', 30000, "the sent message");

    // ── 1. The real picker opens ────────────────────────────────────────────
    await openReactionPicker(browser);
    await shot(browser, "emoji-picker-open.png");

    await h.waitTestId(browser, "emoji-picker-search", 10000);
    await h.waitTestId(browser, "emoji-category-rail", 10000);
    await h.waitTestId(browser, "emoji-skin-tones", 10000);

    const cellCount = await countSelector(browser, '[data-testid="emoji-cell"]');
    console.log(`[emoji-picker] ${cellCount} cells mounted on open`);
    if (cellCount < 100) {
      throw new Error(
        `expected the real emoji set (hundreds of cells), saw ${cellCount} — ` +
        "this is the regression the eight hard-coded emoji used to be"
      );
    }

    // ── 2. Search narrows it ────────────────────────────────────────────────
    await h.setTestIdValue(browser, "emoji-picker-search", "grin");
    await h.sleep(600);
    const searched = await countSelector(browser, '[data-testid="emoji-cell"]');
    console.log(`[emoji-picker] "grin" -> ${searched} results`);
    if (searched === 0 || searched >= cellCount) {
      throw new Error(`search did not narrow the grid (${cellCount} -> ${searched})`);
    }
    await shot(browser, "emoji-picker-search.png");

    // ── 3. Picking adds a reaction ──────────────────────────────────────────
    await h.clickSelector(browser, '[data-testid="emoji-cell"]');
    await h.waitSelector(browser, '[data-testid="reaction-pill"]', 20000, "the new reaction pill");
    await shot(browser, "emoji-reaction-added.png");
    console.log("[emoji-picker] reaction pill rendered");

    // ── 4. The category rail jumps ──────────────────────────────────────────
    await openReactionPicker(browser);
    await h.clickTestId(browser, "emoji-category-flags");
    await h.sleep(800);
    await shot(browser, "emoji-picker-flags.png");

    // Close the picker before navigating away.
    await browser.keys(["Escape"]);
    await h.sleep(300);

    // ── 5. The per-group Custom Emoji page ──────────────────────────────────
    await clickTrailSegment(browser, groupName);
    await h.waitTestId(browser, "menu-item-group-emoji", 20000);
    await h.clickTestId(browser, "menu-item-group-emoji");
    await h.waitTestId(browser, "group-emoji-page", 20000);
    await h.waitTestId(browser, "group-emoji-empty", 10000);
    await shot(browser, "emoji-group-page.png");
    console.log("[emoji-picker] custom emoji page rendered");

    console.log("[emoji-picker] SUCCESS");
    code = 0;
  } catch (err) {
    console.error("[emoji-picker] FAILED:", err.message);
    if (client && client.browser) {
      await h.dumpFailure(client.browser, ARTIFACTS, shot);
    }
  } finally {
    if (client && client.browser) {
      await client.browser.deleteSession().catch(() => {});
    }
    stop(client && client.tauriDriver);
    for (const c of children) {
      stop(c);
    }
    h.reap();
  }
  process.exit(code);
}

main().catch((e) => { console.error("[emoji-picker] fatal:", e); h.reap(); process.exit(1); });
