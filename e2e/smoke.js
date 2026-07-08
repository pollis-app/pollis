#!/usr/bin/env node
/*
 * Fast smoke check: the real Tauri app launches and the login screen
 * renders. Unlike e2e.js, this never touches the delivery service or Turso —
 * `checkStoredSession()` (frontend/src/App.tsx) resolves the logged-out path
 * entirely from local Tauri commands (getSession / listKnownAccounts), so
 * proving the auth screen shows up needs nothing but Vite + the app binary.
 * That makes this safe to run in CI with no shared-DB dependency.
 *
 * Flow: launch → wait for [data-testid="auth-screen"] → screenshot. Exits
 * non-zero (with FAIL.png/FAIL.html) if the screen never appears.
 *
 * No .env.development required (CI has none — it's Doppler-sourced, not
 * committed): the login screen doesn't read any of those vars before it
 * renders, so a missing file just means an empty env override.
 *
 * Config::from_env() (pollis-core/src/config.rs) DOES hard-require
 * TURSO_URL/TURSO_TOKEN/R2_S3_ENDPOINT/R2_PUBLIC_URL to be present — baked
 * in at compile time via option_env! or present at runtime — or the app
 * panics in its Tauri setup hook before any window opens, login screen or
 * not. Since this binary is built with a clean env (no baked secrets — see
 * README Prerequisites), unresolved placeholders are supplied here so the
 * app can boot; they are never dialed before auth-screen renders. Same
 * trick as mls-tests.yml's placeholder `.env.test`.
 */

const fs = require("fs");
const path = require("path");
const { spawn } = require("child_process");
const dotenv = require("dotenv");
const { remote } = require("webdriverio");
const h = require("./lib/harness");

const ARTIFACTS = path.join(__dirname, "artifacts");
const shot = h.makeShot(ARTIFACTS);

const REQUIRED_PLACEHOLDERS = {
  TURSO_URL: "libsql://placeholder.invalid",
  TURSO_TOKEN: "placeholder",
  R2_S3_ENDPOINT: "https://placeholder.invalid",
  R2_PUBLIC_URL: "https://placeholder.invalid",
};

function readDevEnv() {
  try {
    return dotenv.parse(fs.readFileSync(path.join(h.ROOT, ".env.development")));
  } catch {
    return {};
  }
}

async function main() {
  h.reap();
  const devEnv = readDevEnv();

  const children = [];
  const stop = (c) => { try { c && c.kill("SIGKILL"); } catch (_) {} };

  const vite = h.spawnVite(devEnv);
  children.push(vite);

  // No POLLIS_DELIVERY_URL override — the login screen never calls out to
  // it, so the baked-in default is never dialed. Placeholders come before
  // devEnv so a real local .env.development still wins when present.
  const appEnv = { ...process.env, ...REQUIRED_PLACEHOLDERS, ...devEnv,
    POLLIS_DATA_DIR: path.join(__dirname, ".tmp-data-smoke"),
    WEBKIT_DISABLE_COMPOSITING_MODE: "1", GDK_BACKEND: "x11" };
  fs.rmSync(appEnv.POLLIS_DATA_DIR, { recursive: true, force: true });
  fs.mkdirSync(appEnv.POLLIS_DATA_DIR, { recursive: true });

  let tauriDriver;
  let browser;
  let code = 1;
  try {
    await h.waitViteReady();

    tauriDriver = spawn(h.TAURI_DRIVER, ["--port", "4444"], {
      stdio: ["ignore", "inherit", "inherit"], env: appEnv,
    });
    await h.waitPort(4444, "127.0.0.1", 15000);

    browser = await remote({
      hostname: "127.0.0.1", port: 4444, path: "/",
      capabilities: { "tauri:options": { application: h.APP_BIN } },
      logLevel: "error",
      connectionRetryTimeout: 45000,
      connectionRetryCount: 1,
    });

    await h.sleep(6000);

    await h.waitTestId(browser, "auth-screen", 30000);
    await shot(browser, "smoke-auth-screen.png");
    console.log("[smoke] SUCCESS: login screen rendered");
    code = 0;
  } catch (err) {
    console.error("[smoke] FAILED:", err.message);
    if (browser) {
      await h.dumpFailure(browser, ARTIFACTS, shot);
    }
  } finally {
    if (browser) {
      await browser.deleteSession().catch(() => {});
    }
    stop(tauriDriver);
    for (const c of children) {
      stop(c);
    }
    h.reap();
  }
  process.exit(code);
}

main().catch((e) => { console.error("[smoke] fatal:", e); h.reap(); process.exit(1); });
