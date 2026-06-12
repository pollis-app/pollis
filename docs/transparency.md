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
an Ed25519 signature over `(tree_size, root_hash, timestamp)`. From that one
signed root, anyone can demand:

- an **inclusion proof** that a specific commit really is in the tree, and
- a **consistency proof** that a newer tree is an append-only extension of an
  older one (nothing was deleted or rewritten between two heads).

Because the tree is append-only and the heads are signed, the server cannot fork,
roll back, or equivocate without producing a signature or a proof that fails to
verify — and that failure is detectable by anyone running the verifier.

## Trust model (the load-bearing idea)

> A verifier trusts **only** the log's published Ed25519 **public key**, plus the
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
> issue #330, now covering **both** published trees (the MLS commit log and the
> account-key directory — see below). The end-to-end threat-model writeup lives
> elsewhere and is out of scope here.

## Two domain-separated trees

The log actually publishes **two independent Merkle trees**, each with its own
entries, its own Signed Tree Heads, and its own append-only history:

- **the MLS commit log** — every membership/key-change commit, the tree described
  throughout this document; and
- **the account-key directory** — one leaf per account identity-key version
  (`user_id`, `identity_version`, the Ed25519 account public key), so anyone can
  audit that a user's published key history is append-only and that
  `identity_version` only ever increases (no silent key substitution, no replay
  of a revoked key).

The two trees are **never interleaved**. They are signed by the same Ed25519 key
but under **different domain-separation contexts** (`…:sth:v1` for the commit log,
`…:sth:v1:account-keys` for the account keys), so an STH minted for one tree
**cannot** be replayed as the other's — a verifier checks each head under its own
context. The commit-log tree and every one of its `/v1/...` bytes are exactly as
before; the account-key tree lives entirely under `/v1/account-keys/...`.

## The four pieces

The system is four small Rust/JS components, each building on the one before it.
Each has its own README with the full detail; this is the map.

| Piece | Crate / path | Role |
|-------|--------------|------|
| **monitor** | [`verifiable-log`](../verifiable-log/README.md) | The Merkle-log core **and** the fully offline verifier CLI. Verifies a downloaded bundle with no network and no database. |
| **builder** | [`verifiable-log-builder`](../verifiable-log-builder/README.md) | Reads the real `mls_commit_log` from Turso/libSQL and emits a **signed bundle** the monitor verifies byte-for-byte. Hashes each commit blob and drops the raw bytes — they are never stored or logged. |
| **serve** | [`verifiable-log-serve`](../verifiable-log-serve/README.md) | Turns a signed bundle into the immutable static `/v1/...` read API, plus a dev HTTP server and the `/verify/group/<id>` endpoint the explorer calls. |
| **pollis-verify** | [`verifiable-log-serve`](../verifiable-log-serve/README.md) | The auditor CLI shipped to security analysts: a whole-log HTTP verifier (`pollis-verify remote`) and a per-conversation verifier (`pollis-verify group`). |
| **website explorer** | [`website/transparency.html`](../website/transparency.html) | A browser convenience demo that calls the serve layer's `/verify/group/<id>` endpoint and visualizes the result. It is **not** the trust anchor — the trustworthy path is running the tool yourself. |

Data flows one direction: `mls_commit_log` → **builder** signs a bundle →
**serve** generates the static tree → **monitor** / **pollis-verify**
(`remote` / `group`) / the explorer check it. The core Merkle, proof, signature, and
invariant logic lives in `verifiable-log` and is **reused, never reimplemented**,
by every layer — so the CLI, the HTTP endpoint, and the website can never reach
different verdicts for the same input.

### The commit-log invariant

Beyond raw Merkle inclusion, the log enforces two rules per conversation when the
commits are replayed (the publicly-auditable mirror of the live DB's
`UNIQUE(conversation_id, epoch)` constraint):

- **No fork** — no two commits share the same `(conversation_id, epoch)`.
- **No epoch regression / replay** — within a conversation, `epoch` strictly
  increases in `seq` order.

A fork or regression in the source data **aborts the build** rather than producing
a bundle that hides it, and the verifiers re-check it independently on replay.

## The static read API (`/v1/...`)

Everything a transparency log serves is deterministic and immutable: the STH for
`tree_size = N` never changes; an inclusion or consistency proof for a given
`(leaf, tree_size)` is fixed forever. So the serve layer is **not** a query
service over a database — it is a precomputed directory of immutable JSON files
served as plain static assets. The URL path mirrors the file path exactly.

| URL | Contents | Cache |
|-----|----------|-------|
| `/v1/public_key.json` | the log's Ed25519 public key | immutable |
| `/v1/index.json` | discovery manifest | short (`no-cache`) |
| `/v1/sth/latest.json` | newest STH | short (`no-cache`) |
| `/v1/sth/<tree_size>.json` | STH at that tree size | immutable |
| `/v1/entries.json` | full ordered `[Entry]` | immutable |
| `/v1/entries/<index>.json` | one entry | immutable |
| `/v1/proof/inclusion/<tree_size>/<leaf_index>.json` | inclusion proof | immutable |
| `/v1/proof/consistency/<first>-<second>.json` | consistency proof | immutable |

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
