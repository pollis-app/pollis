---
name: pre-release-sweeps
description: One-off drift sweeps for Pollis, run by hand before a release that affects every app (desktop, mobile, CLI, DS, website together). `node scripts/sweep.mjs` runs every tier-1 checker, works out what changed since each dimension was last swept (scripts/sweep-ledger.json) and lists what is due; this skill then follows each due dimension's procedure: public facts (#1722), published pollis-verify commands and installer env vars (#1721), public claims (#1723), feature parity, i18n, the surface registry and platform coverage matrix (#1728). Not CI; never gates a merge. Use before a full release, when /release-check says "sweeps due", or when asked to "run the sweeps".
argument-hint: "[dimension ...] [--since <ref>] [--deployed-only]"
---

# Pre-release sweeps

These checks keep what Pollis publishes (website, `/learn`, whitepaper, docs,
store copy) and how it is tested true to what the code does. They are
deliberately **not** in CI: a stale sentence on a docs page must never hold up
an unrelated merge. Run them once, by hand, before a release that ships every
app together, and fix what they find in one sweep PR (or in the release PR).

Everything you need is in this file. Read the repo's `CLAUDE.md` first for the
vocabulary rules it references; the engine is `scripts/sweep.mjs`, the registry
of public surfaces is `scripts/surfaces.json`, and the last-swept commit per
dimension is `scripts/sweep-ledger.json`.

**Arguments.** Any dimension names (`facts cli claims …`; `node scripts/sweep.mjs --list`
names them all) limit the sweep to those. `--since <ref>` makes every dimension
diff from `<ref>` instead of its ledger entry (e.g. the last release tag).
`--deployed-only` skips to § After the website deploy.

## 0. Ground rules

- Work on a branch off an up-to-date `main` (`fix/sweep-<date>` or the release
  branch), never on `main`. One PR for the whole sweep.
- **Policy pages are flag-only:** `website/privacy.html`, `terms.html`,
  `subprocessors.html`, `legal-requests.html`, `docs/metadata-retention-policy.md`
  (`policy: true` in `surfaces.json`). Never edit them. If one disagrees with the
  code, mark it `disputed` (facts or claims) and list it under "Needs Dan's
  decision" in the PR.
- Refer to tickets by number (#1723), never by lane labels.
- A dimension reported **NOT BUILT** is a lane that has not landed. Say so in
  the report; never count it as passing, and do not build it here.
- Findings outside the sweep's scope: comment on epic #1720, do not open issues.

## 1. What is due

```bash
git fetch origin && git switch -c fix/sweep-$(date +%F) origin/main
node scripts/sweep.mjs [dimension ...] [--since <ref>]     # checkers + due list, ~15s plus the cargo test
node scripts/sweep.mjs --quick                              # same, skipping the cargo-built CLI test
node scripts/sweep.mjs --json                               # machine-readable, for a long sweep
```

For each dimension it prints the checker result (`PASS`, `FAIL`, `FINDINGS`
for a report-only checker, `SKIPPED` with `--quick`, `NOT BUILT`), then what is
due since the dimension's last sweep:

- `due … since <sha>`: the listed items changed; follow that dimension's section below.
- `never swept`: no ledger entry yet; run the dimension's full procedure once.
- `nothing since <sha>`: skip the section unless its checker failed.
- `mechanical`: the checker is the whole sweep; fix what it names.

Exit status 1 means a checker (or the registry check) failed. The last line is
the summary `/release-check` also prints.

Then work through the sections below for every dimension that failed or is due.

## surfaces (#1728)

`scripts/surfaces.json` names every public content item (website pages, `/learn`
topics, public docs, user-facing READMEs, installers, store listings), every
store, and the features whose behaviour varies by platform or skin. sweep.mjs
checks it in both directions: every tracked file under `registration.globs` is
claimed by a content item or listed under `internal` with a reason, and every
registered path still matches a file. It also cross-checks `checked_by`
against `claims.json` → `audited_docs` and `facts.json` → `asserted_in`.

- **Unregistered file:** add it under `content` with `source`, `rendered`,
  `checked_by` (which dimensions check it) and `stores` if it is store copy; or
  under `internal` with the reason it is not public.
- **Registered path matches nothing:** the page moved or was deleted; re-point or remove the entry.
- **Due (files added since the last sweep):** confirm each new item's
  `checked_by`. A new public page with claims belongs in `claims.json` →
  `audited_docs` too (§ claims); one stating numbers, in `facts.json` (§ facts).

## facts (#1722)

```bash
node scripts/check-facts.mjs          # repo copies vs the code/config that makes them true
./scripts/test-check-facts.sh         # only if check-facts.mjs itself changed
```

- A failure prints `file:line`, the stated value, the value the code says, and
  a fix hint. Fix the stale copy, or if the code is wrong, the code.
- "pattern not found" means copy was reworded or moved: re-anchor the
  assertion in `scripts/facts.json`.
- `disputed` facts (policy pages vs code) only warn. List the warnings for Dan.
- **Due:** "facts whose source changed" are re-proved by the checker; nothing
  more to do once it passes. "Pages stating facts changed": read their diff
  (`git diff <sha>..HEAD -- <file>`) for a NEW public number or policy value
  with code behind it, and register it (`facts.json` → its `value_from` and
  every `asserted_in`; grep the number and its word form across `website/`,
  `learn/`, `docs/`, `README.md`, `CLAUDE.md`).

## cli (#1721)

```bash
cargo test -p verifiable-log-serve --test doc_invocations -- --include-ignored
```

sweep.mjs runs this unless `--quick`. Every `pollis-verify …` command in docs,
website, `/learn` and workflows must parse against the shipped CLI, and every
subcommand and flag must be shown in a public doc. If the concrete-command list
is stale, regenerate it with `UPDATE_DOC_INVOCATIONS=1` on the same command and
commit the result.

Then run the concrete commands against the live log with the released binary
(read-only; sweep.mjs does not do this part). `remote` replays the whole log
and takes several minutes; the rest take seconds. Run it under bash as written
(zsh does not split `$line` into arguments, so every command would fail), in
the background, without a short per-command timeout:

```bash
bash <<'SWEEP'
set -uo pipefail
tag="$(gh release list --limit 100 --json tagName,publishedAt \
  --jq '[.[] | select(.tagName | startswith("pollis-verify-v"))] | sort_by(.publishedAt) | last | .tagName')"
# linux: pollis-verify-linux-x86_64; Intel mac: pollis-verify-macos-x86_64
bin=pollis-verify-macos-arm64
dir="$(mktemp -d)"
gh release download "$tag" -D "$dir" -p "$bin" -p "$bin.sha256"
(cd "$dir" && shasum -a 256 -c "$bin.sha256") && chmod +x "$dir/$bin"
while IFS= read -r line; do
  [ -n "$line" ] || continue
  read -ra args <<<"$line"
  if "$dir/$bin" "${args[@]}" >/dev/null 2>&1; then echo "ok   $line"; else echo "FAIL $line"; fi
done < verifiable-log-serve/tests/doc_invocations.live.txt
rm -rf "$dir"
SWEEP
```

**Due:** CLI code changed, or pages showing commands changed: the two runs
above are the whole re-check. A live `FAIL` with the parse test green means the
log or the release is at fault, not the docs: report it.

## installers (#1721)

```bash
python3 scripts/check-install-env.py
```

Every `POLLIS_*` variable `website/install.sh` / `cli-install.sh` reads must be
documented (in `docs/deployments.md`), and no doc may show one an installer
ignores. **Due** when an installer or that doc changed: the checker is the re-check.

## claims (#1723)

```bash
node scripts/check-claims.mjs          # ledger valid, every anchor exists, every quoted sentence still present
node scripts/check-learn-sync.mjs      # /learn captions and transcripts match their narration scripts
./scripts/test-check-claims.sh         # only if the claims tooling itself changed
node scripts/claims-impact.mjs --since <sha from the due line>
```

If claims are due, run `/sweep-claims` (`.claude/skills/sweep-claims/SKILL.md`)
inside this sweep: it re-verifies just those claims with its rubric, fixes the
copy, and bumps `verified_sha` and the `claims` ledger entry itself. "Audited
docs changed" is its step 2.4: read those diffs for NEW claims. Disputed
policy-page claims only warn; list them for Dan, never edit the policy pages.

## feature-parity (#1347)

```bash
node scripts/feature-parity.mjs                 # unexplained gaps today
node scripts/feature-parity.mjs --since <sha>   # gaps new since the last sweep (the due list)
```

Desktop and mobile ship together (CLAUDE.md). For each new gap: if it was a
mistake, name the PR that should have carried it and fix it (or comment on
#1720 if it is too big for the sweep); if it is deliberate, add it to
`scripts/feature-parity.json` with its reason. Don't relitigate `known_debt`.

## i18n (#1720)

```bash
node scripts/i18n-check.mjs
```

It fails on anything wrong (orphaned keys, plural categories, placeholders,
empty values, unresolved `t()` calls) and warns on missing keys, which render
English. **Due:** "N en keys added since <sha>; missing in K locales". Translate
the new keys into every locale under `frontend/src/i18n/locales/` (mobile reads
the same catalogues), keeping placeholders and the locale's CLDR plural
categories, then re-run the checker. The older missing-key backlog it warns
about is not this sweep's to clear.

## i18n-surfaces (#1724)

User-facing copy that escapes the catalogues. Not built yet: report it as
"not yet built (#1724)". When #1724 lands, its data file
(`scripts/i18n-surfaces.json`) and checker are wired into `DIMENSIONS` in
`scripts/sweep.mjs` and its procedure goes here.

## ui-inventory (#933), ds-config (#760)

```bash
node scripts/ui-inventory.mjs --check     # .codesight/wiki/ui.md lists every component; fix: node scripts/ui-inventory.mjs
python3 scripts/check-ds-config-chain.py  # every DS config value reaches the container
```

Mechanical: the checker is the whole sweep, so they keep no ledger entry. Fix
what a failure names.

## store-assets (#1729)

Store screenshots and promo images generated from the current UI. Not built
yet: report "not yet built (#1729)". Until it lands, if mobile UI changed since
the last store build, say so in the release notes so Dan can decide whether the
listing screenshots need retaking.

## duplication (#1731)

Copy-paste clones against a committed baseline. Not built yet: report "not yet
built (#1731)". When #1731 lands, its full-baseline run is wired into
`DIMENSIONS` in `scripts/sweep.mjs` and its step goes here.

## platform-matrix (#1728)

Report only; nothing to fix in the sweep. sweep.mjs generates, from the CI
workflows, the native e2e scenarios (`e2e/*.js`), the Playwright specs and the
Maestro flows, which platform × skin cells have e2e on every PR, e2e in CI but
not per PR, e2e by hand only, only builds/unit tests, or nothing; and which
features in `surfaces.json` → `features` sit on a cell with no e2e.

**Due:** a platform-varying feature changed since the last sweep. For each one
on a gap cell, say in the PR how it was verified there (by hand on that OS) or
that it was not. New coverage (or a lost workflow) shows up in the matrix; list
the remaining gaps in the PR. Building coverage is separate work: comment on
#1720 if a gap has started to bite.

## Finish: bump the ledger

When a dimension is swept (fixed, or nothing needed fixing), record it:

```bash
node scripts/sweep.mjs --bump facts,cli,installers,feature-parity,i18n,surfaces,platform-matrix
```

It sets each entry to the `main` commit the sweep branch started from (PRs are
squash-merged, so a branch commit never reaches `main`; `--at <ref>` overrides)
and today's date. `/sweep-claims` bumps `claims` itself. Never bump a dimension
you did not sweep, a NOT BUILT one, or on a dry run. Commit the ledger with the
fixes.

## After the website deploy

Once `website-deploy.yml` has run for the release, check the live pages:

```bash
node scripts/check-facts.mjs --deployed https://pollis.com
```

Red here while § facts is green means the site lags `main`: deploy it.

## Report

PR title `Sweep: <range>`. Body: the `sweep.mjs` output (or its summary line
plus each due list), one line per dimension: pass, or what failed and what you
fixed (severity, `file:line`, old → new, evidence `path:line`). Then "Needs
Dan's decision" (disputed policy values), anything you could not verify (a
live command failing because the log is down), the NOT BUILT dimensions, and
the platform-matrix gaps. `Mobile: N/A (docs/website only)` unless app code
changed. If public pages changed, the website must be deployed after merge
(`website-deploy.yml`, then confirm the deployed commit and run § After the
website deploy).
