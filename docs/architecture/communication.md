# Communication: Direct Messages, Voice, and Notifications

**Avalon connects communities; it does not replace the tools they already use.**
Guild chat is already decided as a network primitive ([guilds.md](./guilds.md)):
a channel belongs to the guild, not to any one game, and is rendered by
whichever client a member happens to have open. This document covers the
remaining communication surfaces that guild chat doesn't — direct/small-group
conversations, voice, and notifications — and states the one architectural
rule that has to hold across all of them: **communication is realtime and
query infrastructure, the same vertical presence already occupies
([presence.md](./presence.md)), never settlement infrastructure.** Narrative:
[Proposal §12](../stakeholders/Proposal.md#12-communication).

None of this is scheduled work. It is documented now so that when it is
picked up, the shape is already decided instead of falling out of whatever
gets implemented first.

## What's already decided elsewhere

- **Guild chat** — a channel and its messages belong to the guild; message
  bodies are explicitly not protocol history. See
  [guilds.md](./guilds.md#guild-chat-is-a-network-primitive) and
  [#22](https://github.com/LunarVagabond/avalon-protocol/issues/22).
- **Blocking** — private, unilateral, server-side, and already specified to
  reject direct messages between blocked parties once they exist. See
  [#97](https://github.com/LunarVagabond/avalon-protocol/issues/97).
- **Presence** — ephemeral, never ledgered, the same vertical this document's
  realtime traffic shares. See [presence.md](./presence.md).

## Direct messages and small-group conversations

A `Conversation` is the identity-to-identity sibling of a `GuildChannel`: it
exists between two or more identities directly, with no guild in between, and
no game owns it. The same reasoning that makes a friendship
([social-graph.md](./social-graph.md)) and a guild
([guilds.md](./guilds.md)) network-level applies here — a conversation between
two players is a fact about their relationship, not about whatever game either
of them happened to be in when they started talking.

The hot/cold split guild chat already established carries over unchanged:

- `Conversation { id, participants }` and message send/receive are ordinary
  server state, not a `ProtocolEvent` — the same reasoning as
  [#22](https://github.com/LunarVagabond/avalon-protocol/issues/22) (high
  volume, non-interoperable, not something a receiving game ever needs to
  verify).
- What *is* worth being explicit about, unlike guild channels: a conversation
  requires the blocking rule from [#97](https://github.com/LunarVagabond/avalon-protocol/issues/97)
  to be enforced on send, not just on friend requests and presence.
- A game may render a conversation as a client of it, exactly as it may render
  a guild channel — it never becomes the conversation's host.

## Voice

**No design exists yet, and none should be assumed.** Voice needs its own
transport and signaling decision — this is the kind of infrastructure choice
that's expensive to unwind, so it belongs behind a `decision` ticket, not a
straight-to-implementation one. What's already clear regardless of how that
research lands:

- A voice session is realtime state, not durable history — the same
  classification as presence and chat delivery, never `SettlementProvider`.
- Voice sessions attach to the same two surfaces conversations and guild
  channels do: a guild's voice room, and a direct call between identities.
- Authorization follows the same membership/capability checks as guild chat
  and DMs — no separate authority model for voice.
- Provider/protocol choice (self-hosted SFU, a third-party voice API, WebRTC
  directly) is explicitly not decided, is out of scope for anything else in
  this document, and should not be inferred from any example elsewhere.

## Notifications

A thin delivery concern, not a new domain: telling a connected client that a
message arrived, a friend came online, or a guild event happened. Not
durable, not queryable history in its own right — it's a signal about
state that already lives in chat, presence, or guild history. No design
exists yet; when it's picked up, it should be a delivery mechanism on top of
the realtime vertical, not a store of its own.

## Avalon is not Discord

Avalon's job is to make guild chat, DMs, and voice work the same way
regardless of which game (if any) a player has open — not to build a
destination players go to instead of the tools they already use. A future
Discord bridge, mentioned as an example client in
[guilds.md](./guilds.md#guild-chat-is-a-network-primitive), is exactly that:
one more authorized client rendering the same network-owned channel, no
different in kind from a game rendering it or the Hub rendering it. Nothing
in this document should be read as scoping that bridge now; it's called out
only so a future conversation/voice design doesn't accidentally paint itself
into a Hub-only or game-only corner.

## Today in the repo

- No `Conversation` type, endpoint, or event exists in any crate.
- No voice module, session type, or transport of any kind exists.
- No notification delivery mechanism exists; `avalon-server` has no
  websocket/push path yet (same gap noted in [presence.md](./presence.md)).
- Guild chat ([#22](https://github.com/LunarVagabond/avalon-protocol/issues/22))
  and blocking ([#97](https://github.com/LunarVagabond/avalon-protocol/issues/97))
  are the only communication-adjacent surfaces with a real design.

## Decisions and tickets

- [guilds.md](./guilds.md), [social-graph.md](./social-graph.md),
  [presence.md](./presence.md) — the decided primitives this document builds
  on and does not restate.
- [#101](https://github.com/LunarVagabond/avalon-protocol/issues/101) — Epic:
  Direct Messages, Voice & Notifications:
  [#102](https://github.com/LunarVagabond/avalon-protocol/issues/102)
  conversation domain model + endpoints,
  [#103](https://github.com/LunarVagabond/avalon-protocol/issues/103) —
  decision: voice transport, signaling, and provider (open),
  [#104](https://github.com/LunarVagabond/avalon-protocol/issues/104) SDK
  conversation API,
  [#105](https://github.com/LunarVagabond/avalon-protocol/issues/105) Hub
  direct-message UI. Backlog — not scheduled ahead of #73/#71 or #14/#19.
