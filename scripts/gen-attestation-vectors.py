#!/usr/bin/env python3
"""Writes the attestation signing vectors: conformance/vectors/attestation-signing.json.

A structured-bytes encoder that shares no code with the Rust one, so the committed
expectations are an independent check of it. Requires the `cryptography` package.
"""
import hashlib
import json
import struct
import sys
import uuid
from pathlib import Path

from cryptography.hazmat.primitives import serialization
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

sys.path.insert(0, str(Path(__file__).resolve().parent / "lib"))
import signing_envelope as env  # noqa: E402

OUT = Path(__file__).resolve().parent.parent / "conformance" / "vectors"
CHECK = "--check" in sys.argv[1:]
SEED = bytes.fromhex("9a8b7c6d5e4f30211203344556677889aabbccddeeff00112233445566778899")
SUBJECT = "82bb722ef3822f3019fe5abd77f2d33c4a9ab52c2f4f6932635a6d1a9e5815e6"
KEY_A = "3f2b8c1a-9d4e-4f6a-8b7c-0a1b2c3d4e5f"
KEY_B = "a1b2c3d4-e5f6-4789-8abc-def012345678"
NETWORK = "avalon-dev-local"
OTHER_NETWORK = "avalon-mainnet-1"
ATTESTATION = "55555555-5555-4555-8555-555555555555"
T = 1_790_000_000_123_456


def pubkey():
    return Ed25519PrivateKey.from_private_bytes(SEED).public_key().public_bytes(
        serialization.Encoding.Raw, serialization.PublicFormat.Raw
    )


def sign(message):
    return Ed25519PrivateKey.from_private_bytes(SEED).sign(message)


def st(text):
    raw = text.encode("utf-8")
    return struct.pack(">I", len(raw)) + raw


def head(tag, i):
    return (
        env.header(tag.encode("ascii"), 1, env.RULES_VERSION)
        + st(i["networkId"])
        + st(i["claimKind"])
        + st(i["issuerRef"])
        + uuid.UUID(i["signingKeyId"]).bytes
    )


def tail():
    return env.extension_region()


def issue_bytes(i):
    return (
        head("avalon.attestation.issue", i)
        + bytes.fromhex(i["subject"])
        + st(i["achievement"])
        + struct.pack(">q", int(i["issuedAtMicros"]))
        + tail()
    )


def bulk_bytes(i):
    return (
        head("avalon.attestation.bulk_issue", i)
        + bytes.fromhex(i["subject"])
        + struct.pack(">q", int(i["issuedAtMicros"]))
        + struct.pack(">I", len(i["achievements"]))
        + b"".join(st(a) for a in i["achievements"])
        + tail()
    )


def revoke_bytes(i):
    return (
        head("avalon.attestation.revoke", i)
        + uuid.UUID(i["attestationId"]).bytes
        + st(i["reasonCode"])
        + bytes([1])
        + hashlib.sha256(i["reason"].encode("utf-8")).digest()
        + tail()
    )


BUILD = {"issue": issue_bytes, "bulk_issue": bulk_bytes, "revoke": revoke_bytes}


def build(i):
    return BUILD[i["operation"]](i)


def legacy(i):
    kind, ref = i["claimKind"], i["issuerRef"]
    if i["operation"] == "issue":
        return f"avalon:{kind}.issued:v1:{ref}:{i['subject']}:{i['achievement']}"
    if i["operation"] == "revoke":
        return f"avalon:{kind}.revoked:v1:{ref}:{i['attestationId']}:{i['reasonCode']}"
    body = b"".join(struct.pack(">I", len(a.encode())) + a.encode() for a in i["achievements"])
    return (
        f"avalon:{kind}.issued.bulk:v1:{ref}:{i['subject']}:".encode()
        + struct.pack(">I", len(i["achievements"]))
        + body
    ).decode()


def issue(kind, ref, achievement, **kw):
    i = {
        "operation": "issue",
        "networkId": NETWORK,
        "claimKind": kind,
        "issuerRef": ref,
        "signingKeyId": KEY_A,
        "subject": SUBJECT,
        "achievement": achievement,
        "issuedAtMicros": T,
    }
    i.update(kw)
    return i


def bulk(kind, ref, achievements, **kw):
    i = issue(kind, ref, None, **kw)
    del i["achievement"]
    i.update(operation="bulk_issue", achievements=achievements)
    i.update(kw)
    return i


def revoke(kind, ref, code, reason, **kw):
    i = {
        "operation": "revoke",
        "networkId": NETWORK,
        "claimKind": kind,
        "issuerRef": ref,
        "signingKeyId": KEY_A,
        "attestationId": ATTESTATION,
        "reasonCode": code,
        "reason": reason,
    }
    i.update(kw)
    return i


def vector(name, i):
    msg = build(i)
    return {"name": name, "input": i, "expected": {"signingBytesHex": msg.hex(), "signatureHex": sign(msg).hex()}}


def replay(name, base, changed):
    assert build(base) != build(changed)
    return {"name": name, "input": changed, "signatureHex": sign(build(base)).hex(), "expected": {"valid": False}}


def legacy_vector(name, i):
    text = legacy(i)
    return {
        "name": name,
        "input": i,
        "legacySigningBytesUtf8": text,
        "signatureHex": sign(text.encode()).hex(),
        "expected": {"valid": False},
    }


G, GREF = "achievement", "game:ashen-realms"
ACH = "game:ashen-realms:achievement:dragon_slayer"
LOST = "game:ashen-realms:achievement:lost_city"
M, MREF = "milestone", "app:wallet-app"

single = issue(G, GREF, ACH)
bulk_base = bulk(G, GREF, [ACH, LOST])
rev = revoke(G, GREF, "cheating", "Used a modified client")

vectors = [
    vector("game issuance", single),
    vector("app milestone issuance", issue(M, MREF, "app:wallet-app:milestone:onboarded")),
    vector("issued_at at the epoch", issue(G, GREF, ACH, issuedAtMicros=0)),
    vector("issued_at before the epoch", issue(G, GREF, ACH, issuedAtMicros=-1)),
    vector("colon-bearing issuer ref and achievement id keep their boundaries", issue(M, "service:a:b", "service:a:b:milestone:c:d")),
    vector("issuance on another network", issue(G, GREF, ACH, networkId=OTHER_NETWORK)),
    vector("multi-byte network id uses its byte length", issue(G, GREF, ACH, networkId="avalon-dev-lán-日本")),
    vector("multi-byte achievement id uses its byte length", issue(G, GREF, "game:ashen-realms:achievement:drachen_töter_日本")),
    vector("game bulk issuance", bulk_base),
    vector("service bulk issuance, single claim", bulk(M, "service:ledger-watch", ["service:ledger-watch:milestone:first_report"])),
    vector("bulk issuance, split-boundary ambiguity guard", bulk(G, GREF, ["ab", "c"])),
    vector("bulk issuance, empty claim list", bulk(G, GREF, [])),
    vector("bulk issuance, repeated claim", bulk(G, GREF, [ACH, ACH])),
    vector("game revocation, known reason code", rev),
    vector("app milestone revocation, unrecognised reason code", revoke(M, MREF, "definition_retired", "The milestone was retired")),
    vector("revocation with an empty reason", revoke(G, GREF, "mistake", "")),
    vector("revocation, colon-bearing reason code and reason", revoke(G, GREF, "a:b", "c:d")),
    vector("revocation, multi-byte reason", revoke(G, GREF, "duplicate", "Doppelt ✓ 日本語")),
]

replays = [
    replay("issuance signed for another network", single, dict(single, networkId=OTHER_NETWORK)),
    replay("issuance signed under another claim kind", single, dict(single, claimKind=M)),
    replay("issuance signed for another issuer", single, dict(single, issuerRef="game:other")),
    replay("issuance signed under another signing key id", single, dict(single, signingKeyId=KEY_B)),
    replay("issuance signed for another subject", single, dict(single, subject="aa" * 32)),
    replay("issuance signed for another achievement", single, dict(single, achievement=LOST)),
    replay("issuance signed at another issued_at", single, dict(single, issuedAtMicros=T + 1)),
    replay("a ':' moved between the issuer ref and the achievement", issue(M, "app:w:x", "y"), issue(M, "app:w", "x:y")),
    replay("bulk signed for another network", bulk_base, dict(bulk_base, networkId=OTHER_NETWORK)),
    replay("bulk signed under another signing key id", bulk_base, dict(bulk_base, signingKeyId=KEY_B)),
    replay("bulk signed at another issued_at", bulk_base, dict(bulk_base, issuedAtMicros=T + 1)),
    replay("bulk signed in another claim order", bulk_base, dict(bulk_base, achievements=[LOST, ACH])),
    replay("bulk signed with a claim dropped", bulk_base, dict(bulk_base, achievements=[ACH])),
    replay("bulk split-boundary replay", bulk(G, GREF, ["ab", "c"]), bulk(G, GREF, ["a", "bc"])),
    replay("a bulk signature offered as a single issuance", bulk(G, GREF, [ACH]), single),
    replay("revocation signed for another network", rev, dict(rev, networkId=OTHER_NETWORK)),
    replay("revocation signed under another signing key id", rev, dict(rev, signingKeyId=KEY_B)),
    replay("revocation signed for another attestation", rev, dict(rev, attestationId="55555555-5555-4555-8555-555555555556")),
    replay("revocation signed with another reason code", rev, dict(rev, reasonCode="mistake")),
    replay("revocation signed with another reason", rev, dict(rev, reason="Other")),
    replay("a ':' moved between the reason code and the reason", revoke(G, GREF, "a", "b:c"), revoke(G, GREF, "a:b", "c")),
]

legacy_vectors = [
    legacy_vector("the old text layout of an issuance does not verify", single),
    legacy_vector("the old text layout of a bulk issuance does not verify", bulk_base),
    legacy_vector("the old text layout of a revocation does not verify", rev),
]

DESC = (
    "Attestation issuance, bulk issuance and revocation signing bytes "
    "(avalon_protocol::achievements::{attestation,bulk_attestation,revocation}_signing_bytes). An "
    "issuer's Ed25519 key signs these exact bytes; the verifying node rebuilds them and rejects "
    "anything that does not match. Tags avalon.attestation.issue, avalon.attestation.bulk_issue and "
    "avalon.attestation.revoke, layout version 1. Every layout starts with the same fields: "
    "network_id (str, the network the signature is valid on), claim_kind (str, \"achievement\" for game issuers and \"milestone\" for app "
    "and service issuers), issuer_ref (str, \"<namespace>:<slug>\") and signing_key_id (uuid, the "
    "issuer key that signs); an attestation signed for one network never verifies on another. Issue then adds subject (32 raw bytes), achievement (str, the claim's "
    "full global id) and issued_at (i64 BE unix microseconds, signed by the issuer and accepted "
    "only within 300 seconds of the verifying node's clock). Bulk issue adds subject, issued_at "
    "(one for every claim), count (u32 BE) and each achievement (str) in order. Revoke adds "
    "attestation_id (uuid), reason_code (str; an unrecognised code is signed as its own string), "
    "hash_algo (u8, 01 = SHA-256) and the 32-byte SHA-256 of the UTF-8 reason, so the reason text can be redacted while the signature stays valid. The layout is the structured signing-bytes encoding "
    "(avalon_protocol::signing_bytes): the ASCII domain tag with no length, a u16 big-endian layout "
    "version (1) and a u32 big-endian rules version (1), then the fields in the order listed, then "
    "the extensions region (count u16 = 0: 00 00). str is a u32 big-endian byte length and the "
    "UTF-8 bytes. replayVectors offer a signature made for one input with another input's bytes; "
    "legacyLayoutVectors carry a signature over the retired colon-delimited layout. Neither may verify."
)
GEN = (
    "Generated by scripts/gen-attestation-vectors.py, an encoder that shares no code with the Rust "
    "one (cryptography Ed25519). signingKeySeedHex is the Ed25519 seed of the issuer key; Ed25519 "
    "signatures are deterministic."
)
REASON = "Protocol-only until the SDK slice lands the structured attestation layouts with the network id."

doc = {
    "$schema": "./SCHEMA.md#attestation-signing",
    "description": DESC,
    "generation": GEN,
    "signingKeySeedHex": SEED.hex(),
    "signingPublicKeyHex": pubkey().hex(),
    "replayVectors": replays,
    "legacyLayoutVectors": legacy_vectors,
    "vectors": vectors,
    "supportedIn": ["rust"],
    "notSupported": {"csharp": REASON, "typescript": REASON},
}
text = json.dumps(doc, indent=2, sort_keys=True, ensure_ascii=False) + "\n"
path = OUT / "attestation-signing.json"
if CHECK:
    if path.read_text() != text:
        sys.exit("stale: attestation-signing.json")
else:
    path.write_text(text)
