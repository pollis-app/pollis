#!/usr/bin/env node
// The bots milestone (#2006, epic #872) on a real stack: the real desktop app as admin and member,
// the real `pollis-bot` as the host, the real `pollis app` CLI (sdk/ts/cli) driving it.
//   1. `pollis app new` (TypeScript) → edit it to declare /ping → `pollis app test`
//   2. `pollis app dev --installation <id>`: the admin approves the developer key on the bot's page;
//      a member picks /ping from the composer's `/` list and the App replies; saving a source file
//      rebuilds and the next /ping answers with the new code
//   3. `pollis app build` → `pollis app run`: the admin approves the printed App hash (development
//      off, updates approved automatically); /ping answers
//   4. an update that widens (a new egress host) does not run until an admin approves it again
//   5. a non-widening update from the same source runs by itself, with no admin acting
// Needs the local stack (e2e/README.md), `cargo build -p pollis -p pollis-bot -p pollis-app-dev`
// and the SDK built (`cd sdk/ts && pnpm install && pnpm build`).

const fs = require("fs");
const os = require("os");
const path = require("path");
const h = require("./lib/harness");
const { BOT_BIN, createBotsLab } = require("./lib/bots-lab");

const ARTIFACTS = process.env.BOTS_ARTIFACTS || path.join(__dirname, "artifacts", "bots-milestone");
const WORK = path.join(__dirname, ".tmp-bots-milestone");
// The App lives outside the repo, as a developer's would (and outside its pnpm workspace).
const APP_PARENT = path.join(os.tmpdir(), "pollis-bots-milestone");
const APP_DIR = path.join(APP_PARENT, "milestone-bot");
const CLI = path.join(h.ROOT, "sdk", "ts", "cli", "dist", "main.js");
const SDK = path.join(h.ROOT, "sdk", "ts");
const APP_DEV_BIN = path.join(h.ROOT, "target", "debug", "pollis-app-dev");
const GROUP = "Milestone";
const CHANNEL = "lab";
const EGRESS = "api.example.com";
// Set once the bot is paired; read by the composer diagnostics.
const GROUP_ID = { value: null };

const {
  log, shot, check, appEnvFor, Host, count, signUp, openGroup, openChannel, typeCodeInto, openBotPage, waitMessage, waitPending, approvePending,
} = createBotsLab({ tag: "bots-milestone", work: WORK, artifacts: ARTIFACTS, group: GROUP, channel: CHANNEL });

// ── the App's source ─────────────────────────────────────────────────────────────────────────

const appFile = (rel) => path.join(APP_DIR, rel);

// Declare /ping beside the template's /hello, answering `pong <tag>: <args>`.
function declarePing(tag) {
  let src = fs.readFileSync(appFile("src/app.ts"), "utf8");
  src = src.replace(
    'commands: [{ name: "hello", description: "Say hello", arg_hint: "[name]" }],',
    'commands: [\n      { name: "hello", description: "Say hello", arg_hint: "[name]" },\n' +
      '      { name: "ping", description: "Answer with pong", arg_hint: "[text]" },\n    ],',
  );
  src = src.replace(
    '    if (cmd.name !== "hello") {',
    `    if (cmd.name === "ping") {\n      await ctx.reply(cmd.message, \`pong ${tag}: \${cmd.args.trim()}\`);\n      return;\n    }\n    if (cmd.name !== "hello") {`,
  );
  if (!src.includes('name: "ping"') || !src.includes("pong ")) {
    throw new Error("the template changed: could not declare /ping in src/app.ts");
  }
  fs.writeFileSync(appFile("src/app.ts"), src);
  fs.appendFileSync(
    appFile("src/app.test.ts"),
    `\ntest("/ping answers pong", async () => {\n  const result = await app.command("ping", "x");\n  assert.ok(result.ok, result.error?.message);\n  assert.equal(result.replies.length, 1);\n  assert.match(result.replies[0], /^pong .*: x$/);\n});\n`,
  );
}

// A new reply tag: changes the code only (no widening).
function retag(from, to) {
  const src = fs.readFileSync(appFile("src/app.ts"), "utf8");
  if (!src.includes(`pong ${from}:`)) {
    throw new Error(`src/app.ts does not answer "pong ${from}:"`);
  }
  fs.writeFileSync(appFile("src/app.ts"), src.replace(`pong ${from}:`, `pong ${to}:`));
}

function addEgress(host) {
  const src = fs.readFileSync(appFile("src/app.ts"), "utf8");
  const next = src.replace("    actions: [\"reply\"],", `    actions: ["reply"],\n    egress: ["${host}"],`);
  if (next === src) {
    throw new Error("could not add egress to src/app.ts");
  }
  fs.writeFileSync(appFile("src/app.ts"), next);
}

function bumpVersion(version) {
  const pkg = JSON.parse(fs.readFileSync(appFile("package.json"), "utf8"));
  pkg.version = version;
  fs.writeFileSync(appFile("package.json"), JSON.stringify(pkg, null, 2) + "\n");
}

// ── the composer's / list ────────────────────────────────────────────────────────────────────

// Type "/pi", pick /ping from the list with Enter, add `args`, send with Enter.
async function sendCommand(browser, command, args) {
  await h.waitTestId(browser, "message-input");
  const input = await browser.$('[data-testid="message-input"]');
  const prefix = `/${command.slice(0, 2)}`;
  for (let attempt = 1; ; attempt++) {
    await input.click();
    await browser.execute(() => document.querySelector('[data-testid="message-input"]').focus());
    const typing = browser.action("key");
    for (const ch of prefix) {
      typing.down(ch).up(ch);
    }
    await typing.perform();
    const ok = await browser.waitUntil(async () => (await input.getAttribute("data-value")) === prefix, { timeout: 5000 }).catch(() => false);
    if (ok) {
      break;
    }
    const where = await browser.execute(() => {
      const a = document.activeElement;
      return a ? `${a.tagName} ${a.getAttribute("data-testid") || ""}` : "none";
    });
    log(`composer holds "${await input.getAttribute("data-value")}" after typing ${prefix} (focus: ${where}), attempt ${attempt}`);
    await shot(browser, `composer-miss-${attempt}.png`);
    if (attempt >= 3) {
      throw new Error(`the composer never took "${prefix}"`);
    }
    await browser.keys(["Escape"]);
  }
  const listed = await h.waitTestId(browser, "command-suggest-list", 20000).then(() => true, () => false);
  if (!listed) {
    const direct = await h.invokeApp(browser, "list_group_commands", { groupId: GROUP_ID.value }).catch((e) => `error: ${e.message}`);
    log(`no / list after typing ${prefix}; list_group_commands now answers ${JSON.stringify(direct)}`);
    await shot(browser, `no-command-list-${command}.png`);
    throw new Error(`typing ${prefix} showed no command list`);
  }
  await h.waitSelector(browser, `[data-testid^="command-option-${command}-"]`, 20000, `the /${command} option`);
  await shot(browser, `command-list-${command}.png`);
  await browser.keys("Enter");
  await browser.waitUntil(
    async () => ((await input.getAttribute("data-value")) || "").startsWith(`/${command} `),
    { timeout: 10000, timeoutMsg: `picking /${command} did not fill the composer` },
  );
  const rest = browser.action("key");
  for (const ch of args) {
    rest.down(ch).up(ch);
  }
  await rest.perform();
  await browser.waitUntil(
    async () => ((await input.getAttribute("data-value")) || "").endsWith(args),
    { timeout: 10000, timeoutMsg: "the arguments never reached the composer" },
  );
  const sent = await input.getAttribute("data-value");
  await browser.keys("Enter");
  await browser.waitUntil(async () => ((await input.getAttribute("data-value")) || "") === "", {
    timeout: 15000, timeoutMsg: "the command was not sent",
  });
  log(`member sent "${sent}"`);
  return sent;
}

// A message from the bot containing `text` that carries the Bot badge.
async function expectBotReply(browser, text, timeoutMs = 120000) {
  const found = await waitMessage(browser, text, timeoutMs);
  check(found.badge, `the App replied "${text}" with the Bot badge`);
}

async function messagePresent(browser, text) {
  return browser.execute((tok) => [...document.querySelectorAll('[data-testid="message-content"]')]
    .some((n) => (n.textContent || "").includes(tok)), text);
}

async function main() {
  h.reap();
  const deliveryUrl = process.env.POLLIS_DELIVERY_URL;
  if (!deliveryUrl) {
    throw new Error("POLLIS_DELIVERY_URL is not set: start the local stack first (e2e/README.md)");
  }
  for (const [bin, how] of [[BOT_BIN, "cargo build -p pollis-bot"], [APP_DEV_BIN, "cargo build -p pollis-app-dev"], [CLI, "cd sdk/ts && pnpm install && pnpm build"]]) {
    if (!fs.existsSync(bin)) {
      throw new Error(`${bin} missing: ${how}`);
    }
  }
  const turso = h.tursoEnv();
  const devEnv = h.readEnvFile(".env.development");
  fs.rmSync(ARTIFACTS, { recursive: true, force: true });
  fs.mkdirSync(WORK, { recursive: true });
  fs.rmSync(APP_PARENT, { recursive: true, force: true });
  fs.mkdirSync(APP_PARENT, { recursive: true });

  const children = [];
  const recs = [];
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

    await signUp(browser, `milestone-admin-${Date.now()}@pollis.test`);
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

    // The bot this App runs in, paired as in bots-real.js.
    const host = new Host("host1", deliveryUrl);
    host.env.POLLIS_BOT_BIN = BOT_BIN;
    host.env.POLLIS_APP_DEV_BIN = APP_DEV_BIN;
    const pair = host.spawn(["pair", "--name", "dev-laptop"], "pair");
    recs.push([host, pair]);
    const code = (await host.waitFor(pair, /Pairing code:\s+([A-Z0-9]{8})/, 60000, "the pairing code"))[1];
    await openGroup(browser);
    await h.clickTestId(browser, "menu-item-bots");
    await h.waitTestId(browser, "bots-page", 20000);
    await h.clickTestId(browser, "bots-add");
    await h.setTestIdValue(browser, "bot-name", "Milestone");
    await typeCodeInto(browser, code);
    await h.clickTestId(browser, "bot-pair-submit");
    const paired = await host.waitFor(pair, /paired: bot (\S+) in group (\S+)\./, 180000, "paired");
    const botId = paired[1];
    GROUP_ID.value = paired[2];
    check((await host.exitOf(pair, 30000, "pair")).code === 0, `bot ${botId} paired`);
    const installation = host.installation().installation_id;

    const cli = (args, tag) => {
      const rec = host.exec("node", [CLI, "app", ...args], tag, { cwd: fs.existsSync(APP_DIR) ? APP_DIR : APP_PARENT });
      recs.push([host, rec]);
      return rec;
    };
    const cliOk = async (args, tag, timeoutMs = 300000) => {
      const rec = cli(args, tag);
      const exit = await host.exitOf(rec, timeoutMs, `pollis app ${args[0]}`);
      check(exit.code === 0, `pollis app ${args.join(" ")} exited 0`);
      return rec;
    };
    const pnpmInstall = async () => {
      const rec = host.exec("pnpm", ["install", "--ignore-workspace"], "pnpm-install", { cwd: APP_DIR });
      const exit = await host.exitOf(rec, 300000, "pnpm install");
      check(exit.code === 0, "pnpm install in the new App");
    };

    // ── 1. new, edit, test ──────────────────────────────────────────────────────────────────
    await cliOk(["new", "milestone-bot", "--lang", "ts", "--sdk", SDK], "new");
    check(fs.existsSync(appFile("src/app.ts")), "pollis app new wrote src/app.ts");
    await pnpmInstall();
    declarePing("dev1");
    const tested = await cliOk(["test"], "test");
    check(/pass 4/.test(tested.text), "pollis app test: the template's 3 tests and /ping pass");
    results.newEditTest = "pass";

    // ── 2. dev with live rebuilds ───────────────────────────────────────────────────────────
    const dev = cli(["dev", "--installation", installation, "--data-dir", host.dataDir], "dev");
    await host.waitFor(dev, /built and signed milestone-bot .*\(App hash ([0-9a-f]+)/, 300000, "the first dev build");
    await host.waitFor(dev, /watching src/, 60000, "the dev watcher");
    await waitPending(browser, botId);
    check(await h.present(browser, "bot-app-pending-summary-developer-key"), "the page offers the developer key");
    await approvePending(browser, "2-dev-key", { development: true });
    await openChannel(browser);
    await sendCommand(browser, "ping", "one");
    await expectBotReply(browser, "pong dev1: one");
    await shot(browser, "2-dev-reply.png");

    const builds = (dev.text.match(/built and signed/g) || []).length;
    retag("dev1", "dev2");
    await host.waitFor(dev, new RegExp(`(?:[\\s\\S]*?built and signed){${builds + 1}}`), 300000, "the rebuild on save");
    log("dev: rebuilt on save");
    await h.sleep(2000);
    await sendCommand(browser, "ping", "two");
    await expectBotReply(browser, "pong dev2: two");
    await shot(browser, "2-dev-live-rebuild-reply.png");
    host.kill(dev, "SIGINT");
    await host.exitOf(dev, 30000, "pollis app dev").catch(() => host.kill(dev));
    results.dev = "pass";

    // ── 3. build, run, approve the hash ─────────────────────────────────────────────────────
    retag("dev2", "v1");
    bumpVersion("1.0.0");
    const built = await cliOk(["build"], "build-v1");
    const v1 = (built.text.match(/App hash:?\s*([0-9a-f]{64})/) || [])[1];
    check(!!v1, `pollis app build printed the App hash ${v1}`);
    let run = cli(["run", "--installation", installation, "--data-dir", host.dataDir], "run-v1");
    await host.waitFor(run, new RegExp(`App hash:\\s*${v1}`), 120000, "pollis-bot app install printing the hash");
    await waitPending(browser, botId);
    const shown = await (await browser.$('[data-testid="bot-app-pending-summary-hash"]')).getText();
    check(shown.replace(/\s/g, "").includes(v1.slice(0, 12)), `the page shows the printed hash (${shown.trim()})`);
    await approvePending(browser, "3-v1", { development: false, autoUpdate: true });
    await host.waitFor(run, new RegExp(`running App ${v1}`), 120000, "the runtime running v1");
    await openChannel(browser);
    await sendCommand(browser, "ping", "three");
    await expectBotReply(browser, "pong v1: three");
    await shot(browser, "3-v1-reply.png");
    results.buildRunApprove = "pass";

    // ── 4. a widening update waits for an admin ─────────────────────────────────────────────
    retag("v1", "v2");
    addEgress(EGRESS);
    bumpVersion("2.0.0");
    const built2 = await cliOk(["build"], "build-v2");
    const v2 = (built2.text.match(/App hash:?\s*([0-9a-f]{64})/) || [])[1];
    await sendCommand(browser, "ping", "four");
    await h.sleep(30000);
    check(!(await messagePresent(browser, "pong v2: four")) && !(await messagePresent(browser, "pong v1: four")),
      "the widened build did not run (no answer to /ping four)");
    check(!new RegExp(`running App ${v2}`).test(run.text), "the runtime never ran the widened build unapproved");
    await waitPending(browser, botId);
    check(await h.present(browser, `bot-app-change-egress:added:${EGRESS}`), `the page highlights the new host ${EGRESS}`);
    await shot(browser, "4-widening-pending.png");
    await approvePending(browser, "4-v2", { autoUpdate: true, accept: [`egress:added:${EGRESS}`] });
    await host.waitFor(run, new RegExp(`running App ${v2}`), 120000, "the runtime running v2 after approval");
    await openChannel(browser);
    await sendCommand(browser, "ping", "five");
    await expectBotReply(browser, "pong v2: five");
    results.wideningWaits = "pass";

    // ── 5. a non-widening update from the same source runs by itself ────────────────────────
    const approvalsBefore = await count(turso.TURSO_URL, "SELECT count(*) FROM bot_app_approval WHERE installation_id = ?", [installation]).catch(() => -1);
    retag("v2", "v3");
    bumpVersion("2.1.0");
    const built3 = await cliOk(["build"], "build-v3");
    const v3 = (built3.text.match(/App hash:?\s*([0-9a-f]{64})/) || [])[1];
    await sendCommand(browser, "ping", "six");
    await expectBotReply(browser, "pong v3: six");
    check(new RegExp(`running App ${v3}`).test(run.text), "the runtime runs v3 with no admin acting");
    const approvalsAfter = await count(turso.TURSO_URL, "SELECT count(*) FROM bot_app_approval WHERE installation_id = ?", [installation]).catch(() => -1);
    check(approvalsAfter === approvalsBefore, `no new approval was signed for v3 (${approvalsBefore} → ${approvalsAfter})`);
    await openBotPage(browser, botId);
    await h.waitTestId(browser, "bot-app-running-hash", 60000);
    await shot(browser, "5-auto-updated.png");
    results.autoUpdate = "pass";

    log(`RESULT ${JSON.stringify(results)}`);
    log("PASS");
  } catch (e) {
    log(`RESULT ${JSON.stringify(results)}`);
    console.error(`[bots-milestone] FAIL: ${e.stack || e}`);
    if (browser) {
      await h.dumpFailure(browser, ARTIFACTS, shot).catch(() => {});
    }
    process.exitCode = 1;
  } finally {
    for (const [host, rec] of recs) {
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

// Children it started may hold pipes open; the scenario is over either way.
main().finally(() => process.exit(process.exitCode || 0));
