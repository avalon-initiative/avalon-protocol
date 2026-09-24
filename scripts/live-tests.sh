#!/usr/bin/env bash
# Runs the `--ignored` live test suite against a private, isolated set of
# avalon-server processes. Nothing here touches a running node, the default
# `public` schema, or any peer: every server binds 127.0.0.1, uses its own
# Postgres schema (dropped on exit) and has no bootstrap/mirror peers unless
# a group wires two of its own processes together.
#
# usage: scripts/live-tests.sh [group ...]     (default: all groups)
#   groups: core ledger-writers relay own-shard cross-shard-login aggregator internal-role
#           gateway-only realtime-proxy remote-settlement remote-submit
#           settlement-only topology topology-probe topology-trace peer-table-bounds
#
# env: AVALON_ENV_FILE  .env to read DATABASE_URL and keys from
#                       (default: <repo>/.env, else the primary checkout's)
#      LIVE_PORT_BASE   first local port used (default 18000)
#      LIVE_KEEP_SCHEMAS=1  keep the test schemas after the run
#      LIVE_HOLD=<secs>  core group: keep the started server up for that long first
#      LIVE_ONLY="a b"  core group: only run these test files

set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
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
BASE_PORT="${LIVE_PORT_BASE:-18000}"
RUN_ID="$$"
LOG_DIR="${TMPDIR:-/tmp}/avalon-live-tests-$RUN_ID"
mkdir -p "$LOG_DIR"
SERVER_BIN="$ROOT/target/debug/avalon-server"
SCHEMAS=()
PIDS=()
RESULTS=()
FAILED=0

# Ed25519 keypair for the managed-hosting test: seed is the test's private
# key, the raw public key is what the server trusts.
MH_SEED="$(openssl genpkey -algorithm ed25519 -outform DER | tail -c 32 | xxd -p -c 64)"
MH_PUB="$( (printf '\x30\x2e\x02\x01\x00\x30\x05\x06\x03\x2b\x65\x70\x04\x22\x04\x20'; echo "$MH_SEED" | xxd -r -p) \
  | openssl pkey -inform DER -pubout -outform DER | tail -c 32 | xxd -p -c 64)"

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
  if [ "${LIVE_KEEP_SCHEMAS:-}" != 1 ] && [ "${#SCHEMAS[@]}" -gt 0 ]; then
    cargo run -q -p avalon-server --example live_schema -- drop "${SCHEMAS[@]}" >/dev/null 2>&1
  fi
  SCHEMAS=()
}
trap stop_all EXIT

# run_test <label> <package> <test-file> [test args...]  (env is inherited)
run_test() {
  local label="$1" pkg="$2" file="$3"
  shift 3
  if cargo test -q -p "$pkg" --test "$file" -- --ignored --test-threads=1 ${LIVE_TEST_ARGS:-} "$@" \
    >"$LOG_DIR/test-${label////-}.log" 2>&1; then
    RESULTS+=("PASS  $label")
  else
    RESULTS+=("FAIL  $label  (log: $LOG_DIR/test-${label////-}.log)")
    FAILED=1
  fi
}

# Every server-crate test file that needs a bespoke topology; `core` covers
# the rest against one plain server.
MULTI_PROCESS="chat_replication cross_node_login_cross_shard cross_node_login_verification \
cross_shard gateway_only_deployment identity_locator internal_role_protocol mirror_push realtime_proxy \
realtime_reconnect realtime_relay remote_settlement remote_submit_status settlement_only topology \
topology_probe topology_trace peer_table_bounds"

# Test files that write or rewrite ledger/projection tables directly. Run
# beside other tests they desync the server's in-memory Merkle leaf cache and
# wipe rows other tests rely on, so each gets a fresh server and schema.
LEDGER_WRITERS="history settlement retention_archive_confirmation rebuild_from_events rollback \
read_model_boundary conversations_no_ledger"

group_core() {
  new_schema live_core || return
  start_node core live_core "$((BASE_PORT + 80))" \
    AVALON_MANAGED_HOSTING_VERIFY_KEY="$MH_PUB" || return
  export DATABASE_URL="$(schema_url live_core)"
  export AVALON_SERVER_URL="http://127.0.0.1:$((BASE_PORT + 80))"
  export AVALON_MANAGED_HOSTING_TEST_SIGNING_KEY="$MH_SEED"
  export AVALON_NETWORK_ID="${AVALON_NETWORK_ID:-avalon-dev-local}"

  [ -n "${LIVE_HOLD:-}" ] && { echo "core server at $AVALON_SERVER_URL; holding ${LIVE_HOLD}s"; sleep "$LIVE_HOLD"; }
  # Tests that write the ledger tables directly (chain, indexer, lib tests)
  # get their own schema: a second writer under a running server desyncs the
  # server's in-memory Merkle leaf cache from the ledger it reads.
  new_schema live_core_direct || return
  local direct_url server_url
  direct_url="$(schema_url live_core_direct)"
  server_url="$DATABASE_URL"
  DATABASE_URL="$direct_url" "$ROOT/target/debug/migrate" up >"$LOG_DIR/migrate-direct.log" 2>&1 \
    || { cat "$LOG_DIR/migrate-direct.log" >&2; return; }

  local d pkg f name
  for d in crates/*/; do
    pkg="$(grep -m1 '^name' "$d/Cargo.toml" | cut -d'"' -f2)"
    [ -d "$d/tests" ] || continue
    for f in "$d"/tests/*.rs; do
      name="$(basename "$f" .rs)"
      [[ " $MULTI_PROCESS " == *" $name "* ]] && continue
      [[ "$name" == milestone_1_three_node ]] && continue
      [[ " $LEDGER_WRITERS " == *" $name "* ]] && continue
      [ -n "${LIVE_ONLY:-}" ] && [[ " $LIVE_ONLY " != *" $name "* ]] && continue
      if [ "$pkg" = avalon-server ] || [ "$pkg" = avalon-cli ]; then
        export DATABASE_URL="$server_url"
      else
        export DATABASE_URL="$direct_url"
      fi
      run_test "core/$name" "$pkg" "$name"
    done
  done
  [ -n "${LIVE_ONLY:-}" ] && return
  export DATABASE_URL="$direct_url"
  if cargo test -q --workspace --lib --bins -- --ignored --test-threads=1 \
    >"$LOG_DIR/test-core-lib.log" 2>&1; then
    RESULTS+=("PASS  core/lib+bins")
  else
    RESULTS+=("FAIL  core/lib+bins  (log: $LOG_DIR/test-core-lib.log)")
    FAILED=1
  fi
}

group_ledger_writers() {
  local name pkg schema port=$((BASE_PORT + 85))
  for name in $LEDGER_WRITERS; do
    pkg="$(cd crates && grep -l "" */tests/"$name".rs 2>/dev/null | head -1 | cut -d/ -f1)"
    [ -n "$pkg" ] || continue
    pkg="$(grep -m1 '^name' "crates/$pkg/Cargo.toml" | cut -d'"' -f2)"
    schema="live_lw_$name"
    new_schema "$schema" || continue
    start_node "lw-$name" "$schema" "$port" || { stop_all; continue; }
    (
      export DATABASE_URL="$(schema_url "$schema")"
      export AVALON_SERVER_URL="http://127.0.0.1:$port"
      export AVALON_NETWORK_ID="${AVALON_NETWORK_ID:-avalon-dev-local}"
      run_test "ledger-writers/$name" "$pkg" "$name"
      printf '%s\n' "${RESULTS[@]}" >"$LOG_DIR/subshell-results"
      echo "$FAILED" >"$LOG_DIR/subshell-failed"
    )
    merge_subshell
    stop_all
  done
}

group_relay() {
  new_schema live_relay || return
  local a=$((BASE_PORT + 10)) b=$((BASE_PORT + 11)) c=$((BASE_PORT + 12))
  start_node relay-a live_relay "$a" AVALON_DHT_ENABLED=true AVALON_IDENTITY_LOCATOR_SCAN_INTERVAL_SECS=2 \
    AVALON_BOOTSTRAP_PEERS="http://127.0.0.1:$b" AVALON_ANNOUNCE_INTERVAL_SECS=2 || return
  start_node relay-b live_relay "$b" AVALON_DHT_ENABLED=true \
    AVALON_BOOTSTRAP_PEERS="http://127.0.0.1:$a" AVALON_MIRROR_PEERS="http://127.0.0.1:$a" \
    AVALON_ANNOUNCE_INTERVAL_SECS=2 || return
  start_node relay-c live_relay "$c" AVALON_DHT_ENABLED=false \
    AVALON_BOOTSTRAP_PEERS="http://127.0.0.1:$a" AVALON_MIRROR_PEERS="http://127.0.0.1:$a" \
    AVALON_ANNOUNCE_INTERVAL_SECS=2 AVALON_MIRROR_POLL_INTERVAL_SECS=5 || return
  (
    export DATABASE_URL="$(schema_url live_relay)"
    export AVALON_REALTIME_RELAY_PEER_DATABASE_URL="$DATABASE_URL"
    export AVALON_SERVER_URL="http://127.0.0.1:$a"
    export AVALON_REALTIME_RELAY_PEER_SERVER_URL="http://127.0.0.1:$b"
    export AVALON_MIRROR_PUSH_PEER_SERVER_URL="http://127.0.0.1:$b"
    export AVALON_MIRROR_PUSH_NO_DHT_PEER_SERVER_URL="http://127.0.0.1:$c"
    export AVALON_SECOND_NODE_SERVER_URL="http://127.0.0.1:$b"
    for t in realtime_relay realtime_reconnect chat_replication mirror_push identity_locator nodes; do
      run_test "relay/$t" avalon-server "$t"
    done
    printf '%s\n' "${RESULTS[@]}" >"$LOG_DIR/subshell-results"
    echo "$FAILED" >"$LOG_DIR/subshell-failed"
  )
  merge_subshell
}

# Subshell groups cannot update RESULTS/FAILED directly.
merge_subshell() {
  RESULTS=()
  while IFS= read -r line; do RESULTS+=("$line"); done <"$LOG_DIR/subshell-results"
  [ "$(cat "$LOG_DIR/subshell-failed")" = 1 ] && FAILED=1
  return 0
}

group_own_shard() {
  new_schema live_own_shard || return
  local p=$((BASE_PORT + 20))
  start_node own-shard live_own_shard "$p" \
    AVALON_OWN_SHARD_ID=game:cross-node-login-verify-test || return
  (
    export DATABASE_URL="$(schema_url live_own_shard)"
    export AVALON_SERVER_URL="http://127.0.0.1:$p"
    run_test own-shard/cross_node_login_verification avalon-server cross_node_login_verification
    printf '%s\n' "${RESULTS[@]}" >"$LOG_DIR/subshell-results"
    echo "$FAILED" >"$LOG_DIR/subshell-failed"
  )
  merge_subshell
}

group_cross_shard_login() {
  new_schema live_xshard_a || return
  new_schema live_xshard_b || return
  local a=$((BASE_PORT + 30)) b=$((BASE_PORT + 31))
  start_node xshard-a live_xshard_a "$a" AVALON_DHT_ENABLED=true \
    AVALON_SETTLEMENT_SUBMIT_KEY= AVALON_IDENTITY_LOCATOR_SCAN_INTERVAL_SECS=2 || return
  start_node xshard-b live_xshard_b "$b" AVALON_DHT_ENABLED=true \
    AVALON_BOOTSTRAP_PEERS="http://127.0.0.1:$a" AVALON_SETTLEMENT_SUBMIT_KEY= || return
  (
    export CROSS_SHARD_NODE_A_URL="http://127.0.0.1:$a"
    export AVALON_SERVER_URL="http://127.0.0.1:$b"
    run_test cross-shard-login/cross_node_login_cross_shard avalon-server cross_node_login_cross_shard
    printf '%s\n' "${RESULTS[@]}" >"$LOG_DIR/subshell-results"
    echo "$FAILED" >"$LOG_DIR/subshell-failed"
  )
  merge_subshell
}

group_aggregator() {
  new_schema live_agg_core || return
  new_schema live_agg_second || return
  local core=$((BASE_PORT + 40)) a=$((BASE_PORT + 41)) b=$((BASE_PORT + 42)) second=$((BASE_PORT + 43))
  start_node agg-core live_agg_core "$core" || return
  start_node agg-second live_agg_second "$second" AVALON_OWN_SHARD_ID=game:agg-second || return
  # Each authority needs committed history before it has a tree head to serve:
  # an identity registration for core, an integrator registration (authored as
  # game:agg-second) for the named shard.
  DATABASE_URL="$(schema_url live_agg_core)" AVALON_SERVER_URL="http://127.0.0.1:$core" \
    cargo test -q -p avalon-server --test passkeys -- --ignored --test-threads=1 \
    >"$LOG_DIR/seed-core.log" 2>&1 || return
  (cd "$LOG_DIR" && "$ROOT/target/debug/avalon" register-integrator --slug agg-second \
    --name agg-second --owner-name agg-second --server "http://127.0.0.1:$second") \
    >"$LOG_DIR/seed-second.log" 2>&1 || { cat "$LOG_DIR/seed-second.log" >&2; return; }
  local port
  for port in "$core" "$second"; do
    for _ in $(seq 1 30); do
      curl -sf "http://127.0.0.1:$port/ledger/sth/latest" >/dev/null && break
      sleep 1
    done
  done
  local shards="core=http://127.0.0.1:$core,game:agg-second=http://127.0.0.1:$second"
  local keys="core=$AVALON_SETTLEMENT_VERIFY_KEY,game:agg-second=$AVALON_SETTLEMENT_VERIFY_KEY"
  start_node agg-a live_agg_core "$a" AVALON_KNOWN_SHARDS="$shards" \
    AVALON_SHARD_VERIFY_KEYS="$keys" || return
  start_node agg-b live_agg_core "$b" AVALON_KNOWN_SHARDS="$shards" \
    AVALON_SHARD_VERIFY_KEYS="$keys" || return
  (
    export AVALON_SERVER_URL="http://127.0.0.1:$core"
    export AVALON_AGGREGATOR_A_URL="http://127.0.0.1:$a"
    export AVALON_AGGREGATOR_B_URL="http://127.0.0.1:$b"
    run_test aggregator/cross_shard avalon-server cross_shard
    printf '%s\n' "${RESULTS[@]}" >"$LOG_DIR/subshell-results"
    echo "$FAILED" >"$LOG_DIR/subshell-failed"
  )
  merge_subshell
}

group_internal_role() {
  # setup_schema in the test itself creates/migrates this schema; the server
  # needs it to exist first.
  new_schema test_internal_role_661 || return
  local p=$((BASE_PORT + 50))
  start_node internal-role test_internal_role_661 "$p" \
    AVALON_INTERNAL_ROLE_KEY=test-internal-role-key-661 || return
  (
    export DATABASE_URL="$BASE_DB_URL"
    export AVALON_INTERNAL_ROLE_SERVER_URL="http://127.0.0.1:$p"
    export AVALON_INTERNAL_ROLE_KEY=test-internal-role-key-661
    export AVALON_INTERNAL_ROLE_SERVER_PID="$LAST_PID"
    run_test internal-role/internal_role_protocol avalon-server internal_role_protocol
    printf '%s\n' "${RESULTS[@]}" >"$LOG_DIR/subshell-results"
    echo "$FAILED" >"$LOG_DIR/subshell-failed"
  )
  merge_subshell
}

group_gateway_only() {
  new_schema test_gateway_only_662 || return
  local c=$((BASE_PORT + 60)) g=$((BASE_PORT + 61))
  start_node gw-combined test_gateway_only_662 "$c" \
    AVALON_INTERNAL_ROLE_KEY=test-gateway-only-role-key-662 || return
  start_node gw-gateway test_gateway_only_662 "$g" AVALON_NODE_ROLES=gateway \
    AVALON_INDEXER_REMOTE_URL="http://127.0.0.1:$c" AVALON_REALTIME_URL="http://127.0.0.1:$c" \
    AVALON_INTERNAL_ROLE_KEY=test-gateway-only-role-key-662 || return
  (
    export DATABASE_URL="$BASE_DB_URL"
    export AVALON_COMBINED_SERVER_URL="http://127.0.0.1:$c"
    export AVALON_GATEWAY_ONLY_SERVER_URL="http://127.0.0.1:$g"
    run_test gateway-only/gateway_only_deployment avalon-server gateway_only_deployment
    printf '%s\n' "${RESULTS[@]}" >"$LOG_DIR/subshell-results"
    echo "$FAILED" >"$LOG_DIR/subshell-failed"
  )
  merge_subshell
}

group_realtime_proxy() {
  new_schema test_realtime_proxy_663 || return
  local rt=$((BASE_PORT + 70)) gw=$((BASE_PORT + 71))
  start_node rt-realtime test_realtime_proxy_663 "$rt" AVALON_NETWORK_ID=avalon-test-663 \
    AVALON_NODE_ROLES=realtime,indexer AVALON_ANNOUNCE_INTERVAL_SECS=1 \
    AVALON_BOOTSTRAP_PEERS="http://127.0.0.1:$gw" || return
  local rt_pid="$LAST_PID"
  start_node rt-gateway test_realtime_proxy_663 "$gw" AVALON_NETWORK_ID=avalon-test-663 \
    AVALON_NODE_ROLES=gateway,indexer AVALON_REALTIME_URL="http://127.0.0.1:$rt" \
    AVALON_ANNOUNCE_INTERVAL_SECS=1 AVALON_BOOTSTRAP_PEERS="http://127.0.0.1:$rt" || return
  (
    export DATABASE_URL="$(schema_url test_realtime_proxy_663)"
    export AVALON_SERVER_URL="http://127.0.0.1:$gw"
    export AVALON_REALTIME_ROLE_SERVER_URL="http://127.0.0.1:$rt"
    export AVALON_REALTIME_ROLE_SERVER_PID="$rt_pid"
    run_test realtime-proxy/realtime_proxy avalon-server realtime_proxy
    printf '%s\n' "${RESULTS[@]}" >"$LOG_DIR/subshell-results"
    echo "$FAILED" >"$LOG_DIR/subshell-failed"
  )
  merge_subshell
}

group_remote_settlement() {
  new_schema live_rs_auth || return
  new_schema live_rs_remote || return
  local auth=$((BASE_PORT + 90)) remote=$((BASE_PORT + 91))
  local submit_key=live-tests-submit-key
  start_node rs-auth live_rs_auth "$auth" AVALON_SETTLEMENT_SUBMIT_KEY="$submit_key" || return
  start_node rs-remote live_rs_remote "$remote" AVALON_SETTLEMENT_SUBMIT_KEY="$submit_key" \
    AVALON_SETTLEMENT_REMOTE_URL="http://127.0.0.1:$auth" \
    AVALON_MIRROR_PEERS="http://127.0.0.1:$auth" AVALON_MIRROR_POLL_INTERVAL_SECS=3 || return
  (
    export DATABASE_URL="$(schema_url live_rs_auth)"
    export AVALON_SETTLEMENT_SUBMIT_KEY="$submit_key"
    export AVALON_SERVER_URL="http://127.0.0.1:$auth"
    export AVALON_REMOTE_SETTLEMENT_SERVER_URL="http://127.0.0.1:$remote"
    export AVALON_REMOTE_SETTLEMENT_DATABASE_URL="$(schema_url live_rs_remote)"
    run_test remote-settlement/remote_settlement avalon-server remote_settlement
    printf '%s\n' "${RESULTS[@]}" >"$LOG_DIR/subshell-results"
    echo "$FAILED" >"$LOG_DIR/subshell-failed"
  )
  merge_subshell
}

group_remote_submit() {
  new_schema live_rsub || return
  local p=$((BASE_PORT + 95))
  start_node rsub live_rsub "$p" AVALON_SETTLEMENT_REMOTE_URL=http://127.0.0.1:9 || return
  (
    export DATABASE_URL="$(schema_url live_rsub)"
    export AVALON_SETTLEMENT_REMOTE_URL=http://127.0.0.1:9
    export AVALON_SERVER_URL="http://127.0.0.1:$p"
    run_test remote-submit/remote_submit_status avalon-server remote_submit_status
    printf '%s\n' "${RESULTS[@]}" >"$LOG_DIR/subshell-results"
    echo "$FAILED" >"$LOG_DIR/subshell-failed"
  )
  merge_subshell
}

group_settlement_only() {
  new_schema live_so_settle || return
  new_schema live_so_gateway || return
  local indexer=$((BASE_PORT + 98)) settle=$((BASE_PORT + 96)) gw=$((BASE_PORT + 97))
  local submit_key=live-tests-submit-key role_key=live-tests-role-key
  # A settlement-only node has no local indexer; it forwards to a combined
  # process on the same schema over the internal role protocol.
  start_node so-indexer live_so_settle "$indexer" AVALON_INTERNAL_ROLE_KEY="$role_key" || return
  start_node so-settle live_so_settle "$settle" AVALON_NODE_ROLES=settlement \
    AVALON_SETTLEMENT_SUBMIT_KEY="$submit_key" AVALON_INTERNAL_ROLE_KEY="$role_key" \
    AVALON_INDEXER_REMOTE_URL="http://127.0.0.1:$indexer" AVALON_REALTIME_URL="http://127.0.0.1:$indexer" || return
  start_node so-gateway live_so_gateway "$gw" AVALON_NODE_ROLES=gateway,indexer,realtime \
    AVALON_SETTLEMENT_REMOTE_URL="http://127.0.0.1:$settle" \
    AVALON_SETTLEMENT_SUBMIT_KEY="$submit_key" || return
  (
    export AVALON_SETTLEMENT_ONLY_SERVER_URL="http://127.0.0.1:$settle"
    export AVALON_GATEWAY_ONLY_SERVER_URL="http://127.0.0.1:$gw"
    run_test settlement-only/settlement_only avalon-server settlement_only
    printf '%s\n' "${RESULTS[@]}" >"$LOG_DIR/subshell-results"
    echo "$FAILED" >"$LOG_DIR/subshell-failed"
  )
  merge_subshell
}

group_topology() {
  new_schema live_topo_a || return
  new_schema live_topo_b || return
  new_schema live_topo_c || return
  local a=$((BASE_PORT + 60)) b=$((BASE_PORT + 61)) c=$((BASE_PORT + 62)) dead=1
  # A's active set is capped at its two bootstrap peers, so C (learned only
  # through B's gossip) stays a known-but-not-active peer.
  start_node topo-a live_topo_a "$a" AVALON_BOOTSTRAP_PEERS="http://127.0.0.1:$b,http://127.0.0.1:$dead" \
    AVALON_NODE_MAX_PEERS=2 AVALON_ANNOUNCE_INTERVAL_SECS=2 \
    AVALON_MIRROR_PEERS="core=http://127.0.0.1:$b" AVALON_MIRROR_POLL_INTERVAL_SECS=5 || return
  start_node topo-b live_topo_b "$b" AVALON_BOOTSTRAP_PEERS="http://127.0.0.1:$a,http://127.0.0.1:$c" \
    AVALON_ANNOUNCE_INTERVAL_SECS=2 || return
  start_node topo-c live_topo_c "$c" AVALON_BOOTSTRAP_PEERS="http://127.0.0.1:$b" \
    AVALON_ANNOUNCE_INTERVAL_SECS=2 || return
  (
    export AVALON_SERVER_URL="http://127.0.0.1:$a"
    export AVALON_TOPOLOGY_NODE_B_URL="http://127.0.0.1:$b"
    export AVALON_TOPOLOGY_NODE_C_URL="http://127.0.0.1:$c"
    export AVALON_TOPOLOGY_DEAD_PEER_URL="http://127.0.0.1:$dead"
    run_test topology/topology avalon-server topology
    printf '%s\n' "${RESULTS[@]}" >"$LOG_DIR/subshell-results"
    echo "$FAILED" >"$LOG_DIR/subshell-failed"
  )
  merge_subshell
}

group_topology_probe() {
  new_schema live_topology || return
  local a=$((BASE_PORT + 100)) b=$((BASE_PORT + 101)) s=$((BASE_PORT + 102)) l=$((BASE_PORT + 103))
  local u="http://127.0.0.1"
  start_node topo-a live_topology "$a" AVALON_ANNOUNCE_INTERVAL_SECS=1 AVALON_ALLOW_PRIVATE_PEERS=true \
    AVALON_BOOTSTRAP_PEERS="$u:$b,$u:$s" || return
  start_node topo-b live_topology "$b" AVALON_ANNOUNCE_INTERVAL_SECS=1 AVALON_ALLOW_PRIVATE_PEERS=true \
    AVALON_BOOTSTRAP_PEERS="$u:$a" || return
  start_node topo-strict live_topology "$s" AVALON_ANNOUNCE_INTERVAL_SECS=1 AVALON_ALLOW_PRIVATE_PEERS=false \
    AVALON_BOOTSTRAP_PEERS="$u:$a" || return
  start_node topo-limited live_topology "$l" AVALON_PROBE_RATE_LIMIT_PER_MINUTE=3 || return
  local cap=$((BASE_PORT + 104)) hole=$((BASE_PORT + 105))
  start_blackhole "$hole"
  start_node topo-cap live_topology "$cap" AVALON_ALLOW_PRIVATE_PEERS=true \
    AVALON_PROBE_MAX_CONCURRENT=1 || return
  (
    export TOPOLOGY_A_URL="$u:$a" TOPOLOGY_B_URL="$u:$b" TOPOLOGY_STRICT_URL="$u:$s" \
      TOPOLOGY_LIMITED_URL="$u:$l" TOPOLOGY_CAP_URL="$u:$cap" TOPOLOGY_BLACKHOLE_URL="$u:$hole"
    run_test topology-probe/topology_probe avalon-server topology_probe
    printf '%s\n' "${RESULTS[@]}" >"$LOG_DIR/subshell-results"
    echo "$FAILED" >"$LOG_DIR/subshell-failed"
  )
  merge_subshell
}

# Prints the given ports ordered by decreasing overlay distance to the first
# one, so a line wired in that order routes toward the first port at every hop.
line_order() {
  python3 - "$@" <<'PY'
import hashlib, sys
ports = sys.argv[1:]
key = lambda p: int(hashlib.sha256(f"http://127.0.0.1:{p}".encode()).hexdigest(), 16)
t = key(ports[0])
rest = sorted(ports[1:], key=lambda p: key(p) ^ t, reverse=True)
print(" ".join(rest + [ports[0]]))
PY
}

group_topology_trace() {
  new_schema live_topology_trace || return
  local u="http://127.0.0.1" b=$((BASE_PORT + 110))
  local -a line ring
  read -r -a line <<<"$(line_order $((b + 3)) $((b + 1)) $((b + 2)) "$b")"
  ring=("$((b + 4))" "$((b + 5))" "$((b + 6))" "$((b + 7))")
  local edge=$((b + 8)) hole=$((b + 9)) closed=$((b + 10)) strict=$((b + 11)) cap=$((b + 12)) limited=$((b + 13))
  local common=(AVALON_ANNOUNCE_INTERVAL_SECS=2 AVALON_ALLOW_PRIVATE_PEERS=true
    AVALON_TRACE_RATE_LIMIT_PER_MINUTE=100000)
  local i n left right peers
  for i in 0 1 2 3; do
    peers=""
    [ "$i" -gt 0 ] && peers="$u:${line[$((i - 1))]}"
    [ "$i" -lt 3 ] && peers="${peers:+$peers,}$u:${line[$((i + 1))]}"
    n=2; [ "$i" -eq 0 ] || [ "$i" -eq 3 ] && n=1
    start_node "trace-line-$i" live_topology_trace "${line[$i]}" "${common[@]}" \
      AVALON_BOOTSTRAP_PEERS="$peers" AVALON_NODE_MAX_PEERS=$n || return
  done
  for i in 0 1 2 3; do
    left="${ring[$(((i + 3) % 4))]}"; right="${ring[$(((i + 1) % 4))]}"
    start_node "trace-ring-$i" live_topology_trace "${ring[$i]}" "${common[@]}" \
      AVALON_BOOTSTRAP_PEERS="$u:$left,$u:$right" AVALON_NODE_MAX_PEERS=2 || return
  done
  start_blackhole "$hole"
  start_node trace-edge live_topology_trace "$edge" "${common[@]}" \
    AVALON_BOOTSTRAP_PEERS="$u:$hole,$u:$closed" AVALON_NODE_MAX_PEERS=2 || return
  start_node trace-strict live_topology_trace "$strict" AVALON_ANNOUNCE_INTERVAL_SECS=2 \
    AVALON_ALLOW_PRIVATE_PEERS=false AVALON_BOOTSTRAP_PEERS="$u:${line[0]}" \
    AVALON_NODE_MAX_PEERS=1 || return
  start_node trace-cap live_topology_trace "$cap" "${common[@]}" \
    AVALON_BOOTSTRAP_PEERS="$u:$hole" AVALON_NODE_MAX_PEERS=1 AVALON_TRACE_MAX_CONCURRENT=1 || return
  start_node trace-limited live_topology_trace "$limited" AVALON_ALLOW_PRIVATE_PEERS=true \
    AVALON_TRACE_RATE_LIMIT_PER_MINUTE=3 || return
  (
    export TRACE_LINE="$(IFS=,; echo "${line[*]}")" TRACE_RING="$(IFS=,; echo "${ring[*]}")"
    export TRACE_EDGE="$edge" TRACE_HOLE="$hole" TRACE_CLOSED="$closed" TRACE_STRICT="$strict" \
      TRACE_CAP="$cap" TRACE_LIMITED="$limited"
    run_test topology-trace/topology_trace avalon-server topology_trace
    printf '%s\n' "${RESULTS[@]}" >"$LOG_DIR/subshell-results"
    echo "$FAILED" >"$LOG_DIR/subshell-failed"
  )
  merge_subshell
}

group_peer_table_bounds() {
  new_schema live_ptb || return
  new_schema live_ptb_other || return
  local cap=$((BASE_PORT + 110)) real=$((BASE_PORT + 111)) strict=$((BASE_PORT + 112)) \
    limited=$((BASE_PORT + 113)) reach=$((BASE_PORT + 114)) other=$((BASE_PORT + 115))
  local u="http://127.0.0.1"
  start_node ptb-cap live_ptb "$cap" AVALON_NODE_MAX_KNOWN_PEERS=5 AVALON_ANNOUNCE_INTERVAL_SECS=1 \
    AVALON_BOOTSTRAP_PEERS="$u:$real" || return
  start_node ptb-real live_ptb "$real" AVALON_ANNOUNCE_INTERVAL_SECS=1 \
    AVALON_BOOTSTRAP_PEERS="$u:$cap" || return
  start_node ptb-strict live_ptb "$strict" AVALON_ALLOW_PRIVATE_PEERS=false \
    AVALON_ANNOUNCE_VERIFY_REACHABILITY=true || return
  start_node ptb-limited live_ptb "$limited" \
    AVALON_ANNOUNCE_NEW_PEERS_PER_SOURCE_PER_MINUTE=3 || return
  start_node ptb-reach live_ptb "$reach" AVALON_ANNOUNCE_VERIFY_REACHABILITY=true || return
  start_node ptb-other live_ptb_other "$other" AVALON_NETWORK_ID=avalon-test-882 || return
  (
    export PTB_CAP_URL="$u:$cap" PTB_REAL_URL="$u:$real" PTB_STRICT_URL="$u:$strict" \
      PTB_LIMITED_URL="$u:$limited" PTB_REACH_URL="$u:$reach" PTB_OTHER_NET_URL="$u:$other"
    run_test peer-table-bounds/peer_table_bounds avalon-server peer_table_bounds
    printf '%s\n' "${RESULTS[@]}" >"$LOG_DIR/subshell-results"
    echo "$FAILED" >"$LOG_DIR/subshell-failed"
  )
  merge_subshell
}

GROUPS_ALL=(core ledger-writers relay own-shard cross-shard-login aggregator internal-role gateway-only realtime-proxy remote-settlement remote-submit settlement-only topology topology-probe topology-trace peer-table-bounds)
SELECTED=("$@")
[ "${#SELECTED[@]}" -eq 0 ] && SELECTED=("${GROUPS_ALL[@]}")

cargo build -q -p avalon-server -p avalon-cli --bins --examples || exit 1
[ -x "$SERVER_BIN" ] || { echo "missing $SERVER_BIN" >&2; exit 1; }

for g in "${SELECTED[@]}"; do
  echo "== group: $g"
  case "$g" in
    core) group_core ;;
    ledger-writers) group_ledger_writers ;;
    relay) group_relay ;;
    own-shard) group_own_shard ;;
    cross-shard-login) group_cross_shard_login ;;
    aggregator) group_aggregator ;;
    internal-role) group_internal_role ;;
    gateway-only) group_gateway_only ;;
    realtime-proxy) group_realtime_proxy ;;
    remote-settlement) group_remote_settlement ;;
    remote-submit) group_remote_submit ;;
    settlement-only) group_settlement_only ;;
    topology) group_topology ;;
    topology-probe) group_topology_probe ;;
    topology-trace) group_topology_trace ;;
    peer-table-bounds) group_peer_table_bounds ;;
    *) echo "unknown group: $g" >&2; FAILED=1 ;;
  esac
  stop_all
done

echo
printf '%s\n' "${RESULTS[@]}"
echo "logs: $LOG_DIR"
exit "$FAILED"
