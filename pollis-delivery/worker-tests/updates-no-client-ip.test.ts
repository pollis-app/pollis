/*
 * No plain client IPs readable anywhere (the hard privacy rule, the same one
 * pollis-delivery/tests/no_client_ip_exposure.rs enforces for the DS), for the
 * mobile OTA update routes the DS front-door Worker serves itself (#1250).
 *
 * Pinned: both DS wrangler configs keep Workers Logs and Logpush off and add
 * no tail consumer; the updates module logs nothing and reads no
 * client-identifying data; and index.ts answers /updates/ before — never
 * through — the container forward, so an update request never reaches the
 * DS process at all.
 *
 * index.ts itself legitimately reads CF-Connecting-IP (to replace it with a
 * keyed bucket before forwarding); that path is covered by the Rust test.
 *
 *   node --test pollis-delivery/worker-tests/*.test.ts
 */

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");

function jsonc(path: string): Record<string, unknown> {
  const raw = readFileSync(path, "utf8");
  // Strip // comments that are not inside strings, then parse.
  const stripped = raw.replace(/("(?:[^"\\]|\\.)*")|\/\/[^\n]*/g, (_m, str: string | undefined) => str ?? "");
  return JSON.parse(stripped);
}

function code(path: string): string {
  return readFileSync(path, "utf8")
    .split("\n")
    .filter((line) => !line.trim().startsWith("//"))
    .join("\n");
}

for (const env of ["dev", "prod"]) {
  test(`wrangler.${env}.jsonc: Workers Logs and Logpush are explicitly off`, () => {
    const config = jsonc(join(root, `wrangler.${env}.jsonc`));
    assert.deepEqual(config.observability, { enabled: false });
    assert.equal(config.logpush, false);
    // Tail consumers receive every request.
    assert.equal(config.tail_consumers, undefined);
  });
}

test("only prod binds the update bucket; both carry the one channel (inert on dev)", () => {
  const prod = jsonc(join(root, "wrangler.prod.jsonc"));
  assert.deepEqual(prod.r2_buckets, [{ binding: "UPDATES", bucket_name: "pollis" }]);
  assert.equal((prod.vars as Record<string, string>).OTA_CHANNEL, "production");
  const dev = jsonc(join(root, "wrangler.dev.jsonc"));
  assert.equal(dev.r2_buckets, undefined);
  // A worker var both configs must carry (ds-config-manifest.json); without the
  // bucket it serves nothing.
  assert.equal((dev.vars as Record<string, string>).OTA_CHANNEL, "production");
});

test("the updates module never logs and never reads client-identifying data", () => {
  const src = code(join(root, "worker", "updates.ts"));
  for (const banned of [/console\./, /\.cf\b/, /cf-connecting-ip/i, /x-forwarded-for/i, /x-real-ip/i, /true-client-ip/i, /user-agent/i, /eas-client-id/i]) {
    assert.ok(!banned.test(src), `updates.ts matches ${banned}`);
  }
});

test("index.ts answers /updates/ before the container forward", () => {
  const src = code(join(root, "worker", "index.ts"));
  const fetchAt = src.indexOf("export default {");
  assert.ok(fetchAt >= 0);
  const entry = src.slice(fetchAt);
  const claim = entry.indexOf("isUpdatesPath(");
  const answer = entry.indexOf("return handleUpdates(request, env)");
  const forward = entry.indexOf("env.POLLIS_DELIVERY");
  assert.ok(claim >= 0 && answer > claim, "the default fetch must claim /updates/ paths");
  assert.ok(forward > answer, "the /updates/ branch must return before the container forward");
});
