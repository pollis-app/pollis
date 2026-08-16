# verifiable-log-builder

Slice 2 of the Key Transparency work (issue #330). Reads real MLS commit data
from a Turso/libSQL database and turns it into the signed **monitor bundle** that
slice 1's [`verifiable-log`](../verifiable-log) `monitor` CLI verifies.

This crate depends on `verifiable-log` for all Merkle / STH / proof logic — and,
since #875, for the bundle *type* and the per-tree STH contexts too; it does not
restate any of them. It adds only: a DB reader, the tenant leaf encodings and
invariants, and the bundle builder/signer.

## What it does

1. **Reads `mls_commit_log`** (ordered by `seq` ascending) over libSQL — remote
   Turso or a local SQLite file. Only the structural columns plus `commit_data`
   are read; each blob is hashed to `sha256` **as it is read and the raw bytes
   are immediately dropped** — they are never returned, logged, or persisted.
   The auth token is read from the environment and never logged.
2. **Appends every commit** to a `verifiable_log::VerifiableLog` with the
   `CommitLogInvariant` registered for the `mls-commit-log` tenant.
3. **Signs Signed Tree Heads** with an ML-DSA-44 key and **emits a JSON bundle**
   that the `monitor` CLI consumes byte-for-byte.

## Canonical leaf encoding (frozen contract extension)

This extends `verifiable-log`'s frozen leaf encoding for the `mls-commit-log`
tenant. The tenant's `Entry.data` is the **compact JSON** of:

```
{"conversation_pseudonym":<hex>,"epoch":<u64>,"sender_pseudonym":<hex>,"seq":<i64>,"commit_sha256":<hex string>}
```

with fields in **exactly that order** (serde emits struct fields in declaration
order, no insignificant whitespace), so the encoding is deterministic and stable.
`generation` is inserted after `conversation_pseudonym` when non-zero (#454 P4).

- `commit_sha256` is `sha256(commit_data)`, lowercase hex (32 bytes). The leaf
  commits to the commit bytes **without storing the raw blob**.
- `conversation_pseudonym` / `sender_pseudonym` are **windowed pseudonyms** (#701),
  not the raw `conversation_id` / `sender_id`. They are keyless SHA-256 over the
  real id(s) and a window index (`window = seq / PSEUDONYM_WINDOW_SIZE`):
  `H(dom || conversation_id || window)` and
  `H(dom || conversation_id || sender_id || window)`. The pseudonym rotates at each
  window boundary, so a third party who only sees the public log cannot build a
  longitudinal activity map, while anyone who knows the real `conversation_id` (a
  member) re-derives every window's pseudonym and verifies their full history. See
  `docs/transparency.md` for the scheme and its boundary residual.
- **This is a frozen-contract change:** leaf bytes are Merkle-hashed, so it takes
  effect only at a full republish — the #672 / PL-11 ML-DSA-44 key rotation
  (executed by #699) under the `sth:v2` contexts. #701 changes *what that
  republished tree contains*.
- `sender_pseudonym` is recorded so a later slice can add cryptographic
  authorization. **This slice does not validate it** — that needs MLS group state
  and is out of scope (see below).

The full leaf hashed by the core is then `verifiable-log`'s own encoding around
this payload: `SHA-256(0x00 || len(tenant) BE || "mls-commit-log" || data)`.

## Commit-log invariant (the auditable form of #357)

`CommitLogInvariant` is registered for the tenant and consulted on every append.
Per **grouping key** (the leaf's `conversation_pseudonym`) it enforces:

- **(a) no fork** — no two entries share the same
  `(conversation_pseudonym, generation, epoch)`;
- **(b) no epoch regression / replay** — within a grouping key, `(generation,
  epoch)` strictly increases in `seq` order;
- **(c) a lineage opens at epoch 0** (#454 P4).

Because commits are appended in `seq` order, the candidate always has the largest
`seq` seen so far for its grouping key, so (b) is exactly "strictly greater than
every prior `(generation, epoch)`". A fork or regression in the source data
**aborts the build** rather than producing a bundle that hides it. (#357 enforces
this with a live `UNIQUE INDEX (conversation_id, generation, epoch)`; this is its
global, publicly-auditable mirror.)

**Windowing and the two integrity passes (#701).** Grouping on the *published*
pseudonym is the check an outside replayer runs, and it is only *within-window*: a
fork whose branches straddle a window boundary carries two different pseudonyms and
so escapes it (the residual — see `docs/transparency.md`). So `build_bundle` runs
`CommitLogInvariant` **twice**: first over leaves keyed on the *real*
`conversation_id` (`to_identity_leaf`, never published), which catches any fork or
regression at full strength including across boundaries; then over the published
pseudonymous leaves. A member re-runs the full-strength check at read time because
they know the real id (`verifiable-log-serve`'s `verify_group`).

The emitted bundle also lists `mls-commit-log` in `enforce_unique`, so the
monitor's own replay re-checks leaf uniqueness independently.

## Bundle shape

Exactly the frozen schema in `verifiable-log/README.md` /
`verifiable-log/fixtures/example.json`: top-level `public_key`, `sths`, `entries`,
`enforce_unique`, `inclusion`, `consistency`. The builder emits the full ordered
`entries`, an STH over the final tree (plus a midpoint STH when there are ≥2
entries), an inclusion proof for **every** entry against the final STH, and a
consistency proof between the midpoint and final STHs (proving the log only
appended between the two heads).

STH timestamps come from `--timestamp` (ms since epoch), **never the system
clock**, so output is deterministic and testable.

## CLI

```bash
# Mint a throwaway dev keypair (hex). Real key custody is a later slice.
# `VLOG_SIGNING_KEY` is still 32 hex-encoded bytes after the move to
# ML-DSA-44 (#668): an ML-DSA-44 private key *is* its 32-byte seed, so key
# custody is byte-identical to the Ed25519 era. Only the public key (1312
# bytes) and the signature (2420) grew.
cargo run -p verifiable-log-builder --bin builder -- keygen

# Build a signed bundle from a local fixture DB (no network).
VLOG_SIGNING_KEY=<32-byte hex> \
  cargo run -p verifiable-log-builder --bin builder -- \
  build --db ./commits.db --out bundle.json --timestamp 1700000000000

# ...or from remote Turso (auth token from the environment):
TURSO_DATABASE_URL=libsql://... TURSO_AUTH_TOKEN=... VLOG_SIGNING_KEY=... \
  cargo run -p verifiable-log-builder --bin builder -- \
  build --out bundle.json --timestamp 1700000000000

# Build the released-binaries tree (binary transparency) from a JSON file of
# BinaryRecords — its own tree, STH signed under the `…:sth:v2:binaries` context.
VLOG_SIGNING_KEY=<32-byte hex> \
  cargo run -p verifiable-log-builder --bin builder -- \
  build-binaries --binaries-in records.json --out binaries-bundle.json \
  --timestamp 1700000000000

# Verify with the slice-1 monitor. Each tree is domain-separated, so tell the
# monitor which one it is checking (`--tree` defaults to `commit-log`).
cargo run -p verifiable-log --bin monitor -- verify bundle.json
cargo run -p verifiable-log --bin monitor -- verify --tree account-keys account-bundle.json
cargo run -p verifiable-log --bin monitor -- verify --tree binaries binaries-bundle.json
```

`--db` may be omitted to fall back to `TURSO_DATABASE_URL`. The signing key comes
from `--signing-key-env` (default `VLOG_SIGNING_KEY`) or `--signing-key-file`; if
neither is present the build **refuses** rather than inventing a key.

The builder now emits **three** tenants, each its own domain-separated tree: the
`mls-commit-log` tenant (above), the `account-key` tenant (`--account-out`, signed
under `…:sth:v2:account-keys`), and the `binaries` tenant (`build-binaries`, signed
under `…:sth:v2:binaries`). See the [`binaries`](src/binaries.rs) module for the
`BinaryRecord` leaf encoding and the `BinaryInvariant` (no fork, monotonic release
tags, payload/signed pairing).

## Out of scope (later slices)

No real signing-key custody/HSM; no deep MLS authorization of committers (the
`sender_pseudonym` is recorded but not validated); no browser/WASM explorer. Tests
use a local fixture file only and never connect to a real/production database.

## Tests

```bash
cargo test -p verifiable-log-builder
```

The gate suite seeds a local libSQL fixture, builds a bundle, and verifies it by
calling the monitor's own `verify_bundle` (the account and binaries suites used to
carry hand-written copies of that loop, because the CLI could not check their trees);
injects a fork row and an epoch regression and asserts both are rejected; asserts each
tree's bundle fails under a sibling tree's context; tampers with an emitted entry and
asserts the monitor fails; and round-trips the keygen output.
