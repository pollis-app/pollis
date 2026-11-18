// Slash commands in the composer (#2018): the mobile port picks, disambiguates and routes like
// desktop (frontend/tests/slash-commands.test.ts holds the full cases and the parity check).

import test from "node:test";
import assert from "node:assert/strict";

import { applyCommand, classifyCommandSend, commandQueryAt, type GroupCommand } from "../lib/slashCommands.ts";

const base: GroupCommand = {
  installationId: "linear",
  botUserId: "u_linear",
  botUsername: "linear_bot",
  appName: "linear",
  command: "ticket",
  description: "File a Linear issue",
  argHint: "<title>",
  request: false,
  channels: ["general"],
  needsTarget: true,
};
const jira: GroupCommand = { ...base, installationId: "jira", botUserId: "u_jira", botUsername: "jira_bot", appName: "jira" };
const forget: GroupCommand = { ...jira, command: "forget-me", request: true, needsTarget: false, argHint: null };

test("a picked shared name carries its bot; a request is never posted", () => {
  assert.deepEqual(commandQueryAt("/tic", 4), { query: "tic", end: 4 });
  assert.equal(applyCommand("/tic", 4, base).text, "/ticket @linear_bot ");
  const all = [base, jira, forget];
  assert.deepEqual(classifyCommandSend("/ticket @linear_bot x", all, "general"), { kind: "post" });
  assert.deepEqual(classifyCommandSend("/ticket x", all, "general"), { kind: "ambiguous", name: "ticket" });
  assert.equal(classifyCommandSend("/forget-me", all, "general").kind, "request");
});
