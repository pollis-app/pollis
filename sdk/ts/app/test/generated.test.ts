// The SDK's copy of the WIT and its generated declarations must match the
// repo's canonical pollis-app-wit/. If this fails, run `pnpm gen`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, readdirSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(dirname(fileURLToPath(import.meta.url)));
const canonical = join(here, "..", "..", "..", "pollis-app-wit");

function files(dir: string, prefix = ""): string[] {
  return readdirSync(join(dir, prefix), { withFileTypes: true }).flatMap((e) =>
    e.isDirectory() ? files(dir, join(prefix, e.name)) : [join(prefix, e.name)],
  );
}

test("wit/ is the canonical pollis:app WIT and manifest schema", () => {
  assert.equal(readFileSync(join(here, "wit", "app.wit"), "utf8"), readFileSync(join(canonical, "wit", "app.wit"), "utf8"));
  assert.equal(
    readFileSync(join(here, "wit", "pollis-app.schema.json"), "utf8"),
    readFileSync(join(canonical, "pollis-app.schema.json"), "utf8"),
  );
});

test("src/generated is what jco generates from the WIT", async () => {
  const { generate } = await import("../scripts/gen-host-types.mjs");
  const out = mkdtempSync(join(tmpdir(), "pollis-gen-"));
  try {
    generate(out);
    const committed = join(here, "src", "generated");
    assert.deepEqual(files(out).sort(), files(committed).sort());
    for (const f of files(out)) {
      assert.equal(readFileSync(join(out, f), "utf8"), readFileSync(join(committed, f), "utf8"), f);
    }
  } finally {
    rmSync(out, { recursive: true, force: true });
  }
});
