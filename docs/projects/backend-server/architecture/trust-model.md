# Trust Model

**Authenticity, validity, and recognition are three separate questions, and
Avalon never collapses them.** A signature proves who signed a claim; it does
not prove the claim is meaningful. **Authenticity is universal. Recognition is
contextual.** Consuming integrators choose what they recognize; the network never
chooses for them.

Narrative: [`../stakeholders/Proposal.md` §9](../../../stakeholders/Proposal.md#9-trust-and-attestations).

This trust model is domain-agnostic — it applies to any issuer and consumer of
attestations, not only integrators — and is illustrated below with gaming examples
because gaming is Avalon's first live use case.

## The three questions

**Authentic** — did the claimed issuer actually issue this? Established by the
issuer's signature over the attestation, verified against the issuer's
registered key that was legitimate at the time of issuance. The answer is the
same for every observer.

**Valid** — is the attestation currently in force? Not revoked, not
superseded, not issued by an issuer that was suspended or revoked at the
relevant time, not signed by a key already revoked at issuance, well-formed
against its declared schema and version, not expired. Also universal, computed
from protocol history at a point in time.

**Recognized** — does a particular consuming integrator choose to honor this claim?
Entirely contextual, expressed as that integrator's own policy. Two integrators can look at
the same authentic, valid claim and decide differently, and both are correct.

| | Who decides | Varies by observer? | Where it lives |
|---|---|---|---|
| authentic | cryptography | no | signature + issuer key history |
| valid | protocol history | no | revocation / issuer-status entries |
| recognized | the consumer | yes | the consumer's policy |

## The limitation, stated plainly

There is no cryptographic mechanism that can stop an integrator from issuing a
meaningless but authentic claim. If Integrator C controls its own issuer key and
signs `Dragon Slayer` for every user who clicks a button, the signature
proves Integrator C issued it. It cannot prove Integrator C made it difficult, fair, or
prestigious. Avalon does not claim otherwise, and no future feature should
imply it does.

## Scoped trust policies

Recognition is a consumer-side policy, scoped however the consumer needs:

```text
Integrator B — recognition policy

  issuer game:ashen-realms
      achievements            RECOGNIZED
      integrator event results     RECOGNIZED
      asset provenance        RECOGNIZED
      currency claims         NOT RECOGNIZED

  issuer game:worldzero
      achievements            RECOGNIZED  (schema achievement.v1+, since 2027-01)
      integrator event results     RECOGNIZED  (integrator_event:avalon-championship-* only)

  issuer game:random-mmo-47
      everything              NOT RECOGNIZED
```

Scoping dimensions: issuer, claim type, achievement schema and version, time
period, specific integrator event, asset class, or any other dimension the consumer
cares about. Avalon may offer standard policy mechanisms later; it never
imposes a network-wide trust list.

## Under issuer suspension or revocation

When an issuer is suspended or revoked ([`./revocation.md`](./revocation.md)),
the protocol exposes the facts and their timestamps. The consumer chooses:
reject new claims but honor historical ones, reject everything from that
issuer, or something else. No default is imposed.

## Statistics inform; they never determine

The [integrator registry](./registry.md) publishes derived facts — observed
identities, attestations issued, integrators that publicly recognize an issuer. A
consumer may write "accept integrator event results from issuers with at least N
observed identities and M recognizing integrators". That is the consumer's rule. Avalon
never enforces "integrators above N are trusted", never publishes a composite integrator
score, and never turns the recognition graph into a verdict. A ten-player integrator
is not automatically malicious; a ten-million-player integrator is not automatically
trustworthy.

## Recognition relationships are facts

An integrator may publish its policy: "Integrator A recognizes Integrator B's achievements and
integrator event results." Avalon records that as a fact, and the registry can show
it. A claim can be valid without being recognized by anyone, and recognized
without being especially meaningful. Recognition is never the same thing as
validity.

## Trust in the log itself is a separate question

Authenticity, validity and recognition are about claims made *in* the log. Whether the
log's own head can be trusted, and whether a fork would be caught, is a different
question and has its own mechanism: independent witnesses cosign heads they have checked
for append-only growth, each verifier keeps its own bounded known list of them, and two
conflicting majority-cosigned heads must share a witness, which is proof of equivocation
from signatures alone. That is resistance to a captured or rewriting operator, not a
proof, and it does not decide what any consumer recognizes. See
[`witness-cosigning.md`](./witness-cosigning.md) and
[`network-trust-anchors.md`](./network-trust-anchors.md).

## Current implementation

- **Authentic** — `avalon_chain::attestations::verify_authenticity`
  (a `chain` crate module, alongside `sth::verify_tree_head` — both do real
  Ed25519 verification, which is why this lives in `chain`, not the
  I/O-free `protocol` crate): resolves the signing key against the
  issuer's full key history at `issued_at` (`resolve_valid_signing_key`)
  and checks the signature over `attestation_signing_bytes`. Returns
  `Authentic { key_id }` / `NotAuthentic { reason }`, never a bare bool.
  The same function backs both the issuing endpoint's own check and the
  public read below — never duplicated per call site. Unit-tested directly
  against scenarios E and F's authenticity half (a claim stays authentic
  after its key is later revoked; a claim "issued" after revocation is not
  authentic).
- **Valid, still honestly partial** —
  `avalon_protocol::achievements::validity(issuer_status, attestation_status)`
  is `Invalid` immediately if the attestation's own `AttestationStatus`
  (computed by `attestation_status_at(revoked_at, at)` — never a stored
  flag) is `Revoked`, otherwise `Valid` iff the issuer's current
  `IntegratorStatus` is `Active`. Individual-attestation revocation
  (scenario C) is real. Still deliberately less than the full design
  above: no supersession check (no protocol event kind exists yet for it),
  no point-in-time issuer-status history (nothing records *when* a status
  changed, only what it is now — so an issuer suspended today reads as
  suspended for claims issued before the suspension too, not just after),
  no schema/version well-formedness check. Real, but a strict subset of
  "is this attestation currently in force."
- **Recognized** — `TrustRelationship` carries `scopes: Vec<RecognitionScope>`
  (`claim_kind`/`schema`/`min_version`/`issued_after`, `Some` narrows,
  `None` matches anything; an empty `scopes` list is the unscoped case) and
  `recognize(policy, issuer, claim_kind, schema, version, issued_at) ->
  Recognition` (`Recognized`/`NotRecognized { reason }`) — pure, I/O-free,
  evaluated entirely on the consumer's own side per this doc's own
  "recognition is contextual" rule. Exhaustively unit-tested
  (unscoped-matches-everything, wrong issuer, and each scoping dimension
  individually plus OR-combined across multiple scopes).
- `GET /attestations/{id}` (`crates/server/src/attestations.rs`) — public,
  unauthenticated. Rebuilds the stored attestation, computes authenticity
  and validity fresh on every read (never cached, never trusted from
  storage), and returns both plus a `history` array — the issuance point,
  plus a `revoked` entry with its reason once `POST
  /attestations/{id}/revoke` has been called. **No `recognition` field,
  checked by an integration test that specifically asserts its absence**
  — matching this doc's own "recognition is the consumer's, never the
  server's, computation" rule at the wire level, not just in code
  comments. Verified live against a real Postgres, including scenario D
  (an authentic, valid claim from an issuer the reader's own policy
  doesn't name reads as `Authentic`/`Valid` from the endpoint, then
  `NotRecognized` when evaluated against that reader's own policy
  client-side) — `crates/server/tests/attestation_reads.rs`.
- **Not built yet**: `PUT /integrations/{slug}/recognition` (publishing a
  policy so the registry can show recognition relationships as facts) —
  `recognize()` exists and is fully usable by any consumer (SDK, an
  integrator's own code) today, just not yet exposed as a server-stored,
  publicly-readable declaration.
</content>
</invoke>
