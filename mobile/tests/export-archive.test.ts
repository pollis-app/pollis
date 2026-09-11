/*
 * The naming of the on-device export (#856) — the part that has to agree
 * with desktop's save-dialog suggestion, pinned without a renderer. Wording
 * is catalogue copy and is covered by `scripts/i18n-check.mjs`.
 *
 *   node --test mobile/tests/
 */

import test from "node:test";
import assert from "node:assert/strict";

import { exportFileName, formatBytes } from "../lib/exportArchive.ts";

test("file names match what desktop suggests in its save dialog", () => {
  assert.equal(exportFileName(null, new Date("2026-09-11T22:00:00Z")), "pollis-archive-2026-09-11.json");
  assert.equal(exportFileName("01HQ7"), "pollis-conversation-01HQ7.json");
});

test("sizes read the way the desktop summary reads them", () => {
  assert.equal(formatBytes(512), "512 B");
  assert.equal(formatBytes(2048), "2.0 KB");
  assert.equal(formatBytes(3 * 1024 * 1024), "3.0 MB");
});
