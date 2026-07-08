#!/usr/bin/env node
/*
 * E2E: a wrong OTP code surfaces an inline error and does not let the user
 * past the code-entry screen. Same local stack as e2e.js (needs the
 * delivery service + writable test Turso — see e2e/README.md), but only
 * drives the auth screen, not the full signup flow.
 *
 * Flow: launch → email → request OTP → enter a WRONG 6-digit code →
 * assert [data-testid="auth-error"] appears and otp-form-container is still
 * shown (i.e. verification was rejected, not silently accepted).
 */

const fs = require("fs");
const path = require("path");
const { spawn } = require("child_process");
const dotenv = require("dotenv");
const { remote } = require("webdriverio");
const h = require("./lib/harness");

const ARTIFACTS = path.join(__dirname, "artifacts");
const shot = h.makeShot(ARTIFACTS);

async function main() {
  h.reap();
  const devEnv = dotenv.parse(fs.readFileSync(path.join(h.ROOT, ".env.development")));
  const testEnv = dotenv.parse(fs.readFileSync(path.join(h.ROOT, ".env.test")));
  if (!testEnv.TURSO_URL || !testEnv.TURSO_TOKEN) {
    throw new Error(".env.test missing TURSO_URL/TOKEN (need a writable disposable DB)");
  }

  const children = [];
  const stop = (c) => { try { c && c.kill("SIGKILL"); } catch (_) {} };

  const vite = h.spawnVite(devEnv);
  children.push(vite);

  // Local delivery service. DEV_OTP is the code that WOULD verify — the
  // whole point of this test is to send something else instead.
  const dsEnv = { ...process.env, TURSO_URL: testEnv.TURSO_URL, TURSO_TOKEN: testEnv.TURSO_TOKEN,
    PORT: String(h.DS_PORT), DEV_OTP: "000000", RUST_LOG: "pollis_delivery=info" };
  delete dsEnv.RESEND_API_KEY; delete dsEnv.LOG_DB_URL; delete dsEnv.LOG_DB_TOKEN; delete dsEnv.LOG_DB_ADMIN_TOKEN;
  console.log(`[invalid-otp] starting delivery service on ${h.DS_URL} (DEV_OTP=000000)`);
  const ds = spawn(h.DS_BIN, [], { env: dsEnv, stdio: ["ignore", "inherit", "inherit"] });
  children.push(ds);

  const appEnv = { ...process.env, ...devEnv, TURSO_URL: testEnv.TURSO_URL, TURSO_TOKEN: testEnv.TURSO_TOKEN,
    POLLIS_DELIVERY_URL: h.DS_URL, POLLIS_DATA_DIR: path.join(__dirname, ".tmp-data-invalid-otp"),
    WEBKIT_DISABLE_COMPOSITING_MODE: "1", GDK_BACKEND: "x11" };
  fs.rmSync(appEnv.POLLIS_DATA_DIR, { recursive: true, force: true });
  fs.mkdirSync(appEnv.POLLIS_DATA_DIR, { recursive: true });

  let tauriDriver;
  let browser;
  let code = 1;
  try {
    await h.waitViteReady();
    await h.waitPort(h.DS_PORT, ["127.0.0.1", "::1"], 20000);
    console.log("[invalid-otp] delivery service up");

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

    const email = `e2e_bad_otp_${Date.now()}@pollis.test`;
    console.log(`[invalid-otp] account: ${email}`);

    await h.waitTestId(browser, "auth-screen", 30000);
    await h.setTestIdValue(browser, "email-input", email);
    await h.clickTestId(browser, "send-otp-button");
    await h.waitTestId(browser, "otp-form-container", 20000);
    console.log("[invalid-otp] email submitted, OTP form shown");

    // Wrong code — DEV_OTP is 000000, this is neither that nor a real code.
    await h.typeCode(browser, "111111");
    await h.waitTestId(browser, "auth-error", 20000);
    const errText = (await (await browser.$('[data-testid="auth-error"]')).getText()).trim();
    if (!errText) {
      throw new Error("auth-error testid present but empty");
    }
    console.log(`[invalid-otp] rejected as expected: "${errText}"`);

    if (!(await h.present(browser, "otp-form-container"))) {
      throw new Error("otp-form-container vanished — wrong code should not advance past OTP entry");
    }
    await shot(browser, "invalid-otp-error.png");
    console.log("[invalid-otp] SUCCESS: wrong code rejected, still on OTP screen");
    code = 0;
  } catch (err) {
    console.error("[invalid-otp] FAILED:", err.message);
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

main().catch((e) => { console.error("[invalid-otp] fatal:", e); h.reap(); process.exit(1); });
