# Design: STH Signing-Key Custody and Rotation

**Status:** every §9 decision is now made. §5 (the key set + overlap window) is **BUILT** (#740).
§4 custody is **DECIDED — Option A, the CI secret** (2026-08-03), knowingly against §4's own
recommendation. §6 (the anchor split) is **DECIDED — adopt** (2026-08-04), implementation tracked in
**#754**; that choice follows directly from the custody one. Two items remain open but need no
decision: the overlap-window length (unset until a rotation needs it) and whether gossip/witness
monitoring (§8) gets its own ticket. See §9 for the ledger. Resolves the design half of #700; part
of #662. The runbook in §7 was executed by #732/#740.
**Scope:** the key that signs Signed Tree Heads for the three transparency trees. Not MLS keys, not
account identity keys, not release code-signing certificates.

---

## 1. Where we are today, precisely

| Fact | Evidence |
|---|---|
| One ML-DSA-44 signing key signs **all three trees** — commit log, account keys, released binaries — separated only by a domain-context string | `verifiable-log/src/sth.rs` |
| The private key is a **32-byte seed** (an ML-DSA private key *is* its seed, so the custody format did not change across the Ed25519 → ML-DSA-44 migration in #668) | `verifiable-log-builder/src/keys.rs` |
| It lives as the GitHub Actions secret **`STH_SIGNING_KEY`** (backed up in Doppler `pollis/prd`), injected as `VLOG_SIGNING_KEY` into the builder | `.github/workflows/transparency-publish.yml` |
| It is consumed by exactly one workflow, on a daily schedule and on manual dispatch | `transparency-publish.yml` |
| The builder **refuses to sign** if no key is present rather than inventing an ephemeral one | `keys.rs::load_signing_key` |
| Clients pin a **set**: `PINNED_LOG_PUBLIC_KEYS: &[PinnedKey]`, each with a `key_id` and an optional `not_after` (§5, #732) | `pollis-core/src/commands/transparency.rs` |
| The set currently holds **one key with no expiry** — the material minted in #732 | same |
| The served trust anchor `/v1/public_key.json` carries a **key list**, with the single `public_key` field retained for older verifiers | `verifiable-log-serve/src/bundle.rs` |
| A rotation path, key set, overlap window and runbook now exist; custody (§4) is decided (Option A, the CI secret) and the anchor split (§6) is decided (adopt — #754) | §5, §7, §9 |

An absent or wholly expired pin set is safe by construction — `check_pin_at` resolves it to
*Unavailable*, never *Ok* and never *Alarm*, so a missing pin can only withhold trust. The set is
populated as of #732, so the verification badge lights up with the release that carries it.

## 2. What an attacker gets from the key

A signing-key holder can mint a **valid-looking STH over a tree they control**. Because a single key
covers all three trees, one leak is three compromises:

- **Commit log** — present a forked or truncated MLS commit history to a targeted client, hiding the
  commit that added an attacker's device to a group. This is the attack the log exists to prevent.
- **Account keys** — present a forked account-key directory, hiding a key substitution. This is the
  attack that turns into a full impersonation.
- **Binaries** — present a signed record for a binary we never built.

What the key does **not** get them: message plaintext, MLS group secrets, or the ability to make a
*client* accept an inconsistent view without also controlling the served artifacts. Forgery requires
both the key and the ability to serve the forged bundle to the target.

**A leak is silent.** Nothing in the system detects a second, well-formed, correctly-signed tree today.
That is a gap independent of custody, and §6 addresses it.

## 3. The circular dependency (the finding that shapes this design)

The same key signs the **binaries** tree. That tree is what a user consults to verify that the Pollis
binary they are running is the one we built.

So consider recovery from a compromise. The pin lives in the client binary
(`PINNED_LOG_PUBLIC_KEY` is a `const`), so re-pinning a new key **requires shipping a client update**.
But the mechanism that lets a user verify a client update is the binaries tree — signed by the key we
are trying to retire. An attacker holding the leaked key can sign a binaries-tree view that vouches for
their build of the "fix".

**Recovering from a compromise currently requires trusting the thing the compromise undermines.** No
amount of KMS hardening fixes this; it is a structural property of using one key for both roles.

This is the strongest argument for §6, and it should have been decided before, not after, #699 minted
the v2 key — the key hierarchy is baked in the moment we publish and pin. It was not: #732 minted and
pinned first. §6 is now decided (adopt, 2026-08-04, tracked in **#754**), which costs a second
rotation rather than riding along with the first.

**Until #754 lands, this gap is open**, and anything describing our recovery posture should say so
rather than imply a compromise is cleanly recoverable.

## 4. Custody options

The decision is where the private key materialises when an STH is signed.

### Option A — CI secret (today)

The raw seed is decrypted into a GitHub-hosted runner's environment on every publish.

- **For:** zero work; already running.
- **Against:** the raw key exists in plaintext in a third-party runner's memory daily. Anyone who can
  push a workflow change, compromise an Action we depend on, or read GitHub's secret store gets it. The
  blast radius is §2, and there is no rotation path if it happens.
- **Verdict:** acceptable only while the pin is `None` and nothing trusts the key. Not acceptable once
  clients pin it.

### Option B — Cloud KMS (non-exportable key, sign-only API)

The key is generated inside a KMS and never leaves; CI calls a sign API.

- **For:** raw key never in CI; per-call audit log; IAM-scoped; rotation is a first-class operation.
- **Against — and this is load-bearing:** **ML-DSA-44 support in mainstream KMS offerings must be
  verified before committing to this option.** PQ signature support has been arriving unevenly and often
  in preview, and we cannot design around an assumption here. If ML-DSA-44 is unavailable, Option B is
  not available to us without either reverting the #668 PQ migration (no) or accepting a hybrid
  classical+PQ arrangement (complexity, and it weakens the PQ claim to whichever half the KMS holds).
- **Action before choosing:** confirm ML-DSA-44 (FIPS 204) sign support, key import vs. generate, and
  the API surface, for whichever provider we would use. Write down the answer with a date.

### Option C — Offline signing ceremony

The builder produces the *tree* in CI, emits the unsigned STH pre-image
(`context || tree_size || root_hash || timestamp`), and an air-gapped machine holding the seed signs it.
The signature is pasted back and published.

- **For:** the raw key never touches a networked machine, let alone a third-party runner. Independent of
  any vendor's PQ roadmap. The pre-image is small and human-inspectable — the operator can see the tree
  size and root they are signing.
- **Against:** publishing stops being fully automatic. The daily cadence becomes "as often as the
  operator runs a ceremony", and a missed ceremony means a stale STH.
- **Mitigation that makes this practical:** decouple *appending* from *signing*. The tree is built and
  published continuously; the signed head lags. Clients already tolerate a lagging STH — freshness is a
  monitoring concern, not a correctness one, provided consistency proofs still chain.

### Recommendation

**Option C for the anchor key, and revisit Option B for the online key once ML-DSA-44 KMS support is
confirmed.** This falls out of §6: once the roles are split, the offline root signs rarely (a key-set
statement), which is exactly the workload a ceremony suits, and the online key signs daily, which is
exactly the workload a KMS suits. Choosing the split first makes the custody question much easier.

## 5. Rotation requires a key *set* — BUILT (#732)

Originally written as required future work. It shipped with #732; this section now
describes what exists.

Every piece of the original design assumed exactly one valid key at a time, which made
rotation a **flag day**: every client had to update at the instant the served key changed,
or its audit flipped from *Ok* to *Alarm* — a false alarm that trains users to ignore the
real one.

**What is now in place:**

| Piece | Where | Shape |
|---|---|---|
| Pinned key set | `pollis-core/src/commands/transparency.rs` | `PINNED_LOG_PUBLIC_KEYS: &[PinnedKey]`, each with `key_id`, `public_key`, `not_after` |
| Served key list | `verifiable-log-serve/src/bundle.rs` | `PublicKeyDoc.keys: Vec<PublicKeyEntry>`; the single `public_key` field is still populated for verifiers that predate the list |
| STH key selection | `verifiable-log/src/sth.rs` | `Sth.key_id: Option<String>` plus `verify_any[_with_context]` |
| Overlap publishing | `builder --retired-key <hex>:<not_after_ms>` | repeatable; emits `retired_keys` into the bundle |
| Rebuilder | `.github/workflows/rebuild-verify.yml` | `PINNED_LOG_KEY` accepts a comma/space-separated list |
| Website | `website/artifacts.js` | `PINNED_KEYS` + `livePinnedKeys()` |

**`key_id` is a hint, not a trust input.** It is derived (`key_id_for` = first 8 bytes of
`SHA-256(key)`), so anyone holding the key can recompute it and no registry can drift. It
is deliberately **outside the signed preimage**, which means the frozen STH signing message
did not change and every previously published signature stays valid. A wrong or hostile
`key_id` costs a few extra verification attempts and can never make a bad head verify or a
good one fail — `a_lying_key_id_changes_nothing` encodes that.

**The ordering that makes a rotation invisible.** The key set only helps if the pins move
*before* the log does:

1. Ship a release whose `PINNED_LOG_PUBLIC_KEYS` contains **both** the current key and its
   successor (successor `not_after: None`, incumbent given the window end).
2. Wait out the overlap window so the fleet updates. **Minimum 90 days** — one full release
   cycle plus the tail of users who update late.
3. Only then move the log onto the successor. Every updated client already accepts it, so
   nothing flips to *Alarm*.
4. A later release drops the retired entry.

Doing it the other way round — rotate first, pin second — is exactly the flag day this
exists to prevent. #732 was the last rotation for which that was safe, because no shipped
build pinned anything at the time.

**Expiry is enforced client-side**, in `check_pin_at`, not trusted from the server: a
retired key stops being accepted on schedule even on a client that never updates again. A
wholly expired set withholds trust (*Unavailable*) rather than alarming — "my pins aged
out" is not evidence of a hostile host.

## 6. Split the anchor from the signer

This is the structural recommendation, and it dissolves both §3 and half of §4.

Introduce **two roles**:

- **Root / anchor key** — offline, in a ceremony (§4 Option C). It signs *nothing but key-set
  statements*: "these key ids are valid signers for these trees, until this date." It signs rarely
  (a rotation, an emergency revocation). **This is what clients pin.**
- **Online signing key(s)** — one per tree, ideally. They sign STHs on the daily cadence. They live
  wherever custody is most operationally convenient (CI today, KMS later) because compromising one is
  now *recoverable*: the root signs a new key-set statement that retires the compromised key id, and
  **clients that already pinned the root accept the retirement without a client update.**

That last clause is the whole point. It breaks the circular dependency in §3: recovery no longer
requires shipping a binary whose provenance depends on the compromised key.

**Per-tree keys, additionally.** Even with the split, give the binaries tree its own online key. A
compromise of the commit-log signer should not imply a compromise of the mechanism used to verify the
fix.

Cost: one more document type, one more verification step, and a ceremony we would want for the root
anyway. Compared to "a leak is unrecoverable", this is cheap.

### DECIDED 2026-08-04: adopt the split. Tracked in #754.

**The custody decision made this one, and not in the direction that lets us skip it.** §4 was settled
as Option A — the raw seed stays a CI secret. That is the *highest*-likelihood custody option on
offer: anyone who can push a workflow change, compromise a dependency Action, or read GitHub's secret
store gets the key. Choosing it is defensible on its own, but only if a compromise is *recoverable*.
Without the split it is not (§3). Accepting "CI secret" **and** "no anchor split" together is the one
combination that has no answer to a leak — a single exposed GitHub secret would be an unrecoverable
compromise of the whole transparency system, because the fix must be shipped as a binary whose
provenance is vouched for by the very tree the leaked key signs.

So the two open decisions were never independent. Having taken the cheap custody option, the split is
what pays for it.

**What is already built.** #740 shipped most of the client half: `PINNED_LOG_PUBLIC_KEYS` is a *set*
with per-key `key_id` and `not_after`, `Sth` carries a `key_id` selector, and the served
`public_key.json` is a key list. What is missing is the part that makes the set *dynamic*: today the
set is a compiled-in `const`, so retiring a key still needs a client release. The split adds a
root-signed key-set statement the client fetches and verifies against a pinned root, so a retirement
propagates without shipping a binary.

**What is not decided here**, deliberately: the root's own custody mechanics and the ceremony
schedule. Those belong with the implementation, where they can be specified against real code rather
than in the abstract.

**Sequencing note, stated honestly.** §6 said to decide this *before* minting, and #732 minted and
pinned first — so adopting it now costs a second rotation rather than riding along with the first.
That cost is real and was avoidable. It is materially smaller than it would have been before #740,
because the overlap window means the next rotation is no longer a flag day: ship a release pinning
both the current key and the new root, wait out the window, then cut over. The work is tracked in
**#754** with no deadline attached; until it lands, `docs/backend-core-invariants.md` and the honest-
limits section of the whitepaper should keep saying that a signing-key compromise is not cleanly
recoverable.

## 7. Rotation runbook (what #699 executes)

Written to be followed literally. Every step is checkable; none of it is "and then verify it works".

**Pre-flight**
1. Decide and record the custody option (§4) and whether the anchor split (§6) lands with this rotation.
   Write the decision and its date into this document before proceeding.
2. Confirm `PINNED_LOG_PUBLIC_KEY` is still `None` in the shipped client. If it is not, this is no longer
   a free rotation and the key-set shape (§5) is mandatory before continuing.

**Mint**
3. Generate the ML-DSA-44 seed on the machine that will hold it, using the builder's own `generate`
   (`keys.rs::generate`) so the format is exactly what `load_signing_key` parses. Record the 1312-byte
   public key hex.
4. Back up the seed to durable offline storage **before** it signs anything. A signing key with no backup
   is a self-inflicted outage waiting for a disk failure.

**Republish**
5. Republish **all three trees from source data** under the v2 contexts. This is a rebuild, not a
   re-signature: the v1 (Ed25519-era) roots are retired and do not chain to the v2 roots.
   Dispatch `transparency-publish.yml` with **`full_resync: true`**. This is not optional, and it is
   the step that makes the rotation actually take. Pass 1 normally syncs `--size-only`, and every
   re-signed artifact is byte-length-*identical* to the one it replaces — ML-DSA-44 keys (1312 B) and
   signatures (2420 B) are fixed-length, roots are SHA-256, and an unchanged tree deliberately reuses
   the timestamp frozen in its published head. Measured on the #732 rotation: `public_key.json` 2646 B
   and `v1/sth/1.json` 4992 B, before and after. Without the flag the sync skips every re-signed head
   while pass 2 (which has no `--size-only`) overwrites `latest.json` with a signature the still-served
   old key cannot verify — a live, self-inconsistent log, strictly worse than not rotating.
6. `full_resync` also suppresses the cross-run equivocation tripwire for that one run and re-seeds it.
   A rotation changes every signature and so is byte-indistinguishable from equivocation; the tripwire
   is right to fire, and the ceremony silences it once, deliberately, rather than the tripwire being
   made lenient. Note the tripwire cache is an `actions/cache` entry and ages out after 7 days of
   non-use, which silently disables equivocation detection — check it is seeded after any rotation.
7. Verify each served bundle end to end with the public verifier — `public_key.json`, `sth/latest.json`,
   `entries.json`, and the inclusion/consistency proofs — for **all three** trees, not just the commit
   log. Run it from a machine that did **not** perform the rotation.

**Pin**
7. Set the pin(s) in `pollis-core/src/commands/transparency.rs` in the same change that ships the
   republished log, per the existing comment. Update `docs/transparency.md` with the new key hex so the
   auditor release notes and the constant agree.
8. Ship a client release. Confirm the in-app check reports **verified** on all three trees
   (`self_audit_account_key`, `audit_peer_account_key`, `verify_own_build`) — this is the launch-gate
   criterion in #687, and it is not met until all three are green.

**Post**
9. Destroy or archive the retired key material per the custody decision.
10. Schedule the next rotation. A rotation path that is never exercised is a rotation path that does not
    work.

## 8. Compromise recovery

Until §6 lands, be honest about this: **there is no clean recovery.** The steps are:
revoke by shipping a client update that re-pins → but the update's provenance rests on the binaries tree
signed by the compromised key → so recovery depends on out-of-band trust (the website, the repository,
whatever channel users already trust) and a plainly-worded public statement.

Once §6 lands, recovery is: the offline root signs a key-set statement retiring the compromised key id;
clients pick it up on their next fetch; no client update is required.

**Detection is the unsolved half either way.** Nothing today notices a second well-formed signed tree.
The standard answer is gossip: independent monitors fetch STHs and compare, and clients cross-check the
STH they saw against what a monitor saw. That is a separate piece of work and should be its own ticket
rather than being smuggled into this one.

## 9. Decisions — what is settled and what is not

Two of these were decided by the owner on **2026-08-03** during the #732 rotation. Recording
them here because they were made against a differently-labelled set of options and would
otherwise read as un-answered. **Careful with the letters:** this section's A/B/C are *custody*
options (§4). The rotation ticket separately offered an unrelated "A or B" — bake one key in now
vs. ship the key set with an overlap window first — and that one is the last checkbox below.
The two are different axes that happened to share letters.

- [x] **Custody (§4): Option A — the raw seed stays a CI secret.** Decided 2026-08-03.
      Diverges from §4's recommendation (C for the anchor key, revisit B once ML-DSA-44 KMS
      support is confirmed) and from §4's own caveat that A is *"acceptable only while the pin is
      `None` and nothing trusts the key"* — the pin is now set (#740), so this is a knowingly
      accepted risk, not an oversight. Current state: `transparency-publish.yml` reads
      `VLOG_SIGNING_KEY` from `secrets.STH_SIGNING_KEY`; no KMS key exists in the account.
      A backup of the seed lives in Doppler `pollis/prd`. **Revisiting this is real follow-up
      work, not a formality** — see the unresolved item directly below, which it interacts with.

- [x] **Key-set shape lands with the rotation — yes.** Shipped in #740: `PinnedKey`
      (`key_id` + `not_after`), `PINNED_LOG_PUBLIC_KEYS`, `Sth::verify_any_with_context`,
      `PublicKeyEntry`/`retired_keys` in the served `public_key.json`, and `builder --retired-key`.
      §5 documents the mechanism.

- [x] **Anchor split (§6): ADOPT. Decided 2026-08-04, tracked in #754.** Not because the gap is
      urgent — it is a recovery path, not a live hole — but because the §4 custody choice made it
      mandatory: "CI secret" plus "no split" is the one combination with no answer to a leak. §3's
      circular dependency is the mechanism: one key signs all three trees including `binaries`, the
      pin is a `const` in the client, so recovering from a compromise means shipping a client update
      that users verify *through the tree the compromised key signs*. Having taken the cheap custody
      option, the split is what pays for it. #740 already shipped the client-side key *set*; #754
      adds the root-signed key-set statement so retiring a key no longer needs a client release.
      Costs a second rotation because #732 minted first — real, avoidable, and much cheaper
      post-#740, since the overlap window means it is no longer a flag day.

- [ ] **Overlap window (§5) length — UNRESOLVED.** The machinery ships and enforces `not_after`
      client-side, but only one key is pinned today, so no window is in force. §5 recommends
      ≥ 90 days; the number is unset until the next rotation needs it.

- [ ] **Gossip / witness monitoring (§8) — no ticket filed.**

Related: #662 (parent), #672 (rotation runbook origin), #699/#732 (executed §7), #668 (the
ML-DSA-44 migration), #740 (built §5, set the pin), #687 (launch gate 4 depends on this).
