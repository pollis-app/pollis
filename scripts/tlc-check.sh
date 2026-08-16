#!/usr/bin/env bash
# Run the TLA+ machine-checked-correctness specs through TLC.
#
# Two specs, both fast CI gates — TLC exhaustively checks each small config in
# seconds (docs/machine-checked-correctness-design.md §3):
#   Spec A — CommitLog (epoch/commit-log machine, invariants I1 + I2)
#   Spec B — Delivery  (delivery/retention,        invariants I3 + I4)
#
# Usage:
#   scripts/tlc-check.sh            # check both SOUND specs (all must PASS)
#   scripts/tlc-check.sh --broken   # check every TEETH config (each must FAIL)
#
# Requirements: a JRE (java on PATH, or JAVA_HOME set) and tla2tools.jar. If the
# jar is absent it is downloaded to $TLA_TOOLS_DIR (default: this script's dir).
# Pin: TLA2TOOLS_VERSION (default v1.8.0).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SPEC_DIR="$(cd "$SCRIPT_DIR/../specs/tla" && pwd)"
TLA_TOOLS_DIR="${TLA_TOOLS_DIR:-$SCRIPT_DIR}"
TLA2TOOLS_VERSION="${TLA2TOOLS_VERSION:-v1.8.0}"
JAR="$TLA_TOOLS_DIR/tla2tools.jar"

JAVA_BIN="java"
if [[ -n "${JAVA_HOME:-}" ]]; then
  JAVA_BIN="$JAVA_HOME/bin/java"
fi
if ! "$JAVA_BIN" -version >/dev/null 2>&1; then
  echo "error: no JRE found (need java on PATH or JAVA_HOME set)" >&2
  exit 127
fi

if [[ ! -f "$JAR" ]]; then
  echo "tla2tools.jar not found; downloading $TLA2TOOLS_VERSION ..."
  curl -fsSL -o "$JAR" \
    "https://github.com/tlaplus/tlaplus/releases/download/${TLA2TOOLS_VERSION}/tla2tools.jar"
fi

# run_tlc <module> <cfg> [extra TLC flags...]
#
# When TLC_LOG_DIR is set, each config's full TLC output is also written to
# "$TLC_LOG_DIR/<cfg>.log" (#877). Until then the state counts, the search depth
# and the counterexamples existed only in a CI job log that ages out, so "we
# model-check the epoch and delivery machines" was a claim with no published
# evidence behind it. scripts/tlc-summary.sh turns these logs into the run
# summary and the numbers on pollis.com/assurance.
#
# The log dir is deliberately OUTSIDE specs/tla: this function runs after a
# `cd "$SPEC_DIR"`, and TLC is invoked with `-cleanup`, which wipes its own
# scratch state under the spec directory.
#
# `set -o pipefail` is on (line 16), so tee cannot swallow a TLC failure — the
# pipeline still reports TLC's non-zero status, which check_teeth depends on.
run_tlc() {
  local module="$1"; local cfg="$2"; shift 2
  if [[ -n "${TLC_LOG_DIR:-}" ]]; then
    mkdir -p "$TLC_LOG_DIR"
    "$JAVA_BIN" -XX:+UseParallelGC -cp "$JAR" tlc2.TLC \
      "$@" -config "$cfg" -cleanup "$module" 2>&1 | tee "$TLC_LOG_DIR/${cfg}.log"
  else
    "$JAVA_BIN" -XX:+UseParallelGC -cp "$JAR" tlc2.TLC \
      "$@" -config "$cfg" -cleanup "$module"
  fi
}

# check_sound <label> <module> <cfg> [extra flags...] — must PASS.
check_sound() {
  local label="$1"; shift
  echo "== TLC check ($label) — all invariants must pass =="
  run_tlc "$@"
  echo "OK: $label passed."
}

# check_teeth <label> <module> <cfg> [extra flags...] — must FAIL (be refuted).
# We invert the exit code: a TLC failure here is SUCCESS for us (the invariant
# has teeth), and a clean pass is a regression (the check went vacuous).
check_teeth() {
  local label="$1"; shift
  echo "== TLC teeth check ($label) — expecting a violation =="
  if run_tlc "$@"; then
    echo "ERROR: $label unexpectedly PASSED — the invariant has no teeth." >&2
    exit 1
  fi
  echo "OK: $label produced the expected counterexample."
}

cd "$SPEC_DIR"

# CommitLog reaches quiescent terminal states (all members removed, or the epoch
# bound hit with everyone caught up), which is expected, not a bug — so disable
# TLC's deadlock check for it. Delivery has a live action in every state.
if [[ "${1:-}" == "--broken" ]]; then
  check_teeth "CommitLog (broken submit-guard)" CommitLog.tla CommitLogBroken.cfg -deadlock
  check_teeth "CommitLog (broken migration compare-and-swap)" \
    CommitLog.tla CommitLogMigrateBroken.cfg -deadlock
  check_teeth "Delivery (broken retention-guard)" Delivery.tla DeliveryBroken.cfg
  echo "OK: all teeth configs produced the expected counterexamples."
  exit 0
fi

check_sound "CommitLog (OnePerEpoch, Gapless, OpeningClosesTheHead, NoForeignAdopt, LineageMonotone)" \
  CommitLog.tla CommitLog.cfg -deadlock
check_sound "Delivery (NoLossForCurrentMember, CursorMonotone, AcceptedLossesOnly)" \
  Delivery.tla Delivery.cfg
echo "OK: both specs passed."
