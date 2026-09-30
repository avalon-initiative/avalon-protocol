#!/usr/bin/env bash
# Self-test for nat-lab.sh: checks each NAT type behaves as documented.
# Needs root, python3, and nat-lab.sh next to this script. Only creates
# avlab- namespaces and removes them on exit.
#
# usage: nat-lab-selftest.sh [type ...]     (default: all five types)
set -uo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
LAB="$HERE/nat-lab.sh"
A=10.99.0.1
B=10.99.0.2
TMP=$(mktemp -d)
SRV_PIDS=()
FAILS=0
ROWS=()

cleanup() {
  local p
  for p in "${SRV_PIDS[@]}"; do kill "$p" 2>/dev/null || true; done
  "$LAB" down-all >/dev/null 2>&1 || true
  rm -rf "$TMP"
}
trap cleanup EXIT

cat >"$TMP/h.py" <<'PY'
import socket, sys, threading, time

def server(ip, port):
    u = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    u.bind((ip, port))
    t = socket.socket()
    t.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    t.bind((ip, port)); t.listen(16)
    def tcp():
        while True:
            c, a = t.accept()
            c.sendall(("%s:%d" % a).encode()); c.close()
    threading.Thread(target=tcp, daemon=True).start()
    while True:
        d, a = u.recvfrom(512)
        if d.startswith(b"poke "):
            h, p = d[5:].decode().split(":")
            u.sendto(b"probe", (h, int(p)))
        else:
            u.sendto(("%s:%d" % a).encode(), a)

def ulisten(lport, out, dur, contacts):
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    s.bind(("0.0.0.0", lport)); s.settimeout(2)
    f = open(out, "a", buffering=1)
    for c in contacts:
        h, p = c.split(":")
        s.sendto(b"hi", (h, int(p)))
        seen = s.recvfrom(512)[0].decode()
        f.write("mapped %s %s\n" % (c, seen.split(":")[1]))
    end = time.time() + dur
    while time.time() < end:
        s.settimeout(max(0.1, end - time.time()))
        try:
            d, a = s.recvfrom(512)
            f.write("rx %s:%d\n" % a)
        except socket.timeout:
            pass

def probe(sip, sport, dip, dport):
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    s.bind((sip, sport))
    for _ in range(3):
        s.sendto(b"probe", (dip, dport)); time.sleep(0.2)

def poke(sip, dip, dport, target, tport):
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    for _ in range(3):
        s.sendto(("poke %s:%d" % (dip, dport)).encode(), (target, tport)); time.sleep(0.2)

def tlisten(port, out, dur):
    t = socket.socket()
    t.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    t.bind(("0.0.0.0", port)); t.listen(4); t.settimeout(dur)
    try:
        c, a = t.accept(); open(out, "a").write("conn %s:%d\n" % a)
    except socket.timeout:
        pass

def tconnect(ip, port):
    s = socket.socket(); s.settimeout(2)
    try:
        s.connect((ip, port)); print(s.recv(64).decode())
    except OSError:
        sys.exit(1)

def uclient(ip, port):
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM); s.settimeout(2)
    s.sendto(b"hi", (ip, port))
    try:
        print(s.recvfrom(64)[0].decode())
    except OSError:
        sys.exit(1)

m, a = sys.argv[1], sys.argv[2:]
{"server": lambda: server(a[0], int(a[1])),
 "ulisten": lambda: ulisten(int(a[0]), a[1], float(a[2]), a[3:]),
 "probe": lambda: probe(a[0], int(a[1]), a[2], int(a[3])),
 "poke": lambda: poke(a[0], a[1], int(a[2]), a[3], int(a[4])),
 "tlisten": lambda: tlisten(int(a[0]), a[1], float(a[2])),
 "tconnect": lambda: tconnect(a[0], int(a[1])),
 "uclient": lambda: uclient(a[0], int(a[1]))}[m]()
PY

inet() { "$LAB" exec inet -- python3 "$TMP/h.py" "$@"; }
home() { local n=$1; shift; "$LAB" exec "$n" -- python3 "$TMP/h.py" "$@"; }

# record <type> <check> <expected yes|no> <got yes|no>
record() {
  local res=PASS
  [ "$3" = "$4" ] || { res=FAIL; FAILS=$((FAILS + 1)); }
  ROWS+=("$(printf '%-16s %-34s %-6s %-6s %s' "$1" "$2" "$3" "$4" "$res")")
}

yn() { if "$@" >/dev/null 2>&1; then echo yes; else echo no; fi; }
has() { grep -q -- "$2" "$1" 2>/dev/null && echo yes || echo no; }
mapped() { awk -v c="$2" '$1=="mapped" && $2==c {print $3}' "$1"; }

wait_mapped() {
  local i
  for i in $(seq 1 40); do
    [ "$(grep -c '^mapped' "$1" 2>/dev/null || true)" -ge "$2" ] && return 0
    sleep 0.25
  done
  return 1
}

test_type() {
  local type=$1 n="t-$1" idx w exp_unsol exp_same exp_fb exp_fa
  "$LAB" up "$n" "$type" >/dev/null || { record "$type" "up" yes no; return; }
  idx=$(awk '{print $1}' "/run/avlab/$n.home")
  w="10.99.0.$((10 + idx))"

  case $type in
    full-cone) exp_unsol=yes exp_same=yes exp_fb=yes exp_fa=yes ;;
    restricted-cone) exp_unsol=no exp_same=yes exp_fb=no exp_fa=yes ;;
    port-restricted) exp_unsol=no exp_same=yes exp_fb=no exp_fa=no ;;
    *) exp_unsol=no exp_same=no exp_fb=no exp_fa=no ;;
  esac

  record "$type" "outbound UDP echo" yes "$(yn home "$n" uclient $A 7001)"
  record "$type" "outbound TCP connect" yes "$(yn home "$n" tconnect $A 7001)"

  # unsolicited inbound, nothing sent out first
  local f="$TMP/$type.unsol"
  : >"$f"
  home "$n" ulisten 5560 "$f" 3 &
  local lp=$!
  sleep 0.5
  inet probe $A 7009 "$w" 5560
  wait "$lp" 2>/dev/null
  record "$type" "unsolicited inbound UDP" "$exp_unsol" "$(has "$f" rx)"

  f="$TMP/$type.tunsol"
  : >"$f"
  home "$n" tlisten 9000 "$f" 3 &
  lp=$!
  sleep 0.5
  record "$type" "unsolicited inbound TCP" "$exp_unsol" "$(yn inet tconnect "$w" 9000)"
  wait "$lp" 2>/dev/null

  # mapping: one socket contacting two remote addresses (retry: random ports can collide)
  local pa pb same=no try
  for try in 1 2 3; do
    f="$TMP/$type.map$try"
    : >"$f"
    home "$n" ulisten $((5600 + try)) "$f" 1 $A:7001 $B:7001 &
    lp=$!
    wait "$lp" 2>/dev/null
    pa=$(mapped "$f" $A:7001)
    pb=$(mapped "$f" $B:7001)
    [ -n "$pa" ] && [ "$pa" = "$pb" ] && same=yes
    [ "$exp_same" = yes ] && break
    [ "$same" = no ] && break
  done
  record "$type" "same external port for 2 remotes" "$exp_same" "$same"

  # filtering: contact only A, then probe from other endpoints
  f="$TMP/$type.filt"
  : >"$f"
  home "$n" ulisten 5570 "$f" 5 $A:7001 &
  lp=$!
  wait_mapped "$f" 1 || true
  pa=$(mapped "$f" $A:7001)
  inet probe $B 7009 "$w" "$pa"
  local rxb
  rxb=$(has "$f" "rx $B:7009")
  inet probe $A 7009 "$w" "$pa"
  local rxa
  rxa=$(has "$f" "rx $A:7009")
  inet poke x $w "$pa" $A 7001
  local rxe
  rxe=$(has "$f" "rx $A:7001")
  wait "$lp" 2>/dev/null
  record "$type" "other address -> mapped port" "$exp_fb" "$rxb"
  record "$type" "same address, other port" "$exp_fa" "$rxa"
  record "$type" "contacted endpoint exact reply" yes "$rxe"
  "$LAB" down "$n" >/dev/null
}

main() {
  [ "$(id -u)" -eq 0 ] || { echo "must run as root" >&2; exit 2; }
  command -v python3 >/dev/null || { echo "python3 required" >&2; exit 2; }
  local types=("$@")
  [ ${#types[@]} -gt 0 ] || types=(full-cone restricted-cone port-restricted symmetric no-inbound)
  "$LAB" down-all >/dev/null
  "$LAB" up scratch full-cone >/dev/null # creates the public side; removed by down-all below
  for ip in $A $B; do
    inet server $ip 7001 &
    SRV_PIDS+=($!)
  done
  "$LAB" down scratch >/dev/null
  sleep 1
  local t
  for t in "${types[@]}"; do test_type "$t"; done
  printf '%-16s %-34s %-6s %-6s %s\n' TYPE CHECK EXPECT GOT RESULT
  printf '%s\n' "${ROWS[@]}"
  if [ "$FAILS" -gt 0 ]; then echo "FAILED: $FAILS check(s)"; exit 1; fi
  echo "all checks passed"
}
main "$@"
