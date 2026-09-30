#!/usr/bin/env bash
# NAT scenario suite: real avalon-server processes inside the NAT lab
# (scripts/nat-lab.sh), one throwaway Postgres schema per node.
#
# usage: sudo scripts/nat-scenarios.sh [scenario ...]     (default: all scenarios)
#   scenarios: public full-cone-direct restricted-cone-detected-public relayed-port-restricted
#              relayed-symmetric relayed-no-inbound punch-port-restricted punch-symmetric-fallback
#              outbound-only relay-failover
#
# See docs/projects/backend-server/for-maintainers/nat-scenarios.md for what each
# scenario proves and how to run the suite as a live drill.
#
# Needs root, ip netns, nftables, jq, curl, xxd and a Postgres reachable at the
# DATABASE_URL in AVALON_ENV_FILE (default .env). A veth pair gives the lab's public
# segment a route to the host, so nodes in the lab reach that Postgres at 10.99.0.250.
#
# env: AVALON_ENV_FILE   .env to read DATABASE_URL from
#      NAT_SERVER_BIN    avalon-server binary to run (default target/debug/avalon-server)
#      NAT_LAB_KEEP=1    keep logs and schemas after the run
#      NAT_LAB_TIMEOUT   seconds any single wait may take (default 90)

set -uo pipefail

for tool in jq curl xxd ip nft; do
  command -v "$tool" >/dev/null 2>&1 || { echo "nat-scenarios.sh needs $tool" >&2; exit 2; }
done
[ "$(id -u)" -eq 0 ] || exec sudo -E "$0" "$@"

HARNESS_LOG_PREFIX=avalon-nat-scenarios
BASE_PORT=0
KEEP_SCHEMAS="${NAT_LAB_KEEP:-}"
# shellcheck source=lib/harness.sh
. "$(dirname "${BASH_SOURCE[0]}")/lib/harness.sh"

SERVER_BIN="${NAT_SERVER_BIN:-$SERVER_BIN}"
LAB="$ROOT/scripts/nat-lab.sh"
WAIT="${NAT_LAB_TIMEOUT:-90}"
HOST_IP=10.99.0.250
NAT_TYPES="full-cone restricted-cone port-restricted symmetric no-inbound"

cleanup() {
  stop_all
  "$LAB" down-all >/dev/null 2>&1
  ip link del avdb0 2>/dev/null
  [ "${NAT_LAB_KEEP:-}" = 1 ] || rm -rf "$LOG_DIR"
}
trap cleanup EXIT

lab_start() {
  "$LAB" down-all >/dev/null 2>&1
  ip link del avdb0 2>/dev/null
  "$LAB" up-inet >/dev/null || return 1
  ip link add avdb0 type veth peer name avdb1 || return 1
  ip link set avdb1 netns avlab-inet
  ip -n avlab-inet link set avdb1 master avlbr0 up
  ip addr add "$HOST_IP/24" dev avdb0 && ip link set avdb0 up
}

lab_reset() {
  stop_all
  "$LAB" down-all >/dev/null 2>&1
  ip link del avdb0 2>/dev/null
}

# lab_db_url <schema>: the harness schema URL with the host swapped for the veth address.
lab_db_url() {
  schema_url "$1" | sed -E "s#@[^/:]+#@$HOST_IP#"
}

# node <name> <ns> <ip> [VAR=value ...]: one server in namespace <ns> at <ip>.
# The dev binary loads the workspace .env from a compiled-in path, so everything that
# file could set is set here explicitly (empty unsets it).
node() {
  local name="$1" ns="$2" ip="$3"
  shift 3
  local schema="live_nat_${name//-/_}"
  new_schema "$schema" || return 1
  mkdir -p "$LOG_DIR/cwd-$name" "$LOG_DIR/home-$name"
  "$LAB" exec "$ns" -- env -i -C "$LOG_DIR/cwd-$name" PATH="$PATH" HOME="$LOG_DIR/home-$name" \
    DATABASE_URL="$(lab_db_url "$schema")" \
    AVALON_SERVER_ADDR="$ip:8080" AVALON_NODE_URL="http://$ip:8080" \
    AVALON_DATA_DIR="$LOG_DIR/data-$name" AVALON_NETWORK_ID=avalon-dev-lan \
    AVALON_WEBAUTHN_RP_ID=localhost AVALON_WEBAUTHN_ORIGIN=http://localhost AVALON_HUB_ORIGIN=http://localhost \
    AVALON_DHT_ENABLED=true AVALON_LIBP2P_LISTEN_ADDR="/ip4/$ip/tcp/4001" \
    AVALON_DHT_BOOTSTRAP_SCAN_INTERVAL_SECS=2 AVALON_ANNOUNCE_INTERVAL_SECS=5 \
    AVALON_ALLOW_PRIVATE_PEERS=true AVALON_AUTONAT_ALLOW_PRIVATE_DIALBACK=true \
    AVALON_ANNOUNCE_VERIFY_REACHABILITY=false AVALON_RATE_LIMIT_PER_MINUTE=1000000 \
    AVALON_LIBP2P_IDENTITY_KEY="$(head -c32 /dev/urandom | xxd -p -c64)" \
    AVALON_BOOTSTRAP_PEERS= AVALON_MIRROR_PEERS= AVALON_MIRROR_ALL_DISCOVERED_SHARDS=false \
    AVALON_KNOWN_SHARDS= AVALON_OWN_SHARD_ID= AVALON_REDIS_URL= AVALON_HOST_IP= \
    AVALON_SETTLEMENT_REMOTE_URLS= AVALON_SETTLEMENT_REMOTE_URL= AVALON_SETTLEMENT_VERIFY_KEY= \
    AVALON_SETTLEMENT_SIGNING_KEY= AVALON_SETTLEMENT_SUBMIT_KEY= AVALON_LIBP2P_EXTERNAL_ADDR= \
    RUST_LOG="${NAT_LAB_LOG:-info}" \
    "$@" "$SERVER_BIN" >"$LOG_DIR/$name.log" 2>&1 &
  LAST_PID=$!
  PIDS+=("$LAST_PID")
}

status() { "$LAB" exec "$1" -- curl -s -m 5 "http://$2:8080/nodes/status"; }

# wait_status <ns> <ip> <jq filter> [what]: waits until the filter is true on /nodes/status.
wait_status() {
  local ns="$1" ip="$2" filter="$3" what="${4:-$3}" i
  for i in $(seq 1 $((WAIT / 2))); do
    if status "$ns" "$ip" | jq -e "$filter" >/dev/null 2>&1; then return 0; fi
    sleep 2
  done
  echo "    timed out after ${WAIT}s waiting for: $what" >&2
  status "$ns" "$ip" | jq -c '{reachability,connectivity,relay_reservations:(.relay_reservations|length),punched_peers,hole_punches}' >&2
  return 1
}

# wait_topology <ns> <ip> <jq filter> [what]: waits until the filter is true on /nodes/topology.
wait_topology() {
  local ns="$1" ip="$2" filter="$3" what="${4:-$3}" i
  for i in $(seq 1 $((WAIT / 2))); do
    if "$LAB" exec "$ns" -- curl -s -m 5 "http://$ip:8080/nodes/topology" | jq -e "$filter" >/dev/null 2>&1; then
      return 0
    fi
    sleep 2
  done
  echo "    timed out after ${WAIT}s waiting for: $what" >&2
  "$LAB" exec "$ns" -- curl -s -m 5 "http://$ip:8080/nodes/topology" \
    | jq -c '[.neighbors[]|{base_url,connectivity,latency:(.latency|{samples,failed_recent,path})}]' >&2
  return 1
}

# peer_id <node name>: the libp2p peer id a node logged at startup.
peer_id() {
  local i id
  for i in $(seq 1 30); do
    id=$(grep -o 'local_peer_id=[A-Za-z0-9]*' "$LOG_DIR/$1.log" 2>/dev/null | head -1 | cut -d= -f2)
    [ -n "$id" ] && { echo "$id"; return 0; }
    sleep 1
  done
  return 1
}

ready() { wait_status "$1" "$2" '.protocol_version' "$2 to answer"; }

# A public seed pair every scenario uses: the relay and a second public node, both
# reachable directly on the lab's public segment.
public_pair() {
  node relay inet 10.99.0.1 AVALON_LIBP2P_EXTERNAL_ADDR=/ip4/10.99.0.1/tcp/4001 AVALON_RELAY_SERVER_ENABLED=true
  node seed inet 10.99.0.2 AVALON_LIBP2P_EXTERNAL_ADDR=/ip4/10.99.0.2/tcp/4001 \
    AVALON_BOOTSTRAP_PEERS=http://10.99.0.1:8080
  ready inet 10.99.0.1 && ready inet 10.99.0.2
}

relay_addr() { echo "/ip4/10.99.0.1/tcp/4001/p2p/$(peer_id relay)"; }

# --- scenarios ---------------------------------------------------------------------

scenario_public() {
  public_pair || return 1
  wait_status inet 10.99.0.2 '.reachability == "public" and .connectivity == "direct"' \
    "the public seed to report direct"
}

# A node behind each kind of NAT is found private, reserves a relay slot and reports relayed.
relayed_behind() {
  local type="$1"
  "$LAB" up home1 "$type" >/dev/null || return 1
  public_pair || return 1
  node home1 home1 10.1.0.2 AVALON_BOOTSTRAP_PEERS=http://10.99.0.2:8080
  wait_status home1 10.1.0.2 \
    '.reachability == "private" and .connectivity == "relayed" and (.relay_reservations|length) == 1' \
    "a node behind a $type NAT to report relayed with a reservation" || return 1
  wait_status home1 10.1.0.2 '(.relayed_listen_addrs[0] // "") | contains("/p2p-circuit/")' \
    "the relayed address to be advertised"
}

scenario_relayed-port-restricted() { relayed_behind port-restricted; }
scenario_relayed-symmetric() { relayed_behind symmetric; }
scenario_relayed-no-inbound() { relayed_behind no-inbound; }

# Two nodes behind NATs find each other through the relay and, when the NATs allow it,
# upgrade to a direct connection.
punch_between() {
  local type_a="$1" type_b="$2" expect="$3"
  "$LAB" up home1 "$type_a" >/dev/null || return 1
  "$LAB" up home2 "$type_b" >/dev/null || return 1
  public_pair || return 1
  node home1 home1 10.1.0.2 AVALON_BOOTSTRAP_PEERS=http://10.99.0.2:8080
  node home2 home2 10.2.0.2 AVALON_BOOTSTRAP_PEERS=http://10.99.0.2:8080
  wait_status home1 10.1.0.2 '.connectivity == "relayed"' "home1 to be relayed" || return 1
  wait_status home2 10.2.0.2 '.connectivity == "relayed" or .connectivity == "nat_traversed"' "home2 to be relayed" || return 1
  if [ "$expect" = punched ]; then
    wait_status home1 10.1.0.2 '.connectivity == "nat_traversed" and (.punched_peers|length) == 1' \
      "home1 to hole punch to home2 ($type_a / $type_b)" || return 1
    wait_status home1 10.1.0.2 '[.hole_punches[]|select(.succeeded)]|length >= 1' "a successful punch to be recorded"
  else
    # A punch that cannot work fails, is recorded, and leaves the relayed path in use.
    wait_status home1 10.1.0.2 '[.hole_punches[]|select(.succeeded|not)]|length >= 1' \
      "a failed punch ($type_a / $type_b) to be recorded" || return 1
    wait_status home1 10.1.0.2 '.connectivity == "relayed" and (.punched_peers|length) == 0' \
      "home1 to stay on the relay"
  fi
}

# A punch takes a while: the higher peer id waits before dialing, then up to three attempts.
scenario_punch-port-restricted() { WAIT=150 punch_between port-restricted port-restricted punched; }
scenario_punch-symmetric-fallback() { WAIT=150 punch_between symmetric symmetric relayed; }

# The lab's full-cone NAT forwards every inbound port, so AutoNAT finds the node reachable.
scenario_full-cone-direct() {
  "$LAB" up home1 full-cone >/dev/null || return 1
  public_pair || return 1
  node home1 home1 10.1.0.2 AVALON_BOOTSTRAP_PEERS=http://10.99.0.2:8080
  wait_status home1 10.1.0.2 '.reachability == "public" and .connectivity == "direct"' \
    "a node behind a full-cone NAT to report direct"
}

# Known limitation: AutoNAT asks a peer the node has already talked to to dial back, and an
# address-restricted NAT lets that peer's address through, so the node is reported public
# although a peer it never contacted cannot reach it.
scenario_restricted-cone-detected-public() {
  "$LAB" up home1 restricted-cone >/dev/null || return 1
  public_pair || return 1
  node home1 home1 10.1.0.2 AVALON_BOOTSTRAP_PEERS=http://10.99.0.2:8080
  wait_status home1 10.1.0.2 '.reachability == "public"' \
    "AutoNAT to report a restricted-cone node public"
}

# A node reachable only through a relay still answers the seed's announces, which travel over
# a libp2p stream because its HTTP URL cannot be reached.
scenario_stream-announce-relayed() {
  "$LAB" up home1 symmetric >/dev/null || return 1
  public_pair || return 1
  node home1 home1 10.1.0.2 AVALON_BOOTSTRAP_PEERS=http://10.99.0.1:8080,http://10.99.0.2:8080
  wait_status home1 10.1.0.2 '.connectivity == "relayed"' "home1 to be relayed" || return 1
  WAIT=150 wait_topology inet 10.99.0.2 \
    '[.neighbors[]|select(.base_url == "http://10.1.0.2:8080")|.latency.samples] | (.[0] // 0) >= 1' \
    "the seed to complete an announce to the relayed node"
}

# With no relay to use, a private node still participates and reports outbound_only.
scenario_outbound-only() {
  "$LAB" up home1 no-inbound >/dev/null || return 1
  node seed inet 10.99.0.2 AVALON_LIBP2P_EXTERNAL_ADDR=/ip4/10.99.0.2/tcp/4001
  node relay inet 10.99.0.1 AVALON_LIBP2P_EXTERNAL_ADDR=/ip4/10.99.0.1/tcp/4001 \
    AVALON_BOOTSTRAP_PEERS=http://10.99.0.2:8080
  ready inet 10.99.0.1 && ready inet 10.99.0.2 || return 1
  node home1 home1 10.1.0.2 AVALON_BOOTSTRAP_PEERS=http://10.99.0.1:8080,http://10.99.0.2:8080
  wait_status home1 10.1.0.2 \
    '.reachability == "private" and .connectivity == "outbound_only" and (.relay_reservations|length) == 0' \
    "a private node with no relay to report outbound_only"
}

# Losing the relay a node reserved on moves it to a second relay.
scenario_relay-failover() {
  "$LAB" up home1 symmetric >/dev/null || return 1
  public_pair || return 1
  ip -n avlab-inet addr add 10.99.0.3/24 dev avlbr0
  node relay2 inet 10.99.0.3 AVALON_LIBP2P_EXTERNAL_ADDR=/ip4/10.99.0.3/tcp/4001 AVALON_RELAY_SERVER_ENABLED=true \
    AVALON_BOOTSTRAP_PEERS=http://10.99.0.1:8080
  local second="$LAST_PID"
  ready inet 10.99.0.3 || return 1
  node home1 home1 10.1.0.2 AVALON_BOOTSTRAP_PEERS=http://10.99.0.1:8080,http://10.99.0.2:8080 \
    AVALON_RELAY_CLIENT_MAX_RESERVATIONS=1 \
    AVALON_RELAY_ADDRS="$(relay_addr),/ip4/10.99.0.3/tcp/4001/p2p/$(peer_id relay2)"
  wait_status home1 10.1.0.2 '.connectivity == "relayed" and (.relay_reservations|length) == 1' \
    "the first reservation" || return 1
  local held
  held=$(status home1 10.1.0.2 | jq -r '.relay_reservations[0].relay_peer_id')
  # Stop whichever relay holds the reservation.
  if [ "$held" = "$(peer_id relay)" ]; then kill "${PIDS[0]}"; else kill "$second"; fi
  wait_status home1 10.1.0.2 \
    ".connectivity == \"relayed\" and (.relay_reservations|length) == 1 and .relay_reservations[0].relay_peer_id != \"$held\"" \
    "the reservation to move to the other relay"
}

# dump_logs: the lines of each node's log that explain a failed wait.
dump_logs() {
  local f
  for f in "$LOG_DIR"/*.log; do
    [ -e "$f" ] || continue
    echo "--- $(basename "$f")"
    grep -E "AutoNAT|reachab|relay|reservation|hole punch|WARN|ERROR|panicked" "$f" \
      | grep -v "mirror_watcher\|node-announce: http://192" | cut -c1-260 | tail -${NAT_LAB_DUMP_LINES:-25}
  done
}

# --- runner ------------------------------------------------------------------------

ALL="public stream-announce-relayed full-cone-direct restricted-cone-detected-public punch-port-restricted punch-symmetric-fallback relayed-port-restricted relayed-symmetric relayed-no-inbound outbound-only relay-failover"
SCENARIOS=("$@")
[ ${#SCENARIOS[@]} -gt 0 ] || read -r -a SCENARIOS <<<"$ALL"

FAILED=0
for s in "${SCENARIOS[@]}"; do
  if ! declare -F "scenario_$s" >/dev/null; then echo "unknown scenario: $s" >&2; exit 2; fi
  echo "== $s"
  lab_reset
  lab_start || { echo "  lab setup failed" >&2; FAILED=1; continue; }
  if "scenario_$s"; then
    echo "   PASS"
    RESULTS+=("PASS $s")
  else
    dump_logs
    echo "   FAIL (logs: $LOG_DIR)"
    RESULTS+=("FAIL $s")
    FAILED=1
    NAT_LAB_KEEP=1
  fi
done
lab_reset

echo
printf '%s\n' "${RESULTS[@]}"
exit "$FAILED"
