---
name: reuse-check
description: Pre-PR check for reimplemented or copy-pasted code in Pollis. Lists what the branch's diff adds (functions, components, hooks, types, CSS classes, scripts, constants), searches the same code boundary for an existing equivalent, reuses or extracts inside the boundary, and writes the PR's "Reuse" section. Run after implementing and before pushing or opening any PR, or when asked to "check for reuse" / "check for duplication".
argument-hint: "[<base ref>, default origin/main]"
---

# Reuse check

Duplication is drift: two copies of a helper diverge, and then a fix lands in
only one. Before a PR goes up, check that the diff does not rebuild something
the repo already has, and does not paste a block that should be one function.
It is a per-PR step, so keep it to minutes: this file is everything you need.

Run from the repo root on your feature branch, with your work committed or
staged.

## 0. Boundaries (read once)

Reuse happens **inside** a boundary. `.codesight/wiki/code-boundaries.md` has
the detail; the short version:

| Boundary | Look for existing code in |
|---|---|
| `frontend/` (desktop React) | `frontend/src/components/ui/` first (CLAUDE.md requires its `Button`, `Switch`, `TextInput`, …), then `frontend/src/hooks/`, `frontend/src/utils/`, `frontend/src/services/`, the feature's own folder |
| `mobile/` (Expo, its own pnpm project) | `mobile/components/ui.tsx`, `mobile/components/`, `mobile/hooks/`, `mobile/lib/` |
| Rust crates | the crate's own modules; then the lowest crate it already depends on: `pollis-api` (wire types), `pollis-schema`, `pollis-device-cert`; in `pollis-core`, the helpers in `pollis-core/src/commands/**` (DS calls go through `commands/mls/ds_client.rs`, reads through `commands/ds_reads.rs`) |
| `website/` (static, no build) | `website/script.js`, `website/styles.css`, `website/doc-page.css`, and the other pages' `.js` |

Never, to save lines:
- import across `frontend/` ↔ `mobile/` (share features, not code; logic both
  need goes in Rust in `pollis-core`);
- add a Rust dependency edge between crates (move the helper to a crate both
  already depend on, or leave it);
- merge a desktop/mobile parity twin (the table in `code-boundaries.md`);
- add a bundler or build step to `website/`.

If sharing across a boundary really is the right fix, propose it in the PR's
Reuse section; do not do it silently.

## 1. List what the diff adds

```bash
base="${1:-origin/main}"
git fetch -q origin
git diff --stat "$base"...HEAD
# new top-level names (TS/JS/Rust/CSS), one per line
git diff -U0 "$base"...HEAD | grep -E '^\+' | grep -vE '^\+\+\+' | grep -oE \
  '(export )?(async )?(function|const|class|interface|type|enum) [A-Za-z_][A-Za-z0-9_]*|(pub(\([a-z]+\))? )?(async )?(fn|struct|enum|trait|type|const|static|macro_rules!) [A-Za-z_][A-Za-z0-9_]*|^\+\.[a-z][a-z0-9_-]+ *\{|use[A-Z][A-Za-z0-9]*' \
  | sort -u
```

Also note new files (`git diff --name-status "$base"...HEAD | grep '^A'`):
a new component, hook, util or script file is the most likely duplicate.

Skip what cannot duplicate anything: tests, fixtures, migrations, locale
catalogue keys, generated files.

## 2. Search for an existing equivalent

For each added item, inside its boundary, try three searches (use `rg`; stop
at the first convincing hit):

1. **Name:** the name and its obvious synonyms (`formatBytes` → `bytes`,
   `humanSize`, `fileSize`; `copyToClipboard` → `clipboard`, `writeText`).
2. **Shape:** the props or signature (`rg 'onCopy|value: string'`, a Rust
   signature's argument types).
3. **Behaviour:** the key calls it makes, which survive renaming:
   `navigator.clipboard`, `Intl.DateTimeFormat`, `useQuery(`, `invoke("cmd"`,
   `ds_post`, `serde_json::from_slice`, `ml_dsa`, `.from_utf8`, a CSS
   property set like `display: inline-flex` plus a border.

## 3. Check the diff for pasted blocks

Run the clone ratchet (a few seconds; jscpd is pinned and fetched by
`pnpm dlx`):

```bash
node scripts/check-clones.mjs            # or --boundary frontend,rust for just the ones you touched
```

It scans each boundary separately and fails only on clones that are not in
the committed baseline (`scripts/clones-baseline.json`), printing both
locations of each new one. Every new clone it reports is yours: fix it in
step 4. Then:

- `↓ N baseline clone(s) are gone` means your diff removed duplication: run
  `node scripts/check-clones.mjs --update-baseline` and commit the smaller
  baseline.
- A new clone you decide to keep (step 4's "leave it" cases): run
  `--update-baseline`, commit it, and give the reason in the Reuse section.
- `scanned no files` or `configured path does not exist`: your diff moved a
  directory the config names; fix `scripts/clones.json`.

The detector only sees token-level copies of 5+ lines. Also skim each hunk
over ~10 lines yourself: two components with the same JSX skeleton but
renamed props, two Rust functions with the same `match` and error mapping,
the same fetch + parse + fallback written twice.

## 4. Fix it

In order of preference:
1. **Reuse** the existing thing as is.
2. **Extend** it (a new optional prop or parameter), keeping every existing
   caller working unchanged.
3. **Extract** the shared part to the nearest shared place inside the
   boundary (table in step 0) and point both copies at it.

Keep behaviour identical, and make sure a test covers the extracted code (add
one if none does). Re-run the touched tests.

Leave it duplicated, and say so, when sharing would cross a boundary, couple
two things that should change independently, or need an abstraction more
complex than the copies.

## 5. Write the PR's Reuse section

One short section in the PR description:

```markdown
## Reuse
- Reused: `TextInput` for the new field (was a hand-styled `<input>`).
- Extracted: `parseRetryAfter()` to `pollis-core/src/commands/mls/ds_client.rs`; both callers use it.
- Not shared: the mobile twin of `FooRow`; parity pair across frontend/ ↔ mobile/ (code-boundaries.md).
```

If nothing was found: `## Reuse` then `Checked with /reuse-check: nothing to reuse or extract.`
