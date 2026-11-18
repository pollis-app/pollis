#!/usr/bin/env node
// Bots (#1182) end to end on a real stack: the real desktop app as the admin, the real
// `target/debug/pollis-bot` binary as the hosts, against a local DS (e2e/scripts/start-backend.sh,
// or sqld + DS + LiveKit per e2e/README.md). Covers the issue's "Done when":
//   1. pair a fresh host with one command and one typed code; Bot badge in the member list
//   2. `run --webhook`, then a curl (bearer and HMAC) posts into a channel with the Bot badge
//   3. a second host through the verified approval; revoke it; the revoked host can no longer post
//   4. a "reboot": kill the host, `run --once` from cold, then `run` again unattended, webhook posts
//   5. the admin (the last person) leaves the group: the bot's DS rows are gone, the host says Fix
//
// Each host gets its own HOME and --data-dir, so its keystore never touches the user's real one.

const crypto = require("crypto");
const fs = require("fs");
const path = require("path");
const { spawn, spawnSync } = require("child_process");
const h = require("./lib/harness");

const ARTIFACTS = process.env.BOTS_ARTIFACTS || path.join(__dirname, "artifacts", "bots-real");
const shot = h.makeShot(ARTIFACTS);
const BOT_BIN = path.join(h.ROOT, "target", "debug", "pollis-bot");
const WORK = path.join(__dirname, ".tmp-bots-real");
const PIN = "1357";
const GROUP = "Ops";
const CHANNEL = "alerts";
const HOOK1 = "127.0.0.1:18787";
const HOOK2 = "127.0.0.1:18788";

const log = (m) => console.log(`[bots-real] ${m}`);

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
    const logFile = path.join(ARTIFACTS, `${this.name}-${tag}.log`);
    fs.mkdirSync(ARTIFACTS, { recursive: true });
    const out = fs.createWriteStream(logFile);
    const p = spawn(BOT_BIN, ["--data-dir", this.dataDir, ...args], { env: this.env, stdio: ["ignore", "pipe", "pipe"] });
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
    try { rec.proc.kill(signal); } catch (_) {}
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
  // A DS on a local file (`TURSO_URL=file:<path>`): read it directly.
  if (tursoUrl.startsWith("file:")) {
    const py = "import json,sqlite3,sys; a=json.loads(sys.argv[3]); c=sqlite3.connect(sys.argv[1]); print(json.dumps([list(r) for r in c.execute(sys.argv[2], a)]))";
    const r = spawnSync("python3", ["-I", "-c", py, tursoUrl.slice(5), q, JSON.stringify(args.map(String))], { encoding: "utf8" });
    if (r.status !== 0) {
      throw new Error(`sql ${q}: ${r.stderr}`);
    }
    return JSON.parse(r.stdout);
  }
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

async function main() {
  h.reap();
  const deliveryUrl = process.env.POLLIS_DELIVERY_URL;
  if (!deliveryUrl) {
    throw new Error("POLLIS_DELIVERY_URL is not set: start the local stack first (e2e/README.md)");
  }
  if (!fs.existsSync(BOT_BIN)) {
    throw new Error(`${BOT_BIN} missing: cargo build -p pollis-bot`);
  }
  const turso = h.tursoEnv();
  const devEnv = h.readEnvFile(".env.development");
  fs.rmSync(ARTIFACTS, { recursive: true, force: true });
  fs.mkdirSync(WORK, { recursive: true });

  const children = [];
  const hostRecs = [];
  let browser;
  const results = {};
  try {
    const vite = h.spawnVite(devEnv);
    children.push(vite);
    await h.waitViteReady();

    const appEnv = appEnvFor(devEnv, turso, deliveryUrl, path.join(WORK, "admin-data"));
    const client = await h.startClient({ index: 0, appEnv, label: "admin" });
    children.push(client.tauriDriver);
    browser = client.browser;

    const email = `bots-admin-${Date.now()}@pollis.test`;
    await signUp(browser, email);
    await h.clickTestId(browser, "menu-item-groups");
    await h.clickTestId(browser, "menu-item-create-group");
    await h.waitTestId(browser, "create-group-page", 20000);
    await h.setSelectorValue(browser, "#create-group-name", GROUP);
    await h.clickTestId(browser, "create-group-submit-button");
    await h.waitTestId(browser, "menu-item-create-channel", 30000);
    await h.clickTestId(browser, "menu-item-create-channel");
    await h.waitTestId(browser, "create-channel-page", 20000);
    await h.setSelectorValue(browser, "#create-channel-name", CHANNEL);
    await h.clickTestId(browser, "create-channel-submit-button");
    await h.waitTestId(browser, "message-form", 30000);
    log("admin: group + channel created");

    // ── 1. pair ──────────────────────────────────────────────────────────────────────────────
    const h1 = new Host("host1", deliveryUrl);
    const pair1 = h1.spawn(["pair", "--name", "host-one"], "pair");
    hostRecs.push([h1, pair1]);
    const code1 = (await h1.waitFor(pair1, /Pairing code:\s+([A-Z0-9]{8})/, 60000, "the pairing code"))[1];
    log(`host1 printed code ${code1}`);
    await openGroup(browser);
    await h.clickTestId(browser, "menu-item-bots");
    await h.waitTestId(browser, "bots-page", 20000);
    await h.clickTestId(browser, "bots-add");
    await h.setTestIdValue(browser, "bot-name", "Alerts");
    await typeCodeInto(browser, code1);
    await shot(browser, "1-add-bot-code-typed.png");
    await h.clickTestId(browser, "bot-pair-submit");
    const paired = await h1.waitFor(pair1, /paired: bot (\S+) in group (\S+)\./, 120000, "paired");
    const botId = paired[1];
    const groupId = paired[2];
    const pairExit = await h1.exitOf(pair1, 30000, "pair");
    check(pairExit.code === 0, `pair exited 0 (bot ${botId})`);
    const inst1 = h1.installation();

    await openGroup(browser);
    await h.clickTestId(browser, "menu-item-members");
    await h.waitTestId(browser, `member-bot-badge-${botId}`, 60000);
    await shot(browser, "1-members-bot-badge.png");
    check(true, "Bot badge on the bot in the member list");
    check(await count(turso.TURSO_URL, "SELECT count(*) FROM users WHERE id = ? AND kind = 'bot'", [botId]) === 1, "DS users row kind=bot");
    results.pair = "pass";

    // ── 2. webhook ───────────────────────────────────────────────────────────────────────────
    let run1 = h1.spawn(["run", "--webhook", HOOK1], "run1");
    hostRecs.push([h1, run1]);
    const secret1 = (await h1.waitFor(run1, /Webhook secret for installation \S+ \(shown once[^\n]*\n\s+(\S+)/, 60000, "the webhook secret"))[1];
    await h1.waitFor(run1, /listening on/, 30000, "webhook listening");
    await h.sleep(3000);
    const r1 = await hook(HOOK1, inst1.installation_id, secret1, "disk 91% on db-1 (bearer)", "bearer");
    check(r1.status === 200 && r1.json.status === "posted", "bearer webhook call posted");
    const r2 = await hook(HOOK1, inst1.installation_id, secret1, "cpu 99% on db-2 (hmac)", "hmac");
    check(r2.status === 200 && r2.json.status === "posted", "HMAC webhook call posted");
    const bad = await hook(HOOK1, inst1.installation_id, "pbw_wrong", "nope", "bearer");
    check(bad.status === 401, "wrong secret refused 401");
    const badSig = await hook(HOOK1, inst1.installation_id, "pbw_wrong", "nope", "hmac");
    check(badSig.status === 401, "wrong HMAC refused 401");
    await openChannel(browser);
    const m1 = await waitMessage(browser, "disk 91% on db-1 (bearer)");
    const m2 = await waitMessage(browser, "cpu 99% on db-2 (hmac)");
    await shot(browser, "2-alerts-in-channel.png");
    check(m1.badge && m2.badge, "both alerts shown in the channel with the Bot badge");
    results.webhook = "pass";

    // ── 3. second host, then revoke it ───────────────────────────────────────────────────────
    const h2 = new Host("host2", deliveryUrl);
    const add2 = h2.spawn(["add-host", "--bot", botId, "--name", "host-two"], "add-host");
    hostRecs.push([h2, add2]);
    const code2 = (await h2.waitFor(add2, /Pairing code:\s+([A-Z0-9]{8})/, 60000, "the add-host code"))[1];
    log(`host2 printed code ${code2}`);
    await openBotPage(browser, botId);
    await h.clickTestId(browser, "bot-add-host");
    await typeCodeInto(browser, code2);
    await shot(browser, "3-add-host-code-typed.png");
    await h.clickTestId(browser, "bot-pair-submit");
    await h2.waitFor(add2, /paired: bot/, 180000, "add-host paired (host1 wraps the key)");
    check((await h2.exitOf(add2, 30000, "add-host")).code === 0, "add-host exited 0");
    check(/wrap/i.test(run1.text), "host1 logged the wrap");
    const inst2 = h2.installation();
    check(inst2.bot_user_id === botId && inst2.device_id !== inst1.device_id, `host2 is a second device (${inst2.device_id}) of the same bot`);

    let run2 = h2.spawn(["run", "--webhook", HOOK2], "run2");
    hostRecs.push([h2, run2]);
    const secret2 = (await h2.waitFor(run2, /Webhook secret for installation \S+ \(shown once[^\n]*\n\s+(\S+)/, 60000, "host2 webhook secret"))[1];
    await h2.waitFor(run2, /listening on/, 30000, "host2 webhook listening");
    await h.sleep(3000);
    const r3 = await settle(h2, run2, await hook(HOOK2, inst2.installation_id, secret2, "from host two", "bearer"));
    check(r3.status === 200 && r3.json.status === "posted", "host2 posts");
    // Same delivery id to both hosts: exactly one posts.
    const dId = `dup-${Date.now()}`;
    const [d1, d2] = await Promise.all([
      hook(HOOK1, inst1.installation_id, secret1, "dedup alert", "bearer", { "X-Pollis-Delivery": dId }),
      hook(HOOK2, inst2.installation_id, secret2, "dedup alert", "bearer", { "X-Pollis-Delivery": dId }),
    ]);
    // A job past 30 s answers 202 and still finishes: read each host's outcome from its log then.
    const outcome = async (host, rec, d) => {
      if (d.json.status !== "accepted") {
        return d.json.status;
      }
      const m = await host.waitFor(rec, new RegExp(`hook:${dId}: (posted|handled by another host)`), 180000, `the outcome of ${dId}`);
      return m[1] === "posted" ? "posted" : "claimed_elsewhere";
    };
    const tDedup = Date.now();
    const statuses = [await outcome(h1, run1, d1), await outcome(h2, run2, d2)].sort();
    log(`dedup outcomes ${JSON.stringify(statuses)} (answers ${d1.status}/${d2.status}, settled in ${Date.now() - tDedup} ms after the answers)`);
    check(statuses[0] === "claimed_elsewhere" && statuses[1] === "posted", "same delivery id to both hosts: exactly one posted");

    await openChannel(browser);
    await waitMessage(browser, "from host two");
    await waitMessage(browser, "dedup alert");
    const dedupCount = await browser.execute(() =>
      [...document.querySelectorAll('[data-testid="message-content"]')].filter((n) => (n.textContent || "").includes("dedup alert")).length);
    check(dedupCount === 1, `dedup alert appears once in the channel (saw ${dedupCount})`);
    await shot(browser, "3-two-hosts-posted.png");

    await openBotPage(browser, botId);
    await h.waitTestId(browser, `bot-host-revoke-${inst2.device_id}`, 30000);
    await shot(browser, "3-bot-page-two-hosts.png");
    await h.clickTestId(browser, `bot-host-revoke-${inst2.device_id}`);
    await h.waitTestId(browser, "revoke-host-submit", 10000);
    await h.clickTestId(browser, "revoke-host-submit");
    await browser.waitUntil(async () => !(await h.present(browser, `bot-host-revoke-${inst2.device_id}`)), { timeout: 30000, timeoutMsg: "revoked host still listed" });
    await shot(browser, "3-host2-revoked.png");
    check(await count(turso.TURSO_URL, "SELECT count(*) FROM user_device WHERE device_id = ? AND revoked_at IS NOT NULL", [inst2.device_id]) === 1
      || await count(turso.TURSO_URL, "SELECT count(*) FROM user_device WHERE device_id = ?", [inst2.device_id]) === 0,
      "DS: host2's device is revoked");
    await h.sleep(5000);
    const r4 = await settle(h2, run2, await hook(HOOK2, inst2.installation_id, secret2, "after revoke from host two", "bearer"));
    check(r4.json.status !== "posted", `revoked host2 cannot post (status ${r4.status} ${r4.json.status})`);
    h2.kill(run2);
    // Cold start of the revoked host: it cannot open (no decrypt), and says how to fix it.
    const once2 = h2.spawn(["run", "--once"], "once-after-revoke");
    const once2Exit = await h2.exitOf(once2, 120000, "run --once after revoke");
    check(once2Exit.code !== 0 && /Fix:/.test(once2.text), "revoked host2 `run --once` fails with a Fix line");
    const r5 = await settle(h1, run1, await hook(HOOK1, inst1.installation_id, secret1, "host one still posts after the revoke", "bearer"));
    check(r5.json.status === "posted", "host1 still posts after host2's revoke");
    await openChannel(browser);
    await waitMessage(browser, "host one still posts after the revoke");
    const leaked = await browser.execute(() =>
      [...document.querySelectorAll('[data-testid="message-content"]')].some((n) => (n.textContent || "").includes("after revoke from host two")));
    check(!leaked, "nothing from the revoked host reached the channel");
    results.hosts = "pass";

    // ── 4. reboot ────────────────────────────────────────────────────────────────────────────
    h1.kill(run1);
    await run1.exited;
    log("host1 killed (simulated reboot)");
    const once1 = h1.spawn(["run", "--once"], "once-cold");
    const once1Exit = await h1.exitOf(once1, 120000, "run --once");
    check(once1Exit.code === 0 && /one pass done/.test(once1.text), "`run --once` from cold, no PIN, exits 0");
    check(!/PIN/i.test(once1.text), "no PIN asked");
    run1 = h1.spawn(["run", "--webhook", HOOK1], "run1-after-reboot");
    hostRecs.push([h1, run1]);
    await h1.waitFor(run1, /listening on/, 60000, "webhook listening after reboot");
    check(!/Webhook secret for installation/.test(run1.text), "secret not re-printed (kept in the keystore)");
    await h.sleep(3000);
    const r6 = await settle(h1, run1, await hook(HOOK1, inst1.installation_id, secret1, "after reboot", "hmac"));
    check(r6.json.status === "posted", "webhook posts after the reboot with the old secret");
    await openChannel(browser);
    await waitMessage(browser, "after reboot");
    await shot(browser, "4-after-reboot.png");
    results.reboot = "pass";

    // ── 5. the group goes, the bot goes ──────────────────────────────────────────────────────
    await openGroup(browser);
    await h.clickTestId(browser, "menu-item-leave-group");
    await h.waitTestId(browser, "leave-group-confirm", 15000);
    await h.clickTestId(browser, "leave-group-confirm");
    const end = Date.now() + 60000;
    let rows;
    while (Date.now() < end) {
      rows = {
        users: await count(turso.TURSO_URL, "SELECT count(*) FROM users WHERE id = ?", [botId]),
        bot: await count(turso.TURSO_URL, "SELECT count(*) FROM bot WHERE user_id = ?", [botId]),
        devices: await count(turso.TURSO_URL, "SELECT count(*) FROM user_device WHERE user_id = ?", [botId]),
        groups: await count(turso.TURSO_URL, "SELECT count(*) FROM groups WHERE id = ?", [groupId]),
        keyPackages: await count(turso.TURSO_URL, "SELECT count(*) FROM mls_key_package WHERE user_id = ?", [botId]).catch(() => -1),
      };
      if (rows.users === 0 && rows.bot === 0 && rows.devices === 0 && rows.groups === 0) {
        break;
      }
      await h.sleep(1000);
    }
    log(`DS rows after leaving: ${JSON.stringify(rows)}`);
    check(rows.users === 0 && rows.bot === 0 && rows.devices === 0 && rows.groups === 0 && rows.keyPackages <= 0, "group, bot user, bot row, devices and key packages gone");
    await shot(browser, "5-group-left.png");
    const r7 = await settle(h1, run1, await hook(HOOK1, inst1.installation_id, secret1, "after delete", "bearer"));
    check(r7.json.status !== "posted", `the deleted bot cannot post (status ${r7.status} ${r7.json.status})`);
    h1.kill(run1);
    const once3 = h1.spawn(["run", "--once"], "once-after-delete");
    const once3Exit = await h1.exitOf(once3, 120000, "run --once after delete");
    const fixLine = (once3.text.match(/[^\n]*Fix:[^\n]*/) || [""])[0];
    log(`host1 after delete: ${fixLine}`);
    check(once3Exit.code !== 0 && /forget --installation/.test(once3.text), "host1 reports a clear Fix line (forget the installation)");
    results.delete = "pass";

    log(`RESULT ${JSON.stringify(results)}`);
    log("PASS");
  } catch (e) {
    log(`RESULT ${JSON.stringify(results)}`);
    console.error(`[bots-real] FAIL: ${e.stack || e}`);
    if (browser) {
      await h.dumpFailure(browser, ARTIFACTS, shot).catch(() => {});
    }
    process.exitCode = 1;
  } finally {
    for (const [host, rec] of hostRecs) {
      host.kill(rec);
    }
    if (browser) {
      await browser.deleteSession().catch(() => {});
    }
    for (const c of children) {
      try { c.kill("SIGKILL"); } catch (_) {}
    }
    h.reap();
  }
}

main();
