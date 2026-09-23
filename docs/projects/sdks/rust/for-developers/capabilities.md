# Capabilities

A `Session` is scoped to whichever capabilities the player actually granted
your integrator. Every read/write method checks its own required
capability before making a request — a method with no grant fails fast with
`SdkError::CapabilityNotGranted`, never a partial or silently-empty result.

For the full permission-model design and how a grant is stored, see
[`../../../backend-server/architecture/bindings.md`](../../../backend-server/architecture/bindings.md) and
[Proposal: Permission Model](../../../../stakeholders/Proposal.md#permission-model).

## The capability list

`avalon_protocol::permissions::Capability` (`crates/protocol/src/permissions.rs`)
is an enum with a permanent wire-string mapping — the string, not
the Rust variant name, is the stable identifier a grant is stored/compared
against. `Capability::Other(String)` preserves any string this build doesn't
know about yet rather than erroring, so a newly-added server-side capability
never breaks an older SDK build.

What the Rust SDK actually checks today:

| Capability            | Unlocks                                                  |
| ---------------------- | --------------------------------------------------------- |
| `friends.read`         | `Session::friends()`                                       |
| `presence.read`        | `Session::presence()`/`presence_of()`/`subscribe_presence()`, and presence embedded in `friends()`/guild rosters |
| `guilds.read`           | `Session::guilds()`, `GuildHandle::roster()`/`events()`     |
| `guilds.chat`           | `GuildHandle::channels()`, `ChannelHandle::messages()`/`send()` |
| `achievements.read`    | `Session::achievements()`                                  |
| `achievements.issue`   | `Session::issue_achievement()`                              |
| `messages.read`        | `Session::conversations()`, `ConversationHandle::messages()` |
| `messages.send`        | `Session::dm()`, `ConversationHandle::send()`                |

A few more (`identity.read`, `profile.read`, `presence.publish`,
`guilds.issue`, `milestones.issue`, `assets.*`, `wallet.*`) exist in the
protocol's own vocabulary but have no SDK method checking them yet — either
the underlying server capability isn't enforced yet either, or the SDK
surface for it hasn't been built. Check `rust/src/*.rs` in the `avalon-sdks`
repo for the current, authoritative set; this table is a snapshot, not a
contract.

## How a player grants one

Today, only through the Hub's own consent flow (`POST
/integrations/{slug}/connect`) — there is no SDK method for *requesting* a
grant, since asking for one is inherently something the player does, not
your integration. `avalon-cli`'s live tests drive this directly over HTTP for
local dev/testing (see [`local-development.md`](local-development.md)); in
production, your integration would send the player to a Hub-hosted consent
page and receive the resulting session token back.

## What `CapabilityNotGranted` means

`SdkError::CapabilityNotGranted(String)` — the string is the capability's
wire name. It means exactly one of:

- The player has never granted this integrator that capability.
- The grant existed once but was revoked.

The SDK's own `Session::require` check runs *before* any network call, so
this error never costs you a round trip — but the server enforces the same
check independently on every request too (`crates/server/src/authz.rs::require_capability`);
never treat the client-side check alone as the security boundary.

See [`errors-and-retries.md`](errors-and-retries.md) for how this fits into
the full `SdkError` taxonomy.
