/* #1640: channel and DM headers share one action row (no per-item margins, no absolute call button), and "return to call" targets the active 1:1 call. */

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";

import { activeCallIdOf, type VoiceState } from "../src/types/voice-state.ts";

const SRC = resolve(dirname(fileURLToPath(import.meta.url)), "../src");

test("activeCallIdOf maps a joining or joined call-* room to its call id", () => {
  const joining: VoiceState = { kind: "joining", channelId: "call-abc", counterpartyUserId: "u-bob" };
  assert.equal(activeCallIdOf(joining), "abc");
  const joined = { ...joining, kind: "joined" } as unknown as VoiceState;
  assert.equal(activeCallIdOf(joined), "abc");
});

test("activeCallIdOf is null for idle, leaving and group voice channels", () => {
  assert.equal(activeCallIdOf({ kind: "idle" }), null);
  assert.equal(activeCallIdOf({ kind: "leaving", channelId: "call-abc" }), null);
  assert.equal(
    activeCallIdOf({ kind: "joining", channelId: "c_lounge", counterpartyUserId: null }),
    null,
  );
});

for (const page of ["pages/DM.tsx", "pages/Channel.tsx"]) {
  test(`${page} lays its header actions out through ConversationHeader`, () => {
    const source = readFileSync(resolve(SRC, page), "utf8");
    assert.match(source, /<ConversationHeader\b/);
    assert.doesNotMatch(source, /position:\s*"absolute"/, "an absolutely-placed action overlaps its neighbours");
    assert.doesNotMatch(source, /icon-btn-sm[^"`]*\bme-\d/, "spacing belongs to the header's gap, not per-button margins");
  });
}
