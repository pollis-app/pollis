# Incident record

How a Pollis incident is recorded, classified and published. Opened under #877, which observed that
`/health` and `/version` had been open on every service for months, nothing published them, and there
was **no incident record of any kind** — so a user watching an outage had no way to tell an outage from
a compromise, and no way to tell either from nothing having happened.

**Two files, one rule.** `website/incidents.json` is the machine-readable index the public
[status page](../../website/status.html) renders. A postmortem is a Markdown file in this directory.
The rule is that **an incident is written down even when nobody noticed** — a record that only contains
the incidents users complained about is a marketing page.

`scripts/check-incidents.py` (gated by `scripts-check.yml`) enforces everything below that a machine
can enforce, so the format cannot rot quietly.

---

## 1. Severity ladder

Severity is about **what a user loses**, not about how much work the fix was.

| Severity | Meaning | Examples |
|---|---|---|
| `sev1` | Message loss, or a break in a cryptographic guarantee | An envelope deleted before every member device collected it; a forked or equivocating transparency log; a key-transparency answer that cannot be verified |
| `sev2` | Users cannot send or receive, or cannot sign in | DS unreachable; Turso down; OTP delivery failing |
| `sev3` | Degraded but working | Slow responses; push notifications delayed; downloads unavailable while the app works |
| `sev4` | No user-visible effect, recorded because it touched the trust path | The transparency publisher failed a run and self-healed; a stale deploy served a page that disagreed with `main` |

## 2. The correctness flag — the Pollis-specific one

Every incident carries `correctness_impact: true|false`, and it is **not** the same question as
severity.

Pollis retains an undelivered envelope until every current member device has reported collecting it —
except that under #720 a device silent for longer than `POLLIS_DS_WATERMARK_STALE_MONTHS` (12) stops
pinning retention, and mail waiting only on it becomes collectable. That bound is keyed on **the
device's last report**, never on message age. A device that cannot reach the DS cannot report.

So an outage does not merely make Pollis unavailable — **it consumes the staleness budget of every
device that is offline for its duration.** A one-hour outage is nowhere near the bound and is honestly
`correctness_impact: false`. An outage, or a client bug, that stops a device reporting for months moves
toward the point where mail is deleted that would otherwise have been delivered, and that is a
`correctness_impact: true` event that must say so on the status page in those words. Do not round this
down because the service came back.

Set the flag `true` whenever the incident either
(a) already caused or could have caused message loss, or
(b) prevented a class of devices from reporting a watermark for a period long enough to matter, or
(c) affected the transparency log's ability to be published or verified.

## 3. Publication deadlines

Deliberately short, and stated so that a missed deadline is itself visible:

- **`status` and `started_at` are published while the incident is open**, not after. An entry with
  `status: "investigating"` and empty fields is the correct thing to publish at minute five.
- **`resolved_at` within 24 hours of resolution.**
- **A postmortem within 14 days for every `sev1` and `sev2`, and for every incident with
  `correctness_impact: true`.** `check-incidents.py` fails the build when one is overdue, so the
  deadline is enforced by CI rather than by memory.
- `sev3` and `sev4` may close with `postmortem: null` and a one-paragraph `summary`.

## 4. Adding an incident

1. Add an entry to `website/incidents.json` (schema in §5). Do it *during* the incident.
2. When resolved, set `resolved_at` and `status: "resolved"`.
3. For anything owing a postmortem, copy `TEMPLATE.md` to
   `docs/incidents/<id>.md` and fill it in; set `"postmortem": "docs/incidents/<id>.md"`.
4. Run `python3 scripts/check-incidents.py`.
5. Deploy the site — `website-deploy.yml` is manual by design (#406), so **an incident entry that is
   committed but not deployed is not published.** This is the single easiest step to forget, and the
   status page is worthless if it lags the incident it describes.

## 5. `website/incidents.json` schema

```jsonc
{
  "record_started_at": "2026-08-28",   // the record makes no claim about anything before this
  "incidents": [
    {
      "id": "2026-08-28-ds-cold-start",   // YYYY-MM-DD-slug, unique, sorts by date
      "title": "One-line description in plain language",
      "severity": "sev2",                 // sev1 | sev2 | sev3 | sev4
      "status": "resolved",               // investigating | identified | monitoring | resolved
      "correctness_impact": false,        // §2 — never left out
      "components": ["ds"],               // ids from website/status-history.json `targets`
      "started_at": "2026-08-28T14:02:00Z",
      "resolved_at": "2026-08-28T15:10:00Z", // null while open
      "user_impact": "What a user experienced, in the words they would use.",
      "summary": "What happened, one paragraph.",
      "postmortem": "docs/incidents/2026-08-28-ds-cold-start.md" // or null, see §3
    }
  ]
}
```

## 6. What the record deliberately does not contain

- **No user identifiers, conversation ids, or counts of affected individuals.** The point of the
  product is that we do not hold that, and an incident report is not a good reason to start
  reconstructing it. "All users of the DS" or "devices that had not synced since X" is the right
  granularity.
- **No claims about legal process.** Requests received, and any canary, are a separate artifact that
  needs the legal entity (#723). An incident report is about failures of our own systems.
- **No retroactive edits to close a story.** Corrections are appended to the postmortem under a dated
  heading. The file's git history is public and a silent rewrite is worse than the original error.
