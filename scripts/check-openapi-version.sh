#!/usr/bin/env bash
# Issue #735: `docs/generated/openapi.json`'s `info.version` only means
# anything if it's actually bumped whenever the schema's shape changes —
# `make openapi-check` (#723) already catches staleness (checked-in file
# doesn't match the real routes), but says nothing about whether a real
# shape change was paired with a version bump. This compares the
# checked-in schema against the same file at $1 (default: origin/main,
# falling back to local main if origin isn't available) with `info.version`
# stripped out of both sides: if the rest of the document differs but the
# version field doesn't, that's a shape change with no version bump.
set -euo pipefail

BASE_REF="${1:-origin/main}"
if ! git rev-parse --verify "$BASE_REF" >/dev/null 2>&1; then
  BASE_REF="main"
fi

SCHEMA_PATH="docs/generated/openapi.json"

BASE_JSON="$(git show "$BASE_REF:$SCHEMA_PATH" 2>/dev/null)" || {
  echo "no $BASE_REF:$SCHEMA_PATH to compare against — skipping version check"
  exit 0
}

CURRENT_SHAPE="$(jq -S 'del(.info.version)' "$SCHEMA_PATH")"
BASE_SHAPE="$(echo "$BASE_JSON" | jq -S 'del(.info.version)')"

if [ "$CURRENT_SHAPE" = "$BASE_SHAPE" ]; then
  exit 0
fi

CURRENT_VERSION="$(jq -r '.info.version' "$SCHEMA_PATH")"
BASE_VERSION="$(echo "$BASE_JSON" | jq -r '.info.version')"

if [ "$CURRENT_VERSION" = "$BASE_VERSION" ]; then
  echo "$SCHEMA_PATH's shape changed relative to $BASE_REF but info.version ($CURRENT_VERSION) wasn't bumped"
  echo "bump the 'version = \"...\"' literal in crates/server/src/openapi.rs, run 'make openapi', and commit the result"
  exit 1
fi

exit 0
