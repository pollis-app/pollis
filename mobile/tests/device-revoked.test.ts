// #1338: `noticeDeviceRevoked` ends the session exactly once, only for that error.
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  getSessionNotice,
  isDeviceRevokedError,
  noticeDeviceRevoked,
  onDeviceRevoked,
  setSessionNotice,
  subscribeSessionNotice,
} from "../lib/deviceRevoked.ts";

const settle = () => new Promise((resolve) => setTimeout(resolve, 0));

test("the core's error is recognised however it was wrapped", () => {
  assert.ok(isDeviceRevokedError(new Error("device_revoked")));
  assert.ok(isDeviceRevokedError("list_user_groups: device_revoked"));
  assert.ok(!isDeviceRevokedError(new Error("ds_post /v1/directory/bootstrap 401 Unauthorized")));
  assert.ok(!isDeviceRevokedError(new Error("network down")));
});

test("many failing commands end the session once", async () => {
  let calls = 0;
  const off = onDeviceRevoked(async () => {
    calls += 1;
    await settle();
  });
  noticeDeviceRevoked(new Error("device_revoked"));
  noticeDeviceRevoked(new Error("device_revoked"));
  noticeDeviceRevoked("device_revoked");
  await settle();
  await settle();
  assert.equal(calls, 1);
  off();
});

test("other failures never end the session", async () => {
  let calls = 0;
  const off = onDeviceRevoked(() => {
    calls += 1;
  });
  noticeDeviceRevoked(new Error("ds_post /v1/read/vault 401 Unauthorized"));
  noticeDeviceRevoked(new Error("timeout"));
  await settle();
  assert.equal(calls, 0);
  off();
});

test("a later revocation runs the handler again once the first has finished", async () => {
  let calls = 0;
  const off = onDeviceRevoked(() => {
    calls += 1;
  });
  noticeDeviceRevoked(new Error("device_revoked"));
  await settle();
  await settle();
  noticeDeviceRevoked(new Error("device_revoked"));
  await settle();
  await settle();
  assert.equal(calls, 2);
  off();
});

test("the sign-in notice is shared state that screens subscribe to", () => {
  let seen = 0;
  const unsubscribe = subscribeSessionNotice(() => {
    seen += 1;
  });
  setSessionNotice("removed");
  assert.equal(getSessionNotice(), "removed");
  setSessionNotice(null);
  assert.equal(getSessionNotice(), null);
  assert.equal(seen, 2);
  unsubscribe();
});
