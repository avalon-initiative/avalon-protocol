# TypeScript SDK

`bindings/ts` — the browser-facing [Avalon SDK](../README.md), implementing
both `IntegratorSession` (capability-gated, mirrors Rust/C#'s `Session`) and
`AccountSession` (first-party) from scratch. See
[`../architecture/sdk.md`](../architecture/sdk.md) for the design that
applies to every language's SDK — this page is the TypeScript-specific
"how," not the "why."

**Status (2026-09-21):** real and shipped (#701), not speculative — ES
modules, `vitest` for tests. Not a workspace member: intentionally outside
the root `package.json`'s `workspaces` array and `npm install`ed
separately from inside `bindings/ts` itself, since the design is for this
package to eventually move into its own `avalon-sdks` org repo with no
internal dependency on `packages/api-client`, `apps/hub`, or
`apps/mobile-hub` anywhere in its source. It is, however, actively
consumed by `apps/hub` — epic #712 is migrating the Hub frontend off the
older `packages/api-client` and onto this SDK, batch by batch.

Unlike the C# port (which scoped WebAuthn ceremony-driving out entirely)
and the Rust port (which drives a virtual/software authenticator, since it
has no browser to run in), this SDK is browser-facing and drives a real
WebAuthn ceremony via `@simplewebauthn/browser` — registration and login
are both implemented for real, not stubbed out.

## Shape of the API

- `AvalonClient` — the entry point: `register(displayName)`,
  `login(credentials)`, `resumeAccountSession(token)`,
  `resumeAccountSessionWithSigningKey(token, seed)`,
  `startAccountDeviceLogin()`, `authenticate(...)` (for
  `IntegratorSession`).
- `AccountSession` — mirrors the Rust/C# `AccountSession` surface
  field-for-field: profile, passkeys, devices/grants/cross-device pairing
  approval, social recovery, friends/blocks/presence/discovery,
  conversations, full guild administration, and integrator
  connect/consent. Every signature-required action
  (`AccountSession.sign(actionTag, fields)`) signs itself automatically —
  callers never hand-construct `signing_key_id`/`signature`.
- `IntegratorSession` — the capability-gated model: every method checks
  its own `require(capability)` client-side before making a request, the
  same fast-fail convention every other SDK in this repo uses (never the
  actual security boundary — the server enforces the same thing
  independently).
- No implicit conversion between `AccountSession` and `IntegratorSession`
  anywhere in this package — no shared base class, no cast — #696's hard
  invariant holds at the type level here too, same as Rust/C#.
- Errors are typed subclasses of `AvalonSdkError`
  (`UnauthorizedError`/`CapabilityNotGrantedError`/`NotFoundError`/
  `ConflictError`/`RejectedError`/`UnavailableError`/`ProtocolError`/
  `NotConversationParticipantError`/`MissingIssuerCredentialsError`/
  `DeviceLoginDeniedError`/`DeviceLoginExpiredError`/
  `NoLocalSigningKeyError`), mapped from HTTP status + the server's own
  `{ error, code }` body.

## No dedicated getting-started guide yet

Unlike the Rust SDK's `for-developers/` set, this SDK doesn't have its own
numbered guide series yet — noted honestly rather than left to look
finished. Until one exists, the most accurate reference is
[`../architecture/sdk.md`](../architecture/sdk.md)'s own "Today in the
repo" section (search for `bindings/ts`), which is kept current alongside
the code, plus the colocated `*.test.ts` files under `bindings/ts/src/`,
which double as runnable usage examples for every domain.

## Related

- [`../README.md`](../README.md) — the SDK project overview, and the other languages.
- [`../rust/README.md`](../rust/README.md) — the Rust SDK, reference implementation.
- [`../csharp/README.md`](../csharp/README.md) — the C# SDK.
- [`../../backend-server/README.md`](../../backend-server/README.md) — what this SDK talks to.
