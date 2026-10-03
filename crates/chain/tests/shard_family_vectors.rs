//! Generates and checks `conformance/vectors/shard-family-head.json` from the
//! real shard family functions. Offline: no server, no database.
//!
//! `cargo test -p avalon-chain --test shard_family_vectors` fails when the file
//! is stale; `AVALON_REGEN_VECTORS=1` rewrites it.

use avalon_chain::cross_shard::{
    compute_shard_family_head, family_inclusion_proof, is_family_member, verify_family_inclusion,
    FamilyInclusionProof, ShardTreeHead,
};
use avalon_protocol::shard::{is_family_owner_id, shard_family_owner};
use avalon_protocol::sth::SignedTreeHead;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::path::PathBuf;

const FILE: &str = "shard-family-head.json";

fn vectors_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../conformance/vectors")
        .join(FILE)
}

fn sha_hex(label: &str) -> String {
    hex::encode(Sha256::digest(label.as_bytes()))
}

/// A deterministic head: the hashes are derived from the label, the signature is
/// 64 pseudo bytes (the leaf commits to its text, it is not verified here).
fn head(shard_id: &str, tree_size: i64, label: &str) -> ShardTreeHead {
    ShardTreeHead {
        shard_id: shard_id.to_string(),
        sth: SignedTreeHead {
            tree_size,
            root_hash: sha_hex(&format!("root:{shard_id}:{label}")),
            network_id: "family-vectors".to_string(),
            signing_key_id: format!("key-{label}"),
            signature: format!(
                "{}{}",
                sha_hex(&format!("sig-a:{shard_id}:{label}")),
                sha_hex(&format!("sig-b:{shard_id}:{label}"))
            ),
            created_at: time::OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap(),
        },
    }
}

fn head_json(h: &ShardTreeHead) -> Value {
    let size = if h.sth.tree_size.unsigned_abs() >= 1 << 53 {
        Value::String(h.sth.tree_size.to_string())
    } else {
        json!(h.sth.tree_size)
    };
    json!({
        "shardId": h.shard_id,
        "treeSize": size,
        "rootHashHex": h.sth.root_hash,
        "signingKeyId": h.sth.signing_key_id,
        "signatureHex": h.sth.signature,
    })
}

fn family_case(name: &str, owner: &str, heads: Vec<ShardTreeHead>) -> Value {
    let computed = compute_shard_family_head(owner, &BTreeSet::new(), heads.clone());
    json!({
        "name": name,
        "input": {
            "owner": owner,
            "heads": heads.iter().map(head_json).collect::<Vec<_>>(),
        },
        "expected": {
            "rootHashHex": computed.root_hash,
            "shardCount": computed.shard_count,
            "memberShardIds": computed.members.iter().map(|m| m.shard_id.clone()).collect::<Vec<_>>(),
        },
    })
}

fn flip(hex_text: &str) -> String {
    let mut bytes = hex::decode(hex_text).unwrap();
    bytes[0] ^= 1;
    hex::encode(bytes)
}

struct Claim {
    owner: String,
    root: String,
    shard_id: String,
    head: ShardTreeHead,
    leaf_index: usize,
    tree_size: usize,
    path: Vec<String>,
}

fn proof_case(name: &str, c: &Claim) -> Value {
    let path: Option<Vec<[u8; 32]>> = c
        .path
        .iter()
        .map(|p| hex::decode(p).unwrap().try_into().ok())
        .collect();
    // A path element that is not 32 bytes is rejected by the wire parser.
    let verified = path.is_some_and(|path| {
        verify_family_inclusion(
            &c.owner,
            &c.root,
            &FamilyInclusionProof {
                leaf_index: c.leaf_index,
                tree_size: c.tree_size,
                path,
            },
            &c.shard_id,
            &c.head.sth,
        )
        .unwrap_or(false)
    });
    json!({
        "name": name,
        "input": {
            "owner": c.owner,
            "rootHashHex": c.root,
            "shardId": c.shard_id,
            "head": head_json(&c.head),
            "proof": {
                "leafIndex": c.leaf_index,
                "treeSize": c.tree_size,
                "pathHex": c.path,
            },
        },
        "expected": { "verified": verified },
    })
}

/// A proof for `member` over `heads`, as the server builds it.
fn honest(owner: &str, heads: &[ShardTreeHead], member: &str) -> Claim {
    let root = compute_shard_family_head(owner, &BTreeSet::new(), heads.to_vec()).root_hash;
    let proof = family_inclusion_proof(owner, heads, member)
        .expect("member present")
        .expect("proof");
    Claim {
        owner: owner.to_string(),
        root,
        shard_id: member.to_string(),
        head: heads.iter().find(|h| h.shard_id == member).unwrap().clone(),
        leaf_index: proof.leaf_index,
        tree_size: proof.tree_size,
        path: proof.path.iter().map(hex::encode).collect(),
    }
}

fn clone_claim(c: &Claim) -> Claim {
    Claim {
        owner: c.owner.clone(),
        root: c.root.clone(),
        shard_id: c.shard_id.clone(),
        head: c.head.clone(),
        leaf_index: c.leaf_index,
        tree_size: c.tree_size,
        path: c.path.clone(),
    }
}

fn five(owner: &str) -> Vec<ShardTreeHead> {
    ["", "/1", "/10", "/2", "/b-3"]
        .iter()
        .enumerate()
        .map(|(i, suffix)| head(&format!("{owner}{suffix}"), 10 + i as i64, "five"))
        .collect()
}

fn family_vectors() -> Vec<Value> {
    let node_id = format!("node:{}", "ab".repeat(32));
    let noisy = vec![
        head("game:x/2", 7, "noisy"),
        head("core", 99, "noisy"),
        head("game:xy", 3, "noisy"),
        head("game:x", 5, "noisy"),
        head("app:x", 4, "noisy"),
        head(&node_id, 8, "noisy"),
        head("game:x/10", 6, "noisy"),
        head("game:xy/2", 2, "noisy"),
    ];
    let mut reversed_five = five("game:x");
    reversed_five.reverse();
    let mut big = vec![head("game:big", i64::MAX, "big")];
    big.push(head("game:big/2", 1 << 53, "big"));
    big.push(head("game:big/3", 0, "big"));
    vec![
        family_case(
            "members out of order with non-members ignored",
            "game:x",
            noisy.clone(),
        ),
        family_case(
            "same heads under another owner keep only that owner's member",
            "app:x",
            noisy.clone(),
        ),
        family_case(
            "instance order is bytewise, not numeric",
            "game:x",
            vec![
                head("game:x/2", 1, "order"),
                head("game:x/10", 1, "order"),
                head("game:x/1", 1, "order"),
            ],
        ),
        family_case("single member", "game:x", vec![head("game:x", 4, "one")]),
        family_case(
            "only an instance, no bare owner shard",
            "service:x",
            vec![head("service:x/eu-1", 9, "inst")],
        ),
        family_case(
            "two members",
            "game:x",
            vec![head("game:x/2", 3, "p2"), head("game:x", 2, "p2")],
        ),
        family_case("five members reversed", "game:x", reversed_five),
        family_case("empty family: no heads at all", "game:x", Vec::new()),
        family_case(
            "empty family: only non-members",
            "game:x",
            vec![head("game:xy", 1, "none"), head("core", 2, "none")],
        ),
        family_case(
            "empty family of another owner is a different root",
            "game:y",
            Vec::new(),
        ),
        family_case(
            "non-ascii owner is length-prefixed in utf-8 bytes",
            "game:\u{e9}x",
            vec![
                head("game:\u{e9}x/2", 5, "utf8"),
                head("game:\u{e9}x", 6, "utf8"),
            ],
        ),
        family_case("tree sizes at the i64 edges", "game:big", big),
    ]
}

fn proof_vectors() -> Vec<Value> {
    let mut out = Vec::new();
    let three = vec![
        head("game:x/2", 7, "p3"),
        head("game:x", 5, "p3"),
        head("game:x/10", 6, "p3"),
        head("game:xy", 1, "p3"),
        head("core", 1, "p3"),
    ];
    let five_heads = five("game:x");
    let two = vec![head("game:x", 2, "p2"), head("game:x/2", 3, "p2")];
    let one = vec![head("game:x", 4, "p1")];
    let big = vec![
        head("game:big", i64::MAX, "pb"),
        head("game:big/2", 1 << 53, "pb"),
    ];
    for (label, owner, heads, members) in [
        (
            "three",
            "game:x",
            &three,
            vec!["game:x", "game:x/10", "game:x/2"],
        ),
        (
            "five",
            "game:x",
            &five_heads,
            vec!["game:x", "game:x/1", "game:x/10", "game:x/2", "game:x/b-3"],
        ),
        ("two", "game:x", &two, vec!["game:x", "game:x/2"]),
        ("one", "game:x", &one, vec!["game:x"]),
        ("big", "game:big", &big, vec!["game:big", "game:big/2"]),
    ] {
        for m in members {
            out.push(proof_case(
                &format!("{label}-member family: proof for {m}"),
                &honest(owner, heads, m),
            ));
        }
    }

    let base = honest("game:x", &five_heads, "game:x/10");
    let case = |name: &str, f: &dyn Fn(&mut Claim)| {
        let mut c = clone_claim(&base);
        f(&mut c);
        proof_case(name, &c)
    };
    let sibling = honest("game:x", &five_heads, "game:x/2");
    out.push(case("wrong owner: another family", &|c| {
        c.owner = "app:x".to_string()
    }));
    out.push(case("wrong owner: sibling-looking owner", &|c| {
        c.owner = "game:xy".to_string()
    }));
    out.push(case("shard is not a member of the owner", &|c| {
        c.shard_id = "game:xy".to_string()
    }));
    out.push(case("shard id is another member's", &|c| {
        c.shard_id = "game:x/2".to_string()
    }));
    out.push(case("tampered path: first element", &|c| {
        c.path[0] = flip(&c.path[0])
    }));
    out.push(case("tampered path: last element", &|c| {
        let last = c.path.len() - 1;
        c.path[last] = flip(&c.path[last])
    }));
    out.push(case("path too short", &|c| {
        c.path.pop();
    }));
    out.push(case("path too long", &|c| c.path.push(sha_hex("extra"))));
    out.push(case("empty path for a multi-member family", &|c| {
        c.path.clear()
    }));
    out.push(case("path element is not 32 bytes", &|c| {
        c.path[0] = c.path[0][..62].to_string()
    }));
    out.push(case("tampered head: signature", &|c| {
        c.head.sth.signature = flip(&c.head.sth.signature)
    }));
    out.push(case("tampered head: tree size", &|c| {
        c.head.sth.tree_size += 1
    }));
    out.push(case("tampered head: root hash", &|c| {
        c.head.sth.root_hash = flip(&c.head.sth.root_hash)
    }));
    out.push(case("tampered head: signing key id", &|c| {
        c.head.sth.signing_key_id.push('x')
    }));
    out.push(case("head of a different member", &|c| {
        c.head = sibling.head.clone()
    }));
    out.push(case("root hash is not the family root", &|c| {
        c.root = flip(&c.root)
    }));
    out.push(case("root hash is not hex", &|c| c.root = "zz".repeat(32)));
    out.push(case("root hash is too short", &|c| {
        c.root = c.root[..62].to_string()
    }));
    out.push(case("wrong leaf index", &|c| c.leaf_index += 1));
    out.push(case("leaf index equals tree size", &|c| {
        c.leaf_index = c.tree_size
    }));
    out.push(case("tree size of a different path shape", &|c| {
        c.tree_size = 3
    }));
    out.push(case("tree size of one", &|c| c.tree_size = 1));
    // The path hashes are opaque, so a larger tree with the same shape still verifies.
    out.push(case(
        "tree size one larger with the same path shape",
        &|c| c.tree_size += 1,
    ));
    out
}

fn owner_vectors() -> (Vec<Value>, Vec<Value>, Vec<Value>) {
    let node_id = format!("node:{}", "ab".repeat(32));
    let shard_ids = [
        "game:x",
        "game:x/2",
        "game:x/eu-1",
        "game:xy",
        "game:xy/2",
        "app:x",
        "app:x/2",
        "service:y/b",
        "core",
        &node_id,
        &format!("node:{}", "AB".repeat(32)),
        "node:abcd",
        "",
        "game:",
        "game:/2",
        "game:x/",
        "game:x/2/3",
        "game:x/UP",
        "game:x/-a",
        &format!("game:x/{}", "a".repeat(64)),
        &format!("game:x/{}", "a".repeat(65)),
        "game:x:y",
        "game:x y",
        "game:x/2 ",
        " game:x",
        "game:x\n",
        "game:x\u{a0}",
        "game:\u{85}",
        "game:\u{3000}",
        "game:\u{2003}x",
        "game:x\u{2028}",
        "game:\u{200b}",
        "game:\u{feff}x",
        "game:\u{e9}",
        "game:\u{e9}/2",
        "GAME:x",
        "other:x",
        "x",
    ];
    let family_owner = shard_ids
        .iter()
        .map(|id| json!({ "shardId": id, "expectedOwner": shard_family_owner(id) }))
        .collect();
    let owner_ids = shard_ids
        .iter()
        .map(|id| json!({ "owner": id, "expected": is_family_owner_id(id) }))
        .collect();
    let members = [
        ("game:x", "game:x"),
        ("game:x", "game:x/2"),
        ("game:x", "game:xy"),
        ("game:x", "game:xy/2"),
        ("game:x", "app:x"),
        ("game:x", "core"),
        ("game:x", node_id.as_str()),
        ("game:x", "game:x/"),
        ("game:x", "game:x/UP"),
        ("game:x", "game:x y"),
        ("game:x", ""),
        ("game:x/2", "game:x/2"),
        ("game:x/2", "game:x"),
        ("", ""),
        ("game:\u{e9}", "game:\u{e9}/2"),
        ("game:e", "game:\u{e9}/2"),
    ]
    .iter()
    .map(|(owner, id)| {
        json!({ "owner": owner, "shardId": id, "expected": is_family_member(owner, id) })
    })
    .collect();
    (family_owner, owner_ids, members)
}

fn build() -> Value {
    let (family_owner, owner_ids, members) = owner_vectors();
    json!({
        "$schema": "./SCHEMA.md#shard-family-head",
        "description": "The per-owner shard family head (avalon_chain::cross_shard compute_shard_family_head, family_inclusion_proof, verify_family_inclusion; avalon_protocol::shard shard_family_owner, is_family_owner_id) served by GET /ledger/shard-family. A family is the owner's shard ids {namespace}:{slug} and {namespace}:{slug}/{instance}; heads of any other shard are ignored. Leaf = 'avalon-shard-family-leaf-v1' || u32be(len(owner)) || owner || u32be(len(shard_id)) || shard_id || i64be(tree_size) || u32be(len(root_hash)) || root_hash || u32be(len(signing_key_id)) || signing_key_id || u32be(len(signature)) || signature, every length counting UTF-8 bytes and every string used as its text (hex is not decoded). Members are sorted bytewise by shard id and combined with the RFC 6962 tree hash; the empty family root is SHA-256('avalon-shard-family-empty-v1' || u32be(len(owner)) || owner). The signatures are placeholders: the leaf commits to their text and no vector verifies them. verified is false for any failure, including a malformed root hash.",
        "supportedIn": ["rust", "csharp", "typescript"],
        "generation": "Generated by crates/chain/tests/shard_family_vectors.rs from the functions named above; AVALON_REGEN_VECTORS=1 rewrites this file and the test fails when it is stale. Head hashes are SHA-256 of a label, signatures are two such hashes, and created_at is not part of a leaf so it is omitted. Every outcome is what the chain functions return, except 'path element is not 32 bytes', which the wire parser rejects before the chain code and is fixed as not verified.",
        "familyVectors": family_vectors(),
        "proofVectors": proof_vectors(),
        "familyOwnerVectors": family_owner,
        "ownerIdVectors": owner_ids,
        "memberVectors": members,
    })
}

#[test]
fn shard_family_vectors_match_the_chain_functions() {
    let doc = build();
    let mut text = serde_json::to_string_pretty(&doc).unwrap();
    text.push('\n');
    let path = vectors_path();
    if std::env::var_os("AVALON_REGEN_VECTORS").is_some() {
        std::fs::write(&path, &text).unwrap();
        return;
    }
    let current = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
    let current: serde_json::Value =
        serde_json::from_str(&current).unwrap_or_else(|e| panic!("{FILE} is not valid JSON: {e}"));
    assert!(
        current == doc,
        "{FILE} is stale: rerun with AVALON_REGEN_VECTORS=1"
    );
}

#[test]
fn shard_family_vectors_cover_both_outcomes() {
    let doc = build();
    let proofs = doc["proofVectors"].as_array().unwrap();
    let verified = |want: bool| {
        proofs
            .iter()
            .filter(|v| v["expected"]["verified"] == want)
            .count()
    };
    assert!(verified(true) >= 12, "a proof per member");
    assert!(verified(false) >= 15, "negatives");
    for v in doc["familyVectors"].as_array().unwrap() {
        let heads = v["input"]["heads"].as_array().unwrap().len();
        assert!(heads >= v["expected"]["shardCount"].as_u64().unwrap() as usize);
    }
}
