# Avalon Protocol

> **Games are experiences. Your identity, friends, guilds, achievements, and history belong to you.**

This is the narrative product overview. The normative architecture reference —
invariants, authority boundaries, what the code is held to — lives in
[`architecture/`](../projects/backend-server/architecture/README.md).

## What Avalon Is

Avalon Protocol is an open, self-hostable identity and social layer for
games, apps, and services. It gives a player one persistent identity,
friends list, guild membership, and achievement history that exist
independently of any single integrator — this project's term for any
independently operated game, app, or service that connects to Avalon — and
survive that integrator shutting down.

An identity is a self-custodied keypair, not an account any company
controls. Friends, presence, guilds, and achievements are network-level
facts, stored once and read by every integrator a player has connected,
rather than duplicated in each integrator's own database.

An integrator opts into whichever parts of Avalon it wants — identity
alone, or identity plus guilds plus achievements — capability by
capability. Avalon never owns the integrators it connects, never sees
their internal data unless they choose to publish it, and never dictates
what an integrator's world looks like or means.

## The Problem

A player's identity is disposable today: a different login for every
service, a friends list that means nothing outside the app it lives in,
achievements that vanish the moment the issuing game shuts its servers
off. Every studio rebuilds the same friends/guilds/chat/presence
infrastructure independently, and none of it survives past that one
game's lifetime.

Avalon exists to answer one question: **what should survive the death of
a particular game or server?** The answer is the player's identity and
history — not the game.

## System Structure

Three layers:

- **Protocol** — the shared vocabulary (identity, guilds, achievements,
  permissions) every implementation is held to.
- **Network** — the actual running implementation: an `avalon-server`
  process (or several role-specialized ones) backed by Postgres, talking
  the protocol over HTTP/WebSocket.
- **Integrators** — sovereign games, apps, and services that opt into
  whichever parts of the network they want.

The reference implementation is a Rust workspace: a `protocol` crate
(pure domain types and traits, no I/O), a `chain` crate (the settlement
ledger), an `indexer` crate (the fast-read query layer, rebuildable from
ledger history at any time), and a `server` crate (the one network-facing
API every client — the Hub, hub-app, and every integrator — talks to).
SDKs and a local dev/ops CLI build on top of that network; neither is
part of what the server deploys as.

## Identity

An Avalon identity is a self-custodied keypair: a WebAuthn passkey for
login, plus a separate Ed25519 key that signs the events the identity
authors. It identifies a keypair, not a person — there is no name,
government ID, biometric, or other real-world identifier anywhere in it,
by construction. An identity can register multiple passkeys and devices,
recover access through M-of-N guardian approval if every device is lost,
and pair a new device onto an existing identity without starting over.

Login today is identity-id-first rather than fully usernameless: the
underlying WebAuthn library's convenience registration path hardcodes
non-resident credentials, so true discoverable (passkey-only, no typed
identity id) login needs attested resident keys, which aren't built yet.

A "character" — race, class, level, inventory, progression — is entirely
integrator-owned gameplay data. Avalon never sees it unless the
integrator chooses to publish a schema describing some of it.

## Social Graph

Friends, presence, and communication persist across every integrator a
player uses. None of it is handed to an integrator wholesale — only
through explicit capability grants and visibility scopes an identity
controls. Presence is ephemeral by design and never enters durable
history; losing it just means an identity shows offline until its next
heartbeat.

## Guilds

A guild is a network-level social primitive with members across many
integrators at once, roles, per-resource permission overrides,
membership, channels, chat, and events with RSVP. It survives any single
integrator shutting down; an integrator is a client of a guild — it can
render and consume guild data — but never its owner.

## Achievements

An achievement (attestation) is a signed claim of the shape "issuer X
asserts identity Y accomplished Z," timestamped and schema-typed. Games,
apps, and services register as issuers under a two-tier root/operational
signing-key model, sign issuance locally, and can revoke a claim later.
Revocation is appended history, never a deletion — both the original
issuance and any later revocation stay visible forever.

Verification answers three separate questions, never collapsed into one:

- **Authentic** — the signature verifies against the issuer's registered
  key. A cryptographic fact, checkable by anyone.
- **Valid** — the claim hasn't been revoked, and its issuer wasn't
  suspended or revoked at the time of issuance.
- **Recognized** — whether a specific *receiving* integrator chooses to
  honor the claim. Entirely contextual, decided per integrator, never by
  the network.

Avalon computes and publishes the first two. The third is always the
receiving integrator's own judgment call — Game B doesn't have to accept
anything Game A issues as meaningful, but it can independently verify
that Game A actually issued it.

## Settlement Ledger

Every identity, guild, and achievement write commits atomically with its
own ledger entry via an outbox pattern, so a crash can never leave one
without the other. The ledger itself is a hash-chained, Merkle-rooted,
append-only, Postgres-backed log with periodically published Signed Tree
Heads, mirror-facing proof/sync endpoints, and node-tiered retention.

It is a public transparency log, not a blockchain: no validator set, no
mining-based consensus, because nothing written to it is ever
contested — an integrator signs its own issuance, an identity signs its
own claim, and there's no scarce resource being fought over. Anyone can
verify an entry without asking permission, and anyone can mirror the log
without being trusted first. One protocol event is never one chain
transaction — events are batched behind a commitment — and real-time
gameplay never touches settlement.

## Permission Model

An integrator never automatically receives everything associated with an
identity. Every capability — `friends.read`, `guilds.chat`,
`achievements.issue`, and so on — is granted explicitly by the player and
can be revoked at any time. Every SDK method checks its own required
grant before making a request, and the server enforces the same check
independently; the client-side check is a fast fail for a better
developer experience, never the actual security boundary. The guiding
rule is least privilege by default: granting access to guild membership
doesn't imply access to friends, private messages, or achievement
history.

## Hub & Clients

The Hub (`apps/hub`) is a player's front door into Avalon with no
integrator open: identity setup, friends, guilds, achievements, and
connected-integrator management. It is a client of the network like any
other — it talks to `avalon-server` through the same API, authentication,
and capability model as any integrator or third-party client, has no
backend of its own, and holds no privilege another authorized client
couldn't have.

A companion app (`apps/hub-app`, a desktop/mobile shell around the
same UI) is a second proof that other clients are possible by
construction: web, mobile, desktop, a Discord integration, or an
integrator's own native UI could all be built the same way. A player who
never installs any Hub client loses nothing at the protocol level —
presence, guild channels, and friends already live at the network layer,
not the game layer, which is what makes an independent client possible in
the first place.

The Hub is not a launcher, a store, a distribution platform, or an
integrator authority. It doesn't own the integrators it lists, and it
shows every authentic, valid claim with its provenance without ever
implying the network has judged one integrator's claim more prestigious
than another's.

## SDKs

A game, app, or service integrates through an SDK for its own language
rather than talking to the network directly — the SDK handles the
network calls, cryptographic signing, retry logic, and capability checks,
so a developer thinks in identity, guilds, achievements, and presence,
never in Postgres instances or node addresses.

Avalon ships an official SDK for Rust (the reference implementation), C#
(Unity-targeted, the priority developer-facing surface for game studios),
and TypeScript (browser-facing, and what the Hub itself is built on).
Every SDK exposes two session types: a capability-gated session for an
integrator acting on a player's explicit grants, and a first-party
account session for an identity's own account operations (registration,
recovery, device and guild administration, and more). The two share no
conversion between them in either direction — an integrator credential
can never yield account-level power, by construction, not just by
convention.

## Self-Hosting

Avalon is open source and self-hostable. Running the code under your own
`network_id` is fully supported, but it's a fork: cryptographically
incapable of merging back into the public network's log later. Multiple
organizations mirroring and serving reads from the *same* public log is
supported and encouraged instead — the way multiple parties can mirror a
Certificate Transparency log today — but that's participation, not
federation: visibility is never conditional on which server happens to
trust which other server, the way federated systems make it.

## Current Limitations & Non-Goals

- **No economic layer.** No cross-game currency, wallet, or purchase
  primitive exists or is planned in the near term.
- **No portable assets.** Ownership and provenance for in-game items is
  unbuilt; an achievement is a claim about an event, not an asset.
- **Login is not fully usernameless yet** — see Identity above.
- **No validator-set consensus.** The ledger is a single-operator
  transparency log today, not a BFT-replicated chain with a validator
  set; that's a separate, still-open piece of design.
- **Not a launcher, store, or distribution platform, and not an
  integrator authority.** Avalon does not own, rank, or curate the
  integrators that connect to it, and there is no score or
  "recommended" ordering anywhere in the network's own surfaces.
- **Federation is explicitly rejected as a model.** Visibility is a
  property of the public log, not a relationship between servers
  deciding whether to trust one another — that would quietly recreate
  the walled gardens Avalon exists to avoid.
- **Not a blockchain-currency or NFT project.** Settlement exists for
  verifiability, not speculation, and there is no token at any layer of
  the system today.

## Known Challenges

- **Identity recovery.** Account recovery and identity security are
  inherently hard problems; guardian-based social recovery mitigates but
  doesn't eliminate the risk of losing an identity for good.
- **Trust.** Integrators need reliable ways to decide which issuers to
  trust; Avalon deliberately doesn't make that judgment for them, which
  is a strength but also real work every integrator has to do itself.
- **Privacy.** A portable identity creates real surveillance risk if
  visibility scoping isn't taken seriously and kept correct.
- **Abuse.** A persistent identity can make harassment persistent too;
  blocking and visibility controls need to hold up under real misuse,
  not just the common case.
- **Interoperability of meaning.** Different integrators have
  fundamentally different concepts of progression and achievement.
  Avalon records provenance, not meaning, by design — a receiving
  integrator always decides what a claim means to it — which is a
  feature, but also a real limit on what "interoperability" can promise.
- **Centralization risk.** A single dominant operator could become a de
  facto gatekeeper even without any formal authority to do so; the
  mirroring model exists specifically so that isn't structurally
  required.
- **Adoption.** The network has limited value until enough independent
  integrators actually use it — this is acknowledged directly rather
  than assumed away.

## Open Questions

- How should guild ownership transfer and leadership succession work at
  the edges — contested transfers, abandoned guilds?
- How much social information should be portable by default versus
  opt-in only?
- What is the long-term technical and consensus design if Avalon ever
  moves past a single-operator log toward a validator set?
- What does the settlement log ultimately anchor to or become, if
  anything, beyond its own verifiable history?
- Should integrator data (assets, schemas) ever standardize across
  integrators, or stay integrator-defined indefinitely?
- How should economic transactions work, if an economic layer is ever
  built at all?

These are open on purpose, not silently resolved by whatever gets
implemented first.

## Guiding Principles

1. **Integrators remain sovereign.** Avalon connects them; it doesn't
   control them.
2. **Identity belongs to the player**, not to any integrator or company.
3. **Interoperability is opt-in.** No integrator is forced to support
   anything it doesn't want.
4. **Least privilege.** An integrator receives only the capabilities it's
   explicitly granted.
5. **History is portable and append-only.** Achievements and attestations
   survive individual integrators; revocation is recorded, never erased.
6. **Functionality is contextual.** A claim doesn't need identical
   meaning everywhere it's recognized.
7. **Settlement is a public log, not a blockchain in the currency sense,
   and not federation.** Anyone can verify and mirror it without being
   trusted first.
8. **Open source first.** The protocol doesn't depend on a proprietary
   implementation.
9. **Developer experience matters.** Integration should be simple, and
   the SDK should hide infrastructure, never domain concepts.
10. **Build the infrastructure, not the universe.** Avalon connects
    worlds; it doesn't try to become one.
