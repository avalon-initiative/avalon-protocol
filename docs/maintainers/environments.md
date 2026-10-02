# Network environments: bringing them up, tearing them down, and protecting prod

How a maintainer creates, operates and retires the networks Avalon runs for itself: development, integration, staging and production. It also covers adding a network to the published trust-anchor list. Operating a network of your own, outside Avalon's list, is a different job: see
[running your own network](https://github.com/avalon-initiative/avalon-docs/blob/main/developers/running-your-own-network.md).

The concepts (what each tier is for, and why) are in
[network environments](https://github.com/avalon-initiative/avalon-docs/blob/main/architecture/network-environments.md).
This page is the procedure.

## The tiers at a glance

| Tier | `network_id` | `environment` in the list | Purpose | Reset policy |
| --- | --- | --- | --- | --- |
| Local | `avalon-dev-local` | `local-dev` | One developer's machine. Template entry, no real server behind its key. | Wipe at will. |
| Dev | `avalon-dev-<name>` | `dev` | A real non-production deployment for trying changes. One to a few nodes. | Wipe at will while actively working it. |
| Int | `avalon-int-<name>` | `int` | A 1 to 5 node interconnected test bed that proves a change integrates across nodes before it ships. | Wiped and rebuilt on a regular cadence (monthly), which doubles as proof the network can be stood up from nothing. |
| Staging | `avalon-staging-<name>` | not yet a listed value | Volume testing: realistic node count and data volume. | Resettable between runs. |
| Prod | `avalon-mainnet-N` | `prod` | The one canonical public network. | Never wiped. See [protecting prod](#protecting-prod). |

Staging is a proposed tier. The `environment` field in
[`docs/trusted-networks.json`](../trusted-networks.json) documents `local-dev`,
`dev`, `int` and `prod` today, and nothing in the repository validates the value,
so adding `staging` is a documentation and Hub-display change plus an entry,
not a format change. Until then a staging network can be listed as `dev`.

Keep tiers on separate hosts and separate Postgres instances. A node refuses to
start under a different `network_id` than its database was created with, but that
guard does not stop a wrong `DATABASE_URL` from being wiped.

## Bringing a network up

Every tier is the same recipe at a different size and with different custody of
the settlement key.

1. **Choose the `network_id`** following the table. Names are unique and never reused
   for a different network. `avalon-mainnet-N` is reserved for the canonical
   mainnet.
2. **Generate the settlement signing key** for the network. It is a 32-byte
   Ed25519 seed:

   ```bash
   python3 -c 'import secrets; print(secrets.token_hex(32))'
   ```

   Derive the verify key (the value that goes in the trust entry):

   ```bash
   python3 -c "
   from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey as K
   print(K.from_private_bytes(bytes.fromhex(input().strip())).public_key().public_bytes_raw().hex())"
   ```

   Paste the seed on standard input so it never appears in shell history. A node
   also derives the verify key itself when `AVALON_SETTLEMENT_VERIFY_KEY` is unset.
3. **Start the first node** with `AVALON_NETWORK_ID` and
   `AVALON_SETTLEMENT_SIGNING_KEY` set, using
   [`avalon setup`](../projects/backend-server/for-hosters/standalone-binary.md)
   or `make stack-up`. First start commits the `network_id` into the ledger genesis.
4. **Add more nodes** as replicas with the same `AVALON_NETWORK_ID`. They learn the
   network from the seeds in the entry and mirror `core`
   ([seed nodes](../projects/backend-server/for-hosters/seed-nodes.md)).
5. **Add the network to the trust-anchor list** as described below.
6. **Verify** before telling anyone: every node answers `GET /nodes/status` with the
   expected `network_id`, and `GET /ledger/sth/latest` verifies against the entry's
   `verify_key` with a `tree_size` that keeps up.

Per-tier notes:

- **Dev:** `make stack-up` on one machine, or a few nodes on a LAN. `make db-reset`
  wipes and re-migrates a dev database. Do it freely, against dev databases only.
- **Int:** the same, with 2 to 5 nodes on separate machines, each built from the
  release under test. Treat the monthly rebuild as the rehearsal for standing up a
  network from nothing: use only the documented commands, and file a gap for anything
  that needed a manual step.
- **Staging:** int's shape at production's node count. The load harness
  (`scripts/load-tests.sh`) starts its own isolated local nodes and refuses any
  non-loopback target, so it cannot drive a deployed staging network. Volume
  testing against a deployed network has no tooling yet.
- **Prod:** see below. Nothing in this section is run casually against it.

## Adding a network to the trust-anchor list

The list is [`docs/trusted-networks.json`](../trusted-networks.json). The SDKs fetch
it at runtime and nodes bake in the copy from their build, so it is the public record
of which networks exist. Changing it is a pull request.

1. Append an entry with `label`, `network_id`, `verify_key`, `signing_key_id`,
   `environment`, `server_url`, `seed_nodes` and `notes`. Field meanings are in
   [network trust anchors](https://github.com/avalon-initiative/avalon-docs/blob/main/protocol/network-trust-anchors.md#the-trust-anchor-list).
2. Add the matching row to the README's trusted-networks table.
3. Run `make check-trust-anchors` (also part of `make check`). It fails when the table
   and the file disagree.
4. List several seeds on different hosts and networks, and confirm each passes the
   monitoring checks in [seed nodes](../projects/backend-server/for-hosters/seed-nodes.md)
   before merging.
5. Open the PR. A `prod` entry additionally needs the checklist under
   [protecting prod](#protecting-prod).

An entry carries no availability promise unless its `notes` say so. Say plainly in
`notes` when a network is private, resettable or short-lived.

## Tearing a network down

1. Remove its `seed_nodes` and then the entry itself in a PR, and merge it first.
   Remove a node from the list before decommissioning it, so joiners are not sent to
   a dead address.
2. Stop the nodes: `make stack-down` for a compose stack, or stop the service.
   Kill by process id, never by process name, when several nodes share a host.
3. Drop the databases and delete the data directories and key files.
4. Retire the `network_id`. Never reuse it for a different network.

For a resettable tier you are rebuilding rather than retiring, keep the entry, wipe
the databases (`make db-reset` per node) and bring the nodes back up. Wipe every
node of the network together before restarting any. A surviving mirror that still
holds the old history will see the rebuilt chain's heads as conflicting with what it
recorded and report equivocation. Reusing the same key and `network_id` keeps the entry
valid; a new key means a new `verify_key` in the entry, updated in the same PR.

## Protecting prod

Today the protection is procedural. Nothing in the code refuses a destructive command
because a node is on a production network: `make db-reset` and
`migrate reset` operate on whatever `DATABASE_URL` points at. A guard is not
yet built, so these rules carry the weight:

- **Never run `make db-reset`, `make migrate-down` or any wipe against a prod database.** Use a separate machine, a separate
  `.env` and separate database credentials for prod, so a mistyped target fails on
  authentication.
- **A rollover is not a wipe.** Moving from `avalon-mainnet-N` to
  `avalon-mainnet-(N+1)` is a deliberate, rare, maintainer-decided migration
  (`avalon migrate-network`) that carries the final checkpoint forward. See
  [network trust anchors](https://github.com/avalon-initiative/avalon-docs/blob/main/protocol/network-trust-anchors.md#genesis-reset-and-migration).
- **Changes to the prod entry go through a reviewed PR** against `main`, which is
  protected: required checks, branches up to date, no direct pushes. Code ownership is
  defined in `.github/CODEOWNERS`.
- **Releases are tag-only.** A published tag and any package are outward-facing and
  happen on an explicit decision, never as a side effect of a PR.
- **Key custody.** The prod settlement key is generated once under a documented
  ceremony, backed up, and rotated only by
  [`key-rotation.md`](../projects/backend-server/for-maintainers/key-rotation.md). The
  ceremony document is planned under the mainnet genesis work and does not exist yet.
- **No test runs against a shared database.** Tests and drills use a throwaway database.

Before a `prod` entry is merged, all of the following hold: the key ceremony is
complete and the signing key is backed up; at least two seeds on separate hosts pass the
monitoring checks; the release binary the nodes run is the tagged release and its
provenance is verified ([verifying a release](../projects/backend-server/for-hosters/verifying-a-release.md));
and the rollback path in [upgrading](../projects/backend-server/for-hosters/upgrading.md)
has been exercised on int.

## Related

- [Seed nodes and trust entries](../projects/backend-server/for-hosters/seed-nodes.md)
- [Key rotation](../projects/backend-server/for-maintainers/key-rotation.md)
- [Cutting a release](../projects/backend-server/for-maintainers/release-process.md)
- [Local development](local-development.md) and [multi-node testing](../projects/backend-server/for-maintainers/multi-node-testing.md)
- [Upgrading and patching a running node](../projects/backend-server/for-hosters/upgrading.md)
