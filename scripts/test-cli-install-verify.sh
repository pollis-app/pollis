#!/usr/bin/env bash
#
# test-cli-install-verify.sh — tests for cli-install.sh's attestation match.
#
# Since #1337 cli-install.sh refuses an unverified binary by default. Without
# cosign, the check is GitHub's attestation store: the download's SHA-256 must
# have a SLSA provenance attestation whose subject is that file name with that
# digest and whose builder is this repository's cli-release.yml. Every case
# below runs the REAL function, loaded from cli-install.sh between its
# BEGIN/END markers, against an API response in the shape GitHub serves (the
# statement is the compact JSON actions/attest-build-provenance emits, base64'd
# into the DSSE envelope).

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
INSTALL="${INSTALL_OVERRIDE:-${ROOT}/website/cli-install.sh}"

fn="$(sed -n '/^# BEGIN attestation_matches/,/^# END attestation_matches/p' "$INSTALL")"
if [[ -z "$fn" ]]; then
    echo "FAIL: attestation_matches markers not found in website/cli-install.sh"
    exit 1
fi
eval "$fn"

LINUX="633d7cafa4793028be2db4f29328daecbb858ac62a42f18684a24c9a61c25ec8"
MACOS="b7d619bdee486db0dafc5bd76f312322d13fb90c37a8efdc2b375b183db8158f"
OTHER="31d4e41bc66b4762d64819c51945d2617cb1a9238ff20a7837612c1ba69c9505"
REPO="https://github.com/actuallydan/pollis"
WORKFLOW=".github/workflows/cli-release.yml"

b64() {
    printf '%s' "$1" | base64 | tr -d '\n'
}

# One in-toto statement, field order as attest-build-provenance writes it.
statement() {
    local subjects=$1 repo=$2 path=$3 predicate=${4:-https://slsa.dev/provenance/v1}
    printf '{"_type":"https://in-toto.io/Statement/v1","subject":[%s],"predicateType":"%s","predicate":{"buildDefinition":{"buildType":"https://actions.github.io/buildtypes/workflow/v1","externalParameters":{"workflow":{"ref":"refs/tags/v1.17.1","repository":"%s","path":"%s"}}}}}' \
        "$subjects" "$predicate" "$repo" "$path"
}

subject() {
    printf '{"name":"%s","digest":{"sha256":"%s"}}' "$1" "$2"
}

# The API response: pretty-printed outer JSON, one attestation per statement.
response() {
    local out='{\n  "attestations": [' first=1 s
    for s in "$@"; do
        if [[ $first -eq 0 ]]; then
            out+=','
        fi
        first=0
        out+="\n    {\n      \"repository_id\": 1119329968,\n      \"bundle\": {\"mediaType\":\"application/vnd.dev.sigstore.bundle.v0.3+json\",\"dsseEnvelope\":{\"payload\":\"$(b64 "$s")\",\"payloadType\":\"application/vnd.in-toto+json\"}}\n    }"
    done
    out+='\n  ]\n}'
    printf '%b' "$out"
}

pass=0
fail=0

expect() {
    local want=$1 label=$2 json=$3 name=$4 sha=$5
    local got=no
    if attestation_matches "$json" "$name" "$sha"; then
        got=yes
    fi
    if [[ "$got" == "$want" ]]; then
        pass=$((pass + 1))
    else
        fail=$((fail + 1))
        echo "FAIL: ${label}: expected match=${want}, got ${got}"
    fi
}

all="$(subject pollis-linux "$LINUX"),$(subject pollis-macos "$MACOS"),$(subject pollis-windows.exe "$OTHER")"
genuine="$(response "$(statement "$all" "$REPO" "$WORKFLOW")")"

expect yes "genuine macOS binary (the live v1.17.1 shape)" "$genuine" pollis-macos "$MACOS"
expect yes "genuine linux binary from the same statement" "$genuine" pollis-linux "$LINUX"

# The digest of one platform's binary under another platform's name: a real
# attested binary, but not the file the user asked for.
expect no "linux digest presented as pollis-macos" "$genuine" pollis-macos "$LINUX"
expect no "digest no subject carries" "$genuine" pollis-macos "0000000000000000000000000000000000000000000000000000000000000000"

# Same subject, built by something else.
expect no "another repository's workflow" \
    "$(response "$(statement "$all" "https://github.com/mallory/pollis" "$WORKFLOW")")" pollis-macos "$MACOS"
expect no "another workflow in this repository" \
    "$(response "$(statement "$all" "$REPO" ".github/workflows/ci.yml")")" pollis-macos "$MACOS"
expect no "not a SLSA provenance statement" \
    "$(response "$(statement "$all" "$REPO" "$WORKFLOW" "https://example.com/other/v1")")" pollis-macos "$MACOS"

# A prefix of the published name must not match (pollis-mac vs pollis-macos).
expect no "a name that is only a prefix" "$genuine" pollis-mac "$MACOS"

# Several attestations: one unrelated, one genuine — the genuine one counts.
expect yes "genuine attestation after an unrelated one" \
    "$(response "$(statement "$(subject other "$OTHER")" "$REPO" ".github/workflows/ci.yml")" "$(statement "$all" "$REPO" "$WORKFLOW")")" \
    pollis-macos "$MACOS"

# Nothing usable at all.
expect no "empty attestation list" '{"attestations":[]}' pollis-macos "$MACOS"
expect no "not JSON" 'rate limit exceeded' pollis-macos "$MACOS"
expect no "payload that is not base64" '{"attestations":[{"bundle":{"dsseEnvelope":{"payload":"!!!"}}}]}' pollis-macos "$MACOS"

echo "attestation_matches: ${pass} passed, ${fail} failed"
[[ $fail -eq 0 ]]
