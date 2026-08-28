# <YYYY-MM-DD> — <one-line title>

**Severity:** sev1 | sev2 | sev3 | sev4
**Correctness impact:** yes | no — and *why*, in terms of `docs/incidents/README.md` §2 (did any device
stop reporting a watermark for long enough to matter, was any message lost, was the transparency log
affected?)
**Duration:** <start UTC> → <end UTC> (<n>h <n>m)
**Components:** <ids from `website/status-history.json` targets>
**Detected by:** <status-probe.yml | website-verify.yml | a user | the owner | a monitor>

---

## What users experienced

Plain language, no hedging. What failed, for whom, for how long. If some users were unaffected, say
which and why. If we do not know, write that we do not know — the record is worth less than nothing if
it is confident about things we did not measure.

## Timeline (UTC)

| Time | Event |
|---|---|
| | First failing observation (link the `status-history.json` sample or the failing run) |
| | Detected |
| | Cause identified |
| | Mitigation applied |
| | Resolved |
| | Confirmed resolved by an independent check |

**Time-to-detect matters more than time-to-fix here** and is recorded separately for that reason. A
one-person project has no on-call rotation; the honest number goes in.

## What happened

The mechanism, not the narrative. Name the file, the workflow, the config value.

## Why it was possible

The condition that allowed it, stated at the lowest layer it could have been prevented at — the
`docs/backend-core-invariants.md` ladder: a database constraint beats a Rust type beats a protocol
chokepoint beats code discipline. If the answer is "code discipline", say so; that is a finding.

## Message-delivery consequences

Answer all three explicitly, even when the answer is "none":

1. Was any message permanently lost? Which of the three accepted losses (`CLAUDE.md`) does it fall
   under, if any — and if none, it is a defect, not an accepted loss.
2. Did any device stop reporting a watermark, and for how long relative to
   `POLLIS_DS_WATERMARK_STALE_MONTHS`?
3. Was any transparency tree unpublished, forked, or unverifiable for any window?

## What we changed

Each item links a merged PR or an open issue. An action item with neither is not an action item.

- [ ] …

## What we did not change, and why

The rejected fixes. This section is what makes the document worth reading in a year.

## Corrections

Append dated corrections here rather than editing above. Never silently rewrite a published record.
