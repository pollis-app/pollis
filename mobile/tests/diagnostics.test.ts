/* Diagnostics (#1305) on mobile: the screen and the clipboard carry the NEWEST lines, and the whole log
   leaves as a .txt through the share sheet. */
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { recentReport } from "../lib/diagnosticsReport.ts";

const HEADER = "Pollis diagnostics\napp: 1\ncore: 1\nos: ios/aarch64\ngenerated: 2026-11-03T17:16:41Z\n----\n";

test("the newest line survives a log far longer than the screen can hold", () => {
  const log = Array.from({ length: 20_000 }, (_, i) => `2026-11-03T00:00:00.000Z [x] line ${i + 1}`).join("\n") + "\n";
  const r = recentReport(HEADER + log);
  assert.ok(r.text.endsWith("line 20000\n"));
  assert.ok(r.omitted > 0);
});

test("copy, preview and save use the right halves", () => {
  const lib = readFileSync(new URL("../lib/diagnostics.ts", import.meta.url), "utf8");
  assert.match(lib, /setStringAsync\(recentReport\(/);
  assert.match(lib, /writeAsStringAsync\(uri, await getDiagnostics\(\)\)/);
  assert.match(lib, /mimeType: "text\/plain"/);
  const screen = readFileSync(new URL("../app/self/diagnostics.tsx", import.meta.url), "utf8");
  assert.match(screen, /setReport\(recentReport\(text\)\)/);
  assert.match(screen, /SaveDiagnosticsButton/);
});
