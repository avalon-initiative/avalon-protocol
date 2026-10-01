# NAT scenarios

A scenario suite that runs real `avalon-server` processes inside the [NAT lab](./nat-lab.md):
a public relay and a public seed on the lab's shared segment, and nodes in home networks behind
each kind of NAT. It checks that a node behind a NAT is detected correctly, reserves a relay
slot, upgrades to a direct connection by hole punching where the NAT allows it, and falls back
to the relay where it does not. Unit and in-process libp2p tests cover the pieces; this suite
covers the whole path over real NATs.

## Running it

```bash
sudo scripts/nat-scenarios.sh                       # every scenario
sudo scripts/nat-scenarios.sh punch-port-restricted # a subset
```

Needs root, `ip netns`, nftables, `jq`, `curl`, `xxd`, a `.env` with a reachable `DATABASE_URL`
(or `AVALON_ENV_FILE`) and the debug server binary (`NAT_SERVER_BIN` to use another).

- Every node gets its own Postgres schema, dropped on exit. The lab's public segment reaches the
  host Postgres over a veth pair at `10.99.0.250`; the script removes it and every `avlab-*`
  namespace on exit. Nothing touches the default schema or a running node.
- The dev server binary loads the workspace `.env` from a compiled-in path, so the script sets
  every variable that file could set (empty unsets it). Without that a scenario node would join
  the fleet.
- Scenarios wait on `GET /nodes/status`; a failure prints the node's `reachability`,
  `connectivity`, reservation count, punched peers and hole-punch outcomes. `NAT_LAB_KEEP=1`
  keeps the logs, `NAT_LAB_LOG` sets `RUST_LOG` for every node (for example
  `info,libp2p_dcutr=debug`), and `NAT_LAB_TIMEOUT` sets how long one wait may take (default 90 s).

## Scenarios

| Scenario | What it does | In CI |
|---|---|---|
| `public` | A node on the public segment is detected `public` and reports `direct`. | yes |
| `full-cone-direct` | A node behind the lab's full-cone NAT, which forwards every inbound port, is detected `public`. | no |
| `relayed-port-restricted`, `relayed-restricted-cone`, `relayed-symmetric`, `relayed-no-inbound` | A node behind that NAT is detected `private`, reserves a slot on a relay it found through gossip from one HTTP seed, reports `relayed` and advertises its circuit address. | `relayed-symmetric` |
| `punch-port-restricted` | Two nodes behind port-restricted NATs, bootstrapped from the seed only, find each other through the relay and upgrade to a direct connection: `nat_traversed`, `punched_peers` and a successful `hole_punches` entry, and `POST /nodes/probe` of the relay from the NATed node reports `path: direct`. | `punch-port-restricted` |
| `punch-symmetric-fallback` | The same with symmetric NATs. The punch fails, the failure is recorded, and both nodes keep reporting `relayed`, and the same probe reports `path: direct`. The node with the lower peer id starts the punch, so either one records the failure. | no |
| `outbound-only` | A node behind a no-inbound NAT with no relay available reports `outbound_only` and no reservation. | no |
| `url-less-admission` | A node behind a no-inbound NAT with no `AVALON_NODE_URL`, and reachability verification on everywhere, announces as `p2p://<peer id>`: both neighbors list it in `/nodes/discover`, the seed's topology describes it, and a `p2p://` announce for the relay's id over plain HTTP stores nothing. | no |
| `relay-failover` | A node holds a reservation on one of two relays; the relay is stopped and the reservation moves to the other. | no |

A hole punch depends on two SYNs crossing inside two NAT filters, so it can take more than one attempt and is not yet run in CI. A punch takes about a minute: the node with the higher peer id waits before dialing a peer that
is reachable only through a relay, then hole punching makes up to three attempts.

## Known limitations

- **Probing a node behind a NAT is not covered here.** A stream request needs the target's
  libp2p identity to be bound to its URL, and the suite's nodes skip the reachability check that
  binds it, so a probe of a home node goes to its unreachable URL. The `traversed` and `relayed`
  labels are covered by in-process libp2p tests instead.
- **The lab's full-cone NAT forwards every inbound port**, so it is reachable by any peer. It is
  more permissive than a real full-cone NAT, see the [NAT lab](./nat-lab.md) notes.
- Most scenarios run nodes that skip the announce reachability check, because a node behind a NAT
  cannot pass an inbound HTTP fetch. A node with a URL nobody can reach is not admitted by
  default, so it is found through gossip and confirmed over libp2p. A node with no
  `AVALON_NODE_URL` announces as `p2p://<peer id>` and is admitted with the check on, see
  `url-less-admission`. Such a node cannot call the write routes `/nodes/relay`,
  `/nodes/replicate-chat` and `/mirror/notify` on its neighbors; the scenario does not cover them.

## Live drill

The suite models NATs with rules; a real NAT differs in timeouts, port allocation and
hairpinning. Before the NAT work is called done, repeat these on the dev fleet with a node behind
a real router (a laptop on a home connection with no port forwarded):

1. Roll the fleet to the release candidate, with one node enabled as a relay
   (`AVALON_RELAY_SERVER_ENABLED=true` and a stated `AVALON_LIBP2P_EXTERNAL_ADDR`).
2. Start the node behind the router with only a seed URL in `AVALON_BOOTSTRAP_PEERS`.
3. Check `GET /nodes/status` on it: `reachability` `private`, one reservation, `connectivity`
   `relayed`, within a minute.
4. Start a second node behind a different router and check that one of them reports
   `nat_traversed` with the other in `punched_peers`, or a recorded failed punch and `relayed`.
5. Stop the relay and check the reservation moves or the node reports `outbound_only`.

Record the outcome, the NAT types if known, and the versions in the tracking issue.
