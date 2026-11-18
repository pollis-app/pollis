/* Bots (#1182): who reads as a bot, which refusals the Bots pages explain, the cap the Bots page
   shows before the DS refuses, and the Bots pages' breadcrumbs. */

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import {
  GROUP_BOT_CAP,
  botAddHostCommand,
  botErrorKind,
  botHandleSet,
  botIdSet,
  isBotUser,
} from "../src/utils/bots.ts";
import { breadcrumbTrail } from "../src/utils/breadcrumbTrail.ts";

test("a member is a bot only when the core says so, in either casing", () => {
  assert.equal(isBotUser({ is_bot: true }), true);
  assert.equal(isBotUser({ isBot: true }), true);
  assert.equal(isBotUser({ is_bot: false }), false);
  // An old core sends no field: a person.
  assert.equal(isBotUser({}), false);
  assert.equal(isBotUser(null), false);
  assert.equal(isBotUser(undefined), false);
});

test("bot ids and handles come from the member list; handles lowercased", () => {
  const members = [
    { user_id: "u_ann", username: "ann" },
    { user_id: "u_bot", username: "Alerts_Bot", is_bot: true },
    { user_id: "u_nameless", is_bot: true },
  ];
  assert.deepEqual([...botIdSet(members)], ["u_bot", "u_nameless"]);
  assert.deepEqual([...botHandleSet(members)], ["alerts_bot"]);
});

test("the core's bot refusals are recognised by prefix, as a string or an Error", () => {
  assert.equal(botErrorKind("pairing_code_mismatch: typed code differs"), "codeMismatch");
  assert.equal(botErrorKind(new Error("bot_cap_reached: 25")), "capReached");
  assert.equal(botErrorKind("pairing_request_expired"), "requestExpired");
  assert.equal(botErrorKind("network unreachable"), null);
  // The prefix must lead: a message that merely mentions one is something else.
  assert.equal(botErrorKind("failed: pairing_code_mismatch"), null);
  assert.equal(botErrorKind(undefined), null);
});

test("the Bots page's cap is the DS's MAX_BOTS_PER_GROUP", () => {
  const api = readFileSync(
    fileURLToPath(new URL("../../pollis-api/src/bots.rs", import.meta.url)),
    "utf8",
  );
  const rust = /pub const MAX_BOTS_PER_GROUP: u32 = (\d+);/.exec(api);
  assert.ok(rust, "MAX_BOTS_PER_GROUP not found in pollis-api/src/bots.rs");
  assert.equal(GROUP_BOT_CAP, Number(rust[1]));
});

test("the add-host command names the bot", () => {
  assert.equal(botAddHostCommand("u_bot1"), "pollis-bot pair --bot u_bot1");
});

const ctx = {
  groups: [{ id: "g1", name: "Acme", channels: [] }],
  channels: {},
  dms: [],
};
const t = (key: string) => key;
const labels = (path: string) => breadcrumbTrail(path, ctx, t).map((s) => s.label);

test("the Bots pages sit under their group", () => {
  const base = ["nav:breadcrumb.home", "nav:breadcrumb.groups", "Acme", "nav:breadcrumb.bots"];
  assert.deepEqual(labels("/groups/g1/bots"), base);
  assert.deepEqual(labels("/groups/g1/bots/new"), [...base, "nav:breadcrumb.addBot"]);
  assert.deepEqual(labels("/groups/g1/bots/b1"), [...base, "nav:breadcrumb.bot"]);
  assert.deepEqual(labels("/groups/g1/bots/b1/add-host"), [
    ...base,
    "nav:breadcrumb.bot",
    "nav:breadcrumb.addBotHost",
  ]);
  assert.deepEqual(labels("/groups/g1/bots/b1/delete"), [
    ...base,
    "nav:breadcrumb.bot",
    "nav:breadcrumb.deleteBot",
  ]);
});
