/* The account-key line's state (#2114): a running audit is "checking", a failed one "unavailable",
   every verdict its own label, so the line always has something to show. */

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import {
  ACCOUNT_KEY_LABEL_KEYS,
  accountKeyDisplayStatus,
  type AuditVerdict,
} from "../src/components/Security/accountKeyAuditStatus.ts";

const here = dirname(fileURLToPath(import.meta.url));
const VERDICTS: AuditVerdict[] = ["ok", "pending", "alarm", "unavailable"];

test("a running audit shows checking", () => {
  assert.equal(accountKeyDisplayStatus({ isPending: true, isError: false }), "checking");
  assert.equal(accountKeyDisplayStatus({ isPending: true, isError: false, data: undefined }), "checking");
});

test("a failed audit shows unavailable, never nothing", () => {
  assert.equal(accountKeyDisplayStatus({ isPending: false, isError: true }), "unavailable");
});

test("each verdict shows itself", () => {
  for (const status of VERDICTS) {
    assert.equal(accountKeyDisplayStatus({ isPending: false, isError: false, data: { status } }), status);
  }
});

test("a verdict already in hand outlives a failed refetch", () => {
  assert.equal(accountKeyDisplayStatus({ isPending: false, isError: true, data: { status: "ok" } }), "ok");
});

test("a settled query with no report shows unavailable", () => {
  assert.equal(accountKeyDisplayStatus({ isPending: false, isError: false, data: null }), "unavailable");
});

test("every state's label is in the English catalogue", () => {
  const en = JSON.parse(readFileSync(join(here, "../src/i18n/locales/en/settings.json"), "utf8"));
  const label = (status: keyof typeof ACCOUNT_KEY_LABEL_KEYS) =>
    ACCOUNT_KEY_LABEL_KEYS[status].split(".").reduce((node, part) => node?.[part], en);
  assert.equal(label("checking"), "Checking key…");
  assert.equal(label("ok"), "Key publicly verified");
  assert.equal(label("pending"), "Key publication pending");
  assert.equal(label("alarm"), "Key verification failed");
  assert.equal(label("unavailable"), "Verification unavailable");
});
