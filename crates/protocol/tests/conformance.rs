//! Server side of the cross-SDK conformance suite — the same shared vectors
//! under `conformance/vectors/` that each
//! SDK's own runner consumes, asserted here against this crate's
//! implementations, which are what `avalon-server` actually verifies
//! incoming signatures with.
//!
//! This half exists because the Rust SDK is now an independent
//! reimplementation like the C# and TypeScript ones: before it, the SDK
//! called these functions directly and the compiler guaranteed the two
//! agreed. Now nothing does — except these vectors, checked from both
//! sides. A change to a signing-byte format here that isn't mirrored in
//! every SDK fails this file; a change in an SDK that isn't mirrored here
//! fails that SDK's runner.
//!
//! Offline and dependency-free — no server, no database.

use avalon_protocol::achievements::{
    attestation_signing_bytes, bulk_attestation_signing_bytes, revocation_signing_bytes,
};
use avalon_protocol::continuation::signing_bytes as continuation_signing_bytes;
use avalon_protocol::cosigned_sth::{
    find_equivocating_witnesses, verify_cosigned_tree_head, CosignedTreeHead,
};
use avalon_protocol::cross_node_login::signing_bytes as cross_node_login_signing_bytes;
use avalon_protocol::ids::{AttestationId, IdentityId};
use avalon_protocol::interest_claim::{
    signing_bytes as interest_claim_signing_bytes, ClaimedScope,
};
use avalon_protocol::sth::{signing_message as sth_signing_message, SignedTreeHead};
use avalon_protocol::witness::WitnessCosignature;
use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
use serde_json::Value;
use std::path::{Path, PathBuf};
use time::OffsetDateTime;
use uuid::Uuid;

fn vectors_dir() -> PathBuf {
    // crates/protocol -> repo root -> conformance/vectors
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/protocol sits two levels under the repo root")
        .join("conformance/vectors")
}

fn load(name: &str) -> Value {
    let path = vectors_dir().join(name);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read conformance vector {}: {e}", path.display()));
    serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("invalid JSON in {}: {e}", path.display()))
}

fn signing_key_from_seed_hex(hex_seed: &str) -> SigningKey {
    let bytes = hex::decode(hex_seed).expect("valid hex seed");
    let seed: [u8; 32] = bytes.try_into().expect("32-byte seed");
    SigningKey::from_bytes(&seed)
}

fn parse_uuid(v: &Value, field: &str) -> Uuid {
    Uuid::parse_str(
        v[field]
            .as_str()
            .unwrap_or_else(|| panic!("missing {field}")),
    )
    .unwrap_or_else(|e| panic!("invalid uuid in {field}: {e}"))
}

fn parse_identity_id(v: &Value, field: &str) -> IdentityId {
    IdentityId::parse(
        v[field]
            .as_str()
            .unwrap_or_else(|| panic!("missing {field}")),
    )
    .unwrap_or_else(|e| panic!("invalid identity id in {field}: {e}"))
}

fn parse_offset(v: &Value, field: &str) -> OffsetDateTime {
    let secs = v[field]
        .as_i64()
        .unwrap_or_else(|| panic!("missing {field}"));
    OffsetDateTime::from_unix_timestamp(secs)
        .unwrap_or_else(|e| panic!("invalid timestamp {field}: {e}"))
}

fn assert_signature_matches(name: &str, key: &SigningKey, bytes: &[u8], expected_sig_hex: &str) {
    assert_eq!(
        hex::encode(key.sign(bytes).to_bytes()),
        expected_sig_hex,
        "[{name}] Ed25519 signature diverged from the shared vector — a signature an SDK \
         produces for this input would no longer be the one this crate verifies"
    );
}

#[test]
fn attestation_signing_matches_shared_vectors() {
    let doc = load("attestation-signing.json");
    let signing_key = signing_key_from_seed_hex(doc["signingKeySeedHex"].as_str().unwrap());
    assert_eq!(
        hex::encode(signing_key.verifying_key().as_bytes()),
        doc["signingPublicKeyHex"].as_str().unwrap(),
        "the shared test keypair's derived public key must match the vector file"
    );

    for vector in doc["vectors"].as_array().unwrap() {
        let name = vector["name"].as_str().unwrap_or("<unnamed>");
        let input = &vector["input"];
        let claim_kind = input["claimKind"].as_str().unwrap();
        let issuer_ref = input["issuerRef"].as_str().unwrap();

        let bytes = match input["operation"].as_str().unwrap() {
            "issue" => attestation_signing_bytes(
                claim_kind,
                issuer_ref,
                parse_identity_id(input, "subject"),
                input["achievement"].as_str().unwrap(),
            ),
            "bulk_issue" => {
                let achievements: Vec<String> = input["achievements"]
                    .as_array()
                    .expect("bulk_issue vectors carry an achievements array")
                    .iter()
                    .map(|v| v.as_str().unwrap().to_string())
                    .collect();
                bulk_attestation_signing_bytes(
                    claim_kind,
                    issuer_ref,
                    parse_identity_id(input, "subject"),
                    &achievements,
                )
            }
            "revoke" => revocation_signing_bytes(
                claim_kind,
                issuer_ref,
                AttestationId(parse_uuid(input, "attestationId")),
                input["reasonCode"].as_str().unwrap(),
            ),
            other => panic!("[{name}] unknown operation {other}"),
        };

        assert_eq!(
            hex::encode(&bytes),
            vector["expected"]["signingBytesHex"].as_str().unwrap(),
            "[{name}] signing bytes diverged from the shared vector"
        );
        assert_signature_matches(
            name,
            &signing_key,
            &bytes,
            vector["expected"]["signatureHex"].as_str().unwrap(),
        );
    }
}

#[test]
fn signed_tree_head_signing_matches_shared_vectors() {
    let doc = load("signed-tree-head.json");
    let signing_key = signing_key_from_seed_hex(doc["signingKeySeedHex"].as_str().unwrap());

    for vector in doc["vectors"].as_array().unwrap() {
        let name = vector["name"].as_str().unwrap_or("<unnamed>");
        let input = &vector["input"];
        let bytes = sth_signing_message(
            input["treeSize"].as_i64().unwrap(),
            input["rootHashHex"].as_str().unwrap(),
            input["networkId"].as_str().unwrap(),
            parse_offset(input, "createdAtUnixSeconds"),
        );
        assert_eq!(
            hex::encode(&bytes),
            vector["expected"]["signingBytesHex"].as_str().unwrap(),
            "[{name}] signing bytes diverged from the shared vector"
        );
        assert_signature_matches(
            name,
            &signing_key,
            &bytes,
            vector["expected"]["signatureHex"].as_str().unwrap(),
        );
    }
}

#[test]
fn cross_node_login_grant_signing_matches_shared_vectors() {
    let doc = load("cross-node-login.json");
    let signing_key = signing_key_from_seed_hex(doc["signingKeySeedHex"].as_str().unwrap());

    for vector in doc["vectors"].as_array().unwrap() {
        let name = vector["name"].as_str().unwrap_or("<unnamed>");
        let input = &vector["input"];
        let bytes = cross_node_login_signing_bytes(
            parse_identity_id(input, "identityId"),
            parse_uuid(input, "signingKeyId"),
            input["destinationBaseUrl"].as_str().unwrap(),
            input["requestingContext"].as_str().unwrap(),
            parse_uuid(input, "nonce"),
            parse_offset(input, "issuedAtUnixSeconds"),
            parse_offset(input, "expiresAtUnixSeconds"),
        );
        assert_eq!(
            String::from_utf8(bytes.clone()).unwrap(),
            vector["expected"]["signingBytesUtf8"].as_str().unwrap(),
            "[{name}] signing bytes diverged from the shared vector"
        );
        assert_signature_matches(
            name,
            &signing_key,
            &bytes,
            vector["expected"]["signatureHex"].as_str().unwrap(),
        );
    }
}

#[test]
fn session_continuation_signing_matches_shared_vectors() {
    let doc = load("session-continuation.json");
    let vector = &doc["vectors"][0];
    let input = &vector["input"];
    let bytes = continuation_signing_bytes(
        parse_identity_id(input, "identityId"),
        parse_uuid(input, "signingKeyId"),
        parse_uuid(input, "nonce"),
        parse_offset(input, "issuedAtUnixSeconds"),
        parse_offset(input, "expiresAtUnixSeconds"),
    );
    assert_eq!(
        String::from_utf8(bytes).unwrap(),
        vector["expected"]["signingBytesUtf8"].as_str().unwrap(),
        "avalon_protocol::continuation::signing_bytes diverged from the shared vector"
    );
}

#[test]
fn websocket_interest_claim_signing_matches_shared_vectors() {
    let doc = load("websocket-interest-claim.json");
    let vector = &doc["vectors"][0];
    let input = &vector["input"];
    let bytes = interest_claim_signing_bytes(
        parse_identity_id(input, "identityId"),
        parse_uuid(input, "signingKeyId"),
        ClaimedScope::Channel {
            channel_id: parse_uuid(&input["scope"], "channelId"),
        },
        input["baseUrl"].as_str().unwrap(),
        parse_uuid(input, "nonce"),
        parse_offset(input, "issuedAtUnixSeconds"),
        parse_offset(input, "expiresAtUnixSeconds"),
    );
    assert_eq!(
        String::from_utf8(bytes).unwrap(),
        vector["expected"]["signingBytesUtf8"].as_str().unwrap(),
        "avalon_protocol::interest_claim::signing_bytes diverged from the shared vector"
    );
}

fn verifying_key_from_hex(hex_value: &str) -> VerifyingKey {
    let bytes: [u8; 32] = hex::decode(hex_value)
        .expect("valid hex verifying key")
        .try_into()
        .expect("32-byte verifying key");
    VerifyingKey::from_bytes(&bytes).expect("valid Ed25519 verifying key")
}

fn sth_from_json(v: &Value, network_id: &str) -> SignedTreeHead {
    SignedTreeHead {
        tree_size: match &v["treeSize"] {
            Value::String(decimal) => decimal.parse().unwrap(),
            number => number.as_i64().unwrap(),
        },
        root_hash: v["rootHashHex"].as_str().unwrap().to_string(),
        network_id: network_id.to_string(),
        signing_key_id: "settlement-operator-1".to_string(),
        signature: v["signatureHex"].as_str().unwrap().to_string(),
        created_at: parse_offset(v, "createdAtUnixSeconds"),
    }
}

fn cosignature_from_json(v: &Value, sth: &SignedTreeHead) -> WitnessCosignature {
    WitnessCosignature {
        tree_size: sth.tree_size,
        root_hash: sth.root_hash.clone(),
        network_id: sth.network_id.clone(),
        author_created_at: sth.created_at,
        witness_key_id: v["witnessKeyId"].as_str().unwrap().to_string(),
        observed_at: parse_offset(v, "observedAtUnixSeconds"),
        signature: v["signatureHex"].as_str().unwrap().to_string(),
    }
}

fn cosigned_head_from_json(v: &Value, network_id: &str) -> CosignedTreeHead {
    let sth = sth_from_json(&v["sth"], network_id);
    let cosignatures = v["cosignatures"]
        .as_array()
        .map(|arr| arr.iter().map(|c| cosignature_from_json(c, &sth)).collect())
        .unwrap_or_default();
    CosignedTreeHead { sth, cosignatures }
}

/// Both the accept/reject decision (`verify_cosigned_tree_head`) and the
/// equivocation-detection function (`find_equivocating_witnesses`) against
/// shared, precomputed-signature fixtures — not just a self-consistent
/// round trip, the same "both sides of the wire" bar every other vector in
/// this suite is held to.
#[test]
fn witness_cosigned_tree_head_matches_shared_vectors() {
    let doc = load("witness-cosigned-tree-head.json");
    let network_id = doc["networkId"].as_str().unwrap();
    let author_verifying_key =
        verifying_key_from_hex(doc["authorVerifyingKeyHex"].as_str().unwrap());
    let witness_verifying_keys = doc["witnessVerifyingKeysHex"].as_object().unwrap();
    let known_list: Vec<(String, VerifyingKey)> = witness_verifying_keys
        .iter()
        .map(|(id, hex_value)| {
            (
                id.clone(),
                verifying_key_from_hex(hex_value.as_str().unwrap()),
            )
        })
        .collect();
    let known_list_for = |ids: &[Value]| -> Vec<(String, VerifyingKey)> {
        ids.iter()
            .map(|id| {
                let id = id.as_str().unwrap();
                known_list
                    .iter()
                    .find(|(known_id, _)| known_id == id)
                    .unwrap_or_else(|| panic!("known list references undeclared witness {id}"))
                    .clone()
            })
            .collect()
    };

    for vector in doc["vectors"].as_array().unwrap() {
        let name = vector["name"].as_str().unwrap_or("<unnamed>");
        let input = &vector["input"];
        let known_list = known_list_for(input["knownList"].as_array().unwrap());
        let freshness_cutoff = parse_offset(input, "freshnessCutoffUnixSeconds");
        let now = parse_offset(input, "nowUnixSeconds");

        if let Some(equivocating_witnesses) = vector["expected"].get("equivocatingWitnesses") {
            let head_a = cosigned_head_from_json(&input["headA"], network_id);
            let head_b = cosigned_head_from_json(&input["headB"], network_id);

            assert_eq!(
                verify_cosigned_tree_head(
                    &author_verifying_key,
                    &head_a,
                    &known_list,
                    freshness_cutoff,
                    now
                ),
                vector["expected"]["headAAccepted"].as_bool().unwrap(),
                "[{name}] head A accept/reject diverged from the shared vector"
            );
            assert_eq!(
                verify_cosigned_tree_head(
                    &author_verifying_key,
                    &head_b,
                    &known_list,
                    freshness_cutoff,
                    now
                ),
                vector["expected"]["headBAccepted"].as_bool().unwrap(),
                "[{name}] head B accept/reject diverged from the shared vector"
            );

            let mut expected: Vec<String> = equivocating_witnesses
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap().to_string())
                .collect();
            expected.sort();
            let mut actual = find_equivocating_witnesses(
                &author_verifying_key,
                &known_list,
                freshness_cutoff,
                now,
                &head_a,
                &head_b,
            );
            actual.sort();
            assert_eq!(
                actual, expected,
                "[{name}] equivocating witnesses diverged from the shared vector"
            );
        } else {
            let head = cosigned_head_from_json(input, network_id);

            assert_eq!(
                verify_cosigned_tree_head(
                    &author_verifying_key,
                    &head,
                    &known_list,
                    freshness_cutoff,
                    now
                ),
                vector["expected"]["accepted"].as_bool().unwrap(),
                "[{name}] accept/reject diverged from the shared vector"
            );
        }
    }
}

/// `identity-chain.json`: event-hash bytes and the deterministic conflict
/// rule, asserted for every ordering of each case's events.
#[test]
fn identity_chain_matches_shared_vectors() {
    use avalon_protocol::events::{IdentityChainPosition, ProtocolEvent};
    use avalon_protocol::identity_chain::{
        apply_chain, chain_event_signing_bytes, compute_event_hash, ActionClass, ChainHashInput,
        ChainedEvent, EventAuthority, EventHash,
    };
    use avalon_protocol::identity_chain_wire::{chain_owner, event_hash};
    use avalon_protocol::identity_id::IdentityId;

    let doc = load("identity-chain.json");
    let hash_from_hex = |s: &str| -> EventHash { hex::decode(s).unwrap().try_into().unwrap() };

    for vector in doc["hashVectors"].as_array().unwrap() {
        let name = vector["name"].as_str().unwrap();
        let i = &vector["input"];
        let want = &vector["expected"];
        let payload: serde_json::Value =
            serde_json::from_str(i["payloadJsonUtf8"].as_str().unwrap()).unwrap();
        assert_eq!(
            avalon_protocol::canonical_payload::canonicalize(&payload).unwrap(),
            want["payloadCanonicalUtf8"].as_str().unwrap(),
            "[{name}] canonical payload"
        );
        let payload_hash = avalon_protocol::ledger_entry::payload_hash(&payload).unwrap();
        assert_eq!(
            hex::encode(payload_hash),
            want["payloadHashHex"].as_str().unwrap()
        );
        let id = IdentityId::parse(i["identityIdHex"].as_str().unwrap()).unwrap();
        let prev = i["prevHashHex"].as_str().map(hash_from_hex);
        let micros: i64 = i["timestampUnixMicros"].as_str().unwrap().parse().unwrap();
        let input = ChainHashInput {
            identity_id: &id,
            seq: i["seq"].as_str().unwrap().parse().unwrap(),
            prev_hash: prev.as_ref(),
            event_id: uuid::Uuid::parse_str(i["eventId"].as_str().unwrap()).unwrap(),
            kind: i["kind"].as_str().unwrap(),
            issuer: i["issuer"].as_str().unwrap(),
            subject: i["subject"].as_str().unwrap(),
            event_version: i["eventVersion"].as_u64().unwrap().try_into().unwrap(),
            timestamp_micros: micros,
            payload_hash: &payload_hash,
        };
        assert_eq!(
            hex::encode(chain_event_signing_bytes(&input).unwrap()),
            want["signingBytesHex"].as_str().unwrap(),
            "[{name}] signing bytes"
        );
        assert_eq!(
            hex::encode(compute_event_hash(&input).unwrap()),
            want["eventHashHex"].as_str().unwrap(),
            "[{name}] hash"
        );

        // The same event built as a ProtocolEvent hashes identically wherever it can be represented.
        let event = ProtocolEvent {
            id: input.event_id,
            kind: input.kind.to_string(),
            issuer: serde_json::from_value(i["issuer"].clone()).unwrap(),
            subject: serde_json::from_value(i["subject"].clone()).unwrap(),
            payload,
            timestamp: OffsetDateTime::from_unix_timestamp_nanos(i128::from(micros) * 1000)
                .unwrap_or(OffsetDateTime::UNIX_EPOCH),
            version: input.event_version,
            identity_chain: Some(IdentityChainPosition {
                seq: input.seq,
                prev_hash: i["prevHashHex"].as_str().map(str::to_string),
            }),
        };
        if chain_owner(&event) == Some(id)
            && OffsetDateTime::from_unix_timestamp_nanos(i128::from(micros) * 1000).is_ok()
        {
            assert_eq!(
                hex::encode(event_hash(&event).unwrap()),
                want["eventHashHex"].as_str().unwrap(),
                "[{name}] via ProtocolEvent"
            );
        }
    }

    for case in doc["resolutionCases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let raw = case["events"].as_array().unwrap();
        let hash_of = |label: &str| -> EventHash {
            let entry = raw.iter().find(|e| e["label"] == label).unwrap();
            hash_from_hex(entry["hashHex"].as_str().unwrap())
        };
        let events: Vec<ChainedEvent> = raw
            .iter()
            .map(|e| ChainedEvent {
                seq: e["seq"].as_u64().unwrap(),
                prev_hash: e["prevLabel"].as_str().map(hash_of),
                event_hash: hash_from_hex(e["hashHex"].as_str().unwrap()),
                timestamp: OffsetDateTime::UNIX_EPOCH
                    + time::Duration::seconds(e["timestampSeconds"].as_i64().unwrap()),
                class: match e["class"].as_str().unwrap() {
                    "ordinary" => ActionClass::OrdinaryEdit,
                    "monotonic" => ActionClass::Monotonic,
                    "critical" => ActionClass::ChainCritical,
                    other => panic!("unknown class {other}"),
                },
                authority: EventAuthority::AuthenticatedSession,
            })
            .collect();
        let expected_labels: Vec<EventHash> = case["expected"]["acceptedLabels"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| hash_of(l.as_str().unwrap()))
            .collect();
        let expected_fork = case["expected"]["forkedAtSeq"].as_u64();

        let mut orderings = vec![events.clone()];
        let mut reversed = events.clone();
        reversed.reverse();
        orderings.push(reversed);
        let mut rotated = events.clone();
        rotated.rotate_left(1);
        orderings.push(rotated);
        for ordering in orderings {
            let outcome = apply_chain(ordering);
            let got: Vec<EventHash> = outcome.accepted.iter().map(|e| e.event_hash).collect();
            assert_eq!(got, expected_labels, "case {name}: accepted chain");
            assert_eq!(outcome.forked_at, expected_fork, "case {name}: fork");
        }
    }
}

#[test]
fn witness_announce_matches_shared_vectors() {
    use avalon_protocol::witness::{verify_witness_announce, witness_announce_message};
    use time::format_description::well_known::Rfc3339;

    let doc = load("witness-announce.json");
    let vectors = doc["vectors"].as_array().expect("vectors array");
    assert!(!vectors.is_empty());
    for v in vectors {
        let name = v["name"].as_str().unwrap();
        let input = &v["input"];
        let base_url = input["baseUrl"].as_str().unwrap();
        let key_id = input["witnessKeyId"].as_str().unwrap();
        let announced_at =
            OffsetDateTime::parse(input["announcedAt"].as_str().unwrap(), &Rfc3339).unwrap();
        let now = OffsetDateTime::parse(input["now"].as_str().unwrap(), &Rfc3339).unwrap();
        let proof = input["proofHex"].as_str().unwrap();
        let accepted = verify_witness_announce(base_url, key_id, announced_at, proof, now);
        assert_eq!(
            accepted,
            v["expected"]["accepted"].as_bool().unwrap(),
            "{name}"
        );
        if let Some(message_hex) = input.get("messageHex").and_then(|m| m.as_str()) {
            assert_eq!(
                hex::encode(witness_announce_message(base_url, key_id, announced_at)),
                message_hex,
                "{name}: signed message bytes"
            );
        }
    }
}

#[test]
fn node_request_matches_shared_vectors() {
    use avalon_protocol::node_request::{
        encode_node_request_header, node_request_signing_message, parse_node_request_header,
        sign_node_request, verify_node_request_header, verify_node_request_header_head,
        NodeRequestTarget,
    };
    use sha2::{Digest, Sha256};

    let doc = load("node-request.json");
    let key = signing_key_from_seed_hex(doc["signingKeySeedHex"].as_str().unwrap());
    assert_eq!(
        hex::encode(key.verifying_key().to_bytes()),
        doc["signingPublicKeyHex"].as_str().unwrap()
    );
    let vectors = doc["vectors"].as_array().expect("vectors array");
    assert!(!vectors.is_empty());
    for v in vectors {
        let name = v["name"].as_str().unwrap();
        let input = &v["input"];
        let body = hex::decode(input["bodyHex"].as_str().unwrap()).unwrap();
        assert_eq!(
            hex::encode(Sha256::digest(&body)),
            input["bodySha256Hex"].as_str().unwrap(),
            "{name}: body hash"
        );
        let target = NodeRequestTarget {
            method: input["method"].as_str().unwrap(),
            path: input["path"].as_str().unwrap(),
            body: &body,
            network_id: input["networkId"].as_str().unwrap(),
        };
        let accepted: Vec<&str> = input["acceptedRecipients"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r.as_str().unwrap())
            .collect();
        let header = input["header"].as_str().unwrap();
        let result = verify_node_request_header(
            header,
            &target,
            &accepted,
            input["now"].as_i64().unwrap(),
            input["maxSkewSeconds"].as_i64().unwrap(),
        );
        let expected = &v["expected"];
        assert_eq!(
            result.is_ok(),
            expected["accepted"].as_bool().unwrap(),
            "{name}"
        );
        let code = result.as_ref().err().map(|e| e.code());
        assert_eq!(code, expected["error"].as_str(), "{name}");
        // Without the body only the body_hash rejection disappears.
        let head_only = verify_node_request_header_head(
            header,
            &(&target).into(),
            &accepted,
            input["now"].as_i64().unwrap(),
            input["maxSkewSeconds"].as_i64().unwrap(),
        );
        let head_code = head_only.as_ref().err().map(|e| e.code());
        let want_head = expected["error"].as_str().filter(|c| *c != "body_hash");
        assert_eq!(head_code, want_head, "{name}: head only");
        let Some(recipient) = input.get("signingRecipient").and_then(|r| r.as_str()) else {
            continue;
        };
        let auth = parse_node_request_header(header).unwrap();
        let message = node_request_signing_message(
            &target,
            recipient,
            &auth.peer_id,
            auth.timestamp,
            &auth.nonce,
        )
        .unwrap();
        assert_eq!(
            hex::encode(message),
            input["messageHex"].as_str().unwrap(),
            "{name}: signed message bytes"
        );
        if input["signedBySeed"].as_bool().unwrap() {
            let signed = sign_node_request(
                &key,
                &auth.peer_id,
                &target,
                recipient,
                auth.timestamp,
                auth.nonce,
            )
            .unwrap();
            assert_eq!(
                encode_node_request_header(&signed),
                header,
                "{name}: header"
            );
        }
    }
}

#[test]
fn known_list_selection_matches_shared_vectors() {
    use avalon_protocol::client_known_list::{
        diversity_prefix_for_url, select_known_list, Candidate,
    };

    let doc = load("known-list-selection.json");
    for v in doc["prefixVectors"].as_array().expect("prefixVectors") {
        let name = v["name"].as_str().unwrap();
        let got = diversity_prefix_for_url(v["input"]["baseUrl"].as_str().unwrap());
        let want = v["expected"]["prefix"].as_str().map(str::to_string);
        assert_eq!(got, want, "{name}");
    }
    for v in doc["selectionVectors"]
        .as_array()
        .expect("selectionVectors")
    {
        let name = v["name"].as_str().unwrap();
        let input = &v["input"];
        let candidates: Vec<Candidate> = input["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| Candidate {
                witness_key_id: c["witnessKeyId"].as_str().unwrap().to_string(),
                base_url: c["baseUrl"].as_str().unwrap().to_string(),
                is_anchor: c["isAnchor"].as_bool().unwrap(),
            })
            .collect();
        let got = select_known_list(
            &candidates,
            input["capacity"].as_u64().unwrap() as usize,
            input["anchorCapacity"].as_u64().unwrap() as usize,
            input["maxPerPrefix"].as_u64().unwrap() as usize,
        );
        let want: Vec<String> = v["expected"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s.as_str().unwrap().to_string())
            .collect();
        assert_eq!(got, want, "{name}");
    }
}

/// Verifies a self-certifying head with the crate's real shard-identity and
/// tree-head code; the key is parsed as exactly 64 lowercase hex characters.
fn self_certifying_outcome(
    shard_id: &str,
    key_hex: Option<&str>,
    sth: &SignedTreeHead,
) -> Result<(), &'static str> {
    use avalon_protocol::shard_identity::{
        is_self_certifying, resolve_self_certifying_key, verify_self_certifying_tree_head,
    };
    if !is_self_certifying(shard_id) {
        return Err("not_self_certifying");
    }
    let key_hex = key_hex.ok_or("missing_key")?;
    let key = avalon_protocol::shard_identity::parse_shard_public_key_hex(key_hex)
        .ok_or("malformed_key")?;
    resolve_self_certifying_key(shard_id, &key).ok_or("key_id_mismatch")?;
    if !avalon_protocol::sth::verify_tree_head(&key, sth) {
        return Err("bad_signature");
    }
    assert!(verify_self_certifying_tree_head(shard_id, &key, sth));
    Ok(())
}

#[test]
fn self_certifying_tree_head_matches_shared_vectors() {
    use avalon_protocol::shard::{parse_shard_id, ParsedShardId};
    use avalon_protocol::shard_identity::derive_self_certifying_id;

    let doc = load("self-certifying-tree-head.json");
    let signing_key = signing_key_from_seed_hex(doc["signingKeySeedHex"].as_str().unwrap());
    let other_key = signing_key_from_seed_hex(doc["otherKeySeedHex"].as_str().unwrap());
    for (key, key_field, id_field) in [
        (&signing_key, "signingPublicKeyHex", "selfCertifyingId"),
        (&other_key, "otherPublicKeyHex", "otherSelfCertifyingId"),
    ] {
        assert_eq!(
            hex::encode(key.verifying_key().as_bytes()),
            doc[key_field].as_str().unwrap()
        );
        assert_eq!(
            derive_self_certifying_id(&key.verifying_key()),
            doc[id_field].as_str().unwrap()
        );
    }

    let vectors = doc["vectors"].as_array().expect("vectors array");
    assert!(!vectors.is_empty());
    for v in vectors {
        let name = v["name"].as_str().unwrap();
        let input = &v["input"];
        let shard_id = input["shardId"].as_str().unwrap();
        let key_hex = input.get("signingPublicKeyHex").and_then(|k| k.as_str());
        let sth = sth_from_json(&input["head"], input["head"]["networkId"].as_str().unwrap());
        let rfc3339 = time::OffsetDateTime::parse(
            input["head"]["createdAtRfc3339"].as_str().unwrap(),
            &time::format_description::well_known::Rfc3339,
        )
        .unwrap();
        assert_eq!(
            rfc3339.unix_timestamp(),
            sth.created_at.unix_timestamp(),
            "{name}: createdAtRfc3339 floors to createdAtUnixSeconds"
        );

        let check = match parse_shard_id(shard_id) {
            Ok(ParsedShardId::SelfCertifying { .. }) => "self_certifying",
            Ok(ParsedShardId::Core) => "core_network",
            _ => "unsupported",
        };
        assert_eq!(check, v["expected"]["check"].as_str().unwrap(), "{name}");

        let outcome = self_certifying_outcome(shard_id, key_hex, &sth);
        assert_eq!(
            outcome.is_ok(),
            v["expected"]["verified"].as_bool().unwrap(),
            "{name}"
        );
        assert_eq!(
            outcome.err(),
            v["expected"]["failure"].as_str(),
            "{name}: failure reason"
        );
    }
}

fn assert_identity_signing_vectors(file: &str, build: impl Fn(&Value, &Value) -> Vec<u8>) {
    use avalon_protocol::ed25519_key::verify_strict_signature;

    let doc = load(file);
    let key = signing_key_from_seed_hex(doc["signingKeySeedHex"].as_str().unwrap());
    assert_eq!(
        hex::encode(key.verifying_key().as_bytes()),
        doc["signingPublicKeyHex"].as_str().unwrap()
    );
    let verifies = |bytes: &[u8], sig_hex: &str| {
        let sig: [u8; 64] = hex::decode(sig_hex).unwrap().try_into().unwrap();
        verify_strict_signature(&key.verifying_key(), bytes, &sig)
    };
    let vectors = doc["vectors"].as_array().expect("vectors array");
    assert!(!vectors.is_empty());
    for v in vectors {
        let name = v["name"].as_str().unwrap();
        let bytes = build(&doc, &v["input"]);
        let expected = &v["expected"];
        assert_eq!(
            hex::encode(&bytes),
            expected["signingBytesHex"].as_str().unwrap(),
            "{name}: hex"
        );
        let sig_hex = expected["signatureHex"].as_str().unwrap();
        assert_signature_matches(name, &key, &bytes, sig_hex);
        assert!(verifies(&bytes, sig_hex), "{name}: strict verify");
    }
    // A signature made for other bytes, or over the retired text layout, never verifies.
    for group in ["replayVectors", "legacyLayoutVectors"] {
        let cases = doc[group].as_array().expect(group);
        assert!(!cases.is_empty(), "{file} {group}");
        for r in cases {
            let name = r["name"].as_str().unwrap();
            let bytes = build(&doc, &r["input"]);
            if let Some(legacy) = r["legacySigningBytesUtf8"].as_str() {
                assert_ne!(legacy.as_bytes(), &bytes[..], "{name}");
                assert!(verifies(
                    legacy.as_bytes(),
                    r["signatureHex"].as_str().unwrap()
                ));
            }
            assert_eq!(
                verifies(&bytes, r["signatureHex"].as_str().unwrap()),
                r["expected"]["valid"].as_bool().unwrap(),
                "{name}"
            );
        }
    }
}

fn identity_id_of(v: &Value, field: &str) -> avalon_protocol::identity_id::IdentityId {
    v[field].as_str().unwrap().parse().unwrap()
}

#[test]
fn identity_id_matches_shared_vectors() {
    use avalon_protocol::ed25519_key::parse_ed25519_public_key_hex;
    use avalon_protocol::identity_id::{derive_identity_id, IdentityId, IDENTITY_ID_DOMAIN_TAG};
    use avalon_protocol::shard_identity::derive_self_certifying_id;

    let doc = load("identity-id.json");
    assert_eq!(
        hex::encode(IDENTITY_ID_DOMAIN_TAG),
        doc["domainTagHex"].as_str().unwrap()
    );
    assert_eq!(
        IDENTITY_ID_DOMAIN_TAG,
        doc["domainTagUtf8"].as_str().unwrap().as_bytes()
    );
    let vectors = doc["vectors"].as_array().expect("vectors array");
    assert!(!vectors.is_empty());
    for v in vectors {
        let name = v["name"].as_str().unwrap();
        let (input, expected) = (&v["input"], &v["expected"]);
        match v["kind"].as_str().unwrap() {
            "derive" => {
                let key = signing_key_from_seed_hex(input["seedHex"].as_str().unwrap());
                let pk = key.verifying_key().to_bytes();
                assert_eq!(hex::encode(pk), input["publicKeyHex"].as_str().unwrap());
                assert_eq!(
                    hex::encode([IDENTITY_ID_DOMAIN_TAG, &pk].concat()),
                    expected["preimageHex"].as_str().unwrap(),
                    "{name}: preimage"
                );
                assert_eq!(
                    derive_identity_id(&pk).to_string(),
                    expected["identityId"].as_str().unwrap(),
                    "{name}"
                );
            }
            "parse" => {
                let parsed = IdentityId::parse(input["identityId"].as_str().unwrap());
                assert_eq!(
                    parsed.is_ok(),
                    expected["valid"].as_bool().unwrap(),
                    "{name}"
                );
            }
            "key_acceptability" => {
                let key = parse_ed25519_public_key_hex(input["publicKeyHex"].as_str().unwrap());
                assert_eq!(
                    key.is_some(),
                    expected["acceptable"].as_bool().unwrap(),
                    "{name}"
                );
            }
            "distinct_from_shard_id" => {
                let key = verifying_key_from_hex(input["publicKeyHex"].as_str().unwrap());
                let id = derive_identity_id(key.as_bytes());
                let shard = derive_self_certifying_id(&key);
                assert_eq!(id.to_string(), expected["identityId"].as_str().unwrap());
                assert_eq!(shard, expected["nodeShardId"].as_str().unwrap());
                assert_ne!(shard.strip_prefix("node:"), Some(id.to_string().as_str()));
                assert!(!expected["equal"].as_bool().unwrap());
            }
            "strict_verify" => {
                let key = verifying_key_from_hex(input["publicKeyHex"].as_str().unwrap());
                let message = hex::decode(input["messageHex"].as_str().unwrap()).unwrap();
                let sig: [u8; 64] = hex::decode(input["signatureHex"].as_str().unwrap())
                    .unwrap()
                    .try_into()
                    .unwrap();
                assert_eq!(
                    avalon_protocol::ed25519_key::verify_strict_signature(&key, &message, &sig),
                    expected["valid"].as_bool().unwrap(),
                    "{name}"
                );
            }
            other => panic!("{name}: unknown vector kind {other}"),
        }
    }
}

fn parse_seq(v: &Value) -> u64 {
    v["seq"].as_str().unwrap().parse().unwrap()
}

fn parse_prev_hash(v: &Value) -> Option<[u8; 32]> {
    v["prevHashHex"]
        .as_str()
        .map(|h| hex::decode(h).unwrap().try_into().unwrap())
}

#[test]
fn identity_created_signing_matches_shared_vectors() {
    use avalon_protocol::identity_id::identity_created_signing_bytes;
    assert_identity_signing_vectors("identity-created-signing.json", |doc, input| {
        let pk = verifying_key_from_hex(doc["signingPublicKeyHex"].as_str().unwrap());
        let id = identity_id_of(doc, "identityId");
        assert!(id.matches_key(pk.as_bytes()));
        identity_created_signing_bytes(
            input["networkId"].as_str().unwrap(),
            input["shardId"].as_str().unwrap(),
            parse_uuid(input, "ticketId"),
            &id,
            pk.as_bytes(),
            input["displayName"].as_str().unwrap(),
        )
    });
}

#[test]
fn device_grant_approval_matches_shared_vectors() {
    use avalon_protocol::identity_id::device_grant_approval_signing_bytes;
    assert_identity_signing_vectors("device-grant-approval.json", |_, input| {
        let requested = verifying_key_from_hex(input["requestedPublicKeyHex"].as_str().unwrap());
        device_grant_approval_signing_bytes(
            parse_uuid(input, "grantId"),
            &identity_id_of(input, "identityId"),
            parse_uuid(input, "approverSigningKeyId"),
            requested.as_bytes(),
            parse_seq(input),
            parse_prev_hash(input).as_ref(),
        )
    });
}

#[test]
fn signing_key_revoked_matches_shared_vectors() {
    use avalon_protocol::identity_id::signing_key_revoked_signing_bytes;
    assert_identity_signing_vectors("signing-key-revoked.json", |_, input| {
        signing_key_revoked_signing_bytes(
            &identity_id_of(input, "identityId"),
            parse_uuid(input, "signingKeyId"),
            parse_uuid(input, "revokedBySigningKeyId"),
            parse_seq(input),
            parse_prev_hash(input).as_ref(),
        )
    });
}

fn conformance_tag(doc_tag: &str) -> avalon_protocol::signing_bytes::DomainTag {
    *avalon_protocol::signing_bytes::tags::ALL
        .iter()
        .find(|t| t.as_str() == doc_tag)
        .unwrap_or_else(|| panic!("tag {doc_tag} is not in the registry"))
}

fn field_bytes(field: &Value) -> Vec<u8> {
    let repeat = |unit: Vec<u8>| unit.repeat(field["count"].as_u64().unwrap() as usize);
    match field["type"].as_str().unwrap() {
        "str" => match field["utf8"].as_str() {
            Some(s) => s.as_bytes().to_vec(),
            None => repeat(field["repeatUtf8"].as_str().unwrap().as_bytes().to_vec()),
        },
        "bytes" => match field["hex"].as_str() {
            Some(h) => hex::decode(h).unwrap(),
            None => repeat(hex::decode(field["repeatByteHex"].as_str().unwrap()).unwrap()),
        },
        _ => hex::decode(field["hex"].as_str().unwrap()).unwrap(),
    }
}

fn field_int(field: &Value) -> String {
    field["value"].as_str().unwrap().to_string()
}

#[test]
fn structured_signing_bytes_build_and_read_back_per_shared_vectors() {
    use avalon_protocol::signing_bytes::{Builder, Reader};
    let doc = load("structured-signing-bytes.json");
    for vector in doc["vectors"].as_array().unwrap() {
        let name = vector["name"].as_str().unwrap();
        let input = &vector["input"];
        let tag = conformance_tag(input["tag"].as_str().unwrap());
        let version = input["version"].as_u64().unwrap() as u16;
        let fields = input["fields"].as_array().unwrap();

        let mut builder = Builder::new(tag, version);
        for f in fields {
            let ty = f["type"].as_str().unwrap();
            builder = match ty {
                "str" => builder.str(std::str::from_utf8(&field_bytes(f)).unwrap()),
                "bytes" => builder.bytes(&field_bytes(f)),
                "key" => builder.key(&field_bytes(f).try_into().unwrap()),
                "hash" => builder.hash(&field_bytes(f).try_into().unwrap()),
                "fixed" => builder.fixed::<4>(&field_bytes(f).try_into().unwrap()),
                "uuid" => builder.uuid(Uuid::parse_str(f["value"].as_str().unwrap()).unwrap()),
                "u8" => builder.u8(field_int(f).parse().unwrap()),
                "u16" => builder.u16(field_int(f).parse().unwrap()),
                "u32" => builder.u32(field_int(f).parse().unwrap()),
                "u64" => builder.u64(field_int(f).parse().unwrap()),
                "i64" => builder.i64(field_int(f).parse().unwrap()),
                other => panic!("[{name}] unknown field type {other}"),
            };
        }
        let message = builder.finish().unwrap();

        let expected = &vector["expected"];
        match expected["signingBytesHex"].as_str() {
            Some(h) => assert_eq!(hex::encode(&message), h, "[{name}] bytes diverged"),
            None => {
                use sha2::{Digest, Sha256};
                assert_eq!(
                    message.len() as u64,
                    expected["signingBytesLength"].as_u64().unwrap(),
                    "[{name}] length diverged"
                );
                assert_eq!(
                    hex::encode(Sha256::digest(&message)),
                    expected["signingBytesSha256Hex"].as_str().unwrap(),
                    "[{name}] digest diverged"
                );
            }
        }

        let mut reader = Reader::new(tag, &message).unwrap();
        assert_eq!(reader.version(), version, "[{name}]");
        for f in fields {
            let ty = f["type"].as_str().unwrap();
            match ty {
                "str" => assert_eq!(reader.str().unwrap().as_bytes(), field_bytes(f), "[{name}]"),
                "bytes" => assert_eq!(reader.bytes().unwrap(), field_bytes(f), "[{name}]"),
                "key" | "hash" => {
                    assert_eq!(reader.fixed::<32>().unwrap().to_vec(), field_bytes(f))
                }
                "fixed" => assert_eq!(reader.fixed::<4>().unwrap().to_vec(), field_bytes(f)),
                "uuid" => assert_eq!(reader.uuid().unwrap().to_string(), f["value"]),
                "u8" => assert_eq!(reader.u8().unwrap().to_string(), field_int(f)),
                "u16" => assert_eq!(reader.u16().unwrap().to_string(), field_int(f)),
                "u32" => assert_eq!(reader.u32().unwrap().to_string(), field_int(f)),
                "u64" => assert_eq!(reader.u64().unwrap().to_string(), field_int(f)),
                "i64" => assert_eq!(reader.i64().unwrap().to_string(), field_int(f)),
                other => panic!("[{name}] unknown field type {other}"),
            }
        }
        reader.finish().unwrap_or_else(|e| panic!("[{name}] {e}"));
    }
}

#[test]
fn structured_signing_bytes_reject_per_shared_vectors() {
    use avalon_protocol::signing_bytes::{Reader, SigningBytesError};
    let doc = load("structured-signing-bytes.json");
    let vectors = doc["rejectVectors"].as_array().unwrap();
    assert!(!vectors.is_empty());
    for vector in vectors {
        let name = vector["name"].as_str().unwrap();
        let input = &vector["input"];
        let message = hex::decode(input["messageHex"].as_str().unwrap()).unwrap();
        let read = || -> Result<(), SigningBytesError> {
            let mut r = Reader::new(conformance_tag(input["tag"].as_str().unwrap()), &message)?;
            for ty in input["read"].as_array().unwrap() {
                match ty.as_str().unwrap() {
                    "str" => drop(r.str()?),
                    "bytes" => drop(r.bytes()?),
                    "u32" => drop(r.u32()?),
                    other => panic!("[{name}] unknown read type {other}"),
                }
            }
            r.finish()
        };
        let code = match read().unwrap_err() {
            SigningBytesError::TagMismatch => "tag_mismatch",
            SigningBytesError::Truncated => "truncated",
            SigningBytesError::InvalidUtf8 => "invalid_utf8",
            SigningBytesError::TrailingBytes => "trailing_bytes",
            SigningBytesError::FieldTooLong => "field_too_long",
        };
        assert_eq!(
            code,
            vector["expected"]["error"].as_str().unwrap(),
            "[{name}]"
        );
    }
}

#[test]
fn domain_tag_registry_matches_shared_vectors() {
    let doc = load("domain-tags.json");
    let want: Vec<&str> = doc["tags"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["tag"].as_str().unwrap())
        .collect();
    let have: Vec<&str> = avalon_protocol::signing_bytes::tags::ALL
        .iter()
        .map(|t| t.as_str())
        .collect();
    assert_eq!(have, want, "registry diverged from domain-tags.json");
}

/// `canonical-payload.json`: restricted RFC 8785 canonical encoding of
/// free-form payloads, accepted and rejected inputs.
#[test]
fn canonical_payload_matches_shared_vectors() {
    use avalon_protocol::canonical_payload::canonicalize_str;

    let doc = load("canonical-payload.json");
    let vectors = doc["vectors"].as_array().expect("vectors array");
    assert!(vectors.len() > 50, "vectors file looks truncated");
    for v in vectors {
        let name = v["name"].as_str().expect("name");
        let text = v["input"]["jsonUtf8"].as_str().expect("jsonUtf8");
        let got = canonicalize_str(text);
        match v["expected"]["error"].as_str() {
            None => assert_eq!(
                got.as_deref(),
                Ok(v["expected"]["canonicalUtf8"]
                    .as_str()
                    .expect("canonicalUtf8")),
                "{name}"
            ),
            Some(code) => {
                let got_code = match got {
                    Ok(out) => panic!("{name}: expected {code}, got {out}"),
                    Err(e) => e.code(),
                };
                assert_eq!(got_code, code, "{name}");
            }
        }
    }
}
