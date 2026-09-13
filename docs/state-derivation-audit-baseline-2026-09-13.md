# Baseline: state-derivation audit, 2026-09-13

Line numbers refer to commit `521b551` (`Export attachments with the archive, and bring the export to mobile (#1077)`).
openmls semantics checked against the pinned rev `34222ef632c87df58baaa7614c8a72ac235c5f07`.
No X3DH or Double Ratchet code exists in the repo (Signal tables documented as removed at `pollis-core/src/db/local_schema.sql:12-15`).

## Inventory and classification

| # | Structure | Type, where defined | Every writer | Bucket | Verdict and evidence |
|---|---|---|---|---|---|
| 1 | MLS ratchet tree, key schedule, epoch secrets, message secrets, proposal queue, own leaf | openmls `MlsGroup` persisted as opaque blobs in `mls_kv` (`local_schema.sql:79-84`), provider `MlsStore` (`signal/mls_storage.rs:142-947`) | `create_mls_group_in_suite` (`group_state.rs:798-841`), `build_external_commit` (714-784), `join_from_welcome` (`welcomes.rs:81-107`), `apply_one_commit` (`group_state.rs:2074-2196`), `reconcile_group_mls_core_staged` (`reconcile.rs:462-624`), `stage_self_update` (`self_update.rs:283-340`), merges at `reconcile.rs:116-134`, `:734-736`, `:666`, `self_update.rs:303-305`, `group_state.rs:944-946`, `:2157`; clears at `reconcile.rs:339-356`, `group_state.rs:1722-1735`; deletes at `group_state.rs:743-745`, `:828-830`, `:991-993`, `:2104-2190`, `welcomes.rs:93-96` | C | Spec-consistent replay: `invariants::classify` (`invariants.rs:216-226`), one commit per epoch, own-commit adoption (`group_state.rs:2144-2168`), typed `RecoverReason` (`:1996-2019`). Drift point: finding 1. |
| 2 | Staged-but-unconfirmed pending commit | `MlsGroupState::PendingCommit` inside item 1 | Staged at `reconcile.rs:565`, `self_update.rs:323`; resolved by `publish_staged_commit` (`reconcile.rs:174-281`) via `invariants::resolve` (`invariants.rs:166-180`); dangling-clear (`group_state.rs:1713-1735`); blind merges at `group_state.rs:944-946`, `reconcile.rs:734-736`, `self_update.rs:303-305` | C | Only `publish_staged_commit` consults the log. Two blind merges hold the MLS lock; `load_group_with_signer` does not (finding 1). |
| 3 | Desired roster to tree diff (`desired_set`, `to_add`, `to_remove`) | Pure functions `reconcile.rs:428-445`, `:479-509` | Nobody; recomputed from `catch_up_full(...).roster` (`reconcile.rs:822-827`), `claimable_devices` (879-880), `registered_devices` (886), fresh tree walk (479-485) | A | Good. `reconcile_no_drift`, `reconcile_idempotent` (`mls/tests.rs:1209`, `:1368`). Deliberate hidden read of existing leaves for retention (428-445). |
| 4 | Desired roster (server definition) | `desired_roster` (`pollis-delivery/src/directory.rs:157-190`) | DS only: `add_member_rows` (`groups.rs:153-183`), `apply_leave_group` (437-483), `apply_remove_member` (692-727), invites (859, 986-1022, 1036-1050), DM writes (`profile.rs:490`, `:719`, `:782`), purge (`account.rs:688-693`, `teardown.rs:249-250`) | A | Single definition read by reconcile and migrate (`migrate.rs:177-184`); `is_member` (`writes.rs:388-408`) is a query. |
| 5 | Stale-leaf precheck | `local_tree_has_stale_leaf` (`sweep.rs:250-309`) | Nobody | A, duplicated rule | Re-states the removal half of `desired_set` by hand (306-308). Finding 5. |
| 6 | Post-commit roster banner diff | `reconcile_group_mls_impl` (`reconcile.rs:1150-1187`) | Nobody | B | Snapshot (`already_in_tree`, 907-937) plus diff; `stage_reconcile_commit` may merge a pending commit first (734-736). UI only. Finding 6. |
| 7 | Commit add-metadata (`added_user_id`, `added_device_ids`) | Columns on `mls_commit_log`; derived at `reconcile.rs:1019-1033`, `migrate.rs:441-452` | Committer, atomically with the commit (`commit.rs:212-300`) | B | Lossy projection (one user id, all device ids). Receivers verify against that one user (`group_state.rs:1592-1600`, `device.rs:606-720`). Finding 2. |
| 8 | Suite generation pointer | `mls_generation` (`local_schema.sql:108-112`); `generation.rs:89-121` | `group_state.rs:667`, `:1974`, `migrate.rs:292` | C | Monotone; checked against published head generation (`group_state.rs:1889-1911`, `1918-1931`). |
| 9 | Self-update clock | `mls_self_update` (`local_schema.sql:86-95`) | `record_self_update` (`self_update.rs:140-149`) from `:232`, `group_state.rs:671-676`, `migrate.rs:296`; `clear_self_update` (160-167) from `group_state.rs:1979` | C | No recompute possible (openmls hides `unmerged_leaves`). Written only on confirmed win. |
| 10 | Commit-log head, retention floor, per-device high-water | DS `commit.rs`: `head_epoch_of` (77-84), `prune_floor` (492-513), `closed_generation_floor` (527-550), `prune_commit_log` (705-760) | `submit_commit` CAS (212-300), `record_commit_since` (553-580), deletes (644-700) | A | Recomputed from full roster and report set each pass; high-water pair-monotone; parity test `messages.rs:2660+`. |
| 11 | Published GroupInfo | DS `mls_group_info`; monotone upsert in `submit_commit`; heal `group_info_is_stale` (`group_state.rs:269-274`, `327-377`) | Committer, external joiner (`:682`), replay end (`:1784`) | A | Good. Known limitation: `voice_e2ee::published_group_epoch` ignores generation (`voice_e2ee.rs:165-178`). |
| 12 | Ingest watermark | `watermark.rs:149` (`next_watermark`), `is_handled` (84-116); DS `apply_advance_watermark` (`messages.rs:1093-1118`) | Ingest pass | A | Kani-proved; server `MAX`; replay bounded by `ReplayBound` (`group_state.rs:1226-1246`, `directory.rs:100-112`). |
| 13 | Voice E2EE key | `export_voice_key` (`voice_e2ee.rs:262-286`); `e2ee_epoch` marker | `on_mls_epoch_changed` (302-357) at `reconcile.rs:1129`, `self_update.rs:242`, `migrate.rs:308`, `group_state.rs:1768`, `:1798` | A | Derived on demand; rotation event-driven. |
| 14 | Pin KEK and wrap marker | `current_pin_kek` (`pinned_messages.rs:160-175`), `pin_key_cache` | `maybe_rewrap_pin_key` (365-415) | A | Pure export compared to marker each pass. |
| 15 | TOFU account-key pins | `contact_verification`; `safety.rs:368-444` | Reconcile step 1b (`reconcile.rs:840-853`), DM ingest | C by design | Re-compared on every reconcile; mismatch clears `verified`. |
| 16 | Read cursors and unread counts | `read_state.rs:173-206`, `83-118` | Mark-read, cross-device merge | A | Query-derived; cursor strictly monotone (`is_newer`, 74-76). |
| 17 | Device cert staleness | `stale_cert_candidates` (`device.rs:416-460`) | Nobody | A | Pure predicate. |
| 18 | KeyPackage pool | `replenish_key_packages` (`key_packages.rs:158-201`) | Login rotate (121-153), post-Welcome top-up | A | Target minus fresh server count. |
| 19 | DS directory index `user_groups`, `user_dms` | `000009_directory_index.sql` | Backfill once; DS deletes only (`groups.rs:457`, `:717`, `teardown.rs:325`, `:488`); never inserted, never read (`directory.rs:20-24`) | B (dead) | Finding 7. |
| 20 | Conversation id registry | DS `conversation` + triggers (`000017_conversation_guard_triggers.sql`) | `writes.rs:460` | A | Enforced at the DB layer. |
| 21 | Voice room roster, realtime participants | `livekit/participants.rs:30-43`; `livekit/realtime.rs:368-381` | Nobody, fetched per call | A | Good. |
| 22 | LiveKit identity memo | `IdentityCache` (`state.rs:46-69`) | Resolver | A | Memo of a deterministic function. |
| 23 | TUI sidebar rows | `HomeState` (`pollis-tui/src/home.rs:151-198`) | `set_tree`, `rebuild_rows` | A | Rebuilt wholesale. |

## Drift checks (summary)

- Items 1 and 2: dangling-clear plus `invariants::resolve` are the reconciliation; neither catches a merge that happens outside them (finding 1). No code asserts tree-equals-roster after an apply; `local_tree_has_stale_leaf` runs only on cold launch/reconnect.
- Leaver eviction: `leave_group` (`membership.rs:103-147`), `leave_dm_channel` (`dm.rs:330-360`) only broadcast; realtime handler runs catch-up, not reconcile (`livekit/realtime.rs:191-196`). Finding 3.
- `MlsStore` writes are single autocommit statements (`mls_storage.rs:165-193`); no transaction anywhere in `pollis-core`. Partial state recovers via `GroupLoadFailed` (`group_state.rs:2093-2096`).
- `registered_devices` empty means evict everyone (`reconcile.rs:424-427`); no client-side sanity check. Finding 4.

## Findings (ranked)

1. Unlocked blind `merge_pending_commit` via `try_mls_encrypt` -> `load_group_with_signer` (`group_state.rs:942-946`, `2271-2282`), reachable from `receipts::emit_receipt` (`receipts.rs:206-227`) without the MLS lock while reconcile/self-update hold a staged commit across an await. Bucket C drift.
2. Add-metadata is a lossy projection of the commit's add set (`reconcile.rs:1019-1033`, `migrate.rs:441-452`; receiver `group_state.rs:1592-1667`). Bucket B.
3. Leaver eviction has no first attempt; only the sweep backstop evicts. Bucket C latency.
4. Empty `registered_devices` treated as total revocation with no sanity check. Bucket A input fragility.
5. Two copies of the removal rule (`sweep.rs:306-308` vs `reconcile.rs:428-445`). Bucket A duplication.
6. Banner diff from a stale snapshot (`reconcile.rs:1150-1187`). Bucket B, low.
7. Dead directory mirror `user_groups` / `user_dms`. Bucket B, low.
