/*
 * Ending a session drops the React Query cache (#1256).
 *
 * The cache holds decrypted message bodies and outlives AppShell. It was
 * cleared on lock only; sign-out, account deletion and the revoked-device
 * sign-out all land on "email-auth" and left it full. The mobile half of this
 * lives in mobile/tests/session-teardown.test.ts.
 *
 *   node --test frontend/tests/
 */

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
  // A signed-in session whose user disappears (the revoked-device path only
  // calls appStore.logout()) is routed to email-auth, which clears.
  assert.match(src, /appState === "ready" && !currentUser\) \{\s*setAppState\("email-auth"\);/);
});
