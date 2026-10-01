//! Builds `conformance/vectors/node-request.json` deterministically (Ed25519 is
//! deterministic, JSON keys are sorted) and checks the committed file matches.
//! Regenerate with `AVALON_REGEN_NODE_REQUEST_VECTORS=1 cargo test -p avalon-protocol
//! --test node_request_vectors`.

use avalon_protocol::node_request::*;
use ed25519_dalek::SigningKey;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::Path;

const PEER: &str = "12D3KooWExampleNodeRequestSigner";
const NOW: i64 = 1_790_000_000;
const NONCE: [u8; 16] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16];
const JSON_BODY: &[u8] = b"{\"amount\":5}";

struct Spec {
    name: String,
    method: String,
    path: String,
    body: Vec<u8>,
    net: String,
    accepted: Vec<String>,
    header: String,
    signing_recipient: Option<String>,
    signed_by_seed: bool,
    max_skew: i64,
    error: Option<&'static str>,
}

fn key() -> SigningKey {
    SigningKey::from_bytes(&[0x5a; 32])
}

fn spec(name: &str, error: Option<&'static str>) -> Spec {
    Spec {
        name: name.to_string(),
        method: "POST".into(),
        path: "/nodes/relay".into(),
        body: JSON_BODY.to_vec(),
        net: "avalon-test".into(),
        accepted: vec!["node-b".into()],
        header: String::new(),
        signing_recipient: None,
        signed_by_seed: false,
        max_skew: 60,
        error,
    }
}

impl Spec {
    fn target(&self) -> NodeRequestTarget<'_> {
        NodeRequestTarget {
            method: &self.method,
            path: &self.path,
            body: &self.body,
            network_id: &self.net,
        }
    }

    /// Signs the spec's own target for `rcpt` at `ts` and stores the header.
    fn signed(mut self, rcpt: &str, ts: i64) -> Self {
        let auth = sign_node_request(&key(), PEER, &self.target(), rcpt, ts, NONCE).unwrap();
        self.header = encode_node_request_header(&auth);
        self.signing_recipient = Some(rcpt.into());
        self.signed_by_seed = true;
        self
    }

    fn header(mut self, header: String) -> Self {
        self.header = header;
        self
    }

    /// Marks the header as covering this spec's target for `rcpt` (pins messageHex).
    fn message_for(mut self, rcpt: &str) -> Self {
        self.signing_recipient = Some(rcpt.into());
        self
    }

    fn json(&self) -> Value {
        let mut input = json!({
            "method": self.method,
            "path": self.path,
            "bodyHex": hex::encode(&self.body),
            "bodySha256Hex": hex::encode(Sha256::digest(&self.body)),
            "networkId": self.net,
            "acceptedRecipients": self.accepted,
            "header": self.header,
            "now": NOW,
            "maxSkewSeconds": self.max_skew,
            "signedBySeed": self.signed_by_seed,
        });
        if let Some(rcpt) = &self.signing_recipient {
            let auth = parse_node_request_header(&self.header).unwrap();
            let msg = node_request_signing_message(
                &self.target(),
                rcpt,
                &auth.peer_id,
                auth.timestamp,
                &auth.nonce,
            )
            .unwrap();
            input["signingRecipient"] = json!(rcpt);
            input["messageHex"] = json!(hex::encode(msg));
        }
        json!({
            "name": self.name,
            "input": input,
            "expected": {"accepted": self.error.is_none(), "error": self.error},
        })
    }
}

fn add_scalar_l(sig: &mut [u8; 64]) {
    const L: [u8; 32] = [
        0xed, 0xd3, 0xf5, 0x5c, 0x1a, 0x63, 0x12, 0x58, 0xd6, 0x9c, 0xf7, 0xa2, 0xde, 0xf9, 0xde,
        0x14, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x10,
    ];
    let mut carry = 0u16;
    for (byte, l) in sig[32..].iter_mut().zip(L) {
        let sum = *byte as u16 + l as u16 + carry;
        *byte = sum as u8;
        carry = sum >> 8;
    }
    assert_eq!(carry, 0);
}

fn build() -> Value {
    let good = spec("", None).signed("node-b", NOW);
    let h = good.header.clone();
    let kh = hex::encode(key().verifying_key().to_bytes());
    let auth_of = |header: &str| parse_node_request_header(header).unwrap();
    let mut v: Vec<Spec> = vec![];

    v.push(spec("accepted: POST with a JSON body", None).signed("node-b", NOW));
    let mut s = spec("accepted: non-UTF-8 binary body", None);
    s.body = vec![0xff, 0x00, 0xfe, 0x80, 0x01];
    v.push(s.signed("node-b", NOW));
    let mut s = spec("accepted: one of several accepted recipients", None);
    s.accepted = vec!["node-a".into(), "node-b".into(), "node-c".into()];
    v.push(s.signed("node-b", NOW));
    let mut s = spec("accepted: GET with empty body", None);
    s.method = "GET".into();
    s.path = "/mirror/notify".into();
    s.body = vec![];
    v.push(s.signed("node-b", NOW));
    v.push(spec("accepted: timestamp exactly 60 s old", None).signed("node-b", NOW - 60));
    v.push(spec("accepted: timestamp exactly 60 s ahead", None).signed("node-b", NOW + 60));
    let mut s = spec("accepted: percent-encoding is signed raw", None);
    s.path = "/nodes/relay%2Fx/%2e%2e".into();
    v.push(s.signed("node-b", NOW));
    let mut s = spec("accepted: trailing slash", None);
    s.path = "/nodes/relay/".into();
    v.push(s.signed("node-b", NOW));
    let mut s = spec("accepted: root path", None);
    s.path = "/".into();
    v.push(s.signed("node-b", NOW));
    // Every bounded field at its maximum; the header stays under 512 bytes.
    let name = "n".repeat(MAX_NAME_LEN);
    let mut s = spec("accepted: every field at its maximum length", None);
    s.method = "M".repeat(MAX_METHOD_LEN);
    s.path = format!("/{}", "a".repeat(MAX_PATH_LEN - 1));
    s.net = name.clone();
    s.accepted = vec![name.clone()];
    let auth = sign_node_request(
        &key(),
        &"p".repeat(MAX_PEER_ID_LEN),
        &s.target(),
        &name,
        NOW,
        NONCE,
    )
    .unwrap();
    s.header = encode_node_request_header(&auth);
    s.signing_recipient = Some(name.clone());
    s.signed_by_seed = true;
    assert!(s.header.len() <= NODE_REQUEST_MAX_HEADER_LEN);
    v.push(s);

    v.push(spec("rejected: timestamp 61 s old", Some("stale")).signed("node-b", NOW - 61));
    v.push(spec("rejected: timestamp 61 s ahead", Some("future")).signed("node-b", NOW + 61));

    let mut s = spec(
        "rejected: body differs from the signed body",
        Some("bad_signature"),
    );
    s.body = b"{\"amount\":6}".to_vec();
    v.push(s.header(h.clone()));
    let mut s = spec("rejected: method differs", Some("bad_signature"));
    s.method = "PUT".into();
    v.push(s.header(h.clone()));
    let mut s = spec("rejected: path differs", Some("bad_signature"));
    s.path = "/nodes/replicate-chat".into();
    v.push(s.header(h.clone()));
    let mut s = spec("rejected: network differs", Some("bad_signature"));
    s.net = "other-net".into();
    v.push(s.header(h.clone()));
    let mut s = spec(
        "rejected: signed recipient is not accepted",
        Some("bad_signature"),
    );
    s.accepted = vec!["node-c".into()];
    v.push(s.header(h.clone()));
    let sig_at = h.find("sig=").unwrap() + 4;
    let mut flipped = h.clone();
    flipped.replace_range(
        sig_at..sig_at + 1,
        if &h[sig_at..sig_at + 1] == "0" {
            "1"
        } else {
            "0"
        },
    );
    v.push(spec("rejected: signature bit flipped", Some("bad_signature")).header(flipped));
    let mut a = auth_of(&h);
    a.public_key = SigningKey::from_bytes(&[0x5b; 32])
        .verifying_key()
        .to_bytes();
    v.push(
        spec(
            "rejected: valid signature under a different key",
            Some("bad_signature"),
        )
        .header(encode_node_request_header(&a))
        .message_for("node-b"),
    );
    // Identity point as key and as R with S = 0: plain Ed25519 accepts, strict must not.
    let mut a = auth_of(&h);
    a.public_key = [0; 32];
    a.public_key[0] = 1;
    a.signature = [0; 64];
    a.signature[0] = 1;
    v.push(
        spec(
            "rejected: small-order public key with a trivial signature",
            Some("bad_signature"),
        )
        .header(encode_node_request_header(&a))
        .message_for("node-b"),
    );
    let mut a = auth_of(&h);
    add_scalar_l(&mut a.signature);
    v.push(
        spec("rejected: non-canonical S (S + L)", Some("bad_signature"))
            .header(encode_node_request_header(&a))
            .message_for("node-b"),
    );
    let mut a = auth_of(&h);
    a.public_key = [0; 32];
    a.public_key[0] = 2;
    v.push(
        spec(
            "rejected: public key is not a curve point",
            Some("invalid_key"),
        )
        .header(encode_node_request_header(&a)),
    );

    let bad_paths: [(&str, &str); 14] = [
        ("path carries a query", "/nodes/relay?x=1"),
        ("path carries a fragment", "/nodes/relay#f"),
        ("path without a leading slash", "nodes/relay"),
        ("empty path", ""),
        ("path with an empty segment", "/nodes//relay"),
        ("path starting with a double slash", "//nodes/relay"),
        ("path with a dot segment", "/nodes/./relay"),
        ("path with a dot-dot segment", "/nodes/.."),
        ("path with a space", "/nodes/re lay"),
        ("path with a backslash", "/nodes\\relay"),
        ("path with a NUL byte", "/nodes/re\u{0}lay"),
        ("path with a newline", "/nodes/re\nlay"),
        ("path with DEL", "/nodes/re\u{7f}lay"),
        ("path with a non-ASCII character", "/nodes/r\u{e9}lay"),
    ];
    for (name, path) in bad_paths {
        let mut s = spec(&format!("rejected: {name}"), Some("invalid_request"));
        s.path = path.into();
        v.push(s.header(h.clone()));
    }
    let mut s = spec(
        "rejected: path one byte over the maximum",
        Some("invalid_request"),
    );
    s.path = format!("/{}", "a".repeat(MAX_PATH_LEN));
    v.push(s.header(h.clone()));
    let mut s = spec("rejected: lowercase method", Some("invalid_request"));
    s.method = "post".into();
    v.push(s.header(h.clone()));
    let mut s = spec("rejected: empty method", Some("invalid_request"));
    s.method = String::new();
    v.push(s.header(h.clone()));
    let mut s = spec("rejected: empty network id", Some("invalid_request"));
    s.net = String::new();
    v.push(s.header(h.clone()));
    let mut s = spec(
        "rejected: empty accepted recipient",
        Some("invalid_request"),
    );
    s.accepted = vec![String::new()];
    v.push(s.header(h.clone()));
    let mut s = spec(
        "rejected: empty accepted-recipient list",
        Some("invalid_request"),
    );
    s.accepted = vec![];
    v.push(s.header(h.clone()));
    let mut s = spec(
        "rejected: a later empty recipient fails even when an earlier one matches",
        Some("invalid_request"),
    );
    s.accepted = vec!["node-b".into(), String::new()];
    v.push(s.header(h.clone()));
    let mut s = spec("rejected: negative skew parameter", Some("invalid_request"));
    s.max_skew = -1;
    v.push(s.header(h.clone()));

    let mut a = auth_of(&h);
    a.peer_id = "p".repeat(MAX_PEER_ID_LEN);
    let long_peer_header = encode_node_request_header(&a);
    let padded = format!(
        "{h}{}",
        " ".repeat(NODE_REQUEST_MAX_HEADER_LEN + 1 - h.len())
    );
    assert_eq!(padded.len(), NODE_REQUEST_MAX_HEADER_LEN + 1);
    let ts = format!("ts={NOW}");
    let sig = hex::encode(auth_of(&h).signature);
    let malformed: Vec<(&str, String)> = vec![
        ("empty header", String::new()),
        ("wrong version", h.replacen("v1;", "v2;", 1)),
        ("uppercase hex key", h.replace(&kh, &kh.to_uppercase())),
        (
            "uppercase hex signature",
            h.replace(&sig, &sig.to_uppercase()),
        ),
        ("short hex key", h.replace(&kh, &kh[2..])),
        (
            "duplicate nonce field",
            h.replacen("; sig=", "; nonce=00; sig=", 1),
        ),
        (
            "missing signature field",
            h[..h.find("; sig=").unwrap()].to_string(),
        ),
        ("fields out of order", h.replacen("; peer=", "; zzz=", 1)),
        ("extra trailing field", format!("{h}; x=1")),
        ("negative timestamp", h.replacen(&ts, "ts=-1", 1)),
        (
            "plus-signed timestamp",
            h.replacen(&ts, &format!("ts=+{NOW}"), 1),
        ),
        (
            "leading zero timestamp",
            h.replacen(&ts, &format!("ts=0{NOW}"), 1),
        ),
        (
            "overflowing timestamp",
            h.replacen(&ts, "ts=9223372036854775808", 1),
        ),
        ("compact separators", h.replace("; ", ";")),
        ("non-ASCII character", h.replace(PEER, "12D3KooW\u{e9}")),
        (
            "peer id with a non-alphanumeric character",
            h.replace(PEER, "peer-id"),
        ),
        ("header one byte over 512", padded),
    ];
    for (name, header) in malformed {
        v.push(
            spec(
                &format!("rejected: malformed header, {name}"),
                Some("malformed"),
            )
            .header(header),
        );
    }
    // A header with the longest peer id parses; its signature is for another peer so it fails there.
    v.push(
        spec(
            "rejected: longest peer id parses but the signature is for another peer",
            Some("bad_signature"),
        )
        .header(long_peer_header),
    );

    json!({
        "$schema": "./SCHEMA.md#node-request",
        "description": "Node-to-node request credential (avalon_protocol::node_request::verify_node_request_header). A node signs one request with its Ed25519 key over avalon-node-request-v1, then u32-BE length-prefixed method, raw wire path (no query, no decoding), sha256 of the body, network id, recipient and signer peer id, then the timestamp as unix seconds big-endian i64, then the 16 nonce bytes. The credential travels in the x-avalon-node-auth header as 'v1; peer=<peer id>; key=<64 lowercase hex>; ts=<decimal unix seconds>; nonce=<32 lowercase hex>; sig=<128 lowercase hex>', parsed strictly: exactly those fields in that order separated by '; ', ASCII, at most 512 bytes, no signed or leading-zero timestamp, peer id [A-Za-z0-9]{1,128}. Signable paths start with '/', use only bytes 0x21-0x7e other than backslash, '?' and '#', and contain no '//' and no '.' or '..' segment; percent-encoding is signed as the raw characters. Method is uppercase ASCII (at most 16), network id and every recipient are non-empty (at most 256), the whole accepted-recipient list is validated before any signature check, and the skew parameter must not be negative. The recipient and network are not in the header: the verifier tries each accepted recipient, so a signature for another recipient or network is bad_signature. Signature verification is strict Ed25519 (small-order keys and S >= L are rejected). Every vector with a non-null expected.error must be rejected with exactly that code; when several apply the first in checkOrder wins. Vectors with signingRecipient carry messageHex, the exact message that header would be verified over for that target; vectors with signedBySeed true are reproduced byte for byte by signing with signingKeySeedHex, peerId and the header's own timestamp and nonce. Replay protection is the receiver's job and is not covered: key a cache on (peer id, nonce), keep entries at least twice the skew, insert only after signature, PeerId-from-key and standing checks pass; the receiver must also derive the libp2p PeerId from the key and compare it to peer. The 512-byte cap is not separately observable for valid fields (the longest valid header is under 512 bytes), so a port may enforce it as an early bound. signerPeerId is a fixed placeholder string, not a real libp2p id.",
        "supportedIn": [],
        "notSupported": {
            "rust": "the Rust SDK has no node-request signing or verification: the credential is for node-to-node routes that are not in the server's OpenAPI document, so no SDK client calls them.",
            "csharp": "the C# SDK has no node-request signing or verification: the credential is for node-to-node routes that are not in the server's OpenAPI document, so no SDK client calls them.",
            "typescript": "the TypeScript SDK has no node-request signing or verification: the credential is for node-to-node routes that are not in the server's OpenAPI document, so no SDK client calls them."
        },
        "checkOrder": ["malformed", "invalid_request", "stale", "future", "invalid_key", "bad_signature"],
        "rejectionCodes": {
            "malformed": "the header text is not in the exact v1 form",
            "invalid_request": "method, path, network id, recipient list, peer id or skew parameter cannot be used",
            "stale": "timestamp older than now minus the skew",
            "future": "timestamp later than now plus the skew",
            "invalid_key": "public key is not a valid Ed25519 point",
            "bad_signature": "strict signature check failed for every accepted recipient"
        },
        "signingKeySeedHex": hex::encode([0x5a; 32]),
        "signingPublicKeyHex": kh,
        "signerPeerId": PEER,
        "vectors": v.iter().map(Spec::json).collect::<Vec<_>>(),
    })
}

/// Re-inserts object keys in sorted order so output does not depend on serde_json's
/// `preserve_order` feature, which other workspace crates may enable.
fn canonical(v: Value) -> Value {
    match v {
        Value::Object(map) => {
            let mut entries: Vec<_> = map.into_iter().collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            Value::Object(
                entries
                    .into_iter()
                    .map(|(k, v)| (k, canonical(v)))
                    .collect(),
            )
        }
        Value::Array(items) => Value::Array(items.into_iter().map(canonical).collect()),
        other => other,
    }
}

#[test]
fn node_request_vectors_are_reproducible() {
    let text = serde_json::to_string_pretty(&canonical(build())).unwrap() + "\n";
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../conformance/vectors/node-request.json");
    if std::env::var_os("AVALON_REGEN_NODE_REQUEST_VECTORS").is_some() {
        std::fs::write(&path, &text).unwrap();
    }
    let on_disk = std::fs::read_to_string(&path).unwrap();
    assert!(on_disk == text, "node-request.json is stale; regenerate it");
}
