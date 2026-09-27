#!/usr/bin/env bash
# One release covers every binary at one workspace version.
#
#   scripts/release.sh bump X.Y.Z          set the workspace version (open a PR with the result)
#   scripts/release.sh tag  X.Y.Z [title]  after that PR is merged: check main, create the annotated tag vX.Y.Z[-title]
#
# Pushing the tag starts the release workflow (`git push origin vX.Y.Z`).
# Environment: SKIP_CHECKS=1 skips the checks in `tag`.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

cmd="${1:-}"
ver="${2:-}"
title="${3:-}"

if [ "$cmd" != "bump" ] && [ "$cmd" != "tag" ]; then
  echo "usage: release.sh bump X.Y.Z | release.sh tag X.Y.Z [title]"
  exit 1
fi
if ! [[ "$ver" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "release: invalid version '$ver' (expected X.Y.Z)"
  exit 1
fi

if [ "$cmd" = "bump" ]; then
  tmp="$(mktemp)"
  awk -v ver="$ver" '
    $0 == "[workspace.package]" { in_pkg = 1; print; next }
    in_pkg && /^\[/ { in_pkg = 0 }
    in_pkg && /^version[[:space:]]*=/ { $0 = "version = \"" ver "\"" }
    { print }' Cargo.toml > "$tmp"
  cat "$tmp" > Cargo.toml
  rm -f "$tmp"
  cargo update -q --workspace
  git add -- Cargo.toml Cargo.lock
  echo "release: staged version $ver; commit it and open a PR"
  exit 0
fi

slug="$(printf '%s' "$title" | sed -E 's/[[:space:]]+/-/g; s/[^A-Za-z0-9._-]//g; s/^-+//; s/-+$//')"
tag="v${ver}${slug:+-$slug}"
if git rev-parse -q --verify "refs/tags/$tag" >/dev/null; then
  echo "release: tag '$tag' already exists"
  exit 1
fi

git fetch -q origin main
if [ "$(git rev-parse HEAD)" != "$(git rev-parse origin/main)" ]; then
  echo "release: HEAD is not origin/main; check out an up-to-date main first"
  exit 1
fi
bash scripts/verify-release-version.sh "$tag"

if [ "${SKIP_CHECKS:-}" = "1" ]; then
  echo "release: skipping checks (SKIP_CHECKS=1)"
else
  make check || { echo "release: checks failed; nothing was tagged"; exit 1; }
fi

git tag -a "$tag" -m "$tag"
echo "release done: $tag"
echo "Next: git push origin $tag   (pushing the tag starts the release workflow)"
