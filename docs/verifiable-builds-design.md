# Verifiable / Reproducible Builds + Binary Transparency

**Status:** Partly shipped. **P0–P2 are SHIPPED** — the `binaries` tenant tree + `BinaryRecord` schema + `BinaryInvariant` (`verifiable-log-builder/src/binaries.rs`), `serve` emitting `/v1/binaries/...` and `/verify/release/<tag>` (`verifiable-log-serve/src/release.rs`), the `pollis-verify release <tag>` auditor subcommand, and the release-pipeline append job that logs each artifact's payload + signed hashes to **verify.pollis.com/v1/binaries** trusting only the pinned Ed25519 key (`175ebfef…7148`). What P2 delivers is a **correct leaf structure (both hashes + the pinned build recipe) and a working publish/verify pipeline** — **not** yet bit-for-bit reproducibility, cosign/SLSA provenance, or an in-app verify button. **P4 (in-app "verify this build") is now SHIPPED** — the optional Security-page affordance (`verify_own_build` in `pollis-core`, `BuildVerifyLine` + "This build" section on `SecurityPage`) reusing the account-key self-audit path. **P5 (full reproducibility + independent rebuilder) is SHIPPED for Linux (#484)** — see §1.5 / §6 Phase 5. **P3 (cosign/SLSA keyless provenance) is now SHIPPED (#484)** — every released installer + updater bundle carries a keyless SLSA build-provenance attestation *and* a cosign signature anchored in the **public Rekor** log via the GitHub Actions OIDC identity (no Pollis key on that verification path), a second independent anchor; see §3 / §6 Phase 3. The full design of record follows.
**Author lens:** performance, security, and *zero user burden* are first-class
constraints, called out explicitly at each decision.
**Audience:** maintainers deciding whether/how to build this, plus the security
auditors who read `docs/security-whitepaper.md`.

---

## 0. The gap this closes (ground truth)

`docs/security-whitepaper.md` §1.1 (line 22) states, verbatim:

> "The trust delegation is the same as Signal Desktop or WhatsApp Desktop: the
> binary is trusted at install time… **Reproducible builds are not currently a
> goal; binary integrity rests on platform code-signing** (Apple Developer ID +
> notarization on macOS, Azure Trusted Signing on Windows…). The auto-update path
> verifies the same OS-native signature on every downloaded installer before
> launch…"

That paragraph is an honest admission of the single largest hole in Pollis's
threat story. Everything else in the whitepaper — MLS, PIN-wrapped keys, the
account-key directory, the MLS commit log — assumes **the binary running on the
user's device is the one whose source you can read**. Code-signing does *not*
establish that. A Developer ID / Azure Trusted Signing signature proves only
"the holder of Pollis's signing key produced these exact bytes." It says nothing
about *what those bytes do*. A compromised or legally compelled release — or a
single targeted build handed to one user — is validly signed and passes
Gatekeeper / Authenticode / the minisign updater check. MLS/E2EE has the same
limitation from the other side: it proves the *server* can't read plaintext,
but a backdoored *client* trivially can. (The relay overlay,
`docs/relay-overlay-design.md`, is orthogonal here — an optional, currently
deferred IP-metadata defense-in-depth that forwards already-encrypted bytes;
it is explicitly not part of the E2EE proof.) **Neither the protocol nor the
signing proves the E2EE claim end to end unless you can also prove the client
binary is honest.** This document is the piece that closes that loop.

The thesis Pollis already sells — *don't trust the operator, verify* — is, today,
only half-true: you can verify the **server's** behaviour (transparency log,
`pollis-verify`), but you cannot verify the **operator's own binary**. This
feature makes the second half true too, and it does so by **reusing the exact
key-transparency machinery already shipped**, extended from keys to binaries.

### What already exists (and that we extend, not replace)

| Asset | File / crate | What it gives us for free |
|---|---|---|
| RFC 6962 Merkle-log core + offline verifier | `verifiable-log/` (`src/merkle.rs`, `src/sth.rs`, `src/log.rs`, `src/proof.rs`) | Append-only tree, STH signing/verify, inclusion + consistency proofs, equivocation detection, `TenantInvariant` hook, `monitor` CLI — **never panics, no clock, no network** (`verifiable-log/README.md`). |
| Tenant → signed bundle builder | `verifiable-log-builder/` | Reads a real libSQL table, hashes each row's payload, drops raw bytes, emits a signed monitor bundle deterministically (timestamp is passed in, never `SystemTime::now`). |
| Static read API + auditor CLI | `verifiable-log-serve/` (`serve`, `pollis-verify`) | Immutable `/v1/...` JSON directory served from any dumb host; `pollis-verify remote|group|account` verifies trusting only the pinned Ed25519 key. |
| Domain-separated multi-tree publishing | `docs/transparency.md` §"Two domain-separated trees" | Two trees today (commit log `…:sth:v1`, account keys `…:sth:v1:account-keys`) under one signing key, one static site. **Adding a third tree is a solved pattern.** |
| Daily publish + self-audit + tripwire | `.github/workflows/transparency-publish.yml` | Builds signed bundles, syncs to R2 (`verify.pollis.com`), re-verifies what's actually served, and runs an across-run equivocation tripwire from cache. |
| Auditor CLI release | `.github/workflows/verifier-release.yml` | Ships `pollis-verify` binaries + SHA256SUMS with the **pinned public key** in the release body: `175ebfef…7148`. |
| Desktop release pipeline | `.github/workflows/desktop-release.yml` | Per-OS Tauri build, Apple notarization, Azure Trusted Signing (`.codesight/wiki/windows-signing.md`), **minisign** updater `.sig` + `update-*.json` manifests, R2 upload, GitHub release, AUR. |
| In-app audit surface | `frontend/src/pages/SecurityPage.tsx` (Account-key section, `useSelfAuditAccountKey`, `AccountKeyAuditLine`) | A page where the running app *already* self-audits its identity key against the public log. The natural home for a "verify this build" line. |

The design principle throughout: **a third tenant tree ("binaries") on the
existing log, a provenance sidecar on the release, and one more advisory line on
the Security page.** Almost no net-new systems.

---

## 1. Reproducible builds

Reproducibility is the *substance*; the transparency log is the *distribution
mechanism*. A logged hash of a binary nobody can independently reproduce only
proves "Pollis shipped this" — which code-signing already proves. The log becomes
*trust-establishing* only when a third party can rebuild the source at a tag and
get **the same bytes**, so a divergence is provable evidence of tampering. So we
separate two artifacts for every platform:

- **The reproducible payload** — the deterministic output of `cargo build` +
  `vite build` + Tauri bundling *before* any signature is applied. This is what
  we make bit-for-bit reproducible and what we log.
- **The signed wrapper** — the notarized `.dmg` / Authenticode `.exe` /
  minisign-signed updater bundle. This is inherently **non-reproducible** (see
  §1.5) and is logged separately as a *derived* artifact bound to the payload.

### 1.1 Toolchain pinning (the foundation)

Today the release workflow uses `dtolnay/rust-toolchain@stable` — **not
reproducible**, because "stable" floats. Determinism requires an exact,
content-addressed toolchain:

- **Rust:** add `rust-toolchain.toml` pinning `channel = "1.NN.0"` and the exact
  `targets`. CI switches to `dtolnay/rust-toolchain@1.NN.0`. The pinned version
  is itself part of the recorded provenance (§3), so a rebuilder installs the
  same rustc byte-for-byte.
- **Node/pnpm:** `packageManager` field + `.nvmrc` pin exact pnpm and Node
  versions (CI already uses Node 20 + `pnpm/action-setup`; make them exact, not
  major-only).
- **System toolchains that touch codegen:** the Linux capture helper's bindgen
  runs against *system* PipeWire headers (`desktop-release.yml` lines 252–303
  document this at length: ubuntu-22.04 vs 24.04 produce different structs).
  bindgen output is a **reproducibility hazard** — it embeds host header layout.
  Two mitigations, in order of preference: (a) vendor a pinned `bindings.rs` and
  gate regeneration behind a feature so release builds never invoke bindgen; or
  (b) pin the exact base image (`ubuntu-24.04` at a digest) and treat the helper
  as a separately-reproduced sub-artifact. The doc already isolates this helper
  in its own job — we lean into that isolation.
- **meson/ninja for `webrtc-audio-processing-sys`:** pin exact versions
  (`meson>=1.3` today is a floor, not a pin). This C++ dep is the single biggest
  practical reproducibility risk after signing — see §1.5.

> **Performance lens:** pinning does not slow builds; `Swatinem/rust-cache@v2` is
> already in place and keys on the lockfile + toolchain, so a pinned toolchain
> actually *improves* cache hit rates.

### 1.2 Locked, vendored dependencies

- `Cargo.lock` is committed (274 KB — present). The `transparency-publish`
  workflow already builds `--locked`; **the desktop build must too.** Add
  `--locked` to every `cargo build` / `tauri build` invocation so a drifted lock
  fails the build instead of silently resolving new versions.
- Consider `cargo vendor` into a checked-in `vendor/` for release builds, so the
  crate bytes are pinned independent of crates.io availability/mutation. This is
  optional for reproducibility (the lock hashes suffice) but valuable for
  **supply-chain durability** and offline rebuild by an auditor.
- Frontend: `pnpm-lock.yaml` present (162 KB); enforce `pnpm install
  --frozen-lockfile` in release CI (today it's plain `pnpm install`).

### 1.3 Cargo / Rust build determinism

The known non-determinism sources and their fixes:

- **Embedded absolute paths.** rustc bakes `$HOME/.cargo/registry/...` and the
  build directory into debug info and panic messages. Fix with
  `--remap-path-prefix` (via `RUSTFLAGS` / `.cargo/config.toml`
  `[build] rustflags`): remap the cargo home, the vendor dir, and the workspace
  root to stable logical roots (`/cargo`, `/src`). The repo already ships a
  `.cargo/` dir — this config lands there.
- **Build timestamps / `SOURCE_DATE_EPOCH`.** Anything embedding "now" (some
  crates, resource compilers, archive mtimes) must read `SOURCE_DATE_EPOCH`. Set
  it in CI to the **commit timestamp of the release tag** (deterministic and
  independently recoverable by a rebuilder from `git`).
- **Codegen parallelism nondeterminism.** Set `codegen-units` deterministically
  and avoid PGO for release (PGO profiles are host-dependent). Prefer
  `codegen-units = 1` for the release profile — slower build, but deterministic
  and marginally faster runtime. **Performance lens:** this is a *build-time*
  cost paid on CI runners, not a user-facing one.
- **`option_env!` baked config.** The release bakes `TURSO_URL`, `LOG_DB_URL`,
  the **read-only** tokens, R2 public URL, LiveKit URL, delivery URL, etc. into
  the binary (`desktop-release.yml` "Load production secrets"). These are part of
  the reproducible input and must be **declared in the provenance** so a
  rebuilder supplies the identical values. They are non-secret by design (RO
  token, public URLs) — but the *build inputs manifest* must pin them, or an
  honest rebuild diverges. This is the subtlest reproducibility gotcha in
  Pollis's specific pipeline and must be documented as a public "build recipe."

### 1.4 Frontend (Vite) bundle determinism

- Vite/Rollup output is largely deterministic given a frozen lockfile, but
  content-hash filenames and chunk ordering can drift across Rollup versions —
  pinning the lockfile handles this.
- **Kill non-deterministic inputs:** any `Date.now()` / build-time timestamp
  banner, any `Math.random`-seeded chunk salt, source-map absolute paths
  (`build.sourcemap` path roots — remap or disable for release). Set a fixed
  `define` for build metadata rather than injecting wall-clock time.
- The Vite build runs *inside* `tauri build`, so its output is an input to the
  bundler; we hash the **final bundle**, not Vite output in isolation, so Vite
  determinism is necessary but the acceptance test is at the Tauri-bundle layer.

### 1.5 The hard parts — an honest accounting

This is where a rigorous spec earns its keep. Not everything is cheaply
reproducible, and pretending otherwise would be dishonest to auditors.

> **Shipped status (P5, #484).** The Linux AppImage payload is now hardened to
> be reproducible *modulo a documented residual list*: the toolchain is pinned
> (`rust-toolchain.toml` → `1.96.0`, `dtolnay/rust-toolchain@1.96.0` in CI),
> absolute build paths are remapped (`--remap-path-prefix` for `$HOME`, cargo
> home, workspace root), `SOURCE_DATE_EPOCH` is the tag commit's seconds, JS
> inputs are frozen (`--frozen-lockfile`), and the desktop build runs `cargo
> build --locked`. The published, itemized residual list lives in
> **`docs/reproducible-builds-residuals.md`** and an independent, fork-runnable
> reproducer lives in **`.github/workflows/rebuild-verify.yml`**. The items below
> remain best-effort exactly as this section describes; the residual doc is their
> canonical, per-release-auditable form. **Biggest residual (call it out):** the
> client still bakes some build-recipe inputs as *secrets* via `option_env!`
> (`R2_SECRET_KEY`, `LIVEKIT_API_SECRET`), so a *fully secretless* third party
> cannot yet bit-reproduce even the Linux payload — only a party holding the
> published recipe can. Stripping secrets from the client binary is the next
> reproducibility milestone.

1. **Code-signing is non-reproducible *by construction*.** A notarized `.dmg`
   embeds an Apple timestamp + a CMS signature; an Authenticode `.exe` embeds an
   RFC 3161 timestamp from `timestamp.acs.microsoft.com` (mandatory — Azure certs
   have 3-day rolling validity, `.codesight/wiki/windows-signing.md`); the
   minisign `.sig` is over the signed artifact. **None of these can be
   reproduced** by a third party without Pollis's private keys. **Resolution:**
   we reproduce and log the **pre-signature payload** (the built `.app`
   bundle contents, the raw NSIS-input tree, the AppImage squashfs payload), and
   log the signed wrapper as a *separate derived hash* with a documented,
   verifiable **binding**: "signed artifact `X` wraps reproducible payload `P`."
   The verifier's chain is: rebuild → get `P` → confirm `P` is logged → confirm
   the shipped signed artifact strips to the same `P`. For macOS `.app` and
   AppImage the payload is directly extractable; for Windows NSIS the reproducible
   unit is the **unsigned `pollis.exe` + bundled resources** the installer wraps,
   verified by extracting the installer.

2. **Notarization mutates the artifact.** Apple's notary service can staple a
   ticket, changing bytes post-build. We log the pre-staple hash and treat the
   staple as part of the signed-wrapper layer, not the reproducible payload.

3. **Platform / cross-compile nondeterminism.** macOS is built on `macos-latest`
   (ARM), Windows on `windows-latest`, Linux on `ubuntu-22.04` (chosen for a low
   glibc floor). Reproducibility is **per-target**: a rebuilder must match the OS
   image. We pin runner images to digests where possible and publish the exact
   image identity in the provenance. We do **not** promise cross-OS reproducibility
   (you cannot bit-reproduce a macOS build on Linux).

4. **`webrtc-sys` / `libwebrtc` + `webrtc-audio-processing-sys`.** These vendored
   C/C++ builds (clang, meson, VAAPI wrappers) are the least-controlled inputs.
   Native compilers embed paths and can vary across patch versions. Pinning
   clang/meson/ninja exactly and remapping paths gets most of the way; the honest
   position is that these deps are the **most likely single source of a
   non-reproducing byte**, and the first reproducibility milestone should
   *measure* their contribution before promising full bit-reproducibility. It is
   acceptable — and still valuable — to ship **"reproducible modulo a documented,
   audited set of vendored native blobs whose own hashes are pinned and logged."**

5. **Sidecar helpers.** `pollis-capture-linux` (built on 24.04), `-macos`,
   `-proto` are bundled as externalBins. Each is a sub-artifact with its own
   reproducibility story; the top-level bundle hash depends on them, so they must
   be reproduced (or hash-pinned) first.

**Bottom line for §1:** full bit-for-bit reproducibility of the *signed*
artifacts is not achievable and not the goal. The goal is a **reproducible
payload per platform**, with signing/notarization cleanly separated as a
non-reproducible outer layer that is itself transparency-logged and bound to the
payload. This is exactly how Signal, Tor Browser, and reproducible-builds.org
frame it, and it is honest. **As of P5 this framing is shipped for Linux** (the
reproducible unit), with the non-reproducible layers enumerated and per-release
auditable in `docs/reproducible-builds-residuals.md`; macOS/Windows payload
reproduction stays best-effort pending a matching-runner reproducer.

---

## 2. Binary transparency log (extend the existing Merkle log)

We add a **third tenant tree** to the existing `verifiable-log` infrastructure,
alongside the commit-log tree and account-key tree already documented in
`docs/transparency.md`. No new crate, no new signing key, no new service — a new
tenant, a new domain-separation context, a new `/v1/...` path prefix, and a new
`pollis-verify` subcommand.

### 2.1 Domain separation

Following the existing pattern (`transparency.md`: commit log signs under
`…:sth:v1`, account keys under `…:sth:v1:account-keys`), the binary tree signs
STHs under a new context:

```
"pollis-verifiable-log:sth:v1:binaries"
```

so a binary STH can never be replayed as a commit-log or account-key STH, and
vice-versa. It reuses the **same** Ed25519 signing key already pinned in
`verifier-release.yml` (`175ebfef…7148`) — one key, three trees, three contexts.
The tree is served under `/v1/binaries/...` mirroring `/v1/account-keys/...`.

### 2.2 Log entry schema (the `binaries` tenant)

Each released *artifact* (one per platform × bundle-type × arch) is one leaf. The
tenant's `Entry.data` is the compact, field-ordered JSON of a `BinaryRecord`
(same discipline as the commit-log leaf in `verifiable-log-builder/README.md` —
serde declaration order, no insignificant whitespace, so the encoding is
deterministic):

```jsonc
{
  "release_tag": "v1.3.0",            // git tag that produced this
  "commit": "<40-hex git sha>",       // exact source revision
  "platform": "darwin",              // darwin | windows | linux
  "arch": "aarch64",                 // aarch64 | x86_64
  "bundle": "dmg",                   // dmg | app | nsis | appimage | deb | rpm
  "artifact_name": "pollis-v1.3.0-macos.dmg",
  "layer": "signed",                 // "payload" (reproducible) | "signed" (wrapped) | "exe" (main binary as installed)
  "payload_sha256": "<hex>",         // hash of the reproducible pre-signature payload
  "artifact_sha256": "<hex>",        // hash of the *shipped* artifact (== payload_sha256 for layer=payload)
  "toolchain": {                      // the reproducibility recipe, pinned
    "rustc": "1.NN.0",
    "node": "20.x.y",
    "pnpm": "9.x.y",
    "runner_image": "macos-14@<digest>",
    "source_date_epoch": 1700000000
  },
  "provenance_uri": "cdn.pollis.com/releases/v1.3.0/pollis-v1.3.0-macos.dmg.intoto.jsonl"
}
```

Design notes:
- **Two leaves per shipped file** where signing applies: one `layer:"payload"`
  (the reproducible unit) and one `layer:"signed"` (the shipped bytes), sharing
  `commit`/`release_tag`, joined by `payload_sha256`. This is what lets the
  verifier prove "the honest reproducible payload `P` is inside the signed thing
  I downloaded" while keeping `P` independently reproducible.
- **Plus an `exe` leaf** on every bundle except the AppImage, holding the sha256
  of the main executable *as installed* (`Contents/MacOS/pollis`, `pollis.exe`,
  `usr/bin/pollis`) in `artifact_sha256`, joined to its payload leaf by the same
  `payload_sha256`. Rationale in §4.2: `payload` leaves hash an extracted
  directory tree or the installer file, so a *running* app has no preimage for
  them and the in-app check could never match on macOS/Windows/deb/rpm. The
  AppImage needs no `exe` leaf — its shipped bytes ARE the payload and the app
  hashes them directly via `$APPIMAGE`, a strictly stronger check. Adding a
  `Layer` variant is backward-compatible: existing leaves' canonical bytes, and
  therefore every published inclusion proof, are untouched.
- The leaf commits to a **content hash + full recipe**, never to binary bytes —
  same privacy/space property as the commit-log builder (hash, drop the blob).
- `provenance_uri` links each leaf to its SLSA/in-toto attestation (§3), so the
  log entry and the provenance are cross-referenced but independently fetchable.

### 2.3 Tenant invariant (`BinaryInvariant`)

Registered via the existing `TenantInvariant` hook. Per `(platform, arch, bundle,
layer)` it enforces:

- **No silent re-issue:** two leaves with the same `(release_tag, platform, arch,
  bundle, layer)` but different `artifact_sha256` is a **fork** → aborts the
  build (mirrors the commit-log "no fork" rule). A legitimate re-release must use
  a new tag.
- **Monotonic releases:** `release_tag` is append-only in publish order; a leaf
  cannot reference a commit that isn't an ancestor of a previously-logged tag on
  the release branch (a weak, cheap supply-chain sanity rule).
- **Derived-layer pairing:** every non-`payload` leaf (`signed`, `exe`) must have
  a matching `layer:"payload"` leaf with equal `payload_sha256` earlier in the
  tree. Stated over "not payload" rather than per-variant, so a future layer
  inherits the rule instead of silently escaping it.

Because these run inside `verifiable-log`'s replay, `pollis-verify` re-checks them
independently — the app and CLI can never disagree, same guarantee the existing
trees enjoy.

### 2.4 Append flow in `desktop-release.yml`

The release workflow currently ends by uploading artifacts, minisign manifests,
`latest.json`, and the GitHub release. We insert a **new job, `attest-and-log`,
after `release`** (it needs the built + signed artifacts in hand):

1. **Compute hashes.** For each platform artifact: extract the reproducible
   payload (`.app` contents / AppImage squashfs / unsigned exe+resources from the
   NSIS installer), hash it → `payload_sha256`; hash the shipped signed file →
   `artifact_sha256`.
2. **Emit `BinaryRecord` leaves** into a small JSON the builder consumes.
3. **Append to the binaries tree.** The `verifiable-log-builder` gains a
   `--binaries-in <records.json>` mode (analogous to `--account-out`): it appends
   the records to the `binaries` tenant and emits a `binaries-bundle.json`, signed
   with `STH_SIGNING_KEY`, timestamped with the **tag commit timestamp** (not
   `now`, preserving determinism and the "byte-stable head" property the existing
   publish relies on).
4. **Where the source of truth lives.** Two clean options; recommend **(A)**:
   - **(A) DB-backed, consistent with today's model.** Insert the records into a
     new `released_binary_log` table (its own migration — take the next free
     migration number at implementation time; additive-only). The daily
     `transparency-publish.yml` already rebuilds trees from DB tables on a
     cadence and does the R2 sync + self-audit + tripwire — we simply teach it to
     also read `released_binary_log` and emit `/v1/binaries/...`. **This reuses
     the entire publish/audit/tripwire pipeline unchanged in shape**, and means
     the release workflow only needs write access to insert rows (through the
     Delivery Service or an admin token, matching the commit-log DB pattern), not
     to touch R2/verify.pollis.com directly.
   - **(B) Release-workflow direct publish.** The release job builds the binaries
     bundle and syncs `/v1/binaries/...` to the transparency R2 bucket itself.
     Faster to appear (no wait for the daily cron) but duplicates the careful
     two-pass/cache-split/tripwire logic already in `transparency-publish.yml`.
     Rejected as primary to avoid two code paths writing the same tree.
   - **Recommendation:** (A) for the tree, with a **manual
     `workflow_dispatch` of `transparency-publish.yml` triggered at the end of the
     release** so the binary appears within minutes, not up to 24h. The workflow
     already treats `workflow_dispatch` as "rebuild now."

> **Zero-user-burden lens:** none of this touches the user or even the release
> operator's manual steps — it's additional automated jobs on a tag push. The
> operator keeps doing `git tag vX.Y.Z && git push --tags`.

### 2.5 Static read API additions

Mirroring the existing tables in `transparency.md`:

| URL | Contents |
|---|---|
| `/v1/binaries/sth/latest.json` | newest binaries-tree STH (`no-cache`) |
| `/v1/binaries/sth/<n>.json` | STH at size n (immutable) |
| `/v1/binaries/entries.json` | full ordered `[BinaryRecord]` |
| `/v1/binaries/proof/inclusion/<size>/<idx>.json` | inclusion proof |
| `/verify/release/<tag>.json` | precomputed report: every artifact for a tag, its two hashes, inclusion status, provenance link |

`/verify/release/<tag>` is the binary analogue of `/verify/group/<id>` and
`/verify/account/<user>` — a precomputed convenience report the website explorer
and the in-app check consume, backed by the same `verify` code the CLI runs.

### 2.6 Monitor / gossip — *who watches the log*

A transparency log only has teeth if someone independent watches it. The existing
account-key/commit-log design leans on `pollis-verify remote` run by third
parties + the across-run tripwire in CI. For binaries we add:

- **First-party tripwire (already have the pattern).** Extend
  `transparency-publish.yml`'s self-audit step to also verify `/v1/binaries/...`
  and to check the across-run equivocation tripwire for the binaries STH (same
  `.sth-prev` cache mechanism, one more `check_tree "binaries"` call).
- **Independent monitors (the real defense).** Publish a tiny
  `pollis-verify monitor` mode (or document a cron one-liner) that fetches
  `/v1/binaries/sth/latest.json`, verifies signature + consistency against a
  locally-cached prior STH, and **shouts on divergence**. Encourage security
  researchers / the reproducible-builds community to run it. Because the tree is
  static and public, *anyone* can be a monitor with zero credentials — the whole
  point of the existing design (`transparency.md`: "public and unauthenticated by
  design").
- **Rebuilder bots.** The strongest monitor is one that *rebuilds from source at
  each tag and checks the payload hash it computes appears in the log.* This can
  be a scheduled GitHub Action in a **separate repo / separate trust domain**
  (ideally run by a third party) so a compromise of the release pipeline doesn't
  also silence the watcher. Ship a `scripts/rebuild-and-verify.sh` that does
  exactly this, runnable on any machine with the pinned release toolchain (§6).

**Security lens:** the log defends against equivocation only if the STH the app
sees is the *same* STH monitors see. Because the head is small, static, and
served from a CDN, cross-checking across mirrors + the CI tripwire + independent
monitors makes a targeted "show one user a different log" attack require forging
an Ed25519 signature or getting caught by any watcher — the identical argument
that already backs the account-key tree.

---

## 3. Provenance (SLSA + sigstore/cosign)

**Status: SHIPPED (P3, #484).** The `provenance` job in
`.github/workflows/desktop-release.yml` (`needs: [release]`, permissions
`id-token: write` + `attestations: write` + `contents: read`) does exactly what
this section specifies: `actions/attest-build-provenance` emits SLSA v1 in-toto
provenance for every released installer + updater bundle, keyless via Fulcio and
recorded in Rekor; `cosign sign-blob --yes` (cosign installed via the pinned
`sigstore/cosign-installer` action) signs each artifact keylessly. Both are
published to `cdn.pollis.com/releases/<tag>/` next to the artifact — the
attestation at exactly the path each `BinaryRecord` leaf records in
`provenance_uri` (`${PROVENANCE_BASE}/${artifact_name}.intoto.jsonl`, emitted by
`scripts/attest-binaries.sh`), the cosign `.sig` + `.pem` beside it. The verify
recipe is in `docs/verify-transparency-log.md` §6. It is an **additional,
optional** anchor: the minisign updater flow and OS code-signing are untouched
and it never gates install or auto-update.

Reproducibility answers "is this the honest source?"; provenance answers "was it
built where and how it claims?" — and provides a **keyless, publicly-anchored**
signature independent of Pollis's own signing keys, which matters precisely
because the threat model includes *compelled Pollis keys*.

- **SLSA provenance (in-toto).** Emit an in-toto `provenance` attestation per
  artifact recording: source repo + commit, the exact workflow + runner, the
  pinned toolchain (same fields as the `BinaryRecord.toolchain`), and the output
  digest. GitHub's `actions/attest-build-provenance` produces SLSA v1 provenance
  signed via **sigstore/Fulcio** (keyless, OIDC-identity-bound to the GitHub
  Actions workflow) with the signature recorded in the **Rekor** public
  transparency log. This gives a *second, independent* transparency anchor
  (Rekor) that Pollis does not control — defense in depth against a compromised
  Pollis STH key. (Implementation note: the release-wide attestation carries each
  artifact as an in-toto subject and is published at each artifact's recorded
  `provenance_uri`; a verifier resolves that URI and checks the specific
  artifact's digest against the attestation's subjects — so every leaf's
  `provenance_uri` resolves and verifies the byte-exact artifact it names.)
- **cosign on the raw artifacts.** `cosign sign-blob --yes` each installer +
  updater bundle keylessly; publish the `.sig` + `.pem` (or bundle) next to the
  artifact on `cdn.pollis.com`. `cosign verify-blob` lets anyone confirm the
  artifact was signed by the Pollis GitHub Actions identity, checked against
  Rekor, with **no Pollis-held key on the verification path.**

**Honest limits (P3).** This proves *build provenance* (these bytes came from
this workflow at this commit) and adds a *non-Pollis* transparency anchor
(Rekor). It does **not**, by itself, prove reproducibility — that the logged
payload rebuilds byte-for-byte from public source is the separate P5 story
(§1.5, `docs/reproducible-builds-residuals.md`). Provenance and reproducibility
are complementary: provenance says "the Pollis CI built this"; reproducibility
says "and public source produces the same bytes."

### 3.1 Composition with existing signing (this is the subtle part)

Pollis already has **three** signature layers; provenance is a fourth, and they
must not fight:

| Layer | Signs what | Key custody | Verifier | Defends |
|---|---|---|---|---|
| OS code-signing (Developer ID / Authenticode) | installer bytes | Apple / Azure HSM | Gatekeeper / SmartScreen at install | install-time integrity + OS trust UX |
| minisign updater `.sig` | updater bundle bytes | `TAURI_SIGNING_PRIVATE_KEY` | Tauri updater before applying | auto-update integrity |
| **binary transparency STH** | *hash* of payload+artifact | Pollis Ed25519 (`175ebfef…`) | `pollis-verify`, in-app | equivocation / targeted-build / "what source is this" |
| **SLSA/cosign (new)** | artifact digest + build facts | keyless (Fulcio/Rekor) | `cosign`, SLSA verifier | build-provenance + a *non-Pollis* anchor |

They **compose, don't conflict**, because each signs a different thing at a
different stage and is verified by a different party. Crucially:

- The **minisign updater flow is untouched** — the updater still fetches
  `update-*.json`, checks the minisign `.sig`, installs. Transparency + provenance
  are an *additional, optional* layer; they never gate the update path (that would
  add user-visible failure modes and violate zero-burden). A future enhancement
  (§4) can have the client *also* confirm the update's hash is in the binaries log
  before applying, but v1 keeps update integrity exactly as today.
- OS signing stays the *install-time* root of trust; transparency is the
  *what-did-I-actually-run* proof.

---

## 4. In-app verification surface (zero mandatory burden)

**Status: SHIPPED (P4).** `verify_own_build` in
`pollis-core/src/commands/transparency.rs` (with the pure, unit-tested
`derive_build_verify` verdict fn), the `verify_own_build` Tauri shim, the
`useVerifyOwnBuild` on-demand hook, `BuildVerifyLine`, and the "This build"
section on `SecurityPage` implement exactly the affordance below.

The design constraint is absolute: **the user does nothing.** Verification is
either fully automatic in the background, or a single optional click for the
curious/auditors. The vast majority of users benefit purely from the *existence*
of third-party monitors and rebuilders — they never see a prompt.

### 4.1 The affordance

On `frontend/src/pages/SecurityPage.tsx`, add a **"This build" section**
immediately below the existing "Account key" section — the exact same
advisory-line pattern already used by `AccountKeyAuditLine` +
`useSelfAuditAccountKey`. It surfaces:

- The running app's version + git commit (baked at build time, already available).
- A single status line rendered by a new `BuildVerifyLine` reusing the
  `AccountKeyAuditLine` component style (three states: verified / pending /
  mismatch — solid colors, **no neon/glow**, per repo UI rules).
- A **"Verify this build"** `Button` (`variant="secondary"`) that triggers the
  check on demand. No auto-run network call on page load beyond the cheap STH
  fetch already used for the account-key line, to respect the zero-burden and
  perf constraints.

### 4.2 What "Verify this build" proves (and how)

The running app knows its **own** binary. A new `pollis-core` command,
`verify_own_build`, does:

1. Compute the running binary's hash **and the leaf layer it is comparable
   against.** An early assumption here ("the app can just hash its own `.app`
   bundle payload, the same computation the release job logged") turned out to be
   false in practice, and shipped a false alarm.
   `scripts/attest-binaries.sh` logs, per shape: `sha_file` of the
   `.AppImage`/`.deb`/`.rpm`, but `sha_tree` — a `SOURCE_DATE_EPOCH`-pinned `tar`
   of an *extracted directory* — for the macOS `.app` (7z-extracted from the
   signed `.dmg` on a Linux runner) and the Windows NSIS tree. An installed
   process has neither the original installer nor that tar preimage, so no hash
   it can take will ever equal those leaves: the comparison missed 100% of the
   time everywhere except the AppImage, rendering the danger-styled "Build not in
   public log" on genuine, signed, notarized releases.

   The resolution is **§2.2's `exe` layer** — a third leaf per bundle holding the
   sha256 of the main executable as installed, which is both what a running
   process can hash and the precise claim this affordance makes ("these running
   bytes are bytes Pollis published"). It is bound to its `payload` leaf by the
   shared `payload_sha256`, so it can never float free of a published,
   reproducible unit. `compute_my_payload` returns a `MyPayload` enum pairing the
   hash with its target layer — `Payload(hash)` for the AppImage (whose shipped
   bytes ARE the payload, a strictly stronger whole-payload check), `Exe(hash)`
   everywhere else — so a cross-layer comparison is unrepresentable rather than
   merely discouraged. Releases predating the `exe` layer (≤ v1.5.2) carry no
   comparable leaf and report **Unavailable**, never Mismatch.
2. Fetch `/v1/binaries/entries.json` + the STH + the inclusion proof for this
   version's `payload` leaf from `verify.pollis.com`, and verify — **reusing the
   exact `verifiable-log` verification functions** already compiled into
   `pollis-core` for the account-key self-audit. Trust only the pinned key.
3. Report one of:
   - **Verified** — "This build's fingerprint is published in the public
     transparency log (release vX.Y.Z, commit abc1234). Independent auditors can
     rebuild it from source."
   - **Pending** — build not yet in the log (e.g. very fresh release before the
     tree republish). Advisory, not alarming.
   - **Mismatch** — the running payload hash is **not** in the log. This is the
     loud case: it means the running binary is not one Pollis publicly attested,
     which is exactly the targeted-backdoor signal. Surface prominently but
     without a modal (repo rule: no modals) — an inline danger-styled line + a
     link to `docs/verify-transparency-log.md`. Only reachable when the tag
     actually published a leaf of this install's layer (step 1) — plus the two
     platform-independent alarms, a tree that fails verification and a served key
     that isn't the pin.
   - **Unavailable** — "couldn't check". The host being unreachable, the local
     hash failing, *or* a tag with no leaf of the comparable layer (releases
     before `exe` shipped). Quiet, not alarming, and its copy points at the
     independent-verification guide.

> **Honesty caveat (must be in the UI copy, not buried):** the in-app check
> proves *inclusion in the log* and *hash match to the logged payload*. It does
> **not**, by itself, prove the logged payload reproduces from source — that step
> requires an *independent rebuilder* (a compromised app could lie about its own
> hash). So the in-app line is worded as "published in the public log; verified
> by independent rebuilders" and links out to the third-party verification story.
> The real proof is the ecosystem (monitors + rebuilders), and the app is one
> convenient, honest window onto it. This mirrors how the website explorer is
> framed as "convenience, not the trust anchor" in `verify-transparency-log.md`.

### 4.3 Why not gate launch on it

Gating launch/update on transparency verification would (a) add a hard network
dependency and a new failure mode to the most critical path, (b) burden every
user for a check that only auditors act on, and (c) be circumventable by the very
backdoored build it's meant to catch (it can skip its own check). The correct
architecture is: **OS signing + minisign gate the update (unchanged); the
transparency layer is verified out-of-band by many independent parties, and
optionally surfaced in-app.** Distributed watching, not a single self-check, is
what makes a targeted build detectable.

---

## 5. Threat model

### 5.1 What this defends

| Threat | How it's caught |
|---|---|
| **Malicious / compelled release** — operator (or an entity compelling them) ships a backdoored build to *everyone*, validly signed. | The backdoored payload hash is either (a) logged — and then *permanently, publicly attested* and reproducible-from-source, so a rebuilder proves the source doesn't match, or (b) not logged — and then the in-app check + any monitor flags a build absent from the log. Either way it's on the record; a compelled operator cannot both ship it and keep the log clean without forging the Ed25519 STH (detected by the tripwire + mirrors) or diverging from public source (detected by rebuilders). |
| **Targeted backdoored build for one user** — the strongest attack E2EE messengers face. | This is the flagship win. A per-user binary has a payload hash that is *not* the one published for that release. The victim's in-app "Verify this build" → **Mismatch**; and because the log is a single global tree, the operator cannot show the victim a "log" containing their special hash without equivocating (two different trees) — caught by any monitor/mirror cross-check. Reproducibility means the honest build's hash is independently derivable, so "just log the backdoor too" doesn't help the attacker: it would have to reproduce from public source, which it doesn't. |
| **Supply-chain tampering in transit** (CDN/R2 swap, MITM of a download). | Already partly covered by OS signing + minisign; transparency adds that the *only* payload whose hash is logged is the honest one, so a swapped artifact fails both the signature check *and* the log-inclusion check. |
| **Dependency / build-tool compromise** (a poisoned crate or a tampered CI step). | SLSA provenance (§3) records the exact source, toolchain, and runner; a build that didn't come from the pinned workflow/commit produces provenance that fails `cosign`/SLSA verification against the GitHub Actions identity, anchored in Rekor (a log Pollis doesn't control). |

**This is what actually *proves* the E2EE claim.** MLS proves the *math* — it
is the E2EE itself that makes the *server unable to read plaintext* (the relay
overlay is only optional, currently deferred IP-metadata defense-in-depth); but
only verifiable builds
prove **the client you're running is the one that implements that math and
doesn't exfiltrate keys.** Without it, "E2EE" rests on trusting an unverifiable
binary — the very thing Pollis's thesis says not to do.

### 5.2 What this does NOT defend

Honesty is the point. This feature does not defend against:

- **A user running a build they never verified and no one monitors.** The
  guarantee is *detectability by the ecosystem*, not *prevention on one isolated
  device*. If nobody watches the log and nobody rebuilds, a targeted attack can
  still land — the value is that it *cannot be done silently at scale or safely*,
  and that the incentive/plausible-deniability calculus flips against the
  attacker.
- **A compromised OS / device.** If the endpoint is owned, the attacker reads
  plaintext regardless of which binary Pollis shipped. Out of scope (device is
  "trusted" in §1.1 of the whitepaper).
- **The reproducibility gaps in §1.5** (native C++ deps, notarization). Until
  those are fully closed, the honest claim is "reproducible payload modulo a
  documented, hash-pinned set of vendored native blobs" — auditors are told
  exactly what they're *not* getting bit-for-bit.
- **Mobile.** This spec is desktop (Tauri) only. The `pollis-core` reuse via
  uniffi means the *log format* extends to mobile later, but iOS/Android store
  signing + reproducibility is a separate effort.
- **First-run trust bootstrap.** A user's *very first* download still trusts OS
  signing; transparency proves things about that binary *after* install, and
  protects every subsequent update. TOFU on the pinned STH key, same as the
  account-key tree.

---

## 6. Phased roadmap

Smallest valuable slice first. Each phase is independently shippable and useful;
each states its acceptance criteria and what's testable **in-box** (the box can
build `pollis-core` headlessly — `cargo test -p pollis --no-default-features
--features test-harness` — and run the `verifiable-log*` crates fully) vs. what
needs the **release runners** (macOS/Windows/Linux signing hardware).

### Phase 0 — Measure & document (measurement on CI; analysis in-box)
- Add `scripts/rebuild-and-verify.sh`: check out a tag, build the Linux target,
  extract the AppImage payload, hash it. Run twice; diff. **Measure** the
  non-determinism delta and attribute it (paths? timestamps? native deps?).
  The builds themselves run on release runners/CI — the Linux AppImage needs
  the Tauri shell with media ON (webkit2gtk, ALSA, dbus), which the box cannot
  build.
- Write the public "build recipe" (pinned toolchain + baked `option_env!` values
  from §1.3) as `docs/reproducible-build-recipe.md`.
- **Acceptance:** a documented, reproducible **Linux** payload (the easiest
  target) — two CI builds of the same commit produce byte-identical AppImage
  squashfs payloads, or a documented, itemized list of the exact remaining
  non-deterministic bytes. **Measurement runs on release runners/CI;** the
  in-box half is extracting/hashing/diffing the CI-built artifacts, plus all
  `verifiable-log*` tree/verifier work.

### Phase 1 — Binaries tree in the log (mostly in-box)
- New `binaries` tenant + `BinaryInvariant` in `verifiable-log-builder`; the
  `BinaryRecord` schema (§2.2); `--binaries-in` builder mode; `serve` emits
  `/v1/binaries/...` and `/verify/release/<tag>`; `pollis-verify release <tag>`.
- Unit/gate tests for the invariant (fork / re-issue / payload-pairing rejected),
  mirroring the commit-log builder's test suite.
- **Acceptance:** `pollis-verify release vX` verifies a fixture binaries tree
  end-to-end; injected fork/re-issue is rejected; tampered leaf fails. **Fully
  in-box** — pure `verifiable-log*` crate work, the box's strong suit.

### Phase 2 — Wire the release pipeline (needs release runners)
- `attest-and-log` job in `desktop-release.yml`: compute `payload_sha256` +
  `artifact_sha256` per artifact, emit records, insert into `released_binary_log`
  (migration), trigger `transparency-publish.yml`.
- Teach `transparency-publish.yml` to build/publish/self-audit/tripwire the
  binaries tree (one more `check_tree "binaries"`).
- **Acceptance:** cutting a real tag results in `/v1/binaries/...` populated and
  `pollis-verify release <tag>` green against `verify.pollis.com`; the tripwire
  covers the binaries STH. **Needs release runners** for the real signed
  artifacts; the *builder/serve/verify* half is in-box-testable with fixtures.

### Phase 3 — SLSA + cosign provenance (CI-only, low risk) — SHIPPED (#484)
- **Shipped:** the `provenance` job in `desktop-release.yml` (`needs: [release]`,
  `permissions: id-token: write` + `attestations: write` + `contents: read`) runs
  `actions/attest-build-provenance@v2` (SLSA v1 in-toto, keyless Fulcio/Rekor)
  over every released installer + updater bundle, and `cosign sign-blob --yes`
  (cosign from the pinned `sigstore/cosign-installer@v3`) on each. It publishes,
  next to the artifact on `cdn.pollis.com/releases/<tag>/`, the `.intoto.jsonl`
  attestation at exactly the `provenance_uri` each `BinaryRecord` leaf records
  (`scripts/attest-binaries.sh` → `${PROVENANCE_BASE}/${artifact_name}.intoto.jsonl`)
  plus the cosign `.sig` + `.pem`. It never gates install/auto-update — the
  minisign updater flow and OS code-signing are untouched.
- **Acceptance:** `cosign verify-blob` + a SLSA verifier pass against a released
  artifact using only the GitHub Actions OIDC identity + Rekor (no Pollis key) —
  the exact invocations are documented in `docs/verify-transparency-log.md` §6.
  **CI-only / exercisable only on a real signed release runner** (needs the
  runner's OIDC token; cosign/attestation cannot be run end-to-end off-CI). In
  box: the workflow wiring (permissions, pinned actions, path consistency) and
  YAML validity are verified; the keyless signing + Rekor upload run only on a
  release.

### Phase 4 — In-app "Verify this build" (in-box for logic, runner for real hashes) — SHIPPED
- `verify_own_build` in `pollis-core` (reuses the account-key verify path, running
  the SAME `verifiable_log_serve::release::verify_release` on the blocking pool and
  pinning the served binaries key); the pure `derive_build_verify` verdict fn;
  `BuildVerifyLine` + "This build" section on `SecurityPage`; the on-demand
  `useVerifyOwnBuild` hook (a mutation — never auto-runs on mount). The running
  commit is baked by `pollis-core/build.rs` (omitted gracefully if absent).
- **Acceptance (met):** the pure `derive_build_verify` unit tests cover
  verified/pending/mismatch/tree-invalid with fixture `ReleaseReport`s (no network,
  no DB) — a deliberately-wrong local hash yields **Mismatch**; the Security page
  renders the states via `BuildVerifyLine`. End-to-end against a *real* release
  still needs a signed build (the per-platform payload hashing is best-effort:
  exact for the Linux AppImage, deferred for `.app`/NSIS/deb/rpm — §4.2).

### Phase 5 — Full reproducibility hardening + independent rebuilder (mixed) — **shipped for Linux (#484)**
- **Shipped:** exact toolchain pin (`rust-toolchain.toml` 1.96.0 +
  `dtolnay/rust-toolchain@1.96.0` across every release job), `--remap-path-prefix`
  for `$HOME`/cargo-home/workspace on the Linux build, `SOURCE_DATE_EPOCH` from
  the tag commit on the Linux compile/bundle steps, `pnpm install
  --frozen-lockfile` + `cargo build --locked` on the desktop build, Vite bundle
  determinism (no source-map host paths, content-hashed assets), the shared
  payload-hashing helper (`scripts/lib/payload-hash.sh`) sourced by both the
  attest job and the reproducer, the published itemized residual list
  (`docs/reproducible-builds-residuals.md`), and the third-party-runnable
  reproducer (`.github/workflows/rebuild-verify.yml`, separate trust domain, no
  Pollis secrets, trusts only the pinned key).
- **Still best-effort (in the residual list, not yet closed):** vendoring the
  capture-helper bindgen output; pinning native `meson`/`clang` and runner
  images by *digest* (labels only today); and — the top item — stripping the
  still-baked *secret* `option_env!` inputs so a fully secretless third party can
  bit-reproduce the Linux payload (today only a party given the published recipe
  can). macOS/Windows payload reproduction is best-effort pending a
  matching-runner reproducer.
- **Acceptance:** an *independent* party reproduces the Linux payload bit-for-bit
  (or modulo the documented list) and confirms its hash is logged; the reproduced
  set is published. Reproducibility of **macOS/Windows payloads** is
  best-effort with a documented residual list. **The Linux rebuild runs on
  CI/release runners (the AppImage needs the full media-ON Tauri shell); the
  in-box part is hashing/diffing the resulting artifacts. macOS/Windows need
  matching runners.**

---

## 7. Dependencies / synergy with the other planned specs

- **Reuses, doesn't rebuild.** Every piece leans on shipped `#330` infra:
  `verifiable-log` (core Merkle/STH/proof, `TenantInvariant` hook),
  `verifiable-log-builder` (DB→signed bundle pattern), `verifiable-log-serve`
  (`serve` + `pollis-verify`), the daily `transparency-publish.yml`
  (build/sync/self-audit/tripwire), the pinned key in `verifier-release.yml`, and
  the `SecurityPage` account-key advisory-line pattern. The binary tree is a
  *third tenant*, not a new system — the strongest argument for doing it.
- **Machine-checked correctness spec (synergy).** Reproducible builds make a
  formally/property-verified `pollis-core` *meaningful*: proving the source
  correct only matters if you can prove the shipped binary *is* that source. The
  two specs are complementary halves of "trust the code, verify the binary."
  Any machine-checked-correctness gate that runs in CI should also feed the
  provenance (record "these proofs passed at this commit") so the attestation
  carries correctness evidence, not just build facts.
- **Metadata-minimization spec (synergy + a shared discipline).** The log leaf
  deliberately carries **no user data** — hashes + build facts only, the same
  "hash and drop the blob" discipline the commit-log builder uses. Binary
  transparency is metadata-*generating* (release facts are public by design), so
  it doesn't conflict with minimization; and it *strengthens* the minimization
  story by letting users verify the binary actually implements the minimizing
  behaviour the spec claims.
- **Relay-overlay spec (the sharp contrast).** `docs/relay-overlay-design.md`
  is optional IP-metadata defense-in-depth (currently deferred): the relay
  forwards already-encrypted bytes and hides who-talks-to-whom, nothing more.
  It is MLS/E2EE that keeps the *server* from reading plaintext; this spec
  proves the *client* is honest. Stated plainly in the whitepaper edit: the
  relay overlay does **not** prove E2EE (it was never part of that proof, and a
  backdoored client defeats E2EE regardless); verifiable builds are what
  actually prove it. The two together are the full "don't trust the operator,
  verify" claim end to end.

---

## 8. Recommendation

**Build it, in the phased order above, and start with Phase 0 + Phase 1 — both
low-risk (Phase 1 fully in-box; Phase 0's builds run on CI, with the
hash/diff analysis in-box), and together they produce the first real artifact:
a public binaries transparency tree that `pollis-verify` can check.** That alone
lets the whitepaper §1.1 admission be rewritten from "reproducible builds are not
a goal" to "released binaries are content-hashed into a public, append-only
transparency log; reproducibility of the Linux payload is verified, with a
documented residual list for signed/native components." The high-leverage,
lower-cost half (log + provenance) lands well before the expensive tail (full
bit-for-bit macOS/Windows reproducibility), and each phase is independently
valuable.

The single most important framing to preserve: **this is the feature that turns
"E2EE" from a promise into a proof.** It is worth prioritising above further
protocol polish, because every other security property is conditional on the
binary being honest — and today nothing proves that.

Do **not** over-scope v1 into full cross-platform bit-reproducibility; that is the
part most likely to stall. Ship the log + provenance + in-app surface first
(Phases 1–4), then grind reproducibility (Phase 5) with an honest residual list.

---

## 9. GitHub-issue-ready summary

**Title:** Verifiable builds + binary transparency — prove the shipped client is the honest source

**Problem.** `docs/security-whitepaper.md` §1.1 admits reproducible builds are not
a goal and binary integrity rests solely on OS code-signing. Code-signing proves
"Pollis produced these bytes," never "these bytes match the public source." A
compromised, compelled, or *per-user-targeted* build is validly signed and passes
Gatekeeper/Authenticode and the minisign updater check — defeating E2EE entirely
while looking legitimate. Every other Pollis security property (MLS, PIN-wrapped
keys, the key-transparency log, the relay overlay) is conditional on the running
binary being honest, and today nothing proves that. This is the flagship trust
gap, and it's closable by extending the *existing* `#330` key-transparency Merkle
infrastructure (`verifiable-log*`, `pollis-verify`, `verify.pollis.com`) from keys
to binaries — a third tenant tree, a provenance sidecar, and one advisory line on
the Security page, with **zero added user burden**.

**Phased milestones.**
- **P0 (CI builds, in-box analysis):** measure Linux payload reproducibility on release runners; publish a pinned build recipe.
- **P1 (in-box):** `binaries` tenant + `BinaryRecord` schema + `BinaryInvariant` in `verifiable-log-builder`; `serve` emits `/v1/binaries/...` + `/verify/release/<tag>`; `pollis-verify release <tag>`.
- **P2 (runners):** `attest-and-log` job in `desktop-release.yml` logs payload+artifact hashes; `transparency-publish.yml` publishes/self-audits/tripwires the binaries tree.
- **P3 (CI):** SLSA provenance via `actions/attest-build-provenance` + keyless `cosign` (Fulcio/Rekor) — a build-provenance anchor Pollis doesn't control.
- **P4 (in-box logic):** optional in-app "Verify this build" on the Security page (`verify_own_build`, reusing the account-key verify path) — automatic-or-one-click, never mandatory.
- **P5 (mixed):** full reproducibility hardening (path remapping, `SOURCE_DATE_EPOCH`, pinned native toolchains, vendored bindgen) + an independent third-party rebuilder; publish an honest residual-nondeterminism list.

**Acceptance (top-line).** A released tag `vX.Y.Z` results in `/v1/binaries/...`
on `verify.pollis.com` with per-artifact `payload`/`signed` leaves;
`pollis-verify release vX.Y.Z` verifies inclusion + invariants trusting only the
pinned Ed25519 key; the CI tripwire covers the binaries STH; an independent
rebuilder reproduces the Linux payload (bit-for-bit or modulo a documented list)
and confirms its hash is logged; the app's optional "Verify this build" returns
**verified** for a genuine build and **mismatch** for a hash absent from the log;
`cosign verify-blob` passes against the GitHub Actions identity via Rekor with no
Pollis-held key. Whitepaper §1.1 is updated to reflect the new guarantee and its
honest limits.
