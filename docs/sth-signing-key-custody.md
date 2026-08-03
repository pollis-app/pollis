# Design: STH Signing-Key Custody and Rotation

**Status:** Design draft — **owner decision required on §4 (custody) and §6 (the anchor split).**
Resolves the design half of #700; part of #662. The runbook in §7 is what #699 executes.
**Scope:** the key that signs Signed Tree Heads for the three transparency trees. Not MLS keys, not
account identity keys, not release code-signing certificates.

---

## 1. Where we are today, precisely

| Fact | Evidence |
|---|---|
| One ML-DSA-44 signing key signs **all three trees** — commit log, account keys, released binaries — separated only by a domain-context string | `verifiable-log/src/sth.rs` |
| The private key is a **32-byte seed** (an ML-DSA private key *is* its seed, so the custody format did not change across the Ed25519 → ML-DSA-44 migration in #668) | `verifiable-log-builder/src/keys.rs` |
| It lives as the GitHub Actions secret **`STH_SIGNING_KEY`**, injected as `VLOG_SIGNING_KEY` into the builder | `.github/workflows/transparency-publish.yml:180` |
| It is consumed by exactly one workflow, on a daily schedule and on manual dispatch | `transparency-publish.yml` |
| The builder **refuses to sign** if no key is present rather than inventing an ephemeral one | `keys.rs::load_signing_key` |
| Clients pin **exactly one** key: `PINNED_LOG_PUBLIC_KEY: Option<&str>` | `pollis-core/src/commands/transparency.rs:56` |
| That pin is **`None` today**, so every in-app transparency audit resolves to *Unavailable* | same |
| The served trust anchor `/v1/public_key.json` is a **one-field document** holding a single key | `verifiable-log-serve/src/bundle.rs:57` |
| There is **no rotation path, no key set, no overlap window, and no runbook** | absence across `docs/`, `.github/workflows/` |

An absent pin is safe by construction — `check_pin` resolves `None` to *Unavailable*, never *Ok* and
never *Alarm*, so a missing pin can only withhold trust. But the verification badge stays dark for every
user until #699 lands, which is why this design is on the launch path rather than after it.

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

This is the strongest argument for §6, and it should be decided before, not after, #699 mints the v2 key
— because the key hierarchy is baked in the moment we publish and pin.

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

## 5. Rotation requires a key *set*, which we do not have

Every piece of the current design assumes exactly one valid key at a time:

- `PINNED_LOG_PUBLIC_KEY` is `Option<&str>` — one key or none.
- `PublicKeyDoc` is a one-field object — one key.
- `Sth::create` takes a single `&SigningKey`.

With a single pinned key, rotation is a **flag day**: every client must update at the same instant the
served key changes, or its audit turns from *Ok* to *Alarm* — a false alarm that trains users to ignore
the real one. That is unacceptable, and it is why rotation must be designed before it is needed.

**Required change — a pinned key set with an overlap window:**

1. `PINNED_LOG_PUBLIC_KEYS: &[PinnedKey]` where `PinnedKey` carries the key hex, a key id, and a
   `not_after` for a key being retired. Verification succeeds if the STH verifies under **any**
   non-expired pinned key; *Alarm* only if it verifies under none.
2. `PublicKeyDoc` grows to a key **list**, each entry carrying `key_id` and `algorithm` — the doc comment
   already anticipates this ("can grow metadata (key id, algorithm) without breaking the URL"). Keep the
   existing single-key field populated for one release for compatibility, then drop it.
3. The STH gains a `key_id` so a verifier selects rather than trial-verifies. This is a **wire-format
   change** and must be additive and versioned, like every other frozen contract in this system.
4. The overlap window is **at least one full client-release cycle plus the tail of users who update
   late** — in practice a minimum of 90 days. Sizing it is an owner decision; sizing it *after* a
   compromise is not an option.

**Do not skip step 1 for #699.** #699 is currently framed as "mint the v2 key and set the pin" — a flag
day that happens to be safe only because the pin is `None` today, so nothing breaks. That makes it the
*last* free rotation we get. Landing the key-set shape as part of #699 costs little now and is the
difference between a routine rotation and an incident later.

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
6. Verify each served bundle end to end with the public verifier — `public_key.json`, `sth/latest.json`,
   `entries.json`, and the inclusion/consistency proofs — for **all three** trees, not just the commit
   log.

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

## 9. Decisions required from the owner

- [ ] **Custody (§4):** Option A (status quo — only viable while the pin is `None`), B (KMS, *pending
      verified ML-DSA-44 support*), or C (offline ceremony).
- [ ] **Anchor split (§6):** adopt the root/online-key split, or accept that a key compromise is
      unrecoverable without out-of-band trust. **Decide before #699 mints the key** — the hierarchy is
      fixed the moment we publish and pin.
- [ ] **Overlap window (§5):** the minimum period two pinned keys are simultaneously valid.
      Recommendation: ≥ 90 days.
- [ ] **Whether the key-set shape lands with #699** or is deferred. Recommendation: lands with it.
- [ ] Whether gossip/witness monitoring (§8) gets its own ticket now.

Related: #662 (parent), #672 (rotation runbook origin), #699 (executes §7), #668 (the ML-DSA-44
migration), #687 (launch gate 4 depends on this).
