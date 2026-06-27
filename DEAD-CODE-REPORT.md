# Dead / Orphaned Code Report — Goal A/B + OTP read-only-client transition

Branch: `chore/remove-dead-write-paths`. **Discovery pass only — no code deleted.**

## Governing finding (read this first)

The transition used the seam `match state.config.pollis_delivery_url { Some(_) => <DS call>, None => <direct remote_db write> }` everywhere, plus a few `if pollis_delivery_url.is_none() { <direct write> }` guards. The central question the brief raised was whether the `None`/direct branches are *test-reachable* (and therefore not blind-deletable). **They are not.** Verified:

1. **The only code that constructs a client `AppState` and dispatches seamed commands is the flows harness** (`src-tauri/tests/flows/harness.rs:2190` via `AppState::new_with_parts`), and it **unconditionally sets `pollis_delivery_url = Some(delivery_url)`** (`harness.rs:132`). It also gives the client a **read-only DB view** (commit `d9da086`) specifically so any stray direct write *fails the test*. So flows exercise the `Some`/DS arm only.
2. **No `pollis-core` lib unit test ever builds an `AppState`.** `Config::for_test()` has exactly one caller in the whole repo — the flows harness (`harness.rs:66`), which then overrides the URL to `Some`. Grep for every `AppState::new` / `AppState::new_with_parts` / `with_keystore` caller: prod runtime (`src-tauri/src/lib.rs:349`, DS configured), the mobile bridge (`pollis-core/src/bridge.rs:172`, real env → DS in prod), and the flows harness. None is a `None`-config lib test.
3. The `#[tokio::test]` lib tests that *do* exist (`r2.rs`, `pin.rs`, `keystore.rs`) call **low-level helpers directly** (`r2_put`/`r2_get`, `unlock_inner`, wrap/unwrap) and never the seamed command — `pin.rs` even comments that "the tauri command wrapper requires a full AppState which is heavy to build." The ~213 lib tests are pure-logic / raw-`Connection` SQL tests (e.g. `auth.rs` `mod tests` operates on an in-memory `rusqlite::Connection`).
4. `multi_device_messaging.rs` is a pure OpenMLS test (`MlsGroup` directly) — no `AppState`, no seam.

**Conclusion:** every `None`/direct seam arm and every `*_direct` fallback is dead in **both** prod (DS always configured) **and** every test. The **TEST-ONLY-REACHABLE category is essentially empty for the seams** — it contains only two genuine test-infra items (`Backend::Local`/`connect_local` and the debug-gated `dev_login`), called out in Table B.

---

## Table A — TRULY DEAD (no caller in prod or any test; safe to delete)

### A1. Dedicated `*_direct` fallback functions (whole-function dead bodies)

| File:line | Symbol | Why dead |
|---|---|---|
| `pollis-core/src/commands/auth.rs:100-163` | `request_otp_direct` | Only caller is `request_otp`'s `None` arm (`auth.rs:95`). DS always configured in prod; no test calls `request_otp` on a `None` config (flows route it to `delivery_request_otp`). Contains the **only client-side use of `resend_api_key`** (see A4). |
| `pollis-core/src/commands/auth.rs:435-573` | `verify_otp_direct` | Only caller is `verify_otp`'s `None` arm (`auth.rs:182`). DS path is `verify_otp_ds`. No `None`-config test reaches it. |
| `pollis-core/src/commands/auth.rs:702-763` | `verify_email_change_direct` | Only caller is `verify_email_change`'s `None` arm (`auth.rs:674`). Flows use the DS route (`delivery_verify_email_change`). |
| `pollis-core/src/commands/mls/delivery.rs:93-…` | `direct_submit` | Only caller is `submit_commit`'s `None` arm (`delivery.rs:76`). Flows configure a DS → `http_submit` path only. |

### A2. Orphaned MLS command wrappers (transition leftovers — registered but zero callers anywhere)

These are registered in `invoke_handler!` (`src-tauri/src/lib.rs`) and have a shim in `src-tauri/src/commands/mls.rs`, but **no frontend `invoke`, no `mobile/` caller, no `pollis-core/src/bridge.rs` dispatch arm, no internal Rust caller** (grep’d each: only the `mls/mod.rs` re-export + the shim). The underlying feature is live via a *different* function (noted), so the wrapper is vestigial.

| File:line | Symbol | Live replacement / note |
|---|---|---|
| `pollis-core/src/commands/mls/key_packages.rs:189` (`fetch_mls_key_package`) + shim `src-tauri/src/commands/mls.rs:22` + reg `lib.rs:490` | `fetch_mls_key_package` | **Audit-confirmed orphan.** KeyPackage CLAIM moved to the DS (commit `31d8619`). No caller. |
| `pollis-core/src/commands/mls/*` (`create_mls_group`) | `create_mls_group` | Group creation flows through `create_group` → `reconcile_group_mls_impl`. Command wrapper unused. |
| `pollis-core/src/commands/mls/*` (`generate_mls_key_package`) | `generate_mls_key_package` | Superseded by `ensure_mls_key_package` (the live path; callers in `auth.rs:48`, `device_enrollment.rs:1124`). |
| `pollis-core/src/commands/mls/*` (`publish_mls_key_package`) | `publish_mls_key_package` | Superseded by `ensure_mls_key_package`. No caller. |
| `pollis-core/src/commands/mls/*` (`process_welcome`) | `process_welcome` | Welcome handling is live via `poll_mls_welcomes` / `apply_welcome`. Wrapper unused. |
| `pollis-core/src/commands/mls/*` (`reconcile_group_mls`) | `reconcile_group_mls` (command) | The **command wrapper** is orphaned; the live internal path is `reconcile_group_mls_impl` (8 callers in dm/auth/groups). Only the registered command is dead. |

Removal for each = pollis-core fn + `src-tauri/src/commands/mls.rs` shim + `lib.rs` `invoke_handler!` entry + `test_harness.rs` registration + the `mls/mod.rs` re-export line.

### A3. Inline `None`/direct seam arms (the direct-write branches)

Every site below is a `match state.config.pollis_delivery_url { Some(_) => <DS post>, None => <direct Turso write> }` whose `None` arm is dead by the governing finding. Listed by file so removal can be done per-module. (Sites that are the *body* of an already-listed orphan, e.g. `key_packages.rs:193` inside `fetch_mls_key_package`, are covered by A2.)

- `pollis-core/src/commands/user.rs:52, 142`
- `pollis-core/src/commands/groups/groups.rs:122, 209, 286`
- `pollis-core/src/commands/groups/channels.rs:50, 112, 191`
- `pollis-core/src/commands/groups/invites.rs:79, 174, 227`
- `pollis-core/src/commands/groups/membership.rs:95, 172, 266` and the guard at **`:212`** (`if pollis_delivery_url.is_none() && member_count <= 1` → sole-member group DELETE; server-side on DS path)
- `pollis-core/src/commands/groups/join_requests.rs:58, 208, 292`
- `pollis-core/src/commands/dm.rs:116, 324, 385, 439, 496`
- `pollis-core/src/commands/messages/send.rs:133`
- `pollis-core/src/commands/messages/edit_delete.rs:116, 205, 377, 506`
- `pollis-core/src/commands/messages/ingest.rs:138, 164, 389, 415`
- `pollis-core/src/commands/messages/reactions.rs:27, 59`
- `pollis-core/src/commands/blocks.rs:52, 99`
- `pollis-core/src/commands/push.rs:42`
- `pollis-core/src/commands/account_identity.rs:512, 606`
- `pollis-core/src/commands/device_enrollment.rs:214, 583, 790, 914, 1039` and the guard at **`:710`** (`if is_none()` → direct security-log write; explicitly SKIPPED on DS path per the in-code comment)
- `pollis-core/src/commands/mls/group_state.rs:79`
- `pollis-core/src/commands/mls/welcomes.rs:129, 189`
- `pollis-core/src/commands/mls/device.rs:204, 376`
- `pollis-core/src/commands/mls/reconcile.rs:565` (the `claimed` direct KP claim)
- `pollis-core/src/commands/mls/delivery.rs:60` (the `submit_commit` seam — `None` arm calls the A1 `direct_submit`)
- `pollis-core/src/commands/r2.rs:467`
- `pollis-core/src/commands/auth.rs:80` (`request_otp`), `:179` (`verify_otp`), `:645` (`verify_email_change`), `:1041` (`is_none()` guard), `:1119, :1256, :1504`

> **Removal-complexity note:** most are plain `match` arms (delete the `None` block, collapse to the DS call). The `is_none()` *guards* (`membership.rs:212`, `device_enrollment.rs:710`, `auth.rs:1041`) and the tuple match (`device_enrollment.rs:214` — `match (pollis_delivery_url, enrollment_session)`) need light restructuring, not a straight block delete. The `account_identity.rs:512` arm binds `new_version` used afterward, so its `Some` value must remain.

### A4. `resend_api_key` cascade (client side only)

Client-side `config.resend_api_key` (`pollis-core/src/config.rs:21`) is referenced **only** by the now-dead `request_otp_direct` (`auth.rs:151-152`) plus its definition/population (`config.rs:52-54`, `:121`) and the mobile bridge passthrough (`bridge.rs:134, :169`). Once A1’s `request_otp_direct` is removed, the entire client `resend_api_key` field + env wiring is dead and can be dropped from the client `Config`. **Do not touch** `pollis-delivery/src/otp.rs`’s `resend_api_key` — that is the **server’s** key and is LIVE (the DS sends the email now).

### A5. Orphaned commands NOT related to the transition (lower priority — confirm product intent)

Registered + shim’d but zero callers (frontend, mobile, bridge, internal, tests). These predate / are unrelated to Goal A/B; flagged for completeness, not as transition artifacts. Verify nothing external (dev tooling) relies on them before deleting:

| Symbol | Note |
|---|---|
| `is_update_required` | Read counterpart to `mark_update_required` (which frontend *does* call). No reader found. |
| `run_message_eviction` | Retention eviction command — no caller. |
| `list_messages_by_sender` | No caller. |
| `publish_ping` (`livekit`) | No caller. |
| `subscribe_screen_share_frames` | Frontend uses `subscribe_screen_share_events`, not `_frames`. |

---

## Table B — TEST-ONLY-REACHABLE (do NOT blind-delete; decision required)

| Symbol | File:line | Depended on by | Decision needed |
|---|---|---|---|
| `Backend::Local` variant + `RemoteDb::connect_local` | `pollis-core/src/db/remote.rs:13` (variant), constructor ~`:60-69` | **Flows harness** `src-tauri/tests/flows/harness.rs:87, :104` (builds the local libsql file the test client/log DB run on). This is the source of the `warning: variant 'Local' is never constructed` — a false positive from building the lib in isolation. | **KEEP.** It is live test infrastructure. The warning is benign; optionally `#[cfg_attr(..., allow(dead_code))]` or leave as-is. |
| `dev_login` | `pollis-core/src/commands/auth.rs:764`; returns an error in release builds (`auth.rs:771`) | No automated test or frontend caller found, but it is an intentional **debug-gated dev affordance** (compiled to an error in release). | **JUDGMENT.** Not transition dead code. Keep if used for local/dev login; delete only if confirmed unused by dev workflow. |

> The seam `None` arms (Table A3) are deliberately **excluded** from this table: as established in the governing finding, no lib unit test builds a `None`-config `AppState`, so they are not test-reachable. If that ever changes (a lib test starts constructing an `AppState` with `Config::for_test()`), re-evaluate before deleting A3.

---

## Table C — UNUSED IMPORTS / COMPILER WARNINGS (`cargo build -p pollis-core -p pollis-delivery`)

| File:line | Warning | Introduced by | Safe? |
|---|---|---|---|
| `pollis-delivery/src/messages.rs:62:35` | unused import `ok_json` | DS write-path refactor | Yes — free delete |
| `pollis-delivery/src/messages.rs:54:16` | unused import `IntoResponse` | DS write-path refactor | Yes — free delete |
| `pollis-core/src/keystore.rs:1:20` | unused import `Error` | **pre-existing** (noted in brief) | Yes — free delete |
| `pollis-core/src/signal/mls_storage.rs:13:13` | unused import `de::DeserializeOwned` | **pre-existing** (noted in brief) | Yes — free delete |
| `pollis-core/src/db/remote.rs:13:5` | `variant 'Local' is never constructed` | — | **NO — see Table B.** Constructed by the flows harness via `connect_local`; false positive in isolated lib build. |

Note: every `src-tauri/src/commands/*.rs` shim file carries `#![allow(unused_imports)]` (and `bridge.rs:115` / `livekit_jwt.rs:97` carry targeted `allow(dead_code)` for platform-gated code) — these are pre-existing and intentional, not transition artifacts.

---

## Recommended removal order

1. **Zero-risk, do first:** Table C unused imports (4 of them; skip the `Local` variant) — pure `cargo fix` wins.
2. **A1 `*_direct` functions** — delete the four fns, then collapse their `Some/None` callers (`request_otp`/`verify_otp`/`verify_email_change`/`submit_commit`) to the DS call unconditionally. Drives the A4 `resend_api_key` client cleanup for free.
3. **A4 client `resend_api_key`** — remove the field from `pollis-core` `Config` + `bridge.rs` passthrough (leave `pollis-delivery` untouched).
4. **A3 inline `None` arms** — per module; straightforward `match`-arm deletions first, then the three `is_none()` guards + the `device_enrollment.rs:214` tuple match + the `account_identity.rs:512` binding (need restructuring, not block-delete).
5. **A2 orphaned MLS command wrappers** — remove fn + shim + `invoke_handler!` + `test_harness.rs` + `mls/mod.rs` re-export, one command at a time, rebuilding between each.
6. **A5 non-transition orphans + `dev_login`** — only after confirming product/dev intent.

## Recommendation for the test-only group

Keep `Backend::Local`/`connect_local` (live flows-harness infra). For the seam `None` arms, **no test conversion is required** — they are already unreachable by tests, so deleting them does not break any test (the flows harness never takes the `None` path, and would in fact fail if it did because the client DB view is read-only). The read-only client connection is itself the structural guarantee that the direct write paths are unreachable — which is the strongest possible evidence they are safe to remove.
