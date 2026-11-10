#!/usr/bin/env node
// #1794: Ctrl+V on X11 against a clipboard owner that never answers targets it
// doesn't offer (Steam's "Copy to clipboard"). The real app, under GDK_BACKEND=x11
// on a private Xvfb, with the fake owner from scripts/x11-clipboard-repro.
// WebKitGTK's own paste asks every owner for targets it never offered and waits
// ~30 s on each, so without the app's keydown interception the page dies here.
// No DS or Turso needed: it pastes into the login screen's email input.

const fs = require("fs");
const os = require("os");
const path = require("path");
const { spawn, spawnSync } = require("child_process");
const dotenv = require("dotenv");
const { remote } = require("webdriverio");
const h = require("./lib/harness");

const ARTIFACTS = path.join(__dirname, "artifacts");
const shot = h.makeShot(ARTIFACTS);
const REPRO = path.join(h.ROOT, "scripts", "x11-clipboard-repro");
const FAKE_OWNER = path.join(REPRO, "target", "debug", "fake_steam");
const XTEST = path.join(REPRO, "target", "debug", "xtest");
const XDISPLAY = process.env.CLIPBOARD_X11_DISPLAY || ":97";

const REQUIRED_PLACEHOLDERS = {
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

// Resolves to the value, or to `fallback` once `ms` passes: a hung page must not hang the run.
function within(ms, promise, fallback) {
  return Promise.race([promise, new Promise((r) => setTimeout(() => r(fallback), ms))]);
}

// The page answers a trivial script within `ms`.
async function responsive(browser, ms) {
  return (await within(ms, browser.execute(() => 1).catch(() => 0), 0)) === 1;
}

async function emailValue(browser) {
  return within(3000, browser.execute(() => document.querySelector('[data-testid="email-input"]').value), null);
}

async function clearAndFocusEmail(browser) {
  await browser.execute(() => {
    const el = document.querySelector('[data-testid="email-input"]');
    const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value").set;
    setter.call(el, "");
    el.dispatchEvent(new Event("input", { bubbles: true }));
    el.focus();
  });
}

// Right-click the email input with real X input, then pick Paste by its mnemonic.
async function contextMenuPaste(browser) {
  // On screen: real pointer input must land on the field.
  await browser.setWindowRect(0, 0, 1200, 900).catch(() => {});
  await h.sleep(500);
  const at = await browser.execute(() => {
    const r = document.querySelector('[data-testid="email-input"]').getBoundingClientRect();
    return {
      x: Math.round(window.screenX + r.left + r.width / 2),
      y: Math.round(window.screenY + (window.outerHeight - window.innerHeight) + r.top + r.height / 2),
    };
  });
  const xtest = (...args) => spawnSync(XTEST, args, { env: { ...process.env, DISPLAY: XDISPLAY }, stdio: "inherit" });
  xtest("click", String(at.x), String(at.y), "3");
  await h.sleep(800);
  if (process.env.CLIPBOARD_X11_DEBUG) {
    console.log("[clipboard-x11] right-click at", at);
    spawnSync("import", ["-display", XDISPLAY, "-window", "root", path.join(ARTIFACTS, "clipboard-x11-menu.png")]);
  }
  xtest("key", "p");
}

// A real Ctrl+V key chord, through WebKit's keyboard path (not an editing command).
async function pressCtrlV(browser) {
  const CONTROL = "";
  await within(
    5000,
    browser.action("key").down(CONTROL).down("v").up("v").up(CONTROL).perform().catch(() => {}),
    undefined,
  );
}

function startOwner(env, mode, file) {
  const log = [];
  const child = spawn(FAKE_OWNER, [mode, file], {
    env: { ...process.env, DISPLAY: XDISPLAY, ...env },
    stdio: ["ignore", "ignore", "pipe"],
  });
  child.stderr.on("data", (d) => log.push(...String(d).trim().split("\n")));
  return { child, log };
}

async function main() {
  h.reap();
  const build = spawnSync("cargo", ["build", "-q", "--bin", "fake_steam", "--bin", "xtest"], { cwd: REPRO, stdio: "inherit" });
  if (build.status !== 0) {
    throw new Error("could not build the fake clipboard owner");
  }
  const devEnv = readDevEnv();
  const children = [];
  const stop = (c) => { try { c && c.kill("SIGKILL"); } catch (_) {} };

  const xvfb = spawn("Xvfb", [XDISPLAY, "-nolisten", "tcp"], { stdio: "ignore" });
  children.push(xvfb);
  await h.sleep(1000);

  const vite = h.spawnVite(devEnv);
  children.push(vite);

  const appEnv = { ...process.env, ...REQUIRED_PLACEHOLDERS, ...devEnv,
    POLLIS_DATA_DIR: path.join(__dirname, ".tmp-data-clipboard-x11"),
    DISPLAY: XDISPLAY, GDK_BACKEND: "x11", WEBKIT_DISABLE_COMPOSITING_MODE: "1" };
  delete appEnv.WAYLAND_DISPLAY;
  fs.rmSync(appEnv.POLLIS_DATA_DIR, { recursive: true, force: true });
  fs.mkdirSync(appEnv.POLLIS_DATA_DIR, { recursive: true });

  const shotFile = path.join(os.tmpdir(), `pollis-1794-${process.pid}.jpg`);
  fs.writeFileSync(shotFile, "jpg");

  let tauriDriver;
  let browser;
  let owner;
  const results = [];
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
    await h.waitTestId(browser, "email-input", 30000);

    const cases = [
      {
        name: "steam-ignore: a file-only owner that never answers other targets leaves the page live",
        mode: "ignore", file: shotFile, env: {},
        check: async () => (await responsive(browser, 3000)) && (await emailValue(browser)) === "",
      },
      {
        name: "text-ignore: text from an owner that never answers other targets is pasted",
        mode: "ignore", file: "", env: { FAKE_TEXT: "paste-test@example.com" },
        check: async () => (await emailValue(browser)) === "paste-test@example.com",
      },
      {
        name: "text-incr: a 300 KB text paste over INCR arrives whole",
        mode: "ignore", file: "", env: { FAKE_TEXT_BYTES: "300000", FAKE_INCR_CHUNK: "65536" },
        // A single-line input turns the line breaks into spaces, so the length survives.
        check: async () => ((await emailValue(browser)) || "").length === 300000,
      },
      {
        name: "context-menu: right-click Paste takes the same bounded path",
        mode: "ignore", file: "", env: { FAKE_TEXT: "menu-paste@example.com" }, via: contextMenuPaste,
        check: async () => (await emailValue(browser)) === "menu-paste@example.com",
      },
    ];
    for (const c of cases) {
      owner = startOwner(c.env, c.mode, c.file);
      await h.sleep(500);
      const alive = await responsive(browser, 3000);
      if (alive) {
        await clearAndFocusEmail(browser);
      }
      const t0 = Date.now();
      await (c.via || pressCtrlV)(browser);
      let ok = false;
      const end = Date.now() + 8000;
      while (alive && Date.now() < end) {
        if (await c.check()) {
          ok = true;
          break;
        }
        await h.sleep(250);
      }
      const ms = Date.now() - t0;
      results.push({ name: c.name, ok, ms });
      console.log(`[clipboard-x11] ${ok ? "PASS" : "FAIL"} (${ms} ms) ${c.name}`);
      console.log(`[clipboard-x11]   owner saw: ${owner.log.filter((l) => l.includes("request")).map((l) => l.replace("fake_steam: request for ", "")).join(", ") || "nothing"}`);
      stop(owner.child);
      owner = null;
      if (!ok) {
        const v = await emailValue(browser);
        console.log(`[clipboard-x11]   field holds ${v === null ? "(no answer)" : `${v.length} chars: ${JSON.stringify(v.slice(0, 40))}`}`);
        await within(5000, shot(browser, `clipboard-x11-${results.length}.png`).catch(() => {}), undefined);
      }
    }
  } catch (err) {
    console.error("[clipboard-x11] FAILED:", err.message);
    results.push({ ok: false });
  } finally {
    if (owner) {
      stop(owner.child);
    }
    if (browser) {
      await within(5000, browser.deleteSession().catch(() => {}), undefined);
    }
    stop(tauriDriver);
    for (const c of children) {
      stop(c);
    }
    fs.rmSync(shotFile, { force: true });
    h.reap();
  }
  const passed = results.length > 0 && results.every((r) => r.ok);
  console.log(`[clipboard-x11] ${passed ? "SUCCESS" : "FAILURE"}`);
  process.exit(passed ? 0 : 1);
}

main().catch((e) => { console.error("[clipboard-x11] fatal:", e); h.reap(); process.exit(1); });
