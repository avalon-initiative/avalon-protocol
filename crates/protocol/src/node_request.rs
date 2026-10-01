//! Node-to-node request credential: an Ed25519 signature over one request,
//! carried in the [`NODE_REQUEST_HEADER`] header. Pure logic, no I/O.
//!
//! The signature covers method, path, body hash, network, intended recipient,
//! claimed peer id, timestamp and nonce, so a captured header cannot be replayed
//! against another route, body, network or recipient. The header also carries the body hash
//! (`bh`), so a receiver verifies the signature first, from the headers alone, and reads the
//! body only for a valid signature, then requires its SHA-256 to equal `bh`.
//!
//! # What gets signed
//! - `method` is the uppercase HTTP method string (`POST`); any non-HTTP stream
//!   framing of the same request must sign the identical string.
//! - `path` is the raw request-target path exactly as it appears on the wire:
//!   no query, no fragment, no decoding or normalisation (`%2F` stays `%2F`).
//!   Both framings must sign the same string. Only canonical printable paths
//!   can be signed: `/`-prefixed, bytes `0x21..=0x7e` excluding `\`, `?` and `#`,
//!   no `//`, no `.` or `..` segment.
//! - `network_id` and every recipient are non-empty (a node with an unset
//!   network id must not accept credentials signed over an empty one).
//!
//! # Receiver responsibilities
//! This module checks the signature and the clock window only. The receiver must:
//! 0. check the signature and clock window with [`verify_node_request_head`] before reading the
//!    body, and the body with [`verify_node_request_body`] after it (the full
//!    [`verify_node_request`] does both, head first);
//! 1. derive the libp2p PeerId from `public_key` and compare it to `peer_id`
//!    before granting any standing;
//! 2. generate nonces as 16 random bytes from a CSPRNG (the signer's job; this
//!    crate has no RNG dependency);
//! 3. keep a replay cache keyed on `(peer_id, nonce)` regardless of which
//!    recipient matched, retain entries for at least twice the skew (a
//!    timestamp of `now + skew` stays valid until `now + 2 * skew`), and insert
//!    only after the signature, the PeerId check and the standing checks pass.
//!
//! The recipient and network are not carried in the header, so a signature made
//! for another recipient or network is indistinguishable from a forged one and
//! reports [`NodeRequestError::BadSignature`]. The matched recipient is not
//! returned, and the public key is not part of the signed bytes.

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use sha2::{Digest, Sha256};

/// HTTP header carrying the credential.
pub const NODE_REQUEST_HEADER: &str = "x-avalon-node-auth";

/// Domain tag prepended to the signed bytes.
pub const NODE_REQUEST_DOMAIN: &[u8] = b"avalon-node-request-v1";

/// Longest header value the parser will look at.
pub const NODE_REQUEST_MAX_HEADER_LEN: usize = 512;

/// Bounds on the signed text fields; they keep every length prefix well inside u32.
pub const MAX_PEER_ID_LEN: usize = 128;
pub const MAX_METHOD_LEN: usize = 16;
pub const MAX_PATH_LEN: usize = 2048;
pub const MAX_NAME_LEN: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum NodeRequestError {
    /// The header text is not in the exact v1 form.
    #[error("malformed node auth header: {0}")]
    Malformed(&'static str),
    /// A method, path, network, recipient, peer id or skew that cannot be used.
    #[error("invalid request field: {0}")]
    InvalidRequest(&'static str),
    /// The public key is not a valid Ed25519 key.
    #[error("invalid public key")]
    InvalidKey,
    /// The timestamp is older than the allowed window.
    #[error("timestamp too old")]
    Stale,
    /// The timestamp is further in the future than the allowed window.
    #[error("timestamp in the future")]
    Future,
    /// The signature does not verify (also covers a wrong recipient or network).
    #[error("bad signature")]
    BadSignature,
    /// The signature is valid but the body does not hash to the signed `bh`.
    #[error("body does not match the signed hash")]
    BodyMismatch,
}

impl NodeRequestError {
    /// Stable snake_case reason for logs and conformance vectors.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Malformed(_) => "malformed",
            Self::InvalidRequest(_) => "invalid_request",
            Self::InvalidKey => "invalid_key",
            Self::Stale => "stale",
            Self::Future => "future",
            Self::BadSignature => "bad_signature",
            Self::BodyMismatch => "body_hash",
        }
    }
}

/// The request parts a signature binds, minus the recipient and signer fields.
#[derive(Debug, Clone, Copy)]
pub struct NodeRequestTarget<'a> {
    /// Uppercase ASCII HTTP method.
    pub method: &'a str,
    /// Raw wire path, see the module docs.
    pub path: &'a str,
    pub body: &'a [u8],
    /// Non-empty.
    pub network_id: &'a str,
}

/// [`NodeRequestTarget`] without the body: what a receiver knows before reading it.
#[derive(Debug, Clone, Copy)]
pub struct NodeRequestHead<'a> {
    pub method: &'a str,
    pub path: &'a str,
    pub network_id: &'a str,
}

impl<'a> From<&NodeRequestTarget<'a>> for NodeRequestHead<'a> {
    fn from(t: &NodeRequestTarget<'a>) -> Self {
        Self {
            method: t.method,
            path: t.path,
            network_id: t.network_id,
        }
    }
}

/// A parsed or freshly signed credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeRequestAuth {
    /// Claimed peer id string, `[A-Za-z0-9]{1,128}`; not checked against the key here.
    pub peer_id: String,
    pub public_key: [u8; 32],
    /// Unix seconds.
    pub timestamp: i64,
    pub nonce: [u8; 16],
    /// SHA-256 of the body the signature covers.
    pub body_hash: [u8; 32],
    pub signature: [u8; 64],
}

fn push_lp(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    out.extend_from_slice(bytes);
}

fn valid_peer_id(peer_id: &str) -> bool {
    !peer_id.is_empty()
        && peer_id.len() <= MAX_PEER_ID_LEN
        && peer_id.bytes().all(|b| b.is_ascii_alphanumeric())
}

fn validate_name(value: &str, what: &'static str) -> Result<(), NodeRequestError> {
    if value.is_empty() || value.len() > MAX_NAME_LEN {
        return Err(NodeRequestError::InvalidRequest(what));
    }
    Ok(())
}

fn validate_path(path: &str) -> Result<(), NodeRequestError> {
    let printable = path
        .bytes()
        .all(|b| (0x21..=0x7e).contains(&b) && b != b'\\');
    let query_free = !path.bytes().any(|b| b == b'?' || b == b'#');
    let canonical = !path.contains("//") && !path.split('/').any(|seg| seg == "." || seg == "..");
    if path.len() > MAX_PATH_LEN
        || !path.starts_with('/')
        || !printable
        || !query_free
        || !canonical
    {
        return Err(NodeRequestError::InvalidRequest("path"));
    }
    Ok(())
}

fn validate_target(target: &NodeRequestTarget<'_>, peer_id: &str) -> Result<(), NodeRequestError> {
    validate_head(&target.into(), peer_id)
}

fn validate_head(target: &NodeRequestHead<'_>, peer_id: &str) -> Result<(), NodeRequestError> {
    let method = target.method;
    let method_ok = !method.is_empty()
        && method.len() <= MAX_METHOD_LEN
        && method.bytes().all(|b| b.is_ascii_uppercase());
    if !method_ok {
        return Err(NodeRequestError::InvalidRequest("method"));
    }
    validate_path(target.path)?;
    validate_name(target.network_id, "network_id")?;
    if !valid_peer_id(peer_id) {
        return Err(NodeRequestError::InvalidRequest("peer_id"));
    }
    Ok(())
}

/// Everything up to (excluding) the recipient; hashed once for all recipients.
fn message_prefix(target: &NodeRequestHead<'_>, body_hash: &[u8; 32]) -> Vec<u8> {
    let mut message = Vec::new();
    message.extend_from_slice(NODE_REQUEST_DOMAIN);
    push_lp(&mut message, target.method.as_bytes());
    push_lp(&mut message, target.path.as_bytes());
    push_lp(&mut message, body_hash);
    push_lp(&mut message, target.network_id.as_bytes());
    message
}

fn message_with_recipient(
    prefix: &[u8],
    recipient: &str,
    peer_id: &str,
    timestamp: i64,
    nonce: &[u8; 16],
) -> Vec<u8> {
    let mut message = prefix.to_vec();
    push_lp(&mut message, recipient.as_bytes());
    push_lp(&mut message, peer_id.as_bytes());
    message.extend_from_slice(&timestamp.to_be_bytes());
    message.extend_from_slice(nonce);
    message
}

/// The exact bytes a node request signature covers.
pub fn node_request_signing_message(
    target: &NodeRequestTarget<'_>,
    recipient: &str,
    peer_id: &str,
    timestamp: i64,
    nonce: &[u8; 16],
) -> Result<Vec<u8>, NodeRequestError> {
    validate_target(target, peer_id)?;
    validate_name(recipient, "recipient")?;
    let prefix = message_prefix(&target.into(), &Sha256::digest(target.body).into());
    Ok(message_with_recipient(
        &prefix, recipient, peer_id, timestamp, nonce,
    ))
}

/// Signs a request for `recipient` as `peer_id`; the caller supplies a fresh random nonce.
pub fn sign_node_request(
    signing_key: &SigningKey,
    peer_id: &str,
    target: &NodeRequestTarget<'_>,
    recipient: &str,
    timestamp: i64,
    nonce: [u8; 16],
) -> Result<NodeRequestAuth, NodeRequestError> {
    let message = node_request_signing_message(target, recipient, peer_id, timestamp, &nonce)?;
    let signature: Signature = signing_key.sign(&message);
    Ok(NodeRequestAuth {
        peer_id: peer_id.to_string(),
        public_key: signing_key.verifying_key().to_bytes(),
        timestamp,
        nonce,
        body_hash: Sha256::digest(target.body).into(),
        signature: signature.to_bytes(),
    })
}

/// Encodes `v1; peer=<id>; key=<hex>; ts=<secs>; nonce=<hex>; bh=<hex>; sig=<hex>` (lowercase hex).
pub fn encode_node_request_header(auth: &NodeRequestAuth) -> String {
    format!(
        "v1; peer={}; key={}; ts={}; nonce={}; bh={}; sig={}",
        auth.peer_id,
        hex::encode(auth.public_key),
        auth.timestamp,
        hex::encode(auth.nonce),
        hex::encode(auth.body_hash),
        hex::encode(auth.signature)
    )
}

fn lower_hex<const N: usize>(value: &str, what: &'static str) -> Result<[u8; N], NodeRequestError> {
    if value.len() != N * 2
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(NodeRequestError::Malformed(what));
    }
    let mut out = [0u8; N];
    hex::decode_to_slice(value, &mut out).map_err(|_| NodeRequestError::Malformed(what))?;
    Ok(out)
}

/// Parses the header strictly: exactly the seven fields in the order above, joined by `"; "`,
/// ASCII only, at most [`NODE_REQUEST_MAX_HEADER_LEN`] bytes, lowercase hex, a plain
/// non-negative decimal timestamp without leading zeros, and no extra or repeated fields.
pub fn parse_node_request_header(header: &str) -> Result<NodeRequestAuth, NodeRequestError> {
    if header.len() > NODE_REQUEST_MAX_HEADER_LEN {
        return Err(NodeRequestError::Malformed("too long"));
    }
    if !header.is_ascii() {
        return Err(NodeRequestError::Malformed("non-ascii"));
    }
    let mut parts = header.split("; ");
    let mut next = |prefix: &'static str| -> Result<&str, NodeRequestError> {
        parts
            .next()
            .and_then(|p| p.strip_prefix(prefix))
            .ok_or(NodeRequestError::Malformed("field order"))
    };
    if next("v")? != "1" {
        return Err(NodeRequestError::Malformed("version"));
    }
    let peer_id = next("peer=")?;
    let key = next("key=")?;
    let ts = next("ts=")?;
    let nonce = next("nonce=")?;
    let bh = next("bh=")?;
    let sig = next("sig=")?;
    if parts.next().is_some() {
        return Err(NodeRequestError::Malformed("extra field"));
    }
    if !valid_peer_id(peer_id) {
        return Err(NodeRequestError::Malformed("peer"));
    }
    let digits_only = !ts.is_empty() && ts.bytes().all(|b| b.is_ascii_digit());
    if !digits_only || (ts.len() > 1 && ts.starts_with('0')) {
        return Err(NodeRequestError::Malformed("ts"));
    }
    Ok(NodeRequestAuth {
        peer_id: peer_id.to_string(),
        public_key: lower_hex::<32>(key, "key")?,
        timestamp: ts.parse().map_err(|_| NodeRequestError::Malformed("ts"))?,
        nonce: lower_hex::<16>(nonce, "nonce")?,
        body_hash: lower_hex::<32>(bh, "bh")?,
        signature: lower_hex::<64>(sig, "sig")?,
    })
}

/// Verifies everything a receiver can check before reading the body: request fields, clock
/// window, key, then the signature (over the header's own body hash) against each accepted
/// recipient. Follow with [`verify_node_request_body`] once the body is read; the receiver
/// still owes the peer id/key check and the nonce replay check (see the module docs).
pub fn verify_node_request_head(
    auth: &NodeRequestAuth,
    head: &NodeRequestHead<'_>,
    accepted_recipients: &[&str],
    now: i64,
    max_skew_secs: i64,
) -> Result<(), NodeRequestError> {
    validate_head(head, &auth.peer_id)?;
    if accepted_recipients.is_empty() {
        return Err(NodeRequestError::InvalidRequest("recipients"));
    }
    for recipient in accepted_recipients {
        validate_name(recipient, "recipient")?;
    }
    if max_skew_secs < 0 {
        return Err(NodeRequestError::InvalidRequest("max_skew"));
    }
    if auth.timestamp < now.saturating_sub(max_skew_secs) {
        return Err(NodeRequestError::Stale);
    }
    if auth.timestamp > now.saturating_add(max_skew_secs) {
        return Err(NodeRequestError::Future);
    }
    let key =
        VerifyingKey::from_bytes(&auth.public_key).map_err(|_| NodeRequestError::InvalidKey)?;
    let signature = Signature::from_bytes(&auth.signature);
    let prefix = message_prefix(head, &auth.body_hash);
    let verified = accepted_recipients.iter().any(|recipient| {
        let message = message_with_recipient(
            &prefix,
            recipient,
            &auth.peer_id,
            auth.timestamp,
            &auth.nonce,
        );
        key.verify_strict(&message, &signature).is_ok()
    });
    if verified {
        Ok(())
    } else {
        Err(NodeRequestError::BadSignature)
    }
}

/// Requires `body` to hash to the credential's signed body hash.
pub fn verify_node_request_body(
    auth: &NodeRequestAuth,
    body: &[u8],
) -> Result<(), NodeRequestError> {
    if Sha256::digest(body).as_slice() == auth.body_hash {
        Ok(())
    } else {
        Err(NodeRequestError::BodyMismatch)
    }
}

/// [`verify_node_request_head`] then [`verify_node_request_body`].
pub fn verify_node_request(
    auth: &NodeRequestAuth,
    target: &NodeRequestTarget<'_>,
    accepted_recipients: &[&str],
    now: i64,
    max_skew_secs: i64,
) -> Result<(), NodeRequestError> {
    verify_node_request_head(
        auth,
        &target.into(),
        accepted_recipients,
        now,
        max_skew_secs,
    )?;
    verify_node_request_body(auth, target.body)
}

/// Parses `header` then [`verify_node_request_head`]; the body is still to be checked.
pub fn verify_node_request_header_head(
    header: &str,
    head: &NodeRequestHead<'_>,
    accepted_recipients: &[&str],
    now: i64,
    max_skew_secs: i64,
) -> Result<NodeRequestAuth, NodeRequestError> {
    let auth = parse_node_request_header(header)?;
    verify_node_request_head(&auth, head, accepted_recipients, now, max_skew_secs)?;
    Ok(auth)
}

/// Parses `header` then [`verify_node_request`]; returns the credential for the caller's own checks.
pub fn verify_node_request_header(
    header: &str,
    target: &NodeRequestTarget<'_>,
    accepted_recipients: &[&str],
    now: i64,
    max_skew_secs: i64,
) -> Result<NodeRequestAuth, NodeRequestError> {
    let auth = parse_node_request_header(header)?;
    verify_node_request(&auth, target, accepted_recipients, now, max_skew_secs)?;
    Ok(auth)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::Verifier;

    const NOW: i64 = 1_790_000_000;
    const SKEW: i64 = 60;
    const PEER: &str = "12D3KooWExampleSignerPeerId";
    const NONCE: [u8; 16] = [7; 16];

    fn key() -> SigningKey {
        SigningKey::from_bytes(&[0x42; 32])
    }

    fn target() -> NodeRequestTarget<'static> {
        NodeRequestTarget {
            method: "POST",
            path: "/internal/v1/things",
            body: b"{\"a\":1}",
            network_id: "avalon-test",
        }
    }

    fn signed() -> NodeRequestAuth {
        sign_node_request(&key(), PEER, &target(), "node-b", NOW, NONCE).unwrap()
    }

    fn verify(auth: &NodeRequestAuth, t: &NodeRequestTarget<'_>) -> Result<(), NodeRequestError> {
        verify_node_request(auth, t, &["node-b"], NOW, SKEW)
    }

    #[test]
    fn round_trip_through_the_header() {
        let auth = signed();
        let header = encode_node_request_header(&auth);
        assert!(header.starts_with("v1; peer=12D3KooWExampleSignerPeerId; key="));
        assert_eq!(parse_node_request_header(&header), Ok(auth.clone()));
        let out = verify_node_request_header(&header, &target(), &["node-a", "node-b"], NOW, SKEW);
        assert_eq!(out, Ok(auth));
    }

    #[test]
    fn signing_is_deterministic() {
        assert_eq!(signed(), signed());
    }

    #[test]
    fn tampering_with_every_signed_field_fails() {
        let auth = signed();
        let t = target();
        let bad =
            |r: Result<(), NodeRequestError>| assert_eq!(r, Err(NodeRequestError::BadSignature));

        bad(verify(&auth, &NodeRequestTarget { method: "PUT", ..t }));
        bad(verify(
            &auth,
            &NodeRequestTarget {
                path: "/internal/v1/other",
                ..t
            },
        ));
        // The body is checked against the signed hash, after the signature.
        let mismatch =
            |r: Result<(), NodeRequestError>| assert_eq!(r, Err(NodeRequestError::BodyMismatch));
        mismatch(verify(
            &auth,
            &NodeRequestTarget {
                body: b"{\"a\":2}",
                ..t
            },
        ));
        mismatch(verify(&auth, &NodeRequestTarget { body: b"", ..t }));
        bad(verify(
            &auth,
            &NodeRequestTarget {
                network_id: "other-net",
                ..t
            },
        ));
        bad(verify_node_request(&auth, &t, &["node-c"], NOW, SKEW));

        let mut a = auth.clone();
        a.peer_id.push('x');
        bad(verify(&a, &t));
        let mut a = auth.clone();
        a.timestamp += 1;
        bad(verify(&a, &t));
        let mut a = auth.clone();
        a.nonce[0] ^= 1;
        bad(verify(&a, &t));
        let mut a = auth.clone();
        a.body_hash[0] ^= 1;
        bad(verify(&a, &t));
        let mut a = auth.clone();
        a.public_key = SigningKey::from_bytes(&[0x43; 32])
            .verifying_key()
            .to_bytes();
        bad(verify(&a, &t));
        let mut a = auth.clone();
        a.signature[10] ^= 1;
        bad(verify(&a, &t));
    }

    #[test]
    fn length_prefixes_keep_adjacent_fields_distinct() {
        let t = target();
        let msg = |net: &'static str, rcpt: &str, peer: &str| {
            let t = NodeRequestTarget {
                network_id: net,
                ..t
            };
            node_request_signing_message(&t, rcpt, peer, NOW, &NONCE).unwrap()
        };
        // Each pair concatenates to the same bytes without length prefixes.
        assert_ne!(msg("ab", "c", PEER), msg("a", "bc", PEER));
        assert_ne!(msg("net", "node-b", "xy"), msg("net", "node-bx", "y"));
    }

    #[test]
    fn any_accepted_recipient_matches() {
        let auth = signed();
        let r = verify_node_request(&auth, &target(), &["node-a", "node-b", "node-c"], NOW, SKEW);
        assert_eq!(r, Ok(()));
    }

    #[test]
    fn skew_window_is_inclusive_in_both_directions() {
        let at = |ts: i64| sign_node_request(&key(), PEER, &target(), "node-b", ts, NONCE).unwrap();
        assert_eq!(verify(&at(NOW - SKEW), &target()), Ok(()));
        assert_eq!(verify(&at(NOW + SKEW), &target()), Ok(()));
        assert_eq!(
            verify(&at(NOW - SKEW - 1), &target()),
            Err(NodeRequestError::Stale)
        );
        assert_eq!(
            verify(&at(NOW + SKEW + 1), &target()),
            Err(NodeRequestError::Future)
        );
        assert_eq!(verify(&at(0), &target()), Err(NodeRequestError::Stale));
        assert_eq!(
            verify(&at(i64::MAX), &target()),
            Err(NodeRequestError::Future)
        );
    }

    #[test]
    fn skew_is_checked_before_the_signature() {
        let mut a = signed();
        a.timestamp -= 1000;
        assert_eq!(verify(&a, &target()), Err(NodeRequestError::Stale));
        a.timestamp += 2000;
        assert_eq!(verify(&a, &target()), Err(NodeRequestError::Future));
    }

    #[test]
    fn extreme_now_does_not_overflow_and_negative_skew_is_an_error() {
        let a = signed();
        assert_eq!(
            verify_node_request(&a, &target(), &["node-b"], i64::MIN, SKEW),
            Err(NodeRequestError::Future)
        );
        assert_eq!(
            verify_node_request(&a, &target(), &["node-b"], i64::MAX, SKEW),
            Err(NodeRequestError::Stale)
        );
        assert_eq!(
            verify_node_request(&a, &target(), &["node-b"], NOW, -5),
            Err(NodeRequestError::InvalidRequest("max_skew"))
        );
        assert_eq!(
            verify_node_request(&a, &target(), &["node-b"], NOW, 0),
            Ok(())
        );
        let r = verify_node_request(&a, &target(), &["node-b"], 0, i64::MAX);
        assert_eq!(r, Ok(()));
    }

    #[test]
    fn invalid_key_is_reported_distinctly() {
        // y = 2 is not on the curve, so decompression fails.
        let mut a = signed();
        a.public_key = [0; 32];
        a.public_key[0] = 2;
        assert_eq!(verify(&a, &target()), Err(NodeRequestError::InvalidKey));
    }

    #[test]
    fn small_order_key_is_rejected_by_strict_verify() {
        // A and R are the identity point and S = 0: plain verification accepts for any message.
        let mut a = signed();
        a.public_key = [0; 32];
        a.public_key[0] = 1;
        a.signature = [0; 64];
        a.signature[0] = 1;
        let message =
            node_request_signing_message(&target(), "node-b", PEER, a.timestamp, &a.nonce).unwrap();
        let weak = VerifyingKey::from_bytes(&a.public_key).unwrap();
        assert!(weak
            .verify(&message, &Signature::from_bytes(&a.signature))
            .is_ok());
        assert_eq!(verify(&a, &target()), Err(NodeRequestError::BadSignature));
    }

    #[test]
    fn non_canonical_s_is_rejected() {
        // S + L encodes the same scalar but is not canonical (S >= L).
        const L: [u8; 32] = [
            0xed, 0xd3, 0xf5, 0x5c, 0x1a, 0x63, 0x12, 0x58, 0xd6, 0x9c, 0xf7, 0xa2, 0xde, 0xf9,
            0xde, 0x14, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x10,
        ];
        let mut a = signed();
        let mut carry = 0u16;
        for (byte, l) in a.signature[32..].iter_mut().zip(L) {
            let sum = *byte as u16 + l as u16 + carry;
            *byte = sum as u8;
            carry = sum >> 8;
        }
        assert_eq!(carry, 0);
        assert_eq!(verify(&a, &target()), Err(NodeRequestError::BadSignature));
    }

    #[test]
    fn invalid_peer_id_in_a_hand_built_auth_is_rejected() {
        for peer in [
            "",
            "has space",
            "semi;colon",
            "\u{e9}",
            &"p".repeat(MAX_PEER_ID_LEN + 1),
        ] {
            let mut a = signed();
            a.peer_id = peer.to_string();
            let r = verify(&a, &target());
            assert_eq!(
                r,
                Err(NodeRequestError::InvalidRequest("peer_id")),
                "{peer}"
            );
        }
    }

    #[test]
    fn recipient_list_is_validated_whole_and_not_empty() {
        let auth = signed();
        let t = target();
        let long = "n".repeat(MAX_NAME_LEN + 1);
        let invalid = Err(NodeRequestError::InvalidRequest("recipient"));
        // The matching recipient comes first; a bad later entry still fails the call.
        assert_eq!(
            verify_node_request(&auth, &t, &["node-b", ""], NOW, SKEW),
            invalid
        );
        assert_eq!(
            verify_node_request(&auth, &t, &["node-b", &long], NOW, SKEW),
            invalid
        );
        assert_eq!(
            verify_node_request(&auth, &t, &["", "node-b"], NOW, SKEW),
            invalid
        );
        assert_eq!(verify_node_request(&auth, &t, &[""], NOW, SKEW), invalid);
        assert_eq!(
            verify_node_request(&auth, &t, &[], NOW, SKEW),
            Err(NodeRequestError::InvalidRequest("recipients"))
        );
    }

    #[test]
    fn empty_recipient_and_network_cannot_be_signed_or_verified() {
        let t = target();
        let no_net = NodeRequestTarget {
            network_id: "",
            ..t
        };
        let r = sign_node_request(&key(), PEER, &t, "", NOW, NONCE);
        assert_eq!(r, Err(NodeRequestError::InvalidRequest("recipient")));
        let r = sign_node_request(&key(), PEER, &no_net, "node-b", NOW, NONCE);
        assert_eq!(r, Err(NodeRequestError::InvalidRequest("network_id")));
        let r = verify_node_request(&signed(), &no_net, &["node-b"], NOW, SKEW);
        assert_eq!(r, Err(NodeRequestError::InvalidRequest("network_id")));
    }

    #[test]
    fn percent_encoding_is_signed_raw_not_decoded() {
        let t = NodeRequestTarget {
            path: "/a%2Fb/%2e%2e",
            ..target()
        };
        let a = sign_node_request(&key(), PEER, &t, "node-b", NOW, NONCE).unwrap();
        assert_eq!(verify(&a, &t), Ok(()));
        let decoded = NodeRequestTarget { path: "/a/b", ..t };
        assert_eq!(verify(&a, &decoded), Err(NodeRequestError::BadSignature));
    }

    #[test]
    fn exact_maximum_lengths_are_accepted() {
        let path = format!("/{}", "a".repeat(MAX_PATH_LEN - 1));
        let method = "M".repeat(MAX_METHOD_LEN);
        let name = "n".repeat(MAX_NAME_LEN);
        let peer = "p".repeat(MAX_PEER_ID_LEN);
        let t = NodeRequestTarget {
            method: &method,
            path: &path,
            body: b"",
            network_id: &name,
        };
        let a = sign_node_request(&key(), &peer, &t, &name, NOW, NONCE).unwrap();
        assert_eq!(verify_node_request(&a, &t, &[&name], NOW, SKEW), Ok(()));
        let header = encode_node_request_header(&a);
        assert!(header.len() <= NODE_REQUEST_MAX_HEADER_LEN);
        assert_eq!(parse_node_request_header(&header), Ok(a));
        // One over each bound is refused.
        let over = |t: NodeRequestTarget<'_>, peer: &str, rcpt: &str| {
            sign_node_request(&key(), peer, &t, rcpt, NOW, NONCE).is_err()
        };
        let long_path = format!("{path}a");
        let long_method = format!("{method}M");
        let long_name = format!("{name}n");
        let long_peer = format!("{peer}p");
        assert!(over(
            NodeRequestTarget {
                path: &long_path,
                ..t
            },
            &peer,
            &name
        ));
        assert!(over(
            NodeRequestTarget {
                method: &long_method,
                ..t
            },
            &peer,
            &name
        ));
        assert!(over(
            NodeRequestTarget {
                network_id: &long_name,
                ..t
            },
            &peer,
            &name
        ));
        assert!(over(t, &peer, &long_name));
        assert!(over(t, &long_peer, &name));
    }

    #[test]
    fn unsignable_targets_are_rejected_on_both_sides() {
        let t = target();
        let long = "a".repeat(MAX_PATH_LEN + 1);
        let long_name = "n".repeat(MAX_NAME_LEN + 1);
        let bad_targets = [
            NodeRequestTarget {
                path: "/a?x=1",
                ..t
            },
            NodeRequestTarget {
                path: "/a#frag",
                ..t
            },
            NodeRequestTarget { path: "a/b", ..t },
            NodeRequestTarget { path: "", ..t },
            NodeRequestTarget { path: "/a//b", ..t },
            NodeRequestTarget { path: "//a", ..t },
            NodeRequestTarget {
                path: "/a/./b",
                ..t
            },
            NodeRequestTarget { path: "/a/..", ..t },
            NodeRequestTarget { path: "/..", ..t },
            NodeRequestTarget { path: "/.", ..t },
            NodeRequestTarget { path: "/a b", ..t },
            NodeRequestTarget { path: "/a\tb", ..t },
            NodeRequestTarget { path: "/a\nb", ..t },
            NodeRequestTarget { path: "/a\0b", ..t },
            NodeRequestTarget {
                path: "/a\u{7f}b",
                ..t
            },
            NodeRequestTarget { path: "/a\\b", ..t },
            NodeRequestTarget {
                path: "/\u{e9}",
                ..t
            },
            NodeRequestTarget { path: &long, ..t },
            NodeRequestTarget {
                method: "post",
                ..t
            },
            NodeRequestTarget { method: "", ..t },
            NodeRequestTarget {
                method: "POSTPOSTPOSTPOSTP",
                ..t
            },
            NodeRequestTarget {
                network_id: &long_name,
                ..t
            },
        ];
        let good = signed();
        for bad in bad_targets {
            let signed = sign_node_request(&key(), PEER, &bad, "node-b", NOW, NONCE);
            assert!(
                matches!(signed, Err(NodeRequestError::InvalidRequest(_))),
                "{bad:?}"
            );
            assert!(
                matches!(
                    verify(&good, &bad),
                    Err(NodeRequestError::InvalidRequest(_))
                ),
                "{bad:?}"
            );
        }
        for peer in [
            "",
            "has space",
            "semi;colon",
            "ünï",
            &"p".repeat(MAX_PEER_ID_LEN + 1),
        ] {
            let r = sign_node_request(&key(), peer, &t, "node-b", NOW, NONCE);
            assert!(
                matches!(r, Err(NodeRequestError::InvalidRequest(_))),
                "{peer}"
            );
        }
        let r = sign_node_request(&key(), PEER, &t, &long_name, NOW, NONCE);
        assert!(matches!(r, Err(NodeRequestError::InvalidRequest(_))));
        let q = NodeRequestTarget {
            path: "/a?x=1",
            ..t
        };
        let r = verify_node_request(&good, &q, &[], NOW, SKEW);
        assert!(matches!(r, Err(NodeRequestError::InvalidRequest(_))));
        let r = verify_node_request(&good, &t, &[&long_name], NOW, SKEW);
        assert!(matches!(r, Err(NodeRequestError::InvalidRequest(_))));
    }

    #[test]
    fn signing_message_layout_is_fixed() {
        let t = NodeRequestTarget {
            method: "GET",
            path: "/p",
            body: b"",
            network_id: "n",
        };
        let m = node_request_signing_message(&t, "r", "peer", 1, &[9; 16]).unwrap();
        let mut want = b"avalon-node-request-v1".to_vec();
        for f in [
            &b"GET"[..],
            b"/p",
            &Sha256::digest(b"")[..],
            b"n",
            b"r",
            b"peer",
        ] {
            want.extend_from_slice(&(f.len() as u32).to_be_bytes());
            want.extend_from_slice(f);
        }
        want.extend_from_slice(&1i64.to_be_bytes());
        want.extend_from_slice(&[9; 16]);
        assert_eq!(m, want);
    }

    fn good_header() -> String {
        encode_node_request_header(&signed())
    }

    #[test]
    fn malformed_headers_are_rejected() {
        let good = good_header();
        let key_hex = hex::encode(signed().public_key);
        let sig_hex = hex::encode(signed().signature);
        let nonce_hex = hex::encode(NONCE);
        let fields = [
            format!("peer={PEER}"),
            format!("key={key_hex}"),
            format!("ts={NOW}"),
            format!("nonce={nonce_hex}"),
            format!("bh={}", hex::encode(signed().body_hash)),
            format!("sig={sig_hex}"),
        ];
        let build = |v: &str, f: &[String]| format!("{v}; {}", f.join("; "));
        assert_eq!(build("v1", &fields), good);

        let mut cases: Vec<(String, String)> = vec![
            ("empty".into(), String::new()),
            ("wrong version".into(), build("v2", &fields)),
            ("uppercase version".into(), build("V1", &fields)),
            ("bare v".into(), build("v", &fields)),
            ("trailing space".into(), format!("{good} ")),
            ("leading space".into(), format!(" {good}")),
            ("trailing separator".into(), format!("{good};")),
            ("extra field".into(), format!("{good}; extra=1")),
            ("compact separators".into(), good.replace("; ", ";")),
            ("double space".into(), good.replace("; ", ";  ")),
            ("tab separator".into(), good.replacen("; ", ";\t", 1)),
            ("non-ascii".into(), good.replace(PEER, "12D3KooWüüü")),
            (
                "oversized".into(),
                format!("{good}; pad={}", "a".repeat(NODE_REQUEST_MAX_HEADER_LEN)),
            ),
            (
                "uppercase key hex".into(),
                good.replace(&key_hex, &key_hex.to_uppercase()),
            ),
            (
                "uppercase sig hex".into(),
                good.replace(&sig_hex, &sig_hex.to_uppercase()),
            ),
            (
                "non-hex nonce".into(),
                good.replace(&nonce_hex, &"g".repeat(32)),
            ),
            (
                "0x prefix key".into(),
                good.replace(&key_hex, &format!("0x{}", &key_hex[2..])),
            ),
            ("short key".into(), good.replace(&key_hex, &key_hex[2..])),
            (
                "long key".into(),
                good.replace(&key_hex, &format!("{key_hex}00")),
            ),
            (
                "short nonce".into(),
                good.replace(&nonce_hex, &nonce_hex[2..]),
            ),
            (
                "long sig".into(),
                good.replace(&sig_hex, &format!("{sig_hex}00")),
            ),
            ("short sig".into(), good.replace(&sig_hex, &sig_hex[2..])),
            ("empty peer".into(), good.replace(PEER, "")),
            ("peer with bad char".into(), good.replace(PEER, "peer-id")),
            (
                "long peer".into(),
                good.replace(PEER, &"p".repeat(MAX_PEER_ID_LEN + 1)),
            ),
            ("empty ts".into(), good.replace(&format!("ts={NOW}"), "ts=")),
            (
                "negative ts".into(),
                good.replace(&format!("ts={NOW}"), "ts=-5"),
            ),
            (
                "plus ts".into(),
                good.replace(&format!("ts={NOW}"), "ts=+5"),
            ),
            (
                "leading zero ts".into(),
                good.replace(&format!("ts={NOW}"), "ts=0123"),
            ),
            (
                "float ts".into(),
                good.replace(&format!("ts={NOW}"), "ts=1.5"),
            ),
            (
                "overflow ts".into(),
                good.replace(&format!("ts={NOW}"), "ts=9223372036854775808"),
            ),
        ];
        // Missing, duplicated, reordered and renamed fields.
        for i in 0..fields.len() {
            let mut f = fields.to_vec();
            f.remove(i);
            cases.push((format!("missing field {i}"), build("v1", &f)));
            let mut f = fields.to_vec();
            f.insert(i, fields[i].clone());
            cases.push((format!("duplicate field {i}"), build("v1", &f)));
            let mut f = fields.to_vec();
            f[i] = f[i].replacen('=', "x=", 1);
            cases.push((format!("renamed field {i}"), build("v1", &f)));
        }
        let mut f = fields.to_vec();
        f.swap(0, 1);
        cases.push(("swapped order".into(), build("v1", &f)));

        for (name, header) in cases {
            let parsed = parse_node_request_header(&header);
            assert!(
                matches!(parsed, Err(NodeRequestError::Malformed(_))),
                "{name}: {parsed:?}"
            );
            let verified = verify_node_request_header(&header, &target(), &["node-b"], NOW, SKEW);
            assert!(
                matches!(verified, Err(NodeRequestError::Malformed(_))),
                "{name}"
            );
        }
        assert!(parse_node_request_header(&good).is_ok());
    }

    #[test]
    fn size_and_charset_guards_fire_first() {
        let good = good_header();
        let big = format!("{good}{}", " ".repeat(NODE_REQUEST_MAX_HEADER_LEN));
        let err = parse_node_request_header(&big).unwrap_err();
        assert_eq!(err, NodeRequestError::Malformed("too long"));
        let err = parse_node_request_header(&good.replace(PEER, "peer\u{e9}")).unwrap_err();
        assert_eq!(err, NodeRequestError::Malformed("non-ascii"));
    }

    #[test]
    fn ts_zero_and_max_parse() {
        let h = good_header().replace(&format!("ts={NOW}"), "ts=0");
        assert_eq!(parse_node_request_header(&h).unwrap().timestamp, 0);
        let h = good_header().replace(&format!("ts={NOW}"), "ts=9223372036854775807");
        assert_eq!(parse_node_request_header(&h).unwrap().timestamp, i64::MAX);
    }

    #[test]
    fn parser_never_panics_on_arbitrary_text() {
        let good = good_header();
        for end in 0..=good.len() {
            let _ = parse_node_request_header(&good[..end]);
        }
        for h in [
            "\u{0}",
            "v1; ",
            "; ; ; ; ; ",
            "v1; peer=; key=; ts=; nonce=; sig=",
            "\u{1F600}",
        ] {
            assert!(parse_node_request_header(h).is_err());
        }
    }

    #[test]
    fn error_codes_are_distinct() {
        let all = [
            NodeRequestError::Malformed("x"),
            NodeRequestError::InvalidRequest("x"),
            NodeRequestError::InvalidKey,
            NodeRequestError::Stale,
            NodeRequestError::Future,
            NodeRequestError::BadSignature,
        ];
        let mut codes: Vec<_> = all.iter().map(|e| e.code()).collect();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), all.len());
    }

    #[test]
    fn the_head_verifies_without_the_body_and_the_body_is_checked_after() {
        let auth = signed();
        let t = target();
        let head = NodeRequestHead::from(&t);
        assert_eq!(
            verify_node_request_head(&auth, &head, &["node-b"], NOW, SKEW),
            Ok(())
        );
        assert_eq!(verify_node_request_body(&auth, t.body), Ok(()));
        assert_eq!(
            verify_node_request_body(&auth, b"other"),
            Err(NodeRequestError::BodyMismatch)
        );
        // A bad signature is reported before the body is even considered.
        let mut forged = auth.clone();
        forged.signature[0] ^= 1;
        let wrong = NodeRequestTarget {
            body: b"other",
            ..t
        };
        assert_eq!(verify(&forged, &wrong), Err(NodeRequestError::BadSignature));
        assert_eq!(
            verify_node_request_head(&forged, &head, &["node-b"], NOW, SKEW),
            Err(NodeRequestError::BadSignature)
        );
        // A valid signature over another body hash does not vouch for this body.
        let other = NodeRequestTarget {
            body: b"other",
            ..t
        };
        let resigned = sign_node_request(&key(), PEER, &other, "node-b", NOW, NONCE).unwrap();
        assert_eq!(verify(&resigned, &t), Err(NodeRequestError::BodyMismatch));
        let header = encode_node_request_header(&auth);
        assert_eq!(
            verify_node_request_header_head(&header, &head, &["node-b"], NOW, SKEW),
            Ok(auth)
        );
    }

    #[test]
    fn the_body_hash_field_is_required_and_strict() {
        let good = good_header();
        let bh = hex::encode(signed().body_hash);
        for (name, header) in [
            ("missing", good.replace(&format!("; bh={bh}"), "")),
            ("uppercase", good.replace(&bh, &bh.to_uppercase())),
            ("short", good.replace(&bh, &bh[2..])),
            ("long", good.replace(&bh, &format!("{bh}00"))),
            ("after sig", {
                let sig = good.rfind("; sig=").unwrap();
                format!("{}{}; bh={bh}", &good[..sig - (bh.len() + 5)], &good[sig..])
            }),
        ] {
            assert!(
                matches!(
                    parse_node_request_header(&header),
                    Err(NodeRequestError::Malformed(_))
                ),
                "{name}"
            );
        }
    }
}
