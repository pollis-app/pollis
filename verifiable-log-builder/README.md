# verifiable-log-builder

Reads MLS commit data from a Turso/libSQL database and writes the signed bundle
that [`verifiable-log`](../verifiable-log)'s `monitor` verifies. It also builds the
account-key and released-binaries trees. Part of the Key Transparency work (#330).

Merkle, STH and proof logic, the bundle type and the per-tree STH contexts all come
from `verifiable-log`. This crate adds the database reader, the tenant leaf
encodings and invariants, and the bundle builder and signer.

## What `build` does

1. Reads `mls_commit_log` in `seq` order over libSQL (remote Turso or a local
   SQLite file). It reads the structural columns and `commit_data`, hashes each
   `commit_data` blob to SHA-256 as it is read, and drops the bytes; they are never
   returned, logged or written. The auth token comes from the environment and is
   not logged.
2. Appends every commit to a `VerifiableLog` with `CommitLogInvariant` registered
   for the `mls-commit-log` tenant.
3. Signs STHs with an ML-DSA-44 key and writes the bundle as JSON.

## Commit leaf encoding

`Entry.data` for the `mls-commit-log` tenant is compact JSON with fields in this
order (serde emits declaration order, no whitespace):

```
{"conversation_pseudonym":<hex>,"epoch":<u64>,"sender_pseudonym":<hex>,"seq":<i64>,"commit_sha256":<hex>}
```

`generation` follows `conversation_pseudonym` when it is non-zero (#454 P4), so
leaves for conversations that never migrated suite are unchanged.

- `commit_sha256` is `sha256(commit_data)`, lowercase hex. The leaf commits to the
  commit without containing it.
- `conversation_pseudonym` and `sender_pseudonym` are windowed pseudonyms (#701),
  keyless SHA-256 hashes of the real id(s) and a window index
  (`window = seq / PSEUDONYM_WINDOW_SIZE`, currently 1024):
  `H(dom || conversation_id || window)` and
  `H(dom || conversation_id || sender_id || window)`. The pseudonym changes at each
  window boundary, so someone reading only the public log cannot track a
  conversation over time. A member, who knows `conversation_id`, can re-derive every
  window's pseudonym and verify the full history. See
  [`docs/transparency.md`](../docs/transparency.md) for the scheme and its limits.
- Leaf bytes are hashed into the tree, so changing the encoding means republishing
  the whole tree. #701's encoding took effect at the republish done for the
  ML-DSA-44 key rotation (#672 / #699).
- `sender_pseudonym` is recorded but not checked against MLS group membership.

The leaf the core hashes is then
`SHA-256(0x00 || len(tenant) BE || "mls-commit-log" || data)`.

## Commit-log invariant

`CommitLogInvariant` runs on every append. Per grouping key (the leaf's
`conversation_pseudonym`) it enforces:

- **no fork**: no two entries share `(conversation_pseudonym, generation, epoch)`;
- **no regression or replay**: `(generation, epoch)` strictly increases in `seq`
  order;
- **a new lineage starts at epoch 0** (#454 P4).

Commits are appended in `seq` order, so the candidate always has the highest `seq`
for its key, and the second rule reduces to "greater than every earlier
`(generation, epoch)`". A fork or regression in the source data aborts the build.
This mirrors the database's `UNIQUE INDEX (conversation_id, generation, epoch)`
(#357) in a form anyone can check.

Grouping by pseudonym only works within a window: a fork whose two branches fall in
different windows has two different pseudonyms and passes. So `build_bundle` runs
the invariant twice, first over leaves keyed on the real `conversation_id`
(`to_identity_leaf`, never published), which catches forks across windows, then
over the published pseudonymous leaves. Members get the same full-strength check at
read time through `verifiable-log-serve`'s `verify_group`.

The bundle also lists `mls-commit-log` in `enforce_unique`, so the monitor's replay
re-checks leaf uniqueness on its own.

## Bundle contents

The bundle uses the schema in [`verifiable-log/README.md`](../verifiable-log/README.md).
`build` writes the full ordered `entries`, an STH over the final tree, a midpoint
STH when there are at least two entries, an inclusion proof for every entry against
the final STH, and a consistency proof from the midpoint to the final STH.

STH timestamps come from `--timestamp` (ms since epoch), never the system clock, so
the output is deterministic.

## CLI

```bash
# Generate a throwaway dev keypair. VLOG_SIGNING_KEY is the 32-byte ML-DSA-44
# seed in hex; the public key is 1312 bytes and signatures 2420.
cargo run -p verifiable-log-builder --bin builder -- keygen

# Build from a local SQLite file (no network).
VLOG_SIGNING_KEY=<32-byte hex> \
  cargo run -p verifiable-log-builder --bin builder -- \
  build --db ./commits.db --out bundle.json --timestamp 1700000000000

# Build from Turso, with the account-key tree as a second bundle.
TURSO_DATABASE_URL=libsql://... TURSO_AUTH_TOKEN=... VLOG_SIGNING_KEY=... \
  cargo run -p verifiable-log-builder --bin builder -- \
  build --out bundle.json --account-out account-bundle.json --timestamp 1700000000000

# Build the released-binaries tree from a JSON array of BinaryRecords.
VLOG_SIGNING_KEY=<32-byte hex> \
  cargo run -p verifiable-log-builder --bin builder -- \
  build-binaries --binaries-in records.json --out binaries-bundle.json \
  --timestamp 1700000000000

# Verify each bundle under its own tree (--tree defaults to commit-log).
cargo run -p verifiable-log --bin monitor -- verify bundle.json
cargo run -p verifiable-log --bin monitor -- verify --tree account-keys account-bundle.json
cargo run -p verifiable-log --bin monitor -- verify --tree binaries binaries-bundle.json
```

`build` options:

- `--db`: main database (holds `account_key_log`); falls back to
  `TURSO_DATABASE_URL`.
- `--log-db`: database holding `mls_commit_log`; falls back to `LOG_DB_URL`, then
  to `--db`. Uses `LOG_DB_AUTH_TOKEN`, then `TURSO_AUTH_TOKEN`.
- `--account-out`: also write the account-key bundle, signed under
  `pollis-verifiable-log:sth:v2:account-keys`. `--account-timestamp` sets its STH
  timestamp separately (default: `--timestamp`), so an unchanged tree can be
  re-emitted byte-identically.
- `--signing-key-env` (default `VLOG_SIGNING_KEY`) or `--signing-key-file`. With
  neither, the build fails rather than inventing a key.
- `--retired-key <public_key_hex>:<not_after_ms>` (repeatable, also on
  `build-binaries`): publish a retiring key alongside the active one during a key
  rotation.

`build-binaries` signs under `pollis-verifiable-log:sth:v2:binaries`. The
`BinaryRecord` leaf and `BinaryInvariant` (no forked re-issue, monotonic release
tags, payload/signed pairing) are in [`src/binaries.rs`](src/binaries.rs).
`key-set` signs the root key-set statement (#754) and is run offline, not in CI.

## Not covered

No HSM or other key custody beyond a CI secret. No check that a committer was
authorized by MLS group state (`sender_pseudonym` is recorded only).

## Tests

```bash
cargo test -p verifiable-log-builder
```

The suite seeds a local libSQL file (never a real database), builds a bundle and
verifies it with the monitor's `verify_bundle`; asserts that an injected fork and an
epoch regression are rejected; asserts that each tree's bundle fails under another
tree's context; asserts that a tampered entry fails; and round-trips `keygen`.
