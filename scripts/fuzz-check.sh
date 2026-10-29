#!/usr/bin/env bash
# Track-B fuzz smoke gate (#481, machine-checked-correctness §5.1). Builds and
# briefly runs each cargo-fuzz target over the load-bearing pure functions on
# NIGHTLY, out of band from the pinned-stable release build. A crash = a property
# violation (P1/P2/P3 no-skip, no-gap-apply, no-foreign-adopt, recovery-gate,
# and since #1537 zero message loss over real MLS state, the untrusted-input
# parsers, the decrypt order and the outbox transitions).
#
#   ./scripts/fuzz-check.sh                # smoke: build + short-run all targets
#   FUZZ_MUTANT=1 ./scripts/fuzz-check.sh  # teeth: build each target's mutant and
#                                          # confirm the fuzzer trips it fast
#   FUZZ_TARGETS="framing outbox" ./scripts/fuzz-check.sh   # a subset
#
# The `fuzz/` crate is DETACHED from the workspace (its own [workspace] table +
# root `exclude`) because it needs nightly while the repo pins Rust 1.96.0 stable
# for reproducible release builds. This script therefore never touches the
# release path.
set -euo pipefail

# Nightly toolchain to use. Overridable; defaults to the rustup `nightly` channel.
NIGHTLY="${NIGHTLY:-nightly}"
TARGETS=(next_watermark classify resolve may_rejoin prune_floor framing sealing_order ds_bodies outbox decrypt_untrusted mls_delivery)
if [[ -n "${FUZZ_TARGETS:-}" ]]; then
  read -ra TARGETS <<< "$FUZZ_TARGETS"
fi
# Short per-target budget for a smoke run; override for a deeper local soak.
RUNS="${FUZZ_RUNS:-200000}"
MAX_TIME="${FUZZ_MAX_TOTAL_TIME:-20}"

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root/fuzz"

# Build a target with LeakSanitizer off. cargo-fuzz's RUSTFLAGS carry
# -Zsanitizer=address, which the `cc` crate forwards to C it compiles — so
# libsql-sqlite3-parser's build script builds its `lemon` parser generator
# with ASan and then runs it, and lemon (which never frees before exit) trips
# LSan and fails the build. Leak checking stays on for the fuzz run itself,
# which reuses this build: LSAN_OPTIONS is not a build input.
build_target() {
  LSAN_OPTIONS="detect_leaks=0" cargo "+$NIGHTLY" fuzz build "$1"
}

if ! command -v cargo-fuzz >/dev/null 2>&1; then
  echo "cargo-fuzz not found — install with: cargo install cargo-fuzz" >&2
  exit 1
fi

# The fuzz crate is a detached workspace, so it carries its OWN copy of the
# root's [patch.crates-io] openmls pin. If the two revs drift apart the fuzz
# build either fails obscurely or — worse — fuzzes a different openmls than the
# one that ships. Refuse to run until they match again.
root_revs="$(grep -oE 'openmls/openmls", rev = "[0-9a-f]+"' "$repo_root/Cargo.toml" | sort -u)"
fuzz_revs="$(grep -oE 'openmls/openmls", rev = "[0-9a-f]+"' Cargo.toml | sort -u)"
if [[ "$root_revs" != "$fuzz_revs" ]]; then
  echo "openmls pin drift: fuzz/Cargo.toml's [patch.crates-io] rev does not match the root Cargo.toml's." >&2
  echo "root: $root_revs" >&2
  echo "fuzz: $fuzz_revs" >&2
  exit 1
fi

# Teeth mode: build each target's deliberate mutant and assert the fuzzer finds a
# crash quickly. A mutant that does NOT crash is itself a failure (toothless).
if [[ "${FUZZ_MUTANT:-0}" == "1" ]]; then
  export RUSTFLAGS="${RUSTFLAGS:-} --cfg fuzz_mutant"
  echo "== TEETH: fuzzing deliberately-broken mutants (expect fast crashes) =="
  for t in "${TARGETS[@]}"; do
    echo "--- mutant: $t ---"
    work="$(mktemp -d)"
    cp -r "corpus/$t/." "$work/" 2>/dev/null || true
    build_target "$t"
    if cargo "+$NIGHTLY" fuzz run "$t" "$work" -- -runs="$RUNS" -max_total_time="$MAX_TIME"; then
      echo "TEETH FAILURE: mutant '$t' did not crash — the property check is toothless" >&2
      exit 1
    else
      echo "OK: mutant '$t' crashed (property has teeth)"
    fi
    rm -rf "$work"
  done
  echo "== all mutants tripped =="
  exit 0
fi

# Smoke mode: the committed (clean) targets must build and run crash-free. Run
# against a throwaway copy of the seed corpus so the committed seeds stay pristine.
echo "== SMOKE: building + short-running all targets on $NIGHTLY (clean) =="
for t in "${TARGETS[@]}"; do
  echo "--- $t ---"
  work="$(mktemp -d)"
  cp -r "corpus/$t/." "$work/" 2>/dev/null || true
  build_target "$t"
  cargo "+$NIGHTLY" fuzz run "$t" "$work" -- -runs="$RUNS" -max_total_time="$MAX_TIME"
  rm -rf "$work"
done
echo "== all targets built + ran clean =="
