// The composer's send gate (#2099): one submit per draft, and the cleared field stays clear.

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import {
  ECHO_WINDOW_MS,
  acceptChange,
  markCleared,
  newSubmitGate,
  rearm,
  takeSubmit,
} from "../lib/composerSubmit.ts";

const src = (p: string) => readFileSync(new URL(`../${p}`, import.meta.url), "utf8");

test("taps delivered in one batch, before the composer renders again, send once", () => {
  const gate = newSubmitGate("hello");
  assert.equal(takeSubmit(gate, false), "hello");
  markCleared(gate, "hello", 1000);
  assert.equal(takeSubmit(gate, false), null, "the second tap of the batch");
  assert.equal(takeSubmit(gate, true), null, "also with attachments, which allow an empty text");
  rearm(gate, "");
  assert.equal(takeSubmit(gate, false), null, "nothing left to send after the render");
});

test("a tap that did not clear the field still waits for the next render", () => {
  const gate = newSubmitGate("/ask x");
  assert.equal(takeSubmit(gate, false), "/ask x");
  assert.equal(takeSubmit(gate, false), null);
  rearm(gate, "/ask x");
  assert.equal(takeSubmit(gate, false), "/ask x");
});

test("the tap sends the newest text the field reported, not an older render's", () => {
  const gate = newSubmitGate("hell");
  assert.ok(acceptChange(gate, "hello", 0));
  assert.equal(takeSubmit(gate, false), "hello");
});

test("an empty draft sends nothing unless attachments allow it", () => {
  const gate = newSubmitGate("   ");
  assert.equal(takeSubmit(gate, false), null);
  assert.equal(takeSubmit(gate, true), "   ");
});

test("the field's late report of the sent text never refills the composer", () => {
  const gate = newSubmitGate("see you tomorow.");
  takeSubmit(gate, false);
  markCleared(gate, "see you tomorow.", 1000);
  assert.equal(acceptChange(gate, "see you tomorow.", 1010), false, "the sent text itself");
  assert.equal(acceptChange(gate, "see you tomorrow.", 1020), false, "revised by autocorrect");
  assert.equal(gate.latest, "", "and a tap now has nothing to send");
  assert.equal(takeSubmit(gate, false), null);
});

test("new typing right after a send is taken", () => {
  const gate = newSubmitGate("ok");
  takeSubmit(gate, false);
  markCleared(gate, "ok", 1000);
  assert.ok(acceptChange(gate, "y", 1150), "a first character");
  assert.ok(acceptChange(gate, "ye", 1200), "and the window closed with it");
  assert.equal(gate.latest, "ye");
});

test("a one-character emoji typed right after a send is new typing", () => {
  const gate = newSubmitGate("hi");
  markCleared(gate, "hi", 1000);
  assert.ok(acceptChange(gate, "👍", 1100));
});

test("after the window, the same text again is new typing", () => {
  const gate = newSubmitGate("hello");
  takeSubmit(gate, false);
  markCleared(gate, "hello", 1000);
  assert.ok(acceptChange(gate, "hello", 1000 + ECHO_WINDOW_MS + 1));
  assert.equal(gate.latest, "hello");
});

test("without a clearing send nothing is filtered", () => {
  const gate = newSubmitGate("");
  assert.ok(acceptChange(gate, "pasted text", 0));
});

test("every composer screen sends the text the composer hands over", () => {
  const composer = src("components/chat/Composer.tsx");
  assert.match(composer, /onPress=\{submit\}/, "Send goes through the gate");
  assert.match(composer, /if \(!acceptChange\(gate, next, Date\.now\(\)\)\) \{/);
  for (const file of ["app/chat/[id].tsx", "app/chat/thread.tsx", "app/vault.tsx"]) {
    const screen = src(file);
    assert.match(screen, /const onSend = \(typed: string\): boolean => \{\n\s+const text = outgoingText\(typed\);/, file);
  }
});
