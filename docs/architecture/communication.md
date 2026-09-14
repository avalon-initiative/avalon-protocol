# Communication: Direct Messages, Voice, and Notifications

**Avalon connects communities; it does not replace the tools they already use.**
Guild chat is already decided as a network primitive ([guilds.md](./guilds.md)):
a channel belongs to the guild, not to any one integrator, and is rendered by
whichever client a member happens to have open. This document covers the
remaining communication surfaces that guild chat doesn't — direct/small-group
conversations, voice, and notifications — and states the one architectural
rule that has to hold across all of them: **communication is realtime and
query infrastructure, the same vertical presence already occupies
([presence.md](./presence.md)), never settlement infrastructure.** Narrative:
[Proposal §12](../stakeholders/Proposal.md#12-communication).

Direct/small-group conversations now have a real, shipped design and
implementation (issue #102); voice and notifications remain undesigned, and
neither is scheduled work — documented now so that when either is picked up,
the shape is already decided instead of falling out of whatever gets
implemented first.

## What's already decided elsewhere

- **Guild chat** — a channel and its messages belong to the guild; message
  bodies are explicitly not protocol history. See
  [guilds.md](./guilds.md#guild-chat-is-a-network-primitive) and
  [#22](https://github.com/LunarVagabond/avalon-protocol/issues/22).
- **Blocking** — private, unilateral, server-side, and enforced against
  conversation sends (below). See
  [#97](https://github.com/LunarVagabond/avalon-protocol/issues/97).
- **Presence** — ephemeral, never ledgered, the same vertical this document's
  realtime traffic shares. See [presence.md](./presence.md).

## Direct messages and small-group conversations

A `Conversation` (`crates/protocol/src/social.rs`) is the identity-to-identity
sibling of a `GuildChannel`: it exists between two or more identities
directly, with no guild in between, and no integrator owns it. The same reasoning
that makes a friendship ([social-graph.md](./social-graph.md)) and a guild
([guilds.md](./guilds.md)) network-level applies here — a conversation between
two identities is a fact about their relationship, not about whatever integrator either
of them happened to be in when they started talking.

The hot/cold split guild chat already established carries over unchanged:

- `Conversation { id, participants }` and message send/receive are ordinary
  server state, not a `ProtocolEvent` — the same reasoning as
  [#22](https://github.com/LunarVagabond/avalon-protocol/issues/22) (high
  volume, non-interoperable, not something a receiving integrator ever needs to
  verify). `ConversationMessage { id, conversation_id, author, body, sent_at }`
  lives in its own `conversation_messages` table, outside
  `SettlementProvider::commit`; no `conversation.message_*` event kind exists.
- Retention is a configurable per-conversation cap
  (`CONVERSATION_MESSAGE_CAP`, default 10,000), keeping the newest N and
  hard-deleting the rest once the cap is exceeded — plain count-cap pruning,
  not the archive tier guild chat later grew (#253); that was a follow-up
  decision specific to guild chat, not part of this design. Conversation
  history is not permanent.
- Creating a conversation is idempotent on its exact participant set: asking
  for a conversation with the same identities again, in any order, returns
  the existing conversation rather than creating a duplicate.
- A conversation requires the blocking rule from
  [#97](https://github.com/LunarVagabond/avalon-protocol/issues/97) to be
  enforced on send, not just on friend requests and presence — including
  across a group conversation of more than two people, where a block between
  *any* two participants (not only the sender) rejects the send. The
  rejection is silent from the blocked party's perspective: it returns the
  exact same error a genuine non-participant gets, never a distinguishable
  "blocked" response. See [social-graph.md](./social-graph.md#blocking-and-harassment).
- An integrator may render a conversation as a client of it, exactly as it may render
  a guild channel — it never becomes the conversation's host.
- **Sending while offline is not this domain's problem to solve.** A message
  composed with no connectivity queues through the SDK's sync journal and
  submission engine ([`./synchronization.md`](./synchronization.md)) like any
  other deferrable operation — a conversation is a consumer of that
  infrastructure, not a second place that invents a pending-message queue.
  The UI pattern it enables: a queued message shows *pending* until the
  submission engine confirms it, never silent loss and never a false
  "delivered."

## Voice

**Deferred, on hold.** [#103](https://github.com/LunarVagabond/avalon-protocol/issues/103)
(decided) chose to defer voice indefinitely rather than commit to a
transport/provider now: text (guild chat, DMs) already covers the core
cross-integrator communication goal, and voice is a materially bigger,
expensive-to-unwind infrastructure commitment (self-hosted SFU operations,
or a third-party dependency the rest of the protocol has otherwise avoided)
with no existing precedent in this codebase — matching the deferral posture
`future-layers.md` already takes for other large optional additions. Tracked
as its own on-hold epic; revisit only when a concrete integration actually
demands it, not on a fixed timeline. What's already clear whenever that
happens:

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
regardless of which integrator (if any) an identity has open — not to build a
destination identities go to instead of the tools they already use. A future
Discord bridge, mentioned as an example client in
[guilds.md](./guilds.md#guild-chat-is-a-network-primitive), is exactly that:
one more authorized client rendering the same network-owned channel, no
different in kind from an integrator rendering it or the Hub rendering it. Nothing
in this document should be read as scoping that bridge now; it's called out
only so a future conversation/voice design doesn't accidentally paint itself
into a Hub-only or integrator-only corner.

## Today in the repo

- `crates/protocol/src/social.rs` — `Conversation { id, participants }` and
  `ConversationMessage { id, conversation_id, author, body, sent_at }`.
- `crates/server/src/conversations.rs` (issue #102) — session-authenticated,
  participant-only endpoints: `POST /conversations` (idempotent on
  participant set), `GET /conversations` (the caller's own list),
  `GET /conversations/{id}/messages?before=&limit=` (cursor pagination, same
  style as guild messages), `POST /conversations/{id}/messages`.
  `conversations`/`conversation_participants`/`conversation_messages`
  (migration `0035_conversations`) hold the state; nothing here touches the
  outbox or `SettlementProvider::commit`, checked both by construction and by
  a source grep (`crates/server/tests/conversations_no_ledger.rs`, mirroring
  `guild_messages_no_ledger.rs`'s approach for #22).
- Blocking is enforced on send via `crate::blocks::has_block_among`, reused
  rather than reinvented — the same table and "never reveal" posture
  [#97](https://github.com/LunarVagabond/avalon-protocol/issues/97) already
  established for friend requests and presence, extended here to check every
  pair within a conversation's participant set, not just a fixed pair.
- **Relationship gate on creation** ([#269](https://github.com/LunarVagabond/avalon-protocol/issues/269)):
  `create_conversation` requires every named participant to already be a
  friend or mutual guild member of the caller, reusing
  `friends::friend_partners` and `discovery::mutual_guild_members`. This also
  closes an identity-existence oracle the endpoint previously had — a
  nonexistent id can't satisfy the relationship check either, so it's
  rejected the same way as an existing-but-unrelated id, never distinguishably.
- No voice module, session type, or transport of any kind exists.
- No notification delivery mechanism exists; `avalon-server` has no
  websocket/push path yet (same gap noted in [presence.md](./presence.md)).
- **Hub UI ([#105](https://github.com/LunarVagabond/avalon-protocol/issues/105)),
  landed**: `apps/hub/src/views/Messages.vue` — a conversation-list sidebar
  next to the active thread, the same "swap selection in place, no remount"
  shape [#241](https://github.com/LunarVagabond/avalon-protocol/issues/241)
  set for Guild.vue's Channels tab. Reuses `AvalonChatMessage`/
  `AvalonChatComposer` unmodified (both already wire-shape-agnostic —
  neither carries a guild/channel field) — no new `packages/ui` component.
  `useConversations`/`useConversationThread`
  (`apps/hub/src/composables/`) mirror `useGuildChat.ts`'s
  load-once-then-poll/cursor-pagination shape, minus everything
  guild-specific (no roles/permissions, no delete — conversations have no
  moderation-delete endpoint). Starting a conversation is a friend-row
  action (`AvalonFriendRow`'s new `message` emit) rather than a separate
  "new message" flow, per this ticket's own design note — `POST
  /conversations`'s idempotent-on-participant-set behavior is what makes
  "start or open" a single call.

## Decisions and tickets

- [guilds.md](./guilds.md), [social-graph.md](./social-graph.md),
  [presence.md](./presence.md) — the decided primitives this document builds
  on and does not restate.
- [#101](https://github.com/LunarVagabond/avalon-protocol/issues/101) — Epic:
  Direct Messages, Voice & Notifications:
  [#102](https://github.com/LunarVagabond/avalon-protocol/issues/102)
  conversation domain model + endpoints (done — `crates/server/src/conversations.rs`),
  [#104](https://github.com/LunarVagabond/avalon-protocol/issues/104) SDK
  conversation API,
  [#105](https://github.com/LunarVagabond/avalon-protocol/issues/105) Hub
  direct-message UI (landed — see "Today in the repo" above). The remainder
  of the epic (voice, notifications) is backlog — not scheduled ahead of
  #14/#19.
- [#269](https://github.com/LunarVagabond/avalon-protocol/issues/269) —
  closed the identity-existence oracle and unsolicited-conversation gap in
  #102's `create_conversation` by requiring an existing relationship
  (friend or mutual guild member), described above.
- [#103](https://github.com/LunarVagabond/avalon-protocol/issues/103) —
  decision: voice transport, signaling, and provider (closed/decided:
  deferred indefinitely). Tracked as
  [#203](https://github.com/LunarVagabond/avalon-protocol/issues/203) —
  Epic: Voice Communication (on hold), split out from #101 since voice is
  deferred while the rest of #101 is not.
- [#108](https://github.com/LunarVagabond/avalon-protocol/issues/108) — Epic:
  Offline & Deferred Protocol Synchronization
  ([`./synchronization.md`](./synchronization.md)) — direct-message queuing
  while offline is this epic's job, consumed by #102, not reinvented here.
