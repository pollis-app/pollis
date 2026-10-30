/* Push registration waits for the unlock (#1620): no attempt before it, one after it, no spin. */

import test from "node:test";
import assert from "node:assert/strict";

import {
  createPushRegistrar,
  createRegistrationTriggers,
  createTokenEchoFilter,
  type EnsureOptions,
  type RegistrationDeps,
} from "../lib/push/registration.ts";

const settle = () => new Promise<void>((resolve) => setTimeout(resolve, 0));

/**
 * The real registrar, echo filter and triggers, wired the way `lib/push` and the hook wire them,
 * over a fake OS whose token request fires the token event (iOS and Android both do), and a DS
 * that refuses to sign until `canSign`.
 */
function device() {
  const d = {
    canSign: false,
    deviceToken: "dev-one",
    attempts: 0,
    accepted: [] as string[],
    warnings: [] as string[],
  };
  const echo = createTokenEchoFilter();
  const listeners: Array<(token: string) => void> = [];
  const emit = (token: string) => {
    for (const l of listeners) {
      l(token);
    }
  };
  const deps: RegistrationDeps = {
    getPermissions: async () => ({ granted: true, canAskAgain: false }),
    requestPermissions: async () => ({ granted: true }),
    prepare: async () => {},
    getToken: async () => {
      const token = d.deviceToken;
      // The OS resolves the request, then fires its token event with the same token.
      setTimeout(() => emit(token), 0);
      echo.noteOwn(token);
      return `ExponentPushToken[${token}]`;
    },
    register: async (_userId, token) => {
      d.attempts += 1;
      if (!d.canSign) {
        throw new Error("not signed in for DS request signing");
      }
      d.accepted.push(token);
      echo.confirmOwn();
    },
    warn: (message) => {
      d.warnings.push(message);
    },
  };
  const registrar = createPushRegistrar(deps);
  const ensured: EnsureOptions[] = [];
  const triggers = createRegistrationTriggers((userId, options) => {
    ensured.push(options);
    return registrar.ensure(userId, options);
  });
  // addPushTokenRotationListener: only a real rotation reaches the triggers.
  listeners.push((token) => {
    if (echo.isRotation(token)) {
      triggers.rotation();
    }
  });
  return { d, triggers, registrar, ensured, emit };
}

test("no registration is attempted while the session is locked (#1620)", async () => {
  const { d, triggers, ensured, emit } = device();

  // Cold start: signed in, PIN not yet entered.
  triggers.setUser(null);
  triggers.foreground();
  triggers.foreground();
  emit("dev-rotated");
  await settle();
  await settle();

  assert.equal(ensured.length, 0, "nothing may ask for a registration before the unlock");
  assert.equal(d.attempts, 0);
  assert.equal(d.warnings.length, 0);
});

test("the unlock registers exactly once, and the token event it causes does not repeat it", async () => {
  const { d, triggers, registrar } = device();
  triggers.setUser(null);

  d.canSign = true;
  triggers.setUser("alice");
  // Re-renders hand the same user again; the echo of our own token request arrives.
  triggers.setUser("alice");
  for (let i = 0; i < 10; i += 1) {
    await settle();
  }

  assert.equal(d.attempts, 1);
  assert.deepEqual(d.accepted, ["ExponentPushToken[dev-one]"]);
  assert.equal(registrar.registeredToken("alice"), "ExponentPushToken[dev-one]");
});

test("a failed attempt does not spin: it is retried only by the next real trigger", async () => {
  const { d, triggers, registrar } = device();

  // Unlocked, but the DS refuses (offline, signing not ready yet...).
  triggers.setUser("alice");
  for (let i = 0; i < 20; i += 1) {
    await settle();
  }
  assert.equal(d.attempts, 1, "a failure must not trigger another attempt by itself");
  assert.equal(d.warnings.length, 1);

  // The next foreground retries it, once.
  d.canSign = true;
  triggers.foreground();
  for (let i = 0; i < 10; i += 1) {
    await settle();
  }
  assert.equal(d.attempts, 2);
  assert.equal(registrar.registeredToken("alice"), "ExponentPushToken[dev-one]");

  // Registered: a later foreground posts nothing.
  triggers.foreground();
  await settle();
  assert.equal(d.attempts, 2);
});

test("a token rotated while locked is registered on the unlock", async () => {
  const { d, triggers, registrar, emit } = device();
  d.canSign = true;
  triggers.setUser("alice");
  for (let i = 0; i < 5; i += 1) {
    await settle();
  }
  assert.equal(d.attempts, 1);

  // Auto-lock, then the OS rotates the token while the PIN screen is up.
  triggers.setUser(null);
  d.deviceToken = "dev-two";
  emit("dev-two");
  await settle();
  assert.equal(d.attempts, 1, "still nothing while locked");

  triggers.setUser("alice");
  for (let i = 0; i < 5; i += 1) {
    await settle();
  }
  assert.equal(d.attempts, 2);
  assert.equal(registrar.registeredToken("alice"), "ExponentPushToken[dev-two]");
});

test("a rotation while unlocked re-registers once", async () => {
  const { d, triggers, registrar, emit } = device();
  d.canSign = true;
  triggers.setUser("alice");
  for (let i = 0; i < 5; i += 1) {
    await settle();
  }

  d.deviceToken = "dev-two";
  emit("dev-two");
  for (let i = 0; i < 10; i += 1) {
    await settle();
  }
  assert.equal(d.attempts, 2);
  assert.equal(registrar.registeredToken("alice"), "ExponentPushToken[dev-two]");
});

test("re-locking and unlocking with nothing new posts nothing", async () => {
  const { d, triggers } = device();
  d.canSign = true;
  triggers.setUser("alice");
  for (let i = 0; i < 5; i += 1) {
    await settle();
  }
  triggers.setUser(null);
  triggers.setUser("alice");
  for (let i = 0; i < 5; i += 1) {
    await settle();
  }
  assert.equal(d.attempts, 1);
});
