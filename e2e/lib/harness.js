// Shared plumbing for the raw-webdriverio Tauri e2e scripts (see e2e/README.md).

const fs = require("fs");
const net = require("net");
const os = require("os");
const path = require("path");
const { spawn, spawnSync } = require("child_process");
const dotenv = require("dotenv");
const { remote } = require("webdriverio");

const ROOT = path.resolve(__dirname, "..", "..");
const UI_PORT = 5173;
const DS_PORT = 8788;
const DS_URL = `http://127.0.0.1:${DS_PORT}`;
const DS_BIN = path.join(ROOT, "target", "debug", "pollis-delivery");
const APP_BIN = path.join(ROOT, "target", "debug", "pollis");
const TAURI_DRIVER = path.resolve(os.homedir(), ".cargo", "bin", "tauri-driver");

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// Parse a repo-root dotenv file, else {} (CI supplies env, so the file may not exist).
function readEnvFile(name) {
  try {
    return dotenv.parse(fs.readFileSync(path.join(ROOT, name)));
  } catch (_) {
    return {};
  }
}

// Test-DB Turso creds: process env (CI's libsql fixture), else .env.test.
function tursoEnv() {
  const fileEnv = readEnvFile(".env.test");
  const TURSO_URL = process.env.TURSO_URL || fileEnv.TURSO_URL;
  const TURSO_TOKEN = process.env.TURSO_TOKEN || fileEnv.TURSO_TOKEN;
  if (!TURSO_URL || !TURSO_TOKEN) {
    throw new Error(
      "need TURSO_URL/TURSO_TOKEN (process env or .env.test) — a writable disposable DB; " +
        "run e2e/scripts/start-backend.sh or see e2e/README.md"
    );
  }
  return { TURSO_URL, TURSO_TOKEN };
}

function reap() {
  const procs = ["tauri-driver", "WebKitWebDriver", "bin/vite"];
  // Only reap a DS we spawned; an external one (POLLIS_DELIVERY_URL) belongs to start-backend.sh.
  if (!process.env.POLLIS_DELIVERY_URL) {
    procs.push("target/debug/pollis-delivery");
  }
  for (const p of procs) {
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

function spawnVite(devEnv) {
  console.log(`[e2e] starting Vite on :${UI_PORT}`);
  return spawn(
    "pnpm",
    ["--filter", "frontend", "dev", "--", "--port", String(UI_PORT), "--strictPort"],
    { cwd: ROOT, env: { ...process.env, ...devEnv }, stdio: ["ignore", "inherit", "inherit"] }
  );
}

async function waitViteReady() {
  await waitPort(UI_PORT, ["::1", "127.0.0.1"], 60000);
  await sleep(3000);
  warmVite();
  await sleep(1500);
  warmVite();
  console.log("[e2e] Vite up and warmed");
}

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

// Composer is contentEditable: press+release per key (browser.keys chords drop repeats, #1298), then await data-value.
async function setComposerText(browser, text) {
  await waitTestId(browser, "message-input");
  const el = await browser.$('[data-testid="message-input"]');
  await el.click();
  const typing = browser.action("key");
  for (const ch of text) {
    typing.down(ch).up(ch);
  }
  await typing.perform();
  await browser.waitUntil(
    async () => (await el.getAttribute("data-value")) === text,
    { timeout: 15000, timeoutMsg: `composer never reached ${JSON.stringify(text)}` },
  );
}

// OTP / PIN: N separate <input maxlength=1> boxes, aria-label="OTP digit K".
async function typeCode(browser, digits) {
  for (let i = 0; i < digits.length; i++) {
    const box = await browser.$(`[aria-label="OTP digit ${i + 1}"]`);
    await box.setValue(digits[i]);
  }
}

// Raw-CSS-selector variants of the testid helpers, for prefix matches and id-only inputs.

async function presentSelector(browser, selector) {
  const els = await browser.$$(selector);
  return els.length > 0;
}

async function waitSelector(browser, selector, timeoutMs = 30000, label = selector) {
  const end = Date.now() + timeoutMs;
  while (Date.now() < end) {
    if (await presentSelector(browser, selector)) {
      return;
    }
    await sleep(400);
  }
  throw new Error(`timed out waiting for ${label}`);
}

// Same in-page DOM click as clickTestId.
async function clickSelector(browser, selector) {
  await waitSelector(browser, selector);
  const ok = await browser.execute((sel) => {
    const el = document.querySelector(sel);
    if (el) { el.click(); return true; }
    return false;
  }, selector);
  if (!ok) {
    throw new Error(`click: ${selector} vanished`);
  }
}

async function setSelectorValue(browser, selector, value) {
  await waitSelector(browser, selector);
  const el = await browser.$(selector);
  await el.setValue(value);
}

// Multi-client: each client gets its own (port, native-port); client 0 keeps 4444/4445 for single-client scripts.
function clientPorts(index = 0) {
  return { driverPort: 4444 + index * 2, nativePort: 4445 + index * 2 };
}

// Spawn tauri-driver + a webdriverio session for one client; `appEnv` is the app's full env.
async function startClient({ index = 0, appEnv, settleMs = 6000, label = `client${index}` }) {
  const { driverPort, nativePort } = clientPorts(index);
  console.log(`[e2e] ${label}: tauri-driver :${driverPort} (native :${nativePort})`);
  const tauriDriver = spawn(
    TAURI_DRIVER,
    ["--port", String(driverPort), "--native-port", String(nativePort)],
    { stdio: ["ignore", "inherit", "inherit"], env: appEnv }
  );
  await waitPort(driverPort, "127.0.0.1", 15000);

  const browser = await remote({
    hostname: "127.0.0.1", port: driverPort, path: "/",
    capabilities: { "tauri:options": { application: APP_BIN } },
    logLevel: "error",
    connectionRetryTimeout: 45000,
    connectionRetryCount: 1,
  });

  // Let the initial Vite page load settle before the first command.
  await sleep(settleMs);
  return { browser, tauriDriver, driverPort, nativePort, label };
}

function makeShot(artifactsDir) {
  // Screenshot with a hard timeout: a wedged WebKit compositor can hang the
  // screenshot endpoint indefinitely; don't let that take the whole run down.
  return async function shot(browser, name, timeoutMs = 25000) {
    fs.mkdirSync(artifactsDir, { recursive: true });
    const file = path.join(artifactsDir, name);
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
  };
}

async function dumpFailure(browser, artifactsDir, shot) {
  await shot(browser, "FAIL.png").catch(() => {});
  const src = await browser.getPageSource().catch(() => "");
  fs.mkdirSync(artifactsDir, { recursive: true });
  fs.writeFileSync(path.join(artifactsDir, "FAIL.html"), src);
  const ids = [...src.matchAll(/data-testid="([^"]+)"/g)].map((m) => m[1]);
  console.error("[e2e] on-screen testids:", [...new Set(ids)].join(", "));
}

module.exports = {
  ROOT, UI_PORT, DS_PORT, DS_URL, DS_BIN, APP_BIN, TAURI_DRIVER,
  sleep, reap, waitPort, curl, warmVite, spawnVite, waitViteReady,
  readEnvFile, tursoEnv,
  present, waitTestId, clickTestId, setTestIdValue, setComposerText, typeCode,
  presentSelector, waitSelector, clickSelector, setSelectorValue,
  clientPorts, startClient,
  makeShot, dumpFailure,
};
