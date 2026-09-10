# Revocation

**Revocation adds history; it never erases it.** An attestation that was issued
and later revoked leaves two facts in durable history — "Game A issued it" and
"Game A later revoked it" — and both stay visible forever. The same applies at
the issuer level: suspending or revoking an issuer is an appended entry, not a
deletion of everything it ever signed.

Narrative context: [`../stakeholders/Proposal.md` §9](../stakeholders/Proposal.md#9-trust-and-attestations)
and the durable-history rule in
[ADR #75](https://github.com/LunarVagabond/avalon-protocol/issues/75).

## Individual revocation

```text
achievement.issued          (Game A, key k1, 2027-03-14)
    Dragon Slayer → Player X
        │
        ▼
achievement.revoked         (Game A, key k1, 2027-05-02)
    references the attestation above
    reason_code: cheating_detected
    reason:      "…"
```

Current state, as a projection:

```text
Player X — Dragon Slayer (Ashen Realms)
    status:  REVOKED
    issued:  2027-03-14
    revoked: 2027-05-02 — cheating_detected
```

The original issuance remains observable. The Hub shows both, not an empty
slot ([`./hub.md`](./hub.md)).

## Issuer-level suspension and revocation

```text
issuer.suspended   (network, 2027-06-01)   →  issuer.reinstated (2027-06-20)
issuer.revoked     (network, 2028-01-10)
```

Neither event deletes historical claims. A consuming game reads the timestamps
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
[`./games-and-issuers.md`](./games-and-issuers.md) for the key half.

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

**C — Game A revokes Dragon Slayer.** History shows issued + revoked. The
Hub shows both. Game B's validity check flips at the revocation timestamp.
Rebuilding the projection from history reproduces the same status.

**F — Game A's signing key is compromised.** Game A (or the network, per
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

- `crates/protocol/src/achievements.rs` — `revoked_at` field and
  `is_valid(now)`, both slated for replacement.
- `crates/protocol/src/permissions.rs` — `PermissionGrant.revoked_at` uses the
  same mutable-field shape; grants are not promised-durable protocol history,
  so this is a projection concern, but it should follow the same pattern #81
  settled once #85 implements it.
- No revocation events, endpoints, or issuer-status types exist.

## Decisions and tickets

- [#75](https://github.com/LunarVagabond/avalon-protocol/issues/75) — ADR:
  history is append-only.
- [#81](https://github.com/LunarVagabond/avalon-protocol/issues/81) —
  decision, closed: revocation mechanics (revocation entries, supersession,
  reinstatement, issuer-status events), described above.
- [#85](https://github.com/LunarVagabond/avalon-protocol/issues/85) —
  implementation: replace `revoked_at` with revocation entries.
- [#80](https://github.com/LunarVagabond/avalon-protocol/issues/80) —
  decision, open: issuer keys (scenario F).
