# Trust Model

**Authenticity, validity, and recognition are three separate questions, and
Avalon never collapses them.** A signature proves who signed a claim; it does
not prove the claim is meaningful. **Authenticity is universal. Recognition is
contextual.** Consuming games choose what they recognize; the network never
chooses for them.

This is the normative statement of
[ADR #76](https://github.com/LunarVagabond/avalon-protocol/issues/76).
Narrative: [`../Proposal.md` §9](../Proposal.md#9-trust-and-attestations).

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

**Recognized** — does a particular consuming game choose to honor this claim?
Entirely contextual, expressed as that game's own policy. Two games can look at
the same authentic, valid claim and decide differently, and both are correct.

| | Who decides | Varies by observer? | Where it lives |
|---|---|---|---|
| authentic | cryptography | no | signature + issuer key history |
| valid | protocol history | no | revocation / issuer-status entries |
| recognized | the consumer | yes | the consumer's policy |

## The limitation, stated plainly

There is no cryptographic mechanism that can stop a game from issuing a
meaningless but authentic claim. If Game C controls its own issuer key and
signs `Dragon Slayer` for every player who clicks a button, the signature
proves Game C issued it. It cannot prove Game C made it difficult, fair, or
prestigious. Avalon does not claim otherwise, and no future feature should
imply it does.

## Scoped trust policies

Recognition is a consumer-side policy, scoped however the consumer needs:

```text
Game B — recognition policy

  issuer game:ashen-realms
      achievements            RECOGNIZED
      tournament results      RECOGNIZED
      asset provenance        RECOGNIZED
      currency claims         NOT RECOGNIZED

  issuer game:worldzero
      achievements            RECOGNIZED  (schema achievement.v1+, since 2027-01)
      tournament results      RECOGNIZED  (tournament:avalon-championship-* only)

  issuer game:random-mmo-47
      everything              NOT RECOGNIZED
```

Scoping dimensions: issuer, claim type, achievement schema and version, time
period, specific tournament, asset class, or any other dimension the consumer
cares about. Avalon may offer standard policy mechanisms later; it never
imposes a network-wide trust list.

## Under issuer suspension or revocation

When an issuer is suspended or revoked ([`./revocation.md`](./revocation.md)),
the protocol exposes the facts and their timestamps. The consumer chooses:
reject new claims but honor historical ones, reject everything from that
issuer, or something else. No default is imposed.

## Statistics inform; they never determine

The [game registry](./game-registry.md) publishes derived facts — observed
players, attestations issued, games that publicly recognize an issuer. A
consumer may write "accept tournament results from issuers with at least N
observed players and M recognizing games". That is the consumer's rule. Avalon
never enforces "games above N are trusted", never publishes a composite game
score, and never turns the recognition graph into a verdict. A ten-player game
is not automatically malicious; a ten-million-player game is not automatically
trustworthy.

## Recognition relationships are facts

A game may publish its policy: "Game A recognizes Game B's achievements and
tournament results." Avalon records that as a fact, and the registry can show
it. A claim can be valid without being recognized by anyone, and recognized
without being especially meaningful. Recognition is never the same thing as
validity.

## Today in the repo

- `crates/protocol/src/achievements.rs` — `TrustRelationship { truster,
  trusted_issuer, established_at }`: one unscoped issuer-level bit. #33 grows
  it into scoped policy.
- `AchievementAttestation.proof` — opaque bytes; no verification exists, so
  "authentic" cannot yet be computed. Depends on issuer keys (#80, #84).
- `AchievementAttestation::is_valid(now)` — checks only `revoked_at`; the
  full validity computation over history is #85.
- `crates/sdk/src/lib.rs` — no verification surface yet.

## Decisions and tickets

- [#76](https://github.com/LunarVagabond/avalon-protocol/issues/76) — ADR:
  attestation trust model.
- [#33](https://github.com/LunarVagabond/avalon-protocol/issues/33) — verify
  attestation + scoped trust relationships.
- [#80](https://github.com/LunarVagabond/avalon-protocol/issues/80) —
  decision, open: issuer signing keys and lifecycle (the authenticity input).
- [#81](https://github.com/LunarVagabond/avalon-protocol/issues/81) —
  decision, open: revocation mechanics (the validity input).
- [#89](https://github.com/LunarVagabond/avalon-protocol/issues/89) — registry
  read model, including recognition relationships.
