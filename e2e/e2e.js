#!/usr/bin/env node
// E2E driver for the real desktop app via raw webdriverio (the mocha runner stalls webkit2gtk).

const fs = require("fs");
const path = require("path");
const { spawn, spawnSync } = require("child_process");
const { remote } = require("webdriverio");
const h = require("./lib/harness");

const ARTIFACTS = path.join(__dirname, "artifacts");
const shot = h.makeShot(ARTIFACTS);

async function main() {
  h.reap();
  // .env.development is optional: absent in CI, where creds come from the workflow env.
  const devEnv = h.readEnvFile(".env.development");
  const { TURSO_URL, TURSO_TOKEN } = h.tursoEnv();

  const children = [];
  const stop = (c) => { try { c && c.kill("SIGKILL"); } catch (_) {} };

  const vite = h.spawnVite(devEnv);
  children.push(vite);

  // External DS when POLLIS_DELIVERY_URL is set (CI), else spawn our own on DS_PORT.
  const deliveryUrl = process.env.POLLIS_DELIVERY_URL || h.DS_URL;
  if (!process.env.POLLIS_DELIVERY_URL) {
    const dsEnv = { ...process.env, TURSO_URL, TURSO_TOKEN,
      PORT: String(h.DS_PORT), DEV_OTP: "000000", RUST_LOG: "pollis_delivery=info" };
    delete dsEnv.RESEND_API_KEY; delete dsEnv.LOG_DB_URL; delete dsEnv.LOG_DB_TOKEN; delete dsEnv.LOG_DB_ADMIN_TOKEN;
    console.log(`[e2e] starting delivery service on ${deliveryUrl} (DEV_OTP=000000)`);
    const ds = spawn(h.DS_BIN, [], { env: dsEnv, stdio: ["ignore", "inherit", "inherit"] });
    children.push(ds);
  } else {
    console.log(`[e2e] using external delivery service at ${deliveryUrl}`);
  }

  // The WebKit vars mirror `pnpm dev`: WebKitGTK compositing stalls screenshots without them.
  const appEnv = { ...devEnv, ...process.env, TURSO_URL, TURSO_TOKEN,
    POLLIS_DELIVERY_URL: deliveryUrl, POLLIS_DATA_DIR: path.join(__dirname, ".tmp-data"),
    WEBKIT_DISABLE_COMPOSITING_MODE: "1", GDK_BACKEND: "x11" };
  fs.rmSync(appEnv.POLLIS_DATA_DIR, { recursive: true, force: true });
  fs.mkdirSync(appEnv.POLLIS_DATA_DIR, { recursive: true });

  let tauriDriver;
  let browser;
  let code = 1;
  try {
    await h.waitViteReady();
    await h.waitPort(h.DS_PORT, ["127.0.0.1", "::1"], 20000);
    console.log("[e2e] delivery service up");

    // tauri-driver → WebKitWebDriver → app (inherits appEnv).
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

    // Let the initial Vite page load settle before the first command.
    await h.sleep(6000);

    const email = `e2e_${Date.now()}@pollis.test`;
    const PIN = "1357";
    console.log(`[e2e] account: ${email}`);

    // 1. Auth screen.
    await h.waitTestId(browser, "auth-screen", 30000);
    await shot(browser, "01-auth-screen.png");

    // 2. Email → request OTP.
    await h.setTestIdValue(browser, "email-input", email);
    await h.clickTestId(browser, "send-otp-button");
    await h.waitTestId(browser, "otp-form-container", 20000);
    console.log("[e2e] email submitted, OTP form shown");

    // 3. Dev OTP 000000 (auto-submits) → new-account secret-key flow.
    await h.typeCode(browser, "000000");
    await h.waitTestId(browser, "save-secret-key-warning-screen", 45000);
    console.log("[e2e] OTP verified, account bootstrapped");

    // 4. Save-secret-key dance.
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
    console.log("[e2e] secret key confirmed");

    // 5. Create PIN (enter + confirm, each auto-advances/submits).
    await h.waitTestId(browser, "pin-create-screen");
    await h.typeCode(browser, PIN);
    await h.typeCode(browser, PIN);
    console.log("[e2e] PIN created");

    // 6. Ready app → proof.
    await h.waitTestId(browser, "app-ready", 60000);
    await shot(browser, "99-app-ready.png");
    console.log("[e2e] SUCCESS: reached app-ready");
    code = 0;
  } catch (err) {
    console.error("[e2e] FAILED:", err.message);
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

main().catch((e) => { console.error("[e2e] fatal:", e); h.reap(); process.exit(1); });
