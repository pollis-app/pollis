# verifiable-log

A tenant-agnostic append-only log built on an RFC 6962 Merkle tree, plus `monitor`,
an offline CLI that verifies a log bundle. It is the base layer of the Key
Transparency work (#330); the builder, the serve layer and the real tenants (MLS
commit log, account-key directory, released binaries) live in
[`verifiable-log-builder`](../verifiable-log-builder) and
[`verifiable-log-serve`](../verifiable-log-serve).

The core has no network, database or clock. Callers pass timestamps in, so trees,
STHs and proofs are deterministic. Every verification function returns
`Result`/`bool` and does not panic.

## Design

### Merkle tree (RFC 6962 / RFC 9162)

- leaf hash: `SHA-256(0x00 || entry_bytes)`
- interior node: `SHA-256(0x01 || left_hash || right_hash)`
- empty tree root: `SHA-256()` (hash of the empty string)

The `0x00`/`0x01` prefixes keep a leaf from being confused with an interior node.

Proof generation (`src/merkle.rs`) follows RFC 6962 §2.1 (`MTH`, `PATH`,
`PROOF`/`SUBPROOF`). Verification follows RFC 9162 §2.1.3.2 (inclusion) and
§2.1.4.2 (consistency), which need only the audit path, the roots and the tree
sizes.

### Tenants

One log instance holds many tenants in a single Merkle tree, as in Certificate
Transparency, so one STH covers all of them. Each `Entry` has an opaque `tenant` id
and an opaque `data` payload. Tenant-specific rules are a pluggable hook:

```rust
pub trait TenantInvariant: Send + Sync {
    fn check(&self, existing: &[&Entry], candidate: &Entry)
        -> Result<(), InvariantViolation>;
}
```

`existing` is every entry already committed for that tenant, in order. Returning an
`InvariantViolation` rejects the append and leaves the log unchanged. This crate
ships one example, `UniqueDataInvariant`, which rejects a duplicate payload within a
tenant; the real invariants are in `verifiable-log-builder`.

### Leaf encoding

```
len(tenant) as u32 big-endian  ||  tenant (UTF-8)  ||  data
```

The length prefix makes the encoding unambiguous: two different `(tenant, data)`
pairs cannot produce the same leaf bytes.

### STH signing message

An STH is an ML-DSA-44 signature over:

```
context  ||  tree_size (u64 BE)  ||  root_hash (32 bytes)  ||  timestamp (u64 BE)
```

`context` is a domain-separation tag, one per tree (`src/sth.rs`, `STH_CONTEXTS`):

| tree | context |
|---|---|
| `commit-log` | `pollis-verifiable-log:sth:v2` |
| `account-keys` | `pollis-verifiable-log:sth:v2:account-keys` |
| `binaries` | `pollis-verifiable-log:sth:v2:binaries` |

One key signs all three trees, so the context is what stops a head for one tree
verifying as a head for another.

## Wire format

The serve layer emits these JSON shapes and the verifiers consume them. They are
frozen. Binary fields are lowercase hex. The serde definitions are in
`src/sth.rs`, `src/log.rs`, `src/proof.rs` and `src/bundle.rs`.

### Entry

```json
{ "tenant": "commits", "data": "67726f75702d612f65706f63682d30" }
```

### Signed Tree Head

```json
{
  "tree_size": 5,
  "root_hash": "3fb8111c…4803",
  "timestamp": 1700000500000,
  "signature": "192a7456…7105",
  "key_id": "…"
}
```

`root_hash` is 32 bytes, `signature` 2420 bytes. `timestamp` is milliseconds since
the epoch by convention. `key_id` is optional and outside the signed message, so a
verifier uses it only as a hint for which key to try first.

### Inclusion proof

```json
{
  "leaf_index": 1,
  "tree_size": 5,
  "audit_path": ["510d5319…251a", "bb23367d…6be9", "ec907f72…efe3"]
}
```

`audit_path` is the sibling hashes, bottom-up. The leaf itself is supplied
separately as an `Entry`.

### Consistency proof

```json
{
  "first_size": 3,
  "second_size": 5,
  "path": ["9fea0e4b…34c7", "77ec2abb…f89e", "3ca2a2a4…5ba5", "ec907f72…efe3"]
}
```

### Monitor bundle

`monitor` reads a single file that aggregates the above. Only `public_key` is
required. `fixtures/example.json` is a complete, valid example.

```jsonc
{
  "public_key": "<ML-DSA-44 public key, 1312 bytes hex>",   // the active signing key
  "retired_keys": [                              // omitted outside a key rotation
    { "key_id": "…", "algorithm": "ML-DSA-44",
      "public_key": "…", "not_after": 1700000000000 }
  ],
  "sths": [ STH, ... ],                          // oldest first
  "entries": [ Entry, ... ],                     // the full ordered log
  "enforce_unique": ["commits"],                 // tenants UniqueDataInvariant applies to
  "inclusion": [ { "entry": Entry, "proof": InclusionProof, "sth_index": 1 } ],
  "consistency": [ { "old_index": 0, "new_index": 1, "proof": ConsistencyProof } ]
}
```

`retired_keys` lists keys that no longer sign but stay valid until `not_after` (ms
since epoch). A head is accepted if it verifies under the active key or any
unexpired retired key. Without this, every head signed during a rotation would look
forged; the monitor dropped the field before #875.

## Library usage

```rust
use verifiable_log::{proof, Entry, SigningKey, UniqueDataInvariant, VerifiableLog};

// Key custody is the caller's problem; this is a fixed test seed.
let signing_key = SigningKey::from_seed(&[7u8; 32].into());

let mut log = VerifiableLog::new();
log.register_invariant("commits", Box::new(UniqueDataInvariant));

log.append(Entry::new("commits", b"group-a/epoch-0".to_vec()))?;
log.append(Entry::new("accounts", b"alice/key-v1".to_vec()))?;

// The core has no clock: the caller supplies the timestamp.
let sth = log.signed_tree_head(&signing_key, 1_700_000_000_000);

let entry = log.entry(0).unwrap().clone();
let incl = log.inclusion_proof(0)?;
assert!(proof::verify_inclusion_proof(&entry, &incl, &sth));
```

## `monitor` CLI

`monitor verify` checks a bundle with no network or database access:

- every STH signature, against the active key and any unexpired retired key;
- equivocation (two STHs of the same `tree_size` with different roots);
- replay of `entries` through the tenant invariants, and each STH root against the
  replayed tree;
- every inclusion and consistency proof in the bundle.

It prints a `PASS`/`FAIL` line per check and exits non-zero if any check fails. The
check loop is the library function `verifiable_log::monitor::verify_bundle`; the
builder's tests call the same function.

```bash
cargo build -p verifiable-log

# Write a known-good example bundle, then verify it (exit 0).
./target/debug/monitor gen-example fixture.json
./target/debug/monitor verify fixture.json

# Other trees. --tree selects the STH context and defaults to commit-log.
./target/debug/monitor verify --tree account-keys account-bundle.json
./target/debug/monitor verify --tree binaries binaries-bundle.json

# Fix the clock used to expire retired keys (default: system clock).
./target/debug/monitor verify --now-ms 1700000000000 fixture.json
```

The bundle does not say which tree it belongs to, and `monitor` does not guess:
letting the input pick the tree would let the log choose which question it is
asked. A bundle checked under the wrong `--tree` fails its signature checks; that
failure means you asked the wrong question, not that the log was tampered with.

## Tests

```bash
cargo test -p verifiable-log
```

`tests/integration.rs` checks that valid inclusion proofs pass; that a tampered
leaf, root or proof fails; that consistency holds across appends and a forged
consistency proof fails; that equivocation is detected; and that a tenant invariant
rejects a violating append; it also runs the CLI against a good bundle (exit 0) and
a tampered one (non-zero). `tests/monitor_cli.rs` covers `--tree` for each tree and
heads signed by a retiring key. `tests/rfc6962_vectors.rs` checks hashes and proofs
against the RFC 6962 / CT known-answer vectors.
