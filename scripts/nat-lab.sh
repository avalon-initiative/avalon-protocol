#!/usr/bin/env bash
# Repeatable NAT lab: isolated "home networks" behind an nftables NAT router,
# all inside network namespaces. The host's own namespace is never modified.
#
# usage: nat-lab.sh up <name> <type>     type: full-cone | restricted-cone |
#                                              port-restricted | symmetric | no-inbound
#        nat-lab.sh up-inet                 (just the shared public side, no homes)
#        nat-lab.sh up-public <name> <ip>   a public node's own namespace on the shared segment,
#                                           <ip> in 10.99.0.0/24 (run it with: exec <name> -- cmd)
#        nat-lab.sh down <name>
#        nat-lab.sh down-all
#        nat-lab.sh exec <name> -- cmd...   (<name> = a home, or "inet" for the shared public side)
#        nat-lab.sh status
#
# Layout (n = per-home index 1..89):
#   avlab-<name>   home host   10.n.0.2/24, default via 10.n.0.1
#   avlab-<name>-r NAT router  LAN 10.n.0.1, WAN 10.99.0.(10+n), nft table "ip avlab"
#   avlab-inet     public side: bridge with 10.99.0.1 and 10.99.0.2 (two distinct remote endpoints)
#
# See docs/projects/backend-server/for-maintainers/nat-lab.md. Needs root, iproute2, nftables.
set -euo pipefail

PFX=avlab
INET_NS=${PFX}-inet
STATE=/run/${PFX}
WAN_NET=10.99.0
TYPES="full-cone restricted-cone port-restricted symmetric no-inbound"

die() { echo "nat-lab: $*" >&2; exit 1; }

need_root() {
  [ "$(id -u)" -eq 0 ] || die "must run as root"
  command -v ip >/dev/null || die "ip (iproute2) not found"
  command -v nft >/dev/null || die "nft not found"
}

valid_name() {
  [[ $1 =~ ^[a-z0-9]([a-z0-9-]{0,18}[a-z0-9])?$ ]] || die "bad name '$1' (lowercase alnum and '-', max 20)"
  [ "$1" != inet ] || die "'inet' is reserved"
  [[ $1 != *-r ]] || die "names ending in '-r' are reserved"
}

lock() {
  mkdir -p "$STATE"
  exec 9>"$STATE/lock"
  flock 9
}

ns_exists() { ip netns list | awk '{print $1}' | grep -qx "$1"; }

state_idx() { awk '{print $1}' "$STATE/$1.home" 2>/dev/null || true; }
state_type() { awk '{print $2}' "$STATE/$1.home" 2>/dev/null || true; }

alloc_idx() {
  local i used
  used=$(cat "$STATE"/*.home 2>/dev/null | awk '{print $1}' || true)
  for i in $(seq 1 89); do
    grep -qx "$i" <<<"$used" || { echo "$i"; return; }
  done
  die "no free home index"
}

ensure_inet() {
  ns_exists "$INET_NS" && return 0
  ip netns add "$INET_NS"
  ip -n "$INET_NS" link set lo up
  ip -n "$INET_NS" link add avlbr0 type bridge
  ip -n "$INET_NS" addr add "$WAN_NET.1/24" dev avlbr0
  ip -n "$INET_NS" addr add "$WAN_NET.2/24" dev avlbr0
  ip -n "$INET_NS" link set avlbr0 up
}

# Emit the router's nft ruleset for a NAT type.
ruleset() {
  local type=$1 w=$2 h=$3 l=$4 pre="" track="" sets="" snat
  snat="snat to $w"
  case $type in
    full-cone)
      pre="iifname wan ip daddr $w dnat to $h" ;;
    restricted-cone)
      sets='set r_udp { type ipv4_addr . inet_service; flags timeout; timeout 120s; }
  set r_tcp { type ipv4_addr . inet_service; flags timeout; timeout 120s; }'
      pre="iifname wan ip daddr $w meta l4proto udp ip saddr . udp dport @r_udp dnat to $h
    iifname wan ip daddr $w meta l4proto tcp ip saddr . tcp dport @r_tcp dnat to $h"
      track='oifname wan meta l4proto udp update @r_udp { ip daddr . udp sport timeout 120s }
    oifname wan meta l4proto tcp update @r_tcp { ip daddr . tcp sport timeout 120s }' ;;
    port-restricted)
      sets='set p_udp { type ipv4_addr . inet_service . inet_service; flags timeout; timeout 120s; }
  set p_tcp { type ipv4_addr . inet_service . inet_service; flags timeout; timeout 120s; }'
      pre="iifname wan ip daddr $w meta l4proto udp ip saddr . udp sport . udp dport @p_udp dnat to $h
    iifname wan ip daddr $w meta l4proto tcp ip saddr . tcp sport . tcp dport @p_tcp dnat to $h"
      track='oifname wan meta l4proto udp update @p_udp { ip daddr . udp dport . udp sport timeout 120s }
    oifname wan meta l4proto tcp update @p_tcp { ip daddr . tcp dport . tcp sport timeout 120s }' ;;
    symmetric|no-inbound)
      snat="meta l4proto udp snat to $w:20000-59999 random
    oifname wan ip saddr $l meta l4proto tcp snat to $w:20000-59999 random
    oifname wan ip saddr $l meta l4proto icmp snat to $w" ;;
  esac
  cat <<NFT
table ip $PFX {
  $sets
  chain input {
    type filter hook input priority 0; policy drop;
    iifname "lo" accept
    iifname "lan" accept
    ct state established,related accept
  }
  chain forward {
    type filter hook forward priority 0; policy drop;
    ct state established,related accept
    iifname "lan" accept
    ct status dnat accept
  }
  chain prerouting {
    type nat hook prerouting priority -100; policy accept;
    $pre
  }
  chain postrouting {
    type nat hook postrouting priority 100; policy accept;
    oifname wan ip saddr $l $snat
  }
  chain track {
    type filter hook postrouting priority 200; policy accept;
    $track
  }
}
NFT
}

cmd_up() {
  [ $# -eq 2 ] || die "usage: up <name> <type>"
  local name=$1 type=$2 idx cur
  valid_name "$name"
  grep -qw -- "$type" <<<"$TYPES" || die "unknown type '$type' (one of: $TYPES)"
  lock
  cur=$(state_type "$name")
  if [ -n "$cur" ]; then
    [ "$cur" = "$type" ] && ns_exists "${PFX}-$name" && { echo "$name already up ($type)"; return 0; }
    cmd_down_locked "$name"
  fi
  idx=$(alloc_idx)
  local h=${PFX}-$name r=${PFX}-$name-r
  local w="$WAN_NET.$((10 + idx))" lan="10.$idx.0" rules
  ensure_inet
  rules=$(mktemp)
  ruleset "$type" "$w" "$lan.2" "$lan.0/24" >"$rules"
  echo "$idx $type" >"$STATE/$name.home"
  trap 'cmd_down_locked "$name"; rm -f "$rules"' ERR
  set -E
  ip netns add "$h"
  ip netns add "$r"
  ip -n "$h" link set lo up
  ip -n "$r" link set lo up
  ip -n "$r" link add wan type veth peer name "w$idx" netns "$INET_NS"
  ip -n "$r" link add lan type veth peer name eth0 netns "$h"
  ip -n "$INET_NS" link set "w$idx" master avlbr0 up
  ip -n "$r" addr add "$w/24" dev wan
  ip -n "$r" addr add "$lan.1/24" dev lan
  ip -n "$r" link set wan up
  ip -n "$r" link set lan up
  ip -n "$h" addr add "$lan.2/24" dev eth0
  ip -n "$h" link set eth0 up
  ip -n "$h" route add default via "$lan.1"
  ip netns exec "$r" sysctl -q -w net.ipv4.ip_forward=1
  ip netns exec "$r" nft -f "$rules"
  trap - ERR
  rm -f "$rules"
  echo "$name up: $type, home $lan.2/24, public address $w"
}

kill_ns_pids() {
  local ns=$1 p
  ns_exists "$ns" || return 0
  for p in $(ip netns pids "$ns" 2>/dev/null); do kill "$p" 2>/dev/null || true; done
}

cmd_down_locked() {
  local name=$1 ns idx
  idx=$(state_idx "$name")
  # explicit delete: netns teardown is asynchronous and a reused index would race it
  [ -z "$idx" ] || ip -n "$INET_NS" link del "w$idx" 2>/dev/null || true
  [ ! -f "$STATE/$name.pub" ] || ip -n "$INET_NS" link del "pu$(cat "$STATE/$name.pub")" 2>/dev/null || true
  for ns in "${PFX}-$name" "${PFX}-$name-r"; do
    kill_ns_pids "$ns"
    ns_exists "$ns" && ip netns delete "$ns"
  done
  rm -f "$STATE/$name.home" "$STATE/$name.pub"
  return 0
}

cmd_down() {
  [ $# -eq 1 ] || die "usage: down <name>"
  valid_name "$1"
  lock
  cmd_down_locked "$1"
  echo "$1 down"
}

cmd_down_all() {
  lock
  local ns
  for ns in $(ip netns list | awk '{print $1}' | grep "^${PFX}-" || true); do
    kill_ns_pids "$ns"
    ip netns delete "$ns"
  done
  rm -rf "$STATE"
  echo "all ${PFX}- namespaces removed"
}

cmd_exec() {
  [ $# -ge 2 ] || die "usage: exec <name|inet> -- cmd..."
  local name=$1 ns
  shift
  [ "$1" = -- ] && shift
  [ $# -ge 1 ] || die "missing command"
  if [ "$name" = inet ]; then ns=$INET_NS; else valid_name "$name"; ns=${PFX}-$name; fi
  ns_exists "$ns" || die "no such namespace $ns (run: up)"
  exec ip netns exec "$ns" "$@"
}

# A public host of its own: two addresses in one namespace share a source-address choice, so
# a node that opens a connection without binding shows up to its peer as the other address.
cmd_up_public() {
  [ $# -eq 2 ] || die "usage: up-public <name> <ip>"
  local name=$1 ip=$2 oct ns
  valid_name "$name"
  [[ $ip =~ ^10\.99\.0\.([0-9]+)$ ]] || die "ip must be in $WAN_NET.0/24"
  oct=${BASH_REMATCH[1]}
  ns=${PFX}-$name
  lock
  ensure_inet
  ns_exists "$ns" && { echo "$name already up"; return 0; }
  ip netns add "$ns"
  ip -n "$ns" link set lo up
  ip -n "$INET_NS" link add "pu$oct" type veth peer name eth0 netns "$ns"
  ip -n "$INET_NS" link set "pu$oct" master avlbr0 up
  ip -n "$ns" addr add "$ip/24" dev eth0
  ip -n "$ns" link set eth0 up
  echo "$oct" >"$STATE/$name.pub"
  echo "$name up: public address $ip"
}

cmd_up_inet() {
  lock
  ensure_inet
  echo "inet up"
}

cmd_status() {
  local f name idx type
  if ! ns_exists "$INET_NS"; then echo "no lab running"; return 0; fi
  printf '%-16s %-16s %-14s %-16s %s\n' NAME TYPE HOME PUBLIC-ADDR NAT-TABLE
  for f in "$STATE"/*.home; do
    [ -e "$f" ] || continue
    name=$(basename "$f" .home)
    read -r idx type <"$f"
    printf '%-16s %-16s %-14s %-16s %s\n' "$name" "$type" "10.$idx.0.2" "$WAN_NET.$((10 + idx))" \
      "$(ip netns exec "${PFX}-$name-r" nft list tables 2>/dev/null | grep -q "ip $PFX" && echo ok || echo MISSING)"
  done
  echo "public endpoints (exec inet): $WAN_NET.1 $WAN_NET.2"
}

main() {
  local c=${1:-}
  [ $# -gt 0 ] && shift
  case $c in
    up | up-inet | up-public | down | down-all | exec | status) need_root ;;
    *) sed -n '2,20p' "$0"; exit 2 ;;
  esac
  "cmd_${c//-/_}" "$@"
}
main "$@"
