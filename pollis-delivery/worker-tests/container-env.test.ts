// A container a rollout restarted with the previous start's environment is restarted with the
// deployed one (#1611: POLLIS_CLIENT_POLICY_URL never reached api-dev's container).

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { ENV_FINGERPRINT_KEY, envFingerprint, needsRestart } from "../worker/container-env.ts";

test("the fingerprint ignores key order and changes with any value or key", async () => {
  const a = await envFingerprint({ PORT: "8788", X: "1" });
  assert.equal(a, await envFingerprint({ X: "1", PORT: "8788" }));
  assert.notEqual(a, await envFingerprint({ PORT: "8788", X: "2" }));
  assert.notEqual(a, await envFingerprint({ PORT: "8788", X: "1", POLLIS_CLIENT_POLICY_URL: "u" }));
  assert.match(a, /^[0-9a-f]{64}$/);
});

test("the fingerprint never contains a value", async () => {
  const secret = "a-secret-value-that-must-not-be-stored";
  assert.ok(!(await envFingerprint({ TURSO_TOKEN: secret })).includes(secret));
});

test("only a running container with another (or no) recorded environment restarts", () => {
  assert.equal(needsRestart(true, "abc", "abc"), false);
  assert.equal(needsRestart(true, "old", "new"), true);
  // Started before fingerprints existed, or by a rollout: restart once.
  assert.equal(needsRestart(true, undefined, "new"), true);
  // Not running: the next start passes the current environment anyway.
  assert.equal(needsRestart(false, "old", "new"), false);
});

test("the Worker records each start's environment and reconciles on its first request", () => {
  const src = readFileSync(new URL("../worker/index.ts", import.meta.url), "utf8");
  assert.ok(src.includes("this.ctx.storage.put(ENV_FINGERPRINT_KEY, await envFingerprint(envVars))"));
  assert.ok(src.includes("this.envReconciled ??= this.reconcileEnv();"));
  // Reconciled before the request reaches the container.
  assert.ok(
    src.indexOf("await this.envReconciled;") < src.lastIndexOf("return super.fetch(new Request(request, { headers }));"),
  );
  assert.equal(ENV_FINGERPRINT_KEY, "container-env-fingerprint");
});
