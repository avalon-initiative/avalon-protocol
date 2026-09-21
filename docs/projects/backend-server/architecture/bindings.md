# Integrator Profiles and Bindings

**A binding says "this Avalon identity participates in this integrator" — nothing
more.** Everything an integrator knows about the identity beyond that (characters, race,
class, level, appearance, progression, inventory) belongs to the integrator and lives
in the integrator's own database. **Integrator authority is scoped to the integrator's own
binding**: Integrator A can create and manage its profile of an identity and issue its
own attestations; it cannot touch Integrator B's.

Narrative: [`../stakeholders/Proposal.md` §19](../../../stakeholders/Proposal.md#19-identity-vs-game-data).

## The tree

```text
Avalon Identity X
    │
    ├── Binding: Ashen Realms
    │       └── (integrator-side) Character: Avion Ranger, level 72
    │
    └── Binding: Ocean World
            └── (integrator-side) Character: Sea Lion Navigator, level 14
```

Avalon holds the two bindings. It does not know what an Avion or a Sea Lion is,
and it never will unless an integrator explicitly promotes a fact into durable history
as an attestation, **or explicitly publishes it as instance data against a
schema it published itself** (Integrator Space's data-exposure mechanism, #255/#384,
decided #381) — subject to that schema's own declared visibility. Absent one
of those two explicit acts, the character rows stay keyed on the integrator's side,
referencing the binding or the identity id; Avalon stores none of their
attributes. Publishing instance data is deliberately narrow in spirit even
though it's technically unrestricted in shape: small, portable, fun-to-carry
flavor data (a character's name, level, race, class, titles), never a
character's full mechanical state (inventory, skills, stats used for integrator
balance) — see [`./identity-aggregate-view.md`](./identity-aggregate-view.md)
for a full worked example and the visibility rules.

## What a binding is

| Field | Meaning |
|---|---|
| `identity_id` | the identity |
| `integrator_id` | the integrator |
| `established_at` | when the identity consented |
| `ended_at` | when the identity (or the integrator) ended it, if ever |

- A binding is established by the **identity**, through the consent flow, when
  they first connect their identity to an integrator. An integrator cannot create one
  unilaterally.
- Capability grants hang off the binding. No active binding, no grants; ending
  the binding ends every grant under it.
- Ending a binding does not delete history. Attestations the integrator issued while
  the binding was active remain in history with their provenance.
- Bindings are durable protocol events (`game.binding_established`,
  `game.binding_ended`), so "which integrators has this identity participated in" is
  reconstructable and so the registry can count players honestly.

## Scoped authority

```text
Integrator A
    can:
        establish an Integrator A profile for a consenting identity
        manage its own integrator-side character data
        issue Integrator A attestations about that identity
        read whatever Integrator B has explicitly published (attestations,
            or instance data Integrator B opted into being visible, #381)

    cannot:
        read or write Integrator B's own internal, unpublished profile of the
            same identity
        write to Integrator B's published data under any circumstance
        issue attestations under Integrator B's issuer identity
        alter the identity itself, its friends, or its guild history
```

This is enforced by the permission model (every grant is scoped to an integrator and a
capability) and by issuer keys (every attestation is signed by the issuing
integrator's own key). See [`./security-model.md`](./security-model.md) and
[`./issuers.md`](./issuers.md).

## Bindings and the registry

"Players of Integrator A" means distinct identities holding an active binding to Integrator
A, observed through protocol activity — not a number the integrator reports about
itself. See [`./registry.md`](./registry.md).

## Scenario A — an identity enters a second integrator

Identity X has a binding to Integrator A. They connect to Integrator B. Integrator B sees an
identity with a display name, whatever capabilities X granted, and X's
authentic attestations from Integrator A (which Integrator B may or may not recognize —
[`./trust-model.md`](./trust-model.md)). Integrator B creates its own character for X
in its own database. It never sees Integrator A's character, and Integrator A never learns
about Integrator B's unless X's permissions expose it.

## Today in the repo

- `crates/protocol/src/integrators.rs` — `IntegratorBinding { identity_id, integrator_id,
  established_at, ended_at }`, real now (#83), alongside `Integrator`,
  `IntegratorRegistration`, `IntegratorCredential`. No game-specific field exists on it,
  by design.
- `crates/server/src/connections.rs` (#27/#83) — `POST /integrations/{slug}/connect`
  is the consent flow: it validates every approved capability against what
  the integrator declared at registration (`GET /integrations/{slug}` /
  `integrator_requested_capabilities`, rejecting anything undeclared), creates the
  `bindings` row only if the caller has no active binding to that integrator yet
  (idempotent — reconnecting grants any newly-approved capabilities without
  duplicating the binding or re-firing `game.binding_established`), and
  inserts one `permission_grants` row per approved capability, all in one
  transaction via the outbox. `DELETE /integrations/{slug}/grants/{capability}`
  revokes a single grant; `DELETE /integrations/{slug}/connect` ends the binding
  and revokes every active grant under it in the same transaction; `GET
  /me/connections` lists the caller's active bindings with their active
  grants. `bindings`/`permission_grants` (migration `0012_game_bindings`)
  are projections — `game.binding_established`, `game.binding_ended`,
  `permission.granted`, `permission.revoked` are the durable history, and
  are network-attributed for now (the same milestone-1 stand-in
  `game.registered` uses), not yet identity-signed despite what the
  event-kind catalogue eventually intends.
- `crates/sdk/src/lib.rs` — `authenticate()` now calls `GET /me/grants`
  (identified by `AvalonConfig::integrator_credential_key_id`) and populates
  `Session.granted` from the caller's real active grants for that integrator,
  rather than always returning an empty list.

## Decisions and tickets

- [#83](https://github.com/LunarVagabond/avalon-protocol/issues/83) — integrator
  profile/binding type, events, endpoints, and the SDK check.
- [#67](https://github.com/LunarVagabond/avalon-protocol/issues/67) — ADR:
  identity is separate from game characters.
- [#27](https://github.com/LunarVagabond/avalon-protocol/issues/27) —
  capability grant/revoke consent flow (produces the binding).
- [#28](https://github.com/LunarVagabond/avalon-protocol/issues/28) —
  permission enforcement middleware.
- [#25](https://github.com/LunarVagabond/avalon-protocol/issues/25) — Epic:
  Integrator Registration & Permissions.
