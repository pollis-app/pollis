import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { dirname, join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

const bin = join(dirname(dirname(fileURLToPath(import.meta.url))), "bin", "pollis.js");

test("`pollis --help` runs @pollis-inc/cli", () => {
  const out = spawnSync(process.execPath, [bin, "--help"], { encoding: "utf8" });
  assert.equal(out.status, 0, out.stderr);
  assert.match(out.stdout, /^pollis app <command>/);
});

test("arguments reach the CLI unchanged", () => {
  const out = spawnSync(process.execPath, [bin, "app", "nope"], { encoding: "utf8" });
  assert.equal(out.status, 2);
  assert.match(out.stderr, /unknown command `pollis app nope`/);
});
