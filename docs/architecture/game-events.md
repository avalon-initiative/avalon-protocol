# Game Events

**A game event is an attestation for a durable, cross-game-relevant result a
game produces** — a tournament, a seasonal championship, a world-first race, a
community campaign, whatever draws users to it and is worth remembering
after the fact. **A game runs the event, decides the outcome, and signs a
claim; Avalon carries that durable fact; another game verifies it and decides
on its own what, if anything, to do about it.** The event's gameplay —
brackets, live scores, matchmaking, spectator state — never enters the
protocol; only the result crosses the boundary. This is one of the clearest
demonstrations of what the network is for, and it needs no new primitive.

The underlying mechanism (a durable, cross-integrator result as an
attestation) is domain-agnostic; it is illustrated here with gaming examples
because gaming is Avalon's first live use case.

## The flow

A tournament is the easiest example to walk through, but the same shape
carries any game event:

```text
The Great Avalon Championship (a tournament — one kind of game event)

Game A (Ashen Realms)
    |
    +-- runs the event, entirely game-side
    |
    +-- determines the outcome
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
                event hall access, special NPC dialogue, or nothing
```

The attestation:

```text
Issuer:        game:ashen-realms
Game Event:    game:ashen-realms:game_event:avalon-championship-2027
Subject:       Avalon Identity X
Achievement:   game:ashen-realms:game_event:avalon-championship-2027:winner
Issued at:     2027-08-14T20:11:03Z
Schema:        game-event-result/v1
Signature:     ...
```

Every check Game B performs is the standard one from the
[trust model](./trust-model.md): the signature proves Ashen Realms issued it; the
history proves it is not revoked and the issuer was in good standing; Game B's
own recognition policy decides whether this particular event from Ashen Realms
unlocks anything in its world.

## Not a new primitive

Game event results reuse [achievements and attestations](./achievements-and-attestations.md)
with a namespaced id and a schema reference:

```text
game:<slug>:game_event:<event-id>:winner
game:<slug>:game_event:<event-id>:finalist
game:<slug>:game_event:<event-id>:participant
```

The event itself gets an identity (a `GlobalId` of kind `game_event`) so that
participation, placement, and victory attestations from one occurrence can be
grouped, and so a consumer can scope a recognition policy to that specific
event rather than to every achievement the issuer ever signs. A schema
reference on the attestation ("this is a game event result, version 1") lets a
consumer recognize the shape independently of the issuer's naming or of which
kind of event it was.

There is no separate settlement path: `game_event.result_issued` is
`achievement.issued` with the game-event schema, batched and committed like any
other [protocol event](./protocol-events.md).

## What counts as a game event

Anything a game runs that produces a durable, interoperable outcome worth
recording outside the game itself:

- tournaments and championships (the running example above)
- seasonal events and world-first races
- guild competitions (a guild-level subject rather than an identity)
- community events and world-wide campaigns
- cross-game quests with a recorded completion
- developer-sponsored events
- esports results
- collaborative achievements

What does not cross the boundary: matchmaking, brackets, live scores, spectator
state, in-progress rounds. Those are gameplay and stay game-side, whatever kind
of event produced them. Do not assume all gameplay needs to become protocol
data — only the final, durable result does.

## Recognition is still contextual

Three games can each run a "championship" and issue "winner". Provenance makes
them distinct; nothing makes them equal. Game B may trust Ashen Realms' event
results and ignore Game C's, and the Hub shows both with the issuer named. The
[game registry](./game-registry.md) may report game-event activity as a
derived metric; it never ranks events or the games that ran them.

## Scenario G — cross-game game event

Can Game A issue an event result that Game B can verify? Yes: Game A signs it
under a registered [issuer key](./games-and-issuers.md), Avalon records and
indexes it, Game B checks authenticity and validity through the SDK and
applies its own policy. If Game A later revokes the result (a disputed match,
a scoring correction), the history shows both the issuance and the
[revocation](./revocation.md); Game B re-evaluates.

## Today in the repo

- Nothing game-event-specific exists. `crates/protocol/src/achievements.rs` has
  `AchievementAttestation` and `crates/protocol/src/ids.rs` has `GlobalId`, both
  of which the design reuses.
- The schema reference field (#88) lives on `AchievementDefinition.schema`
  (`Option<GlobalId>`), not on `AchievementAttestation` itself — an
  attestation only references its achievement, which carries the schema.
- Filed as on-hold: not a milestone-1 item, tracked now so the attestation and
  event designs don't preclude it.

## Decisions and tickets

- [#88](https://github.com/LunarVagabond/avalon-protocol/issues/88) — game
  event result attestations (on-hold).
- [#76](https://github.com/LunarVagabond/avalon-protocol/issues/76) — ADR:
  attestation trust model.
- [#82](https://github.com/LunarVagabond/avalon-protocol/issues/82) — event kind
  catalogue, where the game-event schema is registered.
- [#30](https://github.com/LunarVagabond/avalon-protocol/issues/30) — Epic:
  Achievements & Attestations.
