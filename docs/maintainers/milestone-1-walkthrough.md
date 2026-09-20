# Milestone 1 — End-to-End Vertical Slice Walkthrough

This is the hand-run version of `Proposal.md` §23's fourteen-step
definition of milestone 1, plus two architecture checks beyond it
(issue #64). It proves the epics compose into one working network rather
than fourteen features that each pass their own tests: two players, a
friendship, a guild with a channel, two games, an issued and verified
achievement, all visible in the Hub — then a ledger inspection and a full
projection rebuild.

`crates/cli/tests/milestone_1_walkthrough.rs` (issue #65) is the automated
equivalent of every step below and runs under `make test-live`. Steps 1–7
here were executed by hand against a real local `avalon-server` and
Postgres; steps 8–16 use the identical HTTP/SDK/CLI calls the automated
test drives (verified live via that test — see its own doc comment), shown
here as the equivalent hand-run commands.

Every step uses only public interfaces — HTTP endpoints, the SDK, the CLI,
and the Hub. No direct SQL.

## Setup

```bash
make start                    # avalon-server against the real dev Postgres
export BASE=http://127.0.0.1:8080   # or wherever AVALON_SERVER_ADDR points
```

## Step 1 — Player A creates an Avalon identity

Provided by: epic #2 (`POST /identities/register/start|finish`),
`avalon create-identity`.

```bash
echo "Walkthrough Alice" | avalon create-identity
```

Expected observation: a printed `Identity created: <uuid>`, an
event-signing key and passkey saved under `_running/keys/`, and a
`avalon login <identity_id>` hint. Run live:

```
Identity created: a11031ee-a4db-4b6b-80a6-d028c8b5c58d
Event-signing key saved to: _running/keys/a11031ee-a4db-4b6b-80a6-d028c8b5c58d.signing-key
Passkey saved to:            _running/keys/a11031ee-a4db-4b6b-80a6-d028c8b5c58d.passkey.json
```

## Step 2 — Player B creates an Avalon identity

Same as step 1.

```bash
echo "Walkthrough Bob" | avalon create-identity
```

```
Identity created: 4e1b01cd-92f6-4178-8d7f-ac688180447d
```

Both identities log in to obtain session tokens for the rest of this
walkthrough:

```bash
avalon login a11031ee-a4db-4b6b-80a6-d028c8b5c58d   # -> ALICE token
avalon login 4e1b01cd-92f6-4178-8d7f-ac688180447d   # -> BOB token
```

## Step 3 — They become friends

Provided by: issue #15 (`POST /friends/requests`, `.../accept`).

```bash
curl -s -X POST "$BASE/friends/requests" \
  -H "Authorization: Bearer $ALICE" -H "Content-Type: application/json" \
  -d "{\"to\":\"$BOB_ID\"}"
# {"id":"<request_id>", ...}

curl -s -X POST "$BASE/friends/requests/<request_id>/accept" \
  -H "Authorization: Bearer $BOB"

curl -s "$BASE/friends" -H "Authorization: Bearer $ALICE"
```

Expected observation: `GET /friends` for Alice returns one entry naming
Bob's identity id. Run live:

```
[{"a":"4e1b01cd-92f6-4178-8d7f-ac688180447d","b":"a11031ee-a4db-4b6b-80a6-d028c8b5c58d","since":"2026-09-20T21:32:42.514818Z"}]
```

## Step 4 — Player A creates a guild

Provided by: issue #20 (`POST /guilds`).

```bash
curl -s -X POST "$BASE/guilds" \
  -H "Authorization: Bearer $ALICE" -H "Content-Type: application/json" \
  -d '{"name":"Walkthrough Guild","tag":"WLKT","description":"milestone-1 walkthrough guild"}'
```

Expected observation: a guild object with `"owner"` equal to Alice's
identity id and `"join_policy":"invite_only"` (the default — see step 5).
Run live:

```
{"id":"e61fa1ab-7b7a-408f-8f91-cab13365af54","name":"Walkthrough Guild","tag":"WLKT",
 "owner":"a11031ee-a4db-4b6b-80a6-d028c8b5c58d","join_policy":"invite_only", ...}
```

## Step 5 — Player B joins

Provided by: issue #21 (`POST /guilds/{id}/invites`, `.../accept`). Guilds
default to `invite_only`, so this walkthrough uses a real invite rather
than flipping the guild open — the realistic path for two friends forming
a guild together.

```bash
curl -s -X POST "$BASE/guilds/$GUILD_ID/invites" \
  -H "Authorization: Bearer $ALICE" -H "Content-Type: application/json" \
  -d "{\"to\":\"$BOB_ID\"}"
# {"id":"<invite_id>", ...}

curl -s -X POST "$BASE/guilds/$GUILD_ID/invites/<invite_id>/accept" \
  -H "Authorization: Bearer $BOB"
```

Expected observation: a `guild_members`-shaped response for Bob
(`role_index` 2, the default member role). Run live:

```
{"guild_id":"e61fa1ab-7b7a-408f-8f91-cab13365af54","identity_id":"4e1b01cd-92f6-4178-8d7f-ac688180447d","role_index":2,"joined_at":"2026-09-20T21:32:51.160172709Z"}
```

## Step 6 — The guild creates a channel

Provided by: issue #22 (`POST /guilds/{id}/channels`).

```bash
curl -s -X POST "$BASE/guilds/$GUILD_ID/channels" \
  -H "Authorization: Bearer $ALICE" -H "Content-Type: application/json" \
  -d '{"name":"walkthrough-planning"}'
```

Expected observation: a channel object with the given name, not archived.
Run live:

```
{"id":"823f24d4-ec69-4d52-9399-2c82419b078b","guild_id":"e61fa1ab-7b7a-408f-8f91-cab13365af54","name":"walkthrough-planning","archived":false, ...}
```

## Step 7 — They communicate

Provided by: issue #22 (`POST`/`GET .../messages`).

```bash
curl -s -X POST "$BASE/guilds/$GUILD_ID/channels/$CHANNEL_ID/messages" \
  -H "Authorization: Bearer $ALICE" -H "Content-Type: application/json" \
  -d '{"body":"welcome to the guild!"}'
curl -s -X POST "$BASE/guilds/$GUILD_ID/channels/$CHANNEL_ID/messages" \
  -H "Authorization: Bearer $BOB" -H "Content-Type: application/json" \
  -d '{"body":"glad to be here!"}'
curl -s "$BASE/guilds/$GUILD_ID/channels/$CHANNEL_ID/messages" \
  -H "Authorization: Bearer $ALICE"
```

Expected observation: both messages round-trip, newest first. Run live:

```
[{"id":"86fa4c06-...","author":"4e1b01cd-...","body":"glad to be here!", ...},
 {"id":"67e5af02-...","author":"a11031ee-...","body":"welcome to the guild!", ...}]
```

## Step 8 — Game A registers with Avalon

Provided by: issue #26, `avalon register-game` (#29/#48).

```bash
avalon register-game --slug walkthrough-game-a --name "Walkthrough Game A" \
  --owner-name "Walkthrough Studio" \
  --capability achievements.issue --capability achievements.read
```

Expected observation: a printed integrator id and key id, the private
signing key shown exactly once, and a passing challenge-response sanity
check. Run live:

```
Integrator registered: walkthrough-game-a (6217225e-6fe5-462b-8331-1749082fb2c1)
Key ID:               8d113c15-9894-4b75-ae07-4213f3a0457e
Challenge-response sanity check passed (whoami: 6217225e-6fe5-462b-8331-1749082fb2c1).
```

Game A then defines the achievement it will issue in step 10, over the
integrator challenge-response endpoint
(`POST /integrations/{slug}/challenge` then
`POST /integrations/{slug}/achievements`, signed with the key just
printed) — this is one-time integrator setup, not a player-facing step,
and is exactly what `crates/cli/tests/milestone_1_walkthrough.rs`'s
`define_dragon_slayer` helper does and was verified live by that test's
own run.

## Step 9 — Player A authenticates through Game A

Provided by: SDK `AvalonClient::authenticate()` (done) plus real
binding/consent (#83/#27, `POST /integrations/{slug}/connect`).

Player A first consents (the Hub side — real HTTP, no game code):

```bash
curl -s -X POST "$BASE/integrations/walkthrough-game-a/connect" \
  -H "Authorization: Bearer $ALICE" -H "Content-Type: application/json" \
  -d '{"capabilities":["achievements.issue","achievements.read"]}'
```

Then Game A exchanges Player A's session token for a `Session` scoped to
itself, entirely through the SDK (`docs/developers/getting-started.md`):

```rust
let client = AvalonClient::new(AvalonConfig {
    server_url: base,
    integrator_credential_key_id: game_a_key_id,
    integrator_slug: Some("walkthrough-game-a".into()),
    signing_key: Some(game_a_signing_key),
    retry: Default::default(),
});
let session = client.authenticate(&alice_token).await?;
```

Expected observation: `authenticate()` succeeds and `session.profile()`
shows Alice's real display name — verified live by
`crates/cli/tests/milestone_1_walkthrough.rs`.

## Step 10 — Game A issues `Dragon Slayer`

Provided by: #31, #32, #34 (`Session::issue_achievement`).

```rust
let attestation_id = session.issue_achievement("dragon_slayer").await?;
```

Expected observation: a fresh attestation id, and `session.achievements()`
afterward shows exactly that one attestation in Player A's history —
verified live by the automated test.

## Step 11 — Game B registers

Same as step 8, a second integrator:

```bash
avalon register-game --slug walkthrough-game-b --name "Walkthrough Game B" \
  --owner-name "Walkthrough Studio" \
  --capability achievements.read
```

## Step 12 — Game B verifies the achievement

Provided by: #33 — authenticity and validity, reported separately, never
merged into one server verdict (ADR #76).

Public, unauthenticated read (no session/consent required for this
specific call — this is the one place in the walkthrough that isn't
scoped to a single player's session):

```bash
curl -s "$BASE/attestations/$ATTESTATION_ID"
```

Expected observation:

```json
{
  "authenticity": {"status": "authentic", "key_id": "..."},
  "validity": {"status": "valid"},
  ...
}
```

`crates/cli/tests/milestone_1_walkthrough.rs` instead has Game B read it
through the SDK, after Player A also connects to Game B
(`achievements.read`) and Game B authenticates her — the same
authenticity/validity split, exercised through `Session::achievements()`
rather than the raw public endpoint, proving both paths agree.

## Step 13 — Game B chooses to recognize it

A consumer's own policy decision, never a server verdict (#33/ADR #76).
`avalon_protocol::achievements::recognize()` takes a `TrustRelationship`
(who Game B trusts, and under what scope) and the claim being evaluated,
and returns `Recognition::Recognized` or `::NotRecognized { reason }` —
entirely client-side, no network call. There is no HTTP endpoint for this
step by design.

## Step 14 — The Hub displays identity, friends, guild, and achievement

Provided by: #55, #56, #57, #58. The Hub's own reads, all with Player A's
session token:

```bash
curl -s "$BASE/me" -H "Authorization: Bearer $ALICE"
curl -s "$BASE/friends" -H "Authorization: Bearer $ALICE"
curl -s "$BASE/me/guilds" -H "Authorization: Bearer $ALICE"
curl -s "$BASE/me/achievements" -H "Authorization: Bearer $ALICE"
```

Expected observation: Alice's profile, Bob in her friends list, the guild
from step 4 in her guild memberships, and the Dragon Slayer attestation
from step 10 in her achievement history.

## Step 15 — `avalon inspect-ledger` shows every durable step as intact chain entries

```bash
avalon inspect-ledger
```

Expected observation: one entry per durable event this run produced —
`identity.created` ×2, `guild.created`, `guild.member_added`,
`guild.channel_created`, `game.registered` ×2, `achievement.issued` — each
marked `✓`. This is a correction to this ticket's own original status
table, which described this step as "partial (identity.created only)" —
every one of those kinds is a real, wired-up ledger entry as of this
walkthrough (see `crates/protocol/src/events.rs::ProtocolEventKindVariant`).

Two things found live while running this step, worth knowing:

- **Writes are atomic with an outbox row, not with the ledger entry
  itself** (`crates/server/src/outbox.rs`) — a background worker drains
  that outbox into `ledger_entries` asynchronously, so `avalon
  inspect-ledger` run immediately after step 14's HTTP calls succeed can
  legitimately race ahead of the worker and not show this run's entries
  yet. `avalon outbox-status` (`outbox: N pending` vs. `outbox: empty,
  nothing pending`) is the way to confirm it has caught up before
  inspecting. `crates/cli/tests/milestone_1_walkthrough.rs` polls it.
- **This sandbox's own shared dev database already carries pre-existing
  broken links** (217 as of this writing) from unrelated historical
  activity, predating this walkthrough and unaffected by it — a long-lived
  shared dev ledger with many sessions/agents writing to it over time has
  no cleanup story yet. This means the `chain intact ✓` summary line at
  the bottom of `avalon inspect-ledger`'s output will *not* read clean
  against this specific database; what's actually load-bearing is that
  every block belonging to *this run* (its own identity ids, guild id,
  game slugs) individually reports `verified: ✓`, which is what the
  automated test checks rather than the blanket summary line. Worth a
  follow-up ticket (a real cleanup/reconciliation story for a long-lived
  shared dev ledger), not something this walkthrough fixes.

## Step 16 — Rebuild from the ledger, then repeat step 14

Provided by: #43, #75 (`avalon rebuild-index` /
`avalon_server::rebuild::rebuild_index_from_ledger`).

```bash
avalon rebuild-index
```

Expected observation: `rebuilt index from N ledger entries (...)`, then
repeating every `GET` from step 14 with Player A's still-valid session
token returns exactly the same identity, friends, guild membership, and
achievement — proving the projections are truly derived, not
independently authoritative state.

Found live while running this step: the rebuild logs `indexer: skipping
unrecognized event kind "..."` for several real kinds
(`game.registered`, `achievement.defined`, `guild.channel_created`,
`permission.granted`, `issuer.key_added`) — none of those happen to be
kinds step 14's own read model depends on, so the rebuild guarantee holds
for everything this walkthrough actually checks, but it means the
indexer's `rebuild` path doesn't yet have a projection for every kind the
ledger carries. Worth its own ticket under #43's umbrella rather than
something this walkthrough resolves.

## Result

Steps 1–7 were executed by hand, live, exactly as shown above. Steps
8–16 were verified live via `crates/cli/tests/milestone_1_walkthrough.rs`,
which drives the identical HTTP/SDK/CLI calls end to end against this same
server and database and passed on this run. That file also has a
companion test, `tampering_a_ledger_entry_breaks_the_chain`, proving step
15's chain-intact check actually catches a tampered ledger row — run
against its own throwaway Postgres *schema* inside the same database
(never a separate database: this sandbox's `avalon` Postgres role has no
`CREATEDB` privilege, a real and correct restriction found live while
writing this test, not a bug) and never against the shared dev ledger,
since corrupting a real row there would permanently break every later
entry's chain for every other session sharing this database.

Both tests are `--ignored` and run under `make test-live` (needs
`AVALON_SERVER_URL`, `AVALON_WEBAUTHN_ORIGIN`, `DATABASE_URL`, and
`AVALON_SETTLEMENT_SIGNING_KEY` in the environment — `.env`'s values work
when exported, same as every other `crates/sdk/tests/*.rs` live test in
this repo).
