// The CLI end to end: `new`, `build` and `test` on real Apps, through the real
// App host (`pollis-app-dev`, found in this repo's cargo target dir or via
// POLLIS_APP_DEV_BIN). Run `pnpm build` in sdk/ts first; `pnpm test` does.

import { after, before, describe, test } from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { cpSync, mkdirSync, mkdtempSync, readFileSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { buildApp, BuildError } from "../dist/build.js";
import { findDevTool } from "../dist/devtool.js";
import { newApp } from "../dist/new.js";
import { testApp, type TestApp } from "@pollis/app/testing";

const here = dirname(fileURLToPath(import.meta.url));
const sdk = join(here, "..", "..");
const main = join(here, "..", "dist", "main.js");
const scratch = mkdtempSync(join(tmpdir(), "pollis-cli-test-"));
after(() => rmSync(scratch, { recursive: true, force: true }));

/** Install the in-repo SDK into an App directory without the network. */
function linkSdk(dir: string): void {
  mkdirSync(join(dir, "node_modules", "@pollis"), { recursive: true });
  symlinkSync(join(sdk, "app"), join(dir, "node_modules", "@pollis", "app"), "dir");
  symlinkSync(join(sdk, "cli"), join(dir, "node_modules", "@pollis", "cli"), "dir");
}

function fixture(name: string, into: string): string {
  const dir = join(scratch, into, name);
  cpSync(join(here, "fixtures", name), dir, { recursive: true });
  linkSdk(dir);
  return dir;
}

function template(parent: string): string {
  mkdirSync(join(scratch, parent), { recursive: true });
  const dir = newApp({ name: "hello-bot", lang: "ts", parent: join(scratch, parent), sdk });
  linkSdk(dir);
  return dir;
}

describe("pollis app new + test", () => {
  test("the template builds and its own tests pass in the real sandbox", () => {
    const dir = template("e2e");
    const files = ["package.json", "README.md", ".gitignore", "tsconfig.json", "src/app.ts", "src/app.test.ts"];
    for (const f of files) {
      assert.ok(readFileSync(join(dir, f), "utf8").length > 0, f);
    }
    assert.match(readFileSync(join(dir, "README.md"), "utf8"), /^# hello-bot/);
    const run = spawnSync(process.execPath, [main, "app", "test", "--dir", dir], {
      encoding: "utf8",
      env: { ...process.env, POLLIS_APP_DEV_BIN: findDevTool() },
    });
    assert.equal(run.status, 0, run.stdout + run.stderr);
    assert.match(run.stdout, /pass 3/);
  });

  test("new refuses an invalid name and an unavailable language", () => {
    assert.throws(() => newApp({ name: "Hello Bot", lang: "ts", parent: scratch }), /not a valid App name/);
    assert.throws(() => newApp({ name: "x", lang: "go", parent: scratch }), /not available/);
  });

  test("dev and run say why they are not available yet", () => {
    for (const cmd of ["dev", "run"]) {
      const run = spawnSync(process.execPath, [main, "app", cmd], { encoding: "utf8" });
      assert.equal(run.status, 2);
      assert.match(run.stderr, /not available yet/);
    }
  });
});

describe("pollis app build", () => {
  test("is reproducible: the same source and lockfile give the same App hash, wherever it is built", async () => {
    const a = await buildApp({ dir: template("repro-a"), quiet: true });
    const b = await buildApp({ dir: template("deeper/repro-b"), quiet: true });
    const again = await buildApp({ dir: dirname(a.component).replace(/\/dist$/, ""), quiet: true });
    assert.match(a.appHash, /^[0-9a-f]{64}$/);
    assert.equal(a.appHash, b.appHash);
    assert.equal(a.appHash, again.appHash);
    assert.deepEqual(readFileSync(a.component), readFileSync(b.component));
  });

  test("emits the manifest from the code, with package.json defaults", async () => {
    const built = await buildApp({ dir: fixture("kitchen-sink", "manifest"), quiet: true });
    const written = JSON.parse(readFileSync(built.manifestPath, "utf8"));
    assert.deepEqual(written, {
      name: "kitchen-sink",
      version: "1.2.3",
      description: "Every handler and host function, for the CLI's tests.",
      triggers: ["messages", "mentions", "commands", "schedule", "webhook"],
      read: { channels: ["#support"], history: 7, events: ["created", "edited"] },
      actions: ["post", "reply", "react"],
      state: { quota: 1024 },
      egress: ["api.example.com"],
      commands: [
        { name: "count", description: "Count up" },
        { name: "ticket", description: "File a ticket", arg_hint: "<title>" },
        { name: "leak", description: "Try an undeclared host" },
        { name: "edit", description: "Try an undeclared action" },
        { name: "history", description: "Read history" },
        { name: "random", description: "Roll" },
        { name: "throw", description: "Fail" },
        { name: "other", description: "Falls through" },
      ],
      schedules: [{ name: "daily", every: 86400, offset: 32400 }],
    });
  });

  test("refuses Node built-ins at build, saying why", async () => {
    await assert.rejects(buildApp({ dir: fixture("uses-fs", "builtins"), quiet: true }), (e: Error) => {
      assert.ok(e instanceof BuildError);
      assert.match(e.message, /`node:fs` is a Node built-in, and Pollis Apps cannot use Node built-ins/);
      assert.match(e.message, /Apps have no files/);
      assert.match(e.message, /src\/app\.ts/);
      return true;
    });
  });
});

describe("npm dependencies", () => {
  test("resolve to their browser build, which needs no Node built-ins", async () => {
    const dir = template("deps");
    const pkg = join(dir, "node_modules", "dual");
    mkdirSync(pkg, { recursive: true });
    writeFileSync(
      join(pkg, "package.json"),
      JSON.stringify({ name: "dual", type: "module", exports: { browser: "./web.js", default: "./node.js" } }),
    );
    writeFileSync(join(pkg, "node.js"), 'import { randomUUID } from "node:crypto";\nexport const where = () => randomUUID() && "node";\n');
    writeFileSync(join(pkg, "web.js"), 'export const where = () => "web";\n');
    writeFileSync(
      join(dir, "src", "app.ts"),
      'import { app } from "@pollis/app";\nimport { where } from "dual";\n' +
        'export default app({ manifest: { read: { channels: ["*"] }, actions: ["reply"], commands: [{ name: "x", description: "X" }] },\n' +
        '  async onCommand(cmd, ctx) { await ctx.reply(cmd.message, where()); } });\n',
    );
    const built = await buildApp({ dir, quiet: true });
    const app = await testApp({ component: built.component, manifest: built.manifestPath, bin: findDevTool() });
    try {
      assert.deepEqual((await app.command("x")).replies, ["web"]);
    } finally {
      app.close();
    }
  });
});

describe("@pollis/app in the real sandbox", () => {
  let app: TestApp;
  before(async () => {
    const built = await buildApp({ dir: fixture("kitchen-sink", "sandbox"), quiet: true });
    app = await testApp({ component: built.component, manifest: built.manifestPath, bin: findDevTool() });
  });
  after(() => app.close());

  test("state round-trips and is written only by a job that succeeds", async () => {
    const first = await app.command("count");
    assert.deepEqual(first.replies, ["count is 1"]);
    assert.deepEqual(first.state, { count: "1" });
    const later = await app.command("count", "", { state: { count: "41" } });
    assert.deepEqual(later.replies, ["count is 42"]);
  });

  test("ctx.fetch is web-fetch-shaped and answered from fixtures", async () => {
    const result = await app.command("ticket", "Printer on fire", {
      http: [
        {
          url: "https://api.example.com/tickets",
          method: "POST",
          status: 201,
          headers: { "X-Kind": "bug" },
          body: { id: "T-7" },
        },
      ],
    });
    assert.ok(result.ok, result.error?.message);
    assert.deepEqual(result.replies, ["Filed T-7 (201, bug)"]);
    assert.equal(result.fetches[0].method, "POST");
    assert.equal(result.fetches[0].body, JSON.stringify({ title: "Printer on fire" }));
  });

  test("a host not in egress is refused with the runtime's own message", async () => {
    const result = await app.command("leak");
    assert.deepEqual(result.replies, ["refused: `http-fetch` to `evil.example.net` refused: not in `egress`"]);
    assert.equal(result.fetches.length, 0);
  });

  test("an action not in the manifest rejects, and the failed job sends nothing", async () => {
    const result = await app.command("edit");
    assert.equal(result.ok, false);
    assert.equal(result.error?.kind, "handler_failed");
    assert.match(result.error?.message ?? "", /`edit-own` refused: not in `actions`/);
    assert.deepEqual(result.actions, []);
  });

  test("a thrown error fails the job with its message", async () => {
    const result = await app.command("throw");
    assert.equal(result.ok, false);
    assert.match(result.error?.message ?? "", /something broke/);
  });

  test("messages are routed as the runtime routes them: declared command, then mention, then message", async () => {
    assert.deepEqual((await app.send("/count @test_bot")).replies, ["count is 1"]);
    assert.deepEqual((await app.send("/count @other_bot help")).replies, ["Try /ticket"]);
    assert.deepEqual((await app.send("/deploy help")).replies, ["Try /ticket"]);
    assert.deepEqual((await app.send("hey @Test_Bot help")).replies, ["you rang"]);
    assert.deepEqual((await app.send("@all help")).replies, ["Try /ticket"]);
    assert.deepEqual((await app.command("ticket", "@test_bot  Printer  ", {
      http: [{ url: "https://api.example.com/tickets", method: "POST", body: { id: "T-9" } }],
    })).fetches[0].body, JSON.stringify({ title: "Printer" }));
  });

  test("history comes from granted channels only", async () => {
    const result = await app.command("history", "", {
      messages: [
        { text: "first", channel: "support", sentAt: Date.UTC(2027, 0, 15, 7) },
        { text: "elsewhere", channel: "random", sentAt: Date.UTC(2027, 0, 15, 7) },
        { text: "too old", channel: "support", sentAt: Date.UTC(2026, 0, 1) },
      ],
    });
    assert.deepEqual(result.replies, ["first"]);
  });

  test("randomness is fresh per job, despite the reproducible build", async () => {
    const a = await app.command("random");
    const b = await app.command("random");
    const line = (r: typeof a) => r.logs.find((l) => l.text.startsWith("random "))?.text;
    assert.ok(line(a) && line(b));
    assert.notEqual(line(a), line(b));
    assert.deepEqual(a.reactions, ["🎲"]);
    assert.ok(a.logs.some((l) => l.text.includes("hello from console.log")));
  });

  test("messages, schedules and webhooks reach their handlers", async () => {
    assert.deepEqual((await app.message("I need help")).replies, ["Try /ticket"]);
    // Default slot: the latest at or before now (08:00): 09:00 UTC the day before.
    const scheduled = await app.schedule("daily");
    assert.ok(scheduled.logs.some((l) => l.text === "schedule daily at 2027-01-14T09:00:00.000Z now 1800000000000"), JSON.stringify(scheduled.logs));
    assert.equal((await app.schedule("weekly")).error?.kind, "not_delivered");
    const hook = await app.webhook({ text: "deploy finished", channel: "support" });
    assert.deepEqual(hook.posts, [{ channel: "support", text: "deploy finished" }]);
    assert.ok(hook.logs.some((l) => l.text === "webhook application/json"));
  });

  test("a channel the App was not granted never reaches it", async () => {
    const result = await app.message({ text: "help", channel: "random" });
    assert.equal(result.error?.kind, "not_delivered");
  });

  test("a narrower grant than the manifest is enforced", async () => {
    const result = await app.command("ticket", "x", {
      grants: { channels: { support: "support" }, actions: ["reply"], egress: [] },
      http: [{ url: "https://api.example.com/tickets", body: { id: "T-1" } }],
    });
    assert.equal(result.ok, false);
    assert.match(result.error?.message ?? "", /not in `egress`/);
  });
});
