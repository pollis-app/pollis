/*
 * `vaultSaveFailure` (#1497). A Vault save that failed used to say nothing on
 * desktop (the upload rejection was unhandled and the composer had already
 * cleared) and only "Couldn't save" on mobile. The reason now comes from the
 * core's own error text — a Tauri rejection is a plain string.
 *
 *   node --test frontend/tests/
 */

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

import { vaultSaveFailure } from "../src/utils/vaultSaveFailure.ts";

const src = (p: string) => readFileSync(new URL(`../${p}`, import.meta.url), "utf8");

test("a connection failure or a 5xx is offline: sending again is the fix", () => {
  for (const message of [
    "ds_post /v1/r2/presign: error sending request for url (https://api.pollis.com/v1/r2/presign): client error (SendRequest): connection closed before message completed",
    'ds_post /v1/vault/save 500 Internal Server Error: {"error":"internal error"}',
    "Network error: error sending request for url (https://r2.example/media/x.enc)",
    "R2 upload failed: 503 Service Unavailable — slow down",
    "ds_post /v1/r2/presign: operation timed out",
  ]) {
    assert.equal(vaultSaveFailure(message), "offline", message);
    assert.equal(vaultSaveFailure(new Error(message)), "offline", message);
  }
});

test("an over-cap file is tooLarge; anything else stays generic", () => {
  assert.equal(
    vaultSaveFailure("attachment is 300000000 bytes; the limit is 262144000 bytes"),
    "tooLarge",
  );
  assert.equal(vaultSaveFailure("vault entry is empty"), "other");
  assert.equal(vaultSaveFailure("attachment is 512 bytes"), "other");
  assert.equal(vaultSaveFailure(undefined), "other");
});

test("the Vault page shows the reason, restores the draft and offers a retry", () => {
  const page = src("src/pages/Vault.tsx");
  assert.match(page, /vaultSaveFailure\(err\)/);
  assert.match(page, /chatInputRef\.current\?\.restore\(/);
  assert.match(page, /data-testid="vault-save-failed"/);
  assert.match(page, /chatInputRef\.current\?\.send\(\)/);
});

test("a retry of the same draft uploads only what failed", () => {
  const envelope = src("src/utils/attachmentEnvelope.ts");
  assert.match(envelope, /const done = uploaded\.get\(att\)/);
  assert.match(envelope, /uploaded\.set\(att, result\)/);
  // Every upload settles before the failure is reported, so the memo is whole.
  assert.match(envelope, /Promise\.allSettled\(/);
});
