# verifiable-log-serve

The serve layer for the transparency log (#330). It turns a signed bundle from
`verifiable-log-builder` into a static directory of JSON files that is the log's
public read API, and it ships two binaries:

- `pollis-verify`, the auditor CLI that fetches that API over HTTP(S) and verifies it;
- `serve`, the operator tool that generates the directory, runs a local dev server,
  and runs the `live` server.

All Merkle, STH and proof logic comes from [`verifiable-log`](../verifiable-log);
this crate only handles layout, transport and orchestration.

## Why static files

A transparency log's artifacts never change once written: the STH for
`tree_size = N` is fixed, and so is any proof for a given `(leaf, tree_size)`. So
the read API is a precomputed directory of JSON files, not a query service. It can
sit on any static host and is easy to cache. It is public and needs no credentials.

## Read API

The file path under the output directory is the URL path without the leading `/`.

| URL | Contents | Cache |
|---|---|---|
| `/v1/public_key.json` | the log's ML-DSA-44 public key(s) | immutable |
| `/v1/index.json` | manifest (below) | short |
| `/v1/sth/latest.json` | newest STH | short |
| `/v1/sth/<tree_size>.json` | STH at that size | immutable |
| `/v1/entries.json` | full ordered `[Entry]` | immutable |
| `/v1/entries/<index>.json` | one entry | immutable |
| `/v1/proof/inclusion/<tree_size>/<leaf_index>.json` | inclusion proof | immutable |
| `/v1/proof/consistency/<first>-<second>.json` | consistency proof | immutable |
| `/v1/account-keys/...` | account-key tree, same layout | as above |
| `/v1/binaries/...` | released-binaries tree, same layout | as above |
| `/verify/account/<user_id>` | precomputed `AccountReport` | short |
| `/verify/release/<tag>` | precomputed `ReleaseReport` | short |
| `/verify/group/<conversation_id>` | `GroupReport`, computed per request by a server (not a file) | `no-cache` |
| `/v1/key-set.json` | root-signed key-set statement, only if `generate --key-set` was given (#754) | immutable |

`Entry`, `Sth`, `InclusionProof` and `ConsistencyProof` are defined in
[`verifiable-log/README.md`](../verifiable-log/README.md).

### Per-group verification needs the real conversation id (#701)

Commit-log leaves carry windowed pseudonyms, not raw `conversation_id`s, so
`generate` writes no per-group reports and the manifest lists no conversations;
either would expose the set of groups the pseudonyms hide. To check one
conversation you need its real id, which only members have:

- `pollis-verify group <base-url> <conversation_id>` fetches `entries.json` and does
  the work locally, so it works against a plain static host;
- `GET /verify/group/<conversation_id>` does the same work on a server (`serve serve`
  or `serve live`). A static host alone does not answer it.

Both re-derive the conversation's pseudonym for every window and check the
invariant across windows with the shared `verify_group_in_bundle`.

The account-key tree still publishes `user_id` and keeps its precomputed
`/verify/account/<user_id>` reports, because key transparency has to look users up
by identity.

### Manifest (`/v1/index.json`)

```json
{
  "format_version": 2,
  "version": "v1",
  "public_key": "<ML-DSA-44 public key, 1312 bytes hex>",
  "entry_count": 5,
  "latest_tree_size": 5,
  "sth_sizes": [3, 5],
  "inclusion": [ { "tree_size": 5, "leaf_index": 1 } ],
  "consistency": [ { "first": 3, "second": 5 } ],
  "enforce_unique": ["commits"]
}
```

`format_version` (`bundle::FORMAT_VERSION`) is the served wire format, and verifiers
read it before anything else. If it is newer than the binary supports,
`pollis-verify` exits `2` ("upgrade your verifier") instead of failing with a parse
error. It became `2` at #701, when the commit leaf moved to windowed pseudonyms and
the `conversations` list was removed. A manifest without the field counts as `0`.
`remote` still verifies such a log; `group` refuses it (it needs at least
`MIN_LEAF_FORMAT_VERSION`, currently 2), because decoding pre-#701 leaves would
report an empty, passing result for a conversation that is present.

### Cache headers

Everything is write-once except `sth/latest.json`, `index.json` and the
`verify/account/*` and `verify/release/*` reports, which change as the log grows.

- immutable artifacts: `Cache-Control: public, max-age=31536000, immutable`
- moving artifacts: `Cache-Control: public, max-age=300`

The production publish (`.github/workflows/transparency-publish.yml`) uploads in
that split: immutable files first, then the moving heads and reports, so a
published head never points at a file that is not there yet. The dev server sends
`no-cache` for `latest.json` and `index.json` and the immutable header for
everything else.

## Usage

```bash
cargo build -p verifiable-log-serve

# 1. Generate the static tree from a signed bundle. Add --account-bundle and
#    --binaries-bundle to include the other two trees.
./target/debug/serve generate --bundle bundle.json --out ./site

# 2. Serve it locally (testing and demos only).
./target/debug/serve serve --dir ./site --port 8787

# 3. Verify it over HTTP. pollis-verify trusts only its compiled-in key, so a log
#    signed with your own dev key needs that key added explicitly.
export POLLIS_VERIFY_PINNED_KEYS_HEX=<your log's public key hex>
./target/debug/pollis-verify remote http://127.0.0.1:8787

# Verify one conversation, one user's key history, or one release.
./target/debug/pollis-verify group   http://127.0.0.1:8787 <conversation-id>
./target/debug/pollis-verify account http://127.0.0.1:8787 <user-id>
./target/debug/pollis-verify release http://127.0.0.1:8787 v1.3.0
```

`pollis-verify remote` fetches the public key and manifest, then every STH, the
entries and all proofs, for the commit-log tree and (if published) the account-key
and binaries trees. It checks that the served key is the pinned key, then STH
signatures, equivocation, entry replay against each STH root, and every inclusion
and consistency proof. A missing account-key or binaries tree is a `NOTE`, not a
failure. `group`, `account` and `release` each take `--json` to print the report
instead of a summary.

`serve` also has `verify-remote <base-url>` and
`verify-group --base <url> --group <id> [--json]`, which run the same code for local
development.

### Exit codes

| code | meaning |
|---|---|
| `0` | verification passed |
| `1` | verification failed (bad signature, forged proof, fork, epoch regression, served key not pinned), or a transport or parse error |
| `2` | version skew: the log's `format_version` is newer than this binary supports. Upgrade `pollis-verify`. This is not a tampering finding. |

## Deployment

**https://verify.pollis.com** is this static tree on Cloudflare R2.
`.github/workflows/transparency-publish.yml` rebuilds it daily (and on
`workflow_dispatch`): it builds the signed bundles in CI, runs `serve generate`,
syncs to R2 with the cache split above, then runs `pollis-verify remote` against
the live site. No server process sits on the trust path. The host can serve stale
or broken data, but it cannot forge a head without the signing key, and STH
timestamps make staleness visible.

`serve live` (used by the Docker image, see `docker-entrypoint.sh`) serves the same
`/v1` surface and `/verify/group/<id>`, rebuilt in memory from the commit-log
database at most once per `--ttl-secs`.

## Tests

```bash
cargo test -p verifiable-log-serve
```

The suite checks that `generate` writes every documented file for a fixture bundle;
that the dev server serves them and `verify_remote` passes end to end over HTTP; and
that tampering with a served entry, the entries list, or an STH signature makes
remote verification fail.
