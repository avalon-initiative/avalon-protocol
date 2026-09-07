# ADR 0001: Identity Is Separate From Game Characters

**Status:** Decided

## Context

A player interacts with Avalon through one persistent identity, but plays many
independent games, each with its own character model (levels, stats, inventory,
appearance, progression). Without an explicit boundary, it becomes tempting to let
"identity" absorb game-specific concerns, or to design one universal character
schema that every game must conform to.

## Decision

Avalon identity and game characters are separate concepts, related by ownership,
never merged:

```text
Network Identity
    ├── Profile
    ├── Friends
    ├── Guilds
    ├── Achievements
    ├── Reputation
    ├── Attestations
    └── Permissions
         ├── Game Character A
         ├── Game Character B
         └── Game Character C
```

Avalon never dictates a character's level, class, HP, inventory, stats, quests,
combat progression, appearance, or skill trees. A player can have entirely
different, unrelated characters in different games under the same identity.

## Consequences

- The `protocol` crate models `Identity` and `Profile` with no reference to any
  game's character schema.
- Games remain free to design characters however they want; Avalon has nothing to
  migrate or reconcile if a game changes its character model.
- Cross-game character portability (a shared universal avatar) is explicitly out
  of scope — see `Proposal.md` §7 and §29.
