# The Hub

**The Avalon Hub is a client of the network. It is not the network.** It talks
to `avalon-server` through the same API, authentication, and capability model as
any integrator or third-party client, has no backend of its own, and holds no
privilege another authorized client couldn't have. Narrative in
[Proposal: Hub & Clients](../../../stakeholders/Proposal.md#hub--clients).

## The world outside the integrators

The Hub is where a user interacts with the network itself, with no integrator open:

- create and manage a persistent identity; connect keys or a wallet where that
  applies ([identity](../../backend-server/architecture/identity.md))
- profile and identity-controlled metadata
- friends and presence
- guilds and guild chat
- achievements, attestations, and history
- integrator discovery and per-integrator profiles
- permissions: which integrators have which capabilities, and revoking them
- connected integrators (bindings)
- recognition preferences (which issuers' claims to feature)
- integrator event history
- later: asset ownership

It is not a launcher, a store, a distribution platform, or an integrator authority. It
does not own the integrators it lists. Steam + Roblox + universal launcher is the shape
to avoid (see [Proposal: Current Limitations & Non-Goals](../../../stakeholders/Proposal.md#current-limitations--non-goals)).

## One client among several

Because the Hub uses only the public API, other clients are possible by
construction: web, mobile, desktop, a Discord integration, an integrator's native UI, a
third-party application. `apps/mobile-hub` is the first proof — the same UI in a
Tauri shell for desktop and mobile, for guild chat and presence without an integrator
running (see [Proposal: Hub & Clients](../../../stakeholders/Proposal.md#hub--clients)).
A user who never installs any Hub loses nothing at the protocol level.

Rules that keep this true:

- no Hub-only endpoints, read paths, or credentials
- anything the Hub needs that the server lacks is a server ticket first
- no server-side rendering that reaches into Postgres; no Hub-scoped tables
- `packages/ui` is a component library for clients and knows nothing about
  storage or settlement

## Hub and achievements

The Hub shows every authentic, valid claim with its provenance, regardless of
whether any integrator recognizes it:

```text
Your History

🏆 Dragon Slayer
   Ashen Realms · Verified

🏆 Dragon Slayer
   WorldZero · Verified

🏆 Dragon Slayer
   Community MMO #47 · Verified
```

All three are authentic. The user can filter and sort. Feature and hide are
deferred — they depend on a visibility/preference store that doesn't exist
yet, described under "What's built today" below. The Hub never implies
Avalon has judged one more prestigious than another, and a revoked claim
shows as revoked with its history, not as a gap. See the
[trust model](../../backend-server/architecture/trust-model.md) and
[revocation](../../backend-server/architecture/revocation.md).

## Hub and guilds

Guilds are network primitives, so the Hub is their natural client: create, join,
leave, manage roles, chat, see members and their presence, see which integrators
members are playing, view history, coordinate across integrators. A guild is unaffected
by any integrator shutting down. See [guilds](../../backend-server/architecture/guilds.md).

## Hub and integrator discovery

A directory of integrators connected to Avalon and a profile page per integrator, rendering
[registry](../../backend-server/architecture/registry.md) facts with their definitions and class labels
(durable-derived / realtime / self-reported), issuer status and key history, and
recognition relationships. Sort options are explicit and neutral; there is no
score and no trust-derived "recommended" ordering. A suspended or revoked issuer
is visibly marked.

## What's built today

The Hub is a real, persistent shell with nested routed pages, not a stub or
a design mockup — every view described below is implemented, not a
placeholder screen.

### Shell and navigation

`apps/hub/` is Vue 3 + Vite + TypeScript, routed with `vue-router`. The
fetch client, session store, and WebAuthn/signing-key auth ceremony live in
`packages/api-client` (`@avalon/api-client`) so nothing calls `fetch`
directly outside it and `apps/mobile-hub` shares the exact same code rather
than a copy; `apps/hub/src/api/` keeps its own domain-specific modules
(`guilds.ts`, `friends.ts`, ...) built on top of it.

The logged-in Hub is a persistent shell, not separate pages: `HubShell.vue`
renders a fixed left sidebar on desktop (wordmark, nav, the caller's own
presence) that collapses to a bottom nav bar on mobile, plus a top header
(search, notifications, a user chip linking to Profile), with
`<RouterView />` swapping the page in place. Pages are nested child routes
under a shared parent — URL-addressable and bookmarkable. Nav entries for
features that don't exist yet render disabled with a "Soon" tag rather than
being hidden. The shell heartbeats presence every 60 seconds so a user
reads as online to their friends while the Hub is open. Visual language is
one dark theme via CSS custom properties, imported once and referenced by
every component's own styles.

Nothing on a logged-in page is an open input by default: a value renders as
a styled read-only display and only becomes editable when the user presses
Edit — Enter, Save, or leaving the field commits a change, Escape/Cancel
reverts. Actions that need input (add a friend, recover with a phrase) sit
behind a button that reveals the form. Login and Create Identity are the
exception, since entering an id/name is the whole screen.

### Identity, login, and recovery

Identity creation and login are their own pre-authenticated pages
(`CreateIdentity.vue`, `Login.vue`), backed by a real WebAuthn passkey
ceremony plus a separate Ed25519 signing key — see
[identity](../../backend-server/architecture/identity.md) for the crypto/API
layer underneath. `Profile.vue` is an avatar/name/handle hero with log out,
editable profile fields, and device setup/recovery/pending-approval/device-list
cards. `RecoverIdentity.vue` is the guardian-based recovery flow's entry
point for a device with no registered passkey.

### Achievements

`Achievements.vue` reads the identity's full attestation history, merges in
achievement/milestone display names and issuer display names, and renders
each claim via a card: achievement name, a clickable issuer chip routing to
the integrator's profile page, issued date, a valid/invalid badge with its
reason (never a score or star rating), and an expandable history list
showing any revocation's date and reason alongside the original issuance
rather than replacing it. Filtering (by achievement/integrator name, and by
a specific issuer) and sorting (date/name/integrator) are both client-side.
Feature/hide and recognition-aware display are not built yet — they depend
on a visibility/preference store that doesn't exist yet, and on the trust
model's own recognition-display gap respectively.

### Home, friends, messages, activity, guilds

**Home** is the landing page after login: a welcome header, recently
connected integrator, a grid of every connected app/integrator, quick
actions, friends online, a compact guilds panel, latest guild messages, and
recent activity with a link to the full activity view. Every section has
its own empty state with a next action.

**Friends** keeps each friend's presence live over a websocket, re-subscribed
on every refresh; friend-request list and membership changes still poll
periodically since they aren't pushed. Adding a friend accepts either a raw
identity id or a `display_name#1234` handle. There is no "playing
&lt;integrator&gt;" label yet — no integrator registry exists for that
lookup today.

**Messages** is direct/small-group conversations: a conversation-list
sidebar next to the active thread. Starting a conversation happens from a
friend row's message action rather than a separate "new message" flow,
since starting a conversation and opening an existing one with the same
participants are the same call.

**Activity** is "what does the network know about me": the caller's own
protocol events, rendered as human-readable summary lines with relative
timestamps, not raw event-kind strings. An event kind this build doesn't
recognize falls back to the raw kind rather than erroring, since the kind
catalogue keeps growing. It is not a chain/block explorer.

**Guilds** covers the guild list (with a create form), a guild overview
(roster grouped by role, roles, channels, management), and channel chat,
all nested under the same shell as every other page. Roster and guild data
poll periodically; chat polls faster, closer to a live conversation, with
load-older-on-scroll pagination and a live character counter against the
server's message-length cap. Every management action is gated client-side
on the caller's own resolved permission list, but the server re-checks
independently. A "Currently playing" card groups the roster's merged
presence by integrator; a "History" card states plainly that guild event
history isn't available yet rather than fabricating one from the current
roster.

### Integrator discovery and connections

`IntegrationDirectory.vue`/`IntegrationProfile.vue` are a searchable,
sortable directory of integrators connected to Avalon and a per-integrator
profile page rendering registry metrics with their definitions and class
labels, plus the integrator's full issuer key history. Both are public,
unauthenticated reads. There is no ranking, score, or "recommended"
ordering anywhere in either view. When the viewer is logged in, the profile
page also shows a "Your access" section scoped to just that integrator's
binding. `Connections.vue` lists the caller's own integrator bindings with
a revoke action; `ConnectIntegration.vue` is the capability-consent flow a
user lands on to authorize an integrator.

### Shared component library

`packages/ui` (`@avalon/ui`) holds every shared component — buttons, form
fields, cards, nav, identity/social rows, guild/event/achievement
components — all presentational (props in, events out), split into
`components/`/`styles/`/`stories/`/`types/`/`state/`. No `<style>` blocks
live in `.vue` files; styling lives in `.module.scss` and references design
tokens only.

Responsive layout is CSS-only, living in each component's own styles —
never a separate mobile-only fork. Every component holds up at phone width
via `flex-wrap`, `min-width: 0` on flex children, `text-overflow: ellipsis`
on names/labels, `flex-shrink: 0` on icons, and relative sizing throughout,
rather than a breakpoint or compact-viewport variant per component.

### What the Hub calls

The Hub's full request surface spans identity/session endpoints (WebAuthn
registration and login, profile), friends and presence, guild
CRUD/membership/roles/channels/chat, integrator discovery and registry
reads, and connection/grant management. See
[identity](../../backend-server/architecture/identity.md),
[guilds](../../backend-server/architecture/guilds.md), and
[registry](../../backend-server/architecture/registry.md) for the endpoint-level
detail behind each domain.
