---
name: mobile-release-notes
description: Write the App Store / Google Play "What's new" notes for the next Pollis mobile release. Finds the last version each store approved, lists the PRs merged since then that reach phones, and turns them into short user-facing notes saved to mobile/store/release-notes/<version>.<platform>.txt. Use before a mobile store release (the mobile-release skill calls it), or when asked for mobile release notes, "what's new" or a mobile changelog.
argument-hint: "ios|android|both [ref]"
---

# Mobile release notes

The notes cover everything a phone user gets since the last version **the store
approved**, not since the last upload: a build that was uploaded but never
reviewed, or was rejected, shipped nothing to anyone. The two stores approve
different versions, so iOS and Android each get their own file.

## 1. Gather

From the repo root, once per platform (`both` = run both):

```bash
python3 -I mobile/scripts/release-changes.py ios       # or android; --to <ref> for a commit other than origin/main
python3 -I mobile/scripts/release-changes.py ios --json  # same, with full PR bodies
```

It prints the baseline (iOS: the highest App Store version App Review
approved; Android: the highest versionCode Play production released), the
commit that baseline was built from (via `mobile/store/runtime-versions.json`),
and every first-parent commit since then touching `mobile/`, `pollis-core/` or
`pollis-api/`, with its PR title, labels, the first paragraph of its body and
the top-level areas it touched. Release bookkeeping (ledger entries, version
bumps, earlier notes) is already left out. Commits that only touch tests,
Maestro flows, docs or CI are marked `(tests/docs only)`.

If it stops with "not in runtime-versions.json", the approved build was never
recorded. Record it (mobile-release skill, step 6) before writing notes.

## 2. Choose what goes in

Keep only what a phone user notices: a new feature, a visible fix, a change in
behaviour. Read the PR body (`--json`, or `gh pr view <n>`) whenever the title
does not make that clear.

Leave out:
- **Desktop-only work.** A change whose areas include no `mobile/` reaches a
  phone only if its PR says it changes mobile behaviour. Voice, video,
  screenshare and all real-time media are desktop-only by product decision:
  never list them for mobile.
- Anything marked `(tests/docs only)`, CI, Maestro, refactors, dependency
  re-pins and internal hardening with no visible effect.
- Fixes for something that never shipped in an approved version. For example,
  a bug introduced and fixed between the baseline and this release.

Merge related PRs into one line. One feature built over several PRs is one
bullet.

## 3. Write

```
New
• <a feature, in the user's words>
• …

Fixes
• <what the user saw go wrong, now right>
• …
```

- Plain words the user already knows. No PR numbers, versions, ticket ids or
  internal names (outbox, MLS, OTA, ingest, Maestro, DS, core, NSE).
- One line per bullet, starting with what the user sees. Sentence case. No
  exclamation marks, no marketing, no promises.
- No "end-to-end encrypted" or other security copy (house rule for mobile copy).
- Name the platform's own words: "App Store" on iOS, "Play" on Android, and
  never mention the other platform or desktop.
- Leave out an empty section. If there are only fixes, use one section,
  "Fixes and improvements".
- **Length:** iOS allows 4000 characters. Play allows **500**, so for Android
  pick the 4 to 6 changes users will notice most and end with
  "• Other fixes and improvements." when more were left out.

## 4. Save and show

The version is `mobile/package.json` `version` on the release commit (the one
being shipped, not the baseline). Save the notes as
`mobile/store/release-notes/<version>.<platform>.txt` (for example
`1.0.31.ios.txt`), then check the length:

```bash
wc -m mobile/store/release-notes/<version>.<platform>.txt   # ≤ 4000 iOS, ≤ 500 Android
```

`asc-submit.py` and `play-publish.py --notes-file` check the limits again and
refuse longer text.

Show the notes to the user in the reply: the user owns the copy. The file is
committed in the release's version-bump PR (mobile-release skill), or in its
own `fix/` PR if the notes are written on their own.
