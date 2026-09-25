#!/usr/bin/env bash
# Shared helpers for the isolated-node harnesses (live-tests.sh, load-tests.sh).
# Sourced, never executed. Every server binds 127.0.0.1, uses its own Postgres
# schema (dropped on exit) and has no bootstrap/mirror peers unless the caller
# wires its own processes together.
#
# Callers set before sourcing: HARNESS_LOG_PREFIX, BASE_PORT.
# env: AVALON_ENV_FILE  .env to read DATABASE_URL and keys from
#      KEEP_SCHEMAS=1   keep the created schemas after the run

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

ENV_FILE="${AVALON_ENV_FILE:-$ROOT/.env}"
if [ ! -f "$ENV_FILE" ]; then
  echo "no .env at $ENV_FILE; set AVALON_ENV_FILE" >&2
  exit 2
fi
set -a
# shellcheck disable=SC1090
. "$ENV_FILE"
set +a
: "${DATABASE_URL:?DATABASE_URL must be set in $ENV_FILE}"

BASE_DB_URL="$DATABASE_URL"
RUN_ID="$$"
LOG_DIR="${TMPDIR:-/tmp}/${HARNESS_LOG_PREFIX:?}-$RUN_ID"
mkdir -p "$LOG_DIR"
SERVER_BIN="$ROOT/target/debug/avalon-server"
SCHEMAS=()
PIDS=()
RESULTS=()
FAILED=0

schema_url() {
  local sep='?'
  [[ "$BASE_DB_URL" == *\?* ]] && sep='&'
  echo "${BASE_DB_URL}${sep}options=-c%20search_path%3D$1"
}

new_schema() {
  local name="$1"
  cargo run -q -p avalon-server --example live_schema -- drop "$name" >/dev/null 2>&1
  cargo run -q -p avalon-server --example live_schema -- create "$name" || return 1
  SCHEMAS+=("$name")
}

# start_node <name> <schema> <port> [VAR=value ...]
# Starts one server; sets LAST_PID. Later VAR=value pairs override defaults.
start_node() {
  local name="$1" schema="$2" port="$3"
  shift 3
  env DATABASE_URL="$(schema_url "$schema")" \
    AVALON_SERVER_ADDR="127.0.0.1:$port" \
    AVALON_NODE_URL="http://127.0.0.1:$port" \
    AVALON_BOOTSTRAP_PEERS= AVALON_MIRROR_PEERS= AVALON_KNOWN_SHARDS= \
    AVALON_SETTLEMENT_REMOTE_URLS= AVALON_SETTLEMENT_REMOTE_URL= \
    AVALON_REDIS_URL= AVALON_DHT_ENABLED=false \
    AVALON_LIBP2P_LISTEN_ADDR=/ip4/127.0.0.1/tcp/0 AVALON_DHT_BOOTSTRAP_SCAN_INTERVAL_SECS=2 \
    AVALON_RATE_LIMIT_PER_MINUTE=1000000 \
    AVALON_INTEGRATOR_REGISTRATION_RATE_LIMIT_PER_MINUTE=1000000 AVALON_NAME_CLAIM_RATE_LIMIT_PER_MINUTE=1000000 \
    AVALON_ALLOW_PRIVATE_PEERS=true AVALON_ANNOUNCE_VERIFY_REACHABILITY=false \
    AVALON_ANNOUNCE_NEW_PEERS_PER_SOURCE_PER_MINUTE=1000 \
    "$@" "$SERVER_BIN" >"$LOG_DIR/$name.log" 2>&1 &
  LAST_PID=$!
  PIDS+=("$LAST_PID")
  local i
  for i in $(seq 1 120); do
    if curl -sf "http://127.0.0.1:$port/nodes/status" >/dev/null 2>&1; then
      return 0
    fi
    kill -0 "$LAST_PID" 2>/dev/null || break
    sleep 0.5
  done
  echo "server $name failed to start; see $LOG_DIR/$name.log" >&2
  tail -5 "$LOG_DIR/$name.log" >&2
  return 1
}

# start_blackhole <port>: accepts TCP connections on 127.0.0.1 and never answers.
start_blackhole() {
  python3 -c '
import socket, sys
s = socket.socket()
s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind(("127.0.0.1", int(sys.argv[1])))
s.listen(64)
held = []
while True:
    held.append(s.accept()[0])
' "$1" >/dev/null 2>&1 &
  PIDS+=("$!")
  sleep 0.5
}

stop_all() {
  local p
  for p in "${PIDS[@]:-}"; do
    [ -n "$p" ] && kill "$p" 2>/dev/null
  done
  wait 2>/dev/null
  PIDS=()
  if [ "${KEEP_SCHEMAS:-}" != 1 ] && [ "${#SCHEMAS[@]}" -gt 0 ]; then
    cargo run -q -p avalon-server --example live_schema -- drop "${SCHEMAS[@]}" >/dev/null 2>&1
  fi
  SCHEMAS=()
}
trap stop_all EXIT

