/*
 * Shared plumbing for the raw-webdriverio Tauri e2e scripts (e2e.js,
 * smoke.js, ...). See e2e/README.md for why this drives the real app via
 * tauri-driver + WebKitWebDriver instead of the wdio test runner.
 */

const fs = require("fs");
const net = require("net");
const os = require("os");
const path = require("path");
const { spawn, spawnSync } = require("child_process");
const dotenv = require("dotenv");

const ROOT = path.resolve(__dirname, "..", "..");
const UI_PORT = 5173;
const DS_PORT = 8788;
const DS_URL = `http://127.0.0.1:${DS_PORT}`;
const DS_BIN = path.join(ROOT, "target", "debug", "pollis-delivery");
const APP_BIN = path.join(ROOT, "target", "debug", "pollis");
const TAURI_DRIVER = path.resolve(os.homedir(), ".cargo", "bin", "tauri-driver");

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// Parse a dotenv file from the repo root if it exists, else {}. In CI the app's
// env is supplied by the workflow (e2e/scripts/start-backend.sh -> $GITHUB_ENV),
// so .env.development / .env.test may not exist on disk — reading must not throw.
function readEnvFile(name) {
  try {
    return dotenv.parse(fs.readFileSync(path.join(ROOT, name)));
  } catch (_) {
    return {};
  }
}

// Turso creds for the writable test DB. Prefer the process env (CI: exported by
// e2e/scripts/start-backend.sh, which points these at the local libsql fixture)
// and fall back to .env.test for a local run against a hand-provisioned DB.
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
  // Only reap a delivery service we OWN (the self-spawn path). When
  // POLLIS_DELIVERY_URL is set the DS is an EXTERNAL fixture managed by
  // e2e/scripts/start-backend.sh — killing it here would break the run.
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
  present, waitTestId, clickTestId, setTestIdValue, typeCode,
  makeShot, dumpFailure,
};
