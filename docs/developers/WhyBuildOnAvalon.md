# Why Build Your Game on Avalon

This is the pitch for game developers specifically — what Avalon does for you if
you connect your game to it, as distinct from [`../WhyAvalon.md`](../WhyAvalon.md)
(why the protocol needs to exist at all) and
[`../stakeholders/Proposal.md`](../stakeholders/Proposal.md) (the full design).

**This describes the target, not today's build.** Avalon is early — identity/auth
is real and working end-to-end (see the root `README.md` and
[`../architecture/overview.md`](../architecture/overview.md) for current status);
guilds, achievements, presence, communication, and discovery are still
protocol design and scaffolding. Treat everything below as the destination the
architecture is being built toward, not a feature list you can integrate against
this week. Once `crates/sdk` and `bindings/csharp/AvalonSdk` have real
implementations, this document is what they're for.

## The short version

Avalon is an open network for games that gives developers shared user
identity, communities, communication, presence, achievements, discovery, and
interoperability — without requiring every studio to build and maintain all of
that infrastructure itself.

Your game remains your game. You own your gameplay, characters, world,
progression, economy, servers, and business model. Avalon handles the
connective tissue between your game and the wider network.

Build your game. Let Avalon handle the network around it.

## 1. Stop rebuilding social infrastructure

Every multiplayer game eventually needs some version of friends, friend
requests, blocking, guilds, roles, permissions, chat, DMs, notifications,
presence, user profiles, and moderation — and then discovers it also needs
reconnect logic, realtime infrastructure, message history, cross-device state,
abuse prevention, authentication, and account linking to make any of that
actually work.

Avalon provides these as network capabilities. Instead of building "we need a
chat system," a game consumes "this is the user's Avalon guild, give me its
channels." That lets the developer spend their time on the game instead of on
infrastructure the rest of the industry has already solved a dozen times over.

## 2. Users stay connected outside the game

A guild is a network-level entity ([`../architecture/guilds.md`](../architecture/guilds.md)),
not a row in one game's database — it exists independently of whether any
particular game is running. A user can be playing Game A, browsing the Hub,
on mobile, or playing nothing at all, and still be part of the same community:

```
                    Avalon Guild
                         │
              ┌──────────┼──────────┐
              │          │          │
           Game A       Hub      Mobile
              │          │          │
              └──────────┼──────────┘
                         │
                       Chat
```

The game doesn't have to be running for the community built around it to keep
existing. That matters a lot for games built around long-term communities.

## 3. Discord becomes an integration, not a requirement

Avalon isn't trying to replace Discord — it's trying to make Discord (or any
other client) one of several places a community can be reached from, instead
of the *only* place. A user in your game and a user sitting in Discord
could, in principle, be part of the same conversation, without either of them
needing another account, another friends list, or another application open.
The community follows the user instead of the user following the app.

## 4. Less screen-swapping

A lot of multiplayer gaming is switching applications: game, Discord, browser,
wiki, back to the game. Avalon lets a game surface guild chat, friends, roster,
presence, notifications, and achievements directly in its own UI — the
developer chooses how it looks, Avalon provides the data underneath it. The
user doesn't have to leave the game to stay connected to the people they
play with.

## 5. Cross-game communities become possible

A guild's membership doesn't have to be defined by a single game's user
list. A game can, in principle, tell a user: "43 members of your guild are
currently playing this game" — because the guild is a network entity that
spans whatever games its members happen to be in, not something owned by any
one of them. That opens the door to guild events, recruitment, cross-game
challenges, and social matchmaking that a single isolated game backend can't
easily offer on its own.

## 6. Your game can bring users with it

A new multiplayer game's hardest problem is almost always "how do people find
out this exists." Avalon doesn't solve marketing, but it can give a game
access to social relationships that already exist on the network: "18 members
of your Avalon guild are playing this game" is a very different discovery
signal than "here's another game in a storefront." Users aren't discovering
a stranger's server — they're discovering that the people they already play
with are already here.

## 7. Cross-game features get easier

Avalon gives developers primitives for things that are hard when every game
is an island — cross-game achievements (a boss kill in Game A unlocking a
title in Game B, if Game B chooses to recognize it — see
[`../architecture/trust-model.md`](../architecture/trust-model.md)), durable
tournament and game-event results
([`../architecture/cross-integrator-events.md`](../architecture/cross-integrator-events.md)), and
guild challenges that span multiple games at once. None of it requires Game B
to trust Game A blindly — recognition is always the receiving game's choice.

## 8. User identity without forcing characters to be universal

Avalon does not require every game to share a character model. A user's
identity stays consistent; their characters stay entirely game-specific
([`../architecture/bindings.md`](../architecture/bindings.md)):

```
Avalon Identity
│
├── Game A — Human Warrior, Elf Mage
├── Game B — Dragon
└── Game C — Space Pilot
```

The developer keeps full control of their game's character model. Avalon
provides the identity that connects the user behind those characters, not
the characters themselves.

## 9. Portable history

Users accumulate years of history across the games they play. Avalon lets
games attest to that history — defeated a boss, won a tournament, founded a
guild, reached a milestone — as signed, verifiable claims
([`../architecture/achievements-and-attestations.md`](../architecture/achievements-and-attestations.md)).

The important distinction: **Avalon records provenance; games decide what
that provenance means to them.** Game B doesn't have to accept anything Game A
issues as meaningful, but it can independently verify that Game A actually
issued it. That's interoperability without surrendering control of your own
game's standards.

## 10. You choose your level of integration

Nothing requires a game to become deeply interconnected to use Avalon at all.
A small indie game might use identity, friends, and guilds; a larger game
might add chat, presence, and achievements; a big MMO might expose its full
game space, character schemas, and portable progression. Avalon standardizes
the network primitives — games decide which of them, if any, to expose.

## 11. You don't need to build Discord-lite

Building a multiplayer game is already hard. You shouldn't also have to build
authentication, friends, guilds, chat, presence, notifications, social
permissions, moderation, and realtime synchronization from scratch just
because users expect them. The intent is for the SDK to expose these as
protocol capabilities you consume, not infrastructure you stand up and
operate yourself — see [`../architecture/sdk.md`](../architecture/sdk.md).
Conceptually, something like:

```rust
let avalon = Avalon::connect().await?;
let guild = avalon.user().guild(guild_id).await?;
let chat = guild.chat().connect().await?;
```

where the game consumes the capability and Avalon owns the infrastructure
behind it.

## 12. Your community can outlive your game

Games shut down, go into maintenance, get sequels, or just lose users. If a
guild is a network-level entity rather than rows in that one game's database,
its existence doesn't depend on that game's server still running. The
relationship inverts: a game *participates in* a community, rather than a
community *existing only because* a particular server is up. That's a
healthier long-term relationship between a studio and the people who played
its game, and it's part of what [`../architecture/disaster-recovery.md`](../architecture/disaster-recovery.md)
and the "what survives a game's death" table in
[`../architecture/README.md`](../architecture/README.md) are designed around.

## 13. Community as a retention mechanism

A user who stops playing for three months is much more likely to come back
if their guild and friends are still there and still visibly active — "12 of
your friends are playing tonight" is an organic pull back into the game that
doesn't require the developer to advertise anything. The social graph does
some of the re-engagement work on its own.

## 14. A social distribution layer

Avalon doesn't replace storefronts, marketing, or influencers — it adds a
layer underneath them. Discoverability through guilds, friends, cross-game
activity, and shared events is a different channel than paid acquisition, and
it compounds with the size of the network rather than the size of any one
game's ad budget.

## 15. Network effects work in the developer's favor

None of this is about any single SDK call. It's about the network: ten
integrated games make Avalon marginally useful; a thousand make it
infrastructure developers actively build around. Every new game adds users,
communities, achievements, and potential cross-game experiences — and every
new participant makes the network more valuable to the ones already on it.

## 16. Integration may eventually become an expectation

Early on, Avalon is an optional integration. If the network grows, the
question flips from "why would I add this" to "why would I make users
build another isolated account, friends list, and guild system when they
already have one." The goal isn't to make Avalon mandatory through technical
lock-in — it's to make participation useful enough that users start asking
for it.

## 17. A lighter first session

An Avalon-integrated game can offer "connect your Avalon identity and join
your existing communities" instead of "create another account," "join our
Discord," or "add your friends again." That shrinks the gap between "I want
to try this game" and "I'm socially invested in this game" — a new user
shows up already connected to people, not starting from zero.

## 18. Compete on your game, not your backend

Developers should compete on gameplay, world design, art, mechanics, and
community culture — not on who built the better friends-list backend. Avalon
exists so studios can spend their differentiation budget on the parts of the
game that are actually theirs.

## 19. What Avalon does not take from you

Avalon is not asking a developer to hand over their game. You keep your
servers, your gameplay, your characters, your progression, your combat, your
items, your economy, your monetization, and your in-game moderation — and you
keep the final say over what your game recognizes from anywhere else on the
network.

```
YOUR GAME                       AVALON
├── World                       ├── Identity
├── Gameplay                    ├── Friends
├── Characters                  ├── Guilds
├── Combat                      ├── Communication
├── Progression                 ├── Presence
├── Economy                     ├── Discovery
└── Rules                       ├── Achievements / provenance
                                 └── Interoperability
```

## 20. The bigger opportunity

This isn't fundamentally about moving items between games. It's about games
being less isolated from each other in the first place. Today, every game is
effectively its own island. Avalon is the layer that lets users move
between worlds without losing the people and the history that made those
worlds worth playing in.

```
              AVALON
                 │
       ┌─────────┼─────────┐
       │         │         │
     Game A    Game B    Game C
       │         │         │
       └──── Communities ──┘
```

## The developer pitch

Build the game. Don't build the entire network around it.

Avalon gives games a shared identity, social, communication, discovery,
achievement, and interoperability layer. Connect your game once, and your
users can bring their Avalon identity, friends, guilds, communities, and
history with them — talk to friends without leaving the game, see what their
guild is doing, join conversations from the Hub or mobile, and interact with
friends playing other games, without another isolated account or friends list
just to play yours.

Your game remains independent. Your gameplay remains yours. Your characters
remain yours. Your world remains yours. Avalon connects it to everything
around it.

Don't build another island. Connect your world to the network.

### Why this matters long term

The first games integrate because Avalon saves development time. The next
integrate because Avalon gives them access to existing communities. Later
games integrate because their users expect their identity, friends, guilds,
history, and communication to follow them. At that point Avalon isn't just an
SDK — it's part of the infrastructure users expect a modern multiplayer
game to participate in.

That doesn't require Avalon to become a centralized platform. The network
stays open: one Avalon network, many games, many communities, many
independent developers, and — per [`../architecture/nodes.md`](../architecture/nodes.md)
— many independent infrastructure providers, none of them an authority over
anyone else's claims. The value comes from being connected, not from Avalon
owning the destinations.

## Getting started in C#

`bindings/csharp/AvalonSdk` (targets netstandard2.1, so it works in Unity)
has a real, building surface for friends/presence, guilds, and
conversations — see [`../architecture/sdk.md`](../architecture/sdk.md) for
the full method list and what's still a gap (e.g. `roster()`/`channels()`/
`messages()` apply no visibility scoping yet — issue #87).

```csharp
using Avalon.Sdk;

var client = new AvalonClient(new AvalonConfig("https://avalon.example", "your-integrator-key-id"));
var session = await client.AuthenticateAsync(identityToken);

// Friends and presence
var friends = await session.FriendsAsync();
await session.UpdatePresenceAsync(PresenceStatus.Online);
var presenceUpdates = await session.SubscribePresenceAsync(friends.Select(f => f.IdentityId).ToList());

// Guilds
var memberships = await session.GuildsAsync();
var guild = session.Guild(memberships[0].Guild.Id);
var roster = await guild.RosterAsync();
var general = (await guild.ChannelsAsync()).First();
await guild.Channel(general.Id).SendAsync("hello from the game");

// Conversations
var dm = await session.DmAsync(friends[0].IdentityId);
await dm.SendAsync("hey, are you online?");
```

Every method checks its own capability before making a request — a
`Session` with no grants for `friends.read`/`guilds.chat`/`messages.send`/
etc. throws `CapabilityNotGrantedException` without touching the network,
same as the Rust reference SDK.

## Where this stands today

See the root `README.md` for current build status and
[`../architecture/README.md`](../architecture/README.md) for the invariants
this is held to. For the actual integration surface once it exists, see
[`../architecture/sdk.md`](../architecture/sdk.md) and this directory's
`README.md`.

If your studio wants to run this internally instead — your own games, your
own network, not connected to the public one — that's supported, and
[`../architecture/self-hosting.md`](../architecture/self-hosting.md) covers
what that actually means. Short version: it's a fork, not a way to join, and
none of the network effects above apply to it. We'd rather have your games on
the real network. But if a private instance is genuinely what you need, the
code doesn't stop you.
