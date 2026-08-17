#!/usr/bin/env bash
# Classify the outcome of an independent rebuild (#944). Sourced by
# `.github/workflows/rebuild-verify.yml`; exercised by
# `scripts/test-rebuild-verdict.sh`.
#
# WHY THIS IS A SEPARATE, TESTED FUNCTION
#
# The rebuilder used to answer a binary question — does the hash I reproduced
# appear in the log as this tag's Linux AppImage payload? — and print "did not
# reproduce" for every answer of no. That sentence is the single strongest
# accusation this project's CI can make: read literally, it says the binary
# Pollis shipped is not the honest build of the public source at that tag.
#
# It is also, on its own, not something the hash comparison can know. The Linux
# AppImage is not purely a function of our source. `linuxdeploy` copies system
# shared libraries off the build runner into the bundle — for v1.9.3 that was
# libkrb5, libk5crypto, libgssapi_krb5, libkrb5support, libsqlite3, libsystemd
# and libudev. GitHub re-images the `ubuntu-22.04` label every couple of weeks
# and rolls the new image out gradually, so two jobs half an hour apart can land
# on different images and vendor genuinely different copies of those libraries.
# When that happens the AppImage differs for reasons that have nothing to do
# with Pollis's source, its compiler, or its release process.
#
# That is not hypothetical, and it is not a corner case — it is what actually
# happened. v1.9.3's release built on `ubuntu22@20260810.260.1`; the rebuild ran
# 36 minutes later on `ubuntu22@20260720.234.2`. The workflow compared the two
# and published "did not reproduce" into a permanent public ledger, where it sat
# as the one unexplained red in an otherwise clean history.
#
# So the verdict now has FOUR outcomes rather than two, and the extra
# information that makes them separable is the runner image the release recorded
# in its own transparency leaf (#939) versus the one the rebuild actually ran
# on. Every non-reproducing outcome is still RED — an inconclusive rebuild is
# not a passing rebuild, and quietly greening environment drift would hand a
# tamperer a window that opens every time GitHub re-images a runner. What
# changes is that the ledger can now say which kind of red it was, with the two
# image ids as the evidence.
#
# THE DIRECTION THAT MATTERS: `did_not_reproduce` must still fire. If the images
# match and the bytes differ, that is the real finding this whole program exists
# to surface, and no amount of environment nuance may soften it.
# `scripts/test-rebuild-verdict.sh` asserts both directions.

# rebuild_logged_linux_payload <release-report.json>
#
# Echo the payload_sha256 the log records for this tag's Linux AppImage payload
# leaf, or the empty string when the tag has no such leaf.
rebuild_logged_linux_payload() {
  local report="${1:-}"
  [ -n "$report" ] && [ -f "$report" ] || return 0
  jq -r '[.artifacts[]
           | select(.platform == "linux" and .bundle == "appimage"
                    and .layer == "payload")]
         | .[0].payload_sha256 // ""' "$report"
}

# rebuild_logged_runner_image <release-report.json>
#
# Echo the runner image the release recorded for that same leaf. Empty when the
# tag predates #939's toolchain capture (the field is absent, or present and
# literally "unknown"), which is a different state from "recorded and equal" and
# is treated as such by rebuild_classify.
rebuild_logged_runner_image() {
  local report="${1:-}" image
  [ -n "$report" ] && [ -f "$report" ] || return 0
  image="$(jq -r '[.artifacts[]
                    | select(.platform == "linux" and .bundle == "appimage"
                             and .layer == "payload")]
                  | .[0].toolchain.runner_image // ""' "$report")"
  # `unknown` is what scripts/attest-binaries.sh defaulted to for every leaf
  # through v1.9.7. It names no image, so it must never satisfy an equality
  # test against a real one — and it must never be echoed as though it had.
  if [ "$image" = "unknown" ] || [ "$image" = "null" ]; then
    image=""
  fi
  printf '%s' "$image"
}

# rebuild_classify <reproduced_hash> <logged_hash> <logged_image> <own_image>
#
# Echo exactly one verdict token:
#
#   reproduced         the rebuilt payload hash IS the logged one. A match is a
#                      match regardless of environment — reproducing on a
#                      different image is a stronger result, not a weaker one.
#   did_not_reproduce  hashes differ AND both builds ran on the same recorded
#                      runner image. The real finding. Red, and loud.
#   environment_drift  hashes differ AND the release recorded a different runner
#                      image from the one this rebuild ran on. Inconclusive:
#                      the documented precondition for bit-identity (residual
#                      §6) was not met, so the comparison cannot separate
#                      tampering from vendored host libraries. Red, but it does
#                      not accuse.
#   recipe_unrecorded  hashes differ AND the leaf records no runner image at all
#                      (every tag through v1.9.7). The precondition cannot even
#                      be evaluated. Red, and does not accuse.
#
# Both image arguments are compared as opaque strings, exactly as
# write_toolchain_manifest composes them (`<os>@<version>` plus the Linux
# `+helper:<os>@<version>` suffix), so a rebuild that matched the app image but
# built the capture helper on a different one is correctly NOT called a match.
rebuild_classify() {
  local reproduced="${1:-}" logged="${2:-}" logged_image="${3:-}" own_image="${4:-}"

  if [ -n "$reproduced" ] && [ "$reproduced" = "$logged" ]; then
    echo "reproduced"
    return 0
  fi
  if [ -z "$logged_image" ]; then
    echo "recipe_unrecorded"
    return 0
  fi
  # An unidentifiable local runner is the same epistemic state as an unrecorded
  # remote one: nothing can be asserted about whether the environments matched,
  # so it must not be reported as though they did.
  if [ -z "$own_image" ]; then
    echo "recipe_unrecorded"
    return 0
  fi
  if [ "$logged_image" != "$own_image" ]; then
    echo "environment_drift"
    return 0
  fi
  echo "did_not_reproduce"
}

# rebuild_verdict_is_accusation <verdict>
#
# True only for the verdict that asserts the shipped binary does not correspond
# to the tagged source. Callers use this to decide wording, never to decide the
# exit code — every non-`reproduced` verdict stays red.
rebuild_verdict_is_accusation() {
  [ "${1:-}" = "did_not_reproduce" ]
}
