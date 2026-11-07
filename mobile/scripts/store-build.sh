#!/usr/bin/env bash
# Store build of the mobile app, the way mobile/CLAUDE.md → "Store builds" does it,
# from a commit on main in its own worktree, then the binary checks. Uploads nothing.
#
#   mobile/scripts/store-build.sh ios|android [ref]        (ref: default origin/main)
#
# Env: POLLIS_BUILD (re-upload of an unchanged version: 1, 2, …), STORE_BUILD_LOGS
# (log directory; default a fresh temp dir). Prod EXPO_PUBLIC_* values come from
# Doppler prd_prod, never from a .env file, and are never printed.
#
# On success the last lines are `artifact=<path>`, `version=`, `build_number=`,
# `commit=` and `runtime_version=` (the baked fingerprint for the ledger).
set -uo pipefail

PLATFORM="${1:?usage: store-build.sh ios|android [ref]}"
REF="${2:-origin/main}"
case "$PLATFORM" in ios|android) ;; *) echo "usage: store-build.sh ios|android [ref]" >&2; exit 1;; esac
REPO="$(cd "$(dirname "$0")/../.." && pwd)"
W="$REPO/.claude/worktrees/store-$PLATFORM"
L="${STORE_BUILD_LOGS:-$(mktemp -d)}"
mkdir -p "$L"
fail() { echo "FAILED: $1 (logs: $L)" >&2; exit 1; }
check() { if [ "$2" = "$3" ]; then echo "  ok  $1"; else fail "$1: got '$2', want '$3'"; fi; }

cd "$REPO" && git fetch -q origin || fail "git fetch"
[ -d "$W" ] || git worktree add -q --detach "$W" "$REF" || fail "worktree add"
cd "$W" && git checkout -q --detach "$REF" || fail "checkout $REF"
COMMIT="$(git rev-parse HEAD)"
VERSION="$(jq -r .version mobile/package.json)"
# A dev .env would inline api-dev or the dev gate key into the bundle.
[ ! -e mobile/.env ] || fail "mobile/.env exists in $W; remove it"

unset POLLIS_RELEASE_KEY POLLIS_IOS_NSE POLLIS_ABI_SPLITS EXPO_PUBLIC_POLLIS_DEV_ACCESS_KEY \
  EXPO_PUBLIC_LIVEKIT_API_KEY EXPO_PUBLIC_LIVEKIT_API_SECRET EXPO_PUBLIC_RESEND_API_KEY \
  EXPO_PUBLIC_R2_ACCESS_KEY_ID EXPO_PUBLIC_R2_SECRET_KEY EXPO_PUBLIC_TURSO_TOKEN
export POLLIS_OTA=production POLLIS_BUILD="${POLLIS_BUILD:-0}"
# Private Metro cache: an inlined EXPO_PUBLIC_* value is not in its cache key (1.0.22 shipped api-dev).
export TMPDIR="$(mktemp -d)"
export EXPO_PUBLIC_POLLIS_DELIVERY_URL=https://api.pollis.com
export EXPO_PUBLIC_LIVEKIT_URL="$(doppler secrets get LIVEKIT_URL -p pollis -c prd_prod --plain)"
export EXPO_PUBLIC_R2_ENDPOINT="$(doppler secrets get R2_S3_ENDPOINT -p pollis -c prd_prod --plain)"
export EXPO_PUBLIC_R2_PUBLIC_URL="$(doppler secrets get R2_PUBLIC_URL -p pollis -c prd_prod --plain)"
for v in EXPO_PUBLIC_LIVEKIT_URL EXPO_PUBLIC_R2_ENDPOINT EXPO_PUBLIC_R2_PUBLIC_URL; do
  [ -n "${!v}" ] || fail "missing $v in Doppler prd_prod"
done
export CARGO_INCREMENTAL=0 EXPO_NO_TELEMETRY=1 CI=1
CODE=$(node -e "const [a,b,c]=process.argv[1].split('.').map(Number);console.log(((a*100+b)*100+c)*100+Number(process.argv[2]))" "$VERSION" "$POLLIS_BUILD")
echo "store build: $PLATFORM $VERSION ($CODE) from ${COMMIT:0:8}; logs in $L"

cd "$W/mobile"
npx -y pnpm@10.25.0 install --ignore-workspace --frozen-lockfile > "$L/pnpm.log" 2>&1 || fail "pnpm install"

if [ "$PLATFORM" = ios ]; then
  KID="$(doppler secrets get ASC_KEY_ID -p pollis -c prd_prod --plain)"
  ISS="$(doppler secrets get ASC_ISSUER_ID -p pollis -c prd_prod --plain)"
  KEY="$HOME/.appstoreconnect/private_keys/AuthKey_$KID.p8"
  [ -f "$KEY" ] || fail "no ASC key at $KEY (decode ASC_KEY_P8_BASE64 there, chmod 600)"

  echo "== Rust core (device slice)"
  (cd modules/pollis-native && CARGO_TARGET_DIR="$W/target-ios" uniffi-bindgen-react-native build ios \
    --config ubrn.config.yaml --and-generate --release --no-sim) > "$L/ubrn.log" 2>&1 || fail "ubrn build ios"
  echo "== prebuild + pods"
  ./node_modules/.bin/expo prebuild -p ios --no-install > "$L/prebuild.log" 2>&1 || fail "expo prebuild"
  for f in ios/Pollis/Pollis.entitlements ios/PollisNotificationService/PollisNotificationService.entitlements; do
    [ -f "$f" ] || fail "missing $f"
    grep -q group.com.pollis.ios "$f" && grep -q 8JT3WH45NZ.com.pollis.shared "$f" || fail "$f lacks the app group / keychain group"
  done
  grep -q usernotifications.communication ios/Pollis/Pollis.entitlements || fail "app lacks the communication entitlement"
  (cd ios && pod install) > "$L/pod.log" 2>&1 || fail "pod install"
  echo "== archive (unsigned), entitle, export"
  rm -rf build/Pollis.xcarchive build/export
  xcodebuild -workspace ios/Pollis.xcworkspace -scheme Pollis -configuration Release \
    -destination 'generic/platform=iOS' archive -archivePath build/Pollis.xcarchive \
    DEVELOPMENT_TEAM=8JT3WH45NZ CODE_SIGNING_ALLOWED=NO > "$L/archive.log" 2>&1 || fail "xcodebuild archive"
  APP=build/Pollis.xcarchive/Products/Applications/Pollis.app
  [ -d "$APP/PlugIns/PollisNotificationService.appex" ] || fail "notification service extension not embedded"
  # Inner bundle first: signing the app seals PlugIns/. Without these, push is silently lost.
  codesign -f -s - --entitlements ios/PollisNotificationService/PollisNotificationService.entitlements \
    "$APP/PlugIns/PollisNotificationService.appex" || fail "codesign appex"
  codesign -f -s - --entitlements ios/Pollis/Pollis.entitlements "$APP" || fail "codesign app"
  xcodebuild -exportArchive -archivePath build/Pollis.xcarchive -exportOptionsPlist store/ExportOptions.plist \
    -exportPath build/export -allowProvisioningUpdates -authenticationKeyPath "$KEY" \
    -authenticationKeyID "$KID" -authenticationKeyIssuerID "$ISS" > "$L/export.log" 2>&1 || fail "export"
  ART="$W/mobile/build/export/Pollis.ipa"

  echo "== checks"
  D="$L/ipa"; rm -rf "$D"; mkdir -p "$D"; unzip -q "$ART" -d "$D"
  A="$D/Payload/Pollis.app"; X="$A/PlugIns/PollisNotificationService.appex"
  codesign --verify --deep --strict "$A" || fail "codesign --verify --deep"
  for b in "$A" "$X"; do
    n="$(basename "$b")"
    check "$n signed by Pollis Inc." "$(codesign -dvvv "$b" 2>&1 | grep -c '^Authority=Apple Distribution: Pollis Inc. (8JT3WH45NZ)')" 1
    ENT="$(codesign -d --entitlements :- "$b" 2>/dev/null)"
    check "$n get-task-allow false" "$(echo "$ENT" | plutil -extract get-task-allow raw - 2>/dev/null)" false
    check "$n version" "$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$b/Info.plist")" "$VERSION"
    check "$n build number" "$(/usr/libexec/PlistBuddy -c 'Print :CFBundleVersion' "$b/Info.plist")" "$CODE"
  done
  check "aps-environment" "$(codesign -d --entitlements :- "$A" 2>/dev/null | plutil -extract aps-environment raw -)" production
  check "bundle names api.pollis.com" "$(LC_ALL=C grep -ac 'https://api.pollis.com' "$A/main.jsbundle" | awk '{print ($1>0)}')" 1
  check "bundle never names api-dev" "$(LC_ALL=C grep -ac 'api-dev' "$A/main.jsbundle")" 0
  check "install channel" "$(jq -r .extra.installChannel "$A/EXConstants.bundle/app.config")" store
  RV="$(cat "$A/EXUpdates.bundle/fingerprint")"
else
  # android-env.sh is the Mac's SDK layout; on Linux the environment already points at the SDK.
  if [ "$(uname -s)" = Darwin ]; then
    . ./android-env.sh
  fi
  echo "== Rust core (3 ABIs)"
  (cd modules/pollis-native && CARGO_TARGET_DIR="$W/target-android" uniffi-bindgen-react-native build android \
    --config ubrn.config.yaml --and-generate --release) > "$L/ubrn.log" 2>&1 || fail "ubrn build android"
  echo "== prebuild + bundleRelease (Play upload key)"
  ./node_modules/.bin/expo prebuild -p android --no-install > "$L/prebuild.log" 2>&1 || fail "expo prebuild"
  export POLLIS_UPLOAD_STORE_FILE="$HOME/.pollis/pollis-upload.jks"
  if [ ! -f "$POLLIS_UPLOAD_STORE_FILE" ]; then
    # Not on this machine: decode Doppler's copy to a private temp file, deleted on exit.
    POLLIS_UPLOAD_STORE_FILE="$(mktemp)"
    trap 'rm -f "$POLLIS_UPLOAD_STORE_FILE"' EXIT
    chmod 600 "$POLLIS_UPLOAD_STORE_FILE"
    doppler secrets get POLLIS_UPLOAD_KEYSTORE_B64 -p pollis -c prd_prod --plain | base64 -d > "$POLLIS_UPLOAD_STORE_FILE" \
      || fail "decode POLLIS_UPLOAD_KEYSTORE_B64"
  fi
  export POLLIS_UPLOAD_STORE_PASSWORD="$(doppler secrets get POLLIS_UPLOAD_STORE_PASSWORD -p pollis -c prd_prod --plain)"
  export POLLIS_UPLOAD_KEY_ALIAS="$(doppler secrets get POLLIS_UPLOAD_KEY_ALIAS -p pollis -c prd_prod --plain)"
  export POLLIS_UPLOAD_KEY_PASSWORD="$(doppler secrets get POLLIS_UPLOAD_KEY_PASSWORD -p pollis -c prd_prod --plain)"
  [ -f "$POLLIS_UPLOAD_STORE_FILE" ] || fail "no upload keystore at $POLLIS_UPLOAD_STORE_FILE"
  for v in POLLIS_UPLOAD_STORE_PASSWORD POLLIS_UPLOAD_KEY_ALIAS POLLIS_UPLOAD_KEY_PASSWORD; do
    [ -n "${!v}" ] || fail "missing $v in Doppler prd_prod"
  done
  # The template's 2 GB heap runs out of Metaspace on three ABIs.
  (cd android && ./gradlew :app:bundleRelease "-Dorg.gradle.jvmargs=-Xmx4096m -XX:MaxMetaspaceSize=2g") > "$L/gradle.log" 2>&1 \
    || fail "gradle bundleRelease"
  ART="$W/mobile/android/app/build/outputs/bundle/release/app-release.aab"

  echo "== checks"
  check "AAB signed" "$(jarsigner -verify "$ART" 2>&1 | grep -c 'jar verified')" 1
  check "native ABIs" "$(unzip -l "$ART" | grep -o 'base/lib/[a-z0-9_-]*' | sort -u | tr '\n' ' ')" \
    "base/lib/arm64-v8a base/lib/armeabi-v7a base/lib/x86_64 "
  T="$L/aab"; rm -rf "$T"; mkdir -p "$T"
  unzip -q -o "$ART" base/assets/index.android.bundle base/assets/app.config base/assets/fingerprint -d "$T"
  # Hermes bytecode: count raw bytes, `strings` can miss them.
  check "bundle names api.pollis.com" "$(LC_ALL=C grep -ac 'https://api.pollis.com' "$T/base/assets/index.android.bundle" | awk '{print ($1>0)}')" 1
  check "bundle never names api-dev" "$(LC_ALL=C grep -ac 'api-dev' "$T/base/assets/index.android.bundle")" 0
  check "install channel" "$(jq -r .extra.installChannel "$T/base/assets/app.config")" store
  check "version" "$(jq -r .version "$T/base/assets/app.config")" "$VERSION"
  check "versionCode" "$(jq -r .android.versionCode "$T/base/assets/app.config")" "$CODE"
  RV="$(cat "$T/base/assets/fingerprint")"
fi

echo "artifact=$ART"
echo "version=$VERSION"
echo "build_number=$CODE"
echo "commit=$COMMIT"
echo "runtime_version=$RV"
