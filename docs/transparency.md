# Key Transparency: the Pollis verifiable log

Pollis publishes an append-only transparency log of every MLS commit, every account
identity-key version, every desktop release and every mobile over-the-air update, so that anyone can check that the
server has not rewritten a conversation's history or swapped someone's key. This
document covers the threat, the trust model, the components and the design
decisions. To run the verifier against the live log, see
[verify-transparency-log.md](./verify-transparency-log.md).

## What the log protects against

Pollis is end-to-end encrypted, so the server cannot read messages. It does still
order every group's MLS commits (the membership and key changes that move a
conversation from epoch to epoch). A malicious or compromised server could:

- **fork a conversation**: show Alice one history and Bob another, so they believe
  they share a group when they do not;
- **roll back or replay an epoch**: re-introduce an old commit to undo a removal or
  a key rotation;
- **equivocate**: show different versions of the log to different auditors.

Every commit is a leaf in a Merkle tree (RFC 6962 / RFC 9162, the construction
Certificate Transparency uses). The log publishes Signed Tree Heads (STHs): an
ML-DSA-44 signature over `(tree_size, root_hash, timestamp)`. Against a signed root
anyone can check

- an **inclusion proof**, that a given commit is in the tree, and
- a **consistency proof**, that a newer tree extends an older one without removing
  or changing anything.

With signed heads over an append-only tree, a fork, rollback or equivocation
produces a signature or proof that fails to verify, and anyone running the verifier
sees it.

## Trust model

A verifier trusts the log's ML-DSA-44 public key, plus any signing key that the
log's offline root key authorises in the root-signed key set (`/v1/key-set.json`,
#754, #1454); both the signing key and the root key are compiled in. It checks signed
tree heads and Merkle proofs against those keys, and trusts nothing else: not the server, not the
Turso database, not the host serving the files, not the network.

The files can be served from any CDN, bucket or compromised machine. Changing one
byte of an entry, proof or STH makes a check fail and the verifier exit non-zero.
The read API needs no credentials.

What the verifier must get right is the key. It is small and stable: `pollis-verify`
and the desktop app compile it in, and it is published in `SECURITY.md`,
`README.md`, the `pollis-verify` release notes and on pollis.com, so it can be
cross-checked.

## Three trees

The log is three independent Merkle trees, each with its own entries, STHs and
history:

- **MLS commit log**: one leaf per commit. Most of this document is about this tree.
- **Account-key directory**: one leaf per account identity-key version (`user_id`,
  `identity_version`, the ML-DSA-44 account public key). Anyone can check that a
  user's key history is append-only and that `identity_version` only increases, so
  a key cannot be silently swapped or a revoked key replayed. Check one user with
  `pollis-verify account <base-url> <user_id>`. The desktop app runs the same
  `verify_account` function: `self_audit_account_key` for the signed-in user and
  `audit_peer_account_key` for a TOFU-pinned contact.
- **Released binaries**: one leaf per release artifact (`release_tag`, `platform`,
  `arch`, `bundle`, `layer`, content hashes and the build recipe; never the binary
  itself). Anyone can check that the binary they run is the one logged for its tag,
  and that its reproducible payload was logged too. Check one release with
  `pollis-verify release <base-url> <tag>`, which runs the same `verify_release`
  function that produces the published `/verify/release/<tag>` report. Since #1250
  the same tree also holds every **mobile over-the-air update** and rollback (see
  "Mobile OTA updates in the binaries tree" below), and since #1848 each desktop
  release's **SBOM** (see "Release documents in the binaries tree").

One key signs all three, under different domain-separation contexts
(`pollis-verifiable-log:sth:v2:commit-log-2`, `…:sth:v2:account-keys`,
`…:sth:v2:binaries`). A head signed for one tree does not verify as a head for
another. The commit log is served under `/v1/commit-log-2/...`, the account keys
under `/v1/account-keys/...`, and the binaries under `/v1/binaries/...`.

`commit-log-2` superseded the first tree on 2026-10-29; `/v1/commit-log-reset.json`
lists the retired heads.

## Components

Each crate has a README with the details.

| Component | Crate / path | Role |
|---|---|---|
| **monitor** | [`verifiable-log`](../verifiable-log/README.md) | Merkle-log core, and an offline CLI that verifies a downloaded bundle with no network or database. |
| **builder** | [`verifiable-log-builder`](../verifiable-log-builder/README.md) | Appends new `mls_commit_log` rows to the commit log's persisted leaves (`append-commit-log`), reads the account-key and binaries sources, and writes signed bundles. Hashes each commit blob and discards the bytes. |
| **serve** | [`verifiable-log-serve`](../verifiable-log-serve/README.md) | Turns signed bundles into the static `/v1/...` read API. Also a dev server and a `live` server, both with the dynamic `/verify/group/<id>` endpoint. |
| **pollis-verify** | [`verifiable-log-serve`](../verifiable-log-serve/README.md) | The auditor CLI: `remote` (all three trees), `group`, `account`, `release`. |
| **website explorer** | [`website/transparency.html`](../website/transparency.html) | Runs `pollis-verify group`'s checks in the browser (`website/transparency.js`, a JS port of `group.rs`) against the static `/v1/` files, trusting only the pinned key. A convenience, not a trust anchor. |

Data flows one way: the builder signs bundles, `serve generate` turns them into the
static tree, and `monitor`, `pollis-verify` and the explorer check it. Merkle, proof,
signature and invariant logic lives only in `verifiable-log` (and the tenant
invariants in `verifiable-log-builder`), so the CLI and the HTTP endpoint run the
same code and reach the same verdict on the same input. The website explorer is the
one reimplementation: `website/transparency.js` ports `verify_group` to JavaScript
(ML-DSA-44 via `@noble/post-quantum`) and must produce the identical `GroupReport`.

### The commit-log invariant

On replay, the commit log enforces three rules per grouping key. They are the
public counterpart of the database's `UNIQUE(conversation_id, generation, epoch)`
constraint. The grouping key is the leaf's `conversation_pseudonym`,
which changes every window, so a public replay checks these rules within a window;
a member who knows the real `conversation_id` can check across windows (see
[Windowed pseudonyms](#windowed-pseudonyms-in-the-commit-log-701)).

- **No fork**: no two commits share `(conversation_pseudonym, generation, epoch)`.
- **No regression or replay**: `(generation, epoch)` strictly increases,
  lexicographically, in `seq` order.
- **New lineages start at epoch 0**: the first commit of a generation higher than
  any seen before must be at epoch 0.

`generation` is the cipher-suite lineage (#454 P4). MLS fixes the ciphersuite when
a group is created, so moving a conversation to a new suite creates a successor
group whose epoch restarts at 0. (#454 used this to move conversations from the
classic suite to the post-quantum one. #669 retired the classic suite, but the
mechanism stays because the PQ suite's code point is provisional.) A plain epoch
counter would read every migration as a regression; ordering by
`(generation, epoch)` fixes that. The third rule is needed because the second alone
would let a server open generation N+1 at an arbitrary epoch and pass a fork off as
a migration. Together they ensure a conversation's lineages are contiguous,
non-overlapping and totally ordered. `generation` is left out of the leaf when it is
0, so leaves written before P4, and conversations that never migrated, are
unchanged.

A fork or regression in the source data aborts the build, and the verifiers check
again on replay.

### The commit log of record: persisted leaves, not the DS table (#1579)

The published tree, not the Delivery Service's `mls_commit_log` table, is the
commit log of record. The DS prunes that table below a retention floor (#539,
`pollis-delivery/src/commit.rs`), so the table only ever holds a recent window of
the log, and the tree is never rebuilt from it.

So the commit log works like the binaries tree, whose records live in
`internal/binary-records.json`:

- The leaves the publisher has published are kept in the transparency bucket at
  `internal/commit-log-2-leaves.json`, a JSON array of entries in exactly the shape of
  `/v1/commit-log-2/entries.json`. Each publish also keeps a copy at
  `internal/commit-log-2-leaves/<size>.json`, because R2 has no object versioning.
- Each publish runs `builder append-commit-log`. It appends only the rows whose `seq`
  is above the last persisted leaf, writes the file back to R2 before any head that
  covers it is published, and then `builder build --commit-leaves` signs heads over
  the whole file under the `commit-log-2` context (`--commit-tree commit-log-2`). A
  pruned row is simply not read again, and its leaf stays where it was published.
- It fails closed. The persisted leaves must reproduce the served `latest.json`
  (its size and root). A missing, shrunk or rewritten file, a live row below the
  head that was never logged or that has changed, or a served per-size head with a
  different root stops the commit-log publish. The account-key and binaries trees
  still publish, and the job fails.
- Both invariant passes run on the whole appended stream: the public,
  within-window check on the pseudonymous leaves, and the full-strength check on
  real ids. For the full-strength check, a persisted leaf is matched back to its
  real conversation whenever a live row's conversation derives that leaf's
  pseudonym for its window, so a new commit is still checked against history whose
  rows have been pruned.
- Seeding is a one-shot, explicit act (`append-commit-log --seed-from-live` or
  `--seed-from <entries.json>`, or the workflow's `seed_commit_log` input). It is
  refused when the file already exists, and when a head has already been served it
  still has to reproduce that head. `commit-log-2` was seeded once, from the live
  table, when it started.

The persisted file holds only the published, pseudonymous leaves, never a real
`conversation_id` or `sender_id`, so keeping it longer than the DS keeps its rows
discloses nothing the public log does not.

## Windowed pseudonyms in the commit log (#701)

Commit-log leaves carry windowed pseudonyms, never the real `conversation_id` or
`sender_id`, so the public entries do not map which groups exist, who commits to
them or which groups each user belongs to. A leaf holds no message content or key
material either, only `sha256(commit_data)`.

### Scheme

```
window                 = seq / PSEUDONYM_WINDOW_SIZE
conversation_pseudonym = SHA256(dom_c || conversation_id || window)
sender_pseudonym       = SHA256(dom_s || conversation_id || sender_id || window)
```

`dom_c` and `dom_s` are fixed domain tags, and every field is length-prefixed.

- **No new secret.** The only input an outsider lacks is `conversation_id`, which
  members and the server already hold. No key is created, so there is nothing new to
  protect. That matters because custody of the log's existing key is already hard
  (`docs/sth-signing-key-custody.md`).
- **Senders are bound to the conversation.** The sender pseudonym includes
  `conversation_id`, so one user has unrelated pseudonyms in different groups.
  Following a person across groups is not possible at all, even within a window.

A member re-derives `conversation_pseudonym` for each window and recovers the whole
history. `pollis-verify group <base-url> <conversation_id>` and
`GET /verify/group/<conversation_id>` do this, and so does the website explorer, in
the browser. A leaf's window is computed from its
published `seq`, so no extra field is needed.

### Choosing the window

Every invariant rule is per conversation, so an outsider can check them only for
entries they can group together. A pseudonym that never changes would give no
privacy over time; a pseudonym that changes on every entry would leave nothing to
check. The window is the compromise: within a window, entries share a pseudonym and
an outsider can check fork, monotonicity and lineage rules with no extra knowledge;
across windows, the pseudonym changes and an outsider cannot link them.

`PSEUDONYM_WINDOW_SIZE` (currently 1024) is the one parameter. A larger window gives
the public check more to work with but keeps a conversation linkable for longer; a
smaller one does the reverse and adds more boundaries (see below).
The window is over the global `seq`, which is already published, so it adds no new
field and no timing data. The builder deliberately does not read or publish each
commit's `created_at`. The log grows by one leaf per commit, not per message, so
1024 keeps a busy conversation's commits together within a window while splitting a
long-lived group across many. The value is part of the leaf encoding and can only
change at a full republish. A calendar-based window would give an "unlinkable after
N days" guarantee, at the cost of publishing a coarse timestamp; switching is a
local change at a future republish if that is wanted.

### Forks across a window boundary

A fork or regression whose two branches fall in different windows has two different
pseudonyms, so a public replay grouped by pseudonym cannot see it. This cannot be
fixed for outsiders: anything that let an outsider link two windows would also undo
the unlinkability. Pollis does not add a linking commitment. Instead:

- **Members still catch it.** Knowing `conversation_id`, they group all windows
  under one key and check the full invariant; that is why `verify_group` takes the
  real id. The serve tests plant a cross-boundary regression and confirm a member
  detects it.
- **The builder never emits one.** It has the real ids, so `build_bundle` runs the
  invariant over leaves keyed on `conversation_id` first (aborting on any fork,
  including across boundaries), then over the published leaves. Only a replaced or
  compromised publisher could publish such a fork, and members would catch it.

So a cross-boundary fork is invisible to an outsider-only replay but visible to
members and stopped by an honest builder. This is also recorded in
`docs/metadata-retention-policy.md` §6.

### Format versions

Served manifests carry `format_version` (`verifiable_log_serve::bundle::FORMAT_VERSION`,
currently `2`), and `pollis-verify` reads it before anything else:

- a log newer than the binary exits `2` with *"your pollis-verify is too old for
  this log — upgrade it"*, separate from a verification failure (`1`);
- `remote` accepts any format up to its own, including a legacy manifest with no
  `format_version` (treated as `0`);
- `group`, which decodes leaf contents, also refuses a log below
  `MIN_LEAF_FORMAT_VERSION` (currently `2`) with an error, rather than report an
  empty result.

Use `pollis-verify` `v0.10.0` or later.

The website explorer applies the same two gates in the browser
(`FORMAT_VERSION` / `MIN_LEAF_FORMAT_VERSION` in `website/transparency.js`), so a
format change must update that file in the same change.

### What an outside observer can learn

**Still visible:** the total number of commits, the tree's growth over time (STH
timestamps), and, within one window, that some conversation had a run of commits
with a given epoch progression and whether that run is fork-free.

**Not visible:** which conversation a pseudonym belongs to, whether two
windows are the same conversation, whether pseudonyms in two groups are the same
user, and so the activity map across time and groups. Building that map now needs a
`conversation_id`, which means being a member, or being the server, which already
has the data. The log protects against the server lying, not against it knowing.

### Out of scope: `user_id` in the account-key tree

The account-key tree publishes the real `user_id`, on purpose: key transparency means looking up a specific user's key history by
identity, and pseudonymizing the id would break that.

### The binaries invariant

On replay, the binaries tree enforces four rules over the whole tree:

- **No silent re-issue**: no two leaves share
  `(release_tag, platform, arch, bundle, layer)` with different `artifact_sha256`.
  A re-release needs a new tag.
- **Monotonic releases**: once a newer tag has appeared, an older tag cannot appear
  again.
- **Payload/signed pairing**: every `layer:"signed"` leaf (a signed or notarized,
  non-reproducible wrapper) and every `layer:"exe"` leaf (the installed main executable) must have an earlier `layer:"payload"` leaf with the
  same `payload_sha256`, so the reproducible content of every signed artifact is
  itself logged.
- **Release-document shape** (#1848): a leaf with `bundle` `sbom` is a `payload`
  leaf with `artifact_sha256 == payload_sha256`, platform and arch `any`, and an
  `artifact_name` ending in `.cdx.json`.

A violation in the source records aborts the build, and `pollis-verify release`
checks the invariant again, so a forked or unpaired tree is rejected even if it is
correctly signed.

### Release documents in the binaries tree (#1848)

Every desktop release publishes a CycloneDX SBOM next to its installers. It is a
SLSA provenance subject with a keyless cosign signature (the `provenance` job),
and `attest-and-log` logs it as a leaf after the release's binaries, under the
same tag. A later, quieter SBOM cannot replace the one a release shipped without
the swap showing as a fork. The leaf uses the same frozen `BinaryRecord`.
Neither the leaf contract nor any `Layer` changed, so every verifier already
deployed decodes and replays them:

| field | value |
|---|---|
| `release_tag` | the desktop release's tag |
| `platform`, `arch` | `any` (one document covers every platform's build) |
| `bundle` | `sbom` |
| `artifact_name` | `pollis-<tag>.cdx.json`, as published at `cdn.pollis.com/releases/<tag>/` |
| `layer` | `payload` (artifact = payload; nothing re-signs a document after it is logged) |
| `toolchain` | the `sbom` job's own recipe (`pollis-<tag>-sbom.toolchain.json`), since that job ran the generator |
| `provenance_uri` | `cdn.pollis.com/releases/<tag>/<artifact_name>.intoto.jsonl` |

Releases cut before #1848 have no SBOM leaf and verify exactly as before.
The copy under `releases/oss/desktop/<tag>/` is the same bytes under its
`pollis-desktop-<tag>` name, and `oss-evidence.yml` refuses to republish a
desktop version whose SBOM is logged.

### Mobile OTA updates in the binaries tree (#1250)

An over-the-air update replaces the mobile app's JS — the code that handles
plaintext — without a store review, so it is logged exactly like a release:
`mobile-ota-release.yml` appends its leaves to the publisher's records **before** it
goes live, then flips the live pointer and runs the publish that signs a head over
them, so `pollis-verify release` can check it a few minutes after it is live. It uses the same frozen `BinaryRecord`; nothing in the leaf contract or
the verifier changed:

| field | value |
|---|---|
| `release_tag` | `mobile-ota-<group id>` — one per publish, republish or rollback |
| `platform` | `ios` / `android` |
| `arch` | the **runtime version** (native fingerprint) the update targets — the binary it can run on |
| `bundle` | `ota-manifest` (the exact signed manifest bytes: update id, runtime version, every asset's sha256), `ota-bundle` (the launch bundle), `ota-assets` (the sorted `sha256  key.ext` asset list), or `ota-directive` (a signed `rollBackToEmbedded`) |
| `layer` | `payload` (artifact = payload; nothing re-signs the bytes after they are logged) |
| `provenance_uri` | `cdn.pollis.com/releases/mobile-ota/<tag>/<artifact_name>.intoto.jsonl` (SLSA, keyless) |

What this makes detectable: an update served to anyone that is not in the log
(fetch the manifest from `api.pollis.com/updates/api/manifest` with your platform and runtime
version, hash the manifest part, look for it), and a targeted update (the log is
the same for everyone, and every group is one tag). What it does not prevent: the
update server withholding an update from someone — that is availability, and the
app's code-signing check, not the log, is what stops a forged one.

## The static read API

Every artifact is fixed once written: the STH for `tree_size = N` never changes, and
neither does any proof for a given `(leaf, tree_size)`. So the API is a directory of
precomputed JSON files served as static assets, with URL paths matching file paths.

| URL | Contents | Cache |
|---|---|---|
| `/v1/public_key.json` | the log's ML-DSA-44 public key (1312 bytes, 2624 hex chars) | immutable |
| `/v1/index.json` | manifest, including `format_version` | short |
| `/v1/sth/latest.json` | newest STH | short |
| `/v1/sth/<tree_size>.json` | STH at that size | immutable |
| `/v1/entries.json` | full ordered `[Entry]` | short (grows with every append) |
| `/v1/entries/<index>.json` | one entry | immutable |
| `/v1/proof/inclusion/<tree_size>/<leaf_index>.json` | inclusion proof | immutable |
| `/v1/proof/consistency/<first>-<second>.json` | consistency proof | immutable |

The table shows the layout under `/v1/`, where the retired first commit-log tree
stays served unchanged. The live commit log, `commit-log-2`, uses the same layout
under `/v1/commit-log-2/...`, and the account-key and binaries trees use it under
`/v1/account-keys/...` and `/v1/binaries/...`. `/v1/commit-log-reset.json` lists the
retired heads, and the root-signed key set is at `/v1/key-set.json`; both are
immutable. The publisher also writes precomputed reports at
`/verify/account/<user_id>` and `/verify/release/<tag>`. In production, "short"
means `Cache-Control: public, max-age=300`, and it applies to `latest.json`,
`index.json`, `entries.json` and those reports; everything else is cached as immutable.

There is no static `/verify/group/<id>`. The dynamic endpoint is served by
`serve serve` and `serve live` (dev and self-hosting) and runs the same
`verify_group` code as the CLI. Production serves only the static files, and the
website explorer does not use the endpoint: it fetches `index.json`,
`public_key.json`, `sth/latest.json`, `entries.json` and the selected inclusion
proofs and verifies them in the browser. The
wire shapes (`Entry`, `Sth`, `InclusionProof`, `ConsistencyProof`) are defined in
[`verifiable-log/README.md`](../verifiable-log/README.md).

## Rotating the signing key

The custody options, design and step-by-step ceremony are in
[`sth-signing-key-custody.md`](sth-signing-key-custody.md). That is the runbook;
follow it, not this summary.

Three steps are easy to get wrong:

- Dispatch `transparency-publish.yml` with **`full_resync: true`**. Re-signed
  artifacts are the same byte length as the ones they replace, so the default
  `--size-only` sync skips them and the rotation silently does not take effect.
  `full_resync` also re-signs the midpoint STHs with a new timestamp, which
  rewrites published per-size STHs. Never use it to repair anything else.
- Proofs and per-index entries (`proof/*` and `entries/*` in all three trees)
  are unsigned and derived purely from the entry list, so they sync in their
  own pass **without** `--size-only` on every run. A proof's length depends only
  on (tree size, leaf index), so a size-only sync would keep a stale file whose
  replacement has the same length.
- The public key is copied into several places that must all agree. The list is
  kept in `scripts/check-pinned-log-key.py`, not here. The script treats `pollis-core/src/commands/transparency.rs` as the source of
  truth and fails CI if any copy disagrees or an unregistered copy appears. Run it
  before opening the rotation PR:

  ```bash
  python3 ./scripts/check-pinned-log-key.py
  ```

  Change every copy in one commit. If you add a new copy, register it in the
  script's `COPIES` table in the same commit; the check fails until you do.

**Overlap window (#700, #740).** A retiring key can be published in `retired_keys`
with a `not_after`, and verifiers accept heads from it until then. The window length
for a rotation is set in the ceremony (custody doc §5 and §9).

**The retired commit-log tree after a rotation.** The retired tree is frozen,
so `full_resync` never re-signs it, and its heads and `/v1/commit-log-reset.json`
stay signed by the current key. `pollis-verify` checks them against its pinned keys, so
the next rotation has to keep today's key available for verifying the retired tree
(as a pinned key that signs nothing new); dropping it would turn the RETIRED report
into a signature failure.
