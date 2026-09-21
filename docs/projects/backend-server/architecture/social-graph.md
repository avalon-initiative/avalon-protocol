# Social Graph

**An identity's friends are a network-level relationship that persists across integrators.**
Entering a new integrator never means rebuilding a friends list. **An integrator never
automatically receives an identity's social graph**; every read of it is gated by a
capability the identity granted to that integrator and by the visibility the identity set.

Narrative: [Proposal §11](../../../stakeholders/Proposal.md#11-universal-friends). The social layer
as a whole (friends, guilds, presence, communication) is one of Avalon's main
differentiators — what an identity should not have to rebuild per integrator.

## What persists

| Thing | Persists across integrators? | Where it lives |
|---|---|---|
| Friendship (identity A ↔ identity B) | **Yes — decided** | durable network relationship, owned by the network, not either identity's current integrator |
| Friend requests (pending state) | Yes, until resolved | server state; not promised-durable history |
| Presence of a friend | Ephemeral | see [presence](./presence.md) |
| Blocks / mutes | Yes | design open (below) |
| An integrator's own in-world social features (party, LFG) | No | integrator-side |

A friendship is symmetric: `Friendship { a, b, since }`. It references two
[identities](./identity.md), never two game characters. An identity sees the same
friends list from the Hub, from Integrator A, and from Integrator B — filtered by what each
viewer is allowed to see.

## What an integrator sees

Requesting `friends.read` does not hand an integrator the whole graph. It means: for the
identity who granted it, under an active [integrator binding](./bindings.md), the
integrator may read the friends the identity has chosen to expose to integrators. The integrator
gets what it needs to say "your friend Alice is also here", not a dump of the
identity's relationships across every world they've visited.

Reading `presence.read` is a separate capability. An integrator may know who an identity's
friends are without knowing where they are, and vice versa.

The read path composes three things, in this order:

1. an active binding between the identity and the integrator
2. an active `PermissionGrant` for the specific capability
3. the [visibility](./privacy.md) scope the identity set on the resource

None of them widens the others.

## What this powers

- friends lists in the Hub and in any integrator that asks
- "who's online and where" (with presence)
- cross-integrator invitations and coordination (later, via [communication primitives](./communication.md))
- guild rosters intersected with friends
- matchmaking integrations that an integrator chooses to build on top

## Blocking and harassment

Persistent identity makes harassment persistent too
([Proposal §31](../../../stakeholders/Proposal.md#31-major-risks)). Blocking works across
integrators, not per integrator (#97): a block hides the blocker's presence and refuses friend
requests network-wide, in both directions, and — unlike friendship — is never
durable protocol history, never a `crates/protocol` type, and never revealed to
the blocked party through any endpoint (see [privacy.md](./privacy.md)). What a
block means *inside* an integrator (can they still be matched, can they see each other
in a shared world) is still the integrator's own decision, informed by the network
fact, and remains open per [Proposal §32](../../../stakeholders/Proposal.md#32-open-questions).

**Conversations (#102).** The same never-reveal rule extends to direct/small-group
messaging: `crate::blocks::has_block_among` (`crates/server/src/blocks.rs`)
checks whether a block exists between *any* two participants in a
conversation — not just a fixed sender/recipient pair, since a conversation
can have more than two people — and `crate::conversations::send_message`
rejects the send if so. The rejection reuses the exact same error a genuine
non-participant gets (`AppError::NotConversationParticipant`), so a blocked
participant's failed send looks identical to never having been in the
conversation at all. See [communication.md](./communication.md#direct-messages-and-small-group-conversations).

## Today in the repo

- `crates/protocol/src/social.rs` — `Friendship`, `FriendRequest { from, to,
  requested_at }`, and `Presence` types. No block type.
- `crates/server/src/friends.rs` — session-authenticated endpoints:
  `POST /friends/requests`, `POST /friends/requests/{id}/accept`,
  `DELETE /friends/requests/{id}` (declines or withdraws, depending on which
  side calls it), `DELETE /friends/{identity_id}`, `GET /friends`,
  `GET /friends/requests`. There is no integrator-credential auth path in this repo
  yet at all, so "an integrator cannot act on an identity's behalf" holds by
  construction — every route only ever accepts an identity session token.
  `friend.requested`, `friend.accepted`, and `friend.removed` are enqueued
  through the outbox (`crates/server/src/outbox.rs`, #71) in the same
  transaction as the `friendships`/`friend_requests` row. Events are
  session-authenticated but not yet individually signed — no general
  per-event signing ceremony exists yet, only `identity.created`'s one-off
  Ed25519 signature (#73); see [protocol-events.md](./protocol-events.md).
- `crates/server/db/migrations/0004_social_graph/` — `friendships` (`a < b`
  enforced), `friend_requests` (at most one pending request per direction).
- `crates/server/db/migrations/0005_friend_handles/` (superseded by
  `.../0063_drop_discriminator_unique_display_names`, issue #510) — a short
  handle for adding friends without pasting a raw identity id
  ([#128](https://github.com/LunarVagabond/avalon-protocol/issues/128)).
  Originally a `profiles.discriminator` 4-digit suffix (unique together
  with `display_name`, `display_name#discriminator`). **Now (#510):
  `display_name` itself is the handle** — globally unique,
  case-insensitive (`profiles_display_name_lower_idx`), no discriminator.
  `GET /friends/handle/:handle` resolves it to an identity id — exact
  match only, session-authenticated like every other route in this
  module. Uniqueness is enforced by that index at the actual write
  (`avalon_indexer::projections::profiles::apply`), not a separate prior
  check a concurrent writer could race past; a taken name is a hard
  rejection (`AppError::DisplayNameTaken`) at registration or rename time,
  never an auto-suggested variant. Decided: Discord's *current* scheme
  (globally-unique handle), not the deprecated `name#1234` one it
  replaced — the old scheme's 4-digit space getting crowded at scale was
  exactly the friction Discord dropped it for. Fuzzy/partial handle lookup
  is out of scope here — that's identity discovery
  ([#129](https://github.com/LunarVagabond/avalon-protocol/issues/129), decided:
  two-tier. Private-by-default, always-on surfacing of identities via mutual
  friends ("friends of friends") and shared guild membership — never a name
  search, just relationships that already exist. Separately, a first-class,
  easily reversible per-identity toggle ("make me publicly searchable") that
  opts an identity into open name/handle search — off by default, matches only
  opted-in identities when on, and flips off just as easily as it flips on).
  Implementation tracked as
  [#204](https://github.com/LunarVagabond/avalon-protocol/issues/204)
  (scoped surfacing) and
  [#205](https://github.com/LunarVagabond/avalon-protocol/issues/205)
  (opt-in global toggle), both under this epic.
- `crates/server/src/discovery.rs` (#204) — `GET /people/discover`, the
  always-on half of #129's decided shape. Session-authenticated with no
  query parameter of any kind — the only input is the caller's own
  session, never a search term. Candidates come from two sources, unioned
  and deduplicated: friends-of-friends (`friendships` rows touching one of
  the caller's own friends) and mutual guild membership (`guild_members`
  rows sharing a `guild_id` with the caller). Both sources are filtered
  through the caller themselves, `friends::friend_partners`, and
  `blocks::block_partners` — the exact same "compute related identities"
  helpers `presence.rs` established for #16, reused rather than
  reimplemented, so a blocked relationship is excluded from discovery with
  the identical guarantee it already gets from presence and friend
  requests. Milestone-1 stand-in, matching #154's `guilds::discover_guilds`
  precedent: a direct query, not the real indexer read model. Read-only —
  acting on a suggestion still goes through `POST /friends/requests`
  unchanged; this endpoint never creates or modifies a friendship.
- `crates/server/src/discovery.rs` (#205) — `GET /identities/search?q=&limit=`,
  the opt-in half of #129's decided shape. Session-authenticated, matches
  only identities with `discovery_preferences.discoverable = true` (an
  identity-controlled preference, off by default for every identity, no
  exceptions — same "one row per identity, created lazily on first
  toggle, absence means the default" shape
  `presence_preferences.hide_playing` (#16) already established, see
  `crates/server/db/migrations/0026_discovery_preferences`). Toggled via
  `PATCH /me`'s `discoverable` field
  (`crates/server/src/handlers.rs::update_profile`), same "extend
  `PATCH /me`" precedent #153/#155 set for other small profile-adjacent
  fields — not a dedicated endpoint. Fuzzy, case-insensitive `ILIKE`
  substring match against `display_name` — issue #510: that's the handle
  now, no separate discriminator suffix to also match. A non-opted-in
  identity never appears, full stop, even to a caller who already knows
  its exact handle — that's the separate, untouched
  `friends::resolve_handle` exact-match path. Excludes the caller and any
  blocked relationship in
  either direction via `blocks::block_partners`, same reuse `discover_people`
  (#204) already established. Turning the toggle off removes an identity
  from every subsequent search call immediately — the preference read is
  a plain `WHERE discoverable = true` against `&state.pool` on every
  call, not a cached/snapshotted value, so there is no grace period.
  `GET /me` also returns the caller's own current `discoverable` value, so
  the Hub's "you are currently publicly searchable" indicator can never
  drift out of sync with the stored preference.
- Reads are restricted to the caller's own session for now — the capability/
  visibility composition in [#87](https://github.com/LunarVagabond/avalon-protocol/issues/87)
  (which this doc's "What an integrator sees" section describes) is not built yet, so
  there is no integrator-facing read path at all.
- `crates/sdk/src/social.rs` (#17) — `Session::friends()`,
  `presence()`/`presence_of()`, `update_presence()`, and
  `subscribe_presence()` (#136), each gated behind
  `Session::require(capability)`.
- `crates/server/src/blocks.rs` + `crates/server/db/migrations/0006_blocks/`
  (#97) — `blocks (blocker, blocked, created_at)`, `POST /blocks`,
  `DELETE /blocks/:identity_id`, `GET /blocks` (the caller's own list only).
  Blocking a pending-friend-request partner auto-resolves that request.
  Enforced in `friends::create_friend_request` (a blocked pair's rejection
  is identical to a nonexistent-identity rejection),
  `presence::get_presence`/`presence_ws` (a blocked identity's presence
  reads as `Offline`, indistinguishable from a genuinely missing entry), and
  `conversations::send_message` (#102) via the group-aware
  `blocks::has_block_among` — see "Blocking and harassment" above.
  Reachable from the Hub (#460): `UserProfile.vue`'s actions row
  (Add/Remove friend, Block/Unblock, mirroring whichever relationship
  actually exists between the caller and the viewed identity) and a
  "Blocked users" card on `Profile.vue` (list + unblock, plus block by
  id/handle directly).
- **A friendship is promised-durable history**, not server-only state — the
  same reasoning [guilds](./guilds.md) apply, since it's a social fact
  between two identities, not something any integrator owns. `friend.requested`,
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
- [#129](https://github.com/LunarVagabond/avalon-protocol/issues/129) — decision:
  identity discovery is two-tier (scoped always-on + opt-in global search).
- [#204](https://github.com/LunarVagabond/avalon-protocol/issues/204) — scoped
  identity discovery: friends-of-friends and mutual-guild surfacing (done).
- [#205](https://github.com/LunarVagabond/avalon-protocol/issues/205) — opt-in
  global name/handle search toggle (done).
- [#78](https://github.com/LunarVagabond/avalon-protocol/issues/78) — ADR:
  presence is ephemeral.
