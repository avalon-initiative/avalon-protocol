# Guilds and Friends

Reading a player's social graph and guild membership, with presence
embedded when granted. For the underlying model (what a guild is, roles,
channels, the social graph's own invariants), see
[`../architecture/social-graph.md`](../architecture/social-graph.md) and
[`../architecture/guilds.md`](../architecture/guilds.md).

## Friends

```rust
let friends = session.friends().await?; // requires friends.read
for friend in friends {
    println!("{}: {:?}", friend.identity_id.0, friend.presence);
}
```

`friend.presence` is `Some(..)` only when `presence.read` is *also* granted
alongside `friends.read` — nothing extra to ask for, it's embedded
automatically via one batched `presence_of` call rather than one request per
friend. `friend.display_name` is always `None` today: no endpoint resolves
another identity's profile yet (a documented gap, not silently dropped).

A complete, runnable version: `crates/sdk/examples/list_friends.rs` —
`cargo run -p avalon-sdk --example list_friends`.

## Presence

```rust
let mine = session.presence().await?;               // requires presence.read
let mine_and_others = session.presence_of(&[id1, id2]).await?;
session.update_presence(PresenceStatus::Away).await?; // no capability required — you publish your own
```

`presence_of` is filtered server-side by each subject's own presence
visibility setting (issue #87, default `friends`) — a caller only sees a
real status back for an identity that's currently friends with them (or
themselves), public, or authenticated-only; everyone else reads as
`Offline`, indistinguishable from a genuinely missing entry, the same
posture a block already gets (issue #97). See
[`../architecture/privacy.md`](../architecture/privacy.md) for the full
scope model — this is real and enforced, not a documented gap.

`session.subscribe_presence(&ids)` opens a live-push websocket channel,
additive to the point-in-time reads above — see the method's own rustdoc
for the exact reconnect/lifetime semantics.

## Guilds

```rust
let memberships = session.guilds().await?; // requires guilds.read
let roster = session.guild(guild_id).roster().await?;             // guilds.read
let channels = session.guild(guild_id).channels().await?;         // guilds.chat
let messages = session.guild(guild_id).channel(channel_id).messages(None, None).await?; // guilds.chat
session.guild(guild_id).channel(channel_id).send("hello").await?; // guilds.chat
```

`guilds.read` and `guilds.chat` are separate capabilities — there's no
`guilds.*` blanket grant. `roster()` embeds presence the same way
`friends()` does, gated on `presence.read`.

**The SDK never lets an integrator act with guild authority.** Creating
guilds, inviting, kicking, changing roles, and managing channels all stay
identity-authority-only actions taken through the Hub — not exposed here,
and not planned to be. `channel(id).send(body)` posts *as the identity*,
under their own session, never as your integrator.

## Conversations (direct/small-group messages)

```rust
let conversations = session.conversations().await?;              // messages.read
let handle = session.dm(other_identity_id).await?;                // messages.send
let messages = handle.messages(None, None).await?;                // messages.read
handle.send("hi").await?;                                          // messages.send
```

A rejected read/send — whether the caller was never a participant or is a
blocked one — surfaces as the same `SdkError::NotConversationParticipant`
either way, deliberately: revealing which case applied would leak that a
block exists (issue #97's "never reveal you've been blocked" rule).

## Visibility scoping: what's real, what isn't yet

Presence (`presence_of`/`presence`/`subscribe_presence`) and guild rosters
(`roster()`) are both scoped server-side now (issue #87) — see the sections
above and [`../architecture/privacy.md`](../architecture/privacy.md).
Guild `channels()`/`messages()` are **not** — any member with `guilds.chat`
sees every channel regardless of any per-channel visibility a guild might
eventually want, and there's no such setting yet. Don't build a
production-facing feature that assumes channel-level scoping exists.
