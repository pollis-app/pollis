# verifiable-log-serve

The **serve layer** for the verifiable log — slice 3 of the Key Transparency
work (issue #330). It turns a signed monitor bundle (the output of
`verifiable-log-builder`) into an immutable, host-agnostic **static artifact
tree** that implements the log's public read API, plus a tiny dev HTTP server
and an end-to-end "fetch over HTTP and verify" path.

Slices 1 (`verifiable-log` core + `monitor`) and 2 (`verifiable-log-builder` +
`builder`) are **depended on, never reimplemented**: all Merkle/STH/proof logic
and every verifier comes from `verifiable_log`. This crate is transport and
orchestration only, and the core crate stays dependency-pure (no HTTP).

## Why static (the load-bearing idea)

Every artifact a transparency log serves is **deterministic and immutable**: an
STH for `tree_size = N` never changes; an inclusion or consistency proof for a
given `(leaf, tree_size)` is fixed forever. So the serve layer is **not** a query
service over a database — it is a precomputed directory of immutable JSON files
served as static assets.

Generate the tree once, drop it on any static host — R2, Cloudflare Pages, an
edge CDN (none chosen here, deliberately) — and reads are trivially cacheable.
The read API is **public and unauthenticated by design**: there are no
credentials anywhere on this path.

## Read API (URL → file mapping)

The file path under the output root mirrors the URL exactly (drop the leading
`/`), so serving is a literal static-file mapping.

| URL                                                   | Contents                            | Cache policy |
|-------------------------------------------------------|-------------------------------------|--------------|
| `/v1/public_key.json`                                 | the log's Ed25519 public key        | immutable    |
| `/v1/index.json`                                      | discovery manifest (see below)      | short        |
| `/v1/sth/latest.json`                                 | newest STH                          | short        |
| `/v1/sth/<tree_size>.json`                            | STH at that tree size               | immutable    |
| `/v1/entries.json`                                    | full ordered `[Entry]` (small logs) | immutable    |
| `/v1/entries/<index>.json`                            | one entry                           | immutable    |
| `/v1/proof/inclusion/<tree_size>/<leaf_index>.json`   | inclusion proof                     | immutable    |
| `/v1/proof/consistency/<first>-<second>.json`         | consistency proof                   | immutable    |

All wire shapes (`Entry`, `Sth`, `InclusionProof`, `ConsistencyProof`) are the
frozen contract documented in `verifiable-log/README.md`.

### Manifest (`/v1/index.json`)

So a monitor or explorer can discover everything available without guessing:

```json
{
  "version": "v1",
  "public_key": "<ed25519 public key, 32 bytes hex>",
  "entry_count": 5,
  "latest_tree_size": 5,
  "sth_sizes": [3, 5],
  "inclusion": [ { "tree_size": 5, "leaf_index": 1 } ],
  "consistency": [ { "first": 3, "second": 5 } ],
  "enforce_unique": ["commits"]
}
```

### Cache policy

Every artifact is **write-once / immutable** except the two that move as the log
grows — `sth/latest.json` and `index.json`. Hosts should serve them so:

- immutable artifacts → `Cache-Control: public, max-age=31536000, immutable`
- `latest.json` and `index.json` → `Cache-Control: no-cache`

The dev server below sets exactly these headers; a static host should be
configured to match.

## CLI (`serve`)

```bash
cargo build -p verifiable-log-serve

# 1. Generate the immutable static tree from a signed bundle.
./target/debug/serve generate --bundle bundle.json --out ./site

# 2. Serve it locally for testing/demo (NOT the production path).
./target/debug/serve serve --dir ./site --port 8787

# 3. From anywhere, verify the log over HTTP — trusting only the public key.
./target/debug/serve verify-remote http://127.0.0.1:8787
```

`verify-remote` fetches the public key and manifest, then every STH, the
entries, and all proofs, and runs slice 1's verifiers: STH signatures,
equivocation, entry/STH-root replay (through the tenant invariants), inclusion,
and consistency. It prints a per-check report and **exits non-zero** if anything
fails — a tampered entry, a forged proof, a bad signature, or a mismatched
`latest.json` are all rejected.

## Production note

The `serve` subcommand is for **local testing and demos only**. The real
deployment is "generate the directory and drop it on a static host"; there is no
server process, database, or app logic to run in production. Point a CDN/edge/R2
bucket at the generated tree, apply the cache headers above, and the read API is
live.

## Tests

```bash
cargo test -p verifiable-log-serve
```

The gate covers: (a) the layout generator writes every documented file for a
fixture bundle; (b) the dev server serves them and `verify_remote` verifies the
whole log over HTTP end to end; (c) tampering with a served artifact (an entry,
the entries list, or an STH signature) makes remote verification fail.

## Out of scope (later slices)

No specific cloud deployment config (Cloudflare/R2/Pages), no browser/WASM
explorer (slice 4), no auth on the read API (intentionally public), no
signing-key custody, no account-key tenant, no production DB access.
