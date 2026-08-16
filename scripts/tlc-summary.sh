#!/usr/bin/env bash
# Turn the TLC logs written by scripts/tlc-check.sh into a durable result
# summary (#877).
#
# Before this, both TLA+ specs ran on every PR and published nothing: the state
# counts, the search depth and the refuted counterexamples lived in a CI job log
# that ages out, and the words "TLA", "TLC" and "model check" appeared nowhere
# on the website. The proofs were being paid for and thrown away.
#
# Reads:  $TLC_LOG_DIR/<cfg>.log        (written when tlc-check.sh runs with TLC_LOG_DIR set)
# Writes: markdown on stdout, and JSON to $1 if given.
#
# Usage:
#   TLC_LOG_DIR=/tmp/tlc scripts/tlc-summary.sh [out.json] >> "$GITHUB_STEP_SUMMARY"
#
# Parsing notes, because TLC's output shape is load-bearing here:
#   * The end-of-run counts line is `N states generated, M distinct states
#     found, K states left on queue.` — with thousands separators in the
#     PROGRESS lines but not in the final one, so the final line is matched on
#     its own anchor rather than by taking the last number seen.
#   * A SOUND run prints `Model checking completed. No error has been found.`
#     and never names its invariants; the invariant list therefore comes from
#     the .cfg, not the log. That also keeps the published claim honest if a
#     cfg changes — the page reports what was actually checked.
#   * A safety violation prints `Error: Invariant <Name> is violated.`; a
#     temporal-property violation prints `Error: Temporal properties were
#     violated.` with no name, because LineageMonotone and CursorMonotone are
#     PROPERTIES rather than INVARIANTS. Both are matched.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SPEC_DIR="$REPO_ROOT/specs/tla"
LOG_DIR="${TLC_LOG_DIR:-}"
JSON_OUT="${1:-}"

if [ -z "$LOG_DIR" ] || [ ! -d "$LOG_DIR" ]; then
  echo "error: set TLC_LOG_DIR to the directory tlc-check.sh wrote its logs to" >&2
  exit 2
fi

# The five configs, and whether each is a sound spec (must pass) or a teeth
# config (must be refuted — a clean pass means the invariant went vacuous).
# Keeping the mode here rather than inferring it from the filename means a new
# cfg has to be classified deliberately.
declare -A MODE=(
  [CommitLog.cfg]=sound
  [Delivery.cfg]=sound
  [CommitLogBroken.cfg]=teeth
  [CommitLogMigrateBroken.cfg]=teeth
  [DeliveryBroken.cfg]=teeth
)
declare -A MODULE=(
  [CommitLog.cfg]=CommitLog
  [Delivery.cfg]=Delivery
  [CommitLogBroken.cfg]=CommitLog
  [CommitLogMigrateBroken.cfg]=CommitLog
  [DeliveryBroken.cfg]=Delivery
)

# Pull the INVARIANTS / PROPERTIES / CONSTANT lines out of a .cfg so the summary
# states what was checked and at what model bound.
cfg_block() {
  local cfg="$1" key="$2"
  awk -v k="$key" '
    $1 == k { grabbing = 1; next }
    /^[A-Z]+$/ { grabbing = 0 }
    grabbing && NF { gsub(/^[ \t]+|[ \t]+$/, ""); printf "%s ", $0 }
  ' "$SPEC_DIR/$cfg" 2>/dev/null | sed 's/ *$//'
}

# The CONSTANTS block is the model bound — the thing an auditor needs in order
# to read a state count as anything at all. "72,228 distinct states" means
# nothing without "3 clients, 2 commits, 4 epochs".
cfg_constants() {
  awk '
    $1 == "CONSTANTS" || $1 == "CONSTANT" { grabbing = 1; next }
    /^[A-Z]+$/ { grabbing = 0 }
    grabbing && NF {
      gsub(/^[ \t]+|[ \t]+$/, "");
      out = out (out == "" ? "" : "; ") $0
    }
    END { print out }
  ' "$SPEC_DIR/$1" 2>/dev/null || true
}

results="[]"

for cfg in CommitLog.cfg Delivery.cfg CommitLogBroken.cfg CommitLogMigrateBroken.cfg DeliveryBroken.cfg; do
  log="$LOG_DIR/${cfg}.log"
  [ -f "$log" ] || continue

  counts="$(grep -E '^[0-9]+ states generated, [0-9]+ distinct states found' "$log" | tail -1 || true)"
  generated="$(printf '%s' "$counts" | sed -nE 's/^([0-9]+) states generated.*/\1/p')"
  distinct="$(printf '%s' "$counts" | sed -nE 's/.*, ([0-9]+) distinct states found.*/\1/p')"
  depth="$(sed -nE 's/^The depth of the complete state graph search is ([0-9]+)\./\1/p' "$log" | tail -1 || true)"
  duration="$(sed -nE 's/^Finished in (.*) at .*/\1/p' "$log" | tail -1 || true)"

  violated="$(sed -nE 's/^Error: Invariant ([A-Za-z0-9_]+) is violated\./\1/p' "$log" | head -1 || true)"
  if [ -z "$violated" ] && grep -q '^Error: Temporal properties were violated\.' "$log"; then
    violated="(temporal property)"
  fi

  if grep -q '^Model checking completed\. No error has been found\.' "$log"; then
    outcome="no_error"
  elif [ -n "$violated" ]; then
    outcome="violated"
  else
    outcome="unknown"
  fi

  mode="${MODE[$cfg]}"
  # A sound config must report no error; a teeth config must report a violation.
  # Anything else is a gate that has stopped meaning what it says.
  if { [ "$mode" = "sound" ] && [ "$outcome" = "no_error" ]; } \
     || { [ "$mode" = "teeth" ] && [ "$outcome" = "violated" ]; }; then
    verdict="pass"
  else
    verdict="FAIL"
  fi

  results="$(printf '%s' "$results" | jq \
    --arg cfg "$cfg" --arg module "${MODULE[$cfg]}" --arg mode "$mode" \
    --arg outcome "$outcome" --arg verdict "$verdict" \
    --arg violated "$violated" \
    --arg generated "${generated:-0}" --arg distinct "${distinct:-0}" \
    --arg depth "${depth:-0}" --arg duration "${duration:-}" \
    --arg invariants "$(cfg_block "$cfg" INVARIANTS)" \
    --arg properties "$(cfg_block "$cfg" PROPERTIES)" \
    --arg constants "$(cfg_constants "$cfg")" \
    '. += [{
       cfg: $cfg, module: $module, mode: $mode,
       outcome: $outcome, verdict: $verdict,
       violated: (if $violated == "" then null else $violated end),
       states_generated: ($generated | tonumber),
       distinct_states: ($distinct | tonumber),
       depth: ($depth | tonumber),
       duration: $duration,
       invariants: ($invariants | split(" ") | map(select(length > 0))),
       properties: ($properties | split(" ") | map(select(length > 0))),
       constants: $constants
     }]')"
done

if [ "$(printf '%s' "$results" | jq 'length')" -eq 0 ]; then
  echo "error: no TLC logs found in $LOG_DIR" >&2
  exit 1
fi

summary="$(printf '%s' "$results" | jq -n \
  --arg generated "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
  --arg tla2tools "${TLA2TOOLS_VERSION:-v1.8.0}" \
  --argjson results "$results" \
  '{ generated_at: $generated, tla2tools_version: $tla2tools, configs: $results }')"

if [ -n "$JSON_OUT" ]; then
  printf '%s\n' "$summary" > "$JSON_OUT"
fi

# ── Markdown ───────────────────────────────────────────────────────────────
printf '%s' "$summary" | jq -r '
  "## TLA+ model checking (TLC " + .tla2tools_version + ")",
  "",
  "Exhaustive check of both specs over their small configs, plus the teeth",
  "configs that must still produce a counterexample.",
  "",
  "| Spec | Config | Mode | Result | States | Distinct | Depth | Time |",
  "|---|---|---|---|---:|---:|---:|---|",
  (.configs[]
    | "| `" + .module + "` | `" + .cfg + "` | " + .mode
      + " | " + (if .verdict == "pass" then
                   (if .mode == "sound" then "✅ all invariants hold"
                    else "✅ refuted: `" + (.violated // "?") + "`" end)
                 else "❌ " + .outcome end)
      + " | " + (.states_generated | tostring)
      + " | " + (.distinct_states | tostring)
      + " | " + (.depth | tostring)
      + " | " + (.duration // "-") + " |"),
  "",
  "**Checked invariants**",
  "",
  (.configs[] | select(.mode == "sound")
    | "- `" + .module + "` — " + ((.invariants + .properties) | join(", "))),
  "",
  (.configs[] | select(.mode == "sound")
    | "- `" + .cfg + "` bounds: " + .constants)
'

# Non-zero if any config disagreed with its declared mode, so this cannot
# silently report a green table for a gate that has rotted.
fails="$(printf '%s' "$summary" | jq '[.configs[] | select(.verdict != "pass")] | length')"
if [ "$fails" -gt 0 ]; then
  echo >&2 "error: ${fails} TLC config(s) did not match their declared mode"
  exit 1
fi
