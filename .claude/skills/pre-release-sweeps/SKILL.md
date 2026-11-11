---
name: pre-release-sweeps
description: One-off drift sweeps for Pollis, run by hand before a release that affects every app (desktop, mobile, CLI, DS, website together). Checks that every public number agrees with the code (#1722), every published pollis-verify command parses and runs against the live log (#1721), and the installers' documented env vars match what they read. Not CI; never gates a merge. Use before a full release, or when asked to "run the sweeps".
argument-hint: "[--deployed-only]"
---

# Pre-release sweeps

These checks keep what Pollis publishes (website, `/learn`, whitepaper, docs,
store copy) true to what the code does. They are deliberately **not** in CI:
a stale sentence on a docs page must never hold up an unrelated merge. Run
them once, by hand, before a release that ships every app together, and fix
what they find in the release PR (or a PR just ahead of it).

Run from the repo root on the commit you are about to release.

## 1. Facts registry (#1722)

```bash
node scripts/check-facts.mjs          # repo copies vs the code/config that makes them true
./scripts/test-check-facts.sh         # only if check-facts.mjs itself changed
```

- A failure prints `file:line`, the stated value, the value the code says, and
  a fix hint. Fix the stale copy, or if the code is wrong, the code.
- "pattern not found" means copy was reworded or moved: re-anchor the
  assertion in `scripts/facts.json`.
- `disputed` facts (policy pages vs code) only warn. Do not edit policy pages
  (`privacy`, `terms`, `subprocessors`, `legal-requests`,
  `docs/metadata-retention-policy.md`); list the warnings for Dan to decide.

## 2. Published pollis-verify commands (#1721)

```bash
cargo test -p verifiable-log-serve --test doc_invocations -- --include-ignored
```

Every `pollis-verify …` command in docs, website, `/learn` and workflows must
parse against the shipped CLI, and every subcommand and flag must be shown in
a public doc. If the concrete-command list is stale, regenerate it with
`UPDATE_DOC_INVOCATIONS=1` on the same command and commit the result.

Then run the concrete commands against the live log with the released binary:

```bash
tag="$(gh release list --limit 100 --json tagName,publishedAt \
  --jq '[.[] | select(.tagName | startswith("pollis-verify-v"))] | sort_by(.publishedAt) | last | .tagName')"
dir="$(mktemp -d)"; bin=pollis-verify-macos-arm64   # linux: pollis-verify-linux-x86_64
gh release download "$tag" -D "$dir" -p "$bin" -p "$bin.sha256"
(cd "$dir" && shasum -a 256 -c "$bin.sha256") && chmod +x "$dir/$bin"
while IFS= read -r line; do
  [ -n "$line" ] || continue
  "$dir/$bin" $line >/dev/null 2>&1 && echo "ok   $line" || echo "FAIL $line"
done < verifiable-log-serve/tests/doc_invocations.live.txt
```

(Check `gh release view "$tag"` for the exact asset name on your platform.)

## 3. Installer environment variables (#1721)

```bash
python3 scripts/check-install-env.py
```

## 4. After the website deploy

Once `website-deploy.yml` has run for the release, check the live pages:

```bash
node scripts/check-facts.mjs --deployed https://pollis.com
```

Red here while step 1 is green means the site lags `main`: deploy it.

## Report

One line per step: pass, or what failed and what you fixed. Anything you
could not fix (a disputed policy value, a live command failing because the
log is down) goes in the release notes to Dan, not silently dropped.
