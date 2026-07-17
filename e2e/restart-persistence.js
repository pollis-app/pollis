#!/usr/bin/env node
/*
 * Single-client RESTART PERSISTENCE E2E (issue #570, single-user).
 *
 * Proves the "same device keeps its data" invariant: after a full app restart
 * against the SAME POLLIS_DATA_DIR, the account is still recognised — the app
 * does NOT fall back to a blank first-run signup. Whatever resume path the app
 * takes (auto-resume to app-ready, PIN unlock, or re-auth with the account
 * pre-known), the test drives it back to app-ready.
 *
 * Choreography:
 *   1. Sign up (fresh account) → app-ready. The data dir now holds the local
 *      SQLite DB + MLS state + keystore + accounts index.
 *   2. Tear the app down (delete the WebDriver session, kill the driver) but KEEP
 *      the data dir.
 *   3. Relaunch a new app instance on the SAME data dir.
 *   4. Assert persistence: the app resumes to one of app-ready / pin-entry-screen
 *      / auth-screen-with-a-known-account — never a blank signup — and is driven
 *      back to app-ready.
 *
 * Needs the backend (DS for OTP verify on the re-auth path). Run start-backend.sh
 * first (POLLIS_DELIVERY_URL must be set).
 */

const fs = require("fs");
const path = require("path");
const { spawnSync } = require("child_process");
const h = require("./lib/harness");

// Kill this client's driver/webdriver/app but SPARE the shared Vite server and
// the external DS — the relaunch loads its UI from that same Vite server, so a
// full h.reap() (which pkills bin/vite) would leave the relaunch on a dead port
// ("Connection refused"). Mirrors reap() minus vite/pollis-delivery.
function reapClientKeepVite() {
  for (const p of ["tauri-driver", "WebKitWebDriver"]) {
    spawnSync("pkill", ["-9", "-f", p], { stdio: "ignore" });
  }
  spawnSync("pkill", ["-9", "-x", "pollis"], { stdio: "ignore" });
}

const ARTIFACTS = path.join(__dirname, "artifacts");
const shot = h.makeShot(ARTIFACTS);
const PIN = "1357";
const DATA_DIR = path.join(__dirname, ".tmp-data-restart");

// Env for the app. `wipe` is true only for the first (signup) launch; the
// restart reuses the SAME dir untouched — that's the whole point.
function appEnvFor(devEnv, turso, deliveryUrl, wipe) {
  if (wipe) {
    fs.rmSync(DATA_DIR, { recursive: true, force: true });
    fs.mkdirSync(DATA_DIR, { recursive: true });
  }
  return {
    ...devEnv, ...process.env,
    TURSO_URL: turso.TURSO_URL, TURSO_TOKEN: turso.TURSO_TOKEN,
    POLLIS_DELIVERY_URL: deliveryUrl,
    POLLIS_DATA_DIR: DATA_DIR,
    LOG_DB_URL: "", LOG_DB_TOKEN: "", LOG_DB_ADMIN_TOKEN: "",
    WEBKIT_DISABLE_COMPOSITING_MODE: "1", GDK_BACKEND: "x11",
  };
}

async function signUp(browser, email) {
  console.log(`[restart] signing up ${email}`);
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
  console.log("[restart] reached app-ready (initial signup)");
}

// After relaunch, drive whatever resume screen appears back to app-ready and
// return a label describing the persistence path taken.
async function resumeToAppReady(browser, email) {
  // Wait for the app to settle on one of the known post-boot screens.
  const screens = ["app-ready", "pin-entry-screen", "auth-screen"];
  const end = Date.now() + 60000;
  let landed = null;
  while (Date.now() < end && !landed) {
    for (const s of screens) {
      if (await h.present(browser, s)) { landed = s; break; }
    }
    if (!landed) { await h.sleep(500); }
  }
  if (!landed) {
    throw new Error("relaunch did not settle on a known screen");
  }
  console.log(`[restart] relaunch landed on: ${landed}`);

  if (landed === "app-ready") {
    return "auto-resumed";
  }

  if (landed === "pin-entry-screen") {
    // Session/keystore persisted; just unlock with the PIN.
    await h.typeCode(browser, PIN);
    await h.waitTestId(browser, "app-ready", 30000);
    return "pin-unlock";
  }

  // auth-screen: the account must still be KNOWN (a known-account chip), else
  // persistence was lost and this is indistinguishable from a fresh install.
  const knownAccount = await h.presentSelector(browser, '[data-testid^="known-account-chip-"]');
  if (!knownAccount) {
    throw new Error("relaunch showed a blank auth screen with no known account — local data was lost");
  }
  console.log("[restart] known-account chip present — account persisted; re-authenticating");
  // Re-auth with the same email; existing PIN means pin-entry, not pin-create.
  await h.setTestIdValue(browser, "email-input", email);
  await h.clickTestId(browser, "send-otp-button");
  await h.waitTestId(browser, "otp-form-container", 20000);
  await h.typeCode(browser, "000000");
  // OTP may auto-submit; if a verify button is present, click it.
  if (await h.present(browser, "verify-otp-button")) {
    await h.clickTestId(browser, "verify-otp-button").catch(() => {});
  }
  await h.waitTestId(browser, "pin-entry-screen", 45000);
  await h.typeCode(browser, PIN);
  await h.waitTestId(browser, "app-ready", 30000);
  return "reauth+pin";
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
  const stop = (c) => { try { c && c.kill("SIGKILL"); } catch (_) {} };
  const vite = h.spawnVite(devEnv);
  children.push(vite);

  const email = `e2e_restart_${Date.now()}@pollis.test`;
  let code = 1;
  let client;
  try {
    await h.waitViteReady();

    // 1. Fresh signup (wipes + creates the data dir).
    client = await h.startClient({
      index: 0, label: "signup",
      appEnv: appEnvFor(devEnv, turso, deliveryUrl, true),
    });
    await signUp(client.browser, email);
    await shot(client.browser, "restart-1-signed-up.png");

    // Sanity: the data dir has real state (keystore + a local DB file).
    const files = fs.readdirSync(DATA_DIR);
    const hasKeystore = files.some((f) => f.includes("keystore"));
    const hasDb = files.some((f) => f.endsWith(".db"));
    console.log(`[restart] data dir after signup: ${files.join(", ")}`);
    if (!hasKeystore || !hasDb) {
      throw new Error(`data dir missing keystore/db (keystore=${hasKeystore} db=${hasDb})`);
    }

    // 2. Tear the app down but KEEP the data dir.
    await client.browser.deleteSession().catch(() => {});
    stop(client.tauriDriver);
    reapClientKeepVite();
    await h.sleep(3000);
    // Re-warm Vite's lazy transforms so the relaunch's one-shot page load is warm.
    h.warmVite();

    // 3. Relaunch on the SAME data dir (no wipe).
    client = await h.startClient({
      index: 0, label: "relaunch",
      appEnv: appEnvFor(devEnv, turso, deliveryUrl, false),
    });

    // 4. Assert persistence + drive back to app-ready.
    const pathTaken = await resumeToAppReady(client.browser, email);
    await shot(client.browser, "restart-2-resumed.png");
    console.log(`[restart] SUCCESS: account persisted across restart (path: ${pathTaken})`);
    code = 0;
  } catch (err) {
    console.error("[restart] FAILED:", err.message);
    if (client && client.browser) {
      await shot(client.browser, "restart-FAIL.png").catch(() => {});
      const src = await client.browser.getPageSource().catch(() => "");
      fs.writeFileSync(path.join(ARTIFACTS, "restart-FAIL.html"), src);
      const ids = [...src.matchAll(/data-testid="([^"]+)"/g)].map((m) => m[1]);
      console.error("[restart] on-screen testids:", [...new Set(ids)].join(", "));
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

main().catch((e) => { console.error("[restart] fatal:", e); h.reap(); process.exit(1); });
