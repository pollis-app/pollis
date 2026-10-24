/*
 * When the app sends its push token to the DS (`mobile/lib/push/registration.ts`).
 *
 * #1423: a DM could arrive with no notification because registration could
 * silently stick. A failed `register_push_token` still marked the user
 * registered for the session, and a rotated token never reached the DS. These
 * pin the fix: registered means the DS ACCEPTED this token, a failure is
 * retried, the quiet triggers never prompt, and a rotation re-registers.
 */

import test from "node:test";
import assert from "node:assert/strict";

import {
  createPushRegistrar,
  type PushPermission,
  type RegistrationDeps,
} from "../lib/push/registration.ts";

interface Fake {
  deps: RegistrationDeps;
  permission: PushPermission;
  token: string | null;
  registerFails: boolean;
  registered: Array<[string, string]>;
  prompts: number;
}

function fake(overrides: Partial<Pick<Fake, "permission" | "token" | "registerFails">> = {}): Fake {
  const f: Fake = {
    permission: { granted: true, canAskAgain: true },
    token: "ExponentPushToken[one]",
    registerFails: false,
    registered: [],
    prompts: 0,
    ...overrides,
    deps: undefined as unknown as RegistrationDeps,
  };
  f.deps = {
    getPermissions: async () => f.permission,
    requestPermissions: async () => {
      f.prompts += 1;
      f.permission = { granted: true, canAskAgain: false };
      return { granted: true };
    },
    prepare: async () => {},
    getToken: async () => f.token,
    register: async (userId, token) => {
      if (f.registerFails) {
        throw new Error("network");
      }
      f.registered.push([userId, token]);
    },
    warn: () => {},
  };
  return f;
}

test("a failed register is not marked, and the next trigger retries it", async () => {
  const f = fake({ registerFails: true });
  const r = createPushRegistrar(f.deps);

  assert.equal(await r.ensure("alice", { prompt: false }), null);
  assert.equal(r.registeredToken("alice"), undefined);

  f.registerFails = false;
  assert.equal(await r.ensure("alice", { prompt: false }), "ExponentPushToken[one]");
  assert.deepEqual(f.registered, [["alice", "ExponentPushToken[one]"]]);
  assert.equal(r.registeredToken("alice"), "ExponentPushToken[one]");
});

test("once registered, the quiet triggers are a no-op", async () => {
  const f = fake();
  const r = createPushRegistrar(f.deps);
  await r.ensure("alice", { prompt: false });
  await r.ensure("alice", { prompt: false });
  await r.ensure("alice");
  assert.equal(f.registered.length, 1);
});

test("sign-in, unlock and foreground never fire the OS prompt", async () => {
  const f = fake({ permission: { granted: false, canAskAgain: true } });
  const r = createPushRegistrar(f.deps);

  assert.equal(await r.ensure("alice", { prompt: false }), null);
  assert.equal(f.prompts, 0);
  assert.equal(f.registered.length, 0);

  // The contextual moment (a chat open) may prompt, and then registers.
  assert.equal(await r.ensure("alice", { prompt: true }), "ExponentPushToken[one]");
  assert.equal(f.prompts, 1);
});

test("a permission granted later in Settings is picked up on the next foreground", async () => {
  const f = fake({ permission: { granted: false, canAskAgain: false } });
  const r = createPushRegistrar(f.deps);
  assert.equal(await r.ensure("alice", { prompt: true }), null);
  assert.equal(f.prompts, 0, "an answered prompt is never shown again");

  f.permission = { granted: true, canAskAgain: false };
  assert.equal(await r.ensure("alice", { prompt: false }), "ExponentPushToken[one]");
});

test("a token rotation re-registers the new token", async () => {
  const f = fake();
  const r = createPushRegistrar(f.deps);
  await r.ensure("alice", { prompt: false });

  f.token = "ExponentPushToken[two]";
  assert.equal(await r.ensure("alice", { prompt: false, refresh: true }), "ExponentPushToken[two]");
  assert.deepEqual(f.registered, [
    ["alice", "ExponentPushToken[one]"],
    ["alice", "ExponentPushToken[two]"],
  ]);
  assert.equal(r.registeredToken("alice"), "ExponentPushToken[two]");
});

test("a rotation event that leaves the Expo token unchanged posts nothing", async () => {
  const f = fake();
  const r = createPushRegistrar(f.deps);
  await r.ensure("alice", { prompt: false });
  await r.ensure("alice", { prompt: false, refresh: true });
  assert.equal(f.registered.length, 1);
});

test("triggers that land together POST once", async () => {
  const f = fake();
  const r = createPushRegistrar(f.deps);
  await Promise.all([
    r.ensure("alice", { prompt: false }),
    r.ensure("alice", { prompt: false }),
    r.ensure("alice", { prompt: false }),
  ]);
  assert.equal(f.registered.length, 1);
});

test("a rotation during a registration ends with the newer token registered", async () => {
  const f = fake();
  const r = createPushRegistrar(f.deps);
  const first = r.ensure("alice", { prompt: false });
  f.token = "ExponentPushToken[two]";
  const rotated = r.ensure("alice", { prompt: false, refresh: true });
  await Promise.all([first, rotated]);
  assert.equal(r.registeredToken("alice"), "ExponentPushToken[two]");
});

test("a getToken failure is not marked and does not throw", async () => {
  const f = fake();
  f.deps.getToken = async () => {
    throw new Error("no fcm");
  };
  const r = createPushRegistrar(f.deps);
  assert.equal(await r.ensure("alice", { prompt: false }), null);
  assert.equal(r.registeredToken("alice"), undefined);
});

test("registration is per user, so switching accounts registers again", async () => {
  const f = fake();
  const r = createPushRegistrar(f.deps);
  await r.ensure("alice", { prompt: false });
  await r.ensure("bob", { prompt: false });
  assert.deepEqual(f.registered, [
    ["alice", "ExponentPushToken[one]"],
    ["bob", "ExponentPushToken[one]"],
  ]);
});
