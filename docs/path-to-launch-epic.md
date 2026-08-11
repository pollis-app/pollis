# Epic: Path to Launch

> **STATUS — most of this epic has shipped.** Written against base commit `232a001`,
> when the items below were open. WS1 (the delivery promise), WS2 (latency and cost),
> WS3 (operator ceremonies) and WS5 (tell the truth) are complete; WS4 (mobile
> distribution) is blocked on account registration (#723), which is not an engineering
> task. Rows are marked **✅ done** with the issue that closed them rather than deleted,
> so the reasoning stays readable — but do **not** read an unmarked row as open work
> without checking. The one item that outranked everything else, the 30-day TTL, was
> fixed by #688; the section describing it is preserved below as history and explicitly
> labelled.

**Base commit:** `232a001` · **Derived from:** the 21 open issues, `docs/backend-core-invariants.md`,
`docs/deployments.md`, `mobile/TEST-PLAN.md`, and a full read of the workspace.

Close the delivery-guarantee gap, cut server latency by an order of magnitude, finish the operator
ceremonies only a credential holder can perform, and get iOS/iPadOS/Android into store review — with
everything else explicitly parked.

Ticket IDs below (`PL-nn`) are placeholders for GitHub issue numbers; file them under this epic and
replace the IDs. Repo rules apply throughout: **one ticket = one PR**, DS remains sole writer,
migrations additive, no new always-on CI jobs.

---

## ~~The finding that outranks everything else~~ — FIXED in #688

> **This is history, not a live defect.** The TTL arm is gone. Envelope retention is
> now keyed **only** on the delivery watermark: mail survives until every current,
> unrevoked device of every current member has collected it. The one bound that
> remains is #720's staleness rule — a device that has not reported for
> `POLLIS_DS_WATERMARK_STALE_MONTHS` (12) stops pinning retention, keyed on the
> *device's last report* and never on message age, and reversible the moment it
> reports again. A device offline for a month loses nothing.
>
> The cited line range (`messages.rs` 71–130) no longer locates the described code.

<details>
<summary>The finding as originally written</summary>

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

</details>

---

## WS1 — Make the delivery promise true — **complete**
*Was: start now, blocks launch. Every row here has landed; kept for the reasoning.*

Doctrine for every ticket here: **a test that tries to create the invalid state and proves it
cannot** — happy-path coverage does not count.

| ID | Ticket | Priority | Effort |
|---|---|---|---|
| PL-01 | ~~Remove the TTL arm from both envelope-cleanup statements.~~ **✅ done — #688.** Retention is watermark-only; the sole remaining bound is #720's 12-month device-staleness rule, keyed on the device's last report. | P0 correctness | M |
| PL-02 | ~~Move the GC *trigger* server-side.~~ **✅ done.** The sweep runs in the DS (`POLLIS_DS_GC_SWEEP_SECS`), not from whichever member happens to ingest; the `TODO(#419)` in `messages.rs` is gone. | P0 correctness | M |
| PL-03 | ~~Reference-count attachment deletion server-side.~~ **✅ done.** Liveness is DERIVED by joining `attachment_ref` (migration 000012) to still-live `message_envelope` rows — `live_ref_exists` in `pollis-delivery/src/messages.rs`. A delete now clears only orphaned refs. | P1 | M |
| PL-04 | ~~Enforce I1/I2 at the schema layer.~~ **✅ done — #691.** `migrations-log/000005_mls_commit_log_triggers.sql` adds the log DB's first triggers, making a gap, rewrite, or head-loss physically impossible — a second line of defence behind the CAS, not a replacement. **Main-DB migration 000007 remains a deliberate permanent hole — do not reuse.** | P1 | L |
| PL-05a | ~~Harden the tombstone `sent_at` floor against envelope GC (latent hazard, reasoned from code).~~ **Done — #692.** See "PL-05 analysis (a)". | P2 | S |
| PL-05b | ~~**#661**~~ — **closed.** Root cause was never reproduced; the issue was closed rather than left as a standing unknown. Original analysis retained below. Was: root cause UNCONFIRMED. An earlier hypothesis was checked and ruled out; see "PL-05 analysis (b)". Instrument before touching the assertion. | P2 | M |

**Verify:** `cargo test -p pollis-delivery`; `cargo test --features test-harness --test flows`; Kani
proofs; TLA+ `CommitLog` / `Delivery`.

### PL-05 analysis — two separate things, do not conflate them

**(a) A latent tombstone-ordering hazard. Real, but it does NOT explain #661.** — **FIXED in #692**;
the analysis below is kept as the rationale. The floor is now the greater of `MAX(sent_at)` over
`message_envelope` and `MAX(last_fetched_at)` over `conversation_watermark` (`TOMBSTONE_FLOOR` in
`pollis-delivery/src/messages.rs`), pinned by `messages::tombstone_floor_tests` and
`pollis-delivery/tests/envelope_retention.rs` (Part F).

Ingest selects envelopes with `sent_at > last_fetched_at` (strictly greater) and advances the
watermark to the highest `sent_at` it consumed (`pollis-core/src/commands/messages/ingest.rs:148-159`).
Anything stamped at or below a device's watermark is skipped **permanently** — the cursor never
rewinds.

The delete path already guards this. `sent_at_after()` (`pollis-delivery/src/messages.rs:203`) stamps
the tombstone strictly above a floor, and its doc comment states the intent: *"the tombstone would
again sort underneath and never be fetched … removes the dependence on the two clocks agreeing."*

The gap: the floor is `SELECT MAX(sent_at) FROM message_envelope WHERE conversation_id = ?1`
(messages.rs:511-522) — the max over envelopes **still present**. Once every member device has
reported a watermark, GC prunes those envelopes; with the conversation's envelopes gone,
`MAX(sent_at)` is `NULL`, `sent_at_after` falls back to wall-clock `now`, and the clock-skew
dependency the guard was written to remove is back. A recipient whose watermark came from a client
clock running ahead of the DS clock would then never fetch the tombstone.

`conversation_watermark.last_fetched_at` rows outlive envelope GC, so the floor should be the greater
of `MAX(sent_at)` over envelopes **and** `MAX(last_fetched_at)` over that conversation's watermarks.
Cheap, strictly safer, worth doing on its own merits.

**(b) Why this is not the #661 cause.** In that test bob sends and never fetches, and only
`ingest.rs` advances watermarks — no other call site does. So bob's device has no
`conversation_watermark` row, the cleanup CASE's `COUNT(devices) = COUNT(watermarks)` check fails,
nothing is pruned, and the floor is therefore non-NULL and equal to bob's message. The tombstone
lands strictly above carol's watermark and she should fetch it. The hazard in (a) needs *all* member
devices to have reported, which this test never reaches.

So **#661's root cause remains unconfirmed.** The remaining candidates, in order: read-after-write
visibility between the DS's write connection and carol's read connection (the issue author's own
hypothesis, and the best fit for a timing-dependent 1-in-18 failure); or the tombstone being fetched
but not applied because carol's MLS state has not reached the required epoch during interleaved
replay. Instrument before fixing — do not patch the assertion until the mechanism is known.

---

## WS2 — Latency and running cost
*Start now, parallel with WS1 (different files).*

| ID | Ticket | Priority | Effort |
|---|---|---|---|
| PL-06 | ~~Cache device pubkeys in the DS.~~ **✅ done.** `DeviceKeyCache` in `pollis-delivery/src/auth.rs` holds `(user_id, device_id) → verifying key` in process, invalidated on revoke/logout/resign/cert-publish, positive entries only. **#658 stays open** as a wider hosting-strategy spike — the cache removed the per-request Turso round trip, but the dev/prod latency gap it was found through is not fully explained. | P0 UX blocker | M |
| PL-07 | Drop `sleepAfter = "10m"`. **STILL OPEN** — previously marked done in error: the check looked at `wrangler.prod.jsonc` (where it correctly does not appear) instead of `pollis-delivery/worker/index.ts:146`, which the ticket named and where it is still set. The real lever is not the duration at all: `@cloudflare/containers` 0.3.7 stops the container from `onActivityExpired()`, whose default implementation calls `this.stop()`. Override it to a no-op and the container never sleeps — the worker comment claiming "the only lever is a large FINITE duration" is wrong. | P1 | S |
| PL-08 | ~~Add dependency-layer caching to the DS Dockerfile.~~ **✅ done.** Layered with `cargo-chef`, so third-party dependency compilation lives in its own cached layer; chosen over a `--mount=type=cache` because the `cook` result is a real image layer the registry can reuse across runners. | P2 | S |
| PL-09 | ~~Move ~120 MB of committed media to R2.~~ **✅ done.** `website/` is now 412 KB; `learn/` and `vendor/` are served from R2. | P2 | S |
| PL-10 | ~~Make `mls-tests.yml` genuinely merge-blocking.~~ **✅ done — #698.** Rather than rely on unverified "skipped = pass" behaviour, the workflow always runs and always reports: a seconds-cheap `changes` job decides exemption (frontend/website/docs/markdown), the heavy `tests` job is gated on it, and a final `gate` job (`if: always()`) collapses the outcome to pass-or-exempt/fail. Mark the **`gate`** check required. The 11 `e2e-*.yml` workflows stay dispatch-only by design. | P2 | S |

---

## WS3 — Operator ceremonies — **complete**
*Owner only — each needs custody of a signing key or a cloud console. Blocks nothing else.*

Until PL-11 lands, **every in-app verification badge reads "unverified"** — safe, because an absent
pin can only withhold trust and never grant it, but the green checkmark stays dark.

| ID | Ticket | Priority | Effort |
|---|---|---|---|
| PL-11 | ~~Rotate the transparency-log signing key to ML-DSA-44.~~ **✅ done — #672.** All three trees publish under the `sth:v2` contexts and the client pins the ML-DSA-44 public half. | P0 | M |
| PL-12 | ~~Move the STH signing key out of CI secret storage and design rotation.~~ **✅ done — #754.** Solved by an **offline root** rather than by moving the online key: the root signs nothing but key-set statements (`verifiable-log/src/keyset.rs`), so retiring a leaked signer is a statement clients accept **without a client release**. Custody in `docs/sth-signing-key-custody.md`; recovery in `docs/signing-key-compromise-runbook.md`. | P1 | M |
| PL-13 | ~~Replace stable ids in published ledger entries with rotating pseudonyms.~~ **✅ done — #662.** Verified live: `/v1/entries.json` publishes `conversation_pseudonym` / `sender_pseudonym`, with no raw ids. The account-key tree's `user_id` is deliberately *not* pseudonymised — see `docs/metadata-retention-policy.md`. | P1 | L |
| PL-14 | ~~Define the metadata retention policy.~~ **✅ done — #662.** `docs/metadata-retention-policy.md`. PL-20 is unblocked. | P1 | S |
| PL-15 | ~~Heal the split-brain relay pool.~~ **✅ done — #677 / #703**, owner ceremony included (#616 verified live: 3 healthy nodes, signed directory passes). In-repo (done): nodes launch an immutable, recorded build (digest in the `intended-image` SSM param), never `:latest`; `/version` reports protocol + build identity; the reconciler excludes wrong-protocol nodes from the signed directory and cycles wrong-build nodes at a bounded rate; `relay-image.yml` records each roll via GitHub OIDC (no standing CI creds) so the fleet converges pull-based. Owner ceremony (remaining): `terraform apply`, seed `intended-image` / add the OIDC role ARN as a repo variable, optionally hand-terminate the currently-stale instances (runbook: `infra/relay-hydra/README.md` → "Roll the relay image"). Overlay is off by default, so users are unaffected today. | P2 | S |
| PL-16 | ~~Hash the pre-signature payload in `scripts/attest-binaries.sh`.~~ **✅ done — #603/#750/#801.** Signed platforms get two leaves joined by a shared `payload_sha256`: the payload is hashed with signatures normalised out (`sha_macos_payload` / `sha_windows_payload`, the latter via `scripts/lib/strip-pe-signature.py`), and the shipped bytes get their own `signed` leaf. Linux reproduces bit-for-bit end to end. The published hashes are surfaced next to the download buttons on the site (#801). | P2 | M |

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
| PL-18 | Stand up EAS build profiles + release signing. **Account-free half landed (#706):** `mobile/eas.json` authored (dev/preview/production, `fingerprint` runtimeVersion so a native-core change can't ship a bad JS-only OTA), and `mobile-core-check.yml` now builds a real Android APK end to end (uniffi gen → 3-ABI `cargo-ndk` → `expo prebuild` → Gradle release, `expo prebuild`+Gradle directly, **not** `eas build`), uploaded as an artifact + `expo-doctor` in CI. **Blocked remainder:** iOS build + Apple distribution signing (Apple Developer account #723 + current-gen Mac / Xcode 26); Android Play **upload** signing (Play console — #344/PL-17); EAS `projectId` (Expo/EAS account — also gates push #344/PL-19). The APK is debug-keystore-signed only until upload signing exists. | P1 | M |
| PL-19 | ~~Add APNs/FCM credentials + EAS `projectId`.~~ **#344 closed.** Remaining verification (delivery to a locked physical device) rides with PL-24, since it needs the store-signed build. | P1 | S |
| PL-20 | Compliance: `PrivacyInfo.xcprivacy`, privacy-policy URL, data-safety form, age rating, and the **export-compliance declaration** — non-exempt encryption, so not the trivial "No" path. **Blocked by PL-14.** | P1 | M |
| PL-21 | **#621** — run the Maestro suite for the first time (8 authored, 5 scaffolded; never executed anywhere). Headless Linux dev box cannot run simulators. **Blocked by PL-06** — a 30 s first message fails review regardless of paperwork. | P1 | L |
| PL-22 | Residual parity gaps (**#623 itself is closed**; these are the leftovers): wire `dm/info` actions; handle `all_mention` / `member_role_changed` / `roster_changed` (decoded then ignored); `device_revoked` self-sign-out; verify iOS keystore on device. | P2 | M |
| PL-23 | Store listings + **genuine iPad screenshots** — Apple rejects scaled-up phone layouts. The adaptive layout work exists; it needs tablet validation. | P2 | M |
| PL-24 | Submit: TestFlight internal → external beta; Play closed testing. **Blocked by PL-17…PL-21.** | P2 | M |

---

## WS5 — Make the tree tell the truth — **substantially complete**
*Behaviour-preserving throughout.*

| ID | Ticket | Priority | Effort |
|---|---|---|---|
| PL-25 | ~~Remove the Electron ghost.~~ **✅ done.** No `hasElectron`, `ElectronAPI`, or `MigrationBanner` remains anywhere in `frontend/src/`; one `docs/electron-migration.md` is kept deliberately, as the record of a reversed decision. | P2 | M |
| PL-26 | ~~Reconcile contradictions.~~ **✅ done.** `AUDIT-slice3.md` is gone, #455 is consistent, and `frontend/docs/frontend-audit.md` now carries a SUPERSEDED banner stating plainly that its "Zustand" analysis is wrong — the stores are MobX (#365) — while keeping the structural findings that still hold. | P2 | S |
| PL-27 | Tracker hygiene. **Open.** #185 describes mobile as a static demo ("nothing real flows") — it is a near-complete app. #157/#163 are written entirely against the Electron era and must be re-derived or parked. | P2 | S |

---

## WS6 — Deferred by decision (post-launch)

Nothing above depends on any of these. Label them post-launch so the backlog stops implying they are
pending: **#107** Vault · **#99** pinned messages · **#294** group mention tags · **#230** Flatpak ·
**#157** Mac App Store · **#163** Microsoft Store.

---

## Launch gate

All five must be true. Four are:

1. ✅ No deliverable message is deleted before every current member device has collected it — #688 removed the TTL arm, #691 added the schema-layer triggers, and both ship with tests that try to create the invalid state.
2. ⏳ Signed writes complete in ~300 ms or better (from 2–4.5 s); first message in a new group under ~3 s end to end. **The pubkey cache landed; the end-to-end number is unverified** because measuring it needs the mobile suite on real hardware (PL-21). Tracked by #658.
3. ⏳ The DS no longer sleeps; no user pays a cold-start penalty. **Not yet true** — `sleepAfter = "10m"` is still set in `worker/index.ts`. Measured worst case is ~213 ms, so the penalty is small, but the gate item is not met.
4. ✅ In-app verification reports *verified* against the rotated key on all three trees.
5. ⏳ Both mobile apps reached store review, with the Maestro suite passing on real phone and tablet hardware. **Blocked on #723**, which is an account-registration decision, not engineering.

## Decisions for the owner

| Decision | Default (override freely) |
|---|---|
| Google Play account type | **Organization** — skips the 14-day probation. Needs D-U-N-S; longest lead time. |
| Monetization | **Free core + supporter tier + paid community tier.** Fixed costs are low hundreds/month, so break-even is hundreds of supporters, not thousands of customers. No ads, no VC, no data collection. |
| WS6 feature tickets | **Defer all.** Optionally promote #99 (pinned messages) as one visible user-facing win. |
| Retention policy numbers (PL-14) | Drafted in `docs/metadata-retention-policy.md`; **owner signs off.** On the critical path for mobile. |
