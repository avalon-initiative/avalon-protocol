# For Developers

Documentation for developers integrating Avalon into their own game, app, or
service — the audience the SDKs (`crates/sdk`, `bindings/csharp`) exist for.

Start with [`WhyBuildOnAvalon.md`](WhyBuildOnAvalon.md) for the case for
integrating your game with Avalon at all. It's a vision document, not an
integration guide — there's no stable, documented SDK to integrate against
yet. `crates/sdk` now has real implementations for authentication, friends/
presence, guilds, conversations, and offline sync/submission — achievement
issuance is still `NotImplemented`, and `bindings/csharp/AvalonSdk` is still
a skeleton. Once the surface is stable enough to commit to, this is where
getting-started guides, capability/permission reference, and integration
examples belong.

Until then, see [`../architecture/sdk.md`](../architecture/sdk.md) for the SDK
design principle (protocol capabilities, not infrastructure),
[`../architecture/trust-model.md`](../architecture/trust-model.md) for what an
integrator is and isn't told about an attestation, [`../stakeholders/Proposal.md`](../stakeholders/Proposal.md)
§17–19 and §24 for the intended developer experience, and the repository root
`README.md` for the current build status.
