import { test } from "node:test";
import assert from "node:assert/strict";
import { testApp } from "@pollis/app/testing";

// Runs the built App in the same sandbox the bot runtime uses, offline.
const app = await testApp();

test("/hello greets by name", async () => {
  const result = await app.command("hello", "Sam");
  assert.ok(result.ok, result.error?.message);
  assert.deepEqual(result.replies, ["Hello, Sam!"]);
});

test("/hello with no name greets the sender", async () => {
  const result = await app.command("hello", "", { message: { author: { displayName: "Ann" } } });
  assert.deepEqual(result.replies, ["Hello, Ann!"]);
});

test("other commands are ignored", async () => {
  const result = await app.command("weather");
  assert.ok(result.ok);
  assert.deepEqual(result.actions, []);
});
