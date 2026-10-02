# Running a node with no open port

A node behind carrier-grade NAT or a strict firewall can still be a full network member. It
dials out, announces itself as `p2p://<its libp2p peer id>` and is reached over the
connections it opened. Nothing on it needs to be reachable from outside.

## What you need

- One HTTP-reachable seed (or any reachable node) in `AVALON_BOOTSTRAP_PEERS`. First contact is
  an outbound HTTP request to that seed; after that the node finds the rest through libp2p.
- libp2p enabled (`AVALON_DHT_ENABLED=true`, the default for a node that joins a network).
- **No** `AVALON_NODE_URL`. With it unset and libp2p on, the node announces as
  `p2p://<peer id>`. Leave `AVALON_ANNOUNCE_VERIFY_REACHABILITY` on: neighbors admit the node
  because the announce arrived on a libp2p stream authenticated as that peer id, and nothing is
  dialed back. An announce for a `p2p://` URL over plain HTTP, or on a stream authenticated as
  another peer, stores nothing.
- Outbound access to the seed's HTTP port and to the libp2p TCP port of the nodes it connects
  to. `AVALON_LIBP2P_LISTEN_ADDR` defaults to an ephemeral port; the node itself needs no
  inbound rule, and neither `AVALON_LIBP2P_EXTERNAL_ADDR` nor a port forward.
- A relay somewhere on the network for a reachable address: if a public node runs
  `AVALON_RELAY_SERVER_ENABLED=true` (default off) the node reserves a slot and reports
  `relayed`; without one it reports `outbound_only`.

```bash
AVALON_BOOTSTRAP_PEERS=https://seed.example.org
# AVALON_NODE_URL left unset
```

## Settings that apply

Defaults are what the node uses when the variable is unset; all are listed with their limits in
[Configuration](standalone-binary.md#configuration) and `.env.example`.

| Variable | Default | Effect for this node |
| --- | --- | --- |
| `AVALON_RELAY_CLIENT_ENABLED` | `true` | When detection finds the node not dialable, it reserves a slot on a relay. |
| `AVALON_RELAY_CLIENT_MAX_RESERVATIONS` | `2` (at most `8`) | Reservations held at once. |
| `AVALON_RELAY_ADDRS` | unset | Relays to try first, each ending in `/p2p/<relay peer id>`. Otherwise relays are found among connected peers. |
| `AVALON_RELAY_SERVER_ENABLED` | `false` | Serves other nodes as a relay. Leave off on a node with no open port. |
| `AVALON_DCUTR_ENABLED` | `true` | Tries to replace a relayed connection with a direct one by hole punching. A failed attempt leaves the relay in use. |
| `AVALON_AUTONAT_ENABLED` | `true` | Reachability detection; without it `reachability` stays `unknown` and no relay is reserved. |
| `AVALON_ANNOUNCE_VERIFY_REACHABILITY` | `true` | Whether this node, as a neighbor, fetches a new peer's `/nodes/status` before admitting it. Keep on. |
| `AVALON_NODE_HTTP_*` | see `.env.example` | Size, time and concurrency limits for node-to-node HTTP carried over libp2p streams, which is how neighbors call this node. |
| `AVALON_ALLOW_PRIVATE_PEERS`, `AVALON_AUTONAT_ALLOW_PRIVATE_DIALBACK` | `false` | Only for a fleet on one private network; both must be `true` for private addresses to be dialed back. Not needed behind a home router. |

Relay selection prefers a relay in a different /24 (IPv4) or /48 (IPv6) than those already held
and falls back to one in the same network when no other is available, which is what happens on a
single-/24 LAN such as the development network.

## What works

Checked end to end in the NAT lab (`url-less-participation`, see
[NAT scenarios](../for-maintainers/nat-scenarios.md)), with reachability verification on
everywhere and the node behind a no-inbound NAT:

- **Admission and listing.** Neighbors list the node under its `p2p://` URL in
  `/nodes/peers`, `/nodes/discover` and `/nodes/topology`, with `connectivity` `outbound_only`
  (or `relayed`), and measure a latency to it in `/nodes/topology`.
- **Probe and trace.** `POST /nodes/probe` and `POST /nodes/trace` from another node to the
  `p2p://` URL go over the authenticated stream and report a `path`. Over the connection the
  node itself opened the path is `direct`; a path through a relay circuit is reported
  `relayed`, and a hole-punched one `traversed`. A trace routes through a node's active
  neighbors, so run it from a node that has the target as a neighbor.
- **Mirroring.** With `AVALON_MIRROR_PEERS` or `AVALON_MIRROR_ALL_DISCOVERED_SHARDS=true` the
  node polls its sources over outbound HTTP, verifies their heads and serves the same
  head from `/ledger/sth/latest?shard_id=<shard>`.
- **Settlement.** A node that authors a shard through a remote authority submits each batch
  over outbound HTTP:

  ```bash
  AVALON_OWN_SHARD_ID=game:<slug>
  AVALON_SETTLEMENT_REMOTE_URLS=game:<slug>=https://authority.example.org
  AVALON_SETTLEMENT_SUBMIT_KEY=<shared secret>
  ```

  The authority runs the same `AVALON_OWN_SHARD_ID` with the same
  `AVALON_SETTLEMENT_SUBMIT_KEY`. A node with a remote authority must name its shard; leaving
  `AVALON_OWN_SHARD_ID` unset refuses to start. A wrong key leaves the batch pending and logs
  `401 Unauthorized` from the authority.

## Limits

- A `p2p://` peer can call the routes that carry data (`/nodes/relay`, `/nodes/replicate-chat`,
  `/mirror/notify`) on its neighbors: its libp2p handshake authenticates it, and a self-announced
  `p2p://` entry gives it standing with that neighbor. Each route then checks what the caller may
  push: the relay only for scopes with a local subscriber, chat replication only on a node with a
  storage role and within the signer's rate, and mirror notifications only from a configured
  mirror source.
- The credential and its rollout are described in
  [`standalone-binary.md`](standalone-binary.md#credential-format-and-rollout). The NAT lab
  scenario `url-less-credential` shows a node with no URL replicating chat to a neighbor, and a
  keypair that never announced refused on all three routes over a stream and over HTTP.
- It registers mirror interest under its `p2p://` address, so its mirror sources push to it over
  its stream and it still polls as the fallback. A source must have an HTTP URL to be matched.
  It receives no chat: it is not a replication target, because a `p2p://` entry's roles are
  self-reported, and channel and conversation interest is only registered by nodes with an HTTP
  URL.
- Addresses read from interest lookups are checked for shape only (an http(s) URL without
  credentials, query or fragment, or a `p2p://` peer id), deduplicated and capped at 64 per
  lookup. The direct HTTP pushes for mirror notifications and chat relay then apply the same
  outbound address policy as peer dials: link-local and metadata addresses are always refused,
  loopback and private addresses unless `AVALON_ALLOW_PRIVATE_PEERS` is true. A hostname is
  checked against every address it resolves to when the connection is made, and redirects are
  not followed.
- Each neighbor keeps at most 64 `p2p://` entries, and they are evicted first.
- Clients cannot reach the node directly: a `p2p://` URL is not an HTTP address. Serving
  clients needs a fronting node; a fronting gateway is planned, not implemented.
- Two nodes that both have no open port reach each other only through a relay circuit or a
  hole punch; this has not been exercised between two url-less nodes yet.
- First contact needs a reachable HTTP seed.

Planned, not implemented: a fronting gateway for clients, relay re-selection and probing
when a relay degrades, and binding the `p2p://` identity to a key proof.

## Check that it joined

On the node, `GET /nodes/status` shows:

- `libp2p_peer_id`: the id the node announces as.
- `reachability`: `private` once detection concludes peers cannot dial it (`unknown` until a
  peer has answered a dial-back; a node that stays `unknown` has detection off or no peer to ask).
- `connectivity`: `relayed` while a relay reservation is held, `outbound_only` with none,
  `nat_traversed` while a hole-punched connection is open. Omitted while `reachability` is
  `unknown`.
- `relay_reservations` and `relayed_listen_addrs`: the relay slots held (`relay_peer_id`,
  `relayed_addr`, `renewals`) and the circuit addresses peers can dial.
- `hole_punches` (recent attempts with `peer_id`, `succeeded` and an `error` on failure) and
  `punched_peers` (peers with a direct hole-punched connection open now). A failed punch is
  normal behind a symmetric NAT; the node keeps `relayed`.

On a neighbor:

```bash
curl -s https://neighbor.example.org/nodes/discover | jq '.peers[] | select(.base_url | startswith("p2p://"))'
curl -s -X POST https://neighbor.example.org/nodes/probe -H 'content-type: application/json' \
  -d '{"target":"p2p://<peer id>"}'
```

The entry should list the node's peer id and `connectivity`, and the probe should return
`"ok": true` with a `path`. Announces repeat every `AVALON_ANNOUNCE_INTERVAL_SECS`, so allow
one interval after start.
