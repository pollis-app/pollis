# Reproducible-builds residual list

> The honest accounting of what in a Pollis release does **not** reproduce
> bit-for-bit, and why. Companion to `docs/verifiable-builds-design.md` (design)
> and `.github/workflows/rebuild-verify.yml` (the independent reproducer). If you
> are auditing Pollis, read this first: it is the list of things you should
> **not** expect to reproduce, so a divergence in one of them is expected, not
> evidence of tampering.

## The one reproducible unit, stated plainly

**The Linux AppImage payload is the reproducible unit.** It is the deterministic
output of `cargo build --locked` + `vite build` + Tauri's AppImage bundling, with
the toolchain pinned (`rust-toolchain.toml` → `1.96.0`, `dtolnay/rust-toolchain@1.96.0`
in CI), absolute build paths remapped (`--remap-path-prefix` for `$HOME`, the
cargo home, and the workspace root), `SOURCE_DATE_EPOCH` set to the tag commit's
unix seconds, and JS inputs frozen (`pnpm install --frozen-lockfile`). Its
`payload_sha256` is what the release pipeline logs into the append-only
transparency tree and what `rebuild-verify.yml` independently recomputes and
asserts is present in that tree.

Everything below is **outside** that unit. Each item is either non-reproducible
by construction, cross-platform (not reproducible on a Linux runner at all), or a
tracked gap. Where an item blocks even the Linux payload, it is flagged
**[blocks Linux payload]**.

The signed wrapper is **not** reproducible; it is transparency-logged and
cryptographically **bound** to the payload (a `layer:"signed"` leaf sharing the
payload's `payload_sha256`), so its integrity is provable *via* the payload even
though its bytes are not reproducible.

---

## Residuals

### 1. Code-signing / notarization outer layer — non-reproducible by construction
A notarized macOS `.dmg` embeds an Apple timestamp + CMS signature; an
Authenticode `.exe` embeds a mandatory RFC-3161 timestamp from
`timestamp.acs.microsoft.com`; the Tauri updater `.sig` is a minisign signature
over the artifact. **None can be reproduced** without Pollis's private keys, and
the timestamps differ on every run by design.

**What we log today (corrected):** on Linux the `payload` leaf genuinely is the
pre-signature reproducible unit. On **macOS and Windows it is not** — the attest
job extracts the `.app` out of the already-signed, notarized `.dmg` (and the file
tree out of the signed NSIS installer) and hashes that, so those `payload` leaves
contain Apple's CMS signature, the stapled notarization ticket, and Authenticode
data. **They therefore cannot be reproduced by anyone, including us on a matching
runner** — not "hard", impossible. Capturing a genuine pre-signature payload on
those platforms is tracked in #603. This paragraph previously claimed the
pre-signature behaviour as already implemented; it was describing the intent, not
the pipeline.

The signed wrapper is logged as a separate derived hash bound to the payload. Install-time integrity of the signed wrapper still
rests on platform code-signing (Gatekeeper / Authenticode), exactly as before —
transparency is *added to*, not a *replacement for*, signing.

### 2. macOS `.app` and Windows NSIS native payload bits — best-effort, cross-platform
macOS builds on `macos-latest` (ARM), Windows on `windows-latest`. You cannot
bit-reproduce a macOS or Windows payload on a Linux runner, and we do **not**
promise cross-OS reproducibility. Their payloads (the `.app` contents; the
unsigned exe + resources inside the NSIS installer) are extracted and logged with
the correct two-leaf `payload`/`signed` structure, and reproduction is
**best-effort on a matching runner** — not asserted by `rebuild-verify.yml`,
which reproduces the Linux payload only. Native codegen on these platforms
(MSVC/clang, resource compilers) can embed host paths and vary across patch
versions; treat macOS/Windows payload reproduction as **impossible with the
current leaf definition** (see §1) rather than merely unverified. Two things must
land before it becomes a question of determinism at all: a pre-signature payload
hash (#603) and a matching-platform reproducer.

An empirical note for whoever picks up #603: `codesign --remove-signature` is
**deterministic** — stripping the shipped v1.5.3 `Contents/MacOS/pollis` twice
yields the identical digest (`acd70269be7b…`). That makes normalization viable as
a cross-check between two observers of the same signed artifact. It does **not**
establish that stripped bytes equal a freshly built *unsigned* binary, and that
equality — untested — is the load-bearing assumption for any reproducibility
claim built on stripping rather than on capturing the payload before signing.

### 3. Apple notarization staple — non-reproducible, post-build mutation
Apple's notary service staples a ticket **after** the build, changing the shipped
bytes. We log the pre-staple payload hash and treat the staple as part of the
non-reproducible signed-wrapper layer.

### 4. Baked `option_env!` build recipe — **[blocks Linux payload]** for a secretless third party, via the optional log token only
The client bakes build-configuration values into the binary at compile time via
`option_env!` (`pollis-core/src/config.rs`): `TURSO_URL`, the **read-only**
`TURSO_TOKEN`, `LOG_DB_URL`, `LOG_DB_TOKEN`, `R2_S3_ENDPOINT`, `R2_PUBLIC_URL`,
`LIVEKIT_URL`, `POLLIS_DELIVERY_URL`, and `POLLIS_SEAL_SENDER`. These bytes are
part of the reproducible payload, so a reproducer must bake the **identical**
values or its hash legitimately diverges.

- Most are **non-secret by design** (public URLs, a boolean flag, the RO token)
  and can be published as a build recipe; `rebuild-verify.yml` reads them from
  non-secret repository/environment `vars`.
- **As of #506 (secrets-broker cutover, finishing #393) the client bakes no R2 or
  LiveKit credentials at all.** `R2_ACCESS_KEY_ID`, `R2_SECRET_KEY`, `R2_REGION`,
  `LIVEKIT_API_KEY`, and `LIVEKIT_API_SECRET` are no longer read anywhere in the
  client — media upload (R2) and LiveKit token minting now go through the Delivery
  Service / broker, not the shipped binary — so they are **no longer compiled in**.
  (The desktop-release workflow still exports `R2_SECRET_KEY` and
  `LIVEKIT_API_SECRET` into the build env, but the client no longer reads them, so
  they are dead build-env vars; that workflow cleanup is out of scope here.) This
  closes what was the single largest secretless-reproduction gap.
- The only baked **credentials** that remain are the publishable read-only
  `TURSO_TOKEN` (it reads already-public metadata and encrypted envelopes, never
  plaintext or keys, so it can be published in the recipe and is **not** a
  secretless-repro blocker) and the **optional** observability `LOG_DB_TOKEN` (a
  bearer token to the log DB, consumed at `pollis-core/src/state.rs`; when a
  release bakes it — `desktop-release.yml` still does — a fully-independent,
  zero-secret party **cannot bit-reproduce the Linux payload**, only a party
  holding the recipe can). That optional log token is now the remaining
  secretless-repro blocker, and a much smaller surface than the media secrets it
  replaces at the top of this list. It does **not** weaken log-inclusion
  verification, which needs no build inputs at all.
- Tracked future work: scope/cut over `LOG_DB_TOKEN` so no secret-shaped input is
  baked into the client binary, and publish the remaining recipe as a stable
  per-release manifest (§1.3 of the design doc).

### 5. `bindgen` host-header layout (Linux capture helper) — best-effort
`pollis-capture-linux` links `libspa-sys`, whose `bindgen` step generates Rust
structs from the **build host's system PipeWire headers** (hence the release
builds it on `ubuntu-24.04` for PipeWire 1.0). bindgen output embeds host header
layout and can vary with the exact system-header version, so the helper — and
therefore the top-level AppImage bundle that embeds it — is only reproducible to
the extent the base image's headers are pinned. `rebuild-verify.yml` mirrors the
release exactly (same `ubuntu-24.04` helper job, staged the same way) to minimize
this, but the runner image is pinned only by **label** (see item 6), not digest,
so this remains a best-effort residual. Preferred future fix: vendor a pinned
`bindings.rs` and gate regeneration behind a feature so release builds never
invoke bindgen.

### 6. Runner image pinned by label, not digest — best-effort
Jobs pin `ubuntu-22.04` / `ubuntu-24.04` / `macos-latest` / `windows-latest` by
**label**. GitHub periodically re-images these labels, so the underlying system
libraries and toolchains behind a label can shift between builds. True
reproducibility wants a **digest-pinned** image; GitHub-hosted runners do not
expose a stable content digest for their images, so we pin by label and record
the label in each leaf's `toolchain.runner_image`. Reproducing across a runner
re-image is best-effort until Pollis moves the release to a digest-pinned
container or self-hosted image.

### 7. Native C/C++ dependencies (`webrtc-sys` / `libwebrtc`, `webrtc-audio-processing-sys`) — best-effort
These vendored C/C++ builds (clang, meson, ninja, VAAPI wrappers) are the
least-controlled inputs: native compilers embed paths (mitigated by
`--remap-path-prefix`) and can vary across patch versions. `meson>=1.3` is a
**floor, not a pin**. These are the most likely single source of a
non-reproducing byte in the Linux payload; the honest position is
"reproducible **modulo** this audited set of vendored native builds," and the
first milestone is to *measure* their contribution on real runners before
promising more.

---

## What this means for a verifier

1. **Log inclusion always verifies, for everyone.** `pollis-verify release <tag>`
   (and `rebuild-verify.yml`'s inclusion step) needs no build inputs and no
   secrets — it proves every published artifact for a tag is in the append-only
   log, under the pinned key, with no fork or re-issue. Do this first; it is the
   universally-available guarantee.
2. **Bit-for-bit Linux reproduction verifies for a party holding the build
   recipe** (item 4), on a runner matching the release image (item 6). Run
   `rebuild-verify.yml` from a fork with the non-secret recipe `vars` set.
3. **macOS/Windows payload reproduction is best-effort** on matching runners and
   is not asserted by the shipped reproducer (item 2).
4. **A divergence in an item above is expected, not tampering.** A divergence in
   the Linux payload with the full recipe supplied on a matching image, however,
   is exactly the signal this program exists to surface — investigate it.
