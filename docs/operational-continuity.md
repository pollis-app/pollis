# Operational Continuity — the engineering half

**Status:** Live. This is the **engineering** half of continuity planning and it is complete as written.
The **legal** half — who owns these assets, who inherits them, and what contract obliges anyone to hand
them over — depends on the legal entity and is deferred to **#723**. Every place that boundary is
reached is marked **→ #723** rather than answered with something that sounds reassuring.
**Audience:** the owner; anyone who would have to take this over; and a security researcher or auditor
deciding how much of the transparency story survives the person who built it.
**Scope:** what breaks, in what order, if the single person operating Pollis stops operating it — and
what an outside observer can and cannot conclude while that is happening. Opened under #877.

Written from the repository, not from intent. Where the honest answer is "nothing covers this today",
that is what it says. **A continuity plan that overstates its coverage is worse than none**: it is the
document a reader uses to decide the risk is handled.

---

## 1. The position, stated without softening

`.github/CODEOWNERS` is one line: `* @actuallydan`. There is **one** holder of every credential in §2.
There is no second holder, no escrow, no dead-man's switch, no successor, and no organisation to inherit
anything (**→ #723**). If that person stopped tomorrow:

- **Running services would keep running** until a bill went unpaid or a certificate expired. Messages
  would keep being delivered. Nothing detonates on the first day, and nobody should read the sections
  below as "Pollis stops if the author is hit by a bus".
- **Nothing could be fixed, rotated, deployed or published.** No release, no security patch, no key
  rotation, no incident response, no transparency-log republish.
- **The transparency guarantees would degrade quietly rather than break loudly**, which is the part
  specific to this design and the reason §4 exists.

That is the whole risk, and it is a real one. The mitigations available *without* a legal entity are
documentation, detection and the removal of ambiguity — which is what this document is. They do not
substitute for the legal half.

## 2. Asset register — what is singly held

Every row is held by one person. The last column is what a *user* loses, not what is inconvenient.

| Asset | Where it lives | What it does | If it is lost |
|---|---|---|---|
| GitHub `actuallydan/pollis` | GitHub | Source, all CI, releases, the published status + incident record | No releases, no CI gates, no published record. The source already in the world stays (PolyForm Noncommercial) |
| Doppler (`pollis/prd`) | Doppler | Source of truth for service secrets; synced to GitHub Actions and the Wrangler Secrets Store | Every secret below must be reissued from its own provider |
| `TAURI_SIGNING_PRIVATE_KEY` (+ password) | GitHub secret | Signs the auto-updater manifests | **The worst single loss.** The matching public key is compiled into every shipped client (`src-tauri/tauri.conf.json` → `plugins.updater.pubkey`), so no future build can ever be accepted as an update by an already-installed app. Every user would have to reinstall by hand |
| `STH_SIGNING_KEY` | GitHub secret, backed up in Doppler | Signs all three transparency trees | The log cannot be republished. Rotating to a new key is **not** cleanly recoverable — see §5 |
| Apple: `APPLE_CERTIFICATE`, `APPLE_ID`, `APPLE_TEAM_ID`, `APPLE_PASSWORD` | GitHub secrets / Apple Developer | macOS signing + notarization; the App Store Connect record | No new macOS builds. Installed apps keep working; notarization of *existing* builds is already done |
| Azure Trusted Signing: `AZURE_CLIENT_ID/SECRET/TENANT_ID`, `AZURE_CERT_PROFILE` | GitHub secrets / Azure | Windows installer signing | No new Windows builds; SmartScreen warnings return for anything signed another way |
| Cloudflare account | Cloudflare | DNS for `pollis.com`, `api`, `cdn`, `verify`; Pages (website); Workers + containers (the DS); R2 (releases + the served transparency tree) | **Everything user-facing stops.** This is the single largest concentration |
| Turso | Turso | Main DB and the commit-log DB | The DS cannot serve. Message *content* is unaffected — it is E2EE and the plaintext was never there |
| AWS account | AWS | The relay hydra (Terraform ASG, S3/CloudFront signed directory, reconciler Lambda, SSM state). CI reaches it by short-lived GitHub OIDC (`RELAY_IMAGE_OIDC_ROLE_ARN`), so there are no standing AWS keys in CI — but the account itself is singly held | The optional relay overlay stops rotating and eventually stops. Pollis still works; IP-metadata protection for those who enabled it does not |
| LiveKit + `VPS_SSH_KEY` | Own VPS | Voice/video/screenshare SFU | Calls stop. Text messaging is unaffected |
| Resend | Resend | Sends the email OTP | **Nobody can sign in or add a device.** Existing sessions keep working |
| `AUR_SSH_KEY` | GitHub secret / AUR | Publishes the Arch package | The AUR package goes stale. Already best-effort by design |
| `POLLIS_OVERLAY_DIRECTORY_KEY` | GitHub secret | Signs the relay directory | Clients reject a directory nobody can re-sign; the overlay stops being usable |
| Expo/EAS, APNs key, FCM service account | EAS (held by EAS, not by this repo) | Mobile push | Mobile is in development; no shipped user is affected today |

**Two observations a reader should draw from that table.** First, the failures are mostly *availability*
and *distribution*, not confidentiality: no row above can read a message, because the servers never hold
plaintext or private message keys. Second, the two rows whose loss is genuinely unrecoverable — the
updater key and the log signing key — are both **signing** keys whose public halves are pinned inside
software already installed on other people's machines. Pinning is what makes them strong and what makes
losing them final.

## 3. Degradation ladder — what breaks, in what order

Assume the operator stops, everything keeps auto-paying, and nobody intervenes.

| When | What happens | Visible to a user? |
|---|---|---|
| Immediately | Nothing. Messages deliver; the DS keeps serving; the site stays up | No |
| Within ~2 days | The daily transparency publish stops. New account keys and commits stop being anchored | Only to someone checking the publisher's run history (§4) |
| Within ~2 days | `status.pollis.com` keeps probing and keeps reporting the DS as up, because it is — an abandoned service and a healthy one look identical from the outside until something expires | No; a live status page is not evidence anyone is operating the service |
| ~60 days | GitHub disables scheduled workflows on an inactive repository. Every cron in this repo stops, including the ones above | Same as above, silently more so |
| Months | Apple/Azure signing credentials lapse on their own renewal cycles. No new signed builds are possible even with the keys | Only on the next release that never comes |
| Any time | A dependency CVE, a protocol bug or an outage goes unfixed | Yes, eventually |
| 12 months+ | The #720 device-staleness clock does its normal work: a device silent past `POLLIS_DS_WATERMARK_STALE_MONTHS` stops pinning envelope retention. This is **designed behaviour**, not a continuity failure — but with nobody operating the service it is the one dated rule that changes user-visible outcomes on its own | Yes, for a device offline that long |

The uncomfortable middle of that table is the point: **the system is at its most misleading between day
two and month two**, when everything a user touches still works while every guarantee that depends on
someone publishing has quietly stopped.

## 4. The failure mode specific to Pollis: a stalled log is indistinguishable from a withheld one

This is the part that is not generic continuity planning, and it is why this section is longer than the
rest.

**The ambiguity.** The three transparency trees are republished by a daily cron
(`transparency-publish.yml`). A Signed Tree Head is `{tree_size, root_hash, timestamp, signature}`, and
the publisher **reuses the frozen timestamp from the published head for a given tree size unless the
tree has grown** — deliberately, so a head for a size is byte-stable forever and cannot look like
equivocation. The consequence is that from outside, three situations produce an identical picture:

1. the log is healthy and nothing has happened to publish;
2. the publisher has been broken or abandoned for months;
3. someone is deliberately withholding publication of entries that exist.

An external monitor watching only `verify.pollis.com` **cannot tell these apart, ever.** No amount of
polling the served files resolves it, because the served files are supposed to be identical in all three
cases.

**Detection.** What separates them is the publisher's own run history, which is public API on a public
repository and needs no credential from us:

```
gh run list --workflow=transparency-publish.yml --limit 5 \
  --json conclusion,createdAt,url
```

**Nothing consumes that automatically today.** The check is a manual command, and the gap is known
rather than hidden: an abandoned publisher and a healthy idle one serve byte-identical files, so the
only way to tell them apart is the run history above, and no alarm currently reads it. Publishing the
publisher's own liveness — separately from the head it serves, because a monitor that goes quiet
cannot be the thing that reports it went quiet — belongs with `status.pollis.com` (the `archon` repo),
which already serves service status and uptime; it is tracked there rather than rebuilt here.

**Escalation.**

1. **Owner, publisher red or absent > 48h:** re-dispatch `transparency-publish.yml`. If it fails on the
   signing key, follow `docs/signing-key-compromise-runbook.md`; if it fails on database access, that is
   a DS-side incident. Either way **say so publicly**: an unpublished log is a correctness problem, not a
   cosmetic one, and must not be closed as "the site was up the whole time".
2. **Owner, cannot be fixed within 7 days:** acknowledge it on `status.pollis.com`. An acknowledged
   stall is a completely different object from a silent one, and this is the cheapest thing that turns
   one into the other.
3. **A third party, at any time:** the run history above is readable by anyone, and is the record of
   what was published and when — including any gap.

**What an external monitor should conclude.** Stated as a rule, because getting it wrong in either
direction is harmful:

- A stalled publisher is **evidence of nothing about honesty**. Do not report "Pollis is withholding log
  entries"; report "the log is not currently being published", which is the only supported claim.
- Equally, do not report a served-but-stale head as healthy. An unchanged head with a dead publisher is
  *also* exactly what a coerced or compromised operator would produce, and the observer has no way to
  exclude it.
- The check that actually resolves it is not availability monitoring at all: **keep the heads you were
  served, and compare them later and with other people.** Two parties holding signed heads that
  disagree have permanent, undeniable proof; one party watching an endpoint has nothing. That is what
  `pollis-verify` is released for.
- **Nobody is doing that for you today.** The whitepaper says it plainly — Pollis runs the only log and
  the only first-party auditor, and no third party is contractually watching. Gossip/witness monitoring
  is designed and unbuilt (**#763**, `deferred`/`backlog`). Until it exists, "no alarm" means "nobody
  looked", and this document will not imply otherwise.

## 5. Handover — what a successor needs, and the one thing that does not hand over

In order, assuming they have somehow obtained the accounts (**→ #723** — nothing here compels that):

1. **GitHub org + repo.** Everything else is described in it; this document is useless without it.
2. **Doppler**, then the provider accounts behind each secret, since a Doppler value is only a copy.
3. **Cloudflare**, which is DNS, the website, the DS and the CDN in one account.
4. **Turso**, both databases.
5. **The two signing keys**, last and most carefully: `TAURI_SIGNING_PRIVATE_KEY` and
   `STH_SIGNING_KEY`. Custody, rotation and the runbook: `docs/sth-signing-key-custody.md`,
   `docs/signing-key-compromise-runbook.md`.

**The structural problem, which a successor must be told rather than discover.**
`docs/sth-signing-key-custody.md` §3 records it: the same key signs the *binaries* tree, and the pinned
log key is a `const` in the client. So re-pinning a new log key requires shipping a client update — and
the mechanism a user has for verifying a client update is the binaries tree, signed by the key being
retired. **Recovering from a compromised or lost log key currently requires trusting the thing the
compromise undermines.** The fix is the anchor split, decided and tracked in **#754**, and until it lands
nothing may describe a log-key compromise as cleanly recoverable — including this document.

## 6. What is deferred to #723, explicitly

Not written here, because writing it without a legal entity would be writing fiction:

- **Ownership and succession.** Who owns the accounts and keys, who inherits them, and by what
  instrument.
- **Escrow or a dead-man's switch.** Both need a counterparty who is contractually obliged to act. A
  technical dead-man's switch that publishes a key on a timer is *worse* than none: it is an unattended
  path to the signing key in §2, and it would be the first thing to attack.
- **Any commitment about legal process** — a warrant canary or a transparency report. Those are
  statements by an entity about process served on that entity.
- **A privacy policy or terms of service.** The factual half is already written from the code
  (`docs/metadata-retention-policy.md`, published at `/retention`); what is missing is the counterparty
  they bind.

## 7. Standing commitments this document makes

Each is either already enforced by CI or is a one-line ask. Nothing aspirational.

- [x] **This register is part of the repository**, so a new credential added without a row here is
      visible in review as an asymmetry between `.github/workflows/` and §2.
- [ ] **A stalled transparency publisher raises an alarm.** Today §4's check is a command someone has
      to run. This is the cheapest unbuilt item here and the one this document most depends on:
      an abandoned log and an idle one are byte-identical from outside.
- [ ] **A written incident and postmortem record**, in which an unpublished or unverifiable log is a
      correctness incident rather than a cosmetic one. Belongs with `status.pollis.com` (`archon`),
      alongside the service status it already publishes.
- [ ] **A second holder for the two pinned signing keys.** The single highest-value continuity action
      available, and it needs a person to hold them (**→ #723**).
- [ ] **The anchor split (#754)**, without which a log-key loss is not cleanly recoverable (§5).
- [ ] **Independent witness/gossip monitoring (#763)**, without which "no alarm" means "nobody looked".

## Related

`docs/deployments.md` (what ships where, and the post-merge checklist) ·
`docs/sth-signing-key-custody.md` (§3 is the one to read) ·
`docs/signing-key-compromise-runbook.md` ·
`docs/security-whitepaper.md` (honest limits of the log) ·
`status.pollis.com` (service status; built from the `archon` repo) ·
#723 (legal entity) · #754 (anchor split) · #763 (witness monitoring)
