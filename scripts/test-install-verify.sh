#!/usr/bin/env bash
#
# test-install-verify.sh — tests for install.sh's transparency-log match.
#
# install.sh refuses a download unless the release report served at
# verify.pollis.com/verify/release/<tag> holds one artifact record with that file
# name, that sha256, and "included":true. The match once split the report on `{`,
# which also cut every record at its nested "toolchain" object (#944), so a
# genuine v1.14.1 download was deleted as tampered. Every case below runs the
# REAL function, loaded from install.sh between its BEGIN/END markers, against a
# report in the exact compact shape ReleaseReport serializes to.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
INSTALL="${INSTALL_OVERRIDE:-${ROOT}/website/install.sh}"

fn="$(sed -n '/^# BEGIN report_has_artifact/,/^# END report_has_artifact/p' "$INSTALL")"
if [[ -z "$fn" ]]; then
    echo "FAIL: report_has_artifact markers not found in website/install.sh"
    exit 1
fi
eval "$fn"

# One artifact record, field order as ReleaseArtifact serializes it.
record() {
    local name=$1 layer=$2 sha=$3 included=$4
    printf '{"platform":"darwin","arch":"aarch64","bundle":"dmg","layer":"%s","artifact_name":"%s","payload_sha256":"ae1670e5","artifact_sha256":"%s","provenance_uri":"cdn.pollis.com/releases/v1.14.1/%s.intoto.jsonl","toolchain":{"rustc":"1.96.0","node":"20.20.2","pnpm":"10.25.0","runner_image":"macos26@20260907.0351.1","source_date_epoch":1790876435},"included":%s}' \
        "$layer" "$name" "$sha" "$name" "$included"
}

report() {
    printf '{"release_tag":"v1.14.1","found":true,"sth_tree_size":423,"root_hex":"ab12","artifacts":[%s],"chain_valid":true,"violations":[]}' "$1"
}

DMG="pollis-v1.14.1-macos.dmg"
EXE="pollis-v1.14.1-windows.exe"
GOOD="b8faeb6d99f72b2eeb9fe41c80f16637ac5deabf9f91cc31658e729ec06d2fe2"
OTHER="31d4e41bc66b4762d64819c51945d2617cb1a9238ff20a7837612c1ba69c9505"

pass=0
fail=0

expect() {
    local want=$1 label=$2 json=$3 name=$4 sha=$5
    local got=no
    if report_has_artifact "$json" "$name" "$sha"; then
        got=yes
    fi
    if [[ "$got" == "$want" ]]; then
        pass=$((pass + 1))
    else
        fail=$((fail + 1))
        echo "FAIL: ${label}: expected match=${want}, got ${got}"
    fi
}

full="$(report "$(record "$DMG" payload ae1670e5 true),$(record "$DMG" signed "$GOOD" true),$(record "$EXE" signed "$OTHER" true)")"

expect yes "genuine signed dmg (the v1.14.1 false alarm)" "$full" "$DMG" "$GOOD"
expect no  "hash from another artifact's record" "$full" "$DMG" "$OTHER"
expect no  "hash the log does not record" "$full" "$DMG" "deadbeef"
expect no  "name the log does not record" "$full" "pollis-v9.9.9-macos.dmg" "$GOOD"

not_included="$(report "$(record "$DMG" signed "$GOOD" false)")"
expect no  "recorded but not included in the signed log" "$not_included" "$DMG" "$GOOD"

empty="$(report "")"
expect no  "report with no artifacts" "$empty" "$DMG" "$GOOD"

echo "${pass} passed, ${fail} failed"
if [[ $fail -ne 0 ]]; then
    exit 1
fi
