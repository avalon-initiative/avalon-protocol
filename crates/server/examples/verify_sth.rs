//! Verifies Signed Tree Head responses the way a client would, for the
//! rollout scenario in `scripts/witness-drill.sh`.
//!
//! - `verify_sth old`: reads one STH JSON object on stdin and checks only the
//!   author signature against `AVALON_SETTLEMENT_VERIFY_KEY` (or the key
//!   derived from the signing key) — what a client pinned to one key does.
//! - `verify_sth cosigned <witness-key-hex>...`: reads a JSON array of STH
//!   responses (one per witness node, all for the same head) and accepts the
//!   head only if the author signature verifies and a majority of the given
//!   witness keys cosigned it.
//! - `verify_sth pubkey <seed-hex>`: prints the hex verifying key for a seed.

use std::io::Read;

use avalon_protocol::cosigned_sth::{verify_cosigned_tree_head, CosignedTreeHead};
use avalon_protocol::sth::{load_verify_key_from_env, verify_tree_head, SignedTreeHead};
use avalon_server::cosign_verify::WitnessCosignatureDto;
use ed25519_dalek::{SigningKey, VerifyingKey};
use serde::Deserialize;
use time::OffsetDateTime;

#[derive(Deserialize)]
struct Response {
    tree_size: i64,
    root_hash: String,
    network_id: String,
    signing_key_id: String,
    signature: String,
    #[serde(with = "time::serde::rfc3339")]
    created_at: OffsetDateTime,
    #[serde(default)]
    cosignatures: Vec<WitnessCosignatureDto>,
}

impl Response {
    fn sth(&self) -> SignedTreeHead {
        SignedTreeHead {
            tree_size: self.tree_size,
            root_hash: self.root_hash.clone(),
            network_id: self.network_id.clone(),
            signing_key_id: self.signing_key_id.clone(),
            signature: self.signature.clone(),
            created_at: self.created_at,
        }
    }
}

fn stdin() -> String {
    let mut buf = String::new();
    std::io::stdin()
        .read_to_string(&mut buf)
        .expect("read stdin");
    buf
}

fn hex_key(hex_value: &str) -> VerifyingKey {
    let bytes: [u8; 32] = hex::decode(hex_value)
        .expect("hex key")
        .try_into()
        .expect("32-byte key");
    VerifyingKey::from_bytes(&bytes).expect("valid key")
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("pubkey") => {
            let seed: [u8; 32] = hex::decode(&args[1])
                .expect("hex seed")
                .try_into()
                .expect("32-byte seed");
            println!(
                "{}",
                hex::encode(SigningKey::from_bytes(&seed).verifying_key().to_bytes())
            );
        }
        Some("old") => {
            let response: Response = serde_json::from_str(&stdin()).expect("STH json");
            let key = load_verify_key_from_env().expect("verify key in env");
            std::process::exit(if verify_tree_head(&key, &response.sth()) {
                0
            } else {
                1
            });
        }
        Some("cosigned") => {
            let responses: Vec<Response> = serde_json::from_str(&stdin()).expect("STH json array");
            let author = load_verify_key_from_env().expect("verify key in env");
            let sth = responses[0].sth();
            let mut cosignatures = Vec::new();
            for response in &responses {
                if response.root_hash != sth.root_hash || response.tree_size != sth.tree_size {
                    std::process::exit(2);
                }
                for dto in &response.cosignatures {
                    if !cosignatures
                        .iter()
                        .any(|c: &avalon_protocol::witness::WitnessCosignature| {
                            c.witness_key_id == dto.witness_key_id
                        })
                    {
                        cosignatures.push(dto.to_witness_cosignature(&sth));
                    }
                }
            }
            let known: Vec<(String, VerifyingKey)> =
                args[1..].iter().map(|k| (k.clone(), hex_key(k))).collect();
            let now = OffsetDateTime::now_utc();
            let head = CosignedTreeHead { sth, cosignatures };
            let accepted = verify_cosigned_tree_head(
                &author,
                &head,
                &known,
                now - time::Duration::minutes(10),
                now,
            );
            std::process::exit(if accepted { 0 } else { 1 });
        }
        _ => {
            eprintln!("usage: verify_sth old | cosigned <key>... | pubkey <seed>");
            std::process::exit(64);
        }
    }
}
