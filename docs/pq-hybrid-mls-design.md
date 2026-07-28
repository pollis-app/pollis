# Post-Quantum Hybrid MLS — Design

**Status: COMPLETE — P0 through P5 all shipped (§8); §2.4 has since been SUPERSEDED by
#668, and the dual-suite half of §3 by #669.** The ciphersuite is still an explicit
argument to the two functions that mint suite-bound material, and an existing group still
moves suite by successor generation. What is gone is the *second* suite: every device
publishes one key-package pool and every new group is born on `CS_PQ`, with no gate to
pass. Three things this document argued for have since been overtaken; all are corrected
in place below rather than deleted, because the reasoning is a record:

- **The hybrid suite moved, in place.** #454 shipped `CS_HYBRID` as X-Wing / `0x004D`,
  whose leaves still signed **Ed25519** — post-quantum in confidentiality only. #668 moved
  the constant to `MLS_128_MLKEM768X25519_CHACHA20POLY1305_SHA384_MLDSA44` (`0x0052`),
  which keeps *exactly the same* X-Wing KEM — so nothing in the HNDL argument below changes
  — and replaces the leaf signature with **ML-DSA-44** (`SignatureScheme::MLDSA44`,
  `0x0904`). The KDF moves SHA-256 → SHA-384 with the code point. §2.4 records why
  signatures were deferred and why that recommendation was then overturned.
- **The second crypto backend is gone.** #454 needed suite→provider *routing* (§7.3)
  because only `openmls_libcrux_crypto` implemented `0x004D`. `0x0052` is implemented by
  `openmls_rust_crypto`, and libcrux implements no ML-DSA suite at all, so **one backend
  serves both suites** and there is nothing left to route away from
  (`pollis-core/src/commands/mls/provider.rs`).
- **The dual-suite model is gone; the migration machinery is not.** #669 retired the
  classic suite `MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519` (`0x0001`) outright.
  `CS_CLASSIC` is deleted, `CS_HYBRID` is renamed **`CS_PQ`**, devices publish one
  key-package pool, `init_mls_group` always births on `CS_PQ`, and the whole
  suite-*selection* apparatus this document designed — `suite_for_new_group`, the roster
  and fleet `pq_capable` gates, `FLEET_DORMANCY_DAYS`, the `may_birth_hybrid` predicate
  and its I7 Kani harnesses — is deleted with it. §3.2's Front C survives, generalised:
  `migrate_to_hybrid_if_due` is renamed **`migrate_to_current_suite_if_due`** and triggers
  on `group.ciphersuite() != CS_PQ` rather than `== CS_CLASSIC`, because `0x0052` is a
  provisional code point (§7.4) and the next renumber is this same migration with a
  different constant. See §3.3 for why the gates were sound and what dissolved the premise
  they rested on.

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

- **The HNDL-exposed asset is the MLS key exchange (HPKE/DHKEM), not the signatures.**
  Harvest-now-decrypt-later (HNDL) breaks *confidentiality* by recording ciphertext
  and the KEM material that seals it, then recovering the shared secret once a
  cryptographically-relevant quantum computer (CRQC) exists. Signatures do **not**
  need PQ for HNDL — a forged signature in 2035 cannot retroactively decrypt a 2026
  message. So this design goes hybrid **only on the KEM** and deferred signatures.
  That deferral was right about HNDL and wrong about the *other* reason to move
  signatures early — long-lived verifiability — and #668 has since executed the move
  to ML-DSA-44 anyway. (§1, §2.4.)

- **Go hybrid, not PQ-only:** X25519 **+** ML-KEM-768, combined so the session key
  is secure if *either* primitive holds. This is the IETF/NIST-endorsed posture and
  it protects us against both a future CRQC *and* a not-yet-discovered flaw in the
  young ML-KEM implementation. The concrete instantiation is **X-Wing**
  (`draft-connolly-cfrg-xwing-kem`) as the DHKEM — shipped by #454 under the experimental
  OpenMLS code point `0x004D`, and carried unchanged into `0x0052` by #668 when the
  signature moved (§7). (§2.)

- **Feasibility, honestly: #454's KEM needed no version bump; #668's signatures needed a
  git pin.** The X-Wing suite `MLS_256_XWING_CHACHA20POLY1305_SHA256_Ed25519` (`0x004D`)
  already existed, un-feature-gated, in `openmls_traits 0.5.0` (added upstream in
  openmls/openmls#1546, 2024-04); the only missing piece was the **crypto provider**, and
  `openmls_libcrux_crypto 0.3.1` supplied it. The PQ *signature* suites exist in **no
  released version** — the released 0.8.1 has no `SignatureScheme::MLDSA44` at all — so
  all five openmls crates are now pinned to an upstream `main` rev through one workspace
  `[patch.crates-io]` (`Cargo.toml`), with `draft-ietf-mls-pq-ciphersuites` named on every
  direct dependency. The AEAD still changes *with* the KEM (AES-128-GCM →
  ChaCha20-Poly1305, nominal level 128 → 256), and since #668 the signature changes with
  it too (Ed25519 → ML-DSA-44). (§7.)

- **Recommendation: do it, in phases, gated behind the box's headless MLS harness.
  All phases are now DONE (§8)** — new groups are born hybrid, existing groups migrate,
  and key packages published in **both** suites so an old-app device was never un-addable
  during the overlap (#669 has since ended the overlap and dropped the classic pool).
  Never a flag day. Two things landed differently from the sketch here and are worth
  reading in §8: migration is *not* a ciphersuite-transition commit at an epoch boundary
  (RFC 9420 has no such commit) but a **successor group** under a generation counter; and
  the crypto-provider change landed as a **route**, not a swap, because putting classic
  traffic on libcrux would have been a security regression (§7.3) — a route that #668
  then dissolved by moving to a suite RustCrypto implements.
  The live risk is no longer availability but **code-point churn** — `0x0052` is a
  provisional `draft-ietf-mls-pq-ciphersuites` point, and its predecessor `0x004D` was
  dropped by the same draft one revision earlier; pin the openmls rev and budget
  for a further suite migration when the IETF suites stabilise (§7). (§3, §8.)

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
| **Account identity + device signatures** | ML-DSA-44 since #668 (`account_identity.rs`, `verify_device_cert` in `pollis-device-cert`); Ed25519 before it, and Ed25519 on classic-suite leaves until #669 retired that suite | **NO (for HNDL)** | A signature authenticates; it does not seal. A future CRQC that forges Ed25519 enables *active* impersonation *going forward*, but cannot retroactively decrypt anything. This row is why signatures were deferred — and, per §2.4, why "HNDL-exposed?" turned out to be the wrong question for the *long-lived-verifiability* assets (account keys, device certs, the transparency log). |
| **PIN-wrapped local keys** | Argon2id + XChaCha20-Poly1305 (whitepaper §3) | **NO** | Symmetric; local; never on-wire. Not harvestable. |
| **SQLCipher DB, attachment convergent encryption** | AES-256-GCM (whitepaper §7, §9) | **NO (practically)** | Symmetric AES-256; Grover halves the security level to 128 bits, which is fine. Not asymmetric, not Shor-breakable. |
| **TURSO_TOKEN / R2 / LiveKit transport (TLS)** | TLS 1.3 | out of scope | Transport HNDL is the platform's problem, not the MLS protocol's. Noted, not addressed here. |

**Conclusion:** the crown jewel is the **HPKE/DHKEM key agreement inside MLS.** That —
and only that — is what this design makes hybrid. Everything symmetric is already at
or above the post-quantum-adequate 128-bit floor; everything signature-shaped is not
an HNDL asset. (Signatures moved to ML-DSA-44 later, under #668, for a *different*
threat — see §2.4.)

### 1.3 Why act now (the timeline)

- **NIST finalised the PQ KEM standard (FIPS 203, ML-KEM) in August 2024.** ML-KEM is
  no longer a research artifact; it is the standard. The tooling to adopt it exists
  (it is *literally in our Cargo.lock* — `libcrux-ml-kem`).
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
init keys, and Welcome sealing. When this was written Pollis used **DHKEM(X25519,
HKDF-SHA256)** (RFC 9180), embedded in the `CS_CLASSIC` suite constant in `provider.rs`
— the suite #669 has since retired. The hybrid target
replaces that KEM with a **combiner** that runs *both* X25519 and ML-KEM-768 and
mixes their outputs so the resulting shared secret is secure if **either** component
is secure:

```
ss = KDF( X25519_ss || MLKEM_ss || transcript_binding )
```

Two concrete instantiations exist, and they are not mutually exclusive:

1. **X-Wing** (`draft-connolly-cfrg-xwing-kem`) — a *general-purpose* hybrid KEM
   pairing X25519 + ML-KEM-768 with a fixed, security-proven combiner. This is what
   `hpke-rs-libcrux` / `libcrux-kem` already implement (both crates are in our lock,
   pulled in by `openmls_rust_crypto` → `hpke-rs`). The MLS PQ ciphersuite drafts build
   their DHKEM on X-Wing-shaped constructions.
2. **The MLS-specific PQ ciphersuite drafts** (`draft-ietf-mls-pq-ciphersuites`) — these
   register new MLS `Ciphersuite` code points that carry a hybrid KEM. This is the
   *right* long-term target because it means an interoperable, standard code point
   rather than an experimental one. Note the drafts are still moving: the suite #454
   shipped (`0x004D`, X-Wing) is a 2024 experimental point that draft-06 (2026-07)
   *dropped* in favour of new code points, so "standard" here is a moving target we
   track, not a settled destination (§7).

**Choice as shipped: X-Wing, first under `0x004D` (#454), then under `0x0052` (#668).**
ML-KEM-768 (NIST security category 3, ≈AES-192-equivalent classical, comfortably
PQ-adequate) is the sweet spot the drafts, Signal, and iMessage all landed on — not the
smaller -512 (category 1) nor the larger -1024. Pairing it with X25519 keeps the classical
floor at the 128-bit level the rest of Pollis's suite already sits at (whitepaper §6.1).
The move to `0x0052` — the draft's own point, spelled `MLKEM768X25519` rather than
`XWING` — changed the *signature* (§2.4) and the KDF (SHA-256 → SHA-384), **not** the KEM:
`0x0052` is the same `HpkeKemType::XWingKemDraft6` under a draft-registered code point, so
everything §2.1–§2.2 argues about confidentiality carries over unmodified. That it also
moved Pollis onto a draft code point rather than an OpenMLS-private one is a side benefit,
not the reason.

### 2.2 Why hybrid and not PQ-only

Two independent reasons, both of which matter for a security product:

1. **Defence in depth against a young primitive.** ML-KEM's implementations are new.
   `libcrux-ml-kem` was at version **0.0.8** when #454 shipped and is at **0.0.10**
   after #668 — a pre-1.0, rapidly-moving crate whose version we do not control
   directly (it arrives transitively under `hpke-rs-libcrux` ← `libcrux-kem`, so the
   openmls pin of §7.1 moves it). A hybrid construction means an implementation bug or a
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
suite is not AEAD-neutral.** Every hybrid suite on offer is a ChaCha20-Poly1305 suite —
`MLS_256_XWING_CHACHA20POLY1305_SHA256_Ed25519` when #454 shipped, and
`MLS_128_MLKEM768X25519_CHACHA20POLY1305_SHA384_MLDSA44` since #668 — there is no AES-GCM
hybrid variant to select, so moving a group to hybrid *bundles* the AEAD change
AES-128-GCM → ChaCha20-Poly1305 with the KEM change. This is not a regression:
ChaCha20-Poly1305 is a 256-bit-key AEAD, at least as strong as AES-128-GCM against both
classical and Grover attacks, and it is the AEAD Signal and WireGuard already lean on.
What we *cannot* claim is "AEAD unchanged" — classic groups kept AES-128-GCM, hybrid
groups run ChaCha20-Poly1305, and there is no knob to decouple the AEAD from the KEM.
(Since #669 retired the classic suite there are no AES-128-GCM MLS groups left to be
born; the AES-256-GCM uses above — SQLCipher and attachments — are unaffected, as they
were never suite-bound.)
(Nor can we claim signatures are unchanged any more: #668 bundled ML-DSA-44 into the same
code point — §2.4. The suite's nominal security-level *label* moved 256 → 128 with that
move, which is a labelling artifact of the draft's naming, not a downgrade: the KEM is
byte-for-byte the same X-Wing and the AEAD is the same ChaCha20-Poly1305.)

### 2.4 Signatures: deferred here, then SUPERSEDED by #668

> **Status: this section's recommendation no longer holds.** #454 deferred post-quantum
> signatures for the reasons below, all of which remain sound *as far as they go*. #668
> then moved authentication to **ML-DSA-44** (FIPS 204) anyway, because the deferral's
> central argument does not cover the assets that must stay verifiable for decades. Both
> halves are kept: the reasoning first, the supersession after.

#### 2.4.1 The original argument (as written for #454)

Ed25519 appears in three places: the MLS leaf signature (part of the ciphersuite,
`provider.rs`), the per-device MLS signing key (`user_device.mls_signature_pub`,
`000000_baseline.sql:156`), and the account-identity cross-signing cert
(`account_identity.rs`). The MLS-suite drafts pair the hybrid KEM with a signature that
may be classical **or** ML-DSA (Dilithium).

**Recommendation: keep signatures classical (Ed25519) in Phase 1–3. Treat ML-DSA as a
separate, later track.** Reasoning:

- **HNDL does not threaten signatures.** A signature proves authenticity *at
  verification time*. A CRQC that can forge Ed25519 in 2035 lets an attacker
  impersonate a device *in 2035* — an active attack we would detect and could respond
  to by rotating to PQ signatures *then*. It cannot retroactively decrypt a single
  2026 message. So the *urgency* that drives the KEM change simply is not present for
  signatures.
- **ML-DSA is expensive and everywhere.** An ML-DSA-44 public key is **1312 B** and a
  signature **2420 B**, versus 32 B / 64 B for Ed25519. Signatures sit on **every** leaf
  node, **every** KeyPackage, and **every** commit. Making signatures PQ would inflate
  KeyPackages and commits far more than the KEM change does, and it would touch the
  cross-signing cert format (`device_cert_signed_payload`), the account-key transparency
  log leaf shape (`account_key_log`, `000005_account_key_log.sql`), and the DS
  request-signing credential (`ds_client.rs` signs with `mls_signature_pub`). That is a
  much larger, higher-risk blast radius for a threat that is *not* HNDL.
- **The honest framing:** signatures are a **store-now-forge-later** concern, not a
  harvest-now-*decrypt*-later concern, and the response to the former can be reactive
  (rotate when a CRQC is imminent) rather than pre-emptive. We design the schema so
  ML-DSA *can* be added later (§3.4 makes the key-material columns
  algorithm-tagged), but we do not pay its cost now.

One caveat we state loudly (§6): keeping signatures classical means *this* migration does
**not** make Pollis "fully post-quantum." It makes the *confidentiality* post-quantum,
which is the part HNDL attacks. That distinction was the honest headline for #454.

#### 2.4.2 Why #668 superseded it

The deferral rests on one load-bearing sentence: *a signature proves authenticity at
verification time*, so it can be rotated reactively. That is true of a signature whose
verification happens **once, now** — an MLS commit is validated as it is applied and never
re-validated years later, and a stale forged commit buys an attacker nothing.

It is **false** of every signature Pollis expects to be re-checked indefinitely, and Pollis
has three of those:

| Long-lived signature | Why "verify once, now" fails | Moved to |
|---|---|---|
| **Account identity key** (`account_identity.rs`) | It is the root of the device-cert chain and is *the* answer to "is this really that user's device?". It is re-verified on every new device, every enrollment, every relay handshake, for the life of the account. A forged account key is a permanent identity takeover, not a bounded window. | ML-DSA-44 |
| **Device cert** (`pollis-device-cert`) | It is verified **offline, with zero I/O**, by parties that hold no database — notably the relay tier, deliberately kept out of the Turso metadata plane. There is no revocation lookup on that path to make a reactive rotation land. | ML-DSA-44, cert **v2** |
| **Transparency-log STHs** (`verifiable-log`) | This is the decisive one. The whole product claim of the log is that its signed statements about **history** stay checkable *forever* — an auditor in 2035 re-verifying a 2026 STH is the intended use, not an edge case. A CRQC that can forge Ed25519 in 2035 can mint a consistent-looking 2026 head and destroy exactly the non-repudiation the log exists to provide. Here a forgery **is** retroactive. | ML-DSA-44, contexts bumped to `sth:v2` / `sth:v2:account-keys` / `sth:v2:binaries` |

So the correct decomposition is not "KEM = urgent, signatures = reactive" but
**"confidentiality of traffic = urgent (HNDL); authenticity of *history* = urgent
(retroactive forgery); authenticity of *live protocol steps* = genuinely reactive."** #454
got the first right and collapsed the second into the third. #668 splits them.

The MLS leaf signature itself is in the third, genuinely-reactive bucket — it moved with
`CS_HYBRID` because the ciphersuite bundles KEM, AEAD and signature into one code point
and there is no draft suite that pairs a PQ KEM with Ed25519. That is a consequence of the
suite move, not an independent argument.

The cost side of the original argument was accurate and was simply **paid**, not avoided
(measured numbers in §4.1): a hybrid KeyPackage went 2,659 → 8,670 B, roughly tripling.
What made that affordable is that the two things the deferral warned about had already
been fixed by the time it landed — periodic self-update (#666) keeps leaves merged, so
per-commit cost is logarithmic in N rather than linear, and the fleet-completeness gate
(§3.3) meant only PQ-capable devices ever sat in a PQ group. #669 has since deleted that
gate along with the classic suite; what replaces it is not a weaker guarantee but a
narrower one — every device is in a PQ group because there is no other kind, and a
migration that cannot claim a target-suite KeyPackage for every roster device aborts.

#### 2.4.3 What #668 changed, concretely

- **Suite, in place.** `CIPHERSUITE_HYBRID` moved from
  `MLS_256_XWING_CHACHA20POLY1305_SHA256_Ed25519` (`0x004D`) to
  `MLS_128_MLKEM768X25519_CHACHA20POLY1305_SHA384_MLDSA44` (**`0x0052`**), with
  `SignatureScheme::MLDSA44 = 0x0904`. Same X-Wing KEM; KDF SHA-256 → SHA-384.
- **Account identity key** is ML-DSA-44. The **private** key *is* its 32-byte seed, so
  the Secret-Key wrap, the PIN-wrapped keystore blob and the enrollment envelope are all
  byte-identical to the Ed25519 era — private-key custody did not change at all. Only the
  public half (32 → 1312 B) and the signature (64 → 2420 B) grew.
- **Per-device MLS signing keys are per signature *scheme*, not per device.** A device in
  groups of both suites holds **two** leaf keys at once — Ed25519 for classic-suite
  leaves, ML-DSA-44 for PQ-suite leaves (`device::load_or_create_device_signer` takes the
  scheme; `provider::signature_scheme(suite)` supplies it). New nullable Turso column
  `user_device.mls_signature_pub_pq` (migration `000011_device_pq_signature_pub.sql`)
  carries the PQ half; `mls_signature_pub` keeps the Ed25519 half unchanged.
- **Device cert v2** (`pollis-device-cert-v2\x00`) certifies **both** device leaf keys in
  one account-key signature, so no leaf is ever uncertified during the classic→PQ overlap.
  The public-key length prefixes in the signed payload widened `u8` → `u16` (an ML-DSA-44
  public key is 1312 bytes); `device_id` keeps its `u8` prefix.
- **DS request auth.** `X-Pollis-Signature` is now an ML-DSA-44 signature made with the
  device's PQ key and verified against `user_device.mls_signature_pub_pq`; the header grows
  88 → ~3228 base64 chars (§3.5).
- **Transparency-log STHs** are ML-DSA-44 under bumped domain-separated contexts
  `pollis-verifiable-log:sth:v2`, `…:sth:v2:account-keys`, `…:sth:v2:binaries`.

**Still Ed25519, deliberately:**

- ~~The **classic** MLS suite `MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519` (`0x0001`) and
  the per-device Ed25519 leaf key that signs into it — retiring those is **#669**.~~
  **Done: #669 retired the suite.** No live suite mints an Ed25519 leaf. The per-device
  Ed25519 key itself is *not* gone — it is still generated, published to
  `user_device.mls_signature_pub` and bound by the v2 device cert, because the scheme is
  what decides whether a stored key can verify a leaf persisted under an older code point.
- The **relay directory** signing key (`POLLIS_OVERLAY_DIRECTORY_KEY`,
  `pollis-core/src/net/directory.rs`). The signed directory artifact is produced by a Node
  Lambda and `node:crypto` has no ML-DSA, so this one cannot move until its signer can.
  (The relay *handshake* itself did move — the device signs it with its ML-DSA-44 key.)

With that, the §6 headline changes: authentication *is* post-quantum now, on every path
except the two listed above. §6 states the new scope.

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

> **Superseded by #669 — one suite, and the constant it left behind.** The dual-suite
> model below was the whole point of this section and it did its job; it is recorded, not
> deleted, because the *shape* it left is what remains. As shipped after #669:
>
> ```rust
> // provider.rs — as shipped, post-#669
> pub(crate) const CS_PQ: Ciphersuite =
>     Ciphersuite::MLS_128_MLKEM768X25519_CHACHA20POLY1305_SHA384_MLDSA44;   // 0x0052 (§7)
>
> /// The scheme a suite's leaves sign with. Still a function of the suite, not a
> /// constant: a group persisted under an older code point must be read under its
> /// own scheme.
> pub(crate) fn signature_scheme(suite: Ciphersuite) -> SignatureScheme {
>     suite.signature_algorithm()
> }
> ```
>
> `CS_CLASSIC` is deleted and `CS_HYBRID` is renamed `CS_PQ`. What survives is the
> *parametrisation* — the suite is still an argument to `create_mls_group_in_suite` and
> `build_key_package_in_suite`, and `signature_scheme` is still a function rather than a
> constant — because that is what makes the next code-point move (§7.4) a one-constant
> edit instead of the rewrite this section describes.

Introduce a second ciphersuite constant alongside the existing one, rather than
replacing it:

```rust
// provider.rs — as shipped under #454, post-#668, pre-#669
pub(crate) const CS_CLASSIC: Ciphersuite =
    Ciphersuite::MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519;              // 0x0001
pub(crate) const CS_HYBRID: Ciphersuite =
    Ciphersuite::MLS_128_MLKEM768X25519_CHACHA20POLY1305_SHA384_MLDSA44;    // 0x0052 (§7)

/// The scheme a suite's leaves sign with: Ed25519 for classic, MLDSA44 for hybrid.
pub(crate) fn signature_scheme(suite: Ciphersuite) -> SignatureScheme {
    suite.signature_algorithm()
}
```

Every call site that previously hard-coded `CS` (`key_packages.rs`, `device.rs`,
`group_state.rs`) becomes suite-aware. **The device keeps a stable signing key per
signature *scheme*.** Under #454 that distinction was invisible — both suites signed
Ed25519, so the nine signature-key-gen sites read one `SIGNATURE_SCHEME` constant and one
signing key plus one `device_cert` covered every leaf (§7.5). #668 ended that: `CS_CLASSIC`
leaves sign Ed25519, `CS_HYBRID` leaves sign ML-DSA-44, and a device in groups of both
suites holds **both** keys at once — same `mls_kv` scope, one row per scheme, with the
un-suffixed pre-#668 row kept in step for Ed25519 so a downgrade to a v1.7.0 build still
finds the key its existing groups are signed with. Keying on the *scheme* rather than the
suite is deliberate: it is the scheme that decides whether a stored key can verify a given
leaf, and two suites sharing a scheme should share a key.

So the "cross-signing, DS auth and the transparency log are all unchanged by this work"
claim held for #454 and **does not hold now**: device certs went to v2 and certify both
leaf keys, the DS auth credential moved to the PQ key, and the transparency log's STHs
moved to ML-DSA-44 under `sth:v2` contexts. See §2.4.3, §3.4 and §3.5.

### 3.2 Rollout order (three fronts, staggered)

> **Where the three fronts stand after #669.** Front A has no staggering left: every new
> group is born on `CS_PQ` unconditionally, and the deployment-measurement question below
> ("enough of the fleet") no longer has to be answered. Front B is one pool, not two — the
> classic pool is gone, and with it the mixed-fleet claim logic it existed to serve. Front
> C is the part that survives on its own merits, generalised from "classic → hybrid" to
> "anything that is not `CS_PQ` → `CS_PQ`"; everything it says below about generations,
> the CAS and the epoch-0 accept rule is current.

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

> **SUPERSEDED by #669 — the premise, not the reasoning.** Everything below is a correct
> account of what a *mixed* fleet requires, and it shipped and worked. #669 removed the
> mixed fleet: the classic suite is retired, `CS_CLASSIC` is deleted, and there is no
> longer a classic-only device that a hybrid group could fail to admit. With that, the
> whole capability apparatus this section justified — `roster_is_fully_pq_capable`,
> `fleet_is_fully_pq_capable`, `FLEET_DORMANCY_DAYS`, `may_birth_hybrid` and its I7 Kani
> harnesses, and `devices.rs::mark_pq_capable` together with both writes to
> `user_device.pq_capable` — is deleted rather than left dormant.
>
> **Why a hard cutover was allowed.** #669's own issue text assumed retirement would have
> to wait out the 90-day fleet turnover this section describes. It does not, because the
> deployment has zero active users: there is no old app in the field to strand, so the
> gradual path had nothing left to protect. That is a property of *this* deployment at
> *this* moment, not a correction to the argument — a fleet with real users would still
> need every word below.
>
> **What the no-stranding guarantee rests on now.** The acceptance criterion is unchanged:
> a migrated group can never contain a member who could not follow it across. It is now
> enforced directly, by the step that claims a KeyPackage in the *target* suite for every
> roster device before anything is created and aborts on the first miss — move everyone or
> nobody. `pq_capable` was only ever a server-side *predictor* of that claim; the claim
> itself is the direct test, and it is the layer the "lowest useful layer" bullet below
> already names.

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
  migration is driven only by the cold-launch sweep (`migrate_to_hybrid_if_due`, renamed
  `migrate_to_current_suite_if_due` by #669, bounded
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
  **Post-#669:** the column stays and is still queried — every package now carries
  `0x0052`, and the DS's default for a request that omits the field moved from the
  classic point to `CIPHERSUITE_PQ`. With one suite it no longer separates two live
  pools, but it is what keeps a package published under a *retired* code point from
  being served for a current group, which is precisely the case §7.4 says to expect.
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
  **Post-#669 the column is dead**, and the sentence above is why removing it cost
  nothing: it was always a cheap *predictor* of the KP claim, never the gate. It is
  **retired in place, not dropped** — migrations must stay additive (CLAUDE.md), so the
  column remains with nothing reading or writing it, rather than being removed by a
  narrowing migration.
- **Signature columns were left untouched by the KEM change** (Ed25519 stayed), and the
  forward path this bullet sketched — "if ML-DSA is ever added, the key columns gain an
  algorithm tag the same additive way" — is exactly what #668 then did, one migration
  later: **`000011_device_pq_signature_pub.sql`** adds the single nullable column
  `user_device.mls_signature_pub_pq BLOB`. It is a *second column* rather than an
  algorithm tag on the existing one because `mls_signature_pub` is simultaneously the DS
  request-auth credential, read as a raw 32-byte Ed25519 key by every already-shipped
  client — overwriting it with a 1312-byte ML-DSA key would have broken authentication for
  the whole fleet the instant the migration ran. NULL means "this device predates #668":
  such a row is skipped by `resign_stale_device_certs`, treated as `AbsentRetry` by
  `verify_added_devices`, and self-heals the next time that device runs
  `ensure_device_cert`. The account-key transparency log (`account_key_log`,
  `000005_account_key_log.sql`) needed no migration either way — it records
  `account_id_pub` as an opaque blob, so the Ed25519 → ML-DSA-44 account key changes its
  *length*, not its shape.

No migration drops or narrows anything, so a pre-hybrid desktop app continues to run
against the migrated schema unmodified.

### 3.5 Device key packages during the window

`user_device.mls_signature_pub` (`000000_baseline.sql:156`) is the *signing* key, not
a KEM key, so **the KEM change does not touch it** and the DS-auth path in `ds_client.rs`
is unchanged *by this work* — whichever DS auth scheme is live (the device-header
signature, or the anonymous membership proofs of `docs/metadata-minimization-design.md`
v1.5), the KEM change does not touch it; the two programs are orthogonal.

**#668 did touch it, on its own terms.** The DS request credential moved from
`mls_signature_pub` to `mls_signature_pub_pq`: `X-Pollis-Signature` is now an ML-DSA-44
signature over the same canonical `METHOD | PATH | TIMESTAMP | HEX_SHA256_BODY` message,
verified against the raw 1312-byte PQ key (`pollis-delivery/src/auth.rs`). The header
grows **88 → ~3228 base64 chars** — the one genuinely user-invisible-but-measurable cost
of the change, paid on every DS write. The ordering that makes this safe is that
`publish-device-cert` is session/cert gated and **never** device-signature gated, so a
device always has a reachable path to register its PQ key *before* it needs to sign with
it. The KEM material lives entirely inside the
per-suite KeyPackages (`mls_key_package.key_package`), which is where the ML-KEM public
key rides. A device that is hybrid-capable simply maintains two KP pools; the
`replenish_key_packages` top-up logic (`key_packages.rs:141`) runs once per suite. On
login, `ensure_mls_key_package` rotates both pools; the classic pool is retained for the
entire overlap window so no old-app peer ever fails to add this device.

**#669 ended the window.** `PUBLISHED_SUITES` — the two-element array the paragraph above
describes — is gone. A device publishes and tops up exactly one pool, in `CS_PQ`, and
`replenish_key_packages` counts only that suite. Nothing else in this section changes: the
KEM material still rides inside the KeyPackage blob, and `mls_signature_pub_pq` is still
the DS request credential.

---

## 4. Performance

This is where the "fastest secure messenger" goal gets stressed, and where we must be
disciplined. The size delta is the dominant cost; the CPU delta is negligible.

### 4.1 Sizes: the real cost

**KEM primitives** (the #454 cost, unchanged by #668 — `0x0052` carries the same X-Wing):

| Item | X25519 (classic) | ML-KEM-768 | Hybrid (X25519 + ML-KEM-768) |
|---|---|---|---|
| KEM public key (in KeyPackage / leaf) | 32 B | 1184 B | ~1216 B |
| KEM ciphertext (per HPKE seal, in Welcome / commit path secret) | 32 B | 1088 B | ~1120 B |
| KEM private key (local only) | 32 B | 2400 B | ~2432 B |

**Signature primitives** (the #668 cost, added on top — §2.4):

| Item | Ed25519 | ML-DSA-44 |
|---|---|---|
| Public key (in every leaf, KeyPackage, device cert, account identity) | 32 B | **1312 B** |
| Signature (on every leaf, KeyPackage, commit, cert, STH, DS request) | 64 B | **2420 B** |
| Private key (local only) | 32 B seed | **32 B seed** — byte-identical custody |

The private-key row is the one to notice: an ML-DSA-44 private key *is* its 32-byte seed,
so every place Pollis stores or transports a private signing key — the Secret-Key wrap, the
PIN-wrapped keystore blob, the enrollment envelope, openmls's own `SignatureKeyPair` — is
unchanged in size and shape. All of the cost is on the public/verification side.

Implications, mapped to Pollis's actual payloads. The per-message sizes below are
**measured in-box**, `--release`, with `use_ratchet_tree_extension(true)` (as Pollis sets
at `group_state.rs:508`), a single committer, TLS-serialised bytes. **They are #454-era
numbers**, taken on the Ed25519-signed X-Wing suite (`0x004D`); §"the #668 signature cost"
below carries them forward:

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
  encapsulation), which landed the #454 KP at **2,659 B**. Since #668 it also carries a
  1312-byte ML-DSA-44 leaf key and **two** ML-DSA-44 signatures (the leaf node's and the
  KeyPackage's), taking it to **8,670 B** against a classic KP's **307 B**. With TARGET=5
  packages per suite per device (`key_packages.rs:94`), a hybrid-capable device
  stores/publishes ~43 KB of hybrid KP material in `mls_key_package` (base64-inflated ~33%
  over the DS wire, `key_packages.rs:26-37`). Still small in absolute terms, but no longer
  negligible next to the commit.
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
- **The #668 signature cost: roughly ×3 on top, and the shape is unchanged.** Re-measured
  after the move to ML-DSA-44 (`hybrid_payloads_stay_under_their_ceilings`, pollis-core's
  MLS unit suite): **KeyPackage 8,670 B** (classic 307 B, and 2,659 B on the #454 suite),
  **Welcome 15,241 B** at two members, and **14,839 B** for a commit into a merged
  8-member group. All three roughly tripled. Crucially the *growth shape* did not change —
  a signature is a per-leaf constant, not a per-copath-node cost, so
  `self_update_turns_linear_commit_growth_into_logarithmic` still passes on both suites and
  the "keep leaves merged" lever is still the one that matters. The absolute ceilings were
  re-pinned about 1.4× above each measurement (12,288 B KeyPackage / 21,504 B Welcome /
  20,480 B commit) — tight enough that a doubling fails the build, loose enough to survive
  openmls padding and code-point churn. **The 20,480 B commit ceiling is the number to
  watch now**, and `flows/pq_migration.rs`'s `MAX_HYBRID_COMMIT_BYTES` matches it exactly
  so the flows harness and the unit suite cannot drift.

### 4.2 Latency: negligible

ML-KEM-768 keygen/encaps/decaps run in **tens of microseconds** on desktop hardware — the
same order as X25519, and orders of magnitude below Argon2id's deliberate ~250 ms unlock
cost (whitepaper §3.1) or a single Turso round-trip. Hybrid KEM ops add one ML-KEM
operation alongside the existing X25519 one; the CPU cost is in the noise next to the
network. ML-DSA-44 sign/verify (#668) sit in the same regime — lattice arithmetic on
kilobyte-scale operands, not the exponentiation-bound work that would show up in a
round-trip budget. The MLS crypto already runs in the Rust core off the render thread
(`PollisProvider`/`RustCrypto`, `provider.rs`), consistent with the CLAUDE.md
"lean Rust for perf-critical paths" doctrine, so there is no GC/IPC penalty. **The cost of
both changes is size, not time** — and, since #668, the ~3.1 KB `X-Pollis-Signature`
header on every DS write (§3.5).

### 4.3 How we bound it

- ~~**Suite-scoped, not global:** classic groups keep classic (small) commits. Only
  hybrid groups pay the size cost, and only for the KEM-bearing fields.~~ **Gone with
  #669** — with one suite, every group pays the size cost, so this is no longer a lever
  the design holds. It still bounds *which fields* grow: only the KEM-bearing ones.
- ~~**Keep signatures classical in Phase 1** (§2.4) so we do *not* stack ML-DSA's
  multi-KB-per-signature cost — on *every* leaf and *every* commit — on top of the KEM
  cost. That single decision is the biggest lever keeping commit/KeyPackage sizes down.~~
  **Spent in #668.** This was the largest lever the design held, and it was deliberately
  cashed in for the verifiability argument in §2.4.2: ML-DSA-44's 1312 B key + 2420 B
  signature per leaf roughly tripled the KeyPackage, Welcome and commit (§4.1). It was
  affordable *because* the other levers had already landed — self-update keeps the growth
  logarithmic, and the fleet gate kept the cost off classic groups entirely (that second
  half expired with #669: there are no classic groups to keep it off). With it
  spent, "keep leaves merged" below is now the only structural lever left.
  (The AEAD is *not* a lever we hold either: the hybrid suite is ChaCha20-Poly1305 by
  construction — §2.3 — but that swap is roughly size-neutral versus AES-128-GCM, so it
  does not move the commit-size numbers; the KEM and the signature do.)
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

Net: hybrid MLS costs Pollis **microseconds per op** and, on the KEM change alone, roughly
**8× the classic commit in constant factor** — with #668's ML-DSA-44 signatures roughly
tripling that again — growing `log2(N)` in group size provided leaves stay merged, which
is what periodic self-update (#666) now guarantees. That is a defensible price for
post-quantum confidentiality **and** post-quantum authenticity at any group size Pollis
targets, and it holds as long as self-update keeps running. The constant factor is now
pinned by absolute ceilings (§4.1) rather than argued.

---

## 5. Testing in-box

> **Read the scenarios below as the #454 acceptance plan.** They are written around a
> mixed fleet; #669 retired the classic suite, so the mixed-fleet cases (S3, and the
> classic half of S2 and the marathon schedule) describe a state that can no longer be
> constructed. What survives unchanged is the shape of the suite-transition tests — a
> successor lineage, a boundary a message must not fall through — which still applies to
> any future move off `0x0052`.

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
`external_join_group`) joins a group that has migrated to hybrid. Assert it fetches the
hybrid GroupInfo, external-commits into the hybrid group, and its cross-signing cert still
verifies (`verify_added_devices`) — for #454 that confirmed the cert path was untouched by
the KEM change; post-#668 it is the stronger assertion that the **v2** cert, which binds
both the Ed25519 and the ML-DSA-44 leaf key in one ML-DSA-44 account-key signature,
verifies the leaf key of whichever suite the device lands on (§2.4.3, §3.1).

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
- **Defence in depth for the transition itself:** because the KEM is hybrid, adopting a
  young ML-KEM implementation cannot regress us below today's classical security even
  if ML-KEM (or its implementation) has a flaw (§2.2). Note this argument covers the KEM
  **only**: ML-DSA-44 (#668) is *not* hybrid — the MLS suite carries one signature
  algorithm, and there is no draft suite pairing a PQ KEM with a composite Ed25519+ML-DSA
  signature. A structural break in ML-DSA-44 would therefore cost us authenticity on the
  PQ suite outright, where a break in ML-KEM-768 would leave X25519 standing. That is a
  genuine asymmetry in the posture and we state it rather than smoothing it over; the
  mitigating facts are that a signature break is *detectable and reactive* (§2.4.1's
  argument, which does hold for live protocol steps) and that ML-DSA-44 is a FIPS 204
  standard, not a draft primitive.
- **Post-quantum authenticity on the long-lived paths (#668):** account identity keys,
  device certs and — the one that actually motivated the change — transparency-log STHs
  are ML-DSA-44 signed, so a CRQC cannot retroactively forge a statement about history
  that an auditor is meant to be able to re-check in 2035 (§2.4.2).

**What it explicitly does NOT buy — stated to avoid overclaiming:**

- **It does not fix metadata.** Turso still sees the social graph, membership,
  message timing and sizes (whitepaper §1.2). PQ KEX seals *content*, not the fact that
  A messaged B. If anything, larger PQ ciphertexts make *size*-based metadata slightly
  coarser, not finer.
- **It does not help if an endpoint is compromised.** A device with the unlocked keys,
  or malware in the trusted binary, reads plaintext regardless of KEM. HNDL is a
  *passive network* threat; endpoint compromise is a different axis (whitepaper §1.1).
- ~~**It does not make signatures/authentication post-quantum** (§2.4).~~ **Superseded by
  #668** — but only partially, and the residue must still be stated. Post-quantum
  authentication now covers MLS leaves on the PQ suite, account identity keys, device
  certs, DS request auth and transparency-log STHs. It did **not** cover, when #668
  landed: (a) the classic suite `0x0001`, whose leaves signed Ed25519 — so any group not
  yet migrated was still classically authenticated; (b) the relay **directory** signing key
  (`POLLIS_OVERLAY_DIRECTORY_KEY`), which stays Ed25519 because its signer is a Node
  Lambda and `node:crypto` has no ML-DSA. **#669 closed (a)** by retiring the suite: every
  MLS leaf in existence is now ML-DSA-44. (b) remains the one live Ed25519 surface.
- **It does not retroactively protect already-sent classic traffic.** Everything sent
  before a group migrates was sealed with X25519 and is, in principle, harvestable. The
  migration caps the *future* harvest window; it cannot un-harvest the past. This is
  intrinsic to HNDL and is the reason to start now (§1.3).
- **Nor does it retroactively protect already-*signed* history.** The mirror of the point
  above, and the one #668 is racing: STHs signed with the old Ed25519 log key before the
  rotation stay forgeable by a future CRQC. Bumping the STH contexts to `sth:v2` stops an
  old head being replayed as a new one; it does not make the old heads post-quantum. Only
  re-publishing history under the ML-DSA-44 key does, which is why the log key rotation is
  a *rotation with republication*, not a swap.
- **It is still not a "fully post-quantum messenger" claim.** The honest, defensible
  headline is now: **"post-quantum *confidentiality* for message content via hybrid
  X25519+ML-KEM key exchange, and post-quantum *authenticity* for identity, device certs
  and the transparency log via ML-DSA-44"** — with the two Ed25519 residues above named.
  That is a genuine cutting-edge position — it puts Pollis level with Signal's PQXDH on
  KEX and ahead of it on signatures — without the overclaim that everything is
  quantum-proof.

Positioned this way it is a real flex *and* it survives an auditor reading the
whitepaper's "Honest limits" tradition (whitepaper §6.9, §13). Overclaiming here would
be worse than not shipping.

---

## 7. Feasibility assessment — installed stack vs. what's needed

**Honest bottom line (#454): the released stack already shipped the hybrid ciphersuite. The
P0 spike proved a full hybrid group flow end-to-end, headless, with stock cargo — no
version bump, no fork, no `[patch.crates-io]`. The live risk is not availability; it is
code-point churn, and it is managed by pinning.**

**Amended by #668:** the PQ *signature* suites exist in **no released openmls** — 0.8.1 has
no `SignatureScheme::MLDSA44` and no ML-DSA ciphersuite at all — so `0x0052` is
unreachable from crates.io and the "stock cargo" property no longer holds. All five openmls
crates are now pinned to upstream `main` rev
**`34222ef632c87df58baaa7614c8a72ac235c5f07`** through a single workspace
`[patch.crates-io]` (`Cargo.toml`). It is one `[patch]` rather than five git dependencies
so the pin lives in exactly one place and **no** crate in the workspace — dev-dependency
graphs and transitive deps included — can resolve openmls back to the registry copy; a
split graph would compile (two `openmls` crates, distinct types) and then fail confusingly
at the boundary. `draft-ietf-mls-pq-ciphersuites` must be named on every openmls crate we
depend on *directly*, because openmls's own feature chains it with `?/`, which only fires
for the optional providers it pulls in and we pull the providers in ourselves. Also
non-optional: the `0-8-1-storage-format` feature, because upstream changed how
`ExtensionType`/`ProposalType`/`CredentialType` serialise into the storage provider and
every device already running v1.7.0 has `mls_kv` rows in the old shape — there is no
migration for MLS state, so a device that cannot deserialise its own tree has lost its
groups, not a cache. Upgrading the rev is a **protocol-visible act**, not a dependency
bump: the draft renumbers provisional code points between revisions and a code point change
is a wire-format break for every group already on `0x0052`. **The live risk is unchanged in
kind and larger in degree: code-point churn (§7.4).**

### 7.1 What the pinned tree can already do (as of #454; see the amendment above)

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
  upstream feature flag. As shipped by #454: `pollis-core/Cargo.toml`.
- **Superseded by #668:** the libcrux dependency is gone. `0x0052` is implemented by
  `openmls_rust_crypto` — the provider Pollis already instantiated — while libcrux
  implements no ML-DSA suite at all, so the whole "add a second provider" manoeuvre
  dissolved along with the suite that motivated it. What replaced it is the workspace git
  pin above.

### 7.2 P0 is DONE — proven end-to-end, headless (on the #454 suite)

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
confirming real hybrid encapsulation — not a stubbed or classical-only path. That
`hpke_init_key` is unchanged under `0x0052`: the KEM did not move (§2.1).

### 7.3 Provider integration: suite-routed under #454, single-backend since #668

> **Superseded.** Everything in this subsection describes the #454 state and is kept
> because the *reasoning* — a supply-chain advisory can veto a crypto backend regardless of
> its feature set — is the part worth remembering. As shipped today, one backend
> (`openmls_rust_crypto::RustCrypto`) serves **both** suites, `openmls_libcrux_crypto` is
> out of the dependency tree entirely, and there is no suite→provider dispatch:
> `PollisProvider` is the only provider any path constructs
> (`pollis-core/src/commands/mls/provider.rs`). The `with_suite_provider!` /
> `with_group_provider!` / `with_lineage_provider!` macros that performed the dispatch are
> deleted; two of the three had to deserialise the whole stored group just to read a suite
> before they could dispatch, work every call site then repeated inside the body. The
> routing invariant test `classic_provider_never_routes_to_libcrux` is now
> `mls_backend_is_rustcrypto`, and **nine** libcrux `deny.toml` ignores were retired — not
> by re-arguing reachability, but because the crates left the graph (see §7.3.1).

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

#### 7.3.1 How #668 closed the advisory ledger

Worth recording because the outcome is the *good* one and it was not the plan. #454 carried
nine libcrux ignores in `deny.toml`, in two clusters, each resting on a reachability
argument:

- **RUSTSEC-2026-0207 / -0208 / -0212** (`libcrux-sha3 0.0.8`, `libcrux-secrets 0.0.5`) came
  in transitively through `hpke-rs 0.6.1` ← `openmls_rust_crypto`. The fixes shipped in
  `hpke-rs 0.7.0`, which **no released openmls depends on** — but the #668 pin to openmls
  `main` does, so the tree now builds `libcrux-sha3 0.0.10` and `libcrux-secrets 0.0.6`,
  the patched versions. The git pin taken for the *signature* change happened to close
  three advisories the released stack could not.
- **RUSTSEC-2026-0211 / -0209 / -0210 / -0124 / -0075 / -0073** entered via
  `openmls_libcrux_crypto 0.3.1`. Moving the PQ suite to `0x0052` removed that crate from
  the graph, so all six went with it.

That also retires the *code invariant* the first cluster's argument rested on: the only
reason `PollisPqProvider` existed was to keep classic AES-GCM traffic away from libcrux's
non-constant-time tag check. With one backend there is nothing to route. **If a
libcrux-backed provider is ever reintroduced, every one of the six must be re-argued from
scratch** — the `deny.toml` note says so explicitly, and the entries must not simply be
resurrected.

### 7.4 The live risk: code-point churn, not availability

**This risk has already materialised once, exactly as predicted.** The section below was
written about `0x004D`; one release later #668 moved off it. Everything it says now applies
to `0x0052`, which is *also* provisional.

- `0x004D` was an **experimental** 2024 OpenMLS code point (X-Wing draft-06). The IANA MLS
  ciphersuite registry has **zero** post-quantum entries; nothing here is standards-final.
- `draft-ietf-mls-pq-ciphersuites-06` (2026-07-21) **dropped X-Wing**, moving to
  provisional code points `0x004E` / `0x004F` / `0x0052` built on
  `draft-irtf-cfrg-concrete-hybrid-kems`. WG state: "Waiting for WG Chair Go-Ahead."
- OpenMLS `main` implements those new suites behind a new feature flag
  `draft-ietf-mls-pq-ciphersuites` (openmls/openmls#2046 merged 2026-06-11,
  openmls/openmls#2118 2026-07-16) — **and moved `0x004D` behind that same flag.** So the
  next OpenMLS release is a breaking change for the #454 usage: the suite it relied on
  moves from always-on to feature-gated.
- **Where that left us (#668).** Waiting for a release was not an option — the PQ
  *signature* suites exist only on `main`, so §7's amendment pins the rev directly and
  names `draft-ietf-mls-pq-ciphersuites` on every direct openmls dependency. Pollis is
  therefore on a **provisional code point from an unreleased upstream**, deliberately, and
  the mitigation is procedural: **treat any rev bump as a migration, not a version bump**,
  and read the upstream diff for `types.rs` before moving it. A code point change is a
  wire-format break for every group already on `0x0052`.

**What this means for shipping.** Pollis is a closed fleet (one binary, one DS), so
shipping a provisional code point is *tolerable* — every client and the DS move together,
and there is no third-party interop to honour. But budget for a **further suite migration**
when the IETF suites are finalised (WG go-ahead, IANA registration). Crucially, that is an
argument **for** building the `mls_commit_log.generation` machinery (§3.2, §3.4) properly
rather than against it: the same suite-generation lineage that carries classic → PQ carries
`0x0052` → whatever IANA eventually registers.

**Why #668 needed none of that machinery, and why that is luck rather than design.** #668
moved `CIPHERSUITE_HYBRID` **in place** — the constant changed, and there is no
`0x004D` → `0x0052` migration anywhere in the tree. It got away with that only because the
§3.3/§5-Phase-5 gates mean a group is born hybrid *only* once the whole live fleet is
PQ-capable, so the classic→hybrid transition had not yet fired anywhere when the code point
moved and no group was stranded on `0x004D`. Once hybrid groups actually exist in the
fleet, that shortcut is gone: the next code-point change **is** a generation migration, and
`welcome_ciphersuite` returning `None` for an unimplemented code point means "cannot join",
never "fall back to classic". Do not read #668's in-place edit as a precedent.

### 7.5 In-tree work (our side), verified against `origin/main`

- **Suite plumbing (P1b — DONE):** the 31 references to `CS` / `.ciphersuite()` split far
  more cleanly than the raw count suggested. Both suites signed with Ed25519, so all 9
  signature-key-gen sites were suite-INVARIANT and read a `SIGNATURE_SCHEME` constant
  rather than taking a suite parameter — a device kept ONE signing key and one
  `device_cert` across suites, which is the behaviour the cross-signing design already
  assumed. Only the two families that mint suite-bound material became parametrised:
  group creation (`create_mls_group_in_suite`) and KeyPackage construction
  (`build_key_package_in_suite`), each taking the suite alongside the provider that serves
  it. Parametrising the signature sites too would have been busywork encoding a
  distinction that did not exist.
  **#668 made that distinction real** and the busywork necessary: `SIGNATURE_SCHEME` became
  the function `signature_scheme(suite)`, the nine sites now take the scheme they need from
  the suite they operate in (or from the stored group), a device holds one signing key per
  scheme, and `device_cert` v2 certifies both (§3.1, §2.4.3). The 2024-era judgement was
  right for the facts it had; it was a bet on the suites' signature schemes staying equal,
  and that bet is the thing #668 unwound.
- **DS monotone-head rule** is enforced **solely** in `pollis-delivery/src/commit.rs`:
  `head_epoch` (`commit.rs:131`) and the conditional
  `INSERT ... WHERE ?2 = (SELECT COALESCE(MAX(epoch), -1) + 1 ...)` in `submit_commit`
  (`commit.rs:179-213`), keyed `(conversation_id, epoch)` by the `idx_mls_commit_unique`
  index. That single site is the one that widens to `(conversation, generation, epoch)`
  (§3.2, §3.4).
- **Capability advertisement is genuinely new:** `user_device` advertised **no**
  capabilities before this work (only `mls_signature_pub`), so `pq_capable` (§3.4) is a new
  advertisement channel, not an extension of an existing one. It has since been joined by
  `mls_signature_pub_pq` (#668), which is *also* a capability signal in practice: NULL
  means "predates #668".
- **The provider work (P1a)** generalises `PollisProvider` over its crypto backend and
  added a second alias, `PollisPqProvider`, wired to libcrux — it did **not** move the
  classic suite off RustCrypto (§7.3). Behaviour on every production path was therefore
  byte-identical; the gate was the whole existing suite passing, plus the `supports()`
  regression test and the routing-invariant test from §7.3.
  **#668 removed `PollisPqProvider` and the dispatch macros entirely** (§7.3): one backend,
  no routing, and `MlsProvider<C>` stays generic over `C` purely so reintroducing a backend
  remains a one-line change rather than a signature change everywhere.

All of the in-tree work builds and tests against the classic suite today, and the crypto
route is already proven (§7.2), so there was **no external gate left** for #454 — the long
pole was our own plumbing, not an upstream dependency. #668 reopened exactly one external
dependency: the PQ signature suites are unreleased, so the tree is pinned to an openmls
`main` rev (§7).

---

## 8. Phased roadmap, acceptance criteria, dependencies

> **Read this section as a record of what each phase shipped, not as current state.** Two
> later tickets moved the ground under it: #668 (signatures, suite code point, one crypto
> backend — §2.4, §7.3) and **#669** (the classic suite retired outright — §3.3). Where a
> phase below names `CS_CLASSIC`, `PUBLISHED_SUITES`, `suite_for_new_group`, `pq_capable`
> or `migrate_to_hybrid_if_due`, none of those exist today under those names or at all;
> Phase 5 spells out exactly what #669 removed and why it did not need the multi-release
> dance this roadmap budgeted for.

**Phase 0 — Spike: COMPLETE.** ✅ The crypto route is pinned and proven. A throwaway
two-client crate on `openmls 0.8.1` + `openmls_libcrux_crypto 0.3.1` (stock cargo, no
patch) created a hybrid `0x004D` group and round-tripped an application message plus an
`export_secret` between two clients, headless (§7.2). **Decision recorded:** no version
bump and no fork — keep `openmls = "0.8"`, add a direct dependency on
`openmls_libcrux_crypto = "0.3"`, suite `MLS_256_XWING_CHACHA20POLY1305_SHA256_Ed25519`
(`0x004D`). The one dependency risk carried forward is code-point churn (§7.4): pin both
versions, and treat a future OpenMLS bump as a tracked migration.
**No remaining upstream gate.**
**Superseded by #668:** the suite is now `MLS_128_MLKEM768X25519_CHACHA20POLY1305_SHA384_MLDSA44`
(`0x0052`), the libcrux dependency is gone, and the openmls crates are pinned to an upstream
`main` rev via `[patch.crates-io]` because the PQ signature suites are unreleased (§7).

**Phase 1a — Provider routing: DONE, then REMOVED by #668.** ✅ Landed as *routing*, not
the swap this phase originally proposed. `PollisProvider` (RustCrypto) served `CS_CLASSIC`
and `PollisPqProvider` (`openmls_libcrux_crypto`) served `CS_HYBRID`, both over the custom
`MlsStore`; the crate's bundled `Provider` type was not used (it hardcodes in-memory
storage — §7.3). The swap was rejected because libcrux's AES-GCM decryption has an
unpatched non-constant-time tag check (RUSTSEC-2026-0211), which would have put *classic*
traffic on vulnerable code; the hybrid suite's AEAD is ChaCha20-Poly1305, which the
advisory does not touch. The routing was therefore security-load-bearing and was pinned by
`classic_provider_never_routes_to_libcrux` alongside the `supports()` self-contradiction
regression test (§7.3).
**#668 dissolved rather than routed around the problem:** `0x0052` is implemented by
RustCrypto and by no libcrux provider, so the second backend, the dispatch macros, the
cross-provider interop test and nine `deny.toml` ignores all left the tree together
(§7.3.1). The routing invariant is now the simpler `mls_backend_is_rustcrypto`.

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
  paired provider and rotating/topping-up **per suite** so neither pool is ever
  silently drained. The rotation sends both pools in one
  `/v1/key-packages/replenish`; the DS already scopes its DELETE per suite.
  (Post-#668 there is one provider for both, and what varies per suite is the leaf
  signing key's *scheme* — §3.1.)
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
  KP-size pins (classic init key 32 B, hybrid 1216 B). Post-#668 the two suites also
  differ in signature scheme, pinned by `signature_scheme_per_suite`
  (`mls/tests.rs`): Ed25519 for `CS_CLASSIC`, `MLDSA44` for `CS_HYBRID`, so a suite
  swap cannot silently change which key a leaf is signed with.

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

**Phase 5 — Fleet completion: DONE (gate); classic retirement since DONE by #669.** ✅ What
this phase shipped is the **fleet-completeness gate**, not the retirement: `fleet_is_fully_pq_capable`
requires that deployment-wide no `user_device` with `revoked_at IS NULL` and `last_seen`
within `FLEET_DORMANCY_DAYS` (90) still has `pq_capable = 0`, and it gates **birth and
migration alike** — one switch, one meaning. The per-roster rule of §3.3 is necessary but
nearly vacuous on its own: a roster grows, and a hybrid group that must later admit a
classic-only device has only two options, both bad. The dormancy window is what makes
"the fleet has updated" a reachable condition rather than one hostage to a laptop in a
drawer; the gate fails *toward availability*, so a check that cannot be evaluated keeps the
group classic.
**Left open here, deliberately, and since closed by #669:** as of this phase, classic KP
publication was *not* retired — pre-migration lineages and any client that had not yet
updated still needed it, and the retirement was expected to require a multi-release
additive→drop dance (CLAUDE.md) that could not begin until an app that stopped *needing*
classic had full uptake. **#669 did it as a hard cutover instead**, because the deployment
has zero active users: there is no un-updated app in the field, so there was no uptake to
wait for. `CS_CLASSIC` and `PUBLISHED_SUITES` are deleted, one pool publishes, the gate
this phase built is deleted with the suite it gated, and *no* migration was needed — the
`pq_capable` column is retired in place rather than dropped, keeping migrations additive.
The one thing #669 did **not** do is remove the last Ed25519 MLS leaf key: the device still
mints and publishes `mls_signature_pub` beside `mls_signature_pub_pq`, both bound by the v2
cert, because `load_or_create_device_signer` is keyed by signature *scheme* and a group
persisted under an older code point must stay readable. AES-256 remains a *distinct
future track* (§2.3), off this HNDL critical path.
**No longer open:** ML-DSA shipped as **#668** (§2.4), not as a future track. The reason it
jumped the queue is in §2.4.2 — the transparency log, not MLS, is what made it urgent.

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
The residual dependency is *pinning discipline* against code-point churn (§7.4).
**Reopened and re-resolved by #668:** the PQ *signature* suites exist in no release, so all
five openmls crates are pinned to `main` rev `34222ef6` via one workspace
`[patch.crates-io]`; the residual is unchanged in kind — treat a rev bump as a protocol
migration, not a dependency update. (2) DS write endpoints extended for suite-tagged KP
claim/publish; (3) ~~a
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

> **This is the #454 issue text as filed, kept verbatim as a record.** Where it says
> signatures stay Ed25519, read §2.4: #668 superseded that. Where it names `0x004D`,
> `openmls_libcrux_crypto` or `SIGNATURE_SCHEME`, read §7: the suite is `0x0052`, there is
> one crypto backend, and the scheme is a function of the suite. Where it says *dual-suite*
> — two key-package pools, a capability gate, an overlap window — read §3.3: #669 retired
> the classic suite, so there is one pool, no gate, and no overlap. The corrected end state
> is summarised at the bottom of this section.

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

### 9.1 Corrected end state (post-#668, post-#669)

For anyone reading this document as a statement of *current* fact rather than as a record:

| | As filed (#454) | As shipped today |
|---|---|---|
| PQ suite | `MLS_256_XWING_CHACHA20POLY1305_SHA256_Ed25519`, `0x004D` | `MLS_128_MLKEM768X25519_CHACHA20POLY1305_SHA384_MLDSA44`, **`0x0052`** |
| PQ suite KEM | X-Wing (X25519 + ML-KEM-768) | **unchanged** — same X-Wing |
| PQ suite AEAD | ChaCha20-Poly1305 | **unchanged** |
| PQ suite KDF | SHA-256 | SHA-384 |
| PQ suite signature | Ed25519 | **ML-DSA-44** (`SignatureScheme::MLDSA44`, `0x0904`) |
| Classic suite | `MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519`, `0x0001` | **RETIRED (#669)** — `CS_CLASSIC` deleted, `CS_HYBRID` renamed `CS_PQ` |
| Suites in use | two | **one** (`CS_PQ`) |
| Suite at group birth | `suite_for_new_group`, behind roster + fleet `pq_capable` gates | **always `CS_PQ`** — every gate deleted with the suite it gated |
| KeyPackage pools per device | two (`PUBLISHED_SUITES`), 5 each | **one**, 5 in `CS_PQ` |
| `user_device.pq_capable` | DS-derived, read by both gates | **dead** — no reader, no writer; retired in place, column not dropped |
| Suite migration | `migrate_to_hybrid_if_due`, classic → hybrid, behind both gates | `migrate_to_current_suite_if_due`, anything ≠ `CS_PQ` → `CS_PQ`, no gates; no-stranding enforced by the target-suite KP claim |
| Crypto backends | two, routed by suite (RustCrypto + libcrux) | **one** — `openmls_rust_crypto::RustCrypto` |
| openmls source | crates.io `0.8.1`, stock cargo | `[patch.crates-io]` to `main` rev `34222ef6`, five crates |
| Device MLS signing key | one Ed25519 key per device | **one key per signature scheme**: ML-DSA-44 for every live leaf; the Ed25519 key is still minted, published and certified, but no live suite signs leaves with it |
| Device cert | v1, `u8` length prefixes, one key | **v2** (`pollis-device-cert-v2\x00`), `u16` prefixes, certifies **both** leaf keys |
| Account identity key | Ed25519 | **ML-DSA-44** (private key is still a 32-byte seed) |
| DS request auth | Ed25519 over `mls_signature_pub`, 88 b64 chars | **ML-DSA-44** over `mls_signature_pub_pq`, ~3228 b64 chars |
| Transparency-log STH | Ed25519, `sth:v1*` contexts | **ML-DSA-44**, `sth:v2` / `sth:v2:account-keys` / `sth:v2:binaries` |
| Relay directory key | Ed25519 | **unchanged** — the signer is a Node Lambda and `node:crypto` has no ML-DSA |
| Turso schema | `000010` (KP `ciphersuite`, `user_device.pq_capable`) | + `000011_device_pq_signature_pub.sql` (`user_device.mls_signature_pub_pq`, nullable) |

Honest scope, restated: **post-quantum confidentiality** for message content (hybrid
X25519 + ML-KEM-768 KEM) **and post-quantum authenticity** for identity, device certs and
the transparency log (ML-DSA-44) — with the relay directory key still Ed25519, no fix for
metadata or endpoint compromise, and no retroactive protection of already-sent classic
traffic or already-signed Ed25519 history (§6). Retiring the classic suite does not change
that last clause: a migration is forward-only, and traffic sealed under a retired suite
stays sealed under it.
