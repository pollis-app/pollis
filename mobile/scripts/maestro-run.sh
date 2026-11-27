#!/usr/bin/env bash
# Pollis mobile e2e runner (#619, #2029). LOCAL MAC ONLY. Runs a tier of the
# Maestro suite on one or more devices at once and collects the screenshots
# into a dated, per-device gallery.
#
# Usage:
#   mobile/scripts/maestro-run.sh <tier> [ios] [ipad] [android]
#     gate                      the release gate: every flow tagged `gate`
#     all                       every flow under flows/ except the two-client ones
#     area <base-ref | path…>   the flows .maestro/areas.txt maps the change to
#     <flow> [flow…]            the named flows
#   Devices default to all three. They run concurrently; a device that is not
#   booted yet boots only when ~/.claude/scripts/device-headroom.sh says GO.
#   The original form still works: maestro-run.sh <flow|all> <ios|ipad|android>
# Examples:
#   mobile/scripts/maestro-run.sh gate                       # release gate, 3 devices
#   mobile/scripts/maestro-run.sh area origin/main ios       # what this branch touched
#   mobile/scripts/maestro-run.sh messaging reply android
#
# A device's run (README "Run"): one `maestro test` for the whole list, so the
# iOS driver starts once, in this order:
#   1. the `fresh-account` flows (auth, sign-in-device, …), each signing up an
#      address of its own;
#   2. setup/peer.yaml when a `peer` flow is selected; its @handle is read off
#      the screen, so here the run splits into a second invocation;
#   3. setup/setup.yaml, the run's shared account (a new MAESTRO_EMAIL per
#      device run), then every other flow, each starting from
#      subflows/home.yaml (relaunch + PIN, no clearState);
#   4. the failed flows once more, in one more invocation, on a new shared
#      account: passing then is a flake, failing again fails the run;
#   5. locale-smoke, when selected, through maestro-locale-smoke.sh.
#
# Env: IOS_DEVICE, IPAD_DEVICE, ANDROID_AVD (device names); RETRIES (default
# 1); LOCALES (locale-smoke's locales, default "ar zh hi", "all" = every
# catalogue); NO_HEADROOM=1 skips the memory check; DRY_RUN=1 prints each
# device's plan and runs nothing.
#
# Prereqs (.maestro/README.md): Maestro and a Java runtime; a Release build of
# the app pointed at the dev DS on every target device; .maestro/.env filled
# from env.example.
set -euo pipefail
# shellcheck source=maestro-app-id.sh
source "$(dirname "$0")/maestro-app-id.sh"

HERE="$(cd "$(dirname "$0")/.." && pwd)"
REPO="$(cd "$HERE/.." && pwd)"
MAE="$HERE/.maestro"
ENV_FILE="$MAE/.env"
HEADROOM="$HOME/.claude/scripts/device-headroom.sh"

IOS_DEVICE="${IOS_DEVICE:-iPhone 18 Pro}"
IPAD_DEVICE="${IPAD_DEVICE:-iPad Pro 13-inch (M5)}"
ANDROID_AVD="${ANDROID_AVD:-pollis_e2e}"
RETRIES="${RETRIES:-1}"
LOCALES="${LOCALES:-ar zh hi}"

usage() {
  sed -n '/^# Usage:/,/^# Examples:/p' "$0" | sed 's/^# \{0,1\}//' >&2
  exit 2
}

# ── arguments ──────────────────────────────────────────────────────────────
DEVICES=()
TIER=""
TARGS=()
for a in "$@"; do
  case "$a" in
    ios|ipad|android) DEVICES+=("$a") ;;
    -h|--help) usage ;;
    *)
      if [ -z "$TIER" ]; then
        TIER="$a"
      else
        TARGS+=("$a")
      fi
      ;;
  esac
done
if [ -z "$TIER" ]; then
  usage
fi
if [ ${#DEVICES[@]} -eq 0 ]; then
  DEVICES=(ios ipad android)
fi

# ── flow metadata: the tags in each flow's header ──────────────────────────
flow_file() {
  if [ -f "$MAE/flows/$1.yaml" ]; then
    echo "$MAE/flows/$1.yaml"
  else
    echo "$MAE/setup/$1.yaml"
  fi
}
has_tag() {
  # Not `grep -q`: under pipefail an early exit would fail sed with SIGPIPE.
  sed -n '/^---/q;p' "$(flow_file "$1")" | grep -x -- "  - $2" >/dev/null
}
all_flow_names() {
  local f
  for f in "$MAE"/flows/*.yaml; do
    if [ -e "$f" ]; then
      basename "$f" .yaml
    fi
  done
}
gate_flow_names() {
  local n
  for n in $(all_flow_names); do
    if has_tag "$n" gate; then
      echo "$n"
    fi
  done
}

# Every flow that runs subflow $1 (a file name), directly or through others.
flows_using_subflow() {
  local subs="$1" grown=1 s x
  while [ -n "$grown" ]; do
    grown=""
    for s in "$MAE"/subflows/*.yaml; do
      case " $subs " in *" $(basename "$s") "*) continue ;; esac
      for x in $subs; do
        if grep -q -- "$x" "$s"; then
          subs="$subs $(basename "$s")"
          grown=1
          break
        fi
      done
    done
  done
  for x in $subs; do
    grep -l -- "subflows/$x" "$MAE"/flows/*.yaml 2>/dev/null | sed 's#.*/##; s#\.yaml$##' || true
  done
}

# The flows to run for what changed: .maestro/areas.txt plus the harness's own
# files. Prints flow names; a touched two-client script is named on stderr.
area_flows() {
  local files f line pat rest matched key x
  if [ ${#TARGS[@]} -eq 1 ] && [ ! -e "$REPO/${TARGS[0]}" ] \
    && git -C "$REPO" rev-parse -q --verify "${TARGS[0]}^{commit}" >/dev/null; then
    files="$( (git -C "$REPO" diff --name-only "${TARGS[0]}...HEAD"
      git -C "$REPO" diff --name-only HEAD
      git -C "$REPO" ls-files --others --exclude-standard) | sort -u)"
  else
    if [ ${#TARGS[@]} -eq 0 ]; then
      echo "area needs a base ref or paths" >&2
      exit 2
    fi
    files="$(printf '%s\n' "${TARGS[@]}")"
  fi
  while IFS= read -r f; do
    [ -n "$f" ] || continue
    f="${f#./}"
    matched=""
    case "$f" in
      mobile/.maestro/flows/*.yaml)
        basename "$f" .yaml
        matched=1 ;;
      mobile/.maestro/subflows/*.yaml)
        flows_using_subflow "$(basename "$f")"
        matched=1 ;;
      mobile/.maestro/two-client/*|pollis-tui/examples/*)
        case "$f" in
          mobile/.maestro/two-client/*)
            key="$(echo "$f" | cut -d/ -f4)"
            key="two-client/$key([/\"]|\$)|/$key/" ;;
          *) key="$(basename "$f" .rs)" ;;
        esac
        grep -lE -- "$key" "$HERE"/scripts/maestro-*.sh 2>/dev/null | sed 's#.*/#two-client script: #' >&2 || true
        matched=1 ;;
    esac
    while IFS= read -r line; do
      case "$line" in ''|\#*) continue ;; esac
      pat="${line%%[[:space:]]*}"
      rest="${line#"$pat"}"
      # The pattern is a glob on purpose.
      # shellcheck disable=SC2053
      if [[ "$f" == $pat ]]; then
        matched=1
        for x in $rest; do
          if [ "$x" = "gate" ]; then
            gate_flow_names
          else
            echo "$x"
          fi
        done
      fi
    done < "$MAE/areas.txt"
    if [ -z "$matched" ]; then
      case "$f" in
        mobile/*) gate_flow_names ;;
      esac
    fi
  done <<< "$files"
}

# ── the selection ──────────────────────────────────────────────────────────
SELECTED=()
case "$TIER" in
  gate)
    for n in $(gate_flow_names); do
      SELECTED+=("$n")
    done ;;
  all)
    for n in $(all_flow_names); do
      # A `perf` flow (#2141) needs a build made with EXPO_PUBLIC_PERF_MARKS=1; it runs by name.
      if ! has_tag "$n" two-client && ! has_tag "$n" perf; then
        SELECTED+=("$n")
      fi
    done ;;
  area)
    for n in $(area_flows | sort -u); do
      if [ -f "$MAE/flows/$n.yaml" ] && ! has_tag "$n" two-client && ! has_tag "$n" perf; then
        SELECTED+=("$n")
      fi
    done ;;
  *)
    for n in "$TIER" ${TARGS[@]+"${TARGS[@]}"}; do
      n="${n%.yaml}"
      n="${n##*/}"
      if [ ! -f "$MAE/flows/$n.yaml" ]; then
        echo "no such flow: $MAE/flows/$n.yaml" >&2
        exit 1
      fi
      # A two-client flow needs a second device and a fresh peer (#1950); run
      # alone it can only fail, so stop and name the script instead.
      if has_tag "$n" two-client; then
        runners="$(grep -l -- "flows/$n.yaml" "$HERE"/scripts/maestro-*.sh 2>/dev/null | sed 's#.*/##' | tr '\n' ' ' || true)"
        echo "$n is a two-client flow: it needs a second device and a fresh peer." >&2
        echo "Run it with: ${runners:-a two-client script (see .maestro/README.md, \"Two-client flows\")}" >&2
        exit 2
      fi
      SELECTED+=("$n")
    done ;;
esac
if [ ${#SELECTED[@]} -eq 0 ]; then
  echo "==> nothing to run for: $TIER ${TARGS[*]:-}"
  exit 0
fi

# ── per-device plan ────────────────────────────────────────────────────────
# Splits SELECTED for one device into P_FRESH / P_SHARED / P_PEER / P_LOCALE.
# `groups` runs first among the shared flows: it drives the pill strip's "+"
# itself, which every later group pushes further past the screen edge.
plan_for() {
  local dev="$1" n p ordered=()
  P_FRESH=()
  P_SHARED=()
  P_PEER=()
  P_LOCALE=""
  P_SKIPPED=()
  for n in "${SELECTED[@]}"; do
    if has_tag "$n" ipad && [ "$dev" != "ipad" ]; then
      P_SKIPPED+=("$n")
    elif [ "$n" = "locale-smoke" ] && [ -z "${LOCALE:-}" ]; then
      # Not inside maestro-locale-smoke.sh: hand it the locales.
      P_LOCALE=1
    elif has_tag "$n" fresh-account; then
      P_FRESH+=("$n")
    elif has_tag "$n" peer; then
      P_PEER+=("$n")
    elif [ "$n" = "groups" ]; then
      P_SHARED=("$n" ${P_SHARED[@]+"${P_SHARED[@]}"})
    else
      P_SHARED+=("$n")
    fi
  done
  # report last: it blocks the peer (and unblocks it again).
  for p in dms dm-safety; do
    for n in ${P_PEER[@]+"${P_PEER[@]}"}; do
      if [ "$n" = "$p" ]; then
        ordered+=("$n")
      fi
    done
  done
  for n in ${P_PEER[@]+"${P_PEER[@]}"}; do
    case " dms dm-safety " in *" $n "*) ;; *) ordered+=("$n") ;; esac
  done
  P_PEER=(${ordered[@]+"${ordered[@]}"})
}

DATE="$(date +%Y-%m-%d)"
STAMP="$(date +%H%M%S)"
# A run inside maestro-locale-smoke.sh can start in the same second as the
# run that called it; it must not share (and truncate) that run's log dir.
if [ -n "${LOCALE:-}" ]; then
  STAMP="$STAMP-$LOCALE"
fi
EMAIL_BASE="$(sed -n 's/^MAESTRO_EMAIL=//p' "$ENV_FILE" 2>/dev/null | head -1 || true)"
EMAIL_BASE="${EMAIL_BASE:-pollis-e2e+primary@example.com}"

fmt_secs() {
  printf '%dm%02ds' $(($1 / 60)) $(($1 % 60))
}
ios_udid() {
  xcrun simctl list devices available | grep -F "    $1 (" | grep -Eo '[0-9A-F-]{36}' | head -1 || true
}
device_name() {
  case "$1" in
    ios) echo "$IOS_DEVICE" ;;
    ipad) echo "$IPAD_DEVICE" ;;
    android) echo "$ANDROID_AVD" ;;
  esac
}

needs_media() {
  local n
  for n in ${P_FRESH[@]+"${P_FRESH[@]}"} ${P_SHARED[@]+"${P_SHARED[@]}"} ${P_PEER[@]+"${P_PEER[@]}"}; do
    if grep -qs -- '- addMedia:' "$(flow_file "$n")"; then
      return 0
    fi
  done
  return 1
}

# True when flow $1 did not pass in attempt $2.
failed_in() {
  awk -v n="$1" -v a="$2" '$1 == n && $4 == a && $2 == "pass" { ok = 1 } END { exit ok ? 1 : 0 }' "$LOG/results.txt"
}


# One `maestro test` over several flows, in the given order: a workspace
# config whose executionOrder lists them (a flow's name is its file name).
# Appends "<flow> <pass|fail> <seconds> <attempt>" to results.txt.
invoke() {
  local attempt="$1" label="$2" n cfg junit dbg rc=0 t0 parsed line
  shift 2
  local names=()
  for n in "$@"; do
    if [ -n "$n" ]; then
      names+=("$n")
    fi
  done
  if [ ${#names[@]} -eq 0 ]; then
    return 0
  fi
  # Beside the flows, so its paths read the same however Maestro resolves
  # them; removed afterwards (a copy stays in the logs).
  cfg="$MAE/.run-$DEVICE_LABEL-$STAMP-$label.yaml"
  junit="$LOG/$label.junit.xml"
  dbg="$LOG/$label.debug"
  mkdir -p "$dbg"
  {
    echo "flows:"
    for n in "${names[@]}"; do
      echo "  - $(flow_file "$n" | sed "s#^$MAE/##")"
    done
    echo "executionOrder:"
    echo "  continueOnFailure: true"
    echo "  flowsOrder:"
    for n in "${names[@]}"; do
      echo "    - $n"
    done
  } > "$cfg"
  cp "$cfg" "$LOG/$label.config.yaml"
  echo "==> [$DEVICE_LABEL] $label: ${names[*]}"
  # Flows switch airplane mode on Android; one that failed offline must not
  # take the network from the next invocation (the flows' home.yaml and
  # onFlowComplete do the same inside one).
  if [[ "$DEVICE_ID" == emulator-* ]]; then
    adb -s "$DEVICE_ID" shell cmd connectivity airplane-mode disable >/dev/null 2>&1 || true
  fi
  t0="$(date +%s)"
  maestro --device "$DEVICE_ID" test --no-ansi --config "$cfg" --format JUNIT --output "$junit" \
    --debug-output "$dbg" ${ENV_ARGS[@]+"${ENV_ARGS[@]}"} \
    -e MAESTRO_EMAIL="$SHARED_EMAIL" -e MAESTRO_PEER_HANDLE="$PEER_HANDLE" \
    "$MAE" > "$LOG/$label.log" 2>&1 || rc=$?
  rm -f "$cfg"
  echo "    [$DEVICE_LABEL] $label took $(fmt_secs $(( $(date +%s) - t0 ))) (maestro exit $rc)"
  # Per-flow results from the JUnit report; a flow it does not list never ran.
  parsed="$(python3 -I - "$junit" <<'PY' 2>/dev/null || true
import sys, xml.etree.ElementTree as ET
try:
    root = ET.parse(sys.argv[1]).getroot()
except Exception:
    sys.exit(0)
for tc in root.iter("testcase"):
    name = tc.get("name") or tc.get("id")
    bad = tc.find("failure") is not None or tc.find("error") is not None
    status = tc.get("status")
    if status and status.upper() not in ("SUCCESS", "PASSED"):
        bad = True
    print(name, "fail" if bad else "pass", int(float(tc.get("time") or 0)))
PY
)"
  for n in "${names[@]}"; do
    line="$(printf '%s\n' "$parsed" | awk -v n="$n" '$1 == n { print; exit }')"
    if [ -n "$line" ]; then
      echo "$line $attempt" >> "$LOG/results.txt"
    else
      echo "$n fail 0 $attempt" >> "$LOG/results.txt"
    fi
  done
  # The gallery: named shots and failure captures, flattened.
  find "$dbg" -name "*.png" -exec cp {} "$OUT/" \; 2>/dev/null || true
  return 0
}

# One attempt: the fresh flows (+ the peer's sign-up), then the shared
# account and every flow on it. Two invocations only when a peer handle has
# to be read off the screen in between.
run_plan() {
  local attempt="$1" fresh="$2" shared="$3" peer="$4" second="" n
  local lp="${EMAIL_BASE%@*}" dp="${EMAIL_BASE#*@}"
  # A new shared account per attempt: a retry never inherits what a failed
  # flow left behind.
  SHARED_EMAIL="${lp}-${DEVICE_LABEL}-$(date +%Y%m%d%H%M%S)-a${attempt}@${dp}"
  if [ -n "${shared// /}${peer// /}" ]; then
    second="setup $shared"
  fi
  if [ -n "${peer// /}" ] && [ -z "$PEER_HANDLE" ]; then
    # shellcheck disable=SC2086
    invoke "$attempt" "a${attempt}-1" $fresh peer
    PEER_HANDLE="$(maestro_read_handle "$DEVICE_ID")"
    echo "    [$DEVICE_LABEL] peer handle: ${PEER_HANDLE:-<none>}"
    if [ -z "$PEER_HANDLE" ]; then
      for n in $peer; do
        echo "$n fail 0 $attempt" >> "$LOG/results.txt"
      done
      peer=""
    fi
    if [ -n "${shared// /}${peer// /}" ]; then
      # shellcheck disable=SC2086
      invoke "$attempt" "a${attempt}-2" $second $peer
    fi
  else
    # shellcheck disable=SC2086
    invoke "$attempt" "a${attempt}" $fresh $second $peer
  fi
}

# ── one device, in its own subshell ────────────────────────────────────────
# Writes $LOG/final.txt ("<flow> <pass|flake|fail|skipped> <seconds>") and
# $LOG/times.txt.
run_device() {
  local dev="$1" t0 tb ta tl id attempt rf rs rp n codes
  t0="$(date +%s)"
  OUT="$MAE/artifacts/$DATE/$dev"
  LOG="$OUT/logs/$STAMP"
  mkdir -p "$LOG"
  : > "$LOG/results.txt"
  : > "$LOG/final.txt"
  : > "$LOG/times.txt"
  plan_for "$dev"

  case "$dev" in
    ios|ipad)
      id="$(ios_udid "$(device_name "$dev")")"
      if [ -z "$id" ]; then
        echo "no available simulator named \"$(device_name "$dev")\"; set IOS_DEVICE / IPAD_DEVICE to one of:" >&2
        xcrun simctl list devices available | grep -E "iPhone|iPad" >&2
        return 1
      fi
      # Reset the photo library the flows seed with `addMedia` (#1769); it
      # only ever grows otherwise. A reset reboots the simulator: once per run.
      if needs_media; then
        "$HERE/scripts/maestro-media.sh" reset "$id"
      fi
      xcrun simctl boot "$id" 2>/dev/null || true
      xcrun simctl bootstatus "$id" >/dev/null 2>&1 || true
      ;;
    android)
      if ! adb devices | grep '^emulator-' >/dev/null; then
        echo "==> booting Android emulator: $ANDROID_AVD (headless)"
        # Headless: windowed boots have hung on this machine (#2042).
        ( "$ANDROID_HOME/emulator/emulator" -avd "$ANDROID_AVD" -no-snapshot -no-boot-anim -no-window >/dev/null 2>&1 & )
      fi
      adb wait-for-device
      adb shell 'while [ "$(getprop sys.boot_completed)" != "1" ]; do sleep 1; done'
      id="$(adb devices | awk '/^emulator-/{print $1; exit}')"
      # The `maestro` wrapper resets the seeded photos and keeps the picker
      # synced around the whole invocation (maestro-app-id.sh).
      if needs_media; then
        export POLLIS_MEDIA_SEEDS=1
      fi
      ;;
  esac
  DEVICE_ID="$id"

  ENV_ARGS=()
  if [ -f "$ENV_FILE" ]; then
    while IFS= read -r line; do
      case "$line" in ''|\#*|MAESTRO_EMAIL=*|MAESTRO_PEER_HANDLE=*) continue ;; esac
      ENV_ARGS+=(-e "$line")
    done < "$ENV_FILE"
  else
    echo "WARN: $ENV_FILE missing; copy env.example and fill it in." >&2
  fi
  # Per-device points for the out-of-process pickers and the bottom toggles
  # (profile-photo, keyboard-recovery, keyboard-back); a line in .env overrides them.
  if ! grep -q '^PICK_POINT=' "$ENV_FILE" 2>/dev/null; then
    case "$dev" in
      ios) ENV_ARGS+=(-e "PICK_POINT=16%,24%" -e "USE_POINT=89%,92%") ;;
      ipad) ENV_ARGS+=(-e "PICK_POINT=39%,35%" -e "USE_POINT=61%,35%") ;;
    esac
  fi
  if [ "$dev" = "ipad" ]; then
    if ! grep -q '^SCAN_TOGGLE_POINT=' "$ENV_FILE" 2>/dev/null; then
      ENV_ARGS+=(-e "SCAN_TOGGLE_POINT=50%,96%")
    fi
    if ! grep -q '^PIN_ZERO_POINT=' "$ENV_FILE" 2>/dev/null; then
      ENV_ARGS+=(-e "PIN_ZERO_POINT=50%,68%")
    fi
    # keyboard-back: the chat pane's composer (bounds about 16-92% across, 92% down).
    if ! grep -q '^COMPOSER_POINT=' "$ENV_FILE" 2>/dev/null; then
      ENV_ARGS+=(-e "COMPOSER_POINT=60%,92%")
    fi
    # The iPad's photo picker takes only point taps (subflows/ipad-pick-photos.yaml).
    ENV_ARGS+=(-e "IPAD=1")
  fi
  if [ -n "${LOCALE:-}" ]; then
    ENV_ARGS+=(-e "LOCALE=$LOCALE")
  fi

  PEER_HANDLE=""
  tb="$(date +%s)"
  run_plan 1 "${P_FRESH[*]:-}" "${P_SHARED[*]:-}" "${P_PEER[*]:-}"
  ta="$(date +%s)"
  echo "batch $((ta - tb))" >> "$LOG/times.txt"

  attempt=1
  while [ "$attempt" -le "$RETRIES" ]; do
    rf=""
    rs=""
    rp=""
    for n in ${P_FRESH[@]+"${P_FRESH[@]}"}; do
      if failed_in "$n" "$attempt"; then rf="$rf $n"; fi
    done
    for n in ${P_SHARED[@]+"${P_SHARED[@]}"}; do
      if failed_in "$n" "$attempt"; then rs="$rs $n"; fi
    done
    for n in ${P_PEER[@]+"${P_PEER[@]}"}; do
      if failed_in "$n" "$attempt"; then rp="$rp $n"; fi
    done
    if [ -z "$rf$rs$rp" ]; then
      break
    fi
    attempt=$((attempt + 1))
    echo "==> [$dev] retry:$rf$rs$rp"
    run_plan "$attempt" "$rf" "$rs" "$rp"
  done
  echo "retries $(( $(date +%s) - ta ))" >> "$LOG/times.txt"

  # Final status: pass on the first attempt, flake on a later one, else fail.
  for n in ${P_FRESH[@]+"${P_FRESH[@]}"} ${P_SHARED[@]+"${P_SHARED[@]}"} ${P_PEER[@]+"${P_PEER[@]}"}; do
    awk -v n="$n" '$1 == n { st[$4] = $2; t[$4] = $3; if ($4 > max) max = $4 }
      END {
        s = "fail"; secs = 0
        for (a = 1; a <= max; a++) {
          secs = t[a]
          if (st[a] == "pass") { s = (a == 1 ? "pass" : "flake"); break }
        }
        print n, s, secs
      }' "$LOG/results.txt" >> "$LOG/final.txt"
  done

  # locale-smoke: once per locale with the device language set first. The
  # script runs this runner again for each locale, which reports here.
  if [ -n "$P_LOCALE" ]; then
    tl="$(date +%s)"
    codes="$LOCALES"
    if [ "$codes" = "all" ]; then
      codes=""
    fi
    # shellcheck disable=SC2086
    MAESTRO_RUN_FINAL="$LOG/final.txt" "$HERE/scripts/maestro-locale-smoke.sh" "$dev" $codes \
      > "$LOG/locale-smoke.log" 2>&1 || true
    echo "locale-smoke $(( $(date +%s) - tl ))" >> "$LOG/times.txt"
  fi

  for n in ${P_SKIPPED[@]+"${P_SKIPPED[@]}"}; do
    echo "$n skipped 0" >> "$LOG/final.txt"
  done
  echo "total $(( $(date +%s) - t0 ))" >> "$LOG/times.txt"

  # A run inside maestro-locale-smoke.sh reports into its parent's file too.
  if [ -n "${MAESTRO_RUN_FINAL:-}" ]; then
    awk -v l="${LOCALE:-}" '$2 != "skipped" { print $1 (l != "" ? "[" l "]" : ""), $2, $3 }' \
      "$LOG/final.txt" >> "$MAESTRO_RUN_FINAL"
  fi
}

# ── run the devices ────────────────────────────────────────────────────────
is_booted() {
  case "$1" in
    ios|ipad) xcrun simctl list devices booted 2>/dev/null | grep -F "    $(device_name "$1") (" >/dev/null ;;
    android) adb devices 2>/dev/null | grep '^emulator-' >/dev/null ;;
  esac
}
headroom_ok() {
  local kind=ios
  if [ -n "${NO_HEADROOM:-}" ] || [ ! -x "$HEADROOM" ]; then
    return 0
  fi
  if [ "$1" = "android" ]; then
    kind=android
  fi
  "$HEADROOM" "$kind"
}
running_jobs() {
  local i c=0
  for i in "${!PIDS[@]}"; do
    if kill -0 "${PIDS[$i]}" 2>/dev/null; then
      c=$((c + 1))
    fi
  done
  echo "$c"
}

T_START="$(date +%s)"
echo "==> $TIER on ${DEVICES[*]}: ${#SELECTED[@]} flow(s): ${SELECTED[*]}"
if [ -n "${DRY_RUN:-}" ]; then
  # The plan per device, nothing booted or run.
  for dev in "${DEVICES[@]}"; do
    plan_for "$dev"
    echo "  $dev: fresh [${P_FRESH[*]:-}]${P_PEER[*]:+ peer} setup [${P_SHARED[*]:-}] peer [${P_PEER[*]:-}]${P_LOCALE:+ locale-smoke [$LOCALES]}${P_SKIPPED[*]:+ skipped [${P_SKIPPED[*]}]}"
  done
  exit 0
fi
case " ${DEVICES[*]} " in
  *" ios "*|*" ipad "*)
    # Xcode 27: without this the XCTest driver never starts (README). Once,
    # before the devices start: it rewrites a jar every iOS run shares.
    "$HERE/scripts/maestro-ios-driver-fix.sh" || true ;;
esac

PIDS=()
PDEVS=()
for dev in "${DEVICES[@]}"; do
  if ! is_booted "$dev"; then
    # Booting one more device only on the memory check's GO; with others
    # running, wait for one of them to finish and ask again.
    while ! headroom_ok "$dev"; do
      if [ "$(running_jobs)" -eq 0 ]; then
        echo "WARN: device-headroom says NO-GO for $dev with nothing else running; running it alone" >&2
        break
      fi
      echo "==> $dev waits for memory headroom"
      sleep 30
    done
  fi
  DLOG="$MAE/artifacts/$DATE/$dev/logs/$STAMP"
  mkdir -p "$DLOG"
  ( DEVICE_LABEL="$dev"; run_device "$dev" ) > "$DLOG/run.log" 2>&1 &
  PIDS+=("$!")
  PDEVS+=("$dev")
  echo "==> $dev started (log: ${DLOG#"$REPO"/}/run.log)"
done
for i in "${!PIDS[@]}"; do
  if ! wait "${PIDS[$i]}"; then
    echo "!! ${PDEVS[$i]}: the runner failed, see its run.log" >&2
  fi
done
T_END="$(date +%s)"

# ── report ─────────────────────────────────────────────────────────────────
FAIL_ANY=""
echo
echo "==> $TIER: $(fmt_secs $((T_END - T_START))) wall clock on ${DEVICES[*]}"
for dev in "${DEVICES[@]}"; do
  L="$MAE/artifacts/$DATE/$dev/logs/$STAMP"
  if [ ! -s "$L/final.txt" ]; then
    echo "  $dev: no results (see ${L#"$REPO"/}/run.log)"
    FAIL_ANY=1
    continue
  fi
  tot="$(awk '$1 == "total" { print $2 }' "$L/times.txt")"
  bat="$(awk '$1 == "batch" { print $2 }' "$L/times.txt")"
  pass="$(awk '$2 == "pass" { c++ } END { print c + 0 }' "$L/final.txt")"
  flake="$(awk '$2 == "flake" { printf "%s ", $1 }' "$L/final.txt")"
  fail="$(awk '$2 == "fail" { printf "%s ", $1 }' "$L/final.txt")"
  printf '  %-8s %s (first pass %s)  pass %s  flake %s  fail %s\n' "$dev" \
    "$(fmt_secs "${tot:-0}")" "$(fmt_secs "${bat:-0}")" "$pass" \
    "$(echo "$flake" | wc -w | tr -d ' ')${flake:+ ( $flake)}" \
    "$(echo "$fail" | wc -w | tr -d ' ')${fail:+ ( $fail)}"
  if [ -n "$fail" ]; then
    FAIL_ANY=1
  fi
  echo "           screenshots ${MAE#"$REPO"/}/artifacts/$DATE/$dev/  per-flow results ${L#"$REPO"/}/final.txt"
done
if [ "$TIER" = "all" ]; then
  skipped=""
  for n in $(all_flow_names); do
    if has_tag "$n" two-client; then
      skipped="$skipped $n"
    fi
  done
  if [ -n "$skipped" ]; then
    echo "==> two-client flows run by their own scripts (README \"Two-client flows\"):$skipped"
  fi
fi
if [ -n "$FAIL_ANY" ]; then
  exit 1
fi
