/* Bots (#1182): the pure rules behind the Bots pages and the Bot badge, and what the screens must keep. */

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import {
  BOT_PAIR_COMMAND,
  botCapReached,
  botErrorKey,
  botHandles,
  botIds,
  isBotUser,
  isLastHost,
  type BotHost,
} from "../lib/bots.ts";
import { decodeRealtimeEvent, isTrustedOrigin } from "../lib/realtime/events.ts";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const read = (rel: string) => readFileSync(join(ROOT, rel), "utf8");

test("a refused pairing maps to its own copy; anything else keeps the core's message", () => {
  assert.equal(botErrorKey("pairing_code_mismatch: the code does not match"), "mobile:bots.errors.codeMismatch");
  assert.equal(botErrorKey("bot_cap_reached"), "mobile:bots.errors.capReached");
  assert.equal(botErrorKey("pairing_request_expired: gone"), "mobile:bots.errors.expired");
  // A uniffi prefix the bridge did not strip still matches.
  assert.equal(botErrorKey("BridgeError.Bridge: pairing_code_mismatch"), "mobile:bots.errors.codeMismatch");
  // Only the start counts: a message that merely mentions a code is not that refusal.
  assert.equal(botErrorKey("network error near pairing_code_mismatch"), null);
  assert.equal(botErrorKey(""), null);
  assert.equal(botErrorKey(undefined), null);
});

test("is_bot drives the badge; an old core that sends no flag means human", () => {
  assert.equal(isBotUser({ is_bot: true }), true);
  assert.equal(isBotUser({ isBot: true }), true);
  assert.equal(isBotUser({}), false);
  assert.equal(isBotUser({ is_bot: false }), false);
  assert.equal(isBotUser(null), false);
  const roster = [
    { user_id: "u1", username: "Alice" },
    { user_id: "b1", username: "AlertBot", is_bot: true },
    { user_id: "b2", is_bot: true },
  ];
  assert.deepEqual([...botIds(roster)], ["b1", "b2"]);
  // Mentions are matched lowercased; a bot with no handle has none to mention.
  assert.deepEqual([...botHandles(roster)], ["alertbot"]);
});

test("the cap blocks only once it is known and reached", () => {
  assert.equal(botCapReached(25, 25), true);
  assert.equal(botCapReached(24, 25), false);
  assert.equal(botCapReached(30, undefined), false);
  assert.equal(botCapReached(3, 0), false);
});

test("revoking the only host is called out before it happens", () => {
  const one: BotHost[] = [{ deviceId: "d1", name: "nas", lastSeenAt: null, createdAt: "" }];
  const two: BotHost[] = [...one, { deviceId: "d2", name: "pi", lastSeenAt: null, createdAt: "" }];
  assert.equal(isLastHost(one, "d1"), true);
  assert.equal(isLastHost(two, "d1"), false);
  assert.equal(isLastHost(one, "d2"), false);
  const page = read("app/group/bot.tsx");
  assert.match(page, /isLastHost\(hosts, h\.deviceId\)/);
  assert.match(page, /t\("bots\.unrecoverable"\)/);
});

test("the pairing code is TYPED (#1096): one field, never pre-filled", () => {
  const add = read("app/group/add-bot.tsx");
  assert.match(add, /<SasCodeField\b/);
  assert.match(add, /useState\(""\);\s*const \[error/, "the code starts empty");
  assert.doesNotMatch(add, /setCode\((?!""|next)/, "the code is only ever cleared or typed");
  // The same field the device approval uses: one input rule for every typed code.
  for (const f of ["app/approve-device.tsx", "app/self/security.tsx"]) {
    assert.match(read(f), /<SasCodeField\b/, f);
  }
  assert.match(read("components/SasCodeField.tsx"), /onChange\(normalizeSasInput\(next\)\)/);
  assert.match(add, /BOT_PAIR_COMMAND/);
  assert.equal(BOT_PAIR_COMMAND, "pollis-bot pair");
});

test("the Bot badge sits on member rows, message rows and mentions, driven by the roster", () => {
  assert.match(read("app/group/members.tsx"), /isBot \? <BotBadge/);
  assert.match(read("components/chat/MessageRow.tsx"), /isBot \? <BotBadge/);
  assert.match(read("components/chat/MentionToken.tsx"), /<BotBadge inline/);
  assert.match(read("components/chat/Composer.tsx"), /candidate\.isBot \? \(\s*<BotBadge/);
  assert.match(read("hooks/useMentionCandidates.ts"), /isBot: isBotUser\(m\)/);
  for (const f of ["app/chat/[id].tsx", "app/chat/thread.tsx"]) {
    assert.match(read(f), /isBot=\{botSenders\.has\(/, f);
  }
  // A bot is managed on its own page: no role, alias or remove chips on its member row.
  assert.match(read("app/group/members.tsx"), /iAmAdmin && !isMe && !isOwner && !isBot/);
});

test("every Bots page is a registered route and opens in the iPad pane", () => {
  const layout = read("app/_layout.tsx");
  const pane = read("components/pane/paneContext.ts");
  const md = read("components/MasterDetail.tsx");
  for (const route of ["group/bots", "group/bot", "group/add-bot"]) {
    assert.match(layout, new RegExp(`name="${route}"`), route);
    assert.match(pane, new RegExp(`"/${route}"`), route);
    assert.match(md, new RegExp(`"/${route}": `), route);
  }
});

test("the DS's bots nudge decodes, only from the DS, and an open Bots page re-reads", () => {
  const event = decodeRealtimeEvent(JSON.stringify({ type: "bots_changed", group_id: "g1" }));
  assert.ok(event, "bots_changed must decode");
  assert.equal(event.type, "bots_changed");
  assert.ok(isTrustedOrigin(event, false));
  assert.ok(!isTrustedOrigin(event, true), "a participant cannot fake the DS nudge");
  assert.match(read("hooks/useInboxRealtime.ts"), /case "bots_changed":[\s\S]*?botQueryKeys\.all/);
});
