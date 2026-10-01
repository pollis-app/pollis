# Verify the Pollis transparency log yourself

Pollis publishes an append-only [transparency log](./transparency.md) of every MLS
commit, every account identity-key version and every released binary. This guide
shows how to check it yourself: that the server has not forked, rolled back or
rewritten any of it.

The verifier trusts the log's ML-DSA-44 public key and nothing else. Signed tree
heads and Merkle proofs are checked against that key; the server, the database and
the host serving the files are not trusted. If any byte is altered, a signature or
proof check fails and the tool exits non-zero.

Below, `<base-url>` is where the log is published: **https://verify.pollis.com** in
production, or a dev server you run yourself.

`pollis-verify` checks the served key against a key compiled into the binary and
refuses to continue if they differ. A hostile host therefore cannot swap in its own
key and sign a forged history. To verify a log you run yourself, signed with your
own key, set `POLLIS_VERIFY_PINNED_KEYS_HEX` to that key (hex, 1312 bytes;
comma-separate several). It adds to the compiled key rather than replacing it, and
only you can set it. Leave it unset for production.

## 1. Get the verifier

The CLI is `pollis-verify`. It needs no credentials.

**Download.** Prebuilt binaries for Linux x86_64 and macOS (arm64 and x86_64) are on
[GitHub Releases](https://github.com/actuallydan/pollis/releases) under tags
`pollis-verify-v*`. Check the checksum and make it executable:

```bash
sha256sum -c pollis-verify-linux-x86_64.sha256
chmod +x pollis-verify-linux-x86_64
```

**Build from source** (any platform with a Rust toolchain):

```bash
# pollis-verify (and the operator `serve` binary)
cargo build -p verifiable-log-serve --release

# monitor, the offline bundle verifier (section 6)
cargo build -p verifiable-log --release
```

Both land in `target/release/`. The examples below assume `pollis-verify` is on
your `PATH`. `pollis-verify --version` prints the release number, plus the commit
for release builds or `(source build)` for local ones.

## 2. Verify the whole log: `pollis-verify remote`

```bash
pollis-verify remote <base-url>
```

This fetches all three trees and checks, for each: that the served key is the
pinned key, every STH signature, that `latest.json` matches the newest STH, that no
two heads equivocate, that the entries replay to each signed root, and every
inclusion and consistency proof. A passing run prints one `PASS` line per check and
exits `0` (output shortened; numbers are illustrative):

```
$ pollis-verify remote https://verify.pollis.com
PASS  served public_key.json is the pinned log key
PASS  STH[24] tree_size matches its URL
PASS  STH[24] signature
PASS  STH[49] tree_size matches its URL
PASS  STH[49] signature
PASS  latest.json matches the newest STH
PASS  latest.json signature
PASS  no equivocation between size 24 and size 49
PASS  entries.json count matches manifest
PASS  per-entry files match entries.json
PASS  all entries satisfy tenant invariants
PASS  STH[24] root matches replayed entries
PASS  STH[49] root matches replayed entries
PASS  inclusion: leaf 0 in size 49
…
PASS  consistency: size 24 -> size 49
PASS  account-keys: STH[12] signature (account context)
…
PASS  binaries: consistency: size 14 -> size 21

OK: all checks passed
```

Any failure prints `FAIL` on the offending line, ends with
`FAILED: one or more checks did not pass`, and exits non-zero. If the served key is
not the pinned key, it stops immediately with an error and exits `1`.

```bash
pollis-verify remote <base-url> && echo "log is intact" || echo "VERIFICATION FAILED"
```

### Exit codes

| code | meaning |
|---|---|
| `0` | passed |
| `1` | verification failed (bad signature, forged proof, fork, regression, unpinned key) or a network/parse error |
| `2` | version skew: the log uses a wire format newer than this binary supports |

`pollis-verify` reads the log's `format_version` before checking anything else. If
the log is newer than the binary, it says *"your pollis-verify is too old for this
log — upgrade it"* and exits `2`. That is not evidence of tampering; install a newer
`pollis-verify-v*` release. Binaries built before this check existed cannot report
skew: against a newer log, `remote` still passes but `group` reports `Found: no`.
If in doubt, upgrade.

## 3. Verify one conversation: `pollis-verify group`

```bash
pollis-verify group <base-url> <conversation-id>
```

The id is the opaque MLS conversation id (a ULID such as
`01KP443BSBXS3W1SZNTV5MXQ9C`), not a group name; the log contains no names. Only
members know it: leaves carry per-window pseudonyms, and `group` re-derives them
from the real id.

`group` verifies the latest STH signature first, selects the conversation's
commits, checks each one's inclusion proof against that head, and replays them
through the no-fork / no-regression invariant:

```
$ pollis-verify group https://verify.pollis.com 01KP443BSBXS3W1SZNTV5MXQ9C
Group:   01KP443BSBXS3W1SZNTV5MXQ9C
Found:   yes
STH:     tree_size 49  root b3f0f8a8f675996002633a03c50a2dd733f66ba6c3fe95e39ee4f04935dbe25f
Commits (seq order):
  epoch 0    seq 14     sender 4be1a0…9c3d  commit 79b6e5…cf7a  [included ✓]
  epoch 1    seq 15     sender e07f52…11ab  commit 3beb61…ba37  [included ✓]
  epoch 2    seq 16     sender e07f52…11ab  commit 42cf88…e057  [included ✓]

PASS: group chain is valid
```

A missing inclusion proof, a fork or an epoch regression is listed under
`Violations:`, followed by `FAIL: group chain is NOT valid` and a non-zero exit. An
id that is not in the log reports `Found: no` with an empty chain, which passes.

`group` refuses a log published before the pseudonym change (`format_version` below
2) with an error, rather than report an empty result.

`--json` prints the `GroupReport`, the same shape the `/verify/group/<id>` endpoint
returns:

```bash
pollis-verify group <base-url> <conversation-id> --json
```

```json
{
  "group_id": "01KP443BSBXS3W1SZNTV5MXQ9C",
  "found": true,
  "sth_tree_size": 49,
  "root_hex": "b3f0f8a8f675996002633a03c50a2dd733f66ba6c3fe95e39ee4f04935dbe25f",
  "commits": [
    {
      "generation": 0,
      "epoch": 0,
      "seq": 14,
      "sender_pseudonym": "4be1a0…9c3d",
      "commit_sha256": "79b6e5…cf7a",
      "included": true
    }
  ],
  "chain_valid": true,
  "violations": []
}
```

`chain_valid` is the overall verdict: the STH signature is valid, every commit is
included, and the invariant holds. `violations` is empty exactly when `chain_valid`
is true. `sender_pseudonym` is the published per-window pseudonym, not a user id,
and is not checked against group membership.

## 4. Verify one user's key history: `pollis-verify account`

The account-key tree, under `/v1/account-keys/...`, has one leaf per version of an
account's identity key.

```bash
pollis-verify account <base-url> <user-id>
```

`account` verifies the account tree's latest STH under that tree's own signing
context (a commit-log head will not verify here), selects the user's key versions,
checks each inclusion proof, and checks that `identity_version` strictly increases
with no duplicates. That is what rules out a silent key substitution.

```
$ pollis-verify account https://verify.pollis.com 01KP43R2QK8N0M5VHE3WXGN5H
User:    01KP43R2QK8N0M5VHE3WXGN5H
Found:   yes
STH:     tree_size 12  root 7c1f…b90a
Key history (seq order):
  v1    seq 3      key 9af2c1…7d4e  [included ✓]
  v2    seq 9      key 41bb08…12ff  [included ✓]

PASS: account key chain is valid
```

Failures are listed under `Violations:`, followed by
`FAIL: account key chain is NOT valid` and a non-zero exit. `--json` prints the
`AccountReport`. An unknown `user_id` reports `Found: no` and passes.

The desktop app runs the same verifier: `self_audit_account_key` for your own key
and `audit_peer_account_key` for a contact you have verified.

## 5. Verify a release: `pollis-verify release`

The binaries tree has one leaf per release artifact layer. `payload` is the
reproducible bytes with signing material normalized out; `signed` is the file users
download. The two are linked by a shared `payload_sha256`.

```bash
pollis-verify release <base-url> <tag>
```

`release` verifies the binaries tree's latest STH under its own context, selects the
tag's artifacts, checks each inclusion proof, and checks the binary invariant over
the whole tree (no re-issued artifact, no tag reappearing after a newer one, every
`signed` leaf preceded by its `payload` leaf):

```
$ pollis-verify release https://verify.pollis.com v1.3.6
Release: v1.3.6
Found:   yes
STH:     tree_size 21  root e7f84a0edc5c8ccf4cec6140d474040ad83eb9e0cb8de43336eaa870c7e1a761
Artifacts (publish order):
  darwin   aarch64  dmg       payload  payload fa863e…f4f2  artifact fa863e…f4f2  [included ✓]
  darwin   aarch64  dmg       signed   payload fa863e…f4f2  artifact e0f762…653f  [included ✓]
  windows  x86_64   nsis      payload  payload dcdc72…d469  artifact dcdc72…d469  [included ✓]
  windows  x86_64   nsis      signed   payload dcdc72…d469  artifact 9266ef…d2b7  [included ✓]
  linux    x86_64   appimage  payload  payload dccae0…6d82  artifact dccae0…6d82  [included ✓]
  linux    x86_64   deb       payload  payload 3273d6…5885  artifact 3273d6…5885  [included ✓]
  linux    x86_64   rpm       payload  payload ae3d9c…ae86  artifact ae3d9c…ae86  [included ✓]

PASS: release binaries tree is valid
```

Failures end in `FAIL` and a non-zero exit. `--json` prints the `ReleaseReport`,
which is the same function and shape as the published `/verify/release/<tag>`
report.

To tie the log to the file you downloaded, hash it and compare with the logged
`artifact_sha256` of the matching `signed` leaf (or the `payload` leaf for unsigned
artifacts, where both hashes are equal). Use `--json` to get full hashes:

```bash
sha256sum pollis-v1.3.6-linux.AppImage
```

## 6. Fully offline: `monitor verify`

To avoid trusting the network during verification, download a signed bundle once
and verify it locally. A bundle holds the public key, the STHs, the full ordered
entries and the proofs in one JSON file.

```bash
# Reads only the local file.
./target/release/monitor verify <bundle.json>

# Account-key and binaries bundles need --tree (default: commit-log).
./target/release/monitor verify --tree binaries <binaries-bundle.json>
```

`monitor` checks the bundle against the key the bundle itself carries; it has no
compiled-in pin. Compare that key with the pinned key in
[`SECURITY.md`](../SECURITY.md) yourself.

To try it without any server, generate a known-good bundle and verify it:

```
$ ./target/release/monitor gen-example fixture.json
wrote example fixture to fixture.json
$ ./target/release/monitor verify fixture.json
verifying against tree `commit-log`
PASS  bundle publishes at least one usable verifying key
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

A tampered leaf, forged proof, broken consistency, bad signature or equivocation
produces `FAIL` lines and a non-zero exit.

## 7. Verify build provenance with cosign and SLSA

Sections 1–6 rest on Pollis's own log key. Release artifacts also carry a keyless
cosign signature and a SLSA build-provenance attestation, both tied to Pollis's
GitHub Actions OIDC identity through sigstore/Fulcio and recorded in the public
Rekor log. Checking them involves no Pollis-held key, so they still hold if that
key were compromised or compelled.

Both sit next to each artifact on the CDN. For release `vX.Y.Z` and the Linux
AppImage:

```bash
BASE=https://cdn.pollis.com/releases/vX.Y.Z
ART=pollis-vX.Y.Z-linux.AppImage
curl -sSLO "$BASE/$ART"                 # the artifact
curl -sSLO "$BASE/$ART.sig"             # cosign detached signature
curl -sSLO "$BASE/$ART.pem"             # cosign signing certificate
curl -sSLO "$BASE/$ART.intoto.jsonl"    # SLSA build-provenance attestation
```

### cosign: the bytes were signed by the release workflow

```bash
cosign verify-blob \
  --certificate-identity-regexp '^https://github.com/actuallydan/pollis/\.github/workflows/desktop-release\.yml@refs/tags/v.*$' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  --signature   "$ART.sig" \
  --certificate "$ART.pem" \
  "$ART"
```

Trust is pinned by the workflow identity and GitHub's OIDC issuer; there is no
`--key`. A pass prints `Verified OK` and exits `0`. Altered bytes, or a signature
from any other identity, fail with a non-zero exit.

### SLSA: where and how it was built

The `.intoto.jsonl` is a SLSA v1 provenance attestation from
`actions/attest-build-provenance`, one per release, listing every artifact as a
subject. Verify it with the GitHub CLI:

```bash
gh attestation verify "$ART" \
  --bundle "$ART.intoto.jsonl" \
  --repo actuallydan/pollis \
  --cert-identity-regex '^https://github.com/actuallydan/pollis/\.github/workflows/desktop-release\.yml@refs/tags/v.*$' \
  --cert-oidc-issuer https://token.actions.githubusercontent.com
```

This confirms the artifact's digest is a subject of a provenance statement signed
by that workflow and logged in Rekor, and prints the source repo, commit and
workflow. `slsa-verifier verify-artifact --provenance-path "$ART.intoto.jsonl"
--source-uri github.com/actuallydan/pollis --source-tag vX.Y.Z "$ART"` is an
alternative.

**What this does not prove.** cosign and SLSA show that the Pollis release workflow
built these bytes at a specific commit. They do not show that the bytes rebuild
from source. That is the job of the reproducible build and the independent
rebuilder (`.github/workflows/rebuild-verify.yml`, Linux payload only; see
[`reproducible-builds-residuals.md`](./reproducible-builds-residuals.md)).

## The website explorer

[`website/transparency.html`](../website/transparency.html) takes a conversation id
and shows its commit chain. The browser does no verification: it calls a server's
`GET /verify/group/<id>` endpoint, which runs the same code as
`pollis-verify group`, and draws the returned `GroupReport`. It is only as
trustworthy as that server. For a verdict that rests on checks you ran, use
`pollis-verify` or `monitor` on your own machine.

## Run the pipeline locally

The dev server is for testing; production is a static host serving the generated
directory.

```bash
# Generate the static tree from a signed bundle.
./target/release/serve generate --bundle bundle.json --out ./site

# Serve it locally.
./target/release/serve serve --dir ./site --port 8787

# In another shell. The bundle is signed with your key, so pin it.
export POLLIS_VERIFY_PINNED_KEYS_HEX=<the bundle's public_key>
pollis-verify remote http://127.0.0.1:8787
pollis-verify group http://127.0.0.1:8787 <conversation-id>
```

`monitor gen-example` produces a bundle you can use here. A real `bundle.json` comes
from the [`builder`](../verifiable-log-builder/README.md):
`builder build --db <url|path> --out bundle.json --timestamp <ms>`.
