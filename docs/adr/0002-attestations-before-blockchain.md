# ADR 0002: Attestations Before Blockchain, Chain Architecture Deferred

**Status:** Open (settlement approach for milestone 1 is decided; chain architecture is not)

## Context

Avalon needs a durable, verifiable record of protocol facts — achievements issued,
guild membership changes, asset ownership transfers — that a receiving game can
independently verify without trusting a central database blindly. The first
instinct is "put it on a blockchain." `PROMPT.md` (§5, §6, §14, §22, §23) and
`Proposal.md` (§14, Guiding Principle 7) both argue against that for milestone 1:
real-time gameplay data must never touch a chain, and even durable protocol facts
don't need one from day one — they need to *behave like* a verifiable ledger
(tamper-evident, independently verifiable), which a signed, append-only Postgres
table already provides.

Mid-design, a chain-first framing was raised directly ("avalon-protocol is going to
be the blockchain we use to carry player ID and enable auth"), including whether to
build a fully custom appchain (own consensus, block/DAG production, P2P networking
— PROMPT.md §23 raises Kaspa-style BlockDAG as a research reference, not a
decision) from day one. That direction was reversed back to the deferred approach
below, specifically *because* it needs to stay swappable later — hence this ADR
exists to record the swap point as a first-class open decision, not something
settled by default.

## Decision (settled)

Milestone 1 uses a signed, append-only ledger — not a blockchain:

```text
Game
  │ high-volume events
  ▼
Event Buffer (filter/aggregate)
  ▼
Durable Protocol Events
  ▼
Batch → Commitment
  ▼
SettlementProvider (Postgres-backed impl for now)
```

The `chain` crate defines a `SettlementProvider` trait (`commit(batch)`,
`verify(commitment)`, `get_commitment()`) and ships exactly one implementation: a
Postgres-backed store of signed batches. No consensus, no P2P networking, no block
production in milestone 1.

## Decision (open — needs its own follow-up decision before committing)

What `SettlementProvider` implementation Avalon eventually moves to:

- **Build our own appchain** — full control (consensus, block/DAG production, node
  software), full complexity. Requires the research PROMPT.md §23 calls for before
  picking an architecture (DAG/BlockDAG, sharding, parallel execution, etc.) —
  explicitly not to be chosen "merely because it is fashionable."
- **Anchor to an existing high-throughput chain or L2** — far less scope, but ties
  Avalon's settlement guarantees to an external chain's roadmap and economics.
- **Stay Postgres-backed longer than expected** — if adoption never demands more
  than what a signed ledger provides, there may be no forcing function to build or
  adopt a chain at all.

This is deliberately not decided here. It is tracked as open so scaffolding and
early implementation treat `SettlementProvider` as a real trait boundary — not
something any crate outside `chain` reaches around — so that whichever answer
eventually wins, the swap doesn't ripple into `protocol`, `server`, `sdk`, or any
game integration.

## Consequences

- `chain`'s public surface is the `SettlementProvider` trait plus the Postgres
  implementation. Nothing else in the workspace depends on Postgres directly for
  settlement.
- Revisit this ADR (update to "Decided") once the chain-architecture research is
  actually done — not before adoption creates pressure to.
