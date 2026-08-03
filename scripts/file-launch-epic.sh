#!/usr/bin/env bash
# File the "Path to Launch" epic and its tickets as GitHub issues.
#
# Why this exists: the epic was authored in an environment whose egress policy
# blocked api.github.com, so the issues could not be created directly. Running
# this from a box with normal `gh` auth files them in one shot.
#
#   ./scripts/file-launch-epic.sh --dry-run     # print what would be created
#   ./scripts/file-launch-epic.sh               # actually create them
#
# Idempotency: before creating anything it lists open issues and skips any whose
# title already matches, so a partial run can be resumed safely.
#
# Ticket bodies deliberately stay short and point at docs/path-to-launch-epic.md
# rather than duplicating it — one source of truth.

set -euo pipefail

REPO="${REPO:-actuallydan/pollis}"
DOC="docs/path-to-launch-epic.md"
DRY=0
[[ "${1:-}" == "--dry-run" ]] && DRY=1

command -v gh >/dev/null || { echo "gh CLI not found" >&2; exit 1; }
gh auth status >/dev/null 2>&1 || { echo "gh not authenticated" >&2; exit 1; }

echo "Fetching existing open issues from $REPO ..."
EXISTING="$(gh issue list --repo "$REPO" --state open --limit 300 --json title --jq '.[].title')"

# Labels are only applied if they already exist in the repo; a missing label
# would otherwise abort the create.
REPO_LABELS="$(gh label list --repo "$REPO" --limit 200 --json name --jq '.[].name' 2>/dev/null || true)"
have_label() { grep -Fxq "$1" <<<"$REPO_LABELS"; }

create() {
  local title="$1" body="$2"; shift 2
  local labels=("$@")

  if grep -Fxq "$title" <<<"$EXISTING"; then
    echo "  SKIP (exists): $title"
    return 0
  fi

  local args=(--repo "$REPO" --title "$title" --body "$body")
  for l in "${labels[@]}"; do
    have_label "$l" && args+=(--label "$l")
  done

  if (( DRY )); then
    echo "  WOULD CREATE: $title  [labels: ${labels[*]:-none}]"
  else
    local url; url="$(gh issue create "${args[@]}")"
    echo "  created: $url  <- $title"
  fi
}

ref() { printf 'Part of the **Path to Launch** epic. Full context, dependencies and verification steps: `%s` (workstream %s).\n\n%s\n' "$DOC" "$1" "$2"; }

echo
echo "== Epic =="
create "Epic: Path to Launch" \
"$(cat <<'EOF'
Umbrella epic. The plan of record is committed at `docs/path-to-launch-epic.md`.

Closes the delivery-guarantee gap, cuts Delivery Service latency by an order of
magnitude, finishes the operator ceremonies that only a credential holder can
run, and gets iOS/iPadOS/Android into store review. Everything else is
explicitly parked in WS6 so the backlog stops implying it is pending.

**Headline finding.** Envelope cleanup in `pollis-delivery/src/messages.rs`
(lines 87-112) ORs a 30-day TTL with the delivery-watermark gate — the module
comment concedes "either alone deletes". Undelivered envelopes are therefore
destroyed after 30 days whether or not any recipient device ever collected
them, so a device offline for a month silently loses messages. That contradicts
invariant I3 in `docs/backend-core-invariants.md`, which requires retention
bounded by the slowest member device and explicitly specifies no TTL.

**Launch gate** (all five must hold):
1. No deliverable message is deleted before every current member device has collected it, proven by a test that tries to break it.
2. Signed writes complete in ~300ms or better, down from 2-4.5s.
3. The DS no longer scales to zero, so no user pays a cold-start penalty.
4. In-app verification reports *verified* against the rotated log key on all three trees.
5. Both mobile apps have reached store review with the Maestro suite passing on real hardware.

Workstreams: WS1 durability · WS2 latency/cost · WS3 operator ceremonies ·
WS4 mobile to review · WS5 repo truth · WS6 deferred.
EOF
)" "epic"

echo
echo "== WS1 - delivery guarantee =="
create "WS1: Remove the 30-day TTL arm from envelope cleanup" \
"$(ref WS1 'Drop `sent_at < datetime('"'"'now'"'"', '"'"'-30 days'"'"')` from `CLEANUP_CHANNEL_ENVELOPES` and `CLEANUP_DM_ENVELOPES` in `pollis-delivery/src/messages.rs`.

The watermark arm is already correct: it AND-gates on `COUNT(devices) = COUNT(watermarks)` with `MIN(last_fetched_at)`, and already excludes revoked devices via `ud.revoked_at IS NULL` (#685/#686). Removing the TTL arm therefore makes the remaining predicate correct by construction — no rewrite of the retention logic is needed.

**Regression test required:** an envelope uncollected for well over 30 days must survive. Per repo doctrine the test must try to create the invalid state and prove it cannot.

Verify: `cargo test -p pollis-delivery`, then the flows suite.')" "bug" "security"

create "WS1: Move the envelope GC trigger server-side" \
"$(ref WS1 'Resolves `TODO(#419)` at `pollis-delivery/src/messages.rs:752`. GC currently fires from whichever member happens to ingest, rather than being driven server-side. The SQL predicate is already many-member correct; the *trigger* is not.

Note the existing comment says "do not change the deletion predicate here" — that constraint still applies, this ticket moves the trigger only.')" "bug"

create "WS1: Reference-count attachment deletion server-side" \
"$(ref WS1 'Resolves the second `TODO(#419)` at `pollis-delivery/src/messages.rs:853`. Attachment dedup currently issues an unconditional remote delete, so one conversation removing a shared file can strand another that still references it.')" "bug"

create "WS1: Enforce the gapless-chain invariants at the schema layer" \
"$(ref WS1 'No `TRIGGER` exists anywhere in the migration set. Invariant I1 is held only by the compare-and-swap insert in `pollis-delivery/src/commit.rs` — the protocol layer, not the schema layer — so a bug in our own server code could still produce a gap or a rewrite.

Implements phases 1-5 of `docs/backend-core-invariants.md`.

**Migrations are additive, and 000007 is a deliberate permanent hole — do not reuse it.**')" "security"

create "WS1: Harden the tombstone sent_at floor against envelope GC" \
"$(ref WS1 'See "PL-05 analysis (a)" in the epic doc.

Ingest selects `sent_at > last_fetched_at` and never rewinds the cursor (`pollis-core/src/commands/messages/ingest.rs:148-159`), so an envelope stamped at or below a device watermark is skipped **permanently**.

`sent_at_after()` (`pollis-delivery/src/messages.rs:203`) guards tombstones against this, but its floor is `MAX(sent_at)` over envelopes **still present** (messages.rs:511-522). Once every member device has reported a watermark, GC prunes those envelopes; the floor then goes NULL, the stamp falls back to wall-clock `now`, and the clock-skew dependency the guard was written to remove is back.

`conversation_watermark.last_fetched_at` rows outlive envelope GC, so the floor should be the greater of `MAX(sent_at)` and `MAX(last_fetched_at)`. Cheap and strictly safer.

Latent hazard reasoned from code, not observed in the wild.')" "bug"

create "WS1: Find the actual root cause of flaky #661" \
"$(ref WS1 'See "PL-05 analysis (b)" in the epic doc. **The cause is unconfirmed — instrument before changing anything.**

An earlier hypothesis (the tombstone `sent_at` floor going NULL after GC) was checked and **ruled out for this test**: bob sends and never fetches, and only `ingest.rs` advances watermarks, so bob has no `conversation_watermark` row, the `COUNT(devices) = COUNT(watermarks)` check fails, nothing is pruned, and the floor stays non-NULL. The tombstone therefore sorts above carol'"'"'s watermark and she should fetch it.

Remaining candidates, in order:
1. Read-after-write visibility between the DS write connection and carol'"'"'s read connection — the issue author'"'"'s own hypothesis and the best fit for a timing-dependent 1-in-18 failure.
2. The tombstone is fetched but not applied because carol'"'"'s MLS state has not reached the required epoch during interleaved replay.

Do not add a retry/sleep to the assertion until the mechanism is known — that would convert a possible real dropped-tombstone bug into a green test.')" "bug"

echo
echo "== WS2 - latency and cost =="
create "WS2: Cache device public keys in the Delivery Service (#658)" \
"$(ref WS2 'Every signed endpoint runs `auth::verify_request` -> `lookup_device_pubkey`, one Turso query per request. Unsigned endpoints measure 0ms server-side; signed ones 2000-4700ms. A first message in a fresh mobile group chains ~14 signed calls, producing 15-60s of user-visible latency.

The DS runs at `max_instances: 1`, so there is exactly one process and no cross-instance coherency problem.

**Revocation safety is the hard requirement.** A revoked, logged-out or re-signed device must never authenticate from a stale entry: evict on `/v1/devices/revoke`, `/v1/auth/logout`, `/v1/devices/resign` and the account-lifecycle routes, and add a short TTL as defence in depth. Negative lookups must not be cached in a way that breaks enrollment.

The security regression test — a cached device that authenticates, is revoked, then must fail — is the most important test in this change.')" "bug" "security"

create "WS2: Drop scale-to-zero on the Delivery Service (#515)" \
"$(ref WS2 '`sleepAfter = "10m"` in `pollis-delivery/worker/index.ts:66` is a pre-launch cost saver that adds a cold-start penalty to the first request after any quiet period. The in-code TODO already states it must go before real users arrive.

Cost delta should be stated explicitly in the PR so the tradeoff is on the record.')" "infra"

create "WS2: Add dependency-layer caching to the Delivery Service image" \
"$(ref WS2 'The Dockerfile does a full `cargo build --release` from a `COPY . .`, so every deploy recompiles the whole dependency graph.

Note `.dockerignore` already excludes `website/` (136MB), `.git/`, `target/` and `node_modules/`, so build-context size is NOT the problem — dependency recompilation is.

**Measure before and after.** The benefit depends on whether the Cloudflare build path preserves layer cache between builds; if it does not, cargo-chef layering could make things worse. Do not merge on theory.')" "infra"

create "WS2: Move committed media out of the repository to R2" \
"$(ref WS2 '`website/learn` holds ~77MB of mp4/webm and `website/vendor` a 23MB WASM runtime plus a 22MB ONNX model. `docs/deployments.md:55` already notes this is a one-line base-URL swap.

Every clone and every CI checkout gets cheaper permanently. Note this does not affect the DS image build, which already excludes `website/`.')" "infra"

create "WS2: Make the MLS test workflow genuinely merge-blocking" \
"$(ref WS2 '`mls-tests.yml` is the only `cargo test` gate and is `paths-ignore`-filtered, so it cannot simply be marked required — GitHub leaves required-but-skipped checks pending forever, which the workflow'"'"'s own comment warns about. Resolve via the ruleset "skipped = pass" option.

The 11 `e2e-*.yml` workflows stay dispatch-only by design; this ticket must not add always-on CI load.')" "infra"

echo
echo "== WS3 - operator ceremonies (owner only) =="
create "WS3: Rotate the transparency-log signing key and re-pin it (#672)" \
"$(ref WS3 'Operator ceremony — needs custody of the log signing key, cannot run from CI or a dev box. The runbook is in #672.

Until this lands, `PINNED_LOG_PUBLIC_KEY` is `None` and every in-app transparency audit resolves to *unverified*. That is safe by design (an absent pin can only withhold trust, never grant it) but the verification badge stays dark for every user.

Done means: the public verifier and the in-app check both report verified against the new key on all three trees (commit-log, account-keys, binaries).')" "security"

create "WS3: Harden STH signing-key custody and design a rotation plan (#662)" \
"$(ref WS3 'The signing key currently lives as a CI secret and signs all three trees for every pinned client. A leaked CI secret would let an attacker mint a valid-looking signed head that every client accepts, and there is no rotation path today.

Decide and document: HSM/KMS or offline signing so the raw key never sits in CI; a small pinned key set with an overlap window designed before it is needed; and the compromise-recovery story.')" "security"

create "WS3: Replace stable identifiers in the public ledger with rotating pseudonyms (#662)" \
"$(ref WS3 '`verifiable-log-builder` publishes stable `conversation_id` and real `sender_id` in the clear at `/v1/entries.json`, so anyone can build a longitudinal activity map of every group. The learn material already states this exposure honestly; this is the mitigation.')" "security"

create "WS3: Define and publish the metadata retention policy (#662)" \
"$(ref WS3 'Define retention periods, IP handling, push-token contents, and exactly what the public ledger exposes. Engineering drafts; owner signs off.

**Blocks the mobile store privacy forms** and the public "what we can and cannot see" page.')" "security"

create "WS3: Heal the split-brain relay pool and force cycling on image rolls (#677)" \
"$(ref WS3 'Relay nodes pull `:latest` at boot only, so a rolling image push does not update them in place and the pool is serving two ALPN versions at once. Terminate the stale instances so the ASG relaunches them; then make image rolls trigger an instance refresh rather than waiting for natural replacement.

Needs AWS console access. The overlay is off by default and fails closed, so users are unaffected today.')" "infra"

create "WS3: Hash the pre-signature payload for macOS and Windows (#603)" \
"$(ref WS3 '`scripts/attest-binaries.sh` extracts the app bundle from the already-signed, already-notarized artifact, so the payload leaf hashes bytes that include a per-signing-operation signature and an Apple-minted ticket. Those leaves are unreproducible by construction, forever, by anyone — including us on a matching runner.

Hash the pre-signature payload and log the signed wrapper as a separate derived hash bound to it, which is what `docs/reproducible-builds-residuals.md` already claims we do.')" "bug" "security"

echo
echo "== WS4 - mobile to review =="
create "WS4: Register developer accounts and claim the real bundle identifier" \
"$(ref WS4 'The app still ships the `create-expo-app` default `com.anonymous.mobile`, hardcoded in `mobile/app.json` (x2), `mobile/scripts/maestro-run.sh` and `mobile/.maestro/README.md`.

Enroll in the Apple Developer Program, and **register Google Play as an organization** — organization accounts are exempt from the 12-testers-for-14-continuous-days closed-testing gate that personal accounts created after Nov 2023 must serve before applying for production access.

Organization registration requires a D-U-N-S number, which makes this **the longest lead item in the entire plan**. Start it first.')" "mobile"

create "WS4: Stand up EAS build profiles and release signing" \
"$(ref WS4 'No `eas.json` exists and CI has never built the app itself — `mobile-core-check.yml` only cross-compiles the Rust core with `cargo check`. Create the build profiles, wire release signing for both platforms, and produce a first installable build.

iOS requires Xcode 26 / macOS 26 (Expo SDK 55). The primary dev box is headless Linux and cannot do this.')" "mobile"

create "WS4: Turn on push notifications (#344)" \
"$(ref WS4 'Both halves are already written: `pollis-core/src/commands/push.rs` sends via Expo Push, and `mobile/lib/push/` plus `hooks/usePushNotifications.ts` handle registration and taps. Push is silently dead only because `app.json` has no `expo.extra.eas.projectId`, so `getExpoPushTokenAsync` cannot mint a token and registration no-ops.

Add the EAS project plus APNs key and FCM v1 service-account credentials, then verify delivery to a physically locked device. The payload is deliberately content-free.')" "mobile"

create "WS4: File the mobile compliance paperwork" \
"$(ref WS4 'Missing entirely: `PrivacyInfo.xcprivacy` privacy manifest, privacy-policy URL, Play Data-safety form, content/age rating, and the **export-compliance declaration**.

The last is load-bearing and is *not* the trivial "No" path most apps take — Pollis uses non-exempt encryption (MLS E2EE), so it needs the real declaration and the annual self-classification filing.

Collecting no user data makes every one of these forms unusually easy to answer honestly. **Blocked by the retention-policy ticket in WS3.**')" "mobile"

create "WS4: Run the Maestro parity suite for the first time (#621)" \
"$(ref WS4 'The flows were authored against the screens but have **never been executed anywhere** — 8 authored, 5 scaffolded only. The headless Linux dev box cannot run simulators or emulators, so this needs Mac hardware. Expect real breakage on first contact; that is the point of the ticket.

**Blocked by the DS latency fix** — a 30-second first message would fail store review regardless of how good the paperwork is.')" "mobile"

create "WS4: Close the residual mobile parity gaps" \
"$(ref WS4 'From the closed #623 and `mobile/CLAUDE.md`: wire the `dm/info` actions; handle the realtime events currently decoded and then ignored (`all_mention`, `member_role_changed`, `roster_changed`); implement `device_revoked` self-sign-out on the inbox connection; and verify the iOS keystore (`ios_kek`) on real hardware.

Also purge the three stale "not implemented yet" comments describing things that now exist.')" "mobile"

create "WS4: Produce store listings and genuine iPad screenshots" \
"$(ref WS4 'No store metadata of any kind exists — no listing copy, no screenshots, no privacy-policy URL in the mobile config.

Apple rejects universal apps that are merely scaled-up iPhone layouts. The adaptive tablet work already exists (`hooks/useLayoutClass.ts`, `components/MasterDetail.tsx`); it needs validation on iPad hardware and its own screenshots.')" "mobile"

create "WS4: Submit to TestFlight and Play closed testing" \
"$(ref WS4 'Internal TestFlight testing needs no review; external beta needs Beta App Review. Play closed testing starts the clock on production access.

This doubles as the go-to-market early-access motion rather than being a separate launch step.

Blocked by every other WS4 ticket.')" "mobile"

echo
echo "== WS5 - repo truth =="
create "WS5: Remove the Electron ghost" \
"$(ref WS5 'The app ships from `src-tauri` and there is no `electron/` directory, yet: `docs/electron-migration.md` still reads "Status: code-complete, awaiting end-to-end GUI verification + cutover decision" for a migration that was **reversed** (#386/#389); `frontend/src/bridge/runtime.ts` defines an `ElectronAPI` whose comment says it mirrors `electron/src/preload.ts`, a file that does not exist; ~12 bridge modules branch on `hasElectron()`, permanently false in the shipping build; and `MigrationBanner.tsx` is unreachable dead UI.

Behaviour-preserving for the Tauri path — this is a deletion, not a refactor.')" "chore"

create "WS5: Reconcile contradictory status documentation" \
"$(ref WS5 '#455 is documented as both DEFERRED (`relay-overlay-design.md:384`, `README.md:80`) and deployed (`metadata-minimization-design.md:78`). `AUDIT-slice3.md` sits at the repo root describing a merged branch and flags `fetch_mls_key_package` as a blocker although that function no longer exists. `frontend/docs/frontend-audit.md` analyses a Zustand store the app does not use (state is MobX + React Query). `.codesight/wiki/commands.md` is materially behind the ~167-command surface.')" "chore"

create "WS5: Bring the issue tracker in line with reality" \
"$(ref WS5 '#185 describes mobile as static/demo where "nothing real flows" and lists architectural decisions to make — but the app is now ~95% feature-wired against the real backend. #157 and #163 are written entirely against electron-builder, `electron/build/*.plist` and `pollis-node`, none of which exist post-Tauri.

Close what is done, rewrite what is stale, and park what belongs in WS6 so the open list reflects actual remaining work.')" "chore"

echo
echo "Done."
(( DRY )) && echo "(dry run — nothing was created)"
