# Choosing your shard

Every node authors exactly one shard: the history it commits to its own ledger
and signs with its own settlement key. `AVALON_OWN_SHARD_ID` names it. This page
explains which value a node should use, how to get a registered key for it, what
the startup guard checks, and how one operator runs a shard with redundancy.

## Which shard does my node author?

| You are | Set `AVALON_OWN_SHARD_ID` | Signing key |
|---|---|---|
| Running the network's core authority, the node whose key is pinned in `docs/trusted-networks.json` | `core` (the default) | The pinned key |
| Running a lone throwaway `local-dev` node (a fresh `make stack-up`) | `core` (the default) | The generated key; the server warns that it is not pinned |
| Running anything else on a real network: a game, app or service, or a community node | `game:<slug>`, `app:<slug>` or `service:<slug>` | A `shard_settlement` key registered for the integrator `<slug>` |
| Running a second or third node for the same integrator | `game:<slug>/<instance>` | Any unrevoked `shard_settlement` key registered for `<slug>` |
| Joining with no integrator registration at all | `node:<key-hash>`, derived from the node's own key | The node's own freshly generated key — nothing to register |

`core` is the reserved label of the network's pinned core authority. A node that
holds some other key and leaves the default in place becomes a second author of
`core`. Clients pinned to the network key see its tree heads as a mismatch, the
same as an impostor's. The startup guard below exists to stop that.

`instance` is 1-64 characters of `[a-z0-9-]`, starting with `[a-z0-9]`. The owner
part (`<slug>`) alone decides which keys are accepted, so `game:wow/1` and
`game:wow/2` are both verified against the keys registered for `wow`.

## Authoring with no registration: self-certifying shard ids

`AVALON_OWN_SHARD_ID` can also be `node:<key-hash>`, where `<key-hash>` is the
lowercase hex SHA-256 digest of the node's own settlement verify key
(`avalon_protocol::shard_identity::derive_self_certifying_id`). This id needs no
integrator, no root key, no `avalon add-shard-key` call, and no core authority
online — the id already names the exact key a verifier must check its tree
heads against, so there is nothing left for a registry to resolve. A node
picks a settlement key, derives its id, sets `AVALON_OWN_SHARD_ID` to it, and
starts.

A human-readable name for a self-certifying shard comes later, from a
name-binding claim (`avalon_protocol::shard_identity::NameBindingClaim`) signed
by the same key — the naming/resolution layer this claim feeds is separate,
not-yet-built work. A self-certifying shard with no name at all authors and
verifies exactly the same as one with a claimed name; a name is never required
to join or to be verified.

## Getting a registered shard key

A shard key is an operational issuer key with purpose `shard_settlement`,
authorized by the integrator's root key. The network's core authority records the
integrator and the key, so the core authority is the registrar: registration
requests go to a core authority node (`--server`, or `AVALON_SERVER_URL`).

1. Generate the node's settlement key pair if the node does not have one yet
   (see [`hosting-quickstart.md`](hosting-quickstart.md)); the node holds the
   private half in `AVALON_SETTLEMENT_SIGNING_KEY`.
2. Register the integrator, if it is not registered yet. This saves the
   integrator's root key under `_running/keys/integrator-<slug>.signing-key`:

   ```
   avalon register-integrator --slug <slug> --name "<name>" --owner-name "<owner>" --server <core-authority-url>
   ```

3. Register the node's public settlement key as a shard key. The verify key is
   taken from `AVALON_SETTLEMENT_VERIFY_KEY`, or derived from
   `AVALON_SETTLEMENT_SIGNING_KEY`, unless `--verify-key <hex>` is given:

   ```
   avalon add-shard-key --integrator <slug> --server <core-authority-url>
   ```

   This is `POST /integrations/<slug>/keys` with `role: operational` and
   `purpose: shard_settlement`, signed by the root key. Use `--key <path>` if the
   root key is stored elsewhere.
   The command authenticates as the integrator, so run it on a host that has both
   `_running/keys/integrator-<slug>.signing-key` and `integrator-<slug>.json` (the
   saved credentials holding the key id); pass `--key-id <uuid>` instead of the
   JSON file if you only have the root key.
4. On the node, set `AVALON_OWN_SHARD_ID=game:<slug>` (or the integrator's
   `app`/`service` category, optionally with `/<instance>`) and restart.

Both commands are in the default `dev-tools` build of the `avalon` binary. Any
number of unrevoked shard keys may exist for one integrator; each verifies the
whole owner's shard family.

## The startup guard

When `AVALON_OWN_SHARD_ID` is `core` and the node has a settlement signing key,
the server compares that key's public half with the `verify_key` pinned for
`AVALON_NETWORK_ID` in `docs/trusted-networks.json`.

| Situation | Result |
|---|---|
| Keys match | Starts. `GET /nodes/status` reports `core_author_pinned: true`. |
| Keys differ, anchor `environment` is `dev`, `int` or `prod` | Refuses to start. |
| Keys differ, anchor is `local-dev`, `AVALON_BOOTSTRAP_PEERS` or `AVALON_MIRROR_PEERS` is set | Refuses to start. |
| Keys differ, anchor is `local-dev`, no peers configured | Starts with a warning. `core_author_pinned: false`. |
| The network has no anchor, or the shard is not `core` (including a named shard or a `node:<key-hash>` self-certifying shard) | No check. `core_author_pinned` is absent. |

A self-certifying id needs no entry in this table at all, and not because it
was carved out as an exception: `core` is the one shard id with a key pinned
*outside* the id itself, so it is the one case that can ever mismatch. A
self-certifying id's own bytes are a function of its key — there is no wrong
key to pin it against, so nothing here applies to it, the same way nothing
here applies to a named shard.

The refusal reads:

```
refusing to start: this node authors the reserved `core` shard on network `<network>` but its settlement key does not match the key pinned for that network. Pinned key: id `<id>`, verify key <hex>. This node's key: id `<id>`, verify key <hex>. Clients pinned to `<network>` will report this node's tree heads as a key mismatch. Fix by either (1) setting AVALON_SETTLEMENT_SIGNING_KEY to the pinned key, if this node IS the network's core authority, or (2) authoring a named shard: set AVALON_OWN_SHARD_ID to a registered shard (for example `game:<integrator-slug>`) and use that shard's registered shard_settlement key
```

Fixes:

- The node is the core authority: restore the pinned key in
  `AVALON_SETTLEMENT_SIGNING_KEY` (and its `AVALON_SETTLEMENT_SIGNING_KEY_ID`).
- The node is anything else: follow "Getting a registered shard key" above and set
  `AVALON_OWN_SHARD_ID`.
- The node is a newcomer's local experiment that started failing because it
  added peers: either use a named shard, or remove the peer settings.

An invalid `AVALON_OWN_SHARD_ID` also refuses to start:

```
refusing to start: AVALON_OWN_SHARD_ID="<value>" is invalid: <reason>. Use `core` (only for the network's pinned core authority), a registered shard id of the form `game|app|service:<integrator-slug>[/<instance>]`, or a self-certifying `node:<key-hash>` id derived from this node's own key (avalon_protocol::shard_identity::derive_self_certifying_id)
```

The guard checks the key, not registration. A node with a named shard whose key
was never registered starts, but peers and clients cannot verify its tree heads;
`GET /ledger/cross-shard-root` lists such a shard as missing.

## Mirror the core authority

Every node, including one that authors a named shard, should set
`AVALON_MIRROR_PEERS` to the core authority. Two reasons:

- Clients pinned to the network see only mirrored core history. A node that
  serves none cannot be verified by those clients.
- The registration events for every shard's keys live in the core ledger. A
  node holding a mirror derives sibling shards' `shard_settlement` keys from it,
  so it can still verify other shards, and compose
  `GET /ledger/cross-shard-root`, while the core authority is unreachable.
  Without a mirror, only the registrar's local key table has them.

A node with a named `AVALON_OWN_SHARD_ID` and an empty `AVALON_MIRROR_PEERS`
logs one warning at startup:

```
this node authors the named shard `<shard>` but AVALON_MIRROR_PEERS is empty, so it mirrors no core history: clients pinned to the network cannot verify a node that serves no mirrored core history, and this node cannot verify sibling shards while the core authority is unreachable. Set AVALON_MIRROR_PEERS to the core authority
```

## Witness cosigning

Every node can act as a witness: a node that mirrors a shard checks that each new
head extends the last head it cosigned (an RFC 6962 consistency proof), refuses to
cosign two different roots at one size, and stores and serves its cosignature. Nothing
here needs registration or a named shard, and it does not change how clients pinned
to the network verify heads.

| Setting | Meaning |
|---|---|
| `AVALON_WITNESS_SIGNING_KEY` | Hex 32-byte Ed25519 seed for this node's witness key. If unset, a node that already has `AVALON_SETTLEMENT_SIGNING_KEY` cosigns with that key; a pure mirror with neither only verifies. |
| `AVALON_WITNESS_SIGNING_KEY_ID` | Override for the key id. Leave it unset: the default is the hex verifying key, which is what other nodes match against. A non-hex id makes the node advertise no witness key. |
| `AVALON_WITNESS_COSIGNING_ENABLED` | `false` or `0` turns cosigning off (verify only). On by default. This is also the rollback switch, see [`witness-policy-rollout.md`](../for-maintainers/witness-policy-rollout.md). |
| `AVALON_DATA_DIR` | Directory for node-local, non-secret state, currently the known list (`known_list.json`). Back it up with the node's data; losing it only means the list refills. |
| `AVALON_KNOWN_LIST_*` | Capacity (10), reserved anchor slots (2), per-prefix cap (2), freshness (10 min), probation (30 min) and refill interval. The defaults are the design's; change them only for tests. |

A node advertises its witness key in `POST /nodes/announce` with a signature proving it
holds the key. Other nodes only count a key toward their known list when it comes back
in that peer's own announce response, so a key someone else gossips for your URL never
occupies a slot. Nothing to configure for that. A node whose key is never proven, or
that runs with cosigning off, still joins the peer table and gossip and serves the
network as before; it is just not a witness for anyone.

Keep the witness seed like the settlement key: owner-only file or secret store, never
committed, never logged. A witness key does not need a registration.

## High availability for one operator

One shard is one ledger with one signing history. Two patterns keep it available;
they differ in whether the ledger is shared.

### Hot standby of one shard

Register a second `shard_settlement` key for the same integrator and run a second
node with `AVALON_OWN_SHARD_ID` set to the same shard id, configured as a standby:

- The standby's database is an operator-run replica of the active node's
  database. The protocol does not replicate a ledger between two authors; the
  operator's database replication does.
- Only one node may sign at a time. Enforce this with a lease or fence the
  operator controls (for example a lock in the database or an orchestrator's
  leader election), starting the standby's `avalon-server` only after the active
  node is confirmed stopped or fenced.
- Two nodes signing the same shard concurrently fork its ledger. Peers detect
  this as equivocation (see
  [`equivocation-response.md`](../for-maintainers/equivocation-response.md)) and
  refuse both. Preventing it is the operator's responsibility; the protocol only
  detects it.
- Takeover follows [`authority-promotion.md`](../for-maintainers/authority-promotion.md).
  Using a distinct second key lets the old key be revoked afterwards without
  rotating the standby.

### Sibling shards for load balancing and failover

Give each node its own shard id under the same owner: `game:wow/1`, `game:wow/2`.
Every sibling is authorized by the same integrator keys, but each is an
independent ledger with its own tree heads:

- Writes are routed to one sibling by the operator's choice of remote authority
  (`AVALON_SETTLEMENT_REMOTE_URLS` on the nodes that forward writes). To fail
  over, point new writes at a healthy sibling.
- Ledgers are never merged. A sibling's history stays in that sibling's shard.
- Mirrors that already followed a sibling keep its full verified history after
  the sibling dies, so no history is lost even though the node is gone.
- Events issued by the integrator itself (attestations and their revocations)
  route to `game:<slug>` without an instance. Sibling nodes receive them only when
  configured as the remote authority for that exact shard id, so route them to the
  sibling that should own that traffic.
