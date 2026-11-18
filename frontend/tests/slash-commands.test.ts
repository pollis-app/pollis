/* Slash commands in the composer (#2018): the wire format (mirrors pollis-app-manifest's
   commands.rs), the `/` autocomplete, the duplicate-name disambiguation and the rule that a
   `request` command is never posted. */

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

import {
  applyCommand,
  classifyCommandSend,
  commandQueryAt,
  commandsForChannel,
  parseCommand,
  pendingRequestCommand,
  rankCommands,
  type GroupCommand,
} from "../src/utils/slashCommands.ts";

function cmd(over: Partial<GroupCommand>): GroupCommand {
  return {
    installationId: "linear",
    botUserId: "u_linear",
    botUsername: "linear_bot",
    appName: "linear",
    command: "ticket",
    description: "File a Linear issue",
    argHint: "<title>",
    request: false,
    channels: ["general"],
    needsTarget: false,
    ...over,
  };
}

const linear = cmd({ needsTarget: true });
const jira = cmd({
  installationId: "jira",
  botUserId: "u_jira",
  botUsername: "jira_bot",
  appName: "jira",
  description: "File a Jira issue",
  needsTarget: true,
});
const forget = cmd({
  installationId: "jira",
  botUserId: "u_jira",
  botUsername: "jira_bot",
  appName: "jira",
  command: "forget-me",
  description: "Ask for your data",
  argHint: null,
  request: true,
});
const deploy = cmd({ command: "deploy", description: "Ship it", channels: ["ops"] });
const all = [linear, jira, forget, deploy];

test("parses like the runtime does", () => {
  assert.deepEqual(parseCommand("/ticket @Linear_bot printer is on fire "), {
    name: "ticket",
    target: "Linear_bot",
    args: "printer is on fire",
  });
  assert.deepEqual(parseCommand("  /Ticket"), { name: "ticket", target: null, args: "" });
  assert.deepEqual(parseCommand("/ticket printer @alice"), { name: "ticket", target: null, args: "printer @alice" });
  for (const text of ["", "/", "//", "/ ticket", "hello /ticket", "/usr/bin", "/1abc", "/ti!cket x", "ticket"]) {
    assert.equal(parseCommand(text), null, text);
  }
});

test("the `/` query is only the message's first word", () => {
  assert.deepEqual(commandQueryAt("/tic", 4), { query: "tic", end: 4 });
  assert.deepEqual(commandQueryAt("/", 1), { query: "", end: 1 });
  assert.deepEqual(commandQueryAt("/tic rest", 2), { query: "t", end: 4 });
  assert.equal(commandQueryAt("/ticket now", 9), null, "past the name");
  assert.equal(commandQueryAt("hi /tic", 7), null, "not at the start");
  assert.equal(commandQueryAt("/usr/b", 6), null, "a path");
});

test("only commands granted this channel are offered, ranked by name", () => {
  assert.deepEqual(commandsForChannel(all, "ops"), [deploy]);
  assert.deepEqual(commandsForChannel(all, null), []);
  const ranked = rankCommands(commandsForChannel(all, "general"), "");
  assert.deepEqual(ranked.map((c) => `${c.command}:${c.appName}`), ["forget-me:jira", "ticket:jira", "ticket:linear"]);
  assert.deepEqual(rankCommands(all, "tick").map((c) => c.appName), ["jira", "linear"]);
  assert.deepEqual(rankCommands(all, "data").map((c) => c.command), ["forget-me"], "description matches");
});

test("picking a shared name inserts the bot's mention, so the message reaches only that App", () => {
  assert.deepEqual(applyCommand("/tic", 4, linear), { text: "/ticket @linear_bot ", caret: 20 });
  assert.deepEqual(applyCommand("/tic", 4, deploy), { text: "/deploy ", caret: 8 });
  assert.deepEqual(applyCommand("/t printer", 2, deploy), { text: "/deploy printer", caret: 8 });
  assert.equal(parseCommand(applyCommand("/tic", 4, linear).text)?.target, "linear_bot");
});

test("a request command is never posted; a shared name without its bot is not sent", () => {
  assert.deepEqual(classifyCommandSend("/forget-me", all, "general"), { kind: "request", command: forget, args: "" });
  assert.deepEqual(classifyCommandSend("/forget-me", all, "ops"), { kind: "request", command: forget, args: "" });
  assert.deepEqual(classifyCommandSend("/ticket printer", all, "general"), { kind: "ambiguous", name: "ticket" });
  assert.deepEqual(classifyCommandSend("/ticket @linear_bot printer", all, "general"), { kind: "post" });
  assert.deepEqual(classifyCommandSend("/shrug", all, "general"), { kind: "post" }, "unknown is ordinary");
  assert.deepEqual(classifyCommandSend("hello", all, "general"), { kind: "post" });
  assert.equal(pendingRequestCommand("/forget-me now", all, "general"), forget);
  assert.equal(pendingRequestCommand("/ticket @jira_bot x", all, "general"), null);
});

test("mobile carries the same rules", () => {
  const body = (src: string) => src.slice(src.indexOf("/** One command the group's Apps offer"));
  const desktop = readFileSync(new URL("../src/utils/slashCommands.ts", import.meta.url), "utf8");
  const mobile = readFileSync(new URL("../../mobile/lib/slashCommands.ts", import.meta.url), "utf8");
  assert.equal(body(mobile), body(desktop), "mobile/lib/slashCommands.ts drifted from the desktop rules");
});
