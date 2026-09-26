# Rotating the settlement signing key

How to replace `AVALON_SETTLEMENT_SIGNING_KEY` — the key that signs this
node's Signed Tree Heads (STHs) — without breaking a mirror's ability to
verify this node's history. This covers routine rotation and the "an old
key is no longer trusted" mechanics; it does not cover *how you decide* a
key is compromised in the first place (see
[`equivocation-response.md`](equivocation-response.md) for that judgment
call) or alerting/paging (not built yet).

This is the settlement/log operator key specifically (`crates/protocol/src/sth.rs`)
— a different key domain from issuer keys or user keys. Nothing here
rotates those.

## Why this needs a procedure at all

`crates/protocol/src/sth.rs`'s `signing_key_id` already lets a single STH name
which key generation signed it, so historical STHs stay verifiable under
the old key forever — rotating the active key never requires
re-signing or invalidating anything already committed. The part that isn't
free: **every mirror watching this node loads exactly one
`AVALON_SETTLEMENT_VERIFY_KEY` from its own environment**
(`mirror_watcher::run`) and uses it to verify every STH it observes from
every peer, regardless of that STH's `signing_key_id`. A mirror has no way
to learn a new verify key from the network itself — until a mirror's own
`AVALON_SETTLEMENT_VERIFY_KEY` is updated out of band, every STH signed
under the new key fails that mirror's verification and the mirror-watcher
stops advancing for this network. Rotation is coordination, not just a
local config change.

## Single-node deployment (no mirrors watching this node)

1. Generate a new key: `python3 -c 'import secrets; print(secrets.token_hex(32))'`
   (the same generator `make stack-up` uses).
2. Choose a new `AVALON_SETTLEMENT_SIGNING_KEY_ID`, distinct from the
   current one — e.g. bump a trailing generation number
   (`settlement-operator-1` → `settlement-operator-2`).
3. Update `.env`: set `AVALON_SETTLEMENT_SIGNING_KEY` to the new seed and
   `AVALON_SETTLEMENT_SIGNING_KEY_ID` to the new id. If
   `AVALON_SETTLEMENT_VERIFY_KEY` is set explicitly (rather than left to
   derive from the signing key — see the "single-operator dev convenience"
   note in `sth.rs`), update it to match the new key's public half.
4. Restart `avalon-server` (`make restart`, or the equivalent for however
   this node is deployed). The next batch commit signs under the new key
   and `signing_key_id`; every STH before it keeps naming the old id and
   stays verifiable exactly as before.
5. Securely destroy the old private key material once you've confirmed the
   new one is signing correctly — don't leave both live indefinitely.

## Mirrored pair (this node has peers watching it, or watches peers)

Order matters here specifically because of the single-verify-key
limitation above:

1. **Before rotating**, get the new key's public (verify) half to every
   operator who mirrors this node, out of band (not over the network this
   key secures — a side channel: direct message, a signed announcement in
   whatever channel operators already coordinate through, etc.).
2. **Every mirror updates its own `AVALON_SETTLEMENT_VERIFY_KEY` first**,
   restarts, and confirms via `avalon inspect-ledger` (or its own logs)
   that it's still cleanly verifying this node's *current* (pre-rotation)
   STHs — a mirror only ever holds one verify key at a time today, so this
   step alone doesn't break anything as long as the authority hasn't
   switched keys yet.

   This node currently has no built-in way to hold two valid verify keys
   during a transition window either — if a mirror updates its verify key
   before the authority rotates, or the authority rotates before every
   mirror has updated, that mirror's verification fails until both sides
   agree. For a small, coordinated operator set this means picking a
   maintenance window and confirming everyone is ready before step 3, not
   relying on the protocol to make this safe automatically.
3. Once every mirror confirms readiness, perform steps 1–4 from the
   single-node procedure above on the authority node.
4. Confirm every mirror resumes advancing (`avalon list-equivocations` for
   the network should show no new unresolved findings, and the
   mirror-watcher's logs should show successful `backfill` ticks past the
   rotation point).

## Emergency rotation (suspected compromise)

Same mechanics as above, but skip the leisurely coordination window:
notify every mirror operator immediately, treat the interval between
"key may be compromised" and "every mirror has the new verify key" as an
active incident, and consider whether STHs signed after the suspected
compromise point need manual review before mirrors treat them as trusted.
If trust in the network's history up to that point cannot be
re-established at all, a fresh `AVALON_NETWORK_ID` genesis is the fallback
of last resort — see `docs/projects/backend-server/architecture/self-hosting.md` on why
`network_id` is load-bearing, not just a label.
