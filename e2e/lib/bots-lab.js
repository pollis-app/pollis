// Shared plumbing for the bots real-binary scenarios (#1182, #2006): pollis-bot hosts, the DS's
// rows, the desktop admin's steps. Each scenario gets its own work dir, artifacts dir, group and channel.

const crypto = require("crypto");
const fs = require("fs");
const path = require("path");
const { spawn } = require("child_process");
const h = require("./harness");

const BOT_BIN = path.join(h.ROOT, "target", "debug", "pollis-bot");
const PIN = "1357";

function createBotsLab({ tag, work, artifacts, group, channel }) {
  const WORK = work;
  const ARTIFACTS = artifacts;
  const GROUP = group;
  const CHANNEL = channel;
  const shot = h.makeShot(ARTIFACTS);
  const log = (m) => console.log(`[${tag}] ${new Date().toISOString().slice(11, 23)} ${m}`);

  function appEnvFor(devEnv, turso, deliveryUrl, dataDir) {
    fs.rmSync(dataDir, { recursive: true, force: true });
    fs.mkdirSync(dataDir, { recursive: true });
    return {
      ...devEnv, ...process.env,
      TURSO_URL: turso.TURSO_URL, TURSO_TOKEN: turso.TURSO_TOKEN,
      POLLIS_DELIVERY_URL: deliveryUrl,
      POLLIS_DATA_DIR: dataDir,
      R2_S3_ENDPOINT: process.env.R2_S3_ENDPOINT || "http://127.0.0.1:9/r2",
      R2_PUBLIC_URL: process.env.R2_PUBLIC_URL || "http://127.0.0.1:9/r2-public",
      LOG_DB_URL: "", LOG_DB_TOKEN: "", LOG_DB_ADMIN_TOKEN: "",
      WEBKIT_DISABLE_COMPOSITING_MODE: "1", GDK_BACKEND: "x11",
    };
  }

  // ── pollis-bot hosts ─────────────────────────────────────────────────────────────────────────

  class Host {
    constructor(name, deliveryUrl) {
      this.name = name;
      this.home = path.join(WORK, name, "home");
      this.dataDir = path.join(WORK, name, "data");
      fs.rmSync(path.join(WORK, name), { recursive: true, force: true });
      fs.mkdirSync(this.home, { recursive: true });
      fs.mkdirSync(this.dataDir, { recursive: true });
      const env = { ...process.env };
      delete env.POLLIS_DATA_DIR;
      delete env.POLLIS_BOT_DATA_DIR;
      this.env = {
        ...env,
        HOME: this.home,
        XDG_DATA_HOME: path.join(this.home, ".local", "share"),
        POLLIS_DELIVERY_URL: deliveryUrl,
        R2_S3_ENDPOINT: "", R2_PUBLIC_URL: "",
        LIVEKIT_URL: process.env.LIVEKIT_URL || "",
      };
      this.proc = null;
      this.runs = 0;
    }

    // Start a subcommand; collects stdout/stderr and tees them to a log file.
    spawn(args, tag) {
      return this.exec(BOT_BIN, ["--data-dir", this.dataDir, ...args], tag);
    }

    // Any command in this host's environment (the `pollis app` CLI, which drives pollis-bot).
    exec(cmd, args, tag, { cwd } = {}) {
      const logFile = path.join(ARTIFACTS, `${this.name}-${tag}.log`);
      fs.mkdirSync(ARTIFACTS, { recursive: true });
      const out = fs.createWriteStream(logFile);
      // Its own process group, so a kill reaches what it started (the CLI runs pollis-bot).
      const p = spawn(cmd, args, { env: this.env, cwd, detached: true, stdio: ["ignore", "pipe", "pipe"] });
      const rec = { proc: p, text: "", logFile, exit: null };
      const take = (chunk) => {
        const s = chunk.toString();
        rec.text += s;
        out.write(s);
        for (const line of s.split("\n").filter(Boolean)) {
          console.log(`  [${this.name}:${tag}] ${line}`);
        }
      };
      p.stdout.on("data", take);
      p.stderr.on("data", take);
      rec.exited = new Promise((res) => p.on("exit", (code, sig) => { rec.exit = { code, sig }; out.end(); res(rec.exit); }));
      return rec;
    }

    async waitFor(rec, re, timeoutMs, what) {
      const end = Date.now() + timeoutMs;
      while (Date.now() < end) {
        const m = rec.text.match(re);
        if (m) {
          return m;
        }
        if (rec.exit) {
          throw new Error(`${this.name}: exited (${JSON.stringify(rec.exit)}) before ${what}`);
        }
        await h.sleep(200);
      }
      throw new Error(`${this.name}: timed out waiting for ${what}`);
    }

    async exitOf(rec, timeoutMs, what) {
      const r = await Promise.race([rec.exited, h.sleep(timeoutMs).then(() => null)]);
      if (!r) {
        throw new Error(`${this.name}: ${what} did not exit within ${timeoutMs} ms`);
      }
      return r;
    }

    installation() {
      const root = path.join(this.dataDir, "installations");
      const ids = fs.readdirSync(root);
      if (ids.length !== 1) {
        throw new Error(`${this.name}: expected one installation, found ${ids.length}`);
      }
      return JSON.parse(fs.readFileSync(path.join(root, ids[0], "installation.json"), "utf8"));
    }

    kill(rec, signal = "SIGKILL") {
      try { process.kill(-rec.proc.pid, signal); } catch (_) {
        try { rec.proc.kill(signal); } catch (_) {}
      }
    }
  }

  // POST to a host's webhook; `mode` is "bearer" or "hmac".
  async function hook(bind, installationId, secret, text, mode, extraHeaders = {}) {
    const body = JSON.stringify({ channel: CHANNEL, text });
    const headers = { "Content-Type": "application/json", ...extraHeaders };
    if (mode === "bearer") {
      headers.Authorization = `Bearer ${secret}`;
    } else if (mode === "hmac") {
      headers["X-Pollis-Signature"] = "sha256=" + crypto.createHmac("sha256", secret).update(body).digest("hex");
    }
    const t0 = Date.now();
    const res = await fetch(`http://${bind}/hooks/${installationId}`, { method: "POST", headers, body });
    const json = await res.json().catch(() => ({}));
    log(`webhook ${bind} (${mode}) "${text}" -> ${res.status} ${JSON.stringify(json)} in ${Date.now() - t0} ms`);
    return { status: res.status, json };
  }


  // A webhook job past 30 s answers 202 and still finishes; settle it from the host's log.
  async function settle(host, rec, r) {
    if (r.json.status !== "accepted") {
      return r;
    }
    const id = r.json.delivery_id;
    const t0 = Date.now();
    const m = await host.waitFor(rec, new RegExp(`hook:${id}: ([^\\n]*)`), 180000, `the outcome of ${id}`);
    const status = m[1].startsWith("posted") ? "posted" : m[1].startsWith("handled by another host") ? "claimed_elsewhere" : `failed: ${m[1]}`;
    log(`  settled ${id} after ${Date.now() - t0} ms more: ${status}`);
    return { status: r.status, json: { ...r.json, status } };
  }

  // ── DS rows (the local sqld) ─────────────────────────────────────────────────────────────────

  async function sql(tursoUrl, q, args = []) {
    const res = await fetch(`${tursoUrl}/v2/pipeline`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ requests: [
        { type: "execute", stmt: { sql: q, args: args.map((v) => ({ type: "text", value: String(v) })) } },
        { type: "close" },
      ] }),
    });
    const json = await res.json();
    const r = json.results[0];
    if (r.type !== "ok") {
      throw new Error(`sql ${q}: ${JSON.stringify(r)}`);
    }
    return r.response.result.rows.map((row) => row.map((c) => c.value));
  }

  async function count(tursoUrl, q, args) {
    return Number((await sql(tursoUrl, q, args))[0][0]);
  }

  // ── desktop admin ────────────────────────────────────────────────────────────────────────────

  async function signUp(browser, email) {
    log(`admin: signing up ${email}`);
    await h.waitTestId(browser, "auth-screen", 30000);
    await h.setTestIdValue(browser, "email-input", email);
    await h.clickTestId(browser, "send-otp-button");
    await h.waitTestId(browser, "otp-form-container", 20000);
    await h.typeCode(browser, "000000");
    await h.waitTestId(browser, "save-secret-key-warning-screen", 45000);
    await h.clickTestId(browser, "save-secret-key-acknowledge-button");
    await h.waitTestId(browser, "save-secret-key-screen");
    const secretKey = (await (await browser.$('[data-testid="secret-key-display"]')).getText()).trim();
    await h.clickTestId(browser, "secret-key-saved-button");
    await h.waitTestId(browser, "save-secret-key-confirm-screen");
    await h.setTestIdValue(browser, "secret-key-confirm-input", secretKey);
    await h.clickTestId(browser, "confirm-secret-key-button");
    await h.waitTestId(browser, "pin-create-screen");
    await h.typeCode(browser, PIN);
    await h.typeCode(browser, PIN);
    await h.waitAppReadyAfterSignUp(browser, 60000);
    log("admin: app-ready");
  }

  async function clickByPrefixText(browser, prefix, text) {
    const ok = await browser.execute((pfx, needle) => {
      for (const el of document.querySelectorAll(`[data-testid^="${pfx}"]`)) {
        if ((el.textContent || "").includes(needle)) { el.click(); return true; }
      }
      return false;
    }, prefix, text);
    if (!ok) {
      throw new Error(`no ${prefix}* containing "${text}"`);
    }
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

  async function openGroup(browser) {
    await goHome(browser);
    await h.clickTestId(browser, "menu-item-groups");
    await h.waitTestId(browser, "menu-item-create-group", 15000);
    await clickByPrefixText(browser, "group-option-", GROUP);
    await h.waitTestId(browser, "menu-item-bots", 20000);
  }

  async function openChannel(browser) {
    await openGroup(browser);
    await h.waitSelector(browser, '[data-testid^="channel-option-"]', 20000, "a channel row");
    await clickByPrefixText(browser, "channel-option-", CHANNEL);
    await h.waitTestId(browser, "message-form", 20000);
  }

  async function typeCodeInto(browser, code) {
    await h.setTestIdValue(browser, "bot-pair-code", code);
    await browser.waitUntil(
      async () => (await (await browser.$('[data-testid="bot-pair-code"]')).getValue()) === code,
      { timeout: 10000, timeoutMsg: "code field never held the typed code" },
    );
  }

  async function openBotPage(browser, botId) {
    await openGroup(browser);
    await h.clickTestId(browser, "menu-item-bots");
    await h.waitTestId(browser, "bots-page", 20000);
    // NavigableList puts the testid on the row and the handler on its pressable inner cell.
    await h.clickSelector(browser, `[data-testid="bot-row-${botId}"] [style*="cursor"]`);
    await h.waitTestId(browser, "bot-hosts", 20000);
  }

  // ── the admin's App section ──────────────────────────────────────────────────────────────

  async function waitPending(browser, botId, timeoutMs = 120000) {
    const end = Date.now() + timeoutMs;
    while (Date.now() < end) {
      await openBotPage(browser, botId);
      await h.waitTestId(browser, "bot-app", 20000);
      if (await h.present(browser, "bot-app-pending")) {
        return;
      }
      await h.sleep(4000);
    }
    throw new Error("the bot's page never showed an App waiting for approval");
  }

  async function switchOn(browser, testId, on) {
    await h.waitTestId(browser, testId);
    const checked = await browser.execute((id) => {
      const el = document.querySelector(`[data-testid="${id}"]`);
      const box = el && (el.matches("input,button,[role=switch],[role=checkbox]") ? el : el.querySelector("input,button,[role=switch],[role=checkbox]"));
      if (!box) {
        return null;
      }
      return box.getAttribute("aria-checked") === "true" || box.checked === true;
    }, testId);
    if (checked === null) {
      throw new Error(`${testId}: no switch inside`);
    }
    if (checked !== on) {
      await h.clickTestId(browser, testId);
    }
  }

  async function approvePending(browser, label, { development = false, autoUpdate = false, accept = [] } = {}) {
    await h.waitTestId(browser, "bot-app-pending", 20000);
    await switchOn(browser, "bot-app-development", development);
    if (!development && (await h.present(browser, "bot-app-auto-update"))) {
      await switchOn(browser, "bot-app-auto-update", autoUpdate);
    }
    // Grants: the channel this test talks in, and whatever the update needs switched on.
    const channelSwitch = await browser.execute(() => {
      const el = document.querySelector('[data-testid^="bot-app-pending-grants-channel-"]');
      return el ? el.getAttribute("data-testid") : null;
    });
    if (channelSwitch) {
      await switchOn(browser, channelSwitch, true);
    }
    for (const key of accept) {
      await switchOn(browser, `bot-app-accept-${key}`, true);
    }
    await shot(browser, `${label}-before-approve.png`);
    await h.clickTestId(browser, "bot-app-approve");
    await browser.waitUntil(async () => !(await h.present(browser, "bot-app-pending")) || (await h.present(browser, "bot-app-approve-error")), {
      timeout: 60000, timeoutMsg: "the approval never landed",
    });
    if (await h.present(browser, "bot-app-approve-error")) {
      const text = await (await browser.$('[data-testid="bot-app-approve-error"]')).getText();
      throw new Error(`approving failed: ${text}`);
    }
    await shot(browser, `${label}-approved.png`);
    log(`admin approved (${label})`);
  }

  // Wait for a channel message containing `token`; returns whether its row carries the Bot badge.
  async function waitMessage(browser, token, timeoutMs = 90000) {
    const end = Date.now() + timeoutMs;
    while (Date.now() < end) {
      const found = await browser.execute((tok) => {
        for (const n of document.querySelectorAll('[data-testid="message-content"]')) {
          if ((n.textContent || "").includes(tok)) {
            let row = n;
            for (let i = 0; i < 8 && row; i++) {
              if (row.querySelector('[data-testid="message-bot-badge"]')) {
                return { badge: true };
              }
              row = row.parentElement;
            }
            return { badge: false };
          }
        }
        return null;
      }, token);
      if (found) {
        return found;
      }
      await h.sleep(2000);
    }
    throw new Error(`message "${token}" never appeared in #${CHANNEL}`);
  }

  function check(cond, msg) {
    if (!cond) {
      throw new Error(`CHECK FAILED: ${msg}`);
    }
    log(`ok: ${msg}`);
  }

  return {
    log, shot, check, appEnvFor, Host, hook, settle, sql, count, signUp, clickByPrefixText, goHome,
    openGroup, openChannel, typeCodeInto, openBotPage, waitMessage, waitPending, approvePending,
  };
}

module.exports = { BOT_BIN, PIN, createBotsLab };
