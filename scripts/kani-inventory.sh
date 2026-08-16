#!/usr/bin/env bash
# Enumerate the Kani proof harnesses in the tree (#877).
#
# `kani.yml` runs `cargo kani` over pollis-core and pollis-delivery on every PR
# touching the proof surface, and on every push to main — and published nothing
# at all. A green tick told a reader that *something* was proven, never what.
# "Kani" appeared nowhere on the website.
#
# This emits the inventory the verdict is about: every harness, the function it
# constrains, and whether it is a positive proof or a TEETH harness (a
# deliberately broken mutant that Kani must refute — the thing that stops the
# suite rotting into a vacuous pass). The job's own exit status remains the
# verdict; this says what the verdict ranges over.
#
# It reads the source, not Kani's output, on purpose: the `model-checking/kani-github-action`
# owns its invocation and gives no hook to tee the report, and an inventory
# derived from the source cannot drift from the code the way a pasted number can.
#
# Usage:
#   scripts/kani-inventory.sh [out.json]      # markdown on stdout, JSON to $1

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
JSON_OUT="${1:-}"

# The three files that carry harnesses, and the Kani invocation each belongs to.
# Listed rather than globbed so a harness added in a new file has to be
# classified into a `cargo kani` run deliberately — an unlisted file would
# otherwise be silently reported as covered while no invocation runs it.
declare -A PACKAGE=(
  [pollis-core/src/commands/messages/watermark.rs]="pollis-core"
  [pollis-core/src/commands/mls/invariants.rs]="pollis-core"
  [pollis-delivery/src/commit.rs]="pollis-delivery"
)

harnesses="[]"

for rel in "${!PACKAGE[@]}"; do
  f="$REPO_ROOT/$rel"
  if [ ! -f "$f" ]; then
    echo "error: $rel is missing — the harness inventory is stale" >&2
    exit 1
  fi

  # A harness is `fn <name>()` preceded by #[kani::proof]; #[kani::should_panic]
  # on the same attribute run marks it as a teeth/mutant harness. awk carries
  # the flags forward from the attributes to the fn line that follows them.
  while IFS=$'\t' read -r line name teeth; do
    [ -n "$name" ] || continue
    harnesses="$(printf '%s' "$harnesses" | jq \
      --arg file "$rel" --arg pkg "${PACKAGE[$rel]}" \
      --arg name "$name" --arg line "$line" --arg teeth "$teeth" \
      '. += [{
         file: $file, package: $pkg, harness: $name,
         line: ($line | tonumber),
         kind: (if $teeth == "1" then "teeth" else "proof" end)
       }]')"
  done < <(awk '
    # Anchored to the start of the line so a doc comment MENTIONING the
    # attribute (there are several, explaining what teeth means) is not counted
    # as one. That mis-read inverted the proof/teeth split on first writing.
    /^[[:space:]]*#\[kani::proof\][[:space:]]*$/        { isproof = 1; next }
    /^[[:space:]]*#\[kani::should_panic\][[:space:]]*$/ { teeth = 1; next }
    isproof && /^[[:space:]]*fn [a-zA-Z0-9_]+/ {
      match($0, /fn [a-zA-Z0-9_]+/);
      nm = substr($0, RSTART + 3, RLENGTH - 3);
      printf "%d\t%s\t%d\n", NR, nm, teeth;
      isproof = 0; teeth = 0;
    }
  ' "$f")
done

total="$(printf '%s' "$harnesses" | jq 'length')"
if [ "$total" -eq 0 ]; then
  echo "error: found no Kani harnesses — the parser or the layout changed" >&2
  exit 1
fi

summary="$(printf '%s' "$harnesses" | jq -n \
  --arg generated "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
  --argjson h "$harnesses" '
  {
    generated_at: $generated,
    total: ($h | length),
    proofs: ($h | map(select(.kind == "proof")) | length),
    teeth:  ($h | map(select(.kind == "teeth")) | length),
    by_package: ($h | group_by(.package) | map({
      package: .[0].package,
      total: length,
      proofs: (map(select(.kind == "proof")) | length),
      teeth:  (map(select(.kind == "teeth")) | length)
    })),
    harnesses: ($h | sort_by(.file, .line))
  }')"

if [ -n "$JSON_OUT" ]; then
  printf '%s\n' "$summary" > "$JSON_OUT"
fi

printf '%s' "$summary" | jq -r '
  "## Kani bounded model checking",
  "",
  "`cargo kani` proves these harnesses exhaustively over their bounded input",
  "domains. A **teeth** harness is a deliberately broken mutant that Kani must",
  "refute — if one ever passes, the proof beside it has gone vacuous.",
  "",
  "**" + (.total | tostring) + " harnesses: "
    + (.proofs | tostring) + " proofs, "
    + (.teeth | tostring) + " teeth.**",
  "",
  "| `cargo kani` invocation | Harnesses | Proofs | Teeth |",
  "|---|---:|---:|---:|",
  (.by_package[] | "| `--package " + .package + "` | " + (.total | tostring)
     + " | " + (.proofs | tostring) + " | " + (.teeth | tostring) + " |"),
  "",
  "<details><summary>Every harness</summary>",
  "",
  "| Harness | Kind | Source |",
  "|---|---|---|",
  (.harnesses[] | "| `" + .harness + "` | " + .kind
     + " | `" + .file + ":" + (.line | tostring) + "` |"),
  "",
  "</details>"
'
