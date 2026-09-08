# Why Avalon

This document is heavily inspired by *Ready Player One*.

Not the OASIS itself. Not one universal virtual world, and not one universal avatar.
The part worth taking seriously is smaller and more specific:

> **Your identity does not belong to any single world you visit.**

That idea is now old enough to be a cliché in fiction and young enough to still be
unbuilt in practice. We have the tools to build it. Nobody has.

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

> Your character in one game can remain unique to that game. Your character in
> another game can be completely different. But the player behind them remains
> the same — and for the first time, that fact is something the games themselves,
> not just one platform, can actually recognize.
