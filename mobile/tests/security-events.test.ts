/* #1548: the Security events filters and page arguments, held to desktop's table. */

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import {
  SECURITY_EVENT_FILTERS,
  SECURITY_EVENT_FILTER_ORDER,
  securityEventsArgs,
} from "../lib/securityEvents.ts";
const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const read = (rel: string) => readFileSync(join(ROOT, rel), "utf8");

// Desktop's module cannot load under node, so its table is compared as text.
test("the filter table matches desktop's", () => {
  const squash = (s: string) => s.replace(/\s+/g, "").replace(/,([\]}])/g, "$1");
  const desktop = squash(read("../frontend/src/components/Security/securityEvents.ts"));
  for (const f of SECURITY_EVENT_FILTER_ORDER) {
    const kinds = SECURITY_EVENT_FILTERS[f];
    const literal = `${f}:${kinds === null ? "null" : JSON.stringify(kinds)}`;
    assert.ok(desktop.includes(literal), `desktop's ${f} filter is not ${literal}`);
  }
  assert.ok(
    desktop.includes(`SECURITY_EVENT_FILTER_ORDER:SecurityEventFilter[]=${JSON.stringify(SECURITY_EVENT_FILTER_ORDER)}`),
    "desktop's filter order differs",
  );
});

test("every DS-written kind the screen describes sits under exactly one filter", () => {
  const kinds = [
    "device_enrolled",
    "device_rejected",
    "device_revoked",
    "identity_reset",
    "identity_rotated",
    "secret_key_rotated",
    "recovery_blob_fetched",
    "email_changed",
  ];
  for (const kind of kinds) {
    const homes = SECURITY_EVENT_FILTER_ORDER.filter((f) =>
      (SECURITY_EVENT_FILTERS[f] ?? []).includes(kind),
    );
    assert.equal(homes.length, 1, `${kind} is under ${JSON.stringify(homes)}`);
  }
});

test("All sends no kinds, so its body is the pre-#1548 shape", () => {
  assert.deepEqual(securityEventsArgs("u1", 3, "all", null), {
    userId: "u1",
    limit: 3,
    before: null,
    kinds: null,
  });
  const cursor = { created_at: "2026-10-01 12:00:00", id: "01J" };
  assert.deepEqual(securityEventsArgs("u1", 25, "account", cursor), {
    userId: "u1",
    limit: 25,
    before: cursor,
    kinds: ["email_changed"],
  });
});

test("recovery_blob_fetched has its own copy on both apps", () => {
  for (const file of [
    "components/self/SecurityEventRow.tsx",
    "../frontend/src/components/Security/securityEvents.ts",
  ]) {
    const src = read(file);
    assert.match(src, /case "recovery_blob_fetched":/, file);
    assert.match(src, /eventRecoveryBlobFetchedHeading/, file);
  }
});

test('the relative time never shrinks or wraps ("just now" lost "now")', () => {
  const src = read("components/self/SecurityEventRow.tsx");
  assert.match(src, /<Text style=\{\[ty\.meta, \{ flexShrink: 0 \}\]\} numberOfLines=\{1\}>/);
});
