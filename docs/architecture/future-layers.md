# Future Layers: Portable Assets and Economy

Two phases sit deliberately after the network, the SDK, and external
integrations have proven themselves. **Portable assets are Phase 4. Economic
interoperability is Phase 5. Neither is foundational, and neither has tickets
yet.** What this document does is state what today's design must not preclude,
so that when the time comes nothing has to be torn out.

## Portable assets (Phase 4)

[`../stakeholders/Proposal.md#16-portable-assets`](../stakeholders/Proposal.md#16-portable-assets) and
[`../stakeholders/Proposal.md#26-phase-4--portable-assets`](../stakeholders/Proposal.md#26-phase-4--portable-assets).

Ownership and functionality are separate. Provenance can be portable while
behavior stays game-specific:

```text
Asset
    ID:               game:ashen-realms:asset:legendary-sword:<serial>
    Source game:      Ashen Realms
    Original issuer:  Ashen Realms
    Originally to:    Identity Y
    Transfers:        Y -> Z, Z -> X
    Current owner:    Identity X

Ashen Realms:   a powerful weapon
Game B:         recognized as the "Champion Blade" cosmetic
Game C:         not recognized at all
```

All three games are behaving correctly. The asset's identity and history
survive; what it *does* is each game's decision, exactly as with achievements
([`./trust-model.md`](./trust-model.md)).

Boundaries that hold from day one:

- **Not every item is an Avalon asset.** A game decides which of its items, if
  any, are Avalon-portable. Game-specific inventory stays in the game database.
- **Provenance is first-class.** Who issued it, where it originated, when, who
  owns it now, whether it has been revoked or is still valid, and whether a
  consuming game recognizes it — the same questions as
  [`./provenance.md`](./provenance.md).
- **Games have scoped authority.** Game A can issue Game A assets. It cannot
  rewrite Game B's, and it cannot alter ownership history.
- **Standardized asset schemas are an open question**, listed in
  [`../stakeholders/Proposal.md#32-open-questions`](../stakeholders/Proposal.md#32-open-questions)
  and generalized beyond assets to game data broadly in
  [`./game-space.md`](./game-space.md), tracked by
  [#181](https://github.com/LunarVagabond/avalon-protocol/issues/181). A
  schema reference on the asset, like the one attestations carry, is the likely
  shape; a universal item format is not.

## Economy (Phase 5)

[`../stakeholders/Proposal.md#15-economy-and-currency`](../stakeholders/Proposal.md#15-economy-and-currency)
and [`../stakeholders/Proposal.md#27-phase-5--economy`](../stakeholders/Proposal.md#27-phase-5--economy).

A universal cryptocurrency, currency, item market, or financial layer is not
the foundation of Avalon, and Avalon must deliver value without one. The reasons
are concrete: speculation, volatility, whales, market manipulation, regulatory
and tax exposure, AML/KYC obligations, minors interacting with financial
systems, disruption of game economies, games turning into financial products,
and incentive attacks on everything the network measures
([`./game-registry.md`](./game-registry.md)).

If economic interoperability is ever introduced, the developer-facing shape is
an abstraction, not a wallet:

```rust
store.purchase(player, "premium_mount").await?;
```

with no smart contracts, chain transactions, signing, or settlement visible to
the game. That is the same principle as the rest of the SDK
([`./sdk.md`](./sdk.md)). Economic infrastructure is introduced only after the
network demonstrates real utility, and only if it serves the ecosystem rather
than defining it.

## What today's design must not preclude

- **Asset provenance as attestations plus ownership events.** Issuance is an
  attestation by the source game; each transfer is a durable, signed protocol
  event referencing the previous owner. The event pipeline, batching, and
  revocation model already support this shape
  ([`./protocol-events.md`](./protocol-events.md),
  [`./revocation.md`](./revocation.md)); nothing in `protocol` should assume the
  only attestable thing is an achievement.
- **Namespaced asset ids.** `GlobalId` (`crates/protocol/src/ids.rs`) already
  namespaces by owner and kind; assets get a kind, not a new id scheme.
- **Recognition policies that scope by asset class.** The trust model's scoping
  dimensions include asset class and currency claims from the start, even
  though nothing issues them yet.
- **No protocol-level currency assumptions.** Nothing in identity, guilds, or
  attestations references a balance, a wallet, or a price. The `wallet.read` /
  `wallet.write` capability names in
  [`../stakeholders/Proposal.md#13-permission-model`](../stakeholders/Proposal.md#13-permission-model) are
  reserved names, not a commitment to build them.

## Today in the repo

- No `assets` module, type, event, or ticket. Per
  [#69](https://github.com/LunarVagabond/avalon-protocol/issues/69), when one
  exists it is `crates/protocol/src/assets.rs`, not a new crate.
- `GlobalId` and `AchievementAttestation` in `crates/protocol/src/` are the
  patterns an asset issuance would follow.
- No economic primitive of any kind.

## Decisions and tickets

- [#75](https://github.com/LunarVagabond/avalon-protocol/issues/75) ownership
  and provenance are in the must-reconstruct set once they exist
- [#76](https://github.com/LunarVagabond/avalon-protocol/issues/76) recognition
  is scoped and consumer-decided — applies to assets unchanged
- [#67](https://github.com/LunarVagabond/avalon-protocol/issues/67) game
  inventory stays with the game; portability is opt-in per item
- No open tickets; filed when Phase 4 is in reach, not before.
