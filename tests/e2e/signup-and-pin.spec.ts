// Minimum-viable E2E: launch the real Electron binary, sign in via the
// debug-build `DEV_EMAIL` short-circuit, complete the PIN-create flow,
// and assert the renderer reaches the main app shell.
//
// What this exercises end-to-end:
//   1. `pollis-node` loads into the Electron main process.
//   2. The preload bridge wires every `ipcMain.handle` (registration
//      failures crash the boot before the renderer can mount).
//   3. The React shell loads against the built frontend bundle.
//   4. `verify_otp` (via `dev_login_inner`) writes the user row to the
//      shared test Turso DB.
//   5. `set_pin` derives the KEK via Argon2id and writes the wrapped
//      keystore slots (via the debug-build JSON-file backend, scoped to
//      the per-test `POLLIS_DATA_DIR`).
//   6. `App.tsx` transitions through the auth state machine and ends on
//      `data-testid="app-ready"`.
//
// Faster than the Rust harness it is not — but it covers the layers the
// Rust harness can't see: preload contract, renderer mount, IPC wire
// shape across the JSON boundary, and the React state machine.

import { test } from "@playwright/test";
import { signUpAndUnlock } from "./helpers/auth";
import { dispose, launchPollis, uniqueSuffix, type LaunchedApp } from "./helpers/launch";
import { wipeTestTurso } from "./helpers/turso";

let alice: LaunchedApp;

test.beforeAll(async () => {
  // Wipe at the suite boundary, not per-test: this suite has only one
  // scenario today and resetting per-test would slow follow-up scenarios
  // we add to this file. Per-suite is the same isolation guarantee the
  // Rust harness's `wipe_remote` provides — every scenario starts with
  // an empty Turso.
  await wipeTestTurso();
});

test.afterAll(async () => {
  if (alice !== undefined) {
    await dispose(alice);
  }
});

test("signs up via DEV_EMAIL, sets a PIN, lands on the main app shell", async () => {
  const email = `alice-${uniqueSuffix()}@e2e.local`;
  alice = await launchPollis({ devEmail: email });
  await signUpAndUnlock(alice);
});
