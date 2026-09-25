#!/usr/bin/env bash
# Load-tests isolated avalon-server processes. Every node is started by this
# script on 127.0.0.1 with its own Postgres schema (dropped on exit); the load
# generator (crates/loadtest) refuses any non-loopback target, so nothing here
# can reach a running node, the default `public` schema, or a peer.
#
# usage: scripts/load-tests.sh [scenario ...]     (default: all scenarios)
#   scenarios: flood-public flood-auth-principal flood-auth-ip many-identities
#              announce-source announce-cap announce-strict mesh concurrency
#              db-pool db-pool-reads db-pool-writes db-pool-large slow-conn oversized sustained
#
# env: AVALON_ENV_FILE   .env to read DATABASE_URL and keys from
#      LOAD_PORT_BASE    first local port used (default 19900)
#      LOAD_SCALE        smoke (seconds, default) or full (minutes)
#      LOAD_REPEAT       run the selected scenarios this many times (default 1)
#      LOAD_KEEP_SCHEMAS=1  keep the test schemas after the run
#      LOAD_JSON         file to append one RESULT json line per scenario to

set -uo pipefail

HARNESS_LOG_PREFIX=avalon-load-tests
BASE_PORT="${LOAD_PORT_BASE:-19900}"
KEEP_SCHEMAS="${LOAD_KEEP_SCHEMAS:-}"
# shellcheck source=lib/harness.sh
. "$(dirname "${BASH_SOURCE[0]}")/lib/harness.sh"

SCALE="${LOAD_SCALE:-smoke}"
REPEAT="${LOAD_REPEAT:-1}"
LOAD_BIN="$ROOT/target/debug/avalon-loadtest"
[ "${LOAD_PROFILE:-debug}" = release ] && { SERVER_BIN="$ROOT/target/release/avalon-server"; LOAD_BIN="$ROOT/target/release/avalon-loadtest"; }
ulimit -n 8192 2>/dev/null || true

if [ "$SCALE" = full ]; then LIMIT=600; else LIMIT=60; fi
NO_LIMIT=1000000
PORT_OFFSET=0
# Sets NEXT_PORT (not a subshell echo, so the offset persists).
next_port() { PORT_OFFSET=$((PORT_OFFSET + 1)); NEXT_PORT=$((BASE_PORT + PORT_OFFSET)); }

# lt <scenario> <port> <schema> [--key value ...]: runs the generator against our own node.
lt() {
  local scenario="$1" port="$2" schema="$3"
  shift 3
  local out="$LOG_DIR/scenario-$scenario.log"
  "$LOAD_BIN" "$scenario" --target "http://127.0.0.1:$port" --db-url "$(schema_url "$schema")" \
    --pid "$LAST_PID" --scale "$SCALE" "$@" >"$out" 2>&1
  local rc=$?
  cat "$out"
  if [ -n "${LOAD_JSON:-}" ]; then grep '^RESULT ' "$out" | sed 's/^RESULT //' >>"$LOAD_JSON"; fi
  return "$rc"
}

# single_node <label> <scenario> <node-env...> -- [generator args]
single_node() {
  local label="$1" scenario="$2"
  shift 2
  local node_env=()
  while [ "$#" -gt 0 ] && [ "$1" != "--" ]; do node_env+=("$1"); shift; done
  [ "${1:-}" = "--" ] && shift
  local port schema="live_load_${label//-/_}"
  next_port; port="$NEXT_PORT"
  new_schema "$schema" || return 1
  start_node "$label" "$schema" "$port" "${PINNED[@]}" "${node_env[@]}" || return 1
  lt "$scenario" "$port" "$schema" "$@"
}

# The node's own defaults, pinned so an inherited .env cannot change what a scenario measures.
PINNED=(AVALON_ALLOW_PRIVATE_PEERS=false AVALON_ANNOUNCE_VERIFY_REACHABILITY=true)
PEER_OPEN=(AVALON_ALLOW_PRIVATE_PEERS=true AVALON_ANNOUNCE_VERIFY_REACHABILITY=false)

scenario_flood_public() {
  single_node flood-public flood-public AVALON_RATE_LIMIT_PER_MINUTE="$LIMIT" -- --limit "$LIMIT"
}
scenario_flood_auth_principal() {
  single_node flood-auth-principal flood-auth AVALON_PRINCIPAL_RATE_LIMIT_PER_MINUTE="$LIMIT" -- --mode principal --limit "$LIMIT"
}
scenario_flood_auth_ip() {
  single_node flood-auth-ip flood-auth AVALON_RATE_LIMIT_PER_MINUTE="$LIMIT" \
    AVALON_PRINCIPAL_RATE_LIMIT_PER_MINUTE="$NO_LIMIT" -- --mode ip --limit "$LIMIT"
}
scenario_many_identities() {
  single_node many-identities many-identities AVALON_RATE_LIMIT_PER_MINUTE="$LIMIT" -- --limit "$LIMIT"
}
scenario_announce_source() {
  single_node announce-source announce-flood "${PEER_OPEN[@]}" AVALON_ANNOUNCE_NEW_PEERS_PER_SOURCE_PER_MINUTE=10 \
    -- --mode source --source-limit 10
}
scenario_announce_cap() {
  single_node announce-cap announce-flood "${PEER_OPEN[@]}" AVALON_ANNOUNCE_NEW_PEERS_PER_SOURCE_PER_MINUTE="$NO_LIMIT" \
    AVALON_NODE_MAX_KNOWN_PEERS=50 -- --mode cap --cap 50
}
scenario_announce_strict() {
  single_node announce-strict announce-flood -- --mode strict
}
scenario_concurrency() {
  single_node concurrency concurrency AVALON_MAX_CONCURRENT_REQUESTS=4 \
    AVALON_PRINCIPAL_RATE_LIMIT_PER_MINUTE="$NO_LIMIT" -- --cap 4
}
scenario_db_pool() {
  single_node db-pool db-pool AVALON_MAX_DB_CONNECTIONS=2 AVALON_PRINCIPAL_RATE_LIMIT_PER_MINUTE="$NO_LIMIT" -- --pool 2
}
scenario_db_pool_reads() {
  single_node db-pool-reads db-pool AVALON_MAX_DB_CONNECTIONS=2 AVALON_PRINCIPAL_RATE_LIMIT_PER_MINUTE="$NO_LIMIT" -- --pool 2 --mix reads
}
scenario_db_pool_writes() {
  local pool="${LOAD_POOL:-2}"
  single_node db-pool-writes db-pool AVALON_MAX_DB_CONNECTIONS="$pool" AVALON_PRINCIPAL_RATE_LIMIT_PER_MINUTE="$NO_LIMIT" -- --pool "$pool" --mix writes
}
scenario_db_pool_large() {
  single_node db-pool-large db-pool AVALON_MAX_DB_CONNECTIONS=10 AVALON_PRINCIPAL_RATE_LIMIT_PER_MINUTE="$NO_LIMIT" -- --pool 10
}
scenario_slow_conn() {
  single_node slow-conn slow-conn
}
scenario_oversized() {
  single_node oversized oversized AVALON_PRINCIPAL_RATE_LIMIT_PER_MINUTE="$NO_LIMIT"
}
scenario_sustained() {
  single_node sustained sustained AVALON_PRINCIPAL_RATE_LIMIT_PER_MINUTE="$NO_LIMIT"
}

# Three nodes bootstrapping to each other, each in its own schema; node A is flooded.
scenario_mesh() {
  local a b c s
  next_port; a="$NEXT_PORT"; next_port; b="$NEXT_PORT"; next_port; c="$NEXT_PORT"
  for s in a b c; do new_schema "live_load_mesh_$s" || return 1; done
  local common=("${PEER_OPEN[@]}" AVALON_ANNOUNCE_NEW_PEERS_PER_SOURCE_PER_MINUTE="$NO_LIMIT" \
    AVALON_NODE_MAX_KNOWN_PEERS=60 AVALON_ANNOUNCE_INTERVAL_SECS=2)
  start_node mesh-a live_load_mesh_a "$a" "${PINNED[@]}" "${common[@]}" AVALON_BOOTSTRAP_PEERS="http://127.0.0.1:$b,http://127.0.0.1:$c" || return 1
  local pid_a="$LAST_PID"
  start_node mesh-b live_load_mesh_b "$b" "${PINNED[@]}" "${common[@]}" AVALON_BOOTSTRAP_PEERS="http://127.0.0.1:$a,http://127.0.0.1:$c" || return 1
  start_node mesh-c live_load_mesh_c "$c" "${PINNED[@]}" "${common[@]}" AVALON_BOOTSTRAP_PEERS="http://127.0.0.1:$a,http://127.0.0.1:$b" || return 1
  LAST_PID="$pid_a"
  lt mesh-announce-flood "$a" live_load_mesh_a \
    --nodes "http://127.0.0.1:$a,http://127.0.0.1:$b,http://127.0.0.1:$c" --cap 60 --announce-interval 2
}

ALL=(flood-public flood-auth-principal flood-auth-ip many-identities announce-source announce-cap announce-strict
  mesh concurrency db-pool db-pool-reads db-pool-writes db-pool-large slow-conn oversized sustained)
SELECTED=("$@")
[ "${#SELECTED[@]}" -eq 0 ] && SELECTED=("${ALL[@]}")

BUILD_FLAGS=()
[ "${LOAD_PROFILE:-debug}" = release ] && BUILD_FLAGS=(--release)
# The schema helper example always runs from the debug profile.
cargo build -q -p avalon-server --examples || exit 1
cargo build -q "${BUILD_FLAGS[@]}" -p avalon-server -p avalon-loadtest --bins || exit 1
[ -x "$SERVER_BIN" ] || { echo "missing $SERVER_BIN" >&2; exit 1; }

echo "scale=$SCALE repeat=$REPEAT port-base=$BASE_PORT logs=$LOG_DIR"
echo "machine: $(nproc) cores, $(free -m | awk '/Mem:/ {print $2}') MiB RAM, load average at start: $(cut -d' ' -f1-3 /proc/loadavg)"
for round in $(seq 1 "$REPEAT"); do
  for s in "${SELECTED[@]}"; do
    fn="scenario_${s//-/_}"
    if ! declare -F "$fn" >/dev/null; then echo "unknown scenario: $s" >&2; FAILED=1; continue; fi
    echo "== run $round/$REPEAT: $s (load average: $(cut -d' ' -f1-3 /proc/loadavg))"
    if "$fn"; then RESULTS+=("PASS  $s"); else RESULTS+=("FAIL  $s"); FAILED=1; fi
    stop_all
  done
done

echo
printf '%s\n' "${RESULTS[@]}"
echo "logs: $LOG_DIR"
exit "$FAILED"
