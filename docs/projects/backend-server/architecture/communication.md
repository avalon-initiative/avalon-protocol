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
[Proposal §12](../../../stakeholders/Proposal.md#12-communication).

Direct/small-group conversations have a real, shipped design and implementation.
Voice and notifications-as-a-delivery-layer remain undesigned, and neither is
scheduled work — documented now so that when either is picked up, the shape is
already decided instead of falling out of whatever gets implemented first.

## What's already decided elsewhere

- **Guild chat** — a channel and its messages belong to the guild; message
  bodies are explicitly not protocol history. See
  [guilds.md](./guilds.md#guild-chat-is-a-network-primitive).
- **Blocking** — private, unilateral, server-side, and enforced against
  conversation sends (below).
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
  server state, not a `ProtocolEvent` — high volume, non-interoperable, not
  something a receiving integrator ever needs to verify.
  `ConversationMessage { id, conversation_id, author, body, sent_at }`
  lives in its own `conversation_messages` table, outside
  `SettlementProvider::commit`; no `conversation.message_*` event kind exists.
- Retention is a configurable per-conversation cap
  (`CONVERSATION_MESSAGE_CAP`, default 10,000), keeping the newest N and
  hard-deleting the rest once the cap is exceeded — plain count-cap pruning,
  not the archive tier guild chat later grew, which is a decision specific to
  guild chat, not part of this design. Conversation history is not permanent.
- Creating a conversation is idempotent on its exact participant set: asking
  for a conversation with the same identities again, in any order, returns
  the existing conversation rather than creating a duplicate.
- A conversation requires the blocking rule to be enforced on send, not just
  on friend requests and presence — including across a group conversation of
  more than two people, where a block between *any* two participants (not
  only the sender) rejects the send. The rejection is silent from the blocked
  party's perspective: it returns the exact same error a genuine
  non-participant gets, never a distinguishable "blocked" response. See
  [social-graph.md](./social-graph.md#blocking-and-harassment).
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

**Deferred, on hold.** Voice is deferred indefinitely rather than committed to a
transport/provider now: text (guild chat, DMs) already covers the core
cross-integrator communication goal, and voice is a materially bigger,
expensive-to-unwind infrastructure commitment (self-hosted SFU operations,
or a third-party dependency the rest of the protocol has otherwise avoided)
with no existing precedent in this codebase — matching the deferral posture
`future-layers.md` already takes for other large optional additions. Revisit
only when a concrete integration actually demands it, not on a fixed timeline.
What's already clear whenever that happens:

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
state that already lives in chat, presence, or guild history.

**Guild announcement alerts** are the first real slice: `GET
/me/guild-announcements` is a plain, read-only aggregate — the most recent
posts to any announcement-only channel in any guild the caller
currently belongs to, scoped to current membership by construction (a
`JOIN guild_members` on the read, not a subscription that has to be
explicitly torn down when someone leaves). The Hub polls it on the same
cadence it uses elsewhere, no WebSocket needed for this volume. Read/unread
state is **entirely client-local** — a per-channel "last seen" timestamp kept
in the Hub's own `localStorage` (`avalon-hub/apps/hub/src/api/guildAnnouncements.ts`),
never server state: there is nothing here durable enough to be worth tracking
server-side, and guild chat itself is operational-tier, not protocol history. A
channel is marked seen when a member actually opens it from the alert (not
merely by the alert panel being open).

MOTD-change alerts are explicitly deferred, not built. `guild.updated`'s
payload already distinguishes which fields changed, so detecting "the MOTD
changed" specifically is feasible whenever there's real demand for it; an
MOTD-change alert risks being noisy rather than valuable (unlike an
announcement post, which is inherently something a guild's leadership chose to
broadcast). Revisit if a real request for it shows up — the same "don't build
ahead of demand" posture the rest of this document takes for notifications
generally.

A second slice landed on top of this: `useNotificationSummary`
(`avalon-hub/apps/hub/src/composables/`) aggregates seven pending-action sources into one
badge in `HubShell.vue` — incoming friend requests, guild join requests
awaiting a manager's review, guild invites received, device-grant approval
requests, recovery requests a guardian can approve, new guardian designations,
and unread direct messages. It's a read-only aggregator over each source's own
existing endpoint, not a new server-side notification store. Direct messages
get the identical client-local "last seen" treatment guild announcements
already established (`avalon-hub/apps/hub/src/api/notifications.ts`'s
`markConversationSeen`/`isConversationUnread`), marked seen the moment a
thread is actually opened, not by the aggregate panel merely being open. Being
named a recovery guardian gets the same "have I seen this" local tracking,
since it has no accept/decline of its own to naturally clear it. The other
five sources are genuinely actionable pending state — their count only drops
by resolving the item (accepting, approving, rejecting), not by visiting the
page, since that's the correct signal for something the caller must actually
act on.

Beyond this, a true delivery mechanism (server-pushed, not client-polled) is
still unbuilt; when it's picked up, it should be a delivery layer on top of
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

## Current implementation

- `crates/protocol/src/social.rs` — `Conversation { id, participants }` and
  `ConversationMessage { id, conversation_id, author, body, sent_at }`.
- `crates/server/src/conversations.rs` — session-authenticated,
  participant-only endpoints: `POST /conversations` (idempotent on
  participant set), `GET /conversations` (the caller's own list),
  `GET /conversations/{id}/messages?before=&limit=` (cursor pagination, same
  style as guild messages), `POST /conversations/{id}/messages`.
  `conversations`/`conversation_participants`/`conversation_messages` hold
  the state; nothing here touches the outbox or `SettlementProvider::commit`,
  checked both by construction and by a source grep
  (`crates/server/tests/conversations_no_ledger.rs`, mirroring
  `guild_messages_no_ledger.rs`'s approach).
- Blocking is enforced on send **and read** via `crate::blocks::has_block_among`
  (`require_unblocked_participant`, the single gate both endpoints call),
  reused rather than reinvented — the same table and "never reveal" posture
  already established for friend requests and presence, extended here to check every
  pair within a conversation's participant set, not just a fixed pair.
  `require_unblocked_participant` always runs the participant-membership
  lookup and the pairwise block check regardless of which one would already
  fail, so a blocked participant costs the same two queries as a genuine
  non-participant, and both collapse to the identical
  `AppError::NotConversationParticipant`. Read access (`GET
  .../messages`) runs the same gate as posting — a blocked-out participant
  cannot use the asymmetry between read and write to confirm they've been
  blocked. `list_my_conversations` applies the same check to its own listing
  for consistency, though it leaks nothing on its own since the caller
  already knows the conversation id.
- **Relationship gate on creation.** `create_conversation` requires every
  named participant to already be a friend or mutual guild member of the
  caller, reusing `friends::friend_partners` and
  `discovery::mutual_guild_members`. This also closes an identity-existence
  oracle the endpoint would otherwise have — a nonexistent id can't satisfy
  the relationship check either, so it's rejected the same way as an
  existing-but-unrelated id, never distinguishably.
- No voice module, session type, or transport of any kind exists.
- No voice/notification-delivery transport exists beyond chat/presence
  (voice is deferred, see above; a generic "tell a connected client
  something happened" notification layer doesn't exist as its own thing —
  see the Notifications section above).
- **`GET /ws/messages`** (`crates/server/src/chat.rs`) — live push for new guild
  channel messages, deleted guild channel messages, and new conversation
  messages, matching the freshness-tier policy applied across the Hub (guild
  chat and DMs must feel real-time). Same shape as `presence::presence_ws`:
  `?token=` query param auth, a single broadcast fanned out and filtered
  per-connection by what the client subscribed to
  (`subscribe_channel`/`subscribe_conversation`), lossy on a slow consumer
  (the paginated `GET .../messages` is always there to reconcile against,
  same tradeoff `presence.md` documents for its own push). SDK:
  `GuildHandle::channel(id).subscribe_messages()` and
  `ConversationHandle::subscribe_messages()`, mirroring `Session::subscribe_presence`.
- **Hub UI**: `avalon-hub/apps/hub/src/views/Messages.vue` — a conversation-list sidebar
  next to the active thread, the same "swap selection in place, no remount"
  shape the guild page's Channels tab uses. Reuses `AvalonChatMessage`/
  `AvalonChatComposer` unmodified (both already wire-shape-agnostic —
  neither carries a guild/channel field) — no separate the UI library
  component. `useConversations`/`useConversationThread`/`useGuildChat`
  (`avalon-hub/apps/hub/src/composables/`) subscribe to `GET /ws/messages` for new
  messages; `avalon-hub/apps/hub/src/api/client.ts`'s
  `openChannelMessageSocket`/`openConversationMessageSocket` are the Hub's
  own plain-`WebSocket` clients for it, the same "Hub doesn't consume the
  Rust SDK directly" posture `openPresenceSocket` already established.
  Starting a conversation is a friend-row action (`AvalonFriendRow`'s
  `message` emit) rather than a separate "new message" flow —
  `POST /conversations`'s idempotent-on-participant-set behavior is what
  makes "start or open" a single call.
- **Cross-node relay.** `GET /ws/messages` above is one process's own
  `ChatBus` broadcast — without cross-node relay, a guildmate connected to a
  different node than the sender never gets the live push (the paginated
  `GET` still eventually shows the message, just without the live-push
  immediacy). `guild_messages::send_message`/`delete_message` and
  `conversations::send_message` also call
  `crate::realtime_relay::relay_to_peers` right after their own local
  `ChatBus::publish_*` call, posting once to every same-`network_id` peer
  advertising a `realtime`/`gateway`/`combined` role. The receiving node's
  `POST /nodes/relay` handler calls the exact same `ChatBus::publish_*`
  methods directly — a relayed chat event only ever feeds that node's local
  fan-out, **never** the `guild_messages`/`conversation_messages` Postgres
  tables (async at-rest replication of chat history to additional nodes is a
  separate concern, described next). Single hop by construction: the relay
  handler never calls `relay_to_peers` again, so no origin-node bookkeeping is
  needed to prevent a loop, though that reasoning depends on the peer mesh
  staying small and fully interconnected — see `crate::realtime_relay`'s own
  module doc comment. Live-verified with two real `avalon-server` processes
  sharing one Postgres and a real websocket subscriber on the second node
  (`crates/server/tests/realtime_relay.rs`).
- **At-rest replication, so chat history survives a node's loss.** A separate
  concern from the live relay above — this never touches a broadcast channel,
  and the relay never touches Postgres. `crate::chat_replication`:
  `send_message`/`delete_message` also call `replicate_to_peers` (spawned,
  not awaited inline) right after their existing `ChatBus`/`relay_to_peers`
  calls, posting once to exactly one deterministic target — the
  lexicographically-smallest same-`network_id` peer advertising an
  `indexer`/`combined` role (distinct eligibility from the relay's
  `realtime`/`gateway`, since this is about durable storage capacity, not
  live push). The target's `POST /nodes/replicate-chat` handler writes into
  its own `guild_messages_replica`/`conversation_messages_replica` tables —
  **deliberately not** the live `guild_messages`/`conversation_messages`
  tables themselves, since those tables' foreign keys reference
  `guild_channels`/`identities` data a replication target may not itself
  hold a copy of. A deletion sets `deleted_at` on the replica row rather than
  removing it. Every replication attempt logs its outcome and elapsed time
  (`tracing::debug` on success, `tracing::warn` on failure/rejection) —
  replication lag is observable, never silently swallowed. A single-node
  deployment, or one with no `indexer`/`combined` peer, is unaffected
  (`replicate_to_peers` is a no-op with no eligible target). Live-verified
  with two real `avalon-server` processes: a message sent on node A lands in
  node B's own `guild_messages_replica` (`crates/server/tests/chat_replication.rs`).
</content>
</invoke>
