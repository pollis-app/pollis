// §5.10 memory-after-lock: does `pin::lock` actually zeroize secrets in the
// main `pollis` (Rust core) process memory, or do the PIN / account secret key
// linger after lock? Signs up one client, snapshots process memory while
// UNLOCKED (baseline — secrets SHOULD be present), then Ctrl+L locks, waits,
// and snapshots again. Greps both cores for the real secret key + PIN.
const fs = require("fs");
const path = require("path");
const { spawn, execSync } = require("child_process");
const { remote } = require("webdriverio");
const h = require("./lib/harness");

const PIN = "8364";
const OUT = process.env.MEM_OUT || path.join(__dirname, ".tmp-mem-out");
fs.mkdirSync(OUT, { recursive: true });

function pidOfApp() {
  // the main Rust-core process is the APP_BIN itself (WebKit helpers are named otherwise)
  const out = execSync(`pgrep -f 'target/debug/pollis($| )' || true`).toString().trim();
  const pids = out.split(/\s+/).filter(Boolean);
  return pids[pids.length - 1]; // newest
}

// Dump WITHOUT ptrace. `gcore` and /proc/pid/mem both need
// CAP_SYS_PTRACE or kernel.yama.ptrace_scope=0 (this box is =1), which needs
// root we do not have. But core_pattern pipes to systemd-coredump and
// `ulimit -c` is unlimited, so SIGABRT makes the KERNEL write the core and
// `coredumpctl dump` reads it back as our own user. It also has a property
// gcore lacks: the image is the process exactly as the signal found it, since
// nothing runs in-process to produce it.
//
// The cost is that the process dies, so each phase needs its own app run.
function abortAndGrep(phase, pid, needles) {
  const out = path.join(OUT, `core_${phase}.core`);
  try { execSync(`kill -ABRT ${pid}`); } catch (e) {
    console.log(`[mem] ${phase}: could not signal ${pid}: ${e}`);
    return null;
  }
  // Give systemd-coredump time to receive and store it.
  for (let i = 0; i < 60; i++) {
    try {
      execSync(`coredumpctl dump ${pid} --output=${out}`, { stdio: "pipe", timeout: 180000 });
      break;
    } catch (_) { execSync("sleep 1"); }
  }
  if (!fs.existsSync(out)) {
    console.log(`[mem] ${phase}: NO CORE captured for pid ${pid}`);
    return null;
  }
  const size = fs.statSync(out).size;
  console.log(`[mem] ${phase}: core ${(size / 1e6).toFixed(0)} MB`);
  const result = {};
  for (const [label, needle] of needles) {
    let count = 0;
    try {
      count = parseInt(execSync(`grep -a -c -F ${JSON.stringify(needle)} ${out} || true`).toString().trim() || "0", 10);
    } catch (_) {}
    console.log(`[mem] ${phase}: ${label} occurrences = ${count}`);
    result[label] = count;
  }
  fs.rmSync(out, { force: true });
  return result;
}

async function main() {
  h.reap();
  try { execSync("pkill -x pollis || true"); } catch (_) {}
  const devEnv = h.readEnvFile(".env.development");
  const TURSO_URL = process.env.TURSO_URL || devEnv.TURSO_URL;
  const TURSO_TOKEN = process.env.TURSO_TOKEN || devEnv.TURSO_TOKEN;
  const children = [];
  const stop = (c) => { try { c && c.kill("SIGKILL"); } catch (_) {} };

  const vite = h.spawnVite(devEnv);
  children.push(vite);
  const deliveryUrl = process.env.POLLIS_DELIVERY_URL || h.DS_URL;
  if (!process.env.POLLIS_DELIVERY_URL) {
    const dsEnv = { ...process.env, TURSO_URL, TURSO_TOKEN,
      PORT: String(h.DS_PORT), DEV_OTP: "000000", RUST_LOG: "pollis_delivery=warn" };
    delete dsEnv.RESEND_API_KEY; delete dsEnv.LOG_DB_URL; delete dsEnv.LOG_DB_TOKEN; delete dsEnv.LOG_DB_ADMIN_TOKEN;
    console.log(`[mem] starting delivery service on ${deliveryUrl} (DEV_OTP=000000)`);
    children.push(spawn(h.DS_BIN, [], { env: dsEnv, stdio: ["ignore", "inherit", "inherit"] }));
  } else {
    console.log(`[mem] using external delivery service at ${deliveryUrl}`);
  }

  const appEnv = { ...devEnv, ...process.env, TURSO_URL, TURSO_TOKEN,
    POLLIS_DELIVERY_URL: deliveryUrl, POLLIS_DATA_DIR: path.join(__dirname, ".tmp-data-mem"),
    WEBKIT_DISABLE_COMPOSITING_MODE: "1", GDK_BACKEND: "x11" };
  fs.rmSync(appEnv.POLLIS_DATA_DIR, { recursive: true, force: true });
  fs.mkdirSync(appEnv.POLLIS_DATA_DIR, { recursive: true });

  let tauriDriver, browser, code = 1;
  try {
    await h.waitViteReady();
    await h.waitPort(h.DS_PORT, ["127.0.0.1", "::1"], 20000);
    console.log("[mem] delivery service up");
    tauriDriver = spawn(h.TAURI_DRIVER, ["--port", "4444"], { stdio: ["ignore", "inherit", "inherit"], env: appEnv });
    await h.waitPort(4444, "127.0.0.1", 15000);
    browser = await remote({ hostname: "127.0.0.1", port: 4444, path: "/",
      capabilities: { "tauri:options": { application: h.APP_BIN } },
      logLevel: "error", connectionRetryTimeout: 45000, connectionRetryCount: 1 });
    await h.sleep(6000);

    const email = `mem_${Date.now()}@pollis.test`;
    await h.waitTestId(browser, "auth-screen", 30000);
    await h.setTestIdValue(browser, "email-input", email);
    await h.clickTestId(browser, "send-otp-button");
    await h.waitTestId(browser, "otp-form-container", 20000);
    await h.typeCode(browser, "000000");
    await h.waitTestId(browser, "save-secret-key-warning-screen", 45000);
    await h.clickTestId(browser, "save-secret-key-acknowledge-button");
    await h.waitTestId(browser, "save-secret-key-screen");
    const secretKey = (await (await browser.$('[data-testid="secret-key-display"]')).getText()).trim();
    if (!secretKey) { throw new Error("secret key display empty"); }
    fs.writeFileSync(path.join(OUT, "mem-secret.txt"), `secretKey=${secretKey}\nPIN=${PIN}\n`);
    console.log(`[mem] captured secret key (len ${secretKey.length})`);
    await h.clickTestId(browser, "secret-key-saved-button");
    await h.waitTestId(browser, "save-secret-key-confirm-screen");
    await h.setTestIdValue(browser, "secret-key-confirm-input", secretKey);
    await h.clickTestId(browser, "confirm-secret-key-button");
    await h.waitTestId(browser, "pin-create-screen");
    await h.typeCode(browser, PIN);
    await h.typeCode(browser, PIN);
    await h.waitTestId(browser, "app-ready", 60000);
    console.log("[mem] app-ready (UNLOCKED)");

    const needles = [["secretKey", secretKey], ["PIN", PIN]];
    const pid = pidOfApp();
    console.log(`[mem] app pid = ${pid}`);

    // PHASE is "unlocked" (the baseline that proves the grep works at all) or
    // "locked" (the actual question). The dump kills the process, so the two
    // phases are two runs of this script rather than two dumps in one.
    const phase = process.env.MEM_PHASE === "locked" ? "locked" : "unlocked";

    if (phase === "locked") {
      // Ctrl+L clears in-memory unlock state and closes the local DB.
      await browser.keys(["Control", "l"]);
      await h.waitTestId(browser, "pin-entry-screen", 20000);
      console.log("[mem] LOCKED (pin-entry-screen reached)");
      await h.sleep(parseInt(process.env.MEM_SETTLE_MS || "8000", 10)); // give drop/zeroize a moment
    }

    const found = abortAndGrep(phase, pid, needles);
    if (!found) {
      throw new Error("no core captured - cannot conclude anything");
    }
    if (phase === "unlocked") {
      // The baseline is the control. If the secrets are NOT here, the grep or
      // the dump is broken and a clean "locked" result would mean nothing.
      const ok = found.secretKey > 0;
      console.log("[mem] VERDICT(baseline): secret " + (ok ? "IS" : "is NOT") +
        " resident while unlocked" + (ok ? "" : " - probe not measuring what it claims"));
      code = ok ? 0 : 1;
    } else {
      const clean = found.secretKey === 0 && found.PIN === 0;
      console.log("[mem] VERDICT(locked): secretKey=" + found.secretKey +
        " PIN=" + found.PIN + " - " +
        (clean ? "ZEROIZED (neither survives lock)" : "SECRETS SURVIVE LOCK"));
      code = 0;
    }
    console.log("[mem] DONE");
  } catch (e) {
    console.log(`[mem] FAILED: ${e.message}`);
    try { if (browser) { await browser.saveScreenshot(path.join(OUT, "mem-FAIL.png")); } } catch (_) {}
  } finally {
    try { if (browser) { await browser.deleteSession(); } } catch (_) {}
    stop(tauriDriver);
    for (const c of children) { stop(c); }
    h.reap();
    process.exit(code);
  }
}
main();
