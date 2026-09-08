# Tournaments and Cross-Game Events

**A tournament result is an attestation.** Game A runs the tournament, decides
the winner, and signs a claim; Avalon carries that durable fact; Game B verifies
it and decides on its own what, if anything, to do about it. **The tournament's
gameplay, brackets, and scoring never enter the protocol** — only the result
crosses the boundary. This is the clearest demonstration of what the network is
for, and it needs no new primitive.

## The flow

```text
The Great Avalon Championship

Game A (Ashen Realms)
    |
    +-- runs the tournament, entirely game-side
    |
    +-- determines the winner
    |
    +-- issues a signed attestation
             |
             v
       Avalon Protocol (recorded, indexed, verifiable)
             |
             v
Game B
    verifies:   authentic? valid?
    decides:    recognized? -> legendary title, cosmetic, champion class,
                tournament hall access, special NPC dialogue, or nothing
```

The attestation:

```text
Issuer:        game:ashen-realms
Tournament:    game:ashen-realms:tournament:avalon-championship-2027
Subject:       Avalon Identity X
Achievement:   game:ashen-realms:tournament:avalon-championship-2027:winner
Issued at:     2027-08-14T20:11:03Z
Schema:        tournament-result/v1
Signature:     ...
```

Every check Game B performs is the standard one from the
[trust model](./trust-model.md): the signature proves Ashen Realms issued it; the
history proves it is not revoked and the issuer was in good standing; Game B's
own recognition policy decides whether a championship from Ashen Realms unlocks
anything in its world.

## Not a new primitive

Tournament results reuse [achievements and attestations](./achievements-and-attestations.md)
with a namespaced id and a schema reference:

```text
game:<slug>:tournament:<tournament-id>:winner
game:<slug>:tournament:<tournament-id>:finalist
game:<slug>:tournament:<tournament-id>:participant
```

The tournament itself gets an identity (a `GlobalId` of kind `tournament`) so
that participation, placement, and victory attestations from one event can be
grouped, and so a consumer can scope a recognition policy to a tournament rather
than to every achievement the issuer ever signs. A schema reference on the
attestation ("this is a tournament result, version 1") lets a consumer recognize
the shape independently of the issuer's naming.

There is no separate settlement path: `tournament.result_issued` is
`achievement.issued` with the tournament schema, batched and committed like any
other [protocol event](./protocol-events.md).

## Other cross-game events

The same shape carries any durable, interoperable outcome:

- seasonal championships
- guild competitions (a guild-level subject rather than a player)
- community events and world-wide campaigns
- cross-game quests with a recorded completion
- developer-sponsored events
- esports results
- collaborative achievements

What does not cross the boundary: matchmaking, brackets, live scores, spectator
state, in-progress rounds. Those are gameplay and stay game-side. Do not assume
all gameplay needs to become protocol data.

## Recognition is still contextual

Three games can each run a "championship" and issue "winner". Provenance makes
them distinct; nothing makes them equal. Game B may trust Ashen Realms'
tournament results and ignore Game C's, and the Hub shows both with the issuer
named. The [game registry](./game-registry.md) may report tournament activity as
a derived metric; it never ranks tournaments.

## Scenario G — cross-game tournament

Can Game A issue a championship result that Game B can verify? Yes: Game A signs
it under a registered [issuer key](./games-and-issuers.md), Avalon records and
indexes it, Game B checks authenticity and validity through the SDK and applies
its own policy. If Game A later revokes the result (a disputed match), the
history shows both the issuance and the [revocation](./revocation.md); Game B
re-evaluates.

## Today in the repo

- Nothing tournament-specific exists. `crates/protocol/src/achievements.rs` has
  `AchievementAttestation` and `crates/protocol/src/ids.rs` has `GlobalId`, both
  of which the design reuses.
- No schema reference field exists on attestations yet.
- Filed as on-hold: not a milestone-1 item, tracked now so the attestation and
  event designs don't preclude it.

## Decisions and tickets

- [#88](https://github.com/LunarVagabond/avalon-protocol/issues/88) — tournament
  and cross-game event result attestations (on-hold).
- [#76](https://github.com/LunarVagabond/avalon-protocol/issues/76) — ADR:
  attestation trust model.
- [#82](https://github.com/LunarVagabond/avalon-protocol/issues/82) — event kind
  catalogue, where the tournament schema is registered.
- [#30](https://github.com/LunarVagabond/avalon-protocol/issues/30) — Epic:
  Achievements & Attestations.
