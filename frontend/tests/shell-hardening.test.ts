/*
 * The webview's own privileges, pinned.
 *
 * Three of the shell's defences are single lines of configuration that nothing
 * else in the build would notice going missing:
 *
 *   1. `security.csp` was `null`, i.e. the renderer ran with NO content policy
 *      at all — any injected markup could load and execute a remote script and
 *      talk to any origin it liked.
 *   2. `assetProtocol.enable` was `true`, a second scheme that reads native
 *      files, scoped separately from (and unknown to) the path scope that gates
 *      the four path-taking commands.
 *   3. `dialog:default` let the renderer invoke `plugin:dialog|open` itself, so
 *      a picked path never passed through Rust and could not be recorded.
 *      `pick_open_paths` / `pick_save_path` replace it precisely so that it can
 *      be — see `src-tauri/src/pathscope.rs`.
 *
 * Each of these is a config key. A test is the only thing that can notice one
 * being loosened again, so this file asserts them directly.
 *
 *   node --test frontend/tests/
 */

import test from "node:test";
import assert from "node:assert/strict";
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

const REPO = fileURLToPath(new URL("../..", import.meta.url));
const SRC = fileURLToPath(new URL("../src", import.meta.url));

function readJson(relative: string): Record<string, unknown> {
  return JSON.parse(readFileSync(join(REPO, relative), "utf8"));
}

function tauriSecurity(): Record<string, unknown> {
  const conf = readJson("src-tauri/tauri.conf.json") as {
    app: { security: Record<string, unknown> };
  };
  return conf.app.security;
}

function walk(dir: string): string[] {
  const out: string[] = [];
  for (const entry of readdirSync(dir)) {
    const full = join(dir, entry);
    if (statSync(full).isDirectory()) {
      out.push(...walk(full));
    } else if (/\.(ts|tsx)$/.test(entry)) {
      out.push(full);
    }
  }
  return out;
}

test("the webview runs under a real content security policy", () => {
  const csp = tauriSecurity().csp as Record<string, string> | null;
  assert.ok(
    csp && typeof csp === "object",
    "app.security.csp must be a directive map, not null — null means no policy at all",
  );

  // default-src has to be restrictive, because every directive not named below
  // falls back to it.
  assert.equal(csp["default-src"], "'self'");

  // The two that decide whether injected markup can run code.
  assert.equal(
    csp["script-src"],
    "'self'",
    "script-src must be exactly 'self' — the Vite bundle has no inline script, " +
      "and Tauri adds its own nonce for the IPC bootstrap",
  );
  assert.equal(csp["object-src"], "'none'");
  assert.equal(csp["frame-src"], "'none'");

  for (const directive of ["script-src", "default-src", "connect-src"]) {
    const value = csp[directive] ?? "";
    for (const escape of ["'unsafe-eval'", "'unsafe-inline'", "*"]) {
      assert.ok(
        !value.split(/\s+/).includes(escape),
        `${directive} must not contain ${escape} (got: ${value})`,
      );
    }
  }

  // The renderer's real network surface: the IPC and the loopback media/frame
  // servers. Anything else it needs goes through a Rust command.
  const connect = csp["connect-src"] ?? "";
  for (const source of ["ipc:", "http://ipc.localhost", "http://127.0.0.1:*", "ws://127.0.0.1:*"]) {
    assert.ok(connect.includes(source), `connect-src must allow ${source}`);
  }
});

test("the asset protocol is off, in the config and in the cargo features", () => {
  const assetProtocol = tauriSecurity().assetProtocol as { enable?: boolean };
  assert.equal(
    assetProtocol?.enable,
    false,
    "assetProtocol reads native files on a scope of its own, bypassing " +
      "src-tauri/src/pathscope.rs — it must stay disabled",
  );

  const cargo = readFileSync(join(REPO, "src-tauri/Cargo.toml"), "utf8");
  const tauriDep = cargo
    .split("\n")
    .find((line) => line.startsWith("tauri = {"));
  assert.ok(tauriDep, "the tauri dependency line must exist");
  assert.ok(
    !tauriDep.includes("protocol-asset"),
    "the protocol-asset cargo feature must go with the disabled config",
  );

  const bridge = readFileSync(join(SRC, "bridge.ts"), "utf8");
  assert.ok(
    !bridge.includes("export { getVersion, relaunch, exit, convertFileSrc }"),
    "convertFileSrc mints asset:// URLs and must not be re-exported once the " +
      "protocol is off",
  );
});

test("the renderer cannot reach the dialog plugin directly", () => {
  const capabilities = readJson("src-tauri/capabilities/default.json") as {
    permissions: string[];
  };
  const dialogPermissions = capabilities.permissions.filter((p) =>
    p.startsWith("dialog:"),
  );
  assert.deepEqual(
    dialogPermissions,
    [],
    "the picker is driven from Rust (pick_open_paths / pick_save_path) so the " +
      "chosen path can be recorded before the renderer sees it; granting the " +
      "renderer dialog:* again would give it a path Rust never witnessed",
  );

  for (const file of walk(SRC)) {
    const source = readFileSync(file, "utf8");
    assert.ok(
      !source.includes("plugin:dialog|"),
      `${file} invokes the dialog plugin directly; use bridge/dialog.ts`,
    );
    assert.ok(
      !source.includes("@tauri-apps/plugin-dialog"),
      `${file} imports the dialog plugin directly; use bridge/dialog.ts`,
    );
  }
});
