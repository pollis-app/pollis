#!/usr/bin/env bash
# Vault media soak (#1491) — ad hoc, never part of a CI test run.
#
# Treats the Vault's media mode as a file store and proves, per file, that it is
# encrypted in transit, at rest in object storage, in the DS's rows and on the
# local disk, that viewing leaves no plaintext behind, that a re-download is
# byte-identical, and that a delete really deletes. Report: $WORKDIR/report.md.
#
#   scripts/vault-soak.sh            # local: in-process DS + SeaweedFS in docker
#   scripts/vault-soak.sh dev        # api-dev + dev R2 (needs .env.development)
#
# Knobs (environment):
#   POLLIS_SOAK_COUNT      number of random files          (local 20, dev 4)
#   POLLIS_SOAK_MIN_SIZE   smallest, e.g. 512K             (local 1M, dev 256K)
#   POLLIS_SOAK_MAX_SIZE   largest, e.g. 2G                (local 2G, dev 8M)
#   POLLIS_SOAK_SIZES      explicit list instead, e.g. 1M,100M,600M,1G
#   POLLIS_SOAK_MEDIA_DIR  real media files to add (default: repo images + audio,
#                          plus a generated video/photo when ffmpeg is installed)
#   POLLIS_SOAK_NO_MEDIA=1 random files only
#   POLLIS_SOAK_SEED       reproduce a corpus
#   POLLIS_SOAK_WORKDIR    where the corpus, app data and report go
#   POLLIS_SOAK_KEEP=1     keep the corpus and leave the S3 container running
#   POLLIS_SOAK_S3_PORT    host port for SeaweedFS (default 18333)
#
# Files over the upload cap (pollis_api::broker::R2_MEDIA_MAX_BYTES) are part of
# the run on purpose: the soak proves they are refused before a byte leaves the
# device. Sizes are binary (1M = 1 MiB).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TARGET="${1:-local}"
case "$TARGET" in local|dev) ;; *) echo "usage: $0 [local|dev]" >&2; exit 2;; esac

export POLLIS_SOAK_TARGET="$TARGET"
if [ "$TARGET" = dev ]; then
  : "${POLLIS_SOAK_COUNT:=4}" "${POLLIS_SOAK_MIN_SIZE:=256K}" "${POLLIS_SOAK_MAX_SIZE:=8M}"
else
  : "${POLLIS_SOAK_COUNT:=20}" "${POLLIS_SOAK_MIN_SIZE:=1M}" "${POLLIS_SOAK_MAX_SIZE:=2G}"
fi
export POLLIS_SOAK_COUNT POLLIS_SOAK_MIN_SIZE POLLIS_SOAK_MAX_SIZE
export POLLIS_SOAK_WORKDIR="${POLLIS_SOAK_WORKDIR:-$ROOT/target/vault-soak/$(date -u +%Y%m%dT%H%M%SZ)-$TARGET}"
export POLLIS_SOAK_REPORT="${POLLIS_SOAK_REPORT:-$POLLIS_SOAK_WORKDIR/report.md}"
mkdir -p "$POLLIS_SOAK_WORKDIR"

# ── Real media ──────────────────────────────────────────────────────────────
if [ -z "${POLLIS_SOAK_MEDIA_DIR:-}" ] && [ "${POLLIS_SOAK_NO_MEDIA:-0}" != 1 ]; then
  MEDIA="$POLLIS_SOAK_WORKDIR/media"
  mkdir -p "$MEDIA"
  for f in readme/hero.png readme/new_app.png assets/sfx/call.wav; do
    [ -f "$ROOT/$f" ] && cp "$ROOT/$f" "$MEDIA/"
  done
  if command -v ffmpeg >/dev/null 2>&1; then
    # A real H.264 video (noise keeps it from compressing to nothing) and a JPEG.
    # ~50 MB locally (under the upload cap, over several AEAD chunks).
    if [ "$TARGET" = dev ]; then dur=1; else dur=6; fi
    ffmpeg -loglevel error -y -f lavfi -i "testsrc2=size=1280x720:rate=30:duration=$dur" \
      -vf "noise=alls=40:allf=t" -c:v libx264 -preset veryfast -pix_fmt yuv420p "$MEDIA/clip.mp4" || true
    ffmpeg -loglevel error -y -f lavfi -i "testsrc2=size=2048x1536:duration=1" \
      -vf "noise=alls=30" -frames:v 1 -q:v 3 "$MEDIA/photo.jpg" || true
  else
    echo "note: ffmpeg not found — no generated video/photo in the corpus" >&2
  fi
  export POLLIS_SOAK_MEDIA_DIR="$MEDIA"
fi

# ── Target ──────────────────────────────────────────────────────────────────
CONTAINER=pollis-vault-soak-s3
cleanup() {
  if [ "$TARGET" = local ] && [ "${POLLIS_SOAK_KEEP:-0}" != 1 ]; then
    docker rm -f "$CONTAINER" >/dev/null 2>&1 || true
  fi
}

if [ "$TARGET" = local ]; then
  command -v docker >/dev/null || { echo "docker is required for the local target" >&2; exit 1; }
  PORT="${POLLIS_SOAK_S3_PORT:-18333}"
  # Throwaway credentials for a throwaway store; never printed.
  ACCESS="soak$(od -An -N8 -tx1 /dev/urandom | tr -d ' \n')"
  SECRET="$(od -An -N24 -tx1 /dev/urandom | tr -d ' \n')"
  CONF="$POLLIS_SOAK_WORKDIR/s3.json"
  umask 077
  printf '{"identities":[{"name":"soak","credentials":[{"accessKey":"%s","secretKey":"%s"}],"actions":["Admin","Read","Write","List","Tagging"]}]}\n' \
    "$ACCESS" "$SECRET" > "$CONF"
  umask 022
  docker rm -f "$CONTAINER" >/dev/null 2>&1 || true
  trap cleanup EXIT
  # SeaweedFS (Apache-2.0): master + volume + filer + S3 gateway in one process.
  # The config is COPIED in rather than bind-mounted: a VM-backed Docker
  # (OrbStack, Docker Desktop) may be refused reads under ~/Documents by macOS,
  # and the S3 gateway then hangs at start instead of failing.
  docker create --name "$CONTAINER" -p "127.0.0.1:$PORT:8333" \
    chrislusf/seaweedfs:3.97 server -dir=/data -s3 -s3.port=8333 \
    -s3.config=/etc/seaweedfs/s3.json -master.volumePreallocate=false \
    -master.volumeSizeLimitMB=1024 -volume.max=0 >/dev/null
  docker cp "$CONF" "$CONTAINER:/etc/seaweedfs/s3.json" >/dev/null
  rm -f "$CONF"
  docker start "$CONTAINER" >/dev/null
  echo "==> waiting for SeaweedFS on 127.0.0.1:$PORT"
  weed_shell() {
    echo "$1" | docker exec -i "$CONTAINER" weed shell -master=localhost:9333 -filer=localhost:8888 2>&1
  }
  # The S3 gateway first: a `weed shell` session started while it is still
  # initialising holds the cluster's admin lock and stalls that start.
  for _ in $(seq 1 90); do
    [ "$(curl -m 2 -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$PORT/" || true)" = 403 ] && { s3up=1; break; }
    sleep 1
  done
  [ "${s3up:-0}" = 1 ] || { echo "SeaweedFS S3 gateway did not come up" >&2; docker logs "$CONTAINER" 2>&1 | tail -20 >&2; exit 1; }
  for _ in $(seq 1 30); do
    weed_shell "s3.bucket.create -name pollis-soak" >/dev/null || true
    if weed_shell "s3.bucket.list" | grep -q pollis-soak; then
      ready=1; break
    fi
    sleep 2
  done
  [ "${ready:-0}" = 1 ] || { echo "SeaweedFS did not come up" >&2; docker logs "$CONTAINER" | tail -20 >&2; exit 1; }
  export POLLIS_SOAK_S3_ENDPOINT="http://127.0.0.1:$PORT"
  export POLLIS_SOAK_S3_BUCKET=pollis-soak
  export POLLIS_SOAK_S3_ACCESS_KEY="$ACCESS" POLLIS_SOAK_S3_SECRET_KEY="$SECRET"
else
  # Dev secrets: the environment wins (CI), else .env.development from this
  # checkout or the main one (a worktree has none of its own). Never echoed.
  if [ -z "${R2_ACCESS_KEY_ID:-}" ]; then
    for env in "$ROOT/.env.development" "$(git -C "$ROOT" rev-parse --path-format=absolute --git-common-dir 2>/dev/null)/../.env.development"; do
      if [ -f "$env" ]; then
        set -a; # shellcheck disable=SC1090
        . "$env"; set +a
        break
      fi
    done
  fi
  : "${POLLIS_DELIVERY_URL:=https://api-dev.pollis.com}"
  export POLLIS_DELIVERY_URL
  case "$POLLIS_DELIVERY_URL" in
    *://api.pollis.com|*://api.pollis.com/) echo "refusing to soak the production DS" >&2; exit 1;;
  esac
  for v in DEV_OTP R2_S3_ENDPOINT R2_PUBLIC_URL R2_BUCKET R2_ACCESS_KEY_ID; do
    [ -n "${!v:-}" ] || { echo "dev target needs $v (from .env.development or the environment)" >&2; exit 1; }
  done
fi

echo "==> vault soak: target=$TARGET count=$POLLIS_SOAK_COUNT sizes=${POLLIS_SOAK_SIZES:-$POLLIS_SOAK_MIN_SIZE..$POLLIS_SOAK_MAX_SIZE}"
echo "    workdir $POLLIS_SOAK_WORKDIR"
cd "$ROOT/src-tauri"
status=0
# Our own crates are optimized too (the test profile leaves them at opt-level
# 0): the AES-GCM and hashing generics are instantiated in pollis-core, and at
# opt-level 0 they run an order of magnitude slower than a shipped build, which
# would make every timing in the report meaningless. POLLIS_SOAK_OPT=0 skips it.
OPT=()
if [ "${POLLIS_SOAK_OPT:-1}" = 1 ]; then
  for crate in pollis pollis-core pollis-delivery pollis-api; do
    OPT+=(--config "profile.test.package.$crate.opt-level=2")
  done
  export POLLIS_SOAK_BUILD="test profile, workspace crates at opt-level 2 (debug assertions on)"
else
  export POLLIS_SOAK_BUILD="test profile, workspace crates at opt-level 0 — timings are not representative"
fi
cargo "${OPT[@]}" test -p pollis --no-default-features --features vault-soak --test vault_soak \
  -- --ignored --nocapture --test-threads=1 vault_media_soak || status=$?
echo "==> report: $POLLIS_SOAK_REPORT"
exit "$status"
