#!/usr/bin/env bash
# Fails unless the workspace version matches the version in a release tag.
# Usage: scripts/verify-release-version.sh v0.2.0[-title]
set -euo pipefail

tag="${1:?usage: verify-release-version.sh <tag>}"
cd "$(git rev-parse --show-toplevel)"

if [[ ! "$tag" =~ ^v([0-9]+\.[0-9]+\.[0-9]+)([-.].*)?$ ]]; then
  echo "verify-release-version: tag '$tag' must look like vX.Y.Z[-title]"
  exit 1
fi
want="${BASH_REMATCH[1]}"

have="$(sed -n '/^\[workspace\.package\]/,/^\[/p' Cargo.toml | sed -nE 's/^version[[:space:]]*=[[:space:]]*"([^"]+)".*/\1/p' | head -n1)"
if [[ "$have" != "$want" ]]; then
  echo "verify-release-version: tag $tag is $want but the workspace version is $have"
  exit 1
fi
echo "verify-release-version: $tag matches the workspace version ($want)"
