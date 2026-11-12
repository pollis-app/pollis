// UI steps shared by the group voice-channel e2e scripts (two-client-voice-channel.js,
// voice-removal-frame-keys.js): sign-up, group + voice channel setup, invites, joining
// and the participant roster. Every helper takes a log tag as its last argument.

const fs = require("fs");
const path = require("path");
const h = require("./harness");

const PIN = "1357";

// The app's env for one client, with a fresh data dir.
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

async function signUp(browser, email, who, tag) {
  console.log(`[${tag}] ${who}: signing up ${email}`);
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
    throw new Error(`${who}: secret key display was empty`);
  }
  await h.clickTestId(browser, "secret-key-saved-button");
  await h.waitTestId(browser, "save-secret-key-confirm-screen");
  await h.setTestIdValue(browser, "secret-key-confirm-input", secretKey);
  await h.clickTestId(browser, "confirm-secret-key-button");
  await h.waitTestId(browser, "pin-create-screen");
  await h.typeCode(browser, PIN);
  await h.typeCode(browser, PIN);
  await h.waitTestId(browser, "app-ready", 60000);
  console.log(`[${tag}] ${who}: reached app-ready`);
}

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

async function createGroup(browser, groupName) {
  await h.clickTestId(browser, "menu-item-groups");
  await h.clickTestId(browser, "menu-item-create-group");
  await h.waitTestId(browser, "create-group-page", 20000);
  await h.setSelectorValue(browser, "#create-group-name", groupName);
  await h.clickTestId(browser, "create-group-submit-button");
  await h.waitTestId(browser, "menu-item-create-channel", 30000);
}

async function inviteMember(browser, email) {
  await h.clickTestId(browser, "menu-item-invite-member");
  await h.waitTestId(browser, "invite-member-page", 20000);
  await h.setSelectorValue(browser, "#invite-username", email);
  await h.clickTestId(browser, "send-invite-button");
  await h.waitTestId(browser, "invite-sent-confirmation", 20000);
  await h.clickTestId(browser, "breadcrumb-back-button");
  await h.waitTestId(browser, "menu-item-create-channel", 20000);
}

// Create a VOICE channel; it returns to the group page.
async function createVoiceChannel(browser, channelName) {
  await h.clickTestId(browser, "menu-item-create-channel");
  await h.waitTestId(browser, "create-channel-page", 20000);
  await h.setSelectorValue(browser, "#create-channel-name", channelName);
  await h.clickSelector(browser, "#create-channel-type");
  await h.clickTestId(browser, "create-channel-submit-button");
  await h.waitTestId(browser, "menu-item-create-channel", 30000);
}

async function acceptInvite(browser, timeoutMs, who, tag) {
  const end = Date.now() + timeoutMs;
  let attempt = 0;
  while (Date.now() < end) {
    attempt++;
    await goHome(browser);
    await h.clickTestId(browser, "menu-item-invites");
    await h.waitTestId(browser, "invites-page", 15000);
    if (await h.presentSelector(browser, '[data-testid^="accept-invite-"]')) {
      await h.clickSelector(browser, '[data-testid^="accept-invite-"]');
      await h.sleep(3000);
      return;
    }
    const remaining = end - Date.now();
    console.log(`[${tag}] ${who}: no invite yet (attempt ${attempt}), waiting…`);
    await h.sleep(Math.min(remaining > 0 ? remaining : 0, attempt === 1 ? 4000 : 15000));
  }
  throw new Error(`${who}: group invite never appeared`);
}

// Open the group by name, landing on its page; retries until it is visible.
async function openGroup(browser, groupName, groupTimeoutMs, tag) {
  const end = Date.now() + groupTimeoutMs;
  while (Date.now() < end) {
    await goHome(browser);
    await h.clickTestId(browser, "menu-item-groups");
    await h.waitTestId(browser, "menu-item-create-group", 15000);
    if (await prefixTextPresent(browser, "group-option-", groupName)) {
      await clickByPrefixText(browser, "group-option-", groupName);
      await h.waitSelector(browser, '[data-testid^="channel-option-"]', 20000, "a channel row");
      return;
    }
    console.log(`[${tag}] group "${groupName}" not visible yet, waiting…`);
    await h.sleep(6000);
  }
  throw new Error(`group "${groupName}" never appeared`);
}

// Open the voice channel by name, landing on the not-joined VoiceStage.
async function openVoiceChannel(browser, groupName, channelName, groupTimeoutMs, tag) {
  await openGroup(browser, groupName, groupTimeoutMs, tag);
  await clickByPrefixText(browser, "channel-option-", channelName);
  await h.waitTestId(browser, "voice-join-cta", 20000);
}

async function joinVoice(browser, joinTimeoutMs, who, tag) {
  await h.clickTestId(browser, "voice-join-cta");
  await h.waitTestId(browser, "voice-channel-view", joinTimeoutMs);
  console.log(`[${tag}] ${who}: joined the voice channel`);
}

// Distinct user IDs from the roster; .vs-tile keeps voice-tile-<sub>-* from matching.
async function participantUserIds(browser) {
  return browser.execute(() => {
    const ids = new Set();
    for (const el of document.querySelectorAll('.vs-tile[data-testid^="voice-tile-"]')) {
      let id = (el.getAttribute("data-testid") || "").slice("voice-tile-".length);
      if (id.startsWith("voice-")) { id = id.slice("voice-".length); }
      const c = id.indexOf(":");
      ids.add(c === -1 ? id : id.slice(0, c));
    }
    return Array.from(ids);
  });
}

async function waitForParticipantCount(browser, n, timeoutMs, who, tag) {
  const end = Date.now() + timeoutMs;
  let last = -1;
  while (Date.now() < end) {
    const ids = await participantUserIds(browser).catch(() => []);
    if (ids.length !== last) {
      console.log(`[${tag}] ${who}: sees ${ids.length} participant(s)`);
      last = ids.length;
    }
    if (ids.length === n) {
      return;
    }
    await h.sleep(2000);
  }
  throw new Error(`${who}: participant count never reached ${n} (last ${last})`);
}

async function dumpClient(browser, artifactsDir, shot, who, tag) {
  await shot(browser, `${who}-FAIL.png`).catch(() => {});
  const src = await browser.getPageSource().catch(() => "");
  fs.mkdirSync(artifactsDir, { recursive: true });
  fs.writeFileSync(path.join(artifactsDir, `${who}-FAIL.html`), src);
  const ids = [...src.matchAll(/data-testid="([^"]+)"/g)].map((m) => m[1]);
  console.error(`[${tag}] ${who} on-screen testids:`, [...new Set(ids)].join(", "));
}

module.exports = {
  appEnvFor,
  signUp,
  goHome,
  createGroup,
  inviteMember,
  createVoiceChannel,
  acceptInvite,
  openGroup,
  openVoiceChannel,
  joinVoice,
  waitForParticipantCount,
  dumpClient,
};
