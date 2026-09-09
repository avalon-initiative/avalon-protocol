# Security Policy

## Supported Versions

Avalon Protocol is pre-release — there is no published version yet. Once
there is a first release, security fixes will target the latest code on
`main` until a stable release line is established.

## Reporting a Vulnerability

Please **do not** open a public GitHub issue for security vulnerabilities.

Instead, report it privately by emailing **cconlon@dcorps.dev** with:

- A description of the vulnerability and its potential impact.
- Steps to reproduce (proof-of-concept code or commands are helpful).
- The version/commit and deployment configuration you tested against.

We'll acknowledge your report as soon as we can and follow up with next
steps. Once a fix is available, we'll coordinate on disclosure timing and
credit you in the release notes if you'd like.

## Scope

The areas most relevant to Avalon specifically: the identity/auth path
(`crates/server` — WebAuthn passkey registration/login, Ed25519 event
signing, session issuance), the settlement ledger's hash-chaining and outbox
atomicity (`crates/chain`, `crates/server/src/outbox.rs`), and — once
implemented — the permission/capability model that decides what a game can
read from a player's identity. Anything that could let one identity or one
game act with another's authority, forge an attestation, or read data a
player hasn't granted is in scope even if it's in a crate not listed here.
