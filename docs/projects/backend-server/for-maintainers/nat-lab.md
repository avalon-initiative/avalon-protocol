# NAT lab

A repeatable way to put a node behind a home-router-like NAT on one Linux
host, so NAT-aware connectivity (direct dial, hole punching, relay fallback)
can be tested without real routers. Part of the NAT-aware connectivity work
(epic #918, issue #916). This page covers the lab primitives only; the
scenario suite that runs `avalon-server` inside it comes later.

## Usage

Needs Linux, root, `ip netns` (iproute2) and nftables. `python3` is needed for
the self-test only.

```bash
sudo scripts/nat-lab.sh up home1 port-restricted   # create a home network
sudo scripts/nat-lab.sh status
sudo scripts/nat-lab.sh exec home1 -- ip -br addr  # run a command inside it
sudo scripts/nat-lab.sh exec inet -- python3 -m http.server -b 10.99.0.1 8000
sudo scripts/nat-lab.sh down home1
sudo scripts/nat-lab.sh down-all                   # remove everything the lab made
sudo scripts/nat-lab-selftest.sh                   # prove each type behaves as documented
```

Types: `full-cone`, `restricted-cone`, `port-restricted`, `symmetric`,
`no-inbound`. `up` is idempotent (same name and type is a no-op; a different
type recreates the home).

## Topology

Everything lives in network namespaces named `avlab-*`; the host's own
namespace, routes and firewall are never touched, and there is no host
forwarding or masquerade involved.

| Namespace | Role | Addresses |
| --- | --- | --- |
| `avlab-<name>` | the home host (run the node here) | `10.<n>.0.2/24`, default via `10.<n>.0.1` |
| `avlab-<name>-r` | the NAT router, nft table `ip avlab` | LAN `10.<n>.0.1`, WAN `10.99.0.<10+n>` |
| `avlab-inet` | the shared public side, a bridge | `10.99.0.1` and `10.99.0.2` |

`n` is a per-home index (1 to 89) assigned by `up`. The two public addresses
give tests two distinct remote endpoints. All homes share the `10.99.0.0/24`
public segment, so homes can reach each other's public addresses (needed for
hole-punching between two NATed nodes). State is kept in `/run/avlab`.

## What each type models

| Type | Mapping | Inbound filtering |
| --- | --- | --- |
| `full-cone` | endpoint-independent (port preserved) | any remote may reach the public address on any port |
| `restricted-cone` | endpoint-independent | only remote IPs the host has sent to (per external port) |
| `port-restricted` | endpoint-independent | only remote IP+port pairs the host has sent to |
| `symmetric` | per-flow random port | only the exact remote endpoint of a flow |
| `no-inbound` | per-flow random port | only the exact remote endpoint of a flow |

How it is built, and where it is approximate:

- Mapping: Linux conntrack keeps the source port when it is free, so plain
  `snat to <public-ip>` gives the same external port for every destination as
  long as nothing collides. This is endpoint-independent mapping in practice,
  but not guaranteed under collisions (two internal hosts using one port).
  The lab has one host per home, so it holds. Symmetric uses `snat ... random`,
  which picks a random port per conntrack flow. That is endpoint-dependent
  mapping, but by accident of flows, not by rule: two flows to different
  destinations differ with probability about 1 - 1/40000, and the same flow
  always keeps its port. Address-dependent-only mapping (a distinct port per
  destination IP but shared across ports) is not modelled.
- `full-cone` is a static DMZ-style DNAT of every port on the public address
  to the home host plus port-preserving SNAT. That gives the observable
  full-cone behaviour, but it is more permissive than a real full-cone NAT:
  inbound works even before the host has sent anything. nftables cannot
  express "open a mapping only after an outbound packet, then accept anyone".
- `restricted-cone` and `port-restricted` use nft sets that are updated from
  outbound packets (120 s timeout, refreshed on use) and consulted by a
  prerouting DNAT rule. Sets are keyed on remote IP and external port
  (restricted) or remote IP, remote port and external port (port-restricted).
  The timeout is fixed, unlike real NATs, and TCP is matched on SYNs only.
- `symmetric` and `no-inbound` have identical rules. Both are "outbound only,
  replies only from the contacted endpoint, random mapping". Real
  carrier-grade NATs and firewalls differ in timeouts and port allocation
  policy; the lab does not, so treat `no-inbound` as an alias that documents
  intent in a test.
- Not modelled: hairpinning (reaching a home's own public address from
  inside), UPnP/NAT-PMP/PCP, IPv6, port-allocation policies (sequential,
  parity-preserving), per-NAT UDP timeouts, and NAT64.

## Self-test

`scripts/nat-lab-selftest.sh [type ...]` builds a home per type, starts UDP
and TCP echo servers on both public addresses and checks: outbound UDP and
TCP work, unsolicited inbound UDP and TCP, one socket sees the same external
port from two remotes (cone) or different ports (symmetric), and inbound from
a different address, from the same address on another port, and from the
exact contacted endpoint. It prints a pass/fail table and exits non-zero on
any failure. It only creates `avlab-*` namespaces and removes them on exit.

## Planned use by scenario tests

Later scenarios run real `avalon-server` processes inside the lab:

```bash
sudo scripts/nat-lab.sh up home1 symmetric
sudo scripts/nat-lab.sh exec home1 -- ./target/debug/avalon-server   # bind/advertise settings via env, set per scenario
```

A public node runs under `exec inet` bound to `10.99.0.1` or `10.99.0.2`; a
NATed node runs under `exec <home>` and dials out. Two homes with different
types exercise hole punching and relay fallback between NATed peers, since
their public addresses are on the same segment. Harness code will reuse
`scripts/lib/harness.sh` for node launch, kill servers by PID (never by
process name), and call `down-all` on exit. Database access from inside a lab
namespace needs a route to Postgres; scenarios should use a throwaway
database reachable from the public segment, not the shared dev one.
