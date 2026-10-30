// A container a rollout restarted with the previous start's environment is restarted with the
// deployed one (#1611: POLLIS_CLIENT_POLICY_URL never reached api-dev's container), and a check
// that found nothing running is repeated on the next request (#1624).

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { ENV_FINGERPRINT_KEY, envFingerprint, envVerdict } from "../worker/container-env.ts";

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
  assert.equal(envVerdict(true, "abc", "abc"), "settled");
  assert.equal(envVerdict(true, "old", "new"), "restart");
  // Started before fingerprints existed, or by a rollout: restart.
  assert.equal(envVerdict(true, undefined, "new"), "restart");
});

test("a container that is not running settles nothing (#1624)", () => {
  // The rollout may still start the replacement with the previous environment, so a deploy's
  // first request finding nothing running must not end the checks for this object.
  assert.equal(envVerdict(false, "old", "new"), "recheck");
  assert.equal(envVerdict(false, undefined, "new"), "recheck");
  assert.equal(envVerdict(false, "abc", "abc"), "recheck");
});

test("the Worker records each start's environment and reconciles until settled", () => {
  const src = readFileSync(new URL("../worker/index.ts", import.meta.url), "utf8");
  assert.ok(src.includes("this.ctx.storage.put(ENV_FINGERPRINT_KEY, await envFingerprint(envVars))"));
  // Only a "settled" verdict stops the checks; recheck, restart and errors leave them on.
  assert.ok(src.includes('return verdict === "settled";'));
  assert.ok(src.includes("if (await this.envCheck) {\n      this.envSettled = true;"));
  assert.ok(!src.includes("envReconciled ??="));
  // Reconciled before the request reaches the container.
  assert.ok(
    src.indexOf("await this.ensureEnvReconciled();") <
      src.lastIndexOf("return super.fetch(new Request(request, { headers }));"),
  );
  assert.equal(ENV_FINGERPRINT_KEY, "container-env-fingerprint");
});
