# verifiable-log

A generic, tenant-agnostic **verifiable append-only log** built on an RFC 6962-style
Merkle tree, plus an offline verification CLI (the "monitor"). This is slice 1 of
the Key Transparency work (issue #330): the reusable Merkle-log machinery that later
tenants (an MLS commit log, an account-key directory) and a serve layer are built on.

The core is deliberately **deploy-target-agnostic**:

- **No network, no database, no clock.** Timestamps are passed in by the caller, so
  the tree, its STHs, and all proofs are fully deterministic and testable.
- Everything on the verification path returns `Result`/`bool` and **never panics**.

What this crate is **not** (later slices, deliberately out of scope): any HTTP/serve
layer or Worker, Turso/DB storage, signing-key custody, privacy hashing (salted/VRF)
of user ids, pollis-core client integration, and the browser/WASM explorer. The two
real tenants (commit log, account-key directory) are also out of scope — only the
pluggable hook and a trivial example invariant ship here.

## Design

### Merkle tree (RFC 6962 / RFC 9162)

- leaf hash: `SHA-256(0x00 || entry_bytes)`
- interior node: `SHA-256(0x01 || left_hash || right_hash)`
- empty tree root: `SHA-256()` (hash of the empty string)

The domain-separation prefixes (`0x00` / `0x01`) ensure a leaf can never be confused
with an interior node. The tree is append-only and supports incremental appends.

Generation (`src/merkle.rs`) follows RFC 6962 §2.1 (`MTH`, `PATH`, `PROOF`/`SUBPROOF`).
Verification follows the standalone algorithms in RFC 9162 §2.1.3.2 (inclusion) and
§2.1.4.2 (consistency) — bit-twiddling walks that need only the audit path, the
relevant roots, and the tree sizes.

### Multi-tenant model

A single log instance hosts many tenants in **one** global Merkle tree (one STH covers
every tenant), mirroring how Certificate Transparency works. Each [`Entry`] carries an
opaque `tenant` id and an opaque `data` payload. Tenant-specific correctness rules are
enforced by a pluggable hook:

```rust
pub trait TenantInvariant: Send + Sync {
    fn check(&self, existing: &[&Entry], candidate: &Entry)
        -> Result<(), InvariantViolation>;
}
```

`existing` is every entry already committed for that tenant, in order; `candidate` is
the entry being appended. Returning an `InvariantViolation` rejects the append and
leaves the log unchanged. A future commit-log tenant would use this to enforce "one
commit per (group, epoch)"; the account-key tenant would enforce monotonic key
versions. This crate ships only `UniqueDataInvariant` (rejects a duplicate payload for
a tenant) as an example.

### Canonical leaf encoding

The bytes that get hashed for a leaf are:

```
len(tenant) as u32 big-endian  ||  tenant (UTF-8)  ||  data
```

Length-prefixing the tenant makes the encoding unambiguous, so two different
`(tenant, data)` pairs can never collide into the same leaf bytes.

### Signed Tree Head signing message

An STH is an Ed25519 signature over:

```
"pollis-verifiable-log:sth:v1"  ||  tree_size (u64 BE)  ||  root_hash (32 bytes)  ||  timestamp (u64 BE)
```

The domain tag prevents the signature from being reused as a signature over anything
else; signing size, root, and timestamp together means none can be altered without
detection.

## Wire contract (frozen)

These JSON shapes are the contract a future serve layer must emit and a monitor
consumes. All binary fields are **lowercase hex**. (serde definitions live in
`src/sth.rs`, `src/log.rs`, `src/proof.rs`.)

### Entry

```json
{ "tenant": "commits", "data": "67726f75702d612f65706f63682d30" }
```

`data` is the hex-encoded opaque payload.

### Signed Tree Head (STH)

```json
{
  "tree_size": 5,
  "root_hash": "3fb8111c…4803",
  "timestamp": 1700000500000,
  "signature": "192a7456…7105"
}
```

`root_hash` is 32 bytes hex; `signature` is 64 bytes hex; `timestamp` is a
caller-supplied `u64` (milliseconds since epoch, by convention).

### Inclusion proof

```json
{
  "leaf_index": 1,
  "tree_size": 5,
  "audit_path": ["510d5319…251a", "bb23367d…6be9", "ec907f72…efe3"]
}
```

The leaf bytes themselves are supplied separately (as an `Entry`); `audit_path` is the
list of sibling hashes, bottom-up, each 32 bytes hex.

### Consistency proof

```json
{
  "first_size": 3,
  "second_size": 5,
  "path": ["9fea0e4b…34c7", "77ec2abb…f89e", "3ca2a2a4…5ba5", "ec907f72…efe3"]
}
```

### Monitor bundle

The CLI reads a single bundle file aggregating the above. Every section except
`public_key` is optional. See `fixtures/example.json` for a complete, known-good
instance.

```json
{
  "public_key": "<ed25519 public key, 32 bytes hex>",
  "sths": [ STH, ... ],                          // oldest first
  "entries": [ Entry, ... ],                     // full ordered log (optional)
  "enforce_unique": ["commits"],                 // tenants the example invariant applies to
  "inclusion": [ { "entry": Entry, "proof": InclusionProof, "sth_index": 1 } ],
  "consistency": [ { "old_index": 0, "new_index": 1, "proof": ConsistencyProof } ]
}
```

## Library usage

```rust
use verifiable_log::{Entry, VerifiableLog, UniqueDataInvariant, proof, is_equivocation};
use ed25519_dalek::SigningKey;

let signing_key = SigningKey::from_bytes(&[7u8; 32]); // custody is out of scope here

let mut log = VerifiableLog::new();
log.register_invariant("commits", Box::new(UniqueDataInvariant));

log.append(Entry::new("commits", b"group-a/epoch-0".to_vec()))?;
log.append(Entry::new("accounts", b"alice/key-v1".to_vec()))?;

// Sign a tree head — timestamp is the caller's (no clock in the core).
let sth = log.signed_tree_head(&signing_key, 1_700_000_000_000);

// Prove and verify a leaf.
let entry = log.entry(0).unwrap().clone();
let incl = log.inclusion_proof(0)?;
assert!(proof::verify_inclusion_proof(&entry, &incl, &sth));
```

## CLI ("monitor")

The `monitor` binary verifies a fixture with no network and no DB. It checks STH
signatures against the provided public key, flags equivocation (two STHs of the same
`tree_size` with different roots), replays `entries` through the tenant invariants and
confirms each STH root, and verifies inclusion and consistency proofs. It prints a
per-check report and **exits non-zero** if anything fails.

```bash
# Build
cargo build -p verifiable-log

# Emit a known-good example fixture
./target/debug/monitor gen-example fixture.json

# Verify it (exit 0, prints PASS lines)
./target/debug/monitor verify fixture.json

# A tampered leaf, forged proof, broken consistency, bad signature, or
# equivocation makes verify exit non-zero with a FAIL report.
```

## Tests

```bash
cargo test -p verifiable-log
```

The deterministic gate suite (`tests/integration.rs`) covers: valid inclusion proofs
pass; tampered leaf/root/proof all fail; consistency holds across a sequence of appends
and a forged consistency proof fails; equivocation is detected; the CLI round-trips a
known-good fixture (exit 0) and rejects a tampered one (non-zero); and a violating
append is rejected by a tenant invariant hook.
