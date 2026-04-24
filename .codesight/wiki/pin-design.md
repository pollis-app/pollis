# PIN and Session Cleanup

Working design note. Not for commit. Iterate freely.

## Problem statement

The current login/session model has three overlapping failure modes that all feed the "user gets bounced back to email + OTP for no good reason" bug class (issue #184).

1. **Duplicate source of truth for "who was signed in."**
   - `accounts.json` (`src-tauri/src/accounts.rs`) holds `last_active_user` plus an entry per user.
   - The OS keystore slot `session_{user_id}` holds a serialized `UserProfile` (id, email, username, plus two derived flags).
   - Every successful auth writes to both (auth.rs:275-280, auth.rs:549-554).
   - `get_session` (auth.rs:310-468) reads *both*, and if either one is missing or unparseable it returns `Ok(None)` or `Err(...)`, either of which kicks the frontend back to the OTP screen. Issue #184 is exactly this: keystore read can transiently fail (macOS keychain hiccup, Linux secret-service race, POLLIS_DATA_DIR namespace drift in dev) while `accounts.json` is fine, and the user sees a login prompt despite having perfectly good local keys.
   - The `session_*` blob contains nothing that isn't already in `accounts.json` plus one runtime-recomputable boolean (`enrollment_required`, recomputed at auth.rs:459-465 anyway). The blob is redundant. It exists because it was the first thing written, and `accounts.json` was bolted on later.

2. **Silent parse failures.**
   - `accounts.rs:31`: `serde_json::from_str(&data).unwrap_or_default()`. A malformed `accounts.json` (truncated write, disk-full mid-write, encoding bug) is silently replaced with an empty index. The next `upsert_account` then overwrites the bad file with a one-entry index, permanently losing the record of any other accounts on the device.
   - `keystore.rs:69`: same pattern for the dev-mode JSON keystore.
   - `db/local.rs:38-49`: if the schema-version query fails for *any* reason on an existing encrypted DB file, the file is deleted. The comment only anticipates "wrong key or corrupt," but any rusqlite error (lock contention at the wrong moment, a SQLCipher version mismatch at build time) hits the same path. This is a footgun whether or not it's causing #184 today — it can eat the local DB at any time.

3. **Non-atomic writes.** `accounts.rs:42` uses `std::fs::write`, which truncates before writing. A crash between truncate and write leaves a zero-byte file; combined with (2) that's a silent "every account on this device is gone."

4. **Legacy keystore slots.** `identity_key_private` / `identity_key_public` are deleted in four places (auth.rs:647-648, 883-884) but never read or written anywhere in the current tree. Dead code held together by habit.

5. **OTP doing too much.** Today OTP is the only unlock path. Every cold start where the `session_*` read hiccups forces a full email round trip. OTP should be for proving you own an email address, not for "unlock the keys already on this device."

## Design decisions (not up for re-debate)

- Keep the Secret Key recovery flow (`account_identity.rs`) as-is.
- Introduce a local-only 4-digit PIN that wraps per-user secrets on this device.
- Delete the `session` concept entirely. `UserProfile` is reconstructed from `accounts.json` plus an in-memory unlock bit.
- Remove the four legacy `identity_key_*` delete sites and the `SESSION_KEY` constant.
- Make `accounts.json` writes crash-safe and loud on parse failure.
- OTP's only jobs going forward: brand-new-user signup, new-device enrollment via Secret Key, future email-change flow.

Out of scope for this doc (one-liners):
- Ephemeral "borrowed device" sign-in via OTP without PIN — possibly future.
- Biometric unlock — Linux/Windows parity makes this awkward; PIN is universal.
- Any server-side knowledge of the PIN. The PIN never leaves the device.

## End-state data model

What is stored, where, after this change lands.

### OS keychain (release) / `dev-keystore.json` (debug)

Per-user slots, keyed by `{slot}_{user_id}`:

- `device_id_{user_id}` — unchanged. Plain bytes, the device ULID.
- `db_key_wrapped_{user_id}` — NEW. The SQLCipher key for `pollis_{user_id}.db`, wrapped under PIN-derived material. Blob format below.
- `account_id_key_wrapped_{user_id}` — NEW. The Ed25519 account identity key (currently stored raw at `account_id_key_{user_id}`), wrapped under PIN-derived material.
- `pin_meta_{user_id}` — NEW. Non-secret PIN metadata: version byte, Argon2 params, salt, failed-attempt counter, last-attempt timestamp. Same blob format as the wrapped-key blobs but the ciphertext is a fixed magic string so the app can prove the PIN decrypts correctly without unwrapping the big keys first.

Removed slots:
- `session_{user_id}` — gone. Never written, the load-site is deleted.
- `identity_key_private`, `identity_key_public` (global) — gone. Never written, four delete sites removed.
- `db_key_{user_id}` (unwrapped) — replaced by `db_key_wrapped_{user_id}`.
- `account_id_key_{user_id}` (unwrapped) — replaced by `account_id_key_wrapped_{user_id}`.

### `accounts.json`

Single source of truth for "which users have ever signed in on this device" and UI snapshot fields. Shape:

```json
{
  "accounts": [
    {
      "user_id": "...",
      "username": "...",
      "email": "...",
      "avatar_url": null,
      "last_seen": "...",
      "pin_set": true
    }
  ],
  "last_active_user": "..."
}
```

New field: `pin_set: bool`. Lets the login screen decide whether to prompt for PIN or route to "you haven't set a PIN on this device yet, verify by OTP to set one." Derived from "does `pin_meta_{user_id}` exist in the keystore at the time `accounts.json` is last written"; not load-bearing — the PIN-entry command can also probe the keystore directly.

### Local SQLCipher DB

Unchanged shape (`pollis_{user_id}.db`). Still opened with a 32-byte random key. Only difference: that key lives at rest as ciphertext in `db_key_wrapped_{user_id}`, and is unwrapped into an in-memory `Vec<u8>` when the PIN is entered.

### In-memory AppState

New field roughly:

```rust
pub struct UnlockState {
    pub user_id: String,
    pub db_key: Zeroizing<Vec<u8>>,
    pub account_id_key: Zeroizing<SigningKey>, // currently reloaded from keystore on demand — becomes a held value
}

pub unlock: Arc<Mutex<Option<UnlockState>>>,
```

`lock()` drops this; `unlock(...)` populates it. Every existing call site that does `state.keystore.load_for_user("account_id_key", ...)` either goes through `UnlockState` or fails cleanly with "locked."

## PIN derivation scheme

4-digit PIN has ~13 bits of entropy. On-disk cost must be high enough that offline brute-force of the wrapped blob is infeasible on commodity hardware, *and* we rate-limit attempts at the app layer and nuke keys after N failures.

### KDF

**Argon2id** via the `argon2` crate (already a transitive dep via `password-hash`, confirm before implementing).

Target parameters, tuned to ~250ms on a mid-range M1 / Ryzen 5 laptop:

- `m_cost` = 64 MiB (65536 KiB)
- `t_cost` = 3
- `p_cost` = 1
- Output: 32 bytes

Parameters stored in the wrapped blob so we can bump them later without migrating.

Alternative: scrypt (N=2^17, r=8, p=1). Argon2id is preferred; scrypt only if the argon2 crate lands us in dependency hell.

### AEAD

**XChaCha20-Poly1305** (`chacha20poly1305` crate). 24-byte nonce avoids nonce-reuse paranoia across the small number of wrap events. AES-256-GCM is a fine fallback; we already use `aes-gcm` in `account_identity.rs` for the Secret Key flow, so pulling `chacha20poly1305` in is an add.

32-byte KEK from Argon2id output is used directly as the AEAD key.

### Wrapped blob format

Bincode or length-prefixed concatenation. Pick one and commit; proposal is a fixed byte layout (no bincode dependency, no serde overhead on a hot path):

```
offset  size  field
0       1     version        = 1
1       1     kdf_id         = 1 (argon2id)
2       4     m_cost_kib     (u32 BE)
6       1     t_cost
7       1     p_cost
8       1     salt_len       = 16
9       16    salt
25      1     aead_id        = 1 (xchacha20poly1305)
26      1     nonce_len      = 24
27      24    nonce
51      2     ct_len         (u16 BE)
53      ..    ciphertext||tag
```

Stored base64 under the keyring entry (matches existing keystore encoding at `keystore.rs:87, 141`).

### PIN metadata blob (`pin_meta_{user_id}`)

Same byte layout, but the plaintext is the 16-byte ASCII string `pollis-pin-ok\0\0\0`. Purpose: verify a submitted PIN is correct without having to unwrap two big keys. Also carries the rate-limit counter and timestamp, which sit *outside* the ciphertext (they aren't secret; the threat model is a local brute-forcer who already has keystore read access, and they can count attempts themselves). Append after the main blob:

```
offset  size  field
N       4     failed_attempts (u32 BE)
N+4     8     last_attempt_unix_secs (u64 BE)
```

Updated atomically via `store_for_user` on every attempt (success resets counter to 0).

## PIN lifecycle

### Set (first signup, first-device)

Happens inside `verify_otp` once the user row is created and `account_id_key` is generated. The command does not *return* a session — it returns a profile plus a flag `pin_required_to_complete_signup: true`. Frontend collects the PIN, calls `set_pin(old_pin=None, new_pin=...)`, which:

1. Generates a random 32-byte `db_key` (replaces the current unwrapped path at `state.rs:82-88`).
2. Derives KEK from PIN.
3. Wraps `db_key` → `db_key_wrapped_{user_id}`.
4. Reads the just-generated raw `account_id_key`, wraps it → `account_id_key_wrapped_{user_id}`, deletes the raw slot.
5. Writes `pin_meta_{user_id}`.
6. Populates `AppState.unlock`.
7. Updates `accounts.json` with `pin_set: true`.

### Set (new device via Secret Key recovery)

`enroll_with_secret_key` (or whatever the current recovery command is named in `account_identity.rs` callers) ends by handing the frontend the raw account key in memory. Frontend immediately prompts for a new PIN, calls `set_pin`. Same wrap path.

### Change

`set_pin(old_pin=Some, new_pin)`:
1. Unwrap both keys with `old_pin`.
2. Rewrap under `new_pin` with freshly-random salts and nonces.
3. Overwrite the wrapped blobs.
4. Failure between step 2 and 3 is safe because we only write *after* both unwraps succeed and both rewraps are computed.

### Verify / unlock

`unlock(user_id, pin)`:
1. Load `pin_meta_{user_id}`. If missing → return `PinNotSet` error.
2. Check `failed_attempts` vs threshold. If locked out → return `PinLockedOut`.
3. Derive KEK, attempt to decrypt the magic-string plaintext.
4. On success: decrypt `db_key_wrapped_*` and `account_id_key_wrapped_*`, populate `AppState.unlock`, reset counter to 0 in `pin_meta_*`, open local DB via existing `state.load_user_db` path (refactored to take the already-unwrapped key), return `UserProfile`.
5. On failure: increment counter, store, return `PinIncorrect { attempts_remaining: N }`.

### Forgotten PIN

The user has two recovery paths, neither of which involves the server knowing the PIN:

1. **Any other device they're already signed into on.** Out of scope here but the door is open: a future `device_link` flow can ship an encrypted payload from the old device to the new one, and the user sets a fresh PIN on arrival.
2. **Secret Key recovery.** Already implemented. Wipes this device's local state, unwraps the account key from `account_recovery` using the Secret Key, then sets a fresh PIN. See `account_identity.rs:155-181`.

Pressing "forgot PIN" in the UI is equivalent to "I'm enrolling this device from scratch" — wipe the wrapped blobs, wipe the local DB (it's unreadable without `db_key` anyway), drop the `accounts.json` entry's `pin_set` to false, route to the Secret Key recovery screen.

### Rate limit

Proposed threshold: **10 wrong attempts**, no time-based backoff (4 digits, 10 attempts is 0.1% guess rate — fine). On the 10th wrong attempt:

1. Delete `db_key_wrapped_{user_id}`, `account_id_key_wrapped_{user_id}`, `pin_meta_{user_id}`.
2. Delete `pollis_{user_id}.db` (and `-wal`, `-shm`).
3. Remove the account from `accounts.json` OR keep it with `pin_set: false` — open question, see below.
4. Device is now "known email, must re-enroll via OTP or Secret Key."

Critically: the Turso-side account is untouched. Other devices for the same user keep working. The frontend needs a warning screen before attempt 10 actually fires.

## New and changed commands

Registered in `src-tauri/src/lib.rs`, implemented in `src-tauri/src/commands/auth.rs` (or a new `pin.rs` — prefer keeping auth-adjacent things in `auth.rs`).

- `set_pin(old_pin: Option<String>, new_pin: String) -> Result<()>`
  - Initial set and change. See lifecycle above.
- `unlock(user_id: String, pin: String) -> Result<UserProfile>`
  - Replaces the keystore-read path in today's `get_session` (auth.rs:333-351). Returns the same `UserProfile` shape, reconstructed from `accounts.json` + Turso verification + freshly-recomputed `enrollment_required`.
- `lock() -> Result<()>`
  - Drops `AppState.unlock` and calls `state.unload_user_db()`. Does not touch `accounts.json` — `last_active_user` persists so the login screen can still offer the "continue as X" chip. This is what current `logout(delete_data=false)` becomes (auth.rs:623-665 loses its session-slot delete and its `clear_last_active_user` call).
- `get_unlock_state() -> Result<UnlockStateSnapshot>`
  - Replaces `get_session`. Returns `{ last_active_user_id, is_unlocked, user_profile }` where `user_profile` is only populated if unlocked. Frontend uses this to decide between "PIN entry for user X," "PIN setup flow," or "full login screen." Never blocks on keystore reads — only reads `accounts.json` and in-memory state.
- `verify_otp` — returns `UserProfile { pin_required_to_complete_signup: bool, new_secret_key, ... }`. No longer stores a session blob. The "load_user_db" + "register_device" calls move to happen *after* `set_pin` completes, since the local DB can't be opened without the `db_key` and we now gate that on the PIN being set.
- `request_otp` — unchanged except that its role is documented to shrink to signup/recovery/email-change.
- `logout(delete_data: bool)` — semantics change:
  - `delete_data=false` → delegates to `lock()`. Keeps the account in `accounts.json`.
  - `delete_data=true` → unchanged in spirit but now nukes the wrapped blobs instead of the raw `db_key` / `account_id_key`. The four `identity_key_*` delete calls (auth.rs:647-648, 883-884) are removed.
- `wipe_local_data` — updated to iterate the new slot names: `pin_meta`, `db_key_wrapped`, `account_id_key_wrapped`, `device_id`. Drops `session` and `identity_key_*` from its enumeration (auth.rs:875-884).

## `accounts.json` changes

All in `src-tauri/src/accounts.rs`.

1. **Atomic write.** `write_accounts_index`:
   ```
   let tmp = path.with_extension("json.tmp");
   write tmp
   File::open(&tmp)?.sync_all()?;   // fsync
   std::fs::rename(&tmp, &path)?;
   ```
   POSIX rename is atomic; on Windows use `std::fs::rename` as well (atomic on NTFS since API is `MoveFileEx` with `MOVEFILE_REPLACE_EXISTING` under the hood in newer Rust).

2. **Loud parse failure.** `read_accounts_index`:
   ```
   let data = match fs::read_to_string(&path) { Ok(d) => d, Err(e) if not-found => return default, Err(e) => return Err(...) };
   match serde_json::from_str(&data) {
       Ok(idx) => Ok(idx),
       Err(e) => {
           let bad = path.with_extension(format!("bad-{timestamp}.json"));
           let _ = fs::rename(&path, &bad);
           Err(AccountsIndexCorrupt { backup_path: bad })
       }
   }
   ```
   Change the signature to `Result<AccountsIndex>`. All callers currently at auth.rs:324, 624, 855, 872 need to handle the error. The correct behavior on corruption is to surface a dedicated error variant to the frontend so it can say "local data was corrupted — we backed it up to X, please sign in."

3. **Add `pin_set: bool`** to `AccountInfo`. Updated on every successful `set_pin` or on the rate-limit nuke.

4. **Same fixes to the dev keystore JSON** (`keystore.rs:66-83`): atomic write, loud parse failure, backup on corruption. Release builds use the OS keychain and are already safe.

## Migration

Existing installs hit a version with:
- `session_{user_id}` present.
- `db_key_{user_id}` present, unwrapped.
- `account_id_key_{user_id}` present, unwrapped.
- No PIN.

First launch post-upgrade:

1. `get_unlock_state` inspects `pin_meta_{last_active_user}`. Missing → caller is on an unmigrated install.
2. Frontend shows a one-time "Set a PIN to finish updating Pollis" screen. No skip.
3. User enters a PIN. Frontend calls a new command `migrate_set_initial_pin(new_pin: String)`:
   a. Read `session_{user_id}` — if missing, abort and surface the error (user should sign in fresh).
   b. Read unwrapped `db_key_{user_id}` and `account_id_key_{user_id}`.
   c. Derive KEK, wrap both, write `db_key_wrapped_*` and `account_id_key_wrapped_*`.
   d. Write `pin_meta_*`.
   e. Only after all three writes succeed: delete `session_{user_id}`, `db_key_{user_id}`, `account_id_key_{user_id}`, `identity_key_private`, `identity_key_public`. (Keychain delete failures are logged and ignored — the wrapped versions now take precedence.)
   f. Update `accounts.json` with `pin_set: true` (also triggers the atomic-write path for the first time).
4. Proceed to normal unlocked state.

If the user has multiple accounts in `accounts.json`, the migration runs opportunistically per account: first time each is selected, if `pin_meta_*` is missing but `session_*` / unwrapped keys exist, run the same flow.

Crash recovery during migration: if we crash between (c) and (e), the old unwrapped slots still exist — next launch runs migration again with the same inputs and is idempotent (same `db_key` and `account_id_key` wrap to a new-but-valid blob). The `session_*` slot being still present is the signal "not yet migrated," and its deletion is the commit.

Migration deadline: keep the migration code in the tree for two minor versions, then delete. Users who skip more than two versions are asked to re-authenticate.

## Frontend flow sketch

Screens (existing where named, new otherwise). Living in `frontend/src/pages/` or `frontend/src/components/auth/`.

- **PIN entry** — shown when `get_unlock_state` returns `{ last_active_user, is_unlocked: false, pin_set: true }`. Numeric keypad, 4 boxes, "forgot?" link. Calls `unlock`. On `PinIncorrect` shows remaining attempts. On `PinLockedOut` (or when remaining hits 0) routes to the Secret Key recovery screen.
- **PIN setup** — shown right after `verify_otp` returns `pin_required_to_complete_signup: true`, and at the end of Secret Key recovery, and during one-time migration. Two passes (enter + confirm). Calls `set_pin(None, pin)` or `migrate_set_initial_pin(pin)` depending on context.
- **PIN change** — Settings page. Two passes plus old-PIN pass. Calls `set_pin(Some(old), new)`.
- **PIN failure lockout warning** — interstitial when attempts remaining hits 3, 2, 1. Explains what happens on 10.
- **Login screen (existing)** — gets a minor update: instead of just "enter email," it shows the list from `list_known_accounts`, each with a "continue" chip. Click one → PIN entry. "Use different account" → current email-entry flow.

Commands the frontend uses:
- On app boot: `get_unlock_state`, then route.
- PIN entry: `unlock`.
- Signup: `request_otp`, `verify_otp`, `set_pin`, `initialize_identity`.
- New device via Secret Key: existing enrollment commands, then `set_pin`, then `initialize_identity`.
- Manual lock: `lock`.
- Full logout: `logout(delete_data=false)` (which is now an alias for lock) or `logout(delete_data=true)`.

## Testing

All scenarios to add to `src-tauri/tests/flows.rs` under the `test-harness` feature. Each uses `tauri::test::get_ipc_response` against the real commands.

- `pin_set_lock_unlock_roundtrip` — signup → set_pin → lock → unlock with correct PIN → assert `UserProfile` matches and local DB is queryable.
- `pin_wrong_counter_and_lockout` — 9 wrong attempts return `PinIncorrect` with correct `attempts_remaining`; 10th wipes wrapped blobs and local DB; next `unlock` returns `PinNotSet`.
- `pin_change_roundtrip` — set → change (old, new) → old PIN fails → new PIN unlocks.
- `pin_migration_from_pre_upgrade_state` — harness helper that seeds `session_*` + unwrapped `db_key` + unwrapped `account_id_key` in the `InMemoryKeystore`, then asserts `get_unlock_state` reports "needs migration," `migrate_set_initial_pin` succeeds, old slots are gone, new wrapped slots are present.
- `accounts_json_crash_mid_write_recovers` — simulate by writing a malformed file under the tempdir path before calling `upsert_account`; assert the error is surfaced and the `.bad-{ts}.json` backup exists.
- `accounts_json_atomic_write` — use `POLLIS_DATA_DIR` override, write a big index, kill the process mid-write by intercepting the fsync/rename... or more realistically, assert the `.json.tmp` file never coexists with a valid `.json` at the observable API level (the atomic-rename property).
- `legacy_identity_slots_are_not_read_or_written` — harness helper that fails loudly if `identity_key_private` / `identity_key_public` keys appear in the `InMemoryKeystore` after any command runs.
- `lock_drops_keys_from_memory` — set_pin → lock → assert `AppState.unlock` is `None` and that a DB-touching command returns the locked error.

## What gets deleted

Grep targets, then audit each hit:

- `const SESSION_KEY: &str = "session";` — `src-tauri/src/commands/auth.rs:10`. Delete.
- All `SESSION_KEY` uses: auth.rs:277, 333, 372, 551, 631, 839, 875. Delete.
- `identity_key_private` / `identity_key_public` string literals: auth.rs:647-648, auth.rs:883-884. Delete.
- `.unwrap_or_default()` on parse: `accounts.rs:31`, `keystore.rs:69`. Replace with loud-failure variants described above.
- `std::fs::write(...)` on `accounts.json`: `accounts.rs:42`. Replace with tempfile+fsync+rename.
- Unwrapped `db_key` generation at `state.rs:82-88`. Becomes an unwrap-existing-wrapped-blob path; the random-generate path moves into `set_pin`.
- Raw `account_id_key` keystore writes in `account_identity.rs` (`state.keystore.store_for_user(ACCOUNT_ID_KEY_KEYSTORE_SLOT, user_id, &private_bytes)` at lines 213 and 372). They become "hand the raw bytes back to the caller, which will wrap them." The slot name constant becomes `account_id_key_wrapped` and the load helper (`load_account_id_key`, line 392) changes to "read from `AppState.unlock`" — failing cleanly if locked.
- `db/local.rs:38-49` silent-delete on schema-version query failure. Narrow it: only delete when the specific `sqlcipher: wrong key` error fires; any other error surfaces. This is not strictly part of PIN work but it's in the same failure domain and the redesign is the right time to fix it.

## Open questions

- **PIN length:** 4 vs 6 digits. 4 matches phone muscle memory; 6 raises the offline-brute-force bar meaningfully (but Argon2id already handles offline brute-force, so the practical gain is smaller than it looks). Lean 4.
- **Alphanumeric option:** worth offering? Adds UI complexity. Default no.
- **Lockout threshold:** proposed 10. Could be 5. Could also grow a backoff (1s, 5s, 30s, 5min) before the permanent nuke. Lean "no backoff, 10 then nuke" for simplicity.
- **Same PIN across multi-account devices or per-account PIN?** Per-account is cleaner cryptographically (separate Argon2 salts, separate KEKs, no cross-account leakage if one account is compromised elsewhere). Per-device is friendlier UX. Lean per-account — multi-account users on shared devices are rare, and the salt-per-account design falls out naturally from the wrapped-blob-per-user layout.
- **Surface `pin_set` to the login picker?** Probably yes — the chip for a user without a PIN should look different ("tap to set up on this device") than one with a PIN ("tap to enter PIN"). Minor UX call.
- **"Remember for N minutes" soft-lock?** If the app backgrounds for 30s, do we re-prompt for PIN? Most chat apps don't; the desktop app especially shouldn't. Lean "no auto-lock, only explicit lock command and explicit logout."
- **Do we show PIN attempts remaining to the user, or just count down silently?** Showing it is more honest; hiding it makes brute-force tooling slightly less easy. Lean show — the Argon2id cost is already the real defense.
- **Migration visibility:** if migration fails mid-way, what does the frontend show? A recoverable error path ("re-enter OTP to start fresh") needs a design pass.
