# Social Graph

**A player's friends are a network-level relationship that persists across games.**
Entering a new game never means rebuilding a friends list. **A game never
automatically receives a player's social graph**; every read of it is gated by a
capability the player granted to that game and by the visibility the player set.

Narrative: [Proposal §11](../Proposal.md#11-universal-friends). The social layer
as a whole (friends, guilds, presence, communication) is one of Avalon's main
differentiators — what a player should not have to rebuild per game.

## What persists

| Thing | Persists across games? | Where it lives |
|---|---|---|
| Friendship (identity A ↔ identity B) | Yes | durable network relationship |
| Friend requests (pending state) | Yes, until resolved | server state; not promised-durable history |
| Presence of a friend | Ephemeral | see [presence](./presence.md) |
| Blocks / mutes | Yes | design open (below) |
| A game's own in-world social features (party, LFG) | No | game-side |

A friendship is symmetric: `Friendship { a, b, since }`. It references two
[identities](./identity.md), never two game characters. A player sees the same
friends list from the Hub, from Game A, and from Game B — filtered by what each
viewer is allowed to see.

## What a game sees

Requesting `friends.read` does not hand a game the whole graph. It means: for the
player who granted it, under an active [game binding](./game-bindings.md), the
game may read the friends the player has chosen to expose to games. The game
gets what it needs to say "your friend Alice is also here", not a dump of the
player's relationships across every world they've visited.

Reading `presence.read` is a separate capability. A game may know who a player's
friends are without knowing where they are, and vice versa.

The read path composes three things, in this order:

1. an active binding between the player and the game
2. an active `PermissionGrant` for the specific capability
3. the [visibility](./privacy.md) scope the player set on the resource

None of them widens the others.

## What this powers

- friends lists in the Hub and in any game that asks
- "who's online and where" (with presence)
- cross-game invitations and coordination (later, via communication primitives)
- guild rosters intersected with friends
- matchmaking integrations that a game chooses to build on top

## Blocking and harassment

Persistent identity makes harassment persistent too
([Proposal §31](../Proposal.md#31-major-risks)). Blocking must work across games,
not per game, and a block must at minimum hide the blocker's presence and refuse
friend requests network-wide. What a block means *inside* a game (can they still
be matched, can they see each other in a shared world) is the game's decision,
informed by the network fact. This is listed as open in
[Proposal §32](../Proposal.md#32-open-questions) and has no design yet.

## Today in the repo

- `crates/protocol/src/social.rs` — `Friendship` and `Presence` types only. No
  friend-request state, no block type.
- No server endpoints, storage, or events for friendships exist.
- `crates/sdk/src/lib.rs` — `Session::require(capability)` is the per-method
  capability check that `friends()` and presence reads will use; those methods
  don't exist yet.
- Whether a friendship is promised-durable history (emitting
  `friend.requested` / `friend.accepted` / `friend.removed` events, the names
  #15 uses) or server-only state is not yet written down; the
  [durable history](./protocol-events.md) classification is where that lands.

## Decisions and tickets

- [#14](https://github.com/LunarVagabond/avalon-protocol/issues/14) — Epic: Social
  Graph (Friends & Presence).
- [#15](https://github.com/LunarVagabond/avalon-protocol/issues/15) — friend
  request/accept/remove endpoints + storage.
- [#17](https://github.com/LunarVagabond/avalon-protocol/issues/17) — SDK
  `friends()` / presence reads with capability checks.
- [#18](https://github.com/LunarVagabond/avalon-protocol/issues/18) — Hub friends
  list view.
- [#87](https://github.com/LunarVagabond/avalon-protocol/issues/87) — visibility
  scopes for friends lists and presence.
- [#78](https://github.com/LunarVagabond/avalon-protocol/issues/78) — ADR:
  presence is ephemeral.
