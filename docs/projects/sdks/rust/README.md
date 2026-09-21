# Rust SDK

`crates/sdk` — the reference implementation of the [Avalon SDK](../README.md).
"Reference" means it's built alongside `backend-server` itself, so it's the
most complete SDK and the one other languages are checked against. See
[`../architecture/sdk.md`](../architecture/sdk.md) for the design that
applies to every language's SDK, not just this one.

**Status (2026-09-21):** real, not stubbed. `authenticate()` is wired to a
live server; friends/presence, guilds (roster/channels/chat), and
conversations work end to end; `sync_journal`/`submission` implement
offline durability and deferred submission. Achievement issuance still
returns `NotImplemented` — the one known gap in an otherwise live surface.

## Guides

1. [`for-developers/getting-started.md`](for-developers/getting-started.md) —
   add the crate, `AvalonClient::new`, `authenticate`, read a profile. Ten
   minutes.
2. [`for-developers/capabilities.md`](for-developers/capabilities.md) — the
   capability list, what each unlocks, what `CapabilityNotGranted` means.
3. [`for-developers/achievements.md`](for-developers/achievements.md) —
   define, issue (signed with your own issuer key), read, verify, revoke.
4. [`for-developers/guilds-and-friends.md`](for-developers/guilds-and-friends.md) —
   reading rosters, friends, and presence, and current visibility-scoping gaps.
5. [`for-developers/errors-and-retries.md`](for-developers/errors-and-retries.md) —
   the `SdkError` taxonomy and what's safe to retry.
6. [`for-developers/local-development.md`](for-developers/local-development.md) —
   running the whole vertical slice locally through `avalon-cli`, no game
   client needed.
7. [`for-developers/WhyBuildOnAvalon.md`](for-developers/WhyBuildOnAvalon.md) —
   the pitch for game developers specifically, language-agnostic in
   substance even though it's filed here.

Runnable, `make check`-compiled examples for each guide's core flow live in
`crates/sdk/examples/` (`authenticate.rs`, `issue_achievement.rs`,
`list_friends.rs`).

## Related

- [`../README.md`](../README.md) — the SDK project overview, and the other languages.
- [`../csharp/README.md`](../csharp/README.md) — the C# SDK.
- [`../../backend-server/README.md`](../../backend-server/README.md) — what this SDK talks to.
- [`../../cli/README.md`](../../cli/README.md) — a separate Rust program
  built on this SDK, but a dev/ops tool, not something you'd embed in a game.
