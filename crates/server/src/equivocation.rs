//! Confirms a [`crate::nodes::HeadConflict`] gossip surfaced — the second
//! half of head-summary gossip's fork detection: a conflicting
//! pair of summaries is only a signal (two peers claim different roots at
//! the same `shard_id`/`tree_size`); this module fetches full cosignature
//! detail for both sides directly from the peers that reported them
//! (`GET /ledger/sth/{tree_size}?witnesses=1`, `crate::settlement`) and runs
//! `avalon_protocol::cosigned_sth::find_equivocating_witnesses` to turn that
//! signal into a proof.
//!
//! **Equivocation proof is verifiable by anyone from signatures alone.**
//! `witness_key_id` is a witness's own hex-encoded Ed25519 public key
//! (`avalon_protocol::witness::WitnessCosignature`'s own field doc
//! comment) — so the "known list" this confirmation checks majority
//! against is built directly from the keys present in the two fetched
//! heads' cosignatures, not from any node's own trusted witness list. This
//! is deliberate: the proof this module produces must stand on its own,
//! reproducible by any third party from just the two heads' signed bytes.
//!
//! Confirmed evidence is stored via `avalon_chain::mirror::
//! record_witness_equivocation_evidence` — the same durable table mirror
//! sync's own equivocation detection writes to (`crate::mirror_watcher`),
//! so a confirmed equivocation is on record exactly once regardless of
//! which path (gossip-driven confirmation here, or a mirror poll tick
//! observing two disagreeing heads directly) caught it first.

use avalon_chain::mirror::{
    record_witness_equivocation_evidence, EquivocationEvidenceKind, WitnessEquivocationEvidence,
};
use avalon_chain::PostgresSettlementProvider;
use avalon_protocol::cosigned_sth::{find_equivocating_witnesses, CosignedTreeHead};
use ed25519_dalek::VerifyingKey;
use serde::Deserialize;
use time::OffsetDateTime;

use crate::cross_shard::{pinned_core_verify_key, resolve_shard_verify_keys_from_db};
use crate::known_list::KnownListHandle;
use crate::nodes::{HeadConflict, HeadGossipTracker, PeerTable};

/// Same freshness window the design doc gives as the default
/// (`docs/projects/backend-server/architecture/witness-cosigning.md`) — how
/// far back a cosignature's `observed_at` may be and still count toward
/// this confirmation's majority check.
const CONFIRMATION_FRESHNESS_WINDOW: time::Duration = time::Duration::minutes(10);

/// Mirrors `crate::settlement::SignedTreeHeadResponse`'s wire shape —
/// duplicated rather than shared, matching this codebase's established
/// "small per-module DTO, not a shared internal type" convention
/// (`crate::cross_shard`'s own `FetchedSth`). Cosignatures ride
/// `crate::cosign_verify::WitnessCosignatureDto` directly: that type
/// deliberately omits `tree_size`/`root_hash`/`network_id`/
/// `author_created_at` (they're this struct's own fields, and every
/// cosignature in the array is over this exact head by construction), so
/// [`CosignedTreeHead`] is reassembled by binding each DTO back to the STH
/// via [`crate::cosign_verify::WitnessCosignatureDto::to_witness_cosignature`].
#[derive(Deserialize)]
struct FetchedSthWithWitnesses {
    tree_size: i64,
    root_hash: String,
    network_id: String,
    signing_key_id: String,
    signature: String,
    #[serde(with = "time::serde::rfc3339")]
    created_at: OffsetDateTime,
    #[serde(flatten)]
    envelope: avalon_protocol::signing_bytes::EnvelopeWire,
    #[serde(default)]
    cosignatures: Vec<crate::cosign_verify::WitnessCosignatureDto>,
}

impl TryFrom<FetchedSthWithWitnesses> for CosignedTreeHead {
    type Error = avalon_protocol::signing_bytes::SigningBytesError;

    fn try_from(dto: FetchedSthWithWitnesses) -> Result<Self, Self::Error> {
        let sth = avalon_protocol::sth::SignedTreeHead {
            tree_size: dto.tree_size,
            root_hash: dto.root_hash,
            network_id: dto.network_id,
            signing_key_id: dto.signing_key_id,
            signature: dto.signature,
            created_at: dto.created_at,
            envelope: dto
                .envelope
                .to_envelope(avalon_protocol::signing_bytes::tags::SETTLEMENT_STH)?,
        };
        let cosignatures = crate::cosign_verify::readable_cosignatures(&dto.cosignatures, &sth);
        Ok(CosignedTreeHead { sth, cosignatures })
    }
}

/// `source` as a base URL fit to fetch from: a `p2p://` peer or a plain http(s) URL of bounded
/// length. Its address is checked by the guarded client when the connection is made.
fn fetchable_source(source: &str) -> Result<String, String> {
    if let Some(peer) = crate::node_http::parse_p2p_base(source) {
        return Ok(crate::node_http::p2p_base_url(&peer));
    }
    crate::peer_admission::admission()
        .check_shape(source)
        .map_err(|e| format!("conflict source is not a usable base URL: {e:?}"))
}

/// Fetches full cosignature detail for `shard_id` at `tree_size` directly
/// from `source` — the fetch-on-demand half of head-summary gossip.
async fn fetch_cosigned_head(
    client: &crate::node_http::NodeClient,
    source: &str,
    shard_id: &str,
    tree_size: i64,
) -> Result<CosignedTreeHead, String> {
    let base_url = fetchable_source(source)?;
    let dto: FetchedSthWithWitnesses = client
        .get(format!("{base_url}/ledger/sth/{tree_size}"))
        .query(&[("shard_id", shard_id), ("witnesses", "1")])
        .send()
        .await
        .and_then(crate::node_http::NodeResponse::error_for_status)
        .map_err(|e| format!("{base_url}: {e}"))?
        .json()
        .await
        .map_err(|e| format!("{base_url}: {e}"))?;
    CosignedTreeHead::try_from(dto).map_err(|e| format!("{base_url}: {e}"))
}

/// The union of witness keys present (as valid hex-encoded Ed25519 public
/// keys — an entry whose `witness_key_id` doesn't parse as one is simply
/// excluded, never panics) in either head's cosignature set — see this
/// module's own doc comment for why this, rather than any node's own known
/// list, is what confirmation checks majority against.
fn known_list_from_cosignatures(
    head_a: &CosignedTreeHead,
    head_b: &CosignedTreeHead,
) -> Vec<(String, VerifyingKey)> {
    let mut list = Vec::new();
    for cosig in head_a.cosignatures.iter().chain(head_b.cosignatures.iter()) {
        if list.iter().any(|(id, _)| *id == cosig.witness_key_id) {
            continue;
        }
        if let Some(key) = parse_witness_verify_key(&cosig.witness_key_id) {
            list.push((cosig.witness_key_id.clone(), key));
        }
    }
    list
}

fn parse_witness_verify_key(witness_key_id: &str) -> Option<VerifyingKey> {
    let bytes = hex::decode(witness_key_id).ok()?;
    let array: [u8; 32] = bytes.as_slice().try_into().ok()?;
    VerifyingKey::from_bytes(&array).ok()
}

/// Tries each of `candidate_author_keys` (a shard can have more than one
/// currently-authorized `shard_settlement` key on record) until one
/// confirms both heads as validly cosigned and produces a non-empty
/// equivocation proof. `None` if no candidate key confirms anything —
/// either the heads don't actually reach majority under any resolvable
/// author key, or this node simply can't resolve the right key yet.
fn confirm_equivocation(
    candidate_author_keys: &[VerifyingKey],
    head_a: &CosignedTreeHead,
    head_b: &CosignedTreeHead,
    now: OffsetDateTime,
) -> Option<(VerifyingKey, Vec<String>)> {
    let known_list = known_list_from_cosignatures(head_a, head_b);
    let freshness_cutoff = now - CONFIRMATION_FRESHNESS_WINDOW;
    candidate_author_keys.iter().find_map(|key| {
        let equivocators =
            find_equivocating_witnesses(key, &known_list, freshness_cutoff, now, head_a, head_b);
        (!equivocators.is_empty()).then_some((*key, equivocators))
    })
}

/// The first of `candidate_author_keys` that signed both heads, when the
/// heads are two different roots at the same network and tree size — direct
/// proof the author signed two roots, independent of any cosignature.
fn confirm_author_equivocation(
    candidate_author_keys: &[VerifyingKey],
    head_a: &CosignedTreeHead,
    head_b: &CosignedTreeHead,
) -> Option<VerifyingKey> {
    if head_a.sth.network_id != head_b.sth.network_id
        || head_a.sth.tree_size != head_b.sth.tree_size
        || head_a.sth.root_hash == head_b.sth.root_hash
    {
        return None;
    }
    candidate_author_keys.iter().copied().find(|key| {
        avalon_protocol::sth::verify_tree_head(key, &head_a.sth)
            && avalon_protocol::sth::verify_tree_head(key, &head_b.sth)
    })
}

/// Most confirmations running at once on this node.
const MAX_CONFIRMATIONS_IN_FLIGHT: usize = 4;

/// Whether a conflict was reported by an established peer; only such conflicts may use the slot
/// held back from the rest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Standing(bool);

impl Standing {
    pub fn of(established: bool) -> Self {
        Self(established)
    }
}

/// Bounds running confirmations: one per `(shard, tree size)`, one per reporting peer, and a fixed
/// number overall, with one slot kept for conflicts of good standing.
struct ConfirmGate {
    running: std::sync::Mutex<std::collections::HashMap<(String, i64), String>>,
    limit: usize,
}

struct ConfirmSlot<'a> {
    gate: &'a ConfirmGate,
    key: (String, i64),
}

impl ConfirmGate {
    fn new(limit: usize) -> Self {
        Self {
            running: Default::default(),
            limit,
        }
    }

    fn try_enter(
        &self,
        shard_id: &str,
        tree_size: i64,
        standing: Standing,
        reporter: &str,
    ) -> Option<ConfirmSlot<'_>> {
        let key = (shard_id.to_string(), tree_size);
        let mut running = self.running.lock().unwrap_or_else(|e| e.into_inner());
        let limit = if standing.0 {
            self.limit
        } else {
            self.limit.saturating_sub(1)
        };
        if running.len() >= limit
            || running.contains_key(&key)
            || running.values().any(|r| r == reporter)
        {
            return None;
        }
        running.insert(key.clone(), reporter.to_string());
        Some(ConfirmSlot { gate: self, key })
    }
}

impl Drop for ConfirmSlot<'_> {
    fn drop(&mut self) {
        self.gate
            .running
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.key);
    }
}

fn confirm_gate() -> &'static ConfirmGate {
    static GATE: std::sync::OnceLock<ConfirmGate> = std::sync::OnceLock::new();
    GATE.get_or_init(|| ConfirmGate::new(MAX_CONFIRMATIONS_IN_FLIGHT))
}

/// Shard ids [`spawn_confirmation`] was asked about, for tests of what a path must not start.
#[cfg(test)]
pub(crate) fn attempted() -> &'static std::sync::Mutex<Vec<(String, Standing, String)>> {
    static ATTEMPTED: std::sync::OnceLock<std::sync::Mutex<Vec<(String, Standing, String)>>> =
        std::sync::OnceLock::new();
    ATTEMPTED.get_or_init(Default::default)
}

/// Runs [`confirm_and_record`] in the background unless this conflict's shard and size is already
/// being confirmed or the node-wide limit is reached; a dropped conflict resurfaces on the next
/// gossip round.
pub fn spawn_confirmation(
    chain: &PostgresSettlementProvider,
    head_gossip: &HeadGossipTracker,
    peers: &PeerTable,
    known_list: &KnownListHandle,
    standing: Standing,
    reporter: &str,
    conflict: HeadConflict,
) {
    #[cfg(test)]
    attempted()
        .lock()
        .unwrap()
        .push((conflict.shard_id.clone(), standing, reporter.to_string()));
    let gate = confirm_gate();
    let Some(slot) = gate.try_enter(&conflict.shard_id, conflict.tree_size, standing, reporter)
    else {
        static LOG: std::sync::OnceLock<crate::log_throttle::LogThrottle> =
            std::sync::OnceLock::new();
        let log = LOG.get_or_init(|| {
            crate::log_throttle::LogThrottle::new(std::time::Duration::from_secs(60))
        });
        let key = format!("{}:{}", conflict.shard_id, conflict.tree_size);
        if let Some(held_back) = log.permit(&key, std::time::Instant::now()) {
            tracing::warn!(
                event = "equivocation_confirmation_skipped",
                shard_id = %conflict.shard_id,
                tree_size = conflict.tree_size,
                held_back,
                "a confirmation for this head is running or the node-wide limit is reached",
            );
        }
        return;
    };
    // The slot borrows the static gate; moved into the task so it is held until the end.
    let (chain, head_gossip, peers, known_list) = (
        chain.clone(),
        head_gossip.clone(),
        peers.clone(),
        known_list.clone(),
    );
    tokio::spawn(async move {
        let _slot = slot;
        confirm_and_record(&chain, &head_gossip, &peers, &known_list, conflict).await;
    });
}

/// Fetches, confirms, and (on success) durably records the equivocation
/// `conflict` signals — spawned from `crate::nodes::announce` and
/// `crate::nodes::run_worker` whenever [`crate::nodes::HeadGossipTracker::merge`]
/// surfaces a new conflict. A no-op if this shard's equivocation is already
/// on record, if either peer can't be reached, or if no resolvable author
/// key signed both heads (a false-positive gossip signal — e.g. a peer that
/// has since moved past the size in question — is not an error, just
/// nothing to prove). Cosignatures the two peers do not serve are gathered
/// from confirmed known-list witnesses; a shared witness yields witness-level
/// evidence, otherwise two valid author signatures over different roots yield
/// author-level evidence with no witnesses named.
pub async fn confirm_and_record(
    chain: &PostgresSettlementProvider,
    head_gossip: &HeadGossipTracker,
    peers: &PeerTable,
    known_list: &KnownListHandle,
    conflict: HeadConflict,
) {
    if head_gossip.is_equivocating(&conflict.shard_id) {
        return;
    }

    let client = crate::node_http::NodeClient::guarded();
    let (head_a, head_b) = tokio::join!(
        fetch_cosigned_head(
            &client,
            &conflict.source_a,
            &conflict.shard_id,
            conflict.tree_size
        ),
        fetch_cosigned_head(
            &client,
            &conflict.source_b,
            &conflict.shard_id,
            conflict.tree_size
        ),
    );
    let (head_a, head_b) = match (head_a, head_b) {
        (Ok(a), Ok(b)) => (a, b),
        (a, b) => {
            tracing::warn!(
                event = "equivocation_confirmation_fetch_failed",
                shard_id = %conflict.shard_id,
                tree_size = conflict.tree_size,
                error_a = ?a.err(),
                error_b = ?b.err(),
                "could not fetch full cosignature detail for both sides of a gossiped head \
                 conflict",
            );
            return;
        }
    };

    if head_a.sth.root_hash != conflict.root_hash_a || head_b.sth.root_hash != conflict.root_hash_b
    {
        // The reporting peer has since moved on (or the gossip was stale) —
        // nothing left to confirm at this exact pair of roots.
        return;
    }

    let network_id = chain.network_id();
    if head_a.sth.network_id != network_id || head_b.sth.network_id != network_id {
        return;
    }
    let candidate_author_keys: Vec<VerifyingKey> =
        pinned_core_verify_key(network_id, &conflict.shard_id)
            .into_iter()
            .chain(
                resolve_shard_verify_keys_from_db(chain.pool(), network_id, &conflict.shard_id)
                    .await,
            )
            .collect();

    let sources = crate::cosign_gather::witness_sources(
        &crate::cosign_verify::known_list_verifying_keys(known_list),
        &peers.list_all(),
    );
    let policy = crate::outbound_policy::OutboundPolicy::from_env();
    let (extra_a, extra_b) = tokio::join!(
        crate::cosign_gather::gather_witness_cosignatures(
            policy,
            &sources,
            &head_a,
            &conflict.shard_id
        ),
        crate::cosign_gather::gather_witness_cosignatures(
            policy,
            &sources,
            &head_b,
            &conflict.shard_id
        ),
    );
    let head_a = crate::cosign_gather::merge_cosignatures(head_a, extra_a);
    let head_b = crate::cosign_gather::merge_cosignatures(head_b, extra_b);

    let now = OffsetDateTime::now_utc();
    let (kind, equivocators) = if let Some((_key, equivocators)) =
        confirm_equivocation(&candidate_author_keys, &head_a, &head_b, now)
    {
        (EquivocationEvidenceKind::Witness, equivocators)
    } else if confirm_author_equivocation(&candidate_author_keys, &head_a, &head_b).is_some() {
        (EquivocationEvidenceKind::Author, Vec::new())
    } else {
        return;
    };

    tracing::error!(
        event = "equivocation_confirmed",
        shard_id = %conflict.shard_id,
        network_id = %network_id,
        tree_size = conflict.tree_size,
        root_hash_a = %head_a.sth.root_hash,
        root_hash_b = %head_b.sth.root_hash,
        evidence_kind = ?kind,
        equivocating_witnesses = ?equivocators,
        "confirmed an equivocation: this shard's log showed two different roots at the same \
         tree_size — no longer trusting either head",
    );

    head_gossip.mark_equivocating(&conflict.shard_id);

    let evidence = WitnessEquivocationEvidence {
        kind,
        network_id: network_id.to_string(),
        shard_id: conflict.shard_id.clone(),
        tree_size: conflict.tree_size,
        head_a,
        head_b,
        equivocating_witness_key_ids: equivocators,
        detected_at: None,
    };
    if let Err(e) = record_witness_equivocation_evidence(chain.pool(), &evidence).await {
        tracing::error!(
            event = "equivocation_evidence_storage_failed",
            shard_id = %conflict.shard_id,
            error = %e,
            "confirmed an equivocation but failed to durably record it",
        );
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_head_with_one_unreadable_cosignature_survives_and_a_pre_envelope_body_is_refused() {
        use crate::cosign_verify::test_support::*;
        let served = served_with_one_unreadable_cosignature();
        let dto: FetchedSthWithWitnesses = serde_json::from_value(served.body.clone()).unwrap();
        let head = CosignedTreeHead::try_from(dto).unwrap();
        assert_head_survives(&head, &served);
        assert!(
            serde_json::from_value::<FetchedSthWithWitnesses>(without_envelope(served.body))
                .is_err()
        );
    }

    use super::*;
    use avalon_protocol::sth;
    use ed25519_dalek::SigningKey;

    fn root_hash_fixture(byte: u8) -> String {
        hex::encode([byte; 32])
    }

    fn head(
        author_key: &SigningKey,
        tree_size: i64,
        root_hash: &str,
        network_id: &str,
        now: OffsetDateTime,
        cosigners: &[(&SigningKey, String)],
    ) -> CosignedTreeHead {
        let author_sth = sth::sign_tree_head(
            author_key,
            "settlement-operator-1",
            tree_size,
            root_hash,
            network_id,
            now,
        )
        .unwrap();
        let cosignatures = cosigners
            .iter()
            .map(|(key, key_id)| {
                avalon_protocol::witness::sign_witness_cosignature(key, key_id, &author_sth, now)
                    .unwrap()
            })
            .collect();
        CosignedTreeHead {
            sth: author_sth,
            cosignatures,
        }
    }

    fn witness_fixture() -> (SigningKey, String) {
        let key = SigningKey::generate(&mut rand::rng());
        let key_id = hex::encode(key.verifying_key().to_bytes());
        (key, key_id)
    }

    #[test]
    fn the_confirm_gate_admits_one_run_per_head_and_keeps_a_slot_for_good_standing() {
        let (known, unknown) = (Standing(true), Standing(false));
        let gate = ConfirmGate::new(3);
        let a = gate.try_enter("core", 5, unknown, "r1").expect("first run");
        assert!(
            gate.try_enter("core", 5, known, "r2").is_none(),
            "same head twice"
        );
        let _b = gate
            .try_enter("core", 6, unknown, "r2")
            .expect("another size");
        assert!(
            gate.try_enter("junk", 1, unknown, "r3").is_none(),
            "reserved slot"
        );
        let _c = gate
            .try_enter("known", 1, known, "r3")
            .expect("reserved slot is usable");
        assert!(
            gate.try_enter("known", 2, known, "r4").is_none(),
            "over the limit"
        );
        drop(a);
        assert!(
            gate.try_enter("known", 2, known, "r4").is_some(),
            "a freed slot is reusable"
        );
    }

    #[test]
    fn one_reporter_holds_at_most_one_confirmation_slot() {
        let gate = ConfirmGate::new(4);
        let first = gate
            .try_enter("a", 1, Standing(false), "attacker")
            .expect("first");
        assert!(gate
            .try_enter("b", 1, Standing(false), "attacker")
            .is_none());
        assert!(gate.try_enter("b", 1, Standing(false), "honest").is_some());
        drop(first);
        assert!(gate
            .try_enter("c", 1, Standing(false), "attacker")
            .is_some());
    }

    /// The end-to-end scenario the ticket asks for: a log shows two
    /// different roots at the same `tree_size`, each independently
    /// majority-cosigned by a different (but overlapping) witness group —
    /// mirrors `cosigned_sth::conflicting_majority_cosigned_heads_share_a_witness`,
    /// exercised through this module's fetch-independent confirmation path
    /// instead of calling `find_equivocating_witnesses` directly.
    #[test]
    fn confirms_an_equivocation_shown_to_disjoint_witness_groups() {
        let author_key = SigningKey::generate(&mut rand::rng());
        let now = OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(10_000);
        let w1 = witness_fixture();
        let w2 = witness_fixture();
        let w3 = witness_fixture();

        // A 3-witness majority is 2. head_a: w1 + w2. head_b: w2 + w3 — w2
        // is the equivocator, cosigning both conflicting roots.
        let head_a = head(
            &author_key,
            5,
            &root_hash_fixture(1),
            "avalon-test",
            now,
            &[(&w1.0, w1.1.clone()), (&w2.0, w2.1.clone())],
        );
        let head_b = head(
            &author_key,
            5,
            &root_hash_fixture(2),
            "avalon-test",
            now,
            &[(&w2.0, w2.1.clone()), (&w3.0, w3.1.clone())],
        );

        let result =
            confirm_equivocation(&[author_key.verifying_key()], &head_a, &head_b, now).unwrap();
        assert_eq!(result.0, author_key.verifying_key());
        assert_eq!(result.1, vec![w2.1]);
    }

    #[test]
    fn no_confirmation_when_the_wrong_author_key_is_tried() {
        let author_key = SigningKey::generate(&mut rand::rng());
        let wrong_key = SigningKey::generate(&mut rand::rng());
        let now = OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(10_000);
        let w1 = witness_fixture();
        let w2 = witness_fixture();
        let w3 = witness_fixture();

        let head_a = head(
            &author_key,
            5,
            &root_hash_fixture(1),
            "avalon-test",
            now,
            &[(&w1.0, w1.1.clone()), (&w2.0, w2.1.clone())],
        );
        let head_b = head(
            &author_key,
            5,
            &root_hash_fixture(2),
            "avalon-test",
            now,
            &[(&w2.0, w2.1.clone()), (&w3.0, w3.1.clone())],
        );

        assert!(
            confirm_equivocation(&[wrong_key.verifying_key()], &head_a, &head_b, now).is_none()
        );
    }

    #[test]
    fn no_confirmation_when_neither_head_reaches_majority() {
        let author_key = SigningKey::generate(&mut rand::rng());
        let now = OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(10_000);
        let w1 = witness_fixture();
        let w3 = witness_fixture();

        // Each head has exactly one cosigner, but the two heads' union known
        // list has size 2 (w1, w3 — disjoint sets), so `majority_threshold(2)
        // == 2` and neither head's single cosignature reaches it. Nothing to
        // prove: no shared witness, no majority on either side.
        let head_a = head(
            &author_key,
            5,
            &root_hash_fixture(1),
            "avalon-test",
            now,
            &[(&w1.0, w1.1.clone())],
        );
        let head_b = head(
            &author_key,
            5,
            &root_hash_fixture(2),
            "avalon-test",
            now,
            &[(&w3.0, w3.1.clone())],
        );

        assert!(
            confirm_equivocation(&[author_key.verifying_key()], &head_a, &head_b, now).is_none()
        );
    }

    #[test]
    fn author_level_confirmation_needs_no_cosignatures() {
        let author_key = SigningKey::generate(&mut rand::rng());
        let now = OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(10_000);
        let head_a = head(
            &author_key,
            5,
            &root_hash_fixture(1),
            "avalon-test",
            now,
            &[],
        );
        let head_b = head(
            &author_key,
            5,
            &root_hash_fixture(2),
            "avalon-test",
            now,
            &[],
        );

        assert_eq!(
            confirm_author_equivocation(&[author_key.verifying_key()], &head_a, &head_b),
            Some(author_key.verifying_key())
        );
        // No witness-level proof exists for bare heads.
        assert!(
            confirm_equivocation(&[author_key.verifying_key()], &head_a, &head_b, now).is_none()
        );
    }

    #[test]
    fn author_level_confirmation_rejects_an_invalid_author_signature() {
        let author_key = SigningKey::generate(&mut rand::rng());
        let forger = SigningKey::generate(&mut rand::rng());
        let now = OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(10_000);
        let head_a = head(
            &author_key,
            5,
            &root_hash_fixture(1),
            "avalon-test",
            now,
            &[],
        );
        let forged = head(&forger, 5, &root_hash_fixture(2), "avalon-test", now, &[]);
        let mut tampered = head(
            &author_key,
            5,
            &root_hash_fixture(3),
            "avalon-test",
            now,
            &[],
        );
        tampered.sth.root_hash = root_hash_fixture(4);

        let keys = [author_key.verifying_key()];
        assert!(confirm_author_equivocation(&keys, &head_a, &forged).is_none());
        assert!(confirm_author_equivocation(&keys, &head_a, &tampered).is_none());
    }

    #[test]
    fn author_level_confirmation_ignores_same_root_size_or_network_mismatch() {
        let author_key = SigningKey::generate(&mut rand::rng());
        let now = OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(10_000);
        let keys = [author_key.verifying_key()];
        let base = head(
            &author_key,
            5,
            &root_hash_fixture(1),
            "avalon-test",
            now,
            &[],
        );
        let same_root = head(
            &author_key,
            5,
            &root_hash_fixture(1),
            "avalon-test",
            now,
            &[],
        );
        let other_size = head(
            &author_key,
            6,
            &root_hash_fixture(2),
            "avalon-test",
            now,
            &[],
        );
        let other_net = head(&author_key, 5, &root_hash_fixture(2), "other-net", now, &[]);

        assert!(confirm_author_equivocation(&keys, &base, &same_root).is_none());
        assert!(confirm_author_equivocation(&keys, &base, &other_size).is_none());
        assert!(confirm_author_equivocation(&keys, &base, &other_net).is_none());
    }

    #[test]
    fn known_list_ignores_a_malformed_witness_key_id() {
        let author_key = SigningKey::generate(&mut rand::rng());
        let now = OffsetDateTime::UNIX_EPOCH;
        let w1 = witness_fixture();

        let mut head_a = head(
            &author_key,
            1,
            &root_hash_fixture(1),
            "avalon-test",
            now,
            &[(&w1.0, w1.1.clone())],
        );
        head_a.cosignatures.push(
            avalon_protocol::witness::sign_witness_cosignature(
                &w1.0,
                "not-hex-at-all",
                &head_a.sth,
                now,
            )
            .unwrap(),
        );
        let head_b = head_a.clone();

        let list = known_list_from_cosignatures(&head_a, &head_b);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].0, w1.1);
    }
}
