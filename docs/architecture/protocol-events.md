# Protocol Events

A protocol event is a durable fact Avalon considers part of its history. **Not
every integrator action is a protocol event; ordinary gameplay never becomes one.**
See [`./worked-ledger-example.md`](./worked-ledger-example.md) for one
hypothetical user's ledger rendered as real, ordered JSON instances of the
catalogue below, alongside what never appears on it.
**Events are the canonical record — every table is a projection of them**
([#75](https://github.com/LunarVagabond/avalon-protocol/issues/75)), and
**history is append-only: a correction is a new event, never an edit.**

## Hot gameplay vs durable events

| Stays integrator-side (never an event) | May enter durable history |
|---|---|
| movement, combat, physics, AI | achievement issued / revoked |
| HP, XP ticks, NPC state, player position | integrator event result |
| matchmaking, ordinary chat | guild created, membership / role changed |
| game-specific inventory and economy | integrator registered, binding established |
| typing indicators, connection state, presence | issuer registered, key rotated, suspended |
| | attestation issued / superseded, ownership transferred |

The test is the one in [`./overview.md`](./overview.md): would this fact matter
outside the integrator that produced it, and does Avalon promise to preserve it? If
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

`kind` stays a plain `String` on the wire/storage type itself — the ledger
schema and every existing consumer keep working byte-for-byte — but domain
code no longer hand-types that string: `ProtocolEventKind`
(`crates/protocol/src/events.rs`, issue #82) is an enum with a permanent
wire-string mapping and an `Other(String)` escape hatch, the same template
`Capability` established, so a new kind still never requires a protocol
version bump (`Other` covers it), while every *known* kind gets real
compile-time safety at every call site that builds or matches one. Payload
construction goes through a typed struct per kind
(`crates/protocol/src/event_payloads.rs`) serialized via
`serde_json::to_value`, never an ad-hoc `serde_json::json!({...})`.
`Commitment.proof` is deliberately opaque: `protocol` does not know
whether it is a ledger hash, a signed tree head, or a Merkle root anchored
elsewhere.

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

## Kind catalogue (normative)

The full row-by-row list of every `ProtocolEvent` kind — issuer/subject,
payload, what it drives, and who signs it — lives in its own file:
[`./protocol-events-catalogue.md`](./protocol-events-catalogue.md).
Normative as of [#82](https://github.com/LunarVagabond/avalon-protocol/issues/82):
every "(done)" row has a real `ProtocolEventKindVariant` and typed payload
struct backing it, kept honest by the compiler rather than by convention
alone.

Two conventions worth knowing before you open that table: `issuer` and
`subject` are `GlobalId`s (`crates/protocol/src/ids.rs`), namespaced so two
integrators' `dragon_slayer` never collide (see
[`./provenance.md`](./provenance.md)); and each kind has exactly one payload
schema per version.

## Versioning policy

History may outlive every current maintainer. Therefore:

- Adding an optional field does not change `version`.
- Removing, renaming, or changing the meaning of a field bumps `version`.
- Every version ever emitted stays decodable forever. Decoders are added, never
  deleted.
- An unknown kind is preserved and skipped by an indexer that does not
  understand it; it is never dropped from the log.
- Attestations additionally carry an issuer-declared schema reference so a
  consumer can recognize "integrator event result, schema v1" independently of the
  issuer's naming or which kind of event (tournament, seasonal championship,
  community campaign, ...) produced it.

"We can change the schema later" is true of a projection table and false of the
log.

## History vs current state

Both are kept, and kept distinct:

```text
Guild membership history (events)        Current projection (indexer)
2027-01-01  X joins Guild A              User X
2027-04-14  X becomes Officer                Guild: none
2028-02-10  X leaves Guild A
```

History is reconstructable; current state is optimized for reads and can always
be thrown away and rebuilt. A "current status" column on a projection row (e.g.
an attestation's revoked flag) is a cache of the latest relevant event, never
the record — [`./revocation.md`](./revocation.md).

## Today in the repo

**Issue #82 (kind catalogue + typed payloads + versioning policy): done.**
`ProtocolEventKind`/`ProtocolEventKindVariant` (`crates/protocol/src/events.rs`)
and one payload struct per kind (`crates/protocol/src/event_payloads.rs`)
back every kind the codebase actually emits — every real emitter across
`crates/server/src` builds its `kind` and `payload` through these types,
not a hand-typed string or an ad-hoc `serde_json::json!({...})`. Round-trip
and fixture-decoding tests exist for every payload struct
(`crates/protocol/src/event_payloads.rs`'s own test module); one real bug
was caught by them before it ever reached live infra (an `Option<Option<T>>`
field's default serde `Deserialize` collapsed "absent" and "explicit
`null`" into the same value on the read side — fixed with the standard
`deserialize_with` workaround). `docs/architecture/protocol-events-catalogue.md`
is normative now, not proposed — kept honest by the compiler, not just
convention.

Types live in `crates/protocol/src/events.rs`, as shown above. There's no
single dispatcher — each domain module emits its own kinds directly into
`protocol_outbox` (`crates/server/src/outbox.rs`), in the same transaction as
the row change it accompanies. Two signing postures recur throughout: a real
per-event signature (rare so far — see `identity.created` and issuance
below), or "network as signer," today's milestone-1 stand-in where the node
attributes the event to whichever identity authenticated the request instead
of embedding a signature. Almost every emitter below still uses the latter;
where an emitter is genuinely signed, it's called out explicitly.

- **`crates/server/src/handlers.rs`** — identity and profile.
  - `register_finish` emits `identity.created`, signed by the identity's own
    Ed25519 event-signing key (verified independently of the WebAuthn
    ceremony that authenticated registration). Payload: `identity_id`,
    initial `display_name`, and the handle `discriminator` — no `username`
    field exists anywhere (#73).
  - `update_profile` emits `profile.updated` (#86, widened by #155) only
    when `display_name`, `avatar_url`, `bio`, `favorite_genres`, or
    `pronouns` actually changes; a no-op request emits nothing. Payload
    carries only the changed fields (plus `discriminator` on a rename, so a
    rebuild can reproduce the handle). `avatar_url`/`bio`/`pronouns` use
    `null` for an explicit clear vs. an absent key for untouched;
    `favorite_genres` has no separate clear state — a present key is always
    the field's complete new value, including `[]` to clear it.
    Network-attributed, not identity-signed.
- **`crates/server/src/friends.rs`** (#15) — `friend.requested`,
  `friend.accepted`, `friend.removed`. `issuer`/`subject` are both
  `identity:<id>:self:<verb>` `GlobalId`s naming the acting identity and the
  counterpart. Network-attributed — no general per-event signing ceremony
  exists yet. Declining or withdrawing a request emits no event — see
  [social-graph.md](./social-graph.md).
- **`crates/server/src/devices.rs`** (#135) — `identity.signing_key_added`
  when a device grant is approved (signed by the approving device's own
  Ed25519 key, verified the same way as `identity.created`) and
  `identity.signing_key_revoked` when a key is revoked
  (network-attributed).
- **`crates/server/src/integrators.rs`** (#26) — `game.registered` on
  `POST /integrations`. `issuer`/`subject` are both `game:<slug>:self:registered`
  (`integrator_ref`). Network-attributed rather than integrator-key-signed — nothing has
  verified the registrant controls the submitted key yet at the point this
  event is built, so a real signature claim would be false. Payload:
  `integrator_id`, `slug`, `name`, `developer`, `requested_capabilities`, and the
  initial key's id/algorithm/public key.
- **`crates/server/src/connections.rs`** (#27/#83) — `game.binding_established`
  (only on the first `POST /integrations/{slug}/connect` for a given identity/integrator
  pair; reconnecting emits nothing), `game.binding_ended`
  (`DELETE /integrations/{slug}/connect`), and one `permission.granted`/
  `permission.revoked` per capability. `issuer` is the acting identity;
  `subject` is `game:<slug>:self:<verb>` for binding events and
  `game:<slug>:self:<capability>` for grant events. Network-attributed.
- **`crates/server/src/achievements.rs`** (#31, generalized to App/Service by
  #324/#325) — definitions and issuance, two different signing postures:
  - *Definitions* (`achievement.defined`/`milestone.defined` on creation,
    `.definition_updated`, `.definition_retired`) share one table
    (`achievement_definitions`) across both claim vocabularies. Which
    prefix is used comes from the authenticated issuer's own registered
    category (`IntegratorCategory::claim_kind`), never caller-chosen: `Game`
    issuers get `achievement.*`, `App`/`Service` issuers get `milestone.*`.
    Network-attributed, same reason as `game.registered`.
  - **Issuance (#32) is genuinely issuer-signed, not network-attributed** —
    the first event kind in this catalogue where that's true.
    `POST /integrations/{slug}/achievements/{key}/issue` and its milestone
    equivalent write `achievement.issued`/`milestone.issued` with a real
    detached Ed25519 signature in the payload's `proof` field, verified
    server-side against the issuer's own key set (#84's
    `resolve_valid_signing_key`) before the event is built.
  - **Revocation (#85) follows the same signed posture.**
    `POST /attestations/{id}/revoke` writes `achievement.revoked`/
    `milestone.revoked` with its own embedded signature, requires the
    caller to authenticate as the attestation's original issuer, and
    appends to a dedicated `attestation_revocations` table rather than
    mutating `achievement_attestations`. Supersession
    (`attestation.superseded`) and attestation-level reinstatement (no
    event kind yet) remain unbuilt — `attestation_revocations` is capped at
    one row per attestation for exactly that reason.
- **`crates/server/src/recovery.rs`** (#201) — `identity.recovery_configured`
  (guardian-set/threshold change), `identity.recovery_requested` (a new
  device completes the recovery ceremony), `identity.recovery_approved`
  (one per guardian approval), `identity.recovery_cancelled` (owner or
  guardian veto), and `identity.recovered` (finalize, once the delay elapses
  unvetoed). Network-attributed — most acutely necessary here, since
  `identity.recovery_requested` is authored by a caller who by definition
  has no session, let alone a signing key, for the identity being recovered.
- **`crates/server/src/guilds.rs`** — `guild.created`, `guild.updated`,
  `guild.role_defined`/`.role_deleted`, `guild.member_added`/
  `.member_removed`, `guild.role_changed`, `guild.owner_transferred`,
  `guild.game_associated`, `guild.favorite_games_updated`.
- **`crates/server/src/channels.rs`** — `guild.channel_created`,
  `.channel_renamed`, `.channel_archived`.
- **`crates/server/src/integrator_schemas.rs`** (#255) — `game_schema.published`,
  consumed by `crates/indexer/src/projections/integrator_schemas.rs` for
  schema-version discovery (see [registry.md](./registry.md)).
- **`crates/server/src/integrators.rs`** (#84, implementing #80's two-tier key
  model) — `issuer.key_added` (`POST /integrations/{slug}/keys`) and
  `issuer.key_revoked` (`POST /integrations/{slug}/keys/{key_id}/revoke`). Both
  require a currently-valid **root** key (`authenticate_integrator_root`) — an
  operational key can't author either event, even its own revocation.
  `issuer.key_expired` and the `issuer.suspended`/`.reinstated`/`.revoked`/
  `.deprecated` family remain unimplemented: no expiry-sweep mechanism
  exists, and the latter's network-level authorization model is #84's own
  explicit deferred scope.

The ledger row itself: shape defined across
`crates/server/db/migrations/0002_ledger/up.sql`,
`0014_ledger_batches/up.sql` (#38, adds `batch_id`),
`0024_signed_tree_heads/up.sql` (#210, Merkle root + Signed Tree Heads), and
`0027_ledger_payload_retention/up.sql` (retention tiering). The content hash
covers `event_id`, `kind`, `issuer`, `subject`, `payload`, `timestamp`,
`version` (`crates/chain/src/postgres.rs`). Every event lands in
`protocol_outbox` and is committed as part of whatever `EventBatch` the
settlement worker's current drain tick assembles — batches close on a worker
tick, not on size or a timer, so a single-event batch is legal.

## Decisions and tickets

- #75 durable history is canonical
- #82 event kind catalogue and versioning policy — done, see "Today in the
  repo" above
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
