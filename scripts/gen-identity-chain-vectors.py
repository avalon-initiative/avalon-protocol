#!/usr/bin/env python3
"""Writes the hashVectors and keyEventHashVectors of conformance/vectors/identity-chain.json.

An encoder for the identity chain event hash layouts (avalon.identity.chain_event and
avalon.identity.key_event) that shares no code with the Rust one, so the committed expectations are an independent check of it. The
resolutionCases are kept from the existing file.
"""
import base64
import hashlib
import json
import struct
import sys
import uuid
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent / "lib"))
import signing_envelope as env  # noqa: E402

TAG = b"avalon.identity.chain_event"
OUT = Path(__file__).resolve().parent.parent / "conformance" / "vectors" / "identity-chain.json"


def s(text):
    raw = text.encode("utf-8")
    return struct.pack(">I", len(raw)) + raw


def canonical(doc):
    # Sufficient for the BMP keys and integer/string/bool/null values the vectors use.
    return json.dumps(doc, sort_keys=True, separators=(",", ":"), ensure_ascii=False)


def message(i, payload_hash):
    prev = i["prevHashHex"]
    return (
        env.header(TAG, i["layoutVersion"], i["rulesVersion"])
        + bytes.fromhex(i["identityIdHex"])
        + struct.pack(">Q", int(i["seq"]))
        + uuid.UUID(i["eventId"]).bytes
        + s(i["kind"])
        + s(i["issuer"])
        + s(i["subject"])
        + struct.pack(">I", i["eventVersion"])
        + struct.pack(">q", int(i["timestampUnixMicros"]))
        + env.hash_algo(i["hashAlgo"])
        + (b"\x00" if prev is None else b"\x01" + bytes.fromhex(prev))
        + bytes.fromhex(payload_hash)
        + env.extension_region(
            [(e["type"], e["critical"], bytes.fromhex(e["valueHex"])) for e in i["extensions"]]
        )
    )


ID = "97129fe5aedcb1e8eccb2072bcf83e58478a22330dd589afd9d4545764f0cd56"
SELF = f"identity:{ID}:self:profile_updated"
BASE = dict(
    identityIdHex=ID,
    seq="1",
    prevHashHex=None,
    eventId="0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b",
    kind="profile.updated",
    issuer=SELF,
    subject=SELF,
    eventVersion=1,
    timestampUnixMicros="1700000000000000",
    layoutVersion=1,
    rulesVersion=1,
    hashAlgo=1,
    extensions=[],
)


def vec(name, payload_json, **over):
    i = dict(BASE, **over)
    canon = canonical(json.loads(payload_json))
    ph = hashlib.sha256(canon.encode("utf-8")).hexdigest()
    msg = message(i, ph)
    return {
        "name": name,
        "input": dict(i, payloadJsonUtf8=payload_json),
        "expected": {
            "payloadCanonicalUtf8": canon,
            "payloadHashHex": ph,
            "signingBytesHex": msg.hex(),
            "eventHashHex": hashlib.sha256(msg).hexdigest(),
        },
    }


vectors = [vec("genesis profile update", '{"bio":"first"}')]
first = vectors[0]["expected"]["eventHashHex"]
vectors.append(
    vec(
        "second event points at the first",
        '{"bio":"second"}',
        seq="2",
        prevHashHex=first,
        eventId="0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5c",
        timestampUnixMicros="1700000060000000",
    )
)
vectors += [
    vec(
        "passkey registered",
        '{"label":"laptop"}',
        kind="identity.passkey_registered",
        issuer=f"identity:{ID}:self:passkey_registered",
        subject=f"identity:{ID}:self:passkey_registered",
        seq="3",
        prevHashHex="af" * 32,
    ),
    vec("payload keys are sorted before hashing", '{"b":1,"a":{"z":true,"y":null},"c":[2,"x"]}'),
    vec("empty payload object", "{}"),
    vec("null payload", "null"),
    vec("seq at u64 max", '{"n":1}', seq="18446744073709551615", prevHashHex="ab" * 32),
    vec("seq 0", '{"n":1}', seq="0"),
    vec("event version 0", '{"n":1}', eventVersion=0),
    vec("event version u32 max", '{"n":1}', eventVersion=4294967295),
    vec("event version 2", '{"n":1}', eventVersion=2),
    vec(
        "unknown non-critical extension is hashed",
        '{"n":1}',
        extensions=[{"type": 12, "critical": False, "valueHex": "0102"}],
    ),
    vec("non-zero prev hash", '{"n":1}', seq="2", prevHashHex="ab" * 32),
    vec("all-zero prev hash is not genesis", '{"n":1}', seq="2", prevHashHex="00" * 32),
    vec("other identity", '{"n":1}', identityIdHex="11" * 32),
    vec("nil event id", '{"n":1}', eventId="00000000-0000-0000-0000-000000000000"),
    vec("empty kind, issuer and subject", '{"n":1}', kind="", issuer="", subject=""),
    vec("boundary: kind ab, issuer c", '{"n":1}', kind="ab", issuer="c", subject="d"),
    vec("boundary: kind a, issuer bc", '{"n":1}', kind="a", issuer="bc", subject="d"),
    vec("boundary: issuer ab, subject c", '{"n":1}', kind="k", issuer="ab", subject="c"),
    vec("boundary: issuer a, subject bc", '{"n":1}', kind="k", issuer="a", subject="bc"),
    vec(
        "separators and NUL inside strings",
        '{"n":1}',
        kind="a:b,c",
        issuer="i\u0000j",
        subject=":::",
    ),
    vec(
        "multi-byte UTF-8 in strings and payload",
        '{"name":"héllo 世界 ☃"}',
        kind="ké",
        issuer="世界",
        subject="☃",
    ),
    vec("microsecond time", '{"n":1}', timestampUnixMicros="1700000000000001"),
    vec("time before the epoch", '{"n":1}', timestampUnixMicros="-1"),
    vec("time at the epoch", '{"n":1}', timestampUnixMicros="0"),
    vec("time at i64 min", '{"n":1}', timestampUnixMicros="-9223372036854775808"),
    vec("time at i64 max", '{"n":1}', timestampUnixMicros="9223372036854775807"),
]

KEY_TAG = b"avalon.identity.key_event"


def uuid_of(n):
    return str(uuid.UUID(int=n))


def raw(seed, n):
    return bytes((seed + i) % 256 for i in range(n))


def key_message(i):
    prev = i["prevHashHex"]
    out = (
        env.header(KEY_TAG, 1, 1)
        + bytes.fromhex(i["identityIdHex"])
        + struct.pack(">Q", int(i["seq"]))
        + env.hash_algo()
        + (b"\x00" if prev is None else b"\x01" + bytes.fromhex(prev))
    )
    if i["content"] == "device_grant":
        out += (
            b"\x01"
            + uuid.UUID(i["signingKeyId"]).bytes
            + uuid.UUID(i["approvedBySigningKeyId"]).bytes
            + env.key(bytes.fromhex(i["publicKeyHex"]))
            + uuid.UUID(i["grantId"]).bytes
            + env.signature(bytes.fromhex(i["signatureHex"]))
        )
    else:
        out += (
            b"\x02"
            + uuid.UUID(i["signingKeyId"]).bytes
            + uuid.UUID(i["revokedBySigningKeyId"]).bytes
            + env.signature(bytes.fromhex(i["signatureHex"]))
        )
    return out + env.extension_region()


GRANT = dict(
    content="device_grant",
    identityIdHex=ID,
    seq="1",
    prevHashHex=None,
    signingKeyId=uuid_of(0x11),
    approvedBySigningKeyId=uuid_of(0x22),
    publicKeyHex=raw(1, 32).hex(),
    grantId=uuid_of(0x11),
    signatureHex=raw(40, 64).hex(),
    unsigned=dict(
        eventId="0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b",
        timestampUnixMicros="1700000000000000",
        deviceLabel="phone",
    ),
)
REVOKE = dict(
    content="revocation",
    identityIdHex=ID,
    seq="2",
    prevHashHex="ab" * 32,
    signingKeyId=uuid_of(0x11),
    revokedBySigningKeyId=uuid_of(0x22),
    signatureHex=raw(90, 64).hex(),
    unsigned=dict(
        eventId="0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5c",
        timestampUnixMicros="1700000060000000",
        deviceLabel=None,
    ),
)


def key_vec(name, base, **over):
    i = dict(base, **over)
    msg = key_message(i)
    return {
        "name": name,
        "input": i,
        "expected": {
            "signingBytesHex": msg.hex(),
            "eventHashHex": hashlib.sha256(msg).hexdigest(),
        },
    }


key_vectors = [
    key_vec("device grant at genesis", GRANT),
    key_vec("device grant after another event", GRANT, seq="3", prevHashHex="cd" * 32),
    key_vec("revocation", REVOKE),
    key_vec("revocation at genesis", REVOKE, seq="1", prevHashHex=None),
    key_vec("other identity", GRANT, identityIdHex="11" * 32),
    key_vec("other key id", GRANT, signingKeyId=uuid_of(0x12), grantId=uuid_of(0x12)),
    key_vec("other approver", GRANT, approvedBySigningKeyId=uuid_of(0x23)),
    key_vec("other public key", GRANT, publicKeyHex=raw(2, 32).hex()),
    key_vec("other signature", GRANT, signatureHex=raw(41, 64).hex()),
    key_vec("all-zero prev hash is not genesis", GRANT, seq="2", prevHashHex="00" * 32),
    key_vec("seq at u64 max", REVOKE, seq="18446744073709551615"),
    key_vec(
        "unsigned fields are not part of the hash",
        GRANT,
        unsigned=dict(
            eventId="0190a1b2-c3d4-7e5f-8a9b-ffffffffffff",
            timestampUnixMicros="-1",
            deviceLabel=None,
        ),
    ),
]

old = json.loads(OUT.read_text())
doc = {
    "$schema": "./SCHEMA.md#identity-chain",
    "description": (
        "Per-identity event chains (avalon_protocol::identity_chain). hashVectors pin the chain event "
        "hash (domain tag avalon.identity.chain_event): eventHashHex is the digest of signingBytesHex, "
        "which is the tag (ASCII, no length), layoutVersion as u16 BE, rulesVersion as u32 BE, identityIdHex "
        "(32 raw bytes), seq as u64 BE, eventId (16 raw bytes), kind, issuer and subject (u32 BE length "
        "and UTF-8), eventVersion as u32 BE, timestampUnixMicros as i64 BE (the instant floored to "
        "microseconds), hashAlgo as u8 (1 = SHA-256), prevHashHex (u8 0 for genesis, or u8 1 then 32 raw "
        "bytes), the payload hash (32 raw bytes, the SHA-256 of the canonical payload from "
        "canonical-payload.json) and the extensions region (see ledger-entry-hash.json); extensions is a "
        "list of {type, critical, valueHex}. The chain "
        "position is never part of the payload. seq and timestampUnixMicros are decimal strings. "
        "resolutionCases pin the deterministic conflict rule (apply_chain): a runner must reach the "
        "same accepted chain and fork position for every ordering of a case's events. "
        "keyEventHashVectors pin the chain hash of an owner-signed key event (domain tag "
        "avalon.identity.key_event), which covers only what the event's signature covers: "
        "eventHashHex is the digest of signingBytesHex, which is the tag, layoutVersion 1 as u16 BE, "
        "rules version 1 as u32 BE, identityIdHex (32 raw bytes), seq as u64 BE, hashAlgo u8 (1), "
        "prevHashHex (u8 0, or u8 1 then 32 raw bytes), a content u8 (1 device grant, 2 revocation), then for "
        "a grant signingKeyId and approvedBySigningKeyId (16 raw bytes each), the public key (alg u8 1 then "
        "32 raw bytes), grantId (16 raw) and the approval signature (alg u8 1 then 64 raw bytes), and "
        "for a revocation signingKeyId and revokedBySigningKeyId (16 raw bytes each) and the signature "
        "(alg u8 1 then 64 raw bytes), then an empty extensions region (0000). input.unsigned (event id, "
        "time, device label) is carried by the event but is never hashed: any value yields the same "
        "hash. "
        "hashVectors are generated by scripts/gen-identity-chain-vectors.py, an encoder independent "
        "of the Rust one."
    ),
    "supportedIn": ["rust"],
    "notSupported": old["notSupported"],
    "hashVectors": vectors,
    "keyEventHashVectors": key_vectors,
    "resolutionCases": old["resolutionCases"],
}
text = json.dumps(doc, indent=2, ensure_ascii=False) + "\n"
if len(sys.argv) > 1 and sys.argv[1] == "--check":
    sys.exit(0 if OUT.read_text() == text else "identity-chain.json is stale")
OUT.write_text(text)
