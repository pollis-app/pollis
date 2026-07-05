# Post-Quantum Hybrid MLS — Design

**Status:** design proposal (decision-quality). Not yet implemented.
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
  (`draft-connolly-cfrg-xwing-kem`) as the DHKEM, matching the MLS PQ ciphersuite
  drafts. (§2.)

- **Feasibility, honestly: the installed stack cannot do this today without upstream
  work.** Pollis pins `openmls 0.8.1` with `openmls_rust_crypto 0.5.1`
  (`pollis-core/Cargo.toml:110-113`, `Cargo.lock:5390-5455`). OpenMLS 0.8.1's
  `Ciphersuite` enum contains only the seven classical RFC 9420 suites — **no PQ /
  X-Wing suite exists in this version.** The single suite constant Pollis uses,
  `MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519`
  (`pollis-core/src/commands/mls/provider.rs:57`), is classical. Interestingly, the
  ML-KEM code is *already in the build*: `openmls_rust_crypto 0.5.1` pulls
  `hpke-rs 0.6.1`, whose sibling `hpke-rs-libcrux 0.6.1` + `libcrux-ml-kem 0.0.8` are
  present in `Cargo.lock` (`Cargo.lock:3090-3133, 4042-4055`) — but the provider
  Pollis instantiates uses the `hpke-rs-rust-crypto` backend
  (`Cargo.lock:5443-5445`), which is classical-only. **The primitives are compiled;
  the ciphersuite that would reach them is not.** The path forward is an OpenMLS
  version bump (to a release that lands the MLS PQ ciphersuite) or a pinned
  fork/patch, plus switching the crypto provider to the libcrux/X-Wing backend. (§7.)

- **Recommendation: do it, in phases, gated behind the box's headless MLS harness —
  but Phase 0 is a spike to nail the exact OpenMLS/provider version that ships an
  X-Wing suite, because everything downstream depends on it.** Ship new groups on the
  hybrid suite first; migrate existing groups at an epoch boundary via a
  ciphersuite-transition commit; publish key packages in **both** suites during a
  long overlap window so an old-app device is never un-addable. Never a flag day.
  (§3, §8.)

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
2. **The MLS-specific PQ ciphersuite drafts** (`draft-ietf-mls-*` PQ work) — these
   register new MLS `Ciphersuite` code points that carry a hybrid KEM. This is the
   *right* long-term target because it means an interoperable, standard code point
   rather than a Pollis-private suite.

**Choice: target the standard MLS PQ ciphersuite code point, instantiated over an
X-Wing-style KEM.** ML-KEM-768 (NIST security category 3, ≈AES-192-equivalent
classical, comfortably PQ-adequate) is the sweet spot the drafts, Signal, and iMessage
all landed on — not the smaller -512 (category 1) nor the larger -1024. Pairing it
with X25519 keeps the classical floor at the 128-bit level the rest of Pollis's suite
already sits at (whitepaper §6.1).

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

### 2.3 The symmetric primitives are fine as-is

AES-128-GCM (application messages) and AES-256-GCM (SQLCipher, attachments) are
symmetric. Grover's algorithm gives at most a quadratic speedup, i.e. AES-128 → 64-bit
*quantum* work factor and AES-256 → 128-bit. AES-256 is unambiguously PQ-safe.
AES-128 inside the MLS suite is the MTI level and matches every other 128-bit
primitive in the suite (whitepaper §6.1); it is *weakened* but not *broken* by Grover,
and the realistic cost of 2⁶⁴ *sequential-depth-bounded* quantum operations is far
beyond any near-term CRQC. **We do not change the AEAD in Phase 1.** A later hardening
pass could move application messages to AES-256-GCM by selecting an
`..._AES256GCM_...` variant of the hybrid suite, but that is orthogonal to HNDL and
not on this critical path.

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
    Ciphersuite::<the MLS PQ hybrid code point>;                    // after openmls bump
```

Every call site that currently hard-codes `CS` (`key_packages.rs`, `device.rs`,
`group_state.rs`) becomes suite-aware. The device keeps a **stable signing key per
suite** — note `load_or_create_device_signer` keys the signer on
`CS.signature_algorithm()` (`device.rs:76-90`); since Phase 1 keeps Ed25519 for both
suites, the *same* signing key and the *same* `device_cert` cover both suites' leaves.
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
(`pollis-delivery/src/commit.rs:147`, in `submit_commit`, `commit.rs:137`) — so it would
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
- **When the last laggard updates**, the next natural touch of the group migrates it.
  If the group is idle, a low-priority scheduled self-update-style migration commit
  moves it (bounded, so idle groups still heal — this also closes the "no periodic
  self-update" gap noted in whitepaper §6.7 as a side benefit).

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
  (`commit.rs:147`) widens from `(conversation_id, epoch)` to
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

Implications, mapped to Pollis's actual payloads:

- **KeyPackage size:** a classic KP is a few hundred bytes; a hybrid KP grows by
  ~1.2 KB for the init key, landing at ~1.5–2 KB. With TARGET=5 packages per suite per
  device (`key_packages.rs:94`), a hybrid-capable device stores/publishes ~10 KB of
  hybrid KP material in `mls_key_package` (base64-inflated ~33% over the DS wire,
  `key_packages.rs:26-37`). Small in absolute terms.
- **Welcome size:** a Welcome HPKE-seals the group secrets to each joiner's init key.
  The per-recipient ciphertext grows ~1.1 KB. `mls_welcome.welcome_data`
  (`000000_baseline.sql:128`) and the DS `/v1/...` welcome writes carry it. For a
  Welcome to N joiners this is ~1.1 KB × N extra — the one place size scales with
  membership.
- **Commit size:** the expensive case. A TreeKEM commit that updates a path encrypts a
  path secret to each affected subtree's HPKE key. In a group of N members a full path
  update is O(log N) HPKE ciphertexts, each ~1.1 KB larger under hybrid. So a commit in
  a large group grows by roughly `1.1 KB × log2(N)` — e.g. ~11 KB extra for a 1000-member
  group's full-path commit. `mls_commit_log.commit_data` (`000000_baseline.sql:111`) and
  the DS `submit_commit` write carry this. **This is the number to watch**, because
  membership churn commits are the hot path and the ratchet-tree extension
  (`use_ratchet_tree_extension(true)`, set at `group_state.rs:508`) already inlines the
  full tree into GroupInfo/Welcome.
- **Turso / DS payloads:** all of the above are base64-encoded in JSON bodies through
  the DS (`ds_client.rs`), a ~33% inflation on already-larger blobs, then stored as
  BLOBs in Turso. The bandwidth and storage cost is real but bounded — kilobytes, not
  megabytes — and dwarfed by any attachment (R2, whitepaper §9). It is *not* dwarfed by
  a plain text message, so the *relative* overhead of a small text-only group's commits
  goes up noticeably.

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
- **Keep the AEAD at 128 and signatures classical in Phase 1** (§2.3, §2.4) so we do
  *not* stack ML-DSA's multi-KB-per-signature cost on top of the KEM cost. That single
  decision is the biggest lever keeping commit/KeyPackage sizes down.
- **KP pool size stays at 5 per suite** — resist inflating it; replenishment already
  tops up after each Welcome (`key_packages.rs:141`).
- **Measure in-box** (§5): the marathon fuzzer already exercises large, churny groups
  headless; add commit/Welcome byte-size assertions so a regression that balloons
  payloads is caught before it ships.

Net: hybrid MLS costs Pollis **kilobytes per commit/Welcome and microseconds per op**.
That is a defensible price for post-quantum confidentiality, and it does not threaten
the "fast" positioning as long as we hold the line on signatures + AEAD.

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

**Honest bottom line: the installed OpenMLS lacks any PQ ciphersuite. This needs an
upstream version bump (or a pinned fork), not just Pollis-side code.**

What the tree has today:

- `openmls = "0.8"` → locked `openmls 0.8.1` (`Cargo.toml:110`, `Cargo.lock:5391`).
  The `Ciphersuite` enum in 0.8.1 is the seven classical RFC 9420 suites; **no X-Wing /
  ML-KEM MLS code point exists in this release.** Pollis uses exactly one:
  `MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519` (`provider.rs:57`).
- `openmls_rust_crypto = "0.5"` → locked `0.5.1`, whose HPKE backend is
  `hpke-rs-rust-crypto` (`Cargo.lock:5443-5445`) — **classical only** (aes-gcm,
  chacha, k256, p256; no ML-KEM path reachable).
- **The ML-KEM primitives are already compiled but unreachable.** `hpke-rs 0.6.1`
  declares all three backends as dependencies, so `hpke-rs-libcrux 0.6.1` and
  `libcrux-ml-kem 0.0.8` sit in `Cargo.lock` (`Cargo.lock:3090-3133, 4042-4055`) — but
  the *provider Pollis instantiates* (`RustCrypto`, `provider.rs:38-52`) never selects
  the libcrux/X-Wing backend. So the crates being present is a red herring for
  reachability; it is, however, encouraging for the build (the toolchain already
  compiles ML-KEM cleanly, including in the headless box).

What is needed, in order of preference:

1. **Upstream bump (preferred).** Move to an OpenMLS release that (a) registers the MLS
   PQ/X-Wing ciphersuite code point in its `Ciphersuite` enum and (b) ships (or is
   compatible with) an `openmls_libcrux_crypto`-style provider that routes that suite's
   HPKE through `hpke-rs-libcrux` / `libcrux-kem`. As of this writing OpenMLS PQ support
   is *in flight* upstream, tracking the still-evolving MLS PQ ciphersuite drafts — so
   the exact target version is a **Phase 0 spike deliverable**, not a number we can pin
   today. The bump itself is non-trivial: 0.8 → a newer major may move the
   `openmls_traits` storage API (Pollis implements `StorageProvider` in
   `signal/mls_storage.rs`, whitepaper §6.1) and the provider trait
   (`OpenMlsProvider`, `provider.rs:37`). Budget real integration effort for the bump
   independent of the PQ work.
2. **Pinned fork / patch (fallback).** If upstream has not yet released a stable PQ
   suite when we want to ship, fork OpenMLS at our current major, backport the X-Wing
   ciphersuite registration + a libcrux-backed provider, and pin via a
   `[patch.crates-io]`. Higher maintenance burden (we own the fork until upstream
   catches up), and it means shipping a *Pollis-private* code point that only
   interoperates with other Pollis clients — acceptable, since Pollis is a closed fleet
   (one binary, one DS), but it forfeits standards interop and must be reconciled with
   the real code point later. Prefer (1); use (2) only if timeline forces it.
3. **Wait (explicit non-choice, stated for completeness).** Given HNDL's "act now"
   logic (§1.3), waiting is the option we are arguing *against*. But it is the correct
   choice if the Phase 0 spike finds the upstream suite too unstable to depend on
   (draft still churning code points) — in which case we ship the *scaffolding*
   (dual-suite plumbing, schema, tests) now and flip the hybrid suite on the moment
   upstream stabilises. That de-risks the timeline without betting on an unstable draft.

In-tree vs. upstream split:

- **Upstream (out of our control):** the ciphersuite code point + libcrux provider in
  OpenMLS. This is the dependency gate.
- **In-tree (our work):** dual-suite constants + suite-aware call sites
  (`provider.rs`, `key_packages.rs`, `device.rs`, `group_state.rs`, `reconcile.rs`);
  the additive migrations (§3.4); the Front-A/B/C rollout logic and the §3.3 interop
  gate; the DS claim-by-suite parameter (`ds_client.rs`); and the S1–S6 harness tests.
  All of this can be *built and tested against the classic suite today* (treating
  `CS_HYBRID` as an alias of `CS_CLASSIC` until the real suite lands), so we make
  progress in-tree *before* the upstream gate clears — the scaffolding is the long pole
  and it does not block on OpenMLS.

---

## 8. Phased roadmap, acceptance criteria, dependencies

**Phase 0 — Spike: pin the upstream target.** Determine the exact OpenMLS
version/provider that ships an MLS hybrid (X-Wing / ML-KEM-768) ciphersuite, or confirm
a fork is required. Prototype a single hybrid group in isolation (two headless clients)
to confirm the libcrux provider builds and runs *in the box* (`--no-default-features`,
headless). **Acceptance:** a throwaway binary creates a hybrid group and round-trips one
message between two clients headless; a written decision recorded: bump vs. fork vs.
scaffold-and-wait, with the pinned version or fork ref.
**Dependency:** upstream OpenMLS PQ status. This phase is the gate for everything else.

**Phase 1 — In-tree scaffolding (no behaviour change).** Introduce `CS_CLASSIC` /
`CS_HYBRID` (aliased to classic until Phase 0 clears), make every `CS` call site
suite-aware, add the additive migrations (§3.4), and the DS claim-by-suite parameter —
all defaulting to classic so runtime behaviour is byte-identical to today.
**Acceptance:** full existing `flows` suite + marathon fuzzer pass unchanged, headless,
on `ci/mls-test-gate`; a schema diff shows only additive migrations; no group's observed
suite changes.

**Phase 2 — Hybrid key packages (Front B).** Hybrid-capable devices publish both KP
pools; DS claims by suite; `pq_capable` set. Still no group goes hybrid.
**Acceptance:** S3's first half (mixed fleet, no migration) passes; a device advertises
both pools; claiming a classic KP for an old-app add still works; KP-size assertions
within budget (§4.3).

**Phase 3 — New groups hybrid (Front A).** New groups whose full invited roster is
hybrid-capable are created on `CS_HYBRID`; old-app-inclusive rosters stay classic.
**Acceptance:** S1 passes; an old app invited into a mixed roster gets a classic group
and reads every message; voice-key export works on hybrid groups.

**Phase 4 — Migrate existing groups (Front C).** The epoch-boundary suite-transition
commit, gated by the §3.3 "every member hybrid-capable" rule, plus the bounded
scheduled migration for idle groups.
**Acceptance:** S2, S4, S5, S6 all pass headless; the fuzzer's convergence invariant
holds across the suite boundary with zero dropped/undecryptable messages for any
throughout-member; payload-size budget holds under churn.

**Phase 5 — Fleet completion & (optional) hardening.** Once telemetry/heuristics show
the fleet is effectively all-hybrid, consider retiring classic KP publication (a
multi-release additive→drop dance, per CLAUDE.md) and, separately, evaluate AES-256 and
ML-DSA as *distinct future tracks* (§2.3, §2.4) — not on this HNDL critical path.
**Acceptance:** a defined, measured fleet-hybrid threshold before any classic retirement
begins; no classic drop lands until an app that stopped *needing* classic has full
uptake.

**Cross-cutting acceptance invariants (all phases):**
- No migration is ever a rename/drop/tightening (CLAUDE.md).
- No message sent to a throughout-member is ever dropped or rendered undecryptable
  ("messages must work").
- A hybrid group can *never* contain a member lacking a hybrid KP (invalid state
  unrepresentable, enforced at the claim/reconcile chokepoint).
- Everything builds and gates **headless in-box**.

**Dependencies summary:** (1) OpenMLS PQ ciphersuite upstream — the hard gate;
(2) libcrux-backed provider building headless (already plausible — crates compile in
tree); (3) the OpenMLS storage/provider API surface stability across the version bump;
(4) DS write endpoints extended for suite-tagged KP claim/publish; (5) a coordination
point with the machine-checked-correctness program, which lands before this work in
the program sequence: its M4 TLA+ spec (Gapless ∧ HeadMonotone per conversation,
`docs/machine-checked-correctness-design.md`) and the transparency-log verifier both
enforce per-conversation epoch monotonicity, and both must be extended from
`(conversation, epoch)` to `(conversation, generation, epoch)` (§3.2, §3.4) before the
P4 migration ships.

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
classical for now. **Feasibility caveat:** the pinned `openmls 0.8.1` /
`openmls_rust_crypto 0.5.1` (`Cargo.lock:5391,5434`) has **no PQ ciphersuite**; the
ML-KEM crates (`libcrux-ml-kem 0.0.8`) are already compiled but unreachable through
Pollis's provider. This requires an OpenMLS version bump or a pinned fork.

**Approach:** dual-suite, epoch-boundaried, never a flag day. New groups go hybrid once
the roster is capable; existing groups migrate at a clean commit boundary into a hybrid
successor group; every device publishes classic **and** hybrid key packages during a
long overlap so no old-app peer is ever un-addable; a group migrates only once *every*
member advertises a hybrid KeyPackage (invalid states unrepresentable). The successor
group's epoch lineage is tracked by an additive `mls_commit_log.generation` column, so
the DS monotone-head rule widens to `(conversation, generation, epoch)` and epoch
monotonicity is preserved — never an epoch reset under the old key. Schema changes
are additive only (suite-tag `mls_key_package`, `mls_commit_log.generation`, optional
`user_device.pq_capable`). Signatures + AEAD unchanged in Phase 1.

**Phased milestones:**
- **P0 — Spike:** pin the OpenMLS version/provider that ships X-Wing/ML-KEM-768, or
  decide fork; prove one hybrid group round-trips headless in-box.
- **P1 — Scaffolding:** suite-aware call sites + additive migrations + DS claim-by-suite,
  aliased to classic (zero behaviour change).
- **P2 — Hybrid key packages** (Front B): dual KP pools, claim-by-suite.
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

**Dependencies / risks:** OpenMLS PQ ciphersuite upstream (hard gate; version TBD in
P0); OpenMLS storage/provider API churn across the version bump (`signal/mls_storage.rs`,
`provider.rs`); libcrux provider building headless (`--no-default-features`).
