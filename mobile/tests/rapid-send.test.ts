// Rapid sends (#1636): the composer never waits on an in-flight send. The core stores each
// send at once and delivers a conversation's sends in order (pollis-core outbox), so the app
// only has to hand every tap over immediately and keep each stub distinct.

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const src = (p: string) => readFileSync(new URL(`../${p}`, import.meta.url), "utf8");

test("the chat and thread composers never hold Send on an in-flight send", () => {
  for (const file of ["app/chat/[id].tsx", "app/chat/thread.tsx"]) {
    assert.doesNotMatch(src(file), /sendMessage\.isPending/, `${file} gates sending on an in-flight send`);
  }
});

test("a send goes to the core at once, with nothing on the JS side making it wait", () => {
  const hook = src("hooks/queries/useMessages.ts");
  const send = hook.slice(hook.indexOf("export function useSendMessage("), hook.indexOf("export function useRetryMessage("));
  assert.match(send, /invoke<RawChannelMessage>\("send_message"/);
  assert.doesNotMatch(send, /scope:|\.ready\b|sendLine|isPending/);
});

test("a pending row says it is sending, which the rapid-send flows assert before the second tap", () => {
  const row = src("components/chat/MessageRow.tsx");
  assert.match(row, /const spokenStatus = statusLabel \?\? \(pending \? t\("status\.sending"\) : time\);/);
  assert.match(row, /accessibilityLabel=\{`[^`]*\$\{spokenStatus\}`\}/);
  const burst = src(".maestro/subflows/rapid-send-burst.yaml");
  const precondition = burst.indexOf('- inputText: "${TAG} two"');
  const secondTap = burst.indexOf("- tapOn:\n    id: \"btn-send\"", precondition);
  assert.ok(precondition > 0 && secondTap > precondition);
  assert.match(burst.slice(precondition, secondTap), /assertVisible: "\.\*: \$\{TAG\} one, sending"/);
});

test("two stubs made in one millisecond never share an id", () => {
  // Settling one would otherwise replace both.
  assert.match(src("hooks/queries/useMessages.ts"), /`pending-\$\{Date\.now\(\)\}-\$\{stubSeq\}`/);
});
