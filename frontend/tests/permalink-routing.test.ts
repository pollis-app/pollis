/*
 * Permalink format + conversation routing (#854).
 *
 * Runs on Node's built-in test runner with native TypeScript type stripping —
 * no test framework dependency, and deliberately OUTSIDE `src/` so the
 * production `tsc --noEmit` (which the build runs) does not try to typecheck a
 * file importing `node:test`.
 *
 *   node --test frontend/tests/
 */

import test from "node:test";
import assert from "node:assert/strict";

import {
  formatMessagePermalink,
  parseMessagePermalink,
  routeForConversation,
  type GroupChannelIndex,
} from "../src/utils/urlRouting.ts";

// Opaque, ULID-shaped fixtures. Deliberately free of readable words so the
// "leaks nothing" assertion below tests the FORMAT rather than the fixture.
const CHANNEL_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0XCB";
const GROUP_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0GRP";
const DM_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0DMX";
const MESSAGE_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0MSG";

// One group with two channels, plus a DM that is NOT in it.
const GROUPS: GroupChannelIndex[] = [
  {
    id: GROUP_ID,
    channels: [{ id: CHANNEL_ID }, { id: "01HQ7Z3K9M2P5R8T1V4W6Y0CH2" }],
  },
  { id: "01HQ7Z3K9M2P5R8T1V4W6Y0GR2", channels: [{ id: "01HQ7Z3K9M2P5R8T1V4W6Y0CH3" }] },
];

test("permalink round-trips", () => {
  const link = formatMessagePermalink(CHANNEL_ID, MESSAGE_ID);
  assert.equal(link, `pollis://m/${CHANNEL_ID}/${MESSAGE_ID}`);

  const parsed = parseMessagePermalink(link);
  assert.deepEqual(parsed, {
    conversationId: CHANNEL_ID,
    messageId: MESSAGE_ID,
  });
});

test("permalink carries the two ids and nothing else", () => {
  const link = formatMessagePermalink(CHANNEL_ID, MESSAGE_ID);

  // Whatever else changes, the link must never grow a name, a body, or a
  // marker distinguishing a channel from a DM.
  const withoutIds = link
    .replace(CHANNEL_ID, "")
    .replace(MESSAGE_ID, "")
    .replace("pollis://m/", "");
  assert.equal(withoutIds, "/", "only the separator remains");

  for (const leak of ["acme", "general", "alice", "hello", "channel", "dm", "group"]) {
    assert.ok(
      !link.toLowerCase().includes(leak),
      `permalink must not embed "${leak}"`,
    );
  }
});

test("permalink parsing rejects anything that is not one", () => {
  const rejected = [
    "",
    "hello world",
    "https://pollis.com/m/a/b",
    "pollis://m/",
    // Missing a half.
    `pollis://m/${CHANNEL_ID}`,
    // Extra path segment — must not be silently truncated to something valid.
    `pollis://m/${CHANNEL_ID}/${MESSAGE_ID}/extra`,
    // Path traversal / route injection attempts.
    "pollis://m/../../etc/passwd",
    "pollis://m/a/..%2Fb",
    "pollis://m/a b/c d",
    "pollis://m/a/b?panel=thread",
  ];
  for (const raw of rejected) {
    assert.equal(
      parseMessagePermalink(raw),
      null,
      `must reject: ${JSON.stringify(raw)}`,
    );
  }
});

test("surrounding whitespace from a paste is tolerated", () => {
  const parsed = parseMessagePermalink(
    `  \n pollis://m/${DM_ID}/${MESSAGE_ID}  \n`,
  );
  assert.deepEqual(parsed, { conversationId: DM_ID, messageId: MESSAGE_ID });
});

// The bug this feature must not inherit: `pages/Search.tsx` sends every hit to
// `/dms/$conversationId`, which silently breaks channel results.
test("a CHANNEL conversation routes to the channel route, not the DM route", () => {
  const route = routeForConversation(CHANNEL_ID, GROUPS);
  assert.deepEqual(route, {
    kind: "channel",
    groupId: GROUP_ID,
    channelId: CHANNEL_ID,
  });
});

test("a channel in the second group resolves to THAT group", () => {
  const route = routeForConversation("01HQ7Z3K9M2P5R8T1V4W6Y0CH3", GROUPS);
  assert.deepEqual(route, {
    kind: "channel",
    groupId: "01HQ7Z3K9M2P5R8T1V4W6Y0GR2",
    channelId: "01HQ7Z3K9M2P5R8T1V4W6Y0CH3",
  });
});

test("a DM conversation routes to the DM route", () => {
  const route = routeForConversation(DM_ID, GROUPS);
  assert.deepEqual(route, { kind: "dm", conversationId: DM_ID });
});

test("routing works with no groups loaded", () => {
  assert.deepEqual(routeForConversation(DM_ID, []), {
    kind: "dm",
    conversationId: DM_ID,
  });
});

test("a permalink round-trips through parse into the right route for both kinds", () => {
  // Channel.
  const channelLink = formatMessagePermalink(CHANNEL_ID, MESSAGE_ID);
  const channelParsed = parseMessagePermalink(channelLink);
  assert.ok(channelParsed);
  assert.equal(
    routeForConversation(channelParsed.conversationId, GROUPS).kind,
    "channel",
  );

  // DM. Same link format — the format itself does not encode which it is, so
  // nothing about the target's kind leaks into the string.
  const dmLink = formatMessagePermalink(DM_ID, MESSAGE_ID);
  const dmParsed = parseMessagePermalink(dmLink);
  assert.ok(dmParsed);
  assert.equal(routeForConversation(dmParsed.conversationId, GROUPS).kind, "dm");

  assert.equal(
    channelLink.replace(CHANNEL_ID, "X"),
    dmLink.replace(DM_ID, "X"),
    "channel and DM permalinks are structurally identical",
  );
});
