# Protocol Events

A protocol event is a durable fact Avalon considers part of its history. **Not
every game action is a protocol event; ordinary gameplay never becomes one.**
**Events are the canonical record — every table is a projection of them**
([#75](https://github.com/LunarVagabond/avalon-protocol/issues/75)), and
**history is append-only: a correction is a new event, never an edit.**

## Hot gameplay vs durable events

| Stays game-side (never an event) | May enter durable history |
|---|---|
| movement, combat, physics, AI | achievement issued / revoked |
| HP, XP ticks, NPC state, player position | game event result |
| matchmaking, ordinary chat | guild created, membership / role changed |
| game-specific inventory and economy | game registered, binding established |
| typing indicators, connection state, presence | issuer registered, key rotated, suspended |
| | attestation issued / superseded, ownership transferred |

The test is the one in [`./overview.md`](./overview.md): would this fact matter
outside the game that produced it, and does Avalon promise to preserve it? If
either answer is no, it is not a protocol event. Presence in particular is never
one ([`./presence.md`](./presence.md)).

## The types

`crates/protocol/src/events.rs` defines three types, unchanged since the
scaffold:

```rust
pub struct ProtocolEvent {
    pub id: Uuid,
    pub kind: String,          // namespaced, e.g. "achievement.issued"
    pub issuer: GlobalId,      // who asserts this fact
    pub subject: GlobalId,     // what/whom it is about
    pub payload: serde_json::Value,
    pub timestamp: OffsetDateTime,
    pub version: u32,          // payload schema version for this kind
}

pub struct EventBatch { pub id: Uuid, pub events: Vec<ProtocolEvent>, pub created_at: OffsetDateTime }
pub struct Commitment { pub batch_id: Uuid, pub proof: Vec<u8>, pub committed_at: OffsetDateTime }
```

`kind` is a string rather than a closed enum so a new kind does not require a
protocol version bump. `Commitment.proof` is deliberately opaque: `protocol` does
not know whether it is a ledger hash, a signed tree head, or a Merkle root
anchored elsewhere.

## The pipeline

```text
Protocol Event
      |
      +----> Query Projection          (indexer; rebuildable)
      |
      +----> Event Buffer
                  |
                  v
              Batching                  (EventBatch; #38)
                  |
                  v
          Commitment / Merkle Root      (batch_root plus a real RFC 6962 Merkle
                                          root/STH now, #40 decided, #210)
                  |
                  v
              Settlement                (SettlementProvider; Avalon's own chain, #79/#93)
```

An event fans out to the read model and to the settlement path. The projection
is the optimized copy; the settled history is the record. Never one event = one
settlement transaction — see [`./settlement.md`](./settlement.md).

A batch is not itself a protocol event and never gets a `kind` — it is the
settlement layer's unit of commitment over a group of events (`EventBatch`),
not a durable fact anyone issues, indexes, or replays on its own.

## Design properties

Every durable event must be able to be:

- **signed** — by the actor asserting it (an issuer's key for attestations, the
  identity's key for identity and profile claims once
  [#73](https://github.com/LunarVagabond/avalon-protocol/issues/73) lands)
- **verified** — signature against the key that was valid at `timestamp`
- **indexed** — applied idempotently to a projection
- **replayed** — in order, from genesis, to rebuild any projection
- **committed** — batched behind a durable commitment
- **revoked or corrected** — by a later event referencing it, never by mutation
- **versioned** — decodable for as long as the log exists
- **audited** — issuer, subject, timestamp, and position in the log are all
  independently checkable

Events carry enough canonical information to reconstruct required state and no
more. Anything derivable from other events is computed by the indexer, not
stored twice.

## Kind catalogue (proposed)

Naming: `<domain>.<past-tense-verb>`. This table is the starting point that
[#82](https://github.com/LunarVagabond/avalon-protocol/issues/82) finalizes;
until then it is proposed, not normative. "Network" as signer means the node
records the fact on behalf of an authenticated actor and is the
milestone-1 stand-in until actor signatures exist.

| Kind | Issuer → subject | Payload (canonical) | Drives | Signed by |
|---|---|---|---|---|
| `identity.created` | identity → identity | identity id | identities | identity's Ed25519 event-signing key (#73, done) |
| `identity.signing_key_added` | identity → identity | new signing key id/public key, device label, approving key id | identity signing keys | the approving device's key (#135, done) |
| `identity.signing_key_revoked` | identity → identity | revoked signing key id | identity signing keys | network (milestone-1 stand-in, #135, done) |
| `identity.recovery_configured` | identity → identity | guardian ids, threshold | recovery guardian settings | identity key (session-authenticated, #201, done) |
| `identity.recovery_requested` | identity → identity | request id, threshold | recovery requests | network (milestone-1 stand-in — the requester by definition has no session; #201, done) |
| `identity.recovery_approved` | identity (guardian) → identity | request id, guardian id, approvals count, threshold, delay end | recovery requests/approvals | network (milestone-1 stand-in, #201, done) |
| `identity.recovery_cancelled` | identity (owner or guardian) → identity | request id, cancelled by, reason | recovery requests | network (milestone-1 stand-in, #201, done) |
| `identity.recovered` | identity → identity | request id, new device label | identity keys | network (milestone-1 stand-in, #201, done) |
| `profile.updated` | identity → identity | changed promised-durable fields (`display_name`, `discriminator`, `avatar_url`, `bio`, `favorite_genres`, `pronouns`) | profiles | identity key |
| `game.registered` | game → game | slug, name, developer, requested capabilities, initial key | games, registry | game key |
| `game.binding_established` | identity → game | identity, game | bindings, registry identities | identity key |
| `game.binding_ended` | identity → game | binding ref | bindings | identity key |
| `permission.granted` | identity → game (per capability) | binding, capability | permission grants | identity key |
| `permission.revoked` | identity → game (per capability) | binding, capability, reason | permission grants | identity key |
| `issuer.registered` | issuer → issuer | issuer id, initial key set | issuers | issuer key |
| `issuer.key_added` | issuer → issuer | key id, public key, algorithm, validity window | issuer keys | existing issuer key |
| `issuer.key_revoked` | issuer → issuer | key id, reason (`rotated`, `compromised`, …) | issuer keys | issuer key |
| `issuer.key_expired` | issuer → issuer | key id | issuer keys | issuer key or network |
| `issuer.suspended` / `.reinstated` / `.revoked` / `.deprecated` | network or issuer → issuer | reason, effective at | issuer status | operator (audited) or issuer |
| `friend.requested` / `.accepted` / `.removed` | identity → identity | the two identities, actor | friendships | acting identity's key — decided promised-durable; see [social-graph.md](./social-graph.md) |
| `guild.created` | identity → guild | name, tag, description, founder | guilds | founder key |
| `guild.updated` | guild → guild | changed fields (motd, banner, links, recruiting, join_policy, ...) | guilds | acting officer's key |
| `guild.role_defined` / `.role_deleted` | identity → guild | name_index, name, permissions, description, badge (icon, color), actor | role definitions | acting member's key |
| `guild.member_added` / `.member_removed` | guild → identity | role, actor | rosters, history | acting member's key |
| `guild.role_changed` | guild → identity | old role, new role, actor | rosters, history | acting member's key |
| `guild.owner_transferred` | guild → identity | old owner, new owner, actor | guilds | acting owner's key |
| `guild.game_associated` | guild → game | guild, game | associations | guild officer key |
| `guild.favorite_games_updated` | guild → guild | favorited game ids, actor | favorites | acting officer's key |
| `guild.channel_created` / `.channel_renamed` / `.channel_archived` | guild → channel | channel id, name, actor | channels | acting officer's key |
| `game_schema.published` | game → schema | game id, version, `.proto` source, superseded_by | schema discovery (#255) | game key |
| `achievement.defined` | game → achievement id | name, description, schema | definitions | game key |
| `achievement.definition_updated` | game → achievement id | name, description, schema, version | definitions | game key |
| `achievement.definition_retired` | game → achievement id | achievement id | definitions | game key |
| `achievement.issued` | game → identity | achievement id, attestation id, evidence ref | attestations | issuer key |
| `achievement.revoked` | game → attestation | attestation ref, reason code, reason | attestation status | issuer key |
| `milestone.defined` / `.definition_updated` / `.definition_retired` | app/service → milestone id | same fields as the `achievement.*` row above | definitions | app/service key |
| `milestone.issued` / `.revoked` | app/service → identity / attestation | same fields as `achievement.issued`/`.revoked` above | attestations | issuer key |
| `attestation.superseded` | game → attestation | old ref, new ref | attestation status | issuer key |
| `game_event.result_issued` | game → identity | `achievement.issued` with the game-event schema | attestations, registry | issuer key |
| `recognition.published` | game → issuer | recognized claim types / scopes | recognition graph | game key |

Conventions: `issuer` and `subject` are `GlobalId`s
(`crates/protocol/src/ids.rs`) — namespaced, e.g.
`game:ashen-realms:achievement:dragon_slayer`, so two games' `dragon_slayer`
never collide ([`./provenance.md`](./provenance.md)). Each kind has exactly one
payload schema per version.

## Versioning policy

History may outlive every current maintainer. Therefore:

- Adding an optional field does not change `version`.
- Removing, renaming, or changing the meaning of a field bumps `version`.
- Every version ever emitted stays decodable forever. Decoders are added, never
  deleted.
- An unknown kind is preserved and skipped by an indexer that does not
  understand it; it is never dropped from the log.
- Attestations additionally carry an issuer-declared schema reference so a
  consumer can recognize "game event result, schema v1" independently of the
  issuer's naming or which kind of event (tournament, seasonal championship,
  community campaign, ...) produced it.

"We can change the schema later" is true of a projection table and false of the
log.

## History vs current state

Both are kept, and kept distinct:

```text
Guild membership history (events)        Current projection (indexer)
2027-01-01  X joins Guild A              Player X
2027-04-14  X becomes Officer                Guild: none
2028-02-10  X leaves Guild A
```

History is reconstructable; current state is optimized for reads and can always
be thrown away and rebuilt. A "current status" column on a projection row (e.g.
an attestation's revoked flag) is a cache of the latest relevant event, never
the record — [`./revocation.md`](./revocation.md).

## Today in the repo

- Types: `crates/protocol/src/events.rs` as shown above.
- `crates/server/src/handlers.rs` writes two kinds. `register_finish`
  emits `identity.created` with `issuer = identity:<id>:self:created` — the
  identity's own Ed25519 event-signing key signs it, verified independently
  of the WebAuthn ceremony that authenticated the registration request — with
  a payload of `identity_id`, the initial `display_name`, and the handle
  `discriminator` (no `username` field exists anywhere; #73). `update_profile`
  emits `profile.updated` (#86, widened by #155) whenever `display_name`,
  `avatar_url`, `bio`, `favorite_genres`, or `pronouns` actually changes,
  with a payload of only the changed fields (plus the discriminator on a
  rename, since a rebuild has to reproduce the handle); a no-op request
  emits nothing. `avatar_url`/`bio`/`pronouns` each use `null` in the
  payload for an explicit clear vs. an absent key for untouched;
  `favorite_genres` has no separate clear state — a present key is always
  the field's new, complete value (as genre-vocabulary strings, e.g.
  `["rpg", "puzzle"]`), including `[]` to clear it. Network-attributed, not
  identity-signed — the same milestone-1 stand-in the social-graph events
  use. Both are enqueued into `protocol_outbox` (`crates/server/src/outbox.rs`)
  in the same transaction as the row they describe.
- A second emitter: `crates/server/src/friends.rs` writes `friend.requested`,
  `friend.accepted`, and `friend.removed`, each enqueued into
  `protocol_outbox` in the same transaction as the `friendships`/
  `friend_requests` row it accompanies (#15, closing #71's pattern for a
  second path). `issuer`/`subject` are both `identity:<id>:self:<verb>`
  `GlobalId`s naming the acting identity and the counterpart, respectively;
  unlike `identity.created` these are not yet individually signed — no
  general per-event Ed25519 signing ceremony exists yet, so this is the
  "network as signer" milestone-1 stand-in the catalogue above describes,
  attributed to the identity that authenticated the request rather than to
  the node. Declining or withdrawing a request emits no event — see
  [social-graph.md](./social-graph.md).
- A third emitter: `crates/server/src/devices.rs` (#135) writes
  `identity.signing_key_added` when a device grant is approved (signed by
  the approving device's Ed25519 key, verified the same way
  `register_finish` verifies `identity.created`'s signature) and
  `identity.signing_key_revoked` when a signing key is revoked
  (network-attributed, same milestone-1 stand-in `friend.requested` uses).
- A fourth emitter: `crates/server/src/games.rs` (#26) writes
  `game.registered` on `POST /games`, enqueued into `protocol_outbox` in the
  same transaction as the `games`/`game_requested_capabilities`/
  `issuer_keys` rows it accompanies. `issuer`/`subject` are both
  `game:<slug>:self:registered` `GlobalId`s (`game_ref`, mirroring
  `guilds.rs`'s `guild_ref`); network-attributed rather than signed by the
  game's own key even though the catalogue above lists that as the eventual
  signer — nothing has verified the registrant controls the submitted key
  yet at the point this event is built, so a real signature claim would be
  false. Payload is `game_id`, `slug`, `name`, `developer`,
  `requested_capabilities`, and the initial key's id/algorithm/public key.
- A fifth emitter: `crates/server/src/connections.rs` (#27/#83) writes
  `game.binding_established` (only on the first `POST /games/{slug}/connect`
  for a given identity/game pair — reconnecting to an already-active binding
  emits nothing), `game.binding_ended` (`DELETE /games/{slug}/connect`),
  and one `permission.granted`/`permission.revoked` per capability
  (`connect`, `DELETE /games/{slug}/grants/{capability}`, and every grant a
  `disconnect` revokes), all enqueued into `protocol_outbox` in the same
  transaction as the `bindings`/`permission_grants` row change they
  accompany. `issuer` is `identity:<id>:self:<verb>` (the acting identity);
  `subject` is `game:<slug>:self:<verb>` for binding events and
  `game:<slug>:self:<capability>` for grant events. Network-attributed
  rather than identity-signed, same "network as signer" milestone-1
  stand-in as `game.registered` and the social-graph events — no general
  per-event signing ceremony exists yet, so claiming the catalogue's
  eventual "identity key" signer here would be false.
- A sixth emitter: `crates/server/src/achievements.rs` (#31, generalized to
  App/Service by #324/#325) writes `achievement.defined`/
  `milestone.defined` on `POST /games/{slug}/achievements` /
  `POST /integrations/{slug}/milestones`, `.definition_updated` on the
  matching `PATCH` route when name/description/schema actually change, and
  `.definition_retired` when that same endpoint retires a definition — each
  enqueued into `protocol_outbox` in the same transaction as the
  `achievement_definitions` row it accompanies (one shared table for both
  claim vocabularies — see `achievements.rs`'s own module doc comment).
  Which of the two event-kind prefixes gets used is derived from the
  authenticated issuer's own registered category
  (`IntegratorCategory::claim_kind`), never caller-chosen: `Game` issuers
  get `achievement.*` exactly as #31 shipped, `App`/`Service` issuers get
  `milestone.*`. `issuer` is `<namespace>:<slug>:self:<verb>`
  (`games::issuer_ref`, generalizing `game_ref`); `subject` is the
  definition's own `<namespace>:<slug>:<claim_kind>:<key>` `GlobalId`,
  matching the catalogue's "issuer → claim id" shape above.
  Network-attributed rather than issuer-signed for the same reason
  `game.registered` is: no general per-event signing ceremony exists yet
  beyond `identity.created`.

  **The same module's issuance path (#32) is different: genuinely
  issuer-signed, not network-attributed.** `POST
  /games/{slug}/achievements/{key}/issue` /
  `POST /integrations/{slug}/milestones/{key}/issue` write
  `achievement.issued`/`milestone.issued` — the payload's `proof` field
  carries a real detached Ed25519 signature (verified server-side against
  the issuer's own key set, #84's `resolve_valid_signing_key`, before the
  event is ever built), the first event kind in this catalogue whose
  signer is genuinely the issuer's key and not a "network as signer"
  stand-in. `subject` is `identity:<id>:self:<claim_kind>_issued`
  (`games::issuer_ref`, reused with the `"identity"` namespace). No
  `revoked_at`/revocation path exists yet (#85).
- A seventh emitter: `crates/server/src/recovery.rs` (#201) writes
  `identity.recovery_configured` (guardian-set/threshold change),
  `identity.recovery_requested` (a new device completes the recovery
  ceremony), `identity.recovery_approved` (one per guardian approval, with
  the running approvals count/threshold/delay end in the payload),
  `identity.recovery_cancelled` (owner or guardian veto), and
  `identity.recovered` (finalize, once the delay elapses unvetoed) — each
  enqueued into `protocol_outbox` in the same transaction as the
  `recovery_guardians`/`recovery_requests`/`recovery_approvals`/
  `identity_keys` row change it accompanies. `issuer`/`subject` are
  `identity:<id>:self:<verb>` `GlobalId`s, same shape `friends.rs` and
  `devices.rs` use; `identity.recovery_approved`/`.recovery_cancelled`'s
  issuer is the acting guardian's or canceller's own identity, not
  necessarily the identity being recovered. Network-attributed rather than
  identity-signed for the same "network as signer" milestone-1 stand-in
  every emitter but `identity.created`/`identity.signing_key_added` uses —
  most acutely necessary here, since `identity.recovery_requested` is
  authored by a caller who by definition has no session, let alone a
  signing key, for the identity being recovered.
- An eighth emitter: `crates/server/src/guilds.rs` writes `guild.created`,
  `guild.updated`, `guild.role_defined`/`.role_deleted`, `guild.member_added`/
  `.member_removed`, `guild.role_changed`, `guild.owner_transferred`,
  `guild.game_associated`, and `guild.favorite_games_updated`, each enqueued
  into `protocol_outbox` in the same transaction as the `guilds`/
  `guild_members`/`guild_roles` row change it accompanies.
- A ninth emitter: `crates/server/src/channels.rs` writes
  `guild.channel_created`, `.channel_renamed`, and `.channel_archived`,
  same outbox pattern.
- A tenth emitter: `crates/server/src/game_schemas.rs` (#255) writes
  `game_schema.published`, consumed by
  `crates/indexer/src/projections/game_schemas.rs` for schema-version
  discovery (see [game-registry.md](./game-registry.md)).
- An eleventh emitter: `crates/server/src/games.rs` (#84, implementing
  #80's decided two-tier key model) writes `issuer.key_added` on `POST
  /games/{slug}/keys` and `issuer.key_revoked` on `POST
  /games/{slug}/keys/{key_id}/revoke`, each enqueued into `protocol_outbox`
  in the same transaction as the `issuer_keys` row it accompanies.
  `issuer`/`subject` are both `game:<slug>:self:key_added`/`:key_revoked`
  `GlobalId`s (`game_ref`), same network-attributed posture every emitter
  but `identity.created`/`identity.signing_key_added` uses — the catalogue
  above's "existing issuer key"/"issuer key" signer column describes the
  HTTP-level challenge-response authentication both endpoints require
  (`authenticate_game_root`, specifically a currently-valid **root** key
  per #80's decision — an operational key cannot author either event, even
  its own revocation), not a signature embedded in the event itself.
  `issuer.key_expired` and the `issuer.suspended`/`.reinstated`/`.revoked`/
  `.deprecated` family the catalogue also lists remain unimplemented — the
  former has no expiry-sweep mechanism built, and the latter's
  network-level authorization model is #84's own explicit deferred scope.
- The ledger row shape is `crates/server/db/migrations/0002_ledger/up.sql`
  plus `0014_ledger_batches/up.sql` (issue #38 — adds `batch_id`),
  `0024_signed_tree_heads/up.sql` (#210 — Merkle root + Signed Tree Heads),
  and `0027_ledger_payload_retention/up.sql` (retention tiering); the
  content hash covers `event_id`, `kind`, `issuer`, `subject`, `payload`,
  `timestamp`, `version` (`crates/chain/src/postgres.rs`). Every event lands
  in `protocol_outbox` (#71) and is committed as part of whatever
  `EventBatch` the settlement worker's current drain tick assembles
  (`crates/server/src/outbox.rs`) — batches close on a worker tick, not on
  size or a timer; a single-event batch is legal.
- No typed kinds, no payload structs, no catalogue in code.

## Decisions and tickets

- #75 durable history is canonical
- #82 event kind catalogue and versioning policy
- [#38](https://github.com/LunarVagabond/avalon-protocol/issues/38) batching
  (buffer → `EventBatch` → `Commitment`, real as of `batch_id`/
  `ledger_batches`)
- [#71](https://github.com/LunarVagabond/avalon-protocol/issues/71) events must
  commit atomically with the projection change
- #86 profile events; #73 identity signs its own events
- [#81](https://github.com/LunarVagabond/avalon-protocol/issues/81) revocation
  entry shape (decided; implementation #85)
- [#201](https://github.com/LunarVagabond/avalon-protocol/issues/201) —
  guardian-based recovery event kinds (`identity.recovery_*`,
  `identity.recovered`), described above.
- [#210](https://github.com/LunarVagabond/avalon-protocol/issues/210) —
  Merkle root / Signed Tree Head implementation (decided by #40).
