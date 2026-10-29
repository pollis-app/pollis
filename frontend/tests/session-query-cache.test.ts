/* Ending a session drops the query cache of decrypted bodies (#1256). Mobile: mobile/tests/session-teardown.test.ts. */

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { clearsQueryCache } from "../src/utils/queryCacheLifecycle.ts";

const APP = fileURLToPath(new URL("../src/App.tsx", import.meta.url));

test("locking and every session end clear the query cache", () => {
  assert.equal(clearsQueryCache("pin-entry"), true);
  assert.equal(clearsQueryCache("email-auth"), true);
});

test("a signed-in state does not", () => {
  assert.equal(clearsQueryCache("ready"), false);
  assert.equal(clearsQueryCache("logout-confirm"), false);
});

test("App clears on the transition, and every sign-out lands on email-auth", () => {
  const src = readFileSync(APP, "utf8");
  assert.match(src, /if \(clearsQueryCache\(appState\)\) \{\s*queryClient\.clear\(\);/);
  // A vanished user (revoked device) routes to email-auth, which clears.
  assert.match(src, /appState === "ready" && !currentUser\) \{\s*setAppState\("email-auth"\);/);
});
