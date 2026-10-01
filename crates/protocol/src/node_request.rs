//! Node-to-node request credential: an Ed25519 signature over one HTTP
//! request, carried in the [`NODE_REQUEST_HEADER`] header. Pure logic, no I/O.
//!
//! The signature covers method, path (no query), body hash, network, intended
//! recipient, claimed peer id, timestamp and nonce, so a captured header cannot
//! be replayed against another route, body, network or recipient. This module
//! checks the signature and the clock window only: it knows nothing about
//! libp2p (the caller derives the peer id from the key and compares it to the
//! claimed one) and keeps no nonce cache (the caller rejects repeats).
//!
//! The recipient and network are not carried in the header, so a signature made
//! for another recipient or network is indistinguishable from a forged one and
//! reports [`NodeRequestError::BadSignature`].

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
    /// A method, path, network, recipient or peer id that cannot be signed.
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
        }
    }
}

/// The request parts a signature binds, minus the recipient and signer fields.
#[derive(Debug, Clone, Copy)]
pub struct NodeRequestTarget<'a> {
    /// Uppercase ASCII HTTP method.
    pub method: &'a str,
    /// Request path starting with `/`, without query or fragment.
    pub path: &'a str,
    pub body: &'a [u8],
    pub network_id: &'a str,
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
    pub signature: [u8; 64],
}

fn push_lp(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    out.extend_from_slice(bytes);
}

fn check_len(value: &str, max: usize, what: &'static str) -> Result<(), NodeRequestError> {
    if value.len() > max {
        return Err(NodeRequestError::InvalidRequest(what));
    }
    Ok(())
}

fn valid_peer_id(peer_id: &str) -> bool {
    !peer_id.is_empty()
        && peer_id.len() <= MAX_PEER_ID_LEN
        && peer_id.bytes().all(|b| b.is_ascii_alphanumeric())
}

fn validate_inputs(
    target: &NodeRequestTarget<'_>,
    recipient: &str,
    peer_id: &str,
) -> Result<(), NodeRequestError> {
    let method = target.method;
    if method.is_empty() || !method.bytes().all(|b| b.is_ascii_uppercase()) {
        return Err(NodeRequestError::InvalidRequest("method"));
    }
    check_len(method, MAX_METHOD_LEN, "method")?;
    let path = target.path;
    if !path.starts_with('/') || path.bytes().any(|b| b == b'?' || b == b'#') {
        return Err(NodeRequestError::InvalidRequest("path"));
    }
    check_len(path, MAX_PATH_LEN, "path")?;
    check_len(target.network_id, MAX_NAME_LEN, "network_id")?;
    check_len(recipient, MAX_NAME_LEN, "recipient")?;
    if !valid_peer_id(peer_id) {
        return Err(NodeRequestError::InvalidRequest("peer_id"));
    }
    Ok(())
}

/// The exact bytes a node request signature covers.
pub fn node_request_signing_message(
    target: &NodeRequestTarget<'_>,
    recipient: &str,
    peer_id: &str,
    timestamp: i64,
    nonce: &[u8; 16],
) -> Result<Vec<u8>, NodeRequestError> {
    validate_inputs(target, recipient, peer_id)?;
    let mut message = Vec::new();
    message.extend_from_slice(NODE_REQUEST_DOMAIN);
    push_lp(&mut message, target.method.as_bytes());
    push_lp(&mut message, target.path.as_bytes());
    push_lp(&mut message, &Sha256::digest(target.body));
    push_lp(&mut message, target.network_id.as_bytes());
    push_lp(&mut message, recipient.as_bytes());
    push_lp(&mut message, peer_id.as_bytes());
    message.extend_from_slice(&timestamp.to_be_bytes());
    message.extend_from_slice(nonce);
    Ok(message)
}

/// Signs a request for `recipient` as `peer_id`; the caller supplies the nonce.
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
        signature: signature.to_bytes(),
    })
}

/// Encodes `v1; peer=<id>; key=<hex>; ts=<secs>; nonce=<hex>; sig=<hex>` (lowercase hex).
pub fn encode_node_request_header(auth: &NodeRequestAuth) -> String {
    format!(
        "v1; peer={}; key={}; ts={}; nonce={}; sig={}",
        auth.peer_id,
        hex::encode(auth.public_key),
        auth.timestamp,
        hex::encode(auth.nonce),
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

/// Parses the header strictly: exactly the six fields in the order above, joined by `"; "`,
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
        signature: lower_hex::<64>(sig, "sig")?,
    })
}

/// Verifies a credential: clock window first, then the key, then the signature
/// against each accepted recipient. The caller still owes the peer id/key check
/// and the nonce replay check.
pub fn verify_node_request(
    auth: &NodeRequestAuth,
    target: &NodeRequestTarget<'_>,
    accepted_recipients: &[&str],
    now: i64,
    max_skew_secs: i64,
) -> Result<(), NodeRequestError> {
    validate_inputs(target, "", &auth.peer_id)?;
    let skew = max_skew_secs.max(0);
    if auth.timestamp < now.saturating_sub(skew) {
        return Err(NodeRequestError::Stale);
    }
    if auth.timestamp > now.saturating_add(skew) {
        return Err(NodeRequestError::Future);
    }
    let key =
        VerifyingKey::from_bytes(&auth.public_key).map_err(|_| NodeRequestError::InvalidKey)?;
    let signature = Signature::from_bytes(&auth.signature);
    let mut verified = false;
    for recipient in accepted_recipients {
        let message = node_request_signing_message(
            target,
            recipient,
            &auth.peer_id,
            auth.timestamp,
            &auth.nonce,
        )?;
        if key.verify_strict(&message, &signature).is_ok() {
            verified = true;
            break;
        }
    }
    if verified {
        Ok(())
    } else {
        Err(NodeRequestError::BadSignature)
    }
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
        bad(verify(
            &auth,
            &NodeRequestTarget {
                body: b"{\"a\":2}",
                ..t
            },
        ));
        bad(verify(&auth, &NodeRequestTarget { body: b"", ..t }));
        bad(verify(
            &auth,
            &NodeRequestTarget {
                network_id: "other-net",
                ..t
            },
        ));
        bad(verify_node_request(&auth, &t, &["node-c"], NOW, SKEW));
        bad(verify_node_request(&auth, &t, &[], NOW, SKEW));

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
        a.public_key = SigningKey::from_bytes(&[0x43; 32])
            .verifying_key()
            .to_bytes();
        bad(verify(&a, &t));
        let mut a = auth.clone();
        a.signature[10] ^= 1;
        bad(verify(&a, &t));
    }

    #[test]
    fn length_prefixes_stop_field_boundary_shifts() {
        let t = target();
        let auth = signed();
        let shifted = NodeRequestTarget {
            method: "POS",
            path: "T/internal/v1/things",
            ..t
        };
        assert!(verify(&auth, &shifted).is_err());
        let a = sign_node_request(&key(), PEER, &t, "node-b", NOW, NONCE).unwrap();
        let b = sign_node_request(
            &key(),
            PEER,
            &NodeRequestTarget {
                network_id: "avalon-testnode-b",
                ..t
            },
            "",
            NOW,
            NONCE,
        )
        .unwrap();
        assert_ne!(a.signature, b.signature);
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
    fn extreme_now_and_negative_skew_do_not_overflow() {
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
            Ok(())
        );
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
    fn small_order_key_is_rejected() {
        // The identity point is a valid encoding but a weak key; strict verification refuses it.
        let mut a = signed();
        a.public_key = [0; 32];
        a.public_key[0] = 1;
        a.signature = [0; 64];
        assert_eq!(verify(&a, &target()), Err(NodeRequestError::BadSignature));
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
}
