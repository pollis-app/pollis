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

const fs = require("fs");
const path = require("path");
const h = require("./lib/harness");

const ARTIFACTS = process.env.BOTS_ARTIFACTS || path.join(__dirname, "artifacts", "bots-real");
const { BOT_BIN, createBotsLab } = require("./lib/bots-lab");
const WORK = path.join(__dirname, ".tmp-bots-real");
const GROUP = "Ops";
const CHANNEL = "alerts";
const HOOK1 = "127.0.0.1:18787";
const HOOK2 = "127.0.0.1:18788";
// The published build of the alerts App (apps/alerts), the one an admin approves.
const ALERTS_APP = path.join(h.ROOT, "pollis-bot", "tests", "fixtures", "alerts-app");
const ALERTS_HASH = fs.readFileSync(path.join(h.ROOT, "apps", "alerts", "APP_HASH"), "utf8").trim();

const {
  log, shot, check, appEnvFor, Host, hook, settle, sql, count, signUp, openGroup, openChannel, typeCodeInto,
  openBotPage, waitMessage, waitPending, approvePending,
} = createBotsLab({ tag: "bots-real", work: WORK, artifacts: ARTIFACTS, group: GROUP, channel: CHANNEL });

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
    const tPair = Date.now();
    log("admin: submitted the bot code");
    const paired = await h1.waitFor(pair1, /paired: bot (\S+) in group (\S+)\./, 120000, "paired");
    log(`timing: bot code submitted -> host paired in ${Date.now() - tPair} ms`);
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
    // A bot with no App has nothing to run, and says what to do.
    const noApp = h1.spawn(["run", "--once"], "run-without-an-app");
    const noAppExit = await h1.exitOf(noApp, 60000, "run with no App");
    check(noAppExit.code !== 0 && /app install/.test(noApp.text), "a bot with no App refuses to run and names `app install`");
    const install1 = h1.spawn(["app", "install", "--installation", inst1.installation_id, "--artifact", ALERTS_APP], "install-alerts");
    check((await h1.exitOf(install1, 120000, "app install")).code === 0, "the alerts App installs on host1");
    check(install1.text.includes(ALERTS_HASH), "app install prints the published App hash");
    let run1 = h1.spawn(["run", "--webhook", HOOK1], "run1");
    hostRecs.push([h1, run1]);
    const secret1 = (await h1.waitFor(run1, /Webhook secret for installation \S+ \(shown once[^\n]*\n\s+(\S+)/, 60000, "the webhook secret"))[1];
    await h1.waitFor(run1, /listening on/, 30000, "webhook listening");
    // First-party or not, it runs only once an admin approves it.
    await waitPending(browser, botId);
    await approvePending(browser, "2-alerts-app", { development: false, autoUpdate: false });
    await h1.waitFor(run1, new RegExp(`running App ${ALERTS_HASH}`), 120000, "host1 running the approved alerts App");
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
    const tAdd = Date.now();
    log("admin: submitted the host code");
    await h2.waitFor(add2, /paired: bot/, 180000, "add-host paired (host1 wraps the key)");
    log(`timing: host code submitted -> second host paired in ${Date.now() - tAdd} ms`);
    check((await h2.exitOf(add2, 30000, "add-host")).code === 0, "add-host exited 0");
    check(/wrap/i.test(run1.text), "host1 logged the wrap");
    const inst2 = h2.installation();
    check(inst2.bot_user_id === botId && inst2.device_id !== inst1.device_id, `host2 is a second device (${inst2.device_id}) of the same bot`);
    // The page the admin left open lists the new host without a reload (the DS's bots_changed nudge).
    await h.waitTestId(browser, "bot-hosts", 15000);
    await h.waitTestId(browser, `bot-host-revoke-${inst2.device_id}`, 60000);
    await shot(browser, "3-host2-listed-live.png");
    check(true, "the open bot page lists host2 without a reload");

    const install2 = h2.spawn(["app", "install", "--installation", inst2.installation_id, "--artifact", ALERTS_APP], "install-alerts");
    check((await h2.exitOf(install2, 120000, "app install on host2")).code === 0, "the alerts App installs on host2");
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
    // The confirmation stands in for the host list until the revoke lands.
    await browser.waitUntil(
      async () => !(await h.present(browser, "revoke-host-confirm")) && !(await h.present(browser, `bot-host-revoke-${inst2.device_id}`)),
      { timeout: 60000, timeoutMsg: "revoke never landed, or the revoked host is still listed" },
    );
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
