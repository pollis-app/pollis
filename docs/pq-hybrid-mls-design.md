# Post-Quantum Hybrid MLS — Design

**Status: COMPLETE — P0 through P5 all shipped (§8).** The hybrid suite is reachable
through a second, hybrid-only provider that classic traffic is *routed away from* for the
security reasons in §7.3 (`pollis-core/src/commands/mls/provider.rs`); the ciphersuite is
an explicit argument to the two functions that mint suite-bound material; every device
publishes both key-package pools and the DS derives `user_device.pq_capable` from what
lands; new groups are born on `CS_HYBRID` and existing ones migrate to it by successor
generation — both behind the same two gates (full roster PQ-capable **and** the whole live
fleet PQ-capable). What remains outside this design's scope, deliberately: post-quantum
*signatures* (§2.4) and retiring classic key-package publication, which cannot begin until
an app that no longer needs classic has full uptake.
**Scope:** migrate Pollis's MLS key exchange from classical X25519 to a hybrid
X25519 + ML-KEM-768 construction, transparently, without breaking existing
groups, existing devices, or the "messages must work" doctrine.
**Author lens:** three constraints govern every choice below —
**security** (defend against harvest-now-decrypt-later),
**performance** (Pollis's stated goal is to be the *fastest secure messenger*), and
**zero user burden** (the migration must be invisible; no user takes any action, and
no in-flight group or device is stranded).

Where this document and `docs/security-whitepaper.md` disagree on *current* state,
the code wins and this document cites `file:line`. The whitepaper still says
`libsql 0.6` / describes a direct-to-Turso write path; the tree has since moved to
`libsql 0.9` (`pollis-core/Cargo.toml:65`) and a signed **Delivery Service (DS)**
write seam (`pollis-core/src/commands/mls/ds_client.rs:1-23`). This design targets
the code as it is today.

---

## 0. TL;DR / recommendation

- **The exposed asset is the MLS key exchange (HPKE/DHKEM), not the signatures.**
  Harvest-now-decrypt-later (HNDL) breaks *confidentiality* by recording ciphertext
  and the KEM material that seals it, then recovering the shared secret once a
  cryptographically-relevant quantum computer (CRQC) exists. Signatures do **not**
  need PQ for HNDL — a forged signature in 2035 cannot retroactively decrypt a 2026
  message. So we go hybrid **only on the KEM**, and keep Ed25519 signatures classical
  for now. (§1, §2.4.)

- **Go hybrid, not PQ-only:** X25519 **+** ML-KEM-768, combined so the session key
  is secure if *either* primitive holds. This is the IETF/NIST-endorsed posture and
  it protects us against both a future CRQC *and* a not-yet-discovered flaw in the
  young ML-KEM implementation. The concrete instantiation is **X-Wing**
  (`draft-connolly-cfrg-xwing-kem`) as the DHKEM, shipping today as the experimental
  OpenMLS code point `0x004D` — a point we pin deliberately (§7). (§2.)

- **Feasibility, honestly: the pinned stack can already do this — the P0 spike proved
  it end-to-end, headless, with stock cargo.** The hybrid suite
  `MLS_256_XWING_CHACHA20POLY1305_SHA256_Ed25519` (code point `0x004D`) already exists,
  un-feature-gated, in the pinned `openmls_traits 0.5.0` `Ciphersuite` enum (added
  upstream in openmls/openmls#1546, 2024-04). The only ever-missing piece was the
  **crypto provider**, never OpenMLS itself: `openmls_rust_crypto 0.5.1` `unimplemented!()`
  -panics on this suite (`src/provider.rs:61`), while `openmls_libcrux_crypto 0.3.1`
  implements it (`src/crypto.rs:60`, via `hpke_rs_crypto`'s `KemAlgorithm::XWingDraft06`).
  So the route is: keep `openmls = "0.8"` and take a direct dependency on
  `openmls_libcrux_crypto = "0.3"` — **no version bump, no fork, no `[patch.crates-io]`.**
  The catch: the only hybrid suite on offer is a ChaCha20-Poly1305 suite, so the AEAD
  changes *with* the KEM (AES-128-GCM → ChaCha20-Poly1305, nominal level 128 → 256);
  Ed25519 signatures are unchanged. (§7.)

- **Recommendation: do it, in phases, gated behind the box's headless MLS harness.
  All phases are now DONE (§8)** — new groups are born hybrid, existing groups migrate,
  and key packages publish in **both** suites so an old-app device is never un-addable.
  Never a flag day. Two things landed differently from the sketch here and are worth
  reading in §8: migration is *not* a ciphersuite-transition commit at an epoch boundary
  (RFC 9420 has no such commit) but a **successor group** under a generation counter; and
  the crypto-provider change is a **route**, not a swap, because putting classic traffic
  on libcrux would have been a security regression (§7.3).
  The live risk is no longer availability but **code-point churn** — `0x004D` is an
  experimental X-Wing draft-06 point that OpenMLS `main` has since moved behind a feature
  flag and the IETF has since dropped; pin both dependency versions and budget
  for a second suite migration when the IETF suites stabilise (§7). (§3, §8.)

---

## 1. The threat: harvest-now-decrypt-later

### 1.1 What HNDL is

An adversary who can *observe or store* Pollis network traffic today — the DS write
payloads, the Turso rows they land in, the Welcome/Commit blobs, the key packages —
records them now, at essentially zero marginal cost, and holds them. When a CRQC
becomes available, they run Shor's algorithm against the recorded elliptic-curve
key-agreement material and recover the shared secrets that sealed those messages.
Every message whose confidentiality rests on **X25519** is retroactively decryptable.

The defining property: **the attack is committed today and paid off later.** Unlike
an active attack, there is nothing to detect at capture time — it is a passive
recording. This is why "we'll upgrade when quantum computers arrive" is the wrong
posture: by then, today's traffic is already harvested. The only defence is to stop
emitting harvestable material *now*.

### 1.2 Which Pollis assets are at risk

Walking the key-material summary (whitepaper §12) and the actual MLS code, the assets
split cleanly:

| Asset | Primitive | HNDL-exposed? | Why |
|---|---|---|---|
| **MLS group key agreement (TreeKEM path secrets)** | HPKE **DHKEM(X25519)** | **YES — primary target** | The shared secret sealing every application message derives from X25519 ECDH in the tree. Recorded ciphertext + tree material ⇒ retroactive plaintext. |
| **KeyPackage init keys / leaf HPKE keys** | HPKE(X25519) | **YES** | A recorded KeyPackage (`mls_key_package.key_package`, `000000_baseline.sql:121-127`) is the entry point to seal a Welcome to a joining device; breaking it breaks that device's initial secrets. |
| **Welcome messages** | HPKE(X25519) | **YES** | `mls_welcome.welcome_data` is HPKE-sealed to the joiner's init key. Recorded now, opened later. |
| **Voice frame key** | AES-128-GCM, derived via `MlsGroup::export_secret` (whitepaper §10.2) | **YES, transitively** | The voice key is exported from the MLS epoch secret. If the epoch secret falls to a broken X25519 tree, so does the voice key. AES-128 itself is only *weakened* by Grover (≈2⁶⁴ quantum work), not broken. |
| **Ed25519 account identity + device signatures** | Ed25519 (`account_identity.rs`, `verify_device_cert` at `account_identity.rs:716`) | **NO (for HNDL)** | A signature authenticates; it does not seal. A future CRQC that forges Ed25519 enables *active* impersonation *going forward*, but cannot retroactively decrypt anything. See §2.4. |
| **PIN-wrapped local keys** | Argon2id + XChaCha20-Poly1305 (whitepaper §3) | **NO** | Symmetric; local; never on-wire. Not harvestable. |
| **SQLCipher DB, attachment convergent encryption** | AES-256-GCM (whitepaper §7, §9) | **NO (practically)** | Symmetric AES-256; Grover halves the security level to 128 bits, which is fine. Not asymmetric, not Shor-breakable. |
| **TURSO_TOKEN / R2 / LiveKit transport (TLS)** | TLS 1.3 | out of scope | Transport HNDL is the platform's problem, not the MLS protocol's. Noted, not addressed here. |

**Conclusion:** the crown jewel is the **HPKE/DHKEM key agreement inside MLS.** That —
and only that — is what this design makes hybrid. Everything symmetric is already at
or above the post-quantum-adequate 128-bit floor; everything signature-shaped is not
an HNDL asset.

### 1.3 Why act now (the timeline)

- **NIST finalised the PQ KEM standard (FIPS 203, ML-KEM) in August 2024.** ML-KEM is
  no longer a research artifact; it is the standard. The tooling to adopt it exists
  (it is *literally in our Cargo.lock* — `libcrux-ml-kem`, `Cargo.lock:4042`).
- **CRQC timeline estimates cluster in the 2030s**, with meaningful probability mass
  earlier. The relevant number for Pollis is not "when will a CRQC exist" but
  **"what is the confidentiality lifetime of a message we send today?"** A message
  about someone's health, legal exposure, or dissidence has a sensitivity horizon of
  a decade or more. If the horizon exceeds the CRQC arrival window, HNDL wins. For a
  *privacy-first* product this is the whole ballgame.
- **The migration itself is slow** (long-lived groups, staggered desktop-update
  uptake — CLAUDE.md's additive-migration rule exists precisely because old and new
  apps coexist "for days or weeks"). If it takes us a year to fully roll a hybrid
  suite across the fleet, the year we start matters. Starting now caps the harvest
  window; starting in 2030 does not.

Signal shipped PQXDH (hybrid X25519+ML-KEM) in **2023–2024**; iMessage shipped **PQ3**
(hybrid, with ongoing ratchet PQ re-keying) in **Feb 2024**. A privacy-positioned
messenger that is still 100% classical on its KEX in 2026 is behind the pack it
benchmarks itself against (whitepaper §14 explicitly benchmarks Signal, iMessage,
Wire, Element). This is both a real security win and a genuine flex — *if* we ship it
honestly (§6).

---

## 2. What goes hybrid — precisely

### 2.1 The primitive: X25519 + ML-KEM-768, combined

The MLS ciphersuite defines the HPKE KEM used for TreeKEM path secrets, KeyPackage
init keys, and Welcome sealing. Today Pollis uses **DHKEM(X25519, HKDF-SHA256)**
(RFC 9180), embedded in the suite constant at `provider.rs:57`. The hybrid target
replaces that KEM with a **combiner** that runs *both* X25519 and ML-KEM-768 and
mixes their outputs so the resulting shared secret is secure if **either** component
is secure:

```
ss = KDF( X25519_ss || MLKEM_ss || transcript_binding )
```

Two concrete instantiations exist, and they are not mutually exclusive:

1. **X-Wing** (`draft-connolly-cfrg-xwing-kem`) — a *general-purpose* hybrid KEM
   pairing X25519 + ML-KEM-768 with a fixed, security-proven combiner. This is what
   `hpke-rs-libcrux` / `libcrux-kem` already implement (the crates are in our lock,
   `Cargo.lock:4017-4029`). The MLS PQ ciphersuite drafts build their DHKEM on
   X-Wing-shaped constructions.
2. **The MLS-specific PQ ciphersuite drafts** (`draft-ietf-mls-pq-ciphersuites`) — these
   register new MLS `Ciphersuite` code points that carry a hybrid KEM. This is the
   *right* long-term target because it means an interoperable, standard code point
   rather than an experimental one. Note the drafts are still moving: the suite we ship
   today (`0x004D`, X-Wing) is a 2024 experimental point that draft-06 (2026-07) has
   since *dropped* in favour of new code points, so "standard" here is a moving target we
   track, not a settled destination (§7).

**Choice: ship the X-Wing hybrid code point the pinned stack already carries (`0x004D`,
§7), and track the standardising IETF suites for a later second migration.** ML-KEM-768
(NIST security category 3, ≈AES-192-equivalent classical, comfortably PQ-adequate) is the
sweet spot the drafts, Signal, and iMessage all landed on — not the smaller -512
(category 1) nor the larger -1024. Pairing it with X25519 keeps the classical floor at the
128-bit level the rest of Pollis's suite already sits at (whitepaper §6.1).

### 2.2 Why hybrid and not PQ-only

Two independent reasons, both of which matter for a security product:

1. **Defence in depth against a young primitive.** ML-KEM's implementations are new.
   `libcrux-ml-kem` is at version **0.0.8** (`Cargo.lock:4043`) — a pre-1.0,
   rapidly-moving crate. A hybrid construction means an implementation bug or a
   yet-undiscovered structural weakness in ML-KEM does **not** drop us below the
   classical X25519 security we have today. We can adopt an immature PQ primitive
   *without* regressing our current guarantees. This is exactly why Signal, Apple,
   and the IETF all chose hybrid over PQ-only.
2. **Zero downside for HNDL.** The whole point is future confidentiality; hybrid gives
   the PQ guarantee *and* keeps the classical one. The only cost is size/latency (§4),
   which we bound.

### 2.3 The symmetric primitives — the AEAD changes with the suite

AES-128-GCM (application messages in *classic* groups) and AES-256-GCM (SQLCipher,
attachments) are symmetric. Grover's algorithm gives at most a quadratic speedup, i.e.
AES-128 → 64-bit *quantum* work factor and AES-256 → 128-bit. AES-256 is unambiguously
PQ-safe. AES-128 inside the classic MLS suite is the MTI level and matches every other
128-bit primitive in that suite (whitepaper §6.1); it is *weakened* but not *broken* by
Grover, and the realistic cost of 2⁶⁴ *sequential-depth-bounded* quantum operations is far
beyond any near-term CRQC.

**One correction we make loudly, because it was wrong in an earlier draft: the hybrid
suite is not AEAD-neutral.** The only hybrid suite the pinned stack offers is
`MLS_256_XWING_CHACHA20POLY1305_SHA256_Ed25519` — there is no AES-GCM hybrid variant to
select — so moving a group to hybrid *bundles* the AEAD change AES-128-GCM →
ChaCha20-Poly1305 with the KEM change, and the suite's nominal security-level label moves
128 → 256. This is not a regression: ChaCha20-Poly1305 is a 256-bit-key AEAD, at least as
strong as AES-128-GCM against both classical and Grover attacks, and it is the AEAD Signal
and WireGuard already lean on. What we *cannot* claim is "AEAD unchanged" — classic groups
keep AES-128-GCM, hybrid groups run ChaCha20-Poly1305, and there is no knob to decouple
the AEAD from the KEM. (Signatures *do* stay Ed25519 — §2.4 — that part of the argument
holds.)

### 2.4 Signatures: keep them classical (for now) — argued

Ed25519 appears in three places: the MLS leaf signature (part of the ciphersuite,
`provider.rs:57`), the per-device MLS signing key (`user_device.mls_signature_pub`,
`000000_baseline.sql:156`), and the account-identity cross-signing cert
(`account_identity.rs:693-756`). The MLS-suite drafts pair the hybrid KEM with a
signature that may be classical **or** ML-DSA (Dilithium).

**Recommendation: keep signatures classical (Ed25519) in Phase 1–3. Treat ML-DSA as a
separate, later track.** Reasoning:

- **HNDL does not threaten signatures.** A signature proves authenticity *at
  verification time*. A CRQC that can forge Ed25519 in 2035 lets an attacker
  impersonate a device *in 2035* — an active attack we would detect and could respond
  to by rotating to PQ signatures *then*. It cannot retroactively decrypt a single
  2026 message. So the *urgency* that drives the KEM change simply is not present for
  signatures.
- **ML-DSA is expensive and everywhere.** ML-DSA-65 public keys are ≈1.9 KB and
  signatures ≈3.3 KB, versus 32 B / 64 B for Ed25519. Signatures sit on **every** leaf
  node, **every** KeyPackage, and **every** commit. Making signatures PQ would inflate
  KeyPackages and commits far more than the KEM change does, and it would touch the
  cross-signing cert format (`device_cert_signed_payload`, `account_identity.rs:651`),
  the account-key transparency log leaf shape (`account_key_log`,
  `000005_account_key_log.sql`), and the DS request-signing credential
  (`ds_client.rs` signs with `mls_signature_pub`). That is a much larger, higher-risk
  blast radius for a threat that is *not* HNDL.
- **The honest framing:** signatures are a **store-now-forge-later** concern, not a
  harvest-now-*decrypt*-later concern, and the response to the former can be reactive
  (rotate when a CRQC is imminent) rather than pre-emptive. We design the schema so
  ML-DSA *can* be added later (§3.4 makes the key-material columns
  algorithm-tagged), but we do not pay its cost now.

One caveat we state loudly (§6): keeping signatures classical means the migration does
**not** make Pollis "fully post-quantum." It makes the *confidentiality* post-quantum,
which is the part HNDL attacks. That distinction is the honest headline.

---

## 3. Migration strategy — the hard part

MLS groups in Pollis are long-lived (CLAUDE.md's acceptance test: "a member who joined
4 years and 300 commits ago"). A group's ciphersuite is fixed at creation
(`init_mls_group` builds `MlsGroupCreateConfig::builder().ciphersuite(CS)`,
`group_state.rs:506-509`). You cannot change the suite of a *running* group in place in
RFC 9420 — the suite is bound into the group context. And you cannot flag-day the fleet,
because desktop users update on their own schedule (CLAUDE.md: old + new apps hit prod
"for days or weeks"). So the transition must be **dual-suite** and **epoch-boundaried**,
and it must never drop a message for a current member ("messages must work").

### 3.1 The dual-suite model

Introduce a second ciphersuite constant alongside the existing one, rather than
replacing it:

```rust
// provider.rs
pub(crate) const CS_CLASSIC: Ciphersuite =
    Ciphersuite::MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519;      // today
pub(crate) const CS_HYBRID: Ciphersuite =
    Ciphersuite::MLS_256_XWING_CHACHA20POLY1305_SHA256_Ed25519;     // 0x004D, already in the pinned enum (§7)
```

Every call site that currently hard-codes `CS` (`key_packages.rs`, `device.rs`,
`group_state.rs`) becomes suite-aware. The device keeps a **stable signing key per
suite** — note `load_or_create_device_signer` keys the signer on
`CS.signature_algorithm()` (`device.rs:76-90`); since both suites keep Ed25519
signatures, the *same* signing key and the *same* `device_cert` cover both suites' leaves.
This is a happy consequence of the "signatures stay classical" decision: cross-signing,
the DS auth credential (whichever scheme is live when this ships — §3.5), and the
transparency log are all unchanged by this work.

### 3.2 Rollout order (three fronts, staggered)

**Front A — new groups on hybrid.** Once a hybrid-capable app version is deployed to
"enough" of the fleet (measured, not guessed — see acceptance criteria §8), new groups
are created on `CS_HYBRID`. A group's suite is discoverable by every member from the
GroupInfo / KeyPackage they receive, so a member never has to guess.

**Front B — key packages in both suites.** This is the linchpin of zero-drop
interop. Today each device publishes 5 KeyPackages of the single suite
(`ensure_mls_key_package`, TARGET=5, `key_packages.rs:94`). A hybrid-capable device
publishes **two pools**: 5 classic + 5 hybrid, tagged by suite. When any device
reconciles a group and needs to add a member (`ds_claim_key_package`,
`ds_client.rs:159`), it claims a KeyPackage **of that group's suite**. Result:

- An old-app member of a *classic* group can still be added (their classic KP exists).
- A hybrid group adding a member claims that member's *hybrid* KP — which every
  hybrid-capable device publishes.
- A hybrid group cannot add a member who has *only* a classic app (no hybrid KP). That
  is handled in §3.3.

**Front C — existing groups migrate at an epoch boundary.** An existing classic group
does not switch in place. When (a) every member device in the group is on a
hybrid-capable app *and* (b) the group is touched (a natural commit, or a scheduled
migration commit), the group performs a **suite transition**: the committer creates a
*new* hybrid group seeded from the current roster and issues Welcomes to every member's
*hybrid* KeyPackage, then the old classic group is retired for that conversation.
Mechanically this reuses the machinery already present — it is structurally the same as
`init_mls_group` + a full-roster reconcile, but into `CS_HYBRID` — so it inherits the
commit/Welcome ordering invariant (whitepaper §6.3: stage locally → write remote →
merge on success) that protects against split-brain.

Because MLS binds the suite into the group, "migration" is really *"stand up the hybrid
successor group and move everyone into it at a clean boundary,"* not an in-place
mutation. The conversation ID (the MLS group ID, `group_state.rs:497`) stays the same;
what changes is the suite of the group state stored under it.

One consequence must be engineered, not hand-waved: **a successor group restarts at
epoch 0, and the rest of the system enforces per-conversation epoch monotonicity.** The
DS sole-writer rule accepts a commit only at the current head — the conditional insert
`WHERE ?2 = (SELECT COALESCE(MAX(epoch), -1) + 1 …)` keyed on `conversation_id`
(`pollis-delivery/src/commit.rs:179-213`, in `submit_commit`) — so it would
flatly reject the successor's epoch-0 commit; the transparency log's commit-log
invariant ("within a conversation, `epoch` strictly increases in `seq` order",
`docs/transparency.md` §"The commit-log invariant") aborts the build on an epoch reset;
and `docs/mls-reconcile-hardening.md` explicitly killed "re-create the group at
epoch 0" as a DS-forbidden destructive pattern. So the successor is *not* an exception
to monotonicity — it is a **suite-generation lineage**: `mls_commit_log` gains an
additive `generation INTEGER NOT NULL DEFAULT 0` column (§3.4), and the DS
monotone-head key widens from `(conversation_id, epoch)` to
`(conversation_id, generation, epoch)`. The DS accepts epoch 0 **only** as the first
commit of generation N+1, and generation N+1 may only be opened by a migration commit
referencing the closed head of generation N — a generation can never be opened twice,
and within a generation the existing head+1 rule is unchanged (invalid states
unrepresentable: today's rule, one key wider). The transparency-log verifier and the
machine-checked M4 spec must be extended to the same keyed invariant before this front
ships (§8).

Old messages already
decrypted and persisted locally are unaffected (they live in the local plaintext
`message` table, whitepaper §7.1); only *future* messages ride the hybrid group.

### 3.3 The mixed-fleet interop rule (so nothing drops)

The governing doctrine (CLAUDE.md "Messages must work"): a current member must be able
to read every message sent while they were a member. The transition must not violate
this for a member whose app is old. The rule that guarantees it:

> **A group migrates to hybrid only once every current member device advertises a
> hybrid KeyPackage.** Until then, the group stays classic and every member — old app
> or new — keeps sending/receiving on the classic suite.

**As shipped, this rule was necessary but not sufficient, and a second gate was added.**
A roster is not a fixed set: it grows. A group that satisfied the rule above at migration
time is still one invite away from having to admit a classic-only device, and at that
point reconcile can only skip the device (locking it out of its own conversation) or the
group must be rebuilt classic (the downgrade the suite routing exists to prevent). Neither
is acceptable, and neither is knowable by looking at the group. So both birth (Front A) and
migration (Front C) additionally require **fleet completeness**: no `user_device` with
`revoked_at IS NULL` and `last_seen` within 90 days may still be classic-only
(`fleet_is_fully_pq_capable`). That also settles Front A's "deployed to *enough* of the
fleet" hedge below — the answer is not a percentage, it is *all of the live fleet*, with a
90-day dormancy window so a laptop in a drawer cannot hold the deployment hostage. Both
gates fail toward availability: a check that cannot be evaluated keeps the group classic.
See §8, Phase 5.

This makes the transition *safe by construction* rather than by discipline (the
CLAUDE.md "invalid states unrepresentable" principle):

- **Enforced at the lowest useful layer:** the reconcile/migration path checks the
  roster's advertised suites *before* attempting a hybrid transition, exactly where it
  already claims KeyPackages (`reconcile.rs` / `ds_claim_key_package`). If any member
  lacks a hybrid KP, the transition is a no-op and the group stays classic. No member
  is ever added to a hybrid group they cannot participate in.
- **A hybrid group only ever contains hybrid-capable members**, so there is no
  "old device stuck in a hybrid group it can't read" state — that state cannot be
  expressed.
- **An old app never sees a hybrid group.** New groups it is invited to are created
  classic *if it is a member*, because Front A only fires hybrid when the whole invited
  roster is hybrid-capable. An old app simply keeps living in the classic world until
  it updates.
- **When the last laggard updates**, the next sweep migrates the group — as shipped,
  migration is driven only by the cold-launch sweep (`migrate_to_hybrid_if_due`, bounded
  to `MAX_MIGRATIONS_PER_SWEEP = 2`), not by a natural commit, so an idle group heals the
  next time any member opens the app. The "no periodic self-update" gap in whitepaper §6.7
  was **not** closed as a side benefit of this; it was closed on its own terms by #666,
  which also turned out to be what keeps hybrid commit size logarithmic (§4.1).

The consequence for the "4-years-300-commits" acceptance test: a returning member
catches up their *classic* group state exactly as today; if the group has since
migrated to hybrid, they were by definition hybrid-capable at migration time (the rule
above) and received a hybrid Welcome, so they catch up the hybrid group. Either way,
every message sent while they were a member is reachable. No new drop class is
introduced.

### 3.4 Schema changes (additive only, per CLAUDE.md)

Every change is `ADD COLUMN` (nullable/defaulted), `CREATE TABLE`, or `CREATE INDEX` —
never a rename, drop, or tightened constraint — because old apps keep hitting prod
(CLAUDE.md migration rule). Concretely, in new numbered migrations taking whatever the
next free migration number is at implementation time (other in-flight programs also
add migrations, so the number is claimed when the work lands, not reserved here):

- **`mls_key_package` gets a suite tag.**
  `ALTER TABLE mls_key_package ADD COLUMN ciphersuite INTEGER;`
  A NULL suite means "classic" (the only suite old apps write), so existing rows are
  correct by default. The DS claim path (`ds_claim_key_package`) gains a suite
  parameter and claims a KP of the requested suite; an old app ignores the column and
  claims as it does today. Additive and backward-compatible.
- **`mls_commit_log` gets a suite-generation column** (the Front-C lineage mechanism,
  §3.2). `ALTER TABLE mls_commit_log ADD COLUMN generation INTEGER NOT NULL DEFAULT 0;`
  Every existing row is generation 0, correct by default. The DS head rule
  (`commit.rs:179-213`) widens from `(conversation_id, epoch)` to
  `(conversation_id, generation, epoch)`: within a generation it is byte-for-byte
  today's head+1 rule; epoch 0 is accepted only as the opening commit of generation
  N+1, and only from a migration commit referencing the closed head of generation N.
  Additive and backward-compatible — an old app never writes a non-zero generation.
  The transparency-log verifier and the machine-checked M4 spec extend to the same
  key (§8).
- **`mls_group_info` needs no new columns for suite discovery.** The suite is already
  self-describing inside the TLS-serialised `GroupInfo` and inside each commit, so a
  reader learns the suite by deserialising what it already receives. We *may* add a
  denormalised `ciphersuite INTEGER` to `mls_group_info` purely as an optimisation so
  Front C can decide "is this group hybrid yet?" without deserialising — additive if we
  do.
- **`user_device` gains a capability hint (optional).**
  `ALTER TABLE user_device ADD COLUMN pq_capable INTEGER NOT NULL DEFAULT 0;`
  Set to 1 when a device publishes hybrid KeyPackages. Lets the reconcile path answer
  "is every member hybrid-capable?" cheaply. This is a hint, not a security boundary —
  the actual gate is "does a hybrid KP exist to claim," which fails safe on its own.
- **Signature columns are left untouched** (Ed25519 stays), but we note the forward
  path: if ML-DSA is ever added, `mls_signature_pub` and the `device_cert` columns
  would gain an algorithm tag the same additive way. The account-key transparency log
  (`account_key_log`, `000005_account_key_log.sql`) is likewise untouched by the KEM
  change, since it records `account_id_pub` (Ed25519), not KEM keys.

No migration drops or narrows anything, so a pre-hybrid desktop app continues to run
against the migrated schema unmodified.

### 3.5 Device key packages during the window

`user_device.mls_signature_pub` (`000000_baseline.sql:156`) is the *signing* key, not
a KEM key, and signing stays classical (§2.4), so this column does **not** change and
the DS-auth path in `ds_client.rs` is unchanged *by this work* — whichever DS auth
scheme is live when this ships (today's device-header signature, or the anonymous
membership proofs of `docs/metadata-minimization-design.md` v1.5), the KEM change does
not touch it; the two programs are orthogonal. The KEM material lives entirely inside the
per-suite KeyPackages (`mls_key_package.key_package`), which is where the ML-KEM public
key rides. A device that is hybrid-capable simply maintains two KP pools; the
`replenish_key_packages` top-up logic (`key_packages.rs:141`) runs once per suite. On
login, `ensure_mls_key_package` rotates both pools; the classic pool is retained for the
entire overlap window so no old-app peer ever fails to add this device.

---

## 4. Performance

This is where the "fastest secure messenger" goal gets stressed, and where we must be
disciplined. The size delta is the dominant cost; the CPU delta is negligible.

### 4.1 Sizes: the real cost

| Item | X25519 (classic) | ML-KEM-768 | Hybrid (X25519 + ML-KEM-768) |
|---|---|---|---|
| KEM public key (in KeyPackage / leaf) | 32 B | 1184 B | ~1216 B |
| KEM ciphertext (per HPKE seal, in Welcome / commit path secret) | 32 B | 1088 B | ~1120 B |
| KEM private key (local only) | 32 B | 2400 B | ~2432 B |

Implications, mapped to Pollis's actual payloads. The per-message sizes below are
**measured in-box**, `--release`, with `use_ratchet_tree_extension(true)` (as Pollis sets
at `group_state.rs:508`), a single committer, TLS-serialised bytes:

| N | classic self-update / add / welcome | X-Wing self-update / add / welcome |
|---|---|---|
| 2   | 490 / 805 / 1034     | 3949 / 7819 / 8048       |
| 8   | 1052 / 1367 / 2220   | 13415 / 17287 / 18722    |
| 32  | 3090 / 3405 / 6578   | 43965 / 47835 / 53890    |
| 100 | 8736 / 9017 / 18622  | 126037 / 128688 / 147691 |

- **Commit size grows LINEARLY in N, not `log2(N)`.** An earlier draft of this document
  estimated `~1.1 KB × log2(N)`; that estimate was wrong. Measured, a hybrid commit is
  roughly **8× the classic commit at every group size**, and both grow linearly in N. The
  linear behaviour is *persistent*, not a bulk-add artifact: three successive self-updates
  by the same member produced byte-identical commits. The cause is structural — the other
  members' parent nodes stay blank, so every copath resolution resolves to individual
  leaves (one HPKE ciphertext each) rather than to a single subtree key. At N=100 a hybrid
  self-update commit is ~126 KB; base64 over the DS (~33% inflation, `key_packages.rs:26-37`)
  makes it a **~168 KB write on the membership-churn hot path.**
  `mls_commit_log.commit_data` (`000000_baseline.sql:111`) and the DS `submit_commit`
  write carry this. **This is the number to watch.**
- **Disabling the ratchet-tree extension does NOT help the commit.** It is tempting to
  reach for `use_ratchet_tree_extension(false)` as a mitigation; it is not one. Turning it
  off collapses the **Welcome only** (at N=100: 147691 → 1467 bytes) and leaves the commit
  *unchanged* (126037 bytes either way), because the ratchet tree rides in the
  GroupInfo/Welcome, not in the commit. The extension is not the lever for the number that
  matters; the linear copath cost is.
- **KeyPackage size:** a hybrid KP carries a 1216-byte `hpke_init_key` (ML-KEM-768 public
  key 1184 B + X25519 32 B — the byte count the P0 spike observed, confirming real hybrid
  encapsulation), landing the KP at ~1.5–2 KB. With TARGET=5 packages per suite per device
  (`key_packages.rs:94`), a hybrid-capable device stores/publishes ~10 KB of hybrid KP
  material in `mls_key_package` (base64-inflated ~33% over the DS wire,
  `key_packages.rs:26-37`). Small in absolute terms.
- **Welcome size:** a Welcome HPKE-seals the group secrets to each joiner and (with the
  ratchet-tree extension on) inlines the tree; at N=100 a hybrid Welcome is ~148 KB.
  `mls_welcome.welcome_data` (`000000_baseline.sql:128`) and the DS `/v1/...` welcome
  writes carry it. As above, this is the *one* payload the ratchet-tree extension
  dominates, so it is also the one place turning the extension off would help — at the cost
  of every joiner having to fetch the tree separately.
- **Turso / DS payloads:** all of the above are base64-encoded in JSON bodies through the
  DS (`ds_client.rs`), a ~33% inflation on already-larger blobs, then stored as BLOBs in
  Turso. For a large hybrid group the commit is now the dominant metadata write — a
  ~168 KB base64 body per churn commit at N=100 — no longer dwarfed by anything but an
  attachment (R2, whitepaper §9).
- **RESOLVED in P3/P4: no group-size threshold; the cost was mis-attributed.** The
  linearity above is not a property of hybrid MLS at size N — it is a property of a tree
  full of **unmerged leaves**. Every measurement in the table was taken with a single
  committer, so every *other* member's parent node was blank and each copath resolution
  fell back to one HPKE ciphertext per leaf. Once each member has committed once, those
  parents are populated and a copath resolution collapses to a single subtree key, which
  is the `log2(N)` behaviour the earlier draft assumed and this table appeared to refute.
  Re-measured with that variable controlled: growing a hybrid group 8 → 16 members costs
  **+10.6 KB with unmerged leaves and +2.4 KB once every member has committed.** The
  remedy is therefore periodic self-update (#666), which makes every member commit on its
  own schedule, not a size cap that would have permanently stranded large groups on the
  classic suite for a reason that was never really about size. Pinned by
  `self_update_turns_linear_commit_growth_into_logarithmic`, which controls merge state
  exactly, and by the absolute small-roster ceilings in `flows/pq_migration.rs`
  (`MAX_HYBRID_COMMIT_BYTES`).

### 4.2 Latency: negligible

ML-KEM-768 keygen/encaps/decaps in `libcrux-ml-kem` (a formally-verified, optimised
implementation) run in **tens of microseconds** on desktop hardware — the same order as
X25519, and orders of magnitude below Argon2id's deliberate ~250 ms unlock cost
(whitepaper §3.1) or a single Turso round-trip. Hybrid KEM ops add one ML-KEM operation
alongside the existing X25519 one; the CPU cost is in the noise next to the network. The
MLS crypto already runs in the Rust core off the render thread
(`PollisProvider`/`RustCrypto`, `provider.rs:37-53`), consistent with the CLAUDE.md
"lean Rust for perf-critical paths" doctrine, so there is no GC/IPC penalty.

### 4.3 How we bound it

- **Suite-scoped, not global:** classic groups keep classic (small) commits. Only
  hybrid groups pay the size cost, and only for the KEM-bearing fields.
- **Keep signatures classical in Phase 1** (§2.4) so we do *not* stack ML-DSA's
  multi-KB-per-signature cost — on *every* leaf and *every* commit — on top of the KEM
  cost. That single decision is the biggest lever keeping commit/KeyPackage sizes down.
  (The AEAD is *not* a lever we hold: the hybrid suite is ChaCha20-Poly1305 by
  construction — §2.3 — but that swap is roughly size-neutral versus AES-128-GCM, so it
  does not move the commit-size numbers; the KEM does.)
- **Keep leaves merged, don't cap group size** (§4.1, resolved): the apparent linearity
  was unmerged leaves, not N. Periodic self-update (#666) is the lever — it restores
  `log2(N)` copath resolution — and no group-size threshold was introduced. Disabling the
  ratchet-tree extension is *not* an alternative here either (it only shrinks Welcomes,
  §4.1).
- **KP pool size stays at 5 per suite** — resist inflating it; replenishment already
  tops up after each Welcome (`key_packages.rs:141`).
- **Measure in-box** (§5): shipped. `flows/pq_migration.rs` pins absolute small-roster
  commit/Welcome ceilings, and the marathon fuzzer carries a pool-scaled balloon detector
  across the suite boundary — it cannot control merge state at measurement time, so it is
  deliberately loose and the discriminating proof lives in the self-update test that can.

Net: hybrid MLS costs Pollis **microseconds per op** and roughly **8× the classic commit
in constant factor**, growing `log2(N)` in group size provided leaves stay merged — which
is what periodic self-update (#666) now guarantees. That is a defensible price for
post-quantum confidentiality at any group size Pollis targets, and it holds as long as
signatures stay classical (§2.4) and self-update keeps running.

---

## 5. Testing in-box

The box builds and gates `pollis-core` headless
(`cargo test -p pollis --no-default-features --features test-harness --test flows`,
CLAUDE.md testing section), and the marathon fuzzer + headless MLS flows harness run
locally (per the box's MLS harness capability). This is exactly the tooling to validate
a suite transition, because the failure modes are all *convergence* failures — mixed
members diverging, or a message going undecryptable across the boundary — which are what
the harness is built to catch. Every scenario below is a `flows.rs`-style multi-client
test driving real command implementations (no `_inner` shims, no mocked DB — CLAUDE.md).

**S1 — New hybrid group, happy path.** Three hybrid-capable clients create a
`CS_HYBRID` group, exchange messages, add/remove members. Assert: all messages decrypt
for all current members; the group's suite is hybrid end-to-end; voice-key export
(`export_secret`) still yields a common key across the epoch.

**S2 — Group that spans the suite transition.** Start a classic group with clients A, B,
C (all classic-app). Send messages M1–M5. Upgrade all three to hybrid-capable, publish
hybrid KPs, trigger the Front-C migration commit. Send M6–M10. Assert: M1–M5 remain
readable (they were decrypted pre-migration and persist locally); M6–M10 decrypt for all
three on the hybrid group; **no message in the M1–M10 range is dropped or undecryptable**
for any member who was present throughout. This is the core "messages must work" proof.

**S3 — Mixed fleet: one member stays on the old suite.** Classic group A, B, C. A and B
upgrade and advertise hybrid KPs; C stays classic (no hybrid KP). Attempt a hybrid
migration. Assert: the migration is a **no-op** (the §3.3 rule blocks it — not every
member is hybrid-capable); the group stays classic; A, B, C keep exchanging messages
with zero drops. Then upgrade C, re-trigger, assert the migration now succeeds and all
three converge on hybrid. Proves the interop gate is enforced by construction, and that
adding an old-app member to a hybrid group is *impossible*, not merely avoided.

**S4 — Harvest-then-migrate (the HNDL story, mechanically).** Capture the classic-suite
commit/Welcome/message blobs from S2's pre-migration phase (as an "adversary's
recording"). After migration, assert the *new* hybrid blobs carry ML-KEM ciphertext
(size + structure check on `commit_data` / `welcome_data`) — i.e. post-migration traffic
is no longer classical-only-sealed. We cannot run a CRQC in a test, so this is a
*structural* assertion (the KEM material is present and hybrid) plus a negative check
that no post-migration path secret is derivable from X25519 material alone. It documents
the property the migration buys.

**S5 — Marathon / fuzzer: convergence under churn across the boundary.** Feed the
marathon fuzzer a schedule that (a) builds a large classic group over many
adds/removes/commits, (b) flips the fleet to hybrid-capable mid-run, (c) migrates, (d)
continues churn on hybrid. Assert the fuzzer's existing convergence invariant (all
members at the same epoch see the same message set, no lost pre-commit messages — the
exact class the sweep/realtime bug in the box's MLS notes targets) holds *across the
suite boundary*. Add a payload-size assertion so a commit/Welcome that balloons beyond a
budget fails the run (§4.3).

**S6 — External-join across suites.** A recovered device (Secret-Key path,
`external_join_group`, `group_state.rs:196`) joins a group that has migrated to hybrid.
Assert it fetches the hybrid GroupInfo, external-commits into the hybrid group, and its
cross-signing cert still verifies (`verify_added_devices`, `device.rs:409`) — confirming
the classic Ed25519 cert path is untouched by the KEM change (§3.1).

All six are pure `pollis-core` tests, runnable headless in-box on `ci/mls-test-gate`
before anything reaches CI or the fleet.

---

## 6. Threat model / honest scope

**What hybrid PQ MLS buys:**

- **Forward secrecy against a future quantum adversary on the key exchange.** A CRQC
  that harvested today's *hybrid* traffic cannot recover the shared secret, because it
  would have to break *both* X25519 (classically hard, quantumly broken) *and*
  ML-KEM-768 (quantum-hard). Recorded post-migration ciphertext stays confidential.
  This is the entire HNDL defence, and it is real.
- **Defence in depth for the transition itself:** because it is hybrid, adopting the
  young `libcrux-ml-kem 0.0.8` cannot regress us below today's classical security even
  if ML-KEM (or its implementation) has a flaw (§2.2).

**What it explicitly does NOT buy — stated to avoid overclaiming:**

- **It does not fix metadata.** Turso still sees the social graph, membership,
  message timing and sizes (whitepaper §1.2). PQ KEX seals *content*, not the fact that
  A messaged B. If anything, larger PQ ciphertexts make *size*-based metadata slightly
  coarser, not finer.
- **It does not help if an endpoint is compromised.** A device with the unlocked keys,
  or malware in the trusted binary, reads plaintext regardless of KEM. HNDL is a
  *passive network* threat; endpoint compromise is a different axis (whitepaper §1.1).
- **It does not make signatures/authentication post-quantum** (§2.4). A future CRQC
  could forge Ed25519 and mount an *active* impersonation going forward. That is a
  store-now-*forge*-later concern, addressable reactively; it is out of scope for this
  HNDL-focused change, and we say so.
- **It does not retroactively protect already-sent classic traffic.** Everything sent
  before a group migrates was sealed with X25519 and is, in principle, harvestable. The
  migration caps the *future* harvest window; it cannot un-harvest the past. This is
  intrinsic to HNDL and is the reason to start now (§1.3).
- **It is not a "fully post-quantum messenger" claim.** The honest, defensible headline
  is: **"post-quantum *confidentiality* for message content via hybrid X25519+ML-KEM
  key exchange."** That is a genuine cutting-edge position — it puts Pollis level with
  Signal's PQXDH and ahead of any messenger still fully classical on KEX — without the
  overclaim that everything is quantum-proof.

Positioned this way it is a real flex *and* it survives an auditor reading the
whitepaper's "Honest limits" tradition (whitepaper §6.9, §13). Overclaiming here would
be worse than not shipping.

---

## 7. Feasibility assessment — installed stack vs. what's needed

**Honest bottom line: the pinned stack already ships the hybrid ciphersuite. The P0
spike proved a full hybrid group flow end-to-end, headless, with stock cargo — no version
bump, no fork, no `[patch.crates-io]`. The live risk is not availability; it is code-point
churn, and it is managed by pinning.**

### 7.1 What the pinned tree can already do

- The suite **exists in the pinned enum.** `openmls_traits 0.5.0` (already locked) defines
  `MLS_256_XWING_CHACHA20POLY1305_SHA256_Ed25519 = 0x004D` in its `Ciphersuite` enum,
  **not** feature-gated (added upstream in openmls/openmls#1546, 2024-04). The earlier
  claim that "no PQ suite exists in this version" was simply wrong — it confused the
  missing *provider* for a missing *suite*.
- The missing piece was only ever the **crypto provider.** `openmls_rust_crypto 0.5.1` —
  the provider Pollis instantiates — `unimplemented!()`-panics on this suite
  (`src/provider.rs:61`). `openmls_libcrux_crypto 0.3.1` (published the same day as the
  0.8.1 release) implements it (`src/crypto.rs:60`, mapping to
  `hpke_rs_crypto::types::KemAlgorithm::XWingDraft06`). So the earlier observation (§1.3)
  that the ML-KEM primitives are already in `Cargo.lock` but unreachable was right about
  the primitives and wrong about the reason: the ciphersuite *is* registered; Pollis merely
  instantiates the one provider that refuses it.
- **The route, pinned:** keep `openmls = "0.8"` and add a direct dependency on
  `openmls_libcrux_crypto = "0.3"`. That is the whole dependency change. OpenMLS also
  offers an optional `libcrux-provider` feature, but it does nothing except pull in
  `dep:openmls_libcrux_crypto`; the direct dependency is equivalent and keeps the
  provenance of the provider visible in our own `Cargo.toml` rather than behind an
  upstream feature flag. As shipped: `pollis-core/Cargo.toml`.

### 7.2 P0 is DONE — proven end-to-end, headless

A two-client throwaway crate on `openmls 0.8.1` + `openmls_libcrux_crypto 0.3.1`, stock
cargo, no patches, ran the full flow — KeyPackage → Add → Welcome → join → application
message → `export_secret` (the voice-frame-key path). Output:

```
bob kp init_key len = 1216
welcome secrets = 1
bob joined: epoch GroupEpoch(1), cs MLS_256_XWING_CHACHA20POLY1305_SHA256_Ed25519
bob decrypted: "hello pq world"
```

The 1216-byte `hpke_init_key` is exactly ML-KEM-768 public key (1184) + X25519 (32),
confirming real hybrid encapsulation — not a stubbed or classical-only path.

### 7.3 Provider integration: drop-in, but suite-routed on purpose

- The provider is generic over its crypto half (`pollis-core/src/commands/mls/provider.rs`):
  `MlsProvider<C>` composes any backend `C` with the custom `MlsStore`.
  `openmls_libcrux_crypto` `pub use`s its `CryptoProvider` (`src/lib.rs:6`), so adding a
  hybrid-capable provider means adding one type alias and leaves `MlsStore` untouched —
  storage is ciphersuite- and backend-agnostic, storing opaque blobs in `mls_kv`
  (`pollis-core/src/signal/mls_storage.rs:223`).
- **The libcrux provider *can* serve both suites — and we deliberately do not let it.**
  The full two-party flow ran under it on classic-AES, classic-ChaCha *and* X-Wing, all
  green, so a single-provider design is technically available. It was rejected on a
  security ground discovered when the supply-chain gate was run against the change:
  `openmls_libcrux_crypto 0.3.1` pulls `libcrux-aead 0.0.6` with default features, and
  **`libcrux-aesgcm <= 0.0.8` checks the AES-GCM authentication tag in non-constant time
  (RUSTSEC-2026-0211, `patched = []`)**. Pollis's *current* suite is AES-128-GCM, so a
  wholesale swap would move every message decrypt in the shipping app onto a timing side
  channel — a real regression today, bought with a post-quantum capability nothing yet
  uses. The fix exists only in the renamed `libcrux-aes 0.0.9`, which no released
  `openmls_libcrux_crypto` depends on.
- **Therefore: the crypto provider is routed by ciphersuite.** `PollisProvider`
  (RustCrypto, constant-time `subtle` tag compare) serves the classic suite and is the
  only provider any production path constructs; `PollisPqProvider` (libcrux) serves the
  ChaCha20-Poly1305 hybrid suite and is constructed nowhere in production yet. Both
  compose the same untouched `MlsStore`. The mixed-provider hazard this creates is
  *bounded and already measured*: RustCrypto-backed and libcrux-backed clients interoperate
  in one classic group in all four direction combinations (KeyPackage, Welcome/join,
  application messages both ways, a cross-provider self-update commit, and byte-identical
  `export_secret`), pinned by the cross-provider interop test. The routing itself is pinned
  by `classic_provider_never_routes_to_libcrux`, which is also the stated basis of the
  `deny.toml` entry for RUSTSEC-2026-0211 — if the providers are ever merged, that ignore
  becomes a lie and the test fails first.
- **Budget the other five libcrux advisories too.** Adding the provider pulls
  RUSTSEC-2026-0209/0210 (`libcrux-aesgcm`), -0124 (`libcrux-chacha20poly1305`), -0075
  (`libcrux-ed25519`) and -0073 (`libcrux-poly1305`) into the graph. Under the routing
  above none are reachable from live traffic; -0124 and -0073 *do* sit on the hybrid path
  and must be re-argued (not merely re-ignored) before P2 puts a real group on that suite.
  Reasons are recorded per-ID in `deny.toml`.
- **WARNING — do not use the crate's bundled `Provider` type.** It hardcodes
  `openmls_memory_storage::MemoryStorage` (`src/lib.rs:13`) and would silently lose all
  MLS group state on restart. Pollis composes `openmls_libcrux_crypto::CryptoProvider`
  with its own `MlsStore`, exactly as `PollisProvider` does with `RustCrypto`.
- **SHARP EDGE — the libcrux provider's `supports()` self-contradicts.** Its `supports()`
  returns `Err(UnsupportedCiphersuite)` for Pollis's *current* AES-GCM suite — it demands
  `hpke_aead == ChaCha20Poly1305` (`src/crypto.rs:48-51`) — while its own
  `supported_ciphersuites()` lists that same AES-GCM suite. No path in the full flow
  consults `supports()`, so classic groups work today; but anything that *starts* calling
  `supports()` would break every classic group. P1a pins this with a regression test (§8).

### 7.4 The live risk: code-point churn, not availability

The suite works today, but `0x004D` is a moving target and must be pinned as one:

- `0x004D` is an **experimental** 2024 OpenMLS code point (X-Wing draft-06). The IANA MLS
  ciphersuite registry has **zero** post-quantum entries; nothing here is standards-final.
- `draft-ietf-mls-pq-ciphersuites-06` (2026-07-21) **dropped X-Wing**, moving to
  provisional code points `0x004E` / `0x004F` / `0x0052` built on
  `draft-irtf-cfrg-concrete-hybrid-kems`. WG state: "Waiting for WG Chair Go-Ahead."
- OpenMLS `main` implements those new suites behind a new feature flag
  `draft-ietf-mls-pq-ciphersuites` (openmls/openmls#2046 merged 2026-06-11,
  openmls/openmls#2118 2026-07-16) — **and has moved `0x004D` behind that same flag.** So
  the next OpenMLS release is a **breaking change for this usage**: the suite we rely on
  moves from always-on to feature-gated. **Pin the versions** (`openmls = "0.8"`,
  `openmls_libcrux_crypto = "0.3"`, suite `0x004D`), and treat a future OpenMLS bump as a
  **tracked migration**, not a routine dependency update — the bump is where `0x004D`
  becomes conditional on the `draft-ietf-mls-pq-ciphersuites` feature.
- The new draft suites are not a drop-in replacement yet either: openmls/openmls#2104
  (open) notes they are currently implemented with SHA256 where the draft specifies
  SHAKE256 — known-wrong and unreleased.

**What this means for shipping.** Pollis is a closed fleet (one binary, one DS), so
shipping a private/experimental code point is *tolerable* — every client and the DS move
together, and there is no third-party interop to honour. But budget for a **second suite
migration** when the IETF suites stabilise (`0x004E`/`0x004F`/`0x0052`, SHAKE256 fixed, WG
go-ahead). Crucially, that is an argument **for** building the `mls_commit_log.generation`
machinery (§3.2, §3.4) properly rather than against it: the same suite-generation lineage
that carries classic → X-Wing carries X-Wing → the IETF suite later. **Built once, used
twice.**

### 7.5 In-tree work (our side), verified against `origin/main`

- **Suite plumbing (P1b — DONE):** the 31 references to `CS` / `.ciphersuite()` split far
  more cleanly than the raw count suggested. Both suites sign with Ed25519, so all 9
  signature-key-gen sites are suite-INVARIANT and now read a `SIGNATURE_SCHEME` constant
  rather than taking a suite parameter — a device keeps ONE signing key and one
  `device_cert` across suites, which is the behaviour the cross-signing design already
  assumed. Only the two families that mint suite-bound material became parametrised:
  group creation (`create_mls_group_in_suite`) and KeyPackage construction
  (`build_key_package_in_suite`), each taking the suite alongside the provider that serves
  it. Parametrising the signature sites too would have been busywork encoding a
  distinction that does not exist.
- **DS monotone-head rule** is enforced **solely** in `pollis-delivery/src/commit.rs`:
  `head_epoch` (`commit.rs:131`) and the conditional
  `INSERT ... WHERE ?2 = (SELECT COALESCE(MAX(epoch), -1) + 1 ...)` in `submit_commit`
  (`commit.rs:179-213`), keyed `(conversation_id, epoch)` by the `idx_mls_commit_unique`
  index. That single site is the one that widens to `(conversation, generation, epoch)`
  (§3.2, §3.4).
- **Capability advertisement is genuinely new:** `user_device` advertises **no**
  capabilities today (only `mls_signature_pub`), so `pq_capable` (§3.4) is a new
  advertisement channel, not an extension of an existing one.
- **The provider work (P1a)** generalises `PollisProvider` over its crypto backend and
  adds a second alias, `PollisPqProvider`, wired to libcrux — it does **not** move the
  classic suite off RustCrypto (§7.3). Behaviour on every production path is therefore
  byte-identical; the gate is the whole existing suite passing, plus the `supports()`
  regression test and the routing-invariant test from §7.3.

All of the in-tree work builds and tests against the classic suite today, and the crypto
route is already proven (§7.2), so there is **no external gate left** — the long pole is
now our own plumbing, not an upstream dependency.

---

## 8. Phased roadmap, acceptance criteria, dependencies

**Phase 0 — Spike: COMPLETE.** ✅ The crypto route is pinned and proven. A throwaway
two-client crate on `openmls 0.8.1` + `openmls_libcrux_crypto 0.3.1` (stock cargo, no
patch) created a hybrid `0x004D` group and round-tripped an application message plus an
`export_secret` between two clients, headless (§7.2). **Decision recorded:** no version
bump and no fork — keep `openmls = "0.8"`, add a direct dependency on
`openmls_libcrux_crypto = "0.3"`, suite `MLS_256_XWING_CHACHA20POLY1305_SHA256_Ed25519`
(`0x004D`). The one dependency risk carried forward is code-point churn (§7.4): pin both
versions, and treat a future OpenMLS bump as a tracked migration.
**No remaining upstream gate.**

**Phase 1a — Provider routing: DONE.** ✅ Landed as *routing*, not the swap this phase
originally proposed. `PollisProvider` (RustCrypto) serves `CS_CLASSIC` and `PollisPqProvider`
(`openmls_libcrux_crypto`) serves `CS_HYBRID`, both over the custom `MlsStore`; the crate's
bundled `Provider` type is not used (it hardcodes in-memory storage — §7.3). The swap was
rejected because libcrux's AES-GCM decryption has an unpatched non-constant-time tag check
(RUSTSEC-2026-0211), which would have put *classic* traffic on vulnerable code; the hybrid
suite's AEAD is ChaCha20-Poly1305, which the advisory does not touch. The routing is
therefore security-load-bearing and is pinned by `classic_provider_never_routes_to_libcrux`
alongside the `supports()` self-contradiction regression test (§7.3).

**Phase 1b — Suite parametrisation: DONE.** ✅ `CS_CLASSIC` / `CS_HYBRID` introduced, every
`CS` call site made suite-aware (the 31 references in §7.5), the additive migrations landed
(§3.4), and the DS gained its claim-by-suite parameter — all defaulting to classic, so the
phase itself changed no group's observed suite.

**Phase 2 — Hybrid key packages (Front B): DONE.** ✅ Hybrid-capable devices publish both KP
pools; DS claims by suite; `pq_capable` set. Still no group goes hybrid.
**Acceptance:** S3's first half (mixed fleet, no migration) passes; a device advertises
both pools; claiming a classic KP for an old-app add still works; KP-size assertions
within budget (§4.3).
**What shipped:**
- `ensure_mls_key_package` (rotate) and `replenish_key_packages` (top-up) iterate
  `PUBLISHED_SUITES = [CS_CLASSIC, CS_HYBRID]`, building each pool through its
  paired provider (`PollisProvider`/RustCrypto for classic,
  `PollisPqProvider`/libcrux for hybrid) and rotating/topping-up **per suite** so
  neither pool is ever silently drained. The rotation sends both pools in one
  `/v1/key-packages/replenish`; the DS already scopes its DELETE per suite.
- `pq_capable` is flipped to `1` by the DS `apply_publish_key_packages` /
  `apply_replenish_key_packages` **only** when the write carries a hybrid package —
  derived from what actually landed in `mls_key_package`, never a client-side
  `user_device` UPDATE (CLAUDE.md). Monotonic: a later classic-only top-up never
  clears it. The classic no-DS fallback mirrors this in
  `insert_packages_direct`.
- **No group changed suite:** `init_mls_group` and every add/invite path still pass
  `CS_CLASSIC` — untouched.
- Tests: `pq_key_packages::hybrid_device_advertises_both_pools_and_pq_capable`
  (flows) proves both pools publish, `pq_capable` flips only after the hybrid pool
  lands, and a suite-less (old-app) classic claim still succeeds and draws from the
  classic pool only; DS `publishing_a_hybrid_pool_flips_pq_capable_and_classic_does_not`
  and `rotating_one_suite_leaves_the_other_pool_intact`;
  `claiming_hybrid_against_a_classic_only_pool_finds_nothing` (P1b, the classic-only
  → no-KP outcome) and the `the_two_suites_produce_structurally_different_key_packages`
  KP-size pins (classic init key 32 B, hybrid 1216 B).

**Phase 3 — New groups hybrid (Front A): DONE.** ✅ `suite_for_new_group`
(`mls/group_state.rs`) decides the suite at birth instead of `init_mls_group` passing a
constant. **No group-size threshold was set** — that §4.1 decision is resolved as
"explicitly accepted cost", because the measured driver of hybrid commit size turned out
to be *unmerged leaves*, not membership: at 8→16 members a hybrid commit grows +10.6 KB
with unmerged leaves and +2.4 KB once every member has committed. The fix is therefore
self-update (#666, shipped alongside), which makes growth logarithmic, not a size cap that
would have permanently stranded large groups on classic.
**What shipped:** S1 (`flows/pq_migration.rs`) covers birth-hybrid plus membership churn
on the hybrid suite; `pq_key_packages.rs` pins the absolute Welcome/commit byte ceilings;
`export_secret` on hybrid (the voice-key path) is proved in pollis-core's MLS unit tests,
since the media-gated `voice_e2ee` surface cannot build headless.

**Phase 4 — Migrate existing groups (Front C): DONE.** ✅ Implemented as **suite
generations** rather than an epoch-boundary transition commit, because RFC 9420 provides
no way to change a running group's suite: `migrate_to_hybrid_if_due` (`mls/migrate.rs`)
stands up a **successor group** in `CS_HYBRID`, moves the roster by Welcome, and retires
the predecessor. The monotone key widens to `(conversation_id, generation, epoch)` compared
lexicographically; the DS accepts generation N+1 at epoch 0 only when `closes_epoch` names
the head of generation N (`pollis_delivery::commit::accepts()`, Kani-proved), making the
whole migration one compare-and-swap and two competing successors unrepresentable. Bounded
at `MAX_MIGRATIONS_PER_SWEEP = 2`; eligibility does not expire, so spreading them is free.
**What shipped:** S2/S3 (each gate holding on its own), S4 (a stolen pre-migration leaf is
evicted at the suite boundary), S6 (a dropped successor Welcome recovers by external join),
and S5 — the `model` fuzzer now draws `Sweep`/`Upgrade` ops, crosses the boundary mid-run,
and asserts no client is stranded on a retired lineage plus a pool-scaled commit-size
balloon detector. A `BOUNDARY_CROSSINGS` counter fails the driver if no generated case
actually crossed, so the invariant cannot pass vacuously.

**Phase 5 — Fleet completion: DONE (gate), classic retirement NOT started.** ✅ What
shipped is the **fleet-completeness gate**, not the retirement: `fleet_is_fully_pq_capable`
requires that deployment-wide no `user_device` with `revoked_at IS NULL` and `last_seen`
within `FLEET_DORMANCY_DAYS` (90) still has `pq_capable = 0`, and it gates **birth and
migration alike** — one switch, one meaning. The per-roster rule of §3.3 is necessary but
nearly vacuous on its own: a roster grows, and a hybrid group that must later admit a
classic-only device has only two options, both bad. The dormancy window is what makes
"the fleet has updated" a reachable condition rather than one hostage to a laptop in a
drawer; the gate fails *toward availability*, so a check that cannot be evaluated keeps the
group classic.
**Still open, deliberately:** classic KP publication is *not* retired — pre-migration
lineages and any client that has not yet updated still need it, and dropping it is a
multi-release additive→drop dance (CLAUDE.md) that cannot begin until an app that stopped
*needing* classic has full uptake. AES-256 and ML-DSA remain *distinct future tracks*
(§2.3, §2.4), off this HNDL critical path.

**Cross-cutting acceptance invariants (all phases):**
- No migration is ever a rename/drop/tightening (CLAUDE.md).
- No message sent to a throughout-member is ever dropped or rendered undecryptable
  ("messages must work").
- A hybrid group can *never* contain a member lacking a hybrid KP (invalid state
  unrepresentable, enforced at the claim/reconcile chokepoint).
- Everything builds and gates **headless in-box**.

**Dependencies summary:** (1) ~~OpenMLS PQ ciphersuite upstream~~ — **resolved in P0**:
the suite ships in the pinned `openmls_traits 0.5.0` and the libcrux provider (proven
building and running headless) implements it, so no upstream availability gate remains.
The residual dependency is *pinning discipline* against code-point churn (§7.4) — pin the
OpenMLS and `openmls_libcrux_crypto` versions, and track a future OpenMLS bump as a
migration. (2) DS write endpoints extended for suite-tagged KP claim/publish; (3) ~~a
coordination point with the machine-checked-correctness program~~ — **resolved in P4**:
its M4 TLA+ spec (`specs/tla/CommitLog.tla`,
`docs/machine-checked-correctness-design.md`) and the transparency-log verifier
(`verifiable-log-builder`'s `CommitLogInvariant`) both enforced per-conversation epoch
monotonicity, and both were extended to `(conversation, generation, epoch)` (§3.2, §3.4)
in the same PR that landed the migration. The spec gained the migration actions and
`OpeningClosesTheHead`; the verifier's leaf encoding stayed byte-identical for
generation 0, so every already-published Merkle root and cached inclusion proof
remains valid.

---

## 9. GitHub-issue-ready summary

**Title:** Post-quantum hybrid MLS — migrate key exchange to X25519 + ML-KEM-768 (HNDL defence)

**Problem:** Pollis's MLS key exchange is 100% classical — DHKEM(X25519) in
`MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519` (`pollis-core/src/commands/mls/provider.rs:57`).
An adversary can *harvest today's ciphertext and decrypt it later* with a quantum
computer (harvest-now-decrypt-later). For a privacy-first messenger whose messages have
a multi-year sensitivity horizon, this is the top unaddressed cryptographic risk. Peers
(Signal PQXDH, iMessage PQ3) already ship hybrid PQ key exchange; Pollis does not. The
exposed asset is the KEM/HPKE key agreement (TreeKEM path secrets, KeyPackage init keys,
Welcomes) — **not** signatures, which HNDL does not threaten, so signatures stay
classical for now. **Feasibility (verified in P0):** no version bump and no fork are
needed. The hybrid suite `MLS_256_XWING_CHACHA20POLY1305_SHA256_Ed25519` (`0x004D`)
already exists in the pinned `openmls_traits 0.5.0` enum; the only missing piece was the
crypto provider. Keep `openmls = "0.8"` and add a direct dependency on
`openmls_libcrux_crypto = "0.3"` (which implements the suite — `openmls_rust_crypto 0.5.1`
panics on it) — and a full hybrid group flow is proven headless with stock cargo. The
bundled AEAD change (AES-128-GCM → ChaCha20-Poly1305, level 128 → 256) rides with the
suite; Ed25519 signatures are unchanged. The live risk is **code-point churn**, not
availability: `0x004D` is an experimental X-Wing point that OpenMLS `main` moves behind a
feature flag and the IETF has dropped in favour of `0x004E`/`0x004F`/`0x0052` — pin the
version and feature name, and budget a second suite migration later.

**Approach:** dual-suite, epoch-boundaried, never a flag day. New groups go hybrid once
the roster is capable; existing groups migrate at a clean commit boundary into a hybrid
successor group; every device publishes classic **and** hybrid key packages during a
long overlap so no old-app peer is ever un-addable; a group migrates only once *every*
member advertises a hybrid KeyPackage (invalid states unrepresentable). The successor
group's epoch lineage is tracked by an additive `mls_commit_log.generation` column, so
the DS monotone-head rule widens to `(conversation, generation, epoch)` and epoch
monotonicity is preserved — never an epoch reset under the old key. Schema changes
are additive only (suite-tag `mls_key_package`, `mls_commit_log.generation`, optional
`user_device.pq_capable`). Signatures stay Ed25519 (unchanged); the AEAD is **not**
unchanged — the only available hybrid suite bundles ChaCha20-Poly1305, so hybrid groups
move AES-128-GCM → ChaCha20-Poly1305 (level 128 → 256) while classic groups keep
AES-128-GCM.

**Phased milestones:**
- **P0 — Spike: DONE.** Route pinned and proven headless (`openmls 0.8` + a direct
  `openmls_libcrux_crypto 0.3` dependency, suite `0x004D`); no bump, no fork.
- **P1a — Provider routing:** `PollisProvider` (RustCrypto, classic) and
  `PollisPqProvider` (libcrux, hybrid-only) over one shared `MlsStore`; no production path
  changes backend, so behaviour is byte-identical. Gate on the full suite + the `supports()`
  regression test + `classic_provider_never_routes_to_libcrux` (§7.3).
- **P1b — Suite parametrisation: DONE.** `CS_CLASSIC` / `CS_HYBRID` / `SIGNATURE_SCHEME`;
  the suite is an explicit argument to `create_mls_group_in_suite` and
  `build_key_package_in_suite` (and to `ds_claim_key_package`), with every production
  caller passing `CS_CLASSIC`. Additive migration `000010` adds
  `mls_key_package.ciphersuite` (default `1`) and `user_device.pq_capable` (default `0`).
  The DS publishes and claims per suite, defaulting to classic when the field is absent,
  so an already-shipped client is bit-for-bit unaffected; a replenish now replaces only
  the pools it publishes, which matters the moment P2 gives a device two pools. Gated by
  a full two-party round-trip driven through the production seams on BOTH suites, plus
  DS tests that the two pools cannot contaminate each other.
- **P2 — Hybrid key packages** (Front B): **DONE.** Dual KP pools published + rotated
  per suite, claim-by-suite, `pq_capable` derived server-side from the published
  hybrid pool. No group goes hybrid yet.
- **P3 — New groups hybrid** (Front A).
- **P4 — Migrate existing groups** (Front C) at epoch boundary, capability-gated.
- **P5 — Fleet completion**; classic retirement only after measured full-hybrid uptake;
  AES-256 / ML-DSA as separate future tracks.

**Acceptance criteria:**
- Headless `flows` scenarios S1–S6 pass on `ci/mls-test-gate`, including **S2**
  (a group spanning the suite transition loses no message) and **S3** (a member on the
  old suite blocks migration by construction, drops nothing).
- Marathon fuzzer convergence invariant holds *across* the suite boundary — no
  dropped/undecryptable message for any throughout-member; commit/Welcome payloads
  within a size budget.
- Post-migration commit/Welcome blobs structurally carry ML-KEM ciphertext (hybrid),
  verified in-box.
- Every migration is additive-only; a pre-hybrid desktop app keeps working against the
  migrated schema.
- A hybrid group can never contain a member without a hybrid KeyPackage.
- Honest scope in docs: "post-quantum **confidentiality** via hybrid key exchange" —
  not "fully post-quantum"; does not fix metadata or endpoint compromise; does not
  protect already-sent classic traffic.

**Dependencies / risks:** no upstream availability gate remains — P0 proved the pinned
route (`openmls 0.8` + `openmls_libcrux_crypto 0.3`, suite `0x004D`)
builds and round-trips headless. Residual risk is **code-point churn**: `0x004D` is
experimental and OpenMLS `main` feature-gates it, so pin both versions and treat an
OpenMLS bump as a tracked migration; budget a
second suite migration when the IETF `draft-ietf-mls-pq-ciphersuites` suites
(`0x004E`/`0x004F`/`0x0052`) stabilise. Also carry the group-size cost decision (commits
are linear in N, ~8× classic — §4.1) into P3/P4.
