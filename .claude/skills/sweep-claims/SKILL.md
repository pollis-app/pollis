---
name: sweep-claims
description: Re-verify the public claims (website, /learn, whitepaper, verify docs, store copy) whose anchored code changed since the last claims sweep, fix any that are now false, and bump scripts/claims.json and scripts/sweep-ledger.json. Use when /release-check or release-impact.sh reports "N public claims anchored to changed code", before deploying the website after a security-relevant change, or when asked to sweep or re-check public claims. Incremental; the full baseline audit was #1723.
argument-hint: "[--since <ref>] [--dry-run]"
---

# Sweep public claims

`scripts/claims.json` is the ledger of every falsifiable thing Pollis's public
material says about how Pollis works: the exact sentence (`locator.quote` in
`doc`), what it claims, the code that makes it true (`anchors`), the lines it
was checked against (`evidence`) and the commit it was verified at
(`verified_sha`). None of this runs in CI or gates a merge: sweep checks are
run by hand, by this skill and before a release. `scripts/check-claims.mjs`
catches an anchor that was deleted and a quoted sentence that was reworded;
`scripts/claims-impact.mjs` catches code that still exists but changed under a
claim. Everything you need is in this file; read the repo's `CLAUDE.md`
first for the vocabulary rules it references.

## 0. Ground rules

- Work on a branch (`fix/<ticket>-…` or `fix/sweep-claims-<date>`), never on `main`. One PR for the sweep.
- **Policy pages are flag-only:** `website/privacy.html`, `terms.html`,
  `subprocessors.html`, `legal-requests.html` and
  `docs/metadata-retention-policy.md`. Never edit them. If one disagrees with
  code, set the claim's `status` to `disputed` with a `note` stating both sides,
  and list it under "Needs Dan's decision" in the PR.
- Never call the local DB or its contents "plaintext" (CLAUDE.md → Storage).
  Keep the accepted-loss wording in step with CLAUDE.md → "Messages must work".
- Numbers belong to the facts registry (`scripts/facts.json`, checked by
  `scripts/check-facts.mjs`). A claim with a `fact` field is re-verified by
  running that checker, not by hand.
- No "early access / untested" caveats. A finding is fixed in the product, not disclosed in copy; only genuine by-design properties are stated, plainly (see "Findings are tickets, not copy" in CLAUDE.md).
- **Findings are tickets, not copy.** Never publish known vulnerabilities,
  gaps, incident history or "not yet" items. When the product is weaker than
  the copy, fix the product, or narrow the claim so it is true without a
  caveat. Never delete a sentence whose removal makes a stronger nearby claim
  false; narrow that claim instead. Genuine trade-offs a user needs stay.
- The weakness itself becomes a GitHub issue with a neutral title
  ("Dependency upgrade: quick-xml …", "Sign the per-release VEX …"), not a
  copy edit announcing it. Public issues, comments and PR descriptions never
  spell out vulnerability or exploit details (reachability, affected paths,
  how to trigger it); report those privately per `SECURITY.md`.
- Refer to tickets by number (#1723), never by lane labels.

## 1. What is due

```bash
node scripts/check-claims.mjs                      # validate: schema, anchors exist, quotes still present
node scripts/check-learn-sync.mjs                  # /learn captions + transcripts match their scripts
node scripts/claims-impact.mjs --ledger            # since scripts/sweep-ledger.json → claims.sha
node scripts/claims-impact.mjs --since <ref>       # or since a ref you were given
node scripts/claims-impact.mjs --ledger --json     # machine-readable, with the changed files per claim
```

Every claim `check-claims.mjs` names (a deleted anchor, a reworded quote) is
due as well. The impact output is grouped by doc: each claim id, its one-line
statement, and the changed files that hit its anchors. If nothing is due, skip
to step 5 (bump the ledger) and stop. With `--dry-run`, stop after step 2 and report the list
and your verdicts without editing anything.

## 2. Re-verify each due claim (the rubric)

For each listed claim, read its entry in `scripts/claims.json`, then:

1. Read the quoted sentence in context in `doc` (and in `also_rendered_in`).
2. Read the diff of the changed anchored files (`git diff <since>..HEAD -- <files>`)
   and the code at the claim's `evidence` lines. Decide whether the sentence is
   still true today. Cite `path:line`. If proving it needs runtime evidence
   (deployed config, a dashboard, an ops process), say what evidence would
   settle it and leave or set `status: "unverifiable"` with a `note`.
3. Classify: `wrong` (false today), `outdated` (was true; now incomplete or
   misleading), `incomplete` (true but omits something a user needs),
   `cosmetic` (wording only), or still true.
4. While you are in the diff, look for a NEW public-facing behaviour the
   changed code introduces that a doc in `audited_docs` now states falsely or
   should state. Add a claim for it if a doc already makes it.

If more than ~15 claims are due, use one subagent per doc with this rubric and
have each return structured findings (claim id, verdict, evidence `path:line`,
proposed fix text). Then check every `wrong`/`outdated` verdict against the
code yourself before applying it; subagents are confidently wrong sometimes.

## 3. Fix

- Fix every `wrong` and `outdated` claim in the same PR, and `incomplete` where cheap.
- Where the copy is true only because the product is weaker than it should be
  (a missing check, an unupgraded dependency, an unsigned file), narrow the
  copy to what is true and open a neutrally titled ticket for the product
  work; do not add a sentence announcing the gap.
- **/learn:** the narration script `learn/manim/scripts/<slug>.md` is the
  source of truth. Edit it, then run `node scripts/check-learn-sync.mjs --write`,
  which rewrites the caption track `website/learn/media/<slug>.vtt` and the
  on-page transcript in `website/learn.html` from it. If the video's ON-SCREEN
  text (the strings in `learn/manim/scenes/<slug_with_underscores>.py`) is
  wrong, edit the scene and re-render that topic once
  (`learn/manim/render.sh <SceneClass> <slug> m`, toolchain in
  `learn/README.md`; on macOS `brew install pkgconf pango` first). Copy the
  outputs to a directory under NEW names (`<slug>-r<ticket>.{mp4,webm,jpg}`),
  point the `<video>`/`poster` in `learn.html` at them, and publish them with
  `learn/manim/publish.sh <dir>` before the website is deployed. Uploading is a
  production write: if you may not do it, keep the files outside the worktree
  and list the command in the PR. If you cannot install the toolchain, say so
  plainly in the PR.
- For every claim you touched, update its `locator.quote` to the new exact
  sentence, `claim`, `anchors` and `evidence` if they moved, and set
  `verified_sha` to the commit you verified against (`git rev-parse HEAD` before
  your edits). For claims you re-verified and found still true, bump
  `verified_sha` too.
- A claim whose sentence was deleted on purpose: remove it from the ledger. A
  doc with no claims left must also leave `audited_docs`.

## 4. Check

```bash
node scripts/check-claims.mjs        # ledger valid, anchors exist, quotes present
node scripts/check-learn-sync.mjs    # /learn copies match their scripts
node scripts/check-facts.mjs         # numbers
./scripts/test-check-claims.sh       # only if you changed a checker: it still goes red when it should
```

## 5. Bump the ledger

Set `scripts/sweep-ledger.json` → `claims` to
`{ "sha": "<the commit you verified against>", "date": "YYYY-MM-DD", "by": "sweep-claims" }`.
On a `--dry-run`, change nothing.

## 6. PR

Title `Claims sweep: <range>`. Body: the due list (`claims-impact.mjs` output),
one line per fix (severity, `doc:line`, old → new, evidence `path:line`),
"Needs Dan's decision" for policy-page disputes (worded neutrally: the two
values and where they live, no vulnerability or exploit detail), anything you could not
verify, and `Mobile: N/A (docs/website only)` unless app code changed. If
public pages changed, the website must be deployed after merge
(`website-deploy.yml`, then confirm the deployed commit).
