#!/usr/bin/env node
/*
 * Self-contained E2E driver for the real Pollis desktop app.
 *
 * Same local stack as run.js (Vite dev server + local pollis-delivery with
 * DEV_OTP=000000 + the writable test DB), but instead of the WebdriverIO test
 * runner it drives the app through raw `webdriverio` remote() calls. The raw
 * client reliably drives this webkit2gtk build, whereas the mocha runner
 * intermittently stalls the first WebView command.
 *
 * Flow: launch → create account (email → dev OTP → save secret key → PIN) →
 * land on the ready app → screenshot. Nothing talks to prod; no email is sent.
 */

const fs = require("fs");
const net = require("net");
const os = require("os");
const path = require("path");
const { spawn, spawnSync } = require("child_process");
const dotenv = require("dotenv");
const { remote } = require("webdriverio");

const ROOT = path.resolve(__dirname, "..");
const UI_PORT = 5173;
const DS_PORT = 8788;
const DS_URL = `http://127.0.0.1:${DS_PORT}`;
const DS_BIN = path.join(ROOT, "target", "debug", "pollis-delivery");
const APP_BIN = path.join(ROOT, "target", "debug", "pollis");
const TAURI_DRIVER = path.resolve(os.homedir(), ".cargo", "bin", "tauri-driver");
const ARTIFACTS = path.join(__dirname, "artifacts");

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

function reap() {
  for (const p of ["tauri-driver", "WebKitWebDriver", "bin/vite", "target/debug/pollis-delivery"]) {
    spawnSync("pkill", ["-9", "-f", p], { stdio: "ignore" });
  }
  spawnSync("pkill", ["-9", "-x", "pollis"], { stdio: "ignore" });
}

function waitPort(port, hosts, ms) {
  const list = Array.isArray(hosts) ? hosts : [hosts];
  const end = Date.now() + ms;
  return new Promise((res, rej) => {
    const attempt = () => {
      let pending = list.length;
      let done = false;
      for (const host of list) {
        const s = net.connect({ port, host });
        s.once("connect", () => { s.destroy(); if (!done) { done = true; res(); } });
        s.once("error", () => {
          s.destroy();
          if (--pending === 0 && !done) {
            Date.now() > end ? rej(new Error(`timeout ${list}:${port}`)) : setTimeout(attempt, 150);
          }
        });
      }
    };
    attempt();
  });
}

function curl(url) {
  spawnSync("curl", ["-s", "-g", "--max-time", "8", "-o", "/dev/null", url]);
}

function warmVite() {
  const base = `http://[::1]:${UI_PORT}`;
  const r = spawnSync("curl", ["-s", "-g", "--max-time", "8", `${base}/`], { encoding: "utf8" });
  const html = r.stdout || "";
  const srcs = [...html.matchAll(/<script[^>]+src="([^"]+)"/g)].map((m) => m[1].replace(/^https?:\/\/[^/]+/, ""));
  for (const s of [...srcs, "/@vite/client", "/src/main.tsx"]) {
    curl(`${base}${s.startsWith("/") ? "" : "/"}${s}`);
  }
}

// ---- app-driving helpers -------------------------------------------------

async function present(browser, testId) {
  const els = await browser.$$(`[data-testid="${testId}"]`);
  return els.length > 0;
}

async function waitTestId(browser, testId, timeoutMs = 30000, label = testId) {
  const end = Date.now() + timeoutMs;
  while (Date.now() < end) {
    if (await present(browser, testId)) {
      return;
    }
    await sleep(400);
  }
  throw new Error(`timed out waiting for [data-testid="${label}"]`);
}

// WebKitWebDriver's native click doesn't reliably fire React handlers here; a
// DOM click dispatched in-page does.
async function clickTestId(browser, testId) {
  await waitTestId(browser, testId);
  const ok = await browser.execute((id) => {
    const el = document.querySelector(`[data-testid="${id}"]`);
    if (el) { el.click(); return true; }
    return false;
  }, testId);
  if (!ok) {
    throw new Error(`click: ${testId} vanished`);
  }
}

async function setTestIdValue(browser, testId, value) {
  await waitTestId(browser, testId);
  const el = await browser.$(`[data-testid="${testId}"]`);
  await el.setValue(value);
}

// OTP / PIN: N separate <input maxlength=1> boxes, aria-label="OTP digit K".
async function typeCode(browser, digits) {
  for (let i = 0; i < digits.length; i++) {
    const box = await browser.$(`[aria-label="OTP digit ${i + 1}"]`);
    await box.setValue(digits[i]);
  }
}

// Screenshot with a hard timeout: a wedged WebKit compositor can hang the
// screenshot endpoint indefinitely; don't let that take the whole run down.
async function shot(browser, name, timeoutMs = 25000) {
  fs.mkdirSync(ARTIFACTS, { recursive: true });
  const file = path.join(ARTIFACTS, name);
  try {
    await Promise.race([
      browser.saveScreenshot(file),
      sleep(timeoutMs).then(() => { throw new Error("screenshot timed out"); }),
    ]);
    console.log(`[e2e] screenshot -> ${file}`);
    return true;
  } catch (e) {
    console.log(`[e2e] screenshot ${name} failed (${e.message}) — continuing`);
    return false;
  }
}

// ---- main ----------------------------------------------------------------

async function main() {
  reap();
  const devEnv = dotenv.parse(fs.readFileSync(path.join(ROOT, ".env.development")));
  const testEnv = dotenv.parse(fs.readFileSync(path.join(ROOT, ".env.test")));
  if (!testEnv.TURSO_URL || !testEnv.TURSO_TOKEN) {
    throw new Error(".env.test missing TURSO_URL/TOKEN (need a writable disposable DB)");
  }

  const children = [];
  const stop = (c) => { try { c && c.kill("SIGKILL"); } catch (_) {} };

  // Vite dev server (the UI the debug binary loads).
  console.log(`[e2e] starting Vite on :${UI_PORT}`);
  const vite = spawn(
    "pnpm",
    ["--filter", "frontend", "dev", "--", "--port", String(UI_PORT), "--strictPort"],
    { cwd: ROOT, env: { ...process.env, ...devEnv }, stdio: ["ignore", "inherit", "inherit"] }
  );
  children.push(vite);

  // Local delivery service.
  const dsEnv = { ...process.env, TURSO_URL: testEnv.TURSO_URL, TURSO_TOKEN: testEnv.TURSO_TOKEN,
    PORT: String(DS_PORT), DEV_OTP: "000000", RUST_LOG: "pollis_delivery=info" };
  delete dsEnv.RESEND_API_KEY; delete dsEnv.LOG_DB_URL; delete dsEnv.LOG_DB_TOKEN; delete dsEnv.LOG_DB_ADMIN_TOKEN;
  console.log(`[e2e] starting delivery service on ${DS_URL} (DEV_OTP=000000)`);
  const ds = spawn(DS_BIN, [], { env: dsEnv, stdio: ["ignore", "inherit", "inherit"] });
  children.push(ds);

  // App env: dev creds + writable test DB + local DS. The two WebKit vars
  // mirror the project's own `pnpm dev` script — WebKitGTK compositing is
  // broken on this setup and causes rendering stalls / hung screenshot
  // commands without them.
  const appEnv = { ...process.env, ...devEnv, TURSO_URL: testEnv.TURSO_URL, TURSO_TOKEN: testEnv.TURSO_TOKEN,
    POLLIS_DELIVERY_URL: DS_URL, POLLIS_DATA_DIR: path.join(__dirname, ".tmp-data"),
    WEBKIT_DISABLE_COMPOSITING_MODE: "1", GDK_BACKEND: "x11" };
  fs.rmSync(appEnv.POLLIS_DATA_DIR, { recursive: true, force: true });
  fs.mkdirSync(appEnv.POLLIS_DATA_DIR, { recursive: true });

  let tauriDriver;
  let browser;
  let code = 1;
  try {
    await waitPort(UI_PORT, ["::1", "127.0.0.1"], 60000);
    await sleep(3000);
    warmVite();
    await sleep(1500);
    warmVite();
    console.log("[e2e] Vite up and warmed");
    await waitPort(DS_PORT, ["127.0.0.1", "::1"], 20000);
    console.log("[e2e] delivery service up");

    // tauri-driver → WebKitWebDriver → app (inherits appEnv).
    tauriDriver = spawn(TAURI_DRIVER, ["--port", "4444"], {
      stdio: ["ignore", "inherit", "inherit"], env: appEnv,
    });
    await waitPort(4444, "127.0.0.1", 15000);

    browser = await remote({
      hostname: "127.0.0.1", port: 4444, path: "/",
      capabilities: { "tauri:options": { application: APP_BIN } },
      logLevel: "error",
      // Fail a wedged command in 45s instead of the default 120s.
      connectionRetryTimeout: 45000,
      connectionRetryCount: 1,
    });

    // Let the initial Vite page load settle before the first command (mirrors
    // the reliable standalone probe).
    await sleep(6000);

    const email = `e2e_${Date.now()}@pollis.test`;
    const PIN = "1357";
    console.log(`[e2e] account: ${email}`);

    // 1. Auth screen.
    await waitTestId(browser, "auth-screen", 30000);
    await shot(browser, "01-auth-screen.png");

    // 2. Email → request OTP.
    await setTestIdValue(browser, "email-input", email);
    await clickTestId(browser, "send-otp-button");
    await waitTestId(browser, "otp-form-container", 20000);
    console.log("[e2e] email submitted, OTP form shown");

    // 3. Dev OTP 000000 (auto-submits) → new-account secret-key flow.
    await typeCode(browser, "000000");
    await waitTestId(browser, "save-secret-key-warning-screen", 45000);
    console.log("[e2e] OTP verified, account bootstrapped");

    // 4. Save-secret-key dance.
    await clickTestId(browser, "save-secret-key-acknowledge-button");
    await waitTestId(browser, "save-secret-key-screen");
    const secretKey = (await (await browser.$('[data-testid="secret-key-display"]')).getText()).trim();
    if (!secretKey) {
      throw new Error("secret key display was empty");
    }
    await clickTestId(browser, "secret-key-saved-button");
    await waitTestId(browser, "save-secret-key-confirm-screen");
    await setTestIdValue(browser, "secret-key-confirm-input", secretKey);
    await clickTestId(browser, "confirm-secret-key-button");
    console.log("[e2e] secret key confirmed");

    // 5. Create PIN (enter + confirm, each auto-advances/submits).
    await waitTestId(browser, "pin-create-screen");
    await typeCode(browser, PIN);
    await typeCode(browser, PIN);
    console.log("[e2e] PIN created");

    // 6. Ready app → proof.
    await waitTestId(browser, "app-ready", 60000);
    await shot(browser, "99-app-ready.png");
    console.log("[e2e] SUCCESS: reached app-ready");
    code = 0;
  } catch (err) {
    console.error("[e2e] FAILED:", err.message);
    if (browser) {
      await shot(browser, "FAIL.png").catch(() => {});
      const src = await browser.getPageSource().catch(() => "");
      fs.writeFileSync(path.join(ARTIFACTS, "FAIL.html"), src);
      const ids = [...src.matchAll(/data-testid="([^"]+)"/g)].map((m) => m[1]);
      console.error("[e2e] on-screen testids:", [...new Set(ids)].join(", "));
    }
  } finally {
    if (browser) {
      await browser.deleteSession().catch(() => {});
    }
    stop(tauriDriver);
    for (const c of children) {
      stop(c);
    }
    reap();
  }
  process.exit(code);
}

main().catch((e) => { console.error("[e2e] fatal:", e); reap(); process.exit(1); });
