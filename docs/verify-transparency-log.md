# Verify the Pollis transparency log yourself

Pollis publishes an append-only [transparency log](./transparency.md) of every MLS
commit. This guide shows you how to **independently verify it** — proving the
server has not forked, rolled back, or rewritten any conversation's history.

The one rule that makes this meaningful:

> **You trust only the log's published Ed25519 public key, the signed tree head,
> and the Merkle proofs checked against it.** Not the server, not the database,
> not the host serving the files. If a single byte is tampered with, a signature
> or proof check fails and the tool exits non-zero.

Everywhere below, `<base-url>` is wherever the static log is published. In
production that is **https://verify.pollis.com**; locally it is a dev server you
run yourself.

## 1. Get the verifier

The auditor CLI is **`pollis-verify`**. You can download a prebuilt binary or
build it from source — it needs no credentials, since the read API is public.

**Download (recommended).** Grab the binary for your platform from the
[GitHub Releases](https://github.com/actuallydan/pollis/releases) page (tags
`pollis-verify-v*`), then make it executable and verify its checksum:

```bash
chmod +x pollis-verify-linux-x86_64
sha256sum -c pollis-verify-linux-x86_64.sha256
```

**Build from source.** With a Rust toolchain (`cargo`):

```bash
# Builds the `pollis-verify` auditor CLI (and the operator `serve` binary).
cargo build -p verifiable-log-serve --release

# For the fully offline path, also build the `monitor` bundle verifier.
cargo build -p verifiable-log --release
```

The binaries land in `target/release/`:

```
target/release/pollis-verify   # auditor CLI: remote + group + account + release
target/release/monitor         # offline bundle verifier
```

This guide uses `pollis-verify` (assume it's on your `PATH`, or prefix
`./target/release/`) and, for the offline path, `monitor`.

## 2. Verify the whole log over HTTP — `pollis-verify remote`

This fetches the entire public log over HTTP(S) and verifies it end to end:
every STH signature, equivocation across heads, that each entry replays to the
signed root, every inclusion proof, and every consistency proof — trusting only
the published public key.

```bash
pollis-verify remote <base-url>
```

A passing run prints a `PASS` line per check and exits `0`:

```
$ pollis-verify remote https://verify.pollis.com
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

OK: all checks passed
```

If **anything** fails — a tampered entry, a forged proof, a bad signature, a
`latest.json` that disagrees with the newest head — the offending line reads
`FAIL`, the summary reads `FAILED: one or more checks did not pass`, and the
command **exits non-zero**. That exit code is the whole point: it is computed from
the signature and the proofs, not from anything the server told you to believe.

```bash
pollis-verify remote <base-url> && echo "log is intact" || echo "VERIFICATION FAILED"
```

## 3. Verify one conversation — `pollis-verify group`

To check a single conversation's commit chain — that every one of its commits is
provably included in the signed log, and that its epochs are append-only and
fork-free — pass the base URL and the conversation id. The id is an opaque MLS
**conversation id** (a ULID like `01KP443BSBXS3W1SZNTV5MXQ9C`), **not** a group
name or slug — the public log deliberately carries no human-readable names.

```bash
pollis-verify group <base-url> <conversation-id>
```

It verifies the STH signature **first** (an unsigned head is worth nothing), then
selects that conversation's commits, checks each one's inclusion proof against the
signed head, and replays them through the no-fork / no-epoch-regression invariant:

```
$ pollis-verify group https://verify.pollis.com 01KP443BSBXS3W1SZNTV5MXQ9C
Group:   01KP443BSBXS3W1SZNTV5MXQ9C
Found:   yes
STH:     tree_size 49  root b3f0f8a8f675996002633a03c50a2dd733f66ba6c3fe95e39ee4f04935dbe25f
Commits (seq order):
  epoch 0    seq 14     sender 01KP43…GN5H  commit 79b6e5…cf7a  [included ✓]
  epoch 1    seq 15     sender 01KP3G…02RF  commit 3beb61…ba37  [included ✓]
  epoch 2    seq 16     sender 01KP3G…02RF  commit 42cf88…e057  [included ✓]

PASS: group chain is valid
```

A valid chain exits `0`; a missing inclusion proof, a fork, or an epoch regression
lists the reason under `Violations:`, prints `FAIL: group chain is NOT valid`, and
exits non-zero. (A conversation id that isn't in the log reports `Found: no` with
an empty — and therefore vacuously valid — chain.)

Add `--json` to get the machine-readable `GroupReport` (the exact shape the HTTP
endpoint and the website explorer consume):

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
      "epoch": 0,
      "seq": 14,
      "sender_id": "01KP43R2QK8N0M5VHE3WXGN5H",
      "commit_sha256": "79b6e5…cf7a",
      "included": true
    },
    {
      "epoch": 1,
      "seq": 15,
      "sender_id": "01KP3GRSY1QY760ZEC12R102RF",
      "commit_sha256": "3beb61…ba37",
      "included": true
    }
  ],
  "chain_valid": true,
  "violations": []
}
```

Field notes: `chain_valid` is the overall verdict (STH signature valid **and**
every commit included **and** the invariant holds); `included` is per-commit;
`sender_id` is a user id, recorded but **not** authorization-checked in this slice;
`violations` is empty exactly when `chain_valid` is true.

## 4. Verify one user's account-key history — `pollis-verify account`

Pollis publishes a second tree — the **account-key directory** — under
`/v1/account-keys/...`, with one leaf per account identity-key version. To check
a single user's key history — that every published version is provably included
in the signed account tree, and that `identity_version` is append-only and never
regresses (no silent key substitution) — pass the base URL and the opaque
`user_id`:

```bash
pollis-verify account <base-url> <user-id>
```

It verifies the account STH signature **first** (under the account tree's own
domain context — a commit-log head cannot stand in here), selects that user's
key versions, checks each one's inclusion proof against the signed head, and
replays them through the no-regression / no-duplicate-version invariant:

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

A valid chain exits `0`; a missing inclusion proof, a duplicate version, or a
version regression lists the reason under `Violations:`, prints `FAIL: account
key chain is NOT valid`, and exits non-zero. As with `group`, add `--json` for
the machine-readable `AccountReport`. (A `user_id` not in the log reports
`Found: no` with a vacuously-valid empty chain.)

The Pollis app runs this same verifier internally — `self_audit_account_key`
checks your own key, `audit_peer_account_key` checks a contact you've verified —
so the desktop client and an independent auditor reach the identical verdict.

## 5. Verify a shipped release — `pollis-verify release`

Pollis publishes a third tree — the **binaries directory** — with one leaf per
shipped release artifact **layer**: `payload` is the reproducible pre-signature
bytes, `signed` is the wrapper users actually download, and the two are bound
by a shared `payload_sha256`. To check that a release tag's artifacts are
provably recorded in the signed binaries tree, pass the base URL and the tag:

```bash
pollis-verify release <base-url> <tag>
```

It verifies the binaries STH signature **first** (under the binaries tree's own
domain context), selects the tag's artifacts, checks each one's inclusion proof
against the signed head, and asserts the tree-wide binary invariant:

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

A valid release exits `0`; a missing inclusion proof or a violated binary
invariant lists the reason under `Violations:`, prints `FAIL`, and exits
non-zero. Add `--json` for the machine-readable `ReleaseReport` — the exact
shape the static `/verify/release/<tag>` report carries, computed by the same
function, so the CLI and the hosted report can never disagree.

To connect the log to **the file you downloaded**, hash it and compare against
the logged `artifact_sha256` of the matching `signed` leaf (or `payload` for
unsigned artifacts, where the two hashes are equal):

```bash
sha256sum pollis-v1.3.6-linux.AppImage
```

## 6. Fully offline — `monitor verify`

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

## 7. Verify the keyless build provenance yourself — cosign + SLSA (no Pollis key)

The transparency log above (steps 1–6) is anchored by **Pollis's own** Ed25519
key. Released artifacts carry a **second, independent** anchor that Pollis does
**not** control: a keyless **cosign** signature and a **SLSA build-provenance**
attestation, both bound to Pollis's **GitHub Actions OIDC identity** via
sigstore/Fulcio and recorded in the **public Rekor** log. Verifying them trusts
**only** the GitHub Actions identity + Rekor — *no Pollis-held key is on this
path at all*, which is the point: it holds even against a compromised or
compelled Pollis signing key.

Both live next to each artifact on the CDN. For a release `vX.Y.Z` and, say, the
Linux AppImage:

```bash
BASE=https://cdn.pollis.com/releases/vX.Y.Z
ART=pollis-vX.Y.Z-linux.AppImage
curl -sSLO "$BASE/$ART"                 # the artifact
curl -sSLO "$BASE/$ART.sig"             # cosign detached signature
curl -sSLO "$BASE/$ART.pem"             # cosign signing certificate
curl -sSLO "$BASE/$ART.intoto.jsonl"    # SLSA build-provenance attestation
```

### cosign — confirm the raw bytes were signed by the Pollis workflow

`cosign verify-blob` checks the signature + certificate against Rekor and
asserts the signing identity is the Pollis release workflow. Trust is pinned by
the `--certificate-identity-regexp` (the workflow that is allowed to sign) and
`--certificate-oidc-issuer` (GitHub's OIDC issuer) — nothing else:

```bash
cosign verify-blob \
  --certificate-identity-regexp '^https://github.com/actuallydan/pollis/\.github/workflows/desktop-release\.yml@refs/tags/v.*$' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  --signature   "$ART.sig" \
  --certificate "$ART.pem" \
  "$ART"
```

A passing run prints `Verified OK` and exits `0`. If the bytes were tampered
with, or signed by any identity other than the Pollis `desktop-release.yml`
workflow, verification fails and the command exits non-zero. Note there is **no
`--key` flag** — verification rests entirely on the Fulcio certificate's OIDC
identity and the Rekor transparency-log inclusion, not on any Pollis key.

### SLSA provenance — confirm where and how it was built

The `.intoto.jsonl` is a SLSA v1 in-toto build-provenance attestation produced by
`actions/attest-build-provenance` (a single release-wide attestation carrying
each artifact as a subject). Verify the artifact against it offline with the
GitHub CLI, trusting only the GitHub Actions identity + issuer:

```bash
gh attestation verify "$ART" \
  --bundle "$ART.intoto.jsonl" \
  --repo actuallydan/pollis \
  --cert-identity-regexp '^https://github.com/actuallydan/pollis/\.github/workflows/desktop-release\.yml@refs/tags/v.*$' \
  --cert-oidc-issuer https://token.actions.githubusercontent.com
```

It confirms the artifact's digest is a subject of a provenance statement signed
by the pinned Pollis workflow and logged in Rekor, and prints the source repo +
commit + workflow it was built from. (`slsa-verifier verify-artifact
--provenance-path "$ART.intoto.jsonl" --source-uri github.com/actuallydan/pollis
--source-tag vX.Y.Z "$ART"` is an equivalent check with the standalone SLSA
verifier.)

> **What this proves — and what it does not.** cosign + SLSA prove **build
> provenance**: these exact bytes were produced by the pinned Pollis GitHub
> Actions workflow at a specific commit, recorded in a public log Pollis does not
> control. They do **not**, by themselves, prove the bytes **reproduce from
> source** — that is the reproducible-build story (`docs/reproducible-builds-residuals.md`
> + the independent rebuilder in `.github/workflows/rebuild-verify.yml`, asserted
> for the Linux payload). The two anchors are complementary: the binaries
> transparency log + rebuilder say "the honest source produces these bytes";
> cosign/SLSA say "the Pollis CI built them, provably, in a log no single party
> owns."

## The website explorer is a convenience, not the trust anchor

The page at [`website/transparency.html`](../website/transparency.html) lets you
type a conversation id in a browser and see its commit chain rendered. It is a
**demo for convenience only**: the browser does no verification itself — it calls
the serve layer's `GET /verify/group/<id>` endpoint, which runs the *same*
`group` verification code you ran in step 3, and just visualizes the returned
`GroupReport`.

That means the explorer is exactly as trustworthy as the server hosting it. The
**trustworthy path is running the tool yourself** — `pollis-verify remote`,
`pollis-verify group`, or `monitor verify` on your own machine — because only then
does the verdict rest on the signature and the proofs you checked locally, rather
than on a server's word.

## Run it locally end to end (optional)

You can stand up the whole pipeline yourself to see every step. The dev server is
for **testing/demos only** — production is just a static host serving the
generated directory. Generating and serving use the operator `serve` binary;
verifying uses `pollis-verify` exactly as above.

```bash
# Generate the immutable static tree from a signed bundle.
./target/release/serve generate --bundle bundle.json --out ./site

# Serve it locally (dev/demo only).
./target/release/serve serve --dir ./site --port 8787

# In another shell, verify it over HTTP — trusting only the public key.
pollis-verify remote http://127.0.0.1:8787
pollis-verify group http://127.0.0.1:8787 <conversation-id>
```

(The signed `bundle.json` itself is produced from the real `mls_commit_log` by the
[`builder`](../verifiable-log-builder/README.md) — `builder build --db <url|path>
--out bundle.json --timestamp <ms>` — which hashes each commit and never stores
the raw bytes.)
