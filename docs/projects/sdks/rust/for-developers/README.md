# For Developers

Documentation for developers integrating Avalon into their own game, app, or
service — the audience the SDKs (`crates/sdk`, `bindings/csharp`) exist for.

Start with [`WhyBuildOnAvalon.md`](WhyBuildOnAvalon.md) for the case for
integrating your game, app, or service with Avalon at all — a vision
document, not an integration guide. The pages below are the integration
guide, for the Rust SDK (`crates/sdk`), the reference implementation.
`bindings/csharp/AvalonSdk` mirrors most of that surface but doesn't have
its own parallel guide yet — it'll get one once #51 catches it up.

## Guides

1. [`getting-started.md`](getting-started.md) — add the crate,
   `AvalonClient::new`, `authenticate`, read a profile. Ten minutes.
2. [`capabilities.md`](capabilities.md) — the capability list, what each
   unlocks, how a player grants one, what `CapabilityNotGranted` means.
3. [`achievements.md`](achievements.md) — define, issue (signed with your
   own issuer key), read, verify, revoke.
4. [`guilds-and-friends.md`](guilds-and-friends.md) — reading rosters,
   friends, and presence, and their current visibility-scoping gaps.
5. [`errors-and-retries.md`](errors-and-retries.md) — the `SdkError`
   taxonomy and what's safe to retry.
6. [`local-development.md`](local-development.md) — running the whole
   vertical slice locally through `avalon-cli`, no game client needed.

These pages are about *doing* — they link into
[`../../../backend-server/architecture/`](../../../backend-server/architecture/) for the concepts behind what you're
doing (the trust model, capability grants, revocation, visibility) rather
than restating it. Runnable, `make check`-compiled examples for each guide's
core flow live in `crates/sdk/examples/` (`authenticate.rs`,
`issue_achievement.rs`, `list_friends.rs`).

See also [`../../architecture/sdk.md`](../../architecture/sdk.md) for the SDK
design principle (protocol capabilities, not infrastructure),
[`../../../backend-server/architecture/trust-model.md`](../../../backend-server/architecture/trust-model.md) for what an
integrator is and isn't told about an attestation, [`../../../stakeholders/Proposal.md`](../../../../stakeholders/Proposal.md)
§17–19 and §24 for the intended developer experience, and the repository root
`README.md` for the current build status.
