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

**What we log today:** on all three platforms the `payload` leaf commits to bytes
that exclude the unreproducible signing layer — but it is reached three different
ways, and the differences matter.

**Linux.** The shipped AppImage/deb/rpm bytes already *are* the payload (Tauri's
minisign signature is detached), so the attest job hashes them directly.

**macOS (since #750).** The build job runs **once**, producing the signed, notarized
bundle we actually ship, and the payload digest is computed **from that bundle** by
`sha_macos_payload` — which normalizes Apple's per-signing material back out (the
stapled ticket, each Mach-O's embedded `LC_CODE_SIGNATURE`, the `_CodeSignature`
manifests) and then hashes the remainder with `sha_tree`. This is the ordinary
answer across the reproducible-builds world: you do not reproduce a signature, you
exclude it. F-Droid requires APKs be identical "apart from the signature"; the
Mach-O convention is to normalize the code-signature region away before comparing.
The strip is *verified* rather than assumed — if a signature or ticket survives,
the function fails and the release stops, because a digest over still-signed bytes
is one nobody could ever recompute.

Two properties follow, and they are the point of the change. First, the digest is
**recomputable by a stranger**: mount the public `.dmg`, copy out the `.app`, run
the same normalization, compare to the logged leaf. Second, the release no longer
depends on the Rust toolchain emitting byte-identical output across two compiles of
one source tree — which it does **not** reliably do (see §2).

**Windows (also since #750).** The same scheme, against a different signing
format. The build job runs **once**, producing the Authenticode-signed NSIS
installer we ship, and `sha_windows_payload` extracts its file tree, normalizes
the signature out of every PE inside (`scripts/lib/strip-pe-signature.py`), and
hashes the remainder with `sha_tree`.

Windows needed its own stripper because it has no equivalent of
`codesign --remove-signature`: `signtool remove /s` deletes the certificate table
but leaves behind the PE checksum signtool recomputed, so a stripped file still
differs from the unsigned one. Both fields are normalized — the certificate table
(located via data directory entry 4, whose "VirtualAddress" is a file offset for
that entry alone) is truncated away and its directory entry zeroed, and the
optional header's `CheckSum` is zeroed.

One honest limit: `signtool` pads the image to an 8-byte boundary before appending
the certificate table, so up to 7 bytes of alignment padding can survive
truncation, and the normalized form is then not byte-identical to the never-signed
build. That is deliberate. The property the leaf needs is that the digest is
**deterministic and independently recomputable from the shipped artifact** — every
signing of a given build normalizes to the same bytes — not that it equals a build
no outsider ever had. Chasing byte-identity by stripping trailing zeros would
corrupt any binary that legitimately ends in them.
`scripts/test-attest-helpers.sh` encodes both the guarantee and its limit.

Windows kept the two-build scheme for one release cycle after macOS left it, on the
grounds that it had not hit the non-determinism macOS did. That turned out to be
false: it failed the v1.8.3 release twice in a row, with a **different** hash each
time.

On **every** platform the digest is published as a `*.payload-sha256` release-asset
sidecar that `scripts/attest-binaries.sh` reads back as `payload_sha256`. The attest
job never hashes a signed artifact for the payload leaf (it opens the signed
artifact only to reach the installed executable for the `exe` leaf).

**History.** Before #704 the macOS/Windows `payload` leaves hashed the already-signed
bytes — embedding Apple's CMS signature, the stapled ticket, and Authenticode data —
and so **could never be reproduced by anyone, including us on a byte-identical
runner** (impossible by construction, not merely hard). #704 fixed that with the
two-build capture. #750 then found that the two-build scheme had bought correctness
at the price of a release gate that depended on compiler determinism, and replaced
it on macOS with normalization of the shipped bundle.

**The payload leaf wraps the *shipped* binary by construction.** Under the
two-build scheme this needed enforcing: the leaf hashed a capture build, so it was
meaningful only if the signed build reused the same compiled binary, and
`scripts/verify-presig-binary.sh` fingerprinted the binary after each build to
prove it. That check — and the script — are gone, because there is no longer a
second build to reconcile. Both signed platforms now derive the digest from the
artifact they ship, so a leaf describing a binary that never shipped is not
something CI has to catch; it is not expressible.

**What is still not reproducible on macOS/Windows** is the *degree* to which those
pre-signature payloads reproduce bit-for-bit — see §2. #704 changed *what we hash*
(pre-signature, not signed); it did **not** add `--remap-path-prefix`, digest-pinned
runners, or a matching-platform reproducer, so macOS/Windows payload reproduction
stays **best-effort** (native codegen can still embed host paths). The distinction
is the whole point: "best-effort, closable" replaces "impossible, forever."

**That the shipped artifact is actually signed is also CI-enforced, not assumed.**
Removing the hardcoded macOS `signingIdentity` (above) deleted the config fallback,
so the whole macOS signing configuration now lives in one secret,
`APPLE_SIGNING_IDENTITY`. If it were ever empty, tauri would resolve no identity and
*silently* skip codesigning **and** notarization while the job stayed green —
shipping a Gatekeeper-blocked `.dmg` wrapped in a valid-looking payload leaf. Two
checks close that (`scripts/verify-signed-artifact.sh`, both platforms): a
**preflight** that hard-fails before the signed build if a required signing secret
is empty (GitHub sets an unset secret's env var to `""`, so this catches the
`Some("")` case the bundler's own filter would otherwise decide), and a **post-build
verification** — macOS `codesign --verify --deep --strict` + a Developer ID
Application authority + `spctl` Gatekeeper/notarization acceptance; Windows `signtool
verify /pa` — that refuses to publish an unsigned/unnotarized artifact. Since #750
that post-build verification runs *before* the payload digest is computed on both
signed platforms, so the digest is only ever taken over a tree already proven to be
the signed one. (No identity string, team id, or cert thumbprint is written into
the repo — the authority is matched by prefix only.)

The signed wrapper is logged as a separate derived `layer:"signed"` leaf whose
`payload_sha256` equals the payload leaf's — an explicit, verifiable binding
("this signed artifact wraps that reproducible payload"), never a reproducibility
claim about the signed bytes themselves. Install-time integrity of the signed
wrapper still rests on platform code-signing (Gatekeeper / Authenticode), exactly
as before — transparency is *added to*, not a *replacement for*, signing.

### 2. macOS `.app` and Windows NSIS native payload bits — best-effort, cross-platform
macOS builds on `macos-latest` (ARM), Windows on `windows-latest`. You cannot
bit-reproduce a macOS or Windows payload on a Linux runner, and we do **not**
promise cross-OS reproducibility. Neither is asserted by `rebuild-verify.yml`,
which reproduces the Linux payload only. Native codegen on these platforms
(MSVC/clang, resource compilers) can embed host paths and vary across patch
versions, and these jobs do **not** yet apply `--remap-path-prefix` (macOS/Windows
carry per-target `rustflags` in `.cargo/config.toml`, so a blanket `RUSTFLAGS`
would clobber them — deferred, tracked as its own step). So treat rebuild-from-
source on macOS/Windows as **best-effort and unverified**. The remaining
prerequisite is a **matching-platform reproducer** (the macOS/Windows analogue of
`rebuild-verify.yml`).

**What #750 established about macOS determinism.** Rebuilding the same commit on
`macos-latest` does **not** reliably produce identical bytes. Measured across four
compiler profiles, one build pair each: fat LTO + `opt-level=s` (the shipped
settings) **diverged**; no LTO + `opt-level=s` matched; no LTO + `opt-level=3`
**diverged**; thin LTO + `opt-level=3` matched. An earlier run with the shipped
settings produced two identical builds and a third that differed. So the failure is
**probabilistic**, and it is **not** attributable to LTO alone — turning LTO off
still diverged. This is consistent with known rustc/LLVM non-determinism
(rust-lang [#52044](https://github.com/rust-lang/rust/issues/52044),
[#50556](https://github.com/rust-lang/rust/issues/50556),
[#129080](https://github.com/rust-lang/rust/issues/129080)). No profile can be
called safe on this evidence — each row is a single coin flip.

That is why the macOS payload leaf no longer depends on compiling twice.

**Why normalization is sound here, given #704's objection.** #704 rejected
after-the-fact signature stripping on the grounds that "stripped bytes equal a
freshly built *unsigned* binary" was an untested assumption. That objection is
answered by **changing what the leaf claims**, not by assuming the two are equal.
The macOS `payload_sha256` is *defined* as the digest of the **shipped bundle with
Apple's per-signing material normalized out** — not as the digest of some unsigned
build. Both parties compute that same defined function: we compute it in the build
job, and a verifier computes it from the public `.dmg`. Their agreement does not
require it to coincide with an unsigned rebuild.

The empirical support was already recorded here and still holds:
`codesign --remove-signature` is **deterministic** — stripping the shipped v1.5.3
`Contents/MacOS/pollis` twice yields the identical digest (`acd70269be7b…`).
`scripts/test-attest-helpers.sh` additionally pins the behavioural contract against
stubbed Apple tooling: a signed bundle must normalize to the same digest as the
equivalent unsigned one, two independent signings of one build must agree, the
shipped bundle must be left untouched, and a strip that silently fails must abort
rather than publish an unrecomputable digest.

**What this does and does not buy.** It makes the macOS leaf *falsifiable by an
outsider* — anyone with the public `.dmg` can recompute it — which the #704 scheme
never did, since it hashed a bundle that existed only inside a CI job. It does
**not** make macOS reproducible from source; that still awaits a matching-platform
reproducer and the determinism work above.

### 3. Apple notarization staple — non-reproducible, post-build mutation
Apple's notary service staples a ticket **after** the build, changing the shipped
bytes. We log the pre-staple payload hash and treat the staple as part of the
non-reproducible signed-wrapper layer.

### 4. Baked `option_env!` build recipe — **[blocks Linux payload]** for a secretless third party, via the optional log token only
The client bakes build-configuration values into the binary at compile time via
`option_env!` (`pollis-core/src/config.rs`): `TURSO_URL`, the **read-only**
`TURSO_TOKEN`, `LOG_DB_URL`, `LOG_DB_TOKEN`, `R2_S3_ENDPOINT`, `R2_PUBLIC_URL`,
`LIVEKIT_URL`, and `POLLIS_DELIVERY_URL`. (`POLLIS_SEAL_SENDER` is gone as of
#607 — sealed sender is unconditional, so there is no longer a flag to bake.)
These bytes are part of the reproducible payload, so a reproducer must bake the
**identical** values or its hash legitimately diverges.

- Most are **non-secret by design** (public URLs, the RO token)
  and can be published as a build recipe; `rebuild-verify.yml` reads them from
  non-secret repository/environment `vars`.
- **The recipe is now CI-enforced, because it silently drifted (#697 sweep).**
  `POLLIS_OVERLAY_DIRECTORY_URL` and `POLLIS_OVERLAY_DIRECTORY_KEY` were added to the
  Linux release build and never to the rebuilder's recipe, so from the moment the
  relay directory went live no independent rebuild could match — and the only symptom
  is a hash mismatch, which reads like tampering rather than like a missing input.
  Both of `rebuild-verify.yml`'s runs failed on exactly this while these documents
  described independent Linux reproduction as a working, shipped property.
  `scripts/check-build-recipe.py` now fails CI whenever the three hand-maintained
  lists disagree — `option_env!` in `config.rs`, the release job's exports, and the
  rebuilder's recipe — so the gap cannot silently reopen. Both directory values are
  public by construction (the key is the *verification* half; the private half never
  leaves the minting script), so publishing them in the recipe costs nothing.
- **Status: DEMONSTRATED, as of v1.8.4.** `rebuild-verify.yml` rebuilt the Linux
  AppImage payload from public source at `v1.8.4` and the result matched the payload
  hash in the transparency log, verified against the pinned ML-DSA-44 key:
  `1a4213a162521a3bf9b3d60f34060222c64a47a083a47700e34efa0b6ead26d5` (run
  `31361471279`). This is the first green run; before it the claim had never been
  true, and three documents asserted it anyway.

  **What it took, because each cause hid the next.** (1) The recipe was missing
  `POLLIS_OVERLAY_DIRECTORY_URL`/`_KEY`. (2) The recipe `vars` did not exist at all,
  so every value resolved empty. (3) The release built the capture helper — embedded
  in the AppImage — with no determinism inputs, while the rebuilder applied all
  three. (4) The release built from a warm `Swatinem/rust-cache`, so its output was
  not a pure function of its source. (5) `LOG_DB_TOKEN` was baked by the release and
  withheld from the recipe.

  Ruled out with evidence rather than assumption, and recorded so nobody re-runs the
  search: runner image (identical, `20260720.234.2`), apt package set (identical, 30
  packages), meson version, the pinned toolchain, RUSTFLAGS, `SOURCE_DATE_EPOCH`, the
  tauri config override, the baked git commit, the embedded frontend (vite content
  hashes match), and compiler nondeterminism — two independent rebuilds produced
  byte-identical output, which is what proved the divergence systematic.

  A missing baked value presents as ~64 bytes of `.text` and a shifted `.rodata`, not
  as an obviously absent string: one absent constant changes layout throughout. The
  actionable signal was `strings` set-difference between the shipped and rebuilt
  binaries, which named `LOG_DB_TOKEN` directly.

- **`LOG_DB_TOKEN` is published in the recipe, and that costs nothing.** It is a
  read-only bearer token to the commit-log DB, and it is compiled into every shipped
  binary — it was recovered from the public `v1.8.4` AppImage with `strings` while
  diagnosing the above. Anyone who downloads Pollis already holds it, so publishing it
  as a repository variable adds no exposure that shipping the binary did not. Scoping
  it out of the client remains the better end state (see below); until then, publishing
  it is what makes the reproduction claim true for everyone rather than for us alone.
- **C/C++ build paths are remapped too.** `--remap-path-prefix` is a *rustc* flag and
  does not reach C/C++ compiled by build scripts through `cc-rs`, so v1.8.4 shipped
  with `/home/runner/.cargo/.../cxx-1.0.194/src/cxx.cc` embedded. Both the release and
  the reproducer ran at `/home/runner`, so the CI-to-CI result held — but a third
  party building anywhere else would embed *their* path and fail, which made
  "reproducible by anyone" really mean "by anyone building at the same path".
  `-ffile-prefix-map` is now applied to `CFLAGS`/`CXXFLAGS` in all four reproducible
  jobs; `scripts/check-build-recipe.py` fails CI if a job remaps Rust paths without
  remapping C/C++ ones; and the release **asserts** the finished binary embeds no
  absolute build path, and that the remapped prefixes ARE present, so the check cannot
  pass merely because something failed to compile.
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
expose a stable content digest for their images.

Since #877 the leaf no longer records the *label*. It records
`$ImageOS@$ImageVersion` — e.g. `ubuntu22@20250804.1.0` — which names one
immutable, dated `actions/runner-images` build whose package manifest is
published, rather than a moving pointer that says nothing about which image ran.
That converts this residual from "we recorded something meaningless" into "we
recorded exactly which image, and you still cannot pin a digest". The Linux leaf
records both images (`…+helper:ubuntu24@…`), since the AppImage embeds a helper
compiled on the other one.

Reproducing across a runner re-image remains best-effort until Pollis moves the
release to a digest-pinned container or self-hosted image — but a rebuilder can
now at least *tell* whether the image moved, which was impossible while the field
held a floating label.

**This residual is the one that actually bites, and #944 measured it.** The
AppImage is not purely a function of Pollis source: `linuxdeploy` copies the
app's transitively-linked system shared libraries **off the build runner** into
`usr/lib/` inside the bundle. Those bytes are the runner image's, not ours. When
the image moves, they move, and the payload hash moves with them for a completely
unchanged source tree.

`v1.9.3` is the worked example, and it was published as an unexplained "did not
reproduce" for three days before anyone looked:

| | |
|---|---|
| Release `build-linux` ran on | `ubuntu22@20260810.260.1` |
| Rebuild ran on | `ubuntu22@20260720.234.2` |
| Gap between the two jobs | **36 minutes** |

GitHub rolls a new image out behind the `ubuntu-22.04` label *gradually*, so two
jobs on the same evening landed on different images — and the rebuild got the
**older** one. Seven vendored libraries differed as a result:
`libgssapi_krb5.so.2`, `libk5crypto.so.3`, `libkrb5.so.3`, `libkrb5support.so.0`,
`libsqlite3.so.0`, `libsystemd.so.0`, `libudev.so.1`. Not merely different build
ids — different `.text` sizes and shifted symbol addresses, i.e. genuinely
different upstream builds (`libsqlite3.so.0`'s `.text` went `0xeaf9d` →
`0xeaf3d`).

**Confirmed by controlled experiment, not by argument.** Re-running the identical
rebuild on 2026-08-17 (`gh workflow run rebuild-verify.yml -f tag=v1.9.3`, run
`32036282718`) landed on `ubuntu22@20260810.260.1` — the same image the release
built on — and reproduced
`07bccca82cd4b16a57899bc2f18b81ba69595984d2c417b36289a9be278eebd3`,
**byte-identical** to the logged payload. Same tag, same public source, same
recipe; the only variable was the runner image, and holding it constant makes the
bytes match exactly. **v1.9.3 reproduces.**

That result also settles §6a: the CSP hash order is not re-rolled on every
compile, it tracks the build host's directory layout, so it too was downstream of
the image change.

Because a squashfs is compressed, one differing input file smears across the
whole archive: the August rebuild reported **105,863,272 differing bytes** out of
107,297,272. That number measures compression, not damage, and reading it as
"the binary is wholly different" is a mistake the diagnostic now pre-empts by
splitting differing paths into vendored (`usr/lib/`) and Pollis-built.

Since #944 `rebuild-verify.yml` **checks this precondition instead of assuming
it**: it reads `toolchain.runner_image` from the tag's own leaf (now surfaced by
`pollis-verify release --json`), compares it against the image the rebuild
actually ran on, and classifies the outcome as `environment_drift` rather than
`did_not_reproduce` when they differ. Every non-reproducing outcome still fails
the run — an inconclusive rebuild is not a passing one, and auto-greening drift
would open a window for a genuine divergence every time GitHub re-images a
runner — but the ledger can now say which kind of red it was, with the two image
ids as evidence. `scripts/test-rebuild-verdict.sh` pins both directions,
including that a same-image byte divergence still reports as the real finding.

### 6a. `usr/bin/pollis` — CSP hash ordering in `tauri-codegen`
Found while diagnosing `v1.9.3` (#944), and worth stating precisely because it
is the only thing that has ever made the *Pollis-built* binary differ across an
otherwise-clean rebuild.

The two `usr/bin/pollis` binaries differed **only** in the order of the twelve
`'sha256-…'` CSP hashes embedded in `.rodata` — same twelve values, permuted —
plus the `NT_GNU_BUILD_ID` that necessarily follows from any content change.
Nothing else in the binary differed. That the digests are *identical* is itself
the proof that the frontend bundle is byte-for-byte reproducible; only the order
of the list moved.

The cause is upstream and mechanical: `tauri-codegen`'s
`CspHashes::add_if_applicable` pushes one hash per `.js`/`.mjs` file into a
`Vec<String>` in `WalkDir` order over `frontendDist`, and `WalkDir` yields
`readdir(2)` order with no sort anywhere in the path
(`embedded_assets.rs`, `RawEmbeddedAssets::new`). `to_tokens` then emits the
`Vec` in that order, straight into the binary. So the byte layout of the shipped
executable depends on the order the build host's filesystem happens to return
directory entries in.

**Not fixed here, deliberately.** The three available fixes are each worse than
the defect: forking `tauri-codegen` behind `[patch.crates-io]` pins the whole
Tauri build stack to one version and blocks routine updates (#750 vendored and
patched this same crate once, and reverted it); setting
`dangerousDisableAssetCspModification` removes the hashes by weakening CSP on a
security product; collapsing the frontend to a single JS chunk trades app
startup for byte layout. The list is a CSP allow-list, where order carries no
meaning, so the divergence is cosmetic. The right resolution is an upstream sort;
until then it is documented, and the rebuilder's diagnostic surfaces it by naming
`usr/bin/pollis` as a Pollis-built path so it is never again mistaken for
vendored drift.

### 6b. AppImage top-level icon symlink — unordered, cosmetic
Same rebuild, same class. The AppDir's root `pollis.png` is a symlink that
pointed at `usr/share/icons/hicolor/32x32/apps/pollis.png` in the shipped
AppImage and `…/512x512/…` in the rebuild. The bundler picks the target from an
unordered icon listing. Cosmetic, outside our source, and recorded so the next
reader does not spend time on it.

### 6c. AppImage tooling fetched from floating refs — unpinned by construction
Tauri's AppImage bundler downloads its tooling at build time, and two of the five
URLs are not pinned to a release at all:

```
…/linuxdeploy-plugin-gtk/master/linuxdeploy-plugin-gtk.sh          ← branch
…/linuxdeploy-plugin-gstreamer/master/linuxdeploy-plugin-gstreamer.sh  ← branch
…/linuxdeploy-plugin-appimage/releases/download/continuous/…      ← rolling tag
```

`master` and `continuous` can change between a release and its rebuild, and
neither is recorded in the leaf. This did **not** cause the `v1.9.3` divergence
(the release and the rebuild were 36 minutes apart, and the differing bytes are
fully accounted for above), but it is a live hole in the claim for any rebuild
attempted later — which is the normal case for a third party auditing an old tag.

This is not only a determinism concern — it is an availability one, and it was
observed directly while working on #944: a rebuild of `v1.9.3` died after a
14-minute compile with `failed to bundle project: http status: 429`, GitHub
rate-limiting the tooling download. A reproducer that fetches five files from
two hosts mid-build can fail for reasons that have nothing to do with the bytes
it is checking, and it does so *after* paying the full build cost.

The URLs are hardcoded in `tauri-bundler`
(`bundle/linux/appimage/linuxdeploy.rs`, confirmed by inspecting the shipped
`@tauri-apps/cli` 2.11.4 binary), so there is no per-URL override. The only lever
the bundler exposes is `TAURI_BUNDLER_TOOLS_GITHUB_MIRROR` /
`TAURI_BUNDLER_TOOLS_GITHUB_MIRROR_TEMPLATE`, which rewrites
`github.com/<owner>/<repo>/releases/download/<version>/<asset>` — enough to pin
`AppRun`, `linuxdeploy` and `linuxdeploy-plugin-appimage` behind a mirror we
control, but **not** the two `raw.githubusercontent.com/…/master/…` plugin
scripts, which do not match that pattern. Closing this properly therefore means
either hosting a mirror and vendoring the two shell scripts, or moving the Linux
release into a pinned container. Until then, treat a Linux payload rebuild of an
*old* tag as best-effort for this reason as well as for the runner image.

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
5. **Read the verdict token, not the red X.** Since #944 `rebuild-verify.yml`
   ends in one of four states, and only one of them is a claim about the shipped
   binary:

   | Verdict | Means | Run |
   |---|---|---|
   | `reproduced` | The rebuilt payload hash is the logged one. | green |
   | `did_not_reproduce` | Bytes differ **with the runner image held constant**. The real finding — treat as a security issue until diagnosed. | red |
   | `environment_drift` | Bytes differ, but the rebuild ran on a different runner image than the release recorded. Item 6 applies; the check's precondition was not met. | red |
   | `recipe_unrecorded` | Bytes differ and the leaf records no runner image (every tag ≤ v1.9.7), so the precondition cannot be evaluated at all. | red |

   All three non-green states fail the run deliberately: an inconclusive rebuild
   is not a passing one, and auto-greening drift would open a window for a real
   divergence every time GitHub re-images a runner. The distinction lives in the
   job title, the step summary, and `website/rebuild-ledger.json` — not in the
   exit code.

   The failure diagnostic also splits the differing paths by origin, because the
   single most useful question is which side of the line they fall on:
   `usr/lib/*` is vendored off the runner and tracks item 6, while
   `usr/bin/pollis` is ours and — apart from item 6a's CSP-hash ordering — should
   never differ. "Only vendored libraries differ" is printed explicitly when it
   holds.
