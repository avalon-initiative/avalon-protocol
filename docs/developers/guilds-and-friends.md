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

`presence_of` performs **no visibility filtering yet** (a documented gap,
[`../architecture/privacy.md`](../architecture/privacy.md)/issue #87) —
any valid session can look up presence for any identity id it names. Don't
build a feature that depends on this being scoped; it isn't yet.

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

## Visibility scoping is a known, tracked gap

None of the reads above (`presence_of`, guild roster/channels/messages) are
scoped to what the caller is actually allowed to see yet — see issue #87.
They return exactly what the server returns. Don't build a
production-facing feature that assumes otherwise; this page will be updated
once #87 lands.
