---
name: release-check
description: Post-merge / pre-deploy release check for Pollis. Reports, for every deploy target (dev DS, prod DS, desktop, mobile store/APK/OTA, pollis-verify, transparency log, LiveKit, public docs, website, Pollis CLI), whether main has changes the live deployment lacks, and whether desktop, mobile and the CLI have drifted apart in features. Use after merging a PR, before running any deploy or release, or when asked "what needs deploying?". Advisory only; it never deploys or blocks.
argument-hint: "[PR number | ref]"
---

# Release check

This is the executable half of `docs/deployments.md` → "Post-merge release
checklist". That section is the policy (blast radius, ordering rules, how to
confirm a deploy landed); this skill gathers the facts and walks the decision.
It never deploys anything by itself. Deploying is a separate, deliberate step.

## 1. Gather

Run from the repo root:

```bash
scripts/release-impact.sh            # every target: live version vs main
scripts/release-impact.sh --pr <N>   # if a PR number was given: which targets it feeds
node scripts/feature-parity.mjs --since <last release tag>   # parity gaps new since the last release
node scripts/feature-parity.mjs      # gaps that are neither exempt nor known debt
```

For `--since`, use the newer of the latest `v*` tag and the latest `mobile-v*`
tag (`git tag -l 'v*' 'mobile-v*' --sort=-creatordate | head -1`), or the ref
the user named.

What the rows mean:
- `current`: live has everything on main that feeds it.
- `STALE`: main has commits under the target's source paths that live lacks.
  The commits are listed.
- `DEPS?`: only `Cargo.lock` moved. Check whether the target actually links
  the changed crates (`cargo tree -p <crate> -i <dep>`) before shipping.
- `UNKNOWN`: the live version could not be read (network, `gh` auth, or no tag
  yet). Say so; never treat it as current.

The live version is exact for the DS (`/version` SHA) and for tag releases. It
is a proxy for mobile store builds (latest `mobile-v*` tag), the transparency
log and relay image (last green run), LiveKit (deployment record) and the
website (main at the last deploy). Mention a proxy when it matters to the call.

## 2. Decide, per target

Output one table, every target, every time:

| Target | Status | Decision | Why |
|---|---|---|---|

Decision is one of **deploy**, **defer (reason)** or **N/A**. Deferring is
fine; forgetting is the bug. Rules that change the decision:

- **Ordering.** Read the ordering notes under "Step 2" in
  `docs/deployments.md` whenever the DS and a client both changed. The usual
  rule is DS first: dev, verify, then prod, then confirm `/version`, then the
  client release. Wire-contract changes in `pollis-core` (signing, DS request
  or response shapes, r2 presign, `livekit_jwt`, MLS envelope) need a matching
  hand-written change in `pollis-delivery`, because it doesn't compile against
  core.
- **Mobile.** Native changes (anything in `pollis-core`'s closure or the mobile
  fingerprint) need store builds plus the sideload APK; an OTA cannot carry
  them. JS-only changes can go by OTA. Store builds follow `mobile/CLAUDE.md` →
  "Store builds", then tag `mobile-v*`.
- **Desktop and CLI** release together from one `v*` tag.
- **Migrations** listed as pending ship with whichever runs first: the next prod
  DS deploy or the next desktop release.
- **Transparency log.** Its daily cron republishes on its own, so a STALE row
  means "the next run picks it up". If the last runs failed, that is a bug to
  fix, not a deploy to defer.
- **Public docs.** If a merged feature or fix changes something a public page
  states (`website/*.html`: security, retention, continuity, faq, learn,
  android), reconcile the page in the same cycle, then deploy the website.
  `website-verify.yml` checks the pinned key, the cert and the ledger, but not
  prose.

## 3. Parity

Desktop and mobile ship together (CLAUDE.md). For every gap the parity check
reports:
- it was a mistake: name the PR that should have carried it, and file or fix;
- it is deliberate: add it to `scripts/feature-parity.json` with its reason, in
  the PR that made it so.

Gaps listed only under "known debt" predate this check. Mention their count;
don't relitigate them unless asked. When one is paid off, the script asks for
it to be removed from `known_debt`.

## 4. Deploy (only when authorized), then verify

If the user has authorized deploys, deploy what the table says, in the order it
says. Then confirm each one is live: re-run `scripts/release-impact.sh` and
expect `current`, or use the target-specific check in `docs/deployments.md` →
"Step 3". A workflow that turned green is not proof the new build is live.

Record the decision table in the PR description or a comment on the merged PR,
so the next person can see nothing was dropped.
