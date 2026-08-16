-- Schema-layer enforcement of invariant I1 — the commit log is a gapless,
-- append-only, single-writer-per-epoch chain (issue #691, phase 1 of
-- docs/backend-core-invariants.md).
--
-- IMPORTANT: COMMIT-LOG-DB migration (mls_commit_log lives on the SEPARATE log
-- DB post-#420). Applied by desktop-release.yml's second db-apply step
-- (MIGRATIONS_DIR=pollis-schema/migrations-log). It must NOT go in the
-- main-DB dir. This is migration 000005 of the log DB's OWN sequence (the log DB
-- started its numbering at 000001; do not confuse it with the main DB, whose
-- 000007 is a permanent hole — see docs/goal-a-deploy-runbook.md).
--
-- WHY. Until now no TRIGGER existed anywhere in the schema. I1 was held ONLY by
-- the compare-and-swap INSERT in `pollis-delivery::commit::submit_commit` — the
-- protocol layer. A bug in our own DS code could therefore still open a gap in
-- the chain, rewrite an applied commit, or delete the live head, and nothing in
-- the database would stop it. "Invalid states unrepresentable" wants the
-- invariant enforced at the LOWEST layer (DB > type > protocol > discipline);
-- these triggers are that lowest layer. They are a SECOND line of defence behind
-- the CAS, never a replacement for it — the CAS still picks the single winner
-- per epoch race-free; the triggers make a gap/rewrite/head-loss physically
-- impossible even if the CAS were bypassed or buggy.
--
-- COEXISTENCE WITH RETENTION (the "must not fire on legitimate traffic" bar).
-- The DS legitimately DELETEs from this table in exactly two shapes, both from
-- `commit.rs` and both a PREFIX/whole-lineage removal that NEVER touches the
-- live head (the floor is clamped to `head - 1`, #681, and to `head_generation`):
--   * `delete_commits_below`  — `epoch < floor` within the head generation;
--   * `delete_generations_below` — whole CLOSED generations (`generation < floor`).
-- The DELETE trigger below is written so neither can ever trip it (proof in the
-- per-trigger comments and in pollis-delivery/tests/commit_log_triggers.rs), and
-- the whole integration suite runs the real submit + prune path against this
-- triggered schema (POST_BASELINE_LOG_MIGRATIONS).
--
-- Additive/backward-compatible (CLAUDE.md migration rule): three CREATE TRIGGER
-- statements only — no table/column/index change, nothing an existing row or a
-- shipped read-only client can violate. `IF NOT EXISTS` makes re-apply a no-op.

-- 1. FORWARD-GAP GUARD (kills F1). A commit may only be appended at the head of
-- its lineage: `NEW.epoch` must be <= the current head (`MAX(epoch)+1`) within
-- `(conversation_id, generation)`. A forward jump (`epoch > head`) is what left
-- ELECTRON epochs 4/5/11 missing and wedged `ants`. This is exactly the CAS's
-- forward-gap rejection, restated as a schema invariant.
--   * The head insert (`epoch = MAX(epoch)+1`) passes (`epoch = head`, not `>`).
--   * A racing/stale resubmit at `epoch <= MAX(epoch)` passes the trigger and is
--     absorbed by the CAS's `ON CONFLICT(conversation_id, generation, epoch) DO
--     NOTHING` — the trigger does not interfere with dedup.
--   * The opening commit of a new generation (`epoch = 0`, empty lineage) passes:
--     the subquery is scoped to `NEW.generation`, whose MAX is NULL → head 0.
-- So the ONLY thing it rejects is a forward gap, which no legitimate submit ever
-- produces (the CAS's WHERE only ever emits a row at `epoch = head`).
CREATE TRIGGER IF NOT EXISTS trg_mls_commit_log_no_forward_gap
BEFORE INSERT ON mls_commit_log
FOR EACH ROW
WHEN NEW.epoch > (
    SELECT COALESCE(MAX(epoch), -1) + 1
    FROM mls_commit_log
    WHERE conversation_id = NEW.conversation_id
      AND generation = NEW.generation
)
BEGIN
    SELECT RAISE(ABORT, 'mls_commit_log I1: epoch skips ahead of the head (forward gap)');
END;

-- 2. IMMUTABLE HISTORY (kills the rewrite half of F2). The chain identity
-- (`conversation_id`, `generation`, `epoch`) and the committed payload
-- (`commit_data`) can never be changed once written — a stored commit is
-- final. The DS never issues an UPDATE to this table at all (grep: the only
-- writes in commit.rs are the CAS INSERT and the two retention DELETEs), so this
-- has ZERO legitimate traffic to fire on; it exists purely so a future stray
-- UPDATE cannot silently re-point or edit applied history.
CREATE TRIGGER IF NOT EXISTS trg_mls_commit_log_immutable
BEFORE UPDATE ON mls_commit_log
FOR EACH ROW
WHEN NEW.conversation_id <> OLD.conversation_id
  OR NEW.generation <> OLD.generation
  OR NEW.epoch <> OLD.epoch
  OR NEW.commit_data <> OLD.commit_data
BEGIN
    SELECT RAISE(ABORT, 'mls_commit_log I1: commit history is immutable');
END;

-- 3. HEAD PROTECTION (kills the catastrophic half of F2). The live head commit —
-- MAX(epoch) of MAX(generation) for a conversation — may never be deleted WHILE
-- ANY EARLIER COMMIT OF THE SAME CONVERSATION STILL EXISTS. That is the exact
-- shape that wedges a group (the F2 / ELECTRON symptom: "dan applied epoch 11, it
-- was deleted, ants wedged" — every current member had applied the head and is now
-- stranded on a chain that regressed under them, with earlier commits still
-- present that it can no longer reconcile against). The `EXISTS (... other row of
-- the conversation ...)` clause is what scopes the guard to THAT harm and no more:
--   * Deleting the head while siblings remain (the ELECTRON shape / a targeted
--     stray delete) → the EXISTS is true → ABORT.
--   * Deleting the head when it is the LAST remaining row of the conversation →
--     the EXISTS is false → ALLOWED. The chain becomes empty, which is trivially
--     gapless, and there is nothing left for a member to be inconsistent with.
--     This is deliberately NOT the ELECTRON failure: an empty conversation is a
--     whole-conversation removal, not a head regression under live members.
--
-- Legitimate retention NEVER trips this (unchanged by the EXISTS clause — the
-- head-detection terms already excluded every pruned row):
--   * `delete_commits_below` deletes `epoch < floor` with `floor <= head - 1`
--     (#681 clamp) within the head generation, so every deleted epoch is strictly
--     below the head epoch — the second head-detection term is false for all of them.
--   * `delete_generations_below` deletes only `generation < head_generation`, so
--     the first head-detection term (`OLD.generation = MAX(generation)`) is false
--     for all of them; the head generation is invariant under that delete.
--
-- FULL-CONVERSATION ERASURE / BULK WIPE (the "must not fire on legitimate traffic"
-- bar — this is what a bare `DELETE FROM mls_commit_log` and a future
-- delete-account path do). `seq` is the AUTOINCREMENT PK, so within a conversation
-- the head always has the HIGHEST `seq` (it was appended last). SQLite visits rows
-- of an unqualified DELETE in ascending `seq` (rowid) order, so the head of each
-- conversation is the LAST of that conversation's rows to be deleted — by then its
-- siblings are already gone, the EXISTS is false, and the delete is allowed. The
-- table empties cleanly. This is why the three test-harness wipe sites
-- (src-tauri/src/test_harness.rs `wipe_tables(log, &LOG_TABLES)`, and any
-- `DELETE FROM {table}` loop) need ZERO changes, and why account/conversation
-- deletion needs no bypass primitive. Verified empirically against the real
-- migration, not merely reasoned about (see the commit message and the
-- `bulk_wipe_*` / `delete_head_when_only_row_*` tests in commit_log_triggers.rs).
--
-- NOTE (scope, honest): this guard protects the HEAD against a regression-under-
-- live-members, not against an interior hole punched into the retained window
-- `[floor, head)`. A per-row BEFORE DELETE trigger cannot detect an interior gap
-- without a sibling rule that false-positives on the intermediate state of a
-- legitimate bulk prefix prune (a partly-truncated prefix is indistinguishable
-- from a mid-chain hole). Interior contiguity therefore stays enforced by the CAS +
-- the clamped retention floor in Rust; the schema layer guarantees no forward gap,
-- no rewrite, and no head-regression-while-members-hold-it.
--
-- COST (stated honestly): buggy code that deletes an ENTIRE live conversation's
-- commit rows in one sweep is no longer stopped at the schema layer. That is an
-- acceptable trade — it is a different operation with different semantics (whole-
-- conversation removal, not I1's head-regression failure mode), and nothing above
-- the DB issues an unscoped per-conversation delete of a live conversation (the
-- only production deletes are the two prefix/generation retention shapes above).
CREATE TRIGGER IF NOT EXISTS trg_mls_commit_log_keep_head
BEFORE DELETE ON mls_commit_log
FOR EACH ROW
WHEN OLD.generation = (
        SELECT MAX(generation) FROM mls_commit_log
        WHERE conversation_id = OLD.conversation_id
     )
 AND OLD.epoch = (
        SELECT MAX(epoch) FROM mls_commit_log
        WHERE conversation_id = OLD.conversation_id
          AND generation = OLD.generation
     )
 AND EXISTS (
        SELECT 1 FROM mls_commit_log
        WHERE conversation_id = OLD.conversation_id
          AND seq <> OLD.seq
     )
BEGIN
    SELECT RAISE(ABORT, 'mls_commit_log I1: the live head cannot be deleted while earlier commits of the conversation remain');
END;
