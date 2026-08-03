# Epic: Path to Launch

**Base commit:** `232a001` · **Derived from:** the 21 open issues, `docs/backend-core-invariants.md`,
`docs/deployments.md`, `mobile/TEST-PLAN.md`, and a full read of the workspace.

Close the delivery-guarantee gap, cut server latency by an order of magnitude, finish the operator
ceremonies only a credential holder can perform, and get iOS/iPadOS/Android into store review — with
everything else explicitly parked.

Ticket IDs below (`PL-nn`) are placeholders for GitHub issue numbers; file them under this epic and
replace the IDs. Repo rules apply throughout: **one ticket = one PR**, DS remains sole writer,
migrations additive, no new always-on CI jobs.

---

## The finding that outranks everything else

`pollis-delivery/src/messages.rs` (lines 71–130) ORs a 30-day TTL gate with the delivery-watermark
gate in `CLEANUP_CHANNEL_ENVELOPES` / `CLEANUP_DM_ENVELOPES`. The module comment states it plainly:
*"TTL gate OR watermark gate (OR'd — either alone deletes)."*

Undelivered envelopes are therefore destroyed after 30 days whether or not the recipient device ever
collected them. **A device offline for a month silently and permanently loses messages.**

This contradicts the guarantee in `docs/backend-core-invariants.md`, which permits exactly two
losses: messages predating the member's join, and a brand-new device starting empty. Invariant I3
requires retention bounded by the slowest member device and explicitly specifies **no TTL**.

Note the watermark arm is already correct — it AND-gates on `COUNT(devices) == COUNT(watermarks)`
and `MIN(last_fetched_at)`, and already excludes revoked devices via `ud.revoked_at IS NULL`
(#685/#686). Removing the TTL arm therefore makes the remaining predicate correct by construction.

---

## WS1 — Make the delivery promise true
*Start now. Blocks launch. Residual work orphaned when #419 was closed; currently tracked nowhere.*

Doctrine for every ticket here: **a test that tries to create the invalid state and proves it
cannot** — happy-path coverage does not count.

| ID | Ticket | Priority | Effort |
|---|---|---|---|
| PL-01 | Remove the TTL arm from both envelope-cleanup statements. Regression test: mail uncollected for >30 days survives. | P0 correctness | M |
| PL-02 | Move the GC *trigger* server-side — today it fires from whichever member happens to ingest (`TODO(#419)`, messages.rs:752). The predicate is already many-member correct; the trigger is not. | P0 correctness | M |
| PL-03 | Reference-count attachment deletion server-side (messages.rs:853) — today an unconditional remote delete can strand a still-referenced file. | P1 | M |
| PL-04 | Enforce I1/I2 at the schema layer. No `TRIGGER` exists anywhere; the gapless chain is held only by the CAS insert in `pollis-delivery/src/commit.rs`. Phases 1–5 of the invariants doc. **Migration 000007 is a deliberate permanent hole — do not reuse.** | P1 | L |
| PL-05 | Fix flaky `sealed_admin_delete_of_other_member_works` (#661). Confirm it is test timing, not a real ingest/delete race, before touching the assertion. | P2 | S |

**Verify:** `cargo test -p pollis-delivery`; `cargo test --features test-harness --test flows`; Kani
proofs; TLA+ `CommitLog` / `Delivery`.

---

## WS2 — Latency and running cost
*Start now, parallel with WS1 (different files).*

| ID | Ticket | Priority | Effort |
|---|---|---|---|
| PL-06 | **#658** — cache device pubkeys in the DS. `auth::verify_request → lookup_device_pubkey` runs one Turso query per request; unsigned endpoints measure 0 ms, signed ones 2000–4700 ms. `max_instances: 1` means one process, no coherency problem. **Revocation safety is the hard requirement:** evict on revoke, logout, resign, identity rotation; short TTL as defence in depth. | P0 UX blocker | M |
| PL-07 | **#515** — drop `sleepAfter = "10m"` (worker/index.ts:66) before real users; removes the cold-start penalty. | P1 | S |
| PL-08 | Add dependency-layer caching to the DS Dockerfile (today a full release build from `COPY . .`). Expect 5–10× faster deploys, no new infra. | P2 | S |
| PL-09 | Move ~120 MB of committed media to R2 — `website/learn` (77 MB video), `website/vendor` (23 MB WASM + 22 MB ONNX). `docs/deployments.md:55` notes this is a one-line base-URL swap. | P2 | S |
| PL-10 | Make `mls-tests.yml` genuinely merge-blocking — it is path-filtered, so mark required via the ruleset's "skipped = pass" option. The 11 `e2e-*.yml` workflows stay dispatch-only by design. | P2 | S |

---

## WS3 — Operator ceremonies
*Owner only — each needs custody of a signing key or a cloud console. Blocks nothing else.*

Until PL-11 lands, **every in-app verification badge reads "unverified"** — safe, because an absent
pin can only withhold trust and never grant it, but the green checkmark stays dark.

| ID | Ticket | Priority | Effort |
|---|---|---|---|
| PL-11 | **#672** — rotate the transparency-log signing key to ML-DSA-44, republish all three trees under the `sth:v2` contexts, re-pin the public half. Runbook is in the issue. | P0 | M |
| PL-12 | **#662** — move the STH signing key out of CI secret storage (HSM/KMS or offline) and design the overlap-window rotation before it is needed. | P1 | M |
| PL-13 | **#662** — replace stable `conversation_id` / `sender_id` in published ledger entries with rotating pseudonyms; today anyone can build a longitudinal activity map. | P1 | L |
| PL-14 | **#662** — define the metadata retention policy (periods, IP handling, push-token contents, ledger exposure). **Blocks PL-20.** | P1 | S |
| PL-15 | **#677** — terminate stale relay nodes so the ASG relaunches on `:latest` (ALPN v3), then make image rolls trigger an instance refresh. Overlay is off by default, so users are unaffected today. | P2 | S |
| PL-16 | **#603** — hash the pre-signature payload in `scripts/attest-binaries.sh`; today macOS/Windows leaves hash the already-signed artifact and are unreproducible by construction, forever, by anyone. | P2 | M |

---

## WS4 — Mobile: from 95% built to "In Review"
*Paperwork starts now — longest lead times on the whole plan.*

The app is far more finished than its tickets suggest: 30 screens, real E2EE messaging through the
uniffi bridge, iPad adaptive layouts, 172 testIDs, an authored Maestro suite. Shipping readiness is
nonetheless **zero**. Voice/video on mobile are excluded by product decision (#343) and are not on
this path.

| ID | Ticket | Priority | Effort |
|---|---|---|---|
| PL-17 | Register developer accounts; replace the scaffold default `com.anonymous.mobile` (hardcoded in `app.json` ×2, `scripts/maestro-run.sh`, `.maestro/README.md`). **Register Play as an organization** — orgs skip the 12-testers/14-day probation. Needs a D-U-N-S number: **longest lead item, start first.** | P0 | S |
| PL-18 | Stand up EAS build profiles + release signing. CI has never built the app — `mobile-core-check.yml` only cross-compiles the Rust core. Needs current-gen Mac (Xcode 26 / macOS 26) for iOS. | P1 | M |
| PL-19 | **#344** — add APNs/FCM credentials + EAS `projectId`. Sender and client handlers are already written; push is silently dead only because the token mint no-ops without a project. Verify delivery to a locked physical device. | P1 | S |
| PL-20 | Compliance: `PrivacyInfo.xcprivacy`, privacy-policy URL, data-safety form, age rating, and the **export-compliance declaration** — non-exempt encryption, so not the trivial "No" path. **Blocked by PL-14.** | P1 | M |
| PL-21 | **#621** — run the Maestro suite for the first time (8 authored, 5 scaffolded; never executed anywhere). Headless Linux dev box cannot run simulators. **Blocked by PL-06** — a 30 s first message fails review regardless of paperwork. | P1 | L |
| PL-22 | Residual parity gaps from #623: wire `dm/info` actions; handle `all_mention` / `member_role_changed` / `roster_changed` (decoded then ignored); `device_revoked` self-sign-out; verify iOS keystore on device. | P2 | M |
| PL-23 | Store listings + **genuine iPad screenshots** — Apple rejects scaled-up phone layouts. The adaptive layout work exists; it needs tablet validation. | P2 | M |
| PL-24 | Submit: TestFlight internal → external beta; Play closed testing. **Blocked by PL-17…PL-21.** | P2 | M |

---

## WS5 — Make the tree tell the truth
*Behaviour-preserving throughout.*

| ID | Ticket | Priority | Effort |
|---|---|---|---|
| PL-25 | Remove the Electron ghost: `docs/electron-migration*.md` ×3 still describe a **reversed** migration as "code-complete, awaiting cutover"; `frontend/src/bridge/runtime.ts` ships an `ElectronAPI` mirroring a preload file that does not exist; ~12 bridge modules branch on `hasElectron()`; `MigrationBanner.tsx` is unreachable in the shipping build. | P2 | M |
| PL-26 | Reconcile contradictions: #455 documented as both DEFERRED and deployed; stale root `AUDIT-slice3.md`; three mobile comments claiming missing features that shipped; `frontend/docs/frontend-audit.md` analyses a Zustand store the app does not use. | P2 | S |
| PL-27 | Tracker hygiene: #185 describes mobile as a static demo ("nothing real flows") — it is a near-complete app. #157/#163 are written entirely against the Electron era and must be re-derived or parked. | P2 | S |

---

## WS6 — Deferred by decision (post-launch)

Nothing above depends on any of these. Label them post-launch so the backlog stops implying they are
pending: **#107** Vault · **#99** pinned messages · **#294** group mention tags · **#230** Flatpak ·
**#157** Mac App Store · **#163** Microsoft Store.

---

## Launch gate

All five must be true:

1. No deliverable message is deleted before every current member device has collected it — proven by a test that tries to break it.
2. Signed writes complete in ~300 ms or better (from 2–4.5 s); first message in a new group under ~3 s end to end.
3. The DS no longer sleeps; no user pays a cold-start penalty.
4. In-app verification reports *verified* against the rotated key on all three trees.
5. Both mobile apps reached store review, with the Maestro suite passing on real phone and tablet hardware.

## Decisions for the owner

| Decision | Default (override freely) |
|---|---|
| Google Play account type | **Organization** — skips the 14-day probation. Needs D-U-N-S; longest lead time. |
| Monetization | **Free core + supporter tier + paid community tier.** Fixed costs are low hundreds/month, so break-even is hundreds of supporters, not thousands of customers. No ads, no VC, no data collection. |
| WS6 feature tickets | **Defer all.** Optionally promote #99 (pinned messages) as one visible user-facing win. |
| Retention policy numbers (PL-14) | Engineering drafts; **owner signs off.** On the critical path for mobile. |
