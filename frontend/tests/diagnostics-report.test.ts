// The diagnostics report (#1305) trimmed for the preview and clipboard: the newest lines survive, and both
// apps' copies of the rule stay identical.

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { diagnosticsFileName, recentReport } from "../src/utils/diagnosticsReport.ts";

const HEADER = "Pollis diagnostics\napp: 1\ncore: 1\nos: android/aarch64\ngenerated: 2026-11-03T17:16:41Z\n----\n";
const log = (n: number) => Array.from({ length: n }, (_, i) => `2026-11-03T00:00:00.000Z [x] line ${i + 1}`).join("\n") + "\n";

test("a log that fits is returned whole", () => {
  const report = HEADER + log(10);
  assert.deepEqual(recentReport(report), { text: report, omitted: 0 });
});

test("a long log keeps the header and the NEWEST whole lines", () => {
  const r = recentReport(HEADER + log(5000), 2000);
  assert.ok(r.text.startsWith(HEADER));
  assert.ok(r.text.endsWith("line 5000\n"));
  const body = r.text.slice(HEADER.length);
  assert.ok(body.length <= 2000);
  assert.match(body, /^2026-11-03T/);
  assert.equal(r.omitted + body.split("\n").length - 1, 5000);
});

test("a report without the header separator is left alone", () => {
  assert.deepEqual(recentReport("x".repeat(10), 3), { text: "x".repeat(10), omitted: 0 });
});

test("the file name is a UTC stamp", () => {
  assert.equal(diagnosticsFileName(new Date("2026-11-03T17:16:41Z")), "pollis-diagnostics-20261103-1716.txt");
});

test("mobile's copy of the rule matches the desktop one", () => {
  const strip = (s: string) => s.slice(s.indexOf("export const RECENT_LOG_CHARS"));
  const desktop = readFileSync(new URL("../src/utils/diagnosticsReport.ts", import.meta.url), "utf8");
  const mobile = readFileSync(new URL("../../mobile/lib/diagnosticsReport.ts", import.meta.url), "utf8");
  assert.equal(strip(mobile), strip(desktop));
});
