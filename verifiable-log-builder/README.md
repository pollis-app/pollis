# verifiable-log-builder

Slice 2 of the Key Transparency work (issue #330). Reads real MLS commit data
from a Turso/libSQL database and turns it into the signed **monitor bundle** that
slice 1's [`verifiable-log`](../verifiable-log) `monitor` CLI already verifies —
with **no changes to the monitor**.

This crate depends on `verifiable-log` for all Merkle / STH / proof logic; it does
not reimplement any of it. It adds only: a DB reader, the commit-log tenant
(canonical leaf encoding + invariant), and the bundle builder/signer.

## What it does

1. **Reads `mls_commit_log`** (ordered by `seq` ascending) over libSQL — remote
   Turso or a local SQLite file. Only the structural columns plus `commit_data`
   are read; each blob is hashed to `sha256` **as it is read and the raw bytes
   are immediately dropped** — they are never returned, logged, or persisted.
   The auth token is read from the environment and never logged.
2. **Appends every commit** to a `verifiable_log::VerifiableLog` with the
   `CommitLogInvariant` registered for the `mls-commit-log` tenant.
3. **Signs Signed Tree Heads** with an Ed25519 key and **emits a JSON bundle**
   that the `monitor` CLI consumes byte-for-byte.

## Canonical leaf encoding (frozen contract extension)

This extends `verifiable-log`'s frozen leaf encoding for the `mls-commit-log`
tenant. The tenant's `Entry.data` is the **compact JSON** of:

```
{"conversation_id":<string>,"epoch":<u64>,"sender_id":<string>,"seq":<i64>,"commit_sha256":<hex string>}
```

with fields in **exactly that order** (serde emits struct fields in declaration
order, no insignificant whitespace), so the encoding is deterministic and stable.

- `commit_sha256` is `sha256(commit_data)`, lowercase hex (32 bytes). The leaf
  commits to the commit bytes **without storing the raw blob**.
- `sender_id` is recorded so a later slice can add cryptographic authorization
  ("was this sender entitled to commit at this epoch?"). **This slice does not
  validate it** — that needs MLS group state and is out of scope (see below).

The full leaf hashed by the core is then `verifiable-log`'s own encoding around
this payload: `SHA-256(0x00 || len(tenant) BE || "mls-commit-log" || data)`.

## Commit-log invariant (the auditable form of #357)

`CommitLogInvariant` is registered for the tenant and consulted on every append.
Per conversation it enforces:

- **(a) no fork** — no two entries share the same `(conversation_id, epoch)`;
- **(b) no epoch regression / replay** — within a conversation, `epoch` strictly
  increases in `seq` order.

Because commits are appended in `seq` order, the candidate always has the largest
`seq` seen so far for its conversation, so (b) is exactly "strictly greater than
every prior epoch for this conversation". A fork or regression in the source data
**aborts the build** rather than producing a bundle that hides it. (#357 enforces
this with a live `UNIQUE INDEX (conversation_id, epoch)`; this is its global,
publicly-auditable mirror.)

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
cargo run -p verifiable-log-builder --bin builder -- keygen

# Build a signed bundle from a local fixture DB (no network).
VLOG_SIGNING_KEY=<32-byte hex> \
  cargo run -p verifiable-log-builder --bin builder -- \
  build --db ./commits.db --out bundle.json --timestamp 1700000000000

# ...or from remote Turso (auth token from the environment):
TURSO_DATABASE_URL=libsql://... TURSO_AUTH_TOKEN=... VLOG_SIGNING_KEY=... \
  cargo run -p verifiable-log-builder --bin builder -- \
  build --out bundle.json --timestamp 1700000000000

# Verify with the UNCHANGED slice-1 monitor.
cargo run -p verifiable-log --bin monitor -- verify bundle.json
```

`--db` may be omitted to fall back to `TURSO_DATABASE_URL`. The signing key comes
from `--signing-key-env` (default `VLOG_SIGNING_KEY`) or `--signing-key-file`; if
neither is present the build **refuses** rather than inventing a key.

## Out of scope (later slices)

No HTTP/serve layer (slice 3); no real signing-key custody/HSM; no deep MLS
authorization of committers (the `sender_id` is recorded but not validated); no
account-key tenant; no browser/WASM explorer. Tests use a local fixture file
only and never connect to a real/production database.

## Tests

```bash
cargo test -p verifiable-log-builder
```

The gate suite seeds a local libSQL fixture, builds a bundle, and verifies it
through the slice-1 monitor path; injects a fork row and an epoch regression and
asserts both are rejected; tampers with an emitted entry and asserts the monitor
fails; and round-trips the keygen output.
