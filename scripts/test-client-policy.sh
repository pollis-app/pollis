#!/usr/bin/env bash
# Exercise scripts/client-policy.mjs (#1611) offline: the CDN and the release manifests are local
# directories, the validator is the real pollis-api example, and `aws` is a stub that copies the
# file into the fake CDN, so --apply is tested end to end without touching R2.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

cargo build -q -p pollis-api --example client-policy
export CLIENT_POLICY_VALIDATOR="$ROOT/target/debug/examples/client-policy"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
mkdir -p "$WORK/cdn/dev" "$WORK/releases/cli" "$WORK/releases/android" "$WORK/bin"
echo '{"version":"v1.17.21"}' > "$WORK/releases/latest.json"
echo '{"version":"1.17.21"}' > "$WORK/releases/cli/latest.json"
echo '{"version":"1.0.23"}' > "$WORK/releases/android/latest.json"
export CLIENT_POLICY_CDN_BASE="$WORK/cdn"
export CLIENT_POLICY_RELEASES_BASE="$WORK/releases"

# `aws s3 cp <file> s3://pollis/<key> …` → copy into the fake CDN, and log the call.
cat > "$WORK/bin/aws" <<EOF
#!/usr/bin/env bash
echo "\$@" >> "$WORK/aws.log"
key="\${4#s3://pollis/}"
mkdir -p "\$(dirname "$WORK/cdn/\$key")"
cp "\$3" "$WORK/cdn/\$key"
EOF
chmod +x "$WORK/bin/aws"
export PATH="$WORK/bin:$PATH"
export R2_ACCESS_KEY_ID=test R2_SECRET_KEY=test CLOUDFLARE_ACCOUNT_ID=acct

pass=0
fail() { echo "FAIL: $*" >&2; exit 1; }
ok() { pass=$((pass + 1)); echo "ok - $*"; }
run() { node scripts/client-policy.mjs "$@" </dev/null; }

out="$(run --env dev)"
grep -q "published: nothing yet" <<<"$out" || fail "an unpublished policy is reported as such"
grep -q "dry run" <<<"$out" || fail "the default is a dry run"
[ ! -e "$WORK/cdn/dev/client-policy.json" ] || fail "a dry run uploaded"
ok "nothing published: dry run, nothing uploaded"

out="$(run --env prod --set macos=1.17.0 --set cli=1.17.0)"
grep -q '\* macos .* - .*→ 1.17.0' <<<"$out" || fail "the diff names the change: $out"
grep -q '  ios .* - .*→ -' <<<"$out" || fail "unchanged platforms are listed"
ok "dry run shows a per-platform diff"

if run --env prod --set andriod-play=1.0.0 2>"$WORK/err"; then fail "a typo platform was accepted"; fi
grep -q "unknown platform" "$WORK/err" || fail "typo message"
ok "an unknown platform is refused"

if run --env prod --set macos=1.17 2>"$WORK/err"; then fail "a short version was accepted"; fi
grep -q "not a MAJOR.MINOR.PATCH" "$WORK/err" || fail "bad version message: $(cat "$WORK/err")"
ok "a malformed version is refused by the shared parser"

if run --env prod --set linux=1.18.0 >"$WORK/out" 2>"$WORK/err"; then fail "a floor above the latest release was accepted"; fi
grep -q "above the latest release (1.17.21)" "$WORK/out" || fail "above-latest message"
ok "a minimum above the latest release is refused"
run --env prod --set linux=1.18.0 --force >/dev/null || fail "--force overrides the release guard"
ok "--force overrides the release guard (still a dry run)"

out="$(run --env prod --set ios=1.0.21 --set android-play=1.0.21)"
grep -q "ios: bounded by the sideload APK (1.0.23)" <<<"$out" || fail "store platforms are bounded by the APK: $out"
ok "ios and android-play are bounded by the sideload APK's version, with a warning"
if run --env prod --set ios=1.0.24 >"$WORK/out" 2>&1; then fail "an iOS minimum above the mobile release was accepted"; fi
grep -q "ios: 1.0.24 is above the latest release (1.0.23)" "$WORK/out" || fail "ios above-latest: $(cat "$WORK/out")"
ok "an iOS minimum above the latest mobile release is refused"

mv "$WORK/releases/cli/latest.json" "$WORK/releases/cli/latest.json.bak"
if run --env prod --set cli=1.0.0 >"$WORK/out" 2>&1; then fail "an unreadable release manifest was accepted"; fi
grep -q "cli: the release manifest for cli could not be read" "$WORK/out" || fail "unreadable manifest: $(cat "$WORK/out")"
run --env prod --set cli=1.0.0 --force >/dev/null || fail "--force overrides an unreadable manifest"
mv "$WORK/releases/cli/latest.json.bak" "$WORK/releases/cli/latest.json"
ok "an unreadable release manifest fails closed unless --force"

out="$(run --env dev --set macos=1.2.0)"
grep -q "macos: dev desktop/CLI builds report the unstamped 1.1.0" <<<"$out" || fail "dev placeholder warning: $out"
ok "a dev desktop/CLI minimum above the placeholder warns"

if run --env dev --set macos=1.0.0 --apply 2>"$WORK/err"; then fail "--apply without a terminal or --yes"; fi
grep -q "needs a terminal" "$WORK/err" || fail "confirm message"
[ ! -e "$WORK/cdn/dev/client-policy.json" ] || fail "an unconfirmed apply uploaded"
ok "--apply asks for confirmation"

out="$(run --env dev --apply --yes)"
grep -q "published and verified" <<<"$out" || fail "empty dev policy publish: $out"
diff <(printf '{\n  "v": 1,\n  "minimum": {},\n  "message": null\n}\n') "$WORK/cdn/dev/client-policy.json" || fail "canonical empty policy"
grep -q "s3://pollis/dev/client-policy.json --endpoint-url https://acct.r2.cloudflarestorage.com --content-type application/json --cache-control no-cache" "$WORK/aws.log" || fail "aws call: $(cat "$WORK/aws.log")"
ok "--apply --yes uploads the canonical file to the dev key, like latest.json"

out="$(run --env dev --apply --yes)"
grep -q "no change" <<<"$out" || fail "a no-op publish"
[ "$(wc -l < "$WORK/aws.log")" -eq 1 ] || fail "a no-op uploaded again"
ok "no change: nothing uploaded"

run --env dev --set macos=1.17.0 --message "Please update." --apply --yes >/dev/null
out="$(run --env dev --unset macos --clear-message --out "$WORK/next.json")"
grep -q '\* macos .*1.17.0 .*→ -' <<<"$out" || fail "unset diff: $out"
grep -q '\* message  "Please update." → null' <<<"$out" || fail "message diff: $out"
grep -q '"minimum": {}' "$WORK/next.json" || fail "--out"
ok "unset and clear-message diff against what is published"

echo '{"v":1,"minimum":{"macos":"soon"}}' > "$WORK/cdn/client-policy.json"
if run --env prod >"$WORK/out" 2>&1; then fail "an invalid published file was built on"; fi
grep -q "the published file is invalid" "$WORK/out" || fail "invalid published message: $(cat "$WORK/out")"
out="$(run --env prod --force)"
grep -q "the published file is invalid" <<<"$out" || fail "--force still reports it: $out"
ok "an invalid published file is refused unless --force"

echo "all $pass passed"
