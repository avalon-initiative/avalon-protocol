# Privacy and Visibility

**Nothing about an identity is exposed because the protocol can technically expose
it.** Every read path names the visibility scope it checks. **Network analytics
are aggregates**, never per-identity data. A portable identity makes surveillance
portable too ([Proposal §31](../stakeholders/Proposal.md#31-major-risks)); the answer is
intentional scoping, not hoping nobody looks.

This document is about who can see *what an identity has done within Avalon* —
a separate and prior question from whether Avalon identity ties back to a
real person at all. It does not: see
[`./identity.md`](./identity.md#what-identity-is-not) for why that's a
structural property of self-custodied identity, not a visibility setting
that could be reconfigured.

## Scopes

| Scope | Who can see |
|---|---|
| public | anyone, including unauthenticated readers of a mirror |
| authenticated-only | any Avalon identity |
| friend-visible | identities in the subject's friends list |
| guild-visible | members of a given guild (or of any shared guild) |
| integrator-visible | an integrator holding the relevant capability under an active binding |
| private | the subject only |
| operator-only | node operators, for diagnostics; never surfaced through the API |

A scope applies per resource: profile fields, presence, friends list, guild
membership list, achievement history (with per-claim hide/feature on top). Guilds
also set a policy on their own roster (public, members-only, hidden).

## Composition

An integrator's capability grant never widens what non-game viewers can see, and a
public visibility setting never grants an integrator a capability it wasn't given. For a
read by an integrator the order is: active [binding](./bindings.md) → active
`PermissionGrant` for the specific capability → visibility scope. For a read by a
person: relationship to the subject → visibility scope. One authorization helper
answers (viewer, subject, resource); no endpoint does its own ad-hoc check.

## Defaults

User-controlled, changeable through the API and (once built) the Hub.
Presence and guild membership are real and player/guild-changeable as of
#87; the rest are still the proposed defaults #87 didn't implement this
pass — see "Today in the repo" for exactly what's live.

| Resource | Default |
|---|---|
| display name | public |
| avatar | public |
| presence | friends (real, changeable — `profiles.presence_visibility`) |
| friends list | private (proposed — no third-party read endpoint exists yet) |
| guild membership | guild_members (real, changeable — `guilds.roster_visibility`) |
| achievement history | public, individually hideable (proposed) |
| integrator bindings (which integrators a user plays) | private (proposed) |

Visibility settings are identity state, not durable protocol history, unless a
later decision promotes them.

## Presence and guilds

Presence is the most sensitive realtime signal ("where is this person right
now") and defaults to friends. See [presence](./presence.md). Guild membership
visibility is policy-controlled by both the identity and the guild; a guild that
wants a hidden roster gets one. See [guilds](./guilds.md).

## Analytics

The [integrator registry](./registry.md) publishes counts and aggregates:

```text
2,481,392 unique players
```

Never a list of who they are. Metrics are derived from protocol events, but the
events themselves are subject to the same scopes when read individually — a
public transparency log ([settlement](./settlement.md)) is public, which is
exactly why what goes *into* it is limited to promised-durable facts and never
includes presence, credentials, or anything an identity did not choose to make
durable.

An aggregate small enough stops being anonymous — a count of 1 identifies a
specific person as surely as a name would (issue #96). Every registry metric
below a configurable minimum cohort size (`AVALON_REGISTRY_MIN_COHORT`,
default 5) is coarsened to the floor itself and marked inexact rather than
returned as the real sub-floor count; zero is never coarsened, since
"nobody" identifies no one. Enforced once, centrally, in
`avalon_indexer::registry` — see
[registry.md](./registry.md#privacy).

## Erasure vs. permanence (issue #614, decided)

Visibility scoping (above) answers *who can see* a fact. It does not
answer whether a fact can ever be made to stop existing — a separate
question this section answers honestly rather than by implication.

**Ledger-authored facts — identity, friends, guild membership, achievement
issuance — cannot be erased, by construction, not by oversight.** This
isn't a missing feature; it's the direct consequence of two things
already decided elsewhere: the settlement log is deliberately append-only
and hash-chained (`docs/architecture/nodes.md`'s "Mirrors, not
federation," ADR #70/#79), and a hot-tier node's payload pruning
(`crates/chain/src/retention.rs`, #180/#208) is only ever allowed once an
archive-tier node has *independently confirmed* it holds a durable copy
(#569) — a mechanism that exists specifically to guarantee nothing is
ever lost, which is the opposite of an erasure guarantee. Payload pruning
also never touches the entry's metadata (issuer, subject, kind,
timestamp) on any tier, ever — even a fully pruned entry still says who
did what to whom and when, forever, everywhere.

The real, practical lever for "I want this identity to go dark" is
**pseudonymization**: revoke every signing key, author no further events,
and let the identity's existing history remain exactly as durable and
attributable as it already was. This is the same shape #80/ADR #76
already established for issuer keys and achievement revocation — never
rewrite history, change what's currently trusted going forward.

**Chat content is a genuinely different, already-solved case.** Guild
channel and DM message bodies were deliberately built to never touch the
settlement ledger at all (`crates/server/tests/conversations_no_ledger.rs`
enforces this by construction) — they live in ordinary
`guild_messages`/`conversation_messages` tables (plus an archive tier,
[communication.md](./communication.md)) with their own real deletion
path. Erasing message content is realistic and already works today; it
was simply never part of the durable-history promise to begin with.

## Today in the repo

- `crates/protocol/src/permissions.rs` (#87) — `Visibility`: `Public`,
  `AuthenticatedOnly`, `Friends`, `GuildMembers`, `Private`. A fixed, closed
  vocabulary (unlike `Capability`, which has an `Other(String)` escape
  hatch) — the ticket's own "recommendation... finalize in the ticket, not
  by accident" directive. `Game(GameId)`/`operator-only` from this
  document's own scope table aren't modeled as `Visibility` variants: an
  integrator's read access is entirely the existing capability-grant
  mechanism (`Capability`/`PermissionGrant`, orthogonal to `Visibility` —
  see "Composition" above), and no resource needing an operator-only scope
  exists yet.
- `crates/server/src/visibility.rs` (#87) — `is_visible(state, visibility,
  viewer, subject, guild_id)` is the one shared authorization helper this
  document's "Composition" section describes: the subject always sees
  their own data; otherwise `Public`/`AuthenticatedOnly` are static,
  `Friends` checks `friendships`, `GuildMembers` checks `guild_members`.
  `parse_visibility` never hard-fails a read over an unrecognized stored
  string — falls back to `Public`, same posture `guilds.rs`'s
  `JoinPolicy::parse` already takes for the same class of problem.
- **Presence** (`crates/server/src/presence.rs`, #16/#87): `GET /presence`/
  `GET /ws/presence` are now gated by each subject's own
  `profiles.presence_visibility` (default `friends` — the exact same
  behavior this endpoint always had, now a real, player-changeable setting
  via `PATCH /me` rather than a hardcoded rule). A block always wins
  regardless of setting (#97, checked before the setting is even
  consulted).
- **Guild rosters** (`crates/server/src/guilds.rs::list_members`, #87):
  previously any authenticated session could read any guild's full roster,
  entirely unscoped. Now gated by that guild's own `guilds.roster_visibility`
  (default `guild_members`, settable via `PATCH /guilds/{id}`, `manage_guild`-
  gated same as every other guild setting) — `public`, `guild_members`, or
  `private`. The guild owner always sees it regardless of setting, the same
  "gates outside exposure, never locks the guild out of its own view"
  exception `game_breakdown_public` already established for a different
  guild-level toggle.
- **Deliberately not touched this pass**: profile fields already have their
  own established field-level exposure system (#403) rather than a
  `Visibility` setting; there is no endpoint today that lets one identity
  read another's *friends list* at all, so there's nothing to gate there
  yet; achievement history's per-claim hide/feature control is a separate,
  larger feature. Extending `is_visible` to these as their own read paths
  need it is the intended shape going forward — see
  `crates/server/src/visibility.rs`'s own module doc comment.
- `crates/server/tests/visibility.rs` — a live matrix test (self/friend/
  stranger × public/friends/private for presence; owner/member/outsider ×
  public/guild_members/private for guild rosters).
- `crates/server/src/blocks.rs` (#97) — a block is visible only to the
  identity that created it; no endpoint, anywhere in this crate, reveals to
  the blocked party that they've been blocked. A blocked pair's friend
  request is rejected identically to a request naming a nonexistent
  identity, and a blocked identity's presence reads as `Offline`,
  indistinguishable from a genuinely missing entry — never a
  distinguishable "hidden" error code or presence state.
- The ledger (`crates/server/db/migrations/0002_ledger`) is readable by anyone
  with database access; the `identity.created` payload carries only
  `identity_id`/`display_name` (issue #510: the globally-unique handle
  itself) — no `username` or other login credential — since
  [#73](https://github.com/LunarVagabond/avalon-protocol/issues/73)
  and [#86](https://github.com/LunarVagabond/avalon-protocol/issues/86).
- `crates/server/src/authz.rs` (#28) implements the write-side half of the
  "active binding → active `PermissionGrant`" chain this section describes —
  `require_capability(caller, capability, state)` for a `Caller::Integrator`. Its
  first real caller is `crates/server/src/presence.rs`'s
  `update_integrator_presence` (#16), gating `presence.publish`; every other
  integrator-calling-the-API endpoint is still hypothetical (see the module's
  own doc comment) and should reuse this rather than hand-rolling a check.
- `presence_preferences.hide_playing` (`crates/server/db/migrations/0017_presence_preferences`)
  is an identity-controlled setting that's *not* a visibility scope at all —
  it removes `playing` from view entirely, independent of who's asking or
  what they're otherwise allowed to see. Set via `PUT /me/presence`.
- `discovery_preferences.discoverable` (`crates/server/db/migrations/0026_discovery_preferences`,
  issue #205) is the same kind of setting, applied to a different surface:
  off by default for every identity, no exceptions, it gates whether the
  identity can be found at all via `GET /identities/search` — not a
  visibility scope on an already-locatable identity's fields, but whether
  the identity is locatable by open search in the first place. Set via
  `PATCH /me`. Turning it off removes the identity from every subsequent
  search call immediately (the check reads the live preference on every
  call, never a cached/snapshotted value) — no grace period, matching
  #129's decision. `GET /me` echoes the current value back so an identity that
  flipped it on to test something has a standing "you are currently
  publicly searchable" signal, not just a fire-and-forget toggle.

## Decisions and tickets

- [#87](https://github.com/LunarVagabond/avalon-protocol/issues/87) — visibility
  scopes for profile, presence, friends, and guild membership.
- [#78](https://github.com/LunarVagabond/avalon-protocol/issues/78) — ADR:
  presence is ephemeral and permissioned.
- [#74](https://github.com/LunarVagabond/avalon-protocol/issues/74) — ADR: guilds
  are network primitives; roster visibility is guild policy.
- [#89](https://github.com/LunarVagabond/avalon-protocol/issues/89) — registry:
  aggregates only.
- [#28](https://github.com/LunarVagabond/avalon-protocol/issues/28) — permission
  enforcement middleware (the write-side counterpart).
- Open in [Proposal §32](../stakeholders/Proposal.md#32-open-questions): how much social
  information should be portable; cross-integrator blocking.
- [#614](https://github.com/LunarVagabond/avalon-protocol/issues/614) —
  decided: ledger-authored facts cannot be erased, by construction;
  pseudonymization (key revocation) is the real lever, chat content
  already has genuine erasure via its own separate, non-ledger lifecycle.
  See "Erasure vs. permanence" above.
