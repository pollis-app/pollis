#!/usr/bin/env node
// Two-client group text channel convergence (#570): B accepts the invite and sees A's message.

const fs = require("fs");
const path = require("path");
const h = require("./lib/harness");

const ARTIFACTS = path.join(__dirname, "artifacts");
const shot = h.makeShot(ARTIFACTS);

const PIN = "1357";

const INVITE_TIMEOUT_MS = 120_000;
const GROUP_VISIBLE_TIMEOUT_MS = 90_000;
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

async function signUp(browser, email, tag) {
  console.log(`[two-client-channel] ${tag}: signing up ${email}`);
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
  console.log(`[two-client-channel] ${tag}: reached app-ready`);
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
    throw new Error(`clickByPrefixText: no ${prefix}* containing "${text}"`);
  }
}

async function prefixTextPresent(browser, prefix, text) {
  return browser.execute((pfx, needle) => {
    for (const el of document.querySelectorAll(`[data-testid^="${pfx}"]`)) {
      if ((el.textContent || "").includes(needle)) { return true; }
    }
    return false;
  }, prefix, text);
}

// Go Home via the breadcrumb (absent only when already home).
async function goHome(browser) {
  const clicked = await browser.execute(() => {
    const trail = document.querySelector('[data-testid="breadcrumb-trail"]');
    const btn = trail && trail.querySelector("button");
    if (btn) { btn.click(); return true; }
    return false;
  });
  if (clicked) {
    await h.waitTestId(browser, "menu-item-groups", 15000);
  }
}

// A: create a group; lands on the group page.
async function createGroup(browser, groupName) {
  await h.clickTestId(browser, "menu-item-groups");
  await h.clickTestId(browser, "menu-item-create-group");
  await h.waitTestId(browser, "create-group-page", 20000);
  await h.setSelectorValue(browser, "#create-group-name", groupName);
  await h.clickTestId(browser, "create-group-submit-button");
  await h.waitTestId(browser, "menu-item-create-channel", 30000);
}

// A: invite B by email from the group page, then return to the group page.
async function inviteMember(browser, email) {
  await h.clickTestId(browser, "menu-item-invite-member");
  await h.waitTestId(browser, "invite-member-page", 20000);
  await h.setSelectorValue(browser, "#invite-username", email);
  await h.clickTestId(browser, "send-invite-button");
  await h.waitTestId(browser, "invite-sent-confirmation", 20000);
  await h.clickTestId(browser, "breadcrumb-back-button");
  await h.waitTestId(browser, "menu-item-create-channel", 20000);
}

// A: create a text channel; lands in it.
async function createTextChannel(browser, channelName) {
  await h.clickTestId(browser, "menu-item-create-channel");
  await h.waitTestId(browser, "create-channel-page", 20000);
  await h.setSelectorValue(browser, "#create-channel-name", channelName);
  await h.clickTestId(browser, "create-channel-submit-button");
  await h.waitTestId(browser, "message-form", 30000);
}

// B: poll Invites and accept; accept_group_invite processes the MLS welcome synchronously.
async function acceptInvite(browser, timeoutMs) {
  const end = Date.now() + timeoutMs;
  let attempt = 0;
  while (Date.now() < end) {
    attempt++;
    await goHome(browser);
    await h.clickTestId(browser, "menu-item-invites");
    await h.waitTestId(browser, "invites-page", 15000);
    if (await h.presentSelector(browser, '[data-testid^="accept-invite-"]')) {
      await h.clickSelector(browser, '[data-testid^="accept-invite-"]');
      // The invite row disappears; MLS welcome is polled synchronously.
      await h.sleep(3000);
      return;
    }
    const remaining = end - Date.now();
    console.log(`[two-client-channel] B: no invite yet (attempt ${attempt}), waiting…`);
    await h.sleep(Math.min(remaining > 0 ? remaining : 0, attempt === 1 ? 4000 : 15000));
  }
  throw new Error("B: group invite never appeared");
}

// Open a group's channel by NAME, polling until B's group list shows the group.
async function openChannel(browser, groupName, channelName, groupTimeoutMs) {
  const end = Date.now() + groupTimeoutMs;
  while (Date.now() < end) {
    await goHome(browser);
    await h.clickTestId(browser, "menu-item-groups");
    await h.waitTestId(browser, "menu-item-create-group", 15000);
    if (await prefixTextPresent(browser, "group-option-", groupName)) {
      await clickByPrefixText(browser, "group-option-", groupName);
      await h.waitSelector(browser, '[data-testid^="channel-option-"]', 20000, "a channel row");
      await clickByPrefixText(browser, "channel-option-", channelName);
      await h.waitTestId(browser, "message-form", 20000);
      return;
    }
    console.log(`[two-client-channel] B: group "${groupName}" not visible yet, waiting…`);
    await h.sleep(6000);
  }
  throw new Error(`B: group "${groupName}" never appeared in the group list`);
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

// B: poll the channel, re-opening each round to re-fire the 5s-debounced ingest.
async function waitForChannelMessage(browser, groupName, channelName, token, timeoutMs) {
  const end = Date.now() + timeoutMs;
  while (Date.now() < end) {
    if (await messageVisible(browser, token)) {
      return;
    }
    await openChannel(browser, groupName, channelName, 30000).catch(() => {});
    await h.sleep(6000);
  }
  throw new Error(`B: channel message "${token}" never converged`);
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
  const clients = [];
  const stop = (c) => { try { c && c.kill("SIGKILL"); } catch (_) {} };

  const vite = h.spawnVite(devEnv);
  children.push(vite);

  const stamp = Date.now();
  const emailA = `e2e_ch_a_${stamp}@pollis.test`;
  const emailB = `e2e_ch_b_${stamp}@pollis.test`;
  const groupName = `grp${stamp}`;
  const channelName = `chan${stamp}`;
  const token = `chanconv-${stamp}`;

  let code = 1;
  let A;
  let B;
  try {
    await h.waitViteReady();
    console.log(`[two-client-channel] using external delivery service at ${deliveryUrl}`);

    A = await h.startClient({
      index: 0, label: "A",
      appEnv: appEnvFor(devEnv, turso, deliveryUrl, path.join(__dirname, ".tmp-data-ch-a")),
    });
    clients.push(A);
    B = await h.startClient({
      index: 1, label: "B",
      appEnv: appEnvFor(devEnv, turso, deliveryUrl, path.join(__dirname, ".tmp-data-ch-b")),
    });
    clients.push(B);

    await signUp(A.browser, emailA, "A");
    await shot(A.browser, "two-client-channel-A-ready.png");
    await signUp(B.browser, emailB, "B");
    await shot(B.browser, "two-client-channel-B-ready.png");

    console.log(`[two-client-channel] A: creating group "${groupName}"`);
    await createGroup(A.browser, groupName);
    console.log(`[two-client-channel] A: inviting ${emailB}`);
    await inviteMember(A.browser, emailB);
    console.log(`[two-client-channel] A: creating text channel "${channelName}"`);
    await createTextChannel(A.browser, channelName);
    await shot(A.browser, "two-client-channel-A-channel.png");

    console.log("[two-client-channel] B: waiting for the invite…");
    await acceptInvite(B.browser, INVITE_TIMEOUT_MS);
    console.log("[two-client-channel] B: invite accepted");

    console.log(`[two-client-channel] B: opening channel "${channelName}"`);
    await openChannel(B.browser, groupName, channelName, GROUP_VISIBLE_TIMEOUT_MS);
    await shot(B.browser, "two-client-channel-B-in-channel.png");

    console.log(`[two-client-channel] A: sending channel message (token ${token})`);
    await h.setComposerText(A.browser, `hello channel ${token}`);
    await h.clickTestId(A.browser, "message-send-button");
    await waitForChannelMessage(A.browser, groupName, channelName, token, 30000);
    await shot(A.browser, "two-client-channel-A-sent.png");

    console.log("[two-client-channel] B: waiting for convergence…");
    await waitForChannelMessage(B.browser, groupName, channelName, token, MESSAGE_TIMEOUT_MS);
    await shot(B.browser, "two-client-channel-B-received.png");
    console.log(`[two-client-channel] SUCCESS: B received A's channel message ("${token}")`);
    code = 0;
  } catch (err) {
    console.error("[two-client-channel] FAILED:", err.message);
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
  console.error(`[two-client-channel] ${tag} on-screen testids:`, [...new Set(ids)].join(", "));
}

main().catch((e) => { console.error("[two-client-channel] fatal:", e); h.reap(); process.exit(1); });
