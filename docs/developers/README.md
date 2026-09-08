# For Developers

Documentation for game developers integrating Avalon into their own game —
the audience the SDKs (`crates/sdk`, `bindings/csharp`) exist for.

Nothing lives here yet — there's no stable SDK to document integration against.
Once `crates/sdk` and `bindings/csharp/AvalonSdk` have real implementations
(not the current `NotImplemented` stubs), this is where getting-started guides,
capability/permission reference, and integration examples belong.

Until then, see [`../architecture/sdk.md`](../architecture/sdk.md) for the SDK
design principle (protocol capabilities, not infrastructure),
[`../architecture/trust-model.md`](../architecture/trust-model.md) for what a
game is and isn't told about an attestation, [`../Proposal.md`](../Proposal.md)
§17–19 and §24 for the intended developer experience, and the repository root
`README.md` for the current build status.
