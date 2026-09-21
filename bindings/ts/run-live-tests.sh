#!/usr/bin/env bash
# Local helper (not committed as part of the deliverable, just a runner) —
# loads the repo root .env and runs the opt-in live vitest suite against it.
set -euo pipefail
cd "$(dirname "$0")"
set -a
# shellcheck disable=SC1091
source ../../.env
set +a
export AVALON_SERVER_URL="http://${AVALON_SERVER_ADDR}"
export AVALON_LIVE_DATABASE_URL="${DATABASE_URL}"
npx vitest run --config vitest.live.config.ts
