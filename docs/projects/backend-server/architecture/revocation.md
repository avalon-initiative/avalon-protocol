# Revocation

**Revocation adds history; it never erases it.** An attestation that was issued
and later revoked leaves two facts in durable history — "Integrator A issued it" and
"Integrator A later revoked it" — and both stay visible forever. The same applies at
the issuer level: suspending or revoking an issuer is an appended entry, not a
deletion of everything it ever signed.

Narrative context: [`../stakeholders/Proposal.md` §9](../../../stakeholders/Proposal.md#9-trust-and-attestations)
and the durable-history rule in
[ADR #75](https://github.com/LunarVagabond/avalon-protocol/issues/75).

## Individual revocation

```text
achievement.issued          (Integrator A, key k1, 2027-03-14)
    Dragon Slayer → User X
        │
        ▼
achievement.revoked         (Integrator A, key k1, 2027-05-02)
    references the attestation above
    reason_code: cheating
    reason:      "…"
```

Current state, as a projection:

```text
User X — Dragon Slayer (Ashen Realms)
    status:  REVOKED
    issued:  2027-03-14
    revoked: 2027-05-02 — cheating
```

The original issuance remains observable. The Hub shows both, not an empty
slot ([`./hub.md`](../../hub/architecture/hub.md)).

## Reason code vocabulary and visibility (#534)

Not every revocation reason means the same thing for what should stay
visible. `reason_code` is `avalon_protocol::revocation::RevocationReasonCode`
— a real, extensible vocabulary (not a free-text string), where each known
code carries a default visibility policy:

| Reason code | Meaning | Stays visible after revocation? |
|---|---|---|
| `cheating` | A real fact about the subject — they earned it, then had it pulled for cause. | **Yes** — both `issued` and `revoked` stay in current-state views (the worked example above). |
| `policy_change` | Not about the subject at all — a ruleset/definition changed after the fact. The historical fact of having earned it under the old policy still matters. | **Yes**. |
| `mistake` | An operator/integrator error — this should never have existed as its own entry. Showing "subject had X, then had it revoked" is actively misleading. | **No** — disappears from current-state views entirely, as if never issued. |
| `duplicate` | A duplicate of another still-valid claim. Same "shouldn't have existed on its own" shape as `mistake`. | **No**. |

**Worked example, hidden case.** A developer accidentally publishes an
achievement to production that should never have gone out:

```text
achievement.issued          (Integrator A, key k1, 2027-04-01)
    Beta Tester Badge → User Y
        │
        ▼
achievement.revoked         (Integrator A, key k1, 2027-04-02)
    references the attestation above
    reason_code: mistake
    reason:      "published to prod by mistake, never should have gone out"
```

`GET /attestations/{id}` (a direct lookup by an id the caller already has —
not a "browsing" query) still shows both `issued` and `revoked` entries,
full history, unchanged — raw history is never filtered regardless of
reason code, per this document's own opening rule. `GET /me/achievements`
(a current-state, *listing*-shaped read) simply never includes this
attestation at all, as if User Y never had it — not shown as revoked, not
shown at all.

**Only known codes get a defined policy; an unrecognized code defaults to
visible.** `RevocationReasonCode` mirrors `ProtocolEventKind`'s own
`Known`/`Other` open-enum shape (#82) — a code this build doesn't
recognize (including every revocation recorded before #534 existed, e.g.
historical `"cheating_detected"` strings) decodes to `Other` rather than
an error, and `Other`'s `hides_after_revocation()` is `false`. Staying
visible is the safe default to fail toward; silently hiding history
because a code wasn't recognized would be the wrong one. Adding a new
known code later (with its own visibility policy) is purely additive —
it never changes what an already-recorded reason code means.

## Issuer-level suspension and revocation

```text
issuer.suspended   (network, 2027-06-01)   →  issuer.reinstated (2027-06-20)
issuer.revoked     (network, 2028-01-10)
```

Neither event deletes historical claims. A consuming integrator reads the timestamps
and applies its own policy — reject new claims and honor historical ones,
reject everything, or something in between
([`./trust-model.md`](./trust-model.md)). The network records; it does not
decide for the consumer.

## Point-in-time validity

Validity is a function of history and a timestamp, not a flag on a row:

```text
valid(attestation, history, at) =
    attestation not revoked or superseded as of `at`
  ∧ issuer not suspended/revoked as of `at`   (per consumer policy)
  ∧ signing key valid as of attestation.issued_at
  ∧ attestation well-formed for its schema/version
  ∧ attestation not expired as of `at`
```

"Valid at issuance" and "valid now" are different questions with different
answers, and both must be answerable — see
[`./issuers.md`](./issuers.md) for the key half.

## What is being replaced

`AchievementAttestation.revoked_at: Option<OffsetDateTime>` in
`crates/protocol/src/achievements.rs` is a mutable field on the original record.
That is an edit to history, not an addition to it. Its `is_valid(now)` also
treats a *future-dated* `revoked_at` as "still valid until then", a
scheduled-revocation semantic nobody asked for. Both go away under #85: the
projection may cache a current status, but the record is the chain of entries.

## Who may revoke

Only the original issuer, signing with a key valid at revocation time (or a
legitimate successor key). Never the node operator. Never the subject. An
issuer-level suspension is a separate, explicitly-authorized network operation
with its own audit trail — see
[`./security-model.md`](./security-model.md).

## Scenarios

**C — Integrator A revokes Dragon Slayer.** History shows issued + revoked. The
Hub shows both. Integrator B's validity check flips at the revocation timestamp.
Rebuilding the projection from history reproduces the same status.

**F — Integrator A's signing key is compromised.** Integrator A (or the network, per
#80) revokes the key as of time T. Claims signed at T−1 stay authentic and
valid; claims signed at T+1 are rejected. Nothing historical is destroyed.

## Mechanics

Settled: the invariant above, and — as of
[#81](https://github.com/LunarVagabond/avalon-protocol/issues/81) (decided) —
the mechanics too: a signed revocation entry (reason code, timestamp, signed
by an issuer key valid at revocation time), supersession (an issuer can
replace an attestation rather than revoke-and-reissue), reinstatement (a
later entry can reverse a revocation — validity computed by walking history,
never read from a flag), and issuer-level suspend/revoke as their own
events. Consumer policy under issuer revocation stays the consumer's choice;
no network-wide default is imposed. Implementation (replacing `revoked_at`
with these entries) is [#85](https://github.com/LunarVagabond/avalon-protocol/issues/85),
still open.

## Today in the repo

- **Individual revocation, landed (#85), scoped to exactly scenario C.**
  `avalon_protocol::achievements::AttestationStatus { Active, Revoked }` —
  computed by [`attestation_status_at`], never a mutable field on the
  attestation (`revoked_at` was already removed from
  `AchievementAttestation` by #32, ahead of this ticket). `validity()`
  (#33) now takes the computed `AttestationStatus` alongside the issuer's
  `IntegratorStatus`. `POST /attestations/{id}/revoke`
  (`crates/server/src/attestations.rs`) inserts an append-only row into a
  brand-new `attestation_revocations` table — `achievement_attestations`
  itself is never touched — requires the caller to authenticate as the
  attestation's *original* issuer, and additionally verifies an embedded
  signature over `revocation_signing_bytes` against that issuer's key
  history at revocation time
  (`avalon_chain::attestations::verify_signature`, the same generic core
  `verify_authenticity` uses — one cryptographic check backing both
  issuance and revocation, not two). Emits
  `achievement.revoked`/`milestone.revoked`. `GET /attestations/{id}`'s
  `history` array now shows both `issued` and (if applicable) `revoked`
  entries with their reason. Verified live end to end, including
  scenario C itself (`crates/server/tests/attestation_revocations.rs`):
  before-revoke reads `valid`/1-entry history, after-revoke reads
  `invalid`/2-entry history with the revocation's reason attached,
  authenticity stays `authentic` throughout (revocation doesn't retroactively
  un-sign anything) — plus double-revocation and wrong-issuer-revokes
  rejection.
- **Not built in this pass, honestly**: supersession and reinstatement.
  Neither has a catalogued protocol event kind yet
  (`docs/architecture/protocol-events.md` lists `achievement.revoked` and
  `attestation.superseded`, but no attestation-level "reinstated" kind at
  all), and neither is required by scenario C. `attestation_revocations`
  is deliberately capped at one row per attestation
  (`UNIQUE (attestation_id)`) precisely because there's no reinstatement
  path to need more than one yet — lifting that constraint is what
  building reinstatement will need to do.
- Issuer-level suspend/revoke/reinstate/deprecate transitions (a separate
  axis from attestation revocation) remain exactly where #84 left them:
  the `IntegratorStatus` variants exist and `validity()` already reads whichever
  one is set, but nothing in this repo can transition an issuer into any
  of them yet — that authorization model is still open, tracked
  separately (see [issuers.md](issuers.md)).
- `crates/protocol/src/permissions.rs` — `PermissionGrant.revoked_at` still
  uses the mutable-field shape; grants are not promised-durable protocol
  history (a projection concern, not covered by #81's ruling), left
  untouched by this pass.
- **Entity/instance deletion, landed (#533).** The same "append, never
  erase" standard applied to Integrator Space instance data
  ([`./integrator-space.md`](./integrator-space.md)) — a deleted
  character (or any other integrator-published instance) gets a
  `game_data.deleted` tombstone event (`crates/server/src/integrator_data.rs::delete_instance`)
  referencing the original instance by id, never a physical delete or a
  mutation of the original `instance`/`published_at` fields. Differs
  slightly in mechanics from achievement revocation above: rather than a
  separate `attestation_revocations`-style table, the tombstone sets
  `deleted_at`/`delete_reason_code`/`delete_reason` columns directly on
  `integrator_data_instances`' own row — the same shape this table
  already used for `superseded_by` before #533 existed, so this reuses an
  established local pattern rather than introducing a second one. The
  substantive content (`instance`, `published_at`) is never touched
  either way; only a lifecycle marker changes. `GET
  /identities/{id}/integrator-data` filters `deleted_at IS NULL`, same
  posture as its existing `superseded_by IS NULL` filter — a deleted
  instance simply stops appearing there while staying fully observable in
  raw ledger history and the indexer's own row. Live-verified end to end,
  including a full rebuild from `ledger_entries` alone reproducing the
  tombstoned projection state byte-for-byte
  (`crates/server/tests/rebuild_from_events.rs::rebuild_reproduces_integrator_data_deletion`).
  **Scoping decision, made while building #534**: `GameDataDeletedPayload.reason_code`
  deliberately stays a free-text string, not
  `avalon_protocol::revocation::RevocationReasonCode`. #534's vocabulary
  (`cheating`/`mistake`/`duplicate`/`policy_change`) describes *why an
  issuer revoked something it issued* — a judgment call about validity.
  Instance deletion here is the opposite shape: the *subject themselves*
  choosing to delete their own published data (a character retired, an
  account cleaned up), which isn't a validity judgment at all and doesn't
  map onto that vocabulary. Always stays visible-with-a-marker either way
  (see above) — there's no hidden-vs-visible question for this event to
  answer, so there's nothing for the enum to gain here beyond what a
  free-text reason already gives.

## Decisions and tickets

- [#75](https://github.com/LunarVagabond/avalon-protocol/issues/75) — ADR:
  history is append-only.
- [#81](https://github.com/LunarVagabond/avalon-protocol/issues/81) —
  decision, closed: revocation mechanics (revocation entries, supersession,
  reinstatement, issuer-status events), described above.
- [#85](https://github.com/LunarVagabond/avalon-protocol/issues/85) —
  implementation: replaced `revoked_at` with revocation entries for
  individual attestations (landed, scenario C only — see above).
  Supersession, reinstatement, and issuer-status transitions remain open
  follow-up work, not solved by this pass.
- [#80](https://github.com/LunarVagabond/avalon-protocol/issues/80) —
  decision, closed: issuer keys (scenario F) — see
  [issuers.md](issuers.md).
