// Typing indicator (#1635): the wire matches desktop's, the typist comes from the participant, and
// the sender throttles to desktop's cadence with one trailing stop.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { decodeRealtimeEvent, isTrustedOrigin } from "../lib/realtime/events.ts";
import { createParticipantResolver } from "../lib/realtime/participants.ts";
import {
  TYPING_REFRESH_MS,
  createTypingThrottle,
  describeTyping,
  typingPayload,
  typingRoomKey,
} from "../lib/typing.ts";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const read = (p: string) => readFileSync(join(root, p), "utf8");

test("the payload is desktop's identity-free typing_payload, and it decodes", () => {
  const body = typingPayload("chan-1", null, true);
  assert.deepEqual(body, {
    type: "typing",
    channel_id: "chan-1",
    conversation_id: null,
    is_typing: true,
  });
  assert.equal(Object.keys(body).some((k) => /user|name|sender/.test(k)), false);
  const event = decodeRealtimeEvent(JSON.stringify(body));
  assert.equal(event?.type, "typing");
});

test("a typing packet with no publishing participant is unattributable and dropped", () => {
  const event = decodeRealtimeEvent(JSON.stringify(typingPayload(null, "dm-1", true)));
  assert.ok(event);
  assert.equal(isTrustedOrigin(event, false), false);
  assert.equal(isTrustedOrigin(event, true), true);
});

test("the typist is stamped from the resolved participant, never from the body", () => {
  const client = read("lib/realtime/client.ts");
  const typing = client.slice(client.indexOf('event.type === "typing"'), client.indexOf("for (const listener of entry.listeners) {\n        listener(event);"));
  assert.match(typing, /participant\?\.identity/);
  assert.match(typing, /user_id: sender\.userId/);
  assert.doesNotMatch(typing, /event\.user_id|event\.username/);
});

test("participant resolution is cached per room and identity; a failure is not cached", async () => {
  const calls: Array<[string, string[]]> = [];
  let fail = true;
  const resolve = createParticipantResolver(async (room, ids) => {
    calls.push([room, ids]);
    if (fail) {
      throw new Error("offline");
    }
    return [{ identity: "r-abc", userId: "u-bob", username: "bob" }];
  });
  assert.equal(await resolve("room-1", "r-abc"), null);
  fail = false;
  assert.deepEqual(await resolve("room-1", "r-abc"), { userId: "u-bob", username: "bob" });
  assert.deepEqual(await resolve("room-1", "r-abc"), { userId: "u-bob", username: "bob" });
  assert.equal(calls.length, 2);
  // Pseudonyms are per room (#836): another room is its own lookup.
  await resolve("room-2", "r-abc");
  assert.equal(calls.length, 3);
  // A fabricated identity the DS cannot attribute resolves to nobody.
  assert.equal(await resolve("room-1", "r-forged"), null);
});

test("the sender throttles to desktop's cadence and sends exactly one stop", () => {
  let clock = 1_000_000;
  const sent: boolean[] = [];
  let pending: (() => void) | null = null;
  const throttle = createTypingThrottle({
    publish: (v) => sent.push(v),
    now: () => clock,
    schedule: (fn) => {
      pending = fn;
      return 1 as unknown as ReturnType<typeof setTimeout>;
    },
    cancel: () => {
      pending = null;
    },
  });
  throttle.notify("h");
  throttle.notify("he");
  throttle.notify("hel");
  assert.deepEqual(sent, [true]);
  clock += TYPING_REFRESH_MS;
  throttle.notify("hell");
  assert.deepEqual(sent, [true, true]);
  // Idle: the trailing timer sends the stop.
  (pending as unknown as () => void)();
  assert.deepEqual(sent, [true, true, false]);
  // Nothing active: send/clear sends no second stop.
  throttle.stop();
  throttle.notify("");
  assert.deepEqual(sent, [true, true, false]);
  // Clearing the draft while typing stops at once.
  clock += TYPING_REFRESH_MS;
  throttle.notify("x");
  throttle.notify("");
  assert.deepEqual(sent, [true, true, false, true, false]);
});

test("room keys and the indicator line match desktop", () => {
  assert.equal(typingRoomKey("c", "d"), "channel:c");
  assert.equal(typingRoomKey(null, "d"), "dm:d");
  assert.equal(typingRoomKey(null, null), null);
  assert.equal(describeTyping([]), null);
  assert.deepEqual(describeTyping(["bob"]), { kind: "one", name: "bob" });
  assert.deepEqual(describeTyping(["ann", "bob"]), { kind: "two", first: "ann", second: "bob" });
  assert.deepEqual(describeTyping(["a", "b", "c"]), { kind: "several", count: 3 });
});

test("the chat screen publishes from the composer, stops on send, and shows the indicator", () => {
  const chat = read("app/chat/[id].tsx");
  assert.match(chat, /useTypingPublisher\(conversationId, kind, groupId\)/);
  assert.match(chat, /typing\.notify\(text\)/);
  assert.match(chat, /setDraft\(""\);\n\s*typing\.stop\(\);/);
  assert.match(chat, /<TypingIndicator/);
  assert.match(chat, /onChangeDraft=\{onChangeDraft\}/);
  // The user's own other devices never show as typing.
  assert.match(read("hooks/useConversationRealtime.ts"), /event\.user_id === appStore\.currentUser\?\.id/);
});
