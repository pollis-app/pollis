/* When the app sends its push token to the DS (#1423): registered means ACCEPTED. */

import test from "node:test";
import assert from "node:assert/strict";

import {
  createPushRegistrar,
  createTokenEchoFilter,
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

test("the echo of our own token request is not a rotation; a new token is (#1610)", () => {
  const echo = createTokenEchoFilter();
  // getToken asked for the device token; iOS then fires the token event with that same token.
  echo.noteOwn("apns-a");
  assert.equal(echo.isRotation("apns-a"), false, "our own request's echo must not re-register");
  echo.confirmOwn();
  assert.equal(echo.isRotation("apns-a"), false);
  // The OS rotated it.
  assert.equal(echo.isRotation("apns-b"), true);
  // The refresh it triggers obtains apns-b; its echo is not another rotation.
  echo.noteOwn("apns-b");
  assert.equal(echo.isRotation("apns-b"), false);
});

test("a rotation whose register fails is re-registered on the next foreground (#1610)", async () => {
  const f = fake({ token: "ExponentPushToken[one]" });
  const r = createPushRegistrar(f.deps);
  const echo = createTokenEchoFilter();
  const getToken = f.deps.getToken;
  // The device token tracks the Expo token in this fake.
  f.deps.getToken = async () => {
    const t = await getToken();
    if (t) {
      echo.noteOwn(`device:${t}`);
    }
    return t;
  };
  const register = f.deps.register;
  f.deps.register = async (userId, token) => {
    await register(userId, token);
    echo.confirmOwn();
  };

  // Landing: registered, accepted.
  await r.ensure("alice", { prompt: false });
  assert.equal(r.registeredToken("alice"), "ExponentPushToken[one]");

  // The OS rotates the token; the event is a rotation, but the DS refuses the register.
  f.token = "ExponentPushToken[two]";
  f.registerFails = true;
  assert.equal(echo.isRotation("device:ExponentPushToken[two]"), true);
  assert.equal(await r.ensure("alice", { prompt: false, refresh: true }), null);
  // Its own echo must not loop it...
  assert.equal(echo.isRotation("device:ExponentPushToken[two]"), false);
  // ...and the user is no longer counted registered with the dead token.
  assert.equal(r.registeredToken("alice"), undefined);

  // Next foreground: a quiet ensure re-registers the new token.
  f.registerFails = false;
  assert.equal(await r.ensure("alice", { prompt: false }), "ExponentPushToken[two]");
  assert.deepEqual(f.registered.at(-1), ["alice", "ExponentPushToken[two]"]);
  assert.equal(r.registeredToken("alice"), "ExponentPushToken[two]");
});
