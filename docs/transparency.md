# Key Transparency: the Pollis verifiable log

Pollis publishes an append-only transparency log of every MLS commit, every account
identity-key version and every released binary, so that anyone can check that the
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

A verifier trusts the log's ML-DSA-44 public key. It checks signed tree heads and
Merkle proofs against that key, and trusts nothing else: not the server, not the
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
  function that produces the published `/verify/release/<tag>` report.

One key signs all three, under different domain-separation contexts
(`pollis-verifiable-log:sth:v2`, `…:sth:v2:account-keys`, `…:sth:v2:binaries`). A head
signed for one tree does not verify as a head for another. The commit log is served
under `/v1/...`, the account keys under `/v1/account-keys/...`, and the binaries
under `/v1/binaries/...`.

## Components

Each crate has a README with the details.

| Component | Crate / path | Role |
|---|---|---|
| **monitor** | [`verifiable-log`](../verifiable-log/README.md) | Merkle-log core, and an offline CLI that verifies a downloaded bundle with no network or database. |
| **builder** | [`verifiable-log-builder`](../verifiable-log-builder/README.md) | Reads `mls_commit_log` (and the account-key and binaries sources) and writes signed bundles. Hashes each commit blob and discards the bytes. |
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
constraint. Since #701 the grouping key is the leaf's `conversation_pseudonym`,
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

## Windowed pseudonyms in the commit log (#701)

Before #701, commit-log leaves contained the real `conversation_id` and `sender_id`.
Anyone could download `/v1/entries.json` and map which groups exist, who commits to
them, how often, and which groups each user belongs to. No message content or key
material was exposed (the leaf holds only `sha256(commit_data)`), but the social
graph was. #701 removes it.

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
smaller one does the reverse and adds more boundaries (see the limitation below).
The window is over the global `seq`, which is already published, so it adds no new
field and no timing data. The builder deliberately does not read or publish each
commit's `created_at`. The log grows by one leaf per commit, not per message, so
1024 keeps a busy conversation's commits together within a window while splitting a
long-lived group across many. The value is part of the leaf encoding and can only
change at a full republish. A calendar-based window would give an "unlinkable after
N days" guarantee, at the cost of publishing a coarse timestamp; switching is a
local change at a future republish if that is wanted.

### Limitation: forks across a window boundary

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

### Rollout: landed at the key-rotation republish (#672 / #699)

Leaf bytes are hashed into the tree, so re-encoding existing leaves invalidates
every published root and every cached inclusion proof. Pseudonymizing history means
republishing the tree from scratch. That happened as part of the republish for the
ML-DSA-44 key rotation (#672 / PL-11, executed by #699;
`docs/sth-signing-key-custody.md` §7), which rebuilt all three trees under the
`sth:v2` contexts. The account-key and binaries trees were not changed by #701.

### Old `pollis-verify` binaries after the republish

The republish changed the wire format. Measured against the republished log, a
pre-#701 binary behaves like this:

| subcommand | result | why |
|---|---|---|
| `remote` | still passes | The manifest's `conversations` field was always `#[serde(default)]`, so its removal is not an error, and `remote` never decodes commit leaves (it replays with the generic uniqueness invariant). |
| `group` | wrong: reports `Found: no` for a conversation that is present | It decodes leaves as the old `CommitLeaf`, which requires `conversation_id`. Pseudonymous leaves fail to decode, the error is swallowed, and nothing is selected. |
| `account`, `release` | unaffected | Separate trees; neither the manifest nor the leaf encoding changed. |

Old binaries cannot be fixed, but the next format change will not repeat this.
Served manifests now carry `format_version` (`verifiable_log_serve::bundle::FORMAT_VERSION`,
currently `2`), and `pollis-verify` reads it before anything else:

- a log newer than the binary exits `2` with *"your pollis-verify is too old for
  this log — upgrade it"*, separate from a verification failure (`1`);
- `remote` accepts any format up to its own, including a legacy manifest with no
  `format_version` (treated as `0`);
- `group`, which decodes leaf contents, also refuses a log below
  `MIN_LEAF_FORMAT_VERSION` (currently `2`) with an error, rather than report an
  empty result.

After the republish, upgrade any `pollis-verify` older than `v0.6.0`.

The website explorer applies the same two gates in the browser
(`FORMAT_VERSION` / `MIN_LEAF_FORMAT_VERSION` in `website/transparency.js`), so a
format change must update that file in the same change.

### What an outside observer can learn

**Still visible:** the total number of commits, the tree's growth over time (STH
timestamps), and, within one window, that some conversation had a run of commits
with a given epoch progression and whether that run is fork-free.

**No longer visible:** which conversation a pseudonym belongs to, whether two
windows are the same conversation, whether pseudonyms in two groups are the same
user, and so the activity map across time and groups. Building that map now needs a
`conversation_id`, which means being a member, or being the server, which already
has the data. The log protects against the server lying, not against it knowing.

### Out of scope: `user_id` in the account-key tree

The account-key tree still publishes the real `user_id`. #701 leaves it alone on
purpose: key transparency means looking up a specific user's key history by
identity, and pseudonymizing the id would break that.

### The binaries invariant

On replay, the binaries tree enforces three rules over the whole tree:

- **No silent re-issue**: no two leaves share
  `(release_tag, platform, arch, bundle, layer)` with different `artifact_sha256`.
  A re-release needs a new tag.
- **Monotonic releases**: once a newer tag has appeared, an older tag cannot appear
  again.
- **Payload/signed pairing**: every `layer:"signed"` leaf (a signed or notarized,
  non-reproducible wrapper) must have an earlier `layer:"payload"` leaf with the
  same `payload_sha256`, so the reproducible content of every signed artifact is
  itself logged.

A violation in the source records aborts the build, and `pollis-verify release`
checks the invariant again, so a forked or unpaired tree is rejected even if it is
correctly signed.

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

The account-key and binaries trees use the same layout under `/v1/account-keys/...`
and `/v1/binaries/...`. The publisher also writes precomputed reports at
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
follow it, not this summary. The last rotation was #732, which replaced the
original seed with new key material.

Two steps are easy to get wrong:

- Dispatch `transparency-publish.yml` with **`full_resync: true`**. Re-signed
  artifacts are the same byte length as the ones they replace, so the default
  `--size-only` sync skips them and the rotation silently does not take effect.
- The public key is copied into several places that must all agree. The list is
  kept in `scripts/check-pinned-log-key.py`, not here: an earlier version of this
  paragraph had the wrong count and named constants that no longer existed (#945).
  The script treats `pollis-core/src/commands/transparency.rs` as the source of
  truth and fails CI if any copy disagrees or an unregistered copy appears. Run it
  before opening the rotation PR:

  ```bash
  python3 ./scripts/check-pinned-log-key.py
  ```

  Change every copy in one commit. If you add a new copy, register it in the
  script's `COPIES` table in the same commit; the check fails until you do.

**Overlap window (#700).** The #732 rotation had no overlap window, so it was a flag
day: auditors holding cached pre-rotation heads saw it as equivocation and had to
re-pin from the announcement. The key-set support has since shipped (#740): a
retiring key can be published in `retired_keys` with a `not_after`, and verifiers
accept heads from it until then. Only one key is pinned today, so no window is in
force, and the window length for the next rotation is not decided yet (custody doc
§5 and §9).
