#!/usr/bin/env bash
# Track-B fuzz smoke gate (#481, machine-checked-correctness §5.1). Builds and
# briefly runs each cargo-fuzz target over the load-bearing pure functions on
# NIGHTLY, out of band from the pinned-stable release build. A crash = a property
# violation (P1/P2/P3 no-skip, no-gap-apply, no-foreign-adopt, recovery-gate).
#
#   ./scripts/fuzz-check.sh                # smoke: build + short-run all targets
#   FUZZ_MUTANT=1 ./scripts/fuzz-check.sh  # teeth: build each target's mutant and
#                                          # confirm the fuzzer trips it fast
#
# The `fuzz/` crate is DETACHED from the workspace (its own [workspace] table +
# root `exclude`) because it needs nightly while the repo pins Rust 1.96.0 stable
# for reproducible release builds. This script therefore never touches the
# release path.
set -euo pipefail

# Nightly toolchain to use. Overridable; defaults to the rustup `nightly` channel.
NIGHTLY="${NIGHTLY:-nightly}"
TARGETS=(next_watermark classify resolve may_rejoin prune_floor)
# Short per-target budget for a smoke run; override for a deeper local soak.
RUNS="${FUZZ_RUNS:-200000}"
MAX_TIME="${FUZZ_MAX_TOTAL_TIME:-20}"

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root/fuzz"

if ! command -v cargo-fuzz >/dev/null 2>&1; then
  echo "cargo-fuzz not found — install with: cargo install cargo-fuzz" >&2
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
  cargo "+$NIGHTLY" fuzz run "$t" "$work" -- -runs="$RUNS" -max_total_time="$MAX_TIME"
  rm -rf "$work"
done
echo "== all targets built + ran clean =="
