#!/usr/bin/env bash
# NAT scenario suite: real avalon-server processes inside the NAT lab
# (scripts/nat-lab.sh), one throwaway Postgres schema per node.
#
# usage: sudo scripts/nat-scenarios.sh [scenario ...]     (default: all scenarios)
#   scenarios: public full-cone-direct relayed-port-restricted relayed-restricted-cone
#              relayed-symmetric relayed-no-inbound punch-port-restricted punch-symmetric-fallback
#              outbound-only url-less-admission url-less-participation url-less-credential
#              url-less-shard-author relay-failover relay-ranking relay-reselect
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
#      NAT_LAB_EXTRA     extra VAR=value settings (space separated) for every node
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
    "$@" ${NAT_LAB_EXTRA:-} "$SERVER_BIN" >"$LOG_DIR/$name.log" 2>&1 &
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

# wait_probe_path <ns> <ip> <target url> <path> [what]: waits until POST /nodes/probe from the
# node to a known peer succeeds and reports that path type.
wait_probe_path() {
  local ns="$1" ip="$2" target="$3" want="$4" what="${5:-a $4 probe path}" i out=""
  for i in $(seq 1 $((WAIT / 2))); do
    out=$("$LAB" exec "$ns" -- curl -s -m 8 -H 'content-type: application/json' \
      -d "{\"target\":\"$target\"}" "http://$ip:8080/nodes/probe")
    if echo "$out" | jq -e ".ok == true and .path == \"$want\"" >/dev/null 2>&1; then return 0; fi
    sleep 2
  done
  echo "    timed out after ${WAIT}s waiting for: $what; last probe: $out" >&2
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
  "$LAB" up-public relay 10.99.0.101 >/dev/null && "$LAB" up-public seed 10.99.0.102 >/dev/null || return 1
  node relay relay 10.99.0.101 AVALON_LIBP2P_EXTERNAL_ADDR=/ip4/10.99.0.101/tcp/4001 AVALON_RELAY_SERVER_ENABLED=true
  node seed seed 10.99.0.102 AVALON_LIBP2P_EXTERNAL_ADDR=/ip4/10.99.0.102/tcp/4001 \
    AVALON_BOOTSTRAP_PEERS=http://10.99.0.101:8080
  ready relay 10.99.0.101 && ready seed 10.99.0.102
}

relay_addr() { echo "/ip4/10.99.0.101/tcp/4001/p2p/$(peer_id relay)"; }

# --- scenarios ---------------------------------------------------------------------

scenario_public() {
  public_pair || return 1
  wait_status seed 10.99.0.102 '.reachability == "public" and .connectivity == "direct"' \
    "the public seed to report direct"
}

# A node behind each kind of NAT is found private, reserves a relay slot and reports relayed.
relayed_behind() {
  local type="$1"
  "$LAB" up home1 "$type" >/dev/null || return 1
  public_pair || return 1
  node home1 home1 10.1.0.2 AVALON_BOOTSTRAP_PEERS=http://10.99.0.102:8080
  wait_status home1 10.1.0.2 \
    '.reachability == "private" and .connectivity == "relayed" and (.relay_reservations|length) == 1' \
    "a node behind a $type NAT to report relayed with a reservation" || return 1
  wait_status home1 10.1.0.2 '(.relayed_listen_addrs[0] // "") | contains("/p2p-circuit/")' \
    "the relayed address to be advertised"
}

scenario_relayed-port-restricted() { relayed_behind port-restricted; }
scenario_relayed-restricted-cone() { relayed_behind restricted-cone; }
scenario_relayed-symmetric() { relayed_behind symmetric; }
scenario_relayed-no-inbound() { relayed_behind no-inbound; }

# Two nodes behind NATs find each other through the relay and, when the NATs allow it,
# upgrade to a direct connection.
punch_between() {
  local type_a="$1" type_b="$2" expect="$3"
  "$LAB" up home1 "$type_a" >/dev/null || return 1
  "$LAB" up home2 "$type_b" >/dev/null || return 1
  public_pair || return 1
  node home1 home1 10.1.0.2 AVALON_BOOTSTRAP_PEERS=http://10.99.0.102:8080
  node home2 home2 10.2.0.2 AVALON_BOOTSTRAP_PEERS=http://10.99.0.102:8080
  wait_status home1 10.1.0.2 '.connectivity == "relayed"' "home1 to be relayed" || return 1
  wait_status home2 10.2.0.2 '.connectivity == "relayed" or .connectivity == "nat_traversed"' "home2 to be relayed" || return 1
  if [ "$expect" = punched ]; then
    wait_status home1 10.1.0.2 '.connectivity == "nat_traversed" and (.punched_peers|length) == 1' \
      "home1 to hole punch to home2 ($type_a / $type_b)" || return 1
    wait_status home1 10.1.0.2 '[.hole_punches[]|select(.succeeded)]|length >= 1' "a successful punch to be recorded" || return 1
    wait_probe_path home1 10.1.0.2 http://10.99.0.101:8080 direct "a probe of the relay to report a direct path"
  else
    # A punch that cannot work fails, is recorded, and leaves the relayed path in use. The node
    # with the lower peer id starts the punch, so either one may record it.
    local i failed=""
    for i in $(seq 1 $((WAIT / 2))); do
      status home1 10.1.0.2 | jq -e '[.hole_punches[]|select(.succeeded|not)]|length >= 1' >/dev/null 2>&1 && failed=1
      status home2 10.2.0.2 | jq -e '[.hole_punches[]|select(.succeeded|not)]|length >= 1' >/dev/null 2>&1 && failed=1
      [ -n "$failed" ] && break
      sleep 2
    done
    [ -n "$failed" ] || { echo "    timed out after ${WAIT}s waiting for: a failed punch ($type_a / $type_b) to be recorded" >&2; return 1; }
    wait_status home1 10.1.0.2 '.connectivity == "relayed" and (.punched_peers|length) == 0' \
      "home1 to stay on the relay" || return 1
    wait_probe_path home1 10.1.0.2 http://10.99.0.101:8080 direct "a probe of the relay to report a direct path"
  fi
}

# A punch takes a while: the higher peer id waits before dialing, then up to three attempts.
scenario_punch-port-restricted() { WAIT=150 punch_between port-restricted port-restricted punched; }
scenario_punch-symmetric-fallback() { WAIT=150 punch_between symmetric symmetric relayed; }

# The lab's full-cone NAT forwards every inbound port, so AutoNAT finds the node reachable.
scenario_full-cone-direct() {
  "$LAB" up home1 full-cone >/dev/null || return 1
  public_pair || return 1
  node home1 home1 10.1.0.2 AVALON_BOOTSTRAP_PEERS=http://10.99.0.102:8080
  wait_status home1 10.1.0.2 '.reachability == "public" and .connectivity == "direct"' \
    "a node behind a full-cone NAT to report direct"
}

# With no relay to use, a private node still participates and reports outbound_only.
scenario_outbound-only() {
  "$LAB" up home1 no-inbound >/dev/null || return 1
  "$LAB" up-public relay 10.99.0.101 >/dev/null && "$LAB" up-public seed 10.99.0.102 >/dev/null || return 1
  node seed seed 10.99.0.102 AVALON_LIBP2P_EXTERNAL_ADDR=/ip4/10.99.0.102/tcp/4001
  node relay relay 10.99.0.101 AVALON_LIBP2P_EXTERNAL_ADDR=/ip4/10.99.0.101/tcp/4001 \
    AVALON_BOOTSTRAP_PEERS=http://10.99.0.102:8080
  ready relay 10.99.0.101 && ready seed 10.99.0.102 || return 1
  node home1 home1 10.1.0.2 AVALON_BOOTSTRAP_PEERS=http://10.99.0.101:8080,http://10.99.0.102:8080
  wait_status home1 10.1.0.2 \
    '.reachability == "private" and .connectivity == "outbound_only" and (.relay_reservations|length) == 0' \
    "a private node with no relay to report outbound_only"
}

# discover <ns> <ip>: that node's /nodes/discover.
discover() { "$LAB" exec "$1" -- curl -s -m 5 "http://$2:8080/nodes/discover"; }

# topology <ns> <ip>: that node's /nodes/topology.
topology() { "$LAB" exec "$1" -- curl -s -m 5 "http://$2:8080/nodes/topology"; }

# wait_json <what> <jq filter> <cmd...>: waits until the filter is true on the command's output.
wait_json() {
  local what="$1" filter="$2" i
  shift 2
  for i in $(seq 1 $((WAIT / 2))); do
    if "$@" | jq -e "$filter" >/dev/null 2>&1; then return 0; fi
    sleep 2
  done
  echo "    timed out after ${WAIT}s waiting for: $what" >&2
  "$@" | jq -c '(.peers // .known // [])[0:6] | map({base_url, connectivity})' >&2
  return 1
}

# A node behind a no-inbound NAT with no AVALON_NODE_URL announces as p2p://<peer id>. With
# reachability verification on, its neighbors admit it from the authenticated stream and
# describe it; an announce for the same id over plain HTTP stores nothing.
scenario_url-less-admission() {
  "$LAB" up home1 no-inbound >/dev/null || return 1
  "$LAB" up-public relay 10.99.0.101 >/dev/null && "$LAB" up-public seed 10.99.0.102 >/dev/null || return 1
  local verify=AVALON_ANNOUNCE_VERIFY_REACHABILITY=true
  node seed seed 10.99.0.102 AVALON_LIBP2P_EXTERNAL_ADDR=/ip4/10.99.0.102/tcp/4001 "$verify"
  node relay relay 10.99.0.101 AVALON_LIBP2P_EXTERNAL_ADDR=/ip4/10.99.0.101/tcp/4001 \
    AVALON_BOOTSTRAP_PEERS=http://10.99.0.102:8080 "$verify"
  ready relay 10.99.0.101 && ready seed 10.99.0.102 || return 1
  node home1 home1 10.1.0.2 AVALON_NODE_URL= "$verify" \
    AVALON_BOOTSTRAP_PEERS=http://10.99.0.101:8080,http://10.99.0.102:8080
  local id url
  wait_status home1 10.1.0.2 '.connectivity == "outbound_only"' "home1 to report outbound_only" || return 1
  id=$(status home1 10.1.0.2 | jq -r .libp2p_peer_id)
  url="p2p://$id"

  local entry=".peers[] | select(.base_url == \"$url\" and .libp2p_peer_id == \"$id\" and .connectivity == \"outbound_only\")"
  wait_json "the seed to list home1 in /nodes/discover" "[$entry] | length == 1" discover seed 10.99.0.102 || return 1
  wait_json "the relay to list home1 in /nodes/discover" "[$entry] | length == 1" discover relay 10.99.0.101 || return 1
  wait_json "the seed's topology to describe home1" \
    "[(.known + .neighbors)[] | select(.base_url == \"$url\" and .connectivity == \"outbound_only\")] | length == 1" \
    topology seed 10.99.0.102 || return 1
  wait_json "home1's topology to name itself and both neighbors" \
    ".self.base_url == \"$url\" and (.neighbors | length) >= 2" topology home1 10.1.0.2 || return 1

  echo "    $url listed by seed: $(discover seed 10.99.0.102 | jq "[$entry] | length")," \
    "relay: $(discover relay 10.99.0.101 | jq "[$entry] | length")," \
    "home1 neighbors: $(topology home1 10.1.0.2 | jq '.neighbors | length')"

  # Claiming the relay's id over plain HTTP proves nothing, so nothing is stored for it.
  local other
  other=$(status relay 10.99.0.101 | jq -r .libp2p_peer_id)
  "$LAB" exec seed -- curl -s -m 5 -o /dev/null -X POST "http://10.99.0.102:8080/nodes/announce" \
    -H 'content-type: application/json' \
    -d "{\"base_url\":\"p2p://$other\",\"libp2p_peer_id\":\"$other\",\"roles\":[\"combined\"],\"protocol_version\":\"$(status seed 10.99.0.102 | jq -r .protocol_version)\",\"network_id\":\"avalon-dev-lan\",\"coordinate\":{\"vector\":[0,0,0],\"height\":0.01,\"error\":1.0}}"
  sleep 2
  if discover seed 10.99.0.102 | jq -e "[.peers[] | select(.base_url == \"p2p://$other\")] | length > 0" >/dev/null; then
    echo "    an unauthenticated p2p:// announce was stored" >&2
    return 1
  fi
}

# post_json <ns> <ip> <path> <body>: POSTs a JSON body to that node and prints the response.
post_json() {
  "$LAB" exec "$1" -- curl -s -m 15 -H 'content-type: application/json' -d "$4" "http://$2:8080$3"
}

# wait_post <what> <jq filter> <ns> <ip> <path> <body>: waits until the filter is true on the response.
wait_post() {
  local what="$1" filter="$2" i out=""
  shift 2
  for i in $(seq 1 $((WAIT / 2))); do
    out=$(post_json "$@")
    if echo "$out" | jq -e "$filter" >/dev/null 2>&1; then return 0; fi
    sleep 2
  done
  echo "    timed out after ${WAIT}s waiting for: $what; last response: $out" >&2
  return 1
}

# lab_get <ns> <url>: GET a URL from inside a namespace.
lab_get() { "$LAB" exec "$1" -- curl -s -m 8 "$2"; }

# register_body <slug>: the JSON for POST /integrations with a fresh key.
register_body() {
  jq -nc --arg slug "$1" --arg key "$(head -c32 /dev/urandom | base64)" \
    '{slug:$slug,name:$slug,owner_name:"lab",requested_capabilities:[],initial_key:{algorithm:"ed25519",public_key:$key}}'
}

# A node with no AVALON_NODE_URL does its whole job over connections it opened: a neighbor
# probes and traces it over the stream, it mirrors a source over outbound HTTP, and a write
# it authors reaches a shard authority over outbound HTTP.
scenario_url-less-participation() {
  "$LAB" up home1 no-inbound >/dev/null || return 1
  "$LAB" up-public relay 10.99.0.101 >/dev/null && "$LAB" up-public seed 10.99.0.102 >/dev/null \
    && "$LAB" up-public shard 10.99.0.103 >/dev/null || return 1
  local verify=AVALON_ANNOUNCE_VERIFY_REACHABILITY=true key=lab-submit-key
  node seed seed 10.99.0.102 AVALON_LIBP2P_EXTERNAL_ADDR=/ip4/10.99.0.102/tcp/4001 "$verify"
  node relay relay 10.99.0.101 AVALON_LIBP2P_EXTERNAL_ADDR=/ip4/10.99.0.101/tcp/4001 \
    AVALON_BOOTSTRAP_PEERS=http://10.99.0.102:8080 "$verify"
  node shard shard 10.99.0.103 AVALON_LIBP2P_EXTERNAL_ADDR=/ip4/10.99.0.103/tcp/4001 \
    AVALON_BOOTSTRAP_PEERS=http://10.99.0.102:8080 AVALON_OWN_SHARD_ID=game:urllessapp \
    AVALON_SETTLEMENT_SUBMIT_KEY=$key "$verify"
  ready relay 10.99.0.101 && ready seed 10.99.0.102 && ready shard 10.99.0.103 || return 1
  node home1 home1 10.1.0.2 AVALON_NODE_URL= "$verify" AVALON_MIRROR_ALL_DISCOVERED_SHARDS=true AVALON_MIRROR_POLL_INTERVAL_SECS=5 \
    AVALON_BOOTSTRAP_PEERS=http://10.99.0.101:8080,http://10.99.0.102:8080 \
    AVALON_OWN_SHARD_ID=game:urllessapp AVALON_SETTLEMENT_REMOTE_URLS=game:urllessapp=http://10.99.0.103:8080 \
    AVALON_SETTLEMENT_SUBMIT_KEY=$key
  local id url
  wait_status home1 10.1.0.2 '.connectivity == "outbound_only"' "home1 to report outbound_only" || return 1
  id=$(status home1 10.1.0.2 | jq -r .libp2p_peer_id)
  url="p2p://$id"

  # Both public neighbors list it and measure a latency over the path they share.
  local nb="[.neighbors[] | select(.base_url == \"$url\" and .connectivity == \"outbound_only\" and .latency.samples >= 1 and .latency.path != null)] | length == 1"
  wait_json "the relay's topology to measure home1" "$nb" topology relay 10.99.0.101 || return 1
  wait_json "the shard node's topology to measure home1" "$nb" topology shard 10.99.0.103 || return 1
  wait_json "the shard node's /nodes/peers to list home1" \
    "[.[] | select(.base_url == \"$url\" and .libp2p_peer_id == \"$id\")] | length == 1" \
    "$LAB" exec shard -- curl -s -m 5 http://10.99.0.103:8080/nodes/peers || return 1

  # A neighbor probes and traces it over the authenticated stream.
  wait_post "a probe of home1 from the seed" '.ok == true and (.path != null)' \
    seed 10.99.0.102 /nodes/probe "{\"target\":\"$url\"}" || return 1
  # The seed's neighbor set is the default seed list, unreachable in the lab, so trace from the relay.
  wait_post "a trace to home1 from the relay" \
    ".reached == true and (.hops | length) == 2 and .hops[-1].base_url == \"$url\" and .hops[0].path_to_next != null" \
    relay 10.99.0.101 /nodes/trace "{\"target\":\"$url\"}" || return 1
  echo "    probe: $(post_json seed 10.99.0.102 /nodes/probe "{\"target\":\"$url\"}" | jq -c '{ok,path,min_ms}')" \
    "trace: $(post_json relay 10.99.0.101 /nodes/trace "{\"target\":\"$url\"}" | jq -c '{reached,hops:[.hops[]|{base_url,path_to_next}]}')"

  # It mirrors the seed's shard over outbound HTTP and serves the same head, which verifies.
  local shard head sk
  shard=$(status seed 10.99.0.102 | jq -r .own_shard_replication.shard_id)
  [ -n "$shard" ] && [ "$shard" != null ] || { echo "    the seed reports no shard" >&2; return 1; }
  post_json seed 10.99.0.102 /integrations "$(register_body seedapp)" | jq -e .id >/dev/null || { echo "    seed registration failed" >&2; return 1; }
  local seed_head
  wait_json "the seed to sign a head" '.tree_size >= 1' lab_get seed "http://10.99.0.102:8080/ledger/sth/latest?shard_id=$shard" || return 1
  seed_head=$(lab_get seed "http://10.99.0.102:8080/ledger/sth/latest?shard_id=$shard")
  WAIT=200 wait_json "home1 to mirror the seed's head" \
    ".tree_size == $(echo "$seed_head" | jq .tree_size) and .root_hash == \"$(echo "$seed_head" | jq -r .root_hash)\"" \
    lab_get home1 "http://10.1.0.2:8080/ledger/sth/latest?shard_id=$shard" || return 1
  head=$(lab_get home1 "http://10.1.0.2:8080/ledger/sth/latest?shard_id=$shard")
  sk=$(echo "$head" | jq -r .signing_public_key)
  [ "node:$(echo -n "$sk" | xxd -r -p | sha256sum | cut -d' ' -f1)" = "$shard" ] \
    || { echo "    the served head's key does not hash to the shard id" >&2; return 1; }
  (cd "$ROOT" && cargo build -q -p avalon-server --example verify_sth) || return 1
  echo "$head" | AVALON_SETTLEMENT_VERIFY_KEY="$sk" "$ROOT/target/debug/examples/verify_sth" old \
    || { echo "    the mirrored head does not verify" >&2; return 1; }
  lab_get home1 "http://10.1.0.2:8080/ledger/mirror-progress?network_id=avalon-dev-lan&shard_id=$shard" \
    | jq -e '.last_seq >= 1' >/dev/null || { echo "    no mirrored entries on home1" >&2; return 1; }

  # A write it authors for its shard is submitted to the shard authority over outbound HTTP.
  post_json home1 10.1.0.2 /integrations "$(register_body urllessapp)" | jq -e .id >/dev/null \
    || { echo "    registration on home1 failed" >&2; return 1; }
  wait_json "the shard authority to hold the entry home1 authored" \
    '[.[] | select(.kind == "game.registered" and .payload.slug == "urllessapp")] | length == 1' \
    lab_get shard "http://10.99.0.103:8080/ledger/entries?shard_id=game:urllessapp" || return 1
  echo "    mirrored $shard to seq $(lab_get home1 "http://10.1.0.2:8080/ledger/mirror-progress?network_id=avalon-dev-lan&shard_id=$shard" | jq .last_seq); shard authority holds home1's write"
}

# expect_call <output> <phase> <transport> <path> <status> [code]: the prober printed that result.
expect_call() {
  local want="$2 $3 $4 $5 ${6:--}"
  grep -qxF "$want" <<<"$1" || { echo "    expected '$want' from the prober" >&2; return 1; }
}

# A node's write routes (relay, chat replication, mirror notify) accept a node with standing and
# refuse a free keypair, over a stream and over signed HTTP. A node with no URL delivers a chat
# replication to a neighbor under its own credential.
scenario_url-less-credential() {
  "$LAB" up home1 no-inbound >/dev/null || return 1
  "$LAB" up-public relay 10.99.0.101 >/dev/null && "$LAB" up-public seed 10.99.0.102 >/dev/null \
    && "$LAB" up-public free 10.99.0.104 >/dev/null || return 1
  local verify=AVALON_ANNOUNCE_VERIFY_REACHABILITY=true
  node relay relay 10.99.0.101 AVALON_LIBP2P_EXTERNAL_ADDR=/ip4/10.99.0.101/tcp/4001 "$verify"
  node seed seed 10.99.0.102 AVALON_LIBP2P_EXTERNAL_ADDR=/ip4/10.99.0.102/tcp/4001 \
    AVALON_BOOTSTRAP_PEERS=http://10.99.0.101:8080 RUST_LOG=info,avalon_server::node_auth=debug "$verify"
  ready relay 10.99.0.101 && ready seed 10.99.0.102 || return 1
  (cd "$ROOT" && cargo build -q -p avalon-server --example node_request --example live_seed) || return 1

  # A keypair that never announced is refused on every route over both transports; the same
  # keypair is accepted once a stream announce gives it standing.
  local out seed_id
  seed_id=$(peer_id seed) || return 1
  out=$("$LAB" exec free -- "$ROOT/target/debug/examples/node_request" avalon-dev-lan \
    /ip4/10.99.0.104/tcp/4001 "$seed_id" "/ip4/10.99.0.102/tcp/4001/p2p/$seed_id" http://10.99.0.102:8080) \
    || { echo "    the prober failed: $out" >&2; return 1; }
  echo "$out" | sed 's/^/    /'
  local route
  for route in /nodes/relay /nodes/replicate-chat /mirror/notify; do
    expect_call "$out" free stream "$route" 403 node_auth_no_standing || return 1
    expect_call "$out" free http-signed "$route" 403 node_auth_no_standing || return 1
    expect_call "$out" free http-unsigned "$route" 401 node_auth_missing || return 1
  done
  expect_call "$out" announce stream /nodes/announce 200 || return 1
  expect_call "$out" standing stream /nodes/replicate-chat 204 || return 1
  expect_call "$out" standing http-signed /nodes/replicate-chat 204 || return 1
  # The other two routes get past the credential and are then refused by their own scope checks.
  for route in /nodes/relay /mirror/notify; do
    grep -E "^standing (stream|http-signed) $route " <<<"$out" | grep -qE ' node_auth_|( 401 )' \
      && { echo "    $route refused the credential of a node with standing" >&2; return 1; }
  done
  expect_call "$out" standing http-unsigned /nodes/replicate-chat 401 node_auth_missing || return 1
  # Both deliveries carry one message id, so one replica row, tagged with the caller's key; the
  # stranger's calls left none.
  local caller rows
  caller=$(awk '$1 == "caller" {print $2}' <<<"$out")
  rows=$(cd "$ROOT" && DATABASE_URL="$BASE_DB_URL" target/debug/examples/live_seed replicas live_nat_seed)
  [ "$(grep -c " $caller\$" <<<"$rows")" = 1 ] && [ "$(wc -l <<<"$rows")" = 1 ] \
    || { echo "    seed replica rows: $rows" >&2; return 1; }

  # A real node with no URL: its chat replication reaches a neighbor, with its key on the replica row.
  node home1 home1 10.1.0.2 AVALON_NODE_URL= "$verify" \
    AVALON_BOOTSTRAP_PEERS=http://10.99.0.101:8080,http://10.99.0.102:8080
  wait_status home1 10.1.0.2 '.connectivity == "outbound_only"' "home1 to report outbound_only" || return 1
  local id token guild channel
  id=$(status home1 10.1.0.2 | jq -r .libp2p_peer_id)
  wait_json "the seed to list home1" "[.peers[] | select(.base_url == \"p2p://$id\")] | length == 1" discover seed 10.99.0.102 || return 1
  token=$(cd "$ROOT" && DATABASE_URL="$BASE_DB_URL" target/debug/examples/live_seed session live_nat_home1) || return 1
  guild=$(authed home1 10.1.0.2 POST /guilds '{"name":"Credential Lab","tag":"CLAB1","description":"lab"}' "$token" | jq -r .id)
  channel=$(authed home1 10.1.0.2 GET "/guilds/$guild/channels" '' "$token" | jq -r '.[] | select(.name == "general") | .id')
  [ -n "$channel" ] && [ "$channel" != null ] || { echo "    no channel on home1" >&2; return 1; }
  authed home1 10.1.0.2 POST "/guilds/$guild/channels/$channel/messages" '{"body":"from a node with no url"}' "$token" \
    | jq -e .id >/dev/null || { echo "    sending the message failed" >&2; return 1; }
  local i found=""
  for i in $(seq 1 $((WAIT / 2))); do
    found=$(cd "$ROOT" && for s in live_nat_relay live_nat_seed; do
      DATABASE_URL="$BASE_DB_URL" target/debug/examples/live_seed replicas $s
    done | grep -c " $id\$")
    [ "$found" = 1 ] && break
    sleep 2
  done
  [ "$found" = 1 ] || { echo "    no replica of home1's message on a neighbor" >&2; return 1; }
  echo "    home1 ($id) replicated its message to a neighbor"
}

# A node with no AVALON_NODE_URL authors a self-certifying `node:` shard. One neighbor finds it by
# discovery and polls it; another names it as a static `node:<hash>=p2p://<peer id>` source, polls
# it over the stream and is woken by the author's push, with a poll interval too long to explain it.
scenario_url-less-shard-author() {
  "$LAB" up home1 no-inbound >/dev/null || return 1
  "$LAB" up-public relay 10.99.0.101 >/dev/null && "$LAB" up-public seed 10.99.0.102 >/dev/null \
    && "$LAB" up-public mira 10.99.0.103 >/dev/null && "$LAB" up-public mirb 10.99.0.104 >/dev/null \
    && "$LAB" up-public free 10.99.0.105 >/dev/null || return 1
  (cd "$ROOT" && cargo build -q -p avalon-server --example verify_sth --example node_request) || return 1
  local verify=AVALON_ANNOUNCE_VERIFY_REACHABILITY=true boot=http://10.99.0.101:8080,http://10.99.0.102:8080 poll=30
  local seed pub shard
  seed=$(head -c32 /dev/urandom | xxd -p -c64)
  pub=$("$ROOT/target/debug/examples/verify_sth" pubkey "$seed")
  shard="node:$(echo -n "$pub" | xxd -r -p | sha256sum | cut -d' ' -f1)"
  node seed seed 10.99.0.102 AVALON_LIBP2P_EXTERNAL_ADDR=/ip4/10.99.0.102/tcp/4001 "$verify"
  node relay relay 10.99.0.101 AVALON_LIBP2P_EXTERNAL_ADDR=/ip4/10.99.0.101/tcp/4001 \
    AVALON_BOOTSTRAP_PEERS=http://10.99.0.102:8080 "$verify"
  ready relay 10.99.0.101 && ready seed 10.99.0.102 || return 1
  node home1 home1 10.1.0.2 AVALON_NODE_URL= "$verify" AVALON_BOOTSTRAP_PEERS=$boot \
    AVALON_OWN_SHARD_ID="$shard" AVALON_SETTLEMENT_SIGNING_KEY="$seed" \
    RUST_LOG=info,avalon_server::mirror_push=debug
  wait_status home1 10.1.0.2 '.connectivity == "outbound_only"' "home1 to report outbound_only" || return 1
  local id url
  id=$(status home1 10.1.0.2 | jq -r .libp2p_peer_id)
  url="p2p://$id"
  post_json home1 10.1.0.2 /integrations "$(register_body firstapp)" | jq -e .id >/dev/null \
    || { echo "    registration on home1 failed" >&2; return 1; }
  wait_json "home1 to sign a head for $shard" '.tree_size >= 1' lab_get home1 "http://10.1.0.2:8080/ledger/sth/latest?shard_id=$shard" || return 1

  # mira only discovers the shard; mirb names it. Both start with the author's first head in place.
  node mira mira 10.99.0.103 AVALON_LIBP2P_EXTERNAL_ADDR=/ip4/10.99.0.103/tcp/4001 AVALON_BOOTSTRAP_PEERS=$boot \
    AVALON_MIRROR_ALL_DISCOVERED_SHARDS=true AVALON_MIRROR_POLL_INTERVAL_SECS=5 "$verify"
  node mirb mirb 10.99.0.104 AVALON_LIBP2P_EXTERNAL_ADDR=/ip4/10.99.0.104/tcp/4001 AVALON_BOOTSTRAP_PEERS=$boot \
    AVALON_MIRROR_PEERS="$shard=$url" AVALON_MIRROR_POLL_INTERVAL_SECS=$poll "$verify"
  ready mira 10.99.0.103 && ready mirb 10.99.0.104 || return 1
  local author_head
  author_head() { lab_get home1 "http://10.1.0.2:8080/ledger/sth/latest?shard_id=$shard"; }
  local size root
  size=$(author_head | jq .tree_size) root=$(author_head | jq -r .root_hash)
  WAIT=150 wait_json "mira to mirror the author's head" ".tree_size == $size and .root_hash == \"$root\"" \
    lab_get mira "http://10.99.0.103:8080/ledger/sth/latest?shard_id=$shard" || return 1
  WAIT=$((poll * 3)) wait_json "mirb to mirror the author's head over the stream" ".tree_size == $size and .root_hash == \"$root\"" \
    lab_get mirb "http://10.99.0.104:8080/ledger/sth/latest?shard_id=$shard" || return 1
  grep -q "registering interest" "$LOG_DIR/mirb.log" || { echo "    mirb never registered mirror interest" >&2; return 1; }

  # mirb's first poll runs before it has the author's address, so it mirrors on its second. A write right
  # after that must reach it well inside the next poll interval, which only the push can do.
  post_json home1 10.1.0.2 /integrations "$(register_body secondapp)" | jq -e .id >/dev/null \
    || { echo "    second registration on home1 failed" >&2; return 1; }
  wait_json "home1 to sign a larger head" ".tree_size > $size" lab_get home1 "http://10.1.0.2:8080/ledger/sth/latest?shard_id=$shard" || return 1
  size=$(author_head | jq .tree_size) root=$(author_head | jq -r .root_hash)
  local t0=$SECONDS
  WAIT=$((poll * 4 / 5)) wait_json "mirb to catch up within $((poll * 4 / 5)) s although it polls every $poll s" ".tree_size == $size and .root_hash == \"$root\"" \
    lab_get mirb "http://10.99.0.104:8080/ledger/sth/latest?shard_id=$shard" || return 1
  local took=$((SECONDS - t0))
  grep -q "mirror-push: received a push notification" "$LOG_DIR/mirb.log" \
    || { echo "    mirb logged no push notification" >&2; return 1; }
  WAIT=150 wait_json "mira to mirror the second head" ".tree_size == $size and .root_hash == \"$root\"" \
    lab_get mira "http://10.99.0.103:8080/ledger/sth/latest?shard_id=$shard" || return 1

  # Entries and heads match the author's on both neighbors, and each head verifies under the shard's own key.
  local want got n h
  want=$(lab_get home1 "http://10.1.0.2:8080/ledger/entries?shard_id=$shard" | jq -cS .)
  [ "$(jq length <<<"$want")" -ge 2 ] || { echo "    the author lists fewer than two entries: $want" >&2; return 1; }
  for n in mira:10.99.0.103 mirb:10.99.0.104; do
    got=$(lab_get "${n%%:*}" "http://${n##*:}:8080/ledger/entries?shard_id=$shard" | jq -cS .)
    [ "$got" = "$want" ] || { echo "    ${n%%:*} entries differ from the author's: $got" >&2; return 1; }
    h=$(lab_get "${n%%:*}" "http://${n##*:}:8080/ledger/sth/latest?shard_id=$shard")
    [ "$(echo "$h" | jq -r .signing_public_key)" = "$pub" ] || { echo "    ${n%%:*} serves a head signed by another key" >&2; return 1; }
    echo "$h" | AVALON_SETTLEMENT_VERIFY_KEY="$pub" "$ROOT/target/debug/examples/verify_sth" old \
      || { echo "    the head on ${n%%:*} does not verify" >&2; return 1; }
  done

  # A keypair with standing that is not the author is refused on a node that has the author as a source and on one with none.
  local out target
  for target in mira:10.99.0.103 mirb:10.99.0.104; do
    local tid=$(peer_id "${target%%:*}")
    out=$("$LAB" exec free -- "$ROOT/target/debug/examples/node_request" avalon-dev-lan \
      /ip4/10.99.0.105/tcp/4001 "$tid" "/ip4/${target##*:}/tcp/4001/p2p/$tid" "http://${target##*:}:8080") \
      || { echo "    the prober failed: $out" >&2; return 1; }
    expect_call "$out" standing stream /mirror/notify 403 || return 1
    expect_call "$out" standing http-signed /mirror/notify 403 || return 1
  done
  echo "    $shard authored by $url; mira and mirb match it and verify; mirb caught up ${took}s after the write" \
    "(poll $poll s); a stranger's notify is refused on both"
}

# authed <ns> <ip> <method> <path> <body> <token>: a bearer-authenticated request to that node.
authed() {
  "$LAB" exec "$1" -- curl -s -m 15 -X "$3" -H "authorization: Bearer $6" -H 'content-type: application/json' \
    ${5:+-d "$5"} "http://$2:8080$4"
}

# Losing the relay a node reserved on moves it to a second relay.
scenario_relay-failover() {
  "$LAB" up home1 symmetric >/dev/null || return 1
  public_pair || return 1
  "$LAB" up-public relay2 10.99.0.103 >/dev/null || return 1
  node relay2 relay2 10.99.0.103 AVALON_LIBP2P_EXTERNAL_ADDR=/ip4/10.99.0.103/tcp/4001 AVALON_RELAY_SERVER_ENABLED=true \
    AVALON_BOOTSTRAP_PEERS=http://10.99.0.101:8080
  local second="$LAST_PID"
  ready relay2 10.99.0.103 || return 1
  node home1 home1 10.1.0.2 AVALON_BOOTSTRAP_PEERS=http://10.99.0.101:8080,http://10.99.0.102:8080 \
    AVALON_RELAY_CLIENT_MAX_RESERVATIONS=1 \
    AVALON_RELAY_ADDRS="$(relay_addr),/ip4/10.99.0.103/tcp/4001/p2p/$(peer_id relay2)"
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

# A private node that has measured three discovered relays (150, 60 and 5 ms of added delay) picks
# the nearest one for its next reservation, whichever of them it lost. The lab's public segment
# is a single /24, so prefix diversity is covered by the unit and loopback tests instead.
scenario_relay-ranking() {
  command -v tc >/dev/null 2>&1 || { echo "relay-ranking needs tc" >&2; return 2; }
  "$LAB" up home1 symmetric >/dev/null || return 1
  # Relay latency is attributed by libp2p id, which needs the verified (identity-bound) entries.
  local verify=AVALON_ANNOUNCE_VERIFY_REACHABILITY=true
  local spec name ip delay far_pid mid_pid near_pid
  for spec in "far 10.99.0.101 150ms" "mid 10.99.0.103 60ms" "near 10.99.0.104 5ms"; do
    read -r name ip delay <<<"$spec"
    "$LAB" up-public "$name" "$ip" >/dev/null || return 1
    "$LAB" exec "$name" -- tc qdisc add dev eth0 root netem delay "$delay" || return 1
    if [ "$name" = far ]; then
      node "$name" "$name" "$ip" AVALON_LIBP2P_EXTERNAL_ADDR=/ip4/$ip/tcp/4001 AVALON_RELAY_SERVER_ENABLED=true "$verify"
    else
      node "$name" "$name" "$ip" AVALON_LIBP2P_EXTERNAL_ADDR=/ip4/$ip/tcp/4001 AVALON_RELAY_SERVER_ENABLED=true \
        AVALON_BOOTSTRAP_PEERS=http://10.99.0.101:8080 "$verify"
    fi
    case $name in far) far_pid=$LAST_PID ;; mid) mid_pid=$LAST_PID ;; near) near_pid=$LAST_PID ;; esac
    ready "$name" "$ip" || return 1
  done
  # home1 announces url-less: its private base URL would be refused (422) by the verifying relays.
  node home1 home1 10.1.0.2 AVALON_NODE_URL= "$verify" \
    AVALON_BOOTSTRAP_PEERS=http://10.99.0.101:8080,http://10.99.0.103:8080,http://10.99.0.104:8080 \
    AVALON_RELAY_CLIENT_MAX_RESERVATIONS=1
  wait_status home1 10.1.0.2 '.connectivity == "relayed" and (.relay_reservations|length) == 1' \
    "a relay reservation" || return 1
  # Judge only once every relay has a measured round trip under its own libp2p id, so a correct
  # build cannot fail on timing. Identity binding is not exposed by the API; the verified
  # announces above produce it before an id is listed.
  local u id
  for u in far:10.99.0.101 mid:10.99.0.103 near:10.99.0.104; do
    id=$(peer_id "${u%%:*}")
    wait_json "a measured round trip to ${u%%:*} under its libp2p id" \
      "[.neighbors[] | select(.base_url == \"http://${u##*:}:8080\" and .libp2p_peer_id == \"$id\" and .latency.ewma_ms != null)] | length == 1" \
      topology home1 10.1.0.2 || return 1
  done
  local held want
  held=$(status home1 10.1.0.2 | jq -r '.relay_reservations[0].relay_peer_id')
  # Stop the relay holding the reservation; the next one must be the nearest of the rest.
  case $held in
    "$(peer_id far)") kill "$far_pid"; want=$(peer_id near) ;;
    "$(peer_id mid)") kill "$mid_pid"; want=$(peer_id near) ;;
    "$(peer_id near)") kill "$near_pid"; want=$(peer_id mid) ;;
    *) echo "    reservation on an unknown relay: $held" >&2; return 1 ;;
  esac
  wait_status home1 10.1.0.2 \
    ".connectivity == \"relayed\" and (.relay_reservations|length) == 1 and .relay_reservations[0].relay_peer_id == \"$want\"" \
    "the reservation to move to the nearest remaining relay ($want)"
}

# A node holding a reservation on a discovered relay moves to an operator-listed relay that comes
# up later, only after the hold time, and the peers see the new circuit address well before the
# (long) announce interval would have refreshed it.
scenario_relay-reselect() {
  "$LAB" up home1 symmetric >/dev/null || return 1
  public_pair || return 1
  "$LAB" up-public relay2 10.99.0.103 >/dev/null || return 1
  local key2 hold=30 pid2 id2 home_id
  key2=$(head -c32 /dev/urandom | xxd -p -c64)
  local relay2=(AVALON_LIBP2P_IDENTITY_KEY="$key2" AVALON_LIBP2P_EXTERNAL_ADDR=/ip4/10.99.0.103/tcp/4001
    AVALON_RELAY_SERVER_ENABLED=true AVALON_BOOTSTRAP_PEERS=http://10.99.0.101:8080)
  # Start the listed relay once to learn its peer id, then stop it until the node holds another.
  node relay2 relay2 10.99.0.103 "${relay2[@]}"
  pid2=$LAST_PID
  ready relay2 10.99.0.103 || return 1
  id2=$(peer_id relay2)
  kill "$pid2"
  node home1 home1 10.1.0.2 AVALON_BOOTSTRAP_PEERS=http://10.99.0.101:8080 \
    AVALON_RELAY_CLIENT_MAX_RESERVATIONS=1 AVALON_RELAY_ADDRS="/ip4/10.99.0.103/tcp/4001/p2p/$id2" \
    AVALON_RELAY_RESELECT_HOLD_SECS=$hold AVALON_RELAY_RESELECT_INTERVAL_SECS=5 AVALON_ANNOUNCE_INTERVAL_SECS=600
  home_id=$(peer_id home1)
  wait_status home1 10.1.0.2 \
    ".connectivity == \"relayed\" and (.relay_reservations|length) == 1 and .relay_reservations[0].relay_peer_id == \"$(peer_id relay)\"" \
    "a reservation on the discovered relay" || return 1
  local reserved_at moved_at
  reserved_at=$(date +%s)
  node relay2 relay2 10.99.0.103 "${relay2[@]}"
  ready relay2 10.99.0.103 || return 1
  sleep $((hold / 3))
  status home1 10.1.0.2 | jq -e ".relay_reservations[0].relay_peer_id == \"$(peer_id relay)\"" >/dev/null \
    || { echo "    the node moved before the hold time" >&2; return 1; }
  wait_status home1 10.1.0.2 \
    ".connectivity == \"relayed\" and (.relay_reservations|length) == 1 and .relay_reservations[0].relay_peer_id == \"$id2\"" \
    "the reservation to move to the listed relay" || return 1
  moved_at=$(date +%s)
  [ $((moved_at - reserved_at)) -ge $((hold - 4)) ] \
    || { echo "    moved after $((moved_at - reserved_at))s, under the ${hold}s hold" >&2; return 1; }
  # The announce interval is 600 s, so only the early announce can tell the relay in time.
  wait_json "the first relay's /nodes/peers to list the circuit address through the new relay" \
    "[.[] | select(.libp2p_peer_id == \"$home_id\") | .libp2p_listen_addrs[] | select(contains(\"/p2p/$id2/p2p-circuit\"))] | length >= 1" \
    "$LAB" exec relay -- curl -s -m 5 http://10.99.0.101:8080/nodes/peers || return 1
  [ $(($(date +%s) - moved_at)) -lt 60 ] || { echo "    the new address took over 60s to be announced" >&2; return 1; }
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

ALL="public full-cone-direct relayed-restricted-cone punch-port-restricted punch-symmetric-fallback relayed-port-restricted relayed-symmetric relayed-no-inbound outbound-only url-less-admission url-less-participation url-less-credential url-less-shard-author relay-failover relay-ranking"
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
