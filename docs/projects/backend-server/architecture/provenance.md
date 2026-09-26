# Provenance

**Avalon records provenance; it does not determine universal meaning.**
Interoperability is only useful if a receiving integrator can answer *who* said
something, *when*, *under which key*, and *whether it still stands* — without
asking the original integrator, which may no longer exist. Provenance is a first-class
property of every durable claim, and it survives the death of the integrator that
produced it.

## The questions provenance answers

| Question | Answered by |
|---|---|
| who issued it? | issuer identity + signing key ([`./issuers.md`](./issuers.md)) |
| where did it originate? | source integrator / issuer, namespaced id |
| when was it issued? | `issued_at`, plus its position in the log ([`./settlement.md`](./settlement.md)) |
| who currently holds/owns it? | subject, or ownership history for assets |
| has it been revoked or superseded? | later entries referencing it ([`./revocation.md`](./revocation.md)) |
| is it still valid? | point-in-time validity over history ([`./trust-model.md`](./trust-model.md)) |
| does a consuming integrator recognize it? | that integrator's policy — not provenance, but depends on it |

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
    source integrator:         Ashen Realms
    original issuer:     game:ashen-realms
    originally issued to: identity:…Y
    transfers:           Y → Z, Z → X
    current owner:       identity:…X
```

## Provenance survives integrator death

If Ashen Realms shuts down, its issuer identity, its key history, its
achievement definitions, and every attestation it issued stay in durable
history with their signatures. A consuming integrator can still verify that the
claim was authentic and valid as of any point in time. What disappears is the
integrator-side character state — see the survival table in
[`./overview.md`](./overview.md).

## Why the display name isn't enough

`Dragon Slayer` from three different integrators is three different claims. Only the
namespaced id plus the issuer record distinguish them. Any UI that shows an
achievement without its issuer is misrepresenting provenance; the Hub always
shows the issuer next to the title ([`./hub.md`](https://github.com/avalon-initiative/avalon-hub/blob/main/docs/hub/architecture/hub.md)).

## Provenance and the registry

Aggregate facts derived from provenance — how many attestations an issuer has
produced, which integrators publicly recognize it, how many of its players also play
elsewhere — are published as labelled statistics in the
[integrator registry](./registry.md). They inform a consumer's decision and
never replace it.

## Current implementation

`crates/protocol/src/achievements.rs`'s `AchievementAttestation` carries issuer,
subject, `issued_at`, and an opaque `proof`. `crates/protocol/src/ids.rs` defines the
namespaced `GlobalId` (`<namespace>:<owner>:<kind>:<key>`) every provenance-bearing
record uses to unambiguously identify who issued and what it concerns.
`crates/chain/src/postgres.rs` gives every committed event a position (`seq`) and a
hash chained to its predecessor, which is the log-side half of "when" and "where in
history."

Durable history is canonical — provenance is exactly what that history preserves, and
authenticity, validity, and recognition remain three separate questions: a signature
proves who signed a claim, a point-in-time check over history proves whether it still
stands, and recognition is always the receiving integrator's own policy decision, never
something Avalon's records assert on an integrator's behalf.

No ownership or asset types exist yet; portable assets and ownership provenance are a
later phase, not a foundational one — see [`./future-layers.md`](./future-layers.md).
There is currently no key reference on `AchievementAttestation`, so "which key signed
this specific claim" cannot yet be answered from the attestation alone; issuer key
history is tracked separately (see [`./issuers.md`](./issuers.md)).
