# For Developers

Documentation for developers integrating Avalon into their own game, app, or
service — the audience the SDKs (`crates/sdk`, `bindings/csharp`) exist for.

Start with [`WhyBuildOnAvalon.md`](WhyBuildOnAvalon.md) for the case for
integrating your game, app, or service with Avalon at all. It's a vision document, not an
integration guide — there's no stable, documented SDK to integrate against
yet. `crates/sdk` has real implementations for authentication, friends/
presence, guilds, conversations, achievements (including issuance), Integrator
Space schema publication, and offline sync/submission; `bindings/csharp/AvalonSdk`
mirrors most of that surface but is still catching up. Once the surface is
stable enough to commit to, this is where getting-started guides, capability/
permission reference, and integration examples belong.

Until then, see [`../architecture/sdk.md`](../architecture/sdk.md) for the SDK
design principle (protocol capabilities, not infrastructure),
[`../architecture/trust-model.md`](../architecture/trust-model.md) for what an
integrator is and isn't told about an attestation, [`../stakeholders/Proposal.md`](../stakeholders/Proposal.md)
§17–19 and §24 for the intended developer experience, and the repository root
`README.md` for the current build status.

## Trying the vertical slice locally via `avalon-cli`

`avalon-cli` (`crates/cli`, issue #48) is a local dev/ops tool, not an SDK —
useful for exercising the milestone-1 vertical slice against a real
`make start`/`make migrate` server without writing a game. The mutating
commands below (`register-integrator`, `issue-achievement`, `create-identity`,
`login`) only exist in the default `dev-tools` build (`cargo build -p
avalon-cli` — omit `--no-default-features`); a deployment-facing build strips
them out entirely, not just at runtime (see `crates/cli/src/dev_tools.rs`'s
own doc comment).

1. **Register an integrator** and save its signing key locally:
   ```
   make register-integrator SLUG=my-game NAME="My Game" OWNER="My Studio"
   ```
   Prints the private signing key exactly once — the server only ever stores
   the public half. The key (and its `key_id`) are also saved under
   `_running/keys/integrator-<slug>.*`, so later commands that take
   `--integrator <slug>` don't need them pasted back in.

2. **Define an achievement** for that integrator — not yet a CLI command
   (only issuing an already-defined one is, per this ticket's own scope);
   use `POST /integrations/{slug}/achievements` directly, with the
   challenge-response headers `crates/server/src/integrations.rs` documents,
   or issue it through the Hub once that flow exists there.

3. **Create and log in an identity**:
   ```
   make create-identity
   make login IDENTITY_ID=<uuid>
   ```
   `login` prints a live bearer session token — copy it for the next step.

4. **Consent** to the integrator (an identity must grant `achievements.issue`
   before anything can issue to it) via `POST /integrations/{slug}/connect`
   with that bearer token — not yet a CLI command either.

5. **Issue the achievement**, through `avalon-sdk` exactly the way a real
   game/app/service would (`Session::issue_achievement`):
   ```
   make issue-achievement INTEGRATOR=my-game ACHIEVEMENT=dragon_slayer TOKEN=<session-token>
   ```

6. **Inspect the ledger** to see it land:
   ```
   make inspect-ledger
   ```
