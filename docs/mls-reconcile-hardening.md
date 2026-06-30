# MLS Hardening — Delivery Service Architecture (Runbook)

> The plan for making Pollis's MLS group state **deterministic and bulletproof
> regardless of which users act, network state, ordering, or time between
> actions**. Self-contained so another session/machine can build it. Governed
> by [`backend-core-invariants.md`](backend-core-invariants.md). Tracked in #397.

## The decision

Move MLS **commit ordering and all writes to MLS state** behind a small,
self-hostable **Delivery Service (DS)**: a Dockerized Rust/axum service deployed
**beside the LiveKit container** at `api.pollis.com`. The DS is the **sole
writer** to the MLS control-plane tables and **serializes commits per
conversation**, which makes forks and wedges *structurally impossible* — the
race that caused them can't exist. **Crypto stays 100% client-side** (E2E
preserved). Invariants live in **DS code**, not DB triggers.

Why this shape:
- The only problem that can't be solved with local logic is **distributed
  agreement on commit order**. A single serialization point removes it.
- E2E means a server can only **order opaque blobs**, never compute or read
  them. This is precisely the MLS RFC 9420 **Delivery Service** role.
- Cloudflare Durable Objects would also work, but they're **proprietary** —
  that breaks the open-source / self-host / auditability story for a
  load-bearing component. A **stateless axum service + a per-conversation
  transactional conditional-insert** gives the *same* race-free property,
  portably (Postgres/SQLite/Turso), auditably (our Rust), self-hostably
  (`docker run` beside self-hosted LiveKit).

## Trust boundary

- **DS sees:** opaque commit / welcome / group-info / key-package blobs;
  conversation / user / device IDs; epochs; ordering. (No more than Turso
  already sees today.)
- **DS never sees:** plaintext, group keys, private keys. It cannot decrypt and
  cannot forge a commit (it holds no keys).

## Responsibilities

**Client — crypto, unchanged:**
- Compute commits: `reconcile(current_tree, desired_roster) → add/remove
  proposals → one commit`. Apply commits, encrypt/decrypt, derive keys. The
  **deterministic reconcile *decision* stays here.**
- Submit commits to the DS; fetch the contiguous log from the DS; apply in
  order. On a submit-reject ("head moved"), re-base on the new head and
  resubmit. (Replaces today's client-side CAS-retry / fork-recovery dance.)

**DS — the new service:**
- **Sole writer** to `mls_commit_log`, `mls_welcome`, `mls_group_info`,
  `mls_key_package` (the MLS control plane).
- `submit_commit(conversation, based_on_epoch, commit_blob, welcomes[],
  group_info)`: inside a **per-conversation transaction**, accept iff
  `based_on_epoch == head`; assign epoch `head+1`; insert (append-only,
  contiguous, one-per-epoch **by construction**); store welcomes + group_info.
  Else reject with `{ head, missing_commits[] }` so the client re-bases.
- `fetch_commits(conversation, since_epoch)` → contiguous list.
  `fetch_welcomes(device)`, `publish_key_package` / `claim_key_package`,
  `fetch_group_info(conversation)`.
- **Structural validation only** (epoch linkage, blob sanity, submitter is a
  current member per the roster). No decryption.
- Also hosts the **auth / secrets broker** (OTP request/verify, mint scoped
  short-TTL Turso creds, LiveKit token, R2 presign) — the same service that
  removes the `EXPO_PUBLIC_*` secret-inlining problem.

## What becomes impossible (by construction, in DS code — not DB triggers)

- **Forks** — per-conversation serialization → exactly one commit per epoch.
- **Gaps** — DS assigns the epoch as `head+1` → always contiguous.
- **Deleted / rewritten commits** — DS never deletes or updates; clients can't
  write the tables at all.
- **Malicious direct writes** — clients hold no write creds to the MLS tables.

## What stays a client concern (and still needs hardening)

1. **Deterministic reconcile decision** — today a member whose key-package
   isn't published *at that instant* is silently skipped and "repaired later",
   so the output depends on transient state, not just the roster. The DS is now
   the authority on roster + available KPs; have it report addability (or hold)
   so the decision is a pure function of the roster. **This is gap #1.**
2. **Applying the log** (crypto) — the DS guarantees contiguity, so client
   catch-up simplifies: no gap-recovery; `external_join` is only for "I have no
   local state → fetch group-info → join."
3. **Recovery dead-ends** — `send` should self-heal (external-join) instead of
   erroring; remove the "defer forever" path.

## Mapping to work already done (this effort)

- **KEEP** — `adfe518` (merged): no commit deletes in code + external-join
  recovery. Its spirit is enforced by the DS; the code is harmless defense.
- **KEEP** — the **repair-nuke removal** (branch `mls/repair-and-ds-replan`):
  `repair_mls_group` (re-create the group at epoch 0 + DELETE the commit log)
  is replaced by `external_join` (rejoin this device from published
  group-info). The DS would forbid that destructive write anyway; this is the
  right client recovery regardless.
- **DROP / reverted** — the DB-trigger migration `000007`, the `execute_batch`
  migration-applier detour, and the INV-1 trigger test. Superseded: the DS owns
  these invariants in code. (No DB triggers — your call, and the right one.)
- **SUPERSEDE** — client-side CAS-retry / fork-recovery
  (`external_join_attempt`'s `ON CONFLICT` dance) → becomes "submit to DS, on
  reject re-base." Simpler and centralized.

## Phased build

- **P1 — DS spine:** axum service + Dockerfile, deployed beside LiveKit at
  `api.pollis.com`. `submit_commit` (transactional conditional-insert per
  conversation) + `fetch_commits`. Sole writer to `mls_commit_log`. Client:
  route the two commit-insert sites (`group_state` external-join, `reconcile`)
  through the DS; replace CAS-retry with submit/reject/re-base.
- **P2 — DS owns the rest of the control plane:** welcomes, group-info,
  key-packages via the DS; structural + membership-authorization validation.
- **P3 — client determinism + total recovery:** fix gap #1 (KP availability via
  DS), close the recovery dead-ends, simplify catch-up (contiguous log).
- **P4 — secrets broker in the same DS:** OTP, scoped Turso creds, LiveKit
  token, R2 presign; remove `EXPO_PUBLIC_*` secrets from the client bundle.
- **P5 — tests:** DS unit tests (concurrent submit → one winner, no fork);
  refactor the `flows` harness to drive clients against an **in-process DS**;
  add the **adversarial-ordering harness** (random interleavings → assert all
  clients converge + reach head). Keep the 29 `flows` tests as regression.

## Open question to settle when building

**Read path.** Do clients read the commit log directly (read-only DB access) or
through the DS? Reads-via-DS → clients need *zero* direct DB access (cleanest;
best for scoped creds), at the cost of the DS being on the read path. Recommend
**reads-via-DS for the MLS control plane** (low volume); message envelopes stay
direct (out of scope — delivery/retention deferred).

## Infra / repo facts to carry

- Service stack: **Rust/axum** (matches the existing `verifiable-log-serve`
  crate). Storage: keep **Turso** initially (DS becomes the sole writer to the
  MLS tables); portable to Postgres/SQLite.
- Deploy: **Docker** container beside the LiveKit container; `api.pollis.com`.
  Self-host story intact — `docker run` the DS + self-hosted LiveKit + your DB,
  no proprietary primitives.
- DBs: **prod** = `prod-actuallydan` (clean: migrations `0–5`, `push_token`=6
  pending, deploys next release). **dev** = `dev-actuallydan` (messy old
  lineage; disposable). **test** = `pollis-test`. `.env` files corrected so
  `.env.production` → prod.
- **Out of scope (deferred):** message delivery, `conversation_watermark`, the
  30-day envelope TTL, envelope GC. Crypto + commit-log correctness only. No new
  features.

## Acceptance test (unchanged)

A member who joined a group 4 years and 300 commits ago — through dozens of
adds/removals — comes back, fetches the contiguous log from the DS, applies it
to the head epoch with no gaps, and decrypts every message sent while they were
a member. Only history ever lost: messages before they joined; a brand-new
device starting empty.
