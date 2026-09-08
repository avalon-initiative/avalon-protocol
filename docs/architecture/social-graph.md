# Social Graph

**A player's friends are a network-level relationship that persists across games.**
Entering a new game never means rebuilding a friends list. **A game never
automatically receives a player's social graph**; every read of it is gated by a
capability the player granted to that game and by the visibility the player set.

Narrative: [Proposal §11](../stakeholders/Proposal.md#11-universal-friends). The social layer
as a whole (friends, guilds, presence, communication) is one of Avalon's main
differentiators — what a player should not have to rebuild per game.

## What persists

| Thing | Persists across games? | Where it lives |
|---|---|---|
| Friendship (identity A ↔ identity B) | **Yes — decided** | durable network relationship, owned by the network, not either player's current game |
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
- cross-game invitations and coordination (later, via [communication primitives](./communication.md))
- guild rosters intersected with friends
- matchmaking integrations that a game chooses to build on top

## Blocking and harassment

Persistent identity makes harassment persistent too
([Proposal §31](../stakeholders/Proposal.md#31-major-risks)). Blocking works across
games, not per game (#97): a block hides the blocker's presence and refuses friend
requests network-wide, in both directions, and — unlike friendship — is never
durable protocol history, never a `crates/protocol` type, and never revealed to
the blocked party through any endpoint (see [privacy.md](./privacy.md)). What a
block means *inside* a game (can they still be matched, can they see each other
in a shared world) is still the game's own decision, informed by the network
fact, and remains open per [Proposal §32](../stakeholders/Proposal.md#32-open-questions).

## Today in the repo

- `crates/protocol/src/social.rs` — `Friendship`, `FriendRequest { from, to,
  requested_at }`, and `Presence` types. No block type.
- `crates/server/src/friends.rs` — session-authenticated endpoints:
  `POST /friends/requests`, `POST /friends/requests/{id}/accept`,
  `DELETE /friends/requests/{id}` (declines or withdraws, depending on which
  side calls it), `DELETE /friends/{identity_id}`, `GET /friends`,
  `GET /friends/requests`. There is no game-credential auth path in this repo
  yet at all, so "a game cannot act on a player's behalf" holds by
  construction — every route only ever accepts a player session token.
  `friend.requested`, `friend.accepted`, and `friend.removed` are enqueued
  through the outbox (`crates/server/src/outbox.rs`, #71) in the same
  transaction as the `friendships`/`friend_requests` row. Events are
  session-authenticated but not yet individually signed — no general
  per-event signing ceremony exists yet, only `identity.created`'s one-off
  Ed25519 signature (#73); see [protocol-events.md](./protocol-events.md).
- `crates/server/db/migrations/0004_social_graph/` — `friendships` (`a < b`
  enforced), `friend_requests` (at most one pending request per direction).
- `crates/server/db/migrations/0005_friend_handles/` — a short handle for
  adding friends without pasting a raw identity id
  ([#128](https://github.com/LunarVagabond/avalon-protocol/issues/128)):
  `profiles.discriminator`, a 4-digit string generated server-side (unique
  together with `display_name`, exposed as `display_name#discriminator` on
  `ProfileResponse.handle`). `GET /friends/handle/:handle` resolves a handle
  to an identity id — exact match only, session-authenticated like every
  other route in this module. A display-name change keeps its existing
  discriminator unless the new pair collides, in which case a fresh one is
  generated so uniqueness holds without the handle churning on every rename.
  Fuzzy/partial handle lookup is out of scope here — that's player discovery
  ([#129](https://github.com/LunarVagabond/avalon-protocol/issues/129), not decided).
- Reads are restricted to the caller's own session for now — the capability/
  visibility composition in [#87](https://github.com/LunarVagabond/avalon-protocol/issues/87)
  (which this doc's "What a game sees" section describes) is not built yet, so
  there is no game-facing read path at all.
- `crates/sdk/src/social.rs` (#17) — `Session::friends()`,
  `presence()`/`presence_of()`, `update_presence()`, and
  `subscribe_presence()` (#136), each gated behind
  `Session::require(capability)`.
- `crates/server/src/blocks.rs` + `crates/server/db/migrations/0006_blocks/`
  (#97) — `blocks (blocker, blocked, created_at)`, `POST /blocks`,
  `DELETE /blocks/:identity_id`, `GET /blocks` (the caller's own list only).
  Blocking a pending-friend-request partner auto-resolves that request.
  Enforced in `friends::create_friend_request` (a blocked pair's rejection
  is identical to a nonexistent-identity rejection) and
  `presence::get_presence`/`presence_ws` (a blocked identity's presence
  reads as `Offline`, indistinguishable from a genuinely missing entry).
- **A friendship is promised-durable history**, not server-only state — the
  same reasoning [guilds](./guilds.md) apply, since it's a social fact
  between two identities, not something any game owns. `friend.requested`,
  `friend.accepted`, and `friend.removed` are protocol events (catalogued in
  [protocol-events.md](./protocol-events.md)), emitted atomically with the
  projection row via the outbox (#71). A declined or withdrawn *request* is
  not itself durable history — only an established or ended friendship is.

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
