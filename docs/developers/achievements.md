# Achievements

Define, issue, read, verify, and revoke — the full lifecycle an integrator
(a game, an app, a service) drives through `crates/sdk/src/achievements.rs`
and a handful of raw HTTP calls not yet wrapped by the SDK. For the
underlying design (issuer keys, the authenticity/validity/recognition
split, revocation history), see
[`../architecture/achievements-and-attestations.md`](../architecture/achievements-and-attestations.md)
and [`../architecture/trust-model.md`](../architecture/trust-model.md) — this
page is about *doing*, not re-explaining those.

## 1. Register your integrator

Not an SDK call (there's no player-facing token yet at this point) — either
`POST /integrations` directly, or `avalon-cli`'s `register-integrator`
command, which also saves the resulting private signing key locally. See
[`local-development.md`](local-development.md).

## 2. Register as an issuer on your target network

Separate from integrator registration above: before you can issue on a real
network, your integrator's signing key must be admitted to write on it
(#479/#480's per-network isolation — a signature valid against your key
history isn't itself enough, see
[`../architecture/network-trust-anchors.md`](../architecture/network-trust-anchors.md)).
`AvalonClient::register_issuer` requires you to **explicitly declare which
network you mean** — either the exact `network_id`, or one of the three
real deployment tiers — and refuses, client-side, before sending any
request, if that doesn't match what the server's own Signed Tree Head
independently verifies as. There is no implicit default inferred from the
server URL alone.

```rust
use avalon_sdk::network::{TargetNetwork, TargetNetworkTier};

let registration = client
    .register_issuer("game:ashen-realms", TargetNetwork::Env(TargetNetworkTier::Dev))
    .await?;
```

or declare the exact string if you know it (required for the
milestone-1 local dev network, `avalon-dev-local`, which isn't one of the
three real tiers):

```rust
let registration = client
    .register_issuer("game:ashen-realms", TargetNetwork::NetworkId("avalon-dev-local".to_string()))
    .await?;
```

**What a mismatch error means:** if you get back
`IssuerRegistrationError::NetworkTarget(NetworkTargetError::Mismatch { declared, actual })`,
the server really is a verifiable Avalon network — just not the one you
meant. Double-check `server_url` against which deployment you intended to
target; this is the exact case #483 exists to catch (a copy-pasted `.env`
pointing at the wrong environment, most commonly). A
`NetworkTargetError::Unverified` instead means the server's claimed network
couldn't be confirmed as trustworthy at all (unknown, key mismatch, or
unreachable) — refused regardless of what you declared, since there's
nothing to compare it against.

`avalon-cli`'s `register-issuer --integrator <slug> (--network-id <id> |
--env <dev|int|mainnet>)` command drives this same SDK call.

## 3. Define the achievement

Also not an SDK call yet (issue #49's own scope is issuance, reading,
verifying — defining is a documented gap, not silently skipped):

```text
POST /integrations/{slug}/achievements
x-avalon-integrator-key-id: <key id>
x-avalon-integrator-challenge-id: <from POST /integrations/{slug}/challenge>
x-avalon-integrator-signature: <that challenge's nonce, signed>

{ "key": "dragon_slayer", "name": "Dragon Slayer", "description": "Slew the dragon" }
```

`crates/server/src/achievements.rs` documents the full challenge-response
shape; `crates/sdk/tests/achievements.rs`'s own test helpers are a working
reference implementation of the same request.

## 4. Get the player's consent

The player must grant your integrator `achievements.issue` (and
`achievements.read` if you also want to read their history) — see
[`capabilities.md`](capabilities.md).

## 5. Issue it

```rust
use avalon_sdk::{AvalonClient, AvalonConfig};

let client = AvalonClient::new(AvalonConfig {
    server_url: "http://127.0.0.1:8080".to_string(),
    integrator_credential_key_id: key_id,
    // Both required for issuance — this is what signs the attestation
    // locally with your own issuer key. The server never sees the
    // private key, only a detached signature.
    integrator_slug: Some(slug),
    signing_key: Some(signing_key_bytes),
    retry: Default::default(),
});

let session = client.authenticate(&player_session_token).await?;
let attestation_id = session.issue_achievement("dragon_slayer").await?;
```

`Session::issue_achievement` always issues to the session's own identity —
there's no way to issue to an arbitrary third party through this method,
matching the server's own consent model (the *player* granted this, for
themselves). Two independent proofs go out on the wire: a challenge-response
proving your key is making this call right now, and a signature embedded in
the request body over the attestation's own canonical bytes, proving your
key specifically authorized *this* attestation — see
`crates/sdk/src/achievements.rs`'s own doc comment for the exact byte
format. This call also carries a real `Idempotency-Key` (issue #47) — a
transient network failure never risks a double issuance.

A complete, runnable version: `crates/sdk/examples/issue_achievement.rs` —
`cargo run -p avalon-sdk --example issue_achievement`. `avalon-cli`'s
`issue-achievement` command drives the exact same SDK call.

## 6. Read a player's history

```rust
let history = session.achievements().await?; // requires achievements.read
for attestation in history {
    println!("{}: {:?} / {:?}", attestation.achievement, attestation.authenticity, attestation.validity);
}
```

Deliberately **no `recognition` field** — `authentic` (the signature
verifies), `valid` (not revoked), and `recognized` are three separate
questions per [ADR #76](../architecture/trust-model.md). The server answers
the first two; *recognition* is inherently a judgment call only the
consuming integrator can make (do you trust the issuer? does the claim
matter to you?) — evaluate it yourself against
`avalon_protocol::achievements::recognize` and your own
`TrustRelationship`, never assume the SDK has decided for you.

## 7. Verify a specific attestation

`GET /attestations/{id}` is public and unauthenticated — no SDK wrapper yet,
but a plain HTTP `GET` returns the same authenticity/validity/history shape
`achievements()` does, for a single attestation by id, from anyone (not just
its subject).

## 8. Revoke

Not yet wrapped by the SDK — `POST /attestations/{id}/revoke`, issuer-only
(only the issuer that issued it can revoke it), append-only history (a
revocation is recorded, never erased). See
[`../architecture/revocation.md`](../architecture/revocation.md).
