# Verify the Pollis transparency log yourself

Pollis publishes an append-only [transparency log](./transparency.md) of every MLS
commit. This guide shows you how to **independently verify it** — proving the
server has not forked, rolled back, or rewritten any conversation's history.

The one rule that makes this meaningful:

> **You trust only the log's published Ed25519 public key, the signed tree head,
> and the Merkle proofs checked against it.** Not the server, not the database,
> not the host serving the files. If a single byte is tampered with, a signature
> or proof check fails and the tool exits non-zero.

Everywhere below, `<base-url>` is wherever the static log is published (in
production, the deployed verifier; locally, a dev server you run yourself).

## 1. Get the verifier and build it

You only need the repo and a Rust toolchain (`cargo`). Build the serve layer —
which carries the whole-log and per-group HTTP verifiers — and the `monitor` (the
fully offline verifier):

```bash
# The HTTP verifiers: `serve verify-remote` and `serve verify-group`.
cargo build -p verifiable-log-serve --release

# The offline verifier: `monitor verify <bundle.json>`.
cargo build -p verifiable-log --release
```

The binaries land in `target/release/`:

```
target/release/serve      # generate / serve / verify-remote / verify-group
target/release/monitor    # offline bundle verifier
```

Everything in this guide uses only these two binaries. Neither needs any
credentials — the read API is public and unauthenticated by design.

## 2. Verify the whole log over HTTP — `serve verify-remote`

This fetches the entire public log over plain HTTP and verifies it end to end:
every STH signature, equivocation across heads, that each entry replays to the
signed root, every inclusion proof, and every consistency proof — trusting only
the published public key.

```bash
./target/release/serve verify-remote <base-url>
```

A passing run prints a `PASS` line per check and exits `0`:

```
$ ./target/release/serve verify-remote https://transparency.pollis.com
PASS  STH[3] tree_size matches its URL
PASS  STH[3] signature
PASS  STH[5] tree_size matches its URL
PASS  STH[5] signature
PASS  latest.json matches the newest STH
PASS  latest.json signature
PASS  no equivocation between size 3 and size 5
PASS  entries.json count matches manifest
PASS  per-entry files match entries.json
PASS  all entries satisfy tenant invariants
PASS  STH[3] root matches replayed entries
PASS  STH[5] root matches replayed entries
PASS  inclusion: leaf 1 in size 5
PASS  consistency: size 3 -> size 5

OK: all checks passed
```

If **anything** fails — a tampered entry, a forged proof, a bad signature, a
`latest.json` that disagrees with the newest head — the offending line reads
`FAIL`, the summary reads `FAILED: one or more checks did not pass`, and the
command **exits non-zero**. That exit code is the whole point: it is computed from
the signature and the proofs, not from anything the server told you to believe.

```bash
./target/release/serve verify-remote <base-url> && echo "log is intact" || echo "VERIFICATION FAILED"
```

## 3. Verify one conversation — `serve verify-group`

To check a single conversation's commit chain — that every one of its commits is
provably included in the signed log, and that its epochs are append-only and
fork-free — pass the conversation id:

```bash
./target/release/serve verify-group --base <base-url> --group <conversation-id>
```

It verifies the STH signature **first** (an unsigned head is worth nothing), then
selects that conversation's commits, checks each one's inclusion proof against the
signed head, and replays them through the no-fork / no-epoch-regression invariant:

```
$ ./target/release/serve verify-group --base https://transparency.pollis.com --group design-team
Group:   design-team
Found:   yes
STH:     tree_size 4  root 6e36ea07b3096dbe64c3d0b0acb76c8ccfbe6c218346b754384cb423b7299f1e
Commits (seq order):
  epoch 0    seq 1      sender alice        commit cbb029…05c3  [included ✓]
  epoch 1    seq 2      sender bob          commit 8825a3…f5dd  [included ✓]
  epoch 2    seq 3      sender alice        commit 012904…7b8a  [included ✓]

PASS: group chain is valid
```

A valid chain exits `0`; a missing inclusion proof, a fork, or an epoch regression
lists the reason under `Violations:`, prints `FAIL: group chain is NOT valid`, and
exits non-zero.

Add `--json` to get the machine-readable `GroupReport` (the exact shape the HTTP
endpoint and the website explorer consume):

```bash
./target/release/serve verify-group --base <base-url> --group <conversation-id> --json
```

```json
{
  "group_id": "design-team",
  "found": true,
  "sth_tree_size": 4,
  "root_hex": "6e36ea07b3096dbe64c3d0b0acb76c8ccfbe6c218346b754384cb423b7299f1e",
  "commits": [
    {
      "epoch": 0,
      "seq": 1,
      "sender_id": "alice",
      "commit_sha256": "cbb0293a499663eb04c789af7056dec01cd11cb6d53c09da3e234dea2e7d05c3",
      "included": true
    },
    {
      "epoch": 1,
      "seq": 2,
      "sender_id": "bob",
      "commit_sha256": "8825a34368a0a10051b2957bfc558fbefb2db74cb744e195a267e43db505f5dd",
      "included": true
    }
  ],
  "chain_valid": true,
  "violations": []
}
```

Field notes: `chain_valid` is the overall verdict (STH signature valid **and**
every commit included **and** the invariant holds); `included` is per-commit;
`sender_id` is recorded but **not** authorization-checked in this slice;
`violations` is empty exactly when `chain_valid` is true.

## 4. Fully offline — `monitor verify`

If you would rather not trust the network at all during verification, download the
signed **bundle** once and verify it with zero further network access. The bundle
aggregates the public key, the STHs, the full ordered entries, and the proofs into
a single JSON file.

```bash
# No network is used during this command — it reads only the local file.
./target/release/monitor verify <bundle.json>
```

To try it end to end with no server involved at all, generate a known-good
bundle and verify it:

```bash
./target/release/monitor gen-example fixture.json
./target/release/monitor verify fixture.json
```

```
$ ./target/release/monitor verify fixture.json
PASS  STH[0] signature (tree_size=3)
PASS  STH[1] signature (tree_size=5)
PASS  no equivocation between STH[0] and STH[1] (tree_size=3)
PASS  all entries satisfy tenant invariants
PASS  STH[0] root matches replayed entries
PASS  STH[1] root matches replayed entries
PASS  inclusion[0] leaf 1 in STH[1]
PASS  consistency[0] STH[0] -> STH[1]

OK: all checks passed
```

As with the HTTP path, a tampered leaf, forged proof, broken consistency, bad
signature, or equivocation makes `verify` print a `FAIL` report and **exit
non-zero**. Same checks, same trust model — just no network.

## The website explorer is a convenience, not the trust anchor

The page at [`website/transparency.html`](../website/transparency.html) lets you
type a conversation id in a browser and see its commit chain rendered. It is a
**demo for convenience only**: the browser does no verification itself — it calls
the serve layer's `GET /verify/group/<id>` endpoint, which runs the *same*
`verify-group` code you ran in step 3, and just visualizes the returned
`GroupReport`.

That means the explorer is exactly as trustworthy as the server hosting it. The
**trustworthy path is running the tool yourself** — `serve verify-remote`,
`serve verify-group`, or `monitor verify` on your own machine — because only then
does the verdict rest on the signature and the proofs you checked locally, rather
than on a server's word.

## Run it locally end to end (optional)

You can stand up the whole pipeline yourself to see every step. The dev server is
for **testing/demos only** — production is just a static host serving the
generated directory.

```bash
# Generate the immutable static tree from a signed bundle.
./target/release/serve generate --bundle bundle.json --out ./site

# Serve it locally (dev/demo only).
./target/release/serve serve --dir ./site --port 8787

# In another shell, verify it over HTTP — trusting only the public key.
./target/release/serve verify-remote http://127.0.0.1:8787
./target/release/serve verify-group --base http://127.0.0.1:8787 --group <conversation-id>
```

(The signed `bundle.json` itself is produced from the real `mls_commit_log` by the
[`builder`](../verifiable-log-builder/README.md) — `builder build --db <url|path>
--out bundle.json --timestamp <ms>` — which hashes each commit and never stores
the raw bytes.)
