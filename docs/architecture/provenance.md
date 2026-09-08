# Provenance

**Avalon records provenance; it does not determine universal meaning.**
Interoperability is only useful if a receiving game can answer *who* said
something, *when*, *under which key*, and *whether it still stands* — without
asking the original game, which may no longer exist. Provenance is a first-class
property of every durable claim, and it survives the death of the game that
produced it.

## The questions provenance answers

| Question | Answered by |
|---|---|
| who issued it? | issuer identity + signing key ([`./games-and-issuers.md`](./games-and-issuers.md)) |
| where did it originate? | source game / issuer, namespaced id |
| when was it issued? | `issued_at`, plus its position in the log ([`./settlement.md`](./settlement.md)) |
| who currently holds/owns it? | subject, or ownership history for assets |
| has it been revoked or superseded? | later entries referencing it ([`./revocation.md`](./revocation.md)) |
| is it still valid? | point-in-time validity over history ([`./trust-model.md`](./trust-model.md)) |
| does a consuming game recognize it? | that game's policy — not provenance, but depends on it |

The first six are universal facts. The seventh is contextual and is
deliberately kept out of the record.

## Examples

An achievement:

```text
Achievement
    id:            game:ashen-realms:achievement:dragon_slayer
    issuer:        game:ashen-realms   (key k1, valid at issuance)
    source:        Ashen Realms
    subject:       identity:…X
    issued_at:     2027-03-14T21:07:00Z
    evidence:      opaque reference chosen by the issuer
    status:        ACTIVE   (derived from history)
```

An asset (later phase, see [`./future-layers.md`](./future-layers.md)):

```text
Asset
    id:                  game:ashen-realms:asset:sword-of-aether:0421
    source game:         Ashen Realms
    original issuer:     game:ashen-realms
    originally issued to: identity:…Y
    transfers:           Y → Z, Z → X
    current owner:       identity:…X
```

## Provenance survives game death

If Ashen Realms shuts down, its issuer identity, its key history, its
achievement definitions, and every attestation it issued stay in durable
history with their signatures. A consuming game can still verify that the
claim was authentic and valid as of any point in time. What disappears is the
game-side character state — see the survival table in
[`./overview.md`](./overview.md).

## Why the display name isn't enough

`Dragon Slayer` from three different games is three different claims. Only the
namespaced id plus the issuer record distinguish them. Any UI that shows an
achievement without its issuer is misrepresenting provenance; the Hub always
shows the issuer next to the title ([`./hub.md`](./hub.md)).

## Provenance and the registry

Aggregate facts derived from provenance — how many attestations an issuer has
produced, which games publicly recognize it, how many of its players also play
elsewhere — are published as labelled statistics in the
[game registry](./game-registry.md). They inform a consumer's decision and
never replace it.

## Today in the repo

- `crates/protocol/src/achievements.rs` — issuer, subject, `issued_at`, and
  opaque `proof` on `AchievementAttestation`; no key reference yet, so
  "which key signed this" cannot be answered.
- `crates/protocol/src/ids.rs` — namespaced `GlobalId`.
- `crates/chain/src/postgres.rs` — every committed event gets a position
  (`seq`) and a hash chained to its predecessor, which is the log-side half of
  "when".
- No ownership or asset types exist; that is a later phase by design.

## Decisions and tickets

- [#75](https://github.com/LunarVagabond/avalon-protocol/issues/75) — ADR:
  durable history is canonical; provenance is what that history preserves.
- [#76](https://github.com/LunarVagabond/avalon-protocol/issues/76) — ADR:
  attestation trust model (provenance ≠ meaning).
- [#84](https://github.com/LunarVagabond/avalon-protocol/issues/84) — issuer
  identity and key history (the "which key" half).
- [#85](https://github.com/LunarVagabond/avalon-protocol/issues/85) —
  revocation as history.
- [#89](https://github.com/LunarVagabond/avalon-protocol/issues/89) — registry
  read model derived from provenance.
