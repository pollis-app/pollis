#!/usr/bin/env node
/*
 * Shareable invite links — create → copy → redeem, in BOTH skins (issue #847).
 *
 * The unit and DS tests prove the security properties (a revoked/expired/
 * exhausted token is refused, the stored value is a hash, redemption goes
 * through the normal `add_member_rows` admission path). They cannot prove the
 * thing only a real browser can: that an admin can actually mint a link, that
 * the token is really on the clipboard, and that pasting it into another
 * account's client puts that account in the group — in `terminal` AND `refined`,
 * which render the invite surfaces through separate `useSkin()` branches.
 *
 * Isolation + backend assumptions are identical to two-client-channel.js: two
 * isolated app instances (distinct driver ports + POLLIS_DATA_DIR), ONE shared
 * external backend (start-backend.sh — POLLIS_DELIVERY_URL must be set), ONE
 * Vite server.
 *
 * Choreography (run once per skin):
 *   1. A + B sign up (reused verbatim from two-client-channel.js).
 *   2. Both switch to the skin under test via Preferences (pref-skin-<skin>).
 *   3. A creates a group.
 *   4. A opens the group's invite page and creates an invite link
 *      (create-invite-link), then reads the URL out of created-invite-link-url
 *      and clicks copy-invite-link.
 *   5. A asserts the clipboard actually holds that URL — the copy button is the
 *      entire delivery mechanism for a token that can never be shown again, so
 *      "it rendered" is not good enough.
 *   6. B goes to /join (menu-item-join), pastes the URL, and redeems.
 *   7. B lands on the group page and the group is visible in B's group list.
 *   8. A revokes the link, and B2 — a second attempt with the same URL — fails
 *      with the single opaque error. This is the one behaviour a user can
 *      observe that the DS tests assert at the unit level; proving it end to end
 *      is what stops a UI regression from rendering a revoked link as usable.
 *
 * On failure, dumps per-client A-FAIL.* / B-FAIL.* into e2e/artifacts/.
 *
 * NOTE: not executed in the authoring environment — the harness's reap() does a
 * machine-wide `pkill -9 -f "bin/vite"`, which would kill sibling agents' dev
 * servers. Run it on an idle machine with `node e2e/invite-links.js`.
 */

const fs = require("fs");
const path = require("path");
const h = require("./lib/harness");

const ARTIFACTS = path.join(__dirname, "artifacts");
const shot = h.makeShot(ARTIFACTS);

const PIN = "2468";

const LINK_TIMEOUT_MS = 60_000;
const JOIN_TIMEOUT_MS = 120_000;
const GROUP_VISIBLE_TIMEOUT_MS = 90_000;

const SKINS = ["terminal", "refined"];

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

// Full signup through the real UI — copied verbatim from two-client-channel.js.
async function signUp(browser, email, tag) {
  console.log(`[invite-links] ${tag}: signing up ${email}`);
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
  console.log(`[invite-links] ${tag}: reached app-ready`);
}

// Click the first [data-testid^=prefix] whose visible text contains `text`.
async function clickByPrefixText(browser, prefix, text) {
  const ok = await browser.execute((pfx, needle) => {
    for (const el of document.querySelectorAll(`[data-testid^="${pfx}"]`)) {
      if ((el.textContent || "").includes(needle)) { el.click(); return true; }
    }
    return false;
  }, prefix, text);
  if (!ok) {
    throw new Error(`no [data-testid^="${prefix}"] containing ${JSON.stringify(text)}`);
  }
}

async function goHome(browser) {
  await browser.execute(() => { window.history.pushState({}, "", "/"); window.dispatchEvent(new PopStateEvent("popstate")); });
  await h.sleep(500);
}

/**
 * Switch the skin through the real Preferences UI rather than by writing the
 * preference directly — the point of running twice is to exercise the code path
 * a user actually takes to get there.
 */
async function setSkin(browser, skin, tag) {
  console.log(`[invite-links] ${tag}: switching to ${skin} skin`);
  await goHome(browser);
  await browser.execute(() => { window.history.pushState({}, "", "/preferences"); window.dispatchEvent(new PopStateEvent("popstate")); });
  await h.waitTestId(browser, `pref-skin-${skin}`, 30000);
  await h.clickTestId(browser, `pref-skin-${skin}`);
  await h.sleep(1000);
  await goHome(browser);
}

async function createGroup(browser, name, tag) {
  console.log(`[invite-links] ${tag}: creating group ${name}`);
  await goHome(browser);
  await h.clickTestId(browser, "menu-item-groups");
  await h.waitTestId(browser, "menu-item-create-group", 30000);
  await h.clickTestId(browser, "menu-item-create-group");
  await h.waitTestId(browser, "group-name-input", 30000);
  await h.setTestIdValue(browser, "group-name-input", name);
  await h.clickTestId(browser, "create-group-submit");
  await h.sleep(3000);
}

/**
 * Mint a link on A and return its URL, asserting the clipboard really received
 * it. The token is unrecoverable after this screen, so a copy button that
 * silently no-ops would lose it for good.
 */
async function createAndCopyInviteLink(browser, groupName, tag) {
  console.log(`[invite-links] ${tag}: creating invite link`);
  await goHome(browser);
  await h.clickTestId(browser, "menu-item-groups");
  await h.sleep(1500);
  await clickByPrefixText(browser, "group-option-", groupName);
  await h.sleep(1500);
  await h.waitTestId(browser, "menu-item-invite-member", 30000);
  await h.clickTestId(browser, "menu-item-invite-member");

  await h.waitTestId(browser, "invite-link-manager", LINK_TIMEOUT_MS);
  // 10 uses / 7 days is the default selection; take it as-is so the test
  // exercises the path a user hits without touching anything.
  await h.clickTestId(browser, "create-invite-link");

  await h.waitTestId(browser, "created-invite-link-url", LINK_TIMEOUT_MS);
  const url = (await (await browser.$('[data-testid="created-invite-link-url"]')).getText()).trim();
  if (!url || !url.includes("/invite/")) {
    throw new Error(`${tag}: invite URL looked wrong: ${JSON.stringify(url)}`);
  }

  await h.clickTestId(browser, "copy-invite-link");
  await h.sleep(800);

  // The clipboard is the delivery mechanism — assert it, don't assume it.
  const clip = await browser.execute(async () => {
    try {
      return await navigator.clipboard.readText();
    } catch (e) {
      return `__CLIPBOARD_ERR__:${e}`;
    }
  });
  if (clip !== url) {
    throw new Error(
      `${tag}: clipboard did not receive the invite URL.\n  expected: ${url}\n  got: ${clip}`
    );
  }
  console.log(`[invite-links] ${tag}: link copied — ${url}`);
  return url;
}

async function redeemInviteLink(browser, url, tag) {
  console.log(`[invite-links] ${tag}: redeeming invite link`);
  await goHome(browser);
  await h.waitTestId(browser, "menu-item-join", 30000);
  await h.clickTestId(browser, "menu-item-join");
  await h.waitTestId(browser, "invite-token-input", 30000);
  await h.setTestIdValue(browser, "invite-token-input", url);
  await h.clickTestId(browser, "redeem-invite-link");
}

async function revokeInviteLink(browser, groupName, tag) {
  console.log(`[invite-links] ${tag}: revoking invite link`);
  await goHome(browser);
  await h.clickTestId(browser, "menu-item-groups");
  await h.sleep(1500);
  await clickByPrefixText(browser, "group-option-", groupName);
  await h.sleep(1500);
  await h.clickTestId(browser, "menu-item-invite-member");
  await h.waitTestId(browser, "revoke-invite-link", LINK_TIMEOUT_MS);
  await h.clickTestId(browser, "revoke-invite-link");
  await h.sleep(2000);
}

async function runSkin(skin, A, B, suffix) {
  const groupName = `Invite ${skin} ${suffix}`;
  console.log(`\n[invite-links] ===== skin: ${skin} =====`);

  await setSkin(A.browser, skin, "A");
  await setSkin(B.browser, skin, "B");

  await createGroup(A.browser, groupName, "A");
  const url = await createAndCopyInviteLink(A.browser, groupName, "A");
  await shot(A.browser, `${skin}-A-link-created`);

  await redeemInviteLink(B.browser, url, "B");

  // B should land in the group; poll its group list until the group shows up.
  const deadline = Date.now() + JOIN_TIMEOUT_MS;
  let joined = false;
  while (Date.now() < deadline) {
    await goHome(B.browser);
    await h.clickTestId(B.browser, "menu-item-groups");
    await h.sleep(2000);
    const found = await B.browser.execute((needle) => {
      for (const el of document.querySelectorAll('[data-testid^="group-option-"]')) {
        if ((el.textContent || "").includes(needle)) { return true; }
      }
      return false;
    }, groupName);
    if (found) { joined = true; break; }
  }
  if (!joined) {
    throw new Error(`[${skin}] B never saw ${groupName} after redeeming the invite link`);
  }
  await shot(B.browser, `${skin}-B-joined`);
  console.log(`[invite-links] [${skin}] B joined via invite link`);

  // Revoked links must stop working, and must fail with the ONE opaque message.
  await revokeInviteLink(A.browser, groupName, "A");
  await redeemInviteLink(B.browser, url, "B");
  await h.waitTestId(B.browser, "invite-redeem-error", LINK_TIMEOUT_MS);
  const err = (await (await B.browser.$('[data-testid="invite-redeem-error"]')).getText()).trim();
  if (!err) {
    throw new Error(`[${skin}] revoked link produced an empty error`);
  }
  // The message must not disclose WHY. Anything naming the real cause means the
  // client has re-introduced the distinction the DS deliberately hides.
  for (const leak of ["revoked", "expired", "used up", "exhausted"]) {
    if (err.toLowerCase().includes(leak)) {
      throw new Error(`[${skin}] redeem error leaked the failure cause (${leak}): ${err}`);
    }
  }
  await shot(B.browser, `${skin}-B-revoked-rejected`);
  console.log(`[invite-links] [${skin}] revoked link rejected opaquely: ${err}`);
}

(async () => {
  h.reap();
  const devEnv = h.readEnvFile(".env.development");
  const turso = h.tursoEnv();
  const deliveryUrl = process.env.POLLIS_DELIVERY_URL;
  if (!deliveryUrl) {
    throw new Error("POLLIS_DELIVERY_URL must be set (run scripts/start-backend.sh first)");
  }

  h.warmVite();
  const vite = h.spawnVite(devEnv);
  await h.waitViteReady();

  const suffix = Date.now().toString(36);
  let A, B;
  try {
    A = await h.startClient({
      index: 0,
      appEnv: appEnvFor(devEnv, turso, deliveryUrl, `/tmp/pollis-e2e-invite-a-${suffix}`),
      label: "A",
    });
    B = await h.startClient({
      index: 1,
      appEnv: appEnvFor(devEnv, turso, deliveryUrl, `/tmp/pollis-e2e-invite-b-${suffix}`),
      label: "B",
    });

    await signUp(A.browser, `invite-a-${suffix}@pollis.test`, "A");
    await signUp(B.browser, `invite-b-${suffix}@pollis.test`, "B");

    for (const skin of SKINS) {
      await runSkin(skin, A, B, suffix);
    }

    console.log("\n[invite-links] PASS — create → copy → redeem → revoke in both skins");
  } catch (err) {
    console.error("[invite-links] FAIL:", err);
    if (A) { await h.dumpFailure(A.browser, ARTIFACTS, shot).catch(() => {}); }
    if (B) { await h.dumpFailure(B.browser, ARTIFACTS, shot).catch(() => {}); }
    process.exitCode = 1;
  } finally {
    if (A) { await A.stop().catch(() => {}); }
    if (B) { await B.stop().catch(() => {}); }
    if (vite) { vite.kill("SIGTERM"); }
    h.reap();
  }
})();
