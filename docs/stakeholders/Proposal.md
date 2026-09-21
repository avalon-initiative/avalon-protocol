# Avalon Protocol — Living Proposal

**Status:** Draft / Living Document
**Date:** September 6, 2026
**Project:** Avalon Protocol
**Domain:** Open Gaming Identity & Interoperability

> **Games are experiences. Your identity, friends, guilds, achievements, and history belong to you.**

This is the narrative design document. The normative architecture reference —
invariants, authority boundaries, what the code is held to — lives in
[`architecture/`](../projects/backend-server/architecture/README.md). Decisions are recorded as GitHub
issues: [decided](https://github.com/LunarVagabond/avalon-protocol/issues?q=is%3Aissue+label%3Aarchitecture-decision-record)
and [still open](https://github.com/LunarVagabond/avalon-protocol/issues?q=is%3Aissue+label%3Adecision+is%3Aopen).

---

# Table of Contents

1. [Executive Summary](#1-executive-summary)
2. [The Problem](#2-the-problem)
3. [What Avalon Protocol Is](#3-what-avalon-protocol-is)
4. [What Avalon Protocol Is Not](#4-what-avalon-protocol-is-not)
5. [The Architecture](#5-the-architecture)
6. [Avalon Hub](#6-avalon-hub)
7. [Persistent Identity](#7-persistent-identity)
8. [Achievements and History](#8-achievements-and-history)
9. [Trust and Attestations](#9-trust-and-attestations)
10. [Guilds and Social Identity](#10-guilds-and-social-identity)
11. [Universal Friends](#11-universal-friends)
12. [Communication](#12-communication)
13. [Permission Model](#13-permission-model)
14. [Blockchain](#14-blockchain)
15. [Economy and Currency](#15-economy-and-currency)
16. [Portable Assets](#16-portable-assets)
17. [Developer Experience](#17-developer-experience)
18. [Game Registration](#18-game-registration)
19. [Identity vs. Game Data](#19-identity-vs-game-data)
20. [Self-Hosting and Decentralization](#20-self-hosting-and-decentralization)
21. [Reference Implementation: Rust Workspace](#21-reference-implementation-rust-workspace)
22. [Companion Apps: Presence Beyond the Game](#22-companion-apps-presence-beyond-the-game)
23. [The First Product](#23-the-first-product)
24. [Phase 2 — Developer SDK](#24-phase-2--developer-sdk)
25. [Phase 3 — External Games](#25-phase-3--external-games)
26. [Phase 4 — Portable Assets](#26-phase-4--portable-assets)
27. [Phase 5 — Economy](#27-phase-5--economy)
28. [Difference From Roblox](#28-difference-from-roblox)
29. [What Makes Avalon Interesting](#29-what-makes-avalon-interesting)
30. [Guiding Principles](#30-guiding-principles)
31. [Major Risks](#31-major-risks)
32. [Open Questions](#32-open-questions)
33. [Success Criteria](#33-success-criteria)
34. [Long-Term Vision](#34-long-term-vision)

---

# 1. Executive Summary

Avalon Protocol is an open protocol for connecting independent games through a persistent identity and social layer.

A user should not have to become a completely different person every time they enter a new game.

Their identity can persist.

Their friends can persist.

Their guilds can persist.

Their achievements and history can persist.

Their ownership can persist where supported.

But **the games themselves remain independent.**

Avalon Protocol does not attempt to create one giant MMO, one universal game, or one company-controlled metaverse.

Instead, Avalon provides the infrastructure between games.

A developer should be able to say:

> **“This game uses the Avalon Protocol to connect your User identity to our world.”**

The game remains the developer's world.

Avalon simply provides a standardized way for that world to participate in a larger gaming ecosystem.

---

# 2. The Problem

Modern games treat identity as disposable.

A person can have:

* A Steam identity
* A game-specific account
* A Discord identity
* A character in Game A
* Another character in Game B
* A guild in Game C
* Achievements scattered across dozens of services
* Purchases locked to individual games
* Friends distributed across separate platforms

When a game shuts down, much of that history disappears with it.

The fundamental question Avalon asks is:

> **What should survive the death of a particular server or game?**

The answer should not necessarily be the game itself.

It should be the **user's identity and history.**

---

# 3. What Avalon Protocol Is

Avalon Protocol is an open interoperability layer for games.

It provides standardized concepts and services for:

* Identity
* Profiles
* Friends
* Presence
* Guilds
* Guild communication
* Achievements
* Attestations
* Game registration
* Permissions
* Ownership/provenance
* Optional economic infrastructure
* Game interoperability

Games opt into the parts they want.

A game can use Avalon for identity while ignoring achievements.

Another game can use identity, guilds, and achievements.

Another can integrate ownership and portable assets.

Interoperability is **opt-in and capability-based.**

---

# 4. What Avalon Protocol Is Not

Avalon is not:

* A game
* An MMO
* A game engine
* A centralized game marketplace
* A Roblox replacement
* A mandatory launcher
* A universal character system
* A blockchain game
* A cryptocurrency project
* An NFT marketplace
* A requirement that every game share its data
* A company-controlled metaverse

Avalon does not own the worlds it connects.

---

# 5. The Architecture

Avalon consists of three conceptual layers.

## 5.1 Avalon Protocol

The protocol defines the common language between identities and games.

It defines concepts such as:

* Identity
* Games
* Guilds
* Achievements
* Attestations
* Permissions
* Assets
* Social relationships

The protocol should remain independent from any specific game engine, UI framework, database, or blockchain.

---

## 5.2 Avalon Network

The network provides implementations of the protocol.

A network implementation can provide:

* Identity services
* Authentication
* Social services
* Guild services
* Messaging
* Achievement verification
* Game registration
* Attestation verification

The reference implementation should be open source.

Self-hosting should be possible.

Avalon should ultimately be capable of supporting multiple compatible network implementations rather than requiring one centralized service.

---

## 5.3 Games

Games remain sovereign.

A game controls:

* Its world
* Its characters
* Its gameplay
* Its progression
* Its economy
* Its rules
* Its servers
* Its content

A game decides what Avalon data it wants to recognize.

For example:

> Game A grants a user the `Dragon Slayer` achievement.

Game B can choose to recognize that achievement.

Game C can ignore it.

Neither game needs to surrender control of its own progression system.

---

# 6. Avalon Hub

Avalon should have a **Hub**, but the Hub is not the product itself.

It is the first convenient client for the protocol.

The Hub provides a user's front door into the Avalon ecosystem.

A user could use it to:

* Create their Avalon identity
* Manage their profile
* View friends
* View guilds
* Chat with guild members
* View achievements
* Discover Avalon-enabled games
* Manage connected games
* Manage permissions

The Hub should eventually become optional.

A user might interact with Avalon through:

* A desktop Hub
* A web application
* A mobile application
* A game client
* Discord integrations
* Third-party applications

The protocol matters more than any particular client.

---

# 7. Persistent Identity

Avalon must distinguish between **Identity** and **Game Characters.**

This is fundamental: the persistent identity described here is what "Avalon identity" means everywhere else in this document, not any single game's character.

A person has one Avalon identity.

That identity may have:

* Character A in Game A
* Character B in Game B
* Character C in Game C

Those characters do not need to be identical.

The identity is the persistent layer.

The character belongs to the game.

Therefore:

> **Avalon does not create one universal character.**

It creates a persistent identity capable of participating in many worlds.

---

# 8. Achievements and History

One of Avalon's most important concepts is persistent history.

A game can issue an achievement or historical attestation to an identity.

Examples:

* Defeated the Dragon Lord
* Completed a Hardcore Realm
* Reached level 100
* Founded a guild
* Won a tournament
* Completed a difficult raid
* Participated in a world-first event

Avalon records that an issuer has made a claim about an identity.

It does **not** dictate what another game must do with that claim.

For example:

Game A:

> User defeated the Dragon Lord.

Game B may interpret that as:

> Unlock the Dragon Slayer title.

Game C may interpret it as:

> Unlock a special quest.

Game D may ignore it completely.

The network records the history.

The receiving game determines its meaning.

---

# 9. Trust and Attestations

Achievements should not simply be arbitrary API values.

They should be verifiable claims.

An attestation can establish:

* Issuer
* User
* Achievement
* Timestamp
* Relevant metadata
* Validity
* Revocation status

The receiving game can determine whether it trusts the issuer.

This creates a foundation for portable reputation without requiring every game to blindly trust every other game.

For example:

> Avalon recognizes that `Game A` issued `Dragon Slayer` to User X.

Game B decides:

> I trust Game A and recognize this achievement.

This allows interoperability without centralizing authority over game progression.

---

# 10. Guilds and Social Identity

Guilds are potentially more important than portable assets.

A guild should be capable of existing independently of any single game.

A guild can have:

* Name
* Tag
* Description
* Members
* Roles
* Permissions
* Channels
* History
* Reputation
* Participating games

A guild could exist before its members ever enter a particular game.

For example:

> **The Round Table**

could be an Avalon guild.

Its members might play:

* Game A
* Game B
* Game C

The guild remains the same social organization.

Games can optionally expose that guild inside their own interfaces.

---

# 11. Universal Friends

The same principle applies to friendships.

An identity should be able to maintain a persistent friends list across games.

That identity could see:

> Alice — Online — Playing Game A

> Bob — Offline

> Charlie — Online — Playing Game C

A game does not automatically gain access to someone's entire social graph.

Permissions determine what is exposed.

---

# 12. Communication

Communication belongs to the network layer.

Guild chat should not need to disappear simply because an identity leaves a game.

For example:

User A is playing Game A.

User B is playing Game B.

Both belong to the same Avalon guild.

They can still communicate through the guild's network channel.

This creates a powerful separation:

> **The social layer belongs to the network.
> Gameplay belongs to the game.**

---

# 13. Permission Model

Avalon must use explicit capabilities.

A game should never automatically receive access to everything associated with an identity.

Possible capabilities include:

```text
identity.read
profile.read
friends.read
presence.read
presence.publish
guilds.read
guilds.chat
guilds.issue
achievements.read
achievements.issue
assets.read
assets.issue
wallet.read
wallet.write
```

A game requesting access to an identity's guild membership should not automatically receive:

* Every friend
* Every private message
* Every game the identity has played
* Wallet balances
* Private profile information
* Every achievement
* Every owned asset

The principle is:

> **Least privilege by default.**

---

# 14. Blockchain

Blockchain is an optional infrastructure layer.

It should not be the foundation of real-time gameplay.

The following should remain off-chain:

* HP
* XP
* Combat
* AI
* NPC state
* World simulation
* Chat
* Real-time movement
* Matchmaking
* Server state

Traditional servers and databases are vastly better suited for these problems.

Blockchain may eventually be useful for:

* Ownership
* Provenance
* Transfers
* Durable asset records
* Cross-network settlement
* Certain forms of identity
* Public attestations

Avalon should not require blockchain adoption in order to provide value.

When a durable settlement layer exists, it takes the shape of a public, append-only, cryptographically verifiable log — not a network of servers each deciding whether to trust the others, and not a mining-based consensus network.

Federation solves the wrong problem here. It makes visibility conditional on which server a game happens to trust, which quietly recreates the walled gardens Avalon exists to avoid.

Mining-based consensus also solves the wrong problem. It exists to let mutually distrusting parties agree on who writes next when they're contesting something scarce. Avalon's writes are already unambiguous — a game signs its own achievement issuance, an identity signs its own claim. There is no scarce resource being contested.

What Avalon actually needs is simpler than either: anyone can verify an entry without asking permission, and anyone can mirror the log without being trusted first. That is a transparency log, not a blockchain in the currency sense.

What that log eventually anchors to, or becomes — a log whose checkpoints are published to an existing public chain, a partnership with a specific chain, or a custom Avalon chain — is deliberately still open ([#79](https://github.com/LunarVagabond/avalon-protocol/issues/79)), as is the log's own technical design ([#40](https://github.com/LunarVagabond/avalon-protocol/issues/40)). Two things hold under every outcome: one protocol event is never one chain transaction (events are batched behind a commitment), and real-time gameplay never touches settlement.

---

# 15. Economy and Currency

A universal cryptocurrency should **not** be a foundational requirement.

There are significant risks:

* Speculation
* Volatility
* Whales
* Manipulation
* Regulatory complexity
* Tax implications
* Economic disruption
* Games becoming financial products
* Children interacting with financial systems

Avalon should initially abstract economic functionality.

A developer should eventually be able to write something conceptually similar to:

```rust
store.purchase(identity, "premium_mount")
```

without needing to understand:

* Wallet infrastructure
* Smart contracts
* Blockchain transactions
* Cryptographic signing
* Settlement

Economic infrastructure can be introduced later if it provides genuine utility.

---

# 16. Portable Assets

Portable assets are more complicated than portable identity.

Ownership and functionality should be separated.

A user might own:

> Sword of Aether

Game A may use it as a powerful weapon.

Game B may recognize it only as a cosmetic.

Game C may not recognize it at all.

Therefore:

> **Ownership can be portable without functionality being universal.**

Avalon should provide the infrastructure for proving ownership and provenance.

Games decide what those assets mean inside their worlds.

---

# 17. Developer Experience

Avalon succeeds or fails based on developer experience.

A developer should not need to become a cryptographer or blockchain engineer.

The ideal experience should look something like:

```rust
let identity = avalon.authenticate().await?;

if identity.has_achievement("dragon_slayer") {
    unlock_title("Dragon Slayer");
}
```

Or:

```rust
identity.issue_achievement("dragon_slayer").await?;
```

Under the hood, that call produces a signed attestation on the chain.

The developer never touches signing, issuer trust, or the ledger directly.

The complexity should live inside the SDK and protocol implementation.

The developer should interact with simple domain concepts.

---

# 18. Game Registration

Games register with Avalon.

A registration could contain:

```yaml
game:
  id: ashen-realms
  name: Ashen Realms
  developer: example-studios

capabilities:
  identity: true
  friends: true
  guilds: true
  achievements: true
  assets: false
  wallet: false
```

This allows users to understand what a game is requesting before connecting their identity.

---

# 19. Identity vs. Game Data

Avalon must maintain a strict boundary.

### Avalon owns the protocol-level concepts:

* Identity
* Social relationships
* Guild membership
* Network achievements
* Attestations
* Permissions

### Games own:

* Characters
* Inventory
* Quests
* Combat
* Levels
* World state
* Game-specific progression
* Game-specific purchases

A game should never be forced to expose internal game data merely to participate in Avalon.

---

# 20. Self-Hosting and Decentralization

Avalon should be open source.

The long-term goal should be an ecosystem where organizations and communities can operate compatible Avalon infrastructure.

Possible operators include:

* Communities
* Game studios
* Open-source projects
* Hosting providers
* Gaming organizations

Multiple operators does not mean federation in the walled-garden sense — one server deciding whether to trust another. It means multiple organizations mirroring and serving reads from the same public, verifiable record, the way multiple parties can mirror a Certificate Transparency log today.

The initial implementation does not need multiple mirrors.

The architecture should simply avoid making that impossible later.

Do not claim decentralization before it actually exists.

---

# 21. Reference Implementation: Rust Workspace

The reference implementation is Rust.

Rust is already the language of choice for high-performance game servers and netcode.

It gives strong guarantees around the identity and permission types the protocol depends on.

It produces a single reference binary that is easy to self-host.

The reference implementation is a Cargo workspace of six crates under `crates/`
(confirmed as the intended shape in [#69](https://github.com/LunarVagabond/avalon-protocol/issues/69)):

* **`avalon-protocol`** — the protocol itself. Pure types and traits: identity, game bindings, attestations, capabilities, guilds, events. No I/O, no storage, no network calls. New domains become modules here, not new crates.
* **`avalon-chain`** — the settlement boundary (`SettlementProvider`) and its milestone-1 implementation: a hash-chained, append-only ledger. It does not need to be a blockchain today. It needs to behave like a verifiable log where it matters: tamper-evident, independently verifiable, mirrorable. See [architecture/settlement.md](../projects/backend-server/architecture/settlement.md).
* **`avalon-indexer`** — the query layer. Consumes durable protocol events and maintains read models that can be rebuilt from history at any time. Kept separate from `avalon-chain` on purpose: settlement is not querying.
* **`avalon-server`** — the network-facing service: identity, auth, social graph, guilds, presence, game registration, achievement verification. The one backend every client (the Hub, the mobile Hub, games via the SDK) talks to, and the thing a self-hosted operator actually runs.
* **`avalon-sdk`** — the client library developers depend on. A game never depends on `avalon-server` or `avalon-chain` directly.
* **`avalon-cli`** — local dev and operator tooling (the `avalon` binary): ledger inspection, game registration, diagnostics.

Non-Rust SDKs and third-party network implementations only need `avalon-protocol`.

They should never be forced to pull in a full chain, indexer, or server implementation just to speak the protocol.

---

# 22. Companion Apps: Presence Beyond the Game

Guilds and friends are already cross-game concepts.

That creates an obvious pain point: people will want to talk to their guild without a game open.

Discord already proved the demand for this. Avalon should get ahead of it, not bolt it on later.

This is not a new pillar.

Presence, guild channels, and friends already live at the network layer, not the game layer. That is what makes this possible.

A mobile app and a web app are simply additional clients of the Hub — the same identity, login, and session model underneath, just a different shell around it. The device itself only ever holds one secret worth protecting: the session token, kept in whatever secure storage that platform actually offers rather than plain browser storage.

A companion app could offer:

* Guild chat and DMs, independent of which game (if any) a member is playing
* Presence — who is online, and where
* An achievement feed as history accumulates
* Guild and friend management on the go

This is a client. Not a new layer.

It uses the same permissioned capabilities as any other Avalon client.

A user who never installs it loses nothing.

---

# 23. The First Product

The first product should **not** be a giant game.

It should be the network foundation.

The first milestone should prove:

1. User A creates an Avalon identity.
2. User B creates an Avalon identity.
3. They become friends.
4. User A creates a guild.
5. User B joins.
6. The guild creates a channel.
7. They communicate.
8. Game A registers with Avalon.
9. User A authenticates through Game A.
10. Game A issues `Dragon Slayer`.
11. Game B registers.
12. Game B verifies the achievement.
13. Game B chooses to recognize it.
14. The Avalon Hub displays the user's identity, friends, guild, and achievement.

If this works, the fundamental concept has been proven.

---

# 24. Phase 2 — Developer SDK

Rust is the native SDK.

Build bindings for other major development environments on top of that same core.

Potential targets:

* Rust (native)
* TypeScript
* C#
* C++
* HTTP API

The SDK should abstract:

* Authentication
* Identity
* Permissions
* Guilds
* Social graph
* Achievements
* Attestations
* Game registration

---

# 25. Phase 3 — External Games

Once the protocol and SDK are stable, external games can begin integrating.

The ideal integration experience is:

> **This game uses the Avalon Protocol.**

A user launches the game.

The game requests Avalon authentication.

The user approves.

The game now knows:

> This user is an Avalon identity.

Nothing more is required unless the game requests additional capabilities.

---

# 26. Phase 4 — Portable Assets

Once identity and social interoperability are mature, begin exploring:

* Asset ownership
* Provenance
* Portable cosmetics
* Cross-game recognition
* Item attestations
* Asset registries

Avoid assuming that every asset must work in every game.

---

# 27. Phase 5 — Economy

Only after the network has genuine adoption should Avalon investigate:

* Payments
* Cross-game purchases
* Settlement
* Optional digital currency
* Developer revenue sharing
* Portable economic assets

Economics should serve the ecosystem rather than define it.

---

# 28. Difference From Roblox

Avalon may initially sound similar to Roblox because both connect users with multiple games.

The architectural philosophy is fundamentally different.

Roblox is a centralized platform.

Roblox controls:

* Platform identity
* Infrastructure
* Distribution
* Economy
* Rules
* Developer ecosystem
* User experience

Avalon does not attempt to own the games.

An Avalon game can be:

* Commercial
* Free
* Open source
* Private
* Community-run
* Self-hosted
* Independent

Avalon provides the connective infrastructure.

The destination remains independent.

> **You're building the railroad between games, not trying to own every destination.**

---

# 29. What Makes Avalon Interesting

Avalon combines several ideas that normally exist separately:

### Persistent identity

You remain the same person across worlds.

### Persistent history

Achievements and reputation can survive individual games.

### Persistent communities

Guilds and friendships can exist independently of a specific game.

### Independent worlds

Games maintain sovereignty.

### Selective interoperability

Games choose what they recognize.

### Open infrastructure

The protocol can be implemented and operated independently.

This produces something closer to an **open gaming ecosystem** than a traditional gaming platform.

The appeal was never one universal avatar.

It is the idea that your **digital identity exists independently of any single experience.**

Your character in one game can remain unique to that game.

Your character in another game can be completely different.

But you remain the same behind them.

Your friends, guilds, history, achievements, reputation, and selected ownership travel with you.

---

# 30. Guiding Principles

### 1. Games remain sovereign.

Avalon connects games. It does not control them.

### 2. Identity belongs to you.

You should not lose your Avalon identity because a game disappears.

### 3. Interoperability is opt-in.

No game should be forced to support anything it does not want.

### 4. Least privilege.

Games receive only the capabilities they need.

### 5. History is portable.

Achievements and attestations can survive individual games.

### 6. Functionality is contextual.

An asset does not need identical functionality everywhere.

### 7. Blockchain is optional. Federation is not the model.

Durable settlement, when it exists, is a public log anyone can verify and mirror without permission — not servers that whitelist each other, and not consensus built to referee a scarcity problem Avalon doesn't have.

### 8. Open source first.

The protocol should not depend on a proprietary implementation.

### 9. Developer experience matters.

Integration should be simple.

### 10. Don't build the universe.

Build the infrastructure that lets others build worlds.

---

# 31. Major Risks

Avalon has significant technical and social challenges.

### Identity

Account recovery and identity security are difficult.

### Trust

Games need reliable mechanisms for deciding which issuers to trust.

### Privacy

A portable identity creates the potential for excessive surveillance.

### Abuse

Persistent identities can make harassment persistent.

### Interoperability

Different games have fundamentally different concepts of progression and ownership.

### Economics

Cross-game financial systems can introduce serious regulatory and economic problems.

### Centralization

A single Avalon operator could eventually become a de facto gatekeeper.

### Adoption

The network has little value until independent games actually use it.

The architecture should explicitly acknowledge these risks rather than pretending they don't exist.

---

# 32. Open Questions

Several decisions should remain unresolved until the protocol develops further.
Anything with an issue number is being tracked as an open `decision`; the rest
have no owner yet.

* What exactly constitutes an Avalon identity, and is it a self-custodied keypair? — [#73](https://github.com/LunarVagabond/avalon-protocol/issues/73)
* How should identity recovery work?
* Can identities be transferred?
* What does account ownership mean?
* How do issuer signing keys get registered, rotated, and revoked? — [#80](https://github.com/LunarVagabond/avalon-protocol/issues/80)
* How exactly is an attestation revoked, superseded, or reinstated? — [#81](https://github.com/LunarVagabond/avalon-protocol/issues/81)
* How should guild ownership work?
* How should guild leadership transfer?
* How should harassment and blocking work across games?
* How much social information should be portable?
* What is the technical design of the transparency log? — [#40](https://github.com/LunarVagabond/avalon-protocol/issues/40)
* What does the log anchor to, or become, long term: an anchored log, a partnered chain, or a custom chain? — [#79](https://github.com/LunarVagabond/avalon-protocol/issues/79)
* How does the log operator's own signing key work? — [#39](https://github.com/LunarVagabond/avalon-protocol/issues/39)
* Should assets, and game data more broadly, have standardized schemas? — [#181](https://github.com/LunarVagabond/avalon-protocol/issues/181)
* How should developer authentication work?
* How should network operators be trusted?
* How should economic transactions work?

Answered since this list was first written:

* *Should the protocol support federation?* — No. Settlement is a public log anyone can verify and mirror; federation was rejected in [#70](https://github.com/LunarVagabond/avalon-protocol/issues/70).
* *How should trust relationships work?* — Authenticity, validity, and recognition are separate; consumers choose what they recognize. [#76](https://github.com/LunarVagabond/avalon-protocol/issues/76).
* *Should attestations be revocable without erasing history?* — Yes, always; revocation is appended history. [#75](https://github.com/LunarVagabond/avalon-protocol/issues/75).

These should be solved incrementally.

---

# 33. Success Criteria

Avalon is successful if a developer can genuinely say:

> **“This game uses the Avalon Protocol to connect your User identity to our world.”**

And the statement means something useful.

A user should be able to:

1. Create one identity.
2. Join multiple games.
3. Maintain one social graph.
4. Maintain guild membership.
5. Communicate across games.
6. Accumulate verifiable history.
7. Choose what information games receive.
8. Carry selected forms of ownership across experiences.

Meanwhile developers should retain control over their games.

---

# 34. Long-Term Vision

Imagine installing a new MMO.

Instead of creating:

> Username
> Password
> Character
> Friends
> Guild

the game says:

> **Connect with Avalon Protocol**

You authenticate.

The game can now recognize you as an existing user.

It might say:

> Welcome back.

Your friends are already there.

Your guild is already there.

Your history exists.

Maybe the game recognizes that you defeated the Dragon Lord somewhere else.

Maybe it gives you a title.

Maybe it doesn't.

That's its choice.

You enter the world and create a new character.

That character belongs to the game.

But **you** remain you.

Avalon Protocol should not attempt to build the next giant MMO.

It should build something more fundamental.

An open layer that allows independent games to recognize the same users, communities, history, and eventually ownership without surrendering control of their worlds.

Not one giant game.

Not one company-owned metaverse.

Not a cryptocurrency looking for a reason to exist.

Not a collection of NFTs.

An open protocol connecting games and the people who play them.

> **Games are experiences.**
>
> **Your identity is persistent.**
>
> **Your communities are persistent.**
>
> **Your history can persist.**
>
> **The worlds remain yours to explore.**

**Avalon Protocol**

*The open layer between games.*
