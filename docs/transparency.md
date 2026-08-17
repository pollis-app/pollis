# Key Transparency — the Pollis verifiable log

Pollis publishes an **append-only transparency log** of every MLS commit, so that
anyone — a Pollis user, a journalist, an independent security researcher — can
prove for themselves that the server has not quietly rewritten a conversation's
history. This document explains what the log is, the threat it closes, the trust
model, and the four pieces that make it work.

For a copy-pasteable walkthrough that verifies the live log on your own machine,
see **[verify-transparency-log.md](./verify-transparency-log.md)**.

## What a transparency log is, in plain terms

Pollis is end-to-end encrypted, so the server already cannot read your messages.
But the server still *coordinates* every group: it stores the ordered list of MLS
**commits** (the membership/key changes that drive a conversation forward through
its epochs). A malicious or compromised server can't decrypt anything, but it
could still try to misbehave with that ordering:

- **Fork a conversation** — show Alice one history and Bob a different one, so
  they think they are in the same secure group when they are not.
- **Roll an epoch back / replay** — re-introduce an old commit to undo a member
  removal or a key rotation.
- **Equivocate** — present two different "official" versions of the log to two
  different auditors.

A transparency log closes this. Every commit is added as a leaf to a single
global **Merkle tree** (RFC 6962 / RFC 9162, the same construction Certificate
Transparency uses). The log periodically publishes a **Signed Tree Head (STH)** —
an ML-DSA-44 signature over `(tree_size, root_hash, timestamp)`. From that one
signed root, anyone can demand:

- an **inclusion proof** that a specific commit really is in the tree, and
- a **consistency proof** that a newer tree is an append-only extension of an
  older one (nothing was deleted or rewritten between two heads).

Because the tree is append-only and the heads are signed, the server cannot fork,
roll back, or equivocate without producing a signature or a proof that fails to
verify — and that failure is detectable by anyone running the verifier.

## Trust model (the load-bearing idea)

> A verifier trusts **only** the log's published ML-DSA-44 **public key**, plus the
> **signed tree head** and the **Merkle proofs** that are checked against it. It
> trusts **nothing else** — not the server, not the Turso database, not the host
> serving the files, not the network.

Everything else is treated as hostile. The static files can be served by any CDN,
bucket, or compromised box; if a single byte of an entry, proof, or STH is
altered, a signature or proof check fails and the verifier exits non-zero. The
read API is **public and unauthenticated by design** — there are no credentials
anywhere on the verification path, so anyone can audit the log over plain HTTP.

The one thing a verifier *does* assume is that it has the **genuine public key**.
That key is small and stable; it can be pinned, cross-checked across mirrors, and
published in multiple places. Trust rests on the signature and the proofs — never
on the server that hands them to you.

> **Scope note.** This document describes the verifiable-log tooling shipped under
> issue #330, now covering **three** published trees (the MLS commit log, the
> account-key directory, and the released-binaries tree — see below). The
> end-to-end threat-model writeup lives elsewhere and is out of scope here.

## Three domain-separated trees

The log actually publishes **three independent Merkle trees**, each with its own
entries, its own Signed Tree Heads, and its own append-only history:

- **the MLS commit log** — every membership/key-change commit, the tree described
  throughout this document;
- **the account-key directory** — one leaf per account identity-key version
  (`user_id`, `identity_version`, the ML-DSA-44 account public key), so anyone can
  audit that a user's published key history is append-only and that
  `identity_version` only ever increases (no silent key substitution, no replay
  of a revoked key). A single user is verified with
  `pollis-verify account <user_id>` (against the precomputed
  `/verify/account/<user_id>` report); the Pollis client self-audits the same
  way — `self_audit_account_key` for the running user, `audit_peer_account_key`
  for a TOFU-pinned contact — reusing the exact same `verify_account` function,
  so the app and the CLI can never disagree; and
- **the released-binaries tree** (binary transparency) — one leaf per released
  build artifact (`release_tag`, `platform`, `arch`, `bundle`, `layer`, a
  content hash, and the full reproducibility recipe), so anyone can audit that
  the binary they run is the one the log published for a tag and that its
  reproducible payload was itself logged. A single release is verified with
  `pollis-verify release <tag>` (against the precomputed `/verify/release/<tag>`
  report), reusing the exact same `verify_release` function the static endpoint
  calls, so the CLI and the served report can never disagree. The leaf commits to
  a hash + recipe, **never** the binary bytes.

The three trees are **never interleaved**. They are signed by the same ML-DSA-44 key
but under **different domain-separation contexts** (`…:sth:v2` for the commit log,
`…:sth:v2:account-keys` for the account keys, `…:sth:v2:binaries` for the released
binaries), so an STH minted for one tree **cannot** be replayed as another's — a
verifier checks each head under its own context. The commit-log tree and every one
of its `/v1/...` bytes are exactly as before; the account-key tree lives entirely
under `/v1/account-keys/...` and the binaries tree under `/v1/binaries/...`.

## The four pieces

The system is four small Rust/JS components, each building on the one before it.
Each has its own README with the full detail; this is the map.

| Piece | Crate / path | Role |
|-------|--------------|------|
| **monitor** | [`verifiable-log`](../verifiable-log/README.md) | The Merkle-log core **and** the fully offline verifier CLI. Verifies a downloaded bundle with no network and no database. |
| **builder** | [`verifiable-log-builder`](../verifiable-log-builder/README.md) | Reads the real `mls_commit_log` from Turso/libSQL and emits a **signed bundle** the monitor verifies byte-for-byte. Hashes each commit blob and drops the raw bytes — they are never stored or logged. |
| **serve** | [`verifiable-log-serve`](../verifiable-log-serve/README.md) | Turns a signed bundle into the immutable static `/v1/...` read API, plus a dev HTTP server and the `/verify/group/<id>` endpoint the explorer calls. |
| **pollis-verify** | [`verifiable-log-serve`](../verifiable-log-serve/README.md) | The auditor CLI shipped to security analysts: a whole-log HTTP verifier (`pollis-verify remote`), a per-conversation verifier (`pollis-verify group`), a per-user account-key-history verifier (`pollis-verify account <user_id>`), and a per-release binaries verifier (`pollis-verify release <tag>`). |
| **website explorer** | [`website/transparency.html`](../website/transparency.html) | A browser convenience demo that calls the serve layer's `/verify/group/<id>` endpoint and visualizes the result. It is **not** the trust anchor — the trustworthy path is running the tool yourself. |

Data flows one direction: `mls_commit_log` → **builder** signs a bundle →
**serve** generates the static tree → **monitor** / **pollis-verify**
(`remote` / `group` / `account` / `release`) / the explorer check it. The core Merkle, proof, signature, and
invariant logic lives in `verifiable-log` and is **reused, never reimplemented**,
by every layer — so the CLI, the HTTP endpoint, and the website can never reach
different verdicts for the same input.

### The commit-log invariant

Beyond raw Merkle inclusion, the log enforces three rules per **grouping key** when
the commits are replayed (the publicly-auditable mirror of the live DB's
`UNIQUE(conversation_id, generation, epoch)` constraint). Since #701 the grouping
key is the leaf's `conversation_pseudonym` (a per-window value), so a public replay
checks these *within a window*; a member who knows the real `conversation_id`
regroups across windows and checks them end to end — see
[Windowed pseudonyms](#windowed-pseudonyms-in-the-commit-log-701):

- **No fork** — no two commits share the same
  `(conversation_pseudonym, generation, epoch)`.
- **No epoch regression / replay** — within a grouping key, `(generation, epoch)`
  strictly increases **lexicographically** in `seq` order.
- **A lineage opens at epoch 0** — the first commit of a generation higher than any
  seen before must be at epoch 0.

The `generation` term is the suite lineage (#454 P4). MLS binds the ciphersuite at
group creation, so moving a conversation off a retired code point onto the current
suite is not a commit — it stands up a *successor* group whose epoch counter restarts
at 0. (#454 introduced this to carry conversations from the classic suite to the
post-quantum one; #669 retired the classic suite, but the mechanism stays, because
the PQ suite's own code point is provisional and may yet be renumbered.) A
scalar epoch key would read every honest migration as a regression; ordering the pair
lexicographically fixes that, because the successor's epoch 0 sorts above the retired
lineage's last epoch on the higher generation. Rule three exists because rule two
alone would then be too weak: a server could open generation N+1 at a fabricated
mid-lineage epoch and make a fork look like a migration. Together they prove a
conversation's lineages are contiguous, non-overlapping, and totally ordered.
`generation` is omitted from the leaf encoding when it is 0, so every leaf written
before P4 — and every conversation that has never migrated — is byte-identical to
what it was.

A fork or regression in the source data **aborts the build** rather than producing
a bundle that hides it, and the verifiers re-check it independently on replay.

## Windowed pseudonyms in the commit log (#701)

Before #701 the commit-log leaf published a **stable `conversation_id` and a real
`sender_id` in the clear**. Hex is not obfuscation: anyone who fetched
`/v1/entries.json` — with no access to Pollis at all — could build a longitudinal
activity map of every group (which groups exist, who commits to them, how often,
when, and which groups a given user is in across the whole product). The raw commit
bytes were never published (the leaf commits to `sha256(commit_data)` only), so no
content or key material leaked; the leak was purely the **social/activity graph**.
#701 closes it.

### The scheme

The leaf's two identity fields are now **windowed pseudonyms**, never the raw ids:

```
window                 = seq / PSEUDONYM_WINDOW_SIZE          (a coarse bucket)
conversation_pseudonym = SHA256(dom_c || conversation_id || window)
sender_pseudonym       = SHA256(dom_s || conversation_id || sender_id || window)
```

(`dom_c`/`dom_s` are fixed domain-separation tags; every field is length-prefixed
so the pre-image is unambiguous.) Two properties fall out:

- **Keyless, no new custody problem.** The only "secret" is `conversation_id`
  itself, which members and the server already hold but a third-party log scraper
  does not. No new key is minted, so there is nothing new to guard — deliberately,
  because the log's *existing* key is already a hard custody problem
  (`docs/sth-signing-key-custody.md`); a second secret here would be a second one.
- **Sender bound to conversation.** Because the sender pseudonym mixes in
  `conversation_id`, the same user gets an unrelated pseudonym in every group they
  are in. Following a *person* across the groups they participate in — the worst
  part of the old exposure — is broken outright, not merely windowed.

A **member** (or an auditor a member trusts) knows the real `conversation_id`, so
they re-derive `conversation_pseudonym` for every window and recover their whole
history; `pollis-verify group <conversation_id>` and the
`GET /verify/group/<conversation_id>` endpoint do exactly this. The window of any
leaf is a function of its own published `seq`, so no extra field is published and a
member needs nothing but the id they already know.

### Why a window, and why this window

The unlinkability goal fights the public-auditability goal, and the tension is
real, not hand-waved. The whole point of the log is that *anyone* can replay it and
check the invariant — but every invariant rule is stated **per conversation**. If
the identifier were a single unchanging pseudonym, an outsider still could not group
entries and so could not check anything; if it were the raw id, there is no privacy.
The **window** is the hinge: a conversation's pseudonym is *stable within a window*
and *rotates at each boundary*. So:

- **inside a window**, an outsider groups a conversation's entries by their shared
  pseudonym and checks fork / monotonicity / lineage-opening exactly as before —
  the invariant stays **publicly checkable with no side knowledge**;
- **across windows**, the pseudonym changes, so an outsider cannot tell window `w`
  and window `w+1` are the same conversation — linkage is broken for anyone who
  does not know `conversation_id`.

`PSEUDONYM_WINDOW_SIZE` is the one tunable. Larger → more of a conversation's
commits share a window, so the public check has more to bite on, but a conversation
stays linkable across a longer stretch of the log. Smaller → stronger
unlinkability, but the public check degrades toward per-entry singletons and there
are more boundaries (bigger residual, below). It is defined over the global `seq`
sequence rather than wall-clock time on purpose: `seq` is **already published**, so
windowing adds *no new field and no new timing disclosure* — the most conservative
choice for a privacy change (the builder already, deliberately, does not read or
publish each commit's `created_at`). The commit log grows by one leaf per MLS
*commit* (a membership/key change), not per message, so it is low-volume; the
current value (1024) keeps a busy conversation groupable within a window while
fragmenting a long-lived group across many. Because the value is baked into the
frozen leaf, it only ever changes at a full republish. (A fixed *calendar* window
would give a scale-independent "unlinkable after N days" guarantee; it would cost a
published coarse timestamp and a `created_at` read. If that guarantee is later
wanted, switching the window function is a localized change at the next republish.)

### The residual, named honestly

Windowing weakens the invariant **at window boundaries**: a fork (or regression)
whose two branches straddle a boundary carries two *different* pseudonyms, so a
public replay that groups by pseudonym **cannot** see them as the same conversation
and does not fire. This is not a bug awaiting a patch — it is inherent. A *public*
commitment that let an outsider re-link the two windows would, by construction, also
let them defeat the unlinkability the scheme exists for: **public cross-window fork
detection and public cross-window unlinkability are mutually exclusive for a
third-party observer.** So we do not add a chaining commitment (it could only be
useless-to-outsiders or unlinkability-defeating); we state the residual and close it
for everyone who legitimately can:

- **Members are unaffected.** Knowing `conversation_id`, they regroup every window
  under one key and check the full-strength invariant end to end — the
  `verify_group` path does exactly this, which is *why* it takes the real id and not
  a pseudonym. The serve tests plant a boundary-straddling regression and prove a
  member still catches it.
- **The builder never emits one.** It holds the real ids, so `build_bundle` runs
  the invariant twice: once at **full strength** over leaves keyed on the real
  `conversation_id` (this catches any boundary-straddling fork/regression and aborts
  the build), then over the published pseudonymous leaves. Only a *substituted or
  compromised* publisher could plant a boundary-straddling fork, and a member catches
  that on replay.

So the residual is precisely: **a boundary-straddling fork is invisible to an
outside-only replay, but not to a member and not to the honest builder.** It is
recorded here and in `docs/metadata-retention-policy.md` §6.

### Frozen contract → this lands at the republish (coupling to #672 / #699)

Leaf bytes are hashed into the Merkle tree, so **re-encoding published leaves is not
an edit — it invalidates every signed root ever published and every inclusion proof
a client has cached.** Pseudonymising *existing* history is therefore a **republish
of the tree from scratch**, not a migration. The only sane landing point is the
republish the #672 / PL-11 ML-DSA-44 key rotation (executed by #699,
`docs/sth-signing-key-custody.md` §7) already performs under the `sth:v2` contexts:
that ceremony rebuilds all three trees, and #701's code changes **what the rebuilt
commit-log tree contains**. Whoever runs the ceremony must know this — the
republished tree will carry pseudonymous leaves the moment #701's builder is the one
that builds it. (The account-key and binaries trees are unaffected; see below.) This
follows the `generation` precedent (#454 P4): a compatible leaf extension that takes
effect without pretending old bytes can be rewritten in place.

### A stale `pollis-verify` after the republish: skew, not a false alarm

The republish is a **breaking wire change**, and `pollis-verify` is the tool an
auditor runs to ask *"has this log been tampered with?"*. Handing that person a hard
failure the day after we rotate the key would be the exact false-alarm mode #668
already ruled out for the missing log pin — an inability to check must *withhold*
trust, never *alarm*. So the change ships with a version gate.

**Who actually stops working** (measured, not assumed — the review note's "missing
field" mechanism was off):

| subcommand | pre-#701 binary vs the republished log | why |
|---|---|---|
| `remote` | **still passes** | The manifest's `conversations` field was always `#[serde(default)]`, so its removal is not a missing-field error; `remote` never decodes a commit leaf (it replays through the generic uniqueness invariant), so the leaf-shape change is invisible to it. |
| `group` | **silently wrong** — reports `Found: no` for a conversation that is really there | It decodes each leaf into the *old* `CommitLeaf` (which requires `conversation_id`); a pseudonymous leaf fails to decode, the error is swallowed, and the selection comes back empty. Not a crash — a misleading empty result. |
| `account`, `release` | **unaffected** | Separate trees; neither their manifest shape nor their leaf encoding changed. |

Because a pre-#701 binary is already compiled, we cannot retrofit it — but we can
stop this from recurring, and make the failure legible going forward. The served
manifests now carry a **`format_version`** (`verifiable_log_serve::bundle::FORMAT_VERSION`,
currently `2`). `pollis-verify` reads it **before** it deserializes or verifies
anything: a log published in a format *newer* than the binary understands exits with
a distinct code (`2`) and the message *"your pollis-verify is too old for this log —
upgrade it"*, explicitly **not** a verification failure (exit `1`). A format at or
below what it understands — including a legacy manifest with no `format_version` at
all — is accepted, so a new binary still works against the not-yet-republished log
during the transition window. From this release on, the *next* breaking wire change
surfaces as honest skew rather than a serde error or a silent empty result. **After
the republish, upgrade any `pollis-verify` older than `v0.6.0`.**

The **website explorer is unaffected**: it calls the live server's *dynamic*
`GET /verify/group/<id>` endpoint (`BACKEND_BASE` in `website/transparency.js`), which
always runs the current code — it never deserializes a static manifest or an old leaf
shape, so there is nothing for it to be stale against.

### What an outside observer can and cannot learn after this lands

**Can still learn (unchanged):** the total number of commits, the tree's growth over
time (STH timestamps), and — *within a single window* — that some conversation had a
run of commits with a given epoch progression, and whether that run is fork-free.
This is what keeps the log a transparency log.

**Can no longer learn:** which real conversation a pseudonym is; that two windows
belong to the same conversation; that a pseudonym in group A and a pseudonym in
group B are the same user; and therefore the cross-time, cross-group **activity map**
that was the whole exposure. The map now requires knowing a `conversation_id`, i.e.
being a member (or the server, which already had the data — the log defends against
the server *lying*, not against it *knowing*).

### Out of scope: the account-key tree's `user_id`

The account-key tree still publishes a real `user_id`, and #701 deliberately does
**not** touch it. Key transparency requires looking a user up **by identity** — the
whole point is to audit *that specific user's* published key history — so the
identifier there is load-bearing, not incidental exposure. Pseudonymising it would
break the feature.

### The binaries invariant

The released-binaries tree enforces three rules over the whole tree when the
`BinaryRecord` leaves are replayed (the publicly-auditable form of binary
transparency's per-release rules):

- **No silent re-issue (fork)** — no two leaves share
  `(release_tag, platform, arch, bundle, layer)` but disagree on
  `artifact_sha256`; a legitimate re-release must use a new tag.
- **Monotonic releases** — `release_tag` is append-only in publish order: once a
  newer tag has begun appearing, an earlier tag can never reappear.
- **Payload/signed pairing** — every `layer:"signed"` leaf (a notarized/signed,
  non-reproducible wrapper) must have a matching `layer:"payload"` leaf with equal
  `payload_sha256` earlier in the tree, so the reproducible unit inside a signed
  artifact is always itself published and independently reproducible.

As with the other trees, a violation in the source records **aborts the build**,
and `pollis-verify release <tag>` re-checks the invariant independently on replay
(so a forked or unpaired tree the signature happens to cover is still rejected).

## The static read API (`/v1/...`)

Everything a transparency log serves is deterministic and immutable: the STH for
`tree_size = N` never changes; an inclusion or consistency proof for a given
`(leaf, tree_size)` is fixed forever. So the serve layer is **not** a query
service over a database — it is a precomputed directory of immutable JSON files
served as plain static assets. The URL path mirrors the file path exactly.

| URL | Contents | Cache |
|-----|----------|-------|
| `/v1/public_key.json` | the log's ML-DSA-44 public key (1312 bytes, 2624 hex chars) | immutable |
| `/v1/index.json` | discovery manifest — carries `format_version` (the served wire version a verifier checks first) | short (`no-cache`) |
| `/v1/sth/latest.json` | newest STH | short (`no-cache`) |
| `/v1/sth/<tree_size>.json` | STH at that tree size | immutable |
| `/v1/entries.json` | full ordered `[Entry]` | immutable |
| `/v1/entries/<index>.json` | one entry | immutable |
| `/v1/proof/inclusion/<tree_size>/<leaf_index>.json` | inclusion proof | immutable |
| `/v1/proof/consistency/<first>-<second>.json` | consistency proof | immutable |

The account-key and binaries trees mirror this exact layout one level down, under
`/v1/account-keys/...` and `/v1/binaries/...` respectively (their own
`public_key.json`, `index.json`, `sth/`, `entries.json`, and proofs). The binaries
tree additionally publishes a precomputed `/verify/release/<tag>` report per tag,
just as the account tree publishes `/verify/account/<user_id>`.

Only `latest.json` and `index.json` move as the log grows; everything else is
write-once. The dev server (`serve serve`) additionally exposes the dynamic
`GET /verify/group/<id>` endpoint that the website explorer calls — that endpoint
runs the exact same `verify_group` code the CLI does. The wire shapes (`Entry`,
`Sth`, `InclusionProof`, `ConsistencyProof`) are the frozen contract documented in
[`verifiable-log/README.md`](../verifiable-log/README.md).

## Verify it yourself

The whole point is that you don't have to take any of this on faith. Build the
verifier and run it against the live log — see
**[verify-transparency-log.md](./verify-transparency-log.md)**.

## Rotating the signing key

The design, custody options and the literal step-by-step ceremony live in
[`sth-signing-key-custody.md`](sth-signing-key-custody.md) — that is the canonical
runbook; follow it rather than this summary. Last executed by #732, which replaced
the original seed with fresh material.

Two things about it are easy to get wrong and worth repeating here:

- Dispatch `transparency-publish.yml` with **`full_resync: true`**. Re-signed
  artifacts are byte-length-identical to the ones they replace, so the default
  `--size-only` sync skips them and the rotation silently does not take.
- The published key is pinned in **nine** places that must never disagree. This
  paragraph used to say five, name two constants that no longer exist, and omit
  three real copies — which is exactly the drift #945 was about, in the document
  telling you how to avoid it. So the list is no longer maintained here by hand:
  `scripts/check-pinned-log-key.py` holds it, treats
  `pollis-core/src/commands/transparency.rs` as the source of truth, and fails CI
  if any copy disagrees **or if a new copy appears that it does not know about**.
  Run it locally before opening the rotation PR:

  ```bash
  python3 ./scripts/check-pinned-log-key.py
  ```

  Change every copy in one commit. If you add a tenth surface, register it in
  that script's `COPIES` table in the same commit — the check fails until you do,
  by design.

**Known gap (#700):** there is no overlap window, so a rotation is a flag day —
auditors holding cached pre-rotation heads see it as equivocation and can only
re-pin from the announcement. The key-set shape that fixes this is designed in
§5 of the custody doc and not yet built.
