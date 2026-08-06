#!/usr/bin/env bash
#
# payload-hash.sh — the ONE canonical payload-hashing implementation, sourced by
# both the release attest job (scripts/attest-binaries.sh) and the independent
# rebuilder (.github/workflows/rebuild-verify.yml).
#
# Reproducibility only works if the party who LOGS a hash and the party who
# INDEPENDENTLY RECOMPUTES it use byte-identical hashing. Two copies of this
# logic would be a silent drift hazard: a rebuilder computing a hash a hair
# differently than the attest job would report a spurious mismatch and cry wolf,
# or (worse) a real divergence could hide behind an incidental formatting
# difference. So there is exactly one copy, here, and everything sources it.
#
# It exposes three functions:
#
#   sha_file <path>
#       sha256 of a single file's bytes, lowercase hex. Used for artifacts whose
#       SHIPPED bytes ARE the reproducible payload (Linux AppImage/deb/rpm — the
#       Tauri minisign signature is detached, so the file itself is the payload).
#
#   sha_tree <dir>
#       Deterministic sha256 of a directory tree: a name-sorted tar with fixed
#       mtime/owner/group so the hash depends ONLY on payload contents, never on
#       filesystem or extraction-order noise. Used for artifacts whose shipped
#       file WRAPS a payload (the macOS `.app` inside a `.dmg`, the unsigned
#       exe+resources inside an NSIS installer). Requires SOURCE_DATE_EPOCH to be
#       exported — the tag commit's unix seconds — so the archive mtime is a
#       deterministic, independently-recoverable value (git), not `now`.
#
#   sha_macos_payload <path/to/.app>
#       Deterministic sha256 of a SHIPPED (signed + notarized) macOS `.app`, with
#       everything Apple's signing pipeline wrote into it normalized back out
#       first. macOS only — it shells out to `codesign`/`stapler`. See the long
#       note above the function for why the macOS payload is defined this way and
#       the Linux/Windows ones are not.
#
# No side effects on source: sourcing this file only defines these functions.

# sha256 of a single file, lowercase hex.
sha_file() { sha256sum "$1" | awk '{print $1}'; }

# Deterministic sha256 of a directory tree. SOURCE_DATE_EPOCH (tag commit unix
# seconds) fixes the archive mtime so the hash is a pure function of contents.
#
# GNU tar only: `--sort=name` is a GNU extension (BSD/macOS tar lacks it). The
# pre-signature payload for macOS is captured on a macOS runner, which ships BSD
# tar, so that job installs GNU tar and exports `TAR=gtar`; everywhere else the
# default `tar` is already GNU. Both must be GNU tar for the archive bytes — and
# therefore the hash — to match between the party that LOGS and the party that
# REBUILDS (see the file header). Windows captures run under Git-for-Windows bash,
# whose `tar` is already GNU.
sha_tree() {
  : "${SOURCE_DATE_EPOCH:?SOURCE_DATE_EPOCH required for sha_tree (tag commit unix seconds)}"
  "${TAR:-tar}" --sort=name --mtime="@${SOURCE_DATE_EPOCH}" \
      --owner=0 --group=0 --numeric-owner \
      -cf - -C "$1" . | sha256sum | awk '{print $1}'
}

# ── macOS: the payload digest of a SIGNED bundle ────────────────────────────
#
# Apple's signature and notarization ticket are unreproducible BY CONSTRUCTION:
# the CMS signature embeds a timestamp and the ticket is issued per submission by
# Apple. No amount of build determinism makes two signings byte-equal. Every
# project that takes reproducibility seriously answers this the same way — don't
# try to reproduce the signature, EXCLUDE it. F-Droid requires APKs be identical
# "apart from the signature"; the Mach-O convention is to normalize the
# LC_CODE_SIGNATURE region away before comparing.
#
# Pollis used to answer it differently, and worse (#603/#704): build the app
# TWICE — once unsigned to hash, once signed to ship — and assert the two shared a
# compiled binary. That made the release gate depend on the Rust toolchain
# emitting byte-identical output across two compiles of the same source, which it
# does not reliably do (#750: rustc/LLVM non-determinism under LTO, rust-lang
# #52044/#50556/#129080). The gate failed on a real release for a reason that had
# nothing to do with the release being unsound.
#
# It was also a WEAKER claim than it looked. The digest described an unsigned
# bundle that existed only inside that CI job and which nobody outside could ever
# obtain — so no third party could check it against anything. This function
# derives the digest FROM THE ARTIFACT WE SHIP, which means anyone holding the
# public .dmg can recompute it: mount, copy out the .app, run this, compare to
# the logged leaf. A claim an outsider can falsify is worth more than one they
# cannot evaluate.
#
# What is normalized out, and nothing else:
#   1. the stapled notarization ticket (Apple-issued, per submission),
#   2. each embedded Mach-O code signature (per signing operation),
#   3. the `_CodeSignature` manifests codesign writes into the bundle.
# Extended attributes (quarantine, provenance) never enter the digest at all —
# GNU tar omits them without `--xattrs`, which sha_tree does not pass.
#
# The strip is VERIFIED, not assumed. A silently-failing codesign would otherwise
# yield a digest over still-signed bytes — a leaf nobody could ever recompute,
# which is precisely the failure this whole path exists to prevent.
sha_macos_payload() {
  : "${SOURCE_DATE_EPOCH:?SOURCE_DATE_EPOCH required for sha_macos_payload}"
  local app="$1" work copy digest
  if [ ! -d "$app" ]; then
    echo "sha_macos_payload: not a bundle directory: $app" >&2
    return 1
  fi

  # Normalize a COPY — the bundle we ship must reach the .dmg untouched. -R keeps
  # symlinks as symlinks (framework layouts are symlink farms; dereferencing them
  # would both corrupt the layout and double-count the targets in the digest).
  work="$(mktemp -d)" || return 1
  copy="$work/$(basename "$app")"
  cp -Rp "$app" "$copy" || { rm -rf "$work"; return 1; }

  # 1. Notarization ticket.
  xcrun stapler unstaple "$copy" >/dev/null 2>&1 || true

  # 2. Embedded Mach-O signatures. Order is irrelevant — removing an outer
  #    signature does not rewrite the nested binaries, it only invalidates the
  #    seal over them, and we are about to strip those too.
  local macho
  while IFS= read -r macho; do
    codesign --remove-signature "$macho" >/dev/null 2>&1 || true
  done < <(find "$copy" -type f -exec sh -c 'file -b "$1" | grep -q "Mach-O"' _ {} \; -print)

  # 3. Bundle signature manifests.
  find "$copy" -type d -name '_CodeSignature' -prune -exec rm -rf {} +

  # Prove the strip landed. If any signature survived, the digest would not be
  # reproducible by anyone and must not be published.
  if codesign -dv "$copy" >/dev/null 2>&1; then
    echo "sha_macos_payload: bundle STILL carries a code signature after strip" >&2
    rm -rf "$work"
    return 1
  fi
  if xcrun stapler validate "$copy" >/dev/null 2>&1; then
    echo "sha_macos_payload: bundle STILL carries a notarization ticket after strip" >&2
    rm -rf "$work"
    return 1
  fi

  digest="$(sha_tree "$copy")" || { rm -rf "$work"; return 1; }
  rm -rf "$work"
  printf '%s\n' "$digest"
}
