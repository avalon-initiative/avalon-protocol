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
- Outbound access to the seed's HTTP port and to libp2p (TCP 4001 by default) on the nodes it
  connects to. If a public node runs `AVALON_RELAY_SERVER_ENABLED=true` the node also reserves
  a relay slot and reports `relayed`; without one it reports `outbound_only`.

```bash
AVALON_BOOTSTRAP_PEERS=https://seed.example.org
# AVALON_NODE_URL left unset
```

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

- A `p2p://` peer proves only that it holds its key, so it cannot call the routes that inject
  data (`/nodes/relay`, `/nodes/replicate-chat`, `/mirror/notify`) on its neighbors; they refuse
  it. It receives no chat or mirror pushes either, because its URL is not advertised for
  interest lookups, so it falls back to polling.
- Each neighbor keeps at most 64 `p2p://` entries, and they are evicted first.
- The `p2p://` URL is never given to browsers or used in signed grants, so clients cannot be
  pointed at the node directly. Serving clients needs a fronting node.
- Two nodes that both have no open port reach each other only through a relay circuit or a
  hole punch; this has not been exercised between two url-less nodes yet.
- First contact needs a reachable HTTP seed.

## Check that it joined

On the node, `GET /nodes/status` shows `connectivity` `outbound_only` or `relayed` and
`libp2p_peer_id`. On a neighbor:

```bash
curl -s https://neighbor.example.org/nodes/discover | jq '.peers[] | select(.base_url | startswith("p2p://"))'
curl -s -X POST https://neighbor.example.org/nodes/probe -H 'content-type: application/json' \
  -d '{"target":"p2p://<peer id>"}'
```

The entry should list the node's peer id and `connectivity`, and the probe should return
`"ok": true` with a `path`. Announces repeat every `AVALON_ANNOUNCE_INTERVAL_SECS`, so allow
one interval after start.
