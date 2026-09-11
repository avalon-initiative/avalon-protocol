# Why Avalon

This document is heavily inspired by *Ready Player One*.

Not the OASIS itself. Not one universal virtual world, and not one universal avatar.
The part worth taking seriously is smaller and more specific:

> **Your identity does not belong to any single world you visit.**

That idea is now old enough to be a cliché in fiction and young enough to still be
unbuilt in practice. We have the tools to build it. Nobody has.

## The internet was supposed to do this already

The internet was built as a network to connect people to each other — that
was the entire premise of TCP/IP, before a single website or app existed.
Somewhere between "any computer can talk to any other computer" and today,
identity and social connection never became protocol-level concepts the way
addressing and routing did. Nobody standardized "who you are" or "who you
know" the way everybody standardized "how a packet finds its destination."
So every app, every platform, every game built its own private, incompatible
answer to both questions on top of a network that was never given an open
one — and the thing built to connect everyone ended up full of applications
that each make you reconnect from scratch.

That's not an abstract gap. It's the ordinary experience of using the
internet: a different login for every service, a friends list that means
nothing outside the app it lives in, a history and reputation you built by
hand that evaporates the moment you leave. Avalon doesn't try to fix the
whole internet — that's a much bigger claim than this project makes. It
tries to prove, for one domain, what an open identity and social layer looks
like when it's built as infrastructure instead of a product feature: the
same identity, the same friends, the same earned history, portable across
everything that chooses to plug into it — on the network of people the
internet was supposed to be providing all along.

## What already exists

It would be dishonest to pretend this space is empty. Pieces of this idea exist,
scattered across companies that each built the piece useful to them:

- **Platform accounts** (Steam, Xbox Live, PlayStation Network, Epic Online
  Services) give you one identity, friends, and presence across every game on
  *that* platform. Cross-platform, they don't exist to each other.
- **Achievement systems** (Gamerscore, PlayStation Trophies, Steam achievements)
  give you portable bragging rights — within one platform's games, verified by
  one company, meaningless outside it.
- **Avatar portability** (Ready Player Me, named for the same book this document
  is) gives you one 3D avatar usable across many games — a real and useful idea,
  but about how you *look*, not who you are, what you've done, or who you know.
- **In-game guilds and clans** (every MMO ever made) give you a real community —
  that dies the day the game's servers go dark.
- **Chat platforms** (Discord) give you persistent communities that survive any
  one game's lifespan — but they know nothing about your identity, achievements,
  or trust relationships *inside* those games.
- **Blockchain gaming identity projects** have tried to solve portability with
  tokens and wallets, and mostly ended up building speculative assets before they
  built anything a player actually wanted.

Every one of these is a real piece of the puzzle. None of them is the puzzle.

## The gap

Look at the list above again. Every entry is either:

1. **Platform-locked** — works only across games one company controls, and
2. **Character- or avatar-shaped** — portable appearance, not portable identity, or
3. **Proprietary** — closed, unextendable, and not something an independent
   studio can self-host or build against without that platform's permission.

None of them separate **the player** from **the character**. Xbox Live doesn't
care about your character — it cares about your Xbox account, which is a
platform identity, not a player identity, and it only works inside Microsoft's
walled garden. A guild in an MMO isn't a network-level entity — it's rows in that
one game's database, gone when the game is. Achievements don't carry trust — they
carry a badge image, verified by nobody but the platform that issued it, and
unrecognized the moment you leave that platform.

The specific thing nobody has built is an **open, self-hostable protocol** where:

- identity is a first-class concept, independent of any character, any platform,
  and any single company's servers,
- friends and guilds are network-level entities that outlive any one game,
- achievements are verifiable claims a receiving game can trust or ignore on its
  own terms, not entries in someone else's proprietary database,
- and any independent developer can plug their game into this without asking a
  platform holder's permission first.

That's the gap. Not "nobody has thought of cross-game identity" — clearly, many
people have, repeatedly, for two decades. Nobody has built the *open* version:
the one that isn't owned by the platform you happened to buy the game on.

## What Avalon actually claims

Avalon does not claim to be the first project to imagine persistent identity
across games. It claims to be building the open, protocol-level infrastructure
for it — the piece every platform-locked and character-locked attempt so far has
skipped, because none of them had a reason to make their version of this work for
someone else's game too.

"Open" here means something specific: a player's identity is a fact any
integrated game can read, not a fact that depends on which server a game
happens to trust. That rules out federation (separate servers deciding whether
to recognize each other) as much as it rules out one company's walled garden —
both make identity conditional on a relationship between servers instead of a
property of the player.

See [`stakeholders/Proposal.md`](stakeholders/Proposal.md) for what that actually looks like in practice —
identity separate from characters, guilds and friends as network entities,
achievements as attestations, and games that stay fully sovereign while opting in
to whichever of it they want.

## What this is not

Worth saying plainly, because the shape invites the comparison and a lot of
people (reasonably) don't want this: **this is not a government or platform
digital ID system, and it is not on a path to becoming one.** An Avalon
identity identifies a keypair, not a person — there is no name, government
ID, biometric, or any other real-world identifier anywhere in it, by
construction, not by policy. See
[`architecture/identity.md`](architecture/identity.md#what-identity-is-not)
for the technical detail. The heavy commitment to decentralization
throughout this protocol — no validator set, no platform owning identity, no
single operator anyone could compel — is a large part of *why*: not one
party in the system ever holds, or could be made to produce, a mapping from
a keypair back to a real human, because that mapping is never collected in
the first place. What's portable here is online activity — who you've
played with, what you've earned, which communities you belong to — never
personhood.

> Your character in one game can remain unique to that game. Your character in
> another game can be completely different. But the player behind them remains
> the same — and for the first time, that fact is something the games themselves,
> not just one platform, can actually recognize.
