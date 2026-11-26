/* The account-key line on a peer's profile (#2114): mobile's state rules are desktop's file
   exactly, a running audit is "checking", a failed one "unavailable", and every state's label
   is in the shared catalogue. */
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import {
  ACCOUNT_KEY_LABEL_KEYS,
  accountKeyDisplayStatus,
} from "../lib/accountKeyAuditStatus.ts";

const here = dirname(fileURLToPath(import.meta.url));

test("mobile/lib/accountKeyAuditStatus.ts is desktop's accountKeyAuditStatus.ts, byte for byte", () => {
  const mobile = readFileSync(join(here, "../lib/accountKeyAuditStatus.ts"), "utf8");
  const desktop = readFileSync(
    join(here, "../../frontend/src/components/Security/accountKeyAuditStatus.ts"),
    "utf8",
  );
  assert.equal(
    mobile,
    desktop,
    "edit frontend/src/components/Security/accountKeyAuditStatus.ts and copy it to mobile/lib/accountKeyAuditStatus.ts",
  );
});

test("checking while the audit runs, unavailable when it fails, else the verdict", () => {
  assert.equal(accountKeyDisplayStatus({ isPending: true, isError: false }), "checking");
  assert.equal(accountKeyDisplayStatus({ isPending: false, isError: true }), "unavailable");
  for (const status of ["ok", "pending", "alarm", "unavailable"] as const) {
    assert.equal(accountKeyDisplayStatus({ isPending: false, isError: false, data: { status } }), status);
  }
});

test("every state's label resolves in the English settings catalogue", () => {
  const locales = join(here, "../../frontend/src/i18n/locales");
  const en = JSON.parse(readFileSync(join(locales, "en/settings.json"), "utf8"));
  for (const key of Object.values(ACCOUNT_KEY_LABEL_KEYS)) {
    const value = key.split(".").reduce((node: any, part) => node?.[part], en);
    assert.equal(typeof value, "string", `en settings:${key}`);
  }
});

test("the profile renders the line for a peer from the first frame, not only once the audit lands", () => {
  const screen = readFileSync(join(here, "../app/user/[id].tsx"), "utf8");
  assert.match(screen, /<AccountKeyAuditLine[\s\S]*?status=\{accountKeyDisplayStatus\(peerAudit\)\}/);
  assert.doesNotMatch(screen, /peerAudit\.data\s*&&\s*\(?\s*<AccountKeyAuditLine/);
});
