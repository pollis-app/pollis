#!/usr/bin/env bash
#
# Propagate one version string to every place it lives in the repo.
#
# Usage:
#   scripts/bump-version.sh 1.2.3         # explicit semver
#   scripts/bump-version.sh               # re-apply whatever is in VERSION
#
# Source of truth: the `VERSION` file at repo root. Everything else is
# derived. CI scripts call this with the tag-derived version before
# building, so a tag push (`v1.2.3` → `1.2.3`) automatically stamps
# every package.json / Cargo.toml / tauri.conf.json before the build
# sees them. Local devs can run it ad-hoc to keep things consistent
# between releases.
#
# Locations updated:
#   - VERSION                              (the source itself, if arg given)
#   - Cargo.toml                           [workspace.package].version
#                                          → pollis (src-tauri), pollis-core,
#                                            pollis-node all inherit it via
#                                            `version.workspace = true`
#   - package.json                         root npm workspace
#   - frontend/package.json
#   - electron/package.json
#   - pollis-node/package.json             (npm-side; Cargo side inherits)
#   - src-tauri/tauri.conf.json            "version"
#
# Validation: only accepts strict semver MAJOR.MINOR.PATCH with an
# optional `-prerelease` suffix. Anything else aborts.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

VERSION="${1:-}"
if [ -z "$VERSION" ]; then
  if [ ! -f VERSION ]; then
    echo "error: no version arg given and no VERSION file at repo root" >&2
    exit 1
  fi
  VERSION="$(tr -d '[:space:]' < VERSION)"
fi

# Strict semver MAJOR.MINOR.PATCH (+ optional -prerelease.build.tags).
if ! [[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$ ]]; then
  echo "error: '$VERSION' is not strict semver MAJOR.MINOR.PATCH[-pre]" >&2
  echo "       (got from ${1:+arg}${1:-VERSION file}; '1.1' isn't valid, use '1.1.0')" >&2
  exit 1
fi

echo "Bumping to v${VERSION}"

# 1. VERSION file
echo "$VERSION" > VERSION

# 2. Cargo workspace — pollis-core / pollis-node / src-tauri all inherit
#    via `version.workspace = true`, so this single line stamps three crates.
sed -i.bak -E "/^\[workspace\.package\]/,/^\[/{
  s/^version = \".*\"/version = \"${VERSION}\"/
}" Cargo.toml
rm -f Cargo.toml.bak

# 3. Every package.json — single-line "version" field at top level.
#    Use jq so we don't accidentally rewrite nested "version" fields in
#    sub-trees (e.g. dependencies pinned to a literal "version": "x").
for pkg in package.json frontend/package.json electron/package.json pollis-node/package.json; do
  jq --arg v "$VERSION" '.version = $v' "$pkg" > "$pkg.tmp"
  mv "$pkg.tmp" "$pkg"
done

# 4. tauri.conf.json — top-level "version".
jq --arg v "$VERSION" '.version = $v' src-tauri/tauri.conf.json > src-tauri/tauri.conf.json.tmp
mv src-tauri/tauri.conf.json.tmp src-tauri/tauri.conf.json

echo
echo "Stamped ${VERSION} into:"
grep -H '^version = ' Cargo.toml | sed 's/^/  /'
for f in package.json frontend/package.json electron/package.json pollis-node/package.json src-tauri/tauri.conf.json; do
  v="$(jq -r .version "$f")"
  printf '  %-40s %s\n' "$f" "$v"
done
echo "  VERSION                                  $(cat VERSION)"
