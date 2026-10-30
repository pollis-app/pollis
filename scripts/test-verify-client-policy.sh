#!/usr/bin/env bash
# Exercise scripts/verify-client-policy.sh (#1624) offline: `curl` is a stub that answers a
# scripted sequence of status codes (the last one repeats), so the window logic is tested without
# a deploy. The check must ride out a restart, and must still fail on a 404 that never clears.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
mkdir -p "$WORK/bin"

# Prints the next code from $WORK/codes (one per line; the last repeats) and logs the arguments.
cat > "$WORK/bin/curl" <<EOF
#!/usr/bin/env bash
echo "\$*" >> "$WORK/curl.log"
n=\$(cat "$WORK/n" 2>/dev/null || echo 0)
n=\$((n + 1))
echo "\$n" > "$WORK/n"
total=\$(wc -l < "$WORK/codes")
line=\$(( n < total ? n : total ))
code=\$(sed -n "\${line}p" "$WORK/codes")
if [ "\$code" = 000 ]; then
  printf 000
  exit 28
fi
printf '%s' "\$code"
EOF
chmod +x "$WORK/bin/curl"
export PATH="$WORK/bin:$PATH"
export INTERVAL_SECONDS=0 CURL_TIMEOUT_SECONDS=1

pass=0
fail() { echo "FAIL: $*" >&2; exit 1; }
ok() { pass=$((pass + 1)); echo "ok - $*"; }
# run <window-seconds> <codes…>: the script's output in $WORK/out, its exit status echoed.
run() {
  local window="$1"
  shift
  printf '%s\n' "$@" > "$WORK/codes"
  rm -f "$WORK/n" "$WORK/curl.log"
  local rc=0
  WINDOW_SECONDS="$window" "$ROOT/scripts/verify-client-policy.sh" https://ds.test/ > "$WORK/out" 2>&1 || rc=$?
  echo "$rc"
}

[ "$(run 5 200)" = 0 ] || fail "a first 200 passes"
grep -q "answers 200" "$WORK/out" || fail "the pass names the code: $(cat "$WORK/out")"
grep -q "https://ds.test/v1/client-policy" "$WORK/curl.log" || fail "the URL is base + /v1/client-policy"
ok "200 passes at once"

[ "$(run 5 503)" = 0 ] || fail "503 (configured, first fetch pending) passes"
ok "503 passes"

# The stale container answers 404 until a request finds it, then the restart blocks (000).
[ "$(run 5 404 404 404 000 000 200)" = 0 ] || fail "a 404 cleared by the restart passes: $(cat "$WORK/out")"
[ "$(wc -l < "$WORK/curl.log" | tr -d ' ')" = 6 ] || fail "it polled until the 200"
ok "rides out the env restart (404, then timeouts, then 200)"

# Not vacuous: a persistent 404 fails once the window has passed.
[ "$(run 2 404)" = 1 ] || fail "a persistent 404 must fail"
grep -q "::error::/v1/client-policy still answers 404" "$WORK/out" || fail "names the 404: $(cat "$WORK/out")"
ok "a persistent 404 fails after the window"

[ "$(run 2 000)" = 1 ] || fail "a DS that never answers must fail"
grep -q "::error::/v1/client-policy answers 000" "$WORK/out" || fail "names the timeout: $(cat "$WORK/out")"
ok "a DS that never comes back fails"

[ "$(run 2 500)" = 1 ] || fail "a persistent 500 must fail"
ok "any other persistent status fails"

grep -q -- "-H" "$WORK/curl.log" && fail "no dev-key header without DEV_KEY"
DEV_KEY=k run 5 200 > /dev/null
grep -q "X-Pollis-Dev-Key: k" "$WORK/curl.log" || fail "DEV_KEY sends the dev gate header"
ok "DEV_KEY adds the dev gate header, and only then"

echo "$pass passed"
