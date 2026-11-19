// The CLI end to end for Apps written in Rust: `new`, `build`, `test`, `run` and
// `dev` on real Apps, through cargo and the real App host. Needs the
// wasm32-wasip2 target (the template's rust-toolchain.toml asks rustup for it).

import { after, describe, test } from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { chmodSync, cpSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { buildApp, BuildError } from "../dist/build.js";
import { findDevTool } from "../dist/devtool.js";
import { newApp } from "../dist/new.js";

const here = dirname(fileURLToPath(import.meta.url));
const sdk = join(here, "..", "..");
const main = join(here, "..", "dist", "main.js");
const scratch = mkdtempSync(join(tmpdir(), "pollis-cli-rust-test-"));
after(() => rmSync(scratch, { recursive: true, force: true }));

function template(parent: string): string {
  mkdirSync(join(scratch, parent), { recursive: true });
  return newApp({ name: "hello-bot", lang: "rust", parent: join(scratch, parent), sdk });
}

/** The template, with its source replaced by a fixture's. */
function kitchenSink(parent: string): string {
  const dir = template(parent);
  rmSync(join(dir, "tests"), { recursive: true });
  cpSync(join(here, "fixtures", "rust-kitchen-sink"), dir, { recursive: true });
  const cargo = readFileSync(join(dir, "Cargo.toml"), "utf8")
    .replace('name = "hello-bot"', 'name = "kitchen-sink"')
    .replace('version = "0.1.0"', 'version = "1.2.3"')
    .replace('description = "A Pollis App."', `description = "Every handler and host function, for the CLI's tests."`)
    .replace("[dependencies]\n", '[dependencies]\nserde_json = "1"\n');
  writeFileSync(join(dir, "Cargo.toml"), cargo);
  return dir;
}

function withSource(parent: string, source: string): string {
  const dir = template(parent);
  writeFileSync(join(dir, "src", "lib.rs"), source);
  return dir;
}

/** A stand-in `pollis-bot` that records its arguments and exits 0. */
function fakeBot(name: string): { bin: string; calls: () => string[] } {
  const dir = join(scratch, "bots", name);
  mkdirSync(dir, { recursive: true });
  const log = join(dir, "calls.log");
  const bin = join(dir, "pollis-bot");
  writeFileSync(bin, `#!/bin/sh\necho "$*" >> "${log}"\n`);
  chmodSync(bin, 0o755);
  return { bin, calls: () => (existsSync(log) ? readFileSync(log, "utf8").trim().split("\n") : []) };
}

function cli(args: string[], env: NodeJS.ProcessEnv = {}) {
  return spawnSync(process.execPath, [main, "app", ...args], {
    encoding: "utf8",
    env: { ...process.env, POLLIS_APP_DEV_BIN: findDevTool(), ...env },
  });
}

describe("pollis app new --lang rust", () => {
  test("the template builds and its own tests pass in the real sandbox", () => {
    const dir = template("e2e");
    for (const f of ["Cargo.toml", "README.md", ".gitignore", "rust-toolchain.toml", "src/lib.rs", "tests/hello.rs"]) {
      assert.ok(readFileSync(join(dir, f), "utf8").length > 0, f);
    }
    assert.match(readFileSync(join(dir, "README.md"), "utf8"), /^# hello-bot/);
    const run = cli(["test", "--dir", dir]);
    assert.equal(run.status, 0, run.stdout + run.stderr);
    assert.match(run.stdout, /test result: ok\. 3 passed/);
  });

  test("--sdk needs the Pollis repo's sdk/ts directory", () => {
    assert.throws(() => newApp({ name: "x", lang: "rust", parent: scratch, sdk: join(scratch, "nowhere") }), /expected the Pollis repo's sdk\/ts/);
  });

  test("without --sdk the App depends on the released crate", () => {
    mkdirSync(join(scratch, "released"), { recursive: true });
    const dir = newApp({ name: "hello-bot", lang: "rust", parent: join(scratch, "released") });
    assert.match(readFileSync(join(dir, "Cargo.toml"), "utf8"), /^pollis-app = "\^0\.1\.0"$/m);
    assert.equal(existsSync(join(dir, "vendor")), false);
  });

  test("run installs the build in pollis-bot, then runs it", () => {
    const dir = template("run");
    const { bin, calls } = fakeBot("run");
    const run = cli(["run", "--dir", dir, "--installation", "01I", "--data-dir", "/d"], { POLLIS_BOT_BIN: bin });
    assert.equal(run.status, 0, run.stdout + run.stderr);
    assert.deepEqual(calls(), [
      `--data-dir /d app install --installation 01I --artifact ${join(dir, "dist")}`,
      "--data-dir /d run",
    ]);
    assert.match(run.stdout, /App hash [0-9a-f]{64}/);
  });

  test("dev prints the developer key, signs each build and runs the development installation", () => {
    const dir = template("dev");
    const { bin, calls } = fakeBot("dev");
    const run = cli(["dev", "--dir", dir, "--installation", "01I"], { POLLIS_BOT_BIN: bin });
    assert.equal(run.status, 0, run.stdout + run.stderr);
    assert.deepEqual(calls(), [
      "app dev-key",
      `app sign --artifact ${join(dir, "dist")}`,
      `app install --installation 01I --artifact ${join(dir, "dist")}`,
      "run",
    ]);
    assert.match(run.stdout, /built and signed hello-bot/);
  });
});

describe("pollis app build, for Rust", () => {
  test("is reproducible: the same source and lockfile give the same App hash, wherever it is built", async () => {
    const first = template("repro-a");
    const a = await buildApp({ dir: first, quiet: true });
    const second = template("deeper/still/repro-b");
    cpSync(join(first, "Cargo.lock"), join(second, "Cargo.lock"));
    const b = await buildApp({ dir: second, quiet: true });
    rmSync(join(first, "target"), { recursive: true });
    const again = await buildApp({ dir: first, quiet: true });
    assert.match(a.appHash, /^[0-9a-f]{64}$/);
    assert.equal(a.appHash, b.appHash);
    assert.equal(a.appHash, again.appHash);
    assert.deepEqual(readFileSync(a.component), readFileSync(b.component));
  });

  test("a change to the source changes the App hash", async () => {
    const first = template("change-a");
    const a = await buildApp({ dir: first, quiet: true });
    const lib = join(first, "src", "lib.rs");
    writeFileSync(lib, readFileSync(lib, "utf8").replace("Hello, {who}!", "Howdy, {who}!"));
    const b = await buildApp({ dir: first, quiet: true });
    assert.notEqual(a.appHash, b.appHash);
  });

  test("a lockfile that does not match Cargo.toml is refused, not silently updated", async () => {
    const dir = template("stale-lock");
    await buildApp({ dir, quiet: true });
    const cargo = join(dir, "Cargo.toml");
    writeFileSync(cargo, readFileSync(cargo, "utf8").replace("[dependencies]\n", '[dependencies]\nitoa = "1"\n'));
    await assert.rejects(buildApp({ dir, quiet: true }), /Cargo\.lock does not match Cargo\.toml/);
  });

  test("emits the manifest from the code, with Cargo.toml defaults", async () => {
    const built = await buildApp({ dir: kitchenSink("manifest"), quiet: true });
    assert.deepEqual(JSON.parse(readFileSync(built.manifestPath, "utf8")), {
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
        { name: "panic", description: "Fail harder" },
        { name: "other", description: "Falls through" },
      ],
      schedules: [{ name: "daily", every: 86400, offset: 32400 }],
    });
  });

  test("a manifest mistake is refused at build, naming the Rust handler", async () => {
    const dir = withSource(
      "bad-manifest",
      `use pollis_app::{Command, Context, Manifest};
fn on_command(_: Command, _: &Context) -> pollis_app::Result { Ok(()) }
pollis_app::app! { manifest: Manifest::new().channels(["*"]), on_command: on_command }
`,
    );
    await assert.rejects(buildApp({ dir, quiet: true }), (e: Error) => {
      assert.ok(e instanceof BuildError);
      assert.match(e.message, /^manifest: `on_command` needs its commands declared/);
      return true;
    });
  });

  test("a component the App host would refuse is refused at build", async () => {
    const dir = withSource(
      "sockets",
      `use pollis_app::{Context, Manifest, Message};
fn on_message(_: Message, _: &Context) -> pollis_app::Result {
    std::net::TcpStream::connect("192.0.2.1:80")?;
    Ok(())
}
pollis_app::app! { manifest: Manifest::new().channels(["*"]), on_message: on_message }
`,
    );
    await assert.rejects(buildApp({ dir, quiet: true }), (e: Error) => {
      assert.ok(e instanceof BuildError);
      assert.match(e.message, /the App host refuses this build/);
      assert.match(e.message, /wasi:sockets/);
      return true;
    });
    assert.equal(existsSync(join(dir, "dist", "app.wasm")), false);
  });

  test("a compile error reaches the developer", async () => {
    const dir = withSource("broken", "this is not rust\n");
    await assert.rejects(buildApp({ dir, quiet: true }), /compiling the App failed/);
  });
});

describe("pollis_app in the real sandbox", () => {
  test("every handler and host function, through pollis_app::testing", () => {
    const run = cli(["test", "--dir", kitchenSink("sandbox")]);
    assert.equal(run.status, 0, run.stdout + run.stderr);
    assert.match(run.stdout, /test result: ok\. 12 passed/);
  });
});
